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

## Output-only Ironwood bundle (added for CRD ch.39 R39.6.5d)

A payment from Sapling into the Ironwood pool is built by the dictated chain's own wallets as an
output-only bundle: spends disabled, outputs enabled, anchored at the empty-tree root. The builder
could only build an Ironwood bundle with the bundle version's default flags (spends and outputs
enabled). The extension is additive and tiny:

- `BundlePadding` gained a public field `spends_disabled: bool` (documented in the source) and a
  constant `BundlePadding::OUTPUT_ONLY` (`DEFAULT` with `spends_disabled: true`). `DEFAULT` and
  `UNPADDED` set it to `false`, so every existing user of those constants is unchanged.
- `BuildConfig::ironwood_builder` (the `Standard` arm only) builds the Ironwood bundle with
  `orchard::bundle::Flags::SPENDS_DISABLED` when `ironwood_padding.spends_disabled` is set, and
  with `bundle_version.default_flags()` otherwise, as before.

What it does not touch: the Orchard builder (`orchard_builder`) ignores the field and always uses
the default flags; the coinbase arm, the deferred-PCZT builder, signing, proof construction, fee
arithmetic and the transaction format are unchanged. The field was put on `BundlePadding` rather
than `BuildConfig::Standard` so that no existing `BuildConfig::Standard { .. }` literal, in this
crate or in a dependant, has to change.
