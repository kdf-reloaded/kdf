# Chapter 21 -- Tron Integration

**Status:** driving-spec

> **One-sentence claim:** the project shall provide Tron
> coin-support (native TRX and TRC20 tokens) realised within the
> workspace's EVM coin family, activating over a caller-supplied
> set of Tron full-node HTTP endpoints, deriving Tron addresses
> from secp256k1 keys, building, signing, and broadcasting Tron
> value transfers and TRC20 contract calls, and **participating
> as a first-class counterparty in version-1 atomic swaps** --
> locking, validating, spending, and refunding hash-time-locked
> payments through a Tron-deployed instance of the project's
> version-1 swap (HTLC) contract that uses a SHA-256 secret-hash,
> sending and validating the DEX taker-fee, and discovering
> counterparty spends/refunds through Tron's indexed
> contract-event and transaction-receipt interfaces -- conforming
> on the wire to the public Tron protobuf transaction/contract
> formats, the base58check / `0x41` address encoding, the TRC20
> ABI, the published swap-contract ABI, the bandwidth/energy
> resource fee model, and the public Tron node HTTP API;
> version-2 swaps, watcher-reward swap paths, and NFT
> participation remain out of scope.

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

The integration is **wallet-capable and version-1 swap-capable**.
Activation, address handling, transaction construction, signing,
resource-based fee estimation, and the withdraw pipeline for both
native TRX and TRC20 are functional; on top of that base, a Tron
coin that activates with a swap-contract address participates as
a first-class counterparty in **version-1 atomic swaps** -- maker
and taker hash-time-locked payments, payment validation, spend
and refund, secret extraction, spend/refund discovery, and the
DEX taker-fee -- for both native TRX and TRC20. **Version-2
atomic swaps, the watcher-reward swap variants, and EVM NFT
withdraw remain out of scope**: a Tron coin activated without a
swap-contract address stays wallet-only, and every out-of-scope
call site refuses Tron coins behind a typed boundary, kept
unreachable for an activated coin by the activation gate.

This chapter's subject is the *contract*: the public coin-trait
and activation surface the rest of the workspace calls, the
Tron-protocol and swap-contract interop the module must conform
to, the value-transfer and version-1 atomic-swap behaviour, and
the remaining scope boundary.

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
| Tron version-1 swap participation (HTLC payment/spend/refund, payment validation, DEX taker-fee, event discovery) and the Tron swap-contract address in coin configuration | Introduced after the baseline anchor. |

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

**R-P2 (swap dispatch).** Version-1 atomic-swap operations for
Tron coins MUST be reached through the same EVM-family
swap-operations façade the rest of the workspace calls, dispatched
by chain family to Tron-specific swap behaviour, and Ethereum
coins MUST continue to route to Ethereum swap behaviour. Tron
swap behaviour is bound as Tron-only logic within the EVM coin
family's Tron submodule; the workspace's public maker/taker swap
state drivers MUST NOT carry Tron-specific control flow beyond
selecting the secret-hash algorithm (§21.11). The placement (Tron
swap logic inside the EVM family's Tron submodule, reached through
chain-family dispatch from the shared façade) is a binding
architectural fact; the module split and the private names are
discretionary (R36).

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
here. The **version-1** swap-operations surface is **bound** for
Tron (§21.11--§21.16); version-2 swap operations remain out of
scope (§21.18).

**R-T5 (transaction-enum case).** The coin crate's transaction
enumeration -- the opaque signed-transaction type swap flows store
and pass between steps -- MUST carry a Tron-transaction case
wrapping a signed Tron protobuf transaction together with its
transaction hash and its broadcast-ready hex form, so that swap
drivers hold a Tron payment/spend/refund transaction through the
same enumeration used for every other chain family. The case MUST
present the standard transaction abstraction (its hex form and its
hash); the case *name* and field layout are discretionary.

## 21.4 Activation

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
   the Tron node client attached, binding the swap-contract
   address from configuration when present (R-A3).

**R-A3 (swap-contract binding -- config).** A Tron coin intended
for atomic-swap participation MUST carry a swap-contract address
identifying the Tron-deployed version-1 HTLC contract (§21.12).
The public coin-configuration / activation field
`swap_contract_address` is the dictated config contract carrying
it (R33), reusing the EVM family's existing optional
swap-contract-address parameter and adding no Tron-specific
activation field. It is supplied in base58check display form
(`T...`) and MUST be converted at activation to the internal
20-byte address payload (§21.5). A coin activated without a
swap-contract address remains wallet-only and is refused by the
swap pipeline (§21.18, R-D1).

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
| `POST /wallet/triggerconstantcontract` | Energy estimation for contract calls; TRC20 `balanceOf` reads; swap-contract state reads (`payments(id)`) |
| `POST /wallet/broadcasthex` | Submit a hex-encoded signed transaction |
| `POST /wallet/gettransactioninfobyid` | Post-broadcast receipt and execution result; receipt-log confirmation of swap events; confirmation tracking |
| Indexed contract-event query (public Tron full-node / event-indexer HTTP API) | Swap spend/refund discovery: locating swap-contract events by contract address and swap id (§21.13) |

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

## 21.11 Atomic-Swap Participation (Version 1)

Tron participates as a first-class counterparty in the
workspace's **version-1** atomic-swap protocol, on both the maker
and taker sides, for native TRX and TRC20 tokens. Participation
is realised by locking funds in a Tron-deployed instance of the
project's version-1 hash-time-locked-contract (HTLC) and driving
its lifecycle through chain-family dispatch (R-P2). Version-2
swaps, the watcher-reward swap variants, and NFT swaps are out of
scope (§21.18).

**R-S1 (chain-family dispatch).** Every version-1 swap operation
the EVM-family swap fa\u00e7ade exposes -- maker/taker payment send,
payment validation, payment spend, payment refund, secret
extraction, spend/refund discovery, taker-fee send, taker-fee
validation, and the trade-fee estimators -- MUST, for a Tron coin,
route to Tron-specific behaviour selected by chain family, and
MUST route Ethereum coins to Ethereum behaviour. The public
swap-driver framework (the maker/taker swap state drivers) MUST
remain free of Tron-specific control flow other than secret-hash
algorithm selection (R-S2).

**R-S2 (secret-hash algorithm -- dictated).** For Tron coins the
swap framework MUST select **SHA-256** as the payment secret-hash
algorithm: the on-chain payment hash is `SHA-256(secret)`,
compared as a 32-byte value. This differs from the workspace's
EVM version-1 secret-hash (RIPEMD-160 of SHA-256, a 20-byte
value). The SHA-256 choice is dictated by the Tron-deployed swap
contract (§21.12) and by the established Tron HTLC convention:
SHA-256 is the native Tron VM hashing path and is the algorithm
every interoperating Tron HTLC counterparty expects.

> **Binding scope (R36).** That Tron coins use a SHA-256 32-byte
> secret-hash is a dictated interop fact fixed by the deployed
> contract and the counterparty protocol. The site at which the
> algorithm is selected, and the private dispatch that routes Tron
> swap calls, are discretionary and are not reproduced here.

## 21.12 Swap-Contract ABI and Swap Identifier (dictated interop)

Tron version-1 swaps run against a Tron-deployed instance of the
project's version-1 HTLC contract, compiled for the Tron VM and
deployed at an address carried in coin configuration (R-A3). The
method set, event set, stored payment state, and swap-identifier
derivation below are the contract's **published ABI and protocol**
-- the interop surface any counterparty interacting with the
deployed contract must conform to (R29/R31/R33). They are
reproduced because the ABI dictates them; the Rust types,
encoders, and decoders that realise them are discretionary (R36).

**R-SA1 (contract ABI -- dictated).** The Tron version-1 swap
contract MUST present the ABI surface below. The Tron variant uses
a 32-byte (`bytes32`) secret-hash and payment hash (SHA-256), in
contrast to the EVM variant's 20-byte forms:

| ABI member (dictated signature) | Role |
| --- | --- |
| `ethPayment(bytes32 id, address receiver, bytes32 secretHash, uint64 lockTime)` *(payable)* | Lock a native-TRX HTLC payment |
| `erc20Payment(bytes32 id, uint256 amount, address tokenAddress, address receiver, bytes32 secretHash, uint64 lockTime)` | Lock a TRC20 HTLC payment |
| `receiverSpend(bytes32 id, uint256 amount, bytes32 secret, address tokenAddress, address sender)` | Claim a payment by revealing the secret |
| `senderRefund(bytes32 id, uint256 amount, bytes32 secretHash, address tokenAddress, address receiver)` | Refund a payment after its lock-time |
| `payments(bytes32 id)` → `(bytes32 paymentHash, uint64 lockTime, uint8 state)` | Read stored payment state |
| event `PaymentSent(bytes32 id)` | Emitted on payment lock |
| event `ReceiverSpent(bytes32 id, bytes32 secret)` | Emitted on spend; carries the revealed secret |
| event `SenderRefunded(bytes32 id)` | Emitted on refund |

The stored payment `state` is the dictated lifecycle enumeration
-- uninitialised, payment-sent, receiver-spent, sender-refunded --
exposed as a small unsigned integer through `payments(id)`. The
address arguments are the bare 20-byte EVM-style payloads (§21.5),
not the 21-byte `0x41`-prefixed wire form.

**R-SA2 (swap identifier -- dictated).** The 32-byte swap `id`
used as the contract's payment-mapping key MUST be derived as
`SHA-256( little-endian uint32(lockTime) ‖ secretHash )`. This
derivation is the version-1 swap-payment identification scheme
both counterparties and the contract compute identically; it is
dictated interop, not discretionary.

> **Binding scope (R36).** The method/event signatures, the
> `payments(id)` return tuple, the payment-state enumeration, the
> 32-byte SHA-256 secret-hash, and the swap-id derivation are
> dictated by the deployed swap contract and the version-1 swap
> protocol. The contract source, the Rust ABI-binding types, the
> selector constants, and the encode/decode helpers that realise
> them are discretionary and are not reproduced here.

## 21.13 Swap Lifecycle Behaviour

The Tron swap path implements the version-1 swap-operations
contract by building, signing (§21.7), and broadcasting (§21.9)
Tron `TriggerSmartContract` calls against the deployed swap
contract, and by reading contract state and events back through
the node API. Each operation's behavioural contract:

**R-L1 (payment send).** A maker/taker payment MUST lock funds in
the swap contract under the swap id (R-SA2). For native TRX it
MUST be a `TriggerSmartContract` call to `ethPayment` carrying the
locked amount as the call value (TRX is sent into the payable
method, not transferred separately). For a TRC20 token it MUST
first ensure sufficient allowance for the swap contract (§21.14)
and then call `erc20Payment`. The resulting signed Tron
transaction is returned through the transaction enumeration's Tron
case (R-T5).

**R-L2 (payment validation).** Validation of a counterparty's
payment MUST decode the broadcast payment transaction from its
protobuf form, extract the swap-contract call data, decode it
against the dictated `ethPayment` / `erc20Payment` ABI (R-SA1),
and cross-check the funder, the recipient, the token (for TRC20),
the amount, the secret-hash, and the lock-time against the
negotiated swap terms; it MUST additionally read on-chain
`payments(id)` state (§21.9) to confirm the payment is recorded. A
mismatch MUST yield a typed validation failure.

**R-L3 (spend).** Spending a payment MUST call `receiverSpend`
with the revealed secret and the original payment parameters,
after decoding the original payment transaction to reconstruct the
call arguments and checking on-chain payment state to avoid a
redundant or invalid spend.

**R-L4 (refund).** Refunding a payment MUST call `senderRefund`
with the original payment parameters once the lock-time has
elapsed, again reconstructing arguments from the decoded original
payment and checking on-chain state before sending.

**R-L5 (secret extraction).** Extracting the secret from a
counterparty's spend MUST decode the spend transaction's protobuf
form, confirm the call is a `receiverSpend` invocation, and
ABI-decode the revealed secret from its arguments. The secret is
also observable from the `ReceiverSpent` event (R-SA1).

**R-L6 (spend/refund discovery).** Discovery of whether a payment
was spent or refunded MUST locate the relevant swap-contract event
(`ReceiverSpent` for a spend, `SenderRefunded` for a refund) for
the swap id, using the Tron **indexed contract-event interface**
for discovery and **transaction-receipt logs** for confirmation of
a known transaction (§21.9). Tron does not expose Ethereum-style
log filtering, so discovery is event-indexer-based rather than
filter-based. On finding the event the corresponding Tron
transaction MUST be fetched and surfaced as the spend or refund
outcome. If no event-capable endpoint is configured, discovery
MUST fail with a typed error rather than silently reporting "not
found".

**R-L7 (confirmation tracking).** Confirmation waiting MUST
identify the transaction by the hash derived from its protobuf
form (not by an Ethereum-style RLP hash) and poll the Tron
transaction-receipt endpoint (§21.9) until the configured
confirmation depth is reached.

> **Binding scope (R36).** The lifecycle behaviour above binds the
> observable sequence and the dictated ABI / protobuf / node-API
> surface each step exchanges. The private swap helpers, their
> decomposition, the local reconstruction of call arguments, the
> polling cadence, and any diagnostic wording are discretionary
> and are not reproduced here.

## 21.14 TRC20 Approval and Non-Standard-Token Semantics (dictated interop)

A TRC20 HTLC payment requires the swap contract to be approved to
move the token on the funder's behalf. The approval behaviour must
accommodate non-standard tokens -- notably the mainnet Tron USDT,
whose `transfer` / `transferFrom` / `approve` methods do not return
the boolean the standard TRC20 ABI declares:

**R-AP1 (allowance management -- dictated quirk).** Before a TRC20
`erc20Payment`, the Tron swap path MUST ensure the swap contract's
allowance over the funded token is sufficient. For tokens that
reject a direct non-zero-to-non-zero allowance change (the
USDT-style approval restriction), it MUST first set the allowance
to zero and then set the required allowance ("approve-to-zero
first"). The swap contract itself MUST use a safe-transfer wrapper
tolerant of TRC20 tokens that return no boolean, so that such
tokens can be locked and released. The zero-first sequence and the
non-boolean tolerance are dictated by the deployed token
contracts' published behaviour, not discretionary.

> **Binding scope (R36).** The approve-to-zero-first requirement
> and the non-boolean-return tolerance are dictated by the token
> contracts the swap must inter-operate with. The internal
> allowance-check and approval-build helpers are discretionary and
> are not reproduced here.

## 21.15 DEX-Fee (Taker-Fee) Send and Validation

Tron coins participate in the DEX taker-fee step of the swap
protocol:

**R-DF1 (taker-fee send).** The taker-fee path MUST send the
negotiated fee amount to the protocol's dex-fee recipient address:
a native-TRX transfer for TRX, or a TRC20 token transfer for a
TRC20 coin (§21.6 R-TX1/R-TX2), producing a signed Tron
transaction surfaced through the transaction enumeration's Tron
case (R-T5).

**R-DF2 (taker-fee validation).** Validating a counterparty's
taker-fee payment MUST decode the broadcast transaction from its
protobuf form, confirm it is the expected transfer (native or
TRC20) to the dex-fee recipient, and cross-check the recipient and
the amount against the expected fee, yielding a typed failure on
mismatch.

> **Binding scope (R36).** The dex-fee send/validate behaviour and
> the transfer encodings it relies on are bound; the private fee
> helpers are discretionary and are not reproduced here.

## 21.16 Trade-Fee Estimation

**R-TF1 (trade-fee estimators).** The Tron coin MUST answer the
workspace's trade-fee estimation surface (the maker-side and
taker-side trade-fee queries and the fee-to-send-taker-fee query)
so that both the legacy trade-preimage RPC and the version-2 swap
state machine obtain Tron swap-cost estimates through the same coin
methods used for every other coin. Estimates MUST be computed
against Tron's bandwidth/energy resource model (§21.8): the energy
cost of the relevant swap-contract call is estimated via the
constant-contract (dry-run) endpoint (§21.9) and combined with the
bandwidth cost and the per-unit prices, reported with the
bandwidth/energy breakdown of §21.8 R-F2.

> **Binding scope (R36).** The estimator surface and the
> resource-model inputs are bound; the internal estimation
> helpers, any safety margins, and intermediate arithmetic are
> discretionary and are not reproduced here.

## 21.17 Tests

The following tests bind the swap feature's verification surface;
their realisation (fixtures, helper names, network gating) is
discretionary.

**T1.** Offline unit tests cover the dictated interop: ABI
encode/decode round-trips for `ethPayment` / `erc20Payment` /
`receiverSpend` / `senderRefund`, the swap-id derivation (R-SA2),
the SHA-256 secret-hash, address-form conversions (§21.5),
function-selector checks, and protobuf decode of payment and spend
transactions.

**T2.** Event-normalisation tests cover decoding `PaymentSent`,
`ReceiverSpent`, and `SenderRefunded` from both the indexed
contract-event source and transaction-receipt logs (R-L6).

**T3.** Payment-validation tests cover positive validation and the
negative cases (wrong recipient, wrong amount, wrong secret-hash,
wrong lock-time, missing on-chain state) for both TRX and TRC20
(R-L2).

**T4.** A TRC20 regression test covers the approve-to-zero-first
allowance sequence for USDT-style tokens (R-AP1), and a "no event
endpoint configured" test covers the typed discovery failure
(R-L6).

**T5.** Fee-estimation tests cover the trade-fee estimators across
fee-approximation stages for TRX and TRC20 and the trade-preimage
RPC for both (R-TF1).

**T6.** Feature-gated end-to-end swap tests against a public Tron
testnet cover, for both TRX and TRC20: maker payment → spend,
taker payment → spend, payment → refund after lock-time, payment
validation, secret extraction, and spend/refund discovery
(R-L1--R-L7).

## 21.18 Deferred and Unsupported Work

The following are explicit scope boundaries, documented as scope
limits rather than defects. None is a correctness claim. A Tron
coin activated without a swap-contract address (R-A3) remains
wallet-only; each out-of-scope boundary is made explicit at its
call site as a typed refusal, kept unreachable for an activated
swap-capable coin by the activation gate -- the same
defence-in-depth pattern documented in
[Chapter 04](04-error-aggregation-type-adaptation.md), visible at
every call site rather than hidden behind a single flag.

- **D1 -- Version-2 atomic swaps.** Only version-1 Tron swaps are
  bound. The version-2 maker/taker funding, validation, approval,
  and spend paths do not support Tron coins and refuse them with a
  typed error.
- **D2 -- Watcher-based monitoring and watcher-reward swap
  paths.** Tron version-1 swaps do **not** support the
  watcher-reward payment variants; those paths are explicitly
  refused for Tron with a typed error. Watcher-eligibility and
  watcher-resolution monitoring for Tron is not wired.
- **D3 -- EVM NFT withdraw.** NFT withdraw is explicitly rejected
  for Tron-family chains with a typed "not supported for this
  chain" boundary (see [Chapter 19](19-nft-module-layout.md)).

**R-D1 (gate invariant).** A Tron coin activated without a
swap-contract binding MUST NOT reach any swap operation; a
swap-capable Tron coin MUST NOT reach a version-2, watcher-reward,
or NFT operation. Every such site MUST surface a typed refusal (or
a guarded unreachable) rather than producing incorrect behaviour.

## 21.19 Baseline Verifications

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

V4. The baseline tree contains **no** Tron swap-participation
    code: no Tron-deployed swap-contract binding, no Tron branch
    in the version-1 swap-operations surface, and no Tron
    swap-contract address in coin configuration exist at the
    baseline anchor. The swap feature is post-2022.

V5. The swap-contract ABI (the `ethPayment` / `erc20Payment` /
    `receiverSpend` / `senderRefund` methods, the `payments(id)`
    accessor, and the `PaymentSent` / `ReceiverSpent` /
    `SenderRefunded` events), the 32-byte SHA-256 secret-hash, and
    the swap-id derivation of §21.12 are properties of the
    project's published version-1 swap-contract ABI and the
    version-1 swap protocol -- interop surfaces any counterparty
    must conform to -- not artefacts of any project lineage's
    discretionary expression.

## 21.20 External References

- Public Tron network protocol: the protobuf transaction/contract
  formats, the TAPOS anti-replay construction, the `0x41` /
  base58check address encoding, the keccak-256 address
  derivation, the SHA-256 transaction hashing, the secp256k1
  65-byte signature layout, the SUN unit and the
  `1 TRX = 10^6 SUN` ratio, and the bandwidth/energy resource fee
  model.
- Public Tron full-node HTTP API and indexed contract-event
  interface: the endpoint paths of §21.9.
- TRC20 token standard: the `transfer(address,uint256)` /
  `approve(address,uint256)` / `transferFrom(address,address,uint256)`
  ABI, including the non-boolean-returning behaviour of mainnet
  Tron USDT (§21.14).
- Published version-1 swap-contract ABI: the HTLC method/event
  surface, the `payments(id)` accessor, and the swap-id derivation
  of §21.12 (a per-contract project ABI published by its authors).
- SLIP-44 registry: Tron coin type 195.
- Shared EVM activation RPC surface: the public method strings of
  §21.4 and the `swap_contract_address` config field (R-A3).
- Cross-chapter contracts: [Chapter 04](04-error-aggregation-type-adaptation.md)
  (the defence-in-depth boundary pattern) and
  [Chapter 19](19-nft-module-layout.md) (the NFT chain-support
  boundary).

## 21.21 Provenance Footer

- *Inputs:* the project's own revision history (used for the
  epoch classification of §21.1 and the baseline verifications of
  §21.19, by component role and first-introduction epoch only --
  no code transcribed); the baseline anchor of
  [Chapter 02](02-baseline-state.md); the public Tron network
  protocol and public Tron full-node HTTP API (the protobuf
  transaction/contract formats, TAPOS, the `0x41` / base58check
  address encoding, the keccak-256 address derivation, the
  SHA-256 transaction hashing, the secp256k1 65-byte signature
  layout, the SUN unit and the `1 TRX = 10^6 SUN` ratio, the
  bandwidth/energy resource fee model, the node endpoint paths,
  and the indexed contract-event interface); the public TRC20
  token standard (the transfer/approve ABI and the non-standard
  USDT behaviour); the project's published version-1
  swap-contract ABI and version-1 swap protocol (the HTLC
  method/event surface and the swap-id derivation of §21.12); the
  public SLIP-44 registry (Tron coin type 195); the shared EVM
  activation RPC surface (the public method strings and the
  `swap_contract_address` config field of §21.4); and
  cross-chapter contracts (Chapters 04, 19).
- *Permitted-input classes used:* baseline source (epoch
  classification and absence verification only); external public
  specifications and published ABIs (the Tron protocol and
  protobuf transaction/contract formats, the TRC20 ABI, the
  published version-1 swap-contract ABI, SLIP-44, the Tron node
  HTTP API and indexed contract-event interface); wire-format and
  interop inputs the project must inter-operate with (the
  swap-contract call/event encodings and swap-id derivation, the
  token-approval quirks, and the node request/response shapes);
  behavioural observation of the public Tron networks; and
  cross-chapter contracts (Chapters 04, 19). Every dictated
  fragment embedded in §21.4--§21.16 is sourced from these public
  interop surfaces -- the bytes and calls any conforming Tron
  node, TRC20 token, swap-contract counterparty, or activation
  caller must exchange for interoperability -- not from any
  implementation's discretionary expression. No struct field
  lists, internal type or enum definitions, private method names
  or bodies, builder/signing call chains, local variable names,
  internal module file trees, internal test-function names,
  control-flow transcription, or diagnostic / log string literals
  cross into this chapter; all other content is clean-room
  driving-spec stated by behaviour and public/dictated interface.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* not consulted.
