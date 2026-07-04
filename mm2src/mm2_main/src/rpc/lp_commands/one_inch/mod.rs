//! `experimental::1inch_v6_0::classic_swap_*` JSON-RPC surface (CRD §23.8A).
//!
//! These handlers surface the trading-API 1inch classic-swap library to public
//! callers. The library remains handler-free and coin-free; the coin wiring and
//! the `HttpStatusCode` mapping live here.

pub mod errors;
pub mod rpcs;
pub mod types;

pub use rpcs::{classic_swap_contract, classic_swap_create, classic_swap_liquidity_sources, classic_swap_quote,
               classic_swap_tokens};
