# KDF Reloaded patch

This directory is based on the published `zcash_transparent` 0.10.0 crate and
retains its upstream license files and package metadata. (Re-vendored from
0.8.0 on the ARRR Ironwood plan, Step B; the extension below was re-applied
to the 0.10.0 source, which is the version the `zcash_primitives` 0.30.1 stack
depends on.)

KDF Reloaded carries a narrow compatibility extension for its deployed
atomic-swap P2SH contract. The extension preserves the caller-selected input
sequence and constructs the script signature as
`<SIGHASH_ALL signature> <contract selector/secret> <redeem script>`. It also
accepts already-constructed transparent outputs so their count, order, values,
and scripts remain wire compatible.

Re-application notes for 0.10.0: upstream changed only formatting and let-chain
style in `src/builder.rs`; the extension (`SpendInfo::KdfP2sh`,
`TransparentInputInfo::from_kdf_p2sh_parts`, the per-input `sequence`,
`TransparentBuilder::add_output_raw`, and the matching signing, size and PCZT
arms) applied unchanged in behaviour. Focused tests:
`builder::tests::kdf_p2sh_preserves_sequence_and_script_component_order` and
`builder::tests::raw_outputs_preserve_count_order_value_and_script`.
