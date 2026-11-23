//! `task::get_new_address` (CRD ch.49 §49.2.3, R49.38 -- R49.42), chosen by the coin's key policy (CRD ch.05 R54).
//!
//! Under the software policy the task derives, records and queries without any device state. Under the Trezor
//! policy the device is asked to show the derived address, the task waits for the button, and only then records
//! the address and queries its balance.

use crate::coin_balance::HDWalletBalanceOps;
use crate::hd_wallet::common_impl::{self as hd_wallet_common_impl, PlannedNewAddress};
use crate::hd_wallet::{require_hardware_context_for_trezor_policy_coin, GetNewHDAddressResponse, HDAddress,
                       HDWalletCoinOps, HDWalletRpcError};
use crate::utxo::{UtxoAddressFormat, UtxoCoinFields, UtxoCommonOps};
use crate::{lp_coinfind_or_err, CoinWithDerivationMethod, CoinsContext, MarketCoinOps, MmCoinEnum};
use async_trait::async_trait;
use common::SuccessResponse;
use crypto::hw_rpc_task::{HwConnectStatuses, HwRpcTaskAwaitingStatus, HwRpcTaskUserAction, HwRpcTaskUserActionRequest,
                          TrezorRpcTaskConnectProcessor};
use crypto::trezor::trezor_rpc_task::TrezorRpcTaskProcessor;
use crypto::trezor::utxo::{TrezorInputScriptType, TrezorUtxoCoin};
use crypto::trezor::ProcessTrezorResponse;
use crypto::{Bip44Chain, CryptoCtx, DerivationPath};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::rpc_common::{CancelRpcTaskError, InitRpcTaskResponse, RpcTaskStatusError, RpcTaskStatusRequest,
                           RpcTaskUserActionError};
use rpc_task::{RpcTask, RpcTaskError, RpcTaskHandle, RpcTaskManager, RpcTaskManagerShared, RpcTaskStatus, RpcTaskTypes};

pub type GetNewAddressUserAction = HwRpcTaskUserAction;
pub type GetNewAddressAwaitingStatus = HwRpcTaskAwaitingStatus;
pub type GetNewAddressTaskManager = RpcTaskManager<InitGetNewAddressTask>;
pub type GetNewAddressTaskManagerShared = RpcTaskManagerShared<InitGetNewAddressTask>;
pub type GetNewAddressTaskHandle = RpcTaskHandle<InitGetNewAddressTask>;
pub type GetNewAddressRpcTaskStatus = RpcTaskStatus<
    GetNewHDAddressResponse,
    HDWalletRpcError,
    GetNewAddressInProgressStatus,
    GetNewAddressAwaitingStatus,
>;

#[derive(Deserialize)]
pub struct InitGetNewAddressRequest {
    coin: String,
    #[serde(flatten)]
    params: InitGetNewAddressParams,
}

#[derive(Deserialize)]
pub struct InitGetNewAddressParams {
    account_id: u32,
    #[serde(default = "external_chain")]
    chain: Bip44Chain,
    /// The gap limit of the gap check; the wallet's when absent.
    gap_limit: Option<u32>,
}

fn external_chain() -> Bip44Chain { Bip44Chain::External }

/// The in-progress values of the task (CRD ch.49 R49.39 items 1, 2).
#[derive(Clone, Serialize)]
pub enum GetNewAddressInProgressStatus {
    /// From the start, through the checks that come before the device is asked.
    Preparing,
    /// The device shows the address and waits for the button; `expected_address` is the address the daemon derived,
    /// written as the `address` of the final result. Only a coin of the Trezor policy reaches it.
    ConfirmAddress { expected_address: String },
    /// The address is recorded (or, under the Trezor policy, the exchange with the device has ended) and its balance
    /// is requested.
    RequestingAccountBalance,
}

/// What the device is asked for (CRD ch.49 R49.42).
pub enum DeviceAddressRequest {
    /// The Bitcoin-family address request: the coin's device name and the script type that matches its address
    /// format.
    Utxo {
        trezor_coin: TrezorUtxoCoin,
        script_type: TrezorInputScriptType,
    },
    /// The Ethereum address request; no chain identifier is sent.
    Eth,
}

/// The steps of the task for one coin family. The task itself ([`InitGetNewAddressTask`]) is the same for every
/// family and runs the steps in the order of CRD ch.49 R49.41 item 1.
#[async_trait]
pub trait InitGetNewAddressRpcOps {
    /// The checks that need no device: the coin's HD mode, the account, the gap check, the address limit, and the
    /// derivation of the next address. Nothing is recorded.
    async fn plan_new_address(
        &self,
        account_id: u32,
        chain: Bip44Chain,
        gap_limit: Option<u32>,
    ) -> MmResult<PlannedNewAddress, HDWalletRpcError>;

    /// What the device is asked for under the Trezor policy. A coin whose configuration has no device name answers
    /// `Internal` (CRD ch.49 R49.41 item 3).
    fn device_address_request(&self) -> MmResult<DeviceAddressRequest, HDWalletRpcError>;

    /// Records the planned address, calls `on_recorded`, and requests its balance. The answer is the one of the
    /// direct `get_new_address`.
    async fn record_new_address(
        &self,
        planned: &PlannedNewAddress,
        on_recorded: &(dyn Fn() -> MmResult<(), HDWalletRpcError> + Sync),
    ) -> MmResult<GetNewHDAddressResponse, HDWalletRpcError>;
}

pub struct InitGetNewAddressTask {
    ctx: MmArc,
    coin: MmCoinEnum,
    req: InitGetNewAddressRequest,
}

impl RpcTaskTypes for InitGetNewAddressTask {
    type Item = GetNewHDAddressResponse;
    type Error = HDWalletRpcError;
    type InProgressStatus = GetNewAddressInProgressStatus;
    type AwaitingStatus = GetNewAddressAwaitingStatus;
    type UserAction = GetNewAddressUserAction;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RpcTask for InitGetNewAddressTask {
    fn initial_status(&self) -> Self::InProgressStatus { GetNewAddressInProgressStatus::Preparing }

    // No cancel cleanup: a cancel before the confirmation leaves nothing recorded, and one after it leaves the
    // address known (CRD ch.49 R49.40).

    async fn run(self, task_handle: &GetNewAddressTaskHandle) -> Result<Self::Item, MmError<Self::Error>> {
        let InitGetNewAddressTask { ctx, coin, req } = self;
        let params = req.params;
        match coin.clone() {
            MmCoinEnum::UtxoCoin(utxo) => get_new_address_helper(&ctx, &coin, &utxo, params, task_handle).await,
            MmCoinEnum::QtumCoin(qtum) => get_new_address_helper(&ctx, &coin, &qtum, params, task_handle).await,
            MmCoinEnum::EthCoin(eth) => get_new_address_helper(&ctx, &coin, &eth, params, task_handle).await,
            MmCoinEnum::SiaCoin(sia) => get_new_address_helper(&ctx, &coin, &sia, params, task_handle).await,
            _ => MmError::err(HDWalletRpcError::CoinIsActivatedNotWithHDWallet),
        }
    }
}

/// The task body for a served coin family, in the order of CRD ch.49 R49.41 item 1: the family (done by the caller),
/// the hardware context (a coin of the Trezor policy only), the coin's HD mode, the account, the gap check, the address
/// limit, the device coin name, the device, the recording, the balance.
async fn get_new_address_helper<Coin>(
    ctx: &MmArc,
    coin_enum: &MmCoinEnum,
    coin: &Coin,
    params: InitGetNewAddressParams,
    task_handle: &GetNewAddressTaskHandle,
) -> MmResult<GetNewHDAddressResponse, HDWalletRpcError>
where
    Coin: InitGetNewAddressRpcOps + Sync,
{
    // A coin of any other policy never consults the hardware context, whether or not it exists (CRD ch.05 R54).
    require_hardware_context_for_trezor_policy_coin(ctx, coin_enum)?;
    let uses_device = coin_enum.uses_trezor_policy();

    let planned = coin
        .plan_new_address(params.account_id, params.chain, params.gap_limit)
        .await?;

    if uses_device {
        let request = coin.device_address_request()?;
        confirm_on_device(ctx, task_handle, &planned, request).await?;
    }

    let on_recorded = || -> MmResult<(), HDWalletRpcError> {
        task_handle
            .update_in_progress_status(GetNewAddressInProgressStatus::RequestingAccountBalance)
            .mm_err(rpc_task_error)
    };
    coin.record_new_address(&planned, &on_recorded).await
}

/// The error of the task framework as the task answers it: a user action of the other type is
/// `UnexpectedUserAction` (CRD ch.49 R49.39 item 4), the others are as everywhere else.
fn rpc_task_error(e: RpcTaskError) -> HDWalletRpcError {
    match e {
        RpcTaskError::UnexpectedUserAction { expected } => HDWalletRpcError::UnexpectedUserAction {
            expected: expected.to_owned(),
        },
        other => HDWalletRpcError::from(other),
    }
}

/// The hardware error classes of the device exchange, as `HDWalletRpcError`.
fn hd_error(e: crate::hd_pubkey::HDExtractPubkeyError) -> HDWalletRpcError {
    match e {
        crate::hd_pubkey::HDExtractPubkeyError::RpcTaskError(rpc) => rpc_task_error(rpc),
        other => HDWalletRpcError::from(other),
    }
}

/// Asks the device to show the planned address and waits for the button; the address the device answers must be the
/// planned one (CRD ch.49 R49.39 items 2, 3, R49.42). Nothing is recorded here.
async fn confirm_on_device(
    ctx: &MmArc,
    task_handle: &GetNewAddressTaskHandle,
    planned: &PlannedNewAddress,
    request: DeviceAddressRequest,
) -> MmResult<(), HDWalletRpcError> {
    use crate::hd_pubkey::HDExtractPubkeyError;

    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| HDWalletRpcError::Internal(e.to_string()))?;
    let hw_ctx = crypto_ctx
        .hw_ctx()
        .or_mm_err(|| HDWalletRpcError::HwContextNotInitialized)?;

    let statuses = HwConnectStatuses {
        // The task does not reconnect a device (CRD ch.49 R49.39 item 1): these three are only reached when the
        // connected device needs checking again, and they keep the task in its preparing state.
        on_connect: GetNewAddressInProgressStatus::Preparing,
        on_connected: GetNewAddressInProgressStatus::Preparing,
        on_connection_failed: GetNewAddressInProgressStatus::Preparing,
        on_button_request: GetNewAddressInProgressStatus::ConfirmAddress {
            expected_address: planned.address.clone(),
        },
        on_pin_request: GetNewAddressAwaitingStatus::EnterTrezorPin,
        on_passphrase_request: GetNewAddressAwaitingStatus::EnterTrezorPassphrase,
        on_ready: GetNewAddressInProgressStatus::RequestingAccountBalance,
    };
    let connect_processor = TrezorRpcTaskConnectProcessor::new(task_handle, statuses.clone());
    let trezor = hw_ctx
        .trezor(&connect_processor)
        .await
        .mm_err(|e| hd_error(HDExtractPubkeyError::from(e)))?;
    let mut session = trezor
        .session()
        .await
        .mm_err(|e| hd_error(HDExtractPubkeyError::from(e)))?;

    let address_processor = TrezorRpcTaskProcessor::new(task_handle, statuses.to_trezor_request_statuses());
    let path: DerivationPath = planned.derivation_path.clone();
    let (device_address, ignore_case) = match request {
        DeviceAddressRequest::Utxo {
            trezor_coin,
            script_type,
        } => (
            session
                .get_utxo_address(path, trezor_coin, script_type, true)
                .await
                .mm_err(|e| hd_error(HDExtractPubkeyError::from(e)))?
                .process(&address_processor)
                .await
                .mm_err(|e| hd_error(HDExtractPubkeyError::from(e)))?,
            false,
        ),
        // The two spellings of an EVM address differ in the letter case of the checksum only.
        DeviceAddressRequest::Eth => (
            session
                .get_eth_address(path, true)
                .await
                .mm_err(|e| hd_error(HDExtractPubkeyError::from(e)))?
                .process(&address_processor)
                .await
                .mm_err(|e| hd_error(HDExtractPubkeyError::from(e)))?,
            true,
        ),
    };

    let same = if ignore_case {
        device_address.eq_ignore_ascii_case(&planned.address)
    } else {
        device_address == planned.address
    };
    if !same {
        return MmError::err(HDWalletRpcError::DeviceAddressMismatch {
            expected_address: planned.address.clone(),
            device_address,
        });
    }
    Ok(())
}

pub async fn init_get_new_address(
    ctx: MmArc,
    req: InitGetNewAddressRequest,
) -> MmResult<InitRpcTaskResponse, HDWalletRpcError> {
    // Init answers only `NoSuchCoin` and `Internal`; every other condition is a status of the task (CRD ch.49
    // R49.38 item 3).
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(HDWalletRpcError::Internal)?;
    let task = InitGetNewAddressTask { ctx, coin, req };
    let task_id = GetNewAddressTaskManager::spawn_rpc_task(&coins_ctx.get_new_address_manager, task)
        .mm_err(|e| HDWalletRpcError::Internal(e.to_string()))?;
    Ok(InitRpcTaskResponse { task_id })
}

pub async fn init_get_new_address_status(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<GetNewAddressRpcTaskStatus, RpcTaskStatusError> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(RpcTaskStatusError::Internal)?;
    let mut task_manager = coins_ctx
        .get_new_address_manager
        .lock()
        .map_to_mm(|e| RpcTaskStatusError::Internal(e.to_string()))?;
    task_manager
        .task_status(req.task_id, req.forget_if_finished)
        .or_mm_err(|| RpcTaskStatusError::NoSuchTask(req.task_id))
}

/// Cancels an in-progress `task::get_new_address`. The task has no clean-up step: nothing is recorded before the
/// device confirmed, and an address recorded after it stays known (CRD ch.49 R49.40).
pub async fn init_get_new_address_cancel(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<SuccessResponse, CancelRpcTaskError> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(CancelRpcTaskError::Internal)?;
    let mut task_manager = coins_ctx
        .get_new_address_manager
        .lock()
        .map_to_mm(|e| CancelRpcTaskError::Internal(e.to_string()))?;
    task_manager.cancel_task(req.task_id).mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}

pub async fn init_get_new_address_user_action(
    ctx: MmArc,
    req: HwRpcTaskUserActionRequest,
) -> MmResult<SuccessResponse, RpcTaskUserActionError> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(RpcTaskUserActionError::Internal)?;
    let mut task_manager = coins_ctx
        .get_new_address_manager
        .lock()
        .map_to_mm(|e| RpcTaskUserActionError::Internal(e.to_string()))?;
    task_manager
        .on_user_action(req.task_id, req.user_action)
        .mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}

/// The steps of the families that write an address with its `Display` and report it as the direct method does:
/// the UTXO coins, QTUM and Siacoin.
pub(crate) mod common_impl {
    use super::*;

    pub async fn plan_new_address<Coin>(
        coin: &Coin,
        account_id: u32,
        chain: Bip44Chain,
        gap_limit: Option<u32>,
    ) -> MmResult<PlannedNewAddress, HDWalletRpcError>
    where
        Coin: HDWalletBalanceOps + CoinWithDerivationMethod<HDWallet = <Coin as HDWalletCoinOps>::HDWallet> + Sync,
        <Coin as HDWalletCoinOps>::Address: std::fmt::Display,
    {
        hd_wallet_common_impl::plan_new_address(
            coin,
            account_id,
            chain,
            gap_limit,
            coin.produce_hd_address_scanner(),
            |address| address.to_string(),
        )
        .await
    }

    pub async fn record_new_address<Coin>(
        coin: &Coin,
        planned: &PlannedNewAddress,
        on_recorded: &(dyn Fn() -> MmResult<(), HDWalletRpcError> + Sync),
    ) -> MmResult<GetNewHDAddressResponse, HDWalletRpcError>
    where
        Coin: HDWalletBalanceOps
            + CoinWithDerivationMethod<HDWallet = <Coin as HDWalletCoinOps>::HDWallet>
            + MarketCoinOps
            + Sync,
        <Coin as HDWalletCoinOps>::Address: std::fmt::Display,
    {
        let HDAddress {
            address,
            derivation_path,
            ..
        } = hd_wallet_common_impl::record_next_address(
            coin,
            planned.account_id,
            planned.chain,
            Some(planned.address_id),
        )
        .await?;
        on_recorded()?;
        hd_wallet_common_impl::report_new_address(coin, &address, derivation_path, planned.chain).await
    }

    /// The device request of a UTXO coin: its device name, and the script type that matches its address format
    /// (pay-to-public-key-hash for the legacy and CashAddress formats, native SegWit for the SegWit format).
    pub fn utxo_device_address_request<Coin>(coin: &Coin) -> MmResult<DeviceAddressRequest, HDWalletRpcError>
    where
        Coin: UtxoCommonOps + AsRef<UtxoCoinFields>,
    {
        let fields = coin.as_ref();
        let trezor_coin = fields.conf.trezor_coin.or_mm_err(|| {
            HDWalletRpcError::Internal(format!(
                "Coin {} does not support the Trezor hardware wallet: its configuration has no 'trezor_coin'",
                fields.conf.ticker
            ))
        })?;
        let script_type = match coin.addr_format() {
            UtxoAddressFormat::Segwit => TrezorInputScriptType::SpendWitness,
            UtxoAddressFormat::Standard | UtxoAddressFormat::CashAddress { .. } => TrezorInputScriptType::SpendAddress,
        };
        Ok(DeviceAddressRequest::Utxo {
            trezor_coin,
            script_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::HttpStatusCode;
    use http::StatusCode;
    use serde_json::json;
    use std::time::Duration;

    /// CRD ch.49 R49.38 item 1: `account_id` is required, `chain` defaults to the external chain, and `gap_limit` to
    /// the wallet's (`None` here). `client_id` and any other member are not the task's business.
    #[test]
    fn the_request_has_the_members_of_the_contract() {
        let request: InitGetNewAddressRequest =
            serde_json::from_value(json!({ "coin": "ETH", "account_id": 2, "client_id": 7 })).unwrap();
        assert_eq!(request.coin, "ETH");
        assert_eq!(request.params.account_id, 2);
        assert!(matches!(request.params.chain, Bip44Chain::External));
        assert_eq!(request.params.gap_limit, None);

        let request: InitGetNewAddressRequest =
            serde_json::from_value(json!({ "coin": "ETH", "account_id": 0, "chain": "Internal", "gap_limit": 5 }))
                .unwrap();
        assert!(matches!(request.params.chain, Bip44Chain::Internal));
        assert_eq!(request.params.gap_limit, Some(5));

        assert!(serde_json::from_value::<InitGetNewAddressRequest>(json!({ "coin": "ETH" })).is_err());
    }

    /// CRD ch.49 R49.39 items 1, 2: the in-progress values are bare strings, and the confirmation state is an object
    /// that carries the expected address.
    #[test]
    fn the_in_progress_values_have_the_wire_form_of_the_contract() {
        assert_eq!(
            serde_json::to_value(GetNewAddressInProgressStatus::Preparing).unwrap(),
            json!("Preparing")
        );
        assert_eq!(
            serde_json::to_value(GetNewAddressInProgressStatus::RequestingAccountBalance).unwrap(),
            json!("RequestingAccountBalance")
        );
        assert_eq!(
            serde_json::to_value(GetNewAddressInProgressStatus::ConfirmAddress {
                expected_address: "0xAbC".to_owned()
            })
            .unwrap(),
            json!({ "ConfirmAddress": { "expected_address": "0xAbC" } })
        );
    }

    /// CRD ch.49 R49.39 item 3 and the first code-quality finding (decision 4): an address that differs from the
    /// expected one after the button press is a typed request error naming both addresses, HTTP 400.
    #[test]
    fn a_device_address_mismatch_is_a_typed_request_error_naming_both_addresses() {
        let error = HDWalletRpcError::DeviceAddressMismatch {
            expected_address: "expected".to_owned(),
            device_address: "device".to_owned(),
        };
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({
                "error_type": "DeviceAddressMismatch",
                "error_data": { "expected_address": "expected", "device_address": "device" }
            })
        );
    }

    /// CRD ch.49 R49.41 item 1: the gap check's refusal carries the limit, HTTP 400.
    #[test]
    fn the_gap_check_refusal_carries_the_limit() {
        let error = HDWalletRpcError::EmptyAddressesLimitReached { gap_limit: 20 };
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "error_type": "EmptyAddressesLimitReached", "error_data": { "gap_limit": 20 } })
        );
    }

    /// CRD ch.49 R49.39 item 4: a user action of the other type ends the task with `UnexpectedUserAction` and the
    /// expected one, and a wait that is not answered ends it with `Timeout`.
    #[test]
    fn the_awaiting_errors_of_the_task_framework_have_their_own_error_types() {
        let error = rpc_task_error(RpcTaskError::UnexpectedUserAction { expected: "TrezorPin" });
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "error_type": "UnexpectedUserAction", "error_data": { "expected": "TrezorPin" } })
        );

        let error = rpc_task_error(RpcTaskError::Timeout(Duration::from_secs(300)));
        assert_eq!(error.status_code(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "error_type": "Timeout", "error_data": { "secs": 300, "nanos": 0 } })
        );
    }

    /// CRD ch.49 R49.41 item 4 through ch.05 R35c's vocabulary: the user's refusal on the device is `HwError`
    /// `UserCancelled`, a wrong PIN `InvalidPin`, an unplugged device `NoTrezorDeviceAvailable`.
    #[test]
    fn the_device_answers_map_to_the_hardware_vocabulary() {
        use crypto::trezor::HwRpcError;
        use crypto::HwError;

        for (error, word) in [
            (HwError::DeviceFailure(HwRpcError::UserCancelled), "UserCancelled"),
            (HwError::DeviceFailure(HwRpcError::InvalidPin), "InvalidPin"),
            (HwError::DeviceDisconnected, "NoTrezorDeviceAvailable"),
        ] {
            let wire = serde_json::to_value(HDWalletRpcError::from(error)).unwrap();
            assert_eq!(wire, json!({ "error_type": "HwError", "error_data": word }));
        }
    }
}
