# KDF Reloaded v0.1.0-alpha.1 — Comparison to GLEEC KDF v\<X.Y.Z\>

> **Status: skeleton.** Detail sections will be filled in as v0.1.0-alpha.1 is finalised. The summary lists below are the authoritative public catalogue of differences once the alpha is tagged.

## What is KDF Reloaded?

KDF Reloaded is a GPLv2-only continuation of the Komodo DeFi Framework codebase, anchored to upstream commit `c1d46c0c1592faa0860f704008b2b2381bc3840f` (2022-06-03) — the last commit unambiguously distributed under GPLv2-only by upstream. Post-anchor history in this repository contains independently authored work plus selected imported/adapted components from publicly available, license-compatible sources; see [`docs/reloaded-rewrite/34-provenance-ledger.md`](docs/reloaded-rewrite/34-provenance-ledger.md).

**The project is openly hybrid.** It does not claim that every file is a clean-room rewrite. Source falls into four classes: (1) clean-room originals; (2) constrained-expression fragments (interop/wire-format, convergent-idiomatic, generated, third-party-API-bound — byte-identity expected and lawful under merger doctrine / de minimis); (3) code adapted in place from permissive upstreams (recorded per CRD §1 R27); and (4) **lineage-derived** content from the upstream/GLEEC KDF work product, carried under GPLv2 copyleft. The load-bearing license theory is **GPLv2 copyleft**, not clean-room: under GPLv2 §2(b) integrated derivatives of GPLv2 code are themselves GPLv2, and under GPLv2 §6 (equivalently GPLv3 §7/§10) a downstream fork cannot impose "further restrictions" on GPL-covered code. Clean-room rewriting is applied selectively as additional polish, not as the basis for the right to distribute. The basis for each class is documented in [§34.5 of the provenance ledger](docs/reloaded-rewrite/34-provenance-ledger.md).

Two post-divergence subsystems are named explicitly here to avoid any ambiguity:

- **WalletConnect** (`mm2src/kdf_walletconnect/`) is a post-anchor feature (same crate path as the upstream/GLEEC KDF WalletConnect work product). Its substantive files were **clean-room reimplemented** (2026-06-17) against the Chapter 22 driving-spec and are gated under the R35/R36 residual-similarity rules; they are classified as **clean-room originals** in the provenance ledger, with GPLv2 copyleft retained only as a backstop. A few dictated or thin files (third-party-API-bound or convergent-idiomatic) remain listed there.
- **Orderbook** (`lp_ordermatch`) descends *largely* from pre-GLEEC common ancestry — C-ported legacy code present at the anchor. It is **not** wholly common ancestry, however: it also carries post-anchor additions (e.g. `recently_cancelled` stale-cancellation handling) that are lineage-derived and carried under GPLv2 copyleft, not by direct descent.

Credit for the original design and the bulk of the pre-anchor code is due to Komodo Platform and the AtomicDEX-API contributors, and for ongoing development on the parallel branch to GLEEC and the GLEEC KDF contributors. KDF Reloaded aims to remain **operationally compatible where practical** with GLEEC KDF: where we diverge, we expose per-feature compatibility switches so existing GLEEC-KDF integrations can retain the original behaviour with explicit configuration. We follow GLEEC's evolution and incorporate compatible changes wherever it is legally and technically feasible and where the change does not contradict our own goals (a free, open trading platform under GPLv2).

The GLEEC fork of the same upstream codebase diverged from the joint history at upstream commit `d36369980a6c08f8689b64df56fbccf0097a0a6f` and continues under a different licensing posture. KDF Reloaded and GLEEC KDF share the pre-`d3636998` history; post-divergence histories are maintained independently, with any imported/adapted material expected to be documented in project provenance records.

For licensing details, see [`LEGAL/LICENSE`](LEGAL/LICENSE) and [`LEGAL/COPYING`](LEGAL/COPYING).

## Compatibility convention

KDF Reloaded does **not** define a single global compatibility mode, and does **not** introduce a common code construct (no `compatibility` JSON object, no `CompatMode` enum, no shared registry). Instead, every behavioural divergence from GLEEC KDF that could affect operators, users, or third-party API integrations is implemented in whatever shape fits the feature, and is then documented in two specific places so that any operator can reach the GLEEC-equivalent behaviour:

1. Next to the setting itself, a short "set this to `<value>` for GLEEC compatibility" note.
2. A row in the central admin chapter [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md), which lists every such setting end-to-end.

The developer-facing rule (mandatory for AI assistants, strong recommendation for human contributors) lives in [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md). For v0.1.0-alpha.1 the central chapter is empty — no behavioural divergences require operator configuration yet — but the convention, the central chapter scaffold, and the developer rule are in place.

## Summary

### Added in KDF Reloaded

- Compatibility convention and central admin chapter for GLEEC-equivalent operation; see [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md) and [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
- `regtest-netid` Cargo feature exposing test-only netids 8100, 8999, 9000, 9998 (off by default in production builds).
- `allow_insecure_key_export` `MM2.json` switch (default `false`) gating GLEEC-parity key export (offline / no-activation, HD per-derivation-path ranges, shielded ZHTLC viewing keys); see [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md) and CRD chapter 07. The secure default refuses the export superset; the single-coin activated reveal and own-mnemonic self-export remain available regardless.
- On-demand `Build Linux` GitHub Actions workflow for release-profile binaries.
- Self-hosted CI runner support ([`docs/CI_RUNNERS.md`](docs/CI_RUNNERS.md)).
- Split CI: format → matrix unit-tests → docker-tests, with cancel-in-progress concurrency.
- Specific mismatch-kind reporting in UTXO maker-payment validation (better operator diagnostics).
- Read-only fallback for legacy `<db_root>/wallets/*.wallet` wallet files written by an earlier reloaded build (two-field envelope). New wallets are always written as the canonical `<db_root>/<name>.json` record; the legacy form is read (for login, listing and deletion) but never written. This fallback has no analogue in GLEEC KDF, which never produced the `.wallet` form. The on-disk format does not by itself select HD vs single-address signing (that is governed by `enable_hd` and per-coin activation); see CRD chapter 07 §7.5/§7.7 R9A.
- Reloaded-specific Z-coin shielded database names isolate the upgraded stable
  `librustzcash` schema from GLEEC KDF's legacy schema. See
  [Shielded database isolation](#shielded-database-isolation) below.

### Behavioural divergences without a compatibility switch

These are divergences from GLEEC KDF that are **not** operator-configurable in this release (no compat switch yet). They are logged here and in [`CHANGELOG.md`](CHANGELOG.md) per the convention; a per-feature switch may be added in a later release.

- **Withdraw with an omitted HD `from`.** For an HD-mode UTXO wallet, `task::withdraw` / `withdraw` with no `from` sender defaults to the wallet's enabled address (account 0, external chain, index 0) instead of failing. GLEEC KDF rejects an omitted `from` with `FromAddressNotFound`. This makes the shipped Komodo DeFi wallet's withdraw-preview flow (which omits `from`) work against a reloaded node. An explicit but invalid `from` is still rejected (`UnknownAccount` / `UnexpectedFromAddress`). Code: `coins/utxo/utxo_common/utxo_common_hd.rs` (`get_withdraw_hd_sender`), `utxo_common_helpers.rs` (`validate_task_withdraw_sender`).
- **Z-coin shielded database schema and names.** Reloaded uses the modern stable
  Zcash wallet schema under Reloaded-only filenames. GLEEC KDF continues to use
  the legacy filenames and schema. This is intentionally not configurable:
  sharing either file between the two implementations would permit an older
  binary to open a schema it does not understand and could compromise shielded
  wallet state.

### Removed / disabled in KDF Reloaded

- iOS build target removed from the alpha release matrix.
- *(further entries to be enumerated as the alpha is finalised.)*

### Work in progress

- Hardware wallet — Ledger transport (scaffolding only; crate not built into any artifact).
- Siacoin atomic-swap operations (HD-wallet stubs only; not wired to any default activation flow).
- HD wallet dispatch in swap/ordermatch paths (legacy iguana-key fallback retained for parity with upstream and GLEEC; documented in code).
- *(further entries to be enumerated.)*

### Settings to set for GLEEC-compatible operation

See [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). To match GLEEC KDF's full key-export behaviour, set `allow_insecure_key_export` to `true` (default `false`); no other operator configuration is required to match GLEEC KDF behaviour.

## Detail sections

Detail subsections will be added as features land. Each entry above expands here with: rationale, code locations, test coverage, migration notes, and links to relevant pull requests.

### Anchor and divergence

- **Joint history ends at:** `c1d46c0c1592faa0860f704008b2b2381bc3840f` (2022-06-03), GPLv2-only.
- **GLEEC divergence point:** `d36369980a6c08f8689b64df56fbccf0097a0a6f` (GLEEC-side first commit under modified terms).
- **KDF Reloaded baseline:** `c1d46c0c` verbatim, then post-anchor development in this repository.

### Build and toolchain

- Stable Rust per `rust-toolchain.toml`. (Upstream historical README pinned `nightly-2022-02-01`; this is no longer required.)
- CMake ≥ 3.12, system C/C++ toolchain.

### Networks

- Production netids supported: **8762** (AtomicDEX), **6133** (GLEEC).
- Test netids (8100/8999/9000/9998): only available with `--features regtest-netid` and never compiled into production `kdf` binaries.

### Shielded database isolation

Reloaded and GLEEC KDF deliberately use different native SQLite files for the
modern shielded wallet and compact-block stores. The older native Sapling-state
cache keeps its existing compatible name and format:

| Purpose | KDF Reloaded | GLEEC KDF / pre-upgrade name |
| --- | --- | --- |
| Shielded wallet | `<TICKER>_RELOADED_WALLET.db` | `<TICKER>_WALLET.db` |
| Compact-block cache | `<TICKER>_RELOADED_COMPACT_BLOCKS.db` | `<TICKER>_COMPACT_BLOCKS.db` |
| Legacy native Sapling-state cache | `<TICKER>_CACHE.db` (shared) | `<TICKER>_CACHE.db` (shared) |

On the first supported Light-mode activation after upgrading, Reloaded creates
its own modern wallet database and performs a full shielded rescan from the
resolved sync start. It does not open, migrate, rename, truncate, or delete the
GLEEC/pre-upgrade files. The compact cache is likewise isolated, so cached
blocks are validated and stored independently. Native full-node conversion into
the modern compact cache remains an explicit open item in CRD chapter 39.

`<TICKER>_CACHE.db` is not the upgraded wallet database or the modern
compact-block cache. It is the pre-existing native-mode commitment-tree cache;
Reloaded preserves its compatible table and serialization contract. A
Light-mode activation may create its empty schema when the file is absent, but
Light-mode balance, history, and scan state come from the two Reloaded-specific
files above. Existing compatible cache contents are not renamed merely because
the modern wallet schema changed.

Consequently, switching from Reloaded back to GLEEC KDF does **not** require
manual cache deletion. GLEEC reopens its original files, which Reloaded left
untouched, and resumes or refreshes them according to GLEEC's own sync logic.
The GLEEC database may naturally be behind the chain tip after time spent using
Reloaded, but it has not been converted to the incompatible modern schema.

Reloaded also protects its own filenames conservatively. If a
`<TICKER>_RELOADED_WALLET.db` contains the exact recognized legacy schema, it is
moved to a timestamped `.legacy-v2.*.bak` backup before a fresh modern wallet is
built and rescanned. An unknown or corrupt Reloaded wallet/cache file is left
unchanged and activation returns a typed schema error; it is never silently
deleted or treated as an empty wallet. Manual intervention is required only for
that explicit unknown/corrupt-file error, not for ordinary switching between
Reloaded and GLEEC KDF.

## Forward-compatibility commitment

KDF Reloaded commits to following GLEEC KDF's evolution:

- We monitor GLEEC KDF releases and incorporate compatible changes when they are legally and technically feasible.
- For changes we *cannot* incorporate verbatim (licensing, project-direction, or quality reasons), we still expose a per-setting opt-in so the GLEEC-equivalent behaviour remains reachable, and we list that setting in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
- The only exception is a behaviour that would either violate GPLv2 or directly contradict the project's principle of free, open trading at reasonable cost. In that case the divergent setting defaults to the KDF Reloaded behaviour and the original-compatible value is gated behind an explicit acknowledgement with a runtime warning. See the developer rule in [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md).
