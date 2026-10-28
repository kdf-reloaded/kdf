# Chapter 57 — EVM Fee and Amount Inputs: Legacy Gas-Price Resolution, Per-Coin Gas Limits, the Withdraw Fee Form, and the Minimum Trading Volume

**Status:** driving-spec (mixed treatment — see §57.0).

> **One-sentence claim:** the project shall resolve an EVM transaction's
> two fee inputs — the price per unit of gas and the gas limit — by the
> contract bound here: a legacy gas price taken as the maximum of two
> node-RPC sources and raised only *proportionally* (never by an absolute
> floor), a per-coin configuration block that overrides every default gas
> limit by named key, and a withdraw request fee form that admits both the
> legacy and the EIP-1559 shape and derives the signed transaction's
> envelope type from whichever was resolved — and shall report, as an EVM
> coin's minimum trading volume, one smallest representable unit of that
> coin (Part D, §57.7A), never a figure derived by dividing its decimals.

## 57.0 Treatment & scope split

This chapter is new and spans four adjacent surfaces of the EVM coin
layer. Reloaded ships part of each, so the chapter is split:

- **Part A (§57.2–§57.4), legacy gas-price resolution.** Mostly as-built
  in reloaded, with one bound correction (R57.3.2, removal of the
  absolute one-gwei increment floor) and two bound gaps
  (R57.2.4, R57.4.1).
- **Part B (§57.5–§57.6), per-coin gas-limit overrides.** Required and
  **not** in reloaded: the configuration block is never read and every
  gas limit is a compile-time constant. Since 2026-10-27 Part B also
  binds the one per-coin key that scales an *estimated* (not configured)
  gas limit, R57.5.3b, which reloaded likewise does not read.
- **Part C (§57.7), the withdraw fee form.** Partly in reloaded: the
  legacy fee form is accepted, the EIP-1559 fee form is not, and the
  envelope-type derivation that follows from the resolved form is absent.
- **Part D (§57.7A), the minimum trading volume.** Required and **not**
  in reloaded: reloaded still derives an EVM coin's minimum trading volume
  from its decimals by integer division — a pre-anchor baseline formula
  (V4) that the reference lineage replaced, at `v2.0.0-beta`, with one
  smallest representable unit of the coin. The order-match layer's
  *consumption* of the value is as-built baseline behaviour and is
  restated in §57.7A only so far as is needed to say where the value is
  observable. Part D **passed the KDF Dirty Gate** on 2026-10-24 (chapter
  01 T1, R40); see the clearance statement at the head of §57.7A and the
  second scoped-addition item of §57.12.

> **Binding scope (R57.1).** This chapter binds observable behaviour,
> coins-configuration key names and value shapes, JSON-RPC request field
> names and enum tags, the arithmetic by which a fee input is derived, and
> the numeric defaults a peer's fee quotes are computed from. It does not
> bind any private type, field, helper decomposition, or module layout;
> those are discretionary (chapter 01 R11, chapter 00 R6).

> **Why the numeric defaults are bound here.** A gas limit and a gas
> price together determine the fee figure a node publishes in
> `trade_preimage`, `get_trade_fee` and `withdraw` responses, and the gas
> field of the transaction it signs. Both are therefore observable at the
> external boundary from any node of the reference lineage without
> reference to its source, and both are overridable by a third-party
> coins-configuration file. They are bound as parity data, not as
> transcribed constants.

## 57.1 Reference designations

Facts in this chapter that describe the reference lineage are stated
against public release designations only:

| Designation | Role in this chapter |
| --- | --- |
| `v2.6.0-beta` | The legacy behavioural reference for netid `8762` (AGENTS.md §3). Every rule in this chapter that cites the reference holds at this designation unless stated otherwise. |
| `v3.0.0-beta` | The newest public release of the v3 lineage. Where this chapter notes "unchanged at `v3.0.0-beta`", the two designations agree. |
| `v2.1.0-beta` | Named only in R57.2.3, as the designation at which the third (external gas-oracle) price source had already been withdrawn. |
| `v2.0.0-beta` | Named only in Part D (§57.7A), as the first public release designation at which an EVM coin's minimum trading volume is one smallest unit of the coin and a maker order's price floor is "any positive value". Every later designation, including the two above, agrees. |

The section-number form `§57.1` refers to this section; the binding-scope
rule R57.1 lives in §57.0 above (rule numbers and section numbers run
independently in this chapter).

Where a behaviour is known only as "present in an unreleased state after
`v3.0.0-beta`", the chapter says so and the record goes no finer: no
post-anchor commit identifier or branch state is named (chapter 01 R20).

---

## Part A — Legacy gas-price resolution

## 57.2 The resolved legacy gas price

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §57.12, and are not restated here.

**R57.2.1** *Two sources, maximum wins.* The legacy gas price for an EVM
coin shall be resolved by querying the coin's node for both of:

1. the node's own gas-price suggestion, via the `eth_gasPrice` JSON-RPC
   method, used **unmodified**; and
2. the chain's recent base fee, via the `eth_feeHistory` JSON-RPC method,
   raised proportionally per R57.3.1 by **13 percent**;

and taking the **maximum** of whichever of the two produced a value. A
source that errors is dropped with a diagnostic and does not fail the
resolution. Each of the two queries is itself issued under the multi-node
access contract of [Chapter 35](35-evm-v2-activation-rpcs.md) §35.1.F, so
"the coin's node" above means whichever configured node answers first
under that contract, and a source counts as errored only after every node
has failed it (R35.1.11 there). If neither source produced a value,
resolution fails with an
internal error. The two queries shall be issued concurrently.

**R57.2.2** *Fee-history request and selection.* The `eth_feeHistory`
request for this source shall ask for a **block count of one**, a newest
block of *latest*, and an **empty reward-percentile list**. The base-fee
value taken from the response shall be the **first** element of the
response's `baseFeePerGas` array.

Under the method's dictated semantics (chapter 56 §56.3) a block count of
one yields a two-element array: the newest mined block's base fee, then
the next (pending) block's base fee. The first element is therefore the
**mined** block, not the pending one. This selection is retained
deliberately for parity: it is what the reference does at `v2.6.0-beta`,
unchanged at `v3.0.0-beta`, and unchanged in the later unreleased state
that corrects the *EIP-1559 estimator's* selection
([Chapter 56](56-evm-eip1559-fee-estimation.md) R1). The two call sites
are distinct and only the estimator's was corrected. A Reloaded change to
`.last()` here would be a divergence, not a fix, and is **not** bound.

> **Cross-reference.** [Chapter 56](56-evm-eip1559-fee-estimation.md) R1
> binds the *freshest*-base-fee rule for the EIP-1559 fee-per-gas
> estimator. R57.2.2 is the *other* `eth_feeHistory` consumer and
> deliberately differs from it. Neither rule is a defect in the other's
> terms: the estimator's anchor must not be stale because its output is a
> fee **cap** that a node will reject if it falls below the block base
> fee, whereas this source is only one input to a maximum that already
> includes the node's own suggestion.

**R57.2.3** *No third source.* Resolution shall not consult an external
gas-oracle HTTP service. No public release designation at or after
`v2.1.0-beta` carries such a source, and no entry of the public
third-party coins-configuration file configures one.

> **Code-quality finding (informative).** Reloaded still carries a third,
> first-consulted price source: an external gas-oracle HTTP endpoint
> configured per coin, whose value is raised proportionally by 10 percent
> and folded into the same maximum. No coin in the public coins
> configuration sets the endpoint, so the source is inert in practice and
> the resolved value matches R57.2.1 today. It is nonetheless a live code
> path that can only ever raise the resolved price above the reference's,
> and it carries its own percentage constant that exists nowhere in the
> reference. Removing the source (and with it the per-coin endpoint,
> decimals and policy configuration keys it reads) brings the surface to
> R57.2.1 exactly. *(Trigger: contradiction apparent between two things
> read side by side while establishing R57.2.1.)*

**R57.2.4** *Per-coin multiplier.* A coin's configuration entry may carry
a `gas_price_adjust` object that scales the fee estimates for that coin.
Its shape is dictated by third-party coins-configuration files and shall
be accepted verbatim:

| Key | Type | Applies to |
| --- | --- | --- |
| `legacy_price_mult` | number | The resolved legacy gas price of R57.2.1: the resolved value is multiplied by it as the final step of resolution. |
| `base_fee_mult` | array of exactly three numbers | The EIP-1559 estimator's per-tier base-fee headroom factor, replacing the built-in factor for the tier at the same position (low, medium, high). |
| `priority_fee_mult` | array of exactly three numbers | The EIP-1559 estimator's per-tier priority-fee factor, likewise per tier. |

All three keys are required when the object is present; a malformed
object shall fail coin activation with a configuration error. When the
object is absent the resolved price is used unscaled and the estimator
uses its own factors. For a token, the key shall be read from the token's
own entry, and when absent there, from its platform coin's entry
(R57.5.3).

This key is **not implemented in reloaded** and no entry of the public
coins configuration currently sets it, so adopting it changes no
deployed coin's behaviour today. It is bound because it is the reference's
only chain-relative gas-price mechanism and the sanctioned remedy for a
chain whose fee scale the shared defaults misprice.

**R57.2.5** *Nothing in gas-price resolution is chain-aware.* Resolution
carries no per-chain branch, no layer-two special case, and no
identifier-keyed table. Every chain-specific adjustment is configuration:
`gas_price_adjust` (R57.2.4), the per-coin swap gas-fee policy
([Chapter 35](35-evm-v2-activation-rpcs.md) R35.6), and the per-coin
envelope-type cap (ch. 35 D35.7). A Coder shall not introduce a
ticker-keyed or chain-id-keyed branch into this surface.

**R57.2.6** *Reuse within one request, and a short cache (Reloaded-original,
maintainer Decision R of 2026-10-27; passed the KDF Dirty Gate on 2026-10-27).* The resolved legacy gas price of
R57.2.1 is subject to the same rules as the fee estimate of chapter 38
R38.6.6j items 1–4, under the same `fee_estimate_cache_secs` key:
- an order-posting request resolves it at most once per coin, and uses that
  one value for the sender fee, the receiver fee and the maximum volume;
- order-posting requests may reuse it for up to the configured number of
  seconds;
- a swap's own transactions always resolve it fresh.
The resolution itself, with its two concurrent queries and its maximum, is
unchanged. *Rationale:* a `setprice` resolves the gas price twice today,
once for the sender fee and once for the receiver fee, and each resolution
makes its node queries one after another, contrary to R57.2.1.

> **Upstream divergence (informative; passed the KDF Dirty Gate on 2026-10-27).** The reference resolves the gas
> price separately for each consumer and caches nothing. This rests on the
> same relayed summary as chapter 38 R38.6.6j's note; the Dirty Gate verified
> it as accurate at both reference designations. **Compatibility:** for behaviour matching GLEEC KDF most
> closely, set `fee_estimate_cache_secs` to `0`.

> **As-built note (2026-10-27, clean-side record).** R57.2.1's concurrency and
> R57.2.6's first point are implemented in PR #114. The resolution now issues
> its queries together, and an order-posting request resolves the gas price
> once per coin, through the same request scope as chapter 38 R38.6.6j. The
> cache is not implemented yet.

## 57.3 The proportional-increase arithmetic

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §57.12, and are not restated here.

**R57.3.1** *The bound arithmetic.* Every proportional increase applied
to a gas price or to a fee cap in the EVM coin layer shall be, for a
256-bit unsigned value `v` and an integer percentage `p`:

```text
v + (v / 100) * p          -- integer division, truncating, applied first
```

That is: divide by one hundred with truncation, multiply the quotient by
the percentage, add to the original. No other rounding, no minimum, no
maximum. For a `v` below 100 the increment is exactly zero, which is
intended: the function is monotonic and never reduces its input.

**R57.3.2** *No absolute floor.* The increase of R57.3.1 shall **not**
carry an absolute lower bound on the increment. In particular it shall
not substitute a fixed one-gigawei increment when the proportional
increment is smaller than one gigawei.

The reference removed exactly such a floor; no public release designation
at or after `v2.6.0-beta` carries it, and `v3.0.0-beta` is unchanged.
Reloaded's floor is therefore a parity **defect inherited from
pre-anchor baseline code**, and removing it is a parity **fix** requiring
no compatibility switch and no `docs/COMPAT_SWITCHES.md` entry.

**Magnitudes.** The floor is unconditional on the magnitude of `v`, so
its effect is inversely proportional to the chain's fee scale. Taking the
13-percent fee-history source of R57.2.1:

| Base fee per gas | Correct (R57.3.1) | With a one-gigawei floor | Ratio |
| --- | --- | --- | --- |
| 30 gwei (mainnet-scale) | 33.9 gwei | 33.9 gwei | 1.0 — floor inactive |
| 7.7 gwei (floor threshold) | 8.70 gwei | 8.70 gwei | 1.0 — floor inactive at and above ≈7.70 gwei |
| 1 gwei | 1.13 gwei | 2 gwei | ≈1.8 |
| 0.1 gwei | 0.113 gwei | 1.1 gwei | ≈9.7 |
| 0.01 gwei (10 Mwei) | 0.0113 gwei | 1.01 gwei | ≈89 |
| 0.002 gwei (2 Mwei) | 0.00226 gwei | 1.002 gwei | ≈443 |

The floor becomes inactive once `(v/100)*13 ≥ 1 gwei`, i.e. at a base fee
of roughly 7.70 gwei and above; below that it dominates, and below about
0.01 gwei it sets the price almost single-handedly. A second application
through the stage approximation of R57.3.3 adds a further gigawei, so a
trade-preimage figure on a sub-gigawei chain can carry two whole gigawei
of pure floor. This is the arithmetic behind the reported ~100–400×
over-quote on sub-gigawei chains.

**R57.3.3** *Stage approximation.* A trade-fee or preimage figure may be
inflated by a stage-dependent percentage before it is reported or
reserved, using the arithmetic of R57.3.1 with these percentages:

| Stage | Percentage |
| --- | --- |
| no approximation | 0 (value returned unchanged) |
| swap start | 3 |
| order issue | 5 |
| trade preimage | 7 |

The inflation shall apply to the legacy gas price, and — when the coin's
resolved fee form is the EIP-1559 one — to the **maximum fee per gas
only**, leaving the maximum priority fee per gas untouched.

**R57.3.4** *Additional stages (gap).* The reference carries three stages
beyond those of R57.3.3: an *upper-bound* companion to the order-issue
stage and to the trade-preimage stage, which take the same percentage as
their base stage but additionally cause the sender-side gas limit to
include the refund leg (R57.5.2), and a *watcher-preimage* stage taking
**3 percent**. Reloaded has none of them. Adding them is required for the
reference's trade-preimage figures to be reproducible; it is recorded as
D1 rather than bound here because it also touches the trade-preimage
caller contract.

## 57.4 Which fee form a swap path uses

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §57.12, and are not restated here.

**R57.4.1** *The swap fee form comes from the policy, not from the legacy
price.* Every swap-side fee figure — the flat trade fee, the sender-side
preimage, the receiver-side preimage, and the DEX-fee preimage — shall be
computed from the coin's **resolved swap fee form**, obtained by applying
the coin's current swap gas-fee policy
([Chapter 35](35-evm-v2-activation-rpcs.md) R35.6): the legacy price of
R57.2.1 when the policy is the legacy one, and the corresponding tier of
the EIP-1559 estimate ([Chapter 56](56-evm-eip1559-fee-estimation.md))
when the policy names a priority tier.

Reloaded computes all four from the legacy price **unconditionally**,
never consulting the policy. This is a parity **gap**, and on a chain
whose configuration entry selects a priority tier it is the larger half
of the reported fee error: the public coins configuration selects the
medium tier for the Arbitrum, Base, Ethereum, Avalanche, Polygon, Gnosis
and several other platform coins, so for those coins the reference quotes
an EIP-1559 medium-tier fee where reloaded quotes a legacy price. The
one-gigawei floor of R57.3.2 then compounds it on the sub-gigawei members
of that set.

> **Operator note.** The two defects are independent and both must be
> closed for a sub-gigawei chain's quote to match the reference: removing
> the floor (R57.3.2) fixes the price *magnitude* on the legacy path, and
> consulting the policy (R57.4.1) selects the *right path* in the first
> place. Closing only the first still leaves Arbitrum and Base quoting
> from the wrong fee form; closing only the second still leaves the
> Binance Smart Chain, whose configuration entry caps the envelope type
> at zero and therefore always resolves to the legacy form, signing an
> over-priced transaction.

---

## Part B — Per-coin gas-limit overrides

## 57.5 The `gas_limit` configuration block

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §57.12, and are not restated here. Two later items — R57.5.3a and, in
> §57.8, T9 — were added after that pass and cleared by a second one; their
> record is the scoped-addition item of §57.12, and the derivation record
> above R57.5.3a carries the statement at the point they appear.

**R57.5.1** *The key and its shape.* A coin's configuration entry may
carry a `gas_limit` object whose members override the built-in gas limits
for that coin. Every member is optional and an absent member takes the
default of R57.5.2. The object's member names are dictated by
third-party coins-configuration files and shall be accepted verbatim; an
unknown member shall be ignored, and a member whose value is not a
non-negative integer shall fail coin activation with a configuration
error naming the key.

**R57.5.2** *Complete key set, call site, and default.* The nine
recognised keys, the operation each one prices, and the value used when
the key is absent:

| Key | Priced operation | Default |
| --- | --- | --- |
| `eth_send_coins` | A plain value transfer of the platform coin: the `withdraw` path and any internal send-to-address of the native asset. Also the figure deducted when a maximum-value native withdrawal is estimated, and the threshold reported when a gas estimate fails for insufficient funds. | 21000 |
| `eth_send_erc20` | An ERC-20 `transfer` call: the token `withdraw` path and any internal send-to-address of a token. | 210000 |
| `eth_payment` | The V1 HTLC **payment** call for the platform coin (sender locks funds). Also the sender-side trade-preimage gas for a platform coin. | 65000 |
| `erc20_payment` | The V1 HTLC **payment** call for an ERC-20 token. Also the base of the sender-side trade-preimage gas for a token, to which an `approve` gas estimate is added when the current allowance is below the traded value. | 150000 |
| `eth_receiver_spend` | The V1 HTLC **receiver spend** call for the platform coin. Also the receiver-side trade-preimage gas for a platform coin. | 65000 |
| `erc20_receiver_spend` | The V1 HTLC **receiver spend** call for an ERC-20 token. Also the receiver-side trade-preimage gas for a token. | 150000 |
| `eth_sender_refund` | The V1 HTLC **sender refund** call for the platform coin. Added to `eth_payment` in the upper-bound stages of R57.3.4. | 100000 |
| `erc20_sender_refund` | The V1 HTLC **sender refund** call for an ERC-20 token. Added to `erc20_payment` in the upper-bound stages of R57.3.4. | 150000 |
| `eth_max_trade_gas` | The flat trade-fee figure reported by the non-preimage trade-fee query, for the platform coin and for tokens alike. | 150000 |

> **Refund paths, stated separately because the failure mode is the
> worst in the system.** Exactly two keys price a V1 refund:
> `eth_sender_refund` for the platform coin and `erc20_sender_refund` for
> an ERC-20 token. Each is used at two distinct call sites — the refund
> transaction actually broadcast, and the upper-bound trade-preimage
> addition of R57.3.4 — and both sites take the same value. There is **no**
> separate key for a refund of a V1 *taker-fee* transaction, for a
> watcher-broadcast refund, or for a V2 refund; V2 refunds are priced by
> the separate block of R57.6. A refund must never be priced by a
> payment-path key: a payment-path default is lower than the refund
> default for the platform coin (65000 against 100000), so substituting
> one would under-fund the only transaction that recovers a stuck swap.

**R57.5.3** *Token inheritance.* For a token (ERC-20 or NFT), the
`gas_limit` key shall be read from the token's own configuration entry
and, when absent there, from the entry of its platform coin. The same
inheritance applies to `gas_limit_v2` (R57.6), `gas_price_adjust`
(R57.2.4), the envelope-type cap, the swap gas-fee policy default, and
the gas-estimate multiplier of R57.5.3b. For a platform coin, only its
own entry is consulted.

> **Dirty-side derivation record (sanitized).** R57.5.3a and T9 were
> added after this section's 2026-10-24 clearance and are **not** covered by
> it. They were authored by the KDF Spec Reader under the chapter-01 two-team
> workflow (§1.4A), from the GLEEC KDF public release designations
> `v3.0.0-beta` (reference) and `v2.6.0-beta` (legacy cross-check), which
> agree. Nothing else in §57.5 changed.
>
> **Clearance (chapter 01 T1, R40).** That material — R57.5.3a and T9 —
> **passed the KDF Dirty Gate** on 2026-10-24, in a second pass that left this
> section's earlier clearance untouched: PASS, discretionary-expression measure
> 0.01, finding list empty, and accordingly no remedy class for any line or for
> the candidate as a whole. Its clearance is therefore *complete* and not
> pending, and R40 withholds nothing in it. One limit of that pass belongs here
> and not only in the footer: the comparison reached material at the
> `v3.0.0-beta` designation only, so the agreement between the two designations
> that the record above asserts **is not attested by the clearance** and rests
> on the derivation record alone. The extent, method and measure of the pass
> are recorded once, in the scoped-addition item of §57.12, and are not
> restated here.

**R57.5.3a** *There is no ceiling on a configured gas limit: a configured value
is honoured verbatim.* A value supplied for any member of `gas_limit` or of
`gas_limit_v2` shall be used exactly as configured. It shall **not** be clamped
to a maximum, compared against a built-in upper bound, or rejected for being
large. Four points make the rule unambiguous, because each has been mistaken
for a ceiling:

- `eth_max_trade_gas` is **not** a ceiling on the other members. Despite its
  name it is one of the nine overridable members in its own right (R57.5.2),
  it prices exactly one thing — the flat trade-fee figure the non-preimage
  trade-fee query reports — and a configuration may set it above or below any
  other member with no interaction between them. The reference's own
  configuration-reading tests set it to a value that is not its default and
  assert it is honoured;
- the per-coin envelope-type cap (`max_eth_tx_type`,
  [Chapter 35](35-evm-v2-activation-rpcs.md) D35.7) caps the EIP-2718
  *transaction type*, never a gas limit, and the two are independent;
- the gas-estimate multiplier (R57.5.3b) scales a value **obtained from the
  node by estimation**; it neither bounds nor scales a value read from the
  configuration. A configured limit bypasses estimation entirely at the call
  sites of R57.5.2;
- the only rejection on this path is a **type** rejection. A member whose
  value is not a non-negative integer fails coin activation with a
  configuration error naming the key (R57.5.1). A member that is absent takes
  the default of R57.5.2. An unrecognised member inside the block is ignored
  (R57.5.1, and [Chapter 45](45-startup-configuration-and-environment-tolerance.md)
  R45.3.7 for the same tolerance at coin-entry level).

The consequence a Coder must not reverse: an over-large configured limit is a
configuration defect whose cost is an over-reserved gas allowance and an
over-stated trade fee, not a refusal. Clamping it silently would make two nodes
reading the same shared coins file build transactions with different gas
limits — a divergence invisible to both operators — and would do so in the
refund paths R57.5.2's call-out identifies as the worst place in the system to
under-fund. Rejecting it would make a coins file that a reference node accepts
refuse to activate here.

> **Dirty-side derivation record (sanitized), R57.5.3b and T13.** Added
> 2026-10-27 by the KDF Spec Reader under the chapter-01 two-team workflow
> (§1.4A), from the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02
> §2.15A, read at both and identical at both for every fact R57.5.3b
> states; the Reloaded workspace was read for the divergence note; the
> public coins-configuration file for the deployed values. Nothing else in
> §57.5 changed. **Clearance (chapter 01 T1, R40): passed the KDF Dirty Gate on 2026-10-28** (PASS, measure 0.02, no leaks); the record is the
> scoped-addition item of §57.12. The two earlier clearances of this section are untouched by it.

**R57.5.3b** *The gas-estimate multiplier (`estimate_gas_mult`).* This rule
closes former D3. A coin's configuration entry may carry a top-level
`estimate_gas_mult` member, a JSON number read as a double-precision
floating-point value, that scales a gas limit the node obtained **by
estimation** before the limit is used. Its complete contract:

1. *Reading and inheritance.* The member is read at coin activation, on the
   legacy and the version-two activation paths alike, under the same
   token-inheritance rule as `gas_limit` (R57.5.3): a token entry lacking the
   member takes its platform coin's value; a platform coin consults only its
   own entry. A member whose value is not a JSON number fails activation with
   a configuration error naming the key, exactly as a malformed `gas_limit`
   member does (R57.5.1).
2. *Default.* An absent member means **no scaling**: the estimate is used as
   returned. There is no built-in default other than this identity.
3. *No range validation.* No bound is checked at activation or at use. A
   value that has no finite decimal representation (not-a-number, positive or
   negative infinity) degrades to a factor of one at the point of use; a
   value of zero yields a zero gas limit; a negative value yields a product
   that cannot be represented as an unsigned gas quantity and fails the
   transaction at send time, not at activation. These are the observable
   outcomes; an implementation MUST NOT add an activation-time range check,
   because a coins file a reference node accepts must activate here.
4. *Arithmetic.* The estimate (a whole number of gas units) is multiplied by
   the configured value applied as the exact value of the parsed double, and
   the product is **truncated toward zero to a whole number of gas units**
   (no rounding up). The consequence an implementer must preserve: a decimal
   that is not exactly representable in binary (for example `0.7`) can
   produce a product one unit below the decimal arithmetic result; `1.3`
   applied to an estimate of `50000` yields `65000`.
5. *Scope — estimated limits only.* The multiplier applies to **exactly one
   class of transaction**: one whose gas limit the node obtains from
   `eth_estimateGas` because no fixed limit was supplied for it. At both
   reference rows there is **exactly one** such transaction on the swap
   path: the legacy (V1) ERC-20 `approve` the sender broadcasts before an
   ERC-20 HTLC payment when the current allowance is below the traded value
   (the `erc20_payment` row of R57.5.2 describes the payment this approve
   precedes). Every V1 HTLC payment, receiver spend and sender refund,
   every plain and token send, and every version-two transaction (fungible
   and non-fungible) supplies a fixed limit from the blocks of R57.5.2 and
   R57.6.2 and is **not** multiplied. **The multiplier never touches the
   R57.5.2 / R57.6.2 limits**, whether those come from the configuration or
   from the defaults (restating the third bullet of R57.5.3a from the other
   side).
6. *Not on the fee-quote path.* The `approve` gas estimate that the
   sender-side trade preimage adds to `erc20_payment` when the allowance is
   insufficient (R57.5.2) uses the **raw** estimate; the multiplier is not
   applied to it. A fee quote for a token whose platform sets the member
   therefore understates the gas the eventual approve transaction is
   allowed by the configured factor. This asymmetry is a reference fact at
   both rows and is bound as parity, not endorsed; see the code-quality
   finding below.
7. *Not on the withdraw path.* Withdrawals and the non-swap estimation
   requests of §57.7 obtain their gas limit by a separate estimation that
   never consults the member.
8. *Deployed values.* The public coins-configuration file sets the member to
   `1.3` on exactly two entries, both layer-two platform coins (an
   Arbitrum entry and a Base entry, each carrying the member as a
   top-level number); every ERC-20 token on those platforms inherits it by
   item 1. No token entry sets its own value.

> **Reloaded divergence (2026-10-27).** Reloaded reads neither
> `estimate_gas_mult` nor the per-coin `gas_limit` blocks and uses
> compile-time 150000-class constants for its V1 HTLC gas limits (R57.5.4,
> V3). Its legacy ERC-20 approve already obtains its gas limit by
> estimation (`mm2src/coins/eth/eth_impl.rs`, the approve constructor) and
> its sender-side preimage already adds the raw approve estimate
> (`mm2src/coins/eth/eth_mm_coin.rs`, the token preimage branch), so items
> 5–7 hold today with a factor of one. What is missing is the member
> itself: a Coder closing this rule adds the reading and inheritance of
> items 1–3 and applies the factor of item 4 to the approve's estimate at
> that one site — and nothing else, because R57.5.2 and R57.6.2 remain
> fixed limits and the preimage addition stays raw (item 6).

> **Code-quality finding (informative; trigger: a contradiction between two
> things read side by side).** The reference's own description of the
> multiplier's scope is wider than its behavior. Its documentation of the
> swap-transaction send path says the gas limit is obtained from the node
> when *either* no fixed limit is supplied *or* the multiplier is
> configured; the behavior is that a supplied fixed limit always wins and
> the multiplier is reached only when no fixed limit is supplied. An
> operator reading the reference's description would expect `1.3` to
> raise every swap transaction's limit on the two layer-two chains; it
> raises the approve's only. R57.5.3b binds the behavior (items 5–6), not
> the description. Proposed resolution for Reloaded: none in code — bind as
> above and document the key behaviorally in the operator-facing
> configuration reference, so that nobody sets it expecting R57.5.2 limits
> to scale. If the maintainer ever wants a *configured* way to raise every
> limit on a chain, that is the `gas_limit` block itself (R57.5.1), not this
> member.

> **As built (2026-10-28, PR #107) -- R57.5.3b.** `estimate_gas_mult` is read as a JSON
> number at every EVM activation path (the platform-coin build and both token routes, with
> the token's own value winning and a silent token inheriting its platform coin's; an explicit
> `1.0` on a token is a value, not an absence); any other JSON type fails activation naming
> the key; there is no range check. It scales only the estimated limit of the legacy ERC-20
> approve that precedes a V1 payment, as the exact product of the whole-number estimate and
> the exact binary value of the double, truncated toward zero (1.3 x 50000 = 65000, 0.7 x 10
> = 6; a non-finite factor is one, zero gives zero, a negative factor fails the transaction
> at send time before anything is broadcast). The trade-preimage's approve estimate, every
> fixed R57.5.2/R57.6.2 limit and withdraw stay unscaled. **Finding against item 5:** the
> version-two allowance step (`handle_allowance`) also broadcasts an approve whose limit is
> *estimated*, not fixed (the fixed `approve_payment` belongs to a different V2 call), so
> item 5's "every version-two transaction supplies a fixed limit" does not hold for it in
> Reloaded; it is left unscaled here, as the chapter's scope intends. The `approve_token`
> RPC is a non-swap call and is unscaled too.

**R57.5.4** *Implementation status (gap).* Reloaded reads neither
`gas_limit` nor `gas_limit_v2`. Its V1 gas limits are compile-time
constants inherited from the pre-anchor baseline, and they differ from
the defaults of R57.5.2:

| Operation | Reloaded (baseline constant) | R57.5.2 default |
| --- | --- | --- |
| native send | 21000 | 21000 — agrees |
| token send | 210000 | 210000 — agrees |
| HTLC payment, platform coin | 150000 | 65000 |
| HTLC payment, token | 150000 | 150000 — agrees |
| HTLC receiver spend, either | 150000 | 65000 / 150000 |
| HTLC sender refund, either | 150000 | 100000 / 150000 |
| flat trade fee | 150000 | 150000 — agrees |
| sender-side preimage, platform coin | 300000 (single constant) | 65000, or 165000 in the upper-bound stages |
| receiver-side preimage, either | 150000 | 65000 / 150000 |

Closing R57.5.1–R57.5.3 therefore also adopts the differing defaults.
That is the parity-correct move: the published reference values are
documented as carrying margin over measured on-chain consumption for the
same contract calls, and any chain needing more has the override. A
Coder closing this shall change the defaults and the override reading in
one change, not the override reading alone, and shall record the
sender-side preimage consolidation (one 300000 constant becoming
`eth_payment` plus a conditional `eth_sender_refund`) as part of it.

This gap is also recorded in
[`UPSTREAM-PARITY-GAPS.md`](UPSTREAM-PARITY-GAPS.md).

> **As built (2026-10-28, PR #105) -- R57.5.4 closed.** The `gas_limit` block is read
> at coin activation (token entry first, then its platform coin's whole block, R57.5.3),
> the defaults of R57.5.2 replace the baseline constants, and every V1 call site uses
> the coin's value: native and token sends, the maximum-native-withdrawal deduction and
> the insufficient-funds threshold (`eth_send_coins`, `eth_send_erc20`), the platform and
> token payment, receiver spend and sender refund calls, the flat trade fee
> (`eth_max_trade_gas`) and the sender-side and receiver-side trade preimages. A member
> that is not a non-negative integer fails activation naming the key, and a configured
> value is used verbatim (R57.5.3a). The single 300000 sender-preimage constant is now
> the payment limit plus the refund limit. **Deviation:** the fee-approximation stages
> have no upper-bound variants yet (D1), so every stage reserves the refund leg; the
> platform-coin sender preimage is therefore 165000 by default (the reference's payment-only
> figure would be 65000 outside the upper-bound stages), and the token figure is 300000 as
> before. This never under-funds the refund. The approve gas estimate goes through one
> function, the seam for the per-coin estimate multiplier of D3 (not applied).

## 57.6 The `gas_limit_v2` configuration block

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §57.12, and are not restated here.

**R57.6.1** *Shape.* A coin's configuration entry may carry a
`gas_limit_v2` object with up to three sub-objects, each itself a map of
optional integer members over the same absent-member-takes-the-default
rule as R57.5.1:

| Sub-object | Scope |
| --- | --- |
| `maker` | V2 maker-contract entry points for fungible assets. |
| `taker` | V2 taker-contract entry points for fungible assets, plus the token-allowance call. |
| `nft_maker` | V2 maker-contract entry points for non-fungible assets. |

**R57.6.2** *Members and defaults.*

| Sub-object | Member | Default |
| --- | --- | --- |
| `maker` | `eth_payment` | 65000 |
| `maker` | `erc20_payment` | 150000 |
| `maker` | `eth_taker_spend` | 100000 |
| `maker` | `erc20_taker_spend` | 150000 |
| `maker` | `eth_maker_refund_timelock` | 90000 |
| `maker` | `erc20_maker_refund_timelock` | 100000 |
| `maker` | `eth_maker_refund_secret` | 90000 |
| `maker` | `erc20_maker_refund_secret` | 100000 |
| `taker` | `eth_payment` | 65000 |
| `taker` | `erc20_payment` | 150000 |
| `taker` | `eth_maker_spend` | 100000 |
| `taker` | `erc20_maker_spend` | 115000 |
| `taker` | `eth_taker_refund_timelock` | 90000 |
| `taker` | `erc20_taker_refund_timelock` | 100000 |
| `taker` | `eth_taker_refund_secret` | 90000 |
| `taker` | `erc20_taker_refund_secret` | 100000 |
| `taker` | `approve_payment` | 50000 |
| `nft_maker` | `erc721_payment` | 130000 |
| `nft_maker` | `erc1155_payment` | 130000 |
| `nft_maker` | `erc721_taker_spend` | 100000 |
| `nft_maker` | `erc1155_taker_spend` | 100000 |
| `nft_maker` | `erc721_maker_refund_timelock` | 100000 |
| `nft_maker` | `erc1155_maker_refund_timelock` | 100000 |
| `nft_maker` | `erc721_maker_refund_secret` | 100000 |
| `nft_maker` | `erc1155_maker_refund_secret` | 100000 |

**R57.6.3** *Selection.* A V2 fungible call's gas limit is selected by
(asset kind: platform coin or token) × (side: maker or taker) × (method:
send, spend, timelock refund, secret refund). A V2 non-fungible call's
gas limit is selected by (token standard: ERC-721 or ERC-1155) × (the
same four methods), always from the `nft_maker` sub-object. Requesting a
fungible selection for a non-fungible coin type is an error, not a
fallback.

**R57.6.4** *Implementation status (gap).* Reloaded constructs its V2
gas-limit set unconditionally from built-in defaults and never reads
`gas_limit_v2`. Its own defaults differ materially from R57.6.2 — its
maker and taker payment and spend figures sit at 150000 and 250000 where
the reference's sit at 65000, 150000, 100000 and 115000 — and it carries
no `nft_maker` sub-object as a configurable unit. Closing this adopts
R57.6.2's values along with the override reading, on the same
one-change rule as R57.5.4.

> **As built (2026-10-28, PR #105) -- R57.6.4 closed.** The `gas_limit_v2` block is read
> with the sub-object and member names of R57.6.2 (`maker`, `taker`, `nft_maker`), inherited
> by a token as a whole block from its platform coin, and the V2 defaults are the table of
> R57.6.2. A V2 `approve_payment` is used as configured, with no estimate.

---

## Part C — The withdraw fee form

## 57.7 Withdraw fee resolution and the signed envelope type

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §57.12, and are not restated here.

**R57.7.1** *Accepted fee forms.* A withdraw request's optional fee
object is externally tagged by a `type` member. The EVM withdraw path
shall accept two tags and reject every other with a typed
invalid-fee-policy error naming the received tag:

| `type` | Members | Meaning |
| --- | --- | --- |
| `EthGas` | `gas_price` (decimal, gigawei), `gas` (integer) | Legacy pricing with an explicit gas limit. Both are taken as given; no estimation is performed. |
| `EthGasEip1559` | `max_priority_fee_per_gas` (decimal, gigawei), `max_fee_per_gas` (decimal, gigawei), `gas_option` (object, below) | EIP-1559 pricing. |

`gas_option` is a nested object, likewise tagged by a `type` member, with
two forms: a **`Calc`** form, `{"type":"Calc"}`, asking the node to
estimate the gas limit against the encoded call; and a **`Set`** form
carrying an explicit gas limit.

> **Serialization note.** All four tag values are the identifiers written
> above: both enclosing enumerations are tagged internally on a `type`
> member with no renaming applied, so the wire spellings are
> `EthGas`, `EthGasEip1559`, `Calc` and `Set` exactly. A Coder shall not
> lower-case or snake-case them.

**R57.7.1a** *The explicit-gas-limit nested form is not reachable over
JSON, and its encoding is an open decision.* The reference models the
`Set` form as a single unnamed integer payload under an internally tagged
representation. That combination has no valid JSON encoding under the
serialization framework both projects use: an internally tagged variant
whose payload is a bare integer can be neither produced nor consumed,
because the tag member and the payload cannot occupy the same JSON value.
**No wire encoding of the `Set` form therefore exists**, and none can be
derived from the reference.

Consequently:

- the `Calc` form, `{"type":"Calc"}`, is bound and MUST be accepted;
- the `EthGasEip1559` form MUST be accepted with `gas_option` set to the
  `Calc` form, which is the only form an RPC caller can send to a
  reference node today;
- the `Set` form's JSON encoding is **not** bound by this chapter. A
  Coder MUST NOT guess one. Choosing an encoding (the obvious candidate
  being a named member beside the tag) is a deliberate extension of a
  public request surface beyond what the reference accepts, and so needs
  a `docs/COMPAT_SWITCHES.md` entry and a `docs/GLEEC_COMPATIBILITY.md`
  note; it is recorded as D5.

> **Code-quality finding (informative).** The hazard above is a defect in
> the reference's request surface, not in reloaded's: a caller following
> the reference's own documented fee object cannot supply an explicit gas
> limit alongside EIP-1559 caps, and the attempt fails as a
> deserialization error on the whole request rather than as a typed
> fee-policy error. A reloaded implementation that mirrors the shape
> faithfully inherits the same dead variant. The honest minimum is to
> accept the `Calc` form, reject the `Set` form with a typed error that
> names the limitation rather than with an opaque parse failure, and
> treat adding a usable encoding as the deliberate divergence it is.
> *(Trigger: contradiction apparent between the bound request shape and
> the serialization rules that shape must satisfy, seen while
> establishing R57.7.1.)*

**R57.7.2** *Absent fee object.* When the request carries no fee object,
the withdraw path shall resolve the **legacy** price of R57.2.1 and then
estimate the gas limit (R57.7.3). It shall not consult the coin's swap
gas-fee policy: that policy governs swap-side figures (R57.4.1), not
withdrawals.

**R57.7.3** *Gas-limit estimation.* Where the resolved form does not
carry an explicit gas limit — the absent-fee case of R57.7.2 and the
`Calc` form of R57.7.1 — the gas limit shall be obtained from the node by
`eth_estimateGas` over the encoded call, with the already-resolved fee
fields supplied in the estimate request (a legacy gas price for the
legacy form, both EIP-1559 caps for the EIP-1559 form). Supplying them is
required, not optional: some deployed token contracts branch on the gas
price presented to them, and some strict EVM clients validate the
maximum fee per gas against the current block base fee during estimation.

**R57.7.4** *Envelope type follows the resolved form.* The signed
transaction's envelope type shall be derived from the resolved fee form —
untyped (type `0`) for the legacy form, type `2` for the EIP-1559 form —
and shall be checked against the coin's envelope-type cap
([Chapter 35](35-evm-v2-activation-rpcs.md) D35.7) **before** the
transaction is built. A resolved form above the cap shall fail with a
typed envelope-type-not-supported error. The transaction shall then be
built in that envelope type and signed with the coin's chain identifier.

**R57.7.5** *Applies to every EVM withdraw flavour.* R57.7.1–R57.7.4
bind the platform-coin withdrawal, the ERC-20 withdrawal, and the
non-fungible withdrawals of
[Chapter 19](19-nft-module-layout.md) §19.7 alike: all four resolve their
fee through the same contract, and the non-fungible flows differ only in
the call they encode and in always estimating their gas limit rather than
reading it from the configuration block of §57.5.

**R57.7.6** *Implementation status.* Reloaded accepts only the `EthGas`
form, has no `EthGasEip1559` form at all, always builds the untyped
envelope, and performs no envelope-type-cap check on this path (the cap
is enforced only on its EIP-1559 signing path, which no withdrawal
reaches). Closing this is a parity **gap** on a public request surface:
a caller that sends the EIP-1559 fee form today receives an
expected-`EthGas` error where the reference would honour it.

> **Code-quality finding (informative).** Reloaded's withdraw fee
> enumeration also lacks the sixth form the reference accepts, a
> Cosmos-family gas form carrying an integer gas limit and a
> floating-point gas price. The Tendermint withdraw path therefore cannot
> be driven by an explicit fee at all. This is outside this chapter's
> subject and is recorded for
> [Chapter 18](18-tendermint-ibc-htlc.md) / ch. 36 to pick up.
> *(Trigger: apparent while enumerating the fee-form tags for R57.7.1.)*

---

## Part D — The minimum trading volume

## 57.7A An EVM coin's minimum trading volume and minimum transaction amount

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section — every rule R57.7A.1–R57.7A.6 with the two
> informative notes beneath them, the tests T10–T12, the deferrals D7–D8
> and the matching footer items in §57.12 — was authored by the KDF Spec
> Reader under the two-team procedure of chapter 01 §1.4A, under the
> reference designations named in §57.1, and **passed the KDF Dirty
> Gate** on 2026-10-24 in a third pass that left the chapter's two earlier
> clearances untouched: PASS, discretionary-expression measure 0.01,
> finding list empty, and accordingly no remedy class for any line or for
> the candidate as a whole. Its clearance is therefore *complete* and not
> pending, and R40 withholds nothing in it. The extent, method and
> measure of the pass are recorded once, in the second scoped-addition
> item of §57.12, and are not restated here. V4 in §57.10 rests on the
> pinned baseline tree alone (R1) and was not part of the candidate.

> **As-built status (2026-10-25, clean-side record).** R57.7A.1,
> R57.7A.3 and R57.7A.6 are **implemented in PR #66**
> (`fix/evm-min-trading-volume`, to be merged after PR #57), from this
> cleared section alone: a shared helper in the coin-layer types computes
> one smallest unit exactly as a rational, and both the EVM and the QRC20
> market-operations `min_trading_vol` return it; `min_tx_amount` is
> unchanged at zero; no compatibility switch (R57.7A.5). The tests of
> T10–T12 are in the PR, with the as-built notes beneath T11 and T12
> recording what they pin and what they do not. D7's price-floor remedy
> was left out by maintainer decision. This note and the as-built notes
> below describe this project's own implementation, contain no
> forbidden-class material, and are not part of the cleared text; the
> cleared rules and tests above and below are unchanged.

*Why this section is in this chapter.* The minimum trading volume is a
per-coin property that the coin layer reports and the order-match layer
consumes. For UTXO coins the property is bound where the UTXO coin layer is
bound ([Chapter 38](38-utxo-coin-maintenance-and-features.md) R38.6.6); the
EVM coin layer's analogue belongs with the EVM amount inputs here. The
order-match chapters ([Chapter 11](11-order-match-cancellation.md),
[Chapter 12](12-order-match-state-store.md),
[Chapter 59](59-legacy-ordermatch-negotiation.md)) bind coin-agnostic
behaviour, and the consumption contract they would carry is unchanged by
this section; only the value an EVM coin feeds into it changes. *(Pointer,
2026-10-25: that cross-family consumption contract, and the per-family
value table, are now bound in
[Chapter 60](60-trading-floors-and-refusal-contract.md).)*

*The problem this section answers.* A field report describes a refused
purchase of roughly seven ten-thousandths of a unit of a Bitcoin-pegged
ERC-20 token carrying eight decimals, refused because the coin's minimum
trading volume was one hundredth of a unit — at the time of the report
roughly three orders of magnitude above the volume asked for. Reloaded
derives the value by the baseline formula of V4: ten to the power of minus
the integer quotient of the coin's decimals by three. For eight decimals
that is 10^-2; for eighteen decimals 10^-6; for six decimals 10^-2. The
formula has no recorded basis anywhere in the baseline or in this
document set.

**R57.7A.1** *One smallest unit.* An EVM platform coin's and an ERC-20
token's minimum trading volume shall be exactly one smallest representable
unit of the coin: ten to the power of minus the coin's configured
`decimals`. For an eighteen-decimal coin that is 10^-18; for an
eight-decimal token 10^-8; for a six-decimal token 10^-6. The value shall
be computed exactly (as a rational, not a floating-point number) and shall
not be rounded, clamped or raised by any other floor. No integer division
of the decimals, and no other quotient of them, shall participate.

**R57.7A.2** *Not configurable, not chain-aware.* The value is a function
of the coin's `decimals` alone. The reference lineage reads no
coins-configuration key for it, carries no per-chain or per-netid variant
of it, and the public coins-configuration file carries no such key on any
entry. The project shall not introduce a configuration key for it as part
of closing this gap; doing so would be a deliberate extension of the
public configuration surface and is D8's question, not this rule's.

**R57.7A.3** *The minimum transaction amount stays zero.* An EVM coin's
*minimum transaction amount* — the distinct per-coin accessor that the DEX
fee pipeline uses as its floor ([Chapter 08](08-fee-routing-engine.md)
R6, [Chapter 16](16-swap-v2-pre-burn-output.md) R10) and that the
maximum-taker-volume and maximum-maker-volume computations subtract and
compare against — is **zero** for EVM coins in the reference at every
designation of §57.1, before and after the change R57.7A.1 records, and
is zero in reloaded today. It shall stay zero. The two accessors are
therefore not interchangeable for EVM coins: the trading-volume floor is
10^-decimals and the transaction-amount floor is nil, so an EVM taker
coin's DEX fee is the rate times the amount with no absolute minimum, and
an EVM maker's or taker's maximum volume is bounded by balance and fees
alone. Chapter 08's note that the reference uses only the taker coin's
minimum-transferable amount as the fee floor is consistent with this: for
EVM coins that amount is zero.

**R57.7A.4** *Where the value is consumed — one accessor, every path.* The
reference applies a single per-coin value on every path below, and so
shall the project; no path carries its own EVM floor:

| Path | How the value is applied |
| --- | --- |
| Maker order creation (`setprice`) and maker order update | The order's maximum base volume must be at least the base coin's floor, and that volume times the price at least the rel coin's floor. The order's minimum volume, when the operator sets none, defaults to the larger of the base coin's floor and the rel coin's floor divided by the price; an operator-set minimum below that default is refused. |
| Taker order creation (`buy`, `sell`) | The base amount must be at least the base coin's floor and the rel amount at least the rel coin's floor; the taker order's minimum volume follows the same default-and-refusal rule as a maker order's. |
| `trade_preimage` (maker and taker previews) | A preview builds the same order the real request would and reports the same refusals with the same thresholds; the maker preview additionally reports the base coin's floor as the threshold when a zero volume is asked for. |
| `min_trading_vol` (legacy and version-2 methods) | Report the value verbatim as the coin's minimum trading volume. |
| `max_taker_vol`, `max_maker_vol` | **Not** consumers of this value; they consume the minimum transaction amount of R57.7A.3. |

The default-and-refusal arithmetic in the first two rows is as-built
pinned-baseline behaviour (V4) and is unchanged in the reference at every
designation of §57.1; it is restated here so that an implementer can tell
which observable refusals R57.7A.1 changes. It is bound by whichever
order-match chapter comes to own order validation, not by this one.

> **Amendment to R57.7A.5 (2026-10-25, Spec Reader; gated by the KDF
> Dirty Gate in five passes on 2026-10-25, PASS — see the third
> scoped-addition item of §57.12).** R57.7A.5 below gained two sentences, from "That
> same advertised minimum" to "no disagreement arises". They complete,
> and do not reverse, the rule as cleared on 2026-10-24: the floor is still
> applied only to orders the node itself creates and is still invisible to
> a peer except through the advertised `min_volume`, but the advertised
> minimum also governs which peer requests the maker answers, and the
> separate minimum transaction amount is a value both peers compute. PR
> #66's implementation of R57.7A.1–R57.7A.6 is unaffected: nothing it
> changed and nothing it left alone contradicts the added sentences. The
> cross-family form of this rule, and every refusal and failure point, is
> [Chapter 60](60-trading-floors-and-refusal-contract.md) §60.5.

> **As-built note (2026-10-25, PR #66) — the `min_trading_vol` response
> fields.** The fourth row binds the value, not the field names, which
> this chapter does not pin. As built, the legacy `min_trading_vol` method
> returns the value in the fields `min_trading_vol` and
> `min_trading_vol_fraction`; the version-2 method returns it in `amount`
> and `amount_fraction`. Recorded here so that a wallet reading either
> response has one place to look; it is not cleared rule text.

**R57.7A.5** *Observability and compatibility.* The value is local policy
at order creation: a node applies its own coins' floors to orders *it*
creates, and never applies them to a peer's order — the only validation a
received order record undergoes on this point is that its minimum volume
is non-negative and its price and maximum volume are positive
([Chapter 32](32-orderbook-p2p-and-trie.md) §32.5). The value is
nonetheless visible to other nodes in one place: when a maker sets no
minimum volume, the default of R57.7A.4 travels on the wire as the order
record's `min_volume` field (Chapter 32 R-W1) and is shown to every peer
in orderbook and best-orders responses. That same advertised minimum is
what the maker then applies to a peer's *request*: a taker request whose
matched base volume is below the maker order's minimum volume receives no
reserved message and no refusal of any kind
([Chapter 60](60-trading-floors-and-refusal-contract.md) R60.5.2), so a
peer that respects the advertised minimum is never refused on volume. The
minimum *transaction* amount of R57.7A.3, by contrast, is a value two
peers must agree on, because each computes the DEX fee floor from its own
figure for the taker coin (Chapter 60 R60.5.4); for EVM and QRC20 coins it
is zero on both sides, so no disagreement arises. Two consequences
follow:

- a reloaded node carrying the baseline formula advertises a minimum
  roughly a million times the reference's on an eight-decimal token
  (10^-2 against 10^-8) and refuses to take any reference maker's order
  below 10^-2 — while a reference node can take a reloaded maker's order
  of any size at or above the advertised minimum. The defect is therefore
  one-sided against reloaded's own users and does not break any peer;
- adopting R57.7A.1 makes a reloaded node indistinguishable, on this
  point, from a reference node at every designation of §57.1 on **both**
  production netids (the `v2.6.0-beta` and `v3.0.0-beta` behaviour is
  identical, so no `NetConfig` selection is involved). It is a parity
  **fix** of pre-anchor baseline behaviour, in the same class as R57.3.2,
  and the project's default-side rule (`docs/COMPAT_SWITCHES.md`) does
  not call for a compatibility switch to recover the baseline formula.
  The visible change in the `min_trading_vol` responses a wallet caches is
  a changelog matter, not a compatibility one.

**R57.7A.6** *The same rule for QRC20 tokens.* The QRC20 token layer in
reloaded derives its minimum trading volume by the same baseline formula
and sets the same zero minimum transaction amount, and the reference
changed it at the same designation to the same one-smallest-unit value.
R57.7A.1–R57.7A.5 therefore bind the QRC20 token layer alike. The rule is
stated here rather than in [Chapter 38](38-utxo-coin-maintenance-and-features.md)
only because the two layers share one defect and one remedy; D8 records
the placement question.

> **Code-quality finding (informative).** Reading the reference's change
> side by side with reloaded's order builders showed a second, adjacent
> baseline floor that the reference lifted at the same designation and
> reloaded still carries: a maker order's **price** must today be at least
> 10^-8 (one hundred-millionth), whereas the reference at and after
> `v2.0.0-beta` accepts any strictly positive price and refuses only zero
> and negative prices. The floor is coin-agnostic, so it is not bound here
> (D7). Its effect is the same shape as this section's: an order a
> reference node would place is refused locally, with no effect on any
> peer. A one-line remedy — refuse `price <= 0` instead of `price <
> 10^-8` — needs no compatibility switch for the reason R57.7A.5 gives.
> *(Trigger: an apparent contradiction between two things read side by
> side while establishing R57.7A.4.)*

> **Upstream divergence (informative).** None. The reference's behaviour
> on this surface is a numerical fact of the kind an operator of any
> reference node can read from its `min_trading_vol` response; nothing in
> this section describes how the reference computes it beyond the value
> itself.

## 57.8 Tests

**T1.** *Proportional increase, no floor.* For a 256-bit value `v` and
percentage `p`, the increase of R57.3.1 shall equal
`v + (v / 100) * p` computed with truncating integer division. Test at
least: `v` = 2 000 000 (2 Mwei) with `p` = 13 returning 2 260 000, which
MUST NOT be 1 002 000 000; `v` = 30 000 000 000 (30 gwei) with `p` = 13
returning 33 900 000 000; `v` = 50 with `p` = 13 returning 50. The test
MUST fail if any absolute increment is added.

**T2.** *Two sources, maximum.* Given stubbed sources where the node
suggestion is the larger, resolution returns the node suggestion
unchanged; where the raised fee-history value is the larger, resolution
returns that. Where one source errors, resolution returns the other.
Where both error, resolution returns an internal error.

**T3.** *Fee-history selection parity.* Given a two-element
`baseFeePerGas` array with distinct values, the value this source feeds
into R57.3.1 MUST be the **first** element (R57.2.2). This test exists to
stop a well-meant change to `.last()`; it is the mirror image of
ch. 56 T1 and the two MUST both pass.

**T4.** *Per-coin multiplier.* A coin configured with
`gas_price_adjust.legacy_price_mult` scales the resolved legacy price by
it as the last step; a coin without the key is unscaled. A
`gas_price_adjust` object missing any of its three keys fails activation.

**T5.** *Gas-limit override reading.* For each of the nine keys of
R57.5.2: a coin entry setting the key yields that value at the key's call
site, and a coin entry omitting it yields the stated default. A token
entry omitting a key inherits its platform coin's value; a token entry
setting it overrides the platform's (R57.5.3). At minimum the two refund
keys MUST be covered on both the broadcast-refund and the upper-bound
preimage call site.

**T6.** *V2 gas-limit override reading.* The selection matrix of R57.6.3
resolves to the defaults of R57.6.2 for an unconfigured coin, and to the
configured value for each sub-object member a coin entry sets.

**T7.** *Withdraw fee forms.* Driven from **JSON**, not from constructed
Rust values: a withdraw request carrying
`{"type":"EthGasEip1559","max_fee_per_gas":…,"max_priority_fee_per_gas":…,"gas_option":{"type":"Calc"}}`
builds a type-2 transaction whose caps are the request's and whose gas
limit came from the estimate; `{"type":"EthGas","gas_price":…,"gas":…}`
builds an untyped transaction with both values as given; any other tag is
rejected with the typed invalid-fee-policy error; a request whose
resolved form exceeds the coin's envelope-type cap is rejected before
signing (R57.7.4). A request carrying a `gas_option` that is not the
`Calc` form MUST be rejected with the typed error of R57.7.1a and MUST
NOT be silently treated as `Calc`.

**T8.** *Swap fee form follows the policy.* For a coin whose policy is a
priority tier, the sender-side and receiver-side preimage figures are
computed from the EIP-1559 estimate; for a coin whose policy is the
legacy one, from the legacy price (R57.4.1). The test MUST fail if the
legacy price is used under a priority-tier policy.

**T9.** *No clamp on a configured gas limit.* For at least one `gas_limit`
member and one `gas_limit_v2` member, a configured value far above every
built-in default (for example an order of magnitude above
`eth_max_trade_gas`'s default) MUST appear unchanged at the call site that
consumes it, and activation MUST succeed. A configured `eth_max_trade_gas`
below another member's configured value MUST leave that other member
unchanged. A member set to a negative number, a decimal, or a string MUST fail
activation with a configuration error naming the key (R57.5.3a). The test MUST
fail if any clamp, saturating comparison, or maximum is introduced on this
path. *(Cleared with R57.5.3a — see the clearance statement in the derivation
record in §57.5 and the scoped-addition item in §57.12.)*

**T10.** *One smallest unit, exactly.* For EVM coins constructed with
`decimals` of 18, 8 and 6, the reported minimum trading volume MUST equal
exactly 10^-18, 10^-8 and 10^-6 as rationals, and the reported minimum
transaction amount MUST be exactly zero (R57.7A.1, R57.7A.3). The test
MUST fail if the eight-decimal case returns 10^-2 — the value the baseline
formula yields — so that the integer-division formula cannot return
unnoticed. The same three cases MUST pass for a QRC20 token (R57.7A.6).
*(Cleared with §57.7A — see the clearance statement at its head and the
second scoped-addition item in §57.12.)*

**T11.** *The field report, end to end.* Against an eight-decimal ERC-20
token with the floor of R57.7A.1, a taker order, a maker order and a
maker and a taker trade preview for a volume of 0.00072437 of that token
(a rel-side amount and price such that neither side's floor is crossed)
MUST each succeed or fail for a reason other than a volume-below-minimum
refusal; the same four requests for a volume of 0.5 × 10^-8 MUST each be
refused with the typed volume-too-low refusal carrying 10^-8 as the
threshold. Both the legacy and the version-2 `min_trading_vol` methods
MUST report 10^-8 for that token. *(Cleared with §57.7A — see the
clearance statement at its head and the second scoped-addition item in
§57.12.)*

> **As-built note (2026-10-25, PR #66).** The "typed volume-too-low
> refusal" is, as built, the taker order builder's `BaseAmountTooLow` and
> the maker order builder's `MaxBaseVolTooLow`; both trade previews map
> them to `TradePreimageRpcError::VolumeTooLow` carrying 10^-8 as the
> threshold. The PR's test covers those two refusals and their mapping, the
> maker preview for a zero volume (the third row of R57.7A.4), and both
> `min_trading_vol` methods. **Not driven:** the end-to-end maker and
> taker preview for 0.00072437 on a real eight-decimal coin — the test
> coin lacks the balance and fee mocks it would need — and the maker
> order-update path, which shares the creation path's validator but is not
> separately tested. Those two remain open against this test.

**T12.** *Fee and maximum-volume paths are untouched.* For an EVM taker
coin, the DEX fee for a trade amount `a` MUST equal `a` times the pair's
rate with no absolute floor, and the maximum taker volume for an
available balance MUST be unchanged by R57.7A.1 — both depend on the
minimum transaction amount of R57.7A.3, not on the trading-volume floor.
The test MUST fail if either path begins to read the trading-volume
floor. *(Cleared with §57.7A — see the clearance statement at its head and
the second scoped-addition item in §57.12.)*

**T13.** *Gas-estimate multiplier (R57.5.3b).* Driven from JSON coin entries
against a stubbed node whose `eth_estimateGas` answer is a known whole number
`e`. (a) A platform coin entry setting `estimate_gas_mult` to `1.3` and a
token entry on that platform **omitting** it: the token's legacy `approve`
transaction carries gas limit `trunc(e × 1.3)`. (b) The same platform with
the token entry setting its own value `2.0`: the token's approve carries
`2e`, the platform's value is not used. (c) Neither entry sets it: the
approve carries exactly `e`. (d) With the member set on the platform, the
ERC-20 HTLC payment that follows the approve carries the `erc20_payment`
limit of R57.5.2 unchanged, and the sender-side preimage figure adds the
**raw** `e`, not `trunc(e × 1.3)`; a V2 payment on the same coin carries its
R57.6.2 limit unchanged. (e) A member value that is a string fails
activation with a configuration error naming the key; a member value whose
product with `e` has a fractional part is truncated, not rounded. The test
MUST fail if any R57.5.2 or R57.6.2 limit changes when the member is set.
*(Added 2026-10-27 with R57.5.3b; passed the KDF Dirty Gate on 2026-10-28 — see the
derivation record above R57.5.3b.)*

> **As-built note (2026-10-25, PR #66) — R57.7A.3 and T12.** As built,
> the DEX-fee pipeline's threshold is `max(net_cfg.dex_fee_min_threshold(),
> min_tx_amount)`. The network-level value is zero by the trait's default,
> so for an EVM taker coin the threshold is zero and the fee is the rate
> times the amount, as R57.7A.3 requires. A nonzero per-netid override of
> `dex_fee_min_threshold` would be the one place an absolute floor could
> re-enter this path; none exists today, and any future override must be
> weighed against R57.7A.3. The PR's test guards the DEX-fee half of this
> test; the maximum-taker-volume half is **not** guarded — the test as
> written cannot detect a future read of the trading-volume floor on that
> path, which stays open against this test.

## 57.9 Deferred Work

**D1.** *Upper-bound and watcher approximation stages.* R57.3.4's three
additional stages are not bound as a requirement here because adding
them changes the trade-preimage caller contract (every caller must choose
between a stage and its upper-bound companion) and interacts with the
reserved-funds rules of [Chapter 51](51-legacy-v1-swap-state-machine.md)
§51.8. Specify and implement them together with R57.5.4, whose
upper-bound addition they gate.

**D2.** *Reconciling R57.2.2 with chapter 56 R1.* The project now holds
two deliberately different `eth_feeHistory` base-fee selections, for
defensible reasons (R57.2.2). If the reference later corrects this call
site too, R57.2.2 must change with it and T3 must be inverted. Until
then the asymmetry is intentional and documented, not an oversight.

**D3.** *Gas-estimate multiplier — resolved 2026-10-27, bound as
R57.5.3b.* The reference carries a per-coin configuration key that scales a
node's `eth_estimateGas` result before use, and the public coins
configuration sets it to 1.3 for two layer-two platform coins. This entry
deferred it on the ground that a multiplier on an estimate needed its own
live-chain parity check. The verification that closed it found the scope far
narrower than the entry assumed: at both reference rows the key reaches
exactly one swap-path transaction (the legacy ERC-20 approve) and no fixed
limit of R57.5.2 or R57.6.2, so no live-chain check of payment, spend or
refund gas is implied by adopting it. R57.5.3b binds the complete contract;
T13 drives it. This entry remains as the record of the deferral and its
resolution.

**D4.** *Removing the external gas-oracle source.* The finding under
R57.2.3 proposes removing reloaded's third price source. It is deferred
rather than bound because removal also retires three per-coin
configuration keys, and a configuration key cannot be withdrawn without
checking that no deployed coins file sets it on either supported netid.

**D5.** *A usable encoding for the explicit-gas-limit nested fee form.*
R57.7.1a leaves the `Set` form's JSON encoding unbound because the
reference has none. Deciding one is a deliberate extension of a public
request surface and needs the compatibility-switch treatment named
there; it is deferred because the decision is the maintainer's, not a
Coder's, and because no caller can be using the form today.

**D6.** *Which shape rule governs the canonical section sequence.* This
chapter carries no section labelled *Executive Summary* and none
labelled *Subsystem Shape*; §57.0 does the scope-split work a shape
section would, which satisfies the canonical shape of chapter 00 R5 but
not the sequence chapter 01 R19 lists. The two meta-chapters disagree on
the point and set practice is split, so whether chapter 01 R19 or
chapter 00 R5 together with the prevailing practice is the defect of
record (chapter 01 R24) is a set-wide question that is recorded here and
not answered by this chapter; resolving it is a change to a meta-chapter
under chapter 01 R25–R26 and is the maintainer's, not a Coder's.

**D7.** *The maker price floor and the home of order validation.* The
finding under R57.7A.6 records that reloaded still refuses a maker price
below 10^-8 where the reference accepts any positive price. The floor is
coin-agnostic order-match validation, which no chapter of the set binds
today ([Chapter 59](59-legacy-ordermatch-negotiation.md) binds the
negotiation loop, [Chapter 12](12-order-match-state-store.md) the maker
store, [Chapter 32](32-orderbook-p2p-and-trie.md) the wire record). The
remedy is one line and carries no compatibility question (R57.7A.5), but
binding it — together with the default-and-refusal arithmetic R57.7A.4
restates — belongs to whichever order-match chapter comes to own order
validation. Until then this entry is the record. *(Cleared with §57.7A —
see the clearance statement at its head and the second scoped-addition
item in §57.12.)*

> **Pointer (2026-10-25; cleared with chapter 60 on 2026-10-25).**
> [Chapter 60](60-trading-floors-and-refusal-contract.md) R60.4.2 now
> binds the default-and-refusal arithmetic for every coin family,
> including the strictly-positive price rule as its step 4; this entry
> remains the tracking record for the one-line fix itself.

**D8.** *Placement of the QRC20 rule, and whether a per-coin override is
ever wanted.* R57.7A.6 binds the QRC20 token layer from an EVM chapter
because the defect and the remedy are shared; when
[Chapter 38](38-utxo-coin-maintenance-and-features.md) next revises its
Qtum material, R57.7A.6 should move there as a pointer back to R57.7A.1.
Separately, R57.7A.2 records that neither the reference nor the public
coins file carries a per-coin minimum-volume key; if operators ever want
one (for example to keep a token's dust-sized orders off their own
books), it is an extension of the public configuration surface and needs
the compatibility-switch treatment of `docs/COMPAT_SWITCHES.md` — a
maintainer's decision, not a Coder's. *(Cleared with §57.7A — see the
clearance statement at its head and the second scoped-addition item in
§57.12.)*

## 57.10 Baseline Verifications

**V1.** *The one-gigawei floor is pre-anchor baseline code.*

- *Claim.* The absolute one-gigawei increment floor of R57.3.2 exists in
  the pinned baseline tree; it is inherited, not introduced after the
  anchor.
- *Verification.* In the baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`, the EVM coin module's
  gas-price increase helper substitutes a fixed one-gigawei increment
  when the proportional increment is smaller, and the project's own
  revision history attributes it to a pre-anchor change dated 2020-06-10.
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md).

**V2.** *The V1 gas limits are pre-anchor constants.*

- *Claim.* Reloaded's V1 gas limits (21000, 210000, 150000 and the single
  300000 sender-preimage figure) are baseline constants, so the
  divergence table of R57.5.4 records inherited values and not
  post-anchor drift.
- *Verification.*
  `git show c1d46c0c1592faa0860f704008b2b2381bc3840f:mm2src/coins/eth.rs`
  contains those four literals at the native-send, token-send, HTLC and
  sender-preimage call sites, and contains no configuration-driven
  gas-limit block.
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md).

**V3.** *No per-coin fee-input key is read today.*

- *Claim.* Reloaded reads none of `gas_limit`, `gas_limit_v2`,
  `gas_price_adjust`, or the gas-estimate multiplier `estimate_gas_mult`
  of R57.5.3b.
- *Verification.* A search of `mm2src/` for each key name returns no
  configuration read; the V2 gas-limit set is constructed from its type's
  default at every construction site.

**V4.** *The integer-division volume floor and the 10^-8 price floor are
pre-anchor baseline code.*

- *Claim.* Both the EVM minimum-trading-volume formula R57.7A.1 replaces
  (ten to the power of minus the integer quotient of `decimals` by three)
  and the maker price floor of D7 (one hundred-millionth) exist in the
  pinned baseline tree, as does the zero EVM minimum transaction amount of
  R57.7A.3 and the default-and-refusal arithmetic R57.7A.4 restates. They
  are inherited, not introduced after the anchor, so closing the gap is a
  parity fix of baseline behaviour and not a reversal of a reloaded
  decision.
- *Verification.*
  `git show c1d46c0c1592faa0860f704008b2b2381bc3840f:mm2src/coins/eth.rs`
  contains the quotient-by-three formula in the EVM coin's
  market-operations implementation, next to a zero minimum transaction
  amount; `git show
  c1d46c0c1592faa0860f704008b2b2381bc3840f:mm2src/mm2_main/src/lp_ordermatch.rs`
  contains the one-hundred-millionth price floor in the maker-order price
  validation, and the minimum-volume default and refusal checks in the
  maker and taker order builders. The present reloaded workspace carries
  all four unchanged (the EVM formula in the EVM market-operations file,
  the QRC20 copy in the QRC20 market-operations file, the price floor and
  the builders in the order-match types file).
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md).

## 57.11 External References

- Ethereum JSON-RPC `eth_gasPrice`, `eth_feeHistory` and
  `eth_estimateGas` method definitions (request parameters and response
  shapes). Reference: `https://ethereum.org/developers/docs/apis/json-rpc`.
- EIP-1559 (fee-market change: per-block base fee, the one-eighth maximum
  per-block base-fee change, the maximum-fee / maximum-priority-fee
  pricing model).
- EIP-2718 (typed transaction envelope) and EIP-1559's type-`2` payload,
  for the envelope types of R57.7.4.
- The public third-party coins-configuration file, for the deployed
  `gas_limit` member sets (native-send, token-send, payment, receiver
  spend and sender refund members appear on Arbitrum, Binance-Smart-Chain,
  Polygon, Avalanche and Moonriver entries), the deployed
  `swap_gas_fee_policy` and envelope-type-cap values cited in R57.4.1,
  the two layer-two platform entries that set `estimate_gas_mult` to
  `1.3` (R57.5.3b item 8), and the absence of any `gas_price_adjust`
  entry.
- [Chapter 35 — EVM V2 Activation & Token RPC Surface](35-evm-v2-activation-rpcs.md)
  for the swap gas-fee policy (R35.6) and the envelope-type cap (D35.7).
- [Chapter 56 — EVM EIP-1559 Fee-Per-Gas Estimation](56-evm-eip1559-fee-estimation.md)
  for the estimator this chapter's R57.4.1 consumes and whose base-fee
  selection R57.2.2 deliberately differs from.
- [Chapter 19 — NFT Module Layout](19-nft-module-layout.md) §19.7 for the
  non-fungible withdrawal flows R57.7.5 binds.
- [Chapter 49 — Withdrawal task path](49-withdrawal-task-path.md) for the
  withdraw RPC surface the fee object of R57.7.1 travels on.
- The public release history of the historical lineage, for the release
  designations of §57.1 and the fact that `v2.0.0-beta` is a published
  release (Part D).
- The public third-party coins-configuration file, for the absence of
  any per-coin minimum-trading-volume key (R57.7A.2) and for the set of
  eight-decimal ERC-20 entries the baseline formula affects (Part D).
- [Chapter 32 — Orderbook Patricia-Trie and P2P Surface](32-orderbook-p2p-and-trie.md)
  §32.5 R-W1, for the `min_volume` wire field through which the default of
  R57.7A.4 becomes visible to peers (R57.7A.5).
- [Chapter 38 — UTXO Coin Maintenance & Features](38-utxo-coin-maintenance-and-features.md)
  R38.6.6, for the UTXO counterpart of Part D and the Qtum material D8
  concerns.
- [Chapter 08 — Fee Routing Engine](08-fee-routing-engine.md) R6 and
  [Chapter 16 — Swap V2 Pre-Burn Output](16-swap-v2-pre-burn-output.md)
  R10, for the minimum-transaction-amount accessor R57.7A.3 keeps at zero.
- `docs/COMPAT_SWITCHES.md`, "The default-side rule", for the
  compatibility assessment of R57.7A.5.

## 57.12 Provenance Footer

- *Inputs:* the public Ethereum JSON-RPC method definitions and the
  EIP-1559 / EIP-2718 specifications (the fee-source semantics of §57.2
  and the envelope types of §57.7); the public third-party
  coins-configuration file (the deployed `gas_limit` member sets, the
  per-coin swap gas-fee policy and envelope-type-cap values, and the
  absence of a `gas_price_adjust` entry, and the absence of any
  minimum-trading-volume key together with the eight-decimal ERC-20
  entry set of Part D); the pinned baseline tree (V1, V2, V4); the
  present reloaded workspace (V3, V4 and every implementation-status
  statement); the public release history of the historical lineage (the
  designations of §57.1); a field report from a community maintainer, as
  the statement of the problem Part D answers — a third-party report of
  observed behaviour, carrying no implementation material.
- *Permitted-input classes used:* R3 (external public specifications —
  EIP-1559, EIP-2718), R4 (wire formats and external interfaces the
  project must inter-operate with — the three JSON-RPC methods, the
  coins-configuration key schema, and the orderbook wire record's
  `min_volume` field cited from chapter 32), R1 (the baseline tree, for
  V1, V2 and V4), R6 (public-release facts and the field report of
  Part D), and the project's own tree and revision history.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* consulted **only** to establish, for §57.2–§57.7
  and under the reference designations named in §57.1: that the absolute
  one-gigawei increment floor is absent and the proportional arithmetic of
  R57.3.1 is what remains; that gas-price resolution takes a maximum over
  exactly the two node-RPC sources of R57.2.1 with the thirteen-percent
  raise and the first-element base-fee selection of R57.2.2; the stage
  percentages of R57.3.3 and the existence (not the naming) of the three
  further stages of R57.3.4; that swap-side figures are computed from the
  policy-resolved fee form (R57.4.1); the complete recognised key sets,
  per-key call sites, absent-key defaults and token inheritance of
  §57.5–§57.6, together with the absence of any ceiling, clamp or
  upper-bound comparison on a configured value (R57.5.3a, cleared under the
  scoped-addition item below); the withdraw
  fee-form tags, member names,
  estimation rule and envelope-type derivation of §57.7; and, for §57.7A
  and under the designation `v2.0.0-beta` as the first and `v2.6.0-beta`
  and `v3.0.0-beta` as the agreeing later designations: that an EVM
  coin's and a QRC20 token's minimum trading volume is one smallest unit
  of the coin and its minimum transaction amount zero, that no
  configuration key feeds either, that the maker price floor is "any
  positive value", that a received order record is checked only for a
  non-negative minimum volume and positive price and maximum volume, and
  which public request paths consume which of the two accessors
  (R57.7A.1–R57.7A.6, cleared under the second scoped-addition item
  below). Every dictated
  fact in §57.2's method semantics and §57.7's envelope types is sourced
  from the public specifications (R3/R4); every configuration key name,
  value shape and deployed value is sourced from the public coins
  configuration (R4). No discretionary expression — no private type,
  field or function name, no function body, control-flow transcription,
  helper decomposition, local name, error or log literal — was
  reproduced. The numeric defaults and percentages are bound as parity
  data on the justification stated in §57.0 and are observable at the
  external boundary of any node of the reference lineage.
- *Scoped addition of 2026-10-27 — R57.5.3b and T13 (passed the KDF Dirty Gate on 2026-10-28).*
  **R57.5.3b and T13**, together with the resolution text of D3, the
  R57.5.3b pointers substituted for "D3" in R57.5.3, R57.5.3a and V3, the
  one-sentence additions to the Part B entry of §57.0 and the coins-file
  item of §57.11, and this footer item, were authored by the KDF Spec
  Reader under §1.4A from the rows `v2.6.0-beta` and `v3.0.0-beta` of
  chapter 02 §2.15A, read at both and identical at both for every fact
  stated. The facts taken from the forbidden class are: the key's
  floating-point reading at every EVM activation path under the same
  token-inheritance rule as `gas_limit`; the identity default; the absence
  of any range validation and the degradation of a non-finite value to a
  factor of one; the multiply-then-truncate arithmetic on the exact value
  of the parsed double; that the factor reaches exactly one swap-path
  transaction — the legacy ERC-20 approve whose limit is estimated — and
  no transaction that supplies a fixed limit; that the preimage's approve
  estimate is raw; that the withdraw path never consults it; and that the
  reference's own description of the key's scope is wider than its
  behavior (the code-quality finding). The deployed values are from the
  public coins-configuration file (R4) and the divergence note from this
  workspace. No private identifier, function body, control-flow
  transcription, helper decomposition, log or error literal was
  reproduced. **Dirty Gate clearance (chapter 01 T1, R40): PASS on 2026-10-28**, measure 0.02, no leaks; every
  reference fact was confirmed at both rows. *Extent:* the EVM coin-activation configuration-reading
  surface and the swap-transaction gas-limit and estimation surface, at `v2.6.0-beta` and `v3.0.0-beta`
  (the compared regions identical), plus the public coins-configuration file (not corpus). *Method:*
  reading of the candidate hunks against the reference regions at both rows, and a mechanical regex
  comparison of every added line against identifier, literal, comment-phrase, path and
  version-control-identifier lists; the mechanical gate tool was not run (prose). *Measure:* 0.02 against the
  0.10 threshold. The two clearance items below are neither reopened nor re-dated by it.
- *Scoped addition, and its Dirty Gate clearance (chapter 01 T1, R40, R43).*
  **R57.5.3a and T9** were added after the gate pass recorded below and are
  **not** covered by it: they bind that a configured per-coin gas limit carries
  no ceiling, is never clamped and is rejected only on a type failure. They
  were taken from the same forbidden-class surface as §57.5, under §1.4A, at
  the release designations `v3.0.0-beta` and `v2.6.0-beta`. They were authored
  by the KDF Spec Reader under that procedure and **passed the KDF Dirty Gate**
  on 2026-10-24 in a second pass, scoped to these two numbered items and to the
  footer changes that accompany them: PASS, discretionary-expression measure
  0.01, finding list empty, and accordingly no remedy class for any line or for
  the candidate as a whole. Their clearance is therefore **complete and not
  pending**, and R40 withholds nothing in them, as the derivation record in
  §57.5 states at the point the material appears. The clearance item below is
  unchanged by this pass and keeps its own date and scope: the sections it
  covers were not re-examined here and are neither reopened nor re-dated. This
  item is written on the clean side from the verdict, which reached the clean
  side as relayed text rather than as a gate-written file; that is
  record-keeping and adds no chapter content.

  *Which parts carry an inline pointer to this item.* The derivation record
  that sits immediately above R57.5.3a carries the clearance statement, and T9
  carries a one-line pointer back to it — the same two places the pending
  statements stood, converted where they were. Both items are numbered, rule-
  or test-bearing text an implementer codes from, inserted among R- and
  T-numbers that have stood in this chapter since earlier revisions, which is
  the case in which long-settled requirement and newly-cleared requirement are
  easiest to confuse. §57.5's section-head block gained one sentence naming
  this item, because that block would otherwise read as though a single pass
  covered the whole section. The six section-head pointers the clearance item
  below describes are otherwise left as they are: they belong to that earlier
  pass, and this one neither adds to nor widens them.

  The three elements of this pass's R39 scope attestation, carried forward here
  in corpus-free terms as chapter 01 T1 requires. One of the three is
  incomplete, and it is recorded as a gap rather than completed from elsewhere:

  - *Extent.* The per-coin gas-limit configuration surface and its
    consumption: the two configuration blocks' members as configured values;
    the absence of any upper-bound comparison, clamp or saturating maximum on a
    configured value anywhere on that path; the sole type-level rejection at
    activation; the one member whose name invites confusion with a ceiling,
    together with the single figure it prices; the per-coin
    transaction-envelope-type cap; and the multiplier applied to an estimated
    rather than a configured value. Quantity examined: **four files** read in
    substance, together with token-level searches across the relevant
    subsystem's file set, of the order of ten files. Reference designation: the
    publicly published release designation `v3.0.0-beta`. **The record is no
    more precise than that designation** — no post-anchor commit identifier, no
    branch state and no location within the compared material is given
    (chapter 01 R11 and the reference-version limit of §1.7).

    **The gap in this element, left visible rather than completed.** The scoped
    record above names two designations, and the derivation record in §57.5
    asserts that they agree. This clearance reached material at one of them
    only: no material at the `v2.6.0-beta` designation was available to the
    pass, so **the asserted agreement between the two designations is not
    attested here**. The element is incomplete rather than approximated, and
    chapter 01 T1 is satisfied by saying so — a completed clearance statement
    carries the three elements **or** states that an element is not recorded.
    Nothing in this clearance supports the legacy-cross-check half of that
    record: it rests on the Spec Reader's record alone, and this item neither
    confirms nor contradicts it. Whether either designation is a publicly
    published release designation of the kind R11's carve-out admits is a
    separate question, checkable on the clean side against public release
    records, and is not answered here.

  - *Method.* **Combined.** Reading, together with a mechanical token
    comparison over the tokens extracted from a line-level comparison of this
    chapter against the branch tip — only added and modified lines were judged
    — each then put through the forced-versus-free test individually; and with
    targeted searches used to test the addition's negative claim, that no
    bound, clamp or maximum exists on the path, across the subsystem rather
    than at a single site. No aggregate match-density judgement was used or
    admitted. **This repository's own R35 gate tooling under `tools/` was not
    applicable and was not run:** the added material embeds no code block and
    that tooling's test is defined over source-file bodies, so the instrument
    had no operand. The absence is stated so that it is visible rather than
    implied.

  - *Measure.* **0.01** on the discretionary-expression basis — the same figure
    the leak report carries, not a second one — read against the same standard
    the chapter's earlier sections were read against at their own 0.02: one
    item of genuine discretionary expression fails a candidate however much
    legitimately dictated content surrounds it. All four quoted tokens on the
    added lines are configuration keys of the shared public coins file and were
    excluded from the measured set as dictated; no default value, internal type
    name, helper name or message literal was carried. With the finding list
    empty, no added line was classified into any leak category.
- *Second scoped addition — Part D, and its Dirty Gate clearance (chapter
  01 T1, R40, R43).* **§57.7A (R57.7A.1–R57.7A.6, the two informative
  notes beneath them), T10–T12, D7–D8,** the Part D bullet of §57.0, the
  `v2.0.0-beta` row of §57.1, the Part D additions to this footer's
  *Inputs*, *Permitted-input classes* and *Forbidden corpus* items, and
  the title and one-sentence-claim extension were added after both passes
  recorded in this footer and are covered by **neither** of them. They
  were taken from the forbidden-class surface that reports a coin's
  minimum trading volume and minimum transaction amount and from the
  order-match validation that consumes them, under §1.4A, at the release
  designations `v2.0.0-beta`, `v2.6.0-beta` and `v3.0.0-beta`, and were
  authored by the KDF Spec Reader under that procedure. They **passed the
  KDF Dirty Gate** on 2026-10-24 in a third pass, scoped to exactly that
  material: PASS, discretionary-expression measure 0.01, finding list
  empty, and accordingly no remedy class for any line or for the candidate
  as a whole. That was the first gate pass on this material; no earlier
  verdict for it exists or was relayed. Their clearance is therefore
  **complete and not pending**, and R40 withholds nothing in them, as the
  clearance statement at the head of §57.7A states at the point the
  material appears. V4 is not part of the candidate: it rests on the
  pinned baseline tree and the present reloaded workspace alone. The two
  items above and the completed item below are unchanged by this pass and
  keep their own dates and scopes; the gate confirmed that the only change
  to the completed item below is its scope phrase, from one scoped
  addition to two, so the substance of the two earlier attestations is
  intact and not reopened. This item is written on the clean side from
  the verdict, which reached the clean side as relayed text rather than
  as a gate-written file; that is record-keeping and adds no chapter
  content.

  *Which parts carry an inline pointer to this item.* The block at the
  head of §57.7A carries the clearance statement, and T10–T12 and D7–D8
  each carry a one-line pointer back to it — the same places the pending
  statements stood, converted where they were. The Part D bullet of
  §57.0 names the clearance in one sentence. The one-line pointers to
  §57.7A in chapters 32, 38 and 59, which carried the pending marker as
  dependent text, were updated in the same change, as was entry #12 of
  the parity-gap register.

  The three elements of this pass's R39 scope attestation, carried
  forward here in corpus-free terms as chapter 01 T1 requires. Two gaps
  in the first element are recorded as gaps rather than completed from
  elsewhere:

  - *Extent.* The EVM platform-coin and ERC-20 token coin-layer
    minimum-trading-volume and minimum-transaction-amount accessors, the
    QRC20 token layer's counterparts, and the order-match maker and taker
    order-builder validation that consumes them — Part D's scope as §57.0
    states it. Quantity examined: **three files** of that surface at the
    lineage's current state, and the same three at the pre-anchor
    baseline chapter 02 names, to verify V4's baseline-existence claims;
    the single merged change that moved the value to one smallest unit
    and lifted the price floor, by its file inventory (**11 files**) and
    its diff for the order-match file; and tag-containment history
    establishing `v2.0.0-beta` as the first published release
    designation carrying it, with `v2.6.0-beta` and `v3.0.0-beta`
    agreeing. Reference designations: the three publicly published
    release designations §57.1 names, which the lineage's own releases
    carry and which chapter 01 §1.7 admits as public-release facts. **The
    record is no more precise than those designations** — no post-anchor
    commit identifier, no branch state and no location within the
    compared material is given (chapter 01 R11 and the reference-version
    limit of §1.7).

    **Two gaps in this element, left visible rather than completed.**
    First, the lineage's current unreleased state was compared; it is a
    branch state, which chapter 01 R11 and §1.7 withhold, so the record
    says only that the three named designations agree and nothing about
    where the tip stands. Second, one further release tag also contains
    the change, but its name embeds a commit abbreviation, which is an
    identifier of the withheld class; it is not named here. Chapter 01 T1
    is satisfied by saying so — a completed clearance statement carries
    the three elements **or** states that an element is not recorded.

  - *Method.* **Reading**, combined with mechanical history queries —
    content search, tag containment and retrieval of the files at the
    baseline anchor. **The repository's own R35 gate tooling was not
    run:** it operates on files of source and the added material embeds
    no source-code block, so the instrument had no operand. The absence
    is stated so that it is visible rather than implied.

  - *Measure.* **0.01** on the discretionary-expression basis — the same
    figure the leak report carries, not a second one — read against the
    standard that any single line carrying discretionary expression fails
    the candidate however much legitimately dictated content surrounds
    it; none was found, and no aggregate match-density judgment was used
    or admitted. Excluded from the measured set, so that the figure's
    coverage is visible: the default-and-refusal arithmetic R57.7A.4
    restates and the maker price-floor remedy form in the code-quality
    finding, both verified as pre-anchor baseline behavior (R1); and the
    one-smallest-unit value and the "any positive price" rule, which are
    numerical behavior observable through public RPC responses.
- *Third scoped addition — the R57.7A.5 amendment and the chapter-60
  pointers (2026-10-25).* The two sentences added to R57.7A.5, the
  amendment note above it, and the two chapter-60 pointers (in the
  "Why this section is in this chapter" paragraph and beneath D7) were
  authored by the KDF Spec Reader under the two-team procedure at the
  designations of §57.1. Corpus-sourced: that a maker answers no taker
  request below its order's minimum volume, and that each peer computes
  the DEX fee floor from its own minimum-transaction-amount figure.
  The addition was gated by the KDF Dirty Gate as part of the chapter-60
  candidate in **five passes on 2026-10-25** (pass 4 covered only the
  changes made after pass 3, none of them in this addition; of the
  changes pass 5 reviewed, the only one here renamed a baseline
  reference in this item's method to the chapter 02 row name), each
  returning PASS,
  discretionary-expression measure 0.02, finding list empty, and
  accordingly no remedy class for any line or for the candidate as a
  whole. The 2026-10-24 clearance of the rest of Part D is unaffected.
  The clearance of this addition covers the extent below, subject to
  stated gaps (a)–(c); gap (d) is closed. The full attestation is kept in
  [Chapter 60](60-trading-floors-and-refusal-contract.md) §60.12. The
  scope attestation, in corpus-free terms:
  - *Extent.* Pass 1: the trading-floor subsystem as chapter 60
    describes it, including the maker's test of an incoming taker
    request, which is the ground of the added R57.7A.5 sentences,
    compared in full at `v3.0.0-beta` and cross-checked at `v2.6.0-beta`
    for the floor rule and the configuration classification, with four
    public issue-tracker items read. Pass 2: the first-appearance
    history of the UTXO material and all clearance-record text (chapter
    60 §60.12); nothing in this addition depends on it.
  - *Stated gaps.* (a) No later unreleased v3 state was compared in any
    pass. (b) The per-family legacy taker-fee validation and the
    version-two funding check, on which the "both peers must agree"
    sentence rests, were checked by token inspection only, not side by
    side, although pass 3's mechanical sweep covered them too. (c) The
    `v2.0.0-beta` first designation of §57.1 was not compared in any
    pass; it rests on this chapter's own earlier clearance (the second
    scoped-addition item above). (d) That only one rule-text line changed
    after pass 1 — closed by pass 3.
  - *Method.* Side-by-side reading; a mechanical token search (in pass 2
    also a count across the designations compared); a per-token
    forced-versus-free test of every code-formatted token against the
    compared material, the baseline anchor (R1) and this repository. No gate
    script was run (no code blocks).
  - *Measure.* **0.02** in each pass, on the discretionary-expression
    basis of chapter 01 R39 and R35, dictated and pinned content
    excluded, read against the standard that any single free
    discretionary match, transcribed body or control flow, or
    discretionary message or comment wording fails the candidate; none
    was found.
  - *Pass 3 (final sweep, 2026-10-25).* PASS, measure 0.02, finding list
    empty, standard unchanged. The whole current candidate — all of
    chapter 60 and every added line of the companion diff, this addition
    included — was compared with the comments, string literals and prose
    documentation of the compared material by a word six-gram overlap
    test, which found no overlap, and a five-gram near-match check, which
    left only non-discretionary residue. Every code-formatted token
    passed the forced-versus-free test, and the added non-record lines
    were re-read against the pass-1 reading. The pass confirmed that the
    only rule-text change after pass 1 was in chapter 60 §60.1, so this
    addition is unchanged since pass 1.
  - *Pass 5 (2026-10-25; reference-state names).* PASS, leak measure
    0.02, no findings. The leak check covered that pass's changed lines,
    one of them in this item. The chapter 02 R13 check covered the whole
    chapter-60 candidate, this addition included, and found zero
    findings: every state name has a row, and every state compared in
    passes 1–4 matches its row's identifier. Method, extent and gaps are
    recorded in [Chapter 60](60-trading-floors-and-refusal-contract.md)
    §60.12; gaps (a)–(c) stand.
- *Dirty Gate clearance (chapter 01 T1, R40, R43).* The whole of this
  chapter's forbidden-class-sourced material as it stood on 2026-10-24 — the
  *Forbidden corpus* item above, less the two scoped additions that follow
  it, which is to say the legacy gas-price resolution of §57.2, the
  proportional-increase arithmetic and its stages in §57.3, the swap fee
  form of §57.4, the two per-coin gas-limit configuration blocks of
  §57.5 and §57.6, and the withdraw fee resolution, gas-limit estimation
  and envelope-type derivation of §57.7 — was authored by the KDF Spec
  Reader under the two-team procedure of chapter 01 §1.4A and **passed
  the KDF Dirty Gate** on 2026-10-24: PASS, discretionary-expression
  measure 0.02, finding list empty, and accordingly no remedy class for
  any line or for the candidate as a whole. This was the chapter's first
  gate pass; no earlier verdict exists or was relayed, so nothing was
  re-gated. The clearance is therefore **complete and not pending**, and
  R40 withholds nothing here.

  *Which sections carry a pointer to this item, and why only those.*
  §57.2 through §57.7 — the six sections the scoped item above names —
  each carry a short clearance pointer, so that a reader or an
  implementer entering at the material passes the notice at the point at
  which the material appears (R40) and not only on reaching this footer.
  The remaining sections carry none, and that is a scope judgement rather
  than an oversight. §57.0 and §57.1 bind this chapter's own treatment,
  scope split and reference-designation discipline and state no
  forbidden-class-sourced fact. §57.8's tests, §57.9's deferrals and
  §57.11's external references are derivative: each tests, defers or
  hands on material §57.2–§57.7 already bind, and carries no fact the
  scoped item above does not already scope to one of those six sections,
  so repeating the notice there would multiply it without putting it any
  nearer the material it concerns. §57.10's baseline verifications cite
  R1 baseline material and fall outside a candidate-against-corpus
  comparison altogether. §57.12 is the record itself.

  The three elements of the R39 scope attestation, carried forward here
  in corpus-free terms as chapter 01 T1 requires:

  - *Extent.* The forbidden-class material corresponding to the subject
    matter this chapter's own §57.0 and §57.1 scope: legacy gas-price
    resolution and its source set; the proportional-increase arithmetic
    and its stage application; the per-coin gas-limit and
    gas-price-multiplier configuration blocks, with their absent-key
    defaults, their selection and their platform-coin inheritance; the V2
    gas-limit selection matrix; and withdraw fee-form resolution,
    gas-limit estimation and envelope-type derivation — together with
    the shared withdraw fee-object definition of the coin-abstraction
    surface on which §57.7 depends. Quantity examined: **seven files**
    of that surface, in whole or in part, at the newest state, three of
    them at length; the two principal ones additionally retrieved and
    compared at each of the two named release designations. Reference
    designations: the publicly published release designations
    `v2.6.0-beta` and `v3.0.0-beta`, which the lineage's own releases
    carry and which §1.7 admits as public-release facts, together with
    the newest unreleased state of the lineage, for which **the record is
    deliberately no more precise than "after `v3.0.0-beta`"** — no
    post-anchor commit identifier, no branch state and no location within
    the compared material is given (chapter 01 R11 and the
    reference-version limit of §1.7). No finer extent can be given
    without locating expression.

  - *Method.* **Reading**, combined with a mechanical text search over
    the compared material for every configuration key, member name, wire
    tag, percentage and numeric default this chapter states, and with
    version-to-version retrieval at the two named release designations;
    plus an exhaustive mechanical inventory of this chapter's own quoted
    tokens, each then judged individually by the forced-versus-free test.
    No aggregate match-density judgement was used or admitted. **The
    repository's own R35 gate tooling was not run:** it operates on files
    of source and this chapter embeds no source-code block — its single
    code block is a text-format arithmetic expression — so the
    instrument had no operand and its similarity figure would not have
    been informative. The absence is stated so that it is visible rather
    than implied.

  - *Measure.* **0.02** on the discretionary-expression basis — the same
    figure the leak report carries, not a second one. Excluded from the
    measured set as dictated or public-surface content under the
    forced-versus-free test, each item verified individually rather than
    inferred from the group it sits in: the member names of the
    `gas_limit`, `gas_limit_v2` and `gas_price_adjust` blocks together
    with the block and key names themselves (deserialized schema keys of
    a third-party-maintained public configuration file — R4, and R11's
    externally-visible interop-surface carve-out); the withdraw fee
    object's `type`-tag values and member names and the nested
    gas-option object's tag values (serde wire surface of a public RPC
    request); the public JSON-RPC method names, the fee-history
    response-array field name, that request's parameters — block count,
    newest block, reward-percentile list — and the ERC-20 contract
    symbols (R3 / R4); and the absent-key numeric defaults, the stage
    percentages and the fee-history percentage, which are values of the
    contract rather than authorial choices: each is observable at the
    external boundary of any node of the reference lineage through a
    published fee quote or a signed transaction's gas field, and each is
    overridable by the public configuration file, so each is
    pinned-constant content in R35's sense. The one arithmetic
    expression this chapter embeds (R57.3.1) is fee arithmetic whose
    exact form **and rounding point** are required for value parity
    (`AGENTS.md` §3) and is forced rather than a free authorial choice;
    it is the only token-level correspondence in the chapter not already
    covered by a name-or-shape carve-out, and the figure above reflects
    that single item and generic-prose overlap only. **Judgement
    standard read against:** zero tolerance — under the gate role's
    honesty guard a single instance of discretionary expression (a
    function body, a control-flow transcription, a private non-wire
    identifier, a diagnostic wording, an internal module tree) fails a
    candidate however much of the surrounding content is legitimately
    dictated. None was found, so the figure stands at effectively nil
    rather than at a threshold margin. Absent throughout, as the
    verdict's classification records: private non-wire and
    non-configuration identifiers; verbatim and near-verbatim bodies;
    step-by-step control-flow, branch or loop transcription; diagnostic
    and error **message wording** (this chapter names error *classes*
    only); per-method tables keyed to internal names; internal module
    trees; internal test identifiers; any location within the compared
    material; and any post-anchor commit identifier or branch
    designation.
- *Reloaded-original additions (2026-10-27), cleared by the KDF Dirty Gate.* R57.2.6 was
  written by a session that never read the corpus, from the maintainer's
  Decisions P and R of 2026-10-27 and this project's own code reading, which shows a `setprice` resolving the gas price twice with its node queries made one after another; no EVM latency was measured. The informative notes saying
  the reference reads its fee estimate or gas price per consumer and caches
  nothing rest on a KDF Spec Reader summary relayed before its gate pass, and
  are declared here as that input. *Dirty Gate clearance (chapter 01 T1,
  R40).* PASS on 2026-10-27, measure 0.01, no leaks; the declared input
  was verified accurate at both rows, and the switch names do not occur in the
  reference. *Extent:* the EVM legacy gas-price resolution surface of §57.2, at `v2.6.0-beta` and `v3.0.0-beta`.
  *Method:* reading plus mechanical text search against both rows; the gate
  script was not run because there are no code blocks. *Measure:* 0.01
  against the 0.10 threshold.
