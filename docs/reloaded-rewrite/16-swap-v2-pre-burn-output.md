# Chapter 16 — Atomic-Swap Version-Two Pre-Burn Output

**Status:** driving-spec.

A factory-and-helper substrate that turns the network-level
pre-burn policy into concrete three-output taker-payment-spend
transactions on the UTXO version-two atomic-swap path, completing
the dex-fee split deferred by chapter 15 and removing the explicit
deferred-variant rejection arms its helpers carry.

## 16.1 Executive Summary

The pre-burn output is an optional second leg of the version-two
atomic-swap dex-fee delivery: instead of paying the entire dex fee
to a single fee-collection address, a configurable share is *burned*
— either by routing it to a designated burn address (P2PKH on most
coin families) or by recording it in an `OP_RETURN` script
(KMD-only, provably unspendable). The split ratio is governed by a
network-level numeric (chapter 06 surface; the chapter-bound name
is the chapter-08 accessor `dex_fee_share`); the canonical split
documented for the bound network identifier is 75% to the fee
address and 25% to the burn destination.

The substrate landed by chapter 08 binds the *data* layer (`DexFee`
enum, `DexFeeBurnDestination` enum, the seven network-level
accessors, `compute_dex_fee`). The substrate landed by chapter 15
binds the version-two swap call graph but explicitly rejects
non-standard `DexFee` variants at three taker-payment-spend
helpers, deferring the `WithBurn` and `NoFee` arms to this chapter.

This chapter binds:

1. three new methods on the coin trait (the chapter-bound coin
   trait identifier is `MmCoin`) that expose per-coin burn
   policy — bound names `burn_pubkey`, `should_burn_directly`,
   `should_burn_dex_fee`;
2. two associated factory functions on `DexFee` — bound names
   `new_from_taker_coin` and `new_with_taker_pubkey` — plus two
   dust-aware split helpers;
3. three V2 UTXO taker-payment-spend helper updates that replace
   the deferred-variant rejection arms with explicit `WithBurn`
   and `NoFee` arms under the bound signature-hash strategy
   (R15);
4. a single bound burn-output construction routine that handles
   both burn destinations from the chapter-08 enumeration.

Bound rules R1–R5 cover the per-coin policy trait additions;
R6–R12 cover the factory functions and dust-aware split; R13–R20
cover the version-two UTXO helper updates and signature-hash
strategy; R21–R24 cover the activation surface and parallel
chains.

### 16.1.1 Reloaded policy state (informative)

The data and split substrate of this chapter ships in full in the
reloaded baseline; the per-network numeric policy that drives it
is supplied by the dedicated network-configuration crate
(`mm2_net_config`), with one `NetConfig` implementation per
network identifier, replacing the hard-coded constants of the
upstream baseline. The bound policy values exposed through the
public `NetConfig` accessors are:

- **Network identifier 8762** (original AtomicDEX network): base
  `dex_fee_rate` = 1/777 (~0.129%); `dex_fee_rate_discounted` =
  9/7770 (~0.116%, a 10% discount applied to the KMD ticker);
  `burn_enabled` = false — no pre-burn output is produced on this
  network.
- **Network identifier 6133** (GLEEC network): base `dex_fee_rate`
  = 2/100 (2%); `dex_fee_rate_discounted` = 1/100 (1%, a 50%
  discount whose `fee_discount_tickers` set is `["GLEEC"]`);
  `dex_fee_min_threshold` = 1/10000; `burn_enabled` = true with
  `dex_fee_share` = 3/4 (75% to the fee address, 25% to the burn
  destination, per the canonical split of §16.1).

> **GLEEC-conformance note (informative).** On network identifier
> 6133 the burn-destination public key returned by
> `burn_addr_pubkey` is **deliberately equal** to the
> fee-collection key returned by `dex_fee_addr_pubkey`. Both are
> the single compressed secp256k1 key
> `03a778d9bd346fa704cf3e2508cd074d93a1bbc1e504fbecbb0a8d48e7cccbbf5c`.
> This is **not a reloaded divergence or a defect**: it reproduces
> the GLEEC upstream configuration exactly, where the burn key is
> set equal to the fee key so that the burn is effectively
> neutralised at the address level — the 75/25 split still
> executes structurally (a two-output transaction is built per
> R6/R20), but the burned 25% lands in the same account as the
> fee. The shared value is retained intentionally as a guard:
> should the burn path ever be exercised unexpectedly, value is
> directed to the network's own fee account rather than being
> destroyed, and a genuinely distinct burn key can be substituted
> later (here and in upstream) without a code change.
> Implementations MUST keep these two keys equal on this network
> to match GLEEC and MUST NOT assume the burn key differs from the
> fee key. This equality is a required configuration invariant for
> netid-6133 conformance, not a gap to be closed.

## 16.2 Subsystem Shape

The substrate occupies a structural seam between four chapters:

- chapter 08 (data substrate: the typed `DexFee` enum, the
  `DexFeeBurnDestination` enum, the seven network-level
  accessors, the `compute_dex_fee` pipeline);
- chapter 06 (network-level numerics: `burn_enabled`,
  `dex_fee_share`, `burn_addr_raw_pubkey`);
- chapter 15 (the version-two UTXO swap path with its three
  taker-payment-spend helpers carrying the deferred-variant
  rejection arms this chapter removes);
- chapter 17 (the parallel version-two EVM path, which does not
  participate in the substrate — R23).

The substrate does *not* introduce a fourth `DexFee` variant, a
new network-level accessor, or a new state-machine transition.
All state-machine call sites in chapter 15 pass an opaque
`&DexFee` of arbitrary variant unchanged; the variant chosen by
the factory of R6 determines the helper branch taken inside
chapter 15's helpers.

## 16.3 Bound Per-Coin Burn-Policy Surface

**R1.** The coin trait MUST gain exactly three new methods. The
chapter-bound method names and bound semantics are:

| Method                                  | Bound semantics                                                                                                              |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `burn_pubkey() -> Vec<u8>`              | The compressed public key bytes of the network-designated burn account for this coin family; an empty vector when no burn account is configured. |
| `should_burn_directly() -> bool`        | `true` iff the burn portion MUST be recorded in an `OP_RETURN` script rather than sent to a P2PKH burn address (R20).        |
| `should_burn_dex_fee() -> bool`         | `true` iff this coin participates in the pre-burn split at all. When `false`, the factory of R6 MUST emit the standard variant. |

**R2.** All three methods MUST have default implementations on
the coin trait. The defaults MUST be: empty vector, `false`,
`false` respectively. Coin families that do not participate in
the substrate inherit the defaults unchanged. The defaults
guarantee binary compatibility with every existing coin trait
implementation in the workspace.

**R3.** The bound per-coin override matrix is exactly:

| Coin family                                     | `burn_pubkey()`                                                          | `should_burn_directly()`                | `should_burn_dex_fee()`                            |
| ----------------------------------------------- | ------------------------------------------------------------------------ | --------------------------------------- | -------------------------------------------------- |
| UTXO standard                                   | empty (defers to the network-level burn-account public-key accessor at the factory layer per R4) | `true` for the chapter-bound ticker literal `KMD`, `false` otherwise | `true`                                             |
| UTXO Bitcoin-Cash family / SLP / QRC20          | inherits the UTXO-standard override                                      | always `false`                          | inherits the UTXO-standard override                |
| EVM (Ethereum, ERC-20)                          | empty                                                                    | `false`                                 | `false`                                            |
| Tendermint                                      | already configured by its own swap-operations module; the substrate MUST NOT regress it | `false`                                 | already returns `true` when configured by the module |
| Lightning / NFT / SLP-token / Sia / Solana / Z-coin | empty                                                                    | `false`                                 | `false`                                            |

Substrate MUST NOT introduce a fourth burn-policy method, a
per-coin numeric burn-share, or any network-level override on the
per-coin booleans.

**R4.** The factory of R6 MUST honour a two-layer split between
coin-level opt-in and network-level gating:

- the coin's `should_burn_dex_fee` is the *coin-layer* opt-in;
- the network-level `burn_enabled` accessor is the *network-layer*
  gate (R6's first decision step);
- when the coin returns an empty `burn_pubkey`, the factory MUST
  fall back to the network-level burn-account public-key
  accessor; when the coin returns a non-empty value, the factory
  MUST prefer the coin's value.

**R5.** The chapter MUST NOT modify the network-level accessor
set bound by chapter 06. The chapter consumes them through the
chapter-08 `compute_dex_fee` pipeline and through the new factory
of R6.

## 16.4 Bound `DexFee` Factory and Dust-Aware Split

**R6.** Two associated factory functions MUST be added to the
chapter-08 `DexFee` type. The chapter-bound names are:

| Function                  | Bound role                                                                                                                                 |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `new_from_taker_coin`     | Used at swap initiation, before the taker's public key is known. Decides `Standard` vs `WithBurn` (KMD-OP_RETURN vs burn-account) based on coin and network flags. |
| `new_with_taker_pubkey`   | Used whenever the taker's public key is known, including validation. Returns `NoFee` iff the taker public key equals the burn public key; otherwise delegates to `new_from_taker_coin`. |

The base-fee computation (chapter-bound name `compute_base_fee`,
provided by chapter 08's `compute_dex_fee` pipeline) MUST NOT be
duplicated inside the factory; the factory's bound responsibility
is split-vs-no-split selection, not amount computation. Whether
the factory takes the already-computed base fee as a parameter or
calls `compute_base_fee` internally is a workspace-side
implementation detail not bound here; the decision tree of R7 is
unchanged in either form.

**R7.** The bound decision tree for `new_from_taker_coin` MUST be
exactly:

1. compute the base fee for the trade;
2. if the network-level `burn_enabled` accessor returns `false`
   *or* the coin's `should_burn_dex_fee` returns `false`, return
   the standard single-output variant carrying the entire base
   fee;
3. otherwise, if the coin's `should_burn_directly` returns
   `true`, delegate to the OP_RETURN split helper of R8;
4. otherwise, delegate to the burn-account split helper of R9.

The bound decision tree for `new_with_taker_pubkey` MUST be
exactly:

1. if the byte sequence of the coin's `burn_pubkey` (or its
   network-level fallback per R4) equals the taker public key,
   return `NoFee`;
2. otherwise, delegate to `new_from_taker_coin`.

The `NoFee` short-circuit guarantees the burn account itself is
not charged a fee on its own trades (R17 handles its
helper-branch semantics).

**R8.** The OP_RETURN split path MUST take exactly the base fee and
the coin's minimum-transmissible amount. If the entire fee would
be below the minimum-transmissible amount, it MUST return
the standard variant carrying the fee unchanged (dust fallback,
R10). Otherwise it MUST return the `WithBurn` variant with:

- a zero fee-amount component;
- the entire base fee as the burn-amount component;
- the OP_RETURN destination tag from the chapter-08
  `DexFeeBurnDestination` enumeration.

**R9.** The burn-account split path MUST take the base fee, the
coin's minimum-transmissible amount, the network-level fee-share
numeric, and the burn-account public key. It MUST compute the
fee-amount as `base_fee × fee_share` and the burn-amount as
`base_fee − fee_amount`. If either leg falls below the
minimum-transmissible amount, it MUST return the
standard variant carrying the entire fee (dust fallback, R10).
Otherwise it MUST return the `WithBurn` variant with the
computed fee-amount and burn-amount components and the
burn-account destination tag carrying the burn public key.

**R10.** Dust fallback MUST be uniform: both R8 and R9 MUST fall
back to the standard variant when the variant they would emit
contains any output below the coin's minimum-transmissible
amount. The substrate MUST NOT carry a separate dust
configuration; the coin's `min_tx_amount` accessor (a pre-existing
coin-trait method) MUST be the sole authority.

**R11.** The factory and the two split helpers MUST be pure with
respect to the coin and the network configuration: they MUST NOT
read transient state, MUST NOT lock, and MUST NOT block. Every
input the decision tree depends on is bound to be passed in by
the caller (chapter-15 call sites).

**R12.** The factory MUST emit one of exactly three `DexFee`
variants: `Standard`, `WithBurn`, `NoFee`. Substrate MUST NOT
introduce a fourth variant.

**R12A. Production call-site contract for known taker pubkey.**
Any production path that constructs an expected dex-fee value and
already knows the taker's expected sender public key MUST call the
taker-pubkey-aware factory of R6. It MUST NOT compute the expected
fee with the pubkey-blind factory or with only the chapter-08
`compute_dex_fee` pipeline. The trigger condition is the presence
of the public key that the fee transaction, taker funding, or
taker payment is expected to be signed by or otherwise bound to.
Under that condition, a taker whose public key equals the burn
public key MUST be treated as `NoFee`.

The production call-site contract is:

- V1 maker-side taker-fee validation MUST compute the expected
  `DexFee` from the taker coin, maker coin ticker, taker amount,
  and the expected taker sender public key before validating or
  deciding that no taker-fee transaction is required.
- V1 taker-side fee estimation, taker-fee send, locked-amount,
  and trade-preimage paths MUST use the taker-pubkey-aware
  factory whenever the local taker public key is available; they
  MAY use the pubkey-blind factory only for max-volume or
  pre-negotiation estimates where the relevant taker public key is
  not yet available.
- V2 maker-side validation of taker funding and V2 maker-side
  construction/validation of taker-payment spend/refund arguments
  MUST use the taker-pubkey-aware factory after negotiation has
  supplied the taker's public key.
- V2 taker-side construction of taker funding, taker payment
  spend preimages, funding refunds, and payment refunds MUST use
  the taker-pubkey-aware factory when the local taker public key
  is available; it MAY use the pubkey-blind factory only as a
  conservative estimate before that key is available.
- Watcher-only validation of an already-identified taker-fee
  transaction by hash, sender public key, age, confirmation
  boundary, and fee-output script is not required to recompute the
  swap-negotiated `DexFee` unless that watcher path also validates
  the expected swap fee amount or decides whether the fee is
  absent. If it does, this R12A trigger applies.

A direct unit test of `new_with_taker_pubkey` alone is not
sufficient acceptance coverage for this requirement; at least one
production call site MUST be exercised.

## 16.5 Bound Version-Two UTXO Helper Updates

**R13.** Each of the three taker-payment-spend helpers in the
version-two UTXO swap path MUST be updated. The chapter-bound
helper names are:

| Helper                                  | Bound role                                                       |
| --------------------------------------- | ---------------------------------------------------------------- |
| `gen_taker_payment_spend_preimage`      | Builds the preimage transaction the taker signs.                |
| `validate_taker_payment_spend_preimage` | Validates the preimage transaction the taker forwards.           |
| `sign_and_broadcast_taker_payment_spend`| Cooperative-branch maker signature and broadcast.               |

Each helper currently matches on the dex-fee variant with a
single arm for the standard variant and a catch-all arm returning
a deferred-variant rejection error carrying a chapter-15 deferral
string. R13 MUST replace the catch-all arm with two explicit arms
(`WithBurn` and `NoFee`) following R14 and R17. The compile-time
exhaustiveness check MUST then ensure all variants are handled
without a catch-all.

**R14.** For `DexFee::WithBurn { fee_amount, burn_amount,
burn_destination }`, the preimage builder of R13 MUST:

1. convert the fee-amount and burn-amount components to integer
   satoshi using the existing big-decimal-to-satoshi helper;
2. compute the maker-payout value as the taker-output value
   minus the satoshi fee, minus the satoshi burn, minus the
   HTLC-spend fee. If any subtraction underflows, return the
   previously-bound previous-output-too-low error variant;
3. build exactly three outputs in this order: output zero is a
   P2PKH to the maker address carrying the maker-payout value;
   output one is a P2PKH to the fee-collection address (derived
   from the chapter-06 network-level fee-address public-key
   accessor under the taker coin's chain configuration) carrying
   the satoshi fee; output two is the burn output constructed by
   R20;
4. sign under the signature-hash strategy of R15 and package as
   the existing helper does.

**R15.** The bound signature-hash strategy MUST be exactly:

| `DexFee` variant | Outputs at preimage time | Maker may append outputs? | Taker signature-hash flag |
| ---------------- | ------------------------- | -------------------------- | ------------------------- |
| `Standard`       | one (maker payout)        | yes, one fee output        | single-output flag (existing behaviour) |
| `WithBurn`       | three (maker, fee, burn)  | no                         | all-outputs flag          |
| `NoFee`          | one (maker payout)        | no                         | all-outputs flag          |

The signature-hash flag MUST be byte-exact: `WithBurn` and
`NoFee` MUST use the all-outputs flag combined with the coin's
fork identifier. `Standard` MUST keep the existing single-output
flag-plus-fork-identifier behaviour. This is the central reason
`WithBurn` cannot be implemented as the standard variant plus an
extra output: the signature-hash flag is part of the preimage,
and the validator MUST check the signature under the correct
flag.

**R16.** For `DexFee::NoFee`, the preimage builder of R13 MUST:

1. compute the maker-payout value as the taker-output value
   minus the HTLC-spend fee;
2. build exactly one output (P2PKH to the maker address);
3. sign under the all-outputs flag of R15 and package as the
   existing helper does.

**R17.** The validator of R13 MUST mirror the builder
construction. For `WithBurn` the expected output count MUST be
three; each output MUST be checked against its expected shape
(maker P2PKH, fee-collection P2PKH, burn output per R20). The
fee-budget tolerance MUST be the chapter-15-bound symmetric
margin on each output's value. For `NoFee` the expected output
count MUST be one. Signature verification MUST use the
signature-hash strategy of R15.

**R18.** The cooperative-branch signer-and-broadcaster of R13
MUST NOT append outputs for `WithBurn` or `NoFee`: all outputs
are already in the preimage (R14, R16). The maker MUST:

1. re-derive the same output set for fee-budget re-check;
2. sign under the all-outputs flag of R15;
3. assemble the cooperative-branch input script as the existing
   `Standard` path does, with the all-outputs flag byte combined
   with the coin's fork identifier on both signatures;
4. broadcast.

For the legacy `Standard` path the existing append-fee-output
behaviour MUST be preserved unchanged.

**R19.** The deferred-variant rejection arms in the three
helpers (carrying chapter-15's bound deferral string) MUST be
removed by the substrate. The compile-time exhaustiveness check
MUST then guarantee every `DexFee` variant is handled by R14,
R16, R17, R18.

**R20.** The burn-output construction routine MUST be a single
bound helper taking the satoshi burn amount, the chapter-08
burn-destination enumeration value, and the coin configuration.
The routine MUST handle the two bound destinations exactly:

| Destination                       | Bound output shape                                                                                                  |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| OP_RETURN destination             | An output with value zero and script `OP_RETURN <8-byte little-endian satoshi burn amount>`. The satoshi value is encoded into the script payload (not into the output value) because OP_RETURN outputs are conventionally zero-value; the chain still records the destruction because the value is locked in a script no one can spend. |
| Burn-account destination          | An output with the satoshi burn amount as its value and a standard P2PKH script derived from the burn-account public key via the existing address-from-public-key helper under the coin configuration. |

The substrate MUST NOT introduce a third burn-output shape.

## 16.6 Bound Activation Surface

**R21.** Activation for non-burn coins MUST be unchanged. The
defaults bound in R2 (empty vector, `false`, `false`) MUST
guarantee that activation behaviour for every coin family that
does not override is byte-for-byte identical to the
pre-substrate behaviour.

**R22.** Coin families that override (the matrix of R3) MUST be
updated by a single-method override per coin trait
implementation. The substrate MUST NOT require activation
plumbing changes (no new configuration field on the per-coin
activation request, no new central-context field).

**R23.** The parallel version-two EVM path bound by chapter 17
MUST NOT participate in the substrate. The EVM coin trait
implementation MUST return the R2 defaults; the factory of R6
MUST therefore always emit `Standard` for EVM-side dex-fee
delivery. EVM-side pre-burn integration is bound by chapter 17,
not by this chapter.

**R24.** The parallel Tendermint version-two path MUST NOT be
modified by the substrate. Tendermint's swap-operations module
already branches on the chapter-08 `WithBurn` variant and routes
the burn portion through a separate bank-message recipient; the
substrate consumes this prior work and is the structural
reference behavioural pattern that informed R14, R15, R16, R17
(split-output with explicit burn recipient, single-transaction
atomic delivery).

## 16.7 Tests

**T1.** *Burn-enabled coin produces the split variant.* The
factory of R6 is called with a mock coin returning
`should_burn_dex_fee = true` and a non-default burn public key;
the test asserts the returned variant is `WithBurn` with the
burn-account destination tag, fee-amount equal to the fee-share
fraction of the total, and burn-amount equal to the remainder.

**T2.** *Dust fallback emits the standard variant.* The factory
is called with a trade amount small enough that the burn leg
would fall below the coin's minimum-transmissible amount; the
test asserts the standard variant carrying the full fee is
returned.

**T3.** *KMD ticker emits OP_RETURN destination.* A mock coin
returning `should_burn_directly = true` is passed to the
factory; the test asserts the returned variant is `WithBurn`
with the OP_RETURN destination tag.

**T4.** *Taker public key equals burn public key yields no
fee.* The `new_with_taker_pubkey` factory is called with the
taker public key set equal to the coin's burn public key; the
test asserts the `NoFee` variant is returned.

**T4A.** *Known-taker-pubkey production validation uses `NoFee`.*
Given a burn-enabled UTXO taker coin whose expected taker sender
public key equals the burn public key, the V1 maker-side
taker-fee validation path computes its expected `DexFee` with the
taker-pubkey-aware factory and proceeds through the no-fee branch
without requiring a taker-fee transaction. The same fixture MUST
fail if the path computes the expected fee with the pubkey-blind
factory or with only the base-fee pipeline.

**T4B.** *V2 known-taker-pubkey validation uses `NoFee`.* Given a
V2 maker-side validation fixture after negotiation, where the
taker public key is known and equals the burn public key, the
expected `DexFee` passed to taker-funding validation is `NoFee`.
The test asserts that a `Standard` or `WithBurn` expectation is a
failure for this trigger condition. A companion taker-side fixture
MUST assert that local V2 taker construction uses the same
`NoFee` expectation when the local taker public key is available.

**T5.** *Three-output preimage for burn-account variant.* The
preimage builder of R13 is driven with `WithBurn` carrying the
burn-account destination tag; the test asserts the preimage
carries exactly three outputs, output zero is the maker P2PKH,
output one is the fee-collection P2PKH, and output two is a
P2PKH to the burn address for the burn-amount value.

**T6.** *OP_RETURN preimage shape for KMD path.* Same as T5 but
with the OP_RETURN destination tag; the test asserts output two
has value zero and a script beginning with the OP_RETURN opcode
followed by the eight-byte little-endian satoshi burn amount.

**T7.** *Taker partial signature verifies under the all-outputs
flag.* A `WithBurn` preimage is signed by the taker; the test
asserts the partial signature parses and verifies under the
all-outputs flag combined with the coin's fork identifier
against the cooperative-branch script.

**T8.** *Validator rejects a mutated burn output.* A `WithBurn`
preimage's burn output value is mutated; the validator of R13
is called; the test asserts the validator returns the
invalid-preimage error with a burn-output-value diagnostic.

End-to-end broadcast on a containerised test chain is not
bound here; the integration-test substrate is the appropriate
home for it.

## 16.8 Deferred Work

**D1.** EVM-side pre-burn. The EVM contract surface accepts a
single dex-fee numeric and does not split. Adding pre-burn to
the EVM path requires the EVM contract interface to grow a
burn-amount and burn-address parameter; that work is bound by
chapter 17.

**D2.** A network-level numeric burn-share override per coin
family (currently the bound network-level fee-share numeric
applies uniformly to every burn-enabled coin via R9).

**D3.** End-to-end broadcast coverage on a containerised UTXO
test chain. The bound tests of 16.7 are unit-level; the
containerised broadcast surface is owned by the integration-test
substrate and is not part of this chapter.

**D4.** A second OP_RETURN encoding form (for example a
variable-length integer rather than eight-byte little-endian) is
deferred. The current encoding (R20) is the smallest
representation that records the burned value on chain while
keeping the output value at zero.

## 16.9 Baseline Verifications

**V1.** The baseline tree MUST be confirmed to contain neither
the `WithBurn` nor the `NoFee` variant of the chapter-08
`DexFee` enumeration (chapter 08 binds the enumeration; this
chapter binds the variants' consumers). It MUST be confirmed
that no taker-payment-spend helper at baseline accepts a
non-single-output dex-fee shape.

**V2.** The baseline tree MUST be confirmed to contain none of
the bound coin-trait method names (`burn_pubkey`,
`should_burn_directly`, `should_burn_dex_fee`) and none of the
bound factory names (`new_from_taker_coin`,
`new_with_taker_pubkey`), nor any equivalent of the two
dex-fee split paths (OP_RETURN split and burn-account split).

**V3.** The chapter-15 version-two UTXO helpers' deferred-variant
rejection arms (carrying the bound deferral string) MUST be
confirmed present on the pre-substrate side and removed on the
post-substrate side. The substrate's effect on chapter 15 is
exactly that removal plus the addition of the explicit `WithBurn`
and `NoFee` arms per R14, R16, R17, R18.

## 16.10 External References

- Bitcoin script `OP_RETURN` semantics — standard transaction
  relay rules,
  <https://github.com/bitcoin/bips/blob/master/bip-0011.mediawiki>.
- Signature-hash type combinators
  (single-output / all-outputs, plus fork-identifier byte) —
  Bitcoin Core script verifier reference,
  <https://github.com/bitcoin/bitcoin/blob/master/src/script/interpreter.h>.
- Standard P2PKH script form — Bitcoin Core script reference.

## 16.11 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit; chapter 01 (clean-room rules); chapter 06 (network-level
  burn-enabled, fee-share, and burn-account-public-key accessors);
  chapter 08 (the `DexFee` enumeration, `DexFeeBurnDestination`
  enumeration, and `compute_dex_fee` pipeline); chapter 15 (the
  three version-two UTXO taker-payment-spend helpers and their
  deferred-variant rejection arms); chapter 17 (the parallel
  version-two EVM path, sibling-allowlist reference for D1);
  public Bitcoin script and signature-hash documentation.
- *Permitted-input classes used:* baseline source; bound substrate
  identifiers introduced with in-chapter justification; public
  protocol documentation.
- *Sibling-allowlist consultations:* none beyond the
  cross-chapter references listed in *Inputs*.
- *Forbidden corpus:* not consulted.
