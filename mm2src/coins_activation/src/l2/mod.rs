mod enable_l2;
mod init_l2;
mod l2_activation_errors;

pub use enable_l2::{enable_l2, EnableL2Error, L2ActivationOps, L2ProtocolParams};
// The `l2` module is private, so `EnableL2Request` is not part of the crate's
// public API; it is referenced only from unit tests through this re-export.
#[cfg(test)] pub(crate) use enable_l2::EnableL2Request;
pub use init_l2::{cancel_l2_activation, init_l2, init_l2_status, init_l2_user_action, InitL2ActivationOps,
                  L2ActivationTask, L2InitialStatus, L2TaskManagerShared};
pub use l2_activation_errors::L2ActivationError;
