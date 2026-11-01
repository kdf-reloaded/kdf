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
is the chapter-08 accessor `dex_fee_share`). The active netid-8762
share is 75% to the fee address and 25% to the burn destination. For
small KMD fees the fee leg is clamped up to the coin's dust and only the
excess is burned (chapter 08 R8).

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

The split substrate is shared, while the active policy is selected
by the dedicated network-configuration crate. Which reference
designation each of the two parameter sets below comes from, and why
selecting them per netid inside one binary is a KDF Reloaded
divergence rather than an inherited shape, is recorded in
[Chapter 06](06-network-id-seed-node.md) §6.6A and bound by that
chapter's §6.8 R17:

- **Network identifier 8762** is pinned to the `v2.6.0-beta`
  swap contract. The base rate is 1/777 and the discounted rate is
  9/7770 when either side of the pair is the exact ticker `"KMD"`.
  Coin dust is the sole fee floor. A KMD taker follows the three
  ranges of chapter 08 R8. A total at or below dust gives
  single-output `Standard(dust)`. Above that, the result is a
  two-output `WithBurn` to the fee address and the `KmdOpReturn`
  destination: 75/25 when the 75% leg reaches dust, otherwise a fee leg
  of exactly dust and a burn of the excess. The direct-burn predicate
  is evaluated before the inactive general burn-account predicate.
  Every non-KMD taker uses the single-output `Standard` descriptor.
  On the version-two protocol a swap with `"KMD"` on either side
  carries `NoFee` (R12B). A taker whose key equals the network burn
  key `0369aa10c061cd9e085f4adb7399375ba001b54136145cb748eb4c48657be13153`
  pays no fee (R7).
- **Network identifier 6133** follows the applicable v3/dev swap
  contract. The base rate is 2/100 and the discounted rate is
  1/100 when either side is the exact ticker `"GLEEC"`. Coin dust
  is again the sole floor. Burn is disabled, so KMD and non-KMD
  takers both use `Standard`, on both protocols. There is no
  version-two ticker exemption. The network burn key equals the fee
  key `03a778d9bd346fa704cf3e2508cd074d93a1bbc1e504fbecbb0a8d48e7cccbbf5c`,
  so a taker holding that key pays no fee (R7).

The issue-1 failure demonstrates why the descriptor shape is part
of the wire contract. For a 15.86 KMD trade on netid 8762, the
discounted total converts to 1,837,065 base units at eight
decimals. A legacy taker places 1,377,799 in the fee output and
459,266 in the OP_RETURN output. Treating the expected fee as one
1,837,065-unit standard output rejects that valid transaction.

On both networks the burn key serves two purposes. It is the
destination key of the dormant burn-account path (R9), which
nothing activates on either production network. It is also the
no-fee waiver key of R7, which is active on both networks. The
waiver does not depend on the network burn gate or on either
per-coin burn predicate, so it can hold while every burn split is
disabled.

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
| `should_burn_directly() -> bool`        | `true` iff this coin uses the direct `OP_RETURN` burn path (R20). This predicate is independent of, and has precedence over, the general burn-account predicate. |
| `should_burn_dex_fee() -> bool`         | `true` iff this coin participates in the general burn-account path. It does not disable a direct burn selected by `should_burn_directly`. |

**R2.** All three methods MUST have default implementations on
the coin trait. The defaults MUST be: empty vector, `false`,
`false` respectively. Coin families that do not participate in
the substrate inherit the defaults unchanged. The defaults
guarantee binary compatibility with every existing coin trait
implementation in the workspace.

**R3.** The bound per-coin override matrix is exactly:

| Coin family                                     | `burn_pubkey()`                                                          | `should_burn_directly()`                | `should_burn_dex_fee()`                            |
| ----------------------------------------------- | ------------------------------------------------------------------------ | --------------------------------------- | -------------------------------------------------- |
| UTXO standard                                   | empty (defers to the network-level burn-account public-key accessor at the factory layer per R4) | `true` for the chapter-bound ticker literal `KMD`, `false` otherwise | `false`                                            |
| UTXO Bitcoin-Cash family / SLP / QRC20          | empty                                                                    | `false`                                 | `false`                                            |
| EVM (Ethereum, ERC-20)                          | empty                                                                    | `false`                                 | `false`                                            |
| Tendermint                                      | empty; its swap-operations module can still consume an explicitly supplied descriptor | `false`                                 | `false`                                            |
| Lightning / NFT / SLP-token / Sia / Solana / Z-coin | empty                                                                    | `false`                                 | `false`                                            |

Substrate MUST NOT introduce a fourth burn-policy method, a
per-coin numeric burn-share, or any network-level override on the
per-coin booleans.

**R4.** The factory of R6 MUST honour a two-layer split between
coin-level opt-in and network-level gating:

- the network-level `burn_enabled` accessor is the first,
  network-layer gate;
- after that gate, `should_burn_directly` selects the direct
  OP_RETURN path before the factory considers the general
  `should_burn_dex_fee` burn-account opt-in;
- when the coin returns an empty `burn_pubkey`, the factory MUST
  fall back to the network-level burn-account public-key
  accessor; when the coin returns a non-empty value, the factory
  MUST prefer the coin's value; an empty resolved key disables the
  burn-account path.

**R5.** The chapter consumes the network-level accessors bound by
chapter 06 through the chapter-08 `compute_dex_fee` pipeline and
the factory of R6. It depends on exactly two chapter-06 policy
values beyond the rates and share: the burn-address public key,
which is also the no-fee waiver key of R7 and MUST be non-empty on
both production networks, and the version-two no-fee ticker set
of R12B. It MUST NOT add any other network-level accessor.

## 16.4 Bound `DexFee` Factory and Dust-Aware Split

**R6.** Two associated factory functions MUST be added to the
chapter-08 `DexFee` type. The chapter-bound names are:

| Function                  | Bound role                                                                                                                                 |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `new_from_taker_coin`     | Used at swap initiation, before the taker's public key is known. Decides `Standard` vs `WithBurn` (KMD-OP_RETURN vs burn-account) based on coin and network flags. |
| `new_with_taker_pubkey`   | Used whenever the taker's public key is known, including validation. Returns `NoFee` when the no-fee waiver of R7 applies; otherwise delegates to `new_from_taker_coin`. |

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
2. if the network-level `burn_enabled` accessor returns `false`,
   return the standard single-output variant carrying the entire
   base fee;
3. if the coin's `should_burn_directly` returns
   `true`, delegate to the OP_RETURN split helper of R8;
4. if the coin's `should_burn_dex_fee` returns `false`, return
   the standard variant;
5. otherwise resolve the burn-account key per R4; if it is empty,
   return the standard variant, and if it is non-empty delegate to
   the burn-account split helper of R9.

The bound decision tree for `new_with_taker_pubkey` MUST be
exactly:

1. **No-fee waiver.** If the taker coin is not a privacy coin (the
   shielded Zcash-family coins of chapter 39 are the privacy
   coins), and the resolved burn public key (R4: the coin's own
   value if non-empty, otherwise the network burn-address key) is
   non-empty and byte-for-byte equal to the taker public key,
   return `NoFee`;
2. otherwise, delegate to `new_from_taker_coin`.

Step 1 MUST NOT depend on the network burn gate, the direct-burn
predicate, or the general burn-account predicate. It applies to
every taker coin family and to both swap protocols. The compared
taker key is the taker's taker-coin swap public key: the one the
maker receives in negotiation, and the one the taker derives for
itself. For a non-privacy UTXO coin that is the activated key.
Both reference lineages waive this way: `v2.6.0-beta` on netid
8762 with key `0369aa…3153`, and the v3 lineage on netid 6133 with
key `03a778…bf5c` (full values in §16.1.1). The waiver exists so
that the key holder does not pay a fee on its own trades (R16
covers the version-two helper semantics).

**R8.** The OP_RETURN split path MUST take the base fee (the
dust-floored total of chapter 08 R6), the coin's
minimum-transmissible amount, and the network-level fee-share
numeric. It MUST return exactly the three-range result of chapter
08 R8:

- base fee at or below the minimum-transmissible amount: the
  standard variant carrying exactly the minimum-transmissible
  amount;
- otherwise, when `base_fee × fee_share` is at least the
  minimum-transmissible amount: `WithBurn` with fee amount
  `base_fee × fee_share`, burn amount `base_fee − fee_amount`, and
  the OP_RETURN destination tag;
- otherwise: `WithBurn` with fee amount equal to the
  minimum-transmissible amount, burn amount `base_fee −` that
  amount, and the OP_RETURN destination tag.

Netid 8762 supplies a share of 3/4. The fee leg of a direct-burn
`WithBurn` is therefore never below the minimum-transmissible
amount. The burn leg is always positive, because the base fee is
strictly above the minimum in the second and third ranges.

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

**R10.** The coin's `min_tx_amount` accessor is the sole
minimum-transmissible-amount authority. The direct OP_RETURN path
applies it as the range boundary and the fee-leg clamp of R8, which
preserves the netid-8762 legacy contract. The burn-account path
applies it to both split components. Both paths MUST fall back for
a non-positive component. The substrate MUST NOT carry a separate
dust configuration. Coin-layer taker-fee construction needs no
dust exception for any descriptor this factory emits (chapter 08
R15A). The builder exempts only `OP_RETURN` outputs, and it does so
because of their script.

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
public key MUST be treated as `NoFee` exactly as R7 step 1 binds,
on both networks and both protocols.

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

**R12B. Version-two ticker exemption (netid-selected).** On the
version-two swap protocol only, if the maker coin's ticker or the
taker coin's ticker is in the network's version-two no-fee ticker
set, then every version-two dex-fee computation of R12A MUST
yield `NoFee`. This covers the pubkey-blind estimate and the
pubkey-aware value, on both roles, before any step of R7. The set
is exactly {`"KMD"`} on netid 8762, following the `v2.6.0-beta`
version-two machines. It is empty on netid 6133, following the v3
lineage. The legacy version-one protocol never applies this
exemption: a netid-8762 legacy swap with KMD pays the chapter-08
R8 fee. Under the exemption the version-two taker funding carries
no fee component (chapter 15 R16), and the taker-payment spend
takes the `NoFee` layout of R16.

## 16.5 Bound Version-Two UTXO Taker-Payment-Spend Contract

This section binds the wire contract of the version-two UTXO
taker-payment spend for every dex-fee variant. Both reference
lineages share it (`v2.6.0-beta` for netid 8762, the v3 lineage for
netid 6133). Only the descriptor each network produces differs (R7,
R12B; chapter 08). In the rules below, `P` is the taker-payment
output value, `S` is the spend-fee estimate of R14A, and `fee_sat` /
`burn_sat` are the descriptor components converted to base units by
truncation. `ALL` and `SINGLE` are the all-outputs and single-output
signature-hash flags, each combined with the coin's fork identifier
(chapter 15 R40).

**R13.** The three taker-payment-spend helpers of the version-two
UTXO path MUST implement R14–R20 for all three variants, with no
catch-all arm. The chapter-bound helper names are:

| Helper                                  | Bound role                                                       |
| --------------------------------------- | ---------------------------------------------------------------- |
| `gen_taker_payment_spend_preimage`      | Builds the preimage transaction the taker signs.                |
| `validate_taker_payment_spend_preimage` | Validates the preimage transaction the taker forwards.           |
| `sign_and_broadcast_taker_payment_spend`| Cooperative-branch maker signature and broadcast.               |

**R14.** *Preimage outputs.* The taker's preimage MUST contain
exactly these outputs, in this order:

| Variant    | Preimage outputs (index: script, value) | Maker appends |
| ---------- | ----------------------------------------- | ------------- |
| `Standard` | 0: fee-collection P2PKH, `fee_sat` | one output: maker payout P2PKH, `P − S − fee_sat` (R18) |
| `WithBurn` | 0: fee-collection P2PKH, `fee_sat`; 1: burn output of R20, `burn_sat`; 2: maker payout P2PKH, `P − (fee_sat + burn_sat) − S` | nothing |
| `NoFee`    | 0: maker payout P2PKH, `P − S` | nothing |

The fee-collection P2PKH is derived from the active network's
fee-address public key (chapter 06, chapter 08 R16) under the
taker coin's address configuration. The maker payout P2PKH pays
the maker's taker-coin address. If any subtraction underflows, the
builder MUST return the previous-output-too-low error. Every
variant uses lock time zero, one input spending the taker-payment
output at index zero with sequence `0xFFFFFFFF`, and the coin's
configured transaction version and chain-specific fields. For a
coin with a transaction time field, that field is the preimage's
own.

**R14A.** *Spend-fee estimate.* `S` is computed from the coin's
current fee policy applied to a reference spend size of 496 bytes
(chapter 15 R40):

- *fixed per-kB rate `r`:* `floor(r × 496 / 1000)`, proportional
  and not rounded up to a whole kilobyte;
- *fixed per-kB rate with whole-kB rounding (the chapter-38
  R38.6.6a `dingo_fee` option):* `r` (one kilobyte);
- *dynamic rate `r`:* `floor(r × 496 / 1000)`, with no volatility
  increase at this stage;
- in all cases, if the coin forces the node's minimum relay fee,
  `S` is raised to at least `floor(relay_rate × 496 / 1000)`.

KMD's configured fixed rate of 1,000 base units per kB gives
`S = 496`. The same estimate governs the version-two taker
funding spend of chapter 15 R21/R22. Both sides compute `S`
independently, and the maker's exact-equality check (R17) requires
them to agree for `WithBurn` and `NoFee`.

**R15.** *Signature-hash flags.* The taker's partial signature and
the maker's signature over input zero MUST use:

| Variant    | Taker signature flag | Maker signature flag |
| ---------- | -------------------- | -------------------- |
| `Standard` | `SINGLE`             | `ALL`                |
| `WithBurn` | `ALL`                | `ALL`                |
| `NoFee`    | `ALL`                | `ALL`                |

Under `SINGLE`, the taker's `Standard` signature covers only output
0 (the fee output), so the maker can append its payout. The maker
always signs the final transaction under `ALL`. Each signature in
the script-sig carries its own flag byte, so the two bytes differ
for `Standard`. KMD has fork identifier zero, so the bytes are
`0x03` (`SINGLE`) and `0x01` (`ALL`).

**R16.** *No-fee variant.* For `NoFee`, the preimage holds only the
maker payout of R14, and both signatures use `ALL`. The version-two
taker funding for a `NoFee` trade carries no fee component
(chapter 15 R16).

**R17.** *Maker validation.* The maker MUST:

1. rebuild the expected preimage itself from the negotiated swap
   arguments, its own `S` (R14A), the descriptor recomputed with
   the taker's key (R12A, R12B) and, for coins with a transaction
   time field, the time value carried in the received preimage;
2. verify the taker's signature over the rebuilt preimage under
   the taker flag of R15;
3. require the received preimage to equal the rebuilt one exactly
   in every field: version, lock time, input outpoint, sequence,
   output count, output order, every value and every script.

No value tolerance applies. For `Standard` the check does not
depend on `S`, because the preimage has no maker output.

**R18.** *Maker finalisation.* For `Standard` the maker MUST first
require `S + dust + fee_sat ≤ P`, where `dust` is the coin's
dust amount. It then appends the maker payout of R14 as output 1.
For `WithBurn` and `NoFee` it MUST NOT add, remove or change any
output. In all cases it signs input zero under `ALL` and sets the
input script to exactly: the maker signature with its `ALL` byte,
the taker signature with its R15 byte, the maker secret, `OP_0`,
then the taker-payment redeem script (chapter 15 R9, R28). It then
broadcasts.

**R19.** No deferred-variant rejection arm may remain. The
compile-time exhaustiveness check MUST guarantee every `DexFee`
variant is handled by R14–R18.

**R20.** *Burn output.* A single burn-output routine MUST build
both destinations as follows:

| Destination | Output |
| --- | --- |
| `KmdOpReturn` | value = `burn_sat`; script = exactly the single opcode `OP_RETURN` with no data push. This is the same form as the legacy taker-fee burn output of chapter 08 R15B. |
| `PreBurnAccount` | value = `burn_sat`; standard P2PKH to the address derived from the burn public key under the coin configuration. |

No production network produces a version-two `WithBurn` descriptor
under the references. On netid 8762, KMD pairs are exempt (R12B)
and the burn-account path is inactive. On netid 6133 burn is
disabled. The `WithBurn` layout is still bound, so that the shared
helper stays wire-compatible if a descriptor of that form is ever
negotiated.

> **Compatibility correction (informative).** An earlier revision
> bound a different version-two contract, which no reference ever
> emitted:
> - a `Standard` preimage holding the maker payout under `SINGLE`,
>   with the maker appending the fee output;
> - the maker signing under the taker's flag;
> - a `WithBurn` order of maker, then fee, then burn;
> - a zero-value `OP_RETURN` burn output carrying an 8-byte amount
>   payload;
> - a ±10% value tolerance in validation;
> - a whole-kilobyte, 305-byte spend-fee estimate.
>
> The references have used the fee-first `Standard` preimage since
> their first version-two release. Consequently every version-two
> swap with a UTXO taker coin between this project and a reference
> node fails at preimage validation, on both networks, for any
> ticker. The rules above replace that contract.

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
MUST NOT participate in the split substrate. The EVM coin trait
implementation MUST return the R2 defaults, so the factory of R6
never emits `WithBurn` for EVM-side dex-fee delivery. It emits
`Standard`, or `NoFee` under R7 or R12B, because those are decided
before any per-coin predicate. EVM-side pre-burn integration is bound by chapter 17,
not by this chapter.

**R24.** The parallel Tendermint `WithBurn` implementation MUST
NOT be modified by the substrate. Tendermint has no version-two
swap-trait implementation ([Chapter 18 §18.5.1](18-tendermint-ibc-htlc.md#1851-pre-burn-dexfeewithburn-is-supported)
binds Tendermint as a V1-only counterparty of the V2 state-machine
driver); the existing `WithBurn` branch lives on the V1
swap-operations trait's taker-fee-send method and routes the burn
portion through a separate bank-message recipient. The substrate
consumes this prior work and is the structural
reference behavioural pattern that informed R14, R15, R16, R17
(split-output with explicit burn recipient, single-transaction
atomic delivery).

## 16.7 Tests

**T1.** *Netid-8762 compatibility matrix.* A KMD UTXO taker whose
total is above dust MUST produce `WithBurn` with the OP_RETURN
destination, split per the chapter-08 R8 ranges (75/25 when the
75% leg reaches dust). A KMD taker at or below dust MUST produce
`Standard(dust)`. A non-KMD UTXO taker MUST produce `Standard`.

**T2.** *Exact issue-1 regression.* For netid 8762, taker KMD,
maker CHTA, trade amount 15.86, and eight coin decimals, the test
MUST assert total 1,837,065 base units, fee leg 1,377,799, and burn
leg 459,266 after the coin-layer conversion.

**T2A.** *Small direct-burn regression (issue #11).* For netid
8762, taker KMD, and eight coin decimals: trade amount 0.01 MUST
give fee leg 1,000 and OP_RETURN burn leg 158 after conversion.
Trade amount 0.0084 MUST give `Standard` with a single 1,000 fee
output. Neither may use any builder dust exception. The full
range table and the validation-interop cases are chapter 08 T5A
and T5B.

**T3.** *Netid-6133 compatibility matrix.* KMD and non-KMD takers
MUST both produce `Standard` on both protocols when the taker key
is not the waiver key. The direct-burn coin predicate MUST have
no effect while the network gate is false.

**T4.** *Burn-account substrate remains guarded.* Direct helper
tests MUST cover the dormant burn-account 75/25 split and its
per-component dust fallback.

**T4A.** *No-fee waiver, legacy protocol, both roles, both
netids (R7).* Use a non-privacy UTXO taker coin whose taker key
equals the network burn key: `0369aa…3153` on 8762, `03a778…bf5c`
on 6133.
- *Taker:* the fee-send stage MUST broadcast nothing and MUST emit
  `TakerFeeSent` with the empty transaction identifier (chapter 51
  R27). Locked-amount and trade-preimage paths that know the key
  MUST report no dex fee.
- *Maker:* it MUST compute `NoFee`, MUST NOT decode a fee
  transaction, and MUST emit `TakerFeeValidated` with the empty
  identifier (chapter 51 R14).
- *Contrasts:* the same trade with any other taker key MUST charge
  the normal fee. On netid 8762 with a KMD taker coin, that is the
  chapter-08 R8 fee. A privacy-coin taker holding the waiver key
  MUST still pay the normal fee.
- *Scope:* at least one production call site per role MUST be
  exercised.

**T4B.** *No-fee waiver, version-two protocol, both roles, both
netids (R7, R12A).* Use a non-KMD UTXO pair (so R12B does not
apply) and a taker key equal to the network waiver key.
- *Taker:* it MUST fund with trading amount plus premium only.
- *Maker:* funding validation MUST expect exactly that value.
- *Spend:* the taker-payment-spend preimage MUST be the `NoFee`
  layout of R14. The maker MUST accept it under R17 and finalise
  it under R18 with both flags `ALL`.
- *Contrast:* the same pair with another taker key MUST use
  `Standard`.

**T5.** *Version-two ticker exemption (R12B).* On netid 8762, a
version-two swap with KMD as the taker coin, and separately with
KMD as the maker coin, MUST produce `NoFee` in both roles, both
before and after negotiation. On netid 6133 the same pairs MUST
produce `Standard`. On netid 8762 the legacy protocol for the same
pair MUST produce the chapter-08 R8 descriptor.

**T6.** *Version-two exact vectors, netid 8762, KMD taker (NoFee
by R12B).* Use eight decimals, premium zero, `S = 496` (KMD fixed
1,000 per kB), and fork identifier zero.

| Trade (KMD) | Funding output | `P` = funding − 496 | Preimage outputs | Taker / maker flag bytes |
| --- | --- | --- | --- | --- |
| 0.0084 | 840,000 | 839,504 | [maker 839,008] | `0x01` / `0x01` |
| 0.01 | 1,000,000 | 999,504 | [maker 999,008] | `0x01` / `0x01` |
| 1 | 100,000,000 | 99,999,504 | [maker 99,999,008] | `0x01` / `0x01` |

These are the three legacy ranges of chapter 08 T5A; on the legacy
protocol the same trades pay 1,000 / 1,000 + 158 / 86,872 + 28,957.
No fee or burn output may appear here.

**T7.** *Version-two exact vectors, `Standard` layout.* Use premium
zero, `S = 496`, and a taker coin with dust 1,000 and a fixed 1,000
per kB.
- *Netid 6133, KMD taker, non-GLEEC maker (rate 2/100):*

  | Trade (KMD) | Funding | `P` | Taker preimage | Maker-appended output 1 |
  | --- | --- | --- | --- | --- |
  | 0.0084 | 856,800 | 856,304 | [fee P2PKH to the 6133 fee key: 16,800] | maker 839,008 |
  | 0.01 | 1,020,000 | 1,019,504 | [fee 20,000] | maker 999,008 |
  | 1 | 102,000,000 | 101,999,504 | [fee 2,000,000] | maker 99,999,008 |

- *Netid 8762, non-KMD pair at rate 1/777, trade 1:* fee 128,700;
  funding 100,128,700; `P` 100,128,204; taker preimage [fee P2PKH
  to the 8762 fee key: 128,700]; maker-appended output maker
  99,999,008.
- In every row the taker flag byte MUST be `0x03` and the maker flag
  byte `0x01`. The final transaction MUST have exactly two outputs,
  fee first.
- A preimage that carries the maker output, or that puts the fee
  output at index 1, MUST be rejected by R17.

**T8.** *Version-two `WithBurn` layout (synthetic, R14, R20).* No
reference network produces this descriptor. Force a `KmdOpReturn`
descriptor with fee 86,872 and burn 28,957: the netid-8762 1 KMD
split, with funding 100,115,830 and `P` 100,115,334. The preimage
MUST be [fee P2PKH 86,872, bare-`OP_RETURN` output of value 28,957,
maker 99,999,009], with both flags `ALL`. A burn output with value
zero or a data push, or any other output order, MUST fail R17.
Mutating any single output value by one base unit MUST also fail
R17, because no tolerance applies.

**T9.** *Spend-fee estimate (R14A).* A fixed 1,000 per kB MUST
give `S = 496`, not 1,000 and not 305. A whole-kB-rounding fixed
rate `r` MUST give `r`. A dynamic rate MUST give
`floor(r × 496 / 1000)`.

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
applies uniformly to every active direct-burn or account-burn
path).

**D3.** End-to-end broadcast coverage on a containerised UTXO
test chain. The bound tests of 16.7 are unit-level; the
containerised broadcast surface is owned by the integration-test
substrate and is not part of this chapter.

**D4.** Withdrawn. The payload-carrying `OP_RETURN` encoding it
deferred alternatives to has been replaced by the reference form
of R20: a bare `OP_RETURN` carrying the burned value in the output
value.

**D5.** The version-two funding-spend preimage validation of
chapter 15 R22 uses a value-relative tolerance. The references
instead require the ratio of expected to actual spend fee to lie
within [0.9, 1.1], then compare the rebuilt preimage exactly.
The project's rule accepts every preimage a reference taker
builds. Once R14A is implemented, a reference maker accepts the
project's preimage. Aligning the tolerance itself is deferred as
non-blocking. It is recorded in UPSTREAM-PARITY-GAPS #6.

> **Implementation obligations (for the Coder; clean terms).**
> - *Dex-fee factory and V2 dex-fee selection:* the pubkey-aware
>   factory in the coins crate's fee-descriptor types module MUST
>   implement R7 step 1 with no gate conditions. The V2 dex-fee
>   selection helpers in `mm2src/mm2_main/src/lp_swap/dex_fee.rs`
>   MUST apply R12B first. The V2 maker and taker state machines
>   (`maker_swap_v2.rs`, `taker_swap_v2.rs`) MUST route every
>   dex-fee computation through them, including the pubkey-blind
>   estimate.
> - *Network configuration:* netid 8762 MUST expose the burn key
>   `0369aa…3153` and the V2 no-fee ticker set {`"KMD"`}. Netid
>   6133 keeps its burn key and an empty set. Both are chapter-06
>   accessors (see chapter 06).
> - *V2 UTXO helpers:* the preimage builder, validator and maker
>   finaliser in `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`
>   MUST be rewritten to R14–R18. The burn-output routine there MUST
>   follow R20. The spend-size constant MUST become 496, and the
>   spend-fee computation for these V2 spends MUST follow R14A
>   (proportional fixed rate). The fee-collection output MUST use
>   the active network's fee key (chapter 08 R16 finding).
> - *Tests to replace in `mm2src/coins/utxo/utxo_tests.rs`:*
>   - `should_build_taker_payment_spend_preimage_with_expected_output_to_maker_address`
>     (Standard layout);
>   - `should_build_taker_payment_spend_preimage_with_three_outputs_for_with_burn`;
>   - `should_build_taker_payment_spend_preimage_with_op_return_for_kmd_burn`;
>   - `should_reject_with_burn_preimage_with_wrong_burn_value`
>     (tolerance semantics);
>   - `should_not_waive_fee_for_inactive_burn_key_on_netid_6133`,
>     which MUST be inverted.
> - *Tests to review in `utxo_tests.rs`:*
>   `should_recover_partial_signature_from_taker_payment_spend_preimage`
>   and `should_recover_partial_signature_from_with_burn_preimage_under_sighash_all`,
>   against the new layouts and flags.
> - *Tests to replace in `dex_fee.rs`:*
>   `burn_disabled_network_does_not_waive_fee_for_burn_pubkey`,
>   which MUST be inverted. `t16_4a_…` and `t16_4b_…` MUST be
>   extended to T4A/T4B.
> - *Network-config tests:* `test_netid_8762_has_kmd_burn_policy`
>   in `mm2src/mm2_net_config/src/lib.rs` MUST also assert the
>   8762 burn key and the V2 ticker set.
> - *New tests:* T4A–T9 above.
> - *Operator docs:* `docs/NETWORK_CONFIG.md` (the 8762 and 6133
>   burn/waiver rows, the "inactive burn key" wording, and the V2
>   KMD exemption), `docs/GLEEC_COMPATIBILITY.md` and
>   `RELOADED_VS_GLEEC.md` if they describe version-two fee
>   handling, and a `CHANGELOG.md` entry.

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
that removal, the explicit `WithBurn` and `NoFee` arms of R14–R18,
and the corrected `Standard` layout and spend-fee estimate that
chapter 15 R26–R28 and R40 now defer to this chapter.

## 16.10 External References

- KDF Reloaded issue #11 — KMD swap failures on netid 8762 and
  6133, including the on-wire 1,000 + 158 taker fee of a
  `v2.6.0-beta` node, <https://github.com/kdf-reloaded/kdf/issues/11>.
- KDF Reloaded issue #1 and its attached public swap-failure record —
  observable netid-8762 KMD/CHTA validation failure,
  <https://github.com/kdf-reloaded/kdf/issues/1>.
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
  chapter 18 (the Tendermint `WithBurn` implementation's V1-only
  scope, referenced by R24); public Bitcoin script and
  signature-hash documentation; KDF Reloaded issue #1 and its
  public swap-failure attachment; KDF Reloaded issue #11 public logs
  and the controlled 2026-09-27 KMD mainnet runs. R5, R7, R8, R10,
  R12A, R12B, R13–R20, R23, T1–T9 and D4–D5 were re-derived on
  2026-09-27 under the chapter-01 two-team Spec Reader / Dirty Gate
  workflow (AGENTS.md §2), against the `v2.6.0-beta` (netid 8762)
  and v3-lineage (netid 6133) references. They are stated as
  observable wire contract only.
- *Permitted-input classes used:* baseline source; the present-day
  working tree (R24's Tendermint scope correction, re-verified
  against `mm2src/coins/tendermint/tendermint_swap_ops.rs` — no
  version-two swap-trait implementation exists for the Tendermint
  coin family); bound substrate identifiers introduced with
  in-chapter justification; public protocol documentation.
- *Sibling-allowlist consultations:* none beyond the
  cross-chapter references listed in *Inputs*.
- *Forbidden corpus:* not consulted.
- *Dirty Gate clearance (chapter 01 T1, R40, R43) for the §16.1.1
  cross-reference sentence:* the material this item covers is exactly one
  sentence — the cross-reference added at the head of §16.1.1, which
  points at chapter 06 §6.6A and is bound by that chapter's §6.8 R17.
  It was examined by the KDF Dirty Gate in the same pass that cleared
  chapter 06 §6.6A and **passed** on 2026-10-24: PASS,
  discretionary-expression measure 0.00, **leak-finding list empty**, and
  accordingly no remedy class for the sentence or for the candidate as a
  whole. The clearance is **complete and not pending**. The gate's
  confirmation is that the sentence **carries no independent claim**: it
  defers the whole of its substance to chapter 06 §6.6A and §6.8 R17 and
  introduces no new value of any kind. This item is written on the clean
  side from the verdict, which reached the clean side as relayed and
  transcribed text rather than as a gate-written file; that is
  record-keeping and adds no chapter content.

  *The one non-leak finding: a dependent restatement, not a standing
  claim.* The verdict's separately labelled non-leak section carried one
  advisory item, stated as a location, a category and one remedy class and
  nothing further: the added sentence at the head of §16.1.1 —
  dependent-restatement coupling — `none`. What it records is that one
  phrase of the sentence is a minimal dependent echo of a statement
  chapter 06 §6.6A.4 owns, so **this chapter inherits §6.6A's clearance at
  that phrase rather than standing alone on it**. The coupling resolves on
  this pass, because that clearance was granted in the same pass, which is
  why the class is `none` and no remediation is owed. The consequence for
  a later reader is that the sentence's standing is the standing of
  chapter 06 §6.6A: if that section's clearance were ever reopened, this
  sentence would travel with it. Chapter 06 §6.10 D5 does not reach this
  sentence, chapter 16 being inside the document set.

  **Only that sentence was gated, and this chapter's earlier clearance is
  untouched.** Nothing outside the added lines was examined or re-judged in
  this pass, and the chapter's earlier clearance is **not reopened**. The
  two fee-collection identity keys and the per-network fee parameters
  recorded below the added sentence in §16.1.1 predate this change, were
  confirmed only to be pre-existing rather than newly introduced, and keep
  the clearance they already had. The *Forbidden corpus* item above is
  unchanged by the pass: the added sentence took no forbidden-class input
  of its own, resting entirely on chapter 06 §6.6A, where that section's
  own input and clearance are recorded. The sentence carries no inline
  clearance pointer, which is a scope judgment rather than an oversight: it
  binds nothing, names in its own text the chapter and rule it depends on,
  and this footer is where the record lives.

  The three elements of the R39 scope attestation, carried forward here in
  corpus-free terms as chapter 01 T1 requires:

  - *Extent.* The forbidden-class material corresponding to the subject
    matter this chapter names in its own terms, confined to what a single
    cross-reference sentence could carry — the taker trading-fee rate
    surface and the fee-collection-identity surface — together with a
    check that the added lines newly introduce no fee-identity material.
    Quantity examined: **two units at two reference points**, for four
    unit-at-designation comparisons. Reference points: the published
    release designations `v2.6.0-beta` and `v3.0.0-beta`. **The record is
    no more precise than those designations:** no post-anchor commit
    identifier, no branch state, and no location within the compared
    material is given (chapter 01 R11 and the reference-version limit of
    §1.7).
  - *Method.* **Combined.** Reading, with mechanical retrieval used solely
    to address each comparison to a published release designation rather
    than to any materialised undesignated state. **This repository's own
    clean-room gate tooling was not applied and was not run:** no source
    block was in scope, so the instrument had no operand.
  - *Measure.* **0.00** on the discretionary-expression basis — the same
    figure the leak report carries, not a second one — with
    dictated-interop and pinned-interface content excluded from the
    measured set before the judgment was made. **Judgment standard read
    against:** a presence standard rather than a proportion — any genuine
    discretionary expression fails a candidate irrespective of volume. The
    measured set was empty, so the PASS rests on that and not on a numeric
    margin.
