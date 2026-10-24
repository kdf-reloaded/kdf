# Plan: Clippy CI gate and vendored-tree follow-up

**Status:** parked (the gate shipped 2026-10-14; the vendor-filter follow-up in §3 is open) — **Last reviewed:** 2026-10-18

> **Context:** shipped 2026-10-14 (PR #27). The `clippy` job in `.github/workflows/test.yml`
> enforces clippy on all first-party crates, and `docs/CODING_STANDARDS.md`'s
> hard rule was reconciled to the invocation CI actually runs. One structural
> follow-up remains open (§3); it is intentionally out of scope for the gate's
> introduction.

## 1. What shipped

`docs/CODING_STANDARDS.md` has always required
`cargo clippy ... -- -D warnings` to pass, but there was no clippy job in CI, so
the rule drifted from reality. This change adds the gate and makes the doc and CI
agree.

The enforced invocation, per first-party package, with default features:

```sh
cargo clippy -p <package> --all-targets --no-deps -- -D warnings
```

run across all first-party crates in one invocation (package list via
`cargo metadata --no-deps` + `jq`, mirroring the `fmt` job). This is the form
`AGENTS.md` §6 mandates, **not** the bare `--workspace --all-features` sweep the
hard rule literally used to spell out:

- `--workspace` fails outright — the vendored, patched
  `rust-lightning-patched/lightning` crate trips 13 deny-level clippy lints
  against its own source, so a workspace sweep never reaches first-party code.
- `--all-features` would enable features that never ship in the `kdf` binary
  (`mm2_main`'s `regtest-netid`, `unsafe-rpc-wire-dump-release-override`,
  `custom-swap-locktime`) plus test-only Trezor-emulator / Foundry scaffolding,
  i.e. it would gate CI on code that never ships. Default features are what the
  `kdf` binary builds, so that is what CI lints.

The clippy job is a separate top-level job (not a prerequisite of `unit-tests`),
so a clippy-only failure does not force a full unit-test rebuild on
"Re-run failed jobs" — the same granularity rationale as the `fmt` job.

## 2. Coverage

The gate covers every first-party crate **except** two, which are excluded by
name in addition to the `*-patched/` filter:

- `lightning_persister`
- `lightning_background_processor`

These are vendored/adapted LDK (rust-lightning) companion crates. `AGENTS.md` §6
says not to turn clippy warnings in vendored trees into opportunistic edits, so
their findings are left unenforced rather than rewritten. For the record, the
findings as of 2026-10-14 are:

| Crate | Findings |
| --- | --- |
| `lightning_persister` | `iter_overeager_cloned` ×6, `unnecessary_sort_by` ×3, `permissions_set_readonly_false` ×1 (all in test code) |
| `lightning_background_processor` | `type_complexity` ×5 |

The `permissions_set_readonly_false` finding is in a `#[cfg(test)]` helper that
restores permissions on a temp directory it created itself so it can delete it;
it is not a shipped path and has no security relevance.

## 3. Follow-up: vendor filters key on location, not provenance

`lightning_persister` and `lightning_background_processor` are LDK 0.0.106
companion crates (MIT OR Apache-2.0) that live under `mm2src/coins/`, while their
three siblings — `lightning`, `lightning-invoice`, `lightning-net-tokio` — live
under `rust-lightning-patched/`. Same provenance, different location.

Every vendor filter in CI (the `fmt` job and now the `clippy` job) excludes
vendored trees by **path** (`test("-patched/")`), so these two crates slip
through the location-based filter and have to be excluded by name instead. That
name-based carve-out is the actual latent inconsistency this work surfaced.

Options, none taken here (path churn / unrelated risk):

1. Relocate the two crates under `rust-lightning-patched/` so the existing
   location filter catches them. Rejected for this batch: moving crate
   directories churns every reference across the tree for no functional gain.
2. Make the vendor filter provenance-based rather than location-based — e.g. an
   explicit vendored-crate exclude list sourced from
   `docs/reloaded-rewrite/34-provenance-ledger.md`, or a workspace-metadata
   marker — so both `fmt` and `clippy` select the same set without hard-coding
   crate names in two workflows.

Until then, the name-based exclusion lives in the `clippy` job comment and here.

## 4. Minor observation (not addressed)

`coins_activation` emits unused-import warnings on `wasm32-unknown-unknown` only
(`l2/mod.rs` re-exports that are unused under the wasm cfg). These predate this
work and are not flagged by the clippy gate, which runs on the native target.
Noted for whoever next touches `coins_activation`'s module layout.
