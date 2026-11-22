// lightning_no_abort_tests — a source-level guard for the trait impls of LightningCoin

/// A `LightningCoin` cannot be built in a unit test without a running node, so this guards the abort regression at
/// the source level: the three files that implement `SwapOps`, `MarketCoinOps` and `MmCoin` for it hold no macro
/// that aborts the process. The answers themselves mirror Solana's, whose answers are tested.
#[test]
fn lightning_coin_methods_never_abort_the_process() {
    let files = [
        ("lightning_swap_ops.rs", include_str!("lightning_swap_ops.rs")),
        ("lightning_market_ops.rs", include_str!("lightning_market_ops.rs")),
        ("lightning_mm_coin.rs", include_str!("lightning_mm_coin.rs")),
    ];
    // The macro names are put together here, so that this file does not hold them in their written form.
    for name in ["unimplemented", "todo", "unreachable"] {
        let aborting_macro = format!("{name}!(");
        for (file, source) in files {
            assert!(
                !source.contains(&aborting_macro),
                "{file} contains {aborting_macro}: a method of a Lightning coin must answer an error, not abort"
            );
        }
    }
}
