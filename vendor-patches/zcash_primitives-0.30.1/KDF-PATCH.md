# KDF Reloaded patch

This directory is based on the published `zcash_primitives` 0.30.1 crate and
retains its upstream license files and package metadata. (Re-vendored from
0.28.0 on the ARRR Ironwood plan, Step B; the extension below was re-applied
to the 0.30.1 source.)

KDF Reloaded carries a narrow compatibility extension to the transaction
builder. It exposes ownership of a completed transaction, preserves an
explicit legacy `nLockTime`, accepts already-constructed transparent outputs,
and forwards KDF-family atomic-swap P2SH inputs to the corresponding
`zcash_transparent` extension. These hooks preserve the deployed transaction
shape; they do not alter Zcash consensus arithmetic or proof construction.

Re-application notes for 0.30.1: `Builder` gained `lock_time`, which feeds both
the transaction build and the PCZT build (`PcztParts`); the separate
`DeferredPcztBuilder` is Ironwood-only, carries no transparent data, and keeps
its `lock_time: 0`. The `BuildConfig::Standard` literal in the focused test now
supplies the new Ironwood and padding fields. The `Cargo.toml` `[patch]` that
points `zcash_transparent` at the sibling vendored copy is kept for standalone
test runs. Focused test:
`transaction::builder::tests::kdf_builder_preserves_lock_time_and_raw_output_order`.
