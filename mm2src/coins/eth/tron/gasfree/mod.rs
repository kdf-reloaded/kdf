//! Tron GasFree fee delegation — gasless TRC20 withdraw (Chapter 49).
//!
//! GasFree is a fee-delegation (meta-transaction) scheme for Tron TRC20 tokens:
//! the user authorizes a transfer **off-chain** (a TIP-712 `PermitTransfer`)
//! and a third-party provider broadcasts it on-chain, charging its fee in the
//! token itself. Each user has a deterministic per-network GasFree custody
//! address derived locally by `CREATE2`.
//!
//! This module is **TRC20-only and sign-only today** (§49.0): the withdraw path
//! preflights, signs the authorization, and returns it as a typed off-chain
//! payload with a gasless fee breakdown; it does not yet submit to the provider
//! or track settlement (D-submit). The provider client models the submit/trace
//! endpoints and the rail-selection/preflight/fee logic are complete.
//!
//! Layout (§49.2):
//! - [`config`]   — per-network constants, provider/token config + validation.
//! - [`derive`]   — `CREATE2` custody-address derivation (§49.5).
//! - [`permit`]   — TIP-712 `PermitTransfer` construction + signing (§49.6).
//! - [`client`]   — provider REST API types, auth, envelope (§49.4).
//! - [`preflight`]— availability decision + fee math (§49.7).
//! - [`withdraw`] — rail selection, the sign-only orchestration, public surface.
//! - [`error`]    — error taxonomy + HTTP status mapping (§49.9).

pub mod client;
pub mod config;
pub mod derive;
pub mod error;
pub mod permit;
pub mod preflight;
pub mod withdraw;

pub use config::{GaslessTokenConfig, GaslessTokenRequest, TronGaslessProviderConfig, TronGaslessProviderRequest};
pub use derive::derive_gasfree_address;
pub use error::{GasFreeConfigError, GasFreeProviderError, GasFreeWithdrawError};
pub use withdraw::{gasless_withdraw, select_rail, FeeMethod, GaslessFeeDetails, GaslessWithdrawOptions,
                   GaslessWithdrawOutcome, GaslessWithdrawRequest, RailDecision};
