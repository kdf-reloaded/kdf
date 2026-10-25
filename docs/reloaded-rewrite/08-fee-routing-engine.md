# Chapter 08 — Atomic-Swap Fee-Routing Engine

**Status:** driving-spec.

This chapter binds the typed fee-descriptor substrate and the arithmetic-only
fee-computation function that produces it, along with the two adjusted swap
trait-method signatures (`send_taker_fee` and `validate_fee`) that carry the
new descriptor through the swap.

## 8.1 Executive Summary

In the baseline tree the taker fee on every atomic swap is a single amount
sent to a single address. Three short helpers in the swap module compute the
number from per-process hard-coded constants (one rate, one discount list,
one floor). Two swap-trait methods carry the fee through the protocol:
`send_taker_fee` produces a single-output transaction, and `validate_fee`
accepts a flat parameter list ending in a single bare amount.

This chapter binds a generalisation of the fee path along three orthogonal
axes that do *not* change the on-chain HTLC protocol:

- **A typed fee descriptor.** A three-variant `DexFee` enum
  (`NoFee` / `Standard` / `WithBurn`) plus a companion
  `DexFeeBurnDestination` enum replace the bare numeric parameter at every
  boundary. Per-component accessors (`total_spend_amount`, `fee_amount`,
  `burn_amount`) keep the arithmetic in one place.
- **A network-parameter source of truth.** All numerics — base rate,
  discounted rate, discount-eligible tickers, floor, burn-enabled flag,
  fee/burn share, burn-pubkey — flow exclusively through the network-config
  accessor surface bound in Chapter 06. The fee module is a pure arithmetic
  consumer.
- **A struct-arguments boundary.** `validate_fee` takes a single
  `ValidateFeeArgs<'_>` value rather than a positional list, eliminating a
  same-type-confusion class (`expected_sender` vs `fee_addr`) and giving the
  new `dex_fee` field a named place to live.

The chapter is intentionally silent on every specific numeric: rate, share,
floor, burn destination, etc. are network policy bound in Chapter 06, not
substrate. Chapter 06 §6.6A additionally records which published reference
designation each production netid's parameter set comes from, and its §6.8
R17 forbids propagating a later designation's change onto the other netid or
blending the two sets.

## 8.2 Subsystem Shape

The fee substrate is a one-way pipeline. The arithmetic core consumes a
network-config handle, a taker-coin handle, a maker-coin ticker and a trade
amount; it produces a `DexFee` value. From there the descriptor flows
unchanged through the swap state machine to the two coin-trait methods that
actually touch chain.

The arithmetic core does not resolve any destination address. Per-coin layers
are responsible for deriving the fee-address output, the burn-address output
(for `WithBurn`), and any chain-specific encoding (script, ERC-20 transfer,
bank send). The arithmetic core is purely numeric and contributes only the
shape (`Standard` vs `WithBurn`) and the per-component values.

Two safety fallbacks live inside the core (R10–R11 below). Their purpose is
to guarantee that no `WithBurn` descriptor ever crosses the substrate
boundary with a zero, negative, or below-dust component — when those
guarantees would not hold, the core degrades to `Standard` for the entire
total.

## 8.3 Bound Type Surface

**R1.** The substrate exposes a public enum named `DexFee` with exactly
three variants: `NoFee`, `Standard(MmNumber)`, and
`WithBurn { fee_amount: MmNumber, burn_amount: MmNumber, burn_destination:
DexFeeBurnDestination }`. The variant set and field set are bound: clients
are entitled to exhaustive `match` against this shape.

**R2.** The substrate exposes a public enum named `DexFeeBurnDestination`
with exactly two variants:

- `KmdOpReturn` — payload-free, signalling that the burn output is encoded
  as a Bitcoin-style `OP_RETURN` output.
- `PreBurnAccount { burn_pubkey: Vec<u8> }` — carrying the raw public-key
  bytes the coin layer derives the destination address from.

**R3.** `DexFee` exposes three bound accessors with bound semantics:

| Accessor               | `NoFee`        | `Standard(a)` | `WithBurn { fee, burn, .. }` |
| ---------------------- | -------------- | ------------- | ---------------------------- |
| `total_spend_amount()` | zero           | `a`           | `fee + burn`                 |
| `fee_amount()`         | zero           | `a`           | `fee`                        |
| `burn_amount()`        | zero           | zero          | `burn`                       |

Per-component arithmetic outside these three accessors is forbidden in the
substrate; callers MUST NOT recompute `fee + burn` themselves at any other
site.

**R4.** `DexFee` implements `Display`. `Display` is the *only* surface
outside the coin layer that is permitted to observe the
`(fee_amount, burn_amount)` decomposition for diagnostic / logging purposes.

## 8.4 Bound Arithmetic Pipeline

**R5.** A single public function `compute_dex_fee` is the only producer of
`DexFee` values for normal taker-fee construction. Its parameter list is
bound to four items: a network-config handle (Chapter 06), a taker-coin
handle, a maker-coin ticker, and a trade amount. Its return type is `DexFee`.

**R6.** The pipeline computes the *total* fee in three bound steps:

1. **Multiplier selection.** The multiplier is the discounted rate when
   *either* the taker-coin ticker *or* the maker-coin ticker is a member of
   the discount-eligible set (exact, case-sensitive string match); otherwise
   it is the base rate. The two "sides" compared are specifically the
   taker-coin ticker and the maker-coin ticker of the pair. Both rates are
   read from the network-config accessor surface (R16).
2. **Exact-rational product.** The total is the trade amount multiplied by
   the selected multiplier, evaluated as an *exact rational* (the operating
   `MmNumber` type, a `BigRational`-backed exact rational). No intermediate
   rounding, decimal truncation, or satoshi / base-unit conversion occurs at
   this stage; conversion to an integer base-unit amount is deferred
   entirely to the coin layer at transaction-build time. This exactness is
   contract-relevant: a counterparty validates the fee against the same
   exact-rational product, so any early truncation here would desynchronise
   the two sides and cause the peer to reject the fee.
3. **Dust floor.** The total is floored at the *taker coin's
   minimum-transferable amount* (its dust accessor — the same value R11
   uses for the burn-path checks) after the exact-rational product is
   formed and before any burn split is derived. When the exact-rational
   product is at or below that dust floor, the total becomes exactly the dust
   floor; otherwise the product passes through unchanged. No separate
   network-level minimum-fee constant participates in the total.

The arithmetic is bound to be deterministic and side-effect-free. In this
chapter the dust floor is the only binding floor for the total; the
network-level minimum-fee override is informative only and MUST NOT raise the
effective floor above the taker coin's minimum-transferable amount.

Because each peer applies step 3 with its *own* node's figure for the taker
coin, that figure is effectively negotiated: two nodes agree on a trade whose
product is below it only if they agree on the figure. The per-family values,
and what a disagreement does in each protocol, are bound in
[Chapter 60](60-trading-floors-and-refusal-contract.md) §60.3 and R60.5.4
(Spec Reader 2026-10-25; cleared there by the Dirty Gate on 2026-10-25).

> **Compatibility correction (informative).** An earlier Reloaded
> implementation bound the optional network-level minimum to 1/10000
> (0.0001) on both network identifiers and combined it with coin dust via
> `max()`. Neither reference does so: netid 8762 (`v2.6.0-beta`) and netid
> 6133 (the v3/dev lineage) use only the taker coin's
> minimum-transferable amount. The network accessor therefore returns zero
> for both production configurations, so the existing `max()` is
> mathematically dust-only. A regression case covers a product strictly
> between typical UTXO dust and 0.0001.

**R7.** When the network-config burn-enabled flag is *false*, the pipeline
MUST return `DexFee::Standard(total)` before consulting any per-coin burn
opt-in, share, burn destination, or burn-account key. Networks that do not
participate in a burn scheme stop here. When the flag is *true*, the
chapter-16 factory applies the per-coin policy in R8–R9.

**R8.** A coin whose direct-burn predicate is true takes precedence over the
general burn-account opt-in. The only bound direct-burn coin is the exact,
case-sensitive ticker `"KMD"`. Let `total` be the floored total of R6, `dust`
the taker coin's minimum-transferable amount (R11), and `share` the network
fee share (3/4 on netid 8762). The descriptor is chosen by three exact
rational ranges, evaluated in this order:

1. **Total at or below dust.** If `total ≤ dust`, the result MUST be
   `Standard(dust)`: one fee output, no burn output. Given R6's floor, this
   covers every trade whose exact product is at or below dust, including a
   product exactly equal to dust.
2. **Full split.** Otherwise, if `total × share ≥ dust`, the result MUST be
   `WithBurn { fee_amount: total × share, burn_amount: total − total × share,
   burn_destination: KmdOpReturn }`.
3. **Clamped split.** Otherwise (`dust < total` and `total × share < dust`),
   the result MUST be `WithBurn { fee_amount: dust, burn_amount: total −
   dust, burn_destination: KmdOpReturn }`. The fee-collection leg is raised
   to exactly `dust`. Only the part of the total above dust is burned, so the
   burn share falls below `1 − share`. The total is unchanged.

These ranges are the netid-8762 `v2.6.0-beta` wire contract. Under them the
fee-collection leg of a direct-burn descriptor is never below `dust`. Only
the `OP_RETURN` burn leg can be below dust, and dust policy does not apply to
it (R15A). With KMD's minimum-transferable amount of 1,000 base units and
the discounted rate 9/7770, the ranges fall at these trade amounts: range 1
for trades up to 259/30000 KMD (about 0.0086333), range 3 for trades above
that and below 518/45000 KMD (about 0.0115111), and range 2 from there up.

This precedence is wire-critical on netid 8762. The general burn-account
predicate is false there, but the KMD direct-burn path stays active and
produces the legacy two-output structure (range 2 or 3) or the one-output
dust fee (range 1).

**R9.** For a coin whose direct-burn predicate is false:

- if its general burn-account predicate is false, the result MUST be
  `Standard(total)`;
- if the predicate is true, the pipeline splits the total by the
  network-config share and emits `WithBurn` with
  `PreBurnAccount { burn_pubkey }`; a non-empty coin-specific key takes
  precedence over the network key, and an empty resolved key falls back to
  `Standard(total)`.

The burn-account substrate remains available for later features, but neither
production reference currently activates it: non-KMD netid-8762 takers and all
netid-6133 takers use `Standard`.

**R10.** *Safety fallback A — non-positive split.* If either computed
component is zero or negative, the pipeline MUST return `Standard(total)` and
MUST NOT emit a degenerate `WithBurn` descriptor.

**R11.** *Safety fallback B — minimum-transferable amount.* Let `dust` be the
taker coin's minimum-transferable-amount accessor. The direct OP_RETURN path
applies `dust` as R8's three ranges bind: `Standard(dust)` at or below dust,
and a fee leg of at least `dust` otherwise. This is the netid-8762 legacy
contract. The burn-account path tests both split components and falls back to
`Standard(total)` if either is below `dust`. No separate burn-dust
configuration participates.

**R12.** The `Standard` versus `WithBurn` decision MUST be taken by the single
`compute_dex_fee`/chapter-16 factory path. Coin-layer transaction code MUST
consume the resulting descriptor and MUST NOT re-derive or re-split it.

## 8.5 Bound Trait-Surface Change

**R13.** The swap-side trait method that builds the taker fee transaction
is bound to the signature shape

```text
send_taker_fee(dex_fee: &DexFee, fee_addr: &[u8], uuid: &[u8]) -> TransactionFut
```

The bare-amount parameter present in the baseline is removed; the
`DexFee` reference replaces it.

**R14.** The swap-side trait method that validates a peer-built taker fee
transaction is bound to take a single struct argument:

```text
validate_fee(args: ValidateFeeArgs<'_>) -> <future of unit / error>
```

with `ValidateFeeArgs<'a>` bound to exactly six named fields:

- `fee_tx: &'a TransactionEnum`
- `expected_sender: &'a [u8]`
- `fee_addr: &'a [u8]`
- `dex_fee: &'a DexFee`
- `min_block_number: u64`
- `uuid: &'a [u8]`

Renaming, reordering by position-significance, or collapsing
`expected_sender` and `fee_addr` is forbidden — the struct-arguments
boundary exists specifically to make the two byte-slice fields
non-confusable.

**R15.** Coin-layer implementations of these two methods MUST exhaustively
handle all three `DexFee` variants:

- `DexFee::NoFee` MUST short-circuit both methods to a no-op success.
- `DexFee::Standard(amount)` MUST produce / validate a fee transaction with
  exactly one output to the fee address in `amount`.
- `DexFee::WithBurn { fee_amount, burn_amount, burn_destination }` MUST
  produce / validate a fee transaction with two outputs: one to the fee
  address in `fee_amount`, and one to the destination dictated by
  `burn_destination` in `burn_amount` (`OP_RETURN`-style burn for
  `KmdOpReturn`; address derived from `burn_pubkey` for `PreBurnAccount`).

Structural validation — output count match, address match, value match — is
the coin layer's responsibility; the substrate guarantees only that the
numbers returned by `compute_dex_fee` are non-degenerate per R10–R11.

**R15A.** *No under-dust fee leg; `OP_RETURN` exempt by script.* (This
replaces an earlier R15A that let the builder emit a fee-collection output
below dust for KMD direct-burn taker fees. That rule rested on a wrong
premise about the reference split and is withdrawn; see the compatibility
correction below.) The taker-fee transaction MUST be built with the
generic UTXO transaction builder and its ordinary per-output dust guard,
with no per-output or per-descriptor exemption. An output whose script
begins with `OP_RETURN` is outside that guard because of its script, as in
the baseline builder. That is the only exemption. Under R8 a KMD
direct-burn descriptor always has a fee-collection leg of at least dust, so
none of its outputs needs any other exemption.

**R15B.** *UTXO taker-fee wire shape and validation tolerance (legacy
swap, both netids).* For a UTXO taker coin the taker-fee transaction MUST
have:

- output 0: P2PKH to the network fee address, value = `fee_amount`
  converted to base units;
- for `WithBurn`, output 1: the burn output, value = `burn_amount`
  converted to base units. For `KmdOpReturn` its script is exactly the
  single opcode `OP_RETURN` with no data push, and the burned value is
  carried in the output value itself. For `PreBurnAccount` it is P2PKH to
  the burn address;
- then any change output the builder adds.

Both amounts MUST be converted to base units by truncating the exact
rational toward zero. The maker's validation MUST check that output 0's
script equals the expected fee script and that its value is at least the
expected converted `fee_amount`. For `WithBurn` it MUST also check that
output 1's script equals the expected burn script and that its value is at
least the expected converted `burn_amount`. Validation MUST NOT require
exact equality, MUST NOT bound the output count from above, and MUST NOT
look at outputs past the expected ones. A `Standard` descriptor validates
output 0 only. For `NoFee` no transaction is required (chapter 51 R14).
Both references share this tolerance. Because it is one-sided (at least),
a peer that computes a larger leg is accepted and a peer that computes a
smaller leg is rejected.

> **Compatibility correction (informative, issue #11).** An earlier
> revision of R8/R11/R15A split every KMD total above dust 75/25 after the
> dust floor. It then exempted the resulting under-dust fee-collection
> output from the builder's dust guard, on the belief that `v2.6.0-beta`
> emits 868 + 289 base units for a 0.01 KMD taker. Public-network
> observation contradicts that belief: a `v2.6.0-beta` node emitted 1,000
> to the fee address and 158 to `OP_RETURN` for a 0.01 KMD trade. The
> corrected ranges of R8 reproduce this. Under the withdrawn rule a
> reloaded maker rejected the valid legacy transaction (burn leg 158 below
> the expected 289). A `v2.6.0-beta` maker also rejects every reloaded
> taker fee in ranges 1 and 3 (fee leg 868 or 750 below the expected
> 1,000). The totals the two rules compute are identical; only the split
> differs. A reloaded-to-reloaded swap under the withdrawn rule succeeded
> only because both sides shared the same wrong split.

**R15C.** *No-fee waiver (both netids, both protocols).* The no-fee
descriptor MUST be produced whenever two conditions hold: the taker coin
is not a privacy coin, and the taker's taker-coin swap public key is
byte-for-byte equal to the active network's burn-address public key.
The burn gate and the per-coin burn predicates are not consulted. The
bound keys are:
- netid 8762: `0369aa10c061cd9e085f4adb7399375ba001b54136145cb748eb4c48657be13153`;
- netid 6133: `03a778d9bd346fa704cf3e2508cd074d93a1bbc1e504fbecbb0a8d48e7cccbbf5c`,
  which equals that network's fee key, so the fee-collection key holder
  pays no fee.

This is the observable contract of both reference lineages, adopted by
maintainer decision on 2026-09-27. The decision tree is bound by chapter
16 R7, and the call sites by chapter 16 R12A. On the legacy protocol the
taker then sends no fee transaction and the maker requires none (chapter
51 R14, R27). On the version-two protocol the funding carries no fee
component and the spend uses the `NoFee` layout (chapter 16 R16). The
separate version-two ticker exemption, `"KMD"` on netid 8762, is bound by
chapter 16 R12B.

## 8.6 Bound Network-Parameter Surface (cross-link to Chapter 06)

**R16.** The fee substrate MUST source the following parameters
exclusively from the network-config accessor surface bound in Chapter 06,
and MUST NOT define any of them as compile-time constants:

| Bound semantic                                  | Network-config accessor                  |
| ----------------------------------------------- | ---------------------------------------- |
| Base rate (trade amount → fee multiplier)       | base-rate accessor                       |
| Discounted rate (when discount list applies)    | discounted-rate accessor                 |
| Discount-eligible ticker list                   | discount-ticker-list accessor            |
| Optional network minimum-fee override (informative) | min-threshold accessor |
| Burn-enabled flag                               | burn-enabled accessor (default false)    |
| Fee/total share (remainder is burned)           | share accessor (default one)             |
| Burn-destination raw public-key bytes (also the R15C no-fee waiver key; non-empty on both production netids) | burn-pubkey accessor |
| Version-two no-fee ticker set (chapter 16 R12B) | version-two no-fee ticker accessor (default empty) |

When present, this override MUST NOT raise the effective floor above the
magnitude of the taker coin's minimum-transferable amount on either network.

**R17.** A new network is added by extending the network-config
implementation only. No fee-substrate code change is permitted to add or
adjust a network's numeric policy.

> **Code-quality finding (informative).** R16 binds the burn-destination
> raw public-key bytes (and every other network numeric) as sourced
> exclusively through the Chapter 06 network-config accessor surface. One
> coin-layer consumer beneath the arithmetic core does not follow that rule:
> the UTXO-family V2 swap path's `DexFee::Standard` fee-output builder
> resolves the fee-recipient public key from a deprecated global constant
> instead of resolving it through the active network's config accessor,
> so a UTXO V2 swap on one supported network identifier would build its
> standard dex-fee output against another network identifier's fee
> address. This is the same defect, with its proposed fix, already
> recorded as a Code-quality finding in
> [Chapter 29](29-license-conditions-e-f.md#291-reproduction-detail)
> §29.1.4; it is cross-referenced here rather than re-analysed because
> the rule it violates (R16, network-parameter sourcing) belongs to this
> chapter.

## 8.7 Tests (test invariants)

**T1.** *Floor.* For a trade amount whose exact-rational product with the
network-config base rate is at or below the taker coin's
minimum-transferable amount (dust), `compute_dex_fee` MUST return a
descriptor whose `total_spend_amount()` equals exactly that dust amount.
Conversely, for a product strictly above the dust amount, the descriptor's
`total_spend_amount()` MUST equal the exact-rational product unchanged — no
network-level minimum-fee constant may raise it (netid-8762 `v2.6.0-beta`
and netid-6133 `dev` interop, R6).

**T2.** *Burn-disabled passthrough.* With the network-config burn-enabled
flag false, the pubkey-blind `compute_dex_fee` MUST return `Standard(_)`
even for a coin whose direct-burn predicate is true. The share and
burn-account policy MUST have no effect. The pubkey-aware path still applies
the R15C waiver when the gate is false (T7).

**T3.** *Direct-burn precedence and split.* With burn enabled, direct burn
true, general burn-account opt-in false, share 3/4, and `total × 3/4 ≥
dust` (R8 range 2), the descriptor MUST be `WithBurn { fee_amount: total *
3/4, burn_amount: total * 1/4, burn_destination: KmdOpReturn }`. Ranges 1
and 3 are covered by T5A.

**T4.** *Inactive account burn.* With burn enabled but both per-coin burn
predicates false, the descriptor MUST be `Standard(total)`. A separate helper
test MUST retain coverage of the dormant burn-account 75/25 split and its
per-component dust fallback.

**T5.** *Production compatibility matrix.* The tests MUST prove that netid
8762 emits the KMD direct-burn descriptor of R8 while keeping non-KMD fees
standard. They MUST also prove that netid 6133 emits a standard descriptor
for KMD and non-KMD takers alike, floored at dust, with no split for any
amount. The netid-8762 KMD case MUST cover the issue-1 values: trade amount
15.86 (maker CHTA), discounted total 1,837,065 base units at eight decimals,
fee output 1,377,799, burn output 459,266.

**T5A.** *KMD direct-burn ranges (netid 8762, taker KMD, KMD dust 1,000
base units, eight decimals).* The descriptor and its converted outputs MUST
be:

| Trade amount (KMD) | Range (R8) | Output 0, fee P2PKH | Output 1, `OP_RETURN` |
| --- | --- | --- | --- |
| 0.0084 | 1 | 1,000 (`Standard`) | none |
| 259/30000 (product exactly = dust) | 1 | 1,000 (`Standard`) | none |
| 0.01 | 3 | 1,000 | 158 |
| 0.0115 | 3 | 1,000 | 332 |
| 0.0116 | 2 | 1,007 | 335 |
| 15.86 | 2 | 1,377,799 | 459,266 |

Each descriptor's total spend amount MUST equal the floored exact total of
R6, for example 1,000 base units for 0.0084 and 9/777000 KMD for 0.01. The
pre-correction values (868/289 for 0.01, 750/250 for 0.0084) MUST NOT be
produced. Each descriptor MUST build through the generic builder with its
ordinary dust guard (R15A). An under-dust P2PKH output or under-dust change
MUST still be rejected or folded by the generic policy.

**T5B.** *Validation interop (R15B), netid 8762, KMD taker.* A maker
expecting the 0.01 KMD descriptor of T5A MUST accept a fee transaction
with outputs 1,000 to the fee address, then 158 on a bare `OP_RETURN`, then
change. This is the on-wire shape of a `v2.6.0-beta` taker. The same maker
MUST reject 868 + 289, because the fee leg is below 1,000. A maker
expecting the 0.0084 KMD descriptor MUST accept a single 1,000 fee output
followed by change, and MUST reject 750 + 250. A burn output carrying a data
push after `OP_RETURN`, or one at an index other than 1, MUST be rejected.

**T6.** *No-arithmetic-outside-accessors guard (linter or audit).* A
substrate-internal audit (test or lint) MUST confirm that no call site
outside the accessor implementations performs the `fee_amount + burn_amount`
addition on a `DexFee` value. This protects R3.

**T7.** *No-fee waiver (R15C).*
- On netid 8762 a KMD taker, and separately a non-KMD UTXO taker, whose
  key is `0369aa…3153` MUST get `NoFee` from the pubkey-aware factory.
- On netid 6133 a KMD taker, and separately a non-KMD taker, whose key is
  `03a778…bf5c` MUST get `NoFee`, although the burn gate is false there.
- A privacy-coin taker holding the key MUST get the normal descriptor.
- A key differing in one byte MUST get the normal descriptor.
- The role-level legacy and version-two cases are chapter 16 T4A and T4B.

> **Implementation obligations (for the Coder; clean terms).**
> - *Direct-burn split:* in the coins crate's fee-descriptor types module
>   (`mm2src/coins/lp_coins_types.rs`), the direct-burn split helper MUST
>   implement R8's three ranges on the floored total. The pubkey-aware
>   factory MUST implement R15C (see chapter 16 R7).
> - *Under-dust exemption:* withdraw it entirely. That means the taker-fee
>   exemption selector in `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`,
>   the send variant that takes an allowed-under-dust output in
>   `utxo_common_tx.rs` and `utxo.rs`, and the builder option that carries
>   it. The builder's own script-based `OP_RETURN` exemption stays.
> - *Network configuration:* `mm2src/mm2_net_config/src/netid_8762.rs`
>   MUST return the 8762 burn key, and a version-two no-fee ticker set
>   {`"KMD"`}. `netid_6133.rs` MUST return an empty set.
>   `mm2src/mm2_net_config/src/lib.rs` gains that accessor (chapter 06).
> - *Tests to replace in `mm2src/coins/utxo/utxo_tests.rs`:*
>   - `should_build_small_v2_6_0_beta_kmd_taker_fee_without_weakening_dust_policy`
>     (asserts 868/289 and the exemption);
>   - `should_emit_kmd_op_return_split_on_netid_8762`, re-checked against
>     the ranges.
> - *Tests to extend:* `should_build_v2_6_0_beta_kmd_taker_fee_outputs`
>   (15.86 case), with the T5A table and T5B validation cases.
> - *Tests to replace in `mm2src/mm2_main/src/lp_swap/dex_fee.rs`:*
>   `burn_disabled_network_does_not_waive_fee_for_burn_pubkey`, which MUST
>   be inverted per T7.
> - *Tests to review in `mm2src/mm2_main/src/lp_swap/swap_rpc.rs`:*
>   `should_configure_kmd_burn_on_netid_8762`, for the added burn key.
> - *Operator docs:* in `CHANGELOG.md`, add a correcting entry that
>   supersedes the small-KMD-burn exemption entry and the "75/25" wording.
>   In `docs/NETWORK_CONFIG.md`, update the 8762 "Burn" row to the three
>   ranges, and the 8762 and 6133 waiver-key text.

## 8.8 Deferred Work

**D1.** A configurable per-pair share (rather than a single network-wide
share) is deferred. The current substrate routes a single share through
the network-config accessor.

**D2.** An ERC-20-side burn that is not a transfer-to-pubkey (for example,
an explicit `burn(uint256)` extension on a token that supports it) is
deferred. The current substrate models burn exclusively as a
destination-address output.

**D3.** A pair-specific burn override beyond the network gate and per-coin
predicates is deferred.

**D4.** Per-coin dust accessors that are state-dependent (for example,
varying with fee-rate estimation) are deferred; R11 reads a single
deterministic dust value per coin per evaluation.

## 8.9 External References

- KDF Reloaded issue #1 and its attached public netid-8762 KMD/CHTA
  swap-failure record, <https://github.com/kdf-reloaded/kdf/issues/1>.
- KDF Reloaded issue #11 (KMD swaps fail): public maker/taker logs,
  including the on-wire 1,000 + 158 taker fee of a `v2.6.0-beta` node and
  the 0.0084 KMD case, plus the controlled 2026-09-27 KMD mainnet runs,
  <https://github.com/kdf-reloaded/kdf/issues/11>.
- *Atomic swap* — HTLC-based cross-chain atomic-swap overview.
- *Bitcoin Script `OP_RETURN`* — semantics of provably-unspendable outputs
  used for the `KmdOpReturn` burn destination.
- *Bitcoin Core dust-threshold policy* — relay-policy rationale informing
  the under-dust safety fallback bound in R11.
- *ERC-20 token-transfer semantics* (EIP-20) — relevant to coin-layer fee
  delivery on EVM coins under R15.
- *Cosmos SDK `x/bank`* — relevant to coin-layer fee delivery on Tendermint
  coins under R15.
- Chapter 06 (Network identifier and parameter substrate) — bound source
  of every numeric input to R6–R11 and the burn destination's pubkey bytes.
- Chapter 04 (error-aggregation type adaptation) — bound shape of the
  errors any future fallibility extension to `compute_dex_fee` would use.

## 8.10 Baseline Verifications

**V1.** The baseline tree MUST be confirmed to define exactly the
three pre-substrate helpers (`dex_fee_threshold`, `dex_fee_rate`,
`dex_fee_amount`) inside the monolithic swap module, with no dedicated
fee submodule:

```
git -C <baseline> grep -nE 'fn (dex_fee_threshold|dex_fee_rate|dex_fee_amount)\b'
```

**V2.** The baseline tree MUST be confirmed to lack any `DexFee`,
`DexFeeBurnDestination`, or `ValidateFeeArgs` type:

```
git -C <baseline> grep -nE '\b(DexFee|DexFeeBurnDestination|ValidateFeeArgs)\b'
```

**V3.** The baseline `validate_fee` and `send_taker_fee` swap-trait methods
MUST be confirmed to use positional bare-amount parameters as described in
the executive summary — i.e. the baseline shape this substrate replaces is
the documented one:

```
git -C <baseline> grep -nE 'fn (validate_fee|send_taker_fee)\b'
```

## 8.11 Provenance Footer

- *Inputs consulted for this chapter:* the baseline tree at project
  baseline commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`, Chapter 04
  (error envelope), Chapter 06 (network-id and parameter substrate), the
  public issue-1 failure record, the public issue-11 logs and controlled
  KMD mainnet observations (permitted-input class R6). R8, R11, R15A,
  R15B, R15C, R16, T2, T5A, T5B and T7 were re-derived on 2026-09-27
  under the chapter-01 two-team Spec Reader / Dirty Gate workflow
  (AGENTS.md §2) against the `v2.6.0-beta` and v3-lineage references.
  They are stated as observable contract only. R15C records a
  maintainer decision. Other inputs: the present-day working tree (the
  `dex_fee_standard_output` R16 finding cross-referenced from Chapter 29),
  and the external specifications listed in §8.9.
- *Permitted-input classes used:* baseline source; the present-day working
  tree, quoted as evidence under the legal-position carve-out of chapter 01
  R11, for the R16 finding; chapter-bound type identifiers introduced here
  as substrate-contract surface (`DexFee`, `DexFeeBurnDestination`,
  `ValidateFeeArgs`, `compute_dex_fee`, `send_taker_fee`, `validate_fee`);
  standard chain-protocol terminology (`OP_RETURN`, ERC-20, Cosmos SDK
  `x/bank`).
- *Sibling chapters cross-referenced:* Chapter 04, Chapter 06, Chapter 29
  (R16 finding cross-reference).
- *Author of this chapter:* clean-room round-2 driving-spec working set.
- *Forbidden corpus:* not consulted.
- *Dirty Gate clearance (chapter 01 T1, R40, R43) for the §8.1
  cross-reference sentence:* the material this item covers is exactly one
  sentence — the cross-reference added at the end of §8.1, which points at
  chapter 06 §6.6A and §6.8 R17. It was examined by the KDF Dirty Gate in
  the same pass that cleared chapter 06 §6.6A and **passed** on 2026-10-24:
  PASS, discretionary-expression measure 0.00, **leak-finding list empty**,
  and accordingly no remedy class for the sentence or for the candidate as a
  whole, and no non-leak finding either. The clearance is **complete and not
  pending**. The gate's confirmation is that the sentence **carries no
  independent claim**: it states a cross-reference and restates the
  direction of a rule chapter 06 owns, and introduces no numeric, no ticker,
  no identifier and no designation-attributed fact of its own, so this
  chapter's deliberate silence on specific numerics is intact. This item is
  written on the clean side from the verdict, which reached the clean side
  as relayed and transcribed text rather than as a gate-written file; that
  is record-keeping and adds no chapter content.

  **Only that sentence was gated, and this chapter's earlier clearance is
  untouched.** Nothing outside the added sentence was examined or re-judged
  in this pass, and the chapter's earlier clearance is **not reopened**. The
  *Forbidden corpus* item above is unchanged by the pass and keeps the
  position it already had: the added sentence took no forbidden-class input
  of its own, resting entirely on chapter 06 §6.6A, where that section's own
  input and clearance are recorded. The sentence carries no inline clearance
  pointer, which is a scope judgment rather than an oversight: it binds
  nothing, defers its whole substance to the chapter that owns it, and this
  footer is where the record lives.

  The three elements of the R39 scope attestation, carried forward here in
  corpus-free terms as chapter 01 T1 requires:

  - *Extent.* The forbidden-class material corresponding to the subject
    matter this chapter names in its own terms, confined to what a single
    cross-reference sentence could carry: the taker trading-fee rate surface
    and the fee-collection-identity surface. Quantity examined: **two units
    at two reference points**, for four unit-at-designation comparisons.
    Reference points: the published release designations `v2.6.0-beta` and
    `v3.0.0-beta`. **The record is no more precise than those
    designations:** no post-anchor commit identifier, no branch state, and
    no location within the compared material is given (chapter 01 R11 and
    the reference-version limit of §1.7).
  - *Method.* **Combined.** Reading, with mechanical retrieval used solely
    to address each comparison to a published release designation rather
    than to any materialised undesignated state. **This repository's own
    clean-room gate tooling was not applied and was not run:** no source
    block was in scope, so the instrument had no operand.
  - *Measure.* **0.00** on the discretionary-expression basis — the same
    figure the leak report carries, not a second one — with dictated-interop
    and pinned-interface content excluded from the measured set before the
    judgment was made. **Judgment standard read against:** a presence
    standard rather than a proportion — any genuine discretionary expression
    fails a candidate irrespective of volume. The measured set was empty, so
    the PASS rests on that and not on a numeric margin.
