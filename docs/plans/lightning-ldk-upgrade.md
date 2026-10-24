# Plan: rust-lightning (LDK) uplift

**Status:** parked (scoping only since 2026-08-06; sequence it last) — **Last reviewed:** 2026-10-18

> **Context:** scoping only, 2026-08-06. This is the largest and
> highest-risk item in the current dependency-hygiene family. It touches
> funds-custody code (channel state, HTLCs, on-chain claims) directly, unlike
> every other item tracked in `v0.2.0-dependency-hygiene.md`. Do not start
> this opportunistically; it needs its own review pass and its own timeline,
> separate from the smaller mechanical bumps.

## Goal

Move `rust-lightning-patched` (the vendored, KDF-patched LDK fork currently
pinned at **0.0.106**) to a current upstream LDK release, without changing
channel-state persistence format, HTLC handling, or funds-custody guarantees,
and while preserving every local KDF patch already applied on top.

## Current state

- `rust-lightning-patched/` is a full vendored subtree (not a git dependency)
  at LDK `lightning 0.0.106` / `lightning-invoice 0.14.0` /
  `lightning-net-tokio 0.0.106` / `lightning-background-processor 0.0.106`
  (`lightning-persister` is reloaded's own crate, not upstream LDK's).
- It pulls `bitcoin 0.27.1`, which pulls `secp256k1 0.20.3` /
  `secp256k1-sys 0.4.0`. As of 2026-08-06 (`secp256k1-migration.md`'s "Scope
  reduction" section), `bitcoin`/`lightning`/`lightning-invoice`/
  `lightning-background-processor` are correctly scoped to `not(wasm32)` —
  Lightning has zero WASM footprint, confirmed by `cargo tree --target
  wasm32-unknown-unknown -p coins`. That plan's secp256k1 0.20→0.29 work
  does **not** touch this pin — the two projects are fully independent now.
  This uplift can bump (or vendor-patch) `bitcoin`/`secp256k1` on whatever
  timeline it needs, native-only, with no WASM coupling to worry about.
- **The actual `bitcoin`-crate touchpoint from reloaded's own code is
  narrow and self-contained**, worth knowing before scoping the inventory
  step below: `mm2src/coins/lightning/ln_platform.rs`'s `kdf_tx_to_bitcoin`
  serializes a `chain::Transaction` with KDF's own wire codec and
  re-deserializes the bytes via `bitcoin::consensus::encode::deserialize` —
  a byte round-trip, not a type-level conversion trait, and it's the *only*
  place reloaded's own code bridges `kdf_chain`'s types into `bitcoin`'s.
  (`kdf_chain` itself — the clean-room GPL-2.0 rewrite of the old
  `mm2_bitcoin/chain` — doesn't depend on the `bitcoin` crate at all, unlike
  the sibling GLEEC KDF codebase's `mm2_bitcoin/chain`, which has a
  feature-gated `ext-bitcoin` conversion layer; see
  [GLEECBTC/komodo-defi-framework#2722](https://github.com/GLEECBTC/komodo-defi-framework/pull/2722)
  for what that coupling looks like there and how a feature-default change
  silently broke their `mm2_main` build over it. Reloaded doesn't share that
  exposure, but it's the same underlying `bitcoin`/Lightning boundary, and a
  useful reference for what *not* to reintroduce during this uplift.)
- `docs/reloaded-rewrite/41-lightning-network.md` §41.8 already did a
  feasibility pass on a full uplift for one specific feature
  (`update_channel` / live per-channel fee/policy mutation) and **explicitly
  deferred it**: 0.0.106's `ChannelConfig` already exposes everything needed
  for that one RPC, so a narrow ~50-line vendored patch (adding the one
  missing runtime-mutation entry point LDK itself added upstream in 0.0.107)
  shipped instead of an uplift. That doc names `0.0.113` as the
  "matching corpus" reference version considered at the time — **treat that
  as stale; re-check the current upstream LDK release before scoping this
  plan further**, since LDK ships frequent releases and this doc was written
  against whatever was current when §41.8 landed.
- No `deny.toml` advisory currently forces this — LDK 0.0.106 doesn't appear
  to have a RUSTSEC entry itself. This is pure staleness/maintenance debt,
  not a security gate. That means there's no CI pressure forcing a timeline;
  sequence it last and don't let it block the smaller items.

## Why this is hard

- **It's a vendored fork, not a dependency bump.** There is no `Cargo.toml`
  version bump to do — this means diffing a multi-year LDK upstream delta
  (0.0.106 → current) against the local KDF patch set, re-applying every KDF
  patch (channel-config runtime mutation from §41.8, and whatever else has
  accumulated — inventory this first) on top of the new base, and
  re-verifying each one still does what it did before.
- **API churn across ~15+ LDK point releases** is substantial: channel
  manager construction, event handling, persistence trait signatures, and
  onion-routing internals have all changed multiple times between 0.0.106
  and any current release. This is not a mechanical rename pass like the
  secp256k1 migration — expect real logic changes at call sites.
- **Funds custody.** Channel state persistence format changes between LDK
  versions are the primary risk: an uplift that changes on-disk channel
  monitor format without a correct migration path can strand funds in open
  channels. This needs an explicit persistence-format compatibility check
  and, if the format changed, a migration path — not just "upgrade and
  recompile."
- **`bitcoin 0.27.1` coupling.** A real uplift will very likely also pull a
  newer `bitcoin`/`secp256k1` version transitively (upstream LDK has moved
  its own `bitcoin` pin forward too), which would *then* naturally converge
  with `secp256k1-migration.md`'s target line — worth doing the secp256k1
  migration first so this uplift lands on an already-modern secp256k1
  baseline instead of introducing a third one.

## Scope

- `rust-lightning-patched/lightning`
- `rust-lightning-patched/lightning-invoice`
- `rust-lightning-patched/lightning-net-tokio`
- `mm2src/coins/lightning_background_processor`
- `mm2src/coins/lightning_persister` (reloaded-owned; check its trait
  surface against whatever persistence-trait shape the new LDK version
  expects)
- `mm2src/coins/lightning/` (the `coins` crate's LN integration module,
  `#[cfg(not(target_arch = "wasm32"))]`, native-only — see
  `mm2src/coins/lightning/mod.rs`)
- Every local KDF patch currently applied to the vendored tree (inventory
  required — start from `git log` / diff against a clean upstream 0.0.106
  checkout to enumerate them precisely; §41.8's channel-config-mutation
  patch is at least one, there may be others accumulated since)

## Planned order

1. **Inventory current local patches.** Diff `rust-lightning-patched/`
   against a clean upstream `v0.0.106` checkout to get the exact patch list
   (don't rely on memory/docs — some patches may be undocumented).
2. **Pick the target upstream version.** Check current LDK releases; prefer
   the newest stable tag with a published changelog covering the persistence
   format, rather than an arbitrary "next" bump.
3. **Sequence after `secp256k1-migration.md`.** Land the workspace-wide
   secp256k1 0.20 → 0.29 migration first (independent per that plan's
   scope), so this uplift's `bitcoin`/`secp256k1` transitive bump lands on
   an already-unified baseline instead of creating new multi-version churn.
4. **Stand up a parallel worktree/branch** with the new upstream LDK
   vendored in, unpatched, and get it compiling standalone first (no KDF
   integration) to isolate upstream API churn from patch-reapplication work.
5. **Re-apply each inventoried KDF patch** one at a time against the new
   base, verifying compilation and (where testable) behavior after each.
6. **Persistence-format compatibility check.** Confirm whether channel
   monitor / manager on-disk format changed between 0.0.106 and the target;
   if so, design and test an explicit migration path before touching any
   channel-state code further.
7. **Update `mm2src/coins/lightning/`** for any changed LDK API surface
   (event types, channel manager construction, HTLC handling).
8. Full native build + existing Lightning integration/unit tests + manual
   testnet channel-open/close/payment smoke test before merge.

## Risk areas

- Channel state persistence format compatibility (funds-custody critical)
- HTLC/onion-routing behavior changes between LDK versions
- Event-handling API churn breaking `coins`'s LN event dispatch
- Silent loss of an undocumented local KDF patch during re-application
- Interaction with the eventual `secp256k1-migration.md` bump (sequencing,
  above)

## Required verification

- Standalone build of the newly-vendored LDK tree before KDF integration
- Full native build + `cargo test` for every crate in scope
- Explicit before/after diff review of every re-applied KDF patch against
  its original intent (not just "it compiles")
- Manual testnet Lightning smoke test: open channel, route/forward a
  payment, force-close, verify on-chain claim — this is not covered by any
  existing automated test per the current repo structure and must be done
  by hand before merge
- Persistence-format migration test if the format changed (round-trip an
  existing 0.0.106-format channel monitor through the migration path)

## Release posture

Do not attempt this as a background task. It needs a dedicated branch
(`dep/lightning-ldk-uplift` off `dev`), its own extended review window, and
explicit sign-off given the funds-custody surface — this is qualitatively
different risk from every other item in this dependency-hygiene family.
Sequence it last: after secp256k1, Solana, and WalletConnect, since none of
those block on it and it has no advisory forcing function.
