use crate::coin_balance::HDAccountBalance;
use crate::hd_pubkey::{HDXPubExtractor, RpcTaskXPubExtractor};
use crate::hd_wallet::common_impl as hd_wallet_common_impl;
use crate::hd_wallet::{HDWalletRpcError, NewAccountIndex};
use crate::{lp_coinfind_or_err, CoinWithDerivationMethod, CoinsContext, MmCoinEnum};
use async_trait::async_trait;
use common::log::warn;
use common::{true_f, SuccessResponse};
use crypto::hw_rpc_task::{HwConnectStatuses, HwRpcTaskAwaitingStatus, HwRpcTaskUserAction, HwRpcTaskUserActionRequest};
use crypto::{CryptoCtx, RpcDerivationPath};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::rpc_common::{CancelRpcTaskError, InitRpcTaskResponse, RpcTaskStatusError, RpcTaskStatusRequest,
                           RpcTaskUserActionError};
use rpc_task::{CancelCleanup, RpcTask, RpcTaskHandle, RpcTaskManager, RpcTaskManagerShared, RpcTaskStatus,
               RpcTaskTypes};
use std::sync::{Arc, Mutex};

pub type CreateAccountUserAction = HwRpcTaskUserAction;
pub type CreateAccountAwaitingStatus = HwRpcTaskAwaitingStatus;
pub type CreateAccountTaskManager = RpcTaskManager<InitCreateAccountTask>;
pub type CreateAccountTaskManagerShared = RpcTaskManagerShared<InitCreateAccountTask>;
pub type CreateAccountTaskHandle = RpcTaskHandle<InitCreateAccountTask>;
pub type CreateAccountRpcTaskStatus =
    RpcTaskStatus<HDAccountBalance, HDWalletRpcError, CreateAccountInProgressStatus, CreateAccountAwaitingStatus>;

type CreateAccountXPubExtractor<'task> = RpcTaskXPubExtractor<'task, InitCreateAccountTask>;

#[derive(Deserialize)]
pub struct CreateNewAccountRequest {
    coin: String,
    #[serde(flatten)]
    params: CreateNewAccountParams,
}

#[derive(Deserialize)]
pub struct CreateNewAccountParams {
    #[serde(default = "true_f")]
    pub(crate) scan: bool,
    pub(crate) gap_limit: Option<u32>,
    /// The index of the new account. When absent, the lowest index not in use (CRD ch.05 R35c item 1).
    #[serde(default)]
    pub(crate) account_id: Option<u32>,
}

impl CreateNewAccountParams {
    /// How the index of the new account is chosen.
    pub(crate) fn account_index(&self) -> NewAccountIndex {
        match self.account_id {
            Some(account_id) => NewAccountIndex::Requested(account_id),
            None => NewAccountIndex::LowestUnused,
        }
    }
}

/// The account a `task::create_new_account` task has created, shared between the task and its cancellation
/// cleanup, so that a cancelled task can remove the account again (CRD ch.05 R35c item 4).
#[derive(Clone, Default)]
pub struct CreatedAccount(Arc<Mutex<Option<u32>>>);

impl CreatedAccount {
    /// Records the account the task created.
    pub fn set(&self, account_id: u32) {
        if let Ok(mut created) = self.0.lock() {
            *created = Some(account_id);
        }
    }

    /// The account the task created, if it created one.
    pub fn get(&self) -> Option<u32> { self.0.lock().ok().and_then(|created| *created) }
}

#[derive(Clone, Serialize)]
pub enum CreateAccountInProgressStatus {
    Preparing,
    RequestingAccountBalance,
    Finishing,
    /// The following statuses don't require the user to send `UserAction`,
    /// but they tell the user that he should confirm/decline the operation on his device.
    WaitingForTrezorToConnect,
    WaitingForUserToConfirmPubkey,
}

#[async_trait]
pub trait InitCreateHDAccountRpcOps {
    /// Creates the account and scans it. `xpub_extractor` is the hardware device that provides the account's
    /// extended public key under a hardware login; under a software login it is `None` and the key is derived in
    /// software (CRD ch.05 R35c). The index of the account is recorded in `created_account` once it exists.
    async fn init_create_account_rpc<XPubExtractor>(
        &self,
        params: CreateNewAccountParams,
        xpub_extractor: Option<&XPubExtractor>,
        created_account: &CreatedAccount,
    ) -> MmResult<HDAccountBalance, HDWalletRpcError>
    where
        XPubExtractor: HDXPubExtractor + Sync;
}

pub struct InitCreateAccountTask {
    ctx: MmArc,
    coin: MmCoinEnum,
    req: CreateNewAccountRequest,
    /// The account this task created, for the cancellation cleanup.
    created_account: CreatedAccount,
}

impl RpcTaskTypes for InitCreateAccountTask {
    type Item = HDAccountBalance;
    type Error = HDWalletRpcError;
    type InProgressStatus = CreateAccountInProgressStatus;
    type AwaitingStatus = CreateAccountAwaitingStatus;
    type UserAction = CreateAccountUserAction;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RpcTask for InitCreateAccountTask {
    fn initial_status(&self) -> Self::InProgressStatus { CreateAccountInProgressStatus::Preparing }

    /// Cancelling after the account was created removes it again if it is the wallet's last account (CRD ch.05
    /// R35c item 4).
    fn cancel_cleanup(&self) -> Option<CancelCleanup> {
        let coin = self.coin.clone();
        let created_account = self.created_account.clone();
        Some(Box::new(move || {
            Box::pin(async move {
                let account_id = match created_account.get() {
                    Some(account_id) => account_id,
                    None => return,
                };
                let removed = match &coin {
                    MmCoinEnum::UtxoCoin(utxo) => hd_wallet_common_impl::remove_created_account(utxo, account_id).await,
                    MmCoinEnum::QtumCoin(qtum) => hd_wallet_common_impl::remove_created_account(qtum, account_id).await,
                    MmCoinEnum::EthCoin(eth) => hd_wallet_common_impl::remove_created_account(eth, account_id).await,
                    MmCoinEnum::SiaCoin(sia) => {
                        crate::siacoin::sia_hd_wallet::remove_created_account(sia, account_id).await
                    },
                    _ => return,
                };
                if let Err(error) = removed {
                    warn!(
                        "Cannot remove the account '{}' of the cancelled 'create_new_account' task: {}",
                        account_id, error
                    );
                }
            })
        }))
    }

    async fn run(self, task_handle: &CreateAccountTaskHandle) -> Result<Self::Item, MmError<Self::Error>> {
        async fn create_new_account_helper<Coin>(
            ctx: &MmArc,
            coin: Coin,
            params: CreateNewAccountParams,
            created_account: &CreatedAccount,
            task_handle: &CreateAccountTaskHandle,
        ) -> MmResult<HDAccountBalance, HDWalletRpcError>
        where
            Coin: InitCreateHDAccountRpcOps + Send + Sync,
        {
            // Under a hardware login the device provides the account's extended public key. Under a software
            // login no device is sought and the key is derived in software (CRD ch.05 R35c).
            let xpub_extractor = if uses_hardware_wallet(ctx)? {
                let hw_statuses = HwConnectStatuses {
                    on_connect: CreateAccountInProgressStatus::WaitingForTrezorToConnect,
                    on_connected: CreateAccountInProgressStatus::Preparing,
                    on_connection_failed: CreateAccountInProgressStatus::Finishing,
                    on_button_request: CreateAccountInProgressStatus::WaitingForUserToConfirmPubkey,
                    on_pin_request: CreateAccountAwaitingStatus::EnterTrezorPin,
                    on_passphrase_request: CreateAccountAwaitingStatus::EnterTrezorPassphrase,
                    on_ready: CreateAccountInProgressStatus::RequestingAccountBalance,
                };
                Some(CreateAccountXPubExtractor::new(ctx, task_handle, hw_statuses).mm_err(Into::into)?)
            } else {
                None
            };
            coin.init_create_account_rpc(params, xpub_extractor.as_ref(), created_account)
                .await
        }

        match self.coin {
            MmCoinEnum::UtxoCoin(utxo) => {
                create_new_account_helper(&self.ctx, utxo, self.req.params, &self.created_account, task_handle).await
            },
            MmCoinEnum::QtumCoin(qtum) => {
                create_new_account_helper(&self.ctx, qtum, self.req.params, &self.created_account, task_handle).await
            },
            MmCoinEnum::EthCoin(eth) => {
                create_new_account_helper(&self.ctx, eth, self.req.params, &self.created_account, task_handle).await
            },
            MmCoinEnum::SiaCoin(sia) => {
                create_new_account_helper(&self.ctx, sia, self.req.params, &self.created_account, task_handle).await
            },
            _ => MmError::err(HDWalletRpcError::CoinIsActivatedNotWithHDWallet),
        }
    }
}

/// Whether the account's extended public key comes from a hardware device: the node runs under a hardware login
/// (CRD ch.05 R35c). Under a software login no hardware context exists, and none is needed.
pub(crate) fn uses_hardware_wallet(ctx: &MmArc) -> MmResult<bool, HDWalletRpcError> {
    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| HDWalletRpcError::Internal(e.to_string()))?;
    Ok(crypto_ctx.hw_ctx().is_some())
}

pub async fn init_create_new_account(
    ctx: MmArc,
    req: CreateNewAccountRequest,
) -> MmResult<InitRpcTaskResponse, HDWalletRpcError> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(HDWalletRpcError::Internal)?;
    let task = InitCreateAccountTask {
        ctx,
        coin,
        req,
        created_account: CreatedAccount::default(),
    };
    let task_id =
        CreateAccountTaskManager::spawn_rpc_task(&coins_ctx.create_account_manager, task).mm_err(Into::into)?;
    Ok(InitRpcTaskResponse { task_id })
}

pub async fn init_create_new_account_status(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<CreateAccountRpcTaskStatus, RpcTaskStatusError> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(RpcTaskStatusError::Internal)?;
    let mut task_manager = coins_ctx
        .create_account_manager
        .lock()
        .map_to_mm(|e| RpcTaskStatusError::Internal(e.to_string()))?;
    task_manager
        .task_status(req.task_id, req.forget_if_finished)
        .or_mm_err(|| RpcTaskStatusError::NoSuchTask(req.task_id))
}

/// Cancels an in-progress `task::create_new_account`. An account the task had already created is removed again
/// once the cancelled task has stopped, if it is the wallet's last account (CRD ch.05 R35c item 4).
pub async fn init_create_new_account_cancel(
    ctx: MmArc,
    req: RpcTaskStatusRequest,
) -> MmResult<SuccessResponse, CancelRpcTaskError> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(CancelRpcTaskError::Internal)?;
    let mut task_manager = coins_ctx
        .create_account_manager
        .lock()
        .map_to_mm(|e| CancelRpcTaskError::Internal(e.to_string()))?;
    task_manager.cancel_task(req.task_id).mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}

pub async fn init_create_new_account_user_action(
    ctx: MmArc,
    req: HwRpcTaskUserActionRequest,
) -> MmResult<SuccessResponse, RpcTaskUserActionError> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(RpcTaskUserActionError::Internal)?;
    let mut task_manager = coins_ctx
        .create_account_manager
        .lock()
        .map_to_mm(|e| RpcTaskUserActionError::Internal(e.to_string()))?;
    task_manager
        .on_user_action(req.task_id, req.user_action)
        .mm_err(Into::into)?;
    Ok(SuccessResponse::new())
}

pub(crate) mod common_impl {
    use super::*;
    use crate::coin_balance::HDWalletBalanceOps;
    use crate::hd_wallet::{HDAccountOps, HDWalletCoinOps, HDWalletOps};
    use crate::MarketCoinOps;

    pub async fn init_create_new_account_rpc<'a, Coin, XPubExtractor>(
        coin: &Coin,
        params: CreateNewAccountParams,
        xpub_extractor: Option<&XPubExtractor>,
        created_account: &CreatedAccount,
    ) -> MmResult<HDAccountBalance, HDWalletRpcError>
    where
        Coin: HDWalletBalanceOps
            + CoinWithDerivationMethod<HDWallet = <Coin as HDWalletCoinOps>::HDWallet>
            + Send
            + Sync
            + MarketCoinOps,
        XPubExtractor: HDXPubExtractor + Sync,
    {
        let hd_wallet = coin.derivation_method().hd_wallet_or_err().mm_err(Into::into)?;

        // The account is persisted first, and then scanned (CRD ch.05 R35c item 2).
        let mut new_account = coin
            .create_new_account(hd_wallet, xpub_extractor, params.account_index())
            .await
            .mm_err(Into::into)?;
        created_account.set(new_account.account_id());
        let address_scanner = coin.produce_hd_address_scanner().await.mm_err(Into::into)?;
        let account_index = new_account.account_id();
        let account_derivation_path = new_account.account_derivation_path();

        let addresses = if params.scan {
            let gap_limit = params.gap_limit.unwrap_or_else(|| hd_wallet.gap_limit());
            coin.scan_for_new_addresses(hd_wallet, &mut new_account, &address_scanner, gap_limit)
                .await
                .mm_err(Into::into)?
        } else {
            Vec::new()
        };

        let total_balance = crate::coin_balance::sum_hd_address_balances(coin.ticker(), &addresses);

        Ok(HDAccountBalance {
            account_index,
            derivation_path: RpcDerivationPath(account_derivation_path),
            total_balance,
            addresses,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utxo::utxo_tests::hd_accounts_at_index_tests::{hd_coin_with_account_zero, mock_chain, mock_storage,
                                                              CountingDevice};
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;

    /// CRD ch.05 R35c item 4: the cleanup the task framework runs for a cancelled `task::create_new_account`
    /// removes the account the task created, because it is the wallet's last account.
    #[test]
    fn r35c_item4_the_cancel_cleanup_removes_the_account_the_task_created() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin_with_account_zero();
        let task = InitCreateAccountTask {
            ctx: MmCtxBuilder::default().into_mm_arc(),
            coin: MmCoinEnum::UtxoCoin(coin.clone()),
            req: serde_json::from_value(serde_json::json!({ "coin": "TEST" })).unwrap(),
            created_account: CreatedAccount::default(),
        };

        // The task creates account 1.
        let params: CreateNewAccountParams = serde_json::from_value(serde_json::json!({ "scan": false })).unwrap();
        let account =
            block_on(coin.init_create_account_rpc(params, None::<&CountingDevice>, &task.created_account)).unwrap();
        assert_eq!(account.account_index, 1);
        assert!(items.lock().unwrap().contains_key(&1));

        // The task is cancelled: the framework runs the cleanup.
        let cleanup = task.cancel_cleanup().expect("the task has a cancel cleanup");
        block_on(cleanup());
        assert!(!items.lock().unwrap().contains_key(&1));
    }
}
