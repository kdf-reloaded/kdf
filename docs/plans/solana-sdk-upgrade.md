# Plan: Solana SDK ed25519-dalek 1.x → 2.x

**Status:** parked (deferred by choice 2026-08-06; upstream-pinned, would need a fork) — **Last reviewed:** 2026-10-18

> **Context: scoped, deferred by choice — 2026-08-06.** Checked reloaded's
> actual Solana signing code (see "Confirmed root cause" below): the
> vulnerable-pattern precondition for RUSTSEC-2022-0093 doesn't appear to be
> met, and both advisories are DoS/side-channel-class rather than directly
> key-compromising for reloaded's usage. Per the risk-inheritance framework
> in `v0.2.0-dependency-hygiene.md`, forking Solana Labs/Anza's SDK to fix
> this isn't worth the permanent maintenance ownership it would take on
> right now. Not queued as next work. The options/plan below are preserved
> in case that changes.

## Goal

Clear `RUSTSEC-2022-0093` (ed25519-dalek 1.x double-public-key signing
oracle) and `RUSTSEC-2024-0344` (curve25519-dalek timing variability) from
the Solana integration without changing Solana address derivation, signing,
or wire behavior.

## Confirmed root cause (2026-08-06)

This is **not** a lazy caret-range pin reloaded hasn't bumped — it's an
**exact upstream pin**. Both `solana-keypair 2.2.3` and `solana-signature
2.3.0` (the current Solana SDK 2.x line, already the newest major, resolved
via the workspace's `solana-* = { version = "2", ... }` pins in
`mm2src/coins/Cargo.toml`) declare:

```toml
[dependencies.ed25519-dalek]
version = "=1.0.1"
```

(verified directly against the registry-cached manifests). `solana-signature`
pulls the same exact pin behind its `verify` feature, which `coins/Cargo.toml`
explicitly enables. So there is no version selector on reloaded's side that
gets a newer `ed25519-dalek` here — every current `anza-xyz/solana-sdk` 2.x
release is hard-pinned. This is what deny.toml's section B ("Solana SDK —
upstream-blocked") already correctly labels it, but the pin is now confirmed
exact rather than assumed loose.

The same `curve25519-dalek 3.2.0` (pulled transitively by `ed25519-dalek
1.0.1`) previously also arrived via the libp2p 0.45 fork's Noise stack; that
path is gone now that the libp2p 0.52 fork migration is merged (confirmed via
`cargo tree -i curve25519-dalek@3.2.0` — the only remaining path is through
`solana-keypair`/`solana-signature`/`ed25519-dalek 1.0.1`). So clearing this
advisory no longer needs to wait on libp2p fork modernization (section A) —
it's purely a Solana-side fix now.

**Exploitability check against reloaded's actual code (2026-08-06):**
RUSTSEC-2022-0093's "double public key signing oracle" attack requires a
keypair reconstructed from a public key and secret key sourced
*independently* (e.g. an externally-supplied/untrusted public key paired
with a locally-held secret) — that's the precondition that makes the
mismatch exploitable. `mm2src/coins/solana/solana_types.rs`'s
`generate_keypair_from_slice` always derives the keypair from a single
32-byte seed (`ed25519_dalek::SigningKey::from_bytes(&secret)` →
`.to_keypair_bytes()` → `solana_keypair::keypair_from_seed`), so secret and
public key are always paired/derived together, never independently
sourced. This precondition does not appear to be met anywhere in reloaded's
Solana signing path (grep confirms `generate_keypair_from_slice` is the
only keypair-construction site in `coins/solana/`). Not a guarantee — a
full audit of every `ed25519_dalek`/`solana_keypair` call site would be
needed for certainty — but no evidence of the vulnerable pattern.
`curve25519-dalek`'s timing-variability issue (RUSTSEC-2024-0344) is a
local/co-located side-channel concern, not practically remote-exploitable
over a network.

## Options

**A. Upstream tracking (lowest effort, indefinite timeline).** Watch
`anza-xyz/solana-sdk` for a release that relaxes the `=1.0.1` pin to
`ed25519-dalek 2.x`. No reloaded-side code work, but no control over timing;
Solana's SDK has historically been slow to move this pin (it was already
old when this repo's `solana-*` deps were first added at "version 2").

**B. Fork `solana-keypair`/`solana-signature` (moderate effort, full
control).** Vendor-patch (KDF-PATCH.md style, like the zcash `time-core`
precedent) or fork these two small crates, bump their `ed25519-dalek` pin to
`2.x`, and port their (small — these are thin wrapper crates, not the full
SDK) internal usage to the dalek 2 API (`Keypair` → `SigningKey`, merged
`PublicKey`/`SecretKey`, `Signature` repr change). Lower blast radius than
it sounds since these are leaf crates with a narrow surface, but it's still
new maintenance burden reloaded would own indefinitely (re-diff on every
future `solana-keypair` bump).

**C. Replace with a reloaded-owned thin wrapper (highest effort, cleanest
result).** `mm2src/coins/solana/` (~3000 lines total; check exact call sites
during inventory) likely only needs Ed25519 keypair generation, signing, and
`solana-pubkey`-compatible address formatting from `solana-keypair`/
`solana-signature` — not their full API surface. A small reloaded-owned
`Signer` implementation directly on `ed25519-dalek 2` + `solana-pubkey`
(which is *not* pinned to old dalek — only `solana-keypair` and
`solana-signature`'s `verify` feature are) would drop the old dalek line
entirely without waiting on upstream or forking a whole crate. This is the
same class of decision as the `sia-rust` dalek 1→2 migration already done in
this repo (see `v0.2.0-dependency-hygiene.md`'s "Done" list) — that precedent
is worth reviewing first since it's the closest prior art in this exact
codebase.

**Recommendation (superseded 2026-08-06, see status header): deferred.**
Given the exploitability check above found no evidence the vulnerable
pattern applies to reloaded's code, and both advisories are DoS/side-
channel-class rather than directly key-compromising, this isn't worth
forking Solana Labs/Anza's SDK for right now (option B), let alone
reimplementing the signer to escape it entirely (option C) — both are real
ownership-cost decisions that should wait for a concrete reason. **Option A
(upstream tracking) is the current choice: do nothing, watch for upstream
to relax the pin.** If reloaded ends up needing `coins/solana/`'s keypair
handling touched for an unrelated reason, revisit C then — the `sia-rust`
dalek 1→2 precedent means it's a known-tractable pattern, not a blocker.

## Scope

- `mm2src/coins/solana/` (`mod.rs`, `solana_common.rs`, `solana_types.rs`,
  `rpc_client.rs`, `rpc_pool.rs`, `spl.rs`, plus their test files)
- `mm2src/coins/Cargo.toml` `solana-*` dependency block (lines ~186–196)

## Planned order

1. Inventory every `solana_keypair::`/`solana_signature::` (and any
   re-exported `ed25519_dalek::`) call site in `mm2src/coins/solana/`;
   classify as keygen, signing, verification, or serialization-only.
2. Decide B vs. C based on that inventory (see Recommendation above).
3. If C: implement the reloaded-owned signer against `ed25519-dalek 2`,
   matching `solana-keypair`'s exact byte-level keypair/signature encoding
   (this must produce byte-identical signatures/addresses to today's
   `solana-keypair 2.2.3` output — treat this as a wire-compatibility
   constraint, not just an API-compatibility one).
4. If B: fork, patch, port call sites, same wire-compatibility constraint.
5. Regression-test against known-good vectors: existing wallet addresses and
   at least one previously-broadcast signed transaction, to confirm
   byte-identical output before/after.

## Risk areas

- Address derivation must stay byte-identical (existing users' Solana
  addresses must not change)
- Signature encoding must stay byte-identical (network/RPC compatibility)
- Any accidental widening of the dependency surface if forking pulls in
  more of `ed25519-dalek 2`'s API than intended

## Required verification

- `cargo tree -i ed25519-dalek@1.0.1` / `-i curve25519-dalek@3.2.0` return
  nothing after the change
- Regression test: known keypair seed → same derived pubkey, same signature
  over a fixed test message, before and after
- `coins` unit/integration tests covering Solana activation, balance, and
  transaction signing/broadcast
- `cargo deny check advisories` green with RUSTSEC-2022-0093 and
  RUSTSEC-2024-0344 removed from `deny.toml`'s `ignore` list

## Release posture

Independent of `secp256k1-migration.md` and `walletconnect-fork-upgrade.md`
— can run in parallel with either, on its own branch
(`dep/solana-sdk-dalek2`) off `dev`. Not funds-custody-adjacent in the same
way as Lightning, but wallet-address/signature correctness still warrants
the byte-identical regression check above before merge.
