# Chapter 18 — Tendermint, IBC, and Cross-Chain HTLC Surfaces

**Status:** driving-spec

> **One-sentence claim:** the project supports atomic-swap and
> cross-chain transfer with Tendermint-based chains by speaking the
> Iris-mod and Nucleus HTLC protocols, the ICS-20 IBC transfer wire
> surface, and the V1 atomic-swap trait shape, against a platform
> coin and CW20-style token model that the baseline did not provide.

> **Availability in the beta (maintainer decision 2026-10-26).**
> Tendermint swap support is not finished, so for the coming beta every
> Tendermint platform coin and token is wallet-only: while the build
> constant `TENDERMINT_SWAPS_AVAILABLE` (`coins/lp_coins_ops.rs`) is
> `false`, activation, balances, addresses and `withdraw` work, while
> `setprice`, `buy`, `sell` and `start_swap` refuse the coin with the
> usual "… is wallet only" answer, Tendermint coins decline to match, and an
> order with a Tendermint coin saved by an earlier version is cancelled
> at startup. A swap that has already started still completes or is
> recovered. Swaps are postponed, not dropped: they are planned to
> return in a later release, and the swap rules of this chapter
> (§18.3, §18.5) stay the target for that return.

---

## 18.0 Executive Summary

Tendermint-based blockchains expose a JSON-RPC and gRPC interface
shape distinct from both UTXO chains and EVM chains: transactions
are signed Protobuf `SignDoc` bundles, accounts are bech32-encoded,
balances are multi-denom, and on-chain Hash-Time-Locked Contracts
are provided by a dedicated chain module rather than by a script
language or by a smart contract the wallet must deploy. This chapter
describes how the project speaks to that chain family for the
purposes of atomic swaps and IBC-routed transfers.

The design covers two HTLC dialects (Iris-mod and Nucleus), the
ICS-20 IBC transfer wire surface, a platform-coin type that holds
a single native denom, and a token type that rides on the same
platform-coin connection and holds a single additional denom
(native or CW20-encoded). The trait surface implemented on these
types is the project's existing V1 atomic-swap interface; an extra
branch is provided for the dex-fee burn variant introduced in
[Chapter 16](16-swap-v2-pre-burn-output.md).

Four sub-features are intentionally deferred from this chapter to
later work: a task-managed activation RPC for batching token
enablement, a stand-alone IBC transfer RPC handler that exposes the
wire surface to GUI callers, a balance-event Server-Sent-Events
producer wired into the streaming infrastructure of
[Chapter 10](10-sse-streaming.md), and an integration with the
project's v2 tx-history framework. They are listed in §18.7 with
the external inputs that fix their shape.

Permitted inputs that fix the shape of this chapter:

- The Cosmos SDK proto definitions for `cosmos.base.v1beta1.Coin`,
  `cosmos.bank.v1beta1.MsgSend`, and `cosmos.bank.v1beta1.MsgMultiSend`.
- The Iris-mod HTLC proto definitions published at
  `https://github.com/irismod/htlc`.
- The ICS-20 (`ibc.applications.transfer.v1.MsgTransfer`) wire
  definition published in the IBC specification family.
- BIP-173 bech32 encoding.
- The CometBFT ABCI-Query specification.
- The published JSON-RPC interface of CometBFT nodes for tx
  broadcast, block subscription, and tx search.

The chapter is structured as:

| Section | Topic |
|---------|-------|
| §18.1   | Tendermint chain landscape and dialect split (Iris vs Nucleus) |
| §18.2   | Platform-coin and CW20-style token types |
| §18.3   | HTLC protocol — wire types, ABCI query paths, lifecycle |
| §18.4   | IBC transfer (`MsgTransfer`) wire surface |
| §18.5   | V1 atomic-swap surface — payment, validation, secret extraction |
| §18.6   | Multi-denom and CW20-style token support |
| §18.6A  | Cosmos staking / delegation RPC surface (implemented) |
| §18.7   | Deferred + required-not-yet sub-features (activation RPC, IBC RPC, balance events, tx history, IBC swap-routing precondition) |
| §18.8   | External references |
| §18.9   | Baseline Verifications |
| §18.10  | Provenance Footer |
| §18.11  | Swap behavior of the reference (passed the KDF Dirty Gate on 2026-11-08): time lock, fee and gas, HTLC id, refund recognition, confirmations, fee validation, dialect refusal, watchers, designation agreement |

---

## 18.1 Tendermint chain landscape

The project supports two flavours of Tendermint-based chain:

- **Iris** — chains running the Iris-mod HTLC module (e.g. IRIS,
  ATOM via IBC bridges using the Iris HTLC fork). Wire prefix:
  `irismod.htlc`.
- **Nucleus** — chains running the Nucleus HTLC module (a derivative
  of Iris with adjustments). Wire prefix: `nucleus.htlc`.

The two dialects share an identical *message shape* (create-HTLC
and claim-HTLC) but differ in:

- The Protobuf type URL on the wire.
- The ABCI query path used to fetch HTLC state by id
  (`/irismod.htlc.Query/HTLC` vs `/nucleus.htlc.Query/HTLC`).
- The Iris dialect carries extra optional fields
  (`transfer`, `receiver_on_other_chain`, `sender_on_other_chain`)
  that record cross-chain provenance; Nucleus omits them.

Within the Tendermint coin module, a thin dialect-abstraction
layer reconciles the two dialects behind two sum types:

- a *create-HTLC message* that is a sum over the Iris and Nucleus
  create-HTLC forms;
- a *claim-HTLC message* that is a sum over the Iris and Nucleus
  claim-HTLC forms.

Higher-level swap code holds a trait object over the HTLC
abstraction and dispatches without knowing the dialect.

---

## 18.2 Platform-coin and CW20-style token types

The chain family is represented in the project as two cooperating
types:

- A **platform-coin type** that owns the connection to a chain's
  CometBFT RPC endpoints, the per-chain configuration (bech32 HRP,
  chain id, gas price, native denom, decimals), the signing key,
  and the implementation of the project's general-purpose coin
  trait surface and the V1 atomic-swap trait surface.
- A **CW20-style token type** that holds a per-token denom (either
  a native denom exposed by the chain's bank module, or a
  `cw20:<contract-addr>` denom that the chain's CW20 module
  resolves to a contract-managed balance) and forwards all RPC and
  signing operations to a shared reference to the platform-coin
  type.

Neither type holds its own networking state. Multiple token
instances created against the same platform-coin instance share its
RPC client, its mempool view, and its signing primitives; spawning
a new token costs one allocation plus a per-token denom string.

### 18.2.1 Configuration

The per-coin configuration record carries:

- `account_prefix` — bech32 HRP (e.g. `"cosmos"`, `"iaa"`,
  `"nuc"`).
- `chain_id` — string id used in tx signing (`SignDoc.chain_id`).
- `gas_price` — base gas price (e.g. `0.025uatom`); optional, with a
  built-in default when omitted.
- `denom` — platform-coin base denom.
- `decimals` — display decimals (typically 6 or 18).
- `ibc_channels` — optional map from a **target chain's bech32
  account-prefix** to the **integer IBC channel number** on this
  chain's transfer port toward that target (integer `N` denoting the
  ICS-20 channel id `channel-N`); seeds the destination-prefix →
  channel resolution used by the IBC transfer path (§18.4). Optional,
  defaults to empty.
- `chain_registry_name` — optional Cosmos-chain-registry name;
  carried for external tooling and not consumed by any swap/IBC path.
- `rpc_urls` — list of CometBFT RPC nodes for tx broadcast + query.

> **Config source of truth (informative).** These per-coin and
> per-token fields are carried in the coin-protocol `protocol_data`
> object under the `TENDERMINT` / `TENDERMINTTOKEN` protocol tags and
> are fully pinned — with required/optional status, wire types, and
> the `ibc_channels` map shape — in
> [Chapter 36 §36.3](36-tendermint-v2-activation-rpcs.md). Activation
> reads them from `protocol_data`, not from the top-level coin conf.

The per-token configuration record adds:

- `platform` — ticker of the platform coin this token rides on.
- `decimals` — token display decimals.
- `denom` — token denom (CW20 contract address encoded as a
  `cw20:{contract_addr}` denom, or a native denom for chains that
  expose native token modules).

### 18.2.2 Address derivation

Bech32: `bech32_encode(hrp = account_prefix, data = dhash160(pubkey))`.
The pubkey hash is the standard Cosmos `RIPEMD160(SHA256(pubkey))`
combination (i.e. the same `dhash160` used by Bitcoin); the HRP
disambiguates chains. Account id is the bech32 string in full,
case-sensitive.

> **Implemented in reloaded (informative).** In addition to the
> standard `cosmos.auth.v1beta1.BaseAccount` shape, account-state
> queries also decode the ethermint account proto variant
> (`ethermint.types.v1.EthAccount`) used by EVM-compatible Cosmos
> chains, so that the account number and sequence can be read on
> those chains. Address derivation itself is unchanged from the
> bech32 form above.

---

## 18.3 HTLC protocol

### 18.3.1 Lifecycle

```
Send:    sender calls   MsgCreateHTLC(to, amount, hashLock, timeLock, ...)
              → chain stores HTLC under id = sha256(hashLock || sender || to || amount-string)
                (exact derivation: §18.11.3 R18.11.12)
Spend:   recipient calls MsgClaimHTLC(id, secret)
              → chain verifies sha256(secret) == hashLock, transfers amount to recipient
Refund:  *no broadcast required* — the chain auto-refunds in the block whose
              height reaches expiration_height = creation height + timeLock
              (timeLock is a block count; corrected 2026-11-08, see §18.11.1).
```

The auto-refund is the most important divergence from the UTXO and
EVM HTLC families: there is no `MsgRefundHTLC` message; the funds
return to the sender automatically once the timelock elapses. The
project's V1 swap surface expects every coin to be able to
broadcast a refund transaction; for this family the refund-method
family is to answer with an outcome the swap state machine
recognises as "no refund tx is needed; the chain will auto-refund".

> **Reloaded status (informative).** Both refund methods answer an
> error whose text is "Tendermint HTLCs auto-refund on chain; no
> broadcast required.". The swap state machine does not yet recognise
> that answer as a distinct outcome; teaching it to is part of the
> Tendermint swap work planned for a later release (see the
> availability note at the top of this chapter).

### 18.3.2 Wire types

`MsgCreateHTLC` (Iris dialect; Nucleus omits the three
`*_on_other_chain` fields):

| Field                       | Type                  | Notes                                    |
|-----------------------------|-----------------------|------------------------------------------|
| `sender`                    | bech32 string         | Tx signer.                                |
| `to`                        | bech32 string         | Recipient when claimed.                  |
| `receiver_on_other_chain`   | string (Iris-only)    | Optional cross-chain provenance.         |
| `sender_on_other_chain`     | string (Iris-only)    | Optional cross-chain provenance.         |
| `amount`                    | `Vec<Coin>`           | Amount in `{denom, amount}` per denom.    |
| `hash_lock`                 | hex string            | `sha256(secret)`.                         |
| `timestamp`                 | uint64                | Hash-lock salt, not a lock parameter: non-zero makes the module hash secret and timestamp together; the swap surface sets it to 0 (§18.11.1 R18.11.2). |
| `time_lock`                 | uint64                | Block count from the creating block to expiry; module interval [50, 34560] (§18.11.1 R18.11.1). |
| `transfer`                  | bool (Iris-only)      | If `true`, this HTLC is part of an IBC transfer. |

`MsgClaimHTLC`:

| Field    | Type          | Notes                            |
|----------|---------------|----------------------------------|
| `sender` | bech32 string | The claimer (must equal `to`).   |
| `id`     | hex string    | HTLC id returned by `create`.    |
| `secret` | hex string    | Preimage of `hash_lock`.         |

### 18.3.3 ABCI query

To check whether a previously-broadcast HTLC is still active, the
Rust side issues an ABCI query against the chain's HTLC module
(path varies by dialect):

```
path = "/irismod.htlc.Query/HTLC"  (or "/nucleus.htlc.Query/HTLC")
data = Protobuf-encoded { id: <hex> }
response = Protobuf-encoded HTLC state (state, balance, ...)
```

The Rust client decodes the response and inspects the state field
(`State::Open`, `State::Completed`, `State::Refunded`).

---

## 18.4 IBC transfer

The IBC transfer wire type `MsgTransfer` matches the Cosmos SDK
proto definition `ibc.applications.transfer.v1.MsgTransfer`:

| Field               | Type                       | Default          |
|---------------------|----------------------------|------------------|
| `source_port`       | string                     | `"transfer"`     |
| `source_channel`    | string                     | per-route param  |
| `token`             | `Coin`                     | required         |
| `sender`            | bech32 string              | tx signer        |
| `receiver`          | bech32 string (other chain HRP) | required    |
| `timeout_height`    | `{revision_number, revision_height}` | unused (set to 0) |
| `timeout_timestamp` | uint64 nanoseconds         | `now + 15 min`   |

Defaults:

- `timeout_timestamp = block-time + 15 minutes` (configurable via
  the deferred RPC handler in §18.7.2).
- Gas limit: 150,000 gas units (a withdrawal figure; swap transactions use 125,000, §18.11.2 R18.11.7).

The transfer wire surface is fully defined here; the stand-alone
RPC handler that exposes it to GUI callers is one of the deferred
sub-features (§18.7.2).

---

## 18.5 V1 atomic-swap surface

The project's V1 atomic-swap trait (the baseline `SwapOps` surface,
which predates the baseline commit) is implemented on the
platform-coin type from §18.2. The implementation carries sixteen
methods; the table below lists the eleven that carry behavioural
intent for this chapter (the remaining five — the two refund
methods, the swap-contract-address negotiator, the HTLC key-pair
accessor, and the platform-coin pubkey helper — are either
auto-refund answers or thin delegations to chain-wide
configuration):

| Method                                  | Behaviour                                                                            |
|----------------------------------------|--------------------------------------------------------------------------------------|
| `send_taker_fee`                       | Branches on `DexFee`: `Standard` → single bank send; `WithBurn` → multi-output bank send (split). |
| `send_maker_payment`                   | Builds and broadcasts a create-HTLC message for the payment denom.                  |
| `send_taker_payment`                   | Builds and broadcasts a create-HTLC message for the payment denom.                  |
| `send_maker_spends_taker_payment`      | Builds and broadcasts a claim-HTLC message revealing the secret.                    |
| `send_taker_spends_maker_payment`      | Builds and broadcasts a claim-HTLC message revealing the secret.                    |
| `validate_fee`                         | Decodes the fee tx, asserts denoms and recipients per `DexFee`.                      |
| `validate_maker_payment`               | Validates the on-chain HTLC for the payment denom against negotiated parameters.    |
| `validate_taker_payment`               | Validates the on-chain HTLC for the payment denom against negotiated parameters.    |
| `check_if_my_payment_sent`             | Searches the coin's own create-HTLC transactions (`tx_search` on `create_htlc.sender`, newest first) and returns the one whose hash lock equals the swap's secret hash. |
| `search_for_swap_tx_spend_my/other`    | Cosmos-tx-search for a claim-HTLC referencing the HTLC id.                           |
| `extract_secret`                       | Decode a claim-HTLC from a spend tx and return the `secret` field.                  |

The two refund-method entries answer the auto-refund error of
§18.3.1, which the swap state machine does not yet recognise as a
distinct outcome (see the Reloaded status note there). The
swap-contract-address negotiator returns `None` (Tendermint HTLC
does not use a separate swap-contract address) and the HTLC
key-pair accessor returns the coin's activated key pair (the HTLC
key pair is the coin's primary signing key; a token returns its
platform coin's pair).

### 18.5.1 Pre-burn (`DexFee::WithBurn`) is supported

Tendermint implements the `DexFee::WithBurn` branch (introduced by
[Chapter 16](16-swap-v2-pre-burn-output.md)) on the V1 fee-send
path. Implementation:

- `WithBurn { fee_amount, burn_amount, burn_destination:
  PreBurnAccount { burn_pubkey } }` → build a `MsgMultiSend` with:
  - `inputs[0] = { address: sender, coins: fee_amount + burn_amount }`.
  - `outputs[0] = { address: fee_address, coins: fee_amount }`.
  - `outputs[1] = { address: bech32(dhash160(burn_pubkey)), coins: burn_amount }`.
- `WithBurn { burn_destination: KmdOpReturn }` → rejected
  (`KmdOpReturn` is a UTXO concept; Tendermint has no OP_RETURN).
- `Standard` → single `MsgSend`.
- `NoFee` → rejected with an internal error; no transaction is
  built or broadcast.

Tendermint is treated as a V1-only counterparty by the V2
state-machine driver. The V2 maker- and taker-side trait surfaces
are not implemented for this chain family in this chapter's scope;
treatment of a V2 path for Tendermint is out of scope here.

> **Code-quality finding (informative).** Tendermint's fee-send
> implementation rejects `DexFee::NoFee` with an error rather than
> completing a fee-less send. The UTXO fee-send implementation
> ([Chapter 16](16-swap-v2-pre-burn-output.md) family) treats
> `NoFee` as a zero-output, gracefully-completed case instead of an
> error. Since 2026-09-27 the factory of
> [Chapter 16 §16.4](16-swap-v2-pre-burn-output.md) returns `NoFee` for
> any non-privacy taker whose key equals the network burn key (Chapter
> 16 R7, Chapter 08 R15C), independent of the burn predicates. So a
> Tendermint taker can now receive `NoFee`. The legacy state machine
> never calls fee-send for `NoFee` (Chapter 51 R27), so the rejecting
> branch is still unreachable through a swap. It stays a latent
> inconsistency for any other caller. Aligning
> the Tendermint branch with the UTXO precedent (zero-output success
> rather than a rejection) is a reasonable fix; not applied here
> because it is untested production behavior change outside this
> chapter's documentation-accuracy scope.

### 18.5.2 Trading floors (Spec Reader 2026-10-25; cleared by the Dirty Gate with chapter 60 on 2026-10-25)

A Tendermint platform coin's, and a Tendermint token's, minimum trading
volume and minimum transaction amount are both one base unit of that
coin or token — `10^-decimals` of its own configured decimals — in both
reference designations (`v2.6.0-beta` and `v3.0.0-beta` agree), and in
reloaded. The HTLC claim fee is paid by the claiming account, not
deducted from the HTLC amount, so no fee deduction makes a floor-sized
Tendermint payment unspendable. Consumption and refusal points:
[Chapter 60](60-trading-floors-and-refusal-contract.md) (row C1).

---

## 18.6 Multi-denom and CW20-style token support

The platform-coin type owns a single native denom; each token
instance attached to it adds a single extra denom. Tokens share
the platform's RPC client and signing primitives by reference, so
attaching a new token costs one allocation plus a per-token denom
string. The HTLC and IBC wire types take `Vec<Coin>` (a list of
`{denom, amount}` pairs), so a single transaction can move multiple
denoms; the V1 swap surface only ever uses a single denom per HTLC
(no multi-asset swaps in the V1 protocol).

CW20-style tokens are represented by denom strings of the form
`cw20:<contract_addr>`; the platform coin's CW20 module handles
these natively and no separate code path is needed beyond
constructing the right denom string.

---

## 18.6A Cosmos staking RPC surface

This capability is **implemented in the reloaded baseline** (it is
not deferred). The Tendermint platform-coin family exposes a
staking/delegation RPC surface for proof-of-stake chains. Both a
flat set of method strings and an `experimental::staking::`
namespaced set route to the same handlers. The dictated JSON-RPC
method strings are:

| Method (flat) | Namespaced alias | Purpose |
| --- | --- | --- |
| `add_delegation` | `experimental::staking::delegate` | build, sign, and broadcast a delegation to a validator |
| `remove_delegation` | `experimental::staking::undelegate` | begin undelegation (unbonding) of a delegation |
| `claim_staking_rewards` | `experimental::staking::claim_rewards` | withdraw accrued staking rewards |
| `validators_info` | `experimental::staking::query::validators` | list validators with metadata and status |
| `delegations_info` | `experimental::staking::query::delegations` | list the account's active delegations |
| `ongoing_undelegations_info` | `experimental::staking::query::ongoing_undelegations` | list in-flight unbonding entries |
| `get_staking_infos` | — | summary staking position for the account |

Binding requirements:

- **R-S1.** The flat method names and the `experimental::staking::`
  namespaced aliases above MUST both be accepted and MUST route to
  the same handler, so existing callers and the namespaced surface
  remain interchangeable.
- **R-S2.** The delegation-mutating methods MUST construct, sign,
  and broadcast the corresponding Cosmos SDK staking and
  distribution messages
  (`cosmos.staking.v1beta1.MsgDelegate`,
  `cosmos.staking.v1beta1.MsgUndelegate`, and
  `cosmos.distribution.v1beta1.MsgWithdrawDelegatorReward`)
  using the same signing source as ordinary transfers.
- **R-S3.** The query methods MUST read validator, delegation,
  reward, and unbonding state from the chain's query endpoints and
  return decoded amounts in the coin's display precision.

---

## 18.7 Deferred sub-features

Four sub-features lie within the natural scope of Tendermint support
but are deferred from this chapter. Each is described here at the
level of the external interface it must produce; the design work
that fixes their internals belongs to a later chapter.

### 18.7.1 Task-managed activation RPC

The project's general-purpose `enable` RPC activates a single coin
synchronously and returns once activation has completed or failed.
A Tendermint platform-coin commonly arrives together with several
CW20-style tokens and a list of CometBFT RPC endpoints that must
each be probed before activation can return; the synchronous shape
produces a slow, GUI-blocking call.

The deferred shape is a task-managed RPC pair following the
project's existing init/status pattern (see the HD-wallet and
hardware-wallet activations for the same shape elsewhere in the
project): an `init` call returns a task id immediately and a
`status` call polls progress, with intermediate states for
endpoint probing, per-token enablement, and final readiness. The
input parameters are the chain configuration, the list of token
definitions to bring online in the same call, and the choice of
signing source (the project's HD key derivation, or an active
WalletConnect session per [Chapter 22](22-walletconnect-v2.md)).

### 18.7.2 IBC transfer RPC handler

The `MsgTransfer` wire type defined in §18.4 is already constructed,
signed, and broadcast in code paths that handle cross-chain HTLC
resolution. The deferred sub-feature is a dedicated RPC handler
that exposes the same construction path to GUI callers as a
stand-alone operation: arguments are the destination chain's
channel id, the recipient bech32 address, the source-denom amount,
and an optional timeout override.

### 18.7.3 Balance-event streaming

The project carries a Server-Sent-Events producer infrastructure
described in [Chapter 10](10-sse-streaming.md). A balance-event
producer for a Tendermint chain subscribes to the CometBFT
WebSocket endpoint's `tm.event='Tx'` and `tm.event='NewBlock'`
streams, filters for transfers whose sender or recipient equals the
active account, and emits a balance-update event through the SSE
framework. The deferred sub-feature is the producer; the framework
it plugs into already exists.

### 18.7.4 Tx-history v2 integration

The project's v2 tx-history framework defines a coin-side trait for
tx ingestion, classification, and storage. A Tendermint binding for
that framework queries the chain's tx-search RPC, classifies each
entry by its message type (`MsgSend`, `MsgMultiSend`, `MsgCreateHTLC`,
`MsgClaimHTLC`, `MsgTransfer`), and persists the result through the
framework's storage layer. The framework's own chapter is the
authoritative description of the trait shape; this section records
only that a Tendermint binding is in scope and not yet written.

### 18.7.5 IBC swap-routing minimum-balance precondition (implemented)

> **STATUS: implemented in reloaded behind the
> `ibc-routing-for-swaps` feature flag.** Unlike §18.7.1–§18.7.4
> (informational deferrals), this sub-feature is a binding
> driving-spec item that is now ported.

When a maker offers an order whose two coins live on different
IBC-connected Cosmos chains, the swap's hash-time-locked leg is
settled on one chain and the proceeds are moved over an ICS-20 IBC
transfer (§18.4). To avoid publishing orders that cannot complete
for lack of routing funds, an order-making precondition checks
that the coin which will carry the HTLC leg holds at least a
configured minimum balance before the order is published.

Binding requirements:

- **R-IBC1.** The capability MUST be guarded by a build-time
  feature flag named `ibc-routing-for-swaps`; when the flag is
  off, order-making behaviour is unchanged from the current
  baseline.
- **R-IBC2.** A Tendermint coin MUST accept an optional per-coin
  configuration value `min_balance_for_ibc_routing` (a decimal
  number). When absent, the default minimum MUST be 2.0 units of
  the HTLC-carrying coin.
- **R-IBC3.** At order creation, when the order pairs coins that
  require IBC routing, the engine MUST compare the HTLC-carrying
  coin's spendable balance against the configured minimum and MUST
  reject order creation with a typed insufficient-balance error
  when the balance is below the minimum. The error MUST identify
  the HTLC coin ticker, the required minimum, and the current
  balance.

Acceptance criteria:

- With the feature flag enabled and a per-coin minimum configured
  (or defaulted to 2.0), creating an order that requires IBC
  routing while the HTLC coin balance is below the minimum is
  rejected before publication; raising the balance to or above the
  minimum allows the order to publish.
- With the feature flag disabled, order creation ignores the
  minimum entirely.

> **Implementation note (reloaded):** maker-order creation enforces
> this check in the ordermatch trading path; `min_balance_for_ibc_routing`
> is read from coin config (default `2.0`), cross-chain Tendermint pair
> routing is detected from protocol chain ids, and rejection includes
> ticker/required/current values.

External inputs that fix the shape: the ICS-20 transfer wire
surface of §18.4 and the project's existing order-creation
validation path. The internal decomposition is discretionary.

---

## 18.8 External References

- Cosmos SDK `Coin` type: `cosmos.base.v1beta1.Coin`.
- Cosmos SDK bank module: `cosmos.bank.v1beta1.MsgSend`,
  `cosmos.bank.v1beta1.MsgMultiSend`.
- Iris-mod HTLC proto definitions:
  `https://github.com/irismod/htlc`.
- Nucleus HTLC: post-fork derivative of the Iris-mod HTLC proto.
- ICS-20 IBC transfer: `ibc.applications.transfer.v1.MsgTransfer`.
- bech32 encoding: BIP 173.
- ABCI query: CometBFT specification §"ABCI: Query".
- CometBFT JSON-RPC: tx broadcast, block subscription, and tx-search
  endpoints of the CometBFT node interface.

---

## 18.9 Baseline Verifications

The chapter relies on one baseline-state claim:

- *Claim.* The baseline tree contains no Tendermint-family coin
  support, no IBC wire types, and no on-chain HTLC support for any
  Tendermint-based chain.
- *Verification.* `git ls-tree -r c1d46c0c1592faa0860f704008b2b2381bc3840f -- mm2src/coins/tendermint`
  returns the empty set; `git grep tendermint c1d46c0c1592faa0860f704008b2b2381bc3840f -- mm2src/coins/`
  matches nothing.
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md)
  enumerates the chain families present at the baseline; the
  Tendermint family is absent from that enumeration.

---

## 18.10 Provenance Footer

- *Status:* driving-spec.
- *Version:* v2.
- *Verified against:* baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`; chapter 31 (the
  central application-context substrate the Tendermint coins
  context is fetched through as a sub-context slot per chapter 31
  R7); [Chapter 16](16-swap-v2-pre-burn-output.md) (the `DexFee`
  descriptor and per-coin burn-policy substrate consumed by
  §18.5.1); the present-day working tree, re-verified for this
  revision against `mm2src/coins/tendermint/tendermint_swap_ops.rs`
  for the `DexFee` match arms bound in §18.5.1, and on 2026-10-26
  against the same file, `tendermint_token.rs` and the swap state
  machine for the refund answers, `check_if_my_payment_sent` and the
  HTLC key-pair accessor (§18.3.1, §18.5); Cosmos SDK proto
  definitions for `Coin`, `MsgSend`, `MsgMultiSend`; ICS-20
  (`ibc.applications.transfer.v1.MsgTransfer`); the Iris-mod HTLC
  proto repository at `https://github.com/irismod/htlc`; BIP-173
  bech32; the CometBFT ABCI-Query and JSON-RPC specifications.
- *Forbidden corpus:* not consulted, except for §18.5.2 (added
  2026-10-25): under the chapter-01 two-team procedure, at the
  designations `v2.6.0-beta` and `v3.0.0-beta`, only to establish that
  both report one base unit as a Tendermint coin's and token's minimum
  trading volume and minimum transaction amount. No discretionary
  expression was reproduced. §18.5.2 passed the KDF Dirty Gate on
  2026-10-25 with [Chapter 60](60-trading-floors-and-refusal-contract.md)
  (PASS, measure 0.02, no findings; scope attestation recorded in
  chapter 60 §60.12).
  §18.11 (added 2026-11-08, Spec Reader round 0.4.0-7) was distilled
  from the same two designations and from the `dev (as of 2026-03-27)`
  row, read read-only, for the swap behavior of the Tendermint module
  (time lock, fee and gas, HTLC id, refund, confirmations, fee
  validation, dialects, watchers); the §18.3.1 lifecycle, the
  §18.3.2 `timestamp`/`time_lock` rows and the §18.4 gas-unit line were
  corrected in the same pass from the public irismod module semantics.
  The verification notes of §18.11 name a public-network observation (a
  50-block HTLC on the public Iris testnet `nyancat-9` refunds in about
  four minutes, with state `Refunded` and no transaction); it is an input
  of permitted-input class R6.
  *Dirty Gate clearance (chapter 01 T1, R40).* **KDF Dirty Gate: PASS on
  2026-11-08, measure 0.05, no findings.** The pass covered §18.11 and
  the corrections of §18.3.1, §18.3.2 and §18.4 made in the same pass.
  *Extent:* the Tendermint HTLC swap subsystem: the construction and
  validation of the create and claim messages, fee and gas, the HTLC
  id, the spend search and refund recognition, confirmation and claim
  polling, the sending and validation of the DEX fee, the protocol
  dialects, the watcher and burn hooks, and the IBC gating; compared at
  `v3.0.0-beta` (10 files), at `v2.6.0-beta` (a 3-file difference
  against `v3.0.0-beta`) and at `dev (as of 2026-03-27)` (a 20-file
  comparison, identical). *Method:* line-by-line reading with a
  mechanical difference and token scan; the gate script was not run
  because there are no code blocks. *Measure:* 0.05 on the
  discretionary-expression basis.

---

## 18.11 Swap behavior of the reference

> **Provenance status: passed the KDF Dirty Gate on 2026-11-08, measure
> 0.05** (attestation in the provenance footer above). This
> section was written by the dirty-side specification author on
> 2026-11-08 (Spec Reader round 0.4.0-7) from the reference designations
> `v2.6.0-beta` and `v3.0.0-beta`, from the public Cosmos SDK and
> irismod HTLC module specifications, and from this repository's own
> tree. It answers the
> open questions of the project's Tendermint swap analysis (time lock,
> fee and gas, HTLC id and hash lock, refund recognition, confirmations
> and claim polling, fee validation, IBC denoms and dialect refusal,
> watchers, designation differences). Rules are numbered R18.11.x;
> maintainer decisions D18.11.x.

The two designations behave identically for everything in this section
(§18.11.9). The behavior below is the behavior of a reference node on
netid 8762 and on netid 6133 alike; nothing in it is network-dependent.

### 18.11.1 Time lock of the create-HTLC message

The on-chain HTLC module (public irismod `x/htlc` and its Nucleus
derivative) expresses the lock as a **number of blocks**: the module
records `expiration_height = inclusion height + time_lock` and refunds
the HTLC automatically in the block that reaches that height. The
`timestamp` field is not a lock parameter: when it is non-zero the module
derives the hash lock from the secret *and* the timestamp; when it is
zero the hash lock is the plain SHA-256 of the secret. The module accepts
`time_lock` only in the closed interval 50 to 34,560 blocks.

- **R18.11.1.** *Unit and derivation.* The `time_lock` field of a
  create-HTLC message MUST be a block count derived from the swap's
  negotiated payment lock duration in seconds (the "lock duration" of
  [Chapter 51 R10](51-legacy-v1-swap-state-machine.md), *not* the
  maker-side multiplied locktime) as the integer quotient of that
  duration by the coin's configured `avg_blocktime` (coins-file field,
  whole seconds, required for activation), then clamped into the module's
  interval [50, 34560]. The same formula applies to the maker payment and
  the taker payment: both reference sides pass the *base* lock duration.

> **As-built note (2026-11-08, clean side, PR #299).** `TendermintConf`
> refuses an `avg_blocktime` of 0 at activation with the existing
> `TendermintInitError` shape, naming the key; the block-count division
> keeps a divisor of at least 1 regardless.
- **R18.11.2.** *Timestamp.* The `timestamp` field MUST be zero, so that
  the hash lock is the plain 32-byte SHA-256 of the swap secret
  ([Chapter 51 R72](51-legacy-v1-swap-state-machine.md): a Tendermint
  coin on either side forces the 32-byte algorithm).
- **R18.11.3.** *Hash lock encoding.* `hash_lock` MUST be the
  hex encoding of the 32-byte secret hash; the reference emits lower-case
  hex and compares hash locks case-insensitively, while HTLC ids are
  upper-case hex. An implementation MUST accept either case of a hash
  lock on input.
- **R18.11.4.** *Other create fields.* `sender` is the payer's own bech32
  account; `to` is the bech32 account derived from the counterparty's
  HTLC public key (`dhash160`, §18.2.2) with the coin's own prefix;
  `amount` is exactly one `{denom, amount}` coin in base units of the
  payment denom (platform denom for the platform coin, the token's denom
  for a token); the Iris-only fields `receiver_on_other_chain` and
  `sender_on_other_chain` are empty strings and `transfer` is `false`.
  The transaction memo is empty.
- **R18.11.5.** *Counterparty lock validation (receiving side).* The
  receiving side MUST decode the counterparty's create-HTLC message
  (exactly one message in the transaction body, of the dialect the coin's
  account prefix selects) and MUST require it to be *field-for-field
  equal* to the message it would itself have built from the swap
  parameters: sender = counterparty's derived account, `to` = own
  account, the single expected coin, the hex secret hash, `timestamp` 0,
  and `time_lock` recomputed by R18.11.1 from the same base lock
  duration. It MUST then fetch the transaction from a node by its hash
  (SHA-256 of the raw transaction bytes, upper-case hex), require the
  node's copy to equal the supplied bytes, query the HTLC by its id
  (§18.11.3) and require the module state `Open`. Any other state is the
  distinct unexpected-payment-state validation error.

> **Upstream divergence (informative).** The validation of R18.11.5 is an
> equality check, not a sufficiency check: a counterparty's lock is
> accepted only when it equals the receiver's own estimate, which couples
> the two peers to the same `avg_blocktime` value and the same clamping.
> Two peers whose coins files disagree on `avg_blocktime` for the same
> chain cannot swap with each other. The reference does not check that
> the lock is *long enough* for the swap in any other way.

> **Code-quality finding (informative; trigger: contradiction between two
> things read side by side).** The reference derives the *maker* payment's
> block lock from the base lock duration (R18.11.1), while the swap
> machine fixes the maker payment's locktime at start time plus the lock
> duration scaled by the maker-locktime multiplier (default two; Chapter
> 51 R10/R22/R25 call this asymmetry the protocol's safety margin). For a
> Tendermint maker payment the on-chain expiry therefore falls at roughly
> *half* the time the machine assumes, at about the same moment the
> taker's payment on the other chain becomes refundable. The maker, who
> knows the secret, can wait for its own HTLC to auto-refund and then
> claim the taker's payment; the machine's refund deadlines (Chapter 51
> R18) do not protect against this because they are computed from the
> doubled locktime. Proposed fix (decision D18.11.1): derive the maker
> payment's block lock from the *multiplied* maker locktime, and make the
> taker's validation of a maker payment require at least that; this
> breaks message equality with a reference counterparty and so must be
> paired with the compatibility decision below.

- **D18.11.1 (maintainer decision).** Interoperability versus safety for
  the maker lock: (a) reproduce R18.11.1 exactly so that a reference
  counterparty's equality check passes, accepting the halved maker
  margin on Tendermint maker payments; (b) use the multiplied maker
  locktime for our own maker payments and require a minimum relative
  lock when validating, giving up swaps with reference makers/takers on
  the affected leg; (c) a negotiated or network-conditional choice.
  Chapter 51 R10 and the 8762 contract (`AGENTS.md` §3) favor (a) for
  netid 8762 unless the maintainer accepts the incompatibility.
- **D18.11.2 (maintainer decision).** Whether validation should tolerate
  a lock *longer* than the receiver's estimate (a sufficiency check in
  addition to, or instead of, equality), which the reference does not do.

### 18.11.2 Fee and gas for create, claim and the dex fee

- **R18.11.6.** *Simulation-based fee.* Before broadcasting a create,
  claim or fee-send transaction the sender MUST simulate the signed
  transaction through the Cosmos tx service's simulate query
  (`/cosmos.tx.v1beta1.Service/Simulate`) with a zero fee and the fixed
  gas limit of R18.11.7, and MUST set the fee amount to
  `ceil(gas_used × 1.5 × gas_price)` in base units of the **platform
  denom**, where `gas_price` is the coin's configured gas price or 0.25
  when the configuration omits it. A simulation rejected for an account
  sequence mismatch MUST be retried with the sequence the node reports;
  any other non-zero simulation code is a fee-estimation failure.
- **R18.11.7.** *Gas limit.* Every swap transaction (create, claim, fee
  send) carries the fixed gas limit 125,000 gas units. The figure 150,000
  is the gas limit of an IBC `MsgTransfer` withdrawal (§18.4) and plays
  no role in swaps. Both are gas units; neither is a nanosecond value.
- **R18.11.8.** *Height timeout.* Every swap transaction MUST carry a
  body `timeout_height` of the current chain height plus 100 blocks.
- **R18.11.9.** *External signer fallback.* When the coin has no local
  private key (external signing session), the fee MUST be computed from a
  fixed assumed gas usage of 50,000 instead of a simulation:
  `ceil(50,000 × 1.5 × gas_price)`.
- **R18.11.10.** *Trade-fee preimages.* The sender-side trade fee MUST be
  estimated by simulating a create-HTLC to the fee-address account for the
  requested amount with a fresh random 32-byte hash lock and a 1,750-block
  lock; the receiver-side trade fee MUST reuse the same simulation (the
  reference treats claim and create cost as equal because a claim cannot
  be simulated before the HTLC exists); the fee-to-send-the-dex-fee MUST
  be estimated by simulating a bank send of the fee amount to the
  fee-address account. All three report `paid_from_trading_vol = false`.
  The amount does not influence the fee, so the generic trade-fee query
  uses the minimum transaction amount.
- **R18.11.11.** *Tokens pay in the platform denom.* A token's HTLC and
  fee-send transactions carry the token denom in the message amount and
  the platform denom in the fee; a token's receiver-side trade fee is the
  platform coin's.

> **Code-quality finding (informative; trigger: apparent with little
> extra effort).** For a token, the reference labels the sender-side
> trade fee and the fee-to-send-the-dex-fee with the *token's* ticker
> although the amount is paid in the platform denom (and, in the second
> case, converts the base-unit figure with the token's decimals). The
> swap machine reserves and checks balances by that label, so a token
> taker's platform-coin fee is checked against the token balance. The
> receiver-side fee is labeled with the platform coin, as its own comment
> says it must be. Proposed fix: label all three with the platform ticker
> and convert with the platform decimals.

> **As-built note (2026-11-08, clean side, PR #299).** The proposed fix is
> applied: for a token, the sender-side trade fee and the
> fee-to-send-the-dex-fee are labeled with the platform ticker and
> converted with the platform decimals. The two `_for_denom` helpers take
> the payment denom and its decimals for the amount only; the fee label
> and decimals are the platform coin's own. The receiver-side fee is
> unchanged.

### 18.11.3 HTLC id, hash lock and the claim

- **R18.11.12.** *HTLC id.* The id under which the module stores an HTLC
  is the upper-case hex SHA-256 of the concatenation: the 32 raw bytes of
  the hash lock, the raw bytes of the sender account, the raw bytes of the
  `to` account, and the ASCII string of the amount in the module's coin
  notation (`<integer amount><denom>`, several coins joined by commas in
  the module's sorted order). This is the public module's own id
  derivation; an implementation MUST reproduce it exactly, because every
  query, claim and spend search addresses the HTLC by this id.
- **R18.11.13.** *Claim message.* A claim MUST carry the claimer's own
  bech32 account as `sender`, the id of R18.11.12 and the 32-byte secret
  hex-encoded. The claimer derives the id from the counterparty's
  create-HTLC transaction (its sender, `to` and amount fields) together
  with the swap's secret hash; it does not trust an id carried in the
  payment. The claim transaction's memo is empty, its fee and gas follow
  §18.11.2.
- **R18.11.14.** *Secret extraction.* The secret is recovered from a
  claim transaction by decoding its first message as the dialect's claim
  message and hex-decoding its `secret` field to exactly 32 bytes.

> **As-built note (2026-11-08, clean side, PR #299).** `extract_secret`
> (coin and token) refuses a secret whose hex decodes to any length other
> than 32 bytes, with an error naming the length.

### 18.11.4 Refund: what the reference does and what it reports

The module refunds an expired HTLC itself; there is no refund message
(§18.3.1).

- **R18.11.15.** *Reference refund outcome (both designations).* The
  reference does **not** declare the Tendermint family auto-refundable to
  the swap machine: both refund methods answer a plain error, the family
  does not override the wall-clock refundability hook, and the machine's
  execute stage therefore waits until the locktime has passed, calls the
  refund method, receives the error and records a *refund-failed* event
  (`MakerPaymentRefundFailed` / `TakerPaymentRefundFailed`, Chapter 51
  R18/R31) even though the funds have returned on chain. Manual recovery
  of such a swap reports the payment as already refunded (R18.11.16) and
  stops. Reloaded MUST NOT copy this outcome; it is the behavior the
  reference exhibits, recorded here so that the field can be read.
- **R18.11.16.** *Refund recognition by query.* The spend search for a
  payment MUST compute the HTLC id (R18.11.12) from the payment
  transaction's create message and the swap secret hash, query the module
  state, and answer: `Open` → no spend; `Completed` → the claim
  transaction found by a transaction search `claim_htlc.id='<id>'`
  (ascending, first result; its absence is an error); `Refunded` → a
  *refunded* result whose transaction is an **empty placeholder** (the
  module records no transaction for an automatic refund). Any other
  state is an error.
- **R18.11.17.** *Reloaded refund path.* Reloaded MUST treat
  the family as auto-refundable in the sense of Chapter 51 R18: the
  execute stage waits for the chain's own refund instead of broadcasting.
  The wait MUST be **height-based**: poll the HTLC state (R18.11.16) and
  finish with `*PaymentRefunded` carrying no transaction identifier when
  the state is `Refunded`; finish as a *spend* if the state turns
  `Completed` meanwhile (the counterparty claimed; the machine's spend
  path applies); and keep polling while the state is `Open`, because the
  module's expiry is `inclusion height + time_lock` and a wall-clock
  estimate from `avg_blocktime` can be early. The wall-clock locktime
  remains the deadline for giving up with a refund-failed event.
- **D18.11.3 (maintainer decision).** Whether the refunded outcome
  should carry an empty placeholder transaction as the reference does
  (which the recovery and history surfaces print as an all-zero hash), a
  synthetic marker naming the HTLC id and the closing block height
  (`closed_block` from the module's HTLC record), or no transaction at
  all (as Chapter 51 R18 already allows for auto-refunding coins).

### 18.11.5 Confirmations, reorgs and claim polling

- **R18.11.18.** *Confirmation rule.* A Tendermint coin and token report
  zero required confirmations and no notarization requirement, and ignore
  attempts to set either. Waiting for a payment's confirmation MUST poll
  the tx service (`/cosmos.tx.v1beta1.Service/GetTx`) by the transaction
  hash (SHA-256 of the raw bytes, upper-case hex) at the machine's
  check interval (Chapter 51: 15 seconds) until the deadline; a found
  transaction with result code 0 is confirmed; a found transaction with a
  non-zero code is a definitive failure; an absent transaction is not yet
  confirmed. No block-depth or reorg handling exists: Tendermint
  consensus gives instant finality, so inclusion is final.
- **R18.11.19.** *Claim polling.* Waiting for a payment to be spent MUST
  derive the HTLC id from the payment's create message and the swap
  secret hash and repeat the transaction search `claim_htlc.id='<id>'`
  (ascending, one result) every 5 seconds until a claim is found or the
  deadline passes; the found claim is returned re-encoded as a raw
  transaction (body, auth info, signatures).

### 18.11.6 Dex-fee transaction, its validation, and "my payment sent"

- **R18.11.20.** *Fee transaction.* The taker fee is a bank `MsgSend` of
  the fee amount to the fee address (bech32 of the dex pubkey hash with
  the coin's prefix) for the standard descriptor, or a `MsgMultiSend`
  with one input (sender, fee + burn) and two outputs (fee address with
  the fee amount, then burn address — bech32 of the burn pubkey hash —
  with the burn amount) for the burn descriptor (§18.5.1). Its memo MUST
  be the swap uuid in hyphenated textual form. Fee and gas follow
  §18.11.2. The broadcast is bounded by the fee-send deadline passed in
  by the machine.
- **R18.11.21.** *Fee validation.* The maker MUST decode the fee
  transaction body and require: exactly one message; for the standard
  descriptor a `MsgSend` whose recipient is the fee address, whose amount
  list has exactly one coin equal (denom and decimal string) to the
  expected fee in base units, and whose sender is the taker's derived
  account; for the burn descriptor a `MsgMultiSend` with exactly two
  outputs — the first to the fee address with exactly one coin equal to
  the fee, the second to the burn address with exactly one coin equal to
  the burn amount — and exactly one input whose address is the taker's
  derived account. The memo MUST equal the swap uuid. The maker MUST then
  fetch the transaction by hash from a node and require byte equality
  with the supplied transaction. A no-fee descriptor is rejected on both
  the sending and the validating side (see the §18.5.1 finding).

> **As-built note (2026-11-08, clean side, PR #299).** The validation
> also requires the message type URL of the form (`MsgSend` or
> `MsgMultiSend`). The node's copy is fetched with the tx-service
> `GetTx` query that the confirmation wait uses; its decoded transaction
> is re-encoded to the raw form for the byte comparison. The maker's fee
> validation repeats on any error, up to three more times 10 seconds
> apart, so a transaction the node has not indexed yet is retried.
- **R18.11.22.** *My-payment-sent check.* To find its own payment after a
  restart the sender MUST compute the HTLC id from its own account, the
  counterparty account derived from the counterparty pubkey, the single
  expected coin and the secret hash (R18.11.12), query the module: no
  HTLC → not sent; state `Open`, `Completed` or `Refunded` → search
  `create_htlc.id='<id>'` (descending, one result), fail if that
  transaction's result code is non-zero, decode its create message and
  accept it when its hash lock equals the module's recorded hash lock
  case-insensitively. Amount and counterparty therefore participate
  through the id.

> **As-built note (2026-11-08, clean side, PR #299).** The swap machine
> asks the new provided trait method `check_if_my_payment_sent_for_amount`
> (same arguments plus the payment amount, default: the old method), at
> its four call sites with the amount the payment send uses. The
> Tendermint coin and token override it with the id-first lookup (the
> token with its own denom and decimals, the platform's account and node)
> and answer an error from the old method, which carries no amount.

### 18.11.7 Dialects, IBC denominations and chains without an HTLC module

- **R18.11.23.** *Dialect selection and refusal.* The HTLC dialect is
  selected by the coin's bech32 account prefix: `iaa` → Iris
  (`irismod.htlc`), `nuc` → Nucleus (`nucleus.htlc`). For any other
  prefix the create, claim, validate, query and search operations fail
  with an unsupported-account-type error at the moment they are
  attempted. The reference performs **no** refusal at
  order or negotiation time in its default build: its counterparty
  protocol-info check always accepts and its protocol-info payload is
  empty; the operative guard for chains without the module (ATOM, OSMO)
  is the coins-file `wallet_only` flag. Reloaded SHOULD refuse such
  coins at order time (Chapter 60 refusal contract) rather than at
  payment time.

> **As-built note (2026-11-08, clean side, PR #299).** A coin or token
> whose account prefix selects no dialect (a token uses its platform
> coin's prefix) reports `wallet_only` true in addition to the build
> switch and the coins-file flag, so the existing order refusal applies.
> Chapter 60 R60.2.3 asks for an orderly error for a coin that cannot
> trade, which that refusal is; it needs no other shape.
- **R18.11.24.** *IBC denominations.* A token whose denom is an IBC
  voucher (`ibc/<HASH>`) or a CW20 denom is swapped exactly like a native
  token: the denom appears in the HTLC amount coin; the fee is paid in
  the platform denom (R18.11.11). No denom translation occurs.
- **R18.11.25.** *IBC-routed swaps do not exist at either designation.*
  The IBC swap-routing capability of §18.7.5 is compiled out of both
  designations' default builds (the feature is not a default feature of
  any crate), and even when enabled its swap-start hooks are unfinished:
  the two match-connection paths that would cross-check the routing
  channel stop unconditionally. The only live pieces are the order-time
  minimum-balance check and the channel id carried in order metadata.
  Reloaded MUST NOT advertise or accept an IBC-routed swap.

> **As-built note (2026-11-08, clean side, PR #299).** With the feature
> `ibc-routing-for-swaps` on, its only hook is the order-time
> minimum-balance check, which neither advertises a route nor accepts a
> request. The coin and token advertise empty protocol information, and
> their acceptance check now also declines a counterparty's non-empty
> protocol information (where a routing channel would travel), with or
> without the feature.

### 18.11.8 Watchers

- **R18.11.26.** The Tendermint coin and token do not override the
  watcher-support hook, so the family answers *not supported*; every
  watcher branch of the taker machine (Chapter 09, Chapter 51) is skipped
  when either side of a swap is a Tendermint coin. This is an opt-out at
  both designations and in reloaded; §18.3.1's trait-shape remark that
  the family's watcher trait is empty remains accurate.

### 18.11.9 Designations and the baseline anchor

- **R18.11.27.** *Designation agreement.* `v2.6.0-beta` and `v3.0.0-beta`
  agree on every rule of this section. The reference's Tendermint module
  differs between the two only in: a saturating subtraction replacing an
  equivalent checked-subtraction-with-default when computing broadcast
  deadlines; the secret-extraction trait method losing a watcher-reward
  parameter the family never used; and test fixtures. The `dev (as of
  2026-03-27)` state is identical to `v3.0.0-beta` for the module.
- **R18.11.28.** *Burn flag.* At both designations the family reports
  that the dex fee should not be burned (Chapter 16 table), with a code
  comment deferring the burn to a future negotiation version; a reference
  Tendermint taker therefore always sends the standard fee form while
  still being able to validate the burn form it receives.
- *Baseline anchor.* The baseline tree has no Tendermint support (§18.9);
  every rule here is post-anchor behavior distilled through the two-team
  procedure.

> **Code-quality finding (informative; trigger: apparent with little
> extra effort).** The reference's broadcast helper shortens the deadline
> it is given by five seconds with an unchecked duration subtraction. A
> claim attempted when fewer than five seconds remain before the lock, or
> after it (the machine passes the remaining lock time, saturating at
> zero), or a fee send with an already-passed deadline, overflows that
> subtraction and aborts the swap task. Proposed fix: saturate the
> subtraction (or treat a deadline under five seconds as expired).

### 18.11.10 Our side (informative, from this repository's tree)

Checked against the present working tree on 2026-11-08: the HTLC id
derivation, the create message shape (timestamp 0, block lock clamped
to [50, 34560]), the spend search by module state with an empty
placeholder for `Refunded`, the 5-second claim poll, the confirmation
wait by transaction status, zero required confirmations and the dialect
selection by prefix already match R18.11.1–R18.11.4, R18.11.12–R18.11.14,
R18.11.16, R18.11.18–R18.11.19 and R18.11.23. Differences to close before
swaps return:

- the lock duration passed to the create message is derived from the
  absolute locktime minus the current time (so the maker payment gets the
  multiplied duration — safer, but not equal to a reference
  counterparty's recomputation; D18.11.1);
- payment validation checks hash lock, sender, receiver and amount but
  neither `time_lock`, `timestamp`, node byte-equality nor the `Open`
  state (R18.11.5);
- burn-form fee validation checked only the sender and the first output's
  address, not the amounts, the burn output or the message/output counts
  (R18.11.21): closed (PR #299);
- the my-payment-sent check searched the sender's recent create
  transactions and matched the hash lock only (R18.11.22): closed
  (PR #299);
- the refund methods answer an error and the machine records a
  refund-failed event — the same outcome as the reference, R18.11.15,
  which R18.11.17 replaces;
- the broadcast helper ignores the deadline it is given (no five-second
  overflow, but also no bounded wait);
- tokens' trade-fee labels followed the reference (finding in §18.11.2):
  closed (PR #299).

### 18.11.11 Verification notes

- The lock interval [50, 34560] blocks, the `expiration_height`
  semantics, the id derivation and the hash-lock-with-timestamp rule are
  public irismod `x/htlc` module facts; verify them against the module's
  published source and its `HTLC` proto (`expiration_height`,
  `closed_block`, `state`), not against the reference.
- `v2.6.0-beta` versus `v3.0.0-beta` agreement was established by a
  read-only diff of the reference's Tendermint module between the two
  tags (both tabled in chapter 02 §2.15A); no untabled state was read.
- Live checks for the lab: a 50-block HTLC on the Iris testnet
  (`nyancat-9`) refunds in about four minutes; the module returns state
  `Refunded` with no transaction, which is what R18.11.16/R18.11.17 rely
  on.
