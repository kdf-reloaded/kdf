# KDF Reloaded patch: `zcash_client_backend` 0.23.0

This directory is the published crates.io source for
`zcash_client_backend` 0.23.0 (MIT OR Apache-2.0), patched only in its
Cargo manifest.

KDF Reloaded removes the unconditional exact dependency on
`time-core = 0.1.2`. That dependency was an obsolete resolver workaround for
the optional Tor stack and prevents Cargo from selecting the security-fixed
`time` release. No Rust source, feature, public API, or protocol behavior is
changed.

The patch can be removed when a compatible stable
`zcash_client_backend` release no longer contains the exact `time-core` pin.
