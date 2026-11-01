# Chapter 56 — EVM EIP-1559 Fee-Per-Gas Estimation

**Status:** driving-spec.

This chapter binds the EVM fee-per-gas estimator's contract surface:
how a max-fee / priority-fee estimate is derived from an
`eth_feeHistory` response, and in particular which base-fee value the
estimator MUST select from that response so the resulting
EIP-1559 `maxFeePerGas` reflects the freshest available base fee
rather than a stale one.

## 56.1 Executive Summary

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

## 56.1A Motivating field observation (2026-10-30)

A community maintainer running Reloaded `dev` at `d369906c2` against a
public, load-balanced Ethereum mainnet endpoint reported one failed
maker-side request. The node answered that request with a generic
server error (JSON-RPC code `-32000`) whose text stated that the
request had *failed with N gas* because the *max fee per gas* was *less
than the block base fee*, and appended the two amounts: the submitted
cap of `260384509` wei against a base fee of `289502753` wei
(permitted-input class R6, public-network observation). Three facts
follow from the numbers alone and need no implementation reading:

- the ratio of the two amounts is about 1.112, i.e. **one** full block's
  maximum base-fee rise (one-eighth, §56.3) separates the price the
  daemon used from the base fee the node validated against;
- the submitted cap exceeds the previous block's implied base fee
  (`289502753 / 1.125`) by roughly three million wei — the size of a
  node's suggested tip on a quiet chain, not of any headroom factor;
- the text is the Geth-family phrasing of R5, and the *failed with N
  gas* prefix is the phrasing a node uses when it rejects a
  **gas-limit estimation**, not a broadcast.

Reloaded's own tree had already implemented the freshest-anchor rule
R1 on 2026-10-18 (commit `4410edeaa`, contained in `d369906c2`), so the
symptom is **not** the oldest-element defect of §56.1. The arithmetic
instead matches a request priced from a plain *gas price* with no
headroom that was one block stale by the time the node validated it;
§56.4 (R6, the code-quality finding beneath it) and D5 state what the
daemon must do about it. The field report is recorded here as the
motivating observation for R5 and R6.

> **Clean-side correction (2026-10-30; Reloaded's own tree at
> `d369906c2`, no forbidden-class input).** The failed request was not a
> swap step, and no swap was in progress. The call chain in the logged
> line places it in the balance check of a `setprice` order placement
> (`ordermatch_trading.rs:2364`, then `maker_swap.rs:2438`):
> - **The request** was the gas-limit estimation of the ERC-20 `approve`
>   that a token's first V1 payment needs (`eth_mm_coin.rs:218`,
>   `eth_impl.rs:412`).
> - **Its price** was the legacy gas price of
>   [Chapter 57](57-evm-fee-inputs.md) R57.2.1: the larger of the node's own
>   suggestion and the mined block's base fee raised by 13 percent (R57.2.2).
>   Inside an order-posting request that price may be a reading reused from
>   the short cache of R57.2.6.
>
> The two amounts therefore fit two mechanisms that one log line cannot
> tell apart:
> - a plain node price one block stale, as the bullets above read it;
> - a reused fee-history reading two or more blocks old: the base fee the
>   node validated against is about 1.256 times the mined base fee such a
>   reading implies.
>
> R6 covers both, because the fee fields of an estimation are fetched for
> that request and never reused.

## 56.2 Subsystem Shape

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

## 56.3 Dictated Interop: `eth_feeHistory` and EIP-1559 Base Fee

> **Binding scope (R56).** The facts in this section are dictated by
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

- **Fee-cap validation by the node (addendum 2026-10-30).** EVM clients validate a transaction's fee cap against the
  current block base fee, and several do so already at **gas-limit
  estimation** when the estimation request carries fee fields, not only
  at broadcast. The Geth family does so whenever the estimation request
  carries a non-zero price or cap; Nethermind does so strictly from
  release 1.36.0. The same clients treat a request that
  carries only a legacy `gasPrice` as if both EIP-1559 caps were equal to
  that price, so a legacy price below the base fee is rejected with the
  same fee-cap text as a type-2 cap would be. Consequences for the
  daemon: (i) a price taken from the node without headroom can be
  rejected one block later on a chain whose blocks are full (the
  observation of §56.1A). Two full blocks raise the base fee by about
  26.6 percent (one-eighth twice, as above), so a few blocks of staleness
  defeat a modest headroom. On a fast-block chain (for instance Gnosis,
  §56.6) that is a few seconds. In Reloaded's own tree, the order-posting
  short cache of [Chapter 57](57-evm-fee-inputs.md) R57.2.6 keeps a
  reading for at least two seconds, which spans several blocks of a
  sub-second chain (clean-side observation). (ii) The rejection is a
  node-dictated, recognisable condition, bound in R5.

## 56.4 Bound Estimator Contract Surface

**R1.** *Base-fee anchor selection.* When the simple EVM fee-per-gas
estimator derives an estimate from an `eth_feeHistory` response, the
base-fee anchor it uses MUST be the **last** element of the response's
`baseFeePerGas` array (the freshest base fee, per §56.3). The estimator
MUST NOT use the first / oldest element, nor any fixed interior index,
as the base-fee anchor. This rule fixes a present-source defect in
which the oldest-window element was selected, skewing `maxFeePerGas`
downward (§56.1).

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

> *R5 and R6 below are the addendum of 2026-10-30, see §56.1A and §56.10.*

**R5.** *Typed classification of a fee-cap-below-base-fee rejection.* On
every EVM path that presents a fee cap to a node — gas-limit estimation
for a swap step or a trade preimage, the withdraw gas-details estimation
of [Chapter 57](57-evm-fee-inputs.md) R57.7.3, and the broadcast of a
signed transaction — a node error whose text contains one of the
following node-observable phrasings MUST be surfaced as the typed
"fee cap below base fee" error of the EVM coin layer, never as a raw
transport or internal error:

- `max fee per gas less than block base fee` — the Geth-family text
  (also emitted by several Geth-derived clients), and the text of the
  §56.1A observation;
- `fee cap less than block base fee` — the alternative Geth-family
  phrasing;
- `miner premium is negative` — the text Nethermind emits for the same
  condition in releases **before 1.38.0** (Nethermind from 1.36.0
  validates the cap at gas estimation; from 1.38.0 it no longer uses
  this phrasing, so a deployment that must also cover newer Nethermind
  relies on the two Geth-family texts above being matched as well).

Recognition MUST be by containment, exactly as the node spells the
phrase, because nodes wrap it: the observed form carried a
*failed with N gas:* prefix and a trailing `maxFeePerGas: <wei>,
baseFee: <wei>` pair. Where that pair is present the typed error SHOULD
carry both amounts so a user sees the cap they offered and the base fee
the chain required; the unit presented to the user is discretionary. A
`-32000` error whose text contains none of the three phrasings MUST NOT
be classified under this rule. The list is a floor: a further
node-dictated phrasing for the same condition MAY be added, with its
client and version range recorded in §56.3.

**R6.** *Fee fields of a gas-limit estimation request.* A gas-limit
estimation request issued for a transaction the daemon is about to sign
MUST carry the fee fields the transaction will be signed with, priced
from a fetch made for that transaction: when the swap gas-fee policy of
[Chapter 35](35-evm-v2-activation-rpcs.md) R35.6.6 is an EIP-1559 tier,
both caps of that tier's estimate (R1–R3 headroom included) and no
legacy price; when the policy is legacy, the legacy price. The same rule
already binds the withdraw path as R57.7.3; R6 extends it to the swap
and trade-preimage paths, where today only a legacy price is ever
supplied (see the code-quality finding below). An estimate or price
fetched for one swap step MUST NOT be reused for a later step; each step
fetches afresh. The trade-preimage path includes the fee previews of order
posting. The fee fields of an estimation request are therefore exempt from
the request-scoped reuse and the short cache of
[Chapter 57](57-evm-fee-inputs.md) R57.2.6. They are fetched for that
request even inside an order-posting request, whose other fee figures may
still reuse a reading. *(This exemption is a clean-side addition of
2026-10-30, maintainer Decision AZ.)* Rationale: a node that validates the cap at estimation
(§56.3) validates whatever the request carries, and a legacy price
taken from the node has no headroom at all, so one full block defeats it
— the mechanism of §56.1A.

> **Code-quality finding (informative, Reloaded's own tree).** Two
> estimation sites always price the estimation request with the legacy
> gas price of `eth_impl.rs:3194`, whatever the swap gas-fee policy is:
> - the swap-side gas-limit estimation helper at
>   `mm2src/coins/eth/eth_impl.rs:2623-2638`, which estimates the ERC-20
>   `approve` that precedes a V1 token payment and the approve figure of a
>   token's trade preimage;
> - the taker-fee preview's estimation at
>   `mm2src/coins/eth/eth_mm_coin.rs:302-313`.
>
> The estimation request shape at `mm2src/coins/eth/wire_types.rs:108-121`
> has no field for either EIP-1559 cap. Under an EIP-1559 tier policy the
> approve is then signed with the tier's capped pair
> (`eth_impl.rs:1255-1269`), while its gas limit was estimated under a
> different, headroom-free price. The swap payment, spend and refund use
> configured limits (R57.5.3a) and send no estimation request. *(Line
> references and the approve-only scope were refreshed on the clean side
> against `d369906c2`, whose EVM code equals dev `99b289817`.)* That is
> the contradiction with
> R57.7.3's rationale, and the arithmetic of §56.1A fits it exactly.
> Proposed fix: give the estimation request optional `maxFeePerGas` /
> `maxPriorityFeePerGas` members (dictated JSON names), fill them from
> the same tier estimate the signing path uses, and omit `gasPrice` in
> that case; keep the legacy member for the legacy policy. No typed
> fee-cap error exists in the tree yet, so the R5 variant must be added
> to the EVM coin layer's error types alongside. *(Trigger: a
> contradiction between two things read side by side — ch. 57 R57.7.3
> and the swap-path estimation helpers.)*

> **As-built note (2026-10-30, clean-side record).** PR #174 implements R5,
> R6 and D5 as decided.
> - **Classification (R5).** The three phrasings are recognised by
>   containment wherever the EVM coin layer reads a node's answer to an
>   estimation or a broadcast. The typed error carries the cap and the base
>   fee when the node quotes them, and the host-only node label of
>   [Chapter 35](35-evm-v2-activation-rpcs.md) R35.1.17.
> - **On the wire.** The withdraw, trade-preimage and maximum-maker-volume
>   RPCs report the typed error as the `error_type` `FeeCapBelowBaseFee`.
>   Its `error_data` gives the amounts as decimal wei strings. The HTTP
>   status is 500, as for a transport error.
> - **`send_raw_transaction`.** Its error is a plain string, so it reports
>   the same text.
> - **Fee fields (R6).** Every estimation request carries the fee fields
>   of the coin's swap gas-fee policy, fetched for it outside the request
>   scope and the short cache. The withdraw path carries the legacy price
>   (R57.7.2). The approve that precedes a V1 token payment is estimated
>   and signed with one fetch.
> - **Retry (D5).** One fresh retry applies to estimations only, and a
>   rejection discards the chain's cached gas price.
> - **Out of scope.** The NFT withdraw estimation is classified in the text
>   of its error only; it is not a typed variant there.

## 56.5 Tests

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

> *T4–T6 are the addendum of 2026-10-30.*

**T4.** *Fee-cap rejection classification (R5).* For each of the three
phrasings of R5, a node error constructed as the node would emit it —
the phrase embedded in a longer `-32000` message with the observed
*failed with N gas:* prefix and the trailing `maxFeePerGas` / `baseFee`
pair, and once more bare — MUST map to the typed fee-cap-below-base-fee
error, and where the pair is present the error MUST carry both amounts.
A `-32000` message of similar shape whose text names an unrelated
condition (for instance an insufficient-funds text) MUST NOT map to it.
The test is deterministic and needs no node.

**T5.** *Estimation request carries the signing fee fields (R6).*
Against a mock node that records request bodies: with an EIP-1559 tier
policy selected, the `eth_estimateGas` request issued for a swap step and
the one issued for a trade preimage MUST carry `maxFeePerGas` and
`maxPriorityFeePerGas` equal to the pair the subsequently signed
transaction carries, and no `gasPrice`; with the legacy policy they MUST
carry `gasPrice` only. The test MUST fail against the present tree. In
Reloaded's tree the swap-step request is the one for the ERC-20 `approve`
that precedes a V1 token payment; the payment itself uses a configured
limit (clean-side note).

**T6.** *The field symptom, end to end (R5, R6, D5).* Against a mock
node whose base fee rises by one-eighth between the daemon's price
query and its estimation call, and which rejects an estimation whose
cap (or legacy price) is below the new base fee with the Geth-family
text of R5: a swap step priced the legacy way MUST surface the typed
error of R5 (reproducing §56.1A, not a transport error); the same step
under an EIP-1559 tier with R6 in place MUST be accepted, because the
tier's headroom (R3) covers one block's rise. If D5 is resolved in
favour of a refresh-and-retry, the legacy-priced step MUST succeed on
the retry instead.

## 56.6 Upstream Divergence (informative)

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

*Addendum 2026-10-30.* Two further divergences,
stated as behaviour: (a) upstream recognises the fee-cap rejection only
on the withdraw gas-details surface, whereas R5 binds the recognition on
every path that presents a cap, because the §56.1A observation came
from a swap step; (b) upstream's gas-limit estimation requests carry the
fee fields of the selected gas policy (the legacy price or the EIP-1559
pair) — the behaviour R6 binds — which Reloaded's swap path does not yet
do. Both are Reloaded stating a requirement upstream meets only in part
or in another place, not a reproduction of upstream expression.

## 56.7 Deferred Work

**D1.** *Provider-side fee estimation.* Deriving the estimate from an
external gas-oracle / provider API instead of (or as a fallback to)
`eth_feeHistory` is out of scope for this chapter; only the
history-based simple estimator's base-fee selection is bound here.

**D2.** *Withdraw fee-cap error classification — resolved into R5
(addendum 2026-10-30).* When a transaction is
submitted with an explicit `maxFeePerGas` that is below the node's
current block base fee, strict EVM clients reject the request. Mapping
the client's textual rejection (the external-client error strings that
signal "fee cap below the block base fee" — see §56.6) into a typed,
user-meaningful "fee cap below base fee" error — rather than surfacing a
raw transport error — was originally deferred to the chapter porting the
withdraw gas-details surface. The §56.1A field report showed the same
rejection on a swap step, so the classification is now bound here as R5
for every path that presents a cap; the withdraw-specific fee-form work
remains with [Chapter 57](57-evm-fee-inputs.md) §57.7, which R5
cross-references. D2 is kept as the record of the earlier deferral.

**D5.** *Reaction to a typed fee-cap rejection (addendum 2026-10-30).* Once R5 yields the typed error, the daemon can
either fail the step with it or refresh the estimate (R1–R3, or the
legacy price) and retry the request once before failing. The proposed
default is one refresh-and-retry at the same tier, then the typed error:
the condition is by construction transient (one block of base-fee
movement) and a swap step that fails for it costs the user a trade.
Whether to retry, how many times, and whether a retry may step up one
tier are maintainer decisions and are not bound here; T6 states the
observable outcome for each resolution.

*Decided 2026-10-30 (maintainer Decision AZ, clean-side record):*
- **Estimation.** When a gas-limit estimation draws the typed error of
  R5, the daemon fetches the fee fields once more, fresh and at the same
  tier (or the same legacy source), and retries the estimation once. A
  second rejection fails the step with the typed error. T6's retry branch
  applies.
- **Broadcast.** A transaction a node refuses at broadcast for this
  reason surfaces the typed error. It is not re-signed or retried inside
  the send.
- **Tier.** The tier is never stepped up silently.
- **Cache.** As a detail of R6's exemption, a typed fee-cap rejection also
  discards the chain's cached gas-price reading of
  [Chapter 57](57-evm-fee-inputs.md) R57.2.6. The next order-posting
  request then reads the price fresh.

**D3.** *Reported `base_fee` versus Chapter 10 R36.* [Chapter 10](10-sse-streaming.md)
R36 binds the fee-estimation stream's `base_fee` field as the "next-block
base fee per gas". The simple estimator reports a different quantity in
that field: the 75th percentile of the `baseFeePerGas` window (the
percentile choice is discretionary, see R3), which is not the next-block
value under any reading. This chapter's R1 fix touches only the per-tier
anchor and leaves the reported `base_fee` unchanged. Reconciling the two
(either report the anchor, which is the next-block value on mainline
clients, or relax R36) is deferred and unresolved here.

**D4.** *Empty fee-history arrays: zero degradation versus typed error.*
An empty `baseFeePerGas` array degrades to a zero base-fee anchor, which
collapses the tiers to the tip alone, and an empty `reward` array likewise
yields a zero tip (R2). This behaviour is panic-free and deliberate: it was
kept over returning an error because both paths end in a rejected send and
the case needs a non-mainline client that omits the field. The open question
is whether to return a typed error instead, which would be more honest than
emitting a knowingly underpriced transaction. Doing so would amend R2; that
is a behaviour decision and not a re-gating matter, because both recorded
gate passes pre-date the adoption of R37-R44 and so, under R43, chapter 01 T1
does not reach this chapter. Deferred by maintainer decision on 2026-10-18.

## 56.8 Baseline Verifications

**V1.** *No EIP-1559 fee estimator at baseline.*

- *Claim.* The baseline tree contains no dedicated EVM EIP-1559
  fee-per-gas estimator module; this estimator (and therefore the
  base-fee selection bound in R1) is post-baseline work.
- *Verification.*
  `git ls-tree -r c1d46c0c1592faa0860f704008b2b2381bc3840f -- mm2src/coins/eth/fee_estimation/`
  returns the empty set.
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md).

## 56.9 External References

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

## 56.10 Provenance Footer

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
  informative divergence recorded in §56.6 (the bundled withdraw-path
  recognition of an external EVM-client "fee cap below base fee"
  rejection string). The corrected selection requirement (R1) and every
  dictated fact in §56.3 are sourced from the public EIP-1559
  specification and the public Ethereum JSON-RPC `eth_feeHistory` method
  definition (R3/R4); the external-client error strings named in §56.6
  are dictated by those third-party EVM clients (R4). None of this is
  the corpus's discretionary expression. No discretionary expression —
  no private type, field, or function names, no function bodies,
  control-flow transcription, helper decomposition, local names, error
  or log literals, or discretionary constants — was reproduced.
- *Dirty Gate clearance (chapter 01 T1, R43).* This chapter's
  forbidden-class-consulting material passed the KDF Dirty Gate in two
  passes, both dated 2026-06-19 to 2026-06-26, i.e. before the adoption of
  chapter 01 R37-R44 on 2026-10-11. The passes were recorded originally only
  in the commit messages of the revisions concerned, and this item transcribes
  them into the document set so the record survives any merge strategy. It is
  **not** a fresh clearance and no gate was run for it:
  - *First pass (2026-06-19):* the chapter as first written, **PASS**,
    discretionary-expression measure 0.12.
  - *Second pass (2026-06-26):* the parity revision that added §56.6 and
    deferred item D2 (and renumbered the sections after them), **PASS**,
    discretionary-expression measure 0.04.
  - *Measure:* as stated for each pass above.
  - *Extent:* not recorded beyond the revision each pass attached to, as
    named above; no finer statement of what was compared exists.
  - *Method:* not recorded in the document set.
  - *Temporal scope:* both passes pre-date the adoption, so under R43 T1 does
    not reach this chapter; the clearance is recorded as durable provenance
    and not because T1 compels it.
  - *Later edits:* the renumbering from 50 to 56, the cross-reference updates,
    and deferred item D3 (added when the chapter was brought onto the current
    branch) state Reloaded's own observations and take no forbidden-class
    input; they are not covered by either pass and need none.
- *Addendum of 2026-10-30, passed the KDF Dirty Gate on 2026-10-30
  (chapter 01 R40, T1).* The addendum comprises:
  - §56.1A;
  - the fee-cap bullet of §56.3;
  - R5, R6 and the code-quality finding beneath them;
  - T4–T6;
  - the closing paragraph of §56.6;
  - D2 as amended, and D5.

  It was authored by the KDF Spec Reader under the two-team procedure of
  §1.4A. Its inputs:
  - *Permitted classes:* R6 (a community maintainer's field report of
    2026-10-30 against a public Ethereum mainnet endpoint — the two
    amounts and the error code of §56.1A); R4 (the fee-cap validation
    behaviour and error phrasings of third-party EVM clients — Geth and
    its derivatives, Nethermind with the version boundaries 1.36.0 and
    1.38.0 — which those clients dictate); R1 (Reloaded's own tree at
    `d369906c2` for the code-quality finding and the §56.1A statement
    that R1 was already implemented there). No client publication is
    cited here for the two Nethermind version boundaries. They stand as
    the Spec Reader recorded them, and the same boundaries appear in §56.6,
    which the gate cleared on 2026-06-26. A later edit that checks them
    against the client's public release history should add that
    citation.
  - *Forbidden corpus, scoped record:* consulted on 2026-10-30, through a
    third party's reproduction of the upstream fix's change description
    and through the local lineage history, for four facts only: (i) that
    the upstream fix anchors on the trailing fee-history element and adds
    the pre-1.38.0 Nethermind phrasing to the recognised set — the same
    change the second gate pass of 2026-06-26 already covered; (ii) that
    upstream's recognition lives only on the withdraw gas-details surface
    (§56.6 (a)); (iii) that upstream's gas-limit estimation requests carry
    the selected gas policy's fee fields (§56.6 (b)); (iv) the lineage
    facts reported to the maintainer outside this chapter — that the
    estimator first appeared in a 2024 post-anchor revision, is present
    with the oldest-element anchor in the public releases `v2.6.0-beta`
    and `v3.0.0-beta`, and was not yet corrected in the v3 development
    lineage as held locally. Reference versions: `v2.6.0-beta`,
    `v3.0.0-beta`, and the v3 development lineage as consulted on
    2026-10-30. These public designations, as held in the local lineage,
    are as precise as the record goes. Nothing else was taken: the phrasings of R5 are client
    dictated (R4), the arithmetic of §56.1A is Reloaded's own, and no
    private identifier, body, helper decomposition or discretionary
    constant of the corpus was reproduced.
  - *Dirty Gate verdict and scope attestation (chapter 01 R39, R40):*
    **PASS** on 2026-10-30, discretionary-expression measure 0.04, with no
    finding in any R39 category.
    - *Extent:* the forbidden-class material within this chapter's own
      scope at the public releases `v2.6.0-beta` and `v3.0.0-beta` as held
      in the local lineage, which is as precise as the record goes. That
      material is the EVM EIP-1559 simple estimator, the fee-cap rejection
      classification surface of the withdraw gas details, and the
      construction of gas-limit estimation requests on the swap,
      trade-preimage and withdraw paths. It also covers one
      third-party-supplied reproduction of an upstream change
      description.
    - *Quantity:* two corpus source files read in five regions, about 400
      lines, plus that reproduction; 341 identifier tokens extracted.
    - *Method:* reading, plus a mechanical sweep of those tokens against
      the 232 added lines of the four files the addendum changed.
      `tools/clean_room_gate.py` was not run because the candidate has no
      code blocks.
    - *Measure:* 0.04, against the standard "no finding in any R39
      category and discretionary similarity materially below 0.10".
- *Clean-side edits after the verdict (2026-10-30; Reloaded-original, no
  forbidden-class input).* A session that never read the corpus wrote
  these from Reloaded's own tree at `d369906c2` (whose EVM code equals dev
  `99b289817`), the logged line of §56.1A and maintainer Decision AZ:
  - the correction note of §56.1A (an order-placement fee preview, not a
    swap step, and the second mechanism through the short cache);
  - the exemption sentence of R6 for the request scope and the short cache
    of chapter 57 R57.2.6;
  - the refreshed line references and the approve-only scope of the
    code-quality finding, and the matching note under T5;
  - the EIP-1559 arithmetic and the short-cache observation in the fee-cap
    bullet of §56.3;
  - the record of the maintainer's decision under D5;
  - the removal of the gate-pending markers once the verdict above was
    recorded.

  Chapter 01 R38 is not engaged by these edits, and they need no gate
  pass.
