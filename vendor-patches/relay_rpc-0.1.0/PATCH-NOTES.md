# relay_rpc — vendor patch

## Provenance

Copied verbatim from `komodoplatform/walletconnectrust`, tag `k-0.1.3`
(commit `e2fb03a`), path `relay_rpc`, then patched. Redirected into the graph
with:

```toml
[patch."https://github.com/komodoplatform/walletconnectrust"]
relay_rpc = { path = "vendor-patches/relay_rpc-0.1.0" }
```

## Why it exists

`relay_rpc` depended on `jsonwebtoken` 8.3.0, which carries
**GHSA-h395-gr6q-cpjc**: a type confusion in claim validation. A standard claim
(`nbf`, `exp`) supplied with the wrong JSON type is marked `FailedToParse`, and
the validator treats that identically to `NotPresent` — so an enabled check is
silently skipped unless the claim is also listed in `required_spec_claims`. It
also pulled in `ring` 0.16.20 transitively.

The advisory has **no RustSec id**, so `cargo deny check advisories` could not
see it; it surfaced only through Dependabot, in the 2026-09-23 triage.

It was not exploitable here, for two independent reasons — but the fix was
cheaper than continuing to explain that:

1. `relay_rpc` never used `jsonwebtoken`'s claim validation at all. The single
   call site (`src/jwt.rs`) used `jsonwebtoken::crypto::verify`, which checks a
   signature and nothing else; claim validation is hand-rolled in
   `VerifyableClaims::verify_basic` in the same file.
2. KDF never reaches that path. It mints and sends relay tokens
   (`mm2src/kdf_walletconnect/src/lib.rs:918-926`) over the websocket client;
   the only callers of `decode` in the whole tree were upstream's own unit tests.

## The patch

One call site, in `src/jwt.rs`. `jsonwebtoken::DecodingKey::from_ed_der` +
`jsonwebtoken::crypto::verify` are replaced with direct `ed25519_dalek`
verification:

- the public key comes from `claims.basic().iss.0.as_public_key()` — the same
  accessor the signing side (`VerifyableClaims::encode`) already uses to check
  the keypair against `iss`;
- the signature is decoded with `data_encoding::BASE64URL_NOPAD`, the same
  encoder `encode` uses to produce it;
- verification is `VerifyingKey::verify_strict`, chosen over `verify` because it
  rejects small-order and non-canonical public keys, which is the stricter
  behaviour and closer to what `ring` did underneath `jsonwebtoken`.

Signing and verification are therefore symmetric, both in `ed25519_dalek`, which
was **already a direct dependency** of this crate (`ed25519-dalek = "2.1.1"`) —
so nothing was added. `jsonwebtoken` is removed from `Cargo.toml`, and both it
and its `ring` 0.16.20 edge leave the dependency graph.

Nothing else is changed. The JWT wire format is untouched: same header, same
base64url-nopad encoding, same Ed25519 algorithm, same `JwtError` variants on
failure.

One subtlety worth recording, because the first attempt got it wrong: a
signature that will not even base64-decode must map to `JwtError::Signature`,
not `JwtError::Encoding`. `jsonwebtoken::crypto::verify` decoded internally and
any failure fell through relay_rpc's `_ => Err(JwtError::Signature)` arm, so
that was the observable behaviour upstream. Upstream's own `token_validation`
test pins it, using an 87-character signature — not a valid base64url-nopad
length for 64 bytes — and expects `JwtError::Signature`. It caught the
divergence.

## Test status

`cargo test --manifest-path vendor-patches/relay_rpc-0.1.0/Cargo.toml` →
**44 passed, 0 failed**. The ones that matter here are `jwt::test::token_validation`
(the negative cases: tampered signature, bad multicodec header/base, bad DID
prefix/method) and `rpc::watch::test::watch_{register,unregister,event}_jwt`,
which are full encode-then-decode round trips — so both the accept and the
reject paths of the replaced code are covered by upstream's own tests.

## Removing this patch

Delete the directory and drop the
`[patch."https://github.com/komodoplatform/walletconnectrust"]` stanza and the
`exclude` entry once the fork bumps `jsonwebtoken` to >= 10.3 itself. That work
is tracked in `docs/plans/walletconnect-fork-upgrade.md`, which also has to
touch this fork for the accepted `tungstenite` advisory — do both in one pass.
