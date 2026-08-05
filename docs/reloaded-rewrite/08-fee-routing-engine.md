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
substrate.

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
general burn-account opt-in. Its fee is split as
`fee_amount = total * share` and `burn_amount = total - fee_amount`, where
`share` comes from the network configuration, and the destination is
`DexFeeBurnDestination::KmdOpReturn`. The currently bound direct-burn coin is
the exact, case-sensitive ticker `"KMD"`. If the total itself is below the
coin's minimum-transferable amount, the descriptor falls back to
`Standard(total)`.

This precedence is wire-critical on netid 8762: the general burn-account
predicate is false there, but the KMD direct-burn path remains active and
produces the legacy two-output 75/25 structure.

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
tests the unsplit total against `dust`, preserving the netid-8762 legacy
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

**R15A.** *Legacy KMD direct-burn output dust exception.* Once the unsplit
total has passed the direct-path check in R11, a
`WithBurn { burn_destination: KmdOpReturn, .. }` taker-fee transaction MUST
emit the exact two outputs bound by R15 even when base-unit conversion makes
the positive fee-collection P2PKH leg smaller than the taker coin's
minimum-transferable amount. The transaction builder MUST exempt exactly
output zero of this direct-burn taker-fee transaction from its generic
per-output dust guard.

This exception MUST NOT change the descriptor arithmetic, raise the total,
merge the outputs, or fall back to `Standard`. It MUST NOT exempt standard
fee outputs, burn-account outputs, change, or any unrelated transaction
output. The `OP_RETURN` output remains outside spendable-output dust policy
by its script semantics. This narrow rule preserves the small-trade wire
shape emitted by the netid-8762 `v2.6.0-beta` implementation without
weakening the generic UTXO builder's dust and change handling.

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
| Burn-destination raw public-key bytes           | burn-pubkey accessor                     |

When present, this override MUST NOT raise the effective floor above the
magnitude of the taker coin's minimum-transferable amount on either network.

**R17.** A new network is added by extending the network-config
implementation only. No fee-substrate code change is permitted to add or
adjust a network's numeric policy.

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
flag false, `compute_dex_fee` MUST return `Standard(_)` even for a coin whose
direct-burn predicate is true and even when its public key equals the inactive
burn key. The share and burn-account policy MUST have no effect.

**T3.** *Direct-burn precedence and split.* With burn enabled, direct burn
true, general burn-account opt-in false, and share 3/4, the descriptor MUST be
`WithBurn { fee_amount: total * 3/4, burn_amount: total * 1/4,
burn_destination: KmdOpReturn }`.

**T4.** *Inactive account burn.* With burn enabled but both per-coin burn
predicates false, the descriptor MUST be `Standard(total)`. A separate helper
test MUST retain coverage of the dormant burn-account 75/25 split and its
per-component dust fallback.

**T5.** *Production compatibility matrix.* The tests MUST prove that netid
8762 emits the KMD direct-burn descriptor while keeping non-KMD fees standard,
and that netid 6133 emits a standard descriptor for both KMD and non-KMD
takers. The netid-8762 KMD case MUST cover the issue-1 values: trade amount
15.86, discounted total 1,837,065 base units at eight decimals, fee output
1,377,799, and burn output 459,266.

**T5A.** *Small KMD direct-burn construction.* For netid 8762, taker KMD,
trade amount 0.01, and eight coin decimals, the tests MUST assert the exact
two converted output values: 868 base units to the fee-collection P2PKH and
289 base units to the `OP_RETURN` burn output. The same 868-base-unit P2PKH
MUST fail the generic builder without the R15A exemption, while the
direct-burn taker-fee policy MUST build successfully. A separate under-dust
P2PKH and under-dust change MUST remain subject to the generic policy.

**T6.** *No-arithmetic-outside-accessors guard (linter or audit).* A
substrate-internal audit (test or lint) MUST confirm that no call site
outside the accessor implementations performs the `fee_amount + burn_amount`
addition on a `DexFee` value. This protects R3.

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
  public issue-1 failure record, and the external specifications listed in
  §8.9.
- *Permitted-input classes used:* baseline source; chapter-bound type
  identifiers introduced here as substrate-contract surface (`DexFee`,
  `DexFeeBurnDestination`, `ValidateFeeArgs`, `compute_dex_fee`,
  `send_taker_fee`, `validate_fee`); standard chain-protocol terminology
  (`OP_RETURN`, ERC-20, Cosmos SDK `x/bank`).
- *Sibling chapters cross-referenced:* Chapter 04, Chapter 06.
- *Author of this chapter:* clean-room round-2 driving-spec working set.
- *Forbidden corpus:* not consulted.
