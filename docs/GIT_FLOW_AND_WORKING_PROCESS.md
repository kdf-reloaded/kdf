# Git flow and branch strategy

KDF-Reloaded uses a three-tier permanent branch model plus short-lived
feature branches.

## Permanent branches

| Branch | Purpose | Stability |
|---|---|---|
| `main` | Tagged releases only | Always shippable |
| `staging` | Pre-release integration | Green CI, QA-tested |
| `reloaded-gplv2-base` | Active development (dev) | Green CI, may have unreleased features |

Hierarchy: `main` ← `staging` ← `reloaded-gplv2-base` ← feature branches.

Promotion is one-way and explicit:
1. Feature branch → `reloaded-gplv2-base` after PR review + green CI.
2. `reloaded-gplv2-base` → `staging` when a release candidate is ready.
3. `staging` → `main` after QA sign-off, then tag `vX.Y.Z` (annotated, GPG-signed).

Release tags use the `v*` convention and drive the signed release pipeline
([`.github/workflows/release.yml`](../.github/workflows/release.yml)):

- **Pre-release tags** (`vX.Y.Z-alpha.N` / `-beta.N` / `-rc.N`) are cut on
  `staging`; they publish a signed GitHub **pre-release** (not marked "Latest").
- **Final tags** (`vX.Y.Z`) are cut on `main`; they publish the signed, latest
  GitHub Release.

Only `release.yml` signs and publishes. Unsigned, untagged branch snapshots come
from [`dev-build.yml`](../.github/workflows/dev-build.yml) (manual) and
[`staging-build.yml`](../.github/workflows/staging-build.yml) (automatic on push
to `staging`).

When comparing against an ancestor, use:

```bash
git merge-base HEAD origin/reloaded-gplv2-base
```

If `origin/staging` and `origin/main` already exist in your clone, you may
include them as additional candidates.

The deprecated upstream `mm2.1` branch is not used in Reloaded.

## Feature branches

- Lifetime ≤ 1–2 weeks. Decompose larger work into multiple feature branches.
- Branch from `reloaded-gplv2-base`. Never branch from `main` or `staging`.
- Hotfixes for `main` are exceptional and must be back-merged into
  `staging` and `reloaded-gplv2-base` immediately after.

## Commits

- Small, self-contained commits. Each commit must leave the tree compiling
  and tests passing.
- Run `cargo fmt` before committing. CI fails on unformatted code.
- Run `cargo clippy -p <crate> --all-targets -- -D warnings` on touched
  crates. For WASM-only code add `--target wasm32-unknown-unknown`.
- For larger refactors, follow the phased plan files at the repo root
  (`RELOADED-PLAN.md`, `RELOADED-REFACTOR.md`, `RELOADED-UNIT-TESTS.md`).
  Mark items `[x]` with the commit SHA as you ship them.

## PRs

- PR title prefix indicates state: `[wip]`, `[r2r]` (ready to review).
- Reference the relevant phase / plan item in the PR description.
- See [PR_REVIEW_CHECKLIST.md](./PR_REVIEW_CHECKLIST.md) for review criteria.

## CI

- Self-hosted Linux runner runs `test.yml` on push.
- Platform builds (Windows, macOS, iOS, Android, WASM) live under
  `.github/workflows/build-*.yml` and are dispatched manually until each
  runner is verified — see [CI_RUNNERS.md](./CI_RUNNERS.md).
- The umbrella `dev-build.yml` (manual) fans out to every platform-build child
  via `workflow_call` for on-demand `dev` snapshots.
- `staging-build.yml` runs the same fan-out **automatically on every push to
  `staging`**, giving testers fresh unsigned beta/rc binaries.
- `release.yml` runs on `main` `v*` tags and produces the signed, published
  release (see [DEV_BUILD.md](./DEV_BUILD.md) §7 and [RELEASE.md](./RELEASE.md)).
