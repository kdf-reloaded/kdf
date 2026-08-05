# KDF Reloaded patch

This directory is based on the published `zcash_transparent` 0.8.0 crate and
retains its upstream license files and package metadata.

KDF Reloaded carries a narrow compatibility extension for its deployed
atomic-swap P2SH contract. The extension preserves the caller-selected input
sequence and constructs the script signature as
`<SIGHASH_ALL signature> <contract selector/secret> <redeem script>`. It also
accepts already-constructed transparent outputs so their count, order, values,
and scripts remain wire compatible.
