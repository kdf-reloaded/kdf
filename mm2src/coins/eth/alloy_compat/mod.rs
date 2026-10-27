//! # Purpose
//!
//! Bridge layer between the legacy `rust-web3` types pervasive in the EVM
//! coin code and the modern `alloy` Ethereum stack. After LP-17 Phase 4c
//! the `web3` git dep was dropped entirely; this module now only exposes
//! the alloy provider/transport plumbing the EVM coin code is built on.
//!
//! # Public exports
//!
//! - [`provider::KdfProvider`] / [`provider::build_provider`] — the
//!   alloy `RootProvider<Ethereum>` instance KDF talks to nodes through.
//! - [`transport::AlloyTransport`] — the custom alloy `Transport` impl
//!   wrapping the shared `hyper` client and KDF event handlers.
//! - [`send_shim::assert_send_future`] — `Send`-asserting identity
//!   wrapper required because some alloy futures are conditionally
//!   `!Send` on WASM (sound on the single-threaded WASM runtime).
//!
//! # Invariants
//!
//! - **No facades:** this module never re-exposes a `Web3<T>` namespace
//!   nor a `futures 0.1`-style `impl Future<Item, Error>`. Call-sites
//!   migrate to native `async fn` returns.
//! - **GPL-clean:** all code below is original to the kdf-reloaded
//!   project; the `alloy` crate family is dual-licensed MIT/Apache-2.0
//!   and therefore GPLv2-compatible (per LP-17 prompt §0).

pub mod provider;
pub mod send_shim;
pub mod transport;

pub use provider::{build_direct_provider, build_provider, provider_with_own_node_order, KdfProvider};
pub use send_shim::assert_send_future;
pub use transport::AlloyTransport;
