# KDF Reloaded patch

This directory is based on the published `zcash_primitives` 0.28.0 crate and
retains its upstream license files and package metadata.

KDF Reloaded carries a narrow compatibility extension to the transaction
builder. It exposes ownership of a completed transaction, preserves an
explicit legacy `nLockTime`, accepts already-constructed transparent outputs,
and forwards KDF-family atomic-swap P2SH inputs to the corresponding
`zcash_transparent` extension. These hooks preserve the deployed transaction
shape; they do not alter Zcash consensus arithmetic or proof construction.
