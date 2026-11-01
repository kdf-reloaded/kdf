use crate::standalone_coin::InitStandaloneCoinInitialStatus;
use crypto::hw_rpc_task::{HwRpcTaskAwaitingStatus, HwRpcTaskUserAction};
use serde_derive::Serialize;

pub type UtxoStandardAwaitingStatus = HwRpcTaskAwaitingStatus;
pub type UtxoStandardUserAction = HwRpcTaskUserAction;

/// The in-progress vocabulary of the UTXO-family members (CRD ch. 55 R55.5.12).
/// Payload-carrying values serialise as single-key objects.
#[derive(Clone, Debug, Serialize)]
pub enum UtxoStandardInProgressStatus {
    ActivatingCoin,
    /// SPV block-header synchronisation progress. It is emitted once the SPV progress surface exists (D55.4).
    #[allow(dead_code)]
    SyncingBlockHeaders {
        current_scanned_block: u64,
        last_block: u64,
    },
    /// A transient SPV synchronisation failure that is being retried (D55.4).
    #[allow(dead_code)]
    TemporaryError(String),
    RequestingWalletBalance,
    Finishing,
    /// The hardware wallet waits for a device to be connected.
    WaitingForTrezorToConnect,
    /// This status doesn't require the user to send `UserAction`,
    /// but it tells the user that he should confirm/decline an action on his device.
    FollowHwDeviceInstructions,
}

impl InitStandaloneCoinInitialStatus for UtxoStandardInProgressStatus {
    fn initial_status() -> Self { UtxoStandardInProgressStatus::ActivatingCoin }
}
