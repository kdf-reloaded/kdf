# Chapter 57 — EVM Fee Inputs: Legacy Gas-Price Resolution, Per-Coin Gas Limits, and the Withdraw Fee Form

**Status:** driving-spec (mixed treatment — see §57.0).

> **One-sentence claim:** the project shall resolve an EVM transaction's
> two fee inputs — the price per unit of gas and the gas limit — by the
> contract bound here: a legacy gas price taken as the maximum of two
> node-RPC sources and raised only *proportionally* (never by an absolute
> floor), a per-coin configuration block that overrides every default gas
> limit by named key, and a withdraw request fee form that admits both the
> legacy and the EIP-1559 shape and derives the signed transaction's
> envelope type from whichever was resolved.

## 57.0 Treatment & scope split

This chapter is new and spans three adjacent surfaces of the EVM coin
layer. Reloaded ships part of each, so the chapter is split:

- **Part A (§57.2–§57.4), legacy gas-price resolution.** Mostly as-built
  in reloaded, with one bound correction (R57.3.2, removal of the
  absolute one-gwei increment floor) and two bound gaps
  (R57.2.4, R57.4.1).
- **Part B (§57.5–§57.6), per-coin gas-limit overrides.** Required and
  **not** in reloaded: the configuration block is never read and every
  gas limit is a compile-time constant.
- **Part C (§57.7), the withdraw fee form.** Partly in reloaded: the
  legacy fee form is accepted, the EIP-1559 fee form is not, and the
  envelope-type derivation that follows from the resolved form is absent.

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
resolution. If neither source produced a value, resolution fails with an
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
> §57.12, and are not restated here.

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
the gas-estimate multiplier of D3. For a platform coin, only its own
entry is consulted.

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

**D3.** *Gas-estimate multiplier.* The reference carries a further
per-coin configuration key that scales a node's `eth_estimateGas` result
before use, and the public coins configuration sets it to 1.3 for two
layer-two platform coins. Reloaded does not read it. It is deferred
because it belongs with the estimation surface rather than the
fee-input surface, and because a multiplier applied to an estimate that
already carries chain-specific slack needs its own parity check against
a live node of each affected chain.

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
  `gas_price_adjust`, or the gas-estimate multiplier of D3.
- *Verification.* A search of `mm2src/` for each key name returns no
  configuration read; the V2 gas-limit set is constructed from its type's
  default at every construction site.

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
  and the absence of any `gas_price_adjust` entry.
- [Chapter 35 — EVM V2 Activation & Token RPC Surface](35-evm-v2-activation-rpcs.md)
  for the swap gas-fee policy (R35.6) and the envelope-type cap (D35.7).
- [Chapter 56 — EVM EIP-1559 Fee-Per-Gas Estimation](56-evm-eip1559-fee-estimation.md)
  for the estimator this chapter's R57.4.1 consumes and whose base-fee
  selection R57.2.2 deliberately differs from.
- [Chapter 19 — NFT Module Layout](19-nft-module-layout.md) §19.7 for the
  non-fungible withdrawal flows R57.7.5 binds.
- [Chapter 49 — Withdrawal task path](49-withdrawal-task-path.md) for the
  withdraw RPC surface the fee object of R57.7.1 travels on.

## 57.12 Provenance Footer

- *Inputs:* the public Ethereum JSON-RPC method definitions and the
  EIP-1559 / EIP-2718 specifications (the fee-source semantics of §57.2
  and the envelope types of §57.7); the public third-party
  coins-configuration file (the deployed `gas_limit` member sets, the
  per-coin swap gas-fee policy and envelope-type-cap values, and the
  absence of a `gas_price_adjust` entry); the pinned baseline tree (V1,
  V2); the present reloaded workspace (V3 and every
  implementation-status statement).
- *Permitted-input classes used:* R3 (external public specifications —
  EIP-1559, EIP-2718), R4 (wire formats and external interfaces the
  project must inter-operate with — the three JSON-RPC methods and the
  coins-configuration key schema), R1 (the baseline tree, for V1 and V2),
  and the project's own tree and revision history.
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
  §57.5–§57.6; and the withdraw fee-form tags, member names,
  estimation rule and envelope-type derivation of §57.7. Every dictated
  fact in §57.2's method semantics and §57.7's envelope types is sourced
  from the public specifications (R3/R4); every configuration key name,
  value shape and deployed value is sourced from the public coins
  configuration (R4). No discretionary expression — no private type,
  field or function name, no function body, control-flow transcription,
  helper decomposition, local name, error or log literal — was
  reproduced. The numeric defaults and percentages are bound as parity
  data on the justification stated in §57.0 and are observable at the
  external boundary of any node of the reference lineage.
- *Dirty Gate clearance (chapter 01 T1, R40, R43).* The whole of this
  chapter's forbidden-class-sourced material — the scoped item above,
  which is the legacy gas-price resolution of §57.2, the
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
