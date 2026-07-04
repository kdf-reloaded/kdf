//! Native entry point for the Komodo DeFi Framework (Reloaded).

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        kdflib::mm2_main()
    }
}
