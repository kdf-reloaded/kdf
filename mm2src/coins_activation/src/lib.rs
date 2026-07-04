#![allow(clippy::result_large_err)]

mod bch_with_tokens_activation;
mod context;
mod erc20_token_activation;
mod eth_with_tokens_activation;
mod init_platform_coin_with_tokens;
mod l2;
#[cfg(not(target_arch = "wasm32"))] mod lightning_activation;
mod platform_coin_with_tokens;
mod prelude;
mod sia_activation;
mod slp_token_activation;
#[cfg(not(target_arch = "wasm32"))]
mod solana_with_tokens_activation;
#[cfg(not(target_arch = "wasm32"))] mod spl_token_activation;
mod standalone_coin;
mod tendermint_token_activation;
mod tendermint_with_tokens_activation;
mod token;
mod utxo_activation;
#[cfg(not(target_arch = "wasm32"))] mod z_coin_activation;

pub use init_platform_coin_with_tokens::{cancel_init_platform_coin_with_tokens, init_platform_coin_with_tokens,
                                         init_platform_coin_with_tokens_status,
                                         init_platform_coin_with_tokens_user_action};
pub use l2::enable_l2;
pub use l2::{cancel_l2_activation, init_l2, init_l2_status, init_l2_user_action};
pub use platform_coin_with_tokens::enable_platform_coin_with_tokens;
pub use standalone_coin::{cancel_init_standalone_coin, init_standalone_coin, init_standalone_coin_status,
                          init_standalone_coin_user_action};
pub use token::enable_token;

#[cfg(test)] mod tests;
