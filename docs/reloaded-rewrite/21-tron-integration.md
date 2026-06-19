# Chapter 21 -- Tron Integration

**Status:** driving-spec

> **One-sentence claim:** the project shall provide Tron
> coin-support (native TRX and TRC20 tokens) realised within the
> workspace's EVM coin family, activating wallet-only over a
> caller-supplied set of Tron full-node HTTP endpoints, deriving
> Tron addresses from secp256k1 keys, and building, signing, and
> broadcasting Tron value transfers and TRC20 contract calls --
> conforming on the wire to the public Tron protobuf
> transaction/contract formats, the base58check / `0x41` address
> encoding, the TRC20 ABI, the bandwidth/energy resource fee
> model, and the public Tron node HTTP API; atomic-swap and NFT
> participation are explicitly deferred.

## 21.0 Executive Summary

Tron support is a coin-support capability inside the workspace's
multi-protocol coin crate, realised **within the EVM coin
family** rather than as a free-standing protocol module. Tron and
Ethereum share a key model (secp256k1) and a 20-byte EVM-style
address payload and ABI-encoded token transfers, so the EVM coin
type carries Tron as additional coin-type cases; the
Tron-specific wire concerns (base58check addresses, protobuf
transactions, SHA-256 transaction hashing, the bandwidth/energy
fee model, and a Tron node HTTP client) are bound on top of that
shared base.

The integration is **wallet-only and withdraw-capable today**.
Activation, address handling, transaction construction, signing,
resource-based fee estimation, and the withdraw pipeline for both
native TRX and TRC20 are functional. Atomic-swap participation
(V1 and V2), DEX-fee validation, watcher-based payment
monitoring, and EVM NFT withdraw are **explicitly deferred**: the
coin activates with no swap-contract binding, and every
swap/NFT-adjacent call site refuses Tron coins behind a typed
deferral, kept unreachable in production by the activation gate.

This chapter's subject is the *contract*: the public coin-trait
and activation surface the rest of the workspace calls, the
Tron-protocol interop the module must conform to, the
value-transfer behaviour, and the deferred-work boundary.

> **Binding scope (R36).** Throughout this chapter, requirements
> bind observable *behaviour*, the *public* cross-crate interface
> the rest of the workspace calls (the coin-trait surface and the
> activation RPC surface), and externally *dictated* interop --
> the public Tron protobuf transaction/contract formats, the
> base58check / `0x41` address encoding, the TRC20 ABI, the
> bandwidth/energy resource model, the SLIP-44 coin type, the
> Tron node HTTP API endpoint paths and request JSON shapes, and
> the secp256k1/SHA-256 signing facts (marked R29/R31/R33). The
> private types, struct field layouts, enum variant names, helper
> decomposition, builder call chains, local names, control flow,
> and diagnostic wording an implementation uses to meet these
> requirements are informative, not mandated. Where a fragment is
> reproduced because the Tron wire format, the node API, or the
> ABI dictates it, that is stated explicitly and distinguished
> from the discretionary Rust shape around it.

## 21.1 Baseline Verification

The Tron integration is a **post-2022** feature. It is absent
from the baseline anchor of [Chapter 02](02-baseline-state.md)
(commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`, 3 June 2022):
no Tron coin-support code, no Tron coin-type cases in the EVM coin
type, no `TRX` / `TRC20` coin-protocol cases, and no Tron node
client exist at that revision. The classification here is by
*component role and first-introduction epoch only*; it is derived
from the project's own revision history and does **not**
transcribe code.

| Component (role) | Basis |
| --- | --- |
| Tron coin-support code (addresses, transactions, signing, fee model, node client, withdraw) | Introduced after the baseline anchor. |
| Tron / TRC20 cases in the EVM coin type | Introduced after the baseline anchor. |
| `TRX` / `TRC20` coin-protocol cases in the activation config | Introduced after the baseline anchor. |

Per §1.3, the entire Tron integration is in clean-room
remediation scope (it is **not** baseline-carryforward). This
chapter is therefore a clean-room driving-spec: it states the
integration's contract by behaviour and public/dictated interface
only, and does not reproduce the authorial expression of the
post-2022 code.

## 21.2 Subsystem Placement

Tron support is bound **inside the EVM coin family** of the
multi-protocol coin crate. The placement is a binding
architectural fact; the file layout, the module split, and the
private type and field names that realise it are discretionary
(R36):

- The workspace's **EVM coin type** carries Tron as additional
  coin-type cases (one for native TRX, one for TRC20 tokens),
  alongside its Ethereum and ERC20 cases. Reusing the EVM coin
  type is deliberate: Tron and Ethereum share secp256k1 keys, a
  20-byte address payload, and ABI-encoded token transfers.
- The coin carries an **optional Tron node-API client**,
  populated only for the Tron coin-type cases and absent for
  Ethereum/ERC20.
- The Tron-specific wire concerns -- address encoding,
  protobuf transactions, SHA-256 hashing, the resource fee
  model, and the node HTTP client -- are bound as Tron-only
  behaviour reached through coin-type dispatch.

**R-P1 (dispatch).** Every coin-generic operation in the EVM
family MUST route Tron coin-type cases to Tron-specific behaviour
(address display, transaction encoding, hashing, fee model) and
Ethereum cases to Ethereum behaviour, so that the shared coin
type presents the correct chain semantics for each case.

## 21.3 Public Coin and Type-System Surface

**R-T1 (coin-type cases).** The EVM coin type MUST distinguish a
native-TRX case and a TRC20-token case. The TRC20 case MUST carry
the platform ticker and the token's contract address, mirroring
the ERC20 case so that token-generic code can treat TRC20 like
any other smart-contract token. Native TRX MUST be fixed at the
dictated 6-decimal precision (§21.5). The case *names* and field
layout are discretionary; the distinction is binding.

**R-T2 (coin-protocol cases).** The activation-config protocol
enumeration MUST carry a `TRX` case (selecting a Tron network) and
a `TRC20` case (carrying the platform and the token contract
address). The `TRX` / `TRC20` protocol *type strings* and the
field names inside the protocol data are part of the public coin
configuration JSON contract (§21.4) and are dictated to that
extent; the Rust enum that deserialises them is discretionary.

**R-T3 (network selector).** The `TRX` protocol case MUST carry a
Tron network selector with the public Tron networks -- mainnet
and the public testnets (Shasta, Nile). The selector fixes the
chain identity used at activation. The network *names* are
dictated by Tron; their Rust representation is discretionary.

**R-T4 (coin-trait surface).** The Tron coin MUST present through
the workspace's standard coin-trait surface (the general coin
trait and the market/chain-operations trait) so that the RPC
layer, balance reporting, and withdraw dispatch interact with
Tron through the same abstractions used for every other coin.
These traits are the *public* cross-crate contract; their method
semantics are bound by the coin-trait chapters, not redefined
here. The swap-operations surface is **deferred** for Tron
(§21.11).

## 21.4 Activation (Wallet-Only)

Tron activates through the **shared EVM activation RPC surface**;
it does not introduce a Tron-specific activation namespace. The
public method strings are part of the external API contract
(R33):

| Method string | Role |
| --- | --- |
| `enable_eth_with_tokens` | One-shot activation of the platform coin (TRX) together with its TRC20 tokens |
| `task::enable_eth::init` | Begin task-based activation (long-running) |
| `task::enable_eth::status` | Poll activation status |

> **Binding scope (R36).** The method strings above are dictated
> public API (R33). The Tron coin is activated by supplying
> `"ticker": "TRX"`, a `nodes` array whose entries each carry a
> node `url`, and an `erc20_tokens_requests` array naming the
> TRC20 tokens to enable alongside it -- the same request shape
> the EVM family uses. The coin's static configuration selects
> the Tron protocol via a `protocol` object of dictated shape:
>
> ```jsonc
> // Native TRX coin config (public deserialization contract).
> "protocol": { "type": "TRX",
>               "protocol_data": { "network": "Nile" } }
>
> // TRC20 token coin config (public deserialization contract).
> "protocol": { "type": "TRC20",
>               "protocol_data": { "platform": "TRX",
>                                  "contract_address": "T..." } }
> ```
>
> These JSON field names and type strings are the public
> activation/config contract a caller sends (R33); the Rust types
> that deserialise them are discretionary.

**R-A1 (activation flow).** Activation MUST:

1. Parse the coin's static configuration (ticker, decimals,
   required confirmations, derivation path, network).
2. Validate the caller-supplied node list: it MUST be non-empty
   and every entry MUST be a syntactically valid URL. **No Tron
   node endpoints are embedded in the project;** the node list is
   entirely caller-supplied.
3. Construct the Tron node-API client over the validated node
   list, with multi-node failover (§21.9).
4. For a TRC20 token, parse the configured contract address in
   either base58check or hex form (§21.5), reject the zero or
   empty address, and require the coin config to declare
   `decimals` (the module does not introspect the token's
   on-chain `decimals()` at activation time).
5. Build the coin with the native-TRX or TRC20 coin-type case and
   the Tron node client attached, leaving the swap-contract
   binding empty (§21.11).

**R-A2 (HD derivation -- dictated coin type).** When activated
under an HD-wallet private-key policy, Tron addresses MUST be
derived under the **SLIP-44 registered Tron coin type 195** (the
derivation path is `m/44'/195'/...`). The coin-type value 195 is
dictated by the public SLIP-44 registry (R29/R33); the derivation
plumbing is discretionary.

## 21.5 Address Encoding (dictated interop)

Tron addresses are dictated by the public Tron protocol. The
module conforms to them; it does not define them (R29/R31/R33):

| Property | Dictated form |
| --- | --- |
| Key scheme | secp256k1 (shared with Ethereum) |
| Address payload | 20-byte EVM-style payload, keccak-256-derived from the secp256k1 public key (same derivation as Ethereum) |
| Wire form | 21 bytes: the constant prefix byte `0x41` followed by the 20-byte payload |
| Display form | base58check of the 21-byte wire form; always begins with the letter `T` and is 34 characters long |
| ABI recipient | the bare 20-byte payload (the `0x41` prefix is **not** included inside a TRC20 `transfer(address,uint256)` argument) |

**R-AD1 (dual parse).** Address parsing MUST accept both the
base58check display form and the hex wire form, and MUST validate
the `0x41` prefix and the base58check checksum, rejecting
malformed inputs with a typed error. TRC20 contract addresses
supplied at activation MUST be accepted in either form.

> **Binding scope (R36).** The `0x41` prefix, the 21-byte wire
> form, the base58check display encoding, and the keccak-256
> address derivation are dictated by the Tron protocol. The
> internal address type, its storage choice, and its accessor
> names are discretionary.

## 21.6 Transaction and Contract Format (dictated interop)

Tron transactions are protobuf-encoded per the public Tron
protocol. The module conforms to the published schema; the
following are the interop facts it must satisfy (R29/R31):

- A transaction carries a **raw body** (the to-be-signed payload)
  plus a list of signatures.
- The raw body carries the **anti-replay TAPOS fields** (a
  reference-block-bytes field and a reference-block-hash field),
  an expiry/timestamp pair, a smart-contract execution
  **fee-limit** denominated in SUN, and a contract list that in
  practice carries exactly one contract entry.
- TAPOS fields are derived from a recent block obtained from the
  node API (§21.9): the reference-block-bytes field is taken from
  the low bytes of the block number and the reference-block-hash
  field from a slice of the block id, per the public Tron TAPOS
  rules.

**R-TX1 (contract types).** The module MUST emit the dictated
Tron contract types for the operations it supports:

| Tron contract type | Numeric id | Module use |
| --- | --- | --- |
| `TransferContract` | 1 | Native TRX value transfer |
| `TriggerSmartContract` | 31 | TRC20 transfer (and contract calls generally) |

**R-TX2 (TRC20 ABI -- dictated).** A TRC20 transfer MUST be
encoded as a `TriggerSmartContract` call whose data is the ABI
encoding of the standard TRC20 method `transfer(address,uint256)`
-- the 4-byte selector of that signature followed by the 20-byte
recipient payload (§21.5) and the amount. The `transfer`
signature and its ABI encoding are dictated by the TRC20 token
standard (R29/R31); the Rust builder that assembles them is
discretionary.

> **Binding scope (R36).** The protobuf transaction/contract
> schema, the TAPOS construction, the contract-type ids, the
> SUN-denominated fee-limit, and the TRC20 `transfer` ABI are
> dictated by the Tron protocol and the TRC20 standard. The Rust
> message types, builder function names and signatures, and local
> variables that realise them are discretionary and are not
> reproduced here.

## 21.7 Signing (dictated interop)

Tron signing is dictated by the public protocol (R29/R31):

- **Transaction hash:** SHA-256 of the protobuf-encoded raw body.
  This is **SHA-256, not keccak-256** -- a Tron-specific
  divergence from Ethereum signing that the module must honour.
- **Signature:** a secp256k1 signature packed as 65 bytes in
  `r (32) || s (32) || v (1)` order, with the recovery byte `v`
  in `{0, 1}`. The signature is appended to the transaction's
  signature list, and the fully-assembled transaction is
  hex-encoded for broadcast.

> **Binding scope (R36).** The SHA-256 hashing, the secp256k1
> scheme, and the 65-byte signature layout are dictated by the
> Tron protocol. The private signing helper and its internal
> shape are discretionary and are not reproduced here.

## 21.8 Resource (Bandwidth/Energy) Fee Model (dictated interop)

Tron's fee model is its **bandwidth + energy resource model**,
dictated by the protocol and the node API (R29/R31/R33). The
module estimates fees against it rather than against an
Ethereum-style gas price:

**R-F1 (estimation inputs).** Fee estimation MUST account for:

- the account's **free** bandwidth quota and any **staked**
  bandwidth/energy quota (from the account-resource endpoint,
  §21.9);
- the **per-unit prices** for bandwidth, energy, and new-account
  activation (from the chain-parameters endpoint, §21.9);
- the **energy cost** of a TRC20 contract call, estimated via the
  constant-contract endpoint (§21.9);
- the **new-account activation fee** charged when the destination
  account does not yet exist on chain.

**R-F2 (fee reporting).** The withdraw result MUST report the fee
broken into its bandwidth and energy components, surfaced through
a dedicated Tron fee-details variant of the coin crate's
transaction-fee detail type.

> **Binding scope (R36).** The resource model (free vs. staked
> bandwidth/energy, per-unit prices, new-account activation fee)
> and the endpoints that expose its parameters are dictated by
> Tron. The internal fee-computation helpers, intermediate
> variables, and quota arithmetic are discretionary and are not
> reproduced here.

## 21.9 Tron Node HTTP API Surface (dictated interop)

All chain interaction goes through the Tron node-API client over
the caller-supplied node list. The endpoint paths below are the
**public Tron full-node HTTP API** (dictated interop, R29/R33);
the module consumes them, and the base URLs come entirely from
the activation request -- no URL is embedded in the module:

| Endpoint | Module use |
| --- | --- |
| `POST /wallet/getnowblock` | Current block: TAPOS data and chain head |
| `POST /wallet/getaccount` | Account balance and existence |
| `POST /wallet/getaccountresource` | Free / staked bandwidth and energy |
| `POST /wallet/getchainparameters` | Per-unit prices (bandwidth, energy, account activation) |
| `POST /wallet/triggerconstantcontract` | Energy estimation for TRC20 calls; TRC20 `balanceOf` reads |
| `POST /wallet/broadcasthex` | Submit a hex-encoded signed transaction |
| `POST /wallet/gettransactioninfobyid` | Post-broadcast receipt and execution result |

**R-N1 (multi-node failover).** The client MUST tolerate
unhealthy nodes: a request issued to one node MUST fail over to
the remaining nodes on a retryable error, and a node that
succeeds MUST be preferred for subsequent traffic so steady-state
requests stay on a healthy endpoint. The failover ordering policy
is binding behaviour; the locking/storage realisation is
discretionary.

**R-N2 (request timeout).** Each node request MUST be bounded by a
per-request timeout so a stalled node cannot block the pipeline.
The concrete timeout value is discretionary.

> **Binding scope (R36).** The endpoint paths and their JSON
> request/response shapes are the dictated public Tron node API.
> The client struct, its node-storage representation, and the
> request helper internals are discretionary and are not
> reproduced here.

## 21.10 Withdraw (Value Transfer) Behaviour

The module provides a withdraw path producing a signed,
broadcastable Tron transaction for native TRX or for a TRC20
transfer. The behavioural contract:

**R-W1.** Resolve TAPOS from the current-block endpoint (§21.9).

**R-W2.** Parse the destination address (and, for TRC20, the
token contract address) per §21.5, and reject a non-positive
amount.

**R-W3.** Estimate the bandwidth + energy fee per §21.8, using the
account-resource, chain-parameters, and (for TRC20)
constant-contract endpoints.

**R-W4.** Build the dictated contract -- a native `TransferContract`
or a TRC20 `TriggerSmartContract` carrying the ABI-encoded
`transfer(address,uint256)` call (§21.6) -- with the resolved
TAPOS and fee-limit.

**R-W5.** Hash the raw body with SHA-256, sign it with secp256k1,
assemble the full transaction, hex-encode it, and broadcast it via
the broadcast endpoint (§21.9).

**R-W6.** Return transaction details denominated in whole TRX
(converted from SUN at `1 TRX = 10^6 SUN`), with the fee broken
into bandwidth and energy components (R-F2).

> **Gasless TRC20 (fee delegation).** A separate, optional
> "GasFree" fee-delegation rail lets a user move a TRC20 token
> without holding TRX, by signing an off-chain TIP-712 transfer
> authorization whose fee a third-party provider pays in the
> token. It is specified in
> [Chapter 49](49-tron-gasfree-fee-delegation.md); the native
> resource-fee withdraw of this section is unaffected.

## 21.11 Deferred Work

The following are explicit gaps, documented as deferred work
rather than defects. None is a correctness claim. The Tron coin
activates with **no swap-contract binding**, and the deferral is
made explicit at each relevant call site as a typed refusal,
which the activation gate keeps unreachable in production. This
follows the same defence-in-depth pattern documented in
[Chapter 04](04-error-aggregation-type-adaptation.md): the
deferral is visible at every call site rather than hidden behind
a single flag.

- **D1 -- V1 atomic swaps.** No HTLC contract is bound for Tron;
  the V1 swap-operations surface is not implemented and Tron
  coins are refused by the swap pipeline.
- **D2 -- V2 atomic swaps.** Likewise unbound; the V2
  maker/taker funding, validation, approval, and spend paths
  refuse Tron coins.
- **D3 -- DEX-fee validation.** Tron DEX-fee validation is not
  wired and is refused with a typed error.
- **D4 -- Watcher-based monitoring.** The watcher-eligibility and
  watcher-resolution paths are not wired for Tron; activation
  prevents the guarded site from being reached.
- **D5 -- EVM NFT withdraw.** NFT withdraw is explicitly rejected
  for Tron-family chains with a typed "not supported for this
  chain" error (see [Chapter 19](19-nft-module-layout.md)).

**R-D1 (gate invariant).** Because the coin activates with no
swap-contract binding, no production code path may reach a
Tron swap/NFT operation; the deferral sites exist to make the
boundary explicit and MUST surface a typed refusal (or a guarded
unreachable) rather than producing incorrect swap behaviour.

## 21.12 Baseline Verifications

The following are verifiable from the baseline state defined in
[Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree contains **no** Tron coin-support code. A
    tree-wide `git grep -li 'tron'` against the baseline returns
    no coin-support matches, confirming the feature is post-2022.

V2. The baseline EVM coin type carries no Tron / TRC20 cases and
    the baseline activation config carries no `TRX` / `TRC20`
    coin-protocol cases; these are post-2022 additions to
    existing files.

V3. The address encoding, protobuf transaction/contract formats,
    SHA-256 hashing, secp256k1 signature layout, the TRC20 ABI,
    the bandwidth/energy resource model, the SLIP-44 coin type
    195, and the node HTTP endpoint paths of §21.5--§21.9 are
    properties of the public Tron protocol, the TRC20 standard,
    the SLIP-44 registry, and the public Tron node API -- not
    artefacts of any project lineage.

## 21.13 Provenance Footer

- *Inputs:* the project's own revision history (used for the
  epoch classification of §21.1 and the baseline verifications of
  §21.12, by component role and first-introduction epoch only --
  no code transcribed); the baseline anchor of
  [Chapter 02](02-baseline-state.md); the public Tron network
  protocol (the protobuf transaction/contract formats, the TAPOS
  anti-replay construction, the `0x41` / base58check address
  encoding, the keccak-256 address derivation, the SHA-256
  transaction hashing, the secp256k1 65-byte signature layout,
  the SUN unit and the `1 TRX = 10^6 SUN` ratio, and the
  bandwidth/energy resource fee model); the public TRC20 token
  standard (the `transfer(address,uint256)` ABI); the public
  SLIP-44 registry (Tron coin type 195); the public Tron
  full-node HTTP API (the endpoint paths of §21.9); the shared
  EVM activation RPC surface (the public method strings of
  §21.4); and cross-chapter contracts (Chapters 04, 19).
- *Permitted-input classes used:* baseline source (epoch
  classification and absence verification only); external public
  specifications (the Tron protocol and protobuf
  transaction/contract formats, the TRC20 ABI, SLIP-44, the Tron
  node HTTP API); cross-chapter contracts (Chapters 04, 19);
  Interop / wire-and-API-bound reuse (R29/R31/R33) for the
  dictated fragments embedded in §21.4 (the `enable_eth_with_tokens`
  / `task::enable_eth::*` method strings and the `TRX` / `TRC20`
  coin-config JSON shape), §21.5 (the address encoding), §21.6
  (the transaction/contract format and the TRC20 ABI), §21.7 (the
  signing facts), §21.8 (the resource fee model), and §21.9 (the
  node endpoint paths) -- whose authoritative source is the bytes
  and calls any conforming Tron node, TRC20 token, or activation
  caller must exchange for interoperability, not the historical
  lineage's discretionary expression.
- *Sibling-allowlist consultations:* Chapter 04 (the
  error-aggregation / defence-in-depth deferral pattern the
  swap/NFT refusals follow); Chapter 19 (the NFT-withdraw module
  whose chain-support boundary excludes Tron-family chains).
- *Forbidden corpus:* consulted **only** to recover the
  externally-dictated activation method strings and coin-config
  JSON shape of §21.4, the SLIP-44 coin type of §21.4/§21.12, and
  the dictated address/transaction/signing/fee/node-API interop
  of §21.5--§21.9 -- embedded as Interop reuse under R29/R31/R33.
  No discretionary expression from the post-2022 code -- no struct
  field lists, internal type or enum definitions, private method
  names or signatures or bodies, transaction-builder/signing call
  chains, local variable names, per-method or per-field tables
  keyed to internal names, the module file tree, internal test
  function names, control-flow transcription, or diagnostic /
  panic / log string literals -- crosses into this chapter. All
  other content is clean-room driving-spec stated by behaviour and
  public/dictated interface. Any residual similarity of a
  conformant realisation to the historical lineage is governed by
  the R35 gate and the R36 binding-scope notes that head every
  code-bearing section of this chapter.
