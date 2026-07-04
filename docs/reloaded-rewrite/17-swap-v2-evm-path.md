# Chapter 17 — Atomic-Swap V2 EVM Path & Contract Interaction

**Status:** driving-spec

> **One-sentence claim:** the project's V2 atomic-swap protocol
> reaches EVM-native assets (ETH, ERC-20, ERC-721, ERC-1155) by
> driving two deployed Solidity contracts whose entry points
> implement the protocol's funding/payment split, on-chain
> reveal-on-spend, and atomic dex-fee delivery; the asset surface
> is integrated as the maker-side and taker-side branches of the
> project's V2 coin-trait families.

---

## 17.0 Executive Summary

The V2 atomic-swap protocol (described at chain-neutral level in
[Chapter 13](13-swap-version-negotiation.md) and the UTXO binding
in [Chapter 15](15-swap-v2-utxo-path.md)) needs a chain-side
representation on EVM chains. Unlike UTXO scripts, EVM gives no
in-transaction script slot for arbitrary commit/reveal logic, so
the protocol is realised by two purpose-built Solidity contracts
deployed once per supported EVM chain:

- A **maker-side payment contract** that locks the maker's funds
  under a payment id keyed on the protocol's lock time and secret
  hashes, and releases them either by reveal-of-secret (the taker
  spending) or by timelock (the maker reclaiming).
- A **taker-side payment contract** that holds the taker's deposit
  (which carries the dex fee) and the taker's payment, with a
  funding-vs-payment state split that lets the taker reclaim
  cheaply before the maker has confirmed the trade.

Both contracts expose typed entry points (one per asset class:
native coin, ERC-20, ERC-721, ERC-1155) that take the protocol
identifiers as call arguments. The contract recomputes the payment
id from those arguments so both sides converge on the same key in
the contract's payment-state mapping. Spend calls require the
caller to pass the counterparty's secret; the contract recomputes
its hash and rejects the call when it does not match the value
committed at payment time.

The Rust side of this chapter implements the project's V2 coin
traits (one for the maker side, one for the taker side) on the
project's EVM-coin type, dispatching each trait method to the
appropriate contract entry point through the ABI-encoded calldata
path that already exists for the V1 EVM swap surface. NFT support
is realised as a maker-side branch only: NFT-for-fungible-token
swaps use the NFT entry points on the maker side and the standard
fungible entry points on the taker side.

Permitted inputs that fix the shape of this chapter:

- EIP-20 (ERC-20 fungible-token interface).
- EIP-721 (ERC-721 non-fungible-token interface).
- EIP-1155 (ERC-1155 multi-token interface).
- The Solidity Contract ABI Specification (function selectors,
  argument encoding, event topic encoding).
- The Ethereum JSON-RPC `eth_call`, `eth_sendRawTransaction`,
  `eth_getLogs`, `eth_estimateGas`, and `eth_blockNumber` methods.
- The keccak-256 hash function (used for ABI selector derivation,
  event topic derivation, and the protocol's secret-hash check).
- The V2 atomic-swap protocol's chain-neutral state-machine shape,
  fixed in [Chapter 13](13-swap-version-negotiation.md) and
  reproduced for UTXO in [Chapter 15](15-swap-v2-utxo-path.md).

---

## 17.1 Why an EVM-specific V2 path exists

The V1 EVM atomic-swap path relies on a legacy contract whose
state model conflates funding with payment: there is no point at
which the taker can reclaim a deposit before the maker has acted.
The V2 protocol's funding-vs-payment split (necessary for early
abort, watcher rewards, and the dex-fee atomic-delivery property
fixed by [Chapter 8](08-fee-routing-engine.md) and
[Chapter 9](09-watcher-reward-infrastructure.md)) cannot be
expressed against the V1 contract; a new contract pair was
necessary.

The new contracts add, beyond the funding split:

1. **On-chain reveal-on-spend.** Each spend entry point takes the
   secret as a parameter; the contract recomputes the hash and
   accepts the call only when the recomputed hash matches the
   value committed at payment time. The secret is then visible
   in the spend transaction's calldata for the counterparty (and
   for any watcher) to recover.

2. **Dex-fee delivery in the same transaction as the taker
   payment claim.** The taker payment contract forwards the
   payment amount to the maker and the dex-fee amount to the
   network's fee-collection address atomically inside the same
   call frame, eliminating the V1 race between fee payment and
   payment release.

3. **NFT maker payments.** EIP-721 and EIP-1155 are supported as
   maker-side asset classes through dedicated entry points on
   the maker contract; the taker side remains fungible.

---

## 17.2 Contract architecture

Two contracts back the protocol; both are deployed once per EVM
chain and addressed from coin configuration.

### 17.2.1 The maker payment contract — `EtomicSwapMakerV2`

State map: `mapping(bytes32 => MakerPayment) public makerPayments;`

Entry points (signatures given by plain ABI types):

| Entry point                        | Behaviour                                                                 |
|------------------------------------|---------------------------------------------------------------------------|
| `ethMakerPayment`                  | Lock `msg.value` into a new `makerPayments[id]` entry (native coin).      |
| `erc20MakerPayment`                | Pull `amount` via `transferFrom` and lock under `makerPayments[id]`.      |
| `erc721MakerPayment`               | Receive an ERC-721 token; lock under `makerPayments[id]`.                 |
| `erc1155MakerPayment`              | Receive an ERC-1155 token (`amount` units); lock under `makerPayments[id]`.|
| `spendMakerPayment`                | Caller passes `makerSecret`; contract recomputes the hash, releases funds to caller (the taker). |
| `refundMakerPaymentTimelock`       | After `paymentLockTime`, maker reclaims.                                  |
| `refundMakerPaymentSecret`         | Cooperative abort: maker reclaims by revealing `takerSecret`.             |

Common arguments across all `*MakerPayment` entry points:
`(bytes32 id, address taker, bytes32 takerSecretHash, bytes32 makerSecretHash,
uint256 paymentLockTime)` plus the amount/token parameters
appropriate to the asset class.

### 17.2.2 The taker payment contract — `EtomicSwapTakerV2`

State map: `mapping(bytes32 => TakerPayment) public takerPayments;`

Entry points:

| Entry point                        | Behaviour                                                                                       |
|------------------------------------|-------------------------------------------------------------------------------------------------|
| `ethTakerPayment`                  | Lock `msg.value = paymentAmount + dexFee` into `takerPayments[id]` (native coin).               |
| `erc20TakerPayment`                | Pull `paymentAmount + dexFee` via `transferFrom` and lock.                                      |
| `takerPaymentApprove`              | (ERC-20 only) Confirm allowance; updates `takerPayments[id]` to *approved* state.               |
| `spendTakerPayment`                | Caller (the maker) passes `takerSecret`; contract sends `paymentAmount` to caller and `dexFee` to the network's fee-collection address. |
| `refundTakerPaymentTimelock`       | After `paymentLockTime`, taker reclaims everything.                                             |
| `refundTakerPaymentSecret`         | Cooperative abort: taker reclaims by revealing `makerSecret`.                                   |

Common arguments across `*TakerPayment` entry points:
`(bytes32 id, uint256 dexFee, uint256 paymentAmount, address maker,
bytes32 takerSecretHash, bytes32 makerSecretHash,
uint256 fundingLockTime, uint256 paymentLockTime)` plus token
address for the ERC-20 variant.

### 17.2.3 Events

Both contracts emit per-action events that consumers filter by
topic:

- `MakerPaymentSent(bytes32 id)`
- `MakerPaymentSpent(bytes32 id, bytes32 makerSecret)`
- `MakerPaymentRefundedTimelock(bytes32 id)`
- `MakerPaymentRefundedSecret(bytes32 id, bytes32 takerSecret)`
- `TakerPaymentSent(bytes32 id)`
- `TakerPaymentApproved(bytes32 id)`
- `TakerPaymentSpent(bytes32 id, bytes32 takerSecret)`
- `TakerPaymentRefundedTimelock(bytes32 id)`
- `TakerPaymentRefundedSecret(bytes32 id, bytes32 makerSecret)`

The on-disk ABI JSON files (one per contract) carry the canonical
ABI shape and are loaded at coin activation to drive calldata
encoding and event decoding.

---

## 17.3 ABI files

Two JSON files capture the maker- and taker-contract ABIs
(function signatures, event signatures, parameter types). They
are loaded at coin activation by the project's standard ABI loader
and used to encode calldata and decode event payloads.

The files are *factual content* (machine-derivable from the
Solidity source) and ride under the same Conditions E/F licensing
treatment used for other contract ABIs in the tree — see
[Chapter 29 — License conditions E and F](29-license-conditions-e-f.md).

---

## 17.4 Maker-side coin-trait implementation

The project's V2 maker-side coin trait carries five methods
(payment send, payment validate, payment spend, payment refund by
timelock, payment refund by secret); the trait's chain-neutral
shape is set by [Chapter 15](15-swap-v2-utxo-path.md) and shared
across all V2-capable coin families.

The EVM implementation of each method:

### 17.4.1 Payment send

1. Derive `swapId` from the protocol-fixed combination of
   `paymentLockTime` (big-endian) and `makerSecretHash`. The
   contract recomputes the same id from the call arguments so
   both sides converge on the same key in `makerPayments`.
2. Branch on the EVM asset class:
   - Native coin → call `ethMakerPayment(id, taker,
     takerSecretHash, makerSecretHash, paymentLockTime)` with
     `msg.value = paymentAmount`.
   - ERC-20 → call `erc20MakerPayment(id, token, amount, taker,
     takerSecretHash, makerSecretHash, paymentLockTime)` after the
     usual `approve` flow.
   - NFT → dispatch to the maker contract's `erc721MakerPayment`
     or `erc1155MakerPayment` entry point per token standard
     (§17.9).
3. Sign and broadcast; return the signed transaction.

### 17.4.2 Payment validate

Given a signed transaction and the negotiation parameters, the
validator (the taker, in protocol terms):

1. Decodes the tx input via the loaded ABI; asserts the method
   selector matches one of the four `*MakerPayment` selectors.
2. Asserts decoded arguments match negotiation:
   `(takerSecretHash, makerSecretHash, paymentLockTime, taker)`.
3. Asserts `msg.value` (native coin) or `amount` (ERC-20) equals
   the expected payment amount.
4. Reads `makerPayments[id]` via `eth_call` and asserts the state
   is `Sent` (funds locked, not yet spent or refunded).

### 17.4.3 Payment spend

Called by the taker once they observe a valid maker payment and
have learned the maker secret (from the cooperative-spend tx —
§17.5.7). Builds and broadcasts a `spendMakerPayment(id, amount,
makerSecret, taker, ...)` call. The contract recomputes
`keccak256(makerSecret)` and accepts the call iff it equals the
committed `makerSecretHash`.

### 17.4.4 Refund paths

- *Timelock refund* — precondition `block.timestamp >=
  paymentLockTime`; the contract releases funds to the maker.
- *Secret refund* — cooperative abort. The maker passes the
  *taker* secret (which the maker learned via the cooperative-spend
  protocol); the contract recomputes the hash and releases.

---

## 17.5 Taker-side coin-trait implementation

The project's V2 taker-side coin trait carries 15 methods; the
taker side has more surface than the maker side because of the
funding-vs-payment split and the EVM-specific approval step. Each
method's role:

| Trait role                                  | EVM behaviour                                                                |
|--------------------------------------------|-------------------------------------------------------------------------------|
| Send funding                                | Call the `*TakerPayment` entry point appropriate to the asset class.          |
| Validate funding                            | Decode calldata + read `takerPayments[id]` state.                             |
| Refund funding by timelock                  | Use the consolidated funding+payment timelock-refund path (§17.5.4).          |
| Refund funding by secret                    | Cooperative abort by revealing the maker secret.                              |
| Search for funding spend                    | Poll `eth_getLogs` for the matching state-transition event (§17.5.5).         |
| Generate funding-spend preimage             | EVM stub: return the RLP-encoded funding tx as the "preimage"; dummy signature (§17.5.1). |
| Validate funding-spend preimage             | EVM stub: always `Ok` (§17.5.1).                                              |
| Sign and send funding spend                 | ERC-20 only: call `takerPaymentApprove(id)` to promote funding → payment (§17.5.2). Native-coin path is a no-op-equivalent. |
| Refund combined funding+payment             | EVM-specific timelock refund that handles both states in a single call (§17.5.4). |
| Generate payment-spend preimage             | EVM stub: returns `Ok` with empty preimage.                                   |
| Validate payment-spend preimage             | EVM stub: always `Ok`.                                                        |
| Skip payment-spend preimage                 | Returns `true` — the state machine skips the preimage exchange.               |
| Sign and broadcast payment spend            | Maker action: call `spendTakerPayment(id, amount, takerSecret, ...)` (§17.5.6).|
| Find payment-spend tx                       | Log polling for `TakerPaymentSpent` (§17.8).                                  |
| Extract secret from payment-spend tx        | Decode `takerSecret` from the spend tx calldata (§17.5.7).                    |

### 17.5.1 EVM "no real preimage" optimisation

The UTXO V2 protocol uses a preimage-exchange round so the taker
can partial-sign a transaction that the maker completes. EVM
cannot work the same way because every signed tx is already
broadcast-ready — there is no "preimage + partial sig" intermediate.
The trait implementation handles this by returning the RLP-encoded
funding tx as a stand-in preimage, accepting any value at validate
time, and instructing the state machine (via the
skip-payment-spend-preimage trait method returning `true`) to skip
the preimage round entirely. This is the correct behaviour for
EVM and is intentional, not a stub.

### 17.5.2 The `takerPaymentApprove` step (ERC-20 only)

For ERC-20 tokens, `erc20TakerPayment` deposits funds into the
contract but leaves them in a *funding* state. The taker must
follow up with `takerPaymentApprove(id)` to promote the funds to
a *payment* state, which the maker can then claim via
`spendTakerPayment`. For native coin the funding and payment
states are unified — no separate approval call is needed.

The approval call exists because the ERC-20 path requires an
explicit allowance update before the contract can move tokens to
the fee-collection address inside `spendTakerPayment`.

In trait terms this is reached through the
"sign and send funding spend" entry from §17.5's table, which
branches on the asset class internally rather than being a
distinct trait method.

### 17.5.3 Funding-send and funding-validate

*Funding send* selects the `*TakerPayment` ABI entry point per
asset class, packs the arguments `(id, dexFee, paymentAmount,
maker, takerSecretHash, makerSecretHash, fundingLockTime,
paymentLockTime[, tokenAddr])`, signs the transaction, and
broadcasts it. For native coin, `msg.value = paymentAmount +
dexFee`; for ERC-20, `msg.value = 0` and the contract pulls
tokens via `transferFrom` (the taker's allowance must already
cover `paymentAmount + dexFee`).

*Funding validate* decodes the broadcast tx's calldata, asserts
the method selector and arguments match negotiation, and reads
`takerPayments[id]` via `eth_call` to confirm the on-chain state
is `Sent`.

### 17.5.4 Refund paths

- *Combined timelock refund* — invoked when `block.timestamp >=
  paymentLockTime` and no spend has been observed. Calls the
  contract's `refundTakerPaymentTimelock(id)`. On EVM the
  funding and payment states share a single refund path; the
  contract handles both cases (funding-only and funding+payment)
  from one entry.
- *Secret refund* — cooperative abort. The taker calls
  `refundTakerPaymentSecret(id, makerSecret)`; the contract
  recomputes the hash and releases.

### 17.5.5 Searching for the funding spend

Given a funding tx hash, polls `eth_getLogs` for the matching
`TakerPaymentApproved` event (ERC-20 path) or scans subsequent
blocks for the state transition (native-coin path). Returns a
funding-spend descriptor when found; `None` otherwise. The
lookback range is bounded by the funding tx's confirmation
block.

### 17.5.6 Payment spend (maker action)

Called by the **maker** to claim the taker's payment. Builds and
broadcasts `spendTakerPayment(id, paymentAmount, takerSecret, ...)`.
The contract recomputes `keccak256(takerSecret)`, asserts it
equals `takerSecretHash`, then forwards `paymentAmount` to the
caller and `dexFee` to the network fee-collection address — both
transfers happen atomically in the same call frame.

### 17.5.7 Reveal-on-spend and secret extraction

When the taker calls `spendMakerPayment(id, ..., makerSecret, ...)`,
the maker observes the broadcast tx, decodes its calldata via the
loaded ABI, and extracts the `makerSecret` argument. This is the
EVM analogue of the UTXO "secret-in-script_sig" reveal. The
symmetric path extracts the `takerSecret` from a maker-issued
`spendTakerPayment` calldata.

---

## 17.6 Native-coin vs ERC-20 differentiation

| Aspect                       | Native coin                                          | ERC-20                                                                                  |
|------------------------------|------------------------------------------------------|-----------------------------------------------------------------------------------------|
| Funds movement               | `msg.value` carries amount                           | `approve()` + `transferFrom()` inside the contract                                       |
| Maker payment entry          | `ethMakerPayment(...)`                               | `erc20MakerPayment(...)` with `token` argument                                          |
| Taker payment entry          | `ethTakerPayment(...)`, `msg.value = amt + fee`      | `erc20TakerPayment(...)`, no `msg.value`; tokens pulled via `transferFrom`              |
| Approval round               | None                                                 | `takerPaymentApprove(id)` between funding and payment states                            |
| Fee delivery                 | Contract forwards from `msg.value` on `spendTakerPayment` | Contract `transferFrom` taker's allowance, then `transfer` to fee address          |
| Dust / minimum               | Network-level gas floor                              | Token-contract-specific (no on-chain dust rule)                                          |

Each trait method's implementation branches on the EVM asset
class to select the appropriate entry point and pack the matching
argument list.

---

## 17.7 Dex-fee delivery

The V2 EVM contracts accept a flat `dexFee` `uint256` argument on
the `*TakerPayment` entry points and forward the entire amount to
the network's fee-collection address on `spendTakerPayment`. The
EVM path delivers only the `Standard` dex-fee shape;
[Chapter 16](16-swap-v2-pre-burn-output.md) fixes that the
factory used to construct the dex-fee value returns `Standard`
for an EVM taker coin, so the `WithBurn` variant never reaches an
EVM contract argument. Extending pre-burn to the EVM contracts
requires the ABI to grow `burnAmount` and `burnAddress`
parameters; that change is out of scope here.

---

## 17.8 Event monitoring

Finding a taker-payment spend uses an `eth_getLogs` filter over:

- the deployed taker-payment contract address,
- topic 0 = `keccak256("TakerPaymentSpent(bytes32,bytes32)")`,
- topic 1 = the `swapId`,
- a block range starting at the funding-tx confirmation block.

Once a matching log is found, the txhash is fetched and the
calldata decoded for the secret. Confirmations are tracked via
the standard `eth_blockNumber` polling mechanism shared with the
V1 EVM path. The symmetric pattern (a different topic, the
maker-payment contract address) covers `MakerPaymentSpent`.

---

## 17.9 NFT variant

The NFT V2 variant is a maker-side extension of the EVM V2 path.
It covers EIP-721 and EIP-1155 maker payments through the
NFT-aware maker contract while the taker payment remains a
fungible EVM V2 payment. The maker-side state-machine dispatch
decision is therefore part of the production protocol contract,
not an optional optimisation or a test-only helper.

**R17.9.1. Maker-side NFT eligibility.** A candidate NFT V2 swap
MUST be considered only when the maker asset is an enabled EVM
NFT identified by token contract, token id, and token standard.
EIP-721 maker payments carry exactly one token id. EIP-1155 maker
payments carry token id plus amount. The payment id, secret-hash,
secret-reveal, and timelock semantics remain the maker-payment V2
semantics of §17.4.

**R17.9.2. Dispatch-decision interface.** Whenever a candidate
swap has an EVM NFT as the maker asset, the production swap
selection path MUST make a pure local decision before creating or
resuming any maker-payment state-machine action. The same decision
MUST be used by both local roles: the maker role before sending or
refunding its maker payment, and the taker role before validating
or spending the maker payment. The decision inputs are the maker's
advertised swap-version tag, the taker's advertised swap-version
tag, and whether activation configured an NFT-aware maker contract
address for a deployed contract on the maker chain. The decision
MUST return a typed outcome with the following three meanings:

- *Use NFT V2 path* — both advertised tags are NFT V2 and the
  maker chain has a deployed NFT-aware maker contract configured.
- *Version mismatch* — at least one advertised tag is not NFT V2;
  the caller MUST fall back to the negotiated fungible swap path
  when the requested trade is otherwise representable by that
  path; if the maker asset remains an NFT and cannot be represented
  by that path, the caller MUST refuse the candidate.
- *No NFT contract configured* — both advertised tags are NFT V2
  but the maker chain has no NFT-aware maker contract configured;
  the caller MUST refuse the trade.

The outcome MUST NOT be collapsed to a boolean. Callers need to
distinguish an intentional version downgrade from a chain
configuration error.

**R17.9.3. Production call-site contract.** The production
state-machine factory, swap-start/kickstart path, and restart
restoration path MUST route a maker-NFT candidate through a
maker-coin operation interface that exposes NFT maker-payment
send, validate, taker-spend, timelock-refund, and secret-refund
operations. It is not sufficient for the NFT operation surface to
exist only as directly callable coin methods or direct tests. The
normal fungible maker-payment interface MUST NOT be selected for a
candidate whose maker asset is still classified as an EVM NFT.

**R17.9.4. Use-branch behaviour.** When R17.9.2 returns *Use NFT
V2 path*, every production maker-payment action for the candidate
MUST dispatch through the maker-side NFT operation surface. The
ERC-721 path MUST address the selected token contract and token
id. The ERC-1155 path MUST additionally bind the negotiated
amount. In this branch the maker payment MUST NOT be sent,
validated, spent, or refunded through the native-coin or ERC-20
maker-payment entry points.

**R17.9.5. Version-mismatch behaviour.** When R17.9.2 returns
*Version mismatch*, the NFT maker branch MUST NOT be used. The
maker-side state machine MUST continue only with the negotiated
fungible V2 or legacy path selected by Chapter 13. No NFT-aware
maker contract call may be built or broadcast for this trade
decision. If the candidate cannot be represented by the negotiated
fungible path because the maker asset is an NFT, the swap MUST be
refused before any maker-payment transaction is built, signed,
broadcast, or persisted as sent.

**R17.9.6. Missing-contract behaviour.** When R17.9.2 returns *No
NFT contract configured*, the swap MUST be rejected before any
maker-payment transaction is built, signed, broadcast, persisted
as sent, or advertised to the peer. This branch MUST NOT silently
fall back to a fungible path because both peers explicitly chose
NFT V2 and the chain configuration is incomplete.

**R17.9.7. Taker-side NFT absence.** NFT V2 swaps are
maker-NFT-for-taker-fungible only. The taker-side state machine
MUST use the standard fungible EVM V2 path of §17.5 for funding,
payment, spend, refund, event monitoring, and secret extraction.
An attempted trade that requires the taker side to lock or pay an
NFT MUST be rejected as unsupported rather than mapped onto a
maker-side NFT operation.

**R17.9.8. NFT identity validation.** The maker-side NFT branch
MUST validate the NFT identity before a maker-payment transaction
is built. ERC-721 maker payments MUST carry a token contract,
token id, and token standard; their amount is implicit and equal
to one. An explicit ERC-721 amount other than one MUST be
rejected. ERC-1155 maker payments MUST carry a token contract,
token id, token standard, and a positive amount. A zero, missing,
or otherwise non-positive ERC-1155 amount MUST be rejected before
signing or broadcast.

**R17.9.9. Native `MM2.db` compatibility.** Candidate-1 NFT V2
support MUST preserve the native `MM2.db` schema and migration
lineage defined by Chapter 44. It MUST NOT add NFT-swap-specific
native `MM2.db` tables, `my_swaps` columns, migration states,
indexes, or versioning rules for token contract, token id, token
standard, ERC-1155 amount, or other selected-NFT swap intent. The
compatible native restart surface for V2 swaps is limited to the
corpus-compatible generic V2 persistence: `my_swaps`, the generic
scalar V2 fields defined for that table, `events_json`,
`swap_type`, `is_finished`, and `swap_version`. General NFT
wallet/cache metadata MAY exist for wallet inventory and history,
but it is not a swap restart source unless the selected NFT
identity is also bound to the swap by a clean protocol/state
contract. Any proposal to persist selected NFT swap intent by
adding a native `MM2.db` table, column, migration state, index, or
alternate versioning rule is a compatibility-breaking divergence
and requires explicit human approval before implementation.

**R17.9.10. Restart data sources.** NFT V2 restart recovery MUST
reconstruct state only from corpus-compatible generic V2
persistence and, after a maker-payment transaction is available,
from that transaction's public calldata and embedded HTLC
arguments. General NFT wallet/cache metadata MUST NOT be treated
as identifying the selected swap NFT unless a clean protocol/state
contract has bound that metadata to the swap. The recovery path
MUST NOT fabricate NFT identity fields, infer token standard from
an unreliable local default, select a token from wallet inventory,
or read a Reloaded-local native DB extension that is not part of
the Chapter-44-compatible `MM2.db` contract.

**R17.9.11. Pre-maker-payment restart boundary.** If the process
restarts before the NFT maker-payment transaction has been
broadcast or otherwise persisted in generic V2 state, recovery may
resume only when the token contract, token id, token standard, and
ERC-1155 amount when applicable are recoverable from existing
corpus-compatible state or from a clean protocol/state contract
that binds the selected NFT intent to the swap. When those fields
are not swap-bound and recoverable, the recovery handler MUST park
or refuse the unfinished NFT swap with a recoverable status; it
MUST NOT build, sign, or broadcast a maker-payment transaction
with invented NFT identity or with an NFT selected from general
wallet/cache metadata.

**R17.9.12. Post-maker-payment restart recovery.** Once the
maker-payment transaction is available, production restart MUST
route the restored swap through the NFT maker-operation surface
and MUST decode the maker-payment calldata before validating,
spending, or refunding that maker payment. The decoded public
calldata is the authoritative post-payment source for the token
contract, token id, transferred amount for ERC-1155, recipient
NFT-aware maker contract, swap id, taker address, secret hashes,
and payment lock time. The recovery path still MUST obtain the
token standard from a corpus-compatible source before selecting
the ERC-721 or ERC-1155 spend/refund path. If required calldata is
unavailable, malformed, or inconsistent with negotiation, or if
the token standard is unavailable, recovery MUST park or refuse
the NFT-specific on-chain action rather than selecting a path by
guessing. These calldata-decoding requirements are production
restart requirements, not validation-only or test-only substrate.

**T17.9.1. Use NFT V2 path.** Given an ERC-721 maker asset and an
ERC-1155 maker asset in separate cases, with both peers
advertising NFT V2 and the maker chain configured with a deployed
NFT-aware maker contract, the dispatch decision returns
*Use NFT V2 path*. The maker-side call site builds the NFT
maker-payment operation for the selected token standard, and the
taker side uses the fungible EVM V2 payment flow.

**T17.9.2. Production state-machine selection.** Given a
maker-NFT-for-taker-fungible candidate that satisfies T17.9.1,
the production swap-start path and the restart restoration path
select a state-machine binding whose maker-payment send,
validate, spend, timelock-refund, and secret-refund actions call
the NFT maker-operation surface. The test fails if the candidate
is routed to the native-coin or ERC-20 maker-payment surface or
if the NFT operation surface is reachable only by direct coin
method calls outside the production state machine.

**T17.9.3. Version mismatch fallback.** Given any case where the
maker advertises NFT V2 and the taker advertises only a lower
swap-version tag, or the taker advertises NFT V2 and the maker
advertises only a lower tag, the dispatch decision returns
*Version mismatch* and preserves both advertised values for the
caller. The test asserts that no NFT maker-payment operation is
built. For a fungible-compatible candidate, state-machine
selection continues through the negotiated fungible path. For a
candidate whose maker asset remains an NFT, state-machine
selection refuses the swap before maker-payment build or
broadcast.

**T17.9.4. No NFT contract refusal.** Given both peers advertising
NFT V2 and the maker chain lacking an NFT-aware maker contract
address, the dispatch decision returns *No NFT contract
configured*. The state machine rejects the trade before any maker
payment is built or broadcast and does not fall back to the
fungible path.

**T17.9.5. Taker-side NFT absence.** Given a trade request whose
taker payment asset is an NFT, the EVM V2 dispatcher rejects the
trade as unsupported. Given a maker-NFT-for-taker-fungible trade
that satisfies T17.9.1, the same test fixture asserts that the
taker funding and payment actions use the fungible EVM V2
surface, not an NFT-specific taker surface.

**T17.9.6. NFT identity validation.** Given an ERC-721 maker
asset with token contract, token id, and token standard, the
maker-payment builder accepts the implicit amount of one and
rejects any explicit non-one amount. Given an ERC-1155 maker
asset, the builder accepts only a positive amount and rejects a
missing, zero, or otherwise non-positive amount before signing or
broadcasting.

**T17.9.7. `MM2.db` schema compatibility.** A native database used
for candidate-1 NFT V2 support migrates through the Chapter-44
state-15 lineage without creating any NFT-swap-specific table,
column, index, migration state, or versioning rule. Schema
inspection in the test asserts that selected NFT swap intent
fields such as token contract, token id, token standard, and
ERC-1155 amount are not added to native `MM2.db` as swap
persistence.

**T17.9.8. Restart uses only compatible sources.** Given an
unfinished NFT V2 swap row, the recovery handler reads only the
generic V2 `my_swaps` fields, `events_json`, `swap_type`,
`is_finished`, `swap_version`, and any already-available
maker-payment transaction calldata. The test rejects any recovery
path that requires a Reloaded-local NFT metadata table, column, or
migration in native `MM2.db`, or that treats general NFT
wallet/cache metadata as the selected swap NFT without a
swap-bound protocol/state contract.

**T17.9.9. Pre-maker-payment recovery parks unsupported state.**
Given an unfinished NFT V2 swap whose process stops before a
maker-payment transaction is available, and whose token contract,
token id, token standard, or required ERC-1155 amount cannot be
recovered from corpus-compatible state or a clean protocol/state
contract that binds selected NFT intent to the swap, restart
recovery parks or refuses the swap. The test asserts that no
maker-payment transaction is built, signed, broadcast, or marked
sent from fabricated NFT identity or from a token selected out of
general wallet/cache metadata.

**T17.9.10. Post-maker-payment recovery decodes calldata but does
not guess standard.** Given an already-available NFT maker-payment
transaction, restart recovery routes through the NFT
maker-operation surface and decodes calldata to recover and
validate the token contract, token id, ERC-1155 amount when
present, recipient NFT-aware maker contract, swap id, taker
address, secret hashes, and payment lock time. If the token
standard is available from corpus-compatible state, the recovery
path selects the matching ERC-721 or ERC-1155 spend/refund
operation. If the token standard is unavailable, or if required
calldata is missing or inconsistent, the recovery path parks or
refuses the NFT-specific action and does not guess from amount,
coin ticker, or local defaults.

> **Upstream divergence (informative).** The observed lineage
> exposes maker-side NFT swap operations and direct tests for
> them, but does not show a production state-machine selector that
> forces the NFT branch at the maker call site. The same lineage
> decodes NFT maker-payment calldata inside the NFT operation
> surface, while the generic restart path restores ordinary V2
> state from generic persistence. R17.9.2 through R17.9.12 bind
> the missing production routing and restart behaviour for
> Reloaded without adding NFT-swap-specific native `MM2.db`
> schema.

See [Chapter 19 — NFT Module Layout](19-nft-module-layout.md) for
the broader NFT activation surface. Native `MM2.db` persistence
for candidate-1 NFT V2 remains constrained by R17.9.9 through
R17.9.12 and Chapter 44.

---

## 17.10 State-machine integration

The V2 maker-side state machine walks the sequence:

```
Initialized
  → WaitingForTakerFunding
  → TakerFundingReceived
  → MakerPaymentSentFundingSpendGenerated
  → TakerPaymentReceived
  → TakerPaymentSpent
  → Completed
```

with the error branch:

```
... (any state) → MakerPaymentRefundRequired → MakerPaymentRefunded
... (pre-payment) → Aborted
```

The V2 taker-side state machine walks:

```
Initialized
  → Negotiated
  → TakerFundingSent
  → MakerPaymentAndFundingSpendPreimgReceived
  → MakerPaymentConfirmed
  → TakerPaymentSent
  → TakerPaymentSpent
  → MakerPaymentSpent
  → Completed
```

with parallel error/abort branches.

Both state machines dispatch through the V2 maker- and taker-side
coin traits — the same dispatch surface used by the UTXO V2 path
([Chapter 15 §15.5](15-swap-v2-utxo-path.md#155-protocol-surface)).
A single state-machine driver therefore supports UTXO×UTXO,
EVM×EVM, and cross-asset combinations (UTXO×EVM, EVM×Tendermint,
and so on). When the maker asset is an EVM NFT, the production
selection and restart rules in §17.9 override the normal fungible
maker-payment dispatch for maker-payment send, validate, spend,
and refund actions.

---

## 17.11 Watcher reward

The V2 EVM contracts have no `watcherReward` parameter; the
watcher-reward feature (see
[Chapter 9](09-watcher-reward-infrastructure.md)) is therefore a
V1-only opt-in on the EVM path. Extending it to V2 EVM requires
contract redeployment with a `watcherReward` argument across the
relevant entry points.

---

## 17.12 Tests

Unit tests are collocated with each EVM-side V2 implementation
module. End-to-end V2 EVM coverage runs against an Anvil node as
part of the docker test fleet (gated behind the `docker_tests`
feature). NFT dispatch and restart-compatibility acceptance
requirements are enumerated in T17.9.1 through T17.9.10 because
those branches are part of the maker-side state-machine and
native persistence contract.

---

## 17.13 Out of scope / known limitations

1. **Pre-burn on EVM** — see [Chapter 16](16-swap-v2-pre-burn-output.md).
   Requires contract ABI extension.
2. **Watcher reward on V2 EVM** — requires contract redeployment.
3. **NFT taker side** — intentional design decision; NFT trades
   are maker-NFT-for-taker-fungible only.
4. **Cross-EVM-chain swap atomicity** — each side runs its own
   contract; cross-chain finality is at-most-once per chain's
   confirmation policy.
5. **Gas estimation** — gas estimates use `eth_estimateGas` with
   the standard +10% safety margin shared with the V1 EVM path;
   documented in [Chapter 8](08-fee-routing-engine.md).

---

## 17.14 External References

- EIP-20 (ERC-20 fungible-token standard).
- EIP-721 (ERC-721 non-fungible-token standard).
- EIP-1155 (ERC-1155 multi-token standard).
- Solidity Contract ABI Specification (function selectors,
  argument encoding, event topic encoding).
- Ethereum JSON-RPC: `eth_call`, `eth_sendRawTransaction`,
  `eth_getLogs`, `eth_estimateGas`, `eth_blockNumber`. Reference:
  `https://ethereum.org/developers/docs/apis/json-rpc`.
- keccak-256 hash (used for ABI selector derivation, event topic
  derivation, and the protocol secret-hash check).

---

## 17.15 Baseline Verifications

The chapter relies on one baseline-state claim:

- *Claim.* The baseline tree contains no V2 EVM swap surface —
  no V2 maker-side or taker-side coin-trait family, no V2 EVM
  module, no V2 EVM contract ABIs, and no V2 maker/taker state
  machines.
- *Verification.* `git ls-tree -r c1d46c0c1592faa0860f704008b2b2381bc3840f -- mm2src/coins/eth/eth_swap_v2/`
  returns the empty set;
  `git ls-tree c1d46c0c1592faa0860f704008b2b2381bc3840f -- mm2src/mm2_main/src/lp_swap/`
  shows only the V1 `maker_swap.rs` and `taker_swap.rs` (no
  `*_v2.rs` entries); `git grep -l 'MakerCoinSwapOpsV2\|TakerCoinSwapOpsV2\|EtomicSwapMakerV2\|EtomicSwapTakerV2'
  c1d46c0c1592faa0860f704008b2b2381bc3840f` returns nothing.
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md)
  enumerates the V1-only swap surface present at the baseline.

---

## 17.16 Provenance Footer

- *Status:* driving-spec.
- *Version:* v2.
- *Verified against:* baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`; EIP-20, EIP-721,
  EIP-1155; the Solidity Contract ABI Specification; the
  Ethereum JSON-RPC method definitions for `eth_call`,
  `eth_sendRawTransaction`, `eth_getLogs`, `eth_estimateGas`,
  `eth_blockNumber`; keccak-256.
- *Forbidden corpus:* consulted by the KDF Spec Reader for
  upstream-compatible behaviour; no private implementation
  expression is normative in this chapter.
