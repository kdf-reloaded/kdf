use crate::coin_balance::HDAccountBalance;
use crate::rpc_command::hd_account_balance_rpc_error::HDAccountBalanceRpcError;
use crate::{lp_coinfind_or_err, CoinsContext, MmCoinEnum};
use async_trait::async_trait;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::rpc_common::{InitRpcTaskResponse, RpcTaskStatusError, RpcTaskStatusRequest};
use rpc_task::{RpcTask, RpcTaskHandle, RpcTaskManager, RpcTaskManagerShared, RpcTaskStatus, RpcTaskTypes};

pub type AccountBalanceTaskManager = RpcTaskManager<InitAccountBalanceTask>;
pub type AccountBalanceTaskManagerShared = RpcTaskManagerShared<InitAccountBalanceTask>;
pub type AccountBalanceTaskHandle = RpcTaskHandle<InitAccountBalanceTask>;
pub type AccountBalanceRpcTaskStatus = RpcTaskStatus<
    HDAccountBalance,
    HDAccountBalanceRpcError,
    AccountBalanceInProgressStatus,
    AccountBalanceAwaitingStatus,
>;

#[derive(Deserialize)]
pub struct InitAccountBalanceRequest {
    coin: String,
    #[serde(flatten)]
    params: InitAccountBalanceParams,
}

#[derive(Clone, Deserialize)]
pub struct InitAccountBalanceParams {
    pub account_index: u32,
}

#[derive(Clone, Serialize)]
pub enum AccountBalanceInProgressStatus {
    RequestingAccountBalance,
}

/// Compatibility wire format for `task::account_balance::status` expected by the SDK.
///
/// The SDK expects `"Ok"` / `"Error"` / `"InProgress"` as status values, and `details`
/// to be a flat object (not wrapped in `{"result": ...}`).
#[derive(Serialize)]
#[serde(tag = "status", content = "details")]
pub enum AccountBalanceCompatStatus {
    /// Task completed successfully; `details` is the flat `HDAccountBalance` JSON object.
    Ok(HDAccountBalance),
    /// Task failed; `details` is a human-readable error string.
    Error(String),
    InProgress(AccountBalanceInProgressStatus),
}

/// We can't use `std::convert::Infallible` as [`RpcTaskTypes::UserAction`] because it doesn't implement `Serialize`.
/// Use `!` when it's stable.
#[derive(Clone, Serialize)]
pub enum AccountBalanceUserAction {}

/// We can't use `std::convert::Infallible` as [`RpcTaskTypes::AwaitingStatus`] because it doesn't implement `Serialize`.
/// Use `!` when it's stable.
#[derive(Clone, Serialize)]
pub enum AccountBalanceAwaitingStatus {}

#[async_trait]
pub trait InitAccountBalanceRpcOps {
    async fn init_account_balance_rpc(
        &self,
        params: InitAccountBalanceParams,
    ) -> MmResult<HDAccountBalance, HDAccountBalanceRpcError>;
}

pub struct InitAccountBalanceTask {
    coin: MmCoinEnum,
    req: InitAccountBalanceRequest,
}

impl RpcTaskTypes for InitAccountBalanceTask {
    type Item = HDAccountBalance;
    type Error = HDAccountBalanceRpcError;
    type InProgressStatus = AccountBalanceInProgressStatus;
    type AwaitingStatus = AccountBalanceAwaitingStatus;
    type UserAction = AccountBalanceUserAction;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RpcTask for InitAccountBalanceTask {
    fn initial_status(&self) -> Self::InProgressStatus { AccountBalanceInProgressStatus::RequestingAccountBalance }

    async fn run(self, _task_handle: &AccountBalanceTaskHandle) -> Result<Self::Item, MmError<Self::Error>> {
        match self.coin {
            MmCoinEnum::UtxoCoin(utxo) => utxo.init_account_balance_rpc(self.req.params).await,
            MmCoinEnum::QtumCoin(qtum) => qtum.init_account_balance_rpc(self.req.params).await,
            MmCoinEnum::EthCoin(eth) => eth.init_account_balance_rpc(self.req.params).await,
            _ => MmError::err(HDAccountBalanceRpcError::CoinIsActivatedNotWithHDWallet),
        }
    }
}

pub async fn init_account_balance(
    ctx: MmArc,
    req: InitAccountBalanceRequest,
) -> MmResult<InitRpcTaskResponse, HDAccountBalanceRpcError> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(HDAccountBalanceRpcError::Internal)?;
    let task = InitAccountBalanceTask { coin, req };
    let task_id =
        AccountBalanceTaskManager::spawn_rpc_task(&coins_ctx.account_balance_task_manager, task).mm_err(Into::into)?;
    Ok(InitRpcTaskResponse { task_id })
}

pub async fn init_account_balance_status(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<AccountBalanceCompatStatus, RpcTaskStatusError> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(RpcTaskStatusError::Internal)?;
    let mut task_manager = coins_ctx
        .account_balance_task_manager
        .lock()
        .map_to_mm(|e| RpcTaskStatusError::Internal(e.to_string()))?;
    let raw_status = task_manager
        .task_status(req.task_id, req.forget_if_finished)
        .or_mm_err(|| RpcTaskStatusError::NoSuchTask(req.task_id))?;
    let compat = match raw_status {
        RpcTaskStatus::Ok(balance) => AccountBalanceCompatStatus::Ok(balance),
        RpcTaskStatus::Error(e) => AccountBalanceCompatStatus::Error(format!("{}", e.get_inner())),
        RpcTaskStatus::InProgress(s) => AccountBalanceCompatStatus::InProgress(s),
        RpcTaskStatus::UserActionRequired(_) => {
            AccountBalanceCompatStatus::Error("Unexpected user action required".to_owned())
        },
    };
    Ok(compat)
}

pub(crate) mod common_impl {
    use super::*;
    use crate::coin_balance::HDWalletBalanceOps;
    use crate::hd_wallet::{HDAccountOps, HDWalletCoinOps, HDWalletOps};
    use crate::{CoinWithDerivationMethod, MarketCoinOps};
    use crypto::RpcDerivationPath;
    use std::fmt;

    pub async fn init_account_balance_rpc<Coin>(
        coin: &Coin,
        params: InitAccountBalanceParams,
    ) -> MmResult<HDAccountBalance, HDAccountBalanceRpcError>
    where
        Coin: HDWalletBalanceOps
            + CoinWithDerivationMethod<HDWallet = <Coin as HDWalletCoinOps>::HDWallet>
            + MarketCoinOps
            + Sync,
        <Coin as HDWalletCoinOps>::Address: fmt::Display + Clone,
    {
        let account_id = params.account_index;
        let hd_wallet = coin.derivation_method().hd_wallet_or_err().mm_err(Into::into)?;
        let hd_account = hd_wallet
            .get_account(account_id)
            .await
            .or_mm_err(|| HDAccountBalanceRpcError::UnknownAccount { account_id })?;

        let addresses = coin
            .all_known_addresses_balances(&hd_account)
            .await
            .mm_err(Into::into)?;

        let total_balance = crate::coin_balance::sum_hd_address_balances(&addresses);

        Ok(HDAccountBalance {
            account_index: account_id,
            derivation_path: RpcDerivationPath(hd_account.account_derivation_path()),
            total_balance,
            addresses,
        })
    }
}
