//! Core EthCoin and EthCoinImpl method implementations, transaction helpers,
//! utility functions, and coin construction.

use super::*;
// LP-17: bring alloy `Provider` trait into scope so `RootProvider`'s
// inherent + trait methods (`client()`, `get_block_number()`, etc.) are
// callable from the helpers below.
use crate::hd_wallet::{HDAccountOps, HDWalletCoinOps, HDWalletOps};
use crate::{CoinWithDerivationMethod, CryptoCtx, KeyPairPolicy};
use alloy::providers::Provider as _;
use crypto::{Bip44DerivationPath, HDPathToCoin};

#[cfg_attr(test, mockable)]
pub async fn make_gas_station_request(url: &str) -> GasStationResult {
    let resp = slurp_url(url).await.mm_err(Into::into)?;
    if resp.0 != StatusCode::OK {
        let error = format!("Gas price request failed with status code {}", resp.0);
        return MmError::err(GasStationReqErr::Transport {
            uri: url.to_owned(),
            error,
        });
    }
    let result: GasStationData = json::from_slice(&resp.2)?;
    Ok(result)
}

#[cfg_attr(test, mockable)]
impl EthCoinImpl {
    /// LP-17: returns a clone of the cached alloy
    /// [`super::alloy_compat::KdfProvider`]. The provider wraps an
    /// `Arc`-shared transport so cloning is cheap.
    pub(crate) fn alloy_provider(&self) -> super::alloy_compat::KdfProvider { self.web3.clone() }

    /// Reads the per-coin swap gas-fee policy currently in effect (CRD R35.6).
    pub fn swap_gas_fee_policy(&self) -> SwapGasFeePolicy { *self.swap_gas_fee_policy.lock().unwrap() }

    /// Sets the per-coin swap gas-fee policy (CRD R35.6).
    pub fn set_swap_gas_fee_policy(&self, policy: SwapGasFeePolicy) {
        *self.swap_gas_fee_policy.lock().unwrap() = policy;
    }

    /// Registers an ERC-20 token activated on top of this platform coin so the
    /// V2 activation result can later report its balance (CRD §35.1.3).
    pub fn add_erc20_token_info(&self, ticker: String, info: Erc20TokenInfo) {
        self.erc20_tokens_infos.lock().unwrap().insert(ticker, info);
    }

    /// Returns a snapshot of the ERC-20 tokens registered on this platform coin.
    pub fn get_erc20_tokens_infos(&self) -> std::collections::HashMap<String, Erc20TokenInfo> {
        self.erc20_tokens_infos.lock().unwrap().clone()
    }

    /// Returns this coin's own ERC-20 token info (contract address + decimals)
    /// when it is an ERC-20 token coin; `None` for the native gas coin.
    pub fn erc20_token_info(&self) -> Option<Erc20TokenInfo> {
        match &self.coin_type {
            EthCoinType::Erc20 { token_addr, .. } => Some(Erc20TokenInfo {
                token_addr: *token_addr,
                decimals: self.decimals,
            }),
            _ => None,
        }
    }

    /// Returns the wallet public key as a hex string (informative; used to
    /// populate the V2 activation result address-info records).
    pub fn display_public_key(&self) -> String { format!("0x{:02x}", self.signer.public()) }

    /// Signs `tx` with the local signing key for the offline-then-broadcast
    /// send path used by atomic swaps (`sign_and_send_transaction_impl`). For
    /// the `Local` policy behaviour is identical to the prior
    /// `tx.sign(key_pair.secret(), chain_id)`. Under the MetaMask policy this
    /// path is reached only by swap/HTLC sends and is rejected with
    /// [`EthSignerError::SwapSendUnsupported`] (CRD R47.5.12); the user-facing
    /// `withdraw` delegated-broadcast path does not use this entrypoint — it
    /// routes the final step to the wallet via `eth_sendTransaction` directly in
    /// `withdraw_impl` (CRD R47.5.6).
    pub(crate) fn sign_tx_for_send(&self, tx: UnSignedEthTx) -> Result<SignedEthTx, EthSignerError> {
        match &self.signer {
            EthSigner::Local(key_pair) => Ok(tx.sign(key_pair.secret(), self.chain_id)),
            #[cfg(target_arch = "wasm32")]
            EthSigner::Metamask(_) => Err(EthSignerError::SwapSendUnsupported),
            // CRD R50.24 / R47.5.13: a Trezor device driven only through the
            // interactive withdrawal task cannot satisfy the swap protocol's
            // framework-scheduled, non-interactive HTLC signing. Rejected like
            // MetaMask.
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
            EthSigner::Trezor(_) => Err(EthSignerError::SwapSendUnsupported),
        }
    }

    /// Produces an offline-signed raw transaction (no broadcast). Unavailable
    /// under the MetaMask policy (CRD R47.5.7 / R47.5.12): the wallet never
    /// yields a detached, re-broadcastable signed raw transaction. For the
    /// `Local` policy behaviour is identical to the prior offline signing.
    pub(crate) fn sign_raw_tx_offline(&self, tx: UnSignedEthTx) -> Result<SignedEthTx, EthSignerError> {
        match &self.signer {
            EthSigner::Local(key_pair) => Ok(tx.sign(key_pair.secret(), self.chain_id)),
            #[cfg(target_arch = "wasm32")]
            EthSigner::Metamask(_) => Err(EthSignerError::OfflineSigningUnsupported),
            // CRD R50.24 / R47.5.13: a Trezor device never yields a detached,
            // re-broadcastable signed raw transaction. Rejected like MetaMask.
            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
            EthSigner::Trezor(_) => Err(EthSignerError::OfflineSigningUnsupported),
        }
    }

    /// Gets Transfer events from ERC20 smart contract `addr` between `from_block` and `to_block`
    pub(crate) fn erc20_transfer_events(
        &self,
        contract: Address,
        from_addr: Option<Address>,
        to_addr: Option<Address>,
        from_block: BlockNumber,
        to_block: BlockNumber,
        limit: Option<usize>,
    ) -> Box<dyn Future<Item = Vec<Log>, Error = String> + Send> {
        let contract_event = try_fus!(ERC20_CONTRACT.event("Transfer"));
        let topic0 = Some(vec![contract_event.signature()]);
        let topic1 = from_addr.map(|addr| vec![addr.into()]);
        let topic2 = to_addr.map(|addr| vec![addr.into()]);
        let mut filter = FilterBuilder::default()
            .topics(topic0, topic1, topic2, None)
            .from_block(from_block)
            .to_block(to_block)
            .address(vec![contract]);

        if let Some(l) = limit {
            filter = filter.limit(l);
        }

        // LP-17: route the eth_getLogs request through alloy's
        // `RpcClient` while still deserializing into
        // `web3::types::Log`, so call sites and downstream parsing
        // stay bit-for-bit identical. Wire-level method unchanged.
        let provider = self.alloy_provider();
        let filter = filter.build();
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            assert_send_future(provider.client().request::<_, Vec<Log>>("eth_getLogs", (filter,)))
                .await
                .map_err(|e| ERRL!("{}", e))
        };
        Box::new(fut.boxed().compat())
    }

    /// Gets ETH traces from ETH node between addresses in `from_block` and `to_block`
    pub(crate) fn eth_traces(
        &self,
        from_addr: Vec<Address>,
        to_addr: Vec<Address>,
        from_block: BlockNumber,
        to_block: BlockNumber,
        limit: Option<usize>,
    ) -> Box<dyn Future<Item = Vec<Trace>, Error = String> + Send> {
        let mut filter = TraceFilterBuilder::default()
            .from_address(from_addr)
            .to_address(to_addr)
            .from_block(from_block)
            .to_block(to_block);

        if let Some(l) = limit {
            filter = filter.count(l);
        }

        // LP-17: parity-style trace_filter routed through the alloy
        // `RpcClient`. The wire-level method (`trace_filter`) and the
        // returned `Vec<web3::types::Trace>` shape are unchanged.
        let provider = self.alloy_provider();
        let filter = filter.build();
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            assert_send_future(provider.client().request::<_, Vec<Trace>>("trace_filter", (filter,)))
                .await
                .map_err(|e| ERRL!("{}", e))
        };
        Box::new(fut.boxed().compat())
    }

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn eth_traces_path(&self, ctx: &MmArc) -> PathBuf {
        ctx.dbdir()
            .join("TRANSACTIONS")
            .join(format!("{}_{:#02x}_trace.json", self.ticker, self.my_address))
    }

    /// Load saved ETH traces from local DB
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn load_saved_traces(&self, ctx: &MmArc) -> Option<SavedTraces> {
        let content = gstuff::slurp(&self.eth_traces_path(ctx));
        if content.is_empty() {
            None
        } else {
            match json::from_slice(&content) {
                Ok(t) => Some(t),
                Err(_) => None,
            }
        }
    }

    /// Load saved ETH traces from local DB
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn load_saved_traces(&self, _ctx: &MmArc) -> Option<SavedTraces> {
        common::panic_w("'load_saved_traces' is not implemented in WASM");
        unreachable!()
    }

    /// Store ETH traces to local DB
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn store_eth_traces(&self, ctx: &MmArc, traces: &SavedTraces) {
        let content = json::to_vec(traces).unwrap();
        let tmp_file = format!("{}.tmp", self.eth_traces_path(ctx).display());
        std::fs::write(&tmp_file, content).unwrap();
        std::fs::rename(tmp_file, self.eth_traces_path(ctx)).unwrap();
    }

    /// Store ETH traces to local DB
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn store_eth_traces(&self, _ctx: &MmArc, _traces: &SavedTraces) {
        common::panic_w("'store_eth_traces' is not implemented in WASM");
        unreachable!()
    }

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn erc20_events_path(&self, ctx: &MmArc) -> PathBuf {
        ctx.dbdir()
            .join("TRANSACTIONS")
            .join(format!("{}_{:#02x}_events.json", self.ticker, self.my_address))
    }

    /// Store ERC20 events to local DB
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn store_erc20_events(&self, ctx: &MmArc, events: &SavedErc20Events) {
        let content = json::to_vec(events).unwrap();
        let tmp_file = format!("{}.tmp", self.erc20_events_path(ctx).display());
        std::fs::write(&tmp_file, content).unwrap();
        std::fs::rename(tmp_file, self.erc20_events_path(ctx)).unwrap();
    }

    /// Store ERC20 events to local DB
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn store_erc20_events(&self, _ctx: &MmArc, _events: &SavedErc20Events) {
        common::panic_w("'store_erc20_events' is not implemented in WASM");
        unreachable!()
    }

    /// Load saved ERC20 events from local DB
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn load_saved_erc20_events(&self, ctx: &MmArc) -> Option<SavedErc20Events> {
        let content = gstuff::slurp(&self.erc20_events_path(ctx));
        if content.is_empty() {
            None
        } else {
            match json::from_slice(&content) {
                Ok(t) => Some(t),
                Err(_) => None,
            }
        }
    }

    /// Load saved ERC20 events from local DB
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn load_saved_erc20_events(&self, _ctx: &MmArc) -> Option<SavedErc20Events> {
        common::panic_w("'load_saved_erc20_events' is not implemented in WASM");
        unreachable!()
    }

    /// The id used to differentiate payments on Etomic swap smart contract
    pub(crate) fn etomic_swap_id(&self, time_lock: u32, secret_hash: &[u8]) -> Vec<u8> {
        let mut input = vec![];
        input.extend_from_slice(&time_lock.to_le_bytes());
        input.extend_from_slice(secret_hash);
        sha256(&input).to_vec()
    }

    /// The id used to differentiate payments on Etomic swap V2 smart contracts.
    /// Uses u64 timelock (vs u32 for V1).
    pub(crate) fn etomic_swap_id_v2(&self, time_lock: u64, secret_hash: &[u8]) -> Vec<u8> {
        let mut input = Vec::with_capacity(8 + secret_hash.len());
        input.extend_from_slice(&time_lock.to_le_bytes());
        input.extend_from_slice(secret_hash);
        sha256(&input).to_vec()
    }

    /// The EVM chain id this coin was activated on, when configured.
    pub fn chain_id(&self) -> Option<u64> { self.chain_id }

    /// Returns the token address for contract calls.
    /// ETH → zero address, ERC20 → token contract address.
    pub fn get_token_address(&self) -> Result<Address, String> {
        match &self.coin_type {
            EthCoinType::Eth => Ok(Address::default()),
            EthCoinType::Erc20 { token_addr, .. } => Ok(*token_addr),
            // Native TRX has no token contract; return zero like ETH.
            EthCoinType::Tron => Ok(Address::default()),
            EthCoinType::Trc20 { token_addr, .. } => Ok(*token_addr),
        }
    }

    pub(crate) fn estimate_gas(&self, req: CallRequest) -> Web3RpcFut<U256> {
        // LP-17: alloy raw RPC replaces `web3.eth().estimate_gas`.
        // Always pass a single argument as old Geth versions reject
        // the optional block tag. Errors are mapped to
        // `Web3RpcError::Transport(_)` (was `web3::Error::Transport`
        // before LP-17 Phase 4); the existing `From<Web3RpcError>`
        // conversions on `WithdrawError`/`TradePreimageError` keep
        // call-site `?` propagation working.
        let provider = self.alloy_provider();
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            assert_send_future(provider.client().request::<_, U256>("eth_estimateGas", (req,)))
                .await
                .map_to_mm(|e| Web3RpcError::Transport(e.to_string()))
        };
        Box::new(fut.boxed().compat())
    }

    /// Gets `ReceiverSpent` events from etomic swap smart contract since `from_block`
    pub(crate) fn spend_events(
        &self,
        swap_contract_address: Address,
        from_block: u64,
        to_block: u64,
    ) -> Box<dyn Future<Item = Vec<Log>, Error = String> + Send> {
        let contract_event = try_fus!(SWAP_CONTRACT.event("ReceiverSpent"));
        let filter = FilterBuilder::default()
            .topics(Some(vec![contract_event.signature()]), None, None, None)
            .from_block(BlockNumber::Number(from_block))
            .to_block(BlockNumber::Number(to_block))
            .address(vec![swap_contract_address])
            .build();

        // LP-17: route eth_getLogs through alloy's RpcClient while
        // keeping the returned `Vec<web3::types::Log>` shape.
        let provider = self.alloy_provider();
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            assert_send_future(provider.client().request::<_, Vec<Log>>("eth_getLogs", (filter,)))
                .await
                .map_err(|e| ERRL!("{}", e))
        };
        Box::new(fut.boxed().compat())
    }

    /// Gets `SenderRefunded` events from etomic swap smart contract since `from_block`
    pub(crate) fn refund_events(
        &self,
        swap_contract_address: Address,
        from_block: u64,
        to_block: u64,
    ) -> Box<dyn Future<Item = Vec<Log>, Error = String> + Send> {
        let contract_event = try_fus!(SWAP_CONTRACT.event("SenderRefunded"));
        let filter = FilterBuilder::default()
            .topics(Some(vec![contract_event.signature()]), None, None, None)
            .from_block(BlockNumber::Number(from_block))
            .to_block(BlockNumber::Number(to_block))
            .address(vec![swap_contract_address])
            .build();

        // LP-17: route eth_getLogs through alloy's RpcClient while
        // keeping the returned `Vec<web3::types::Log>` shape.
        let provider = self.alloy_provider();
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            assert_send_future(provider.client().request::<_, Vec<Log>>("eth_getLogs", (filter,)))
                .await
                .map_err(|e| ERRL!("{}", e))
        };
        Box::new(fut.boxed().compat())
    }

    /// Try to parse address from string.
    pub fn address_from_str(&self, address: &str) -> Result<Address, String> {
        Ok(try_s!(valid_addr_from_str(address)))
    }

    pub(crate) fn call_request_from(
        &self,
        from: Address,
        to: Address,
        value: Option<U256>,
        data: Option<Bytes>,
    ) -> Web3RpcFut<Bytes> {
        let request = CallRequest {
            from: Some(from),
            to,
            gas: None,
            gas_price: None,
            value,
            data,
        };

        let provider = self.alloy_provider();
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            assert_send_future(
                provider
                    .client()
                    .request::<_, Bytes>("eth_call", (request, BlockNumber::Latest)),
            )
            .await
            .map_err(|e| MmError::new(Web3RpcError::Transport(e.to_string())))
        };
        Box::new(fut.boxed().compat())
    }
}

pub async fn get_raw_transaction_impl(coin: EthCoin, req: RawTransactionRequest) -> RawTransactionResult {
    // LP-17: alloy `Provider::get_transaction_by_hash` replaces
    // `web3.eth().transaction(...)`. The fetched alloy `Transaction`
    // is round-tripped through `signed_tx_from_alloy_tx` to keep the
    // legacy `SignedEthTx` shape (and therefore the same RLP
    // encoding in `tx_hex`) bit-for-bit unchanged.
    use crate::eth::alloy_compat::assert_send_future;
    use alloy::providers::Provider;

    let tx = match req.tx_hash.strip_prefix("0x") {
        Some(tx) => tx,
        None => &req.tx_hash,
    };
    let hash = H256::from_str(tx).map_to_mm(|e| RawTransactionError::InvalidHashError(e.to_string()))?;
    let provider = coin.alloy_provider();
    let alloy_hash = alloy::primitives::B256::from_slice(&hash.0);
    let alloy_tx = assert_send_future(provider.get_transaction_by_hash(alloy_hash))
        .await
        .map_err(|e| RawTransactionError::Transport(e.to_string()))?;
    let alloy_tx = alloy_tx.or_mm_err(|| RawTransactionError::HashNotExist(req.tx_hash))?;
    let raw = signed_tx_from_alloy_tx(alloy_tx).map_to_mm(RawTransactionError::InternalError)?;
    Ok(RawTransactionRes {
        tx_hex: BytesJson(rlp::encode(&raw)),
    })
}

pub(crate) fn validate_evm_withdraw_request(coin: &EthCoin, _req: &WithdrawRequest) -> MmResult<(), WithdrawError> {
    if !matches!(coin.coin_type, EthCoinType::Eth | EthCoinType::Erc20 { .. }) {
        return MmError::err(WithdrawError::CoinDoesntSupportInitWithdraw {
            coin: coin.ticker.clone(),
        });
    }

    Ok(())
}

pub(crate) struct EvmWithdrawSender {
    address: Address,
    key_pair: Option<KeyPair>,
}

impl EvmWithdrawSender {
    pub(crate) fn address(&self) -> Address { self.address }

    fn checksum_address(&self) -> String { checksum_address(&format!("{:#02x}", self.address)) }

    fn sign_tx(&self, coin: &EthCoin, tx: UnSignedEthTx) -> Result<SignedEthTx, EthSignerError> {
        match self.key_pair {
            Some(ref key_pair) => Ok(tx.sign(key_pair.secret(), coin.chain_id)),
            None => coin.sign_tx_for_send(tx),
        }
    }
}

pub(crate) async fn resolve_evm_withdraw_sender(
    ctx: &MmArc,
    coin: &EthCoin,
    req: &WithdrawRequest,
) -> MmResult<EvmWithdrawSender, WithdrawError> {
    let from = match req.from.clone() {
        Some(from) => from,
        None => {
            return Ok(EvmWithdrawSender {
                address: coin.my_address,
                key_pair: None,
            })
        },
    };

    let hd_wallet = match coin.derivation_method() {
        DerivationMethod::Iguana(_) => {
            let error = "'from' is not supported if the EVM coin is initialized with a single private key";
            return MmError::err(WithdrawError::UnexpectedFromAddress(error.to_owned()));
        },
        DerivationMethod::HDWallet(hd_wallet) => hd_wallet,
    };

    let crate::HDAddressId {
        account_id,
        chain,
        address_id,
    } = match from {
        crate::WithdrawFrom::AddressId(id) => id,
        crate::WithdrawFrom::DerivationPath { derivation_path } => {
            let derivation_path = Bip44DerivationPath::from_str(&derivation_path)
                .map_to_mm(|e| WithdrawError::UnexpectedFromAddress(format!("{:?}", e)))?;
            let coin_type = derivation_path.coin_type();
            let expected_coin_type = hd_wallet.coin_type();
            if coin_type != expected_coin_type {
                let error = format!(
                    "Derivation path '{}' must have '{}' coin type",
                    derivation_path, expected_coin_type
                );
                return MmError::err(WithdrawError::UnexpectedFromAddress(error));
            }
            crate::HDAddressId::from(derivation_path)
        },
    };

    let hd_account = hd_wallet
        .get_account(account_id)
        .await
        .or_mm_err(|| WithdrawError::UnknownAccount { account_id })?;
    let is_address_activated = hd_account
        .is_address_activated(chain, address_id)
        .mm_err(|e| WithdrawError::UnexpectedFromAddress(e.to_string()))?;
    let hd_address = coin
        .derive_address(&hd_account, chain, address_id)
        .mm_err(|e| WithdrawError::UnexpectedFromAddress(e.to_string()))?;
    if !is_address_activated {
        let error = format!("'{}' address is not activated", hd_address.address);
        return MmError::err(WithdrawError::UnexpectedFromAddress(error));
    }

    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| WithdrawError::InternalError(e.to_string()))?;
    let global_hd = match crypto_ctx.key_pair_policy() {
        KeyPairPolicy::GlobalHDAccount(global_hd) => global_hd,
        KeyPairPolicy::Iguana => {
            let error = "EVM withdrawal with explicit 'from' requires a software-HD wallet";
            return MmError::err(WithdrawError::UnexpectedFromAddress(error.to_owned()));
        },
    };
    let secret = global_hd
        .derive_secp256k1_secret(&hd_address.derivation_path)
        .mm_err(|e| WithdrawError::InternalError(e.to_string()))?;
    let key_pair =
        KeyPair::from_secret_slice(secret.as_slice()).map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
    if key_pair.address() != hd_address.address {
        let error = format!(
            "Derived signer address '{:#02x}' does not match selected HD address '{:#02x}'",
            key_pair.address(),
            hd_address.address
        );
        return MmError::err(WithdrawError::InternalError(error));
    }

    Ok(EvmWithdrawSender {
        address: hd_address.address,
        key_pair: Some(key_pair),
    })
}

#[cfg_attr(test, mockable)]
pub(crate) fn evm_withdraw_balance(coin: EthCoin, sender: Address) -> BalanceFut<U256> {
    let fut = async move {
        use crate::eth::alloy_compat::assert_send_future;
        match coin.coin_type {
            EthCoinType::Eth => assert_send_future(
                coin.web3
                    .client()
                    .request::<_, U256>("eth_getBalance", (sender, BlockNumber::Latest)),
            )
            .await
            .map_err(|e| MmError::new(BalanceError::Transport(e.to_string()))),
            EthCoinType::Erc20 { ref token_addr, .. } => {
                let function = ERC20_CONTRACT.function("balanceOf")?;
                let data = function.encode_input(&[Token::Address(sender)])?;

                let res = coin
                    .call_request_from(sender, *token_addr, None, Some(data.into()))
                    .compat()
                    .await
                    .mm_err(BalanceError::from)?;
                let decoded = function.decode_output(&res.0)?;
                match decoded[0] {
                    Token::Uint(number) => Ok(number),
                    _ => {
                        let error = format!("Expected U256 as balanceOf result but got {:?}", decoded);
                        MmError::err(BalanceError::InvalidResponse(error))
                    },
                }
            },
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => MmError::err(BalanceError::Internal(
                "TRON balance lookup not wired through EVM withdraw".to_owned(),
            )),
        }
    };
    Box::new(fut.boxed().compat())
}

/// The unsigned EVM withdrawal transaction plus the derived metadata every
/// signing policy needs to assemble the completed `TransactionDetails` (CRD
/// R49.25 / R50.22). Produced by [`build_evm_withdraw_plan`] and consumed by
/// both the software / MetaMask path (`withdraw_impl`) and the Trezor
/// device-signing path (`withdraw_trezor_impl`).
pub(crate) struct EvmWithdrawPlan {
    /// Unsigned legacy EIP-155 transaction ready to be signed for `chain_id`.
    pub(crate) unsigned: UnSignedEthTx,
    /// Recipient address parsed from `req.to` (used for the `to` field and the
    /// self-send `received_by_me` check).
    pub(crate) to_addr: Address,
    /// The transaction's action target: `to_addr` for a native send, or the
    /// token contract for an ERC20 transfer.
    pub(crate) call_addr: Address,
    /// Withdrawn amount in the coin's smallest unit (post max/fee adjustment).
    pub(crate) wei_amount: U256,
    pub(crate) gas: U256,
    pub(crate) gas_price: U256,
    /// Ticker the fee is denominated in (the platform coin for ERC20 tokens).
    pub(crate) fee_coin: String,
}

/// Shared unsigned-transaction construction for an EVM withdrawal, reused by the
/// software / MetaMask path and the Trezor device-signing path. Given the
/// already-resolved `sender_address`, it validates and resolves the recipient,
/// checks the balance, applies max / amount and fee handling, resolves the gas
/// and gas price, holds the shared nonce lock while fetching the nonce, and
/// builds the unsigned legacy EIP-155 transaction. The returned nonce-lock guard
/// must be held by the caller through signing so a concurrent withdrawal cannot
/// reuse the selected nonce.
pub(crate) async fn build_evm_withdraw_plan(
    ctx: &MmArc,
    coin: &EthCoin,
    req: &WithdrawRequest,
    sender_address: Address,
) -> MmResult<(EvmWithdrawPlan, common::custom_futures::TimedMutexGuard<'static, ()>), WithdrawError> {
    let to_addr = coin
        .address_from_str(&req.to)
        .map_to_mm(WithdrawError::InvalidAddress)?;
    let my_balance = evm_withdraw_balance(coin.clone(), sender_address)
        .compat()
        .await
        .mm_err(Into::into)?;
    let my_balance_dec = u256_to_big_decimal(my_balance, coin.decimals).mm_err(Into::into)?;

    let (mut wei_amount, dec_amount) = if req.max {
        (my_balance, my_balance_dec.clone())
    } else {
        let wei_amount = wei_from_big_decimal(&req.amount, coin.decimals).mm_err(Into::into)?;
        (wei_amount, req.amount.clone())
    };
    if wei_amount > my_balance {
        return MmError::err(WithdrawError::NotSufficientBalance {
            coin: coin.ticker.clone(),
            available: my_balance_dec.clone(),
            required: dec_amount,
        });
    };
    let (mut eth_value, data, call_addr, fee_coin) = match &coin.coin_type {
        EthCoinType::Eth => (wei_amount, vec![], to_addr, coin.ticker().to_owned()),
        EthCoinType::Erc20 { platform, token_addr } => {
            let function = ERC20_CONTRACT.function("transfer")?;
            let data = function.encode_input(&[Token::Address(to_addr), Token::Uint(wei_amount)])?;
            (0.into(), data, *token_addr, platform.clone())
        },
        // TRON is diverted to the dedicated pipeline before this helper is
        // reached; this arm is unreachable but kept for exhaustiveness.
        EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
            return MmError::err(WithdrawError::InternalError(
                "TRON withdraw must route through tron::withdraw::withdraw_tron".to_owned(),
            ))
        },
    };
    let eth_value_dec = u256_to_big_decimal(eth_value, coin.decimals).mm_err(Into::into)?;

    let (gas, gas_price) = match &req.fee {
        Some(WithdrawFee::EthGas { gas_price, gas }) => {
            let gas_price = wei_from_big_decimal(gas_price, 9).mm_err(Into::into)?;
            (U256::from(*gas), gas_price)
        },
        Some(fee_policy) => {
            let error = format!("Expected 'EthGas' fee type, found {:?}", fee_policy);
            return MmError::err(WithdrawError::InvalidFeePolicy(error));
        },
        None => {
            let gas_price = coin.get_gas_price().compat().await.mm_err(Into::into)?;
            // covering edge case by deducting the standard transfer fee when we want to max withdraw ETH
            let eth_value_for_estimate = if req.max && coin.coin_type == EthCoinType::Eth {
                eth_value - gas_price * U256::from(21000)
            } else {
                eth_value
            };
            let estimate_gas_req = CallRequest {
                value: Some(eth_value_for_estimate),
                data: Some(data.clone().into()),
                from: Some(sender_address),
                to: call_addr,
                gas: None,
                // gas price must be supplied because some smart contracts base their
                // logic on gas price, e.g. TUSD: https://github.com/KomodoPlatform/atomicDEX-API/issues/643
                gas_price: Some(gas_price),
            };
            // TODO Note if the wallet's balance is insufficient to withdraw, then `estimate_gas` may fail with the `Exception` error.
            // TODO Ideally we should determine the case when we have the insufficient balance and return `WithdrawError::NotSufficientBalance`.
            let gas_limit = coin
                .estimate_gas(estimate_gas_req)
                .compat()
                .await
                .mm_err(WithdrawError::from)?;
            (gas_limit, gas_price)
        },
    };
    let total_fee = gas * gas_price;
    let total_fee_dec = u256_to_big_decimal(total_fee, coin.decimals).mm_err(Into::into)?;

    if req.max && coin.coin_type == EthCoinType::Eth {
        if eth_value < total_fee || wei_amount < total_fee {
            return MmError::err(WithdrawError::AmountTooLow {
                amount: eth_value_dec,
                threshold: total_fee_dec,
            });
        }
        eth_value -= total_fee;
        wei_amount -= total_fee;
    };
    let nonce_lock = NONCE_LOCK
        .lock(|_start, _now| {
            if ctx.is_stopping() {
                let error = "MM is stopping, aborting withdraw in NONCE_LOCK".to_owned();
                return MmError::err(WithdrawError::InternalError(error));
            }
            Ok(0.5)
        })
        .await?;
    let nonce_fut = get_addr_nonce(sender_address, coin.web3_instances.clone()).compat();
    let nonce = match select(nonce_fut, Timer::sleep(30.)).await {
        Either::Left((nonce_res, _)) => nonce_res.map_to_mm(WithdrawError::Transport)?,
        Either::Right(_) => return MmError::err(WithdrawError::Transport("Get address nonce timed out".to_owned())),
    };
    let unsigned = UnSignedEthTx {
        nonce,
        value: eth_value,
        action: Action::Call(call_addr),
        data,
        gas,
        gas_price,
    };

    Ok((
        EvmWithdrawPlan {
            unsigned,
            to_addr,
            call_addr,
            wei_amount,
            gas,
            gas_price,
            fee_coin,
        },
        nonce_lock,
    ))
}

/// Assemble the completed `TransactionDetails` from a withdrawal plan and the
/// final signed-transaction bytes / hash. Shared by the software path and the
/// Trezor path so both emit the identical completed-payload field set (CRD
/// R49.25 / R50.22 / R50.23).
pub(crate) fn build_evm_withdraw_details(
    coin: &EthCoin,
    plan: &EvmWithdrawPlan,
    from_checksum: String,
    from_addr: Address,
    tx_hex: BytesJson,
    tx_hash: String,
) -> MmResult<TransactionDetails, WithdrawError> {
    let amount_decimal = u256_to_big_decimal(plan.wei_amount, coin.decimals).mm_err(Into::into)?;
    let mut spent_by_me = amount_decimal.clone();
    let received_by_me = if plan.to_addr == from_addr {
        amount_decimal.clone()
    } else {
        0.into()
    };
    let fee_details = EthTxFeeDetails::new(plan.gas, plan.gas_price, &plan.fee_coin).mm_err(Into::into)?;
    if coin.coin_type == EthCoinType::Eth {
        spent_by_me += &fee_details.total_fee;
    }
    Ok(TransactionDetails {
        to: vec![checksum_address(&format!("{:#02x}", plan.to_addr))],
        from: vec![from_checksum],
        total_amount: amount_decimal,
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        tx_hex,
        tx_hash,
        block_height: 0,
        fee_details: Some(fee_details.into()),
        coin: coin.ticker.clone(),
        internal_id: vec![].into(),
        timestamp: now_ms() / 1000,
        kmd_rewards: None,
        transaction_type: Default::default(),
    })
}

pub async fn withdraw_impl(ctx: MmArc, coin: EthCoin, req: WithdrawRequest) -> WithdrawResult {
    // TRON uses a dedicated pipeline: its transaction format, address
    // encoding, signing digest and fee model all differ from the EVM flow.
    if matches!(coin.coin_type, EthCoinType::Tron | EthCoinType::Trc20 { .. }) {
        // CRD R47.5.6a: the non-EVM-keypair TRON family is rejected under the
        // MetaMask signing policy as an unsupported withdraw. The delegated
        // sign-and-broadcast model (`eth_sendTransaction`, R47.5.6) is EVM-only
        // and cannot drive TRON's distinct transaction format/signing digest,
        // and the framework holds no local TRON secret under MetaMask. (TRON is
        // not activated under the MetaMask policy in practice, so this is a
        // defensive early rejection rather than a reachable runtime path.)
        #[cfg(target_arch = "wasm32")]
        if matches!(coin.signer, EthSigner::Metamask(_)) {
            return MmError::err(WithdrawError::UnsupportedUnderMetamask(
                "TRON withdraw is not supported under the MetaMask signing policy".to_owned(),
            ));
        }
        let _ = ctx;
        return crate::eth::tron::withdraw::withdraw_tron(coin, req).await;
    }

    validate_evm_withdraw_request(&coin, &req)?;
    let sender = resolve_evm_withdraw_sender(&ctx, &coin, &req).await?;
    let (plan, _nonce_lock) = build_evm_withdraw_plan(&ctx, &coin, &req, sender.address).await?;

    // CRD R47.5.6/R47.5.7 -- delegated sign-and-broadcast under the MetaMask
    // policy. Unlike the Local (Iguana) path below -- which signs `tx` offline
    // and returns re-broadcastable raw bytes for a later `send_raw_transaction`
    // -- under MetaMask the framework holds no key: it hands the unsigned
    // EIP-1193 transaction object to the wallet, which SIGNS AND BROADCASTS it
    // immediately and returns only a transaction hash. There is therefore no
    // separate broadcast step and NO detached, re-broadcastable raw signed
    // bytes exist; `tx_hex` is deliberately left empty (the framework must not
    // fabricate signed bytes).
    #[cfg(target_arch = "wasm32")]
    if let EthSigner::Metamask(metamask_arc) = &coin.signer {
        // CRD R47.5.9 -- re-read the wallet's currently-active account and verify
        // it still matches the activated account BEFORE any signing/broadcast
        // request, so the wrong account is never asked to sign.
        metamask_arc
            .check_active_eth_account()
            .await
            .mm_err(|e| WithdrawError::Transport(e.to_string()))?;

        // CRD R47.5.10 -- ensure the wallet's active chain matches the coin's
        // EIP-155 chain (requesting a switch otherwise) before broadcasting.
        if let Some(coin_chain_id) = coin.chain_id {
            metamask_arc
                .ensure_active_chain(coin_chain_id)
                .await
                .mm_err(|e| WithdrawError::Transport(e.to_string()))?;
        }

        // CRD R47.5.8 -- build the EIP-1193 transaction object. `from` is the
        // activated (connected) account; `to`/`value`/`gas`/`gasPrice` reuse the
        // exact fields the local path computed above. The wallet owns the nonce
        // for delegated broadcast, so `nonce` is intentionally omitted (the
        // framework's computed `tx.nonce` is consumed only by the local path).
        let mut tx_object = serde_json::json!({
            "from": format!("{:#x}", sender.address),
            "to": format!("{:#x}", plan.call_addr),
            "value": format!("{:#x}", plan.unsigned.value),
            "gas": format!("{:#x}", plan.gas),
            "gasPrice": format!("{:#x}", plan.gas_price),
        });
        if !plan.unsigned.data.is_empty() {
            tx_object["data"] = serde_json::json!(format!("0x{}", hex::encode(&plan.unsigned.data)));
        }

        let tx_hash = metamask_arc
            .eth_send_transaction(tx_object)
            .await
            .mm_err(|e| WithdrawError::Transport(e.to_string()))?;

        // CRD R47.5.6a -- best-effort `tx_hex`. The wallet already broadcast the
        // transaction (R47.5.6), so no detached, re-broadcastable raw signed
        // bytes were produced by the framework. To still populate `tx_hex` when
        // possible, poll the node for a bounded window for the broadcast tx to
        // appear; if found, round-trip the returned web3/alloy `Transaction`
        // through `signed_tx_from_alloy_tx` (the same helper `get_raw_transaction`
        // uses, preserving the legacy `SignedEthTx` RLP shape) and rlp-encode it.
        // If the tx has not appeared before the deadline, leave `tx_hex` empty
        // (current behaviour) -- withdraw does NOT fail on a poll timeout since
        // the broadcast already succeeded.
        let tx_hex: BytesJson = {
            use crate::eth::alloy_compat::assert_send_future;
            use alloy::providers::Provider;

            let mut found = BytesJson::from(Vec::new());
            if let Ok(hash) = H256::from_str(tx_hash.trim_start_matches("0x")) {
                let alloy_hash = alloy::primitives::B256::from_slice(&hash.0);
                let provider = coin.alloy_provider();
                // ~12s overall best-effort window; each attempt is capped so a
                // hung lookup cannot stall past the deadline.
                let deadline_ms = now_ms() + 12_000;
                while now_ms() < deadline_ms {
                    let lookup = Box::pin(assert_send_future(provider.get_transaction_by_hash(alloy_hash)));
                    match select(lookup, Timer::sleep(3.)).await {
                        Either::Left((Ok(Some(alloy_tx)), _)) => {
                            if let Ok(signed) = signed_tx_from_alloy_tx(alloy_tx) {
                                found = BytesJson(rlp::encode(&signed).to_vec());
                            }
                            break;
                        },
                        // Not yet mined / not yet visible / transient transport
                        // error: brief backoff, then retry until the deadline.
                        Either::Left((_, _)) => Timer::sleep(1.).await,
                        // Per-attempt timeout: loop re-checks the overall deadline.
                        Either::Right(_) => {},
                    }
                }
            }
            found
        };

        // CRD R47.5.6/R47.5.6a/R47.5.7: already broadcast by the wallet, so no
        // detached re-broadcastable bytes exist; `tx_hex` is filled best-effort
        // from the node above and is empty if the tx had not yet appeared within
        // the poll window.
        return build_evm_withdraw_details(
            &coin,
            &plan,
            sender.checksum_address(),
            sender.address,
            tx_hex,
            tx_hash.trim_start_matches("0x").to_lowercase(),
        );
    }

    let signed = sender
        .sign_tx(&coin, plan.unsigned.clone())
        .map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
    let bytes = rlp::encode(&signed);
    build_evm_withdraw_details(
        &coin,
        &plan,
        sender.checksum_address(),
        sender.address,
        bytes.into(),
        format!("{:02x}", signed.tx_hash()),
    )
}

pub async fn sign_raw_eth_tx_impl(coin: EthCoin, args: SignRawTransactionRequest) -> RawTransactionResult {
    let eth_args = match &args.tx {
        SignRawTransactionEnum::ETH(params) => params,
        _ => return MmError::err(RawTransactionError::InvalidParam("ETH type expected".to_string())),
    };

    let value = wei_from_big_decimal(eth_args.value.as_ref().unwrap_or(&BigDecimal::from(0)), coin.decimals)
        .mm_err(|e| RawTransactionError::InvalidParam(e.to_string()))?;

    let action = if let Some(to) = &eth_args.to {
        Action::Call(Address::from_str(to).map_to_mm(|e| RawTransactionError::InvalidParam(e.to_string()))?)
    } else {
        Action::Create
    };

    let data = hex::decode(eth_args.data.as_deref().unwrap_or(""))
        .map_to_mm(|e| RawTransactionError::DecodeError(e.to_string()))?;

    let gas_price =
        wei_from_big_decimal(&eth_args.gas_price, 9).mm_err(|e| RawTransactionError::InvalidParam(e.to_string()))?;
    let gas_limit = U256::from(eth_args.gas_limit);

    let nonce_fut = get_addr_nonce(coin.my_address, coin.web3_instances.clone()).compat();
    let nonce = match select(nonce_fut, Timer::sleep(30.)).await {
        Either::Left((Ok(n), _)) => n,
        Either::Left((Err(e), _)) => return MmError::err(RawTransactionError::Transport(e)),
        Either::Right(_) => {
            return MmError::err(RawTransactionError::Transport(
                "Get address nonce timed out".to_string(),
            ))
        },
    };

    let tx = UnSignedEthTx {
        nonce,
        value,
        action,
        data,
        gas: gas_limit,
        gas_price,
    };

    let signed = coin
        .sign_raw_tx_offline(tx)
        .map_to_mm(|e| RawTransactionError::SigningError(e.to_string()))?;
    let bytes = rlp::encode(&signed);

    Ok(RawTransactionRes {
        tx_hex: BytesJson::from(bytes.to_vec()),
    })
}

pub fn signed_eth_tx_from_bytes(bytes: &[u8]) -> Result<SignedEthTx, String> {
    let tx: UnverifiedTransaction = try_s!(rlp::decode(bytes));
    let signed = try_s!(SignedEthTx::new(tx));
    Ok(signed)
}

// We can use a shared nonce lock for all ETH coins.
// It's highly likely that we won't experience any issues with it as we won't need to send "a lot" of transactions concurrently.
// For ETH it makes even more sense because different ERC20 tokens can be running on same ETH blockchain.
// So we would need to handle shared locks anyway.
lazy_static! {
    pub(crate) static ref NONCE_LOCK: TimedAsyncMutex<()> = TimedAsyncMutex::new(());
}

pub type EthTxFut = Box<dyn Future<Item = SignedEthTx, Error = TransactionErr> + Send + 'static>;

pub async fn sign_and_send_transaction_impl(
    ctx: MmArc,
    coin: EthCoin,
    value: U256,
    action: Action,
    data: Vec<u8>,
    gas: U256,
) -> Result<SignedEthTx, TransactionErr> {
    let mut status = ctx.log.status_handle();
    macro_rules! tags {
        () => {
            &[&"sign-and-send"]
        };
    }
    let _nonce_lock = NONCE_LOCK
        .lock(|start, now| {
            if ctx.is_stopping() {
                return ERR!("MM is stopping, aborting sign_and_send_transaction_impl in NONCE_LOCK");
            }
            if start < now {
                status.status(tags!(), "Waiting for NONCE_LOCK…")
            }
            Ok(0.5)
        })
        .await;
    status.status(tags!(), "get_addr_nonce…");
    let nonce = try_tx_s!(
        get_addr_nonce(coin.my_address, coin.web3_instances.clone())
            .compat()
            .await
    );
    status.status(tags!(), "get_gas_price…");
    let gas_price = try_tx_s!(coin.get_gas_price().compat().await);
    let tx = UnSignedEthTx {
        nonce,
        gas_price,
        gas,
        action,
        value,
        data,
    };
    let signed = try_tx_s!(coin.sign_tx_for_send(tx));
    let bytes = rlp::encode(&signed).to_vec();
    status.status(tags!(), "send_raw_transaction…");

    // LP-17: alloy `Provider::send_raw_transaction` replaces
    // `web3.eth().send_raw_transaction`. Wire-level method
    // (`eth_sendRawTransaction`) and the broadcast bytes are
    // identical.
    {
        use crate::eth::alloy_compat::assert_send_future;
        let provider = coin.alloy_provider();
        // Discard the returned `PendingTransactionBuilder`: the
        // legacy `web3.eth().send_raw_transaction` call only
        // confirmed broadcast acceptance; inclusion polling happens
        // separately in the `get_addr_nonce` loop below.
        try_tx_s!(
            assert_send_future(async move { provider.send_raw_transaction(&bytes).await.map(|_pending| ()) })
                .await
                .map_err(|e| ERRL!("{}", e)),
            signed
        );
    }

    status.status(tags!(), "get_addr_nonce…");
    loop {
        // Check every second till ETH nodes recognize that nonce is increased
        // Parity has reliable "nextNonce" method that always returns correct nonce for address
        // But we can't expect that all nodes will always be Parity.
        // Some of ETH forks use Geth only so they don't have Parity nodes at all.
        let new_nonce = match get_addr_nonce(coin.my_address, coin.web3_instances.clone())
            .compat()
            .await
        {
            Ok(n) => n,
            Err(e) => {
                log!("Error " [e] " getting " [coin.ticker()] " " [coin.my_address] " nonce");
                // we can just keep looping in case of error hoping it will go away
                continue;
            },
        };
        if new_nonce > nonce {
            break;
        };
        Timer::sleep(1.).await;
    }
    Ok(signed)
}

impl EthCoin {
    /// Queries the on-chain balances of every ERC-20 token registered on this
    /// platform coin (via `balanceOf`), keyed by token ticker (CRD §35.1.3).
    pub async fn get_tokens_balance_list(
        &self,
    ) -> Result<std::collections::HashMap<String, CoinBalance>, MmError<BalanceError>> {
        let infos = self.get_erc20_tokens_infos();
        let mut result = std::collections::HashMap::new();
        for (ticker, info) in infos {
            let function = ERC20_CONTRACT
                .function("balanceOf")
                .map_to_mm(|e| BalanceError::Internal(e.to_string()))?;
            let data = function
                .encode_input(&[Token::Address(self.my_address)])
                .map_to_mm(|e| BalanceError::Internal(e.to_string()))?;
            let res = self
                .call_request(info.token_addr, None, Some(data.into()))
                .compat()
                .await
                .mm_err(BalanceError::from)?;
            let decoded = function
                .decode_output(&res.0)
                .map_to_mm(|e| BalanceError::Internal(e.to_string()))?;
            let wei: U256 = match decoded.into_iter().next() {
                Some(Token::Uint(number)) => number,
                other => {
                    return MmError::err(BalanceError::InvalidResponse(format!(
                        "Expected U256 as balanceOf result but got {:?}",
                        other
                    )))
                },
            };
            let spendable =
                u256_to_big_decimal(wei, info.decimals).mm_err(|e| BalanceError::Internal(e.to_string()))?;
            result.insert(ticker, CoinBalance {
                spendable,
                unspendable: BigDecimal::from(0),
            });
        }
        Ok(result)
    }

    /// Runs the legacy ETH transaction-history loop using this coin's own
    /// (weakly held) MM context. Used by the V2 platform activation path to
    /// start background history fetching when `tx_history` is requested.
    pub async fn process_eth_history_loop(&self) {
        if let Some(ctx) = MmArc::from_weak(&self.ctx) {
            let _ = self.process_history_loop(ctx).compat().await;
        }
    }

    /// Builds an ERC-20 token `EthCoin` that shares this platform coin's transport
    /// (web3 instances, key pair, address and swap contracts) but carries the token's
    /// own ticker, contract address, decimals and confirmation requirement.
    ///
    /// Per CRD §35.2.2 the token decimals are taken from the coin configuration; the
    /// contract is not introspected. Returns an error if the configuration does not
    /// declare a valid `decimals` value.
    pub fn erc20_token_from_conf(
        &self,
        ticker: String,
        token_addr: Address,
        required_confirmations: u64,
    ) -> Result<EthCoin, String> {
        let ctx = MmArc::from_weak(&self.ctx).ok_or_else(|| "MM context has been dropped".to_string())?;
        let conf = crate::coin_conf(&ctx, &ticker);
        let decimals = match conf["decimals"].as_u64() {
            Some(d) if d > 0 && d <= 19 => d as u8,
            _ => {
                return Err(format!(
                    "Token {} decimals must be declared in its coin configuration",
                    ticker
                ))
            },
        };
        let token_impl = EthCoinImpl {
            signer: self.signer.clone(),
            my_address: self.my_address,
            coin_type: EthCoinType::Erc20 {
                platform: self.ticker().to_string(),
                token_addr,
            },
            sign_message_prefix: self.sign_message_prefix.clone(),
            swap_contract_address: self.swap_contract_address,
            fallback_swap_contract: self.fallback_swap_contract,
            decimals,
            ticker,
            gas_station_url: self.gas_station_url.clone(),
            gas_station_decimals: self.gas_station_decimals,
            gas_station_policy: self.gas_station_policy.clone(),
            web3: self.web3.clone(),
            web3_instances: self.web3_instances.clone(),
            history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
            ctx: self.ctx.clone(),
            required_confirmations: required_confirmations.into(),
            chain_id: self.chain_id,
            logs_block_range: self.logs_block_range,
            derivation_method: DerivationMethod::Iguana(self.my_address),
            swap_v2_contracts: self.swap_v2_contracts,
            gas_limit_v2: self.gas_limit_v2.clone(),
            tron_api: None,
            nft_swap_v2_contract: self.nft_swap_v2_contract,
            swap_gas_fee_policy: Mutex::new(self.swap_gas_fee_policy()),
            erc20_tokens_infos: Default::default(),
        };
        Ok(EthCoin(Arc::new(token_impl)))
    }

    /// Downloads and saves ETH transaction history of my_address, relies on Parity trace_filter API
    /// https://wiki.parity.io/JSONRPC-trace-module#trace_filter, this requires tracing to be enabled
    /// in node config. Other ETH clients (Geth, etc.) are `not` supported (yet).
    #[allow(clippy::cognitive_complexity)]
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) async fn process_eth_history(&self, ctx: &MmArc) {
        // Artem Pikulin: by playing a bit with Parity mainnet node I've discovered that trace_filter API responds after reasonable time for 1000 blocks.
        // I've tried to increase the amount to 10000, but request times out somewhere near 2500000 block.
        // Also the Parity RPC server seem to get stuck while request in running (other requests performance is also lowered).
        let delta = U256::from(1000);

        let mut success_iteration = 0i32;
        loop {
            if ctx.is_stopping() {
                break;
            };
            {
                let coins_ctx = CoinsContext::from_ctx(ctx).unwrap();
                let coins = coins_ctx.coins.lock().await;
                if !coins.contains_key(&self.ticker) {
                    ctx.log.log("", &[&"tx_history", &self.ticker], "Loop stopped");
                    break;
                };
            }

            // LP-17: alloy `Provider::get_block_number` returns `u64`
            // directly; coerced back to `U256` to keep the existing
            // `SavedTraces::earliest_block` arithmetic untouched.
            // Wire-level RPC method (`eth_blockNumber`) is unchanged.
            let current_block = {
                use crate::eth::alloy_compat::assert_send_future;
                use alloy::providers::Provider;
                let provider = self.alloy_provider();
                match assert_send_future(provider.get_block_number()).await {
                    Ok(block) => U256::from(block),
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_block_number, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                }
            };

            let mut saved_traces = match self.load_saved_traces(ctx) {
                Some(traces) => traces,
                None => SavedTraces {
                    traces: vec![],
                    earliest_block: current_block,
                    latest_block: current_block,
                },
            };
            *self.history_sync_state.lock().unwrap() = HistorySyncState::InProgress(json!({
                "blocks_left": saved_traces.earliest_block.as_u64(),
            }));

            let mut existing_history = match self.load_history_from_file(ctx).compat().await {
                Ok(history) => history,
                Err(e) => {
                    ctx.log.log(
                        "",
                        &[&"tx_history", &self.ticker],
                        &ERRL!("Error {} on 'load_history_from_file', stop the history loop", e),
                    );
                    return;
                },
            };

            // AP: AFAIK ETH RPC doesn't support conditional filters like `get this OR this` so we have
            // to run several queries to get trace events including our address as sender `or` receiver
            // TODO refactor this to batch requests instead of single request per query
            if saved_traces.earliest_block > 0.into() {
                let before_earliest = if saved_traces.earliest_block >= delta {
                    saved_traces.earliest_block - delta
                } else {
                    0.into()
                };

                let from_traces_before_earliest = match self
                    .eth_traces(
                        vec![self.my_address],
                        vec![],
                        BlockNumber::Number(before_earliest.as_u64()),
                        BlockNumber::Number(saved_traces.earliest_block.as_u64()),
                        None,
                    )
                    .compat()
                    .await
                {
                    Ok(traces) => traces,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_traces, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let to_traces_before_earliest = match self
                    .eth_traces(
                        vec![],
                        vec![self.my_address],
                        BlockNumber::Number(before_earliest.as_u64()),
                        BlockNumber::Number(saved_traces.earliest_block.as_u64()),
                        None,
                    )
                    .compat()
                    .await
                {
                    Ok(traces) => traces,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_traces, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let total_length = from_traces_before_earliest.len() + to_traces_before_earliest.len();
                mm_counter!(ctx.metrics, "tx.history.response.total_length", total_length as u64,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "eth_traces");

                saved_traces.traces.extend(from_traces_before_earliest);
                saved_traces.traces.extend(to_traces_before_earliest);
                saved_traces.earliest_block = if before_earliest > 0.into() {
                    // need to exclude the before earliest block from next iteration
                    before_earliest - 1
                } else {
                    0.into()
                };
                self.store_eth_traces(ctx, &saved_traces);
            }

            if current_block > saved_traces.latest_block {
                let from_traces_after_latest = match self
                    .eth_traces(
                        vec![self.my_address],
                        vec![],
                        BlockNumber::Number((saved_traces.latest_block + 1).as_u64()),
                        BlockNumber::Number(current_block.as_u64()),
                        None,
                    )
                    .compat()
                    .await
                {
                    Ok(traces) => traces,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_traces, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let to_traces_after_latest = match self
                    .eth_traces(
                        vec![],
                        vec![self.my_address],
                        BlockNumber::Number((saved_traces.latest_block + 1).as_u64()),
                        BlockNumber::Number(current_block.as_u64()),
                        None,
                    )
                    .compat()
                    .await
                {
                    Ok(traces) => traces,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_traces, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let total_length = from_traces_after_latest.len() + to_traces_after_latest.len();
                mm_counter!(ctx.metrics, "tx.history.response.total_length", total_length as u64,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "eth_traces");

                saved_traces.traces.extend(from_traces_after_latest);
                saved_traces.traces.extend(to_traces_after_latest);
                saved_traces.latest_block = current_block;

                self.store_eth_traces(ctx, &saved_traces);
            }
            saved_traces.traces.sort_by(|a, b| b.block_number.cmp(&a.block_number));
            for trace in saved_traces.traces {
                let hash = sha256(&json::to_vec(&trace).unwrap());
                let internal_id = BytesJson::from(hash.to_vec());
                let processed = existing_history.iter().find(|tx| tx.internal_id == internal_id);
                if processed.is_some() {
                    continue;
                }

                // TODO Only standard Call traces are supported, contract creations, suicides and block rewards will be supported later
                let call_data = match trace.action {
                    TraceAction::Call(d) => d,
                    _ => continue,
                };

                mm_counter!(ctx.metrics, "tx.history.request.count", 1, "coin" => self.ticker.clone(), "method" => "tx_detail_by_hash");

                // LP-17: alloy `Provider::get_transaction_by_hash` /
                // `get_transaction_receipt` / `get_block_by_number`
                // replace the three legacy web3 fetches in this
                // tx-history sync path. Wire-level RPC methods
                // (`eth_getTransactionByHash`, `eth_getTransactionReceipt`,
                // `eth_getBlockByNumber`) are unchanged. The fetched
                // alloy `Transaction` is round-tripped through
                // `signed_tx_from_alloy_tx` so the persisted
                // `TransactionDetails::tx_hex` (RLP of `SignedEthTx`)
                // and `tx_hash` are bit-for-bit identical to the
                // pre-migration output.
                use crate::eth::alloy_compat::assert_send_future;
                use alloy::providers::Provider;

                let provider = self.alloy_provider();
                let trace_hash = trace.transaction_hash.unwrap();
                let alloy_hash = alloy::primitives::B256::from_slice(&trace_hash.0);

                let alloy_tx = match assert_send_future(provider.get_transaction_by_hash(alloy_hash)).await {
                    Ok(tx) => tx,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on getting transaction {:?}", e, trace_hash),
                        );
                        continue;
                    },
                };
                let alloy_tx = match alloy_tx {
                    Some(t) => t,
                    None => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("No such transaction {:?}", trace_hash),
                        );
                        continue;
                    },
                };

                mm_counter!(ctx.metrics, "tx.history.response.count", 1, "coin" => self.ticker.clone(), "method" => "tx_detail_by_hash");

                let receipt = match assert_send_future(provider.get_transaction_receipt(alloy_hash)).await {
                    Ok(r) => r,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on getting transaction {:?} receipt", e, trace_hash),
                        );
                        continue;
                    },
                };
                let raw = signed_tx_from_alloy_tx(alloy_tx).unwrap();
                let fee_coin = match &self.coin_type {
                    EthCoinType::Eth => self.ticker(),
                    EthCoinType::Erc20 { platform, .. } => platform.as_str(),
                    EthCoinType::Tron => self.ticker(),
                    EthCoinType::Trc20 { platform, .. } => platform.as_str(),
                };
                let fee_details: Option<EthTxFeeDetails> = match receipt {
                    Some(r) => Some(EthTxFeeDetails::new(U256::from(r.gas_used), raw.gas_price, fee_coin).unwrap()),
                    None => None,
                };

                let total_amount: BigDecimal = u256_to_big_decimal(call_data.value, 18).unwrap();
                let mut received_by_me = 0.into();
                let mut spent_by_me = 0.into();

                if call_data.from == self.my_address {
                    // ETH transfer is actually happening only if no error occurred
                    if trace.error.is_none() {
                        spent_by_me = total_amount.clone();
                    }
                    if let Some(ref fee) = fee_details {
                        spent_by_me += &fee.total_fee;
                    }
                }

                if call_data.to == self.my_address {
                    // ETH transfer is actually happening only if no error occurred
                    if trace.error.is_none() {
                        received_by_me = total_amount.clone();
                    }
                }

                // LP-17: alloy `Provider::get_block_by_number` replaces
                // `web3.eth().block(BlockId::Number(...))`. Wire-level
                // RPC method (`eth_getBlockByNumber`) is unchanged.
                // alloy's `Block.header.timestamp` is already `u64`,
                // matching the legacy `block.timestamp.into()` pattern.
                let block_ts = {
                    use crate::eth::alloy_compat::assert_send_future;
                    use alloy::eips::BlockNumberOrTag;
                    use alloy::providers::Provider;
                    use std::future::IntoFuture;
                    let provider = self.alloy_provider();
                    match assert_send_future(
                        provider
                            .get_block_by_number(BlockNumberOrTag::Number(u64::from(trace.block_number)))
                            .into_future(),
                    )
                    .await
                    {
                        Ok(Some(b)) => b.header.timestamp,
                        Ok(None) => {
                            ctx.log.log(
                                "",
                                &[&"tx_history", &self.ticker],
                                &ERRL!("Block {} is None", trace.block_number),
                            );
                            continue;
                        },
                        Err(e) => {
                            ctx.log.log(
                                "",
                                &[&"tx_history", &self.ticker],
                                &ERRL!("Error {} on getting block {} data", e, trace.block_number),
                            );
                            continue;
                        },
                    }
                };

                let details = TransactionDetails {
                    my_balance_change: &received_by_me - &spent_by_me,
                    spent_by_me,
                    received_by_me,
                    total_amount,
                    to: vec![checksum_address(&format!("{:#02x}", call_data.to))],
                    from: vec![checksum_address(&format!("{:#02x}", call_data.from))],
                    coin: self.ticker.clone(),
                    fee_details: fee_details.map(|d| d.into()),
                    block_height: trace.block_number,
                    tx_hash: format!("{:02x}", BytesJson(raw.hash.as_bytes().to_vec())),
                    tx_hex: BytesJson(rlp::encode(&raw)),
                    internal_id,
                    timestamp: block_ts,
                    kmd_rewards: None,
                    transaction_type: Default::default(),
                };

                existing_history.push(details);
                existing_history.sort_unstable_by(|a, b| {
                    if a.block_height == 0 {
                        Ordering::Less
                    } else if b.block_height == 0 {
                        Ordering::Greater
                    } else {
                        b.block_height.cmp(&a.block_height)
                    }
                });

                if let Err(e) = self.save_history_to_file(ctx, existing_history.clone()).compat().await {
                    ctx.log.log(
                        "",
                        &[&"tx_history", &self.ticker],
                        &ERRL!("Error {} on 'save_history_to_file', stop the history loop", e),
                    );
                    return;
                }
            }
            if saved_traces.earliest_block == 0.into() {
                if success_iteration == 0 {
                    ctx.log.log(
                        "😅",
                        &[&"tx_history", &("coin", self.ticker.clone().as_str())],
                        "history has been loaded successfully",
                    );
                }

                success_iteration += 1;
                *self.history_sync_state.lock().unwrap() = HistorySyncState::Finished;
                Timer::sleep(15.).await;
            } else {
                Timer::sleep(2.).await;
            }
        }
    }

    /// Downloads and saves ERC20 transaction history of my_address
    #[allow(clippy::cognitive_complexity)]
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) async fn process_erc20_history(&self, token_addr: H160, ctx: &MmArc) {
        let delta = U256::from(10000);

        let mut success_iteration = 0i32;
        loop {
            if ctx.is_stopping() {
                break;
            };
            {
                let coins_ctx = CoinsContext::from_ctx(ctx).unwrap();
                let coins = coins_ctx.coins.lock().await;
                if !coins.contains_key(&self.ticker) {
                    ctx.log.log("", &[&"tx_history", &self.ticker], "Loop stopped");
                    break;
                };
            }

            // LP-17: alloy `Provider::get_block_number` (see step 7d
            // sibling site in process_eth_history). Coerced back to
            // `U256` for `SavedErc20Events::earliest_block`.
            let current_block = {
                use crate::eth::alloy_compat::assert_send_future;
                use alloy::providers::Provider;
                let provider = self.alloy_provider();
                match assert_send_future(provider.get_block_number()).await {
                    Ok(block) => U256::from(block),
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_block_number, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                }
            };

            let mut saved_events = match self.load_saved_erc20_events(ctx) {
                Some(events) => events,
                None => SavedErc20Events {
                    events: vec![],
                    earliest_block: current_block,
                    latest_block: current_block,
                },
            };
            *self.history_sync_state.lock().unwrap() = HistorySyncState::InProgress(json!({
                "blocks_left": saved_events.earliest_block.as_u64(),
            }));

            // AP: AFAIK ETH RPC doesn't support conditional filters like `get this OR this` so we have
            // to run several queries to get transfer events including our address as sender `or` receiver
            // TODO refactor this to batch requests instead of single request per query
            if saved_events.earliest_block > 0.into() {
                let before_earliest = if saved_events.earliest_block >= delta {
                    saved_events.earliest_block - delta
                } else {
                    0.into()
                };

                let from_events_before_earliest = match self
                    .erc20_transfer_events(
                        token_addr,
                        Some(self.my_address),
                        None,
                        BlockNumber::Number(before_earliest.as_u64()),
                        BlockNumber::Number((saved_events.earliest_block - 1).as_u64()),
                        None,
                    )
                    .compat()
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on erc20_transfer_events, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let to_events_before_earliest = match self
                    .erc20_transfer_events(
                        token_addr,
                        None,
                        Some(self.my_address),
                        BlockNumber::Number(before_earliest.as_u64()),
                        BlockNumber::Number((saved_events.earliest_block - 1).as_u64()),
                        None,
                    )
                    .compat()
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on erc20_transfer_events, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let total_length = from_events_before_earliest.len() + to_events_before_earliest.len();
                mm_counter!(ctx.metrics, "tx.history.response.total_length", total_length as u64,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "erc20_transfer_events");

                saved_events.events.extend(from_events_before_earliest);
                saved_events.events.extend(to_events_before_earliest);
                saved_events.earliest_block = if before_earliest > 0.into() {
                    before_earliest - 1
                } else {
                    0.into()
                };
                self.store_erc20_events(ctx, &saved_events);
            }

            if current_block > saved_events.latest_block {
                let from_events_after_latest = match self
                    .erc20_transfer_events(
                        token_addr,
                        Some(self.my_address),
                        None,
                        BlockNumber::Number((saved_events.latest_block + 1).as_u64()),
                        BlockNumber::Number(current_block.as_u64()),
                        None,
                    )
                    .compat()
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on erc20_transfer_events, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let to_events_after_latest = match self
                    .erc20_transfer_events(
                        token_addr,
                        None,
                        Some(self.my_address),
                        BlockNumber::Number((saved_events.latest_block + 1).as_u64()),
                        BlockNumber::Number(current_block.as_u64()),
                        None,
                    )
                    .compat()
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on erc20_transfer_events, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let total_length = from_events_after_latest.len() + to_events_after_latest.len();
                mm_counter!(ctx.metrics, "tx.history.response.total_length", total_length as u64,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "erc20_transfer_events");

                saved_events.events.extend(from_events_after_latest);
                saved_events.events.extend(to_events_after_latest);
                saved_events.latest_block = current_block;
                self.store_erc20_events(ctx, &saved_events);
            }

            let all_events: HashMap<_, _> = saved_events
                .events
                .iter()
                .filter(|e| e.block_number.is_some() && e.transaction_hash.is_some() && !e.is_removed())
                .map(|e| (e.transaction_hash.unwrap(), e))
                .collect();
            let mut all_events: Vec<_> = all_events.into_iter().map(|(_, log)| log).collect();
            all_events.sort_by(|a, b| b.block_number.unwrap().cmp(&a.block_number.unwrap()));

            for event in all_events {
                let mut existing_history = match self.load_history_from_file(ctx).compat().await {
                    Ok(history) => history,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on 'load_history_from_file', stop the history loop", e),
                        );
                        return;
                    },
                };
                let internal_id = BytesJson::from(sha256(&json::to_vec(&event).unwrap()).to_vec());
                if existing_history.iter().any(|item| item.internal_id == internal_id) {
                    // the transaction already imported
                    continue;
                };

                let amount = U256::from(event.data.0.as_slice());
                let total_amount = u256_to_big_decimal(amount, self.decimals).unwrap();
                let mut received_by_me = 0.into();
                let mut spent_by_me = 0.into();

                let from_addr = H160::from(event.topics[1]);
                let to_addr = H160::from(event.topics[2]);

                if from_addr == self.my_address {
                    spent_by_me = total_amount.clone();
                }

                if to_addr == self.my_address {
                    received_by_me = total_amount.clone();
                }

                mm_counter!(ctx.metrics, "tx.history.request.count", 1,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "tx_detail_by_hash");

                // LP-17: alloy `Provider::get_transaction_by_hash` /
                // `get_transaction_receipt` replace the legacy web3
                // fetches in this ERC20 tx-history sync path. Wire-
                // level RPC methods (`eth_getTransactionByHash`,
                // `eth_getTransactionReceipt`) unchanged. The fetched
                // alloy `Transaction` is round-tripped through
                // `signed_tx_from_alloy_tx` so persisted
                // `TransactionDetails::tx_hex` / `tx_hash` are
                // bit-for-bit identical.
                use crate::eth::alloy_compat::assert_send_future;
                use alloy::providers::Provider;

                let provider = self.alloy_provider();
                let event_hash = event.transaction_hash.unwrap();
                let alloy_hash = alloy::primitives::B256::from_slice(&event_hash.0);

                let alloy_tx = match assert_send_future(provider.get_transaction_by_hash(alloy_hash)).await {
                    Ok(tx) => tx,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on getting transaction {:?}", e, event_hash),
                        );
                        continue;
                    },
                };

                mm_counter!(ctx.metrics, "tx.history.response.count", 1,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "tx_detail_by_hash");

                let alloy_tx = match alloy_tx {
                    Some(t) => t,
                    None => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("No such transaction {:?}", event_hash),
                        );
                        continue;
                    },
                };

                let receipt = match assert_send_future(provider.get_transaction_receipt(alloy_hash)).await {
                    Ok(r) => r,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on getting transaction {:?} receipt", e, event_hash),
                        );
                        continue;
                    },
                };
                let raw = signed_tx_from_alloy_tx(alloy_tx).unwrap();
                let fee_coin = match &self.coin_type {
                    EthCoinType::Eth => self.ticker(),
                    EthCoinType::Erc20 { platform, .. } => platform.as_str(),
                    EthCoinType::Tron => self.ticker(),
                    EthCoinType::Trc20 { platform, .. } => platform.as_str(),
                };
                let fee_details = match receipt {
                    Some(r) => Some(EthTxFeeDetails::new(U256::from(r.gas_used), raw.gas_price, fee_coin).unwrap()),
                    None => None,
                };
                let block_number = event.block_number.unwrap();
                // LP-17: alloy `Provider::get_block_by_number` replaces
                // `web3.eth().block(...)`. Wire-level RPC method
                // (`eth_getBlockByNumber`) unchanged; timestamp is
                // already `u64` on alloy's header.
                let block_ts = {
                    use crate::eth::alloy_compat::assert_send_future;
                    use alloy::eips::BlockNumberOrTag;
                    use alloy::providers::Provider;
                    use std::future::IntoFuture;
                    let provider = self.alloy_provider();
                    match assert_send_future(
                        provider
                            .get_block_by_number(BlockNumberOrTag::Number(block_number.as_u64()))
                            .into_future(),
                    )
                    .await
                    {
                        Ok(Some(b)) => b.header.timestamp,
                        Ok(None) => {
                            ctx.log.log(
                                "",
                                &[&"tx_history", &self.ticker],
                                &ERRL!("Block {} is None", block_number),
                            );
                            continue;
                        },
                        Err(e) => {
                            ctx.log.log(
                                "",
                                &[&"tx_history", &self.ticker],
                                &ERRL!("Error {} on getting block {} data", e, block_number),
                            );
                            continue;
                        },
                    }
                };

                let details = TransactionDetails {
                    my_balance_change: &received_by_me - &spent_by_me,
                    spent_by_me,
                    received_by_me,
                    total_amount,
                    to: vec![checksum_address(&format!("{:#02x}", to_addr))],
                    from: vec![checksum_address(&format!("{:#02x}", from_addr))],
                    coin: self.ticker.clone(),
                    fee_details: fee_details.map(|d| d.into()),
                    block_height: block_number.as_u64(),
                    tx_hash: format!("{:02x}", BytesJson(raw.hash.as_bytes().to_vec())),
                    tx_hex: BytesJson(rlp::encode(&raw)),
                    internal_id: BytesJson(internal_id.to_vec()),
                    timestamp: block_ts,
                    kmd_rewards: None,
                    transaction_type: Default::default(),
                };

                existing_history.push(details);
                existing_history.sort_unstable_by(|a, b| {
                    if a.block_height == 0 {
                        Ordering::Less
                    } else if b.block_height == 0 {
                        Ordering::Greater
                    } else {
                        b.block_height.cmp(&a.block_height)
                    }
                });
                if let Err(e) = self.save_history_to_file(ctx, existing_history).compat().await {
                    ctx.log.log(
                        "",
                        &[&"tx_history", &self.ticker],
                        &ERRL!("Error {} on 'save_history_to_file', stop the history loop", e),
                    );
                    return;
                }
            }
            if saved_events.earliest_block == 0.into() {
                if success_iteration == 0 {
                    ctx.log.log(
                        "😅",
                        &[&"tx_history", &("coin", self.ticker.clone().as_str())],
                        "history has been loaded successfully",
                    );
                }

                success_iteration += 1;
                *self.history_sync_state.lock().unwrap() = HistorySyncState::Finished;
                Timer::sleep(15.).await;
            } else {
                Timer::sleep(2.).await;
            }
        }
    }
}

#[cfg_attr(test, mockable)]
impl EthCoin {
    pub(crate) fn sign_and_send_transaction(&self, value: U256, action: Action, data: Vec<u8>, gas: U256) -> EthTxFut {
        let ctx = try_tx_fus!(MmArc::from_weak(&self.ctx).ok_or("!ctx"));
        let fut = Box::pin(sign_and_send_transaction_impl(
            ctx,
            self.clone(),
            value,
            action,
            data,
            gas,
        ));
        Box::new(fut.compat())
    }

    pub fn send_to_address(&self, address: Address, value: U256) -> EthTxFut {
        match &self.coin_type {
            EthCoinType::Eth => self.sign_and_send_transaction(value, Action::Call(address), vec![], U256::from(21000)),
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let abi = try_tx_fus!(Contract::load(ERC20_ABI.as_bytes()));
                let function = try_tx_fus!(abi.function("transfer"));
                let data = try_tx_fus!(function.encode_input(&[Token::Address(address), Token::Uint(value)]));
                self.sign_and_send_transaction(0.into(), Action::Call(*token_addr), data, U256::from(210_000))
            },
            // TRON has its own transfer pipeline (build TransactionRaw,
            // sign with SHA-256+secp256k1, broadcast via TronApiClient).
            // Activation gating prevents this from being reached. P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => Box::new(futures01::future::err(TransactionErr::Plain(
                ERRL!("TRON send_to_address not yet wired (pending P10.2.5)"),
            ))),
        }
    }

    pub(crate) fn send_hash_time_locked_payment(
        &self,
        id: Vec<u8>,
        value: U256,
        time_lock: u32,
        secret_hash: &[u8],
        receiver_addr: Address,
        swap_contract_address: Address,
    ) -> EthTxFut {
        match &self.coin_type {
            EthCoinType::Eth => {
                let function = try_tx_fus!(SWAP_CONTRACT.function("ethPayment"));
                let data = try_tx_fus!(function.encode_input(&[
                    Token::FixedBytes(id),
                    Token::Address(receiver_addr),
                    Token::FixedBytes(secret_hash.to_vec()),
                    Token::Uint(U256::from(time_lock))
                ]));
                self.sign_and_send_transaction(value, Action::Call(swap_contract_address), data, U256::from(150_000))
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let allowance_fut = self
                    .allowance(swap_contract_address)
                    .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)));

                let function = try_tx_fus!(SWAP_CONTRACT.function("erc20Payment"));
                let data = try_tx_fus!(function.encode_input(&[
                    Token::FixedBytes(id),
                    Token::Uint(value),
                    Token::Address(*token_addr),
                    Token::Address(receiver_addr),
                    Token::FixedBytes(secret_hash.to_vec()),
                    Token::Uint(U256::from(time_lock))
                ]));

                let arc = self.clone();
                Box::new(allowance_fut.and_then(move |allowed| -> EthTxFut {
                    if allowed < value {
                        Box::new(
                            arc.approve(swap_contract_address, U256::max_value())
                                .and_then(move |_approved| {
                                    arc.sign_and_send_transaction(
                                        0.into(),
                                        Action::Call(swap_contract_address),
                                        data,
                                        U256::from(150_000),
                                    )
                                }),
                        )
                    } else {
                        Box::new(arc.sign_and_send_transaction(
                            0.into(),
                            Action::Call(swap_contract_address),
                            data,
                            U256::from(150_000),
                        ))
                    }
                }))
            },
            // V1 Ethereum HTLC swaps; TRON uses a separate atomic-swap flow.
            // Activation gating prevents this code path. P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => Box::new(futures01::future::err(TransactionErr::Plain(
                ERRL!("TRON HTLC payment not yet wired (pending P10.2.5)"),
            ))),
        }
    }

    pub(crate) fn spend_hash_time_locked_payment(
        &self,
        payment: SignedEthTx,
        swap_contract_address: Address,
        secret: &[u8],
    ) -> EthTxFut {
        let spend_func = try_tx_fus!(SWAP_CONTRACT.function("receiverSpend"));
        let clone = self.clone();
        let secret_vec = secret.to_vec();

        match self.coin_type {
            EthCoinType::Eth => {
                let payment_func = try_tx_fus!(SWAP_CONTRACT.function("ethPayment"));
                let decoded = try_tx_fus!(payment_func.decode_input(&payment.data[4..]));

                let state_f = self.payment_status(swap_contract_address, decoded[0].clone());
                Box::new(
                    state_f
                        .map_err(TransactionErr::Plain)
                        .and_then(move |state| -> EthTxFut {
                            if state != PAYMENT_STATE_SENT.into() {
                                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                                    "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                                    payment,
                                    state
                                ))));
                            }

                            let value = payment.value;
                            let data = try_tx_fus!(spend_func.encode_input(&[
                                decoded[0].clone(),
                                Token::Uint(value),
                                Token::FixedBytes(secret_vec),
                                Token::Address(Address::default()),
                                Token::Address(payment.sender()),
                            ]));

                            clone.sign_and_send_transaction(
                                0.into(),
                                Action::Call(swap_contract_address),
                                data,
                                U256::from(150_000),
                            )
                        }),
                )
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let payment_func = try_tx_fus!(SWAP_CONTRACT.function("erc20Payment"));
                let decoded = try_tx_fus!(payment_func.decode_input(&payment.data[4..]));
                let state_f = self.payment_status(swap_contract_address, decoded[0].clone());

                Box::new(
                    state_f
                        .map_err(TransactionErr::Plain)
                        .and_then(move |state| -> EthTxFut {
                            if state != PAYMENT_STATE_SENT.into() {
                                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                                    "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                                    payment,
                                    state
                                ))));
                            }
                            let data = try_tx_fus!(spend_func.encode_input(&[
                                decoded[0].clone(),
                                decoded[1].clone(),
                                Token::FixedBytes(secret_vec),
                                Token::Address(token_addr),
                                Token::Address(payment.sender()),
                            ]));

                            clone.sign_and_send_transaction(
                                0.into(),
                                Action::Call(swap_contract_address),
                                data,
                                U256::from(150_000),
                            )
                        }),
                )
            },
            // V1 Ethereum HTLC swaps; TRON path is gated. P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => Box::new(futures01::future::err(TransactionErr::Plain(
                ERRL!("TRON HTLC spend not yet wired (pending P10.2.5)"),
            ))),
        }
    }

    pub(crate) fn refund_hash_time_locked_payment(
        &self,
        swap_contract_address: Address,
        payment: SignedEthTx,
    ) -> EthTxFut {
        let refund_func = try_tx_fus!(SWAP_CONTRACT.function("senderRefund"));
        let clone = self.clone();

        match self.coin_type {
            EthCoinType::Eth => {
                let payment_func = try_tx_fus!(SWAP_CONTRACT.function("ethPayment"));
                let decoded = try_tx_fus!(payment_func.decode_input(&payment.data[4..]));

                let state_f = self.payment_status(swap_contract_address, decoded[0].clone());
                Box::new(
                    state_f
                        .map_err(TransactionErr::Plain)
                        .and_then(move |state| -> EthTxFut {
                            if state != PAYMENT_STATE_SENT.into() {
                                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                                    "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                                    payment,
                                    state
                                ))));
                            }

                            let value = payment.value;
                            let data = try_tx_fus!(refund_func.encode_input(&[
                                decoded[0].clone(),
                                Token::Uint(value),
                                decoded[2].clone(),
                                Token::Address(Address::default()),
                                decoded[1].clone(),
                            ]));

                            clone.sign_and_send_transaction(
                                0.into(),
                                Action::Call(swap_contract_address),
                                data,
                                U256::from(150_000),
                            )
                        }),
                )
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let payment_func = try_tx_fus!(SWAP_CONTRACT.function("erc20Payment"));
                let decoded = try_tx_fus!(payment_func.decode_input(&payment.data[4..]));
                let state_f = self.payment_status(swap_contract_address, decoded[0].clone());
                Box::new(
                    state_f
                        .map_err(TransactionErr::Plain)
                        .and_then(move |state| -> EthTxFut {
                            if state != PAYMENT_STATE_SENT.into() {
                                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                                    "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                                    payment,
                                    state
                                ))));
                            }

                            let data = try_tx_fus!(refund_func.encode_input(&[
                                decoded[0].clone(),
                                decoded[1].clone(),
                                decoded[4].clone(),
                                Token::Address(token_addr),
                                decoded[3].clone(),
                            ]));

                            clone.sign_and_send_transaction(
                                0.into(),
                                Action::Call(swap_contract_address),
                                data,
                                U256::from(150_000),
                            )
                        }),
                )
            },
            // V1 Ethereum HTLC refund; TRON refund is a separate flow,
            // and activation gating prevents reaching this branch. P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => Box::new(futures01::future::err(TransactionErr::Plain(
                ERRL!("TRON HTLC refund not yet wired (pending P10.2.5)"),
            ))),
        }
    }

    pub(crate) fn my_balance(&self) -> BalanceFut<U256> {
        let coin = self.clone();
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            match coin.coin_type {
                EthCoinType::Eth => assert_send_future(
                    coin.web3
                        .client()
                        .request::<_, U256>("eth_getBalance", (coin.my_address, BlockNumber::Latest)),
                )
                .await
                .map_err(|e| MmError::new(BalanceError::Transport(e.to_string()))),
                EthCoinType::Erc20 { ref token_addr, .. } => {
                    let function = ERC20_CONTRACT.function("balanceOf")?;
                    let data = function.encode_input(&[Token::Address(coin.my_address)])?;

                    let res = coin
                        .call_request(*token_addr, None, Some(data.into()))
                        .compat()
                        .await
                        .mm_err(BalanceError::from)?;
                    let decoded = function.decode_output(&res.0)?;
                    match decoded[0] {
                        Token::Uint(number) => Ok(number),
                        _ => {
                            let error = format!("Expected U256 as balanceOf result but got {:?}", decoded);
                            MmError::err(BalanceError::InvalidResponse(error))
                        },
                    }
                },
                // TRON balance is fetched via the dedicated TRON HTTP API,
                // not via web3. Activation gating prevents this branch. P10.2.5.
                EthCoinType::Tron | EthCoinType::Trc20 { .. } => MmError::err(BalanceError::Internal(
                    "TRON balance lookup not yet wired (pending P10.2.5)".to_owned(),
                )),
            }
        };
        Box::new(fut.boxed().compat())
    }

    /// Estimates how much gas is necessary to allow the contract call to complete.
    /// `contract_addr` can be a ERC20 token address or any other contract address.
    ///
    /// # Important
    ///
    /// Don't use this method to estimate gas for a withdrawal of `ETH` coin.
    /// For more details, see `withdraw_impl`.
    ///
    /// Also, note that the contract call has to be initiated by my wallet address,
    /// because [`CallRequest::from`] is set to [`EthCoinImpl::my_address`].
    pub(crate) fn estimate_gas_for_contract_call(&self, contract_addr: Address, call_data: Bytes) -> Web3RpcFut<U256> {
        let coin = self.clone();
        Box::new(coin.get_gas_price().and_then(move |gas_price| {
            let eth_value = U256::zero();
            let estimate_gas_req = CallRequest {
                value: Some(eth_value),
                data: Some(call_data),
                from: Some(coin.my_address),
                to: contract_addr,
                gas: None,
                // gas price must be supplied because some smart contracts base their
                // logic on gas price, e.g. TUSD: https://github.com/KomodoPlatform/atomicDEX-API/issues/643
                gas_price: Some(gas_price),
            };
            coin.estimate_gas(estimate_gas_req)
        }))
    }

    pub(crate) fn eth_balance(&self) -> BalanceFut<U256> {
        // LP-17: alloy raw RPC for eth_getBalance. Returned U256
        // shape preserved by deserializing into web3's U256 alias.
        let provider = self.alloy_provider();
        let addr = self.my_address;
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            assert_send_future(
                provider
                    .client()
                    .request::<_, U256>("eth_getBalance", (addr, BlockNumber::Latest)),
            )
            .await
            .map_err(|e| MmError::new(BalanceError::Transport(e.to_string())))
        };
        Box::new(fut.boxed().compat())
    }

    pub(crate) fn call_request(&self, to: Address, value: Option<U256>, data: Option<Bytes>) -> Web3RpcFut<Bytes> {
        let request = CallRequest {
            from: Some(self.my_address),
            to,
            gas: None,
            gas_price: None,
            value,
            data,
        };

        // LP-17: alloy raw RPC for eth_call.
        let provider = self.alloy_provider();
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            assert_send_future(
                provider
                    .client()
                    .request::<_, Bytes>("eth_call", (request, BlockNumber::Latest)),
            )
            .await
            .map_to_mm(|e| Web3RpcError::Transport(e.to_string()))
        };
        Box::new(fut.boxed().compat())
    }

    pub(crate) fn allowance(&self, spender: Address) -> Web3RpcFut<U256> {
        let coin = self.clone();
        let fut = async move {
            match coin.coin_type {
                EthCoinType::Eth => MmError::err(Web3RpcError::Internal(
                    "'allowance' must not be called for ETH coin".to_owned(),
                )),
                EthCoinType::Erc20 { ref token_addr, .. } => {
                    let function = ERC20_CONTRACT.function("allowance")?;
                    let data = function.encode_input(&[Token::Address(coin.my_address), Token::Address(spender)])?;

                    let res = coin.call_request(*token_addr, None, Some(data.into())).compat().await?;
                    let decoded = function.decode_output(&res.0)?;

                    match decoded[0] {
                        Token::Uint(number) => Ok(number),
                        _ => {
                            let error = format!("Expected U256 as allowance result but got {:?}", decoded);
                            MmError::err(Web3RpcError::InvalidResponse(error))
                        },
                    }
                },
                // TRC20 allowance would use the dedicated TRON read-only call;
                // ETH allowance() pipeline is not used. Gated until P10.2.5.
                EthCoinType::Tron | EthCoinType::Trc20 { .. } => MmError::err(Web3RpcError::Internal(
                    "TRON allowance not yet wired (pending P10.2.5)".to_owned(),
                )),
            }
        };
        Box::new(fut.boxed().compat())
    }

    pub(crate) fn approve(&self, spender: Address, amount: U256) -> EthTxFut {
        let coin = self.clone();
        let fut = async move {
            let token_addr = match coin.coin_type {
                EthCoinType::Eth => return TX_PLAIN_ERR!("'approve' is expected to be call for ERC20 coins only"),
                EthCoinType::Erc20 { token_addr, .. } => token_addr,
                // ERC20 approve() is not used for TRON/TRC20 (the TRON contract
                // surface is invoked via the TRON HTTP API). Gated until P10.2.5.
                EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                    return TX_PLAIN_ERR!("TRON approve not yet wired (pending P10.2.5)")
                },
            };
            let function = try_tx_s!(ERC20_CONTRACT.function("approve"));
            let data = try_tx_s!(function.encode_input(&[Token::Address(spender), Token::Uint(amount)]));

            let gas_limit = try_tx_s!(
                coin.estimate_gas_for_contract_call(token_addr, Bytes::from(data.clone()))
                    .compat()
                    .await
            );

            coin.sign_and_send_transaction(0.into(), Action::Call(token_addr), data, gas_limit)
                .compat()
                .await
        };
        Box::new(fut.boxed().compat())
    }

    /// Gets `PaymentSent` events from etomic swap smart contract since `from_block`
    pub(crate) fn payment_sent_events(
        &self,
        swap_contract_address: Address,
        from_block: u64,
        to_block: u64,
    ) -> Box<dyn Future<Item = Vec<Log>, Error = String> + Send> {
        let contract_event = try_fus!(SWAP_CONTRACT.event("PaymentSent"));
        let filter = FilterBuilder::default()
            .topics(Some(vec![contract_event.signature()]), None, None, None)
            .from_block(BlockNumber::Number(from_block))
            .to_block(BlockNumber::Number(to_block))
            .address(vec![swap_contract_address])
            .build();

        // LP-17: route eth_getLogs through alloy's RpcClient while
        // keeping the returned `Vec<web3::types::Log>` shape.
        let provider = self.alloy_provider();
        let fut = async move {
            use crate::eth::alloy_compat::assert_send_future;
            assert_send_future(provider.client().request::<_, Vec<Log>>("eth_getLogs", (filter,)))
                .await
                .map_err(|e| ERRL!("{}", e))
        };
        Box::new(fut.boxed().compat())
    }

    /// Returns events from `from_block` to `to_block` (or latest if None) for a given contract and event.
    pub(crate) async fn events_from_block(
        &self,
        swap_contract_address: Address,
        event_name: &str,
        from_block: u64,
        to_block: Option<u64>,
        swap_contract: &Contract,
    ) -> MmResult<Vec<Log>, FindPaymentSpendError> {
        let contract_event = swap_contract.event(event_name)?;
        let mut filter_builder = FilterBuilder::default()
            .topics(Some(vec![contract_event.signature()]), None, None, None)
            .from_block(BlockNumber::Number(from_block))
            .address(vec![swap_contract_address]);
        if let Some(block) = to_block {
            filter_builder = filter_builder.to_block(BlockNumber::Number(block));
        }
        let filter = filter_builder.build();
        // LP-17: alloy raw RPC for eth_getLogs; web3 Log shape kept.
        use crate::eth::alloy_compat::assert_send_future;
        let provider = self.alloy_provider();
        let events_logs = assert_send_future(provider.client().request::<_, Vec<Log>>("eth_getLogs", (filter,)))
            .await
            .map_err(|e| FindPaymentSpendError::Transport(e.to_string()))?;
        Ok(events_logs)
    }

    /// Waits until the allowance for `spender` reaches `required_allowance` or `wait_until` expires.
    pub(crate) fn wait_for_required_allowance(
        &self,
        spender: Address,
        required_allowance: U256,
        wait_until: u64,
    ) -> Web3RpcFut<()> {
        const CHECK_ALLOWANCE_EVERY: f64 = 5.;

        let selfi = self.clone();
        let fut = async move {
            loop {
                if now_ms() / 1000 > wait_until {
                    return MmError::err(Web3RpcError::Internal(ERRL!(
                        "Waited too long until {} for allowance to be updated to at least {}",
                        wait_until,
                        required_allowance
                    )));
                }

                match selfi.allowance(spender).compat().await {
                    Ok(allowed) if allowed >= required_allowance => return Ok(()),
                    Ok(_allowed) => (),
                    Err(e) => match e.get_inner() {
                        Web3RpcError::Transport(e) => error!("Error {} on trying to get the allowed amount!", e),
                        _ => return Err(e),
                    },
                }

                Timer::sleep(CHECK_ALLOWANCE_EVERY).await;
            }
        };
        Box::new(fut.boxed().compat())
    }

    pub(crate) fn validate_payment(
        &self,
        payment_tx: &[u8],
        time_lock: u32,
        sender_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        expected_swap_contract_address: Address,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let unsigned: UnverifiedTransaction = try_fus!(rlp::decode(payment_tx));
        let tx = try_fus!(SignedEthTx::new(unsigned));
        let sender = try_fus!(addr_from_raw_pubkey(sender_pub));
        let expected_value = try_fus!(wei_from_big_decimal(&amount, self.decimals));
        let selfi = self.clone();
        let secret_hash = secret_hash.to_vec();
        let fut = async move {
            let swap_id = selfi.etomic_swap_id(time_lock, &secret_hash);
            let status = try_s!(
                selfi
                    .payment_status(expected_swap_contract_address, Token::FixedBytes(swap_id.clone()))
                    .compat()
                    .await
            );
            if status != PAYMENT_STATE_SENT.into() {
                return ERR!("Payment state is not PAYMENT_STATE_SENT, got {}", status);
            }

            // LP-17: alloy `Provider::get_transaction_by_hash` replaces
            // `web3.eth().transaction(...)`. The fetched alloy
            // `Transaction` carries the typed-envelope inner; field
            // accesses are routed through `alloy::consensus::Transaction`
            // (gas_price/value/input/to/etc.) plus the wrapper's own
            // `inner.signer()`. Wire-level RPC method
            // `eth_getTransactionByHash` is unchanged.
            use crate::eth::alloy_compat::assert_send_future;
            use alloy::consensus::Transaction as _;
            use alloy::providers::Provider;

            let provider = selfi.alloy_provider();
            let alloy_hash = alloy::primitives::B256::from_slice(&tx.hash.0);
            let tx_from_rpc = try_s!(assert_send_future(provider.get_transaction_by_hash(alloy_hash)).await);
            let tx_from_rpc = match tx_from_rpc {
                Some(t) => t,
                None => return ERR!("Didn't find provided tx {:?} on ETH node", tx),
            };

            let from_addr = Address::from_slice(tx_from_rpc.inner.signer().as_slice());
            if from_addr != sender {
                return ERR!(
                    "Payment tx {:?} was sent from wrong address, expected {:?}",
                    tx_from_rpc,
                    sender
                );
            }

            let envelope = tx_from_rpc.inner.inner();
            let to_addr = envelope.to().map(|a| Address::from_slice(a.as_slice()));
            let tx_value = {
                let bytes: [u8; 32] = envelope.value().to_be_bytes();
                U256::from_big_endian(&bytes)
            };
            let tx_input: Vec<u8> = envelope.input().to_vec();

            match &selfi.coin_type {
                EthCoinType::Eth => {
                    if to_addr != Some(expected_swap_contract_address) {
                        return ERR!(
                            "Payment tx {:?} was sent to wrong address, expected {:?}",
                            tx_from_rpc,
                            expected_swap_contract_address
                        );
                    }

                    if tx_value != expected_value {
                        return ERR!(
                            "Payment tx {:?} value is invalid, expected {:?}",
                            tx_from_rpc,
                            expected_value
                        );
                    }

                    let function = try_s!(SWAP_CONTRACT.function("ethPayment"));
                    let decoded = try_s!(function.decode_input(&tx_input[4..]));
                    if decoded[0] != Token::FixedBytes(swap_id.clone()) {
                        return ERR!("Invalid 'swap_id' {:?}, expected {:?}", decoded, swap_id);
                    }

                    if decoded[1] != Token::Address(selfi.my_address) {
                        return ERR!(
                            "Payment tx receiver arg {:?} is invalid, expected {:?}",
                            decoded[1],
                            Token::Address(selfi.my_address)
                        );
                    }

                    if decoded[2] != Token::FixedBytes(secret_hash.to_vec()) {
                        return ERR!(
                            "Payment tx secret_hash arg {:?} is invalid, expected {:?}",
                            decoded[2],
                            Token::FixedBytes(secret_hash.to_vec())
                        );
                    }

                    if decoded[3] != Token::Uint(U256::from(time_lock)) {
                        return ERR!(
                            "Payment tx time_lock arg {:?} is invalid, expected {:?}",
                            decoded[3],
                            Token::Uint(U256::from(time_lock))
                        );
                    }
                },
                EthCoinType::Erc20 {
                    platform: _,
                    token_addr,
                } => {
                    if to_addr != Some(expected_swap_contract_address) {
                        return ERR!(
                            "Payment tx {:?} was sent to wrong address, expected {:?}",
                            tx_from_rpc,
                            expected_swap_contract_address
                        );
                    }

                    let function = try_s!(SWAP_CONTRACT.function("erc20Payment"));
                    let decoded = try_s!(function.decode_input(&tx_input[4..]));
                    if decoded[0] != Token::FixedBytes(swap_id.clone()) {
                        return ERR!("Invalid 'swap_id' {:?}, expected {:?}", decoded, swap_id);
                    }

                    if decoded[1] != Token::Uint(expected_value) {
                        return ERR!(
                            "Payment tx value arg {:?} is invalid, expected {:?}",
                            decoded[1],
                            Token::Uint(expected_value)
                        );
                    }

                    if decoded[2] != Token::Address(*token_addr) {
                        return ERR!(
                            "Payment tx token_addr arg {:?} is invalid, expected {:?}",
                            decoded[2],
                            Token::Address(*token_addr)
                        );
                    }

                    if decoded[3] != Token::Address(selfi.my_address) {
                        return ERR!(
                            "Payment tx receiver arg {:?} is invalid, expected {:?}",
                            decoded[3],
                            Token::Address(selfi.my_address)
                        );
                    }

                    if decoded[4] != Token::FixedBytes(secret_hash.to_vec()) {
                        return ERR!(
                            "Payment tx secret_hash arg {:?} is invalid, expected {:?}",
                            decoded[4],
                            Token::FixedBytes(secret_hash.to_vec())
                        );
                    }

                    if decoded[5] != Token::Uint(U256::from(time_lock)) {
                        return ERR!(
                            "Payment tx time_lock arg {:?} is invalid, expected {:?}",
                            decoded[5],
                            Token::Uint(U256::from(time_lock))
                        );
                    }
                },
                // V1 ETH/ERC20 payment validation; TRON HTLC payments use a
                // separate validator. Activation gating prevents this branch. P10.2.5.
                EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                    return ERR!("TRON HTLC payment validation not yet wired (pending P10.2.5)");
                },
            }

            Ok(())
        };
        Box::new(fut.boxed().compat())
    }

    pub(crate) fn payment_status(
        &self,
        swap_contract_address: H160,
        token: Token,
    ) -> Box<dyn Future<Item = U256, Error = String> + Send + 'static> {
        let function = try_fus!(SWAP_CONTRACT.function("payments"));

        let data = try_fus!(function.encode_input(&[token]));

        Box::new(
            self.call_request(swap_contract_address, None, Some(data.into()))
                .map_err(|e| ERRL!("{}", e))
                .and_then(move |bytes| {
                    let decoded_tokens = try_s!(function.decode_output(&bytes.0));
                    match decoded_tokens[2] {
                        Token::Uint(state) => Ok(state),
                        _ => ERR!("Payment status must be uint, got {:?}", decoded_tokens[2]),
                    }
                }),
        )
    }

    pub(crate) async fn search_for_swap_tx_spend(
        &self,
        tx: &[u8],
        swap_contract_address: Address,
        search_from_block: u64,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let unverified: UnverifiedTransaction = try_s!(rlp::decode(tx));
        let tx = try_s!(SignedEthTx::new(unverified));

        let func_name = match self.coin_type {
            EthCoinType::Eth => "ethPayment",
            EthCoinType::Erc20 { .. } => "erc20Payment",
            // V1 ETH/ERC20 search; TRON spend search uses TRON HTTP API.
            // Activation gating prevents this branch. P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                return ERR!("TRON spend search not yet wired (pending P10.2.5)");
            },
        };

        let payment_func = try_s!(SWAP_CONTRACT.function(func_name));
        let decoded = try_s!(payment_func.decode_input(&tx.data[4..]));
        let id = match &decoded[0] {
            Token::FixedBytes(bytes) => bytes.clone(),
            _ => panic!(),
        };

        // Wrap futures with `assert_send_future` so the resulting
        // future is `Send`. The web3 `Box<dyn Future + Send>` futures
        // we still call here are themselves Send, but `.compat()`
        // produces a wrapper that the compiler can't always prove is
        // Send across `await` points; the assertion is sound on
        // native and required for WASM single-thread runtime parity.
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::providers::Provider;

        let mut current_block = try_s!(assert_send_future(self.current_block().compat()).await);
        if current_block < search_from_block {
            current_block = search_from_block;
        }

        let mut from_block = search_from_block;

        loop {
            let to_block = current_block.min(from_block + self.logs_block_range);

            let spend_events = try_s!(
                assert_send_future(self.spend_events(swap_contract_address, from_block, to_block).compat()).await
            );
            let found = spend_events.iter().find(|event| &event.data.0[..32] == id.as_slice());

            if let Some(event) = found {
                match event.transaction_hash {
                    Some(tx_hash) => {
                        // LP-17: alloy `Provider::get_transaction_by_hash` +
                        // `signed_tx_from_alloy_tx` round-trip replaces the
                        // legacy `web3.eth().transaction(...).wait()` +
                        // `signed_tx_from_web3_tx` pair. Converting this
                        // function from sync (`.wait()`) to `async` is
                        // sound because all callers (search_for_swap_tx_spend
                        // _my/_other in eth_swap_ops.rs and tests) already
                        // `.await` the returned value.
                        let provider = self.alloy_provider();
                        let alloy_hash = alloy::primitives::B256::from_slice(&tx_hash.0);
                        let transaction =
                            match try_s!(assert_send_future(provider.get_transaction_by_hash(alloy_hash)).await) {
                                Some(t) => t,
                                None => {
                                    return ERR!(
                                        "Found ReceiverSpent event, but transaction {:02x} is missing",
                                        tx_hash
                                    )
                                },
                            };

                        return Ok(Some(FoundSwapTxSpend::Spent(TransactionEnum::from(try_s!(
                            signed_tx_from_alloy_tx(transaction)
                        )))));
                    },
                    None => return ERR!("Found ReceiverSpent event, but it doesn't have tx_hash"),
                }
            }

            let refund_events = try_s!(
                assert_send_future(self.refund_events(swap_contract_address, from_block, to_block).compat()).await
            );
            let found = refund_events.iter().find(|event| &event.data.0[..32] == id.as_slice());

            if let Some(event) = found {
                match event.transaction_hash {
                    Some(tx_hash) => {
                        let provider = self.alloy_provider();
                        let alloy_hash = alloy::primitives::B256::from_slice(&tx_hash.0);
                        let transaction =
                            match try_s!(assert_send_future(provider.get_transaction_by_hash(alloy_hash)).await) {
                                Some(t) => t,
                                None => {
                                    return ERR!(
                                        "Found SenderRefunded event, but transaction {:02x} is missing",
                                        tx_hash
                                    )
                                },
                            };

                        return Ok(Some(FoundSwapTxSpend::Refunded(TransactionEnum::from(try_s!(
                            signed_tx_from_alloy_tx(transaction)
                        )))));
                    },
                    None => return ERR!("Found SenderRefunded event, but it doesn't have tx_hash"),
                }
            }

            if to_block >= current_block {
                break;
            }
            from_block = to_block;
        }

        Ok(None)
    }

    /// Get gas price
    pub(crate) fn get_gas_price(&self) -> Web3RpcFut<U256> {
        let coin = self.clone();
        let fut = async move {
            // TODO refactor to error_log_passthrough once simple maker bot is merged
            let gas_station_price = match &coin.gas_station_url {
                Some(url) => {
                    match GasStationData::get_gas_price(url, coin.gas_station_decimals, coin.gas_station_policy)
                        .compat()
                        .await
                    {
                        Ok(from_station) => Some(increase_by_percent_one_gwei(from_station, GAS_PRICE_PERCENT)),
                        Err(e) => {
                            error!("Error {} on request to gas station url {}", e, url);
                            None
                        },
                    }
                },
                None => None,
            };

            // LP-17: alloy raw RPC for eth_gasPrice + eth_feeHistory.
            let eth_gas_price = {
                use crate::eth::alloy_compat::assert_send_future;
                let provider = coin.alloy_provider();
                match assert_send_future(provider.client().request_noparams::<U256>("eth_gasPrice")).await {
                    Ok(eth_gas) => Some(eth_gas),
                    Err(e) => {
                        error!("Error {} on eth_gasPrice request", e);
                        None
                    },
                }
            };

            let eth_fee_history_price = {
                use crate::eth::alloy_compat::assert_send_future;
                let provider = coin.alloy_provider();
                match assert_send_future(provider.client().request::<_, FeeHistoryResult>(
                    "eth_feeHistory",
                    (U256::from(1u64), BlockNumber::Latest, &[] as &[f64]),
                ))
                .await
                {
                    Ok(res) => res
                        .base_fee_per_gas
                        .first()
                        .map(|val| increase_by_percent_one_gwei(*val, BASE_BLOCK_FEE_DIFF_PCT)),
                    Err(e) => {
                        error!("Error {} on eth_feeHistory request", e);
                        None
                    },
                }
            };

            let all_prices = vec![gas_station_price, eth_gas_price, eth_fee_history_price];
            all_prices
                .into_iter()
                .flatten()
                .max()
                .or_mm_err(|| Web3RpcError::Internal("All requests failed".into()))
        };
        Box::new(fut.boxed().compat())
    }

    /// Get EIP-1559 gas fee estimates (base fee + priority fees at low/medium/high levels).
    /// When `use_simple` is true, only the internal fee-history estimator is used.
    /// Otherwise, tries the configured gas api provider first, falling back to simple.
    pub async fn get_eip1559_gas_fee(
        &self,
        use_simple: bool,
    ) -> Web3RpcResult<fee_estimation::eip1559::FeePerGasEstimated> {
        use fee_estimation::eip1559::block_native::BlocknativeFeeFetcher;
        use fee_estimation::eip1559::infura::InfuraFeeFetcher;
        use fee_estimation::eip1559::simple::FeePerGasSimpleEstimator;
        use fee_estimation::eip1559::{GasApiConfig, GasApiProvider};

        let coin = self.clone();
        let ctx = MmArc::from_weak(&coin.ctx).or_mm_err(|| Web3RpcError::Internal("ctx is null".into()))?;

        let gas_api_conf = ctx.conf["gas_api"].clone();
        if gas_api_conf.is_null() || use_simple {
            return FeePerGasSimpleEstimator::estimate_fee_by_history(&coin)
                .await
                .mm_err(|e| Web3RpcError::Internal(e.to_string()));
        }

        let gas_api_conf: GasApiConfig = serde_json::from_value(gas_api_conf)
            .map_to_mm(|e| Web3RpcError::InvalidResponse(format!("Invalid gas_api config: {}", e)))?;

        let provider_result = match gas_api_conf.provider {
            GasApiProvider::Infura => InfuraFeeFetcher::fetch_fee_estimation(&gas_api_conf.url).await,
            GasApiProvider::Blocknative => BlocknativeFeeFetcher::fetch_fee_estimation(&gas_api_conf.url).await,
        };

        match provider_result {
            Ok(fees) => Ok(fees),
            Err(provider_err) => {
                error!("Gas api provider failed: {}, using internal estimator", provider_err);
                FeePerGasSimpleEstimator::estimate_fee_by_history(&coin)
                    .await
                    .mm_err(|history_err| {
                        Web3RpcError::Internal(format!(
                            "All gas api requests failed. Provider: {}, History: {}",
                            provider_err, history_err
                        ))
                    })
            },
        }
    }
}

pub trait TryToAddress {
    fn try_to_address(&self) -> Result<Address, String>;
}

impl TryToAddress for BytesJson {
    fn try_to_address(&self) -> Result<Address, String> {
        {
            let s = self.0.as_slice();
            if s.len() == 20 {
                Ok(Address::from_slice(s))
            } else {
                Err(format!("invalid address length: {}", s.len()))
            }
        }
    }
}

impl<T: TryToAddress> TryToAddress for Option<T> {
    fn try_to_address(&self) -> Result<Address, String> {
        match self {
            Some(ref inner) => inner.try_to_address(),
            None => ERR!("Cannot convert None to address"),
        }
    }
}

pub fn addr_from_raw_pubkey(pubkey: &[u8]) -> Result<Address, String> {
    let pubkey = try_s!(PublicKey::from_slice(pubkey).map_err(|e| ERRL!("{:?}", e)));
    let eth_public = Public::from_slice(&pubkey.serialize_uncompressed()[1..65]);
    Ok(public_to_address(&eth_public))
}

pub fn addr_from_pubkey_str(pubkey: &str) -> Result<String, String> {
    let pubkey_bytes = try_s!(hex::decode(pubkey));
    let addr = try_s!(addr_from_raw_pubkey(&pubkey_bytes));
    Ok(format!("{:#02x}", addr))
}

pub fn display_u256_with_decimal_point(number: U256, decimals: u8) -> String {
    let mut string = number.to_string();
    let decimals = decimals as usize;
    if string.len() <= decimals {
        string.insert_str(0, &"0".repeat(decimals - string.len() + 1));
    }

    string.insert(string.len() - decimals, '.');
    string.trim_end_matches('0').into()
}

pub fn u256_to_big_decimal(number: U256, decimals: u8) -> NumConversResult<BigDecimal> {
    let string = display_u256_with_decimal_point(number, decimals);
    Ok(string.parse::<BigDecimal>()?)
}

pub fn wei_from_big_decimal(amount: &BigDecimal, decimals: u8) -> NumConversResult<U256> {
    let mut amount = amount.to_string();
    let dot = amount.find(|c| c == '.');
    let decimals = decimals as usize;
    if let Some(index) = dot {
        let mut fractional = amount.split_off(index);
        // remove the dot from fractional part
        fractional.remove(0);
        if fractional.len() < decimals {
            fractional.insert_str(fractional.len(), &"0".repeat(decimals - fractional.len()));
        }
        fractional.truncate(decimals);
        amount.push_str(&fractional);
    } else {
        amount.insert_str(amount.len(), &"0".repeat(decimals));
    }
    U256::from_dec_str(&amount)
        .map_err(|e| format!("{:?}", e))
        .map_to_mm(NumConversError::new)
}

/// Convert a BigDecimal amount in gwei to U256 in wei (multiply by 10^9).
pub fn wei_from_gwei_decimal(amount: &BigDecimal) -> NumConversResult<U256> { wei_from_big_decimal(amount, 9) }

/// Convert a U256 in wei to BigDecimal in gwei (divide by 10^9).
pub fn wei_to_gwei_decimal(amount: U256) -> NumConversResult<BigDecimal> { u256_to_big_decimal(amount, 9) }

impl Transaction for SignedEthTx {
    fn tx_hex(&self) -> Vec<u8> { rlp::encode(self).to_vec() }

    fn tx_hash(&self) -> BytesJson { self.hash.as_bytes().to_vec().into() }
}

/// LP-17: alloy-flavoured replacement for the legacy
/// `signed_tx_from_web3_tx`.
///
/// Re-builds a legacy `UnverifiedTransaction` (artemii235's parity-ethereum
/// fork — only legacy 9-RLP txs supported here) from an
/// [`alloy::rpc::types::eth::Transaction`] returned by
/// `eth_getTransactionByHash`. This is the chokepoint that lets the
/// HTLC-validation paths (`SwapOps::check_if_my_payment_sent`,
/// `MarketCoinOps::wait_for_tx_spend`, `EthCoinImpl::search_for_swap_tx_spend`)
/// keep producing a `SignedEthTx` whose embedded signature passes the
/// public-key recovery check inside `SignedEthTx::new`.
///
/// Wire / cryptographic invariants:
///
/// - `r`, `s`: alloy returns `U256` (its own); we reflect to
///   `ethereum_types::U256` via the canonical 32-byte big-endian repr.
/// - `v`: alloy stores y_parity as a `bool`; the legacy struct stores
///   the EIP-155-encoded `u64`. Re-encode as `27 + parity` for
///   pre-EIP-155 chains and `35 + 2*chain_id + parity` otherwise. This
///   mirrors what the legacy `web3` JSON-RPC layer used to return for
///   the `v` field.
/// - `hash`: alloy `B256` is bit-for-bit `H256`; copied via
///   `H256::from_slice(...)`.
/// - `gas_price`: alloy returns `Option<u128>` (None for type-2 / 1559
///   txs). The legacy struct expects `U256`; we coerce via
///   `unwrap_or(0)`. Type-2 txs would not survive RLP-decoding through
///   this struct anyway — the existing code path is legacy-only.
/// - `gas`, `nonce`: alloy `u64` → `U256`.
/// - `value`, `data`, `to`: direct mapping.
pub fn signed_tx_from_alloy_tx(transaction: alloy::rpc::types::eth::Transaction) -> Result<SignedEthTx, String> {
    use alloy::consensus::Transaction as _;

    let envelope = transaction.into_inner();
    let signature = envelope.signature();
    let alloy_hash = *envelope.tx_hash();

    let r_bytes: [u8; 32] = signature.r().to_be_bytes();
    let s_bytes: [u8; 32] = signature.s().to_be_bytes();
    let value_bytes: [u8; 32] = envelope.value().to_be_bytes();

    let r_legacy = U256::from_big_endian(&r_bytes);
    let s_legacy = U256::from_big_endian(&s_bytes);
    let value_legacy = U256::from_big_endian(&value_bytes);

    let parity = u64::from(signature.v());
    let v_legacy = match envelope.chain_id() {
        Some(cid) => 35 + 2 * cid + parity,
        None => 27 + parity,
    };

    let action = match envelope.to() {
        Some(addr) => Action::Call(Address::from_slice(addr.as_slice())),
        None => Action::Create,
    };

    let gas_price_u128 = envelope.gas_price().unwrap_or(0);
    let gas_price_bytes = {
        let mut b = [0u8; 32];
        b[16..32].copy_from_slice(&gas_price_u128.to_be_bytes());
        b
    };

    let unverified = UnverifiedTransaction {
        r: r_legacy,
        s: s_legacy,
        v: v_legacy,
        hash: H256::from_slice(alloy_hash.as_slice()),
        unsigned: UnSignedEthTx {
            data: envelope.input().to_vec(),
            gas_price: U256::from_big_endian(&gas_price_bytes),
            gas: U256::from(envelope.gas_limit()),
            value: value_legacy,
            nonce: U256::from(envelope.nonce()),
            action,
        },
    };

    Ok(try_s!(SignedEthTx::new(unverified)))
}

#[derive(Deserialize, Debug, Serialize)]
pub struct GasStationData {
    // matic gas station average fees is named standard, using alias to support both format.
    #[serde(alias = "average", alias = "standard")]
    pub(crate) average: MmNumber,
    pub(crate) fast: MmNumber,
}

/// Using tagged representation to allow adding variants with coefficients, percentage, etc in the future.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(tag = "policy", content = "additional_data")]
pub enum GasStationPricePolicy {
    /// Use mean between average and fast values, default and recommended to use on ETH mainnet due to
    /// gas price big spikes.
    MeanAverageFast,
    /// Use average value only. Useful for non-heavily congested networks (Matic, etc.)
    Average,
}

impl Default for GasStationPricePolicy {
    fn default() -> Self { GasStationPricePolicy::MeanAverageFast }
}

impl GasStationData {
    pub(crate) fn average_gwei(&self, decimals: u8, gas_price_policy: GasStationPricePolicy) -> NumConversResult<U256> {
        let gas_price = match gas_price_policy {
            GasStationPricePolicy::MeanAverageFast => ((&self.average + &self.fast) / MmNumber::from(2)).into(),
            GasStationPricePolicy::Average => self.average.to_decimal(),
        };
        wei_from_big_decimal(&gas_price, decimals)
    }

    pub(crate) fn get_gas_price(uri: &str, decimals: u8, gas_price_policy: GasStationPricePolicy) -> Web3RpcFut<U256> {
        let uri = uri.to_owned();
        let fut = async move {
            make_gas_station_request(&uri)
                .await
                .mm_err(Into::into)?
                .average_gwei(decimals, gas_price_policy)
                .mm_err(|e| Web3RpcError::Internal(e.0))
        };
        Box::new(fut.boxed().compat())
    }
}

pub async fn get_token_decimals(web3: &super::alloy_compat::KdfProvider, token_addr: Address) -> Result<u8, String> {
    let function = try_s!(ERC20_CONTRACT.function("decimals"));
    let data = try_s!(function.encode_input(&[]));
    let request = CallRequest {
        from: Some(Address::default()),
        to: token_addr,
        gas: None,
        gas_price: None,
        value: Some(0.into()),
        data: Some(data.into()),
    };

    // LP-17: alloy raw RPC for eth_call.
    let res: Bytes = try_s!(web3
        .client()
        .request::<_, Bytes>("eth_call", (request, BlockNumber::Latest))
        .await
        .map_err(|e| ERRL!("{}", e)));
    let tokens = try_s!(function.decode_output(&res.0));
    let decimals: u64 = match tokens[0] {
        Token::Uint(dec) => dec.as_u64(),
        _ => return ERR!("Invalid decimals type {:?}", tokens),
    };
    Ok(decimals as u8)
}

pub async fn get_token_symbol(web3: &super::alloy_compat::KdfProvider, token_addr: Address) -> Result<String, String> {
    let function = try_s!(ERC20_CONTRACT.function("symbol"));
    let data = try_s!(function.encode_input(&[]));
    let request = CallRequest {
        from: Some(Address::default()),
        to: token_addr,
        gas: None,
        gas_price: None,
        value: Some(0.into()),
        data: Some(data.into()),
    };

    let res: Bytes = try_s!(web3
        .client()
        .request::<_, Bytes>("eth_call", (request, BlockNumber::Latest))
        .await
        .map_err(|e| ERRL!("{}", e)));
    let tokens = try_s!(function.decode_output(&res.0));
    let symbol = match tokens.into_iter().next() {
        Some(Token::String(s)) => s,
        other => return ERR!("Invalid symbol type {:?}", other),
    };
    Ok(symbol)
}

pub fn valid_addr_from_str(addr_str: &str) -> Result<Address, String> {
    let addr = try_s!(addr_from_str(addr_str));
    if !is_valid_checksum_addr(addr_str) {
        return ERR!("Invalid address checksum");
    }
    Ok(addr)
}

pub fn addr_from_str(addr_str: &str) -> Result<Address, String> {
    if !addr_str.starts_with("0x") {
        return ERR!("Address must be prefixed with 0x");
    };

    Ok(try_s!(Address::from_str(&addr_str[2..])))
}

pub fn rpc_event_handlers_for_eth_transport(ctx: &MmArc, ticker: String) -> Vec<RpcTransportEventHandlerShared> {
    let metrics = ctx.metrics.weak();
    vec![CoinTransportMetrics::new(metrics, ticker, RpcClientType::Ethereum).into_shared()]
}

pub async fn eth_coin_from_conf_and_request(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    req: &Json,
    priv_key: &[u8],
    protocol: CoinProtocol,
) -> Result<EthCoin, String> {
    let key_pair: KeyPair = try_s!(KeyPair::from_secret_slice(priv_key));
    eth_coin_from_conf_and_request_with_signer(ctx, ticker, conf, req, EthSigner::Local(key_pair), protocol).await
}

/// MetaMask-policy EVM activation (CRD §47.5.A). Builds an [`EthCoin`] whose
/// signer delegates signing and broadcast to the connected browser MetaMask
/// session (`EthSigner::Metamask`); the framework holds no local key. The
/// coin's address is the connected account (CRD R47.5.3); the centrally-threaded
/// `priv_key` is intentionally ignored under this policy.
#[cfg(target_arch = "wasm32")]
pub async fn eth_coin_from_conf_and_request_with_metamask(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    req: &Json,
    metamask_arc: crypto::MetamaskArc,
    protocol: CoinProtocol,
) -> Result<EthCoin, String> {
    eth_coin_from_conf_and_request_with_signer(ctx, ticker, conf, req, EthSigner::Metamask(metamask_arc), protocol)
        .await
}

/// Shared EVM-coin builder body: identical for every signing policy except how
/// the [`EthSigner`] is constructed and where the coin address comes from
/// (`signer.address()`). The `Local` path is byte-identical to the prior
/// `eth_coin_from_conf_and_request` behaviour.
pub(crate) async fn eth_coin_from_conf_and_request_with_signer(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    req: &Json,
    signer: EthSigner,
    protocol: CoinProtocol,
) -> Result<EthCoin, String> {
    // Defensive: TRON activates through a dedicated builder
    // (`tron::tron_coin_from_conf_and_request`); reject any attempt to route
    // TRX/TRC20 through the EVM legacy activator.
    if matches!(protocol, CoinProtocol::TRX { .. } | CoinProtocol::TRC20 { .. }) {
        return ERR!("TRON protocol must activate through tron_coin_from_conf_and_request, not the EVM activator");
    }

    let mut urls: Vec<String> = try_s!(json::from_value(req["urls"].clone()));
    if urls.is_empty() {
        return ERR!("Enable request for ETH coin must have at least 1 node URL");
    }
    let mut rng = small_rng();
    urls.as_mut_slice().shuffle(&mut rng);

    let swap_contract_address: Address = try_s!(json::from_value(req["swap_contract_address"].clone()));
    if swap_contract_address == Address::default() {
        return ERR!("swap_contract_address can't be zero address");
    }

    let fallback_swap_contract: Option<Address> = try_s!(json::from_value(req["fallback_swap_contract"].clone()));
    if let Some(fallback) = fallback_swap_contract {
        if fallback == Address::default() {
            return ERR!("fallback_swap_contract can't be zero address");
        }
    }

    let my_address = signer.address();

    let mut web3_instances = vec![];
    let event_handlers = rpc_event_handlers_for_eth_transport(ctx, ticker.to_string());
    for url in urls.iter() {
        // LP-17: alloy provider per URL + alloy raw web3_clientVersion.
        let provider = match super::alloy_compat::build_provider(vec![url.clone()], event_handlers.clone()) {
            Ok(p) => p,
            Err(e) => {
                log!("Failed to build alloy provider for url " (url) ", " (e));
                continue;
            },
        };
        let version: String = match provider.client().request_noparams("web3_clientVersion").await {
            Ok(v) => v,
            Err(e) => {
                log!("Couldn't get client version for url " (url) ", " (e));
                continue;
            },
        };
        web3_instances.push(Web3Instance {
            web3: provider,
            is_parity: version.contains("Parity") || version.contains("parity"),
        })
    }

    if web3_instances.is_empty() {
        return ERR!("Failed to get client version for all urls");
    }

    let web3 = try_s!(super::alloy_compat::build_provider(urls, event_handlers));

    let (coin_type, decimals) = match protocol {
        CoinProtocol::ETH { .. } => (EthCoinType::Eth, 18),
        CoinProtocol::ERC20 {
            platform,
            contract_address,
        } => {
            let token_addr = try_s!(valid_addr_from_str(&contract_address));
            let decimals = match conf["decimals"].as_u64() {
                None | Some(0) => try_s!(get_token_decimals(&web3, token_addr).await),
                Some(d) => d as u8,
            };
            (EthCoinType::Erc20 { platform, token_addr }, decimals)
        },
        _ => return ERR!("Expect ETH or ERC20 protocol"),
    };

    // param from request should override the config
    let required_confirmations = req["required_confirmations"]
        .as_u64()
        .unwrap_or_else(|| conf["required_confirmations"].as_u64().unwrap_or(1))
        .into();

    if req["requires_notarization"].as_bool().is_some() {
        log!("Warning: requires_notarization doesn't take any effect on ETH/ERC20 coins");
    }

    let sign_message_prefix: Option<String> = json::from_value(conf["sign_message_prefix"].clone()).unwrap_or(None);

    let initial_history_state = if req["tx_history"].as_bool().unwrap_or(false) {
        HistorySyncState::NotStarted
    } else {
        HistorySyncState::NotEnabled
    };

    let gas_station_decimals: Option<u8> = try_s!(json::from_value(req["gas_station_decimals"].clone()));
    let gas_station_policy: GasStationPricePolicy =
        json::from_value(req["gas_station_policy"].clone()).unwrap_or_default();

    let coin = EthCoinImpl {
        signer,
        my_address,
        coin_type,
        sign_message_prefix,
        swap_contract_address,
        fallback_swap_contract,
        decimals,
        ticker: ticker.into(),
        gas_station_url: try_s!(json::from_value(req["gas_station_url"].clone())),
        gas_station_decimals: gas_station_decimals.unwrap_or(ETH_GAS_STATION_DECIMALS),
        gas_station_policy,
        web3,
        web3_instances,
        history_sync_state: Mutex::new(initial_history_state),
        ctx: ctx.weak(),
        required_confirmations,
        chain_id: conf["chain_id"].as_u64(),
        logs_block_range: conf["logs_block_range"].as_u64().unwrap_or(DEFAULT_LOGS_BLOCK_RANGE),
        derivation_method: DerivationMethod::Iguana(my_address),
        swap_v2_contracts: None,
        gas_limit_v2: EthGasLimitV2::default(),
        // ETH/ERC20 coins never use the TRON HTTP API; populated only by the
        // dedicated TRON activation path.
        tron_api: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
    };
    Ok(EthCoin(Arc::new(coin)))
}

/// Displays the address in mixed-case checksum form
/// https://github.com/ethereum/EIPs/blob/master/EIPS/eip-55.md
pub fn checksum_address(addr: &str) -> String {
    let mut addr = addr.to_lowercase();
    if addr.starts_with("0x") {
        addr.replace_range(..2, "");
    }

    let mut hasher = Keccak256::default();
    hasher.update(&addr);
    let hash = hasher.finalize();
    let mut result: String = "0x".into();
    for (i, c) in addr.chars().enumerate() {
        if c.is_digit(10) {
            result.push(c);
        } else {
            // https://github.com/ethereum/EIPs/blob/master/EIPS/eip-55.md#specification
            // Convert the address to hex, but if the ith digit is a letter (ie. it's one of abcdef)
            // print it in uppercase if the 4*ith bit of the hash of the lowercase hexadecimal
            // address is 1 otherwise print it in lowercase.
            if hash[i / 2] & (1 << (7 - 4 * (i % 2))) != 0 {
                result.push(c.to_ascii_uppercase());
            } else {
                result.push(c.to_ascii_lowercase());
            }
        }
    }

    result
}

/// Checks that input is valid mixed-case checksum form address
/// The input must be 0x prefixed hex string
pub fn is_valid_checksum_addr(addr: &str) -> bool { addr == checksum_address(addr) }

/// Requests the nonce from all available nodes and checks that returned results equal.
/// Nodes might need some time to sync and there can be other coins that use same nodes in different order.
/// We need to be sure that nonce is updated on all of them before and after transaction is sent.
#[cfg_attr(test, mockable)]
pub fn get_addr_nonce(addr: Address, web3s: Vec<Web3Instance>) -> Box<dyn Future<Item = U256, Error = String> + Send> {
    let fut = async move {
        let mut errors: u32 = 0;
        loop {
            let futures: Vec<_> = web3s
                .iter()
                .map(|w| {
                    let provider = w.web3.clone();
                    let is_parity = w.is_parity;
                    use crate::eth::alloy_compat::assert_send_future;
                    assert_send_future(async move {
                        // LP-17: alloy raw RPC for parity_nextNonce / eth_getTransactionCount.
                        if is_parity {
                            provider
                                .client()
                                .request::<_, U256>("parity_nextNonce", (addr,))
                                .await
                                .map_err(|e| e.to_string())
                        } else {
                            provider
                                .client()
                                .request::<_, U256>("eth_getTransactionCount", (addr, BlockNumber::Pending))
                                .await
                                .map_err(|e| e.to_string())
                        }
                    })
                })
                .collect();

            let nonces: Vec<_> = join_all(futures)
                .await
                .into_iter()
                .filter_map(|nonce_res| match nonce_res {
                    Ok(n) => Some(n),
                    Err(e) => {
                        log!("Error " (e) " when getting nonce for addr " [addr]);
                        None
                    },
                })
                .collect();
            if nonces.is_empty() {
                // all requests errored
                errors += 1;
                if errors > 5 {
                    return ERR!("Couldn't get nonce after 5 errored attempts, aborting");
                }
            } else {
                let max = nonces.iter().max().unwrap();
                let min = nonces.iter().min().unwrap();
                if max == min {
                    return Ok(*max);
                } else {
                    log!("Max nonce " (max) " != " (min) " min nonce");
                }
            }
            Timer::sleep(1.).await
        }
    };
    Box::new(Box::pin(fut).compat())
}

pub fn increase_by_percent_one_gwei(num: U256, percent: u64) -> U256 {
    let one_gwei = U256::from(10u64.pow(9));
    let percent = (num / U256::from(100)) * U256::from(percent);
    if percent < one_gwei {
        num + one_gwei
    } else {
        num + percent
    }
}

pub fn increase_gas_price_by_stage(gas_price: U256, level: &FeeApproxStage) -> U256 {
    match level {
        FeeApproxStage::WithoutApprox => gas_price,
        FeeApproxStage::StartSwap => {
            increase_by_percent_one_gwei(gas_price, GAS_PRICE_APPROXIMATION_PERCENT_ON_START_SWAP)
        },
        FeeApproxStage::OrderIssue => {
            increase_by_percent_one_gwei(gas_price, GAS_PRICE_APPROXIMATION_PERCENT_ON_ORDER_ISSUE)
        },
        FeeApproxStage::TradePreimage => {
            increase_by_percent_one_gwei(gas_price, GAS_PRICE_APPROXIMATION_PERCENT_ON_TRADE_PREIMAGE)
        },
    }
}

// ─── V2 helper functions ────────────────────────────────────────────────────

/// Decodes the input data of a contract function call.
pub(crate) fn decode_contract_call(func: &Function, data: &[u8]) -> Result<Vec<Token>, AbiError> {
    // The first 4 bytes are the function selector
    if data.len() < 4 {
        return Err(AbiError::InvalidData);
    }
    func.decode_input(&data[4..])
}

/// Extracts a single token from decoded contract call data by index,
/// validating the function ABI parameter name at that position.
pub(crate) fn get_function_input_data(decoded: &[Token], func: &Function, index: usize) -> Result<Token, String> {
    decoded
        .get(index)
        .cloned()
        .ok_or_else(|| format!("Missing token at index {index} for function {}", func.name()))
}

/// Converts a `BigDecimal` amount to `U256` wei using the given decimals.
pub(crate) fn u256_from_big_decimal(amount: &BigDecimal, decimals: u8) -> NumConversResult<U256> {
    wei_from_big_decimal(amount, decimals)
}

// ─── ParseCoinAssocTypes for EthCoin ────────────────────────────────────────
