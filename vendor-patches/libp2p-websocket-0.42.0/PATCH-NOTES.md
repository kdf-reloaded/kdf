# libp2p-websocket 0.42.0 — vendor patch

## Provenance

Copied from `KomodoPlatform/rust-libp2p`, tag `k-0.52.12`
(commit `8bcc1fda79d56a2f398df3d45a29729b8ce0148d`), path `transports/websocket`,
then patched (TLS layer only). Redirected into the graph with:

```toml
[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]
libp2p-websocket = { path = "vendor-patches/libp2p-websocket-0.42.0" }
```

## Why it exists

The fork's `libp2p-websocket` pins `futures-rustls 0.22`, which pins
`rustls 0.20.4` and `webpki-roots 0.23`. `rustls 0.20.4` carries
**RUSTSEC-2026-0349** (fixed in `>= 0.23.46`).

This is hygiene, not an incident. The advisory is about losing post-quantum
confidentiality when a client is steered off a hybrid key-exchange group; no
affected rustls version implements such a group, so there is nothing to be
downgraded from.

A plain version bump is impossible: the fork has no tag newer than `k-0.52.12`,
its `komodo-master` is identical for websocket, and its `master` has
`libp2p-websocket 0.43.0` on `futures-rustls 0.24` (rustls 0.21), also unusable.
Upstream reaches rustls 0.23 only at `libp2p-websocket 0.43.2`, part of libp2p
0.53, and moving there means leaving the fork (relay-mesh gossipsub extension,
wasm-ext). That is a separate project.

## The patch

Only `src/tls.rs` and one type in `src/framed.rs` changed; `lib.rs`, `error.rs`
and the rest of `framed.rs` are byte-identical to the fork.

- `futures-rustls` 0.22 -> `0.26` with `default-features = false` and features
  `ring`, `tls12`. **`ring`, never `aws-lc-rs`**: rustls 0.23 wants exactly one
  process-wide crypto provider and a mismatch panics at the first wss connect at
  runtime, not at build time. `tls12` keeps TLS 1.2 available, as it was on
  rustls 0.20 (the 0.26 default features would have provided it). Both
  `ClientConfig` and `ServerConfig` are built with
  `builder_with_provider(ring::default_provider())` so no process-global default
  provider is consulted at all.
- `webpki-roots` 0.23 -> `1.0` (already in the graph); trust anchors are built
  from `rustls::pki_types::TrustAnchor`.
- `tls::PrivateKey` / `tls::Certificate` wrap `PrivateKeyDer<'static>` /
  `CertificateDer<'static>`. `PrivateKey` keeps `Clone` through `clone_key()`
  (`PrivateKeyDer` has no `Clone`).
- `framed.rs`: `dns_name: Option<ServerName<'static>>`; `dns_name_ref` returns
  `ServerName<'static>`.
- `libp2p-identity` and the fork-workspace dependencies are rewritten as explicit
  git deps on the same tag (same approach as the yamux vendor).

### Deliberate deviation from upstream 0.43.2

Upstream's `PrivateKey::new` does `.expect("unknown or invalid key format")`,
which aborts the process on a malformed key file. Here `PrivateKey::new` returns
`Result<Self, tls::Error>`. This changes the public signature; the single caller
(`mm2_p2p::atomicdex_behaviour::build_dns_ws_transport`) now propagates the error
as `AdexBehaviourError::WssConfig` -> `P2PInitError::InvalidWssConfig`, instead
of panicking on `PrivateKey::new` and on `Config::new(..).unwrap()`.

The accepted key formats are what `PrivateKeyDer::try_from(Vec<u8>)` recognises:
PKCS#8, PKCS#1 and SEC1 DER (the 0.20 line accepted PKCS#8 and PKCS#1).

## Tests

`tests/wss_handshake.rs` runs real wss handshakes over loopback TCP with a
`/dns4/localhost` dial: server and client, PKCS#8 key (rcgen), PKCS#1 RSA key
(fixtures), payload exchange; plus the negative cases (untrusted certificate
rejected, malformed key is an `Err`, key/certificate mismatch is an `Err`).

`tests/fixtures/rsa_pkcs1_key.der` is a **throwaway test key** (2048-bit RSA,
`CN=localhost`, `CA:FALSE`, 100-year validity) generated for this test only; it
protects nothing. Regenerate with `openssl genrsa -traditional`,
`openssl req -x509 ... -addext basicConstraints=critical,CA:FALSE` and
`openssl x509 -outform DER`.

Run (the crate is excluded from the workspace, and when the checkout lives under
another cargo workspace root, as a worktree under `local/worktrees/` does, cargo
needs an empty `[workspace]` table appended to a scratch copy of the manifest):

```
cargo test --manifest-path vendor-patches/libp2p-websocket-0.42.0/Cargo.toml
```

`dialer_connects_to_listener_ipv6` needs an IPv6 loopback and fails on hosts
without one; that is environmental, not caused by the patch.

## Removing this patch

Delete the directory and drop the `libp2p-websocket` line from the
`[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]` stanza and the
`exclude` entry once a fork tag ships `libp2p-websocket` on rustls 0.23, or the
ch28 modernization moves the pin to upstream libp2p (0.53+).
