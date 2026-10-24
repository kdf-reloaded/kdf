# Plan: version-two dynamic-fee spend-estimate mismatch

**Status:** parked (options recorded, none chosen; staying at parity) — **Last reviewed:** 2026-10-18

> **Context:** this is a problem statement and a set of options,
> not a design. The maintainer decided to stay at parity with the reference
> implementation for now; none of the options below has been chosen, and none
> is planned. Found while implementing the version-two UTXO wire contract for
> issue #11.

## The problem

A version-two UTXO taker-payment spend carries a spend-fee estimate, `S`,
that both sides compute independently. The taker computes `S` from its
taker coin's fee rate when it builds the taker-payment-spend preimage
([Chapter 16](../reloaded-rewrite/16-swap-v2-pre-burn-output.md) R14A). The
maker recomputes `S` the same way when it validates that preimage, and
requires the two to match exactly, with no tolerance (ch.16 R17). The same
estimate, from the same rule, also governs the funding-spend preimage
([Chapter 15](../reloaded-rewrite/15-swap-v2-utxo-path.md) R21/R22).

For a coin whose fee rate is **dynamic** — fetched from the node at request
time, rather than a fixed per-kilobyte constant in the coins configuration —
the two sides can observe different rates: a different server, a different
moment, or ordinary rate movement between the taker building the preimage
and the maker validating it. When that happens, the maker's `S` differs
from the taker's, the exact-equality check in R17 fails, and the maker
rejects an otherwise-legitimate preimage. The swap does not simply pause;
the taker has already funded it, so the failure routes into the refund
path rather than completing.

This affects the version-two protocol only. Legacy (v1) swaps compute and
validate the dex fee itself, not a spend-fee estimate for a subsequent
spend transaction, and are unaffected. Version-two is used only when both
peers negotiate it, so the exposure is also bounded by version-two
adoption, not universal.

## Scale

In the public `komodo-coins` `coins` file, 47 of the 172 UTXO coins
enabled for swaps configure a dynamic fee rate rather than a fixed one —
BTC, NMC, PPC, and CHIPS are examples. A dynamic-fee coin on either side of
a version-two UTXO pair is exposed to this mismatch.

## This is also the reference implementation's behavior

The exact-equality rule in R17 is not a Reloaded design choice: it is part
of the `v2.6.0-beta` wire contract that chapter 16 binds this project to.
A deployed reference node runs the same rule against the same dynamic-fee
sources, so it has the same failure mode. Staying at parity here is a
deliberate decision to match a known, deployed contract, not an oversight
carried over from not looking at the problem.

## Options

None of these is chosen. They are recorded so the trade-offs are visible
the next time this comes up.

- **(A) Stay at parity.** Keep the exact-equality rule as chapter 16 R17
  binds it. This is the current decision. It costs nothing to maintain and
  matches the deployed reference exactly, at the cost of the failure mode
  above for dynamic-fee coins on version-two swaps.

- **(B) A maker-side acceptance tolerance on `S`, within a bounded band.**
  The maker would accept a taker-built preimage whose implied `S` falls
  within some band around its own recomputed value, instead of requiring
  an exact match. This stays wire-compatible in the sense that it only
  widens what our own maker *accepts* — it does not change what our taker
  *sends*, and a preimage a reference taker builds would still validate.
  It would need a bound on how much of the maker's own payout it can lose
  per fee form (the `Standard` case appends the maker's payout after the
  fact, so a mis-sized `S` there shifts the maker's own output; `WithBurn`
  and `NoFee` preimages are taker-built in full, so the same slack shows up
  differently). Because it is a deliberate divergence from the reference
  contract, it would need a `docs/COMPAT_SWITCHES.md` entry with a scoped
  opt-back to the strict rule, and a corresponding update to
  `GLEEC_COMPATIBILITY.md`. It relates to ch.16 D5, which already defers an
  analogous tolerance question on the funding-spend side (chapter 15
  R21/R22) as non-blocking.

- **(C) Carry `S`, or the fee rate it was computed from, explicitly in the
  version-two swap messages.** Instead of each side deriving `S`
  independently, the taker would state the value it used and the maker
  would validate against the stated value. This removes the mismatch at
  the root, but it is a wire-protocol change: a deployed peer running the
  reference implementation does not expect or understand an extra field
  in these messages. It is only viable coordinated with upstream, or gated
  behind a version-two protocol-version negotiation that does not exist
  today.

- **(D) Pin a fixed per-coin rate for version-two spends in the coins
  configuration**, overriding the dynamic source for this purpose only.
  This avoids touching the wire format, but only works if both peers in a
  swap configure the same fixed rate for the same coin — which this
  project cannot enforce for a counterparty running a different
  implementation or a different configuration file. Fragile across
  implementations for exactly that reason.

## What would trigger revisiting this

- Version-two becoming the default swap protocol rather than an opt-in.
- Dynamic-fee coins seeing real version-two trade volume in practice.

## References

- [Chapter 16](../reloaded-rewrite/16-swap-v2-pre-burn-output.md) R14A
  (spend-fee estimate), R17 (exact-match preimage validation), D5 (the
  related, already-deferred funding-spend tolerance question).
- [Chapter 15](../reloaded-rewrite/15-swap-v2-utxo-path.md) R21/R22
  (funding-spend preimage generation and validation, the same estimate).
- [KDF Reloaded issue #11](https://github.com/kdf-reloaded/kdf/issues/11) —
  the context in which this was found.
