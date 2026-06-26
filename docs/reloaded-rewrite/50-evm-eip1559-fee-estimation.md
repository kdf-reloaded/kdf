# Chapter 50 — EVM EIP-1559 Fee-Per-Gas Estimation

**Status:** driving-spec.

This chapter binds the EVM fee-per-gas estimator's contract surface:
how a max-fee / priority-fee estimate is derived from an
`eth_feeHistory` response, and in particular which base-fee value the
estimator MUST select from that response so the resulting
EIP-1559 `maxFeePerGas` reflects the freshest available base fee
rather than a stale one.

## 50.1 Executive Summary

EVM chains since the EIP-1559 fee-market change price a transaction
by a *base fee per gas* (burned, set by the protocol per block) plus
a *priority fee per gas* (the miner/validator tip). To submit a
type-2 transaction the wallet must supply a `maxFeePerGas` (an upper
bound on base-fee + tip it will pay) and a `maxPriorityFeePerGas`
(the tip). The estimator derives both from the chain's recent fee
record obtained via the `eth_feeHistory` JSON-RPC method, then adds
headroom on top of the latest base fee so the transaction remains
includable even if the base fee rises before it is mined.

The `eth_feeHistory` response carries a `baseFeePerGas` array. By the
method's dictated semantics this array is ordered oldest-to-newest and
contains **one more** element than the number of blocks requested: the
trailing element is the base fee of the next (not-yet-mined / pending)
block. The freshest base-fee value available is therefore the **last**
element of the array.

**The defect this chapter corrects.** The simple fee-per-gas estimator
selected the **first** element of `baseFeePerGas` — the *oldest* block
in the requested window — as the "latest" base fee. On any chain where
the base fee is moving (almost always), the oldest-window base fee is
unrepresentative of current conditions and is typically lower than the
current/next-block base fee. Feeding that stale, too-low base fee into
the EIP-1559 computation skews `maxFeePerGas` downward, so the
estimated max fee can fall below the actual block base fee. Such a
transaction is rejected by nodes (it cannot be included while base-fee
> max-fee) or sits stuck until the base fee falls back. The corrected
estimator selects the **last** element of `baseFeePerGas`, the freshest
base fee, and applies the headroom multiplier on top of that value.

This chapter is deliberately narrow. It binds the base-fee **selection
semantics** dictated by `eth_feeHistory` / EIP-1559 and the obligation
to estimate from the freshest base fee. The numeric headroom
multipliers, the number of history blocks requested, the percentile
list, and every Rust-side data-shape decision are discretionary and are
**not** bound here.

## 50.2 Subsystem Shape

The estimator is a pure function of one input — a decoded
`eth_feeHistory` response for the EVM coin — and produces a
fee-per-gas estimate bundle (a max-fee figure and a priority-fee
figure, typically banded into low/medium/high tiers). It performs no
chain I/O of its own beyond consuming the already-fetched fee history.
Its two responsibilities are:

1. select the base-fee anchor from the response (the subject of this
   chapter), and
2. combine that anchor with a priority-fee figure derived from the
   response's reward percentiles, plus a headroom multiplier, into the
   reported estimate.

Only responsibility (1) carries an externally-dictated correctness
constraint; responsibility (2) is discretionary policy.

## 50.3 Dictated Interop: `eth_feeHistory` and EIP-1559 Base Fee

> **Binding scope (R50).** The facts in this section are dictated by
> the public Ethereum fee-market change (EIP-1559) and the public
> Ethereum JSON-RPC `eth_feeHistory` method definition (permitted
> inputs R3/R4). They are the contract any conforming EVM node
> presents; they are not derived from any particular implementation.
> The Rust types used to decode the response, their field and variant
> names, and the estimator's internal shape are discretionary and are
> not bound here.

The dictated facts the estimator MUST honour are:

- **Base fee is per-block and protocol-set.** Under EIP-1559 each
  block carries a `baseFeePerGas`. The base fee may change by at most
  one-eighth (12.5%) between consecutive blocks; when blocks are full
  it rises by that maximum, when empty it falls. A correct
  `maxFeePerGas` estimate must allow for this upward drift between
  estimation time and inclusion.

- **`eth_feeHistory` response shape.** A call for `blockCount` blocks
  ending at a given newest block returns, among other fields, a
  `baseFeePerGas` array. That array:
  - is ordered **oldest block first, newest last**;
  - has length **`blockCount` + 1** on mainline clients — the extra
    trailing element is the base fee computed for the **next**
    (pending, not-yet-mined) block;
  - therefore exposes the freshest available base-fee value as its
    **last** element.

- **Non-mainline clients.** Some non-mainline EVM clients omit the
  extra trailing next-block entry, so their `baseFeePerGas` array has
  length `blockCount` and its last element is the newest *mined*
  block's base fee. Selecting the **last** element remains correct in
  this case — it is still the freshest base fee the response carries —
  so the selection rule is uniform across both client behaviours.

## 50.4 Bound Estimator Contract Surface

**R1.** *Base-fee anchor selection.* When the simple EVM fee-per-gas
estimator derives an estimate from an `eth_feeHistory` response, the
base-fee anchor it uses MUST be the **last** element of the response's
`baseFeePerGas` array (the freshest base fee, per §50.3). The estimator
MUST NOT use the first / oldest element, nor any fixed interior index,
as the base-fee anchor. This rule fixes a present-source defect in
which the oldest-window element was selected, skewing `maxFeePerGas`
downward (§50.1).

**R2.** *Empty / missing array tolerance.* If the `baseFeePerGas` array
is absent or empty, the estimator MUST degrade gracefully to a
zero-valued base-fee anchor rather than failing or panicking; the
headroom and priority-fee terms (R3) still apply. (A conforming node
always returns a non-empty array; this rule bounds only the malformed /
unsupported-node path.)

**R3.** *Headroom on the freshest base fee.* The reported max-fee
figure MUST be the selected base-fee anchor (R1) scaled up by a
headroom factor that accounts for the EIP-1559 per-block base-fee
increase before inclusion, combined with the estimated priority fee.
The specific headroom multipliers, the per-tier (low/medium/high)
banding, the number of history blocks requested, and the reward
percentiles are discretionary policy and are **not** bound by this
chapter beyond the requirement that the factor be greater than or equal
to one (headroom never reduces the anchor).

**R4.** *Estimation surface placement.* The EIP-1559 fee-per-gas
estimator is part of the EVM coin's fee-estimation surface, distinct
from the gas-**limit** estimation (`eth_estimateGas`) covered for the
swap path in [Chapter 17](17-swap-v2-evm-path.md). A change to the
base-fee selection rule (R1) is local to this estimator and does not
alter the swap or withdraw transaction-construction contracts; those
consumers receive the corrected, higher (freshest) estimate
transparently.

## 50.5 Tests

**T1.** *Freshest-base-fee regression (the fix).* Given a synthesized
`eth_feeHistory` response whose `baseFeePerGas` array is strictly
increasing with a distinct first and last element (e.g. an array whose
last element is materially larger than its first), the estimator's
selected base-fee anchor MUST equal the **last** element. The test MUST
fail if the anchor equals the first / oldest element — this is the
direct regression guard for the corrected selection of R1.

**T2.** *Headroom monotonicity.* For the same response, the reported
max-fee figure MUST be greater than or equal to the selected base-fee
anchor (R3): headroom never produces a max fee below the freshest base
fee.

**T3.** *Empty-array tolerance.* Given a response with an absent or
empty `baseFeePerGas` array, the estimator MUST return a well-formed
estimate (zero base-fee anchor plus the priority-fee term) rather than
erroring or panicking (R2).

## 50.6 Upstream Divergence (informative)

Upstream bundled the base-fee selection correction (R1) together with a
second, related-but-distinct change on the **withdraw gas-details**
path: it widened the set of external EVM-client rejection strings that
are recognised as "the submitted fee cap is below the block base fee".
Concretely, in addition to the Geth-style strings (`fee cap less than
block base fee`, `max fee per gas less than block base fee`) it also
recognises the Nethermind (pre-1.38.0) phrasing `miner premium is
negative`, so all three map to the same typed fee-cap-below-base-fee
error instead of an opaque transport error. This matters on fast-block
chains (e.g. Gnosis) where strict clients (Nethermind ≥ 1.36.0) validate
`maxFeePerGas ≥ baseFee` during `eth_estimateGas`.

This behaviour is **informative only** for this chapter: it lives on the
withdraw gas-details surface, which the reloaded tree has not yet ported
(tracked as D2). The base-fee selection fix (R1) — the part within this
chapter's scope — is fully captured and matches upstream's functional
behaviour. The external-client error strings named above are dictated by
those third-party EVM clients (not upstream expression) and are recorded
here so the deferred classification work (D2) can reproduce the contract
faithfully.

Reloaded additionally hardens the percentile selection against an empty
reward slice (R2) — a path upstream still leaves able to panic — making
the reloaded estimator a strict superset on the malformed-input path.

## 50.7 Deferred Work

**D1.** *Provider-side fee estimation.* Deriving the estimate from an
external gas-oracle / provider API instead of (or as a fallback to)
`eth_feeHistory` is out of scope for this chapter; only the
history-based simple estimator's base-fee selection is bound here.

**D2.** *Withdraw fee-cap error classification.* When a transaction is
submitted with an explicit `maxFeePerGas` that is below the node's
current block base fee, strict EVM clients reject the request. Mapping
the client's textual rejection (the external-client error strings that
signal "fee cap below the block base fee" — see §50.6) into a typed,
user-meaningful "fee cap below base fee" error — rather than surfacing a
raw transport error — is part of the withdraw gas-details estimation
surface, **not** the fee-per-gas estimator bound here. That
error-classification surface is not yet present in the reloaded tree (it
arrived upstream with the Tron withdrawal feature); binding it is
deferred to the chapter that ports that surface.

## 50.8 Baseline Verifications

**V1.** *No EIP-1559 fee estimator at baseline.*

- *Claim.* The baseline tree contains no dedicated EVM EIP-1559
  fee-per-gas estimator module; this estimator (and therefore the
  base-fee selection bound in R1) is post-baseline work.
- *Verification.*
  `git ls-tree -r c1d46c0c1592faa0860f704008b2b2381bc3840f -- mm2src/coins/eth/fee_estimation/`
  returns the empty set.
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md).

## 50.9 External References

- EIP-1559 (fee-market change for the Ethereum chain; per-block base
  fee, the one-eighth maximum per-block base-fee change, and the
  `maxFeePerGas` / `maxPriorityFeePerGas` transaction-pricing model).
- Ethereum JSON-RPC `eth_feeHistory` method definition
  (`baseFeePerGas` array ordering, the `blockCount` + 1 length with the
  trailing next-block entry, `reward` percentiles, `gasUsedRatio`,
  `oldestBlock`). Reference:
  `https://ethereum.org/developers/docs/apis/json-rpc`.
- [Chapter 17 — Atomic-Swap V2 EVM Path](17-swap-v2-evm-path.md) for
  the related but distinct gas-**limit** estimation (`eth_estimateGas`).

## 50.10 Provenance Footer

- *Inputs:* the public EIP-1559 fee-market specification and the public
  Ethereum JSON-RPC `eth_feeHistory` method definition (the dictated
  base-fee selection semantics); the baseline tree (V1).
- *Permitted-input classes used:* R3 (external public specifications —
  EIP-1559), R4 (wire formats / external interfaces the project must
  inter-operate with — the `eth_feeHistory` response shape), R1 (the
  baseline tree, for V1).
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* consulted **only** to confirm the present
  upstream fee-per-gas estimator's base-fee-selection defect (the
  oldest-window element being used as the latest base fee) and, on a
  later parity pass, to confirm that the upstream fix changes exactly the
  same base-fee selection (oldest → freshest) and to identify the
  informative divergence recorded in §50.6 (the bundled withdraw-path
  recognition of an external EVM-client "fee cap below base fee"
  rejection string). The corrected selection requirement (R1) and every
  dictated fact in §50.3 are sourced from the public EIP-1559
  specification and the public Ethereum JSON-RPC `eth_feeHistory` method
  definition (R3/R4); the external-client error strings named in §50.6
  are dictated by those third-party EVM clients (R4). None of this is
  the corpus's discretionary expression. No discretionary expression —
  no private type, field, or function names, no function bodies,
  control-flow transcription, helper decomposition, local names, error
  or log literals, or discretionary constants — was reproduced.
