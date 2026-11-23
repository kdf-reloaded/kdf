# Plan: an operator switch that retries recovery of failed swaps

**Status:** planning (target v0.6; starts with a decision, not with code) — **Last reviewed:** 2026-11-23

> **Context:** in 0.5.0 a swap whose refund reads stay undetermined under the
> two-node evidence rule (CRD ch.51 R100) waits until its refund deadline and
> then ends as a refund failure that names the fix: add a node on a second host
> or set `swap_evidence_node`, then call `recover_funds_of_swap`. The maintainer
> chose that bounded wait so that an unattended node shows a clear failure
> instead of waiting without end. The same "manual recovery applies" ending
> exists for other failures too. While deciding this, the maintainer proposed a
> switch that lets the operator, or the author of a frontend application, choose
> whether such swaps are retried automatically. The decision is postponed to
> v0.6 because it needs a discussion with experienced users.

## Goal

Give the operator (or the frontend author) the choice between today's behavior
(a swap that needs manual recovery stays failed until someone acts) and
automatic retries of the recovery, without hiding from the user that something
went wrong.

## Step 0: the decision (before any design or code)

Discuss with experienced users (market makers running unattended nodes,
frontend authors, desktop and mobile wallet maintainers), then decide:

1. **Whether to add a switch at all.**
   - (a) No switch. The failure plus `recover_funds_of_swap` stays the only way.
   - (b) A switch, off by default.
   - (c) A switch, on by default.
2. **Where the choice lives.**
   - Node configuration (one setting for all swaps).
   - Per coin (for example only for coins with unreliable servers).
   - Per swap, through an RPC that a frontend calls.
   - Several of these, with a stated precedence.
3. **What the switch covers.**
   - Only refunds that failed because reads stayed undetermined.
   - Every failure that ends with "manual recovery applies" (refund rejected,
     claim not confirming, payment not found, and similar).
   - A list chosen per failure kind.
4. **How the user still learns about the problem.**
   - The swap is marked failed at the deadline as today, and a later automatic
     recovery appends a recovery record.
   - A separate "recovering" state instead of failed.
   - What the GUI, the events and the swap statistics show in each case, and
     what wallets built against the reference expect.
5. **Retry policy.**
   - Interval and backoff.
   - An upper limit, if any, after which only manual recovery remains.
   - Behavior across restarts.
   - Behavior when the operator changes the node list or `swap_evidence_node`
     (an immediate retry?).
6. **Safety limits.**
   - No retry may ever spend or refund on an undetermined read (R100 still
     binds).
   - Every retry uses the same checks as a manual `recover_funds_of_swap`.

Questions to put to the users:

- Do you run nodes unattended, and how do you learn about failed swaps today?
- Would you rather see a failure and act, or have the node retry and tell you
  afterwards?
- Does your frontend need to control this per swap, or is a node setting
  enough?
- Which failure kinds have you had to recover by hand, and how often?

## After the decision

1. CRD text: a Reloaded-original rule in ch.51 next to R100 and the recovery
   rules, with the chosen states, events and limits.
2. Compatibility: RELOADED_VS_GLEEC and GLEEC_COMPATIBILITY entries. A new state
   or event is a wire-visible change for wallets that read swap status.
3. Implementation in the swap engine and in `recover_funds_of_swap`, sharing one
   recovery path.
4. Tests, per failure kind covered: a scripted outage that recovers, an outage
   that never recovers, and a switch change while a swap waits.
5. Lab rows: an unattended maker with one server held down past the refund
   deadline, then restored, with the switch off and on.
6. Documentation for operators and frontend authors.

## Non-goals

- Changing the 0.5.0 behavior. It stays as decided: a bounded wait, then a clear
  failure naming the fix.
- Retrying anything that R100 forbids.
