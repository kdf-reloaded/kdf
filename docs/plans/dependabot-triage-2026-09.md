# Dependabot triage — 2026-09-23

> **Status:** done. Snapshot triage of all 76 open Dependabot alerts on `dev`,
> the fixes that came out of it, and the accepted-risk record for what stays.
> Ongoing dependency work continues to live in
> [`v0.2.0-dependency-hygiene.md`](v0.2.0-dependency-hygiene.md); this file is the
> audit trail for the one-off pass.

## Why the count was 76 while CI was green

`cargo deny check advisories` reported `advisories ok` on every run while the
Security tab showed 76 open alerts (4 critical, 16 high, 36 medium, 20 low). Both
were accurate. Sorting the alerts by manifest explains the gap:

| Manifest | Alerts | What it actually is |
|---|---:|---|
| `mm2src/common/Cargo.lock` | 22 (all 4 criticals) | Orphan lockfile last written 2021-03-11. `common` is a path dep inside the workspace, so Cargo reads only the root lock. Dead file, referenced by no workflow, script or doc. |
| `vendor-patches/*/Cargo.lock` | 17 | Standalone lockfiles for the three `exclude`d zcash crates, used only by their focused tests. The shipped build resolves them through the root lock. |
| `rust-lightning-patched/*`, `lightning_persister` | 6 | Manifest-*range* alerts: vendored rust-lightning 0.0.106 declares `secp256k1 0.20.2` / `tokio 1.0`. |
| `mm2src/*/Cargo.toml` (rmp-serde) | 3 | Already documented as wire-compat-blocked. |
| root `Cargo.lock` | 28 | The only alerts describing shipped code. |

cargo-deny reads the root lock only, so 48 of the 76 were invisible to it by
construction. Of the 28 that were visible, 14 were already accepted in
`deny.toml`.

Two structural findings came out of the pass:

1. **cargo-deny cannot see GHSA-only advisories.** It reads RustSec; the GitHub
   Advisory Database is a superset. Five root-lock alerts — both gossipsub ones,
   `yamux`, `jsonwebtoken`, `serde_with` — have no RustSec ID at all. One of them
   was a live remote crash (below) and the gate was green throughout.
2. **cargo-deny does not fail on `unsound` advisories**, contrary to what
   `deny.toml` asserted. Measured with the CI-pinned 0.19.9: `lock_api 0.3.4`,
   `lru 0.7.5` and `rand 0.7.3` are all in the graph below their patched
   versions, and the check still passes. Adding their IDs to `ignore` yields
   `advisory-not-detected`.

Both are now addressed — see *CI gate* below.

## The one exploitable finding

`libp2p-gossipsub` 0.45.0, as shipped in the pinned `KomodoPlatform/rust-libp2p`
fork tag `k-0.52.12`, has a **remotely reachable, unauthenticated panic**. A
peer's PRUNE control message carries a `backoff: u64` that reaches `Instant`
arithmetic unchecked, so one crafted PRUNE crashes the swarm state machine of any
node that peers with the sender — including seed nodes.

- GHSA-gc42-3jg7-rxr2 — `Instant::now() + time`, `src/backoff.rs:75`.
- GHSA-xqmp-fxgv-xvq5 — `backoff_time + slack` on a later heartbeat, `src/backoff.rs:161`.
- Plus a third overflow **not** in either advisory, found by the regression test
  written for them: the `heartbeat_index + heartbeats(time) + backoff_slack`
  `usize` sum, which panics in debug builds and on 32-bit targets (armv7, wasm32
  are both shipped).

Upstream fixed the first two in gossipsub 0.49.3/0.49.4, which belong to libp2p
0.56. `k-0.52.12` is the newest tag the fork has, so there was nothing to bump to.
Fixed by vendoring the single crate and patching it:
`vendor-patches/libp2p-gossipsub-0.45.0/` plus a
`[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]` stanza in the root
manifest, mirroring how the three zcash crates are already handled. Details,
including the verification that the vendored copy's 6 failing behaviour tests fail
identically on pristine fork source, are in that directory's `PATCH-NOTES.md`.

## Fixed (43 alerts)

| # | Change | Alerts |
|---|---|---:|
| 1 | Deleted `mm2src/common/Cargo.lock`. `cargo metadata --locked` byte-identical before and after; root lock untouched. | 22 |
| 2 | `cargo update` in the three `vendor-patches/*` lockfiles. All semver-compatible. | 15 |
| 3 | Vendored + patched `libp2p-gossipsub` 0.45.0 (above). | 2 |
| 4 | `serde_with` 3.12.0 → 3.22.0. Needed `ref-cast` 1.0.3 → 1.0.27 first: `sp-storage 6.0.0` had it pinned and blocked the resolve. | 1 |
| 5 | `env_logger` 0.7 → 0.11 in `mm2_p2p`, moved to `[dev-dependencies]` (only used from `tests.rs`). `atty` left the graph. | 1 |
| 6 | `dirs` 1 → 6 in `coins` and `mm2_main`, `hdrhistogram` 7.1 → 7.6 in `mm2_metrics`. Dropped `redox_users 0.3` → `rust-argon2 0.7` and `crossbeam-channel 0.4`. | 0 — see correction below |

Only `dirs::home_dir()` is used, and it is unchanged across those majors — note
that `dirs >= 4` resolves the Windows home via `SHGetKnownFolderPath` rather than
`%HOME%`/`%USERPROFILE%`.

Incidental fix: `vendor-patches/zcash_primitives-0.28.0` gained a
`[patch.crates-io]` for its sibling `zcash_transparent`. Without it the standalone
test build — the stated reason those crates sit in `exclude` — resolved the
unpatched crates.io copy and did not compile. All three suites now pass
(`--all-features` is required for `zcash_transparent`).

### Not taken

`async-std` 1.6.2 → 1.13.2 was tried and reverted: 1.9 removed
`async_std::sync::channel`, which `mm2_main/src/lp_swap.rs:360` still uses, and
the bump cleared no alert on its own.


### Correction (2026-09-23, after merge)

That last row originally claimed **1** alert — `crossbeam-utils 0.7.2`,
RUSTSEC-2022-0041 / GHSA-qc84-gqf4-9926, alert #7 — and it did not clear it.

`crossbeam-utils 0.7.2` had **three** pullers, not two. The `dirs` and
`hdrhistogram` bumps removed `rust-argon2 0.7` (via `redox_users 0.3`) and
`crossbeam-channel 0.4`, but **`async-std 1.6.2`** is a third and is sufficient
on its own. The clearance was checked while `async-std` was briefly at 1.13.2
during the residual-bump attempts, and not re-checked after that bump was
reverted for breaking the build — `async_std::sync::channel`, removed in
async-std 1.9, is still used at `mm2_main/src/lp_swap.rs:360`. The
`dirs`/`hdrhistogram` bumps were still worth keeping: they are what removed
`atty` and two of the three pullers. They just did not close #7.

#7 is therefore **dismissed rather than fixed** — `tolerable_risk`, on the
grounds that the advisory is `informational = unsound` and affects only
`AtomicCell<{i,u}64>` `fetch_*` on 32-bit targets that have `Atomic{I,U}64`
(64-bit targets are unaffected outright). The real fix is the `async-std` bump,
which is a swap-shutdown code change — `Receiver::recv` returns `Option` today
and `Result` on the `async-channel` replacement, in the `select!` arms of both
`maker_swap.rs` and `taker_swap.rs` — and is tracked as its own item in
[`v0.2.0-dependency-hygiene.md`](v0.2.0-dependency-hygiene.md).

The final split, confirmed against the Security tab after the merge rescan, is
**43 fixed, 33 dismissed, 0 open** — not the 42/34 predicted before merge. Two
movements account for the difference: #7 went from "fixed" to "dismissed" (this
correction), and the two `rsa` Marvin alerts (#112, #113) went the other way,
from "dismissed" to **fixed**. They had been dismissed `no_bandwidth` on the
grounds that no fixed release existed; the `cargo update` in
`vendor-patches/zcash_client_backend-0.23.0` moved `rsa` to 0.9.10, which
GitHub now scores as outside the affected range. A better outcome than the
dismissal reason claimed, so it stands uncorrected in the alert itself.

Worth noting what caught this: the `dependabot-alerts` gate, on its first armed
run. It is the only check that would have — `cargo deny check advisories` is
green on this exact tree, because RUSTSEC-2022-0041 is `unsound` and cargo-deny
does not match those at all.

## Accepted (33 alerts, dismissed on the Security tab)

Dismissal reason and a short justification are attached to each alert; the full
rationale lives in `deny.toml`. Summary:

| Group | Alerts | Reason |
|---|---:|---|
| Already in `deny.toml` `ignore` — libp2p fork TLS/net stack, Solana SDK, WalletConnect, rmp-serde | 14 | `tolerable_risk` |
| Vendored rust-lightning 0.0.106 manifest ranges (`secp256k1 0.20.2`, `tokio 1.0`) | 6 | `tolerable_risk` |
| `rmp-serde` manifest alerts — P2P wire compat with GLEEC | 3 | `tolerable_risk` |
| `lock_api` ×5, `lru`, `rand` — RustSec `unsound`, transitively pinned | 7 | `tolerable_risk` |
| `yamux` — vulnerable 0.12.1 compiled in but never constructed | 1 | `not_used` |
| `jsonwebtoken` — verify path is dead code | 1 | `not_used` |
| `crossbeam-utils` 0.7.2 — unsound, 32-bit only, pinned by `async-std 1.6.2` | 1 | `tolerable_risk` |

The two `not_used` ones are worth spelling out, because both look alarming on the
dashboard and neither is reachable:

- **`yamux` (GHSA-vxx9-2994-q338, high).**
  `mm2src/mm2_p2p/src/atomicdex_behaviour.rs:1213` uses
  `libp2p::yamux::Config::default()`, which in libp2p-yamux 0.44 is
  `Either::Right(Config013)` — the runtime muxer is `yamux 0.13.10`, the patched
  version. `yamux 0.12.1` is in the lock only because libp2p-yamux depends on both
  lines unconditionally; nothing in the tree constructs `Config::client()` or
  `Config::server()`.
- **`jsonwebtoken` (GHSA-h395-gr6q-cpjc, medium).**
  Arrives only through `relay_rpc` (WalletConnect fork `k-0.1.3`). KDF mints and
  sends tokens (`mm2src/kdf_walletconnect/src/lib.rs:918-926`, `:253`);
  `relay_rpc` signs them by hand with `ed25519_dalek`, and its sole
  `jsonwebtoken::crypto::verify` call site (`relay_rpc/src/jwt.rs:190-195`) is
  reached only from upstream's own unit tests. The framework never verifies a JWT
  from an untrusted source, which is the whole premise of the advisory.

## Corrections to existing documentation

`deny.toml` and `docs/reloaded-rewrite/28-libp2p-modernization.md` (OQ2) attribute
`mio` and `remove_dir_all` to the libp2p fork's transitive stack. `cargo tree -i`
shows otherwise: `mio 0.7.13` comes from `crossterm` and `signal-hook-mio` (the
TUI path in `common`/`gstuff`/`mm2_test_helpers`), and `remove_dir_all 0.5.3` from
`tempfile 3.3.0` (`prost-build`, `rusty-fork`). Both are clearable independently of
the fork, so OQ2's residue list is two entries shorter than it claims. `deny.toml`
now says so; ch28 has not been edited.

Two things ch28 should pick up when it is next revised, both recorded only in the
plans for now: that correction, and the fact that the fork's `libp2p-yamux` 0.44
is what keeps the vulnerable `yamux 0.12.1` in the tree (section F1 of the
hygiene plan) — dropping the `yamux012` dependency is part of what modernizing
the fork buys, and OQ2 does not currently mention it.

## CI gate

`.github/workflows/audit.yml` gains a `dependabot-alerts` job that fails on any
open, non-dismissed Dependabot alert against the **root** `Cargo.lock`. That is
the lockfile the shipped binaries resolve from; vendor-patch and orphan lockfiles
are noise, not build inputs.

> **One-time setup required — the job is inert until it is done.** The default
> `GITHUB_TOKEN` **cannot** read the Dependabot alerts API: it returns
> `403 Resource not accessible by integration`, and there is no `permissions:`
> key that grants it (`security-events: read` is not the relevant scope).
>
> **Preferred: a GitHub App.** Owned by `kdf-reloaded`, installed on this repo,
> with the single repository permission `Dependabot alerts: Read-only` and no
> webhook. Put its App ID and private key in the repo secrets `AUDIT_APP_ID` and
> `AUDIT_APP_PRIVATE_KEY`; the job mints a short-lived token per run via
> `actions/create-github-app-token`. Chosen over a PAT because it does not
> expire, has nothing to rotate, is not tied to a person's account — notably not
> to the release-signing identity — and makes the audit log attribute
> security-alert reads to the App rather than to a human.
>
> **Fallback: a PAT** in `DEPENDABOT_ALERTS_TOKEN`. Fine-grained, scoped to this
> repo with `Dependabot alerts: Read-only`, or classic with `public_repo` (this
> repo is public) or `security_events`. Note that fine-grained PATs against an
> org-owned repo require the org to have opted into them
> (Org Settings → Personal access tokens), and that they expire.
>
> Until one of those exists the job prints a warning naming this setup step and
> **passes**, deliberately: a gate that is permanently red because it is
> misconfigured teaches everyone to ignore it, which is worse than not having it.
> So a green `dependabot-alerts` means either "no open root-lock alerts" or "not
> armed yet" — check the job log for the warning to tell them apart. Any other
> API failure (bad credentials, outage) still fails the job.
>
> One asymmetry to be aware of if you use a PAT: an **expired** token returns 401
> and fails the job loudly, but a token that has **lost the permission** returns
> 403 and is indistinguishable from "never configured", so it goes quietly inert.
> The App has no expiry, which is the main reason it is preferred.

The accept mechanism for that job is *dismissing* the alert with a reason and a
justification, exactly as `deny.toml`'s `ignore` list is the accept mechanism for
the cargo-deny job. Keep the two in sync: when you dismiss an alert, record why in
`deny.toml`.

One wrinkle worth knowing about: Dependabot alert state is a property of the
repository, derived from the **default branch** — there is no per-ref view. A PR
that fixes an alert cannot close it until it merges, so the job reports on
`pull_request` and only fails on pushes to the default branch and on the weekly
schedule. Expect the PR carrying this change to warn about the five root-lock
alerts it fixes, and to go quiet once merged.

## Still open, deliberately

- **rust-lightning 0.0.106 → 0.0.12x**, and with it `bitcoin 0.27`,
  `secp256k1 0.20` and the `core2-shim` yank workaround. See
  [`lightning-ldk-upgrade.md`](lightning-ldk-upgrade.md).
- **`hyper 0.14` / `h2 0.3` → 1.x** — already scheduled in `deny.toml`.
- **libp2p fork modernization** — blocked on a `KomodoPlatform/rust-libp2p` tag
  newer than `k-0.52.12`. When one lands carrying gossipsub ≥ 0.49.4, drop the
  vendor patch.
- **`wasm-timer` 0.2.4** — unmaintained since 2020, sole reason `parking_lot 0.9`
  and `lock_api 0.3.4` are in the tree. Replacing it would clear 5 alerts.
- **The two `not_used` dismissals — `yamux` and `jsonwebtoken`.** Both are
  judgements about our call sites, not about the advisories: the vulnerable code
  is compiled in and simply never reached, so each dismissal is only as true as
  one specific fact about our source, and nothing in CI checks that fact. Tracked
  with the invalidating conditions spelled out in
  [`v0.2.0-dependency-hygiene.md`](v0.2.0-dependency-hygiene.md) section F.
