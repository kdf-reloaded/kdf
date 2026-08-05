pub mod for_tests;

// A throwaway `geth --dev` node for EVM integration tests (native only).
#[cfg(not(target_arch = "wasm32"))] pub mod geth_dev;
