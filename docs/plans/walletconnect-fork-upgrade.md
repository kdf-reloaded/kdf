# Plan: WalletConnect relay transport — tungstenite bump

> **Status:** scoped, **deferred** — 2026-08-06. See "Priority assessment"
> below: this is not currently worth the ownership/maintenance cost it would
> take on. Revisit if the threat model changes or upstream fixes it for free.

## Goal

Clear `RUSTSEC-2023-0065` (tungstenite DoS via unbounded frame buffering on
large frames) from the WalletConnect relay client's native transport, without
changing the WalletConnect pairing/relay wire protocol or breaking the WASM
transport path — *if and when* this is judged worth doing (see below).

## Second advisory in the same fork (added 2026-09-23)

`relay_rpc` also pulls **`jsonwebtoken` 8.3.0**, which carries
GHSA-h395-gr6q-cpjc (type confusion in claim validation — a standard claim
supplied with the wrong JSON type is treated as absent, so a `validate_nbf` /
`validate_exp` check silently does not run unless the claim is also in
`required_spec_claims`). It has no RustSec id, so `cargo deny check advisories`
cannot see it at all; it was found through Dependabot on 2026-09-23.

It is **dismissed as `not_used`**, and correctly so today: `relay_rpc` signs by
hand with `ed25519_dalek` (`relay_rpc/src/jwt.rs:106-121`) and links
`jsonwebtoken` only through `VerifyableClaims::decode` (`jwt.rs:260`), whose
sole callers are upstream's own unit tests. KDF uses the websocket client and
never verifies a relay-supplied JWT. That stops being true the moment anything
here uses `relay_client::http` (the `watchRegister`/`watchUnregister` path) or
starts verifying a token.

This does not change the priority assessment below on its own — it is a second
*dormant* advisory, not a second live one. But it does mean the fork bump now
clears two advisories rather than one, and both live in the same two files, so
do `jsonwebtoken` 8 -> 10.3+ in the same upstream pass as the tungstenite bump
rather than as separate work. Tracked alongside it in
`docs/plans/v0.2.0-dependency-hygiene.md` section F2.

## Confirmed root cause and ownership (corrected 2026-08-06)

**Correction:** an earlier version of this doc claimed the fork chain here
was reloaded-controlled and that `deny.toml`'s "upstream/fork-blocked" label
was mistaken. That was inaccurate, corrected by the project owner: the
`KomodoPlatform`/`komodoplatform` GitHub org (both casings, same org —
`rust-libp2p`, `tokio-tungstenite-wasm`, `walletconnectrust`) is maintained
by Gleec, who acquired the Komodo company roughly nine months prior to this
writing. Reloaded does not maintain that org. `deny.toml`'s original
"upstream/fork-blocked" framing was correct; this doc's earlier "correction"
was the actual mistake. Pull requests can be opened there — the project
owner has done so before (see `GLEECBTC/komodo-defi-framework#2722`) — but
review/merge activity on Komodo-universe repos currently appears limited, so
in practice a fix here would still mean maintaining an independent fork
rather than waiting on a merge.

```
kdf_walletconnect
 └─ relay_client (git: komodoplatform/walletconnectrust, tag k-0.1.3)  [Gleec-maintained]
     └─ tokio-tungstenite-wasm 0.1.1-alpha.0
        (git: KomodoPlatform/tokio-tungstenite-wasm, rev 8fc7e2f  [Gleec-maintained]
         — itself a fork of TannerRogalsky/tokio-tungstenite-wasm)
         └─ [target.not(wasm32)] tokio-tungstenite = "0.16"
             └─ tungstenite 0.16.0   ← the flagged crate
         └─ [target.wasm32] raw `web-sys::WebSocket` (no tungstenite at all)
```

Verified directly against both forks' checked-out manifests: `relay_client`
depends unconditionally on `tokio-tungstenite-wasm`, which internally
branches on `target_arch = "wasm32"` — the WASM build path uses a raw
`web-sys::WebSocket` shim with **no tungstenite dependency at all**, so this
advisory only affects the **native** relay transport. That also means the
WASM path is not at risk here and needs no change.

## Priority assessment (2026-08-06)

Applying the responsibility framework the project owner articulated: as
long as reloaded consumes this exact code unmodified, its security posture
is effectively shared with the upstream KDF project (which pulls the
identical `tungstenite 0.16.0` through the identical fork chain) — fixing it
here would mean maintaining an independent fork, which shifts ongoing
maintenance of that dependency onto reloaded permanently: every future
tungstenite/websocket security advisory would need to be tracked and
re-ported locally, rather than arriving as part of an upstream update. That
cost is only worth taking on if the threat is concrete enough to justify it.

**RUSTSEC-2023-0065 is a DoS only** (unbounded memory growth from an
oversized frame/message — no key material exposure, no RCE, no
authentication bypass). Reloaded's exposure is a **WebSocket client**
connecting outbound to a WalletConnect relay server — not a public-facing
server accepting arbitrary inbound connections. The realistic attacker is a
malicious or compromised relay endpoint (or a break in TLS) able to crash
the local KDF process by sending an oversized frame. That's a real
node-availability risk while the WalletConnect feature is active (a crash
mid-swap could leave funds in an HTLC needing manual recovery), but it's
bounded to that one feature and doesn't touch key material or fund custody
directly — not in the same tier as a signing/key-derivation bug.

**Recommendation: defer.** Keep the `deny.toml` ignore line. Taking on
permanent fork-maintenance for this isn't warranted right now, given it's
DoS-only, client-side, and scoped to an optional feature. Revisit if: the
upstream fork is updated (then it's a free tag bump, no added maintenance);
the WalletConnect relay usage pattern changes to something more exposed
(e.g. connecting to untrusted/arbitrary relays rather than a known
operator); or reloaded ends up maintaining a fork of this dependency chain
for an unrelated reason anyway (at which point fixing this becomes nearly
free as a side effect). The mechanical plan below is preserved in case any
of those trigger it.

The fix, if undertaken, would mean maintaining a fork of Gleec's
repositories at two hops:

1. In `KomodoPlatform/tokio-tungstenite-wasm`, bump `tokio-tungstenite =
   "0.16"` (native target block) to a current release (pulls a current,
   non-vulnerable `tungstenite`). Port any native-path API breakage — check
   `tokio-tungstenite`'s changelog between 0.16 and the target version for
   `WebSocketStream`/message-type changes; the crate's native branch is a
   thin wrapper so expect a small, mechanical diff, but verify.
2. In `komodoplatform/walletconnectrust`'s `relay_client/Cargo.toml`, bump
   the pinned `rev` to the new `tokio-tungstenite-wasm` commit from step 1,
   and cut a new tag (or bump `k-0.1.3` → `k-0.1.4`) for reloaded to pin to.
3. In this repo's workspace `Cargo.toml`, bump the `walletconnectrust`
   `tag` for `pairing_api`/`relay_client`/`relay_rpc`/`wc_common` to the new
   tag from step 2.

## Scope

- `KomodoPlatform/tokio-tungstenite-wasm` (Gleec-maintained repository —
  would need a maintained fork if a pull request there isn't merged, per
  the priority assessment above)
- `komodoplatform/walletconnectrust` (Gleec-maintained repository — same
  caveat)
- `Cargo.toml` (workspace) — `pairing_api`/`relay_client`/`relay_rpc`/
  `wc_common` git `tag` bump (this repo's side, last)
- `mm2src/kdf_walletconnect/` — no expected direct code change (it consumes
  `relay_client`'s public API, not `tungstenite` directly), but run its full
  test suite since it's the only consumer of the bumped transport

## Planned order

1. Confirm current upstream `tungstenite`/`tokio-tungstenite` stable release
   and scan its changelog from 0.16 for breaking API changes relevant to
   `tokio-tungstenite-wasm`'s (small) native-path usage.
2. Land the bump in `KomodoPlatform/tokio-tungstenite-wasm`; build + test
   that crate standalone (native target) before touching anything else.
3. Land the `rev`/tag bump in `komodoplatform/walletconnectrust`'s
   `relay_client`; build + test `relay_client` standalone.
4. Bump the tag in this repo's workspace `Cargo.toml`; full
   `cargo build`/`cargo test -p kdf_walletconnect` + WASM check (should be a
   no-op for WASM per the root-cause section above, but verify — a no-op
   confirmation is still a required check, not an assumption).
5. Remove `RUSTSEC-2023-0065` from `deny.toml`'s `ignore` list;
   `cargo deny check advisories` must stay green.

## Risk areas

- Native relay transport reconnect/backoff behavior if `tokio-tungstenite`'s
  API shape changed between 0.16 and the target version
- WalletConnect pairing/session-persistence flows that depend on the relay
  transport staying connected during a session — regression-test an actual
  pairing + message round-trip, not just a compile check
- Keeping the WASM path untouched (it doesn't use tungstenite) — a
  regression here would be a sign the bump touched more than intended

## Required verification

- `cargo tree -i tungstenite@0.16.0` returns nothing after the bump
- Native `cargo test -p kdf_walletconnect` and `-p relay_client` (via
  `--manifest-path` against the updated fork checkout during development)
- `cargo check --target wasm32-unknown-unknown -p kdf_walletconnect` stays
  clean (regression check that the WASM path, which doesn't touch
  tungstenite, wasn't broken)
- Manual smoke test: pair with a real WalletConnect-compatible wallet and
  round-trip at least one signing request over the native relay transport
- `cargo deny check advisories` green with the ignore line removed
- `cargo tree -i jsonwebtoken` shows >= 10.3.0 if the second advisory above was
  taken in the same pass; reopen and re-close Dependabot alert #26 rather than
  leaving it dismissed as `not_used`, since it is then actually fixed

## Release posture

Independent of `secp256k1-migration.md` and `solana-sdk-upgrade.md` — can
run in parallel with either. Needs its own branch
(`dep/walletconnect-tungstenite`) off `dev` since it spans two external
fork repos plus this one; land the two upstream-fork commits and get them
merged/tagged before starting the reloaded-side branch, so the reloaded PR
is a clean single-tag bump rather than three repos moving at once.
