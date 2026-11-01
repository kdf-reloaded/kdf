use crate::eth_with_tokens_activation::EthTaskManagerShared;
use crate::init_token::InitTokenTaskManagerShared;
#[cfg(not(target_arch = "wasm32"))]
use crate::l2::L2TaskManagerShared;
use crate::sia_activation::SiaTaskManagerShared;
use crate::tendermint_with_tokens_activation::TendermintTaskManagerShared;
use crate::utxo_activation::{BchTaskManagerShared, QtumTaskManagerShared, UtxoStandardTaskManagerShared};
use crate::z_coin_activation::ZcoinTaskManagerShared;
#[cfg(not(target_arch = "wasm32"))]
use coins::lightning::LightningCoin;
use coins::{activation_locks::ActivationGuard, CoinsContext};
use mm2_core::mm_ctx::{from_ctx, MmArc};
use rpc_task::RpcTaskManager;
use std::sync::Arc;

pub struct CoinsActivationContext {
    pub(crate) init_utxo_standard_task_manager: UtxoStandardTaskManagerShared,
    pub(crate) init_qtum_task_manager: QtumTaskManagerShared,
    pub(crate) init_bch_task_manager: BchTaskManagerShared,
    pub(crate) init_sia_task_manager: SiaTaskManagerShared,
    pub(crate) init_eth_task_manager: EthTaskManagerShared,
    pub(crate) init_erc20_task_manager: InitTokenTaskManagerShared<coins::eth::EthCoin>,
    pub(crate) init_tendermint_task_manager: TendermintTaskManagerShared,
    pub(crate) init_z_coin_task_manager: ZcoinTaskManagerShared,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) init_lightning_task_manager: L2TaskManagerShared<LightningCoin>,
    /// The registry of the task-framework tests, which drive a scripted coin through the standalone-coin family.
    #[cfg(test)]
    pub(crate) init_test_coin_task_manager:
        crate::standalone_coin::InitStandaloneCoinTaskManagerShared<coins::TestCoin>,
    /// The registries of the serialized-activation tests, which drive a scripted coin through the other families.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) init_test_platform_task_manager:
        crate::init_platform_coin_with_tokens::InitPlatformCoinWithTokensTaskManagerShared<coins::TestCoin>,
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) init_test_token_task_manager: InitTokenTaskManagerShared<coins::TestCoin>,
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) init_test_l2_task_manager: L2TaskManagerShared<coins::TestCoin>,
}

impl CoinsActivationContext {
    /// Obtains a reference to this crate context, creating it if necessary.
    pub fn from_ctx(ctx: &MmArc) -> Result<Arc<CoinsActivationContext>, String> {
        from_ctx(&ctx.coins_activation_ctx, move || {
            Ok(CoinsActivationContext {
                init_utxo_standard_task_manager: RpcTaskManager::new_shared(),
                init_qtum_task_manager: RpcTaskManager::new_shared(),
                init_bch_task_manager: RpcTaskManager::new_shared(),
                init_sia_task_manager: RpcTaskManager::new_shared(),
                init_eth_task_manager: RpcTaskManager::new_shared(),
                init_erc20_task_manager: RpcTaskManager::new_shared(),
                init_tendermint_task_manager: RpcTaskManager::new_shared(),
                init_z_coin_task_manager: RpcTaskManager::new_shared(),
                #[cfg(not(target_arch = "wasm32"))]
                init_lightning_task_manager: RpcTaskManager::new_shared(),
                #[cfg(test)]
                init_test_coin_task_manager: RpcTaskManager::new_shared(),
                #[cfg(all(test, not(target_arch = "wasm32")))]
                init_test_platform_task_manager: RpcTaskManager::new_shared(),
                #[cfg(all(test, not(target_arch = "wasm32")))]
                init_test_token_task_manager: RpcTaskManager::new_shared(),
                #[cfg(all(test, not(target_arch = "wasm32")))]
                init_test_l2_task_manager: RpcTaskManager::new_shared(),
            })
        })
    }
}

/// Waits for the activation locks of `tickers` and returns the guard that holds them (CRD ch.55 R55.1.12).
///
/// Every activation path calls this before it builds anything and keeps the guard until the coin is registered
/// and the steps that belong to the activation are done. After the wait the path checks again whether its
/// ticker is registered.
pub(crate) async fn lock_activation(ctx: &MmArc, tickers: &[&str]) -> Result<ActivationGuard, String> {
    CoinsContext::from_ctx(ctx)?.lock_activation(tickers).await
}
