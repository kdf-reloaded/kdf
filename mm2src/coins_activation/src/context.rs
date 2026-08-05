use crate::eth_with_tokens_activation::EthTaskManagerShared;
#[cfg(not(target_arch = "wasm32"))]
use crate::l2::L2TaskManagerShared;
use crate::sia_activation::SiaTaskManagerShared;
use crate::tendermint_with_tokens_activation::TendermintTaskManagerShared;
use crate::utxo_activation::{QtumTaskManagerShared, UtxoStandardTaskManagerShared};
#[cfg(not(target_arch = "wasm32"))]
use crate::z_coin_activation::ZcoinTaskManagerShared;
#[cfg(not(target_arch = "wasm32"))]
use coins::lightning::LightningCoin;
#[cfg(not(target_arch = "wasm32"))]
use futures::lock::Mutex as AsyncMutex;
use mm2_core::mm_ctx::{from_ctx, MmArc};
use rpc_task::RpcTaskManager;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))] use std::sync::{Mutex, Weak};

pub struct CoinsActivationContext {
    pub(crate) init_utxo_standard_task_manager: UtxoStandardTaskManagerShared,
    pub(crate) init_qtum_task_manager: QtumTaskManagerShared,
    pub(crate) init_sia_task_manager: SiaTaskManagerShared,
    pub(crate) init_eth_task_manager: EthTaskManagerShared,
    pub(crate) init_tendermint_task_manager: TendermintTaskManagerShared,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) init_z_coin_task_manager: ZcoinTaskManagerShared,
    #[cfg(not(target_arch = "wasm32"))]
    z_coin_activation_locks: ZCoinActivationLocks,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) init_lightning_task_manager: L2TaskManagerShared<LightningCoin>,
}

impl CoinsActivationContext {
    /// Obtains a reference to this crate context, creating it if necessary.
    pub fn from_ctx(ctx: &MmArc) -> Result<Arc<CoinsActivationContext>, String> {
        from_ctx(&ctx.coins_activation_ctx, move || {
            Ok(CoinsActivationContext {
                init_utxo_standard_task_manager: RpcTaskManager::new_shared(),
                init_qtum_task_manager: RpcTaskManager::new_shared(),
                init_sia_task_manager: RpcTaskManager::new_shared(),
                init_eth_task_manager: RpcTaskManager::new_shared(),
                init_tendermint_task_manager: RpcTaskManager::new_shared(),
                #[cfg(not(target_arch = "wasm32"))]
                init_z_coin_task_manager: RpcTaskManager::new_shared(),
                #[cfg(not(target_arch = "wasm32"))]
                z_coin_activation_locks: ZCoinActivationLocks::default(),
                #[cfg(not(target_arch = "wasm32"))]
                init_lightning_task_manager: RpcTaskManager::new_shared(),
            })
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn z_coin_activation_lock(&self, ticker: &str) -> Result<Arc<AsyncMutex<()>>, String> {
        self.z_coin_activation_locks.lock_for(ticker)
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct ZCoinActivationLocks {
    by_ticker: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ZCoinActivationLocks {
    fn lock_for(&self, ticker: &str) -> Result<Arc<AsyncMutex<()>>, String> {
        let mut by_ticker = self
            .by_ticker
            .lock()
            .map_err(|error| format!("ZCoin activation lock registry is unavailable: {}", error))?;
        by_ticker.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = by_ticker.get(ticker).and_then(Weak::upgrade) {
            return Ok(lock);
        }

        let lock = Arc::new(AsyncMutex::new(()));
        by_ticker.insert(ticker.to_owned(), Arc::downgrade(&lock));
        Ok(lock)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn zcoin_activation_locks_are_shared_only_by_ticker() {
        let locks = ZCoinActivationLocks::default();
        let arrr_first = locks.lock_for("ARRR").unwrap();
        let arrr_second = locks.lock_for("ARRR").unwrap();
        let other = locks.lock_for("ZEC").unwrap();
        assert!(Arc::ptr_eq(&arrr_first, &arrr_second));
        assert!(!Arc::ptr_eq(&arrr_first, &other));

        let guard = arrr_first.try_lock_owned().unwrap();
        assert!(arrr_second.try_lock_owned().is_none());
        assert!(other.try_lock_owned().is_some());
        drop(guard);
        assert!(arrr_second.try_lock_owned().is_some());
    }
}
