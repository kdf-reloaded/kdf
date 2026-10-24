# Git flow and branch strategy

KDF-Reloaded uses a three-tier permanent branch model plus short-lived
feature branches.

## Permanent branches

| Branch | Purpose | Stability |
|---|---|---|
| `main` | Tagged releases only | Always shippable |
| `staging` | Pre-release integration | Green CI, QA-tested |
| `dev` | Active development | Green CI, may have unreleased features |

Hierarchy: `main` ← `staging` ← `dev` ← feature branches.

`reloaded-gplv2-base` is a historical branch. It stopped receiving work in
June 2026 and is far behind `dev`, so do not branch from it or compare
against it. It is kept deliberately rather than pruned: it records the
GPL-2.0-only base that the project's distribution rights rest on (see
[`LEGAL/LICENSING-POLICY.md`](../LEGAL/LICENSING-POLICY.md)), so it remains
available for provenance and review even though no further work will land on
it.

Promotion is one-way and explicit:
1. Feature branch → `dev` after PR review + green CI.
2. `dev` → `staging` when a release candidate is ready.
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
git merge-base HEAD origin/dev
```

If `origin/staging` and `origin/main` already exist in your clone, you may
include them as additional candidates.

The deprecated upstream `mm2.1` branch is not used in Reloaded.

## Feature branches

- Lifetime ≤ 1–2 weeks. Decompose larger work into multiple feature branches.
- Branch from `origin/dev`. Never branch from `main`, `staging`, or
  `reloaded-gplv2-base`.
- Hotfixes for `main` are exceptional and must be back-merged into
  `staging` and `dev` immediately after.

## Commits

- Small, self-contained commits. Each commit must leave the tree compiling
  and tests passing.
- Format touched packages with the pinned toolchain
  (`cargo +nightly-2026-05-08 fmt -p <package>`) before committing. CI fails
  on unformatted code. Never run a bare workspace-wide `cargo fmt`.
- Run `cargo clippy -p <package> --all-targets --no-deps -- -D warnings` on
  touched packages. For WASM-only code add `--target wasm32-unknown-unknown`.
  `AGENTS.md` §6 has the full verification list.
- For larger work, write or follow a plan under [`docs/plans/`](./plans/)
  and record progress there as items ship. The rule, the threshold and the
  plan header are in [Plans and the changelog](#plans-and-the-changelog) below.

## PRs

- PR title prefix indicates state: `[wip]`, `[r2r]` (ready to review).
- Reference the relevant phase / plan item in the PR description.
- A plan covering the work must be current at the branch tip before review, and
  must match what landed when the branch merges (rule **P1** below).
- See [PR_REVIEW_CHECKLIST.md](./PR_REVIEW_CHECKLIST.md) for review criteria.

## Plans and the changelog

Adopted 2026-10-18 by the maintainer. **P0** is the rule; **P1**–**P7** under it
are what make it hold. Cite them by id.

**P0 — the rule.** Everything we are working on or intend to work on has a plan:
one plan per work package, each in one place, carrying its state — which parts
are already in the tree, which still need analysis, what was decided. Each plan
is forward-looking, and [`docs/plans/`](./plans/) is where most of them live
(**P5**). Small, simple fixes of the kind done in a single commit need no plan,
and neither does work that is already finished (**P3**). Everything important is
in [`CHANGELOG.md`](../CHANGELOG.md), kept brief, derived from commit history
(**P4**), and every release names the plans it touched (**P7**).

**P1 — currency, per commit, judged at the branch tip.** Every commit that
changes a plan's implementation, or anything the plan states, updates the plan
too — not just the PR that collects those commits. A rule with no trigger
decays, and a stale plan is worse than no plan.

One practical bound, so the rule does not create its own problem: within a
single branch what is required is that the plan is **current at the branch
tip**, not that every commit touches it. Several sessions edit `docs/plans/` in
parallel, and a thirteen-commit branch that rewrites its plan thirteen times is
thirteen conflict opportunities for no added accuracy. So fold the plan update
into the commit that makes the change where that is natural, and otherwise bring
the plan current before the branch goes up for review. A branch must never merge
with a plan that does not match what landed. A reviewer who sees implementation
move while its plan stands still says so, and the plan's `Last reviewed` date
(**P2**) moves with the update.

**P2 — every plan carries a status and a last-reviewed date.** The first line
after every plan's title is exactly:

```
**Status:** <status> — **Last reviewed:** YYYY-MM-DD
```

so that a finished plan is distinguishable from an abandoned one and from a
current one. A short free-text qualifier may follow the status word in
parentheses — `parked (T1–T10 shipped; T11–T13 queued)`. The status word is one
of six:

| Status | Meaning |
|---|---|
| `planning` | The approach is not settled yet. The document is input to a design pass, not something to execute. |
| `in progress` | Being implemented now. |
| `blocked` | Ready to execute, but waiting on something outside this repository (an upstream release, a third party, a decision someone else owes us). Name the blocker. |
| `parked` | Approach settled, nothing in flight, not scheduled. Priority is the only thing in the way. |
| `done` | Complete in **code, documentation and tests** — all three. A **transient** state: see the lifecycle below. |
| `superseded` | The approach was abandoned or replaced. **Must** point at what replaced it. Transient on the same clock as `done`. |

`Last reviewed` is the date someone last checked the plan against the tree — not
the date it was written. Where the real status genuinely cannot be determined,
write `unknown` with a one-line reason rather than guessing; that is a defect to
clear, not a seventh status.

### Lifecycle of a finished plan

`done` means the work package is complete in **all three** of code,
documentation and tests. Any residual item — an unported path, a missing test,
an undocumented switch, a decision still open — means the plan is **not** `done`:
use `in progress` or `parked` and name what is left in the qualifier. A
qualifier that subtracts from the status ("done, except …") is a contradiction;
rewrite it as the lesser status.

A `done` (or `superseded`) plan is kept **until the nearest release** and may
then be deleted. "Nearest release" here is the next `v*` tag published from
`staging` or `main` after the plan reached that state — **pre-release tags
count**, so for work finishing in the current `0.3.0-alpha.x` line that is the
upcoming beta. The register stays forward-looking: it holds live and intended
work, not an archive of shipped work.

**Precondition for deletion — not advice.** Before a `done` or `superseded` plan
is deleted, confirm its decisions are recorded outside it:

1. **binding behaviour** — in the governing CRD chapter under
   [`docs/reloaded-rewrite/`](./reloaded-rewrite/) (and, where it is a
   divergence or a switch, in [`GLEEC_COMPATIBILITY.md`](GLEEC_COMPATIBILITY.md)
   / [`COMPAT_SWITCHES.md`](COMPAT_SWITCHES.md)); and
2. **the reasoning** — rejected alternatives, accepted costs, measurements — in
   the pull request(s) that shipped it.

If neither holds for some decision the plan carries, the plan is **not
deletable**: record that decision in the CRD first, or keep the plan one more
release. Deleting on a schedule without this check destroys institutional
knowledge on a schedule. Deletion is a release-time action, and like every other
transition it is the maintainer's to make (**P6**), never a side effect of
another change.

Checking by inspection, for the plans under `docs/plans/`:

```bash
# every plan has a well-formed header
grep -L '^\*\*Status:\*\* \(planning\|in progress\|blocked\|parked\|done\|superseded\|unknown\)' docs/plans/*.md
# current statuses and review dates
grep -H '^\*\*Status:\*\*' docs/plans/*.md
```

**P3 — a plan is for live or intended work, and the threshold is a decision the
record does not already hold.** "One commit" is the rough size cut; the sharp
test is whether the work carries a decision the record cannot show — a rejected
alternative, a why-not, an accepted cost, a measurement a later reader would
otherwise have to repeat. The record is the code **and** the governing CRD
chapter together: where that chapter already carries the decisions and the
deferred items, the work package needs no plan of its own. That is what a plan
uniquely holds, and such work needs one whatever the diff size. A mechanical
fix whose reasoning is fully visible in the diff needs no plan, however large.
A two-line change that encodes a choice between two defensible designs does.

**Work that has already shipped does not get a plan written for it after the
fact.** Its reasoning belongs to the pull request(s) that shipped it, and any
behaviour it binds belongs to its CRD chapter — which is also what makes a
finished plan deletable (the lifecycle above). The register is forward-looking;
backfilling it with archives of completed work makes it harder to see what is
actually live.

**P4 — the changelog is user- and integrator-facing.** `CHANGELOG.md` is filtered
to what someone running or integrating against KDF Reloaded would notice: RPC
and wire behaviour, configuration keys, defaults, compatibility switches,
platform support, security-relevant behaviour. Internal hygiene — refactors,
test scaffolding, CI, dependency moves with no observable effect — belongs in
the PR description, and in a plan only while that work is live (**P3**). Entries
stay brief and derive from commit history.

**The pull request is the vessel for the detail** — explicitly agreed with the
maintainer. An entry says what a user or integrator would notice; the
measurements, the alternatives weighed and the costs accepted stay in the PR
that shipped the change, which is also where they remain readable long after the
plan is deleted.

**P5 — a plan's location follows its work package.** Each work package keeps its
plan in the place that suits it. For most, that place is
[`docs/plans/`](./plans/), which is where to look first. Some belong elsewhere —
because the material is sensitive, because its distribution is restricted,
because it sits better next to the artifacts it is about, or because a third
party's material is involved. Choosing the home is an ordinary part of setting a
work package up, and it is the maintainer's choice, like the branch name or the
release line.

Location changes nothing else. **A plan is a plan under this rule wherever it
lives**, and carries the same obligations: one per work package, kept current
(**P1**), carrying a status and a last-reviewed date (**P2**), moving through
the same lifecycle under the same hand (**P6**), and eligible to be named in a
release's plan list (**P7**). The `grep` checks above read `docs/plans/`, so they
speak for the plans kept there and for no others; a plan with another home is
reviewed where it lives.

**P6 — status transitions and deletion are the maintainer's decision.** Starting
implementation, deferring, pausing, resuming, marking `done`, cancelling,
superseding and deleting are all the operator's calls. An agent or a contributor
**proposes** a transition and brings the evidence for it — what landed, what is
missing, what the tree says — and the maintainer decides. Nothing in **P2**'s
vocabulary authorises a transition by itself: a commit that moves a status needs
the operator behind it, and so does one that removes a plan.

> **One standing exception, and this branch is an instance of it rather than a
> precedent against the rule.** A **bulk reconciliation** — checking every plan's
> status against the tree and correcting the ones that disagree — is itself an
> operator-approved action, approved as a batch before it starts. The statuses in
> `docs/plans/` were assigned that way on 2026-10-18, four of them correcting a
> plan whose own header contradicted the tree. Read that commit as the batch it
> was: it is not licence for an agent or a contributor to reassign a status on its
> own judgement.

**P7 — every release lists the plans it touched.** A release's `CHANGELOG.md`
entry and its release notes both carry a list of the plans the released version
touched **in any sense** — advanced, completed, deferred, superseded, or only
corrected — each with a very brief line saying what was done. It is the quickest
honest answer to "what moved in this release, and where is the reasoning?", and
it is the one place a reader learns that a plan changed state at all.

Shape — one line per plan, name then what happened:

```markdown
### Plans touched

- `arrr-ironwood-compatibility` — field testing green-lit; reorg recovery merged.
- `walletconnect-fork-upgrade` — superseded; its advisories cleared another way.
- `clippy-ci-gate` — gate shipped; the vendor-filter follow-up stays open.
```

The list may include plans whose home is elsewhere (**P5**), at the maintainer's
sole discretion — nothing requires their inclusion and nothing precludes it.

## CI

- `test.yml` runs on GitHub-hosted runners for every pull request into, and
  every push to, `dev`, `staging` and `main` (and `reloaded-gplv2-base`).
  Branch protection requires its `tests-passed` job, which succeeds only when
  `fmt`, `clippy`, every `unit-tests` leg, `wasm32-check` and `evm-geth-tests`
  (a pinned `geth --dev` node; it fails rather than skips when geth is missing) all succeeded. To
  make a new test job required, add it to `tests-passed`'s `needs:` list in
  `test.yml`; repository settings do not change. `docker-tests`,
  `trezor-emulator-tests` and `external-network-tests` run but are not
  required, because they depend on third-party services or live networks.
- Platform builds (Windows, macOS, iOS, Android, WASM) live under
  `.github/workflows/build-*.yml` and are dispatched manually until each
  runner is verified — see [CI_RUNNERS.md](./CI_RUNNERS.md).
- The umbrella `dev-build.yml` (manual) fans out to every platform-build child
  via `workflow_call` for on-demand `dev` snapshots.
- `staging-build.yml` runs the same fan-out **automatically on every push to
  `staging`**, giving testers fresh unsigned beta/rc binaries.
- `release.yml` runs on `main` `v*` tags and produces the signed, published
  release (see [DEV_BUILD.md](./DEV_BUILD.md) §7 and [RELEASE.md](./RELEASE.md)).
