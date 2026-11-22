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

> **Availability in the beta (maintainer decision 2026-10-26).** TRON
> support is not finished, so it is switched off for the coming beta:
> while the build constant `TRON_ACTIVATION_AVAILABLE`
> (`coins/lp_coins_ops.rs`) is `false`, every TRON activation route
> (legacy `enable` and `electrum`, the only routes that activate TRX
> or TRC20, Chapter 55 D55.5) answers "TRON support is not available
> in this build" and registers nothing; the coin code stays compiled
> and its unit tests keep running. TRON is postponed, not dropped: it
> is planned to return in the next beta, and the rules of this
> chapter stay the target for that return. Statements below that
> describe TRON as activatable or functional describe the code with
> the constant set to `true`.

*Status note (2026-11-20), TRON is switched on as a wallet-only coin
family.* The availability note above describes the tree until this
date. `TRON_ACTIVATION_AVAILABLE` is now `true`: every activation route
of §21.4 activates TRX and TRC20 (the mmrpc 2.0 methods, and the legacy
`enable` and `electrum` of Chapter 55 D55.5), and the "TRON support is
not available in this build" refusal no longer exists on any route. The
wallet surface of §21.4 -- §21.10C is reachable: activation, balances,
addresses, the HD wallet, withdraw, and the transaction history with a
TronGrid API key. Swaps stay off: a second build constant,
`TRON_SWAPS_AVAILABLE` (`coins/lp_coins_ops.rs`), is `false`, and while
it is `false` every TRX and TRC20 coin is wallet-only whatever its
coins-config entry says, declines every match, and an order saved
earlier that names one is cancelled at startup (the status notes at
R21.10B.9, D6 and R21.B3). Where a sentence of this chapter says that a
statement holds "when switched on", "behind the availability constant"
or "while the constant is `false`", read it with the constant `true`
from this date.

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

The integration is **wallet-only and withdraw-capable today**
when switched on (in this beta it is switched off; see the
availability note above).
Activation, address handling, transaction construction, signing,
resource-based fee estimation, and the withdraw pipeline for both
native TRX and TRC20 are functional. Atomic-swap participation
(V1 and V2), DEX-fee validation, watcher-based payment
monitoring, and EVM NFT withdraw are **explicitly deferred**: the
coin activates with no swap-contract binding, and every
swap/NFT-adjacent call site refuses Tron coins behind a typed
deferral, kept unreachable in production by the activation gate.

*Amended 2026-11-10 (clean-side status note):* the sentence above
that calls the withdraw pipeline functional overstated the code
until this date. Withdraw itself built, signed and broadcast Tron
transfers, but the coin's generic wallet surface did not: the
balance query answered an internal "not wired" error for TRX and
TRC20, raw-transaction broadcast went to the EVM node interface,
and the coin's own address was shown in the EVM form. §21.10A
records the wallet path as it stands from this date. All of it
stays unreachable while the availability constant is `false`.

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

> **In this beta (informative).** TRON activation is switched off
> (`TRON_ACTIVATION_AVAILABLE` is `false`; see the availability note
> at the top of this chapter): no route activates TRX or TRC20, and
> the legacy `enable` / `electrum` answer "TRON support is not
> available in this build". The flow below is the target for when
> TRON returns, planned for the next beta.

*Status note (2026-11-20).* The box above no longer holds: TRON
activation is switched on, and the flow below is what the tree does (the
status note of 2026-11-20 at the top of this chapter).

> **Reference behavior and as-built route (Spec Reader 2026-11-07, rows
> `v3.0.0-beta` and `feat/tron-swap-support (as of 2026-04-23)` of
> chapter 02 §2.15A, identical on this point; passed the KDF Dirty
> Gate on 2026-11-07, measure 0.01).** At
> the reference the three method strings of the table above are the
> **only** activation routes for TRX and TRC20: the one-shot
> `enable_eth_with_tokens` and the task family `task::enable_eth::init`
> / `status` / `user_action` / `cancel` accept a platform whose
> configured protocol is the TRON platform protocol and activate its
> TRC20 tokens from the same token list the EVM family uses; a request
> that also carries an NFT request is refused for a TRON platform. The
> legacy `enable` and `electrum` methods **refuse** a coin whose
> protocol is `TRX` or `TRC20` as a protocol they do not serve. There
> is no TRON-specific method on either row. *As built (this repository,
> not corpus-sourced):* Reloaded today has the opposite shape -- TRX
> and TRC20 are reachable only through legacy `enable` / `electrum`
> (behind the availability constant), and its `enable_eth_with_tokens`
> and `task::enable_eth::*` do not accept the TRON platform protocol
> ([Chapter 55](55-coin-activation-wire-contract.md) D55.5). The
> mmrpc-2.0 route of the table is therefore the parity target, and the
> legacy route a documented Reloaded extension; chapter 35 R35.1.1's
> `ticker` row describes the target, not the current tree.

*Status note (2026-11-17), the mmrpc 2.0 route is built (PR #405; D21.B1 (a),
decided 2026-11-17).* `enable_eth_with_tokens` and `task::enable_eth::init` /
`status` / `user_action` / `cancel` accept a platform whose protocol is `TRX`,
with its TRC20 tokens in `erc20_tokens_requests`; `enable_erc20` and
`task::enable_erc20::init` accept a `TRC20` token on a TRON platform. The
legacy `enable` and `electrum` routes are kept, as a documented Reloaded
extension of the reference (which refuses them for TRON). Both routes stay
behind `TRON_ACTIVATION_AVAILABLE` (`false`); while it is `false` the mmrpc
route answers the same "TRON support is not available in this build" refusal
(`PlatformCoinCreationError` on the platform route, `Internal` on the token
routes) and registers nothing. Built on the route, per R21.10B.2, R21.10B.4
and R21.10B.5 for the single-key login: the `nodes` list (`url`,
`komodo_proxy` parsed and, as on the EVM route, not signing requests; the
per-node `headers` object and `tron_api_key` of R-N9 apply to its entries),
the swap-contract rule of F21.B3 (base58check and hex forms accepted, see the
note at D21.B7), `required_confirmations`, `get_balances`, the NFT refusal, the
key-policy refusals (see the note at D21.10B.3), and the result keyed by the
base58check address. Not built in this change: the global-HD path (a
global-HD login is refused with `PrivKeyPolicyNotAllowed` naming TRON HD as not
yet available, except for `IguanaPrivKey`, which is honored on the node's
internal key; unreachable while the constant is `false`), transaction
history (`tx_history` is ignored), `convertaddress`, `get_my_address` and
`show_priv_key`, and swaps. On `task::enable_erc20::init` a token of the other
family than its active platform is `PlatformCoinMismatch` inside the task
status, not at `init`.

*Status note (2026-11-20), transaction history.* The item "transaction
history (`tx_history` is ignored)" of the note above no longer holds: on both
routes `tx_history: true` starts the TRON history when the node list has a
keyed TronGrid node, and is without effect otherwise (§21.10C).

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

*Status note (2026-11-17), step 4 (decided with R21.10B.2, PR #405).* Step 4 no
longer requires the coin config to declare `decimals` for a TRC20 token: a token
config without `decimals`, or with `decimals` 0, reads the token's `decimals()`
through `POST /wallet/triggerconstantcontract` at activation and uses the
answer; a configured value of 1 to 255 is used as it is with no node request. A
value above 255 or a failed read ends the activation with `Transport` (HTTP
502), and a configured value above 255 is refused as an invalid payload. This
holds on both routes (the mmrpc 2.0 route and the legacy `enable` / `electrum`
route). The zero or empty contract address is still refused. The sentence of
step 4 above, that the module does not introspect `decimals()`, is superseded.

**R-A2 (HD derivation -- dictated coin type).** When activated
under an HD-wallet private-key policy, Tron addresses MUST be
derived under the **SLIP-44 registered Tron coin type 195** (the
derivation path is `m/44'/195'/...`). The coin-type value 195 is
dictated by the public SLIP-44 registry (R29/R33); the derivation
plumbing is discretionary.

*Status note (2026-11-19), R-A2 built on the mmrpc 2.0 route.* Under a
global-HD login, `enable_eth_with_tokens` and `task::enable_eth::init` build
the TRON HD wallet for an absent `priv_key_policy` and for `ContextPrivKey`:
the address is the one at the coin config's `derivation_path` extended by
`path_to_address` (`m/44'/195'/0'/0/0` by default), the wallet is the one an
EVM platform has, and TRC20 tokens use it. A config without `derivation_path`
answers `InvalidPayload` (HTTP 400) before any node is contacted.

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
The host-only node-naming rule of
[Chapter 35](35-evm-v2-activation-rpcs.md) R35.1.14 applies to Tron node
URLs in every log line and error text as well. It is a naming rule only:
Tron requests do not go through the EVM node transport, so the ordering
policy of this rule, including its stop on a non-retryable rejection, stays
Tron's own and is not overridden by R35.1.8 there. The credentials rule of
R35.1.17 applies to the Tron client and its node list in the same way, with
the same scope limit for the RPC wire dump (Reloaded-original; maintainer
decision 2026-10-26; no switch). *Status (2026-10-28):* the Tron client's
debug form already hides its node list.

**R-N2 (request timeout).** Each node request MUST be bounded by a
per-request timeout so a stalled node cannot block the pipeline.
The concrete timeout value is discretionary.

*Status note (2026-11-17).* The sentence of R-N1 that speaks of the
ordering policy's "stop on a non-retryable rejection" is narrowed to
R-N6: the only answer that ends a request at a node without trying the
others is a well-formed broadcast rejection. Every other node failure
(R-N4) moves on to the next node. The sentence itself is left as written.

**R-N3 (per-attempt cutoff; Reloaded-original).** Each node attempt MUST
be bounded by a wall-clock cutoff of 10 seconds that covers connecting,
sending the request, reading the whole body and decoding it. There is no
overall deadline beyond the attempts: a request over N nodes takes at most
about N times the cutoff. The value is that of
[Chapter 35](35-evm-v2-activation-rpcs.md) R35.1.10, so that Tron and EVM
nodes behave alike. This realizes R-N2. A native build ends the spawned
exchange at the cutoff; a browser build ends the wait only (the browser's
own request is not aborted).

**R-N4 (what a node failure is; Reloaded-original).** Each of the
following moves the request on to the next node: (a) a connection,
transport or name-lookup failure; (b) the cutoff of R-N3; (c) an HTTP
status outside 2xx, 429 included; (d) a body that is not JSON or does not
decode into the endpoint's answer; (e) a 2xx body that is the node's error
form, a JSON object with a top-level `Error` string (measured on a public
node). A read MUST never be answered from a non-2xx body. An endpoint's
"empty" answer (an absent account) counts only as a 2xx answer in the
endpoint's documented empty form. A redirect is not followed: a 3xx status
is a non-2xx failure (c).

**R-N5 (order; Reloaded-original).** The starting order is the configured
order; the client does not shuffle it. Whether the reference shuffles it as
the EVM path does (R35.1.7) is an open parity question; until it is
answered the configured order stands. The node that answers becomes the
first node of the order and the nodes that failed before it move to the
tail in their relative order. The order is shared by the coin's client and
its clones and lives as long as the coin is activated.

**R-N6 (broadcast; Reloaded-original).** `broadcasthex` moves on after
(a) to (d) like every other endpoint (and after (e), as before), so a signed
transaction may reach several nodes; this is safe because the transaction
id is fixed. A well-formed rejection by a node (`result: false` with a code)
ends the request with that node's code and message; no further node is
asked. How a node answers a duplicate of a transaction that an earlier,
timed-out attempt already delivered has not been measured: the rejection is
reported as it is, and the caller can confirm the transaction through
`gettransactioninfobyid`.

**R-N7 (concurrency; Reloaded-original).** No lock is held while a node is
being asked. A request walks a snapshot of the order and, when a node
answers, stores the rotation of that snapshot; the last store wins and the
stored order is always a complete permutation (as D35.8 (iii) for the EVM
path). Two requests of one coin MAY run at the same time, and a request
stuck on a slow node does not hold up another.

**R-N8 (all nodes failed; Reloaded-original).** The caller gets one error:
of the class of the last failure that was not a cutoff, or, when every node
hit the cutoff, a timeout-class "nodes unreachable" error. The text is
host-only (R35.1.14 via R-N1) and credential-free (R35.1.17), and lists each
host tried once with its failure. Each node failure is logged at debug level
with the coin's ticker and the node's position in the configured list
(R35.1.16 form); logging the all-failed error is the caller's.

**R-N9 (request headers and API key; Reloaded-original).** An entry of the
legacy `urls` array of the activation request is either a URL string or an
object `{"url": "...", "headers": {"Name": "value", ...}}`; the headers of
an object entry go to that node only. Header names MUST be valid HTTP
header names and values valid HTTP header values; an invalid one refuses
the activation with an error that does not echo it. The optional top-level
`MM2.json` key `tron_api_key` is sent as the header `TRON-PRO-API-KEY` only
to the hosts `api.trongrid.io`, `nile.trongrid.io` and
`api.shasta.trongrid.io` (an exact host match, letter case ignored; never a
prefix, suffix or substring match, and never the userinfo of a URL), and only
when the node URL's scheme is `https`: a TronGrid host given as `http` gets no
key, while the headers of its own entry stay as configured. The key is never
sent to another host of the same list. A header of the same name (letter
case ignored) on the node's own entry wins over the key for that node. The
headers and the key are secrets: no log line, error text, RPC answer or
debug output shows them (the debug forms say `headers: <set>` and
`tron_api_key: <set>`), they go to no other host, and no peer sees them. The
placeholder alloy provider that activation builds from the same node list
(§21.4) receives the plain URL strings only, without headers: a known
boundary, as no Tron request goes through it. A reference client never sends
either form (a Reloaded request extension; no switch, because it adds an
input and changes nothing for a client that does not send it).

*Status note (2026-11-17), follow-ups of R-N3 and R-N6.* (1) On the browser
build the request to a node is not aborted at the cutoff of R-N3; only the
wait ends. Remedy: wire an abort signal (`AbortController`) into the browser
request. (2) R-N6: an attempt that timed out may already have delivered the
transaction, so a later node's rejection of the duplicate can report a sent
transaction as rejected. Remedy: measure on the Nile test network how a node
answers a duplicate broadcast (lab row W9); until that is measured, no swap
path may rely on broadcast failover, and a caller confirms a broadcast through
`gettransactioninfobyid`.

*Status note (2026-11-17), alignment with R21.10B.3.* The reference's node
order and failover rules of R21.10B.3 are now followed where they differ
from the text above; the sentences above are left as written.
(1) R-N5: the configured list is shuffled once when the coin's client is
built, then each request walks from the head and the rotation of R-N5
stays; the parity question that R-N5 names is answered. A node keeps its
configured list position in log lines. (2) R-N4, R-N6: a 2xx error object
(`result: false` with a `code`, a `result` object with a `code`, or an `error`
object with a `code`) whose code is `SERVER_BUSY`, `NO_CONNECTION`,
`NOT_ENOUGH_EFFECTIVE_CONNECTION` or `BLOCK_UNSOLIDIFIED` moves the request to
the next node, `broadcasthex` included; any other code stays final (R-N6). A
re-sent transaction is rejected by the Nile test network as a duplicate and
moves funds once. (3) R-N4 (d) is checked per node for more endpoints: a
current-block answer with a negative number or a non-positive timestamp, a
chain-parameter answer without a positive `getTransactionFee` or
`getEnergyFee`, and a balance read whose `constant_result` is absent, empty or
whose first entry is not hex move on. (4) Activation accepts only the `http` and `https` schemes
(letter case ignored); another scheme is refused, naming the node's position
only. R-WP2 still governs the top-level `Error` form (any text moves on), and
R-N8 governs the all-failed error, where R21.10B.3 differs.

*Status note (2026-11-17), request URL of a node (PR #405).* The request URL of an
endpoint is the node URL with the endpoint path appended to the URL's path and
the URL's query kept at the end: `https://h/x?key=1` is asked as
`https://h/x/wallet/getaccount?key=1`; a fragment is dropped, a trailing slash
on the node URL is ignored (`https://h/` is asked as
`https://h/wallet/getaccount`), and a URL without a query is asked as before. A key in the query string of a node URL therefore
works; the node is still named host-only everywhere (R-N1). The `headers` object
and `tron_api_key` of this rule also apply to the entries of the `nodes` list of
the mmrpc 2.0 route (R21.10B.2).

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

*Amended 2026-11-10 (clean-side status note):* the fee of a Tron
withdraw is always estimated as R-W3 describes. Until this date a
`fee` descriptor in the request was silently ignored; from this
date it is refused by R-WP6 (§21.10A).

**R-W4.** Build the dictated contract -- a native `TransferContract`
or a TRC20 `TriggerSmartContract` carrying the ABI-encoded
`transfer(address,uint256)` call (§21.6) -- with the resolved
TAPOS and fee-limit.

*Amended 2026-11-10 (clean-side status note):* until this date no
Tron withdraw reached the chain. The current-block answer was read
under a key the node does not send (the node sends `block_header`),
and a native `TransferContract` was encoded without its contract-type
field, which the node then read as type 0. Both were found by testnet
withdrawals on 2026-11-10 (chapter 01 R6) and are fixed; T-WP pins the
node's block answer and the encoded contract bytes.

**R-W5.** Hash the raw body with SHA-256, sign it with secp256k1,
assemble the full transaction, hex-encode it, and broadcast it via
the broadcast endpoint (§21.9).

**R-W6.** Return transaction details denominated in whole TRX
(converted from SUN at `1 TRX = 10^6 SUN`), with the fee broken
into bandwidth and energy components (R-F2).

> **GasFree fee delegation (pointer, 2026-11-08).** A second,
> provider-relayed withdraw rail for TRC20 tokens -- the user signs
> an off-chain `PermitTransfer` authorization and a GasFree provider
> pays the resource cost, charging its fee in the token -- is
> specified by [Chapter 63](63-tron-gasfree-fee-delegation.md). That
> chapter extends the shared withdraw request (`fee_method`,
> `gasless`), the balance reports (`gasfree_address`) and the
> EVM/Tron activation requests. R-W1–R-W6 above are the native rail
> chapter 63 calls `fee_method = native`; they are unchanged by it.
> Chapter 63 passed the KDF Dirty Gate on 2026-11-08.

## 21.10A Wallet Path (clean-side, added 2026-11-10)

Reloaded-original rules for the coin's generic wallet surface:
the balance query, raw-transaction broadcast, the coin's own
address and the withdraw fee descriptor. The node's answer forms
they rely on are public-network observations of the Nile testnet
node on 2026-11-10 (chapter 01 R6): an account the chain has not
created yet answers HTTP 200 with an empty JSON object, and a
request the node cannot serve answers HTTP 200 with a JSON object
carrying a top-level `Error` string.

**R-WP1 (TRX balance).** The TRX balance is the account query's
`balance` in SUN (§21.9), converted at `1 TRX = 10^6 SUN`. Only a
2xx empty object means an account the chain has not created yet,
and only it reads as a zero balance. A non-empty answer with
neither `balance` nor `create_time` is an error. A non-2xx status,
an unreachable node, an undecodable body or a negative balance is
an error, never zero.

**R-WP2 (node failure).** A non-2xx answer from any node endpoint,
and a 2xx answer whose JSON object has a top-level `Error` key,
is a node failure. The request moves to the next configured node.
When the last node fails with the `Error` form, the caller gets
that node's own error text, typed, with no node URL or credential
part (R-N1).

**R-WP3 (TRC20 balance).** The TRC20 balance is `balanceOf(owner)`
through the constant-contract endpoint (§21.9). The result MUST be
exactly 32 bytes, read big-endian and scaled by the token's
decimals. A missing, empty, non-hex or wrong-length result
(revert data included) is an error. The withdraw path reads the
balance by the same rule.

**R-WP4 (broadcast).** Raw-transaction broadcast posts the
lowercase hex of the signed transaction to the hex broadcast
endpoint (§21.9), never to an EVM node. Success needs the success
flag and a 32-byte `txid`. The id is returned as lowercase hex
without `0x`, the form the EVM path returns. A refusal
(`result: false`) carries the node's code and message. An answer
without the success flag or without a usable `txid` is an error,
never an empty or invented id.

**R-WP5 (own address).** The coin's own address is shown in the
base58check display form of §21.5. Address validation for a Tron
coin accepts exactly the forms a Tron withdraw accepts: the
base58check form and the 21-byte wire form in hex (`41…`,
optionally behind `0x`). An EVM `0x` 20-byte form is refused.

**R-WP6 (withdraw fee descriptor).** A Tron withdraw request that
carries a `fee` descriptor of any form is refused with
`InvalidFeePolicy` before any node request, the answer chapter 49
gives for a fee descriptor of a type the coin does not take. The
fee is the R-W3 estimate. A request without `fee` is unchanged.

*2026-11-20 (clean-side note): the shared fee descriptor now has a
`Tron` form, so a request whose `fee` is `{"type":"Tron"}` (for
example a TRON result's `fee_details` sent back) parses and is
refused with `InvalidFeePolicy` like any other form; before, it
failed request parsing.*

**T-WP (tests).** Unit tests against a loopback stub node
(`coins/eth/tron/wallet_path_tests.rs`, `tron/balance.rs`,
`tron/broadcast.rs`, `tron/address.rs`) cover each rule: the
absent-account, `Error`-form, non-2xx, undecodable and negative
answers of R-WP1/R-WP2, including failover to a second node;
the exactly-32-byte result of R-WP3; accepted, refused and
malformed broadcast answers of R-WP4; the display and validation
forms of R-WP5 for TRX, TRC20 and an unchanged EVM coin; and every
fee descriptor form of R-WP6, with no node request made. Further
tests decode the node's current-block answer in its observed form,
take a TRX and a TRC20 withdraw through a stub node to a signed
transaction, and check that the encoded contract of a TRX transfer
starts with its type field (`08 01`) and that of a TRC20 transfer
with `08 1f`.

## 21.10B Reference behavior of the TRON wallet surface

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-17,
> measure 0.04 (attestation in §21.13).**
> This section was written on 2026-11-17 by the KDF Spec Reader (Spec Reader
> round 0.5.0-10) under the two-team procedure of chapter 01 §1.4A, from the
> reference lineage at the rows `v3.0.0-beta`, `v2.6.0-beta` and
> `feat/tron-swap-support (as of 2026-04-23)` of chapter 02 §2.15A (read
> read-only; no untabled state was read), public TRON documentation, and
> this repository's own tree. It edits no sentence of §21.0 -- §21.11B. Each
> rule names the existing rule it completes; where a statement here is more
> exact than the sentence it completes, the statement here describes the
> reference and the completed rule still governs Reloaded until the lane
> decides otherwise. Numbering: R21.10B.1 -- R21.10B.9, T21.10B.1 --
> T21.10B.15, D21.10B.1 -- D21.10B.3.

*Scope.* The wallet surface a client meets when TRON is switched on: the
mmrpc 2.0 activation route, the node client's order and failover, key
policies and HD methods, the activation result, transaction history, the
withdraw fee estimate, the address methods, and orders. Swaps stay with
§21.11B. Unless a rule says otherwise it states the row `v3.0.0-beta`.

**R21.10B.1** *Reference states (completes the §21.4 reference note).* The
row `v2.6.0-beta` has **no TRON wallet behavior**: the `TRX` protocol case
and its network selector exist, there is no `TRC20` case, and a `TRX`
platform given to `enable_eth_with_tokens` or `task::enable_eth::init` is
not refused but built as an ordinary EVM coin (EVM JSON-RPC transport over
the given nodes, 18 decimals, the EVM address form, `swap_contract_address`
required as for every EVM platform at that row); no rule below holds there.
The row `feat/tron-swap-support (as of 2026-04-23)` agrees with
`v3.0.0-beta` on every point of this section except one: its withdraw fee
estimate has no account-creation cost and its fee details have no
`account_creation_fee` member (R21.10B.7).

**R21.10B.2** *Activation request (completes R-A1 and the §21.4 note; the
member table of chapter 35 R35.1.1 applies with these TRON points).*

- *Routes.* Both `enable_eth_with_tokens` and `task::enable_eth::init`
  read the same request; TRC20 tokens may also be added later through
  `enable_erc20` and `task::enable_erc20::init`, which accept a `TRC20`
  protocol on a TRON platform. A token whose `platform` is not the
  requested platform answers `PlatformCoinMismatch` (HTTP 400).
- *`nodes`.* Each entry reads `url` and `komodo_proxy` (default `false`;
  when `true` the requests to that node carry the signed-proxy
  authentication of chapter 35's `nodes[].komodo_proxy` row). Only the
  `http` and `https` schemes are accepted; any other scheme, or a `url`
  that does not parse, answers `InvalidPayload` (HTTP 400). An empty or
  absent list answers `AtLeastOneNodeRequired` (HTTP 400). The nodes are
  not contacted while the list is checked.
- *`swap_contract_address`, `fallback_swap_contract`, `swap_v2_contracts`.*
  The generic EVM rule of F21.B3 (§21.11B.1): ignored for a coin whose
  config says `"wallet_only": true`; for a trading-capable coin
  `swap_contract_address` is required and non-zero, `fallback_swap_contract`
  optional and non-zero, and `swap_v2_contracts` required when the node
  runs trading protocol v2, each refusal `InvalidPayload` (HTTP 400). The
  members take the EVM `0x` 20-byte hex form; a base58check value fails
  request decoding.
- *`required_confirmations`.* The request value, else the coin config's,
  else 1; a token's `required_confirmations` in `erc20_tokens_requests`
  overrides the token config's, else the platform's effective value.
- *`tx_history`.* Not a member of this request: the value is ignored and
  no history process starts (R21.10B.6).
- *`get_balances`.* Default `true`. When `false`, a single-address result
  carries no balances and lists the token tickers instead (chapter 35
  R35.1.3); an HD result is unaffected.
- *Other members.* `priv_key_policy` (R21.10B.4), `rpc_mode`,
  `path_to_address`, `gap_limit`, `scan_policy`, `min_addresses_number`,
  `mm2`, `contract_supports_watchers` and `swap_gas_fee_policy` are read as
  for every EVM platform.
- *Refused requests.* A request carrying `nft_req` answers `Internal`
  (HTTP 500) on the one-shot route, and the same token in the task status,
  before any key or node work; nothing is registered.
- *Token decimals.* A TRC20 token whose config has no `decimals`, or
  `decimals` 0, is **not** refused: the reference reads the token's
  `decimals()` through `POST /wallet/triggerconstantcontract` at activation
  and uses the answer. A value above 255 or a failed read answers
  `Transport` (HTTP 502). A configured contract address that is neither
  form of R-AD1 answers `TokenProtocolParseError` (HTTP 500). This differs
  from R-A1 step 4, which stays the Reloaded rule.
- *Node failure at activation.* When no node yields the current block, the
  activation answers `Internal` (HTTP 500), not `Transport`; a failed
  balance read answers `Transport` (HTTP 502).

**R21.10B.3** *Node order and failover (completes R-N1, R-N2 and R-WP2).*

- *Order.* The configured list is shuffled into a random order once, at
  activation. Each request walks the list from its head. When a node
  succeeds, the list is rotated so that this node becomes the head and the
  nodes that failed before it move, in their order, to the tail.
- *Time bound.* Each request to one node is bounded by **10 seconds** (all
  builds). There is no bound over the whole walk, so a request can take up
  to 10 seconds per configured node.
- *Answers that move the request to the next node:* a connection or
  transport failure; the time bound; **any non-2xx HTTP status** (429 and
  5xx included, with any body); a 2xx body that is not JSON; a 2xx JSON
  body that does not have the shape the endpoint documents; a 2xx error
  object (below) whose code is one of the public TRON `response_code`
  names `SERVER_BUSY`, `NO_CONNECTION`, `NOT_ENOUGH_EFFECTIVE_CONNECTION`
  or `BLOCK_UNSOLIDIFIED`, or whose text is the answer of the java-tron
  HTTP rate limiter (the text is java-tron's own, published in that
  project's source). Endpoint-level checks also move on: a current-block
  answer with a negative number or a non-positive timestamp, a
  chain-parameter answer without a positive `getTransactionFee` or
  `getEnergyFee`, and a balance or decimals read whose first
  `constant_result` is empty or not hex. An energy dry run whose answer
  has no positive `energy_used` does **not** move on: the withdraw fails
  with `Transport` (HTTP 502).
- *2xx error objects recognized* (the forms the public TRON HTTP API
  uses): an object with a top-level `Error` string; an object with an
  `error` object carrying `code` and `message`; an object whose `result`
  is an object with `result: false`, or without `result` but with `code`;
  and an object with `result: false` beside `code` and `message`. Any such
  object whose code and text are not in the list above is a **final**
  rejection: the request fails at once and no further node is asked. A
  2xx empty object is not an error object (it is the absent-account
  answer of R-WP1).
- *Exhaustion.* When every node failed by a moving answer, the caller
  gets the last node's failure. A withdraw reports every node failure,
  final or not, as `Transport` (HTTP 502).

R-WP2 makes Reloaded move on after a top-level `Error` object of any text;
the reference stops there unless the text is the rate limiter's. R-WP2
governs Reloaded.

**R21.10B.4** *Key policies and HD (completes R-A2).*

- *Single key and global HD.* Both activate. Under a global-HD login the
  address is derived under the coin config's `derivation_path` (the
  reference's own TRON configs use `m/44'/195'`, the SLIP-44 TRON type of
  R-A2), with `path_to_address` and `gap_limit` as for every EVM platform;
  a config without `derivation_path` answers `InvalidPayload` (HTTP 400).
  TRC20 tokens use the platform's HD wallet and therefore its coin type.
- *HD methods.* `get_new_address`, `account_balance`,
  `task::get_new_address::init`, `task::scan_for_new_addresses::init` and
  `task::account_balance::init` work for a TRX or TRC20 ticker under a
  global-HD login; addresses are reported in base58check and balances as a
  map from ticker to balance covering the platform and its enabled
  tokens. An address counts as used when the chain has created its
  account (a non-empty `POST /wallet/getaccount` answer) or when any
  enabled TRC20 token has a non-zero balance at it. `task::create_new_account::init`
  fails for every TRON coin with `Internal` in the task status, before any
  account is created.
- *Trezor.* `enable_eth_with_tokens` refuses every hardware policy with
  `UnexpectedDeviceActivationPolicy` (HTTP 400). On `task::enable_eth::init`
  a TRON platform under the Trezor policy, with an otherwise valid request,
  ends with `Internal` in the task status before any device exchange.
- *WalletConnect.* Refused on both routes with `Internal` (HTTP 500 on the
  one-shot route), because a TRON chain has no EVM chain identifier.
- *MetaMask (browser builds only).* `rpc_mode` `Metamask` is refused with
  `Internal`. The MetaMask `priv_key_policy` with the default `rpc_mode`
  is **accepted**: the coin activates on the MetaMask account's key, shown
  in base58check, while withdraw (chapter 47 R47.5.6a) and message signing
  refuse it. D21.10B.3.

*Status note (2026-11-19), the HD methods for a TRC20 ticker (Spec Reader
round 0.5.0-15; passed the KDF Dirty Gate).* The "HD methods" item of
R21.10B.4 describes a TRX ticker. For a TRC20 ticker the reference answers
from the platform's HD wallet, which the token shares, and each balance map
holds that token's ticker only. The scan then counts an address as used
when the chain has created its account, and no token balance is
consulted. The shared wallet and the token-only ticker set match what
chapter 49 §49.2.1 (R49.32) records for an ERC-20 ticker on an EVM
platform; the used-address criterion does not. The sentences of
R21.10B.4 above are unchanged.

*Status note (2026-11-22), the task form of `get_new_address` for a TRON
ticker (Spec Reader round 0.5.0-26; passed the KDF Dirty Gate with chapter
49 §49.2.3).* The "HD methods" item of R21.10B.4 lists
`task::get_new_address::init` among the methods that work for a TRX or
TRC20 ticker. At the row `v3.0.0-beta` init answers a task id for such a
ticker, but the task cannot complete: it requires a hardware context and
ends with `HwContextNotInitialized` without one, and with one it asks the
device for an Ethereum-form address that never equals the base58check
address it expects ([Chapter 49](49-withdrawal-task-path.md) R49.38 item 5,
R49.42). The direct `get_new_address` and the other listed methods are not
affected. The sentences of R21.10B.4 above are unchanged.

*Reloaded note (2026-11-22), `task::get_new_address::init` for a TRON ticker
is served here.* This repository serves the task form for a TRX and a TRC20
ticker on the software path, the only path a TRON coin has, and it completes
with the base58check address and the balance map of the direct method. The
internal chain is refused with `InvalidBip44Chain`. The sentence of the
Reloaded note of 2026-11-19 that this repository has no
`task::get_new_address::init` is no longer true. The record is in
[Chapter 49](49-withdrawal-task-path.md) §49.2.3, the status note "the task
form of `get_new_address` (as built)". No sentence above is changed.

*Reloaded note (2026-11-19), the HD methods for a TRC20 ticker in this
repository (its own code, not the reference; PR #428).* Asked for a TRC20
ticker, `get_new_address`, `account_balance`,
`task::account_balance::init` and `task::scan_for_new_addresses::init`
answer from the HD wallet of the platform, and each balance map holds that
token's ticker only, as chapter 49 §49.2.1 (R49.32) records for an ERC-20
ticker on an EVM platform. Asked for the TRX ticker, each balance map
covers TRX and the enabled TRC20 tokens. The activation result is the same
as before. The used-address criterion is deliberately not the reference's:
for a TRC20 ticker, as for the TRX ticker, an address counts as used when
the chain has created its account or when any enabled TRC20 token has a
non-zero balance at it. Receiving a TRC20 token does not create the
account, so a scan that consulted no token balance would miss an address
that holds only a token. The addresses found this way are a superset of
those the account-only criterion finds, so no funds are found by the
reference's criterion and missed by this one; the difference is not
visible to peers and has no switch.

**R21.10B.5** *Activation result (completes the §21.4 note and R-WP5).* The
result has the EVM family's shape with no TRON-specific member. Single
key: `current_block` is the TRON head block number from
`POST /wallet/getnowblock`; `eth_addresses_infos` and `erc20_addresses_infos`
are keyed by the base58check address; each entry carries
`derivation_method` (`{"type":"Iguana"}`), `pubkey` (the uncompressed
secp256k1 key as `04` followed by 128 hex digits) and `balances` (or
`tickers` when `get_balances` is `false`). TRX balances are in whole TRX at
6 decimals, TRC20 balances at the token's decimals, each as `spendable`
and `unspendable` (0) decimal strings; `nfts_infos` is empty. Global HD:
`current_block`, `ticker`, `wallet_balance` (accounts with base58check
addresses and their derivation paths) and an empty `nfts_infos`.
`enable_erc20` reports the token contract in base58check as
`token_contract_address`.

*Status note (2026-11-20), `gasfree_address`.* R21.10B.5 describes the rows it
names, which have no GasFree. When the platform was activated with a GasFree
provider, the address entries of the result also carry `gasfree_address`
([Chapter 63](63-tron-gasfree-fee-delegation.md) §63.3.3 and its status note of
2026-11-20); without one the result is as R21.10B.5 states. The sentences of
R21.10B.5 above are unchanged.

**R21.10B.6** *Transaction history (completes R-T4).* No TRON history source
exists at either row: the reference reads none of the full-node API, a
provider's account endpoints or the event service for history, and no
background history process runs for TRX or TRC20. The versioned
`my_tx_history` answers `NotSupportedFor` (HTTP 400) for TRX and TRC20, as
for the whole EVM family. The legacy `my_tx_history` answers success with
an empty `transactions` list (only a history file stored earlier under the
same ticker and address would be read), `current_block` from the TRON
node, and `sync_status` `{"state":"NotEnabled"}`. The only TRON transaction
record the reference produces is the withdraw result of R21.10B.7: `from`
and `to` in base58check, `tx_hash` as 64 lowercase hex digits without
`0x`, `tx_hex` the signed protobuf transaction, `block_height` 0,
`internal_id` empty, `timestamp` the local time of building,
`transaction_type` `StandardTransfer`, `memo` null and `fee_details` of
R21.10B.7.

*Status note (2026-11-20), Reloaded has a TRON history.* R21.10B.6 describes
the reference. Reloaded keeps these answers for a TRON coin without a TronGrid
API key or without `tx_history: true`, and otherwise serves a history from a
provider's account listings through both methods: §21.10C.

**R21.10B.7** *Withdraw fee estimate and amounts (completes R-F1, R-F2,
R-W2, R-W3 and R-W6).*

- *Inputs read per withdraw:* the current block, the sender's
  `POST /wallet/getaccountresource` (`freeNetUsed`, `freeNetLimit`,
  `NetUsed`, `NetLimit`, `EnergyUsed`, `EnergyLimit`; an empty answer is
  all zero) and `POST /wallet/getchainparameters` (`getTransactionFee`,
  `getEnergyFee`, `getCreateNewAccountFeeInSystemContract`,
  `getCreateAccountFee`, `getCreateNewAccountBandwidthRate`; the last
  defaults to 1).
- *Bandwidth* is the serialized size of the transaction with one 65-byte
  signature plus 64 bytes per contract. For a recipient whose account
  exists, the bandwidth fee is the shortfall of the sender's free plus
  staked bandwidth, times `getTransactionFee`.
- *New account (TRX only).* For a TRX transfer to an address the chain has
  not created, the estimate adds `getCreateNewAccountFeeInSystemContract`
  as `account_creation_fee`; the bandwidth is the size times
  `getCreateNewAccountBandwidthRate` and is free when the sender's
  **staked** bandwidth covers it, else it is replaced by
  `getCreateAccountFee` (reported with `bandwidth_used` 0). A node that
  does not report a positive `getCreateNewAccountFeeInSystemContract`
  makes such a withdraw fail with `InternalError` (HTTP 500). A TRC20
  transfer never counts an account-creation cost.
- *Energy (TRC20).* The dry run of `transfer(address,uint256)` through
  `POST /wallet/triggerconstantcontract` gives `energy_used`; the energy
  fee is its shortfall over the sender's staked energy, times
  `getEnergyFee`. The contract's origin energy share is **not** read: the
  whole dry-run energy is charged to the sender. The transaction's
  `fee_limit` is the dry-run energy times `getEnergyFee`, with no margin
  (D21.10B.1). A TRX transfer has no energy and `fee_limit` 0.
- *Amounts.* TRX `max: true`: a zero balance answers
  `ZeroBalanceToWithdrawMax`; otherwise the amount is the balance less the
  fee of the transaction carrying that amount, settled when the fee no
  longer changes, and a balance that does not exceed the fee answers
  `AmountTooLow` with `amount` 0 and the fee as `threshold`. TRX without
  `max`: an amount above the balance, or an amount plus fee above it,
  answers `NotSufficientBalance` (the second with `required` the sum).
  TRC20 `max: true` sends the whole token balance; for any TRC20 withdraw
  a TRX balance below the fee answers `NotSufficientPlatformBalanceForFee`
  with the platform ticker, the TRX balance and the fee. An amount of zero
  without `max` is not refused before building. All four answer HTTP 400.
- *Other request members.* Any `fee` answers `InvalidFeePolicy`; a `memo`
  answers `UnsupportedError`; `expiration_seconds` (TRON only) sets the
  transaction's expiry that many seconds after the reference block's
  time, default 60, with no upper bound.
- *Fee details.* `{"type": "Tron"}` with `coin` (TRX; for a token, the
  platform ticker), `bandwidth_used` and `energy_used` (integers),
  `bandwidth_fee`, `energy_fee` and `total_fee` (decimal strings in TRX),
  and `account_creation_fee` (decimal string, present only when not zero).
  `spent_by_me` adds the fee for TRX and not for a token.
- *Routes.* The direct `withdraw` and `task::withdraw::init` take the same
  path for TRX and TRC20; the task delivers the same result or error
  token in its status. Key-policy refusals are chapter 49's (R49.9 and the
  `UnsupportedError` arm of R49.27a).

The node's own bandwidth rule (TRON Developer Hub, *Resource Model*) draws
a transaction's bandwidth from staked or from free bandwidth as a whole and
otherwise burns TRX for the whole size; the reference charges only the
shortfall of the two pools summed. Remedy: Reloaded's estimate follows the
node's rule, so that it is never below what the node charges.

*Status note (2026-11-19), what Reloaded does for R21.10B.7.* Reloaded's TRON
withdraw follows R21.10B.7 with the remedy above.

- *No fallback figures.* The estimate is part of the withdraw and no longer
  best-effort. A failed read of the current block, of the sender's
  `getaccountresource`, of `getchainparameters`, of the recipient's account
  (TRX) or of the energy dry run (TRC20, a missing or non-positive
  `energy_used` included) fails the withdraw with `Transport`; so does a
  resource answer that does not decode and a parameter answer without a
  positive `getTransactionFee` or `getEnergyFee`. Every balance and resource
  read is for the sender: the activated address, or the HD address a `from`
  member selects.
- *Bandwidth.* The size is the serialized size of the transaction with its one
  65-byte signature, plus 64 for its contract. It costs nothing when the
  sender's remaining staked bandwidth covers the whole size, nothing when the
  remaining free bandwidth covers the whole size, and otherwise the whole size
  is burned at `getTransactionFee`. The two quotas are never summed, so an
  account whose quotas only together reach the size is estimated the full
  burn, as the node charges it.
- *New account (TRX only).* As the rule states. The account-creating case also
  needs `getCreateAccountFee` when the staked bandwidth does not cover the
  transfer; a node that does not report it makes that withdraw fail with
  `InternalError`, like a node without a positive
  `getCreateNewAccountFeeInSystemContract`. A reported
  `getCreateNewAccountBandwidthRate` that is not positive is an unusable
  parameter answer (`Transport`). A TRC20 transfer counts no account-creation
  cost and does not read the recipient's account.
- *Energy.* As the rule states; the `fee_limit` is the one of the note at
  D21.10B.1.
- *Amounts.* As the rule states. For TRX `max` the amount is recomputed from
  the fee of the transaction that carries it until that fee no longer changes.
  Where the amount lies next to a length boundary of its encoding, so that the
  two neighboring fees would alternate, the amount is the largest one found
  that fits the balance together with its own fee; at most the price of the
  alternating bytes stays unsent, and the amount plus the fee is never above
  the balance. An amount of zero without `max` is still not refused before
  building.
- *Other request members.* `fee` (`InvalidFeePolicy`, R-WP6) and `memo`
  (`UnsupportedError`) are refused before any node request.
  `expiration_seconds` is an optional member of the shared withdraw request
  (chapter 49), read for a TRON coin only and ignored by every other family;
  absent is 60. A value whose expiry does not fit the protocol's 64-bit
  millisecond field answers `UnsupportedError`. The transaction's own
  `timestamp` stays the local time of building.
- *Fee details.* The members of the rule. Before this date the object carried
  integer members in SUN under other names; nothing stored them. `total_fee`
  is the expected charge; no `fee_limit` member is added. `spent_by_me` is the
  amount plus `total_fee` for TRX and the amount for a token.

Tests: `coins/eth/tron/withdraw_fee_tests.rs` (T21.10B.11 -- T21.10B.13 and
each point above, against a stub node, with hand-computed figures),
`tron/fee.rs`, `tron/withdraw.rs` and, for the HD sender,
`tron/hd_tests.rs`.

*Status note (2026-11-20), TRC20 amounts.* A TRC20 amount is encoded as a
256-bit value, in the balance comparison, the energy dry run and the
transaction's call data, so a token's whole balance can be sent whatever its
decimals. Before this date the amount was narrowed to 64 bits and a larger one
answered `InternalError`. Call data of an amount that fits 64 bits is
unchanged; TRX amounts in SUN stay 64-bit values. Tests:
`tron/tx_builder.rs` and `tron/withdraw_fee_tests.rs`, against hand-computed
call data.

*Status note (2026-11-20), origin-energy share.* What the reference does
about the energy a TRC20 contract pays itself is stated in R21.10B.10 --
R21.10B.12 below, and what it does when the dry run reverts in R21.10B.13
(Spec Reader round 0.5.0-17). The sentences of R21.10B.7 are unchanged.

> **Clearance (chapter 01 R40) -- passed the KDF Dirty Gate on
> 2026-11-20, measure 0.02, for R21.10B.10 -- R21.10B.13, T21.10B.16 --
> T21.10B.18 and the notes between them (Spec Reader round 0.5.0-17;
> attestation in §21.13).**

**Origin-energy share of a TRC20 transfer (added 2026-11-20).** *Public
rule (TRON Developer Hub, Resource Model and smart-contract energy
pages; not corpus-derived).* A contract's public settings
`consume_user_resource_percent` and `origin_energy_limit`, returned by
`POST /wallet/getcontract`, let the contract's deployer pay part or all of
a call's energy: the caller is charged that percentage of the energy used,
and the rest is drawn from the deployer's staked energy, bounded per call
by `origin_energy_limit` and by what the deployer has available; what the
deployer cannot cover falls back on the caller. A receipt shows the split
as `origin_energy_usage` within `energy_usage_total`. The rules below
state the rows `v3.0.0-beta`, `v2.6.0-beta`, `feat/tron-swap-support (as of
2026-04-23)` and `feat/tron-gasfree (as of 2026-05-04)` of chapter 02
§2.15A.

**R21.10B.10** *The reference does not model the origin share (completes
the "Energy (TRC20)" bullet of R21.10B.7).* At `v3.0.0-beta` the only
energy input of a TRC20 fee is the `energy_used` member of the dry run
through `POST /wallet/triggerconstantcontract`; that figure is the whole
energy of the call, whoever pays it. The contract's percentage, its origin
limit and the deployer's available energy are not inputs of any fee figure.
Per method:

- *`withdraw` and `task::withdraw::init` (TRC20).* `energy_used` of the
  fee details is the whole dry-run energy; `energy_fee` is the part of it
  the sender's own unused staked energy does not cover, times
  `getEnergyFee`; `total_fee` adds the bandwidth fee. All of the energy
  is treated as the caller's. For a contract whose deployer pays the
  energy, the reported `energy_fee` and `total_fee` are therefore above
  what the chain charges, by up to the whole energy fee.
- *The TRX-for-fee check of a TRC20 withdraw.* The sender's TRX balance
  is compared with that same `total_fee`. A sender whose TRX covers the
  bandwidth and the caller's real share, but not the whole-energy figure,
  is refused with `NotSufficientPlatformBalanceForFee` (HTTP 400) and
  no transaction is returned.
- *Trade-fee and swap-fee methods.* There is no TRON fee estimate in any
  of them, so the question does not arise. `trade_preimage` refuses a
  wallet-only ticker with `CoinIsWalletOnly` before any fee work, and
  `setprice`, `buy` and `sell` behave as R21.10B.9 states. The legacy
  `get_trade_fee`, `max_taker_vol` and `max_maker_vol`, and the swap-fee
  estimates behind order placement for a trading-capable TRON coin, have
  no TRON case: their fee figures come from the EVM gas path, which a
  TRON coin has no node for (R21.10B.9). No TRON swap exists (F21.B1), so
  there is no payment, spend, refund or DEX-fee estimate for a TRC20
  contract call.
- *Balance and activation results.* No balance, activation or address
  method reports or reserves a TRON fee.

Per state: `feat/tron-swap-support (as of 2026-04-23)` and the native
(non-delegated) TRC20 withdraw of `feat/tron-gasfree (as of 2026-05-04)`
have the same energy rule, the same `fee_limit` rule and the same TRX
check as `v3.0.0-beta`; they differ from it only as R21.10B.1 and chapter
63 state. On the fee-delegated route of chapter 63 the sender pays the
provider's fee in the token and no energy figure is computed. `v2.6.0-beta`
has no TRON fee estimate, dry run or `fee_limit` at all (R21.10B.1).

**R21.10B.11** *`fee_limit` and the balance check use different figures
(completes D21.10B.1).* At the three rows that build a TRC20 transfer,
`fee_limit` is the whole dry-run energy times `getEnergyFee`: it is not
reduced by the sender's staked energy, by the contract's percentage or by
the origin limit, and it carries no margin. The TRX check uses `total_fee`
of R21.10B.10, in which the energy part **is** reduced by the sender's
unused staked energy and to which the bandwidth fee is added. The check
is therefore against the full estimate of what the sender would burn if
it paid all the energy, never against a figure reduced for the origin
share, and the sender's TRX balance is not compared with `fee_limit`
itself. Under the public rule above, the caller's share never exceeds the
whole energy of the call, so a `fee_limit` set this way is not made too
low by the origin share; it has no room for a call that uses more energy
at inclusion than in the dry run (D21.10B.1).

**R21.10B.12** *No contract metadata is read.* No path of the reference at
any of the four rows requests `POST /wallet/getcontract`,
`POST /wallet/getcontractinfo` or an equivalent, and no response member
carrying the percentage, the origin limit or a per-contract energy factor
is decoded. The deployer's account resources are not requested either:
`POST /wallet/getaccountresource` is sent for the sender only. The node
endpoints the reference uses are those of §21.9 and R21.10B.7 and nothing
more. The row `feat/tron-swap-support (as of 2026-04-23)` adds documents
only (F21.B4); a research document there lists the contract-information
endpoint among public node endpoints, and no source file at that row uses
it.

*Code-quality finding (informative).* The reference's TRC20 fee is an
upper bound, not an estimate, for a contract with an origin share: the fee
details overstate the cost, and a withdraw the chain would accept can be
refused for lack of TRX (R21.10B.10). A possible remedy, in this chapter's
own terms: read the contract's two settings and the deployer's available
energy once per withdraw; report the caller's expected share as the fee;
keep `fee_limit` at a figure that still covers the whole energy, because
the deployer's energy can be used up between the estimate and inclusion,
in which case the caller pays the remainder; and decide explicitly which
of the two figures the TRX check uses. Whether Reloaded adopts this is the
lane's decision; the reference gives no precedent for either choice.

*Not established (this block).* Nothing was run against a reference
binary or a live node; the receipt members and the charging rule are
taken from the public documentation, not from a measurement of this
round. Whether a node's dry run reports a different `energy_used` when
the deployer's energy is exhausted was not checked. The trade-fee
outcomes of R21.10B.10 were read, not executed.

T21.10B.16 *No contract read (parity).* Against a stub whose token
contract would report a caller percentage of 0 and an ample origin limit,
a TRC20 withdraw at parity sends no contract-metadata request; its
`energy_fee` equals the stub's dry-run `energy_used`, less the sender's
unused staked energy, times the stub's `getEnergyFee`, and the built
transaction's `fee_limit` equals the dry-run `energy_used` times
`getEnergyFee`. Reloaded: the row is restated when the lane decides
whether the origin share is modeled.

T21.10B.17 *TRX check on the full figure (parity).* With the same stub and
a sender whose TRX balance covers the bandwidth fee but is below
`total_fee` of T21.10B.16, the withdraw answers
`NotSufficientPlatformBalanceForFee` with the platform ticker, the TRX
balance and that `total_fee`. Reloaded: as T21.10B.16.

**R21.10B.13** *A reverting dry run is not detected (completes the "Energy
(TRC20)" bullet of R21.10B.7).* At `v3.0.0-beta` the withdraw's dry run of
`transfer(address,uint256)` is judged by two things only: the node-answer
classification of R21.10B.3, and whether `energy_used` is present and
above zero. The revert message, the returned data in `constant_result`
and the execution result of the simulated transaction are not examined
for this call.

- *Revert with the inner success flag true and a positive `energy_used`.*
  Not detected. The withdraw goes on exactly as for a successful dry run:
  the fee details and `fee_limit` are computed from that `energy_used`
  (R21.10B.7, R21.10B.11), the TRX check runs, and the result carries a
  signed transaction and its `tx_hash`. No error is answered. The
  withdraw itself sends nothing to the chain at any row; if the client
  then broadcasts the transaction and the call reverts at inclusion, the
  sender pays for the energy used.
- *Inner success flag false, or absent together with an error code.* The
  answer is an error object and follows R21.10B.3: a transient code moves
  the request to the next node, any other code ends it. The withdraw
  answers `Transport` (HTTP 502) and nothing is signed.
- *`energy_used` missing or zero.* Treated as an unusable answer: the
  request moves to the next node, and when every node answers so the
  withdraw answers `Transport` (HTTP 502) and nothing is signed.
- *Order of checks.* The token balance is read, and an amount above it is
  refused with `NotSufficientBalance`, before the dry run is sent, so the
  commonest cause of a revert does not reach the dry run. Other causes
  (a contract that refuses the sender or the recipient, a paused
  contract) do.
- *Routes and states.* `withdraw` and `task::withdraw::init` take the same
  path; the task reports the same result or error token in its status.
  `feat/tron-swap-support (as of 2026-04-23)` and the native TRC20
  withdraw of `feat/tron-gasfree (as of 2026-05-04)` behave the same.
  `v2.6.0-beta` has no dry run (R21.10B.1).
- *Balance reads.* The reference's `balanceOf` read does not look at the
  revert indication either; it differs only in what it requires of the
  returned data. It takes the first `constant_result` entry of up to 32
  bytes as the number; an empty list or a non-hex entry moves the read to
  the next node, and an entry longer than 32 bytes ends the read as
  failed without trying another node. Revert data of the usual
  error-string form is longer than 32 bytes and so fails the read; a
  shorter returned value would be read as a balance. Reloaded's stricter
  rule for balance reads is R-WP3 and is not changed by this rule.

*Code-quality finding (informative).* The reference can hand the client a
signed TRC20 transfer that its own dry run has already shown to revert,
with a fee estimate presented as if the transfer would succeed
(R21.10B.13). A possible remedy, in this chapter's own terms: treat a dry
run that carries a revert indication -- a revert message beside a true
success flag, or a simulated execution result other than success -- as a
refusal of the withdraw before anything is signed, with a typed error
that names the contract's refusal, and do not move such an answer to
another node, since the outcome belongs to the contract and not to the
node. Which error token Reloaded uses is the lane's decision; the
reference gives no precedent.

*Not established (R21.10B.13).* Which members a TRON node fills for a
reverting constant call (the success flag, the message, the simulated
result, `energy_used`) was taken from the question put to this round and
from public TRON documentation, not measured here.

T21.10B.18 *Reverting dry run (parity).* Against a stub whose
`triggerconstantcontract` answer for `transfer(address,uint256)` has the
inner success flag true, a revert message, error-string data in
`constant_result` and a positive `energy_used`, a TRC20 withdraw at
parity answers a result with `tx_hex`, with `energy_used` equal to the
stub's figure, and the stub receives no broadcast request. With the same
stub answering `energy_used` zero on every node, the withdraw answers
`Transport`, HTTP 502. Reloaded: the first half is restated when the lane
decides the refusal of the finding above.

*Status note (2026-11-20), what Reloaded does for R21.10B.10 -- R21.10B.13.*
The sentences of the block above are unchanged; this note restates its three
acceptance rows for Reloaded.

- *A reverting dry run is refused (T21.10B.18 restated; the remedy of the
  finding after R21.10B.13).* Reloaded reads the dry-run answer of a TRC20
  withdraw for a revert indication, and either of two markers is one: the
  status object has the success flag true **and** a non-empty `message`; or an
  entry of `transaction.ret` names an execution result (`ret`, or
  `contractRet`) other than `SUCCESS`. An empty `constant_result` is no marker
  on its own. A withdraw whose dry run carries the indication answers
  `UnsupportedError`, HTTP 400 on the direct method and in `details` of an
  `Error` status on the task route (chapter 49 R49.27a: the request is the
  client's to change). Its text is "the token contract refused this transfer
  (its dry run reverted): " followed by the node's `message`, or by "simulated
  result " and the result's name when there is no message. The node's text has
  the node's URL removed, as every node text has (R-N1), and is cut to 200
  characters, marked with "...". Nothing is signed, no `tx_hex` is returned,
  and the answer is not moved to another node: the outcome belongs to the
  contract and not to the node, so the node order does not change either. The
  revert is judged before `energy_used`. Against the stub of T21.10B.18
  (success flag true, a revert message, error-string data in
  `constant_result`, a positive `energy_used`) Reloaded therefore answers that
  `UnsupportedError`, the stub receives no broadcast request, and with two
  such nodes one node is asked for the dry run. The second half of the row
  holds as written: `energy_used` zero or missing answers `Transport`, HTTP
  502, and so does an answer whose success flag is not true and that carries
  no usable energy figure. A `message` beside a success flag that is not true
  is no revert marker; such an answer is judged by R21.10B.3 and by its
  `transaction.ret` entries. Token balance reads are not changed (R-WP3).
- *Answer forms (public chain data, chapter 01 R6).* On 2026-11-20 a public
  mainnet node answered the dry run of a USDT `transfer(address,uint256)`
  that succeeds with the status object `{"result": true}` and no `message`,
  one 32-byte word in `constant_result`, `transaction.ret` equal to `[{}]`,
  a `logs` list and integer `energy_used` and `energy_penalty`. For a
  transfer the token refuses (a sender without a token balance; an amount
  above the balance) it answered with the status object `{"result": true,
  "message": "REVERT opcode executed"}`, the message as plain text,
  `constant_result` equal to `[""]`, `transaction.ret` equal to
  `[{"ret": "FAILED"}]`, no `logs`, and positive figures: `energy_used`
  8,624 and `energy_penalty` 6,640. This settles, for that node, the
  members the paragraph "Not established (R21.10B.13)" leaves open.
- *The origin share is not modeled (T21.10B.16 and T21.10B.17 restated as
  parity; the maintainer's decision of 2026-11-20).* Reloaded keeps R21.10B.10
  -- R21.10B.12: no contract metadata and no resources of the deployer are
  read, the whole dry-run energy counts as the sender's, and the TRX check is
  against the full `total_fee`. The estimate and the amount the check asks for
  can therefore be above what the chain charges when the contract's deployer
  pays part of the energy. For mainnet USDT the effect was zero on 2026-11-20:
  a public node reported the deployer's energy as fully used (9,335 of 9,335),
  and ordinary transfers that day showed no origin share in their receipts.
  T21.10B.16 holds for Reloaded with one difference that predates this note:
  the `fee_limit` is the dry-run energy times `getEnergyFee` plus the margin
  of D21.10B.1. T21.10B.17 holds as written.
- *Tests.* `tron/withdraw_fee_tests.rs` (the refusal with each marker, one
  node asked, an unchanged fee for a successful dry run in the node's full
  form, zero energy, and the two parity rows), `tron/node_client_tests.rs`
  and the unit tests of `tron/api.rs` and `tron/withdraw.rs`.

**R21.10B.8** *Address methods (completes R-WP5 and R-AD1).*

- `validateaddress` accepts the base58check form (34 characters starting
  with `T`, checksum checked) and the hex wire form (42 hex digits
  starting `41`, or 44 starting `0x41`); everything else, the EVM `0x`
  20-byte form included, answers `is_valid: false` with a `reason`.
- `convertaddress` is the EVM method unchanged: `to_address_format` takes
  the EVM formats only, `from` must be an EVM `0x` address (a base58check
  or `41…` value is refused with the legacy error body), and the answer is
  the EVM mixed-case `0x` form. D21.10B.2.
- `get_my_address` refuses TRX and TRC20 with `CoinIsNotSupported`
  (HTTP 400); a coin config without `mm2` answers `CoinsConfCheckError`
  (HTTP 400) first.
- `show_priv_key` answers the single key or the activated HD key as
  `0x` followed by 64 hex digits, the EVM form; hardware, WalletConnect and
  MetaMask keys are refused with the legacy error body.
- The withdraw destination and `my_balance` follow R-WP5: both address
  forms in, base58check out.

*Status note (2026-11-17), what Reloaded does for each method of
R21.10B.8.* `validateaddress` is as the bullet states: base58check, `41...` (42)
and `0x41...` (44) are valid, the EVM `0x` 20-byte form is
`is_valid: false` with a `reason`. `convertaddress` differs from the first
bullet as decided in D21.10B.2: it converts between the TRON forms and
refuses the EVM form (see the note at D21.10B.2); for an EVM coin it is
unchanged. `get_my_address` refuses TRX and TRC20 with `CoinIsNotSupported`
(HTTP 400); the general check of the coin config runs first, and a missing
coin or a protocol that does not parse answers `CoinsConfCheckError`
(HTTP 400); the extra "no `mm2` member" case of the bullet is not checked by
this method for any coin. `show_priv_key` answers the local key as `0x` and
64 hex digits for a TRON coin as for an EVM coin; a key the node does not
hold (Trezor, MetaMask) is refused with the legacy error body, and a
WalletConnect key cannot occur because activation refuses that policy
(R21.10B.4).

**R21.10B.9** *Orders (completes R-D1 and the order-placement bullet of
F21.B3).* `setprice`, `buy` and `sell` have no TRON-specific check. With
the coin configured `"wallet_only": true` -- every TRON config of the
reference -- each answers the legacy error body (chapter 55 R55.1.10,
HTTP 500) from the wallet-only check that runs before any balance or fee
work, naming the wallet-only ticker; no order is created. A TRON coin
activated as trading-capable passes that check and then fails the
balance-and-fee check, because the swap fee is computed through the EVM
gas path and the coin has no EVM node; the answer is the same legacy
error body and no order is created. The row `v2.6.0-beta` has the same
wallet-only check.

*Status note (2026-11-20), how Reloaded keeps a TRON coin wallet-only
(clean-side, not corpus-sourced).* Reloaded has no TRON swap (R-D1), so
while the build constant `TRON_SWAPS_AVAILABLE` is `false` a TRX or
TRC20 coin reports itself wallet-only whatever its coins-config entry
says. For a coin configured `"wallet_only": true` the answer of
`setprice`, `buy` and `sell` is the one R21.10B.9 states: the legacy
error body, HTTP 500, from the wallet-only check, naming the ticker, and
no order is created (T21.10B.15); this is parity. For a TRON coin whose
config is trading-capable Reloaded diverges in the check that refuses:
the order is refused by the wallet-only check, where the reference
passes that check and refuses at the balance-and-fee check. The answer
has the same form (the legacy error body, HTTP 500, no order created);
its text names the wallet-only ticker. No compatibility switch is
offered: the reference's path ends in a refusal too, and a switch could
only let a later, less clear check refuse the same order. The v2
`start_swap` front of the three methods refuses the same way. A TRON
coin also declines every match (its answer to the protocol-support
question of order matching is no), and an order saved earlier that names
a coin whose coins-config protocol is `TRX` or `TRC20` is cancelled at
startup with a log line, as a saved Tendermint order is (Chapter 18,
availability note). The read-only views (`orderbook`, `best_orders`,
`trade_preimage`) read the coins-config `wallet_only` flag alone and are
unchanged, and so are `max_taker_vol`, `max_maker_vol` and the legacy
`get_trade_fee`, which do not ask whether a coin is wallet-only.

*Status note (2026-11-22), a saved TRON order is kept, not cancelled
(clean-side, not corpus-sourced).* From this date an order saved earlier
that names a coin whose coins-config protocol is `TRX` or `TRC20` is no
longer cancelled at startup. It is kept, in memory and in the storage,
so `my_orders` lists it and `cancel_order` removes it, but it is never
announced and never matched, and one log line names it. The rule, which
is the same for every coin that cannot trade, is in the status note of
2026-11-22 at [Chapter 60](60-trading-floors-and-refusal-contract.md)
R60.6.5. Where the status notes of 2026-11-20 in this chapter (the one
before §21.0, the one above, and the one at R21.B3) say that such an
order is cancelled at startup, read them with this note. Nothing else in
them changes.

*Status note (2026-11-22), EVM-family methods called on a TRON coin (Spec
Reader round 0.5.0-22; passed the KDF Dirty Gate).* What the reference answers
when `get_raw_transaction`, `sign_raw_transaction`, the legacy `get_trade_fee`,
`trade_preimage` for a coin that is not wallet-only,
`get_eth_estimated_fee_per_gas`, `stream::fee_estimator::enable` and
`get_token_info` are called for a TRX or TRC20 coin is stated in R21.10B.14 --
R21.10B.21, in the block "EVM-family methods called on a TRX or TRC20 coin" at
the end of this section. The sentences of R21.10B.9 and R21.10B.10 are
unchanged.

**D21.10B.1** *TRC20 `fee_limit` policy.* The reference sets `fee_limit` to
the dry-run energy times the energy price with no margin; Reloaded today
sets a fixed cap (compatibility note). The cap is not visible to a peer.
Options: (a) the reference's figure; (b) the estimate plus a stated margin,
bounded by a stated maximum; (c) a fixed cap. The choice also fixes what
the fee details report as the most the transfer can cost (D21.B5 of
§21.11B.4 states the charging semantics).

*Status note (2026-11-19), D21.10B.1 decided: (b):* the estimate plus 20%,
rounded up to whole SUN, at most 100 TRX; an estimate above the maximum is
refused before broadcast. The estimate is the dry-run energy times
`getEnergyFee`, counting the whole energy whether the sender's staked energy
covers it or not, because the limit bounds both (D21.B5). The `fee_limit` of
a TRC20 transfer is that estimate plus a fifth of it, rounded up to whole
SUN, and never above 100 000 000 SUN (100 TRX). When the estimate itself is
above that maximum the withdraw is refused with `UnsupportedError` (HTTP
400), whose text names the estimate and the maximum; nothing is signed, so
nothing can be broadcast. A limit below the need is never set: by the
charging semantics of D21.B5 such a call would fail on chain and still be
charged. The `fee_limit` is the most the contract call of a TRC20 transfer
can cost the sender; bandwidth, when it is burned, is charged beside it. The
fee details keep `total_fee` as the expected charge (the energy shortfall and
the bandwidth of R21.10B.7) and carry no `fee_limit` member. The margin and
the maximum are Reloaded's figures: the reference sets no margin, and
neither figure is visible to a peer.

**D21.10B.2** *`convertaddress` for a TRON coin.* The reference answers an
EVM `0x` form for a TRON coin. Options: (a) parity; (b) refuse the method
for TRX and TRC20; (c) convert between the base58check and the `41…` hex
form under a TRON-specific format value.

*Status note (2026-11-17), D21.10B.2 decided: (c).* For a TRX or TRC20 coin,
`convertaddress` converts between the base58check form and the `41...` hex
form. `to_address_format` takes `{"format": "base58"}` (the base58check form,
34 characters starting with `T`) or `{"format": "hex"}` (42 hex digits, the
prefix `41` and the 20-byte payload, lowercase, no `0x`). `from` is any input
form of R-AD1: base58check, `41...` hex, or `0x41...` hex. The EVM `0x`
20-byte form as `from`, and the EVM format values (`mixedcase`, `singlecase`)
or any other format value, are refused with the legacy error body (HTTP 400,
`InvalidParam`). The two TRON format values are accepted for TRON coins only:
an EVM coin keeps `mixedcase` and `singlecase` and refuses `base58` and `hex`.

**D21.10B.3** *Key policies that activate but cannot send.* At the
reference a browser build activates TRON under MetaMask and shows an
address that withdraw then refuses. Options: (a) parity; (b) refuse at
activation every key policy the TRON withdraw refuses (R21.10B.4), with
the refusal token of the route.

*Status note (2026-11-17), D21.10B.3 decided: (b).* At activation, both
routes refuse every key policy the TRON withdraw refuses, with the refusal
token of the route: the Trezor policy (`UnexpectedDeviceActivationPolicy` on the
one-shot route, `Internal` in the task status, before any device exchange) and
the WalletConnect policy (`Internal`), as R21.10B.4 states for the reference,
and, beyond the reference, the MetaMask `priv_key_policy` and the MetaMask
`rpc_mode` (`Internal`, with a text naming the reason). MetaMask exists on the
browser build only, so the refusal is tested on the policy facts it reads. The
bullet "MetaMask (browser builds only)" of R21.10B.4, which accepts the MetaMask
policy with the default `rpc_mode`, describes the reference and is not Reloaded's
rule.

*Status note (2026-11-17), HD interim rule (maintainer decision across the
coin families).* Under a global-HD login a TRON activation that asks for
`IguanaPrivKey` is honored: the coin uses the node's internal key
(`mm2_internal_privkey_secret`), the key the UTXO family uses for that policy
under an HD login, on both routes, and is a single-address coin. An absent
policy, `ContextPrivKey` and any HD-wallet policy under a global-HD login stay
refused with `PrivKeyPolicyNotAllowed` until the TRON HD wallet (R21.10B.4,
R-A2) is built. Under an iguana login nothing changes.

*Status note (2026-11-19), HD interim rule retired.* The interim refusal of
the note above is gone: under a global-HD login an absent policy,
`ContextPrivKey` and the HD-wallet forms build the TRON HD wallet (R21.10B.4,
R-A2) instead of answering `PrivKeyPolicyNotAllowed`. `IguanaPrivKey` is
unchanged: it still uses the node's internal key
(`mm2_internal_privkey_secret`) and is a single-address coin, on both routes.
The Trezor, WalletConnect and MetaMask refusals are unchanged under a
global-HD login. The HD methods (`get_new_address`, `account_balance` and the
task forms of `scan_for_new_addresses` and `account_balance`) answer for a TRX
or TRC20 ticker with base58check addresses
and a balance map: over TRX and the enabled TRC20 tokens for the TRX ticker,
over that token only for a TRC20 ticker, which is served by the HD wallet of
its platform. For either ticker an address counts as used when the chain has
created its account or when any enabled TRC20 token has a non-zero balance at
it (the Reloaded note of 2026-11-19 after R21.10B.4, which records how this
differs for a TRC20 ticker). This repository has no
`task::get_new_address::init` for any coin, so that task form of R21.10B.4 is
not served for TRON either. `task::create_new_account::init` ends with
`Internal` in the task status for every TRON coin, before any account is
created. `withdraw` and
`task::withdraw::init` of a TRX or TRC20 coin take the HD `from` forms of the
EVM withdraw (an address id or a derivation path, resolved in the HD wallet of
the platform) and sign with the key of that path. For a TRC20 coin this is a
Reloaded extension: any activated address of the platform's HD wallet can be
named, whereas an EVM ERC-20 coin in Reloaded refuses `from`; without `from` the sender is
the activated address; a single-key TRON coin refuses `from` with
`UnexpectedFromAddress`. `show_priv_key` answers the key of the activated
address (R21.10B.8).

*Status note (2026-11-20), ERC-20 `from`.* The clause above that an EVM ERC-20
coin in Reloaded refuses `from` describes the code before PR #438. Since then an
ERC-20 withdraw under a global-HD login resolves `from` in the platform's HD
wallet as well (chapter 49 R49.34 and its as-built status note); the TRC20 rule
above is unchanged.

> **Compatibility (this repository on dev, 2026-11-17; its own code, not
> the reference).** TRX and TRC20 activate only through legacy `enable`
> and `electrum`, behind the availability constant (D55.5); the request
> reads `urls`, not `nodes`, so `komodo_proxy` has no counterpart, and any
> parseable URI is accepted. The node list keeps the given order (no
> shuffle); the 10-second bound and the rotation to the head agree.
> Activation takes the node's internal key for every login, so under a
> global-HD login the TRON address is not derived under the coin's
> `derivation_path` and differs from the reference's address for the same
> seed; R-A2 is not met on that route today. A TRC20 config without
> `decimals` is refused (R-A1 step 4). Reloaded has no TRON history source
> either; its `convertaddress` is the same EVM method, and its order
> creation has a wallet-only check of its own. The withdraw
> estimate uses fixed bandwidth figures, counts an account-creation cost
> for a TRC20 recipient too, and is best-effort (it is left out when a
> node read fails); `max: true` for TRX sends the whole balance without
> deducting the fee, and a TRX amount plus fee above the balance is not
> refused before building; the TRC20 `fee_limit` is a fixed cap.

*Status note (2026-11-19), compatibility block.* On the mmrpc 2.0 route R-A2
is now met: under a global-HD login the TRON address is derived under the
coin's `derivation_path`, as R-A2 and R21.10B.4 require.
The legacy `enable` and `electrum` routes keep the node's internal key under a
global-HD login, as the EVM and UTXO legacy routes do in Reloaded (decided
2026-11-19), so their TRON address differs from the address of the mmrpc 2.0
route for the same seed; the reference does not serve TRON on those routes.

*Status note (2026-11-19), withdraw fees of the compatibility block.* The
withdraw defects the block lists are fixed. The estimate uses the real size
of the transaction, counts an account-creation cost for a TRX recipient
only, and is no longer best-effort: a failed node read fails the withdraw.
`max: true` for TRX deducts the fee, and a TRX amount plus fee above the
balance is refused before building. The TRC20 `fee_limit` follows D21.10B.1
and is no longer a fixed cap. The status notes after R21.10B.7 and at
D21.10B.1 state the rules.

*Status note (2026-11-20), compatibility block.* The sentence "Reloaded has no
TRON history source either" of the block no longer holds for a coin with a
TronGrid API key and `tx_history: true`: §21.10C. Without them the answers are
those of R21.10B.6.

*Not established.* (1) Nothing was measured against a running reference
binary or a live node. (2) The exact text the legacy error body carries in
R21.10B.8 and R21.10B.9 is deliberately not recorded. (3) Whether the TRON
node's energy dry run includes the dynamic-energy penalty in
`energy_used` was not checked. (4) The swap-fee failure of R21.10B.9 for a
trading-capable coin was read, not executed.

*Lab note (2026-11-20), on item (3).* On 2026-11-20 a public mainnet node's
dry run of a USDT `transfer(address,uint256)` to an address that holds the
token reported `energy_used` 64,285 with `energy_penalty` 49,635, equal to
the receipts of real transfers of that kind: the dry run includes the
dynamic-energy penalty. To an address without a token balance it reported
130,285; no real receipt of that kind was compared. Public chain data
(chapter 01 R6); the sentence of item (3) is unchanged.

*Acceptance rows.* Each row runs against a stub TRON node that answers the
public endpoints of §21.9 with the forms the row names. *Parity* is the
reference behavior above; where Reloaded keeps a different rule the row
names it.

T21.10B.1 *Activation result.* A one-shot activation of a TRX platform
with one TRC20 token, single key, answers a result keyed by the
base58check address, the stub's head block as `current_block`, the TRX
balance scaled by 6 decimals and the token balance by the token's
decimals.

T21.10B.2 *NFT request.* The same request with `nft_req` answers
`Internal`, HTTP 500, and leaves TRX and the token inactive.

T21.10B.3 *Token without decimals.* A token config without `decimals`:
parity activates the token with the decimals the stub's `decimals()` answer
gives, and a stub that fails that read gives `Transport`, HTTP 502.
Reloaded, while R-A1 step 4 stands: refused.

T21.10B.4 *Node list.* An empty `nodes` answers `AtLeastOneNodeRequired`
and a `ws` URL answers `InvalidPayload`, both HTTP 400, with no request to
any node.

T21.10B.5 *Moving answers.* With two stubs, the first answering in turn
HTTP 429, HTTP 503, a non-JSON 2xx body, a 2xx body of the wrong shape and
a 2xx error object with a transient TRON code, each balance read succeeds
from the second stub, and the next read starts at the second stub.

T21.10B.6 *Final answer.* With two stubs, the first answering a 2xx error
object with a permanent TRON code, the read fails and the second stub
receives no request (parity; R-WP2 governs Reloaded for the `Error` form).

T21.10B.7 *Time bound.* A first stub that never answers is abandoned
after 10 seconds and the second stub serves the read.

T21.10B.8 *Global HD.* Under a global-HD login with a config whose
`derivation_path` names coin type 195, the activated address equals the
base58check address derived at `m/44'/195'/0'/0/0` from the same seed;
`get_new_address` and `account_balance` answer base58check addresses;
`task::create_new_account::init` ends with `Internal`.

T21.10B.9 *Key policies.* `enable_eth_with_tokens` under the Trezor policy
answers `UnexpectedDeviceActivationPolicy`, HTTP 400; under WalletConnect
it answers `Internal`, HTTP 500.

T21.10B.10 *History.* The versioned `my_tx_history` for TRX answers
`NotSupportedFor`, HTTP 400; the legacy method answers an empty list with
`sync_status` `NotEnabled`.

T21.10B.11 *New recipient.* A TRX withdraw to an address the stub reports
as absent carries `account_creation_fee` equal to the stub's
`getCreateNewAccountFeeInSystemContract` and a `total_fee` that includes
it; a TRC20 withdraw to the same address carries no such member.

T21.10B.12 *TRX max.* With a balance above the fee, `max: true` sends the
balance less `total_fee`; with a balance not above the fee it answers
`AmountTooLow`; with a zero balance `ZeroBalanceToWithdrawMax`.

T21.10B.13 *TRC20 fee from TRX.* A TRC20 withdraw whose TRX balance is
below the estimated fee answers `NotSufficientPlatformBalanceForFee` with
the platform ticker; with `max: true` and enough TRX it sends the whole
token balance.

T21.10B.14 *Address methods.* `validateaddress` accepts a base58check, a
`41…` and a `0x41…` form of one address and refuses its EVM `0x` form;
`get_my_address` for TRX answers `CoinIsNotSupported`, HTTP 400.

T21.10B.15 *Orders.* With TRX configured wallet-only, `setprice`, `buy`
and `sell` against any active coin each answer the legacy error body and
`my_orders` stays empty.

### 21.10B-E EVM-family methods called on a TRX or TRC20 coin (added 2026-11-22)

> **Clearance (chapter 01 R40) -- passed the KDF Dirty Gate on 2026-11-22,
> measure 0.03 (attestation in §21.13).** This block was written on 2026-11-22
> by the KDF Spec Reader (Spec Reader round 0.5.0-22) under the two-team
> procedure of chapter 01 §1.4A, from the reference lineage at the rows
> `v3.0.0-beta`, `v2.6.0-beta`, `feat/tron-swap-support (as of 2026-04-23)` and
> `feat/tron-gasfree (as of 2026-05-04)` of chapter 02 §2.15A (read read-only;
> no untabled state was read) and this repository's own chapters. It edits no
> sentence of §21.0 -- §21.11B. It describes the reference; what Reloaded
> answers on these methods is for the lane's own notes. Numbering: R21.10B.14
> -- R21.10B.21, T21.10B.19 -- T21.10B.22.

**R21.10B.14** *Common ground and reference states (completes R21.10B.1
and the "Trade-fee and swap-fee methods" bullet of R21.10B.10).* At the
rows `v3.0.0-beta`, `feat/tron-swap-support (as of 2026-04-23)` and
`feat/tron-gasfree (as of 2026-05-04)` each method below is the EVM
family's own method. A TRX or TRC20 coin reaches it because it has the EVM
coin type; none of the methods has a TRON case, and none refuses a coin for
being TRON. A TRON coin is activated with no EVM node connection at all:
its configured nodes are served by the TRON HTTP client only (R21.10B.3),
and none of these methods uses that client. Every step that would ask an
EVM node therefore fails inside the process, at once, and **no request
reaches any node**. The two exceptions are named where they occur: the
gas-fee provider of R21.10B.19, which is not a node of the coin, and the
balance read of R21.10B.18. The three rows agree on every rule below; the
two branch rows change nothing in these methods. TRX and TRC20 are
answered alike unless a rule says otherwise, and a ticker that is not
active is answered as for every other coin.

The row `v2.6.0-beta` has no TRON behavior here either (R21.10B.1): a `TRX`
platform is an ordinary EVM coin with EVM JSON-RPC over the given nodes,
and there is no `TRC20` case. Every method below then takes the ordinary
EVM path and **does** send EVM JSON-RPC requests to the configured nodes;
the answer is what the EVM rules make of those nodes' replies. One outcome
is fixed at that row: `sign_raw_transaction` is refused with
`TransactionError` (HTTP 400) after the account nonce was read, because
the coin has no EVM chain identifier to sign for.

**R21.10B.15** *`get_raw_transaction`.* A `tx_hash` that is not 64 hex
digits, with or without `0x`, answers `InvalidHashError` (HTTP 400). Any
other request answers `Transport` (HTTP 502), with no request to any node.
The method never answers `HashNotExist` and never succeeds for a TRON
coin. A TRON transaction id has the accepted form, so the id of a real
TRON transaction gets `Transport`.

**R21.10B.16** *`sign_raw_transaction`.* The method never returns a signed
transaction for a TRON coin and asks no node.

- A request whose `type` is `UTXO` answers `InvalidParam` (HTTP 400).
- With `type` `ETH`, the members are read as for an EVM coin: `to` must
  be an EVM hex address, so a base58check address answers `InvalidParam`
  (HTTP 400); `data` must be hex.
- Under a single key, a request without `pay_for_gas`, or one whose
  `pay_for_gas` names a fee policy instead of prices, fails at the
  gas-price read with `InternalError` (HTTP 500) (but see the last
  paragraph of R21.10B.19).
- Under a single key, a request whose `pay_for_gas` gives the prices
  goes on to the account nonce, which is asked of an empty set of nodes
  in six rounds one second apart; the answer is `TransactionError`
  (HTTP 400), about five seconds after the request.
- A key the node does not hold is refused with `InvalidParam` (HTTP 400)
  as for every EVM coin.

**R21.10B.17** *Legacy `get_trade_fee` (completes R21.10B.10).* The answer
is the legacy error body (chapter 55 R55.1.10, HTTP 500), produced by the
failed gas-price read; no node is asked and no fee figure is returned. A
TRC20 ticker is answered the same way, through the fee policy of its
platform.

**R21.10B.18** *`trade_preimage` (completes R21.10B.9 and R21.10B.10).*

- *Wallet-only config.* `CoinIsWalletOnly` (HTTP 400) naming the ticker,
  before either coin is looked up (R21.10B.10).
- *Trading-capable config.* There is no TRON check. Every fee estimate
  of a TRON coin fails at the gas-price read, and the method answers
  `InternalError` (HTTP 500).
- *`setprice` with the TRON coin as `rel`.* The sender fee of the `base`
  coin is estimated first, and may ask that coin's own nodes; the
  receiver fee of the TRON coin then fails as stated.
- *`buy` and `sell` with the TRON coin as the coin received* (`base` of
  `buy`, `rel` of `sell`). The two estimates of the paying coin run first;
  the receiver fee of the TRON coin then fails as stated.
- *The TRON coin as the paying coin* (`base` of `setprice` and `sell`,
  `rel` of `buy`). The same `InternalError`, at the TRON coin's own first
  estimate. With `setprice` and `max: true` the TRON balance is read
  first, through the TRON client; this is the one TRON node request in
  this block.

No order is created and no TRON fee figure is returned in any case.

**R21.10B.19** *`get_eth_estimated_fee_per_gas`.*

- `estimator_type` `Simple`: `Internal` (HTTP 500); no request.
- `estimator_type` `Provider` on a node whose configuration has no
  `gas_api` member: as `Simple`.
- `estimator_type` `Provider` with `gas_api` configured: one request goes
  to the configured gas-fee provider. That service is not a node of the
  coin, and the figures asked of it are those of the Ethereum main chain
  whatever coin the request names. When it answers, the method
  **succeeds** in its ordinary shape (the members of chapter 10 R36, with
  the provider as `source`): a TRON ticker is answered with Ethereum's
  fees. When it fails, the answer is `Internal` (HTTP 500).

The same estimate is behind every fee read of R21.10B.16 -- R21.10B.18 when
the platform's swap fee policy is not the legacy one (the activation member
`swap_gas_fee_policy` of R21.10B.2). With the default legacy policy those
reads fail as stated. With another policy and `gas_api` configured they
obtain the provider's Ethereum figures instead of failing, and the method
goes on with them.

**R21.10B.20** *`stream::fee_estimator::enable`.* The request is **not**
refused: it succeeds with `streamer_id` `FEE_ESTIMATION:<ticker>` (chapter
10 R38), the periodic estimator starts, and no node is asked at
activation. Each cycle then runs the estimate of R21.10B.19 for the
configured `estimator_type`: with `Simple`, the default, every cycle emits
an error event and never a fee event; with `Provider` and `gas_api`
configured the events carry the provider's Ethereum figures. The stream
runs until it is disabled or the coin is deactivated.

**R21.10B.21** *`get_token_info` for a token protocol on a TRON platform.*
The rule is the same for an `ERC20` protocol whose `platform` is the TRX
ticker and for a `TRC20` protocol.

- The platform is not active: `NoSuchCoin` (HTTP 404).
- `contract_address` is read as an EVM address: a base58check value, or
  either hex wire form of R-AD1, answers `InvalidRequest` (HTTP 400).
- A `contract_address` in the EVM `0x` 20-byte form that passes the
  method's address check: `RetrieveInfoError` (HTTP 500), at the symbol
  read; no node is asked.

The method never succeeds on a TRON platform, so a TRON token's symbol
and decimals cannot be read through it at any of the three rows, although
activation reads the token's `decimals()` through the TRON client
(R21.10B.2).

*Code-quality finding (informative; apparent from the reading, no audit
was made).* None of these methods tells the caller that the coin is not
supported. They answer with the transport and internal errors of a node
outage: `get_raw_transaction` answers the HTTP 502 a client would retry,
and `sign_raw_transaction` can hold the request for about five seconds
first. The fee stream is accepted and then reports an error every cycle for
as long as it runs. With a gas-fee provider configured, the fee estimate
and the stream report Ethereum's fees under a TRON ticker, and no member of
the answer says so. A possible remedy, in this chapter's own terms: refuse
each of these methods for a TRX or TRC20 coin with a typed "not supported
for this coin" answer before any other work, until the method has a TRON
case.

*Not established (R21.10B.14 -- R21.10B.21).* (1) Every outcome was read,
none was executed against a reference binary. (2) At `v2.6.0-beta`, what
the given nodes answer to the EVM JSON-RPC requests. (3)
`sign_raw_transaction` under a global-HD login. (4) The form the legacy
`trade_preimage` gives these answers. (5) How far each method of R21.10B.16
-- R21.10B.18 gets once it holds the provider's figures. (6) The text of
the legacy error body of R21.10B.17 is deliberately not recorded.

T21.10B.19 *No node is asked (parity).* With TRX and one TRC20 token
active under a single key against a stub TRON node that counts its
requests: `get_raw_transaction` with a 64-digit hex id answers
`Transport`, HTTP 502; `get_token_info` with an `ERC20` protocol on the
TRX platform and an EVM-form contract address answers `RetrieveInfoError`,
HTTP 500, and with a base58check address `InvalidRequest`, HTTP 400;
`get_eth_estimated_fee_per_gas` with `Simple` answers `Internal`, HTTP
500; the legacy `get_trade_fee` answers the legacy error body, HTTP 500.
The stub's count does not change.

T21.10B.20 *Raw signing (parity).* In the setup of T21.10B.19,
`sign_raw_transaction` with `type` `ETH` and no `pay_for_gas` answers
`InternalError`, HTTP 500; with explicit prices it answers
`TransactionError`, HTTP 400, no sooner than five seconds later; with a
base58check `to` it answers `InvalidParam`, HTTP 400. The stub's count does
not change and no answer carries a transaction.

T21.10B.21 *Trade preimage (parity).* With TRX configured trading-capable
and a second active coin, `trade_preimage` answers `InternalError`, HTTP
500, for `setprice` with TRX as `rel`, for `buy` with TRX as `base` and for
`sell` with TRX as `rel`; the stub TRON node receives no request. With TRX
configured wallet-only each answers `CoinIsWalletOnly`, HTTP 400.

T21.10B.22 *Fee stream (parity).* `stream::fee_estimator::enable` for TRX
with an empty `config` succeeds with `streamer_id` `FEE_ESTIMATION:TRX`,
and the first event on the stream is an error event; the stub TRON node
receives no request.

A row marked *parity* states the reference. Where Reloaded refuses one of
these methods for a TRON coin earlier and with another token, the lane's
note names the token and the row is restated there.

## 21.10C Transaction history (Reloaded-original, added 2026-11-20)

Reloaded-original rules for the transaction history of TRX and TRC20 coins. The
reference has no TRON history (R21.10B.6); nothing in this section was taken
from it. The request and answer shapes of the two account endpoints are those
of TronGrid's public API documentation (TRON Developer Hub,
developers.tron.network, the pages *Get transaction info by account address*
and *Get TRC20 transaction info by account address*, read on 2026-11-20): the
parameters `only_confirmed`, `limit` (at most 200), `fingerprint` and, on the
TRC20 endpoint only, `contract_address`, and the answer members `data`,
`success` and `meta.fingerprint`, which is present only when a further page
exists. The receipt and
chain-parameter shapes are those of the public TRON full-node HTTP API (§21.9).
The figures the tests check are public Nile test-network data captured on
2026-11-20 (chapter 01 R6).

**R21.10C.1 (when the history runs).** The history of a TRON coin is enabled
only when both hold: the activation request asks `tx_history: true`, and the
coin's node list has a *keyed TronGrid node*, that is, a node whose URL is
`https` on one of the TronGrid hosts of R-N9 (exact host match) and whose
requests carry the `TRON-PRO-API-KEY` header. The header comes from the node's
own `headers` entry or, failing that, from the `MM2.json` key `tron_api_key`,
in the order R-N9 states. This holds on both activation routes (the mmrpc 2.0
route and the legacy `enable` / `electrum` route). Honoring `tx_history` is a
divergence from R21.10B.2, where the reference ignores the member for TRON; it
is additive, not visible to a peer, and has no switch.

**R21.10C.2 (without a key).** A coin whose history is not enabled keeps the
reference behavior of R21.10B.6: the versioned `my_tx_history` answers
`NotSupportedFor` (HTTP 400), the legacy `my_tx_history` answers an empty
`transactions` list (unless rows were stored earlier), `current_block` from the
TRON node and `sync_status` `{"state":"NotEnabled"}`, and no account listing is
ever asked. A key header on a host that is not a TronGrid host, and a TronGrid
host given as `http`, do not enable the history.

**R21.10C.3 (source and hosts).** The rows come from two listings per address,
asked by GET of keyed TronGrid nodes only, never of another node of the list:

| Request | Use |
| --- | --- |
| `GET /v1/accounts/{address}/transactions?only_confirmed=true&limit=200` | TRX rows |
| `GET /v1/accounts/{address}/transactions/trc20?only_confirmed=true&limit=200` (with `contract_address` when one token is served) | TRC20 rows |
| `POST /wallet/gettransactioninfobyid` (any configured node) | block height and fee of a TRC20 row, once per new transaction |
| `POST /wallet/getchainparameters` (any configured node) | the bandwidth price, once per pass |

`{address}` is the base58check form. The GET requests follow R-N3 to R-N8 as
the POST requests do: the per-attempt cutoff, failover on every node failure of
R-N4 (a listing not marked `success: true` or without a `data` array is a bad
answer), the order rotation of R-N5, no redirect, and the one all-failed error
of R-N8. A rate-limit answer (HTTP 429) and a refusal of the key (HTTP 403) are
non-2xx node failures like any other: the next keyed node is asked, and when
none answers the pass fails and is tried again after the pause of R21.10C.9,
not sooner. The next page is asked with the same parameters and the
`meta.fingerprint` value as the `fingerprint` parameter; a page without
`meta.fingerprint` is the last one. The link the provider sends beside it is
not followed,
and a cursor that is not made of letters, digits, `-` and `_` ends the walk
with an error. The API key is a secret under R-N9: no log line, error text,
RPC answer or history state shows it or any part of a node URL beyond the host.

**R21.10C.4 (addresses).** Every address a row names is in base58check. The
history covers every *known* address of the wallet: the enabled address of a
single-key coin, and for an HD wallet the known (activated) external addresses
of every account, read again on each pass, so an address added by
`get_new_address` or a scan is covered from the next pass. The rows of all
addresses form one history. A transaction that two own addresses list is one
row. When the wallet comes to know a further address (and at the first pass
after a start), `spent_by_me`, `received_by_me` and `my_balance_change` of the
stored transfer rows are recomputed from the rows' own members by the rules of
R21.10C.5 and R21.10C.7, so a send to an address that became known later reads
as a transfer between own addresses; fee-only rows are left as they are. A TRC20 ticker enabled on a TRX platform is served by the platform's
wallet and its addresses.

**R21.10C.5 (TRX rows).** For each row of the transaction listing whose first
contract is a successful `TransferContract` from or to an own address:
`from` is `owner_address`, `to` is `to_address`, `total_amount` is `amount` in
TRX, `received_by_me` is the amount when the receiver is an own address, and
`spent_by_me` is the amount plus the fee when the sender is an own address
(as R21.10B.7 states for a withdraw). `block_height` is `blockNumber`,
`timestamp` is `block_timestamp` in seconds, `tx_hash` is `txID` (64 lowercase
hex digits), `internal_id` is the transaction id, `transaction_type` is
`StandardTransfer`, and `tx_hex` is the signed protobuf transaction assembled
from `raw_data_hex` and `signature`. A row without a member named here is left
out and logged; no member is ever filled with a stand-in value.

**R21.10C.6 (fee-only rows).** Any other transaction of the listing that an
own address sent and paid a fee for is a row of the TRX coin that carries the
fee only: `total_amount` 0, `spent_by_me` the fee, `to` the contract called
(when the transaction names one). Its `transaction_type` is `FeeForTokenTx`
when it is a TRC20 `transfer(address,uint256)` call and `StandardTransfer`
otherwise. The token amount of such a call is a row of the token's own history
(R21.10C.7), so nothing is counted twice. A transaction that changes no TRX
balance of the wallet (no fee paid by an own address, no transfer to or from
one) is not a row.

**R21.10C.7 (TRC20 rows).** Each `Transfer` event of the token listing whose
token is an enabled TRC20 coin is a row of that coin: `from`, `to`,
`total_amount` (`value` scaled by the coin's decimals), `spent_by_me` the
amount when the sender is an own address, `received_by_me` the amount when the
receiver is one; the fee is paid in TRX and is never part of `spent_by_me`.
`timestamp` is the event's `block_timestamp` in seconds. The listing carries
neither a block number nor a fee: both come from `gettransactioninfobyid`,
read once for a new transaction and stored with the row. A row whose receipt no
node has yet is not stored and is asked again on a later pass. `tx_hex` is
empty, because neither source carries the raw transaction. `internal_id` is a
hash over the transaction id, the token contract, the two addresses, the value
and the position among equal events of that token in the transaction, since one
transaction can carry several transfer events, of one token or of several. The
id of an event is therefore the same whether the listing was filtered by the
token contract or not. `transaction_type` is `StandardTransfer`.

**R21.10C.8 (fee details).** `fee_details` has the shape of the withdraw result
(R21.10B.7), with `coin` the platform ticker, from the chain's own figures (the
listing row for TRX, the receipt for TRC20): `total_fee` is the fee the owner
paid, `bandwidth_fee` is `net_fee`, `energy_fee` is `energy_fee`,
`energy_used` is `energy_usage_total`, and `bandwidth_used` is the bytes the
transaction consumed, whatever paid for them:
`net_usage + net_fee / getTransactionFee`, with the chain parameter read once
per pass (a pass that cannot read it fails; no price is assumed). On a TRX
transfer row, the part of the fee beyond bandwidth and energy
(`fee - net_fee - energy_fee`), when it is positive, is reported as
`account_creation_fee`; on a transfer to an address the chain had not created
this is the creation fee. For such a row `net_fee` is the chain's fixed
create-account charge and not a price per byte, so `bandwidth_fee` is
`net_fee` and `bandwidth_used` is `net_usage` alone (0 when the owner's staked
bandwidth did not cover the transaction), as in a withdraw result. Other rows
carry no `account_creation_fee`.

**R21.10C.9 (the process).** One process per platform coin writes the history
of the platform and of every TRC20 token enabled on it (a token enabled later
is picked up on the next pass); a TRC20 coin activated on its own through the
legacy route has a process of its own. A pass walks each known address, stores
new rows page by page, and is followed by a pause of 60 seconds. The first walk
of an address in a process reads its whole listing; a later walk stops at the
first page with nothing new. A stored row is never asked for again. The
process ends when the node stops or the coin is disabled. The rows go to the
coin's history store, which both `my_tx_history` methods read, newest first.

**R21.10C.10 (history state).** `sync_status` is `NotEnabled` without an
enabled history, `NotStarted` before the first pass, `InProgress` with
`{"transactions_left": n}` while the first pass works (the rows of the page in
hand, or the token rows still waiting for a receipt), and `Finished` after a
pass that left nothing open. A pass that fails before the first `Finished`
shows `Error` with `{"message": "..."}`, a text under the secrecy rule of
R21.10C.3; a pass that fails after it keeps `Finished` and is logged.

**R21.10C.11 (limits).** Confirmed rows only; a reorganization of the chain is
not followed, and a stored row is not removed. Amounts moved by staking, by
TRC10 assets, by a contract call's own value and by internal transactions are
not shown as amounts (an own transaction of these kinds shows its fee). A
TRC20 row has an empty `tx_hex`. The bandwidth price is the current chain
parameter, so `bandwidth_used` of a row older than a price change is derived
with the newer price. On a browser build the same rules hold for the legacy
`my_tx_history`; the versioned method answers `NotSupportedInWasm` there, as
for every coin family, and the keyed history has not been verified in a
browser.

**T21.10C (tests).** Unit tests against loopback stub nodes
(`coins/eth/tron/history_tests.rs`, `node_client_tests.rs`, `nodes.rs`, with
the captured listings and receipts under
`coins/eth/tron/test_fixtures/history/`): the key rule of R21.10C.1 and
R21.10C.2 (no key, a key on another host, a TronGrid host over `http`), and
that only keyed nodes are asked; failover, cutoff and the request forms of
R21.10C.3, and that no error or state text carries a key or a URL part; the
record mapping of R21.10C.5 to R21.10C.8 for a TRX transfer out and in, a
transfer to a new account, a TRC20 transfer out and in, a fee-only row and the
rows that are left out; the bandwidth derivation (bandwidth of the owner only,
burned only, mixed); the receipt read once and the row left out without one;
paging; the merge over several own addresses, and a stored row recomputed when
its receiver becomes a known address; the id of a token row being the same in
a filtered and an unfiltered listing; the states of R21.10C.10; and
the answers of both methods with `current_block` from the TRON node, including
T21.10B.10 for a coin without a key.

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
  coins are refused by the swap pipeline. What the reference itself
  does on this point, and the decisions a Reloaded TRON swap needs,
  are recorded in §21.11B (passed the KDF Dirty Gate on 2026-11-08).
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
- **D6 -- Switching TRON back on (beta availability, maintainer
  decision 2026-10-26).** Activation is switched off for the coming
  beta by `TRON_ACTIVATION_AVAILABLE` (see the availability note at
  the top of this chapter). Setting it to `true` and retiring the
  refusal tests that go with it is planned for the next beta.

  *Status note (2026-11-20), D6 done: switched on as wallet-only.*
  `TRON_ACTIVATION_AVAILABLE` is `true` from this date and the refusal
  tests are retired; each route they covered has a test that activates a
  TRX coin and a TRC20 token against a loopback node. The refusal text
  and its test-only override are removed, and no route reads the
  constant any more. Swaps stay off behind a second constant,
  `TRON_SWAPS_AVAILABLE` (`false`): D1 -- D4 are unchanged, and that
  constant is set to `true` only together with a TRON swap
  implementation (§21.11B).

- **D7 -- EVM-only RPC methods (Reloaded status note, 2026-11-22;
  clean-side, not corpus-sourced).** A TRX or TRC20 coin is a member of
  the EVM coin family, but its node list holds TRON nodes, and a TRON
  node does not serve EVM JSON-RPC. Seven methods of the family have no
  TRON implementation in Reloaded and reach, for an EVM coin, the
  coin's EVM nodes or its gas API. Until this date a TRON coin took the
  same path: the method failed only after the coin's TRON nodes had
  been asked in the EVM form (`sign_raw_transaction`, which has no
  node to ask for a TRON coin: only after its wait for a nonce had run
  out), with an error that reads like a node outage. From this date
  each of the seven refuses a TRX and a TRC20 coin with a typed "not
  supported" answer, before any request to a node or to a gas API:

  - `get_raw_transaction`: `NotImplemented` naming the coin, HTTP 501.
  - `sign_raw_transaction`: `NotImplemented` naming the coin, HTTP 501.
  - Legacy `get_trade_fee`: the coin's trade fee fails with the text
    "TRON trade fee is not supported" (the unit tests call the coin
    method; the legacy error body around it is not asserted).
  - `trade_preimage` whose receiving side is a TRON coin: the coin's
    receiver trade fee fails with `InternalError` and the text "TRON V1
    receiver trade fee is not supported", the same form in which the
    sending side was already refused (the unit tests call the coin
    method; the RPC status is not asserted). The check of the
    coins-config `wallet_only` flag in `trade_preimage` is not changed.
  - `get_eth_estimated_fee_per_gas`: `CoinNotSupported`, HTTP 501.
  - `stream::fee_estimator::enable`: `NotSupportedFor` naming the coin,
    HTTP 501; no streamer is started.
  - `get_token_info` with an `ERC20` protocol whose platform is a TRX
    or TRC20 coin: `UnsupportedTokenProtocol` naming the platform, HTTP
    400, the answer a `TRC20` protocol already got.

  An EVM coin is unchanged in all seven. The note of 2026-11-20 after
  R21.10B.9 calls the legacy `get_trade_fee` and `trade_preimage`
  unchanged; that holds for the wallet-only question it describes, and
  the two refusals above are added to it from this date.

  *Against the reference (restated 2026-11-22, after the block
  "EVM-family methods called on a TRX or TRC20 coin" of §21.10B was
  cleared).* The reference fails five of the seven methods for a TRON
  coin as well, with no node request, four of them with other answers
  than Reloaded's; it accepts the fee stream, and with a gas-fee
  provider it answers the fee estimate:

  - `get_raw_transaction` (R21.10B.15): the reference answers
    `InvalidHashError` (HTTP 400) for a malformed `tx_hash` and
    `Transport` (HTTP 502) otherwise; Reloaded answers `NotImplemented`
    (HTTP 501) for every request, before the hash is read.
  - `sign_raw_transaction` (R21.10B.16): the reference answers
    `InvalidParam`, `InternalError` or, after about five seconds,
    `TransactionError`, depending on the request; Reloaded answers
    `NotImplemented` (HTTP 501) at once, before the request members are
    read.
  - Legacy `get_trade_fee` (R21.10B.17): both answer the legacy error
    body; the text differs (the reference's text is not recorded).
  - `trade_preimage` (R21.10B.18): the same tokens, `CoinIsWalletOnly`
    for a wallet-only config and `InternalError` (HTTP 500) otherwise.
    Where the reference could obtain Ethereum figures through a
    non-legacy swap fee policy and a gas-fee provider (the last
    paragraph of R21.10B.19), Reloaded refuses the TRON coin's fee
    reads before any figure is asked for.
  - `get_eth_estimated_fee_per_gas` (R21.10B.19): the reference answers
    `Internal` (HTTP 500) without a gas-fee provider and **succeeds**
    with Ethereum's fees under the TRON ticker with one; Reloaded
    answers `CoinNotSupported` (HTTP 501) and does not ask the provider.
  - `stream::fee_estimator::enable` (R21.10B.20): the reference
    **accepts** the request and runs a stream of error events, or of
    Ethereum's fees with a gas-fee provider; Reloaded answers
    `NotSupportedFor` (HTTP 501) and starts no stream.
  - `get_token_info` (R21.10B.21): the reference answers `NoSuchCoin`
    (HTTP 404) for a platform that is not active, `InvalidRequest`
    (HTTP 400) for a base58check or `41...` contract address and
    `RetrieveInfoError` (HTTP 500) for a `0x` one, for an `ERC20` and a
    `TRC20` protocol alike. For an `ERC20` protocol on a TRON platform
    Reloaded answers `NoSuchCoin` for a platform that is not active and
    `UnsupportedTokenProtocol` (HTTP 400) otherwise, whatever the
    address; a `TRC20` protocol is refused with
    `UnsupportedTokenProtocol` before the platform is looked up, as
    before this date.

  The fee estimate with a gas-fee provider and the fee stream are
  deliberate divergences, by the maintainer's decision of 2026-11-22: a
  TRON ticker is not answered with another chain's fees, and a stream that
  can only report errors is not started. The other differences are in the
  error token of a call that fails on both sides. All of them are local,
  no peer sees them, and no compatibility switch exists
  (`docs/COMPAT_SWITCHES.md`, "The default-side rule", arm 2).

  The parity rows of that block are restated for Reloaded:

  - T21.10B.19: `get_raw_transaction` answers `NotImplemented`, HTTP
    501; `get_token_info` with an `ERC20` protocol on the TRX platform
    answers `UnsupportedTokenProtocol`, HTTP 400, for both address
    forms (by the order of the checks: the address is read after the
    refusal); `get_eth_estimated_fee_per_gas` with `Simple` answers
    `CoinNotSupported`, HTTP 501; the legacy `get_trade_fee` answers the
    legacy error body. The stub's count does not change.
  - T21.10B.20: `sign_raw_transaction` answers `NotImplemented`, HTTP
    501, at once, for each of the three requests (by the order of the
    checks: the refusal comes before the request members are read). The
    stub's count does not change and no answer carries a transaction.
  - T21.10B.21: unchanged; Reloaded gives the same answers (the unit
    tests call the coin's fee methods; the RPC status is not asserted).
  - T21.10B.22: `stream::fee_estimator::enable` for TRX answers
    `NotSupportedFor`, HTTP 501, no stream is started, and the stub
    receives no request.

**R-D1 (gate invariant).** Because the coin activates with no
swap-contract binding, no production code path may reach a
Tron swap/NFT operation; the deferral sites exist to make the
boundary explicit and MUST surface a typed refusal (or a guarded
unreachable) rather than producing incorrect swap behaviour.

## 21.11A Trading floors (pointer; Spec Reader 2026-10-25, cleared by the Dirty Gate with chapter 60 on 2026-10-25)

TRX and TRC20 tokens are cases of the EVM coin family in both trees, so
their minimum trading volume is the EVM rule of
[Chapter 57](57-evm-fee-inputs.md) R57.7A.1 — one smallest unit,
`10^-decimals` (TRX: 10^-6) — and their minimum transaction amount is zero
(R57.7A.3), in both reference designations (`v2.6.0-beta` and
`v3.0.0-beta` agree). Reloaded today reports the baseline formula's
10^-2 for TRX; the chapter-57 change (PR #66) corrects TRON with the rest
of the EVM family. No peer observes the value while D1–D2 keep Tron out
of swaps ([Chapter 60](60-trading-floors-and-refusal-contract.md) row T1).

## 21.11B Swap behavior of the reference (Spec Reader 2026-11-08)

> **Status of this section.** Written by the dirty-side specification
> author on 2026-11-08 for round 0.4.0-6 (topic: the reference TRON
> swap); it **passed the KDF Dirty Gate on 2026-11-08**, measure 0.01
> (clearance record in §21.13). It answers one question first -- *does any
> released reference build carry a TRON atomic swap?* -- and then records
> what the historical record and the public swap-contract repository do
> carry, so that the maintainer decisions D21.B1-D21.B9 below can be taken
> on facts rather than on the unreviewed branch rewrite of this chapter
> that lives on `origin/feat/tron-swap-support` in this repository.

### 21.11B.1 Finding: no released reference build carries a TRON swap

**F21.B1 (the two designations).** Neither `v2.6.0-beta` nor
`v3.0.0-beta` (chapter 02 §2.15A) carries a TRON atomic-swap
implementation: there is no TRON contract call for a payment, a spend or
a refund, no TRON swap-contract binding, no TRON payment validation, no
TRON spend discovery, no TRON DEX-fee send or validation, and no TRON
trade-fee estimator in either release. The release notes of both
designations describe TRON as groundwork (`v2.6.0-beta`) and as
*wallet-only* support (`v3.0.0-beta`). The `dev (as of 2026-03-27)` row
is identical to `v3.0.0-beta` on this point.

**F21.B2 (what `v2.6.0-beta` has).** The `TRX` protocol case, its network
selector and the address encoding of §21.5 exist; the TRON module is a
placeholder. There is no TRC20 protocol case, no TRON node client, no
TRON transaction builder, no withdraw, and nothing swap-related.

**F21.B3 (what `v3.0.0-beta` does when a TRON coin meets the swap
pipeline).** The wallet-only integration of §21.2-§21.10 is what the
release has. Where the swap pipeline touches a TRON coin, the observable
behavior is:

- *Order placement.* Deriving an orderbook address for a coin whose
  protocol is `TRX` or `TRC20` is refused with a typed outcome that
  reports the coin as unsupported, so an order for a TRON coin cannot be
  created or matched at this release. (The *generic* public-key-to-address
  helper of the coin layer, used by RPCs that render an address for a
  given public key and coin config, does already produce the base58check
  display form of §21.5 for `TRX` and `TRC20`; only the orderbook path
  refuses.)
- *Swap-contract fields at activation.* The one-shot and task-based
  activation routes of §21.4 accept `swap_contract_address` and
  `fallback_swap_contract` for a TRON platform exactly as for any EVM
  platform: for a coin whose config says `"wallet_only": true` the values
  are accepted and ignored; for a trading-capable coin (`wallet_only`
  absent or `false`) a non-zero `swap_contract_address` is required and a
  zero `fallback_swap_contract` is rejected. There is no TRON-specific
  rule. The release's own TRON coin configurations (its test fixtures)
  are `wallet_only: true`, declare `required_confirmations: 1`, and set
  `avg_blocktime: 3`.
- *Version-1 payment, spend, refund, taker fee.* The EVM family's V1
  swap operations build an Ethereum-type transaction and decode
  counterparty transactions as RLP; a TRON coin has no EVM chain
  identifier and no EVM node transport, so a TRON payment or taker-fee
  send fails at the signing step with an untyped error before anything
  is broadcast, and validation, spend, refund and spend-search would fail on
  the RLP decode or on the absent EVM transport. No TRON contract call
  is ever built. None of this is reachable in practice because of the
  orderbook refusal above.
- *Secret-hash algorithm.* The V1 pair selection of the release lists the
  Tendermint and Sia families as the ones forcing the 32-byte SHA-256
  alternate; a TRON coin is not listed and would take the 20-byte default
  ([Chapter 51](51-legacy-v1-swap-state-machine.md) R71/R72 describe the
  same selection). The release's EVM V1 payment path additionally narrows
  a 32-byte secret hash to 20 bytes by RIPEMD-160 before encoding it for
  the EVM V1 contract (chapter 51 R72A).
- *Version-2 swaps, watchers, NFT.* Unchanged from §21.11 D2, D4, D5.

**F21.B4 (the reference branch row).** The row `feat/tron-swap-support
(as of 2026-04-23)` of chapter 02 §2.15A -- the historical record's own
branch of that name, not this repository's -- carries **no TRON swap
code**. It is a single commit over its base `dev (as of 2026-03-09)`,
and the files that commit adds are documents only -- no source file is
added or changed. At that base the TRON module is an earlier state of
the `v3.0.0-beta` module (the release's later withdraw and fee
refinements are absent) and the activation surfaces are identical to the
release (the §21.4 note). Nothing on this row is behavior at a state
beyond what `dev (as of 2026-03-09)` already has; **no R-rule of this
chapter derives from it**.

**F21.B5 (the public swap-contract repository).** The project's V1 swap
contract lives in a sibling public repository (chapter 01 R14
citation in §21.11B.6). That repository's branch `feat/tron-v1-swap`,
tabled as `etomic-swap feat/tron-v1-swap (as of 2026-09-01)` in chapter
02 §2.15A, carries a Tron-VM build of the V1 HTLC contract under the MIT
license, TronBox compile and deployment tooling for the Nile testnet and
mainnet, a contract test suite, and a Nile proof script. It records a
deployment on the **Nile testnet** at the address below; it records **no
mainnet and no Shasta deployment**. The branch is a contract-side
artifact only: no released reference client calls it, and the KDF-side
code that would call it exists at no tabled state.

| Network | Deployed TRON V1 swap contract | Verified |
| --- | --- | --- |
| Nile (testnet) | `TTTzsLDg81qoUkiEbeHsKbXZ4rWi1uFpo3` (hex wire form `41bfec12aa6f266fc0010df10b9e5a9d49149a5c4e`) | Public-network observation on 2026-11-08 (chapter 01 R6): the Nile full-node contract query returns a contract of that name whose ABI is the one of §21.11B.2 |
| Mainnet | none recorded at any tabled state | -- |
| Shasta (testnet) | none recorded at any tabled state | -- |

**Answer to the decisive question.** A TRON swap *contract* exists on a
branch of the public contract repository and on the Nile testnet; a
TRON swap *client* exists in no released reference build and on no
branch of the historical record. There is therefore **no reference
counterparty** a Reloaded TRON swap could be tested against today, and
no released behavior to be wire-compatible with. Whatever Reloaded binds
for TRON swaps is a Reloaded extension (D21.B3).

### 21.11B.2 The public Tron-VM V1 swap contract (dictated interop, conditional)

The following is the interop surface of the contract of F21.B5. It is
recorded because any client that talks to that contract -- or to a
Reloaded deployment of the same source -- must conform to it (R29/R31/
R33). The ABI symbols are quoted under chapter 01 R11's
inter-operability carve-out (contract Application Binary Interface
symbols); the deployment facts rest on the public-network observation of
chapter 01 R6 (the ABI and the code hash are readable from the chain);
the repository is cited under chapter 01 R14 in §21.11B.6. It binds
Reloaded **only if** D21.B2 selects this contract.

| ABI member (dictated signature) | Role |
| --- | --- |
| `ethPayment(bytes32 id, address receiver, bytes32 secretHash, uint64 lockTime)` *(payable)* | Lock a native-TRX payment; the amount is the call value and must be positive |
| `erc20Payment(bytes32 id, uint256 amount, address tokenAddress, address receiver, bytes32 secretHash, uint64 lockTime)` | Lock a TRC20 payment; pulls `amount` from the caller through the token's `transferFrom`, so an allowance for the contract must exist first |
| `receiverSpend(bytes32 id, uint256 amount, bytes32 secret, address tokenAddress, address sender)` | Claim a payment by revealing the 32-byte secret |
| `senderRefund(bytes32 id, uint256 amount, bytes32 secretHash, address tokenAddress, address receiver)` | Refund a payment once the chain time has reached `lockTime` |
| `payments(bytes32 id)` → `(bytes32 paymentHash, uint64 lockTime, uint8 state)` | Read stored payment state; `state` is 0 uninitialized, 1 payment sent, 2 receiver spent, 3 sender refunded |
| event `PaymentSent(bytes32 id)` | Emitted on lock |
| event `ReceiverSpent(bytes32 id, bytes32 secret)` | Emitted on spend; carries the revealed secret |
| event `SenderRefunded(bytes32 id)` | Emitted on refund |
| error `SafeERC20FailedOperation(address token)` | The token-safety wrapper's failure (OpenZeppelin), surfaced when `transferFrom` fails |

Contract semantics a client depends on (all verifiable against the
deployed code and the public source):

- **Hash lock: SHA-256, 32 bytes.** The contract stores a *payment hash*
  computed as SHA-256 over the packed encoding of `(receiver, sender,
  secretHash, tokenAddress, amount)` with the zero address standing for
  native TRX; a spend recomputes it with `SHA-256(secret)` in place of
  `secretHash`. The secret hash a client must supply is therefore
  `SHA-256(secret)`, 32 bytes -- **not** the 20-byte
  `RIPEMD-160(SHA-256(secret))` of the EVM V1 contract -- and a client
  MUST NOT narrow it. The payment-hash composition is why a client's
  validation can read `payments(id)` and compare against the arguments
  it decoded from the counterparty's call: a mismatch in any of
  receiver, sender, secret hash, token or amount changes the stored hash.
- **Lock time** is compared against the chain's block timestamp in
  seconds (`block.timestamp >= lockTime`); the node API reports block
  times in milliseconds (TRON developer documentation, the *Transaction*
  protocol page), so a client comparing the two divides by 1000.
- **Addresses in the ABI** are the bare 20-byte payloads of §21.5 (no
  `0x41` prefix inside the ABI encoding); the zero address denotes native
  TRX.
- **Token transfers.** The payment leg uses the safe-transfer wrapper
  for `transferFrom`; the spend and refund legs send the token with a
  plain `transfer` and verify the receiver's balance increased by
  `amount`, tolerating tokens that return no boolean or `false` on
  success (the mainnet TRON USDT behavior). The token's *allowance*
  semantics (including a zero-first requirement some tokens impose) are
  the token's, not the contract's; see D21.B6.
- **Swap identifier.** The contract does not compute `id`; the client
  does. The V1 client derivation this repository already carries for the
  EVM family (baseline-derived, `mm2src/coins/eth/eth_impl.rs`) is
  `SHA-256( little-endian uint32(lockTime) ‖ secretHash )`. No tabled
  state binds the TRON variant's input width; with a 32-byte secret hash
  the natural reading is the same derivation over the 32-byte value
  (D21.B4).

### 21.11B.3 Reference facts that bind now (requirements)

**R21.B1 (no compatibility claim).** Reloaded MUST NOT state or imply
wire compatibility of TRON swaps with `v2.6.0-beta` or `v3.0.0-beta`;
both lack the capability (F21.B1). Operator-facing compatibility
documents MUST list TRON swaps as a Reloaded extension with a
Reloaded-only counterparty set until a reference release carries them.

**R21.B2 (activation-time contract binding follows the EVM rule).**
When TRON swaps are enabled, the swap contract MUST be bound at
activation through the EVM family's `swap_contract_address` (and
optional `fallback_swap_contract`) with the reference's generic
validation of F21.B3: required and non-zero for a trading-capable coin,
ignored for a `wallet_only` coin. No TRON-specific activation field is
introduced. The address MUST be accepted in either form of R-AD1
(base58check or hex) and normalized to the 20-byte payload; the form
operators are told to write is D21.B7.

**R21.B3 (a wallet-only TRON coin never reaches a swap operation).**
R-D1 continues to hold for a TRON coin activated with `wallet_only:
true` or without a swap-contract binding; the refusal MUST remain typed
and MUST occur before any transaction is built.

*Status note (2026-11-20), R21.B3 as built.* Every TRON coin activates
without a swap-contract binding (R-A1 step 5), so R21.B3 covers every
TRON coin Reloaded can activate. From this date it is enforced before
the deferral sites of §21.11 are reached: while `TRON_SWAPS_AVAILABLE`
is `false` a TRX or TRC20 coin is wallet-only whatever its config says,
so order placement refuses it before any balance or fee work, it
declines every match, a saved order naming it is cancelled at
startup (the status note at R21.10B.9), and an unfinished saved swap
whose maker or taker coin is a TRON coin, by the coins-config protocol
`TRX` or `TRC20`, is not resumed at startup, in the legacy swap files
and in both v2 swap storages. Such a swap is skipped with one log line
that names its uuid and the reason; its record is neither deleted nor
marked failed, so it can resume in a build that has TRON swaps, and its
coins are not among those the kick-start asks to be enabled. The typed
refusals of §21.11 stay in place behind that; a `recover_funds_of_swap`
asked for by hand for such a record is not stopped by this switch and meets
them. The swap-contract members of an activation request are still validated
by the rule of F21.B3 and still not bound.

**R21.B4 (orderbook address).** When TRON swaps are enabled, the
orderbook address derived from an order's public key for a `TRX` or
`TRC20` coin MUST be the base58check display form of §21.5 -- the form
the reference's generic public-key-to-address helper already yields
(F21.B3) -- so that a TRON address shown for an order and the address
shown for a balance are the same string. The order and swap messages
themselves carry public keys, not addresses; no wire change is implied.

**R21.B5 (confirmations).** The confirmation depth a TRON leg waits for
MUST come from `required_confirmations` with the EVM family's request-
over-config override (F21.B3); the coin configurations of the reference
and of the lab use 1. Whether Reloaded waits for *solidified* blocks in
addition is D21.B8.

**R21.B6 (secret-hash arm follows the contract).** If D21.B2 selects the
Tron-VM contract of §21.11B.2, the TRON coin family MUST force the
32-byte SHA-256 alternate of chapter 51 R71/R72 on whichever side it
appears, and the TRON payment path MUST pass the 32-byte value through
unnarrowed (chapter 51 R72A). Chapter 51 R72's list of forcing families
then gains TRON; that amendment is part of the same change and MUST NOT
be made for a contract with a 20-byte lock.

*Status note (2026-11-22), R21.B6 as built.* D21.B2 selects the Tron-VM
contract of §21.11B.2 (the note there), so the first half of this rule
is built: the pair selection of the legacy V1 path
(`select_secret_hash_algo`, chapter 51 R72 and its status note of this
date) selects the 32-byte SHA-256 alternate whenever a TRX or TRC20 coin
is the maker coin or the taker coin. An ETH or ERC20 coin keeps the
20-byte default. The second half, the TRON payment path that passes the
32-byte value through unnarrowed, is not built yet; it comes with the
TRON swap operations. While `TRON_SWAPS_AVAILABLE` is `false` no TRON
coin reaches a swap (the note at R21.B3), so no swap that can run today
changes, and a saved swap keeps the secret hash it was started with.
The TRON swap operations MUST refuse a secret hash that is not 32
bytes, and the TRON swap switch is not turned on before they do.

### 21.11B.4 Open maintainer decisions

**D21.B1 -- Activation-route parity.** The reference activates TRON only
through `enable_eth_with_tokens` and `task::enable_eth::*` and refuses
the legacy methods (§21.4 note); Reloaded has the opposite shape
([Chapter 55](55-coin-activation-wire-contract.md) D55.5). TRON swaps
need `swap_contract_address` at activation, which the mmrpc 2.0 route
already carries for every EVM platform. Options: (a) add the mmrpc 2.0
route for the TRON platform (parity; chapters 21, 35, 55 to agree) and
keep the legacy route as a documented extension; (b) extend the legacy
route with the swap-contract fields instead. The umbrella plan
`docs/plans/tron-support.md` work package 1 is this decision.

*Status note (2026-11-17), D21.B1 decided: (a).* TRON has the mmrpc 2.0 route
and keeps the legacy route as a documented Reloaded extension (maintainer
decision of 2026-11-08; built in PR #405, see the note at §21.4).

**D21.B2 -- Contract choice.** No released reference client exists, so
the contract is Reloaded's to choose: (a) adopt the public MIT Tron-VM
V1 contract of §21.11B.2 (same source, Reloaded-operated deployments on
Nile and mainnet; the existing Nile instance of F21.B5 was deployed by a
third-party key and is suitable for lab rows only); (b) a Reloaded-own
contract (own audit and deployment; frees the hash arm and ABI); (c)
wait for a reference release. Under (a) the MIT license is compatible
(chapter 01 R5); the source is public and its deployment bytes are
verifiable on chain; the contract itself has no published audit at any
tabled state.

*Status note (2026-11-22), D21.B2 decided: (a).* Reloaded adopts the
public MIT Tron-VM V1 contract of §21.11B.2, with Reloaded-operated
deployments (maintainer decision of 2026-11-08).

**D21.B3 -- Interop scope.** Reloaded-to-Reloaded only (R21.B1), with the
compatibility documents saying so; cross-implementation rows in the lab
only when a counterparty release appears.

*Status note (2026-11-22), D21.B3 decided.* TRON swaps are
Reloaded-to-Reloaded only (maintainer decision of 2026-11-08).

**D21.B4 -- Hash arm and swap id.** With (a) of D21.B2: 32-byte SHA-256,
chapter 51 R72 amended per R21.B6, swap id over the 32-byte secret hash.
With (b): decided with the contract.

*Status note (2026-11-22), D21.B4 decided.* As D21.B4 states for (a) of
D21.B2: 32-byte SHA-256 and the swap id over the 32-byte secret hash
(maintainer decision of 2026-11-08). The pair selection is built (the
note at R21.B6); the swap id and the unnarrowed payment path come with
the TRON swap operations.

**D21.B5 -- Fee handling.** Native payment as a smart-contract trigger
carrying the amount as call value (a plain transfer does not reach a
contract on TRON; TRON Developer Hub, *Smart Contract Deployment and
Invocation*); every contract call carries a `fee_limit` cap in SUN that
bounds the caller-side energy budget, staked and burned alike (TRON
Developer Hub, *Resource Model*; *Transaction* protocol page); energy
estimated by the constant-contract dry run the withdraw path already
uses (§21.8, §21.9), plus bandwidth; the safety margin and the cap policy
are Reloaded's, and MUST follow the charging semantics those pages
state for a call whose cap is below its need. No reference figure exists
for any TRON swap leg; the trade-preimage numbers for TRON are therefore
Reloaded-defined.

**D21.B6 -- TRC20 allowance.** Before `erc20Payment` the client must
ensure the contract's allowance; whether to set the allowance to zero
first for tokens that reject a non-zero-to-non-zero change, and whether
to approve exactly the amount or more, is Reloaded's to decide per
token; no tabled state has a behavior.

**D21.B7 -- Address form in config and documents.** base58check (`T...`,
what explorers show) or hex (`41...`/`0x`-prefixed payload, what the
EVM request type otherwise takes). R21.B2 requires both to be accepted;
the documented form is the decision.

*Status note (2026-11-17), D21.B7 decided.* Reloaded accepts both forms for
`swap_contract_address`, `fallback_swap_contract` and the `swap_v2_contracts`
members on a TRON platform (the base58check `T...` form, the hex wire form
`41...` with or without `0x`, and the EVM `0x` 20-byte form), normalizes each to
the 20-byte payload, and documents the base58check form. This is a documented
extension of the reference's hex-only decoding of R21.10B.2. An EVM platform is
unchanged: it keeps the `0x` 20-byte form only, and another form fails request
decoding (`InvalidRequest`, HTTP 400) as before; the dispatcher checks the
members against the platform's protocol before decoding.

**D21.B8 -- Spend discovery and finality.** TRON full nodes expose no
log filter; discovery of a counterparty spend or refund needs either the
indexed contract-event endpoint of the event service (TRON Developer
Hub, *Event*; HTTP reference *Get events by contract address* -- served
by the event service, not by the plain full-node API of §21.9) or
polling of the transaction-info endpoint for transactions the client
already knows (HTTP reference *Get Transaction Info By Id*, which
returns the receipt and the log entries). If the discovery source is
unavailable, the operation MUST fail typed and MUST NOT report "not
spent" (a refund over a completed leg is the hazard). Whether to wait
for solidified data (the `/walletsolidity/...` endpoint family of the
public HTTP API serves only blocks the super-representative majority has
confirmed) or for `required_confirmations` only is part of this
decision.

**D21.B9 -- DEX fee on TRON.** No tabled state has one. The EVM family's
shape is a plain transfer of the fee amount to the fee address derived
from the DEX fee public key ([Chapter 08](08-fee-routing-engine.md)),
with no burn share on EVM ([Chapter 16](16-swap-v2-pre-burn-output.md)).
Parity would be a `TransferContract` for TRX and a TRC20 `transfer` for
tokens to the base58check form of that address, validated by decoding
the protobuf transaction; the maintainer decides whether TRON takes the
EVM shape.

### 21.11B.5 Relation to the branch rewrite of this chapter in this repository

This repository's `origin/feat/tron-swap-support` carries a rewrite of
this chapter that binds TRON V1 swaps as if they were reference
behavior. Read against the facts above:

- *Agrees with the public contract* (§21.11B.2): the ABI table, the
  32-byte SHA-256 hash lock, the four lifecycle methods, the three
  events, the `payments(id)` tuple and the swap-id derivation. These
  are now verifiable against the deployed Nile contract and the MIT
  source, which is a sounder basis than the rewrite's own provenance
  footer (it claims the corpus was not consulted while binding facts
  that, at the time of its writing, existed at no released state and in
  no public source this chapter can cite).
- *Disagrees with the tabled states*: it presents the SHA-256 arm, the
  indexed-event discovery, the approve-to-zero-first allowance sequence
  and the base58check config form as dictated by "the deployed
  contract and the counterparty protocol". No released reference client
  has any of them; they are design choices (D21.B2, D21.B6-D21.B8).
  Its contract-side token-safety wording ("a safe-transfer wrapper")
  also differs from the public contract, which uses the wrapper on the
  payment leg and balance-checked plain transfers on the spend and
  refund legs.
- *Disagrees with chapter 51*: its per-coin secret-hash selection is the
  one `docs/plans/tron-swap-support.md` already records as unsound
  against R71/R72's pair selection.

Nothing from that rewrite is incorporated here; this section was
authored from the tabled states and the public sources named in
§21.11B.6.

### 21.11B.6 Verification notes and provenance (section-scoped)

- *Re-verifying F21.B5 (public network, chapter 01 R6):* query any Nile
  full node's contract endpoint for the address of the table; the
  response names the contract, lists the ABI of §21.11B.2 and carries the
  runtime bytecode. The SHA-256 of the bytecode returned on 2026-11-08
  was `92f0d9fc6e41ad269798300b3a1c4fb5c098fa918102eda46705139b0f79e357`;
  a Reloaded deployment of the same source and compiler settings is
  expected to match it only if the compiler pin and optimizer settings
  are identical.
- *Sibling-repository citation (chapter 01 R14):* the public swap-
  contract repository `KomodoPlatform/etomic-swap` (mirrored
  commit-for-commit at `GLEECBTC/etomic-swap`), branch
  `feat/tron-v1-swap`; earliest commit containing the Tron-VM contract
  `cb3c7bf7e4f18e2eb983321f11708f315c15f6dd` (2026-04-23); earliest
  commit containing the Nile proof script and its deployed address
  `d928cfd0b95e92a95ab507fe1932db3946dd43a8` (2026-09-01); contract
  license MIT by its SPDX header (the repository root carries no license
  file at that branch). Not part of the forbidden corpus. The citation
  carries the repository and its state only; the ABI symbols of
  §21.11B.2 are admitted by chapter 01 R11's inter-operability class
  (contract ABI symbols), and the deployment facts of F21.B5 by the
  public-network observation of chapter 01 R6 above.
- *Public TRON documentation cited (chapter 01 R12/R13):* TRON Developer
  Hub, *Resource Model* (energy and bandwidth charging; the `fee_limit`
  cap on the caller-side energy budget); TRON Developer Hub, *Transaction*
  protocol page (the `fee_limit` field of the raw transaction body and the
  smart-contract trigger); TRON Developer Hub, *Smart Contract Deployment
  and Invocation*; TRON Developer Hub, *Event* (event subscription and the
  event-query service) and the HTTP reference *Get events by contract
  address* (the indexed contract-event endpoint, served by the event
  service rather than by the plain full-node API) and *Get Transaction
  Info By Id* (receipt and log entries of a known transaction); the public
  full-node HTTP API's solidity endpoint family (`/walletsolidity/...`),
  which serves solidified data only. These are the D21.B5 and D21.B8
  inputs.
- *Not determined:* any mainnet or Shasta deployment (none recorded);
  any reference client-side TRON swap figure (fee, `fee_limit`,
  confirmation count, discovery endpoint) -- none exists at a tabled
  state; the provenance of the branch rewrite in this repository (its
  footer's "corpus not consulted" claim could not be confirmed from any
  tabled state or public source).
- *States read for this section:* `v2.6.0-beta`, `v3.0.0-beta`,
  `dev (as of 2026-03-27)`, `dev (as of 2026-03-09)`,
  `feat/tron-swap-support (as of 2026-04-23)` and
  `etomic-swap feat/tron-v1-swap (as of 2026-09-01)` (chapter 02 §2.15A).
  No other state of the historical record was read for this section.
- *Clearance:* **passed the KDF Dirty Gate on 2026-11-08**, measure 0.01
  (chapter 01 R37-R41); the attestation is in the scoped record of §21.13.

### 21.11B.7 Pointer (2026-11-22): the EVM legacy token-leg points the TRON swap track mirrors

> **Status note (2026-11-22, KDF Spec Reader, round 0.5.0-27).** This pointer
> carries no reference content of its own. Its target, chapter 51 §51.6F,
> **passed the KDF Dirty Gate on 2026-11-22, measure 0.03, no findings**; the
> pointer passed with it, as recorded in chapter 51 §51.16. No sentence of
> §21.0 -- §21.11B.6 is edited.

A TRON legacy swap would take the EVM family's legacy shapes (R21.B2,
F21.B3). Three of those shapes are recorded for the reference's ERC20 token
leg in [Chapter 51](51-legacy-v1-swap-state-machine.md) §51.6F, at the rows
`v2.6.0-beta` and `v3.0.0-beta`:

- *Fees (R93 -- R96).* Which coin each swap fee names, the
  `paid_from_trading_vol` value, the `trade_preimage` members, the fee
  members of the swap status, and the answer when the platform coin cannot
  pay the gas (`NotSufficientBaseCoinBalance` and its data).
- *Volume limits (R97).* What `max_maker_vol` and `max_taker_vol` do about
  the platform coin for a token.
- *Swap-contract addresses (R98 -- R99).* Which addresses the negotiation
  messages carry, how a declared address is accepted, replaced by the
  fallback contract or refused, and at which stage.

These rules describe ERC20 on an EVM platform. The reference has no TRON
case in any of them (F21.B1, R21.10B.17, R21.10B.18): at the reference a
TRX or TRC20 fee figure fails before a descriptor exists. Which coin a TRC20
descriptor names (TRX by the mirror of R93), how the energy and bandwidth
figures replace the gas figures, and the byte form of a TRON contract
address in the negotiation members (the 20-byte payload of R-AD1 by the
mirror of R98) are decisions of the TRON swap lane, not reference facts.

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
- *Forbidden corpus:* (addition of 2026-10-25, §21.11A, cleared by the
  Dirty Gate with [Chapter 60](60-trading-floors-and-refusal-contract.md)
  on 2026-10-25, attestation in its §60.12:
  also consulted under the chapter-01 two-team procedure, at
  `v2.6.0-beta` and `v3.0.0-beta`, only to establish that TRON coins take
  the EVM family's floors; no discretionary expression reproduced.)
  Otherwise consulted **only** to recover the
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
- *Scoped addition (2026-11-08) -- §21.11B, swap behavior of the
  reference.* **KDF Dirty Gate: PASS on 2026-11-08 (second pass), measure
  0.01, no findings.** *Inputs:* the forbidden corpus at the rows `v2.6.0-beta`,
  `v3.0.0-beta`, `dev (as of 2026-03-27)`, `dev (as of 2026-03-09)` and
  `feat/tron-swap-support (as of 2026-04-23)` of chapter 02 §2.15A,
  consulted **only** to establish whether a TRON swap client exists at
  any of them, how the swap pipeline treats a TRON coin at `v3.0.0-beta`
  (orderbook refusal, swap-contract field validation, failure before
  broadcast, secret-hash pair selection), and the kind of change the
  branch row carries over its base (documents only, no source); the
  sibling public swap-contract repository at the row `etomic-swap
  feat/tron-v1-swap (as of 2026-09-01)` (chapter 01 R14 citation in
  §21.11B.6; MIT-licensed contract source; ABI symbols under the R11
  inter-operability class) for the contract ABI and the Nile deployment
  record; a public-
  network observation of the Nile testnet on 2026-11-08 (chapter 01 R6)
  for the deployed contract's name, ABI and code hash; this repository's
  own code for the EVM V1 swap-id derivation; and cross-chapter contracts
  (Chapters 08, 16, 51, 55). Public method strings, public config fields,
  the public contract ABI and behavior only; no private identifier,
  function body, helper decomposition, error, log, comment or
  document text was reproduced. *Dirty Gate clearance (chapter 01
  T1, R40).* PASS on 2026-11-08 at the second pass, measure 0.01, no
  findings. *Extent:* the TRON swap capability and the swap pipeline's
  treatment of a TRON coin (orderbook address derivation, the
  activation-time swap-contract fields, the V1 swap operations and their
  signing path, the secret-hash pair selection, test fixtures and release
  notes), at `v2.6.0-beta`, `v3.0.0-beta`, `dev (as of 2026-03-27)`,
  `dev (as of 2026-03-09)` and `feat/tron-swap-support (as of
  2026-04-23)`: about eight source files read at `v3.0.0-beta`, the
  subsystem's file sets compared at three rows, and the branch row's
  single commit of nine documents read in the relevant passages and
  searched by pattern; plus the sibling row `etomic-swap feat/tron-v1-swap
  (as of 2026-09-01)` (the contract, its proof script and deployment
  record, on both mirrors) and one public Nile network observation (the
  contract's name, nine ABI members and its bytecode hash). *Method:*
  reading, with mechanical git and pattern checks, a remote reference
  listing and an on-chain query; the gate script was not run because there
  are no code blocks. *Measure:* 0.01 on the discretionary-expression
  basis.
- *Clean-context addition (2026-11-17, forbidden corpus not consulted):*
  R-N3 to R-N9 and the status note after R-N2 in §21.9 are
  Reloaded-original. They rest on this repository's own code, chapter 35
  R35.1.7 to R35.1.17 and D35.8 (iii), and on a black-box measurement of a
  public Tron test node on 2026-11-10 (the error form of the node API: a 2xx
  answer with a top-level `Error` string, a non-2xx status only from the HTTP
  layer). The open parity question is whether the reference shuffles the
  configured node order (R-N5); it was not asked and nothing was taken from
  the reference. Their tests are in the Tron node-client test module
  (T-N1 to T-N8 and the header tests of R-N9).
- *Clean-context addition (2026-11-19, forbidden corpus not consulted):* the
  status notes of this date after R21.10B.7, at D21.10B.1 and after the
  compatibility block of §21.10B are Reloaded-original. They rest on the
  cleared text of R21.10B.7 and D21.10B.1, on D21.B5, on this repository's
  own code and on the public TRON protobuf transaction format (§21.6) for the
  sizes. The whole-draw bandwidth rule, the margin and maximum of the
  `fee_limit`, the refusal above the maximum and the settling of a TRX `max`
  amount are Reloaded's own. No cleared sentence was edited.
- *Clean-context addition (2026-11-20, forbidden corpus not consulted):* the
  status note of this date on TRC20 amounts after R21.10B.7 is
  Reloaded-original. It rests on this repository's own code and on the public
  token standard's `transfer(address,uint256)` interface. No cleared sentence
  was edited.
- *Clean-context addition (2026-10-28, forbidden corpus not consulted):* the
  sentence in §21.9 R-N1 that applies chapter 35 R35.1.17 to the Tron client,
  with its status remark, is Reloaded-original. It rests on the maintainer
  decision of 2026-10-26 (host-only node naming everywhere, except the RPC
  wire dump) and on this repository's code; its test is chapter 35 T35.36.
- *Scoped addition (2026-11-07) -- the reference-behavior note in §21.4.*
  **KDF Dirty Gate: PASS on 2026-11-07, measure 0.01.** *Inputs:* the forbidden corpus at the rows `v3.0.0-beta` and
  `feat/tron-swap-support (as of 2026-04-23)` of chapter 02 §2.15A, identical
  on this point, consulted **only** to establish which activation methods
  serve the TRX platform coin and TRC20 tokens and which refuse them, and
  that an NFT request is refused for a TRON platform. Public method names and
  behavior only; no private identifier, function body, helper decomposition,
  error, log or comment text was reproduced. The as-built clause rests on
  this repository's own code and chapter 55 D55.5.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-07, measure
  0.01, no findings. *Extent:* the activation routes that serve or refuse the
  TRON platform and its tokens, at `v3.0.0-beta`, with the activation and
  legacy-initialization modules of `feat/tron-swap-support (as of 2026-04-23)`
  diffed against it (no difference); for the whole batch, about 24 source
  files were read in full or in the relevant regions at the `dev (as of
  2026-03-27)` row, whose source tree is identical to `v3.0.0-beta`, and the
  same modules were diffed against `v2.6.0-beta`. *Method:* reading, with
  mechanical cross-checks (diffs between the tabled states; pattern searches
  of the added lines for paths, identifiers and markers); the gate script was
  not run because the candidate has no code blocks. *Measure:* 0.01, read
  against the standard that any discretionary finding fails a chapter;
  dictated-interop and public-surface tokens were excluded from the measured
  set.
- *Clean-context addition (2026-11-10, forbidden corpus not consulted):*
  §21.10A (R-WP1–R-WP6, T-WP) and the dated status notes in §21.0 and
  after R-W3 are Reloaded-original, as is the dated note in chapter 49
  §49.2A that points to R-WP6. They rest on this repository's code,
  chapter 49's fee-descriptor rule, the public Tron node HTTP API, and a
  public-network observation of the Nile testnet node on 2026-11-10
  (chapter 01 R6) for the node's empty-account and `Error` answer forms.
- *Scoped addition (2026-11-17, Spec Reader round 0.5.0-10) -- §21.10B
  (R21.10B.1 -- R21.10B.9, D21.10B.1 -- D21.10B.3, the compatibility note,
  the not-established paragraph, T21.10B.1 -- T21.10B.15); no other text of
  this chapter was changed.* **KDF Dirty Gate: PASS on 2026-11-17, measure 0.04, no findings**
  (chapter 01 R40; the attestation closes this item).
  *Inputs:* (1) the forbidden corpus at the rows `v3.0.0-beta`,
  `v2.6.0-beta` and `feat/tron-swap-support (as of 2026-04-23)` of chapter
  02 §2.15A, read read-only (the `v3.0.0-beta` sources were read from a
  checkout at the row `dev (as of 2026-03-27)` whose source tree was
  confirmed identical to `v3.0.0-beta` by mechanical difference), and
  consulted **only** to establish: for a TRON platform and its TRC20
  tokens, which mmrpc 2.0 activation members are read and how, which
  requests are refused with which `error_type` token and HTTP status, the
  node list's order, the per-request time bound, which node answers move a
  request to the next node and which end it, the key policies and HD
  methods that work or fail, the activation result's shape and address
  form, whether any history source exists and what the history methods
  answer, the withdraw fee inputs and amounts, the fee-details members, the
  address methods' accepted and returned forms, and the outcome of order
  creation; and the agreement of the three rows on these points (the
  `v2.6.0-beta` row has no TRON wallet behavior; the branch row differs in
  the account-creation cost only). Behavior, public wire names, error
  tokens, statuses, public TRON API endpoint paths, response members,
  chain-parameter keys and `response_code` names only; no private
  identifier, function body, helper decomposition, error, log, comment or
  test text was reproduced, the legacy error texts are deliberately not
  recorded, and the java-tron rate-limiter text is described, not quoted.
  (2) Public TRON documentation: TRON Developer Hub, *Resource Model*, for
  the node's bandwidth rule in R21.10B.7 (already cited in §21.11B.6).
  (3) This repository's tree at the base of this round (the dev branch as
  of 2026-11-17), for the compatibility note and the cross-references.
  D21.10B.1 -- D21.10B.3 are Reloaded-original. No row was added to chapter
  02 §2.15A: every state read already had one, and each row's identifier
  was confirmed, by read-only inspection of the historical record's
  reference metadata, to be the commit the row names.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-17, measure
  0.04 on the discretionary-expression basis, no findings. Reference states
  (chapter 02 R13): PASS; the rows `dev (as of 2026-03-27)` and
  `v3.0.0-beta`, whose trees are identical, `feat/tron-swap-support (as of
  2026-04-23)` and `v2.6.0-beta`, all tabled in §2.15A. Extent: the TRON
  wallet surface of the reference (mmrpc 2.0 platform and token activation;
  the node client and its failover; key policies and HD; the activation
  result; history; the withdraw fee estimate and amounts; fee details;
  address methods; order creation), compared at the rows `dev (as of
  2026-03-27)` and `v3.0.0-beta` (the same tree), `feat/tron-swap-support
  (as of 2026-04-23)` and `v2.6.0-beta`; 22 source files of that surface
  compared at the dev/`v3.0.0-beta` tree, the core client, fee, withdraw and
  activation files read in full or in their relevant parts; the branch row
  and `v2.6.0-beta` compared on the points attributed to them. Method:
  reading, combined with a mechanical word n-gram comparison (6-gram and
  4-gram) and a per-token inventory; the additions embed no code blocks.
- *Status note added 2026-11-19 (Spec Reader round 0.5.0-15) -- after
  R21.10B.4.* **KDF Dirty Gate: PASS on 2026-11-19, measure 0.02, no
  findings** (with chapter 49 §49.2.1; the attestation is in chapter 49
  §49.7). *Inputs:* the forbidden corpus at
  the row `v3.0.0-beta` of chapter 02 §2.15A, consulted **only** to establish
  which tickers the HD address methods carry, and what makes an address
  used for the scan, when the request names a TRC20 ticker. The reading was
  done together with chapter 49 §49.2.1, whose scoped record covers the
  shared EVM-family surface. Behavior only; no private identifier, body,
  error, log or comment text was reproduced. The sentences of R21.10B.4 are
  unchanged.
- *§21.10C added 2026-11-20 (transaction history; clean-side,
  Reloaded-original).* R21.10C.1 -- R21.10C.11, T21.10C, the status note
  after R21.10B.6 and the dated notes of 2026-11-20 in §21.4 and after the
  compatibility block of §21.10B. *Inputs:* TronGrid's public API
  documentation for the two account listings (paths, parameters and answer
  members); the public TRON full-node HTTP API for the receipt and the
  chain parameters (§21.9); public Nile test-network data captured on
  2026-11-20 (chapter 01 R6); and this repository's own code. *Forbidden
  corpus:* not consulted for this section; R21.10B.6 already records that
  the reference has no TRON history. No sentence of §21.0 -- §21.10B was
  edited.
- *Scoped addition (2026-11-20, Spec Reader round 0.5.0-17) -- the status
  note after R21.10B.7 and the block that follows it (the public-rule
  paragraph, R21.10B.10 -- R21.10B.13, the two code-quality findings, the
  two not-established paragraphs, T21.10B.16 -- T21.10B.18); no other text
  of this chapter was changed.* **KDF Dirty Gate: PASS on 2026-11-20,
  measure 0.02, no findings** (chapter 01 R40; the attestation closes
  this item).
  *Inputs:* (1) the forbidden corpus at the rows `v3.0.0-beta` (read from
  a checkout at the row `dev (as of 2026-03-27)`, whose source tree was
  confirmed identical by mechanical difference), `v2.6.0-beta`,
  `feat/tron-swap-support (as of 2026-04-23)` and `feat/tron-gasfree (as
  of 2026-05-04)` of chapter 02 §2.15A, read read-only and consulted
  **only** to establish: which energy figure the TRC20 withdraw fee, its
  `fee_limit` and its TRX check use; whether any trade-fee or swap-fee
  method has a TRON fee estimate; whether any path requests contract
  metadata or the deployer's resources; which members of the dry-run
  answer decide whether a TRC20 withdraw or a token balance read goes on,
  and with which error token and status it ends otherwise; and the
  agreement of the rows on these points. Behavior, public wire names, error tokens, statuses and
  public TRON endpoint paths and member names only; no private
  identifier, body, helper decomposition, error, log, comment or test
  text was reproduced. (2) Public TRON documentation (TRON Developer Hub,
  *Resource Model* and the smart-contract energy pages; the HTTP API
  reference for `wallet/getcontract` and for the transaction receipt) for
  the origin-energy rule. (3) This repository's tree at the base of this
  round (the dev branch as of 2026-11-20) for the cross-references. No
  row was added to chapter 02 §2.15A: every state read already had one.
  The remedies sketched in the two code-quality findings and the three
  acceptance rows are Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-20, measure
  0.02, no findings. *Extent:* the TRON wallet surface the block reaches:
  the TRC20 withdraw fee estimate and its energy input, the `fee_limit`,
  the check that the sender holds the TRX for the fee, the node client's
  handling of a dry-run answer and of a token balance read, and the
  endpoints requested; at the rows `dev (as of 2026-03-27)` and
  `v3.0.0-beta` (identical trees), `v2.6.0-beta`, `feat/tron-swap-support
  (as of 2026-04-23)` and `feat/tron-gasfree (as of 2026-05-04)`.
  *Method:* reading plus mechanical comparison (word n-gram overlap at
  lengths 6, 5, 4 and 3, and a per-token forced-versus-free inventory);
  the repository's clean-room gate tool was not applicable because the
  additions embed no code blocks. *Measure:* 0.02, on the
  discretionary-expression basis.
- *Status and lab notes added 2026-11-20 (clean-side, Reloaded-original)
  -- the status note after T21.10B.18 and the lab note after the "Not
  established" paragraph of the compatibility block.* *Inputs:* the cleared
  text of R21.10B.10 -- R21.10B.13 and T21.10B.16 -- T21.10B.18 above; the
  public TRON full-node HTTP API (§21.9); answers of a public mainnet node
  and public mainnet receipts captured on 2026-11-20 (chapter 01 R6); and
  this repository's own code. *Forbidden corpus:* not consulted for these
  notes. No cleared sentence was edited.
- *Status notes added 2026-11-20 (clean-side, Reloaded-original) -- TRON
  switched on as a wallet-only coin family: the notes after the
  availability note at the top, after the "In this beta" box of §21.4,
  after R21.10B.9, at D6 and after R21.B3.* *Inputs:* the cleared text of
  R-D1, R21.10B.9, T21.10B.15, R21.B3 and F21.B3 above; the availability
  note of chapter 18 for the form of the swap switch; and this
  repository's own code. *Forbidden corpus:* not consulted for these
  notes. No cleared sentence was edited.
- *Scoped addition (2026-11-22, Spec Reader round 0.5.0-22) -- the block
  "EVM-family methods called on a TRX or TRC20 coin" at the end of §21.10B
  (R21.10B.14 -- R21.10B.21, the code-quality finding, the not-established
  paragraph, T21.10B.19 -- T21.10B.22) and the status note of 2026-11-22
  after R21.10B.9; no other text of this chapter was changed.* **KDF Dirty
  Gate: PASS on 2026-11-22, measure 0.03, no findings** (chapter 01 R40;
  the attestation closes this item).
  *Inputs:* (1) the forbidden corpus at the rows `v3.0.0-beta`,
  `v2.6.0-beta`, `feat/tron-swap-support (as of 2026-04-23)` and
  `feat/tron-gasfree (as of 2026-05-04)` of chapter 02 §2.15A, read
  read-only and consulted **only** to establish, for a TRX and a TRC20
  coin: whether `get_raw_transaction`, `sign_raw_transaction`, the legacy
  `get_trade_fee`, `trade_preimage`, `get_eth_estimated_fee_per_gas`,
  `stream::fee_estimator::enable` and `get_token_info` are reachable; with
  which `error_type` token and HTTP status, or which success shape, each
  ends; whether a request is sent to a node or to another service on the
  way; and the agreement of the rows on these points. Behavior, public
  method names, request and answer members, error tokens and statuses
  only; no private identifier, body, helper decomposition, error, log,
  comment or test text was reproduced, and the legacy error text is
  deliberately not recorded. (2) This repository's chapters 10, 43 and 55
  for the cross-references. No row was added to chapter 02 §2.15A: every
  state read already had one. The remedy sketched in the code-quality
  finding and the four acceptance rows are Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-22, measure
  0.03, no findings, in a single pass; the chapter 02 §2.15A rows were
  verified. *Extent:* the EVM coin family's handling of the seven public
  methods (raw-transaction fetch, raw-transaction signing, legacy trade fee,
  trade preimage, fee-per-gas estimate, fee-estimator stream, token info) as
  applied to a TRON-family coin, with the node-client selection, the
  gas-price and fee-estimate reads and the nonce read they depend on; the
  row `v3.0.0-beta` read directly (six source files); the rows
  `feat/tron-swap-support (as of 2026-04-23)`, `feat/tron-gasfree (as of
  2026-05-04)` and `v2.6.0-beta` compared mechanically against it over the
  same subsystem (13 files and directories), plus a search of the TRON
  handling of `v2.6.0-beta`; on the candidate side, the 223 added lines
  only. *Method:* reading plus mechanical comparison (state-to-state
  difference statistics and token search) and a per-token forced-versus-free
  test; the repository's clean-room gate tool was not run because the
  additions embed no code blocks. *Measure:* 0.03, on the
  discretionary-expression basis.
- *Status note added 2026-11-22 (clean-side, Reloaded-original) -- D7 of
  §21.11, the EVM-only RPC methods a TRON coin refuses.* *Inputs:* the
  cleared text of R21.10B.9 and R21.10B.10 above for what this chapter
  already records, R21.10B.14 -- R21.10B.21 and T21.10B.19 --
  T21.10B.22 (cleared 2026-11-22) for the comparison, and this
  repository's own code. *Forbidden corpus:* not consulted for this
  note. No cleared sentence was edited.
- *Status note added 2026-11-22 (clean-side, Reloaded-original) -- a
  saved TRON order is kept, at R21.10B.9.* *Inputs:* the maintainer's
  decision of 2026-11-22 on stored orders of untradable coins, the
  status note of that date at chapter 60 R60.6.5, and this repository's
  own code. *Forbidden corpus:* not consulted for this note. No cleared
  sentence was edited.
- *Status note added 2026-11-22 (Spec Reader round 0.5.0-26) -- after the
  2026-11-19 status note under R21.10B.4.* It qualifies, for the one method
  `task::get_new_address::init`, the "HD methods" item of R21.10B.4 from
  chapter 49 R49.38 and R49.42, read at the row `v3.0.0-beta` of chapter 02
  §2.15A; it travels with chapter 49 §49.2.3 for the KDF Dirty Gate. **KDF
  Dirty Gate: PASS on 2026-11-22, measure 0.03, no findings**, as recorded for
  §49.2.3 in chapter 49's provenance footer. No cleared sentence was
  edited.
- *Pointer added 2026-11-22 (Spec Reader round 0.5.0-27) -- §21.11B.7.* It
  names chapter 51 §51.6F (R93 -- R99) and states which of its points the
  TRON swap track mirrors; the reference facts are in that section, whose
  provenance record in chapter 51 §51.16 names the rows read. It carries no
  reference content of its own. No cleared sentence was edited. **KDF Dirty
  Gate: PASS on 2026-11-22, measure 0.03, no findings**, as recorded for
  §51.6F in chapter 51 §51.16.
- *Status notes added 2026-11-22 (clean-side, Reloaded-original) -- the
  32-byte secret-hash arm: the notes after R21.B6 and at D21.B2, D21.B3
  and D21.B4 of §21.11B.4.* *Inputs:* the cleared text of §21.11B.2,
  R21.B3, R21.B6 and D21.B2 -- D21.B4 above, the maintainer's decisions
  of 2026-11-08 on D21.B2 -- D21.B4, the cleared text of chapter 51 R71,
  R72 and R72A, and this repository's own code. *Forbidden corpus:* not
  consulted for these notes. No cleared sentence was edited.
