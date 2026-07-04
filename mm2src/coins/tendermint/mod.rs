pub(crate) mod ethermint_account;
pub mod htlc;
mod ibc;
mod rpc;

// Modular coin implementation (split by responsibility)
mod tendermint_coin;
mod tendermint_helpers;
mod tendermint_market_ops;
mod tendermint_mm_coin;
mod tendermint_staking;
mod tendermint_swap_ops;
mod tendermint_token;
mod tendermint_types;
pub mod wc_integration;

pub use cosmrs::tendermint::PublicKey as TendermintPublicKey;
pub use cosmrs::AccountId;
pub use tendermint_coin::*;
pub use tendermint_token::*;
pub use tendermint_types::*;

pub(crate) const TENDERMINT_COIN_PROTOCOL_TYPE: &str = "TENDERMINT";
pub(crate) const TENDERMINT_ASSET_PROTOCOL_TYPE: &str = "TENDERMINTTOKEN";

const IRIS_PREFIX: &str = "iaa";
const NUCLEUS_PREFIX: &str = "nuc";
