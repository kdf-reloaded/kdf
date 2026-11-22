# libp2p-noise 0.43.0 — vendor patch

## Provenance

Copied verbatim from `KomodoPlatform/rust-libp2p`, tag `k-0.52.12`
(commit `8bcc1fda79d56a2f398df3d45a29729b8ce0148d`), path `transports/noise`, then
patched. Redirected into the graph with:

```toml
[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]
libp2p-noise = { path = "vendor-patches/libp2p-noise-0.43.0" }
```

## Why it exists

`curve25519-dalek` 3.2.0 carries **RUSTSEC-2024-0344**: the scalar subtraction is
not constant-time on some compiler versions and targets, so its timing can depend
on secret data. The fix is in `curve25519-dalek` 4.1.3; the 3.x line has no fixed
release.

The fork's `libp2p-noise` 0.43.0 pulls the 3.x line twice:

- through `x25519-dalek = "1.1.0"`, whose 1.1 line builds on `curve25519-dalek` 3;
- through its own direct requirement `curve25519-dalek = "3.0.0"` (declared, but
  not used anywhere in `src/`).

Upstream `libp2p-noise` 0.43.2 moved both requirements to the fixed lines. It
cannot be used directly: it builds on the crates.io `libp2p-core`, whose types are
not the fork's. `k-0.52.12` is the newest tag the fork has, so there is nothing to
bump to.

## The patch

Only `Cargo.toml` is patched. The effect is identical to upstream `libp2p-noise`
0.43.2, whose `src/` equals that of 0.43.0 (its changelog: "0.43.1: Update
dependencies", "0.43.2: Update x25519-dalek to 2.0.0").

- `x25519-dalek = "1.1.0"` becomes `"2"`, and `curve25519-dalek = "3.0.0"` becomes
  `"4.1.1"`, as upstream 0.43.2 has them. Both resolve to versions the workspace
  already builds (`x25519-dalek` 2.0.1, `curve25519-dalek` 4.1.3), so no crate is
  added and `x25519-dalek` 1.1.0 leaves the graph: `cargo tree -i
  x25519-dalek@1.1.0` returns nothing.
- The fork's `workspace = true` entries are written out, because this copy is not
  a member of the fork's workspace: `libp2p-core` and the test-only `quickcheck`
  (`quickcheck-ext`) as git dependencies on the same fork tag; `libp2p-identity`,
  `multiaddr` and `multihash` with the fork's own registry requirements
  (`0.2.1`, `0.18.0`, `0.19.0`); `rust-version` as `1.65.0`. They resolve to the
  same packages as before.
- `[dev-dependencies]` asks for `libp2p-identity` with its `rand` feature. The
  crate's own tests call `Keypair::generate_ed25519`, and the fork's workspace
  enabled that feature for them through feature unification.
- Package version, features and the two target-specific `snow` entries are
  unchanged.

The source files are the fork's, byte for byte, with one addition: the test module
`rfc7748_tests` at the end of `src/protocol.rs` (marked `PATCHED`).

## Wire compatibility

Nothing on the wire changes.

- No handshake parameter changes: the protocol id `/noise`, the pattern
  `Noise_XX_25519_ChaChaPoly_SHA256`, the prologue, the static-key signature
  prefix `noise-libp2p-static-key:` and the handshake payload are all in the
  unchanged source.
- The crate uses the dalek crates in one place: `x25519(k, u)` in
  `src/protocol.rs`. In both `x25519-dalek` versions it is the X25519 function of
  RFC 7748, with the same clamping of the scalar (clear the three lowest bits and
  the highest bit, set the second-highest bit). The output bytes are equal for
  every input: the same public key from the same secret key, and the same shared
  secret.
- `snow`'s own Diffie-Hellman is not used. The crate's `Resolver` supplies the DH
  and asks `snow` only for the hash, the cipher and the random numbers; those are
  unchanged on both native and `wasm32`.

A peer that completes the handshake with the unpatched crate completes it with
this one.

## The test

`src/protocol.rs`, module `rfc7748_tests`, test
`x25519_keys_and_shared_secret_match_rfc7748_section_6_1_vectors`: the Curve25519
Diffie-Hellman vectors of RFC 7748 section 6.1. Alice's and Bob's private keys give
the published public keys through both key paths of the crate (`SecretKey` →
`Keypair`, and `snow::types::Dh::set`), and both sides' `Dh::dh` gives the published
shared secret.

`cargo test --manifest-path vendor-patches/libp2p-noise-0.43.0/Cargo.toml` →
**15 passed, 0 failed**: the new test, the crate's two prologue tests, the
handshake round trip `tests/smoke.rs` (`xx`), the ten
`tests/webtransport_certhashes.rs` tests and the doc-test.

## Removing this patch

Delete the directory, drop the `libp2p-noise` line from the
`[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]` stanza and the
`exclude` entry, once a `KomodoPlatform/rust-libp2p` tag ships a `libp2p-noise` on
`x25519-dalek` 2 and `curve25519-dalek` 4, or the ch28 modernization moves the pin
to upstream libp2p.
