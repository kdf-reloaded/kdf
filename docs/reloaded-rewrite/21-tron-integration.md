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

**D21.10B.1** *TRC20 `fee_limit` policy.* The reference sets `fee_limit` to
the dry-run energy times the energy price with no margin; Reloaded today
sets a fixed cap (compatibility note). The cap is not visible to a peer.
Options: (a) the reference's figure; (b) the estimate plus a stated margin,
bounded by a stated maximum; (c) a fixed cap. The choice also fixes what
the fee details report as the most the transfer can cost (D21.B5 of
§21.11B.4 states the charging semantics).

**D21.10B.2** *`convertaddress` for a TRON coin.* The reference answers an
EVM `0x` form for a TRON coin. Options: (a) parity; (b) refuse the method
for TRX and TRC20; (c) convert between the base58check and the `41…` hex
form under a TRON-specific format value.

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

*Not established.* (1) Nothing was measured against a running reference
binary or a live node. (2) The exact text the legacy error body carries in
R21.10B.8 and R21.10B.9 is deliberately not recorded. (3) Whether the TRON
node's energy dry run includes the dynamic-energy penalty in
`energy_used` was not checked. (4) The swap-fee failure of R21.10B.9 for a
trading-capable coin was read, not executed.

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

**D21.B3 -- Interop scope.** Reloaded-to-Reloaded only (R21.B1), with the
compatibility documents saying so; cross-implementation rows in the lab
only when a counterparty release appears.

**D21.B4 -- Hash arm and swap id.** With (a) of D21.B2: 32-byte SHA-256,
chapter 51 R72 amended per R21.B6, swap id over the 32-byte secret hash.
With (b): decided with the contract.

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
