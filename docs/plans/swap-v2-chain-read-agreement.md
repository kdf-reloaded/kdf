# Plan: swap protocol v2 follows the agreement of chain reads

**Status:** planning — **Last reviewed:** 2026-11-23

**CRD:** ch.51 §51.6G R100 (with its status notes) and ch.52 (dated status notes). Reloaded-original, clean side.

The chain reads that a refund step or a secret-using step rests on must agree between two nodes of the coin's
node pool, or come from the node the operator trusts. An undetermined read makes the step wait; it never refunds
or claims on it, and a running swap's wait has the bound of R100 item 9 with both its status notes of 2026-11-23.
This plan brings the version 2 swap state machines (`lp_swap/maker_swap_v2.rs`, `lp_swap/taker_swap_v2.rs`) and
their coin methods to that rule.

## Starting point (dev, 2026-11-23)

- A fresh v2 swap cannot start in this build; a saved unfinished v2 swap is resumed (ETH, UTXO).

## Steps

1. The maker's search for the funding spend retries on a node error. A failed or unfinished confirmation read
   after a node's own claim is undetermined, for the maker and the taker. An ETH secret read from a claim is used
   only when it matches the swap's secret hash.
2. The two-node reads of the v2 coin methods, UTXO and ETH: the search for the funding spend, the search for the
   taker payment's spend, the refund gate, and the confirmation reads the steps rest on. Each answers
   "undetermined" in the form the legacy path already understands.
3. The v2 steps wait on an undetermined read, and the refund states read the counterpart payment before refunding.
   A refund wait ends at its deadline; `recover_funds_of_swap` then runs the refund again for a v2 swap. The taker's
   immediate funding refund waits for an agreeing "funding not spent"; after the funding locktime it takes the
   timelock branch.
4. The family switch, once the reads of a family ask two nodes, with the test that ties v2 to that switch.

## Decisions

- 2026-11-23, maintainer:
  - R100 binds swap protocol v2 in 0.5.0, in full.
  - `recover_funds_of_swap` also accepts a v2 swap and runs its refund again with fresh agreeing reads.
  - The taker's immediate funding refund counts as a secret-using step under R100. It waits for an agreeing
    "funding not spent". After the funding locktime it takes the timelock branch, after an agreeing gate read; then
    it ends as above.
  - Every v2 wait, the taker's funding refund included, has the bound of R100 item 9 with both its status notes of
    2026-11-23, as the legacy path: the later of the lock time plus 3700 seconds and 3700 seconds after the step
    first could act.
