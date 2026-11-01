# Plan: V2 swap engine dedup, error-path hardening, and V1 driver consolidation

**Status:** parked (T1–T7, T9–T10 shipped; T8 deferred, T11–T13 queued) — **Last reviewed:** 2026-10-18

> **Context:** T1-T7 and T9-T10 are done (commits `220f8cafc`, `ba0b0a739`,
> `9f79af496`, `5923c4558`, `7c021ea47`). T8 was reassessed as riskier than
> categorized and deferred, not attempted (see its own entry below). T11-T13
> (V1 consolidation) remain queued for a future round, per the plan's own
> original phasing. T-numbering is used in chat with the user (not
> otherwise written down); see the numbered findings/phases below for the
> underlying P-numbering this plan originally shipped with.
>
> This plan is adapted from an external code review of `lp_swap/` (2026-08-25),
> supplemented with independent verification against this repository. Findings
> below are marked **CONFIRMED** where re-checked directly against source,
> **CONFIRMED (severity revised)** where the underlying fact is real but the
> reviewer's framing overstated it, or **TAKEN ON TRUST** where the reviewer's
> demonstrated precision on adjacent, checked claims made independent
> re-verification a poor use of time for that specific item. Two items in
> Phase 2 turned out to be new, previously-undocumented gaps rather than pure
> style cleanup; those are now also recorded in CRD chapters directly (see
> "New CRD entries" below) so they survive independently of this plan's
> lifecycle.

## Goal

Reduce `lp_swap/`'s maintenance burden (parallel V1+V2 implementations,
~21K lines across five files >2K lines each) through mechanical,
behavior-preserving deduplication first, then a small set of deliberate,
individually-justified error-path hardening changes, then (lowest priority,
highest risk) V1 driver consolidation. Every phase is independently
shippable and reversible; later phases are not blocked on earlier ones
landing, only on the review discipline this project already applies (see
`AGENTS.md` §2, the Coder/dispatcher review loop) being followed for each.

## Verification baseline (re-run, not assumed, 2026-08-25)

- `cargo check --workspace` — clean.
- `cargo test -p mm2_main --lib` — passes (450 tests at review time).
- `cargo clippy -p lightning --no-deps` — **fails**: 13 deny-level errors in
  the vendored, patched `rust-lightning` crate, confirmed independently.
  `cargo clippy --workspace` therefore cannot succeed today; any crate that
  depends on `lightning` (most of the workspace, transitively) never gets
  linted by a bare `cargo clippy --workspace` invocation. This is real and
  already the single highest-leverage process fix in this plan (Phase 0).

## Findings, most-to-least confirmed-severe

### FIXED (T1, commit `220f8cafc`) — D8-class: `MakerPaymentSpent`'s
timeout-abort persisted an unusable, empty payment record (new; not in the
original review's own framing, found while verifying it)

`taker_swap_v2.rs`'s `MakerPaymentSpent::on_changed`, on the maker-payment-
spend-confirmation-timeout path, transitions to `TakerPaymentRefundRequired`
carrying `BytesJson::default()` for the taker's own payment transaction
(comment: "We don't have taker_payment bytes here; use empty as fallback").
That refund state hands the empty bytes straight to
`refund_combined_taker_payment` as `RefundTakerPaymentArgs::payment_tx`,
which cannot construct a valid refund from zero bytes. Root cause: the field
is dropped two transitions earlier than where it's needed —
`TakerPaymentSent` carries `taker_payment: BytesJson`, but `TakerPaymentSpent`
(the very next state) already drops it, keeping only `taker_payment_spend`
(what consumed it) and `maker_payment`. `StoredTakerNegotiationData` was
never scoped to carry it either. **This is a live, reachable bug on native
builds**, not a style nit — any delayed, dropped, or reorganized
confirmation of the maker's own payment spend triggers it. Now recorded as
[Chapter 52](../reloaded-rewrite/52-swap-v2-state-machine.md) D8 (now
**Closed**). Took direction (a), the additive `#[serde(default)]` field —
verified in the fix that it was not awkward anywhere, `taker_payment` was
in scope at every call site as expected, so direction (b) was never
needed.

### CONFIRMED — F8: WASM V2 swap storage is a total no-op

`swap_v2_common.rs`'s `wasm` module's `StateMachineStorage` impls for both
maker and taker are stubs: writes are `Ok(())` no-ops, `has_record_for` is
always `false`, `get_unfinished` is always empty. A V2 swap run from a
WebAssembly build persists nothing; a reload during a live swap is
unrecoverable. This is also a confirmed violation of an existing binding
requirement — [Chapter 26](../reloaded-rewrite/26-cross-platform-and-wasm.md)
R8 names the V2 swap state stores explicitly among the consumers required
to have a real IndexedDB backend, not a stub. Now recorded as ch.52 D7 and
cross-referenced at ch.26 D6, since it's simultaneously "this subsystem has
a gap" and "this binding rule has a known violation."

### CONFIRMED (severity revised): F9 — vendored `lightning` breaks bare
`cargo clippy --workspace`, but not per-package clippy

Re-run directly: `cargo clippy -p lightning --no-deps` exits with 13
deny-level errors, confirming the crash is real. But re-checked further
(2026-08-26) before writing the fix: `cargo clippy -p coins --no-deps --
-D warnings` exits 0 and reports only `coins`' own diagnostics — `lightning`
being in the dependency graph does not suppress or block first-party
linting for a package that merely *depends* on it. The original framing
("first-party clippy coverage silently stops at the dependency boundary")
overstated the impact: `AGENTS.md`'s already-documented per-package clippy
pattern was never affected by this. What actually breaks is a literal
`--workspace` sweep, or invoking clippy with `-p lightning`/
`-p lightning-invoice` directly — real, and worth documenting so nobody hits
it running an ad hoc audit (exactly how the external review found it), but
not the "silent workspace-wide blind spot" it first looked like. Documented
in `AGENTS.md` §6 rather than as a CI fix, since no CI job runs `cargo
clippy --workspace` today — there was nothing in CI to actually fix.

### CONFIRMED: F1 — 47 copy-paste refund-transition blocks

Recounted directly: `TakerFundingRefundRequired::new` appears 24 times in
`taker_swap_v2.rs`, `MakerPaymentRefundRequired::new` 23 times in
`maker_swap_v2.rs` — the reviewer's "47" is exact for the two constructors
they named. Not counted in their figure, found while verifying:
`TakerPaymentRefundRequired::new` appears a further 12 times in the same
file — the real copy-paste surface for refund-transition blocks is larger
than the headline number, not smaller.

### CONFIRMED (severity revised): F3's other three sub-findings

- The `net_config_or_panic` and recovery-path `.expect()` sites are real
  or-panic calls, confirmed present at the cited files. Whether each is
  truly reachable (vs. a "this config value is registered at startup and
  cannot be absent by construction" case, the way the zero-secret fallback
  below turned out to be) was not individually re-verified for all seven
  `net_config_or_panic` sites — worth confirming per-site before deciding
  each one warrants a behavior change, not assuming uniformly.
- **The zero-secret fallback is not reachable.** `sm.taker_secret` is typed
  `primitives::hash::H256` — a fixed 32-byte array type (`define_hash!(H256,
  32)`) — so `.as_slice().try_into()` into a `&[u8; 32]` can never fail;
  `unwrap_or(&[0u8; 32])` is dead code, not a live silent-corruption path.
  It should still be cleaned up (a fallback that implies fallibility where
  none exists is misleading to a future reader, and would become a real bug
  if `H256`'s definition ever changed to a variable-length type), but it
  does not belong in the same severity class as the `MakerPaymentSpent`
  empty-bytes bug above, which the original review's F3 grouped it with.

### CONFIRMED: F4 — `append_swap_v2_event`'s read-modify-write has no
transaction

`swap_v2_common.rs`'s `append_swap_v2_event` does `SELECT` → parse → push →
`UPDATE` with no `BEGIN`/`COMMIT` wrapper. The reviewer's own caveat — "safe
today only because a per-uuid file lock serializes writers" — is also
confirmed: both `MakerSwapStateMachine`/`TakerSwapStateMachine` declare
`type ReentrancyLock = SwapLock`, acquired per swap uuid before the machine
runs, which does serialize `store_event` calls for a given uuid in
practice. Real gap, correctly self-caveated by the reviewer as low-urgency;
cheap to fix regardless (wrap in `conn.transaction()`).

### TAKEN ON TRUST (not independently re-verified line-by-line; reviewer's
other quantitative claims all checked out): F2, F5, F6, F7

Duplicated derivation, twin SQL/kickstart functions, V1 driver duplication,
event/mirror boilerplate. No reason found to doubt these; not re-counted
given the effort-to-value ratio of re-deriving numbers the reviewer already
demonstrated care in producing elsewhere.

## New CRD entries (already written; not gated on this plan being executed)

- [Chapter 52](../reloaded-rewrite/52-swap-v2-state-machine.md) §52.13 D7
  (WASM storage no-op) and D8 (`MakerPaymentSpent` empty-bytes bug).
- [Chapter 26](../reloaded-rewrite/26-cross-platform-and-wasm.md) §26.14 D6
  (R8 non-conformance cross-reference).

These are documentation of the gap, not a fix. D8 in particular should not
be treated as merely deferred-and-fine; see "Recommend prioritizing" above.

## Phased plan (adapted from the original review; sequencing unchanged,
annotated where this repository's own verification changed the picture)

### Phase 0 — Guardrails (½ day, no behavior change) — DONE, see T2 below

- P0.1/P0.2 **Done** (`AGENTS.md` §6, 2026-08-26): documented that a bare
  `cargo clippy --workspace` (or `-p lightning`/`-p lightning-invoice`
  directly) fails on the vendored crate's own deny-level lints, confirmed
  this does *not* affect the already-documented per-package clippy pattern
  (re-verified directly, see the revised F9 note above), and gave the
  `RUSTFLAGS="--cap-lints=warn"` escape hatch for the rare case a genuine
  workspace-wide sweep is wanted. No CI change was needed — no CI job runs
  `cargo clippy --workspace` today.
- P0.3 Not done as a standalone step: a clean, uncached first-party warning
  count needs a full rebuild to be trustworthy (an incrementally-cached run
  under-reports), which wasn't worth the wall-clock cost on its own: the
  external review's own reported figure (~180) is the working baseline
  until a future full-clean sweep re-derives it.

### Phase 1 — Mechanical dedup, no behavior change

- ~~P1.1~~ **Done as T7, commit `5923c4558`.** One private helper method per
  originating state type (not a single universal generic function — lower
  risk, and each `Self::change_state` call site was already statically
  typed to one concrete state, so no generic `TransitionFrom` bound
  engineering was needed). 53 call sites replaced across both files; two
  single-occurrence sites deliberately left inline. Net −328 lines
  (estimate was −450…−550; the difference is the per-type helper bodies
  themselves, which a single universal function wouldn't have needed —
  a real cost of choosing the lower-risk approach, not a shortfall).
- ~~P1.2~~ **Done as T3, commit `9f79af496`.** Deleted the redundant second
  `taker_taker_coin_pub_bytes` derivation; the two other occurrences of the
  same shape were confirmed to be distinct, single derivations and left
  alone.
- ~~P1.3~~ **Done as T6, commit `9f79af496` — SQL functions only.**
  `insert_swap_v2_{maker,taker}` merged into a shared private
  `insert_swap_v2` plus a `SwapV2InsertFields` struct; public signatures
  unchanged; SQL/parameter shape verified byte-identical to both
  originals. The two kickstart handlers were deliberately **not**
  merged — they differ by type (`DbRepr`/`Storage`/`StateMachine`
  triads), not just value, so a real merge needs a generic trait
  abstraction, its own separate design decision. Still open if wanted.
- P1.4 (T8) **Reassessed and deferred, not attempted — riskier than its
  original categorization.** F7's `*SwapEventDeser` mirrors turn out to
  live in `taker_swap.rs`/`maker_swap.rs` — the **V1** legacy swap files,
  not V2 — meaning this is compat-sensitive persisted-event-log
  deserialization territory (`AGENTS.md` §1's persistent-data
  compatibility rule applies directly), not a same-crate V2 refactor like
  T7. Looked closer before dispatching anything: `TakerSwapEvent`'s custom
  `Deserialize` impl pre-checks three variants before falling through to
  the full `TakerSwapEventDeser` mirror + its 47-line `From` impl. Two of
  the three existing regression tests
  (`test_legacy_taker_watcher_message_sent_event_is_accepted`,
  `test_legacy_taker_maker_payment_spend_confirmed_event_is_accepted`)
  prove the pre-check tolerates an *unexpected, ignored* `data` payload on
  what's otherwise a unit variant — the **opposite** direction of
  tolerance from what the in-code comment ("observed with and without
  payloads") suggests at a glance, and not something a naive
  directly-derived `Deserialize` would reproduce automatically (serde's
  default adjacently-tagged unit-variant handling typically errors on an
  unexpected payload, it doesn't silently ignore one). A real fix here
  needs to reproduce that exact tolerance deliberately, verified against
  real historical JSON shapes, not just "existing tests still pass" —
  genuine design work, not mechanical dedup. Queued for a dedicated future
  pass rather than rushed in this round.
- Update `dex_fee.rs`'s `include_str!` source-text meta-tests alongside
  P1.1/P1.2 — they assert on `taker_swap_v2.rs`/`maker_swap_v2.rs`'s literal
  source text and will need updating for any refactor touching those files.

### Phase 2 — Error-path hardening (small, deliberate behavior changes)

- ~~P2.0~~ **Done as T1, commit `220f8cafc`.** `taker_payment` threaded
  through `TakerPaymentSpent`/`MakerPaymentSpent` as an additive field; two
  new regression tests; ch.52 D8 marked Closed.
- ~~P2.1~~ **Done as T9, commit `7c021ea47`.** All 7 sites replaced with
  the existing fallible `net_config_for`, mirroring each site's own
  established sibling abort pattern. New `dex_fee.rs` source-text
  meta-test (`t18_...`).
- ~~P2.2~~ **Done as T5, commit `9f79af496`.** Replaced with `&sm.taker_secret`
  — `H256: Deref<Target = [u8; 32]>` (verified directly in
  `kdf_primitives`' `define_hash!` macro), so this is a plain infallible
  deref, not a fallback of any kind.
- ~~P2.3~~ **Investigated as T10, commit `7c021ea47`. Documentation, not
  a behavior change.** All 3 `.expect()` messages traced to their root
  cause rather than reflexively converted. `SwapsContext::from_ctx`'s
  `.expect()` left as-is (structurally infallible in practice, matches
  this codebase's own pervasive convention). The two P2P-pubkey
  `.expect()`s left as panics, deliberately not converted to a "logged
  graceful degrade" — that would silently break the swap's P2P routing,
  judged worse than a loud panic. Investigation surfaced a real,
  previously-undocumented gap: CRD ch.52 R9's "fifth case" (malformed
  persisted bytes must fail recreation with a distinct error) isn't
  implemented for the P2P identity pubkey specifically — now ch.52 D9.
- ~~P2.4~~ **Done as T4, commit `9f79af496`.** `append_swap_v2_event`'s
  SELECT/UPDATE now run inside one `conn.transaction()`, matching the
  pattern already used elsewhere in this codebase.

### Phase 3 — V1 consolidation (medium risk; do after Phase 1 experience)

- P3.1 Factor `run_taker_swap`/`run_maker_swap`'s shared driver skeleton
  into one parameterized helper (F6).
- P3.2 Snapshot-style lock extraction in V1 swaps (pattern already exists
  at `taker_swap.rs:1515-1558`) to make the not-`Send`-across-`.await`
  discipline structural rather than tribal.
- P3.3 (Optional) Move V1 `Result<_, String>` internals to
  `MmError<SwapError>` at the seams P3.1 touches only — do not churn
  untouched signatures.

### Explicitly out of scope (recorded, not planned)

- Full WASM IndexedDB V2 storage implementation (ch.52 D7) — its own
  project; the CRD entry, not this plan, is where its scope belongs once
  someone picks it up.
- Any serde layout change to persisted events/negotiation structs beyond
  the additive, `#[serde(default)]`-guarded field P2.0 may need.
- Completing the V1→V2 product migration — product decision, would
  obsolete much of Phase 3.
- Reworking `MmError`'s nightly-auto-trait bootstrap — fragile but
  documented, pinned, revisit only on the next toolchain bump.

## Suggested execution order & sizing

| Step | Items | Risk | Est. |
|---|---|---|---|
| 1 | P0.1–P0.3 | none | done (commit `ba0b0a739`) |
| 2 | P2.0 | correctness fix | done (commit `220f8cafc`) |
| 3 | P1.2, P1.3 (SQL only), P2.2, P2.4 | trivial | done (commit `9f79af496`) |
| 4 | P1.1 (dex_fee meta-tests needed no update) | low | done (commit `5923c4558`) |
| 5 | P1.4 | **reassessed as medium-risk, V1 compat territory** | deferred, not attempted |
| 6 | P2.1, P2.3 | medium (behavioral) | done (commit `7c021ea47`) |
| 7 | P3.1–P3.2 | medium | pending, queued for a later round |
| 8 | P3.3 | optional | pending, queued for a later round |

Total mechanical-dedup yield unchanged from the original estimate: roughly
−700…−900 lines in `lp_swap/` with net-zero behavior change from Phase 1.
The one real correctness fix (P2.0) is done.
