use crate::context::CoinsActivationContext;
use crate::prelude::*;
use crate::standalone_coin::{InitStandaloneCoinActivationOps, InitStandaloneCoinError,
                             InitStandaloneCoinInitialStatus, InitStandaloneCoinTaskHandle,
                             InitStandaloneCoinTaskManagerShared};
use async_trait::async_trait;
use coins::coin_balance::{EnableCoinBalance, EnableCoinBalanceOps, EnableCoinScanPolicy, IguanaWalletBalance};
use coins::hd_pubkey::{HDXPubExtractor, RpcTaskXPubExtractor};
use coins::siacoin::{SiaCoin, SiaCoinActivationRequest, SiaCoinNewError};
use coins::swap_evidence::SwapEvidenceNodeNotInList;
use coins::{BalanceError, CoinBalance, CoinProtocol, MarketCoinOps, PrivKeyBuildPolicy, RegisterCoinError};
use crypto::hw_rpc_task::{HwConnectStatuses, HwRpcTaskAwaitingStatus, HwRpcTaskUserAction};
use crypto::{CryptoCtx, CryptoCtxError};
use derive_more::Display;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::RpcTaskError;
use ser_error_derive::SerializeErrorType;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::time::Duration;

pub type SiaTaskManagerShared = InitStandaloneCoinTaskManagerShared<SiaCoin>;
pub type SiaRpcTaskHandle = InitStandaloneCoinTaskHandle<SiaCoin>;
pub type SiaAwaitingStatus = HwRpcTaskAwaitingStatus;
pub type SiaUserAction = HwRpcTaskUserAction;

/// The final `ok` activation result of `task::enable_sia::status` (R46.2.4).
#[derive(Clone, Serialize)]
pub struct SiaActivationResult {
    pub ticker: String,
    pub current_block: u64,
    pub wallet_balance: EnableCoinBalance,
}

impl CurrentBlock for SiaActivationResult {
    fn current_block(&self) -> u64 { self.current_block }
}

/// The in-progress status enumeration reported by `status` (R46.2.3).
#[derive(Clone, Serialize)]
#[non_exhaustive]
pub enum SiaInProgressStatus {
    ActivatingCoin,
    RequestingWalletBalance,
    Finishing,
}

impl InitStandaloneCoinInitialStatus for SiaInProgressStatus {
    fn initial_status() -> Self { SiaInProgressStatus::ActivatingCoin }
}

/// The `activation_params` object of `task::enable_sia::init` (R46.1.2).
///
/// This newtype wraps the Sia coin's own activation-request type so the same
/// `{client_conf, tx_history, required_confirmations, gap_limit}` shape is
/// reused verbatim; the wrapper exists only to satisfy the local `TxHistory`
/// bound the standalone-coin task framework requires.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct SiaActivationParams(SiaCoinActivationRequest);

impl TxHistory for SiaActivationParams {
    fn tx_history(&self) -> bool { self.0.tx_history }
}

#[derive(Clone, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum SiaInitError {
    #[display(fmt = "Error on coin {} creation: {}", ticker, error)]
    CoinCreationError {
        ticker: String,
        error: String,
    },
    CoinIsAlreadyActivated {
        ticker: String,
    },
    #[display(fmt = "Initialization task has timed out {:?}", duration)]
    TaskTimedOut {
        duration: Duration,
    },
    CouldNotGetBalance(String),
    CouldNotGetBlockCount(String),
    /// A `swap_evidence_node` other than the coin's `server_url` (CRD ch.51 R100 item 2).
    #[display(fmt = "{}", _0)]
    SwapEvidenceNodeNotInList(SwapEvidenceNodeNotInList),
    Internal(String),
}

/// The `MM2.json` key of the compatibility switch for the shape of `wallet_balance` under an HD wallet.
const SIA_SINGLE_ADDRESS_BALANCE_KEY: &str = "sia_activation_single_address_balance";

/// Whether `wallet_balance` is always reported in the single-address `"Iguana"` form (CRD ch.46 R46.2.4).
///
/// Docs vs reference: the public docs do not document Sia activation at all. The reference always reports
/// the single-address `"Iguana"` form, also under an HD wallet. Reloaded deliberately reports the HD
/// per-account report under an HD wallet instead, so that a Sia coin shows its balance in the same shape
/// as the other coins of an HD wallet (maintainer decision 2026-10-11, R46.2.4). Both shapes carry the
/// same single address and balance, and a client tells them apart by `wallet_type`. Setting this switch to
/// `true` restores the reference behavior: `wallet_balance` is then always the single-address form, with
/// `address` being the single derived address under HD. Under the single-key policy the switch changes
/// nothing, because that form is the only one.
fn single_address_balance_requested(conf: &Json) -> bool {
    conf[SIA_SINGLE_ADDRESS_BALANCE_KEY].as_bool() == Some(true)
}

/// The single-address `wallet_balance` of R46.2.4.
fn single_address_wallet_balance(address: String, balance: CoinBalance) -> EnableCoinBalance {
    EnableCoinBalance::Iguana(IguanaWalletBalance { address, balance })
}

impl SiaInitError {
    fn from_new_err(err: SiaCoinNewError, ticker: String) -> Self {
        match err {
            SiaCoinNewError::SwapEvidenceNodeNotInList(error) => SiaInitError::SwapEvidenceNodeNotInList(error),
            // A signing policy other than the single-key and the HD one is a task-level
            // `CoinCreationError`, not a dedicated key-policy discriminant (CRD ch.46 R46.1.3, R46.5.2).
            SiaCoinNewError::UnsupportedPrivKeyPolicy => SiaInitError::CoinCreationError {
                ticker,
                error: "Sia supports only the single-key and single-address HD signing policies".to_string(),
            },
            other => SiaInitError::CoinCreationError {
                ticker,
                error: other.to_string(),
            },
        }
    }
}

impl From<BalanceError> for SiaInitError {
    fn from(err: BalanceError) -> Self { SiaInitError::CouldNotGetBalance(err.to_string()) }
}

impl From<RegisterCoinError> for SiaInitError {
    fn from(reg_err: RegisterCoinError) -> SiaInitError {
        match reg_err {
            RegisterCoinError::CoinIsInitializedAlready { coin } => {
                SiaInitError::CoinIsAlreadyActivated { ticker: coin }
            },
            RegisterCoinError::Internal(internal) => SiaInitError::Internal(internal),
        }
    }
}

impl From<RpcTaskError> for SiaInitError {
    fn from(rpc_err: RpcTaskError) -> Self {
        match rpc_err {
            RpcTaskError::Timeout(duration) => SiaInitError::TaskTimedOut { duration },
            internal_error => SiaInitError::Internal(internal_error.to_string()),
        }
    }
}

impl From<CryptoCtxError> for SiaInitError {
    fn from(err: CryptoCtxError) -> Self { SiaInitError::Internal(err.to_string()) }
}

impl From<SiaInitError> for InitStandaloneCoinError {
    fn from(err: SiaInitError) -> Self {
        match err {
            SiaInitError::CoinCreationError { ticker, error } => {
                InitStandaloneCoinError::CoinCreationError { ticker, error }
            },
            SiaInitError::CoinIsAlreadyActivated { ticker } => {
                InitStandaloneCoinError::CoinIsAlreadyActivated { ticker }
            },
            SiaInitError::TaskTimedOut { duration } => InitStandaloneCoinError::TaskTimedOut { duration },
            // The coin was built (walletd answered the probe) but the height or the balance could not be
            // read afterwards (CRD ch.46 R46.5.2). The public catalogue's `CouldNotGetBalance` and
            // `CouldNotGetBlockCount` names never reach the wire (R46.5.3).
            SiaInitError::CouldNotGetBalance(e) | SiaInitError::CouldNotGetBlockCount(e) => {
                InitStandaloneCoinError::Transport(e)
            },
            SiaInitError::SwapEvidenceNodeNotInList(error) => InitStandaloneCoinError::SwapEvidenceNodeNotInList(error),
            SiaInitError::Internal(e) => InitStandaloneCoinError::Internal(e),
        }
    }
}

pub struct SiaProtocolInfo;

impl TryFromCoinProtocol for SiaProtocolInfo {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::SIA => Ok(SiaProtocolInfo),
            protocol => MmError::err(protocol),
        }
    }
}

#[async_trait]
impl InitStandaloneCoinActivationOps for SiaCoin {
    type ActivationRequest = SiaActivationParams;
    type StandaloneProtocol = SiaProtocolInfo;
    type ActivationResult = SiaActivationResult;
    type ActivationError = SiaInitError;
    type InProgressStatus = SiaInProgressStatus;
    type AwaitingStatus = SiaAwaitingStatus;
    type UserAction = SiaUserAction;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &SiaTaskManagerShared {
        &activation_ctx.init_sia_task_manager
    }

    async fn init_standalone_coin(
        ctx: MmArc,
        ticker: String,
        coin_conf: Json,
        activation_request: &SiaActivationParams,
        _protocol_info: SiaProtocolInfo,
        _task_handle: &SiaRpcTaskHandle,
    ) -> MmResult<Self, SiaInitError> {
        let priv_key_policy = PrivKeyBuildPolicy::from_activation_policy(&ctx, activation_request.0.priv_key_policy)
            .mm_err(SiaInitError::from)?;
        let coin = SiaCoin::new(&ctx, coin_conf, &activation_request.0, priv_key_policy)
            .await
            .mm_err(|e| SiaInitError::from_new_err(e, ticker))?;
        Ok(coin)
    }

    async fn get_activation_result(
        &self,
        ctx: MmArc,
        task_handle: &SiaRpcTaskHandle,
        _activation_request: &Self::ActivationRequest,
    ) -> MmResult<Self::ActivationResult, SiaInitError> {
        task_handle
            .update_in_progress_status(SiaInProgressStatus::RequestingWalletBalance)
            .mm_err(Into::into)?;
        let current_block = self
            .current_block()
            .compat()
            .await
            .map_to_mm(SiaInitError::CouldNotGetBlockCount)?;

        // `wallet_balance`'s shape is dictated by ch.46 R46.2.4: the single-address
        // ("Iguana") `{address, balance}` report or, for an HD wallet, the generic
        // per-account/per-address balance report -- the same `EnableCoinBalanceOps`
        // blanket impl UTXO/ETH's own activation results use, which branches on
        // `self.derivation_method()` (CRD ch.20 D1). `xpub_extractor` is
        // constructed the same way UTXO/ETH's own `get_activation_result` construct
        // theirs. A Trezor priv-key policy is rejected long before this point
        // (R-T3, ch.46 R46.1.3), so a `SiaCoin` always derives its accounts in
        // software; a hardware context that exists beside a software login makes
        // the extractor `Some`, and the coin ignores it (CRD ch.05 R35c).
        let xpub_extractor = RpcTaskXPubExtractor::new_unchecked(&ctx, task_handle, sia_xpub_extractor_rpc_statuses());
        let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(|error| SiaInitError::Internal(error.to_string()))?;
        let xpub_extractor = if crypto_ctx.hw_ctx().is_some() {
            Some(&xpub_extractor)
        } else {
            None
        };

        let wallet_balance = activation_wallet_balance(self, &ctx.conf, xpub_extractor).await?;

        task_handle
            .update_in_progress_status(SiaInProgressStatus::Finishing)
            .mm_err(Into::into)?;

        Ok(SiaActivationResult {
            ticker: self.ticker().to_owned(),
            current_block,
            wallet_balance,
        })
    }
}

/// The `wallet_balance` of R46.2.4 for an activated coin: the single-address form, or the HD report.
///
/// Activation itself never grows the HD wallet past account 0 (ch.46 R46.1.3: only a single-address
/// HD-account state reaches a successfully activated Sia coin). The HD report therefore lists account 0 with
/// exactly its address 0, the address `my_address()` answers (ch.20 §20.4.1), and that address's balance:
/// `min_addresses_number` of `1` makes the activation list address 0 of the external chain, and
/// `EnableCoinScanPolicy::DoNotScan` keeps the gap-limit scan from listing any further address (a scan would
/// report every used address it finds). `SiaHDWallet` has no persistent account storage to have grown a
/// second account from a prior run, so the new account 0 is the only one ever reported.
async fn activation_wallet_balance<XPubExtractor>(
    coin: &SiaCoin,
    conf: &Json,
    xpub_extractor: Option<&XPubExtractor>,
) -> MmResult<EnableCoinBalance, SiaInitError>
where
    XPubExtractor: HDXPubExtractor + Sync,
{
    if single_address_balance_requested(conf) {
        // Compatibility switch `sia_activation_single_address_balance`: the reference's
        // single-address report, also under HD (see `single_address_balance_requested`).
        let address = coin.my_address().map_to_mm(SiaInitError::Internal)?;
        let balance = coin
            .my_balance()
            .compat()
            .await
            .mm_err(|error| SiaInitError::CouldNotGetBalance(error.to_string()))?;
        Ok(single_address_wallet_balance(address, balance))
    } else {
        coin.enable_coin_balance(xpub_extractor, EnableCoinScanPolicy::DoNotScan, 1, None)
            .await
            .mm_err(|error| SiaInitError::CouldNotGetBalance(error.to_string()))
    }
}

/// `HwConnectStatuses` mandates values even for the Trezor-connect states Sia can
/// never reach in practice (see `get_activation_result`'s doc comment) --
/// `SiaInProgressStatus` deliberately gains no new variants for them (unlike
/// `UtxoStandardInProgressStatus`'s `WaitingForTrezorToConnect`/
/// `WaitingForUserToConfirmPubkey`); the existing states are reused since these
/// slots are structurally required but never actually surfaced for Sia.
fn sia_xpub_extractor_rpc_statuses() -> HwConnectStatuses<SiaInProgressStatus, SiaAwaitingStatus> {
    HwConnectStatuses {
        on_connect: SiaInProgressStatus::ActivatingCoin,
        on_connected: SiaInProgressStatus::ActivatingCoin,
        on_connection_failed: SiaInProgressStatus::Finishing,
        on_button_request: SiaInProgressStatus::ActivatingCoin,
        on_pin_request: SiaAwaitingStatus::EnterTrezorPin,
        on_passphrase_request: SiaAwaitingStatus::EnterTrezorPassphrase,
        on_ready: SiaInProgressStatus::ActivatingCoin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{HttpStatusCode, StatusCode};
    use serde_json::json;

    fn task_error(err: SiaInitError) -> (Json, StatusCode) {
        let err: InitStandaloneCoinError = err.into();
        (serde_json::to_value(&err).unwrap(), err.status_code())
    }

    #[test]
    fn height_and_balance_failures_are_transport_errors_with_a_string() {
        // T46.8.2 (b)
        for err in [
            SiaInitError::CouldNotGetBlockCount("walletd consensus tip is unavailable".to_owned()),
            SiaInitError::CouldNotGetBalance("walletd balance is unavailable".to_owned()),
        ] {
            let (wire, status) = task_error(err);
            assert_eq!(wire["error_type"], "Transport");
            assert!(wire["error_data"].is_string());
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    #[test]
    fn unsupported_key_policy_is_a_coin_creation_error() {
        // T46.8.2 (c)
        let err = SiaInitError::from_new_err(SiaCoinNewError::UnsupportedPrivKeyPolicy, "SC".to_owned());
        let (wire, status) = task_error(err);
        assert_eq!(wire["error_type"], "CoinCreationError");
        assert_eq!(wire["error_data"]["ticker"], "SC");
        assert!(wire["error_data"]["error"].is_string());
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// CRD ch.51 R100 item 2: a `swap_evidence_node` other than the `server_url` is refused before any network
    /// access, with the wire form `{error_type, error_data: {ticker, node}}`.
    #[test]
    fn a_swap_evidence_node_outside_the_node_list_is_refused() {
        use coins::PrivKeyBuildPolicy;
        use common::block_on;
        use mm2_core::mm_ctx::MmCtxBuilder;
        let ctx = MmCtxBuilder::new().with_conf(json!({ "netid": 8762 })).into_mm_arc();
        crypto::CryptoCtx::init_with_global_hd_account(
            ctx.clone(),
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .expect("valid test mnemonic");
        let request: SiaCoinActivationRequest = serde_json::from_value(json!({
            "client_conf": { "server_url": "http://127.0.0.1:1/", "timeout": 1 },
            "swap_evidence_node": "other.example",
        }))
        .unwrap();
        let priv_key_policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap();
        let err = match block_on(SiaCoin::new(
            &ctx,
            json!({ "coin": "SC", "required_confirmations": 1 }),
            &request,
            priv_key_policy,
        )) {
            Err(err) => err,
            Ok(_) => panic!("the field is outside the node list"),
        };
        let (wire, status) = task_error(SiaInitError::from_new_err(err.into_inner(), "SC".to_owned()));
        assert_eq!(
            wire,
            json!({ "error_type": "SwapEvidenceNodeNotInList", "error_data": { "ticker": "SC", "node": "other.example" } })
        );
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn no_sia_error_exposes_a_forbidden_discriminant() {
        // R46.5.3
        let forbidden = [
            "PrivKeyNotAllowed",
            "UnexpectedDerivationMethod",
            "HwError",
            "HardwareWalletsAreNotSupportedYet",
            "CouldNotGetBalance",
            "CouldNotGetBlockCount",
        ];
        for err in [
            SiaInitError::CoinCreationError {
                ticker: "SC".to_owned(),
                error: "e".to_owned(),
            },
            SiaInitError::CoinIsAlreadyActivated {
                ticker: "SC".to_owned(),
            },
            SiaInitError::TaskTimedOut {
                duration: Duration::from_secs(1),
            },
            SiaInitError::CouldNotGetBalance("e".to_owned()),
            SiaInitError::CouldNotGetBlockCount("e".to_owned()),
            SiaInitError::Internal("e".to_owned()),
        ] {
            let (wire, _) = task_error(err);
            let name = wire["error_type"].as_str().unwrap().to_owned();
            assert!(!forbidden.contains(&name.as_str()), "{name} must not be observable");
        }
    }

    #[test]
    fn single_address_balance_switch_defaults_to_false_and_needs_a_boolean_true() {
        // T46.8.3: the two values of `sia_activation_single_address_balance`.
        assert!(!single_address_balance_requested(&json!({})));
        assert!(!single_address_balance_requested(
            &json!({ "sia_activation_single_address_balance": false })
        ));
        assert!(!single_address_balance_requested(
            &json!({ "sia_activation_single_address_balance": "true" })
        ));
        assert!(single_address_balance_requested(
            &json!({ "sia_activation_single_address_balance": true })
        ));
    }

    #[test]
    fn single_address_wallet_balance_has_the_iguana_wire_shape() {
        // R46.2.4: what the switch selects under HD is the single-address "Iguana" form.
        let balance = single_address_wallet_balance("addr:abc".to_owned(), CoinBalance::default());
        assert_eq!(
            serde_json::to_value(balance).unwrap(),
            json!({
                "wallet_type": "Iguana",
                "address": "addr:abc",
                "balance": { "spendable": "0", "unspendable": "0" }
            })
        );
    }

    /// A Siacoin built under a global-HD login against a mock walletd that serves the consensus tip and a balance of
    /// 5 SC for every address, and nothing else (an address's events, a scan, would be answered with a 404).
    #[cfg(not(target_arch = "wasm32"))]
    fn hd_sia_coin_on_a_mock_walletd() -> (mm2_core::mm_ctx::MmArc, SiaCoin) {
        use common::block_on;
        use mm2_core::mm_ctx::MmCtxBuilder;
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // 5 SC in hastings.
        const SPENDABLE_HASTINGS: &str = "5000000000000000000000000";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&chunk[..read]),
                    }
                }
                let request = String::from_utf8_lossy(&request).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or_default().to_owned();
                let (status, body) = if path.ends_with("/api/consensus/tip") {
                    ("200 OK", format!(r#"{{"height":7,"id":"{}"}}"#, "0".repeat(64)))
                } else if path.ends_with("/balance") {
                    (
                        "200 OK",
                        format!(r#"{{"siacoins":"{SPENDABLE_HASTINGS}","immatureSiacoins":"0"}}"#),
                    )
                } else {
                    ("404 Not Found", "not served".to_owned())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let ctx = MmCtxBuilder::new().with_conf(json!({ "netid": 8762 })).into_mm_arc();
        crypto::CryptoCtx::init_with_global_hd_account(
            ctx.clone(),
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .expect("valid test mnemonic");
        let request: SiaCoinActivationRequest = serde_json::from_value(json!({
            "client_conf": { "server_url": format!("http://127.0.0.1:{port}/"), "timeout": 5 },
        }))
        .unwrap();
        let priv_key_policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap();
        let coin = block_on(SiaCoin::new(
            &ctx,
            json!({ "coin": "SC", "required_confirmations": 1 }),
            &request,
            priv_key_policy,
        ))
        .expect("the coin is built against the mock walletd");
        (ctx, coin)
    }

    /// T46.8.3 (R46.2.4): under an HD context the default `wallet_balance` is the HD report with account 0 and
    /// exactly its address 0 -- the address `my_address()` answers -- and that address's balance. The mock
    /// walletd serves the consensus tip and the balance only; a request for an address's events (a scan) would
    /// be answered with a 404 and fail the activation.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn hd_activation_report_lists_account_0_with_its_single_address() {
        use async_trait::async_trait;
        use coins::hd_pubkey::HDExtractPubkeyError;
        use common::block_on;
        use crypto::trezor::utxo::TrezorUtxoCoin;
        use crypto::{DerivationPath, HardwareWalletArc, XPub};

        struct NoExtractor;

        #[async_trait]
        impl HDXPubExtractor for NoExtractor {
            async fn extract_utxo_xpub(
                &self,
                _trezor_utxo_coin: TrezorUtxoCoin,
                _derivation_path: DerivationPath,
            ) -> MmResult<XPub, HDExtractPubkeyError> {
                MmError::err(HDExtractPubkeyError::CoinDoesntSupportTrezor)
            }

            fn hardware_context(&self) -> Option<HardwareWalletArc> { None }
        }

        let (_ctx, coin) = hd_sia_coin_on_a_mock_walletd();

        let report = match block_on(activation_wallet_balance(&coin, &json!({}), None::<&NoExtractor>)) {
            Ok(report) => report,
            Err(e) => panic!("the HD report is not built: {e}"),
        };
        let report = serde_json::to_value(report).unwrap();
        assert_eq!(report["wallet_type"], "HD");
        let accounts = report["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["account_index"], 0);
        let addresses = accounts[0]["addresses"].as_array().unwrap();
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0]["address"], coin.my_address().unwrap());
        assert_eq!(
            addresses[0]["balance"]["SC"],
            json!({ "spendable": "5", "unspendable": "0" })
        );
    }

    /// `task::get_new_address` for a Siacoin under a software HD login (CRD ch.49 R49.43 item 5, a Reloaded extension
    /// the maintainer decided on 2026-11-22): the task is served on the software path, reaches `Ok` with the result
    /// shape of the direct method, shows only the software statuses, and records the address. The internal chain is
    /// refused as the direct method refuses it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_get_new_address_task_is_served_for_a_siacoin_on_the_software_path() {
        use coins::rpc_command::init_get_new_address::{init_get_new_address, init_get_new_address_status};
        use coins::{lp_register_coin, MmCoinEnum, RegisterCoinParams};
        use common::block_on;
        use rpc_task::rpc_common::RpcTaskStatusRequest;

        let (ctx, coin) = hd_sia_coin_on_a_mock_walletd();
        // Activation creates account 0 with its address 0.
        struct NoExtractor;
        #[async_trait]
        impl HDXPubExtractor for NoExtractor {
            async fn extract_utxo_xpub(
                &self,
                _trezor_utxo_coin: crypto::trezor::utxo::TrezorUtxoCoin,
                _derivation_path: crypto::DerivationPath,
            ) -> MmResult<crypto::XPub, coins::hd_pubkey::HDExtractPubkeyError> {
                MmError::err(coins::hd_pubkey::HDExtractPubkeyError::CoinDoesntSupportTrezor)
            }

            fn hardware_context(&self) -> Option<crypto::HardwareWalletArc> { None }
        }
        block_on(activation_wallet_balance(&coin, &json!({}), None::<&NoExtractor>)).unwrap_or_else(|e| panic!("{e}"));
        block_on(lp_register_coin(
            &ctx,
            MmCoinEnum::SiaCoin(coin.clone()),
            RegisterCoinParams {
                ticker: "SC".to_owned(),
                tx_history: false,
            },
        ))
        .unwrap_or_else(|e| panic!("{e}"));

        let run = |request: Json| -> (Json, Vec<Json>) {
            let request = serde_json::from_value(request).unwrap();
            let response = block_on(init_get_new_address(ctx.clone(), request)).unwrap_or_else(|e| panic!("{e}"));
            let mut seen = Vec::new();
            for _ in 0..4000 {
                let status = block_on(init_get_new_address_status(
                    ctx.clone(),
                    serde_json::from_value::<RpcTaskStatusRequest>(
                        json!({ "task_id": response.task_id, "forget_if_finished": false }),
                    )
                    .unwrap(),
                ))
                .unwrap_or_else(|e| panic!("{e}"));
                let status = serde_json::to_value(&status).unwrap();
                match status["status"].as_str() {
                    Some("Ok") | Some("Error") => return (status, seen),
                    _ => {
                        if !seen.contains(&status["details"]) {
                            seen.push(status["details"].clone());
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    },
                }
            }
            panic!("the task did not finish");
        };

        let (status, seen) = run(json!({ "coin": "SC", "account_id": 0 }));
        assert_eq!(status["status"], "Ok", "{status}");
        for state in &seen {
            assert!(
                *state == json!("Preparing") || *state == json!("RequestingAccountBalance"),
                "no state beyond the software ones, saw {state}"
            );
        }
        let new_address = &status["details"]["new_address"];
        assert_ne!(new_address["address"], coin.my_address().unwrap().as_str());
        assert_eq!(new_address["chain"], "External");
        assert_eq!(
            new_address["balance"]["SC"],
            json!({ "spendable": "5", "unspendable": "0" }),
            "{status}"
        );

        let (status, _) = run(json!({ "coin": "SC", "account_id": 0, "chain": "Internal" }));
        assert_eq!(status["status"], "Error", "{status}");
        assert_eq!(status["details"]["error_type"], "InvalidBip44Chain", "{status}");
    }
}
