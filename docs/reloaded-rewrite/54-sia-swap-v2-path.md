# Chapter 54 — Siacoin Atomic-Swap Version-Two Path

**Status:** DRAFT. Not yet approved for implementation. This chapter closes
[Chapter 20](20-siacoin-integration.md)'s §20.10 D5 ("V2 swap protocol").

> **Provenance note (read before anything else in this chapter).** Unlike
> most chapters in this set, this chapter is **not** a port of an existing
> upstream/baseline implementation gated through `KDF Spec Reader`/
> `KDF Dirty Gate`. No Sia V2 atomic-swap implementation exists in the
> restricted behavior-analysis corpus this project's clean-room process
> otherwise draws from (Sia was never wired to the version-two swap
> protocol upstream). This chapter is **original design**, binding the
> already-specified, coin-generic V2 trait surface
> (`mm2src/coins/lp_coins_traits.rs`) to Siacoin's already-bound V1
> primitives ([Chapter 20](20-siacoin-integration.md) §20.4–§20.8) and to
> the already-shipped V2 substrate ([Chapter 14](14-state-machine-runtime.md)
> and its V2-swap specialization, [Chapter 52](52-swap-v2-state-machine.md)).
> Every requirement
> below is traceable to one of: (a) an already-bound Sia V1 fact this
> project already implements and has tested, (b) the coin-generic V2
> trait/type definitions as they exist in this repository today, or (c) a
> verified capability of the pinned `sia_rust` client library. Nothing
> here is inferred from what any other coin's V2 path happens to do beyond
> using it as a structural template — Sia's on-chain mechanism (native
> spend policies) is different enough from both Bitcoin script (chapter
> 15) and EVM contracts (chapter 17) that most binding content had to be
> derived from Sia's own primitives directly. This chapter reuses the
> already-shipped, coin-generic state-machine runtime ([Chapter
> 14](14-state-machine-runtime.md)) and its V2-swap specialization
> ([Chapter 52](52-swap-v2-state-machine.md)) as-is. Sections the drafter
> genuinely could not pin down as a single dictated answer are marked
> **OPEN QUESTION** rather than silently resolved — these are exactly the
> points this draft is being circulated for before any implementation
> pass begins.

> **Binding scope (R54.0).** Requirements bind the mapping from the
> coin-generic V2 trait surface's associated types and methods to Sia
> concrete types, spend-policy trees, and wire behavior. The V2 trait
> definitions themselves (`ParseCoinAssocTypes`, `CommonSwapOpsV2`,
> `MakerCoinSwapOpsV2`, `TakerCoinSwapOpsV2`, and their argument/result
> types) are bound unchanged from the existing baseline — this chapter
> does not modify them. Sia's already-bound V1 facts ([Chapter 20](20-siacoin-integration.md))
> are load-bearing inputs, not re-litigated here.

## 54.1 Executive Summary

The version-two atomic-swap protocol ([Chapter 52](52-swap-v2-state-machine.md))
replaces V1's single-payment hash-time-locked contract with a two-stage
taker-side payment flow and a dual-secret maker-side contract, exactly as
already bound for UTXO ([Chapter 15](15-swap-v2-utxo-path.md) §15.1) and EVM
([Chapter 17](17-swap-v2-evm-path.md) §17.0):

| Side             | V1 (Sia today, [ch.20](20-siacoin-integration.md) §20.6–§20.7) | V2 (this chapter)                                                                          |
| ---------------- | ---------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| Maker            | Single spend policy bound to one secret hash                     | Spend policy bound to *both* the maker-secret hash and the taker-secret hash                 |
| Taker            | Single spend policy bound to the maker-secret hash                | Two-stage funding-then-payment flow (a cooperative funding-spend converts funding into payment) |
| Dex-fee delivery | Separate dex-fee transaction (R-S1)                               | Folded into the funding amount; funding-spend output routes to the dex-fee address           |
| On-chain mechanism | Native `SpendPolicy` tree (§20.6), no script/contract           | Same mechanism — deeper `SpendPolicy::Threshold` trees, no new on-chain primitive needed      |

The load-bearing fact this whole chapter rests on: Sia's `SpendPolicy` enum
(`Above`, `After`, `PublicKey`, `Hash`, `Threshold{n, of}`, verified against
the pinned `sia_rust` source, `types/spend_policy.rs`) is expressive enough
to encode every branch V2 needs using the *same* primitives V1's
`SpendPolicy::atomic_swap`/`atomic_swap_refund` already use in production
(§54.4) — no new on-chain capability, no upstream feature request, and no
corpus consultation was needed to establish this.

## 54.2 Subsystem Shape

This chapter occupies the same structural seam [Chapter 15](15-swap-v2-utxo-path.md)
§15.2 and [Chapter 17](17-swap-v2-evm-path.md) §17.1 already describe for
UTXO/EVM:

- the generic storable state-machine runtime ([Chapter 14](14-state-machine-runtime.md))
  and its V2-swap specialization ([Chapter 52](52-swap-v2-state-machine.md))
  drive every V2 swap through the coin-generic trait surface; this chapter
  does not touch either of them;
- [Chapter 20](20-siacoin-integration.md) is the load-bearing input: its
  §20.4 (key derivation), §20.6 (HTLC-as-native-spend-policy), and its now-
  closed D3/D4/D7 deferred-work items (swap-spend search, message signing,
  watcher eligibility) are all directly reused, not re-derived, by this
  chapter;
- [Chapter 15](15-swap-v2-utxo-path.md) and [Chapter 17](17-swap-v2-evm-path.md)
  are structural precedent only — Sia's on-chain mechanism is closer to
  UTXO's (a native locking condition evaluated against a fixed set of
  primitives) than to EVM's (a deployed contract with function selectors),
  so §54.4's spend-policy trees are modeled on chapter 15's three Bitcoin
  scripts' *branch semantics*, translated to `SpendPolicy` combinators, not
  copied as script bytecode;
- dispatch wiring touches `mm2src/mm2_main/src/lp_swap/swap_v2_common.rs`'s
  existing `(MmCoinEnum, MmCoinEnum)` match matrix (§54.9) — verified by
  reading that file's current `EthCoin`/`UtxoCoin` arms, not assumed;
- [Chapter 46](46-sia-v2-activation-rpcs.md) (Sia's task-based activation
  RPC surface) is **not** touched by this chapter — V2 swap eligibility is
  a per-swap protocol-negotiation concern ([Chapter 13](13-swap-version-negotiation.md)),
  not an activation-time concern.

## 54.3 Bound Coin-Trait Implementation Surface

**R54.1.** The implementation MUST implement exactly four traits on
`SiaCoin`: `ParseCoinAssocTypes`, `CommonSwapOpsV2`, `MakerCoinSwapOpsV2`,
`TakerCoinSwapOpsV2` (all defined in `mm2src/coins/lp_coins_traits.rs`,
unchanged by this chapter). All argument/result types
(`SendMakerPaymentArgs`, `ValidateMakerPaymentArgs`, `SendTakerFundingArgs`,
`GenTakerFundingSpendArgs`, `GenTakerPaymentSpendArgs`, `TxPreimageWithSig`,
`FundingTxSpend`, `RefundMakerPaymentTimelockArgs`,
`RefundMakerPaymentSecretArgs`, `RefundTakerPaymentArgs`,
`RefundFundingSecretArgs`, `SpendMakerPaymentArgs`, `GenPreimageResult`,
`ValidateSwapV2TxResult`, `ValidateTakerFundingSpendPreimageResult`,
`ValidateTakerPaymentSpendPreimageResult`, `FindPaymentSpendError`,
`SearchForFundingSpendErr`, `SwapTxTypeWithSecretHash`, all in
`mm2src/coins/lp_coins_types.rs`/`lp_coins_errors.rs`) MUST be reused
unchanged.

**R54.2.** The ten `ParseCoinAssocTypes` associated types MUST be bound to
the following Sia concrete types:

| Associated type      | Bound Sia concrete type                                                                                                          |
| --------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `Address`             | `sia_rust::types::Address` (already used throughout `siacoin_swap_ops.rs`).                                                      |
| `AddressParseError`   | The existing Sia address-parse error type used by `SiaCoin::parse_address`/`ParseCoinAssocTypes::my_addr` equivalents today.       |
| `Pubkey`              | `sia_rust::types::PublicKey` (ed25519), wire-encoded with the same 33-byte leading-zero-padded field convention D7's tests already establish and exercise (`ed25519_swap_field_round_trips`, `siacoin_swap_ops.rs`) — reused unchanged, not reinvented. |
| `PubkeyParseError`    | The existing Sia pubkey-parse error type (whatever D3/D4/D7's own `PublicKey::from_bytes(...)` call sites already surface as their error).            |
| `Tx`                  | `SiaTransaction` (already `impl Transaction`, used by every existing Sia swap path).                                              |
| `TxParseError`        | The existing `SiaTransaction::try_from(&[u8])` error type (already used by D3/D4/D7).                                             |
| `Preimage`            | A *new* local newtype, `SiaTxPreimage`, wrapping an unsigned/partially-satisfied `V2Transaction` plus enough branch information to know which `SpendPolicy::Threshold` arm is being satisfied (§54.7). |
| `PreimageParseError`  | The existing transaction/policy deserialization error type, reused.                                                              |
| `Sig`                 | `sia_rust::types::Signature` (ed25519), already `ToBytes`-compatible via the hex `Display` D4 already established and tested.      |
| `SigParseError`       | The existing Sia signature-parse error type (D4's `verify_message` already has one to reuse).                                     |

**R54.3.** `Preimage`/`Sig` MUST satisfy `ToBytes` (blanket-implemented for
any `AsRef<[u8]>`, `lp_coins_traits.rs:344`). Unlike UTXO's R4 (chapter 15),
no `AsRef<[u8]>` coherence adapter is anticipated to be necessary for
`Pubkey`/`Sig`, since both are Sia-crate-local types this project already
controls (no orphan-rule conflict expected) — but this MUST be verified
against the pinned `sia_rust` revision at implementation time, not assumed
from this sentence; if the library's actual type shapes force a newtype
the way UTXO needed one, that is a discretionary implementation detail, not
a deviation from this binding.

## 54.4 Bound Sia V2 Spend-Policy Trees

Sia's `SpendPolicy` enum (variants verified against the pinned `sia_rust`
revision, `types/spend_policy.rs`): `Above(height)`, `After(timestamp)`,
`PublicKey(pk)`, `Hash(secret_hash)`, `Threshold{n, of}` (an n-of-`of.len()`
boolean combinator — AND when `n == of.len()`, OR when `n == 1`), `Opaque`,
`UnlockConditions` (V1-compat, not used here). `SpendPolicy::atomic_swap`
(V1, already in production) is itself built from exactly these primitives:

```text
atomic_swap(success_pub, refund_pub, locktime, hash) =
  Threshold{n:1, of:[
    Threshold{n:2, of:[PublicKey(success_pub), Hash(hash)]},        // success path
    Threshold{n:2, of:[PublicKey(refund_pub),  After(locktime)]},   // refund path
  ]}
```

This section defines the three V2 policy trees the same way, translating
[Chapter 15](15-swap-v2-utxo-path.md) §15.4's three Bitcoin-script branch
sets (R8/R9/R10) one-for-one into `Threshold` trees. No new `SpendPolicy`
variant or on-chain capability is required.

**R54.4 (maker-payment V2 policy, mirrors chapter 15 R10).** MUST be:

```text
maker_payment_v2(maker_pub, taker_pub, locktime, maker_secret_hash, taker_secret_hash) =
  Threshold{n:1, of:[
    Threshold{n:2, of:[PublicKey(maker_pub), After(locktime)]},          // refund by timelock
    Threshold{n:2, of:[PublicKey(taker_pub), Hash(maker_secret_hash)]},  // taker spends, reveals maker secret
    Threshold{n:2, of:[PublicKey(maker_pub), Hash(taker_secret_hash)]},  // maker immediate-refunds, reveals taker secret
  ]}
```

The third branch (maker refunds early by revealing the *taker's* secret) is
the bound difference from V1 and is what enforces the two-payment
protocol's atomicity, identical in purpose to chapter 15 R10's third
branch: if the taker abandons the swap after funding but before
co-signing the funding-spend, and the maker has already revealed her
secret in a published maker-payment spend, the taker can immediately
recover the funding output via its own secret-reveal branch (R54.5)
without waiting for the funding timelock.

**R54.5 (taker-funding V2 policy, mirrors chapter 15 R8).** MUST be:

```text
taker_funding_v2(taker_pub, maker_pub, funding_locktime, taker_secret_hash) =
  Threshold{n:1, of:[
    Threshold{n:2, of:[PublicKey(taker_pub), After(funding_locktime)]},   // refund by timelock
    Threshold{n:2, of:[PublicKey(taker_pub), PublicKey(maker_pub)]},      // cooperative co-sign: funding -> payment
    Threshold{n:2, of:[PublicKey(taker_pub), Hash(taker_secret_hash)]},   // taker self-refund by revealing own secret
  ]}
```

**R54.6 (taker-payment V2 policy, mirrors chapter 15 R9).** MUST be:

```text
taker_payment_v2(taker_pub, maker_pub, locktime, maker_secret_hash) =
  Threshold{n:1, of:[
    Threshold{n:2, of:[PublicKey(taker_pub), After(locktime)]},                                    // refund by timelock
    Threshold{n:3, of:[PublicKey(taker_pub), PublicKey(maker_pub), Hash(maker_secret_hash)]},       // cooperative spend, maker reveals secret
  ]}
```

**R54.7 (secret-hash width).** Reuses [Chapter 20](20-siacoin-integration.md)'s
already-bound convention ([Chapter 51](51-legacy-v1-swap-state-machine.md)
§51.9.1 R71/R72/R72A): Sia is in the 32-byte-secret-hash coin family. No new
hashing/width decision is needed for V2 — `Hash(secret_hash)` above takes
the same 32-byte `Hash256` V1 already uses; this chapter does not
reintroduce a 20-byte legacy form.

**R54.8 (output placement).** Mirrors R-H1 (ch.20 §20.6): each policy's
funded output MUST sit at a fixed, deterministic index in its transaction
(one constant per role: maker-payment, taker-funding, taker-payment),
analogous to `HTLC_VOUT_INDEX` today, so validation/search code can locate
it positionally.

## 54.5 Bound `MakerCoinSwapOpsV2` Methods

**R54.9 `send_maker_payment_v2`.** Build and broadcast a `V2Transaction`
funding an output locked to §54.4's `maker_payment_v2` policy for
`args.amount`, using `args.maker_secret_hash`/`args.taker_secret_hash`/
`args.time_lock`/`args.taker_pub` plus this node's own HTLC pubkey (R54.16).
Mirrors `send_maker_payment_htlc` (V1, `siacoin_swap_ops.rs`) with the
richer policy substituted in.

**R54.10 `validate_maker_payment_v2`.** Reconstruct the expected
`maker_payment_v2` policy address from `args`' fields and this node's own
`taker_pub` (the validating party is the taker), and check the funding
tx's fixed-position output (R54.8) against it — mirrors D7's
`check_taker_payment_output`/`watcher_validate_taker_payment` shape
(`siacoin_mm_coin.rs`), extended to the three-branch policy.

**R54.11/R54.12 `refund_maker_payment_v2_timelock`/`_secret`.** Build a
transaction spending the maker-payment output via, respectively, branch 1
(refund by timelock, needs `After(locktime)` to be satisfied — i.e. the
chain's median timestamp has passed it, per R-H4's existing walletd-tip
comparison) or branch 3 (immediate refund by revealing
`args.taker_secret`). Mirrors `send_refund_htlc` (V1) with the policy
substituted.

**R54.13 `spend_maker_payment_v2`.** Build a transaction spending the
maker-payment output via branch 2 (taker spends, revealing
`args.maker_secret`). Mirrors `send_taker_spends_maker_payment` (V1).

## 54.6 Bound `TakerCoinSwapOpsV2` Methods

**R54.14 `send_taker_funding`.** Build and broadcast a `V2Transaction`
funding an output locked to §54.4's `taker_funding_v2` policy. Per §54.1's
table, the dex-fee (`args.dex_fee`) MUST be folded into the funding amount
rather than sent as a separate transaction — the funding-spend preimage
(R54.18) is what routes the fee portion to the resolved dex-fee address
(ch.20 §20.4.2) once the maker generates it. `args.premium_amount`/
`trading_amount` compose the remainder, matching chapter 15 R21's UTXO
analogue.

**R54.15 `validate_taker_funding`.** Reconstruct the expected
`taker_funding_v2` policy address (maker validates, using its own
`maker_pub` and the taker's `args.taker_pub`) and check the funding tx's
output, mirroring R54.10's shape.

**R54.16/R54.17 `refund_taker_funding_timelock`/`refund_taker_funding_secret`.**
Spend the funding output via branch 1 (timelock) or branch 3 (self-reveal
of the taker's own secret), mirroring R54.11/R54.12.

**R54.18 `search_for_taker_funding_spend`.** Walk the funding address's
walletd event log (reuse D3's `fetch_all_events` paging pattern,
`siacoin_swap_ops.rs`) for the event consuming the funding output, and
classify it into `FundingTxSpend`'s three variants by which branch the
spend transaction's satisfied policy matches: branch 1 →
`RefundedTimelock`, branch 3 (a valid taker-secret preimage present) →
`RefundedSecret{tx, secret}`, branch 2 (both pubkeys signed, no secret
present) → `TransferredToTakerPayment`. Mirrors D3's `classify_htlc_spend`
shape (`siacoin_swap_ops.rs`), extended to three branches instead of two.

**R54.19 `gen_taker_funding_spend_preimage`/R54.20
`validate_taker_funding_spend_preimage`/R54.21
`sign_and_send_taker_funding_spend`.** The cooperative funding→payment
conversion (branch 2 of §54.5, both parties sign). §54.7 defines the
mechanism this binds to: because Sia's V2 signature hash provably excludes
each input's `satisfied_policy` bytes (`V2TransactionBuilder::input_sig_hash`,
already verified and relied upon by D7's `create_taker_payment_refund_preimage`),
maker and taker can each independently compute a valid signature over the
*same* transaction structure without needing to see the other's signature
first. Concretely: the maker (R54.19) builds the unsigned funding-spend
transaction (spending the funding output, creating the taker-payment
output per §54.4's `taker_payment_v2` policy) and signs it, producing a
`TxPreimageWithSig{preimage: SiaTxPreimage(unsigned tx), signature:
maker's sig}`; the taker (R54.20) independently recomputes the same
unsigned-transaction structure from `GenTakerFundingSpendArgs` (not
trusting the maker's bytes blindly — recomputing and comparing, mirroring
chapter 15 R31's "MUST NOT trust the preimage bytes" requirement) and
verifies the maker's signature against it, then (R54.21) attaches its own
signature, assembles the branch-2 `Threshold` satisfaction with both
signatures, and broadcasts.

**R54.22 `refund_combined_taker_payment`.** Spend the (now taker-payment)
output via branch 1 (timelock refund), reusing R54.16's shape against the
`taker_payment_v2` policy instead of `taker_funding_v2`.

**R54.23 `skip_taker_payment_spend_preimage`.** MUST return `false` (the
trait default). Unlike EVM (chapter 17 §17.5.1, which returns `true`
because the deployed contract itself enforces spend authorization on-
chain and no off-chain preimage exchange is needed), Sia has no on-chain
enforcement beyond the `SpendPolicy` a transaction must satisfy at spend
time — the preimage/co-signature exchange (R54.19–21's mechanism, mirrored
for the taker-payment spend) is load-bearing here exactly as it is for
UTXO (chapter 15).

**R54.24/R54.25/R54.26 `gen_taker_payment_spend_preimage`/
`validate_taker_payment_spend_preimage`/
`sign_and_broadcast_taker_payment_spend`.** Same independent-signing
mechanism as R54.19–21, applied to the taker-payment output's branch 2
(cooperative spend, maker reveals `maker_secret_hash`'s preimage). The
taker generates the preimage+signature (R54.24, since the taker holds
`taker_pub` and must supply that half of the 3-of-3 threshold); the maker
validates (R54.25) then supplies both its own signature and the secret
preimage to complete the threshold and broadcast (R54.26) — this is the
step that reveals the maker's secret on-chain, which R54.27 depends on.

**R54.27 `find_taker_payment_spend_tx`.** Poll (bounded by `wait_until`)
for the taker-payment output to be spent, reusing D3/R54.18's event-walk
pattern against the taker-payment address.

**R54.28 `extract_secret_v2`.** Reuse the existing public
`SwapOps::extract_secret`/`extract_secret_from_tx` logic
(`siacoin_swap_ops.rs`, D3) unchanged — the V1 and V2 preimage-extraction
mechanism (reading `SatisfiedPolicy.preimages` off a spend transaction) is
identical; only the policy shape around it differs.

## 54.7 Bound `CommonSwapOpsV2` Derivations

**R54.29 `derive_htlc_pubkey_v2`/`derive_htlc_pubkey_v2_bytes`.** Reuse the
same per-swap HTLC-key derivation V1 already uses (ch.20 §20.4.1's SLIP-10
ed25519 derivation), wire-encoded with D7's existing 33-byte padded-field
convention (R54.2). No new derivation path — V2 does not need a
*different* key from V1 for the same swap-unique-data input.

## 54.8 Independent-Signing Mechanism (detail)

This section makes explicit the mechanism R54.19–21/R54.24–26 depend on,
since it is the one piece of this chapter's design that has no direct V1
precedent to point at (V1 never needs two independently-computed
signatures over the same transaction):

1. Sia's V2 signature hash (`V2TransactionBuilder::input_sig_hash`,
   verified against the pinned `sia_rust` source by D7's Coder pass) is
   computed over the transaction's core fields — inputs' parent IDs,
   outputs, miner fee, etc. — and explicitly **excludes** each input's
   `satisfied_policy` (the signatures/preimages actually attached).
2. Consequently, two different signers, each independently constructing
   the *same* unsigned transaction shape (same inputs, outputs, fee), sign
   the *same* hash, regardless of order and without needing to observe
   each other's signature first.
3. This lets the "preimage" exchanged between maker and taker be exactly
   `TxPreimageWithSig<SiaCoin>{preimage: SiaTxPreimage(unsigned tx bytes),
   signature: sender's own sig}` — the receiving party recomputes the
   unsigned tx independently from the swap's own already-agreed
   parameters (never trusting the sender's tx bytes as ground truth,
   matching chapter 15 R31), verifies the attached signature against
   their own recomputed hash, then adds their own signature and completes
   the `Threshold` satisfaction.
4. **OPEN QUESTION for implementation:** whether `SiaTxPreimage` should
   carry the fully-built `V2Transaction` (simpler receiver-side
   recomputation-and-compare) or a minimal structural descriptor (smaller
   wire size). Chapter 15's UTXO analogue (`UtxoTxPreimage`, R4) wraps a
   full transaction-input-signer; this chapter recommends the same
   (`SiaTxPreimage` wraps a full unsigned `V2Transaction`) for consistency
   and because Sia transactions are already compact (fixed-width
   `Currency` encoding, D6), but this is a discretionary implementation
   choice, not dictated by anything above — flagging it rather than
   silently picking one, since the Coder pass implementing R54.19–21 will
   need to commit to a concrete `SiaTxPreimage` shape.

## 54.9 State-Machine Dispatch Wiring

**R54.30.** `mm2src/mm2_main/src/lp_swap/swap_v2_common.rs` currently
matches `(MmCoinEnum, MmCoinEnum)` pairs to construct the correct
monomorphized `MakerSwapStateMachine<M, T>`/`TakerSwapStateMachine<M, T>`
(verified: today's arms are `(EthCoin, EthCoin)`, `(UtxoCoin, UtxoCoin)`,
`(UtxoCoin, EthCoin)`, `(EthCoin, UtxoCoin)`). Adding Sia as a third V2-
capable family requires extending this match with the new combinations
that should be swap-eligible: at minimum `(SiaCoin, SiaCoin)`; whether
Sia↔UTXO and Sia↔EVM cross-family V2 swaps are in scope for the first cut
or deferred is an **OPEN QUESTION** for the implementer/reviewer to
resolve against the existing V1 Sia↔other-coin swap matrix (ch.13) — this
chapter does not by itself expand which coin *pairs* are tradeable, only
how a Sia leg of an already-eligible pair executes.

**R54.31.** [Chapter 13](13-swap-version-negotiation.md)'s existing
protocol-version negotiation (V1 vs V2 selection per swap) is unaffected
by this chapter; Sia simply becomes eligible for the V2 branch of that
existing negotiation once R54.1–R54.30 land, exactly as UTXO/EVM already
are.

## 54.10 Watcher Considerations

Out of scope for this chapter. [Chapter 20](20-siacoin-integration.md)'s
D7 (now closed) implements V1 watcher eligibility only; a V2-protocol
`WatcherOps` story (if the coin-generic trait surface even distinguishes
one — verify before assuming it does) is not designed here and should be
its own follow-up once this chapter's core V2 path is implemented and
reviewed, mirroring how chapter 17 §17.11's watcher-reward section is
EVM-specific and chapter 15 has no equivalent section at all.

## 54.11 Hardware-Wallet / HD Considerations

Out of scope for this chapter. [Chapter 15](15-swap-v2-utxo-path.md)
§15.13 binds Trezor/hardware-wallet support for UTXO's V2 path; Sia has no
comparable hardware-wallet story anywhere in this project today (ch.20
never mentions one), and D1's HD-wallet work (ch.20, now closed for
infrastructure, activation-wiring in progress separately) is a V1-and-
activation concern, not a V2-swap concern. If Sia HD-wallet swap
participation needs anything V2-specific, that is a future chapter, not
this one.

## 54.12 Tests (plan, not yet written)

Mirroring chapter 15 §15.14's shape at a level appropriate for Sia's
simpler on-chain mechanism:

- Pure `SpendPolicy`-tree construction tests for all three policies
  (§54.4), analogous to D3/D4/D7's existing pure-function test style
  (build the expected policy tree by hand from primitives, compare against
  the bound builder's output) — no live client needed.
- Pure classification tests for `search_for_taker_funding_spend`
  (R54.18), mirroring D3's `swap_spend_search_tests` (three branches
  instead of two).
- Pure preimage round-trip tests for the independent-signing mechanism
  (§54.8): build an unsigned tx, sign as party A, recompute independently
  as party B, verify A's signature, add B's signature, confirm the
  assembled `Threshold` satisfaction validates.
- No live-walletd integration tests are expected, consistent with every
  other Sia module in this crate (`SiaClient::new()` pings walletd on
  construction; no mock-client harness exists yet, per D7/D8's own
  reports) — flagged again here since it will apply to this work too.

## 54.13 Deferred Work

- **D54.1 — Cross-family V2 swap eligibility** (R54.30's open question):
  whether Sia participates in V2 swaps against UTXO/EVM counterparties or
  only Sia↔Sia, at least for a first implementation pass.
- **D54.2 — `SiaTxPreimage` wire shape** (§54.8's open question): full
  transaction vs. minimal descriptor.
- **D54.3 — V2 watcher eligibility** (§54.10): not designed here.
- **D54.4 — V2 HD-wallet interaction** (§54.11): not designed here, and
  gated behind D1's activation-wiring work landing first in any case.

## 54.14 Baseline Verifications

None. No V2 Sia implementation exists yet — this chapter defines the
target behavior a future implementation will be checked against, not a
currently-observable baseline.

## 54.15 External References

- Sia `SpendPolicy`/V2 transaction encoding — the pinned `sia_rust`
  revision's own source (`types/spend_policy.rs`,
  `utils/tx_builder.rs`), the same library this project already binds
  for every other Sia module (ch.20 §20.4–§20.5).
- [Chapter 15](15-swap-v2-utxo-path.md) — structural precedent for the
  three-script/three-policy branch semantics.
- [Chapter 17](17-swap-v2-evm-path.md) — structural precedent for the
  taker-side two-stage flow and `skip_taker_payment_spend_preimage`'s
  meaning.

## 54.16 Provenance Footer

- *Inputs:* [Chapter 20](20-siacoin-integration.md) (all bound Sia V1
  facts this chapter reuses: key derivation, native-spend-policy HTLC
  model, DEX-fee resolution, the now-closed D3/D4/D7 deferred-work items);
  [Chapter 14](14-state-machine-runtime.md) and
  [Chapter 52](52-swap-v2-state-machine.md) (the generic state-machine
  runtime and its V2-swap specialization, reused unmodified); [Chapter
  15](15-swap-v2-utxo-path.md) and
  [Chapter 17](17-swap-v2-evm-path.md) (structural precedent only, read
  for branch-semantics/trait-conformance shape, not copied); [Chapter
  13](13-swap-version-negotiation.md) (protocol-version negotiation, left
  unmodified); [Chapter 51](51-legacy-v1-swap-state-machine.md) §51.9.1
  (secret-hash-width convention, reused unchanged); the coin-generic V2
  trait and argument/result type definitions as they exist in
  `mm2src/coins/lp_coins_traits.rs`/`lp_coins_types.rs`/
  `lp_coins_errors.rs` today, read directly from the repository; the
  pinned `sia_rust` revision's public `SpendPolicy`/`V2TransactionBuilder`
  source, read directly, the same library this project already binds
  elsewhere; `mm2src/mm2_main/src/lp_swap/maker_swap_v2.rs`/
  `swap_v2_common.rs`, read directly to verify the dispatch-matrix shape
  claimed in §54.9.
- *Restricted corpus consultation:* **none.** No Sia V2 swap
  implementation exists in the restricted behavior-analysis corpus this
  project's clean-room process otherwise draws from; this chapter is
  original design bound entirely to already-bound Sia V1 facts and the
  coin-generic V2 trait surface, both read directly from this
  repository, plus the pinned `sia_rust` library's own public source.
  `KDF Spec Reader`/`KDF Dirty Gate` were not invoked for this chapter,
  consistent with `AGENTS.md` §2's escalation path applying only when a
  real corpus-derived fact is genuinely needed.
- *Open questions requiring reviewer/implementer resolution before or
  during implementation:* D54.1–D54.4 (§54.13), R54.3's coherence-adapter
  verification, §54.8's `SiaTxPreimage` shape choice.
