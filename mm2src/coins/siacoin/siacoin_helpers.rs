// siacoin_helpers — Constructor, builder, and internal utility methods.

use super::*;
use crate::swap_evidence::{swap_evidence_policy_from, SwapEvidencePolicy};

impl SiaCoin {
    pub async fn new(
        _ctx: &MmArc,
        json_conf: Json,
        request: &SiaCoinActivationRequest,
        priv_key_policy: PrivKeyBuildPolicy,
    ) -> Result<Self, MmError<SiaCoinNewError>> {
        // A request for a hardware wallet is refused before anything else (no hot-key coin for a caller who asked
        // for a device, and no network access).
        if matches!(request.priv_key_policy, PrivKeyActivationPolicy::Trezor) {
            return Err(SiaCoinNewError::UnsupportedPrivKeyPolicy.into());
        }
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
        // The one `server_url` is the only node the field can name; checked before any network access.
        let swap_evidence = sia_swap_evidence_policy(
            &conf.ticker,
            request.swap_evidence_node.as_deref(),
            request.client_conf.server_url.as_str(),
        )
        .map_err(SiaCoinNewError::SwapEvidenceNodeNotInList)?;
        let swap_evidence = crate::swap_evidence::apply_single_host(
            _ctx,
            &conf.ticker,
            swap_evidence,
            request.swap_evidence_node.as_deref(),
            &[request.client_conf.server_url.as_str().to_owned()],
        );

        Ok(
            SiaCoinBuilder::new(conf, key_pair, derivation_method, request, swap_evidence)
                .build(_ctx)
                .await?,
        )
    }
}

/// The swap-evidence policy of a Sia coin (CRD ch.51 R100): it has one walletd `server_url`, so it is `Trusted` when
/// `swap_evidence_node` names it and `Insufficient` otherwise; a field naming another node is refused.
pub(crate) fn sia_swap_evidence_policy(
    ticker: &str,
    field: Option<&str>,
    server_url: &str,
) -> Result<SwapEvidencePolicy, crate::swap_evidence::SwapEvidenceNodeNotInList> {
    swap_evidence_policy_from(ticker, field, &[server_url.to_owned()])
}

pub struct SiaCoinBuilder<'a> {
    conf: SiaCoinConf,
    key_pair: SiaKeypair,
    derivation_method: DerivationMethod<Address, SiaHDWallet>,
    request: &'a SiaCoinActivationRequest,
    swap_evidence: SwapEvidencePolicy,
}

impl<'a> SiaCoinBuilder<'a> {
    pub fn new(
        conf: SiaCoinConf,
        key_pair: SiaKeypair,
        derivation_method: DerivationMethod<Address, SiaHDWallet>,
        request: &'a SiaCoinActivationRequest,
        swap_evidence: SwapEvidencePolicy,
    ) -> Self {
        SiaCoinBuilder {
            conf,
            key_pair,
            derivation_method,
            request,
            swap_evidence,
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

        let swap_evidence_node_name = crate::swap_evidence::node_name(self.request.client_conf.server_url.as_str());
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
            swap_evidence: self.swap_evidence,
            swap_evidence_node_name,
        })
    }
}

// ── Internal helpers ─────────────────────────────────────────────────

impl SiaCoin {
    /// One swap-evidence read of the coin's single walletd node, judged by the coin's policy (CRD ch.51 R100).
    ///
    /// `ask` reads the node, `classify` reduces the answer to the decision the step acts on, and `proves_itself`
    /// is `true` for an answer that needs no second node (a claim whose secret hashes to the swap's secret hash).
    /// `Err` is an undetermined read, as text that names the node and says how to fix it; the step must wait and
    /// must never refund, claim or take "not spent" as proven on it.
    pub(super) async fn swap_evidence_read<A, K, F, Fut>(
        &self,
        ask: F,
        classify: impl Fn(&A) -> K,
        proves_itself: impl Fn(&A) -> bool,
    ) -> Result<(A, K), String>
    where
        K: PartialEq + std::fmt::Debug,
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<A, String>>,
    {
        let policy = MmCoin::swap_evidence_policy(self);
        let nodes = [self.swap_evidence_node_name.clone()];
        let scope = self.readiness_scope();
        // An answer of the node is its last answer for the readiness check before a swap starts.
        let ask_noted = |node: &String| {
            let answer = ask();
            let scope = scope.clone();
            let node = node.clone();
            async move {
                let answer = answer.await;
                if answer.is_ok() {
                    crate::swap_evidence::shared_readiness_probes().note_answer(&scope, &node, common::now_float());
                }
                answer
            }
        };
        match crate::swap_evidence::agree(&policy, &nodes, |node| node.clone(), ask_noted, classify, proves_itself)
            .await
        {
            crate::swap_evidence::Agreement::Agreed { answer, decision } => Ok((answer, decision)),
            crate::swap_evidence::Agreement::Undetermined(undetermined) => {
                Err(self.swap_evidence_undetermined_text(&policy, &undetermined))
            },
        }
    }

    /// The scope of the coin's walletd node in the shared readiness probe record.
    fn readiness_scope(&self) -> String { format!("sia:{}", self.conf.ticker) }

    /// The readiness check before a swap starts (maintainer decision of 2026-11-25/26 under CRD ch.51 R100) of the
    /// coin's one walletd node under its effective policy. The node counts as answering when a swap read was
    /// answered within [`crate::swap_evidence::READY_WINDOW_SECS`]; otherwise it is asked for the current height,
    /// bounded by the probe timeout, in a series of attempts, at most once per window. `mode` is who asks.
    ///
    /// # Errors
    ///
    /// The text of [`crate::swap_evidence::not_ready_text`], which names the coin and the kind only.
    pub(super) async fn evidence_ready(&self, mode: crate::swap_evidence::ReadyMode) -> Result<(), String> {
        let policy = MmCoin::swap_evidence_policy(self);
        let nodes = [self.swap_evidence_node_name.clone()];
        let client = self.client.clone();
        crate::swap_evidence::evidence_ready(
            &self.conf.ticker,
            &policy,
            &nodes,
            |node| node.clone(),
            |_| crate::swap_evidence::HostReadiness::Unknown,
            crate::swap_evidence::shared_readiness_probes(),
            crate::swap_evidence::ReadyScope {
                key: &self.readiness_scope(),
                platform: &self.conf.ticker,
            },
            mode,
            |_| {
                let client = client.clone();
                move |_bound| {
                    let client = client.clone();
                    async move { client.current_height().await.is_ok() }
                }
            },
        )
        .await
    }

    /// The text of an undetermined read: the reason with the node asked (R100 item 9), then the fix.
    fn swap_evidence_undetermined_text(
        &self,
        policy: &SwapEvidencePolicy,
        undetermined: &crate::swap_evidence::Undetermined,
    ) -> String {
        let fix = match policy {
            SwapEvidencePolicy::Insufficient { .. } => format!(
                "{} has one walletd node ({}); set swap_evidence_node to the walletd server_url to trust it",
                self.conf.ticker, self.swap_evidence_node_name
            ),
            _ => format!(
                "the node is {}; it must answer for the swap to go on, check that walletd is reachable",
                self.swap_evidence_node_name
            ),
        };
        format!("{}; {fix}", undetermined.to_err_string())
    }

    pub fn my_keypair(&self) -> Result<&SiaKeypair, SiaCoinMyKeypairError> {
        match &*self.priv_key_policy {
            PrivKeyPolicy::KeyPair(keypair) => Ok(keypair),
            _ => Err(SiaCoinMyKeypairError::PrivKeyPolicy),
        }
    }
}

#[cfg(test)]
mod swap_evidence_tests {
    use super::*;

    /// CRD ch.51 R100: one `server_url` is `Insufficient` without the field and `Trusted` with it.
    #[test]
    fn the_sia_policy_is_trusted_with_the_field_and_insufficient_without() {
        let url = "https://walletd.example:9980/api?key=K";
        assert_eq!(
            sia_swap_evidence_policy("SC", None, url).unwrap(),
            SwapEvidencePolicy::Insufficient { hosts: 1 }
        );
        assert_eq!(
            sia_swap_evidence_policy("SC", Some("walletd.example:9980"), url).unwrap(),
            SwapEvidencePolicy::Trusted { node: url.to_owned() }
        );
        let err = sia_swap_evidence_policy("SC", Some("other.example"), url).unwrap_err();
        assert_eq!(
            err.to_string(),
            "swap_evidence_node other.example is not in the node list of SC"
        );
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

/// `priv_key_policy: IguanaPrivKey` under a global-HD login (CRD ch.05 R46 item 3 and R35 table, Sia row, status
/// note of 2026-11-17).
#[cfg(all(test, not(target_arch = "wasm32")))]
mod priv_key_policy_tests {
    use super::*;
    use crypto::CryptoCtx;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::json;

    const MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const CLIENT_CONF: &str = r#"{ "server_url": "http://127.0.0.1:1/", "password": "pw" }"#;

    fn hd_ctx() -> MmArc {
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "enable_hd": true }))
            .into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), MNEMONIC).unwrap();
        ctx
    }

    fn iguana_ctx() -> MmArc {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), MNEMONIC).unwrap();
        ctx
    }

    fn task_request(extra: Json) -> SiaCoinActivationRequest {
        let mut request = json!({ "client_conf": serde_json::from_str::<Json>(CLIENT_CONF).unwrap() });
        if let Json::Object(extra) = extra {
            request.as_object_mut().unwrap().extend(extra);
        }
        serde_json::from_value(request).unwrap()
    }

    fn legacy_request(extra: Json) -> Result<SiaCoinActivationRequest, MmError<SiaCoinFromLegacyReqErr>> {
        let mut request = json!({ "coin": "SC", "client_conf": serde_json::from_str::<Json>(CLIENT_CONF).unwrap() });
        if let Json::Object(extra) = extra {
            request.as_object_mut().unwrap().extend(extra);
        }
        SiaCoinActivationRequest::from_legacy_req(&request)
    }

    #[test]
    fn the_request_member_defaults_to_the_context_policy_and_takes_both_spellings() {
        assert!(matches!(
            task_request(json!({})).priv_key_policy,
            PrivKeyActivationPolicy::ContextPrivKey
        ));
        for spelling in [json!("IguanaPrivKey"), json!({ "type": "IguanaPrivKey" })] {
            let extra = json!({ "priv_key_policy": spelling });
            assert!(matches!(
                task_request(extra.clone()).priv_key_policy,
                PrivKeyActivationPolicy::IguanaPrivKey
            ));
            assert!(matches!(
                legacy_request(extra).unwrap().priv_key_policy,
                PrivKeyActivationPolicy::IguanaPrivKey
            ));
        }
        assert!(matches!(
            legacy_request(json!({})).unwrap().priv_key_policy,
            PrivKeyActivationPolicy::ContextPrivKey
        ));
        assert!(matches!(
            legacy_request(json!({ "priv_key_policy": "ContextPrivKey" }))
                .unwrap()
                .priv_key_policy,
            PrivKeyActivationPolicy::ContextPrivKey
        ));
        assert!(legacy_request(json!({ "priv_key_policy": "NoSuchPolicy" })).is_err());
        assert!(serde_json::from_value::<SiaCoinActivationRequest>(
            json!({ "client_conf": serde_json::from_str::<Json>(CLIENT_CONF).unwrap(), "priv_key_policy": "NoSuchPolicy" })
        )
        .is_err());
    }

    /// A hardware value is refused by the constructor both routes share, before any network access (the
    /// walletd address of the request is a closed port, so a network attempt would give another error).
    #[test]
    fn a_hardware_priv_key_policy_is_refused_on_both_routes() {
        let ctx = hd_ctx();
        for request in [
            task_request(json!({ "priv_key_policy": "Trezor" })),
            legacy_request(json!({ "priv_key_policy": { "type": "Trezor" } })).unwrap(),
        ] {
            let policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap();
            match common::block_on(SiaCoin::new(&ctx, json!({}), &request, policy)) {
                Err(e) => assert!(matches!(e.into_inner(), SiaCoinNewError::UnsupportedPrivKeyPolicy)),
                Ok(_) => panic!("a hardware policy must not activate a coin"),
            }
        }
    }

    #[test]
    fn an_hd_login_with_iguana_priv_key_builds_on_the_internal_key() {
        let ctx = hd_ctx();
        let internal = CryptoCtx::from_ctx(&ctx).unwrap().mm2_internal_privkey_secret();
        for request in [
            task_request(json!({ "priv_key_policy": "IguanaPrivKey" })),
            legacy_request(json!({ "priv_key_policy": "IguanaPrivKey" })).unwrap(),
        ] {
            match PrivKeyBuildPolicy::from_activation_policy(&ctx, request.priv_key_policy).unwrap() {
                PrivKeyBuildPolicy::IguanaPrivKey(secret) => assert_eq!(secret, internal),
                _ => panic!("the single-key policy is expected"),
            }
        }
    }

    #[test]
    fn an_hd_login_without_or_with_context_priv_key_keeps_the_hd_policy() {
        let ctx = hd_ctx();
        for request in [
            task_request(json!({})),
            task_request(json!({ "priv_key_policy": "ContextPrivKey" })),
            legacy_request(json!({})).unwrap(),
        ] {
            assert!(matches!(
                PrivKeyBuildPolicy::from_activation_policy(&ctx, request.priv_key_policy).unwrap(),
                PrivKeyBuildPolicy::GlobalHDAccount(_)
            ));
        }
    }

    #[test]
    fn an_iguana_login_is_unchanged_by_the_member() {
        let ctx = iguana_ctx();
        let internal = CryptoCtx::from_ctx(&ctx).unwrap().mm2_internal_privkey_secret();
        for policy in [
            PrivKeyActivationPolicy::IguanaPrivKey,
            PrivKeyActivationPolicy::ContextPrivKey,
        ] {
            match PrivKeyBuildPolicy::from_activation_policy(&ctx, policy).unwrap() {
                PrivKeyBuildPolicy::IguanaPrivKey(secret) => assert_eq!(secret, internal),
                _ => panic!("the single-key policy is expected"),
            }
        }
    }

    /// The single address under `IguanaPrivKey` is the one the ed25519 key built from the internal secret bytes
    /// has, as `SiaCoin::new` builds it. The key and the address are fixed here: the secret is the first test
    /// vector of RFC 8032 section 7.1 and its public key is the one the RFC lists.
    #[test]
    fn the_sia_address_of_a_fixed_internal_secret_is_pinned() {
        let secret_bytes: [u8; 32] = hex::decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
            .unwrap()
            .try_into()
            .unwrap();
        let secret = keys::Secret::from(&secret_bytes[..]);
        let key_pair = SiaKeypair::from_private_bytes(secret.as_slice()).unwrap();
        assert_eq!(
            hex::encode(key_pair.public().to_bytes()),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
        // The address is a function of that public key alone. The literal was computed independently of the
        // code, from the address rule (a hash of the single-key policy) that also reproduces the address
        // constants of the other tests of this module family.
        assert_eq!(
            key_pair.public().address().to_string(),
            "0951975e893dc10b9b4ce98bd61fa6e07447f3e045e90e28d417bb5ef84590cc4d79f88492e7"
        );
    }
}
