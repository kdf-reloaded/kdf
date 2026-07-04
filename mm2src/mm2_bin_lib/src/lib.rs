//! Platform entry-point library for the Komodo DeFi Framework (Reloaded).
//!
//! This crate provides the top-level binary and C-library interfaces.
//! The actual application logic lives in [`kdflib`] (the mm2_main crate);
//! this crate is a thin wrapper that wires up platform-specific concerns
//! (native main, WASM bindings, mobile FFI).

// Re-export public entry points from mm2_main.
pub use kdflib::lp_main;
pub use kdflib::mm2_status;
pub use kdflib::MainStatus;

#[cfg(not(target_arch = "wasm32"))]
pub use kdflib::{mm2_main, run_lp_main};
