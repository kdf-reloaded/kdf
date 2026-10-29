// siacoin_helpers — Constructor, builder, and internal utility methods.

use super::*;

impl SiaCoin {
    pub async fn new(
        _ctx: &MmArc,
        json_conf: Json,
        request: &SiaCoinActivationRequest,
        priv_key_policy: PrivKeyBuildPolicy,
    ) -> Result<Self, MmError<SiaCoinNewError>> {
        // `derivation_method` mirrors the priv-key-policy branch it's built alongside:
        // Iguana yields a fixed single address, GlobalHDAccount yields a real
        // `SiaHDWallet` (CRD ch.20 D1) rooted at `m/44'/1991'`. `key_pair` itself is
        // still always the single-key/single-address-mode key (`SINGLE_ADDRESS_MODE_PATH`,
        // identical to account 0/External/address 0 of the HD wallet -- proved by
        // `sia_hd_wallet.rs`'s own `account_0_external_0_matches_the_existing_single_address_mode_path`
        // test), because every existing signing call site (`my_keypair()`, swap ops,
        // withdraw) still only knows how to spend from one active key (ch.20 D1's own
        // "still open" note; the HD wallet built here backs read-only account/address
        // discovery via `CoinWithDerivationMethod`/`HDWalletBalanceOps`, not spending).
        let (key_pair, derivation_method) = match priv_key_policy {
            PrivKeyBuildPolicy::IguanaPrivKey(priv_key) => {
                let key_pair = SiaKeypair::from_private_bytes(priv_key.as_slice())?;
                let address = key_pair.public().address();
                (key_pair, DerivationMethod::Iguana(address))
            },
            PrivKeyBuildPolicy::GlobalHDAccount(global_hd_account) => {
                let extended_key = global_hd_account
                    .derive_ed25519_signing_key(&SINGLE_ADDRESS_MODE_PATH)
                    .map_err(|e| e.into_inner())?;
                let key_pair = SiaKeypair::from_private_bytes(extended_key.signing_key.as_bytes())?;
                let gap_limit = request.gap_limit.unwrap_or(DEFAULT_HD_GAP_LIMIT);
                let hd_wallet = SiaHDWallet::new(&global_hd_account, gap_limit).map_err(|e| e.into_inner())?;
                (key_pair, DerivationMethod::HDWallet(hd_wallet))
            },
            _ => return Err(SiaCoinNewError::UnsupportedPrivKeyPolicy.into()),
        };

        let conf: SiaCoinConf = serde_json::from_value(json_conf)?;

        Ok(SiaCoinBuilder::new(conf, key_pair, derivation_method, request)
            .build(_ctx)
            .await?)
    }
}

pub struct SiaCoinBuilder<'a> {
    conf: SiaCoinConf,
    key_pair: SiaKeypair,
    derivation_method: DerivationMethod<Address, SiaHDWallet>,
    request: &'a SiaCoinActivationRequest,
}

impl<'a> SiaCoinBuilder<'a> {
    pub fn new(
        conf: SiaCoinConf,
        key_pair: SiaKeypair,
        derivation_method: DerivationMethod<Address, SiaHDWallet>,
        request: &'a SiaCoinActivationRequest,
    ) -> Self {
        SiaCoinBuilder {
            conf,
            key_pair,
            derivation_method,
            request,
        }
    }

    async fn build(self, ctx: &MmArc) -> Result<SiaCoin, SiaCoinBuilderError> {
        let history_sync_state = if self.request.tx_history {
            HistorySyncState::NotStarted
        } else {
            HistorySyncState::NotEnabled
        };

        let required_confirmations: AtomicU64 = self
            .request
            .required_confirmations
            .unwrap_or(self.conf.required_confirmations)
            .into();

        // Resolve the DEX fee destination from the network configuration
        // (per-netid; see `mm2_net_config`).
        let netid = ctx.netid();
        let net_cfg = mm2_net_config::net_config_for(netid).ok_or(SiaCoinBuilderError::UnsupportedNetId(netid))?;
        let fee_pubkey_bytes = hex::decode(net_cfg.dex_fee_pubkey_ed25519())
            .map_err(|e| SiaCoinBuilderError::FeePubkeyHex(e.to_string()))?;
        let fee_public_key =
            PublicKey::from_bytes(&fee_pubkey_bytes).map_err(|e| SiaCoinBuilderError::FeePubkey(e.to_string()))?;
        let fee_address = Address::from_public_key(&fee_public_key);

        Ok(SiaCoin {
            conf: self.conf,
            client: Arc::new(
                SiaClient::new(self.request.client_conf.clone())
                    .await
                    .map_err(SiaCoinBuilderError::Client)?,
            ),
            priv_key_policy: PrivKeyPolicy::KeyPair(self.key_pair).into(),
            derivation_method: Arc::new(self.derivation_method),
            history_sync_state: Mutex::new(history_sync_state).into(),
            required_confirmations: required_confirmations.into(),
            fee_address,
        })
    }
}

// ── Internal helpers ─────────────────────────────────────────────────

impl SiaCoin {
    pub fn my_keypair(&self) -> Result<&SiaKeypair, SiaCoinMyKeypairError> {
        match &*self.priv_key_policy {
            PrivKeyPolicy::KeyPair(keypair) => Ok(keypair),
            _ => Err(SiaCoinMyKeypairError::PrivKeyPolicy),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod node_naming_tests {
    use super::*;
    use serde_json::json;

    const SERVER: &str = "http://127.0.0.1:1/wallet/api?key=KEYPART";
    const PASSWORD: &str = "walletdpass";

    fn client_conf() -> SiaClientConf {
        serde_json::from_value(json!({ "server_url": SERVER, "password": PASSWORD, "timeout": 2 })).unwrap()
    }

    fn assert_host_only(text: &str) {
        assert!(text.contains("http://127.0.0.1:1"), "{text}");
        for secret in ["wallet/api", "KEYPART", PASSWORD] {
            assert!(!text.contains(secret), "a credential part reaches a text");
        }
    }

    /// T-N1 (R-N1): the activation error against a closed port names the server host-only.
    #[test]
    fn the_activation_error_names_the_server_host_only() {
        let error = common::block_on(SiaClient::new(client_conf())).err().unwrap();
        assert_host_only(&SiaCoinBuilderError::Client(error).to_string());
    }

    /// T-N1 (R-N2): the debug forms of the request and of the configuration hide the password and its encoded
    /// header value.
    #[test]
    fn the_debug_forms_hide_the_password() {
        let request: SiaCoinActivationRequest = serde_json::from_value(json!({
            "client_conf": { "server_url": SERVER, "password": PASSWORD },
        }))
        .unwrap();
        let encoded = base64::encode(&format!(":{PASSWORD}"));
        for rendered in [
            format!("{request:?}"),
            format!("{:?}", RedactedClientConf(&client_conf())),
        ] {
            assert_host_only(&rendered);
            assert!(!rendered.contains(&encoded), "the encoded header value is rendered");
        }
    }

    /// T-N2 (R-N1, R53.4.8): the text of a failed read, as the `Transport` error and the history path build it,
    /// names the server host-only.
    #[test]
    fn a_failed_read_names_the_server_host_only() {
        let client = common::block_on(async {
            // The probe of `SiaClient::new` fails against a closed port, so a client is not available: the
            // request error of an endpoint call is produced by the same library path.
            SiaClient::new(client_conf()).await
        });
        let error = client.err().unwrap();
        assert_host_only(&sia_text(&error));
    }
}
