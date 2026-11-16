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

- `BundlePadding` has the same shape as upstream (`bundle_required`, `pad_to_minimum`; constants
  `DEFAULT` and `UNPADDED`). The output-only choice is deliberately not a field of it: a public
  field of a struct that dependants build by literal would stop `zcash_client_backend` from
  compiling with its `orchard` feature on (an earlier form of this patch did that).
- `BuildConfig::ironwood_builder` (private) takes a `spends_disabled` flag: with it, the Ironwood
  bundle of `BuildConfig::Standard` is built with `orchard::bundle::Flags::SPENDS_DISABLED`;
  without it, with `bundle_version.default_flags()`, as upstream does.
- `Builder::new` is unchanged and passes `false`. A new `Builder::new_with_output_only_ironwood`
  takes the same arguments and passes `true`; only KDF Reloaded code calls it, for a payment from
  Sapling into Ironwood.

What it does not touch: the Orchard builder (`orchard_builder`), the coinbase arm, the
deferred-PCZT builder, signing, proof construction, fee arithmetic and the transaction format are
unchanged, and no `BuildConfig::Standard` or `BundlePadding` literal in this crate or in a
dependant needs a change. The flags a built output-only bundle carries are asserted by
`z_pirate1_tests.rs` in `coins` (CRD ch.39 T39.6.5d).
