# Chapter 15 — Atomic-Swap Version-Two UTXO Path

**Status:** driving-spec.

The binding specification for the UTXO concrete implementation of
the version-two atomic-swap trait surface: three Bitcoin scripts,
four coin-trait implementations, a delegated helper inventory in
the existing UTXO swap-helper module, and the dispatch wiring that
lets the chapter-14 generic state-machine substrate drive UTXO
swaps end-to-end.

## 15.1 Executive Summary

The version-two atomic-swap protocol replaces the version-one
single-payment hash-time-locked contract with a two-stage payment
flow on the taker side and a dual-secret hash-time-locked contract
on the maker side. The chapter-bound version-one-versus-version-two
contrast is exactly:

| Side                | Version one                                          | Version two                                                                                |
| ------------------- | ---------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Maker               | Single hash-time-locked contract bound to one secret hash | Hash-time-locked contract bound to *both* the maker-secret hash and the taker-secret hash |
| Taker               | Single hash-time-locked contract bound to the maker-secret hash | Two-stage funding-then-payment flow (a cooperative funding-spend converts funding into payment) |
| Dex-fee delivery    | Separate dex-fee transaction                          | Folded into the funding amount; funding-spend output routes to the dex-fee address         |
| Optional pre-burn   | Not applicable                                       | Optional pre-burn output funded by the same trade (chapter 16)                              |

The chapter-14 generic state-machine substrate is in place and the
version-two coin-trait surface is in place. The parallel EVM
implementation (chapter 17) is the only consumer at substrate
landing time. This chapter is the *driving specification* for the
UTXO concrete implementation: an implementer who reads this
chapter, the version-two trait definitions, and the existing
version-one UTXO swap helpers MUST be able to produce a working
UTXO version-two implementation.

Bound rules R1–R4 cover the trait surface and associated types;
R5–R10 cover the three Bitcoin scripts; R11–R20 cover the maker
trait methods; R21–R35 cover the taker trait methods; R36–R48
cover the common-trait derivations, the shared spend-construction
helper, the helper-inventory boundary, the numeric constants, the
state-machine wiring, the optional confirmation-gate policy, and
the hierarchical-deterministic and hardware-wallet address,
public-key, and signing contracts.

## 15.2 Subsystem Shape

The substrate occupies a structural seam between five chapters:

- chapter 14 (the generic storable state-machine runtime that
  drives every version-two swap);
- chapter 08 (the typed dex-fee enumeration and its three-variant
  closure plus the network-level numerics the
  taker-payment-spend helpers consume);
- chapter 16 (the pre-burn-output substrate that completes the
  `WithBurn` and `NoFee` taker-payment-spend arms this chapter
  defers);
- chapter 17 (the parallel EVM implementation; structural
  reference for trait conformance; the EVM contract surface
  handles spend authorisation on-chain, which is why the bound
  `skip_taker_payment_spend_preimage` flag of R34 differs across
  the two paths);
- chapter 05 (the secret-hash-algorithm discriminator chapter 05
  binds is consumed by the script builders here).

The substrate does *not* mutate the chapter-14 state-machine
shape, the chapter-08 enumeration, the chapter-17 EVM contract
surface, or the chapter-05 derivation helpers. It binds only the
UTXO-side concrete implementation of the coin-trait methods the
state machines call.

## 15.3 Bound Coin-Trait Implementation Surface

**R1.** The substrate MUST implement exactly four traits on the
chapter-bound coin type `UtxoStandardCoin`:

| Trait                     | Bound shape                                              |
| ------------------------- | -------------------------------------------------------- |
| `ParseCoinAssocTypes`     | Ten associated types plus six parse methods (R3, R4).    |
| `CommonSwapOpsV2`         | Two derivation methods (R36).                            |
| `MakerCoinSwapOpsV2`      | Five asynchronous methods (R11–R15).                     |
| `TakerCoinSwapOpsV2`      | Fourteen asynchronous methods plus one synchronous-flag accessor (R21–R35). |

The substrate MUST NOT modify the trait signatures or add a fifth
trait; the binding is implementation-only.

**R2.** All chapter-bound argument and result types
(`SendMakerPaymentArgs`, `ValidateMakerPaymentArgs`,
`SendTakerFundingArgs`, `GenTakerFundingSpendArgs`,
`GenTakerPaymentSpendArgs`, `TxPreimageWithSig`, `FundingTxSpend`,
`RefundMakerPaymentTimelockArgs`, `RefundMakerPaymentSecretArgs`,
`RefundTakerPaymentArgs`, `RefundFundingSecretArgs`,
`SpendMakerPaymentArgs`, `GenPreimageResult`,
`ValidateSwapV2TxResult`,
`ValidateTakerFundingSpendPreimageResult`,
`ValidateTakerPaymentSpendPreimageResult`,
`FindPaymentSpendError`, `SearchForFundingSpendErr`,
`SwapTxTypeWithSecretHash`) MUST be re-used unchanged from the
chapter-bound trait module.

**R3.** The ten associated types MUST be bound to the following
chapter-bound UTXO concrete types:

| Associated type        | Bound UTXO concrete type                                       |
| ---------------------- | -------------------------------------------------------------- |
| `Address`              | The chapter-bound UTXO address type.                          |
| `AddressParseError`    | The chapter-bound UTXO key-error type.                        |
| `Pubkey`               | The chapter-bound UTXO compressed-public-key type.             |
| `PubkeyParseError`     | The chapter-bound UTXO key-error type.                         |
| `Tx`                   | The chapter-bound UTXO transaction alias.                      |
| `TxParseError`         | The chapter-bound serialisation-error type.                    |
| `Preimage`             | A *new* local newtype `UtxoTxPreimage` wrapping the chapter-bound transaction-input-signer type (R4). |
| `PreimageParseError`   | The chapter-bound serialisation-error type.                    |
| `Sig`                  | The chapter-bound UTXO signature type.                         |
| `SigParseError`        | The chapter-bound UTXO key-error type.                         |

**R4.** The substrate MUST introduce two coherence-mechanic
adapters:

- a new local newtype `UtxoTxPreimage` wrapping the
  transaction-input-signer type. The newtype is required because
  the trait surface bounds the `Preimage` associated type by a
  byte-conversion marker trait with a blanket `AsRef<[u8]>`
  implementation; Rust's orphan rules forbid a direct adapter on
  the foreign signer type. The newtype MUST provide its own
  byte-conversion implementation that serialises the contained
  signer as a finalised transaction;
- two plain `AsRef<[u8]>` implementations on the chapter-bound
  UTXO public-key and signature types so the blanket
  byte-conversion implementation covers them. Both types already
  expose their bytes through dereference; the additional
  implementations are pure coherence adapters with no behavioural
  effect.

Parse methods MUST be exactly: compressed 33-byte public-key form
via the chapter-bound `from_slice` constructor; transactions via
the chapter-bound `deserialize` helper; preimages by deserialising
to a transaction and converting into a signer via the
chapter-bound conversion; signatures from the raw byte form used
by the existing UTXO swap code.

## 15.4 Bound Bitcoin Scripts

**R5.** The substrate MUST introduce exactly three Bitcoin scripts
in a new dedicated version-two swap-script module under the UTXO
crate. Each script MUST be wrapped
as pay-to-script-hash: the on-chain output is
`OP_HASH160 <ripemd160(sha256(redeem))> OP_EQUAL`, and the redeem
script is supplied at spend time via the script-sig.

**R6.** Secret-hash width MUST be uniform across the substrate:
secret hashes carried in arguments and encoded into scripts are
32-byte single-SHA-256 digests of the protocol secret. Script
builders MUST apply RIPEMD-160 *inside* the script-builder
boundary before pushing the resulting 20-byte digest into the
`OP_HASH160 <…> OP_EQUALVERIFY` check, so the on-chain comparison
is `OP_HASH160(secret) == RIPEMD-160(SHA-256(secret))` (the
chapter-bound double-hash form). The 32-byte width MUST match the
chapter-bound argument types.

**R7.** Locktimes MUST be encoded as 4-byte little-endian pushes,
matching the existing version-one UTXO swap-script convention.
Argument types carry locktimes as 64-bit values for cross-protocol
uniformity; the UTXO script builders MUST cast at the boundary;
locktimes outside the 32-bit range are not valid Bitcoin-script
values and MUST be rejected by the builder.

**R8.** The bound *taker-funding* script MUST have two outer
branches selected by a single `OP_IF`:

| Outer flag | Branch                                                   | Bound shape                                                                                                       |
| ---------- | -------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `OP_1`     | Refund by timelock.                                      | `<locktime> OP_CHECKLOCKTIMEVERIFY OP_DROP <taker_pub> OP_CHECKSIG`                                              |
| `OP_0`     | Cooperative path (further nested `OP_IF`):               |                                                                                                                   |
| `→ OP_1`   | Cooperative co-signature (funding-to-payment conversion).| `<taker_pub> OP_CHECKSIGVERIFY <maker_pub> OP_CHECKSIG`                                                          |
| `→ OP_0`   | Secret-reveal refund (taker reveals her own secret).      | `OP_SIZE <32> OP_EQUALVERIFY OP_HASH160 <ripemd160(taker_secret_hash)> OP_EQUALVERIFY <taker_pub> OP_CHECKSIG` |

The builder MUST take exactly four parameters: locktime,
taker-secret-hash (32 bytes), taker public key, maker public key.

**R9.** The bound *taker-payment* script MUST have two branches:

| Outer flag | Branch                                                   | Bound shape                                                                                                                                                  |
| ---------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `OP_1`     | Refund by timelock (taker reclaims after locktime).      | `<locktime> OP_CHECKLOCKTIMEVERIFY OP_DROP <taker_pub> OP_CHECKSIG`                                                                                          |
| `OP_0`     | Cooperative spend with maker-secret reveal.              | `OP_SIZE <32> OP_EQUALVERIFY OP_HASH160 <ripemd160(maker_secret_hash)> OP_EQUALVERIFY <taker_pub> OP_CHECKSIGVERIFY <maker_pub> OP_CHECKSIG`              |

The builder MUST take exactly four parameters: locktime,
maker-secret-hash, taker public key, maker public key.

**R10.** The bound *maker-payment* script MUST have three logical
branches encoded with nested `OP_IF`:

| Flags         | Branch                                          | Bound shape                                                                                                              |
| ------------- | ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `OP_1`        | Refund by timelock.                             | `<locktime> OP_CHECKLOCKTIMEVERIFY OP_DROP <maker_pub> OP_CHECKSIG`                                                     |
| `OP_0 / OP_1` | Taker spends with maker-secret reveal.          | `OP_SIZE <32> OP_EQUALVERIFY OP_HASH160 <ripemd160(maker_secret_hash)> OP_EQUALVERIFY <taker_pub> OP_CHECKSIG`        |
| `OP_0 / OP_0` | Maker immediately refunds with taker-secret reveal. | `OP_SIZE <32> OP_EQUALVERIFY OP_HASH160 <ripemd160(taker_secret_hash)> OP_EQUALVERIFY <maker_pub> OP_CHECKSIG`        |

The third branch — maker refunds with taker secret — is the bound
shape difference from version one and is what enforces the
atomicity property of the two-payment exchange: if the taker
abandons the swap after sending funding but before signing the
funding-spend, the maker cannot move forward; but if the maker has
already revealed her secret in a published spend, the taker can
refund via the funding script's secret-reveal branch and reclaim
without waiting for the timelock. The builder MUST take exactly
five parameters: locktime, maker-secret-hash, taker-secret-hash,
maker public key, taker public key.

## 15.5 Bound `MakerCoinSwapOpsV2` Methods

All maker methods return either the chapter-bound UTXO transaction
type wrapped in the chapter-bound transaction-error type, or the
chapter-bound validation-result type. The trait implementation on
the coin type MUST be a thin forwarder to a corresponding
chapter-bound helper in the existing UTXO swap-helper module
(R38).

**R11.** `send_maker_payment_v2` MUST: (1) derive the maker
hash-time-locked-contract keypair from the swap-unique data via
the chapter-bound version-two helper of R36; (2) parse the taker
public key via R4; (3) build the maker-payment script of R10 with
the chapter-bound time-lock, maker-secret-hash, taker-secret-hash,
maker public key, taker public key; (4) wrap as pay-to-script-hash
and build a single output whose value is the satoshi conversion of
the argument amount; (5) reuse the chapter-bound version-one
output-generation helper (generalised to accept the version-two
script) plus the broadcast helper; (6) return the resulting
transaction.

**R12.** `validate_maker_payment_v2` MUST: (1) derive the maker
hash-time-locked-contract public key from the argument; (2) call
the chapter-bound shared validation helper with the maker-payment
transaction, the bound default output index (R40), the maker and
taker public keys, the chapter-bound `MakerPaymentV2` variant of
the secret-hash-typed enumeration carrying both secret hashes, the
amount, the absent watcher-reward marker (UTXO version two does
not consume watcher rewards), the time-lock, the simplified
payment-verification deadline, and the confirmation count; (3) the
helper MUST reconstruct the expected redeem script from the
secret-hash-typed enumeration accessor, compare the output's
script-pubkey to its pay-to-script-hash form, verify the amount,
poll for confirmations, and (under the chapter-bound electrum
mode) verify the simplified-payment-verification proof.

The secret-hash-typed enumeration's `redeem_script()` accessor
MUST dispatch the three version-two variants
(`MakerPaymentV2`, `TakerPaymentV2`, `TakerFunding`) to their
respective script builders of R10, R9, R8.

**R13.** `refund_maker_payment_v2_timelock` MUST delegate to the
chapter-bound generic refund helper with: the payment
transaction, the `MakerPaymentV2` variant of the secret-hash-typed
enumeration, the parsed taker public key, the time-lock, the
script-data byte sequence `[OP_1]` selecting the outer timelock
branch, and the bound enabling sequence (the bound `SEQUENCE_FINAL
- 1` of R40). The helper MUST build a pay-to-script-hash spending
preimage, sign with the maker hash-time-locked-contract keypair,
assemble the script-sig as `[sig, OP_1, redeem_script]`, broadcast,
and return the transaction.

**R14.** `refund_maker_payment_v2_secret` is the bound *immediate*
refund path: maker reveals the taker's secret. The script-data
byte sequence MUST be `[taker_secret (32 bytes), OP_0, OP_0]` —
the secret push followed by two `OP_0` flags selecting the inner
"maker refunds with taker secret" branch of R10. Sequence MUST be
`SEQUENCE_FINAL` (R40). Time-lock MUST be zero. Signs with maker
keypair; broadcasts. This path lets the maker reclaim her own
funds the moment she learns the taker's secret (typically by
observing the taker's funding refund on-chain) without waiting for
the timelock.

**R15.** `spend_maker_payment_v2` is the taker spending the
maker's payment to extract the agreed coin; the witness MUST
reveal the *maker's* secret. The script-data byte sequence MUST be
`[maker_secret (32 bytes), OP_1, OP_0]` — secret push followed by
`OP_1` and `OP_0` flags selecting the inner "taker spends with
maker secret" branch of R10. Sequence MUST be `SEQUENCE_FINAL`.
Time-lock MUST be zero. Signs with the taker keypair; assembles
the final script-sig as `[taker_sig, maker_secret, OP_1, OP_0,
redeem_script]`; broadcasts. The maker-payment script's
secret-reveal branch carries only one signature requirement (the
taker's), because the cooperative two-signature shape is the
timelock-refund alternative for the maker.

## 15.6 Bound `TakerCoinSwapOpsV2` Methods

The fourteen asynchronous methods plus one synchronous-flag
accessor MUST follow the contracts below. All amount-to-satoshi
conversions go through the chapter-bound conversion helper using
the coin's decimals accessor.

**R16.** `send_taker_funding` MUST: (1) derive the taker
hash-time-locked-contract keypair from the swap-unique data via
R36; (2) compute the funding amount as the trading amount plus
the premium amount plus the dex-fee fee component (the chapter-08
`fee_amount` accessor); (3) build the taker-funding script of R8
and the pay-to-script-hash output; (4) reuse the chapter-bound
version-one output-generation and broadcast helpers; (5) return
the funding transaction.

**R17.** `validate_taker_funding` MUST: (1) parse the funding
transaction; (2) reconstruct the expected script via the R8
builder; (3) verify the output at the bound default index equals
the pay-to-script-hash form of the script; (4) verify the output
value equals the chapter-bound funding-amount formula (R16's
formula, converted to satoshi); (5) on the chapter-bound native
mode, call the chapter-bound address-import helper for the
pay-to-script-hash address so the node tracks spends; (6) return
the success arm of the bound validation-result enumeration.

**R18.** `refund_taker_funding_timelock` has the same shape as
R13 but on the funding transaction, with the `TakerFunding`
variant of the secret-hash-typed enumeration carrying the
taker-secret-hash, and script-data `[OP_1, OP_0]` (the funding
script's outer timelock branch is the true arm of the outer
`OP_IF`).

**R19.** `refund_taker_funding_secret` is the bound immediate
refund: taker reveals her own secret on the funding script's
inner secret-reveal branch. The script-data byte sequence MUST be
`[taker_secret, OP_0, OP_0]`. Signs with taker keypair;
broadcasts.

**R20.** `search_for_taker_funding_spend` MUST scan from the
caller-supplied starting block for any transaction spending the
funding output at the bound default output index. When found, it
MUST inspect the spend's script-sig at instruction index one
(after the signature) and dispatch to the chapter-bound
funding-spend classification:

| Inspected instruction       | Bound classification                                  |
| --------------------------- | ----------------------------------------------------- |
| `OP_1`                      | Timelock refund (`RefundedTimelock` variant).         |
| `OP_PUSHBYTES_32` (raw 32-byte push) | Secret refund (`RefundedSecret` variant), with the pushed bytes returned as the extracted secret. |
| Otherwise                   | Assumed cooperative spend; the funding has been converted to a taker-payment by the maker-signed funding-spend (`TransferredToTakerPayment` variant). |

On chapter-bound native UTXO chains without per-output spend
index, the substrate MUST use the existing version-one
spend-search polling pattern, adapted for the version-two
funding-script branch flags.

**R21.** `gen_taker_funding_spend_preimage` MUST generate the
unsigned transaction that, once both parties sign, converts the
funding output into the taker-payment output. It MUST: (1) build
the taker-payment script of R9 with the taker-payment time-lock
and the maker-secret-hash; (2) compute the funding-spend fee via
the chapter-bound coin-estimated fee-policy variant with the
bound default swap-spend transaction-size constant
of R40; (3) build a transaction-input signer spending the funding
output with a single pay-to-script-hash output for value
`funding_value - fee`; (4) set lock-time zero and sequence
`SEQUENCE_FINAL`; (5) sign the input with the taker
hash-time-locked-contract keypair using the chapter-bound
all-outputs sighash flag; (6) return the
preimage-plus-signature pair.

**R22.** `validate_taker_funding_spend_preimage` (maker side)
MUST re-derive the expected preimage as in R21, then: (1)
compare the preimage's input outpoint, output script, and output
value, allowing the chapter-bound symmetric 10% fee tolerance
(re-derive the fee both ways and check
`|preimage_value − expected| ≤ 0.1 × expected`); (2) verify the
supplied taker signature against the preimage's input
signature-hash for the funding-script cooperative co-signature
branch (not the timelock path), using the all-outputs
sighash digest; (3) return the success arm or the appropriate
chapter-bound error variant.

**R23.** `sign_and_send_taker_funding_spend` (taker side, having
received the maker's signature on top of the preimage) MUST: (1)
re-derive the preimage transaction exactly as in R21; (2) sign
the input with the taker keypair under the all-outputs sighash
flag; (3) build the script-sig as `[maker_sig (with sighash
byte), taker_sig (with sighash byte), OP_1, OP_0, redeem_script]`
— the two-flag pair selects the cooperative branch of the
funding script of R8; (4) set sequence and locktime as in R21;
(5) broadcast.

The funding-script cooperative branch uses sequential
`OP_CHECKSIGVERIFY` plus `OP_CHECKSIG` (not `OP_CHECKMULTISIG`);
the leading-stuffer pad common to multisignature spends MUST NOT
be emitted by the substrate.

**R24.** `refund_combined_taker_payment` is the bound timelock
refund of the taker-payment transaction (after funding was
already converted). Same machinery as R18, but with the
`TakerPaymentV2` variant of the secret-hash-typed enumeration
and script-data `[OP_1]` (the taker-payment script's outer
timelock branch is a single-flag selector under R9).

**R25.** `skip_taker_payment_spend_preimage` is the bound
synchronous-flag accessor. The UTXO implementation MUST return
`false`: UTXO requires a preimage exchange because the maker
MUST add her signature to the taker-payment-spend transaction
before broadcast. The EVM implementation returns `true` because
the EVM contract handles spend authorisation on-chain without
preimage exchange.

**R26.** `gen_taker_payment_spend_preimage` (taker side) MUST
generate the unsigned spend of the taker-payment output that,
once the maker signs and reveals her secret, transfers the
agreed amount to the maker and the dex-fee amount to the
dex-fee address. The construction MUST branch on the chapter-08
dex-fee variant:

- *`Standard(amount)` arm:* build a single-output preimage —
  maker's address receiving
  `taker_payment_value − dex_fee_amount − fee_estimate`. The
  dex-fee output is appended later by the maker in R28. Sign with
  the chapter-bound single-output sighash flag so the maker can
  append outputs without invalidating the signature.
- *`WithBurn` and `NoFee` arms:* deferred to chapter 16. The
  substrate MUST emit an explicit chapter-bound
  deferred-variant rejection error in this method and in R27, R28
  for the deferred arms; chapter 16 binds the deferred-arm
  replacement (R13–R20 of chapter 16).

**R27.** `validate_taker_payment_spend_preimage` (maker side)
MUST mirror R26: (1) re-derive the expected preimage; (2)
verify the taker signature against the appropriate
signature-hash (single-output for the `Standard` arm,
all-outputs for the chapter-16-bound arms); (3) for `Standard`,
allow that the preimage has only the maker-bound output and the
dex-fee output will be appended; (4) return the success arm or
the appropriate chapter-bound error variant.

**R28.** `sign_and_broadcast_taker_payment_spend` (maker side,
finalising with her secret) MUST: (1) start from the validated
preimage; (2) for `Standard`, append the dex-fee output (value
`dex_fee.fee_amount()`, address from the coin's chapter-bound
dex-fee-address configuration), with the new output's fee taken
out of the maker's share, not re-computed; (3) sign the
taker-payment input matching the sighash scheme the taker used;
(4) assemble the script-sig as `[maker_sig, taker_sig,
maker_secret, OP_0, redeem_script]` — the `OP_0` selects the
cooperative-with-secret branch of the taker-payment script of
R9; (5) broadcast.

For the chapter-16-bound `WithBurn` and `NoFee` arms, the
preimage already carries all outputs and the maker MUST NOT
append; see chapter 16 R18.

**R29.** `find_taker_payment_spend_tx` MUST poll the chain from
the caller-supplied starting block for any transaction spending
the taker-payment output. The polling interval MUST be ten
seconds. The deadline MUST be the caller-supplied
unix-seconds deadline. Returns the spending transaction, or the
chapter-bound timeout variant of the find-payment-spend error.

**R30.** `extract_secret_v2` MUST walk the spend transaction's
input at the bound default input index (R40). For each
`OP_PUSHBYTES_32` push, it MUST compute the chapter-bound
double-hash (RIPEMD-160 of SHA-256) over the push and compare
against the secret-hash argument (itself the double-hash of the
protocol secret). On match, MUST return the 32 raw bytes;
otherwise MUST return a chapter-bound "secret not found in spend
transaction" diagnostic.

**R31–R35** are reserved here for the chapter-bound trait-method
ordering required by a clean-room implementer: the methods
listed in R16–R30 cover the fourteen asynchronous methods plus
the one synchronous flag-accessor of R25, totalling the fifteen
bound `TakerCoinSwapOpsV2` items of R1.

## 15.7 Bound `CommonSwapOpsV2` Derivations

**R36.** The substrate MUST expose two derivation methods on the
common-swap-operations trait:

| Method                             | Bound role                                                                                                                                                  |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `derive_htlc_pubkey_v2(swap_unique_data)` | Returns the chapter-bound UTXO public-key type. Delegates to a *new* version-two signing-key/public-key accessor added alongside (not replacing) the version-one keypair accessor. |
| `derive_htlc_pubkey_v2_bytes(swap_unique_data)` | Returns the compressed 33-byte form for peer-to-peer transmission in the version-two negotiation messages.                                                  |

The new version-two accessor MUST dispatch on the chapter-05
key-pair policy:

- single-keypair mode uses the activated single keypair and
  returns its compressed public key;
- hierarchical-deterministic mode uses the currently enabled HD
  address recorded by the active HD wallet state and returns that
  address's compressed public key;
- Trezor/hardware-wallet mode uses the currently enabled
  hardware-wallet HD address recorded by the active HD wallet
  state and returns that address's compressed public key.

For HD and hardware-wallet modes, `swap_unique_data` MUST NOT
select a different local key. The per-swap data is retained for
protocol compatibility, but key isolation is not bound in this
chapter (D2).

The accessor MUST NOT synthesize a software key for a
hardware-wallet coin, MUST NOT derive from a host mnemonic, and
MUST NOT fall back to the single-address activation key. If an
enabled HD address with a cached public key is unavailable, every
fallible call path that needs the local HTLC public key MUST fail
before P2P negotiation, transaction construction, or broadcast
with a structured address-selection or hardware-wallet-readiness
error. Public trait methods that are infallible MUST only be
called after the activation or swap-start path has established
this invariant.

A distinct version-two accessor is required
because version one's accessor has a different signature and a
different behavioural contract under the
hierarchical-deterministic and hardware-wallet variants. The
substrate MUST NOT mutate the version-one accessor.

## 15.8 Bound Pay-to-Script-Hash Spend-Construction Helper

**R37.** The substrate MUST expose a single shared spending
helper — the version-two pay-to-script-hash spend-preimage
builder — that underlies every spend path (refund, cooperative
spend, funding-spend conversion). It MUST take: the coin handle,
the previous transaction, a chapter-bound locktime-selection
enumeration distinguishing a zero lock-time from a
check-locktime-derived 32-bit lock-time, a chapter-bound
time-field-selection enumeration distinguishing the
non-proof-of-stake case (time field unused) from the
proof-of-stake case (time field set to the current time), the
sequence, and the output set. The helper MUST: (1) spend the
previous transaction's output at the bound default output index
(R40); (2) set the lock-time per the locktime-selection variant;
(3) set the time-field per the time-field-selection variant; (4)
set the consensus branch identifier for the chapter-bound
Komodo/Zcash-family chains; (5) return an unsigned
transaction-input signer ready for the caller to sign in whatever
sighash mode the script branch requires. The signing step MUST be
performed by the caller using the chapter-bound version-one
pay-to-script-hash spend-finalisation helper, which produces the
final script-sig given the redeem script, a signature, and the
optional script-data prefix bytes.

## 15.9 Bound Helper-Inventory Boundary

**R38.** The trait-method implementations on the coin type MUST
be thin forwarders. The bound helper-inventory additions to the
existing UTXO swap-helper module MUST be exactly:

| Helper                                         | Bound forwarder                          |
| ---------------------------------------------- | ---------------------------------------- |
| `send_maker_payment_v2`                        | R11                                      |
| `spend_maker_payment_v2`                       | R15                                      |
| `refund_maker_payment_v2_secret`               | R14                                      |
| `send_taker_funding`                           | R16                                      |
| `validate_taker_funding`                       | R17                                      |
| `refund_taker_funding_secret`                  | R19                                      |
| `gen_taker_funding_spend_preimage`             | R21                                      |
| `validate_taker_funding_spend_preimage`        | R22                                      |
| `sign_and_send_taker_funding_spend`            | R23                                      |
| `gen_taker_payment_spend_preimage`             | R26                                      |
| `validate_taker_payment_spend_preimage`        | R27                                      |
| `sign_and_broadcast_taker_payment_spend`       | R28                                      |
| `extract_secret_v2`                            | R30                                      |
| Generic v1+v2 timelock-refund helper           | R13, R18, R24                            |
| Generic v1+v2 payment-validation helper (pre-existing) | R12                              |
| The version-two swap-fee keypair accessor      | R36                                      |

The substrate MUST extend the secret-hash-typed enumeration's
`redeem_script()` accessor to dispatch the three new variants
(`MakerPaymentV2`, `TakerPaymentV2`, `TakerFunding`) to the
script builders of R10, R9, R8.

## 15.10 Bound Numeric Constants

**R39.** All numeric values consumed by the substrate MUST be
sourced from chapter-bound named constants. Substrate MUST NOT
inline numeric magic; substrate MUST NOT introduce new constants
when an existing one applies.

**R40.** The chapter-bound constants and their bound values are
exactly:

| Constant                       | Bound value     | Bound purpose                                                                                            |
| ------------------------------ | --------------- | -------------------------------------------------------------------------------------------------------- |
| Default swap-output index constant | `0`         | Hash-time-locked-contract output index in every swap transaction.                                        |
| Default swap-input index constant  | `0`         | Input index that consumes a swap output in every spend transaction.                                      |
| Default swap-spend transaction-size constant | byte count sized for the three-output pre-burn case bound by chapter 16 | Estimated spend-transaction size for fee calculation.                          |
| `SEQUENCE_FINAL`               | `0xFFFFFFFF`    | Disables checklocktimeverify and check-sequence-verify checks; used for cooperative and secret-reveal branches. |
| `SEQUENCE_FINAL - 1`           | `0xFFFFFFFE`    | Enables checklocktimeverify check; used in timelock-refund spends.                                       |
| All-outputs sighash flag       | `0x01`          | Standard signature-hash for fully-fixed-outputs spends.                                                  |
| Single-output sighash flag     | `0x03`          | Signature-hash for the chapter-08 `Standard` taker-payment-spend preimage.                              |

All values either already exist in the chapter-bound UTXO
swap-helper module or in the chapter-bound underlying script
crate; the substrate MUST reuse them.

## 15.11 Bound State-Machine Dispatch and Kickstart Wiring

**R41.** The state machines never call coin methods directly
except through the version-two trait surface. The substrate MUST
NOT modify the chapter-14 state-machine bodies; the version-two
state-machine call sites in the chapter-bound version-two taker
and maker swap-machine modules dispatch into
R11–R30 unchanged because the state machines are generic over
the trait bounds.

**R42.** Once the trait implementations land on the coin type,
the chapter-14 kickstart-recovery handler MUST be extended. The
chapter-bound common version-two kickstart module's two
recovery handlers (one for the maker role, one for the taker
role) currently match only the
chapter-17 EVM variant of the chapter-bound coin enumeration.
Both MUST be extended to also match the UTXO variant and
dispatch to the same generic handler so interrupted UTXO
version-two swaps can resume after a restart. The substrate
MUST NOT introduce a new swap-type discriminant; the existing
maker/taker version-two discriminants are coin-agnostic, as is
the stored database representation.

## 15.12 Bound Preview and Confirmation-Gate Policy

**R43.** Every optional version-two confirmation gate controlled
by a `require_*_confirm` state-machine flag MUST wait for
`min(configured_confirmations, 1)` confirmations. A configured
confirmation count of zero MUST remain zero. This policy applies
only to the optional confirmation gates for taker funding, maker
payment, taker-payment spend, and maker-payment spend. It MUST
NOT change the normal non-optional payment confirmation waits
that enforce the order's configured confirmation policy.

**R44.** UTXO trade-preimage and taker-volume estimation, and
every version-two swap path that needs the local UTXO address
through the public version-two parse/coin interface, MUST derive
the sender address from the active derivation method. Under
single-address derivation it MUST use the activated single
address. Under hierarchical-deterministic derivation it MUST use
the currently enabled HD address recorded by the active HD wallet
state. It MUST NOT reconstruct an arbitrary root-public-key
address when a different HD address is enabled, and it MUST NOT
reject HD activation solely because a single-address private key
or single-address value is unavailable.

The interface contract is: callers receive the same address type
as the UTXO coin normally uses; the HD case is selected by the
coin's active derivation method; failure to read an enabled HD
address MUST be reported as a structured derivation/address
selection error by call sites that already have an error channel.
In public version-two interfaces that are currently infallible,
the implementation MUST ensure the enabled-address invariant
before invoking the infallible accessor.

For Trezor/hardware-wallet activation, the active address is the
address selected by the UTXO activation parameters for the
hardware-backed HD wallet. The selected address record MUST carry:
the displayable address, the compressed public key, and the full
BIP-32 derivation path from the hardware wallet's master node to
that address. That same path is the signing path for every
version-two HTLC signature made by the local side.

## 15.13 Bound Trezor/Hardware-Wallet Support

**R45.** Trezor/hardware-wallet UTXO version-two swaps MUST use
the active enabled hardware-wallet HD address as the local HTLC
identity. The local HTLC public key sent in version-two
negotiation messages and embedded in the maker-payment,
taker-funding, and taker-payment scripts MUST be the compressed
public key stored with that enabled address. The implementation
MUST NOT request or expose private key material, MUST NOT use a
host-side substitute key, and MUST NOT derive a different public
key from the root public key when the user has enabled a specific
address path.

The enabled address path MUST be selected before the first
version-two negotiation message that carries the local HTLC
public key. If the enabled hardware-wallet address cannot be
read, if it has no compressed public key, if the derivation path
is missing, if the configured coin has no hardware-wallet coin
mapping, or if the initialized hardware wallet is not the
expected device, the swap start MUST fail before negotiation with
a structured hardware-wallet or address-selection error.

**R46.** Every wallet-funded transaction in a Trezor/hardware-
wallet UTXO version-two swap MUST be signed by the hardware
wallet. This includes maker-payment creation and taker-funding
creation. Each wallet-controlled input supplied to the device
MUST carry the derivation path and public key for the address
that controls that input. Outputs that pay to the version-two
pay-to-script-hash HTLC addresses are ordinary external outputs
from the device's point of view. Change outputs, when present,
MUST be identified as change by derivation path; otherwise they
MUST be presented as external outputs. The host MUST reject the
operation if it cannot provide complete derivation-path metadata
for every wallet-controlled input.

**R47.** Every version-two HTLC spend that requires the local
party's signature MUST be authorized by the hardware wallet using
the enabled address path bound by R44 and R45. This applies to:
maker timelock refund, maker secret refund, taker spending the
maker payment, taker funding timelock refund, taker funding
secret refund, taker funding-spend preimage generation, taker
funding-spend finalization, taker-payment timelock refund,
taker-payment-spend preimage generation, and maker finalization
of the taker-payment spend.

The host is responsible for constructing the version-two redeem
script and final script-sig or witness stack exactly as specified
by R8, R9, R10, R13–R15, R18, R19, R21, R23, R24, R26, and R28.
The hardware wallet is responsible only for authorizing the
transaction digest for the enabled address path and returning a
signature. The signature-hash flag MUST match the branch contract:
all-outputs for fully fixed transactions and single-output for
the chapter-08 `Standard` taker-payment-spend preimage. The final
transaction MUST be rejected before broadcast if the returned
signature count is wrong, the returned signature does not verify
against the enabled address public key and expected digest, or
the completed script does not spend the expected HTLC output.

**R48.** Hardware-wallet user actions and errors MUST surface
through the same observable task/swap status channel used by
other hardware-wallet operations. During version-two swap
activation and signing, callers MUST be able to distinguish at
least: waiting for device connection, PIN entry required,
passphrase entry required, on-device confirmation required, user
cancellation or rejection, device disconnected, unexpected
device, unsupported coin mapping, unsupported script/signing
mode, transport failure, and invalid device response. These
conditions MUST fail the current swap step without panicking and
MUST leave already published transactions recoverable through the
chapter-14 kickstart flow.

If the connected firmware or coin mapping cannot sign the P2SH
HTLC input form required by this chapter, the implementation MUST
fail at the first HTLC-signing step with a structured unsupported
script/signing-mode error. It MUST NOT broadcast a partially
signed HTLC spend, MUST NOT ask the user to export a private key,
and MUST NOT fall back to software signing. Wallet-funded sends
MAY still be supported for the same coin, but that is not
sufficient to advertise end-to-end version-two swap support.

## 15.14 Tests

**T1.** *Script-layout invariants.* Three unit tests build each
of the three scripts (R8, R9, R10) with known inputs and assert
the resulting byte sequence matches the bound shape table
opcode-for-opcode and push-for-push.

**T2.** *Maker-payment validation against a known-good
transaction.* A previously-generated maker-payment transaction
is validated through R12; the test asserts the success arm.

**T3.** *Maker-payment rejection on amount drift.* A
maker-payment transaction is mutated to carry the wrong amount;
the validator returns the bound amount-mismatch error variant.

**T4.** *Secret extraction.* A taker-payment-spend transaction
that reveals the maker's secret is parsed through R30; the test
asserts the extracted bytes equal the protocol secret.

**T5.** *Funding-spend classification — timelock refund.* A
spend whose script-sig instruction one is `OP_1` is classified
through R20; the test asserts the timelock-refund variant.

**T6.** *Funding-spend classification — secret refund.* A spend
whose script-sig instruction one is a raw 32-byte push is
classified through R20; the test asserts the secret-refund
variant carrying the pushed bytes.

**T7.** *Funding-spend classification — cooperative spend.* A
spend whose script-sig instruction one is neither of the above
is classified through R20; the test asserts the
transferred-to-payment variant.

**T8.** *Containerised swap coverage.* The chapter-bound
container-test substrate MUST be extended to cover the
end-to-end UTXO version-two swap in three scenarios: happy
path, taker abort after funding, maker abort after payment.
Coin-configuration entries for the UTXO version-two path MUST
be added; the version-two state-machine integration-test
scaffolding MUST be reused.

**T9.** *Optional confirmation-gate cap.* A unit test exercises
the version-two confirmation-gate helper and asserts that zero
remains zero, one remains one, and values greater than one are
capped to one.

**T10.** *HD trade-preview sender derivation.* A unit test
constructs an HD UTXO coin field set with a known enabled HD
address and asserts that trade-preimage sender derivation returns
that enabled address. The test MUST use an enabled address that is
distinguishable from the address that would be obtained by
blindly applying the default UTXO address format to the root
activated public key. The test asserts that HD estimation does not
require a single-address private key or single-address value.

**T11.** *HD version-two local address contract.* A unit test
constructs a version-two-capable UTXO coin under HD derivation
with a known enabled HD address and asserts that the public
version-two local-address accessor returns that same enabled
address. A companion error-path test exercises an HD wallet state
with no readable enabled address and asserts a structured
address-selection failure at the nearest fallible call boundary,
with no swap negotiation message emitted and no transaction
constructed.

**T12.** *Hardware-wallet HTLC public-key derivation.* A unit test
activates a UTXO coin under a Trezor/hardware-wallet key policy
with a known enabled hardware-wallet HD address. The test asserts
that `derive_htlc_pubkey_v2_bytes` returns the compressed public
key stored with that enabled address, that a different enabled
address path produces a different local HTLC public key when the
device account data differs, and that no software fallback key is
used.

**T13.** *Hardware-wallet active-path selection.* A task-level
test activates a hardware-backed UTXO coin with a non-default
account/change/address-index path and starts version-two
negotiation. The emitted local HTLC public key and local sender
address MUST correspond to the selected enabled path, not to the
coin root, account root, first external address, or any
single-address activation value. The error-path variant activates
without a readable enabled address and asserts that no
negotiation message or transaction is emitted.

**T14.** *Trezor-emulator wallet-funded sends.* An emulator-backed
acceptance test runs maker-payment creation and taker-funding
creation for a Standard UTXO version-two swap. The test MUST
assert that every wallet-controlled input sent to the emulator has
the correct derivation path, every HTLC output matches the R8 or
R10 pay-to-script-hash address built from the enabled hardware
public key, user-action statuses are observable while the device
is waiting, and the resulting transaction is accepted by the
regtest or container node.

**T15.** *Trezor-emulator HTLC signing.* An emulator-backed
acceptance test covers every local HTLC signature required by the
happy-path Standard UTXO version-two swap: taker funding-spend
preimage, taker funding-spend finalization, maker taker-payment
finalization, and taker maker-payment spend. The test MUST verify
the returned signatures against the enabled address public key and
the expected signature-hash mode, inspect the final script-sig
branch selectors, broadcast the completed transactions, and
complete the swap.

**T16.** *Trezor-emulator refund and abort paths.* Emulator-backed
acceptance tests cover maker timelock refund, maker secret refund,
taker funding timelock refund, taker funding secret refund, and
taker-payment timelock refund. Each test MUST assert that the
device is asked to sign with the enabled address path, that the
branch selectors and revealed secret pushes match R13, R14, R18,
R19, and R24, and that restart recovery can continue from the
last published transaction.

**T17.** *Hardware-wallet negative paths.* Task-level tests
exercise: no initialized device, unexpected device, user
cancellation, disconnected device during signing, unsupported
coin mapping, unsupported P2SH HTLC signing mode, missing
derivation path metadata for a wallet input, invalid signature
count, and a signature that fails verification against the enabled
address public key. Each condition MUST surface as a structured
hardware-wallet or signing error, MUST NOT panic, and MUST NOT
broadcast a transaction.

## 15.15 Deferred Work

**D1.** Chapter 16's `WithBurn` and `NoFee` arms of R26, R27,
R28. The substrate emits explicit deferred-variant rejection
errors in those arms; chapter 16 binds the replacement.

**D2.** Per-swap hierarchical-deterministic key isolation. The
substrate currently does not thread the swap-unique data into
derivation; the same coin yields the same
hash-time-locked-contract keypair for every swap under a given
key-pair policy. Per-swap key isolation is deferred.

**D3.** Watcher-reward consumption on UTXO version two (R12
passes the absent marker). Watcher-reward integration is
deferred.

**D4.** Native-mode address-import parity. R17 performs the
chapter-bound address-import call on validation; R12 does not.
A future revision MUST decide whether both paths should import
or neither.

**D5.** Threading the swap-unique data into the funding-spend
argument types
(`GenTakerFundingSpendArgs` /
`ValidateTakerFundingSpendPreimageArgs`). The helpers currently
pass an empty slice to the keypair helper; this is harmless
under the current single-keypair derivation but blocks D2.

## 15.16 Baseline Verifications

**V1.** The baseline tree MUST be confirmed to contain none of
the four trait implementations of R1 on the coin type. The
trait surface itself exists at baseline; only the UTXO
implementation is absent.

**V2.** The baseline tree MUST be confirmed to contain none of
the three script builders of R8–R10 and none of the helper-
inventory additions of R38. The script-builder module of R5
MUST be absent at baseline.

**V3.** The baseline tree MUST be confirmed to contain the
chapter-14 state-machine substrate, the chapter-17 EVM
implementation, and the chapter-08 dex-fee data substrate so
the substrate has all preconditions to land. The substrate's
effect on chapter 14 and chapter 17 is exactly the kickstart-
handler match-arm extension of R42 and the EVM-side
non-participation noted in chapter 16 R23.

## 15.17 External References

- Bitcoin opcode semantics —
  <https://en.bitcoin.it/wiki/Script>.
- BIP-65, *checklocktimeverify* —
  <https://github.com/bitcoin/bips/blob/master/bip-0065.mediawiki>.
- Signature-hash flag semantics —
  <https://en.bitcoin.it/wiki/OP_CHECKSIG>.
- BIP-16, *pay-to-script-hash* —
  <https://github.com/bitcoin/bips/blob/master/bip-0016.mediawiki>.

## 15.18 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit; chapter 01 (clean-room rules); chapter 05 (the
  secret-hash-algorithm discriminator and the key-pair policy
  discriminator the script builders and the derivation helper
  consume); chapter 08 (the typed dex-fee enumeration, the
  three-variant closure, the `fee_amount` accessor); chapter 14
  (the generic storable state-machine runtime that drives every
  version-two swap); chapter 16 (the pre-burn-output substrate
  that completes the deferred arms of R26, R27, R28); chapter 17
  (the parallel EVM implementation; structural reference for
  trait conformance, kickstart-handler reference for R42); the
  version-two trait definitions and argument-and-result type
  enumerations carried forward unchanged from the baseline trait
  module; public Bitcoin-script and signature-hash documentation.
- *Permitted-input classes used:* baseline source; bound substrate
  identifiers introduced with in-chapter justification; public
  protocol documentation; restricted behavior-analysis corpus
  consulted only for functional/interface behavior.
- *Sibling-allowlist consultations:* none beyond the cross-chapter
  references listed in *Inputs*.
- *Restricted corpus output policy:* no source text, private
  helper decomposition, private identifiers, or implementation
  bodies are carried into this chapter.
