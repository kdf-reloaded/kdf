# Chapter 63 -- Tron GasFree Fee Delegation (Gasless TRC20 Withdraw)

**Status:** driving-spec.

> **Clearance (chapter 01 R40) -- passed the KDF Dirty Gate on
> 2026-11-08, measure 0.02 (attestation in §63.15).** This revision of
> the chapter was re-established on 2026-11-08 by the KDF Spec Reader
> from an earlier revision that lived on a parked branch of this
> repository, compared against the reference state `feat/tron-gasfree
> (as of 2026-05-04)` of chapter 02 §2.15A (base `dev (as of
> 2026-03-16)`). Passages that state a *reference fact* established in
> that re-establishment are marked as reference facts; passages that
> record a *Reloaded-only decision* are marked as decisions and take no
> reference input.

> **One-sentence claim:** the project shall let a user move a
> TRC20 token on Tron without holding TRX for resource fees, by
> deriving the user's deterministic GasFree custody address
> locally, preflighting transfer eligibility and fees against a
> third-party GasFree provider's HTTP API, and having the user
> sign an off-chain TIP-712 (EIP-712-shaped) "permit transfer"
> authorization whose fee is paid by the provider in the token
> itself -- conforming on the wire to the public GasFree provider
> REST API, the GasFree `PermitTransfer` typed-data domain, and
> the Tron `CREATE2` (`0x41`-prefixed) account-derivation rule;
> submitting the signed authorization to the provider and
> tracking its on-chain settlement are deferred (D63.6), so
> today's withdraw is **sign-only**.

## 63.0 Executive Summary

"GasFree" is a fee-delegation (meta-transaction) scheme for Tron
TRC20 tokens. Instead of paying the network's bandwidth/energy
resource cost in TRX (the model of [Chapter 21](21-tron-integration.md)
§21.8), the user authorizes a TRC20 transfer **off-chain** and a
third-party provider broadcasts it on-chain, charging its fee in
the same token. Each user has a deterministic per-network
**GasFree custody address** (a smart-contract account) derived by
`CREATE2`; the user funds that address with the token, and the
provider moves funds out of it on presentation of a valid signed
authorization.

This capability is a coin-support extension layered onto the
existing Tron integration of Chapter 21 (Tron lives inside the
EVM coin family). It adds: a platform-activation-time GasFree
provider binding; per-token gasless enablement; a local GasFree
address derivation surfaced wherever the project reports a Tron
address with its balance; a typed HTTP client for the provider's
public REST API; an account-preflight/eligibility layer; a TIP-712
`PermitTransfer` signer; and a new gasless branch in the withdraw
pipeline driven by a new `fee_method` request field.

The integration is **TRC20-only and sign-only today**. Gasless
applies exclusively to Tron TRC20 tokens (never native TRX, never
non-Tron EVM chains). The withdraw path preflights, signs the
authorization, and returns it as a typed off-chain payload with a
gasless fee breakdown; it does **not** yet submit the
authorization to the provider or track on-chain settlement.
Submission and settlement tracking are deferred work (D63.6).

> **Reference state (chapter 02 §2.15A).** The reference facts of this
> chapter describe the row `feat/tron-gasfree (as of 2026-05-04)` -- the
> tip of an open, unmerged draft of the historical record -- over its base
> `dev (as of 2026-03-16)`. No released designation of the reference
> carries this feature; a statement "at the reference row" below means at
> that tip and nothing later. Where this chapter binds something the
> reference row does not do, it says so and records it as a Reloaded
> decision.

> **Binding scope (R36).** Throughout this chapter, requirements
> bind observable *behavior*, the *public* RPC request/response
> shapes the workspace exposes (the withdraw, activation, and
> balance surfaces), and externally *dictated* interop -- the
> GasFree provider's public REST API (paths, the response
> envelope, the HMAC request-auth construction), the GasFree
> `PermitTransfer` TIP-712 typed-data domain and message layout,
> the Tron `CREATE2` (`0x41`) account-derivation rule and the
> published per-network GasFree contract artifacts, the TRC20
> `balanceOf` read, and the secp256k1 65-byte signature layout
> with EIP-712 `v` normalization. The private Rust types, struct
> field identifiers, enum variant names, helper decomposition,
> module file tree, local names, control flow, and diagnostic
> wording an implementation uses to meet these requirements are
> informative, not mandated. Where a fragment is reproduced
> because the GasFree provider API, the typed-data spec, or the
> Tron derivation rule dictates it, that is stated explicitly and
> distinguished from the discretionary Rust shape around it.

## 63.1 Baseline Verification

Tron GasFree fee delegation is a **post-2022** feature. It is
absent from the baseline anchor of
[Chapter 02](02-baseline-state.md) (the row `baseline anchor (R1)`,
3 June 2022): the baseline has no Tron coin-support at all (per
§21.1), and a fortiori no GasFree provider client, no gasless
withdraw rail, no `CREATE2` GasFree address derivation, and no
`fee_method`/gasless withdraw request fields. The classification
here is by *component role and first-introduction epoch only*; it
is derived from the project's own revision history and does **not**
transcribe code.

| Component (role) | Basis |
| --- | --- |
| GasFree provider HTTP client and typed API schema | Introduced after the baseline anchor. |
| `CREATE2` GasFree custody-address derivation | Introduced after the baseline anchor. |
| TIP-712 `PermitTransfer` signing | Introduced after the baseline anchor. |
| Gasless branch in the withdraw pipeline (`fee_method`, gasless options) | Introduced after the baseline anchor. |
| Platform/token GasFree activation config | Introduced after the baseline anchor. |

## 63.2 Subsystem Placement

GasFree support belongs **inside the Tron portion of the EVM coin
family**, as a self-contained module group adjacent to the Tron
fee/withdraw code of Chapter 21, not as a free-standing protocol
crate. It reuses the Tron address type, the Tron signature/hash
types, and the Tron node client (for the on-chain TRC20
`balanceOf` read) already specified by Chapter 21, and it reuses
the shared EVM key-policy and EIP-712 encoder. This placement is
informative: the binding-scope note above (R36) governs, and module
layout is not mandated.

Two pieces of cross-cutting infrastructure are extended in support
of GasFree and are specified by their own chapters, not here:

- a **cross-platform "POST JSON with custom request headers"**
  HTTP capability (native and WASM), required because the
  provider API needs per-request authentication headers; see
  [Chapter 26](26-cross-platform-and-wasm.md) for the
  HTTP-transport contract. This chapter only requires that such a
  capability exists and is used, and that it has the property
  R23 binds. *Cross-chapter note:* chapter 26's transport table
  (§26.8 R13) lists the GET-with-headers and plain POST-JSON
  accessors but not yet a POST-JSON-with-headers accessor; the
  table is to be extended when this rail is scheduled.
- the **shared EIP-712 encoder** is generalized so it can encode
  the GasFree `PermitTransfer` typed data (§63.6). The encoder is
  a public-spec implementation; this chapter requires only that it
  produces spec-correct typed-data hashes. (Informative, 2026-11-08:
  `PermitTransfer` uses only `address`, `uint256` and `string`
  typed-data members, so no encoder extension beyond
  spec-correct handling of those is needed for this chapter.
  Reloaded's encoder already encodes a `bytes32` member as the raw
  32 bytes and rejects a wrong length; it has no dynamic `bytes`
  member type, which this chapter does not need.)

## 63.3 Public RPC and Activation Surface

This section specifies the *public contract* the workspace
exposes for GasFree. The JSON field names below are part of that
public API and are binding; the Rust types behind them are not.

**Placement (binding).** The withdraw fee-rail fields (§63.3.4)
and the balance custody-address field (§63.3.3) MUST be exposed on
the **shared, cross-coin** withdraw-request and balance-report
structures of the workspace (the same request the standard
withdraw RPC deserializes and the same reports the standard
balance and address-management RPCs serialize), not on a
Tron-local request/response type. The fields are inert
(`None`/omitted) for every non-Tron coin, but the public JSON
contract is the shared one: a caller drives the gasless rail
through the ordinary withdraw RPC and reads `gasfree_address`
from the ordinary balance and activation responses. Likewise the
gasless withdraw error family (§63.9) MUST be a variant of the
shared withdraw-error type so its status mapping flows through the
standard dispatcher. Surfacing these only through a Tron-internal
type would leave the §63.3 contract unexposed and is
non-conforming.

### 63.3.1 Platform activation (dictated by our public API)

The EVM/Tron platform-activation request (the mmrpc 2.0
platform-with-tokens request of [Chapter 35](35-evm-v2-activation-rpcs.md),
in both its one-shot and its task form) gains an optional
provider binding, valid only for Tron chains and rejected for
non-Tron EVM chains:

- `tron_gasless_provider` (optional object):
  - `base_url` -- the provider's URL, scheme and host only. A
    value that carries a path is rejected at activation; the
    network segment and the `api/v1` prefix of §63.4.1 are added
    by the implementation when each request is formed.
  - `api_key` -- provider API key.
  - `api_secret` -- provider API secret (used for request signing,
    §63.4.2).
  - `service_provider` -- the base58check Tron address of the
    fee-paying provider. Activation fails when the value is not a
    well-formed Tron address; a well-formed value is later copied
    into the `serviceProvider` member of every authorization the
    user signs (§63.6).
  - `request_timeout_ms` (optional) -- per-call HTTP timeout.
    Default `15000` (15 s). *(Reference fact at the row
    `feat/tron-gasfree (as of 2026-05-04)`.)*
  - `status_poll_interval_ms` (optional) -- settlement poll
    interval. Accepted today; it has an effect only once the
    submission flow of D63.6 exists. Default `3000` (3 s).
    *(Reference fact at the same row.)*

  The two credential values are secrets. An implementation keeps
  them off every surface a caller or an operator can read other
  than the provider request itself: daemon log lines, diagnostic
  renderings of the configuration, and any RPC response that
  reflects activation parameters (R9).

  Presence of the object on a non-Tron EVM platform MUST be
  rejected with the activation request's invalid-payload error.

### 63.3.2 Token activation (dictated by our public API)

A Tron TRC20 token-activation request (one-shot and task form)
gains an optional gasless block:

- `gasless` (optional object):
  - `enabled` (bool, default false) -- opt the token into the
    gasless rail. A block with `enabled: false` behaves exactly as
    an absent block.
  - `transfer_max_fee` (optional decimal) -- an activation-time
    cap, in token units, on the provider fee the user will accept;
    must be non-negative. It is converted to token base units at
    activation (so a value finer than the token's decimals is a
    rejected payload).

  A token gasless block is valid only on Tron chains, and only
  when the platform has a `tron_gasless_provider`; otherwise
  activation is rejected with an invalid-payload error.

### 63.3.3 Where the custody address is reported (dictated by our public API)

The locally derived (§63.5) GasFree custody address is reported
to the caller as an optional string field `gasfree_address`
(base58check Tron form), populated only for a Tron coin whose
platform has a configured GasFree provider and **omitted**
otherwise (never `null`). It appears next to the address it
belongs to in every place the project reports a Tron address with
its balance *(reference fact at the row
`feat/tron-gasfree (as of 2026-05-04)`)*:

- the legacy `my_balance` response (beside `address`);
- every per-address entry of an HD balance report, and the
  single-address balance report -- hence inside the
  platform-with-tokens activation result (both the platform's and
  the token's address-information block), the task-based token
  activation result and the one-shot single-token activation
  result;
- the responses of the HD address-management RPCs
  `get_new_address`, `account_balance`, `init_account_balance`,
  `init_scan_for_new_addresses` and `init_create_account`.

A placement that reports a Tron address with its balance but
without the field, while the provider is configured, is
non-conforming. (Informative: at the reference row the field is
carried by the shared cross-coin balance-report structures as an
interim choice, attached by each reporting site rather than by the
report builder. See the code-quality finding in §63.10.)

### 63.3.4 Withdraw request (dictated by our public API)

The withdraw request (the shared request of
[Chapter 49](49-withdrawal-task-path.md), direct and task form) gains
a fee-rail selector and gasless constraints:

- `fee_method` (optional enum): `native` (default behavior;
  unchanged), `gasless` (require the gasless rail), or `auto`
  (the native rail first; the gasless rail only when the native
  rail cannot be funded -- §63.8 step 1).
- `gasless` (optional object), only meaningful with
  `fee_method = gasless | auto`; supplying it with `native`/absent
  `fee_method` is rejected as a fee-validation error (HTTP 400):
  - `max_fee` (optional decimal) -- per-request cap, in token
    units, on the accepted provider fee.
  - `deadline_seconds` (optional integer) -- authorization
    validity window from signing time; must be greater than zero;
    a default window applies when omitted (§63.8 step 3).
  - `fallback_to_native` (bool, default false) -- with
    `fee_method = gasless`, permit silently falling back to the
    native rail when the gasless rail is deterministically
    unavailable.

On a coin that is **not** a Tron coin, `fee_method` `gasless` or
`auto` MUST be refused with the gasless *unavailable* error of
§63.9 (HTTP 503), and `gasless` options with `native`/absent
`fee_method` MUST be refused as above, before any other
processing of the request. *(Reference fact at the row
`feat/tron-gasfree (as of 2026-05-04)`.)*

### 63.3.5 Withdraw response (gasless fee details, dictated by our public API)

When a withdraw is routed through the gasless rail, its fee
details object carries a distinct shape from the native Tron
resource-fee details of Chapter 21:

- `coin`, `fee_method` (value: `gasless`), `provider_name`
  (value: `gasfree`), `gasfree_address`, `transfer_fee`,
  `activation_fee`, `total_token_fee`, `signed_max_fee`
  (optional), `trace_id` (optional; `null` on the sign-only
  path). All fee amounts are in token units; the fee is paid in
  the token, not in TRX.

The shared fee-details object is type-tagged on the wire; the
reference row introduces a **new tag value** for this shape
(`TronGasless`) beside the native Tron tag. Whether Reloaded adopts
that exact tag is decision D63.2 (§63.12A); until it is taken the
tag is recorded here as the reference's public interface, not as a
requirement. *(Reference fact.)*

### 63.3.6 Long-running withdraw progress phases

The reference's long-running (`task::*`) withdraw status
vocabulary carries five gasless phases, reported to the caller as
the operation progresses: fetching the gasless quote; signing the gasless
authorization; submitting the authorization; waiting on the
provider (carrying a `trace_id` and a provider-reported state
string); and waiting for on-chain settlement (carrying a
`trace_id` and an optional transaction hash). The submit/wait
phases exist for the deferred submission flow (§63.8); the
sign-only path emits the quote/sign phases only.

The reference row names these five phases, in the status
enumeration's wire vocabulary, `FetchingGaslessQuote`,
`SigningGaslessAuthorization`, `SubmittingGaslessAuthorization`,
`WaitingForGaslessProvider` and `WaitingForGaslessSettlement`.
Whether Reloaded adopts these exact values is decision D63.2
(§63.12A). *(Reference fact.)*

## 63.4 GasFree Provider REST API (dictated interop)

The provider API is an external, public REST service. The
fragments in this section are reproduced because they are the
bytes and calls any client must exchange to interoperate; their
authoritative source is the GasFree provider's published API, not
any project lineage.

### 63.4.1 Endpoints

All paths below are appended to the per-network request base
described after the table and share the prefix `api/v1`:

| Operation | Method + path |
| --- | --- |
| Supported tokens (with provider fees) | `GET api/v1/config/token/all` |
| Account info (custody address, nonce, assets, activation flag) | `GET api/v1/address/{account_address}` |
| Submit a signed transfer authorization | `POST api/v1/gasfree/submit` |
| Trace a submitted transfer by id | `GET api/v1/gasfree/{trace_id}` |

The URL configured at activation names the scheme and host only
(R13). Every request path begins with a network segment -- `tron`
on Tron mainnet, `nile` on the Nile testnet, `shasta` on the
Shasta testnet -- followed by the `api/v1` prefix, so that on
mainnet the supported-tokens call is
`<host>/tron/api/v1/config/token/all`. The network segments are
dictated by the GasFree provider's public developer documentation
(§63.14). The supported-token list MAY
be cached for the lifetime of the activated coin.

### 63.4.2 Request authentication (dictated)

Each request carries two headers:

- `Timestamp` -- the current Unix time in seconds.
- `Authorization` -- of the form `ApiKey {api_key}:{signature}`,
  where `{signature}` is the Base64 encoding of an HMAC-SHA256,
  keyed by `api_secret`, computed over the ASCII concatenation of
  the HTTP method, the request path, and the timestamp (in that
  order, no separators).

An implementation MUST match this construction byte-for-byte to
authenticate; because the construction is a deterministic function
of a fixed (method, path, timestamp, api_key, api_secret), it MUST
be validated against a fixed auth vector (the expected
`ApiKey {api_key}:{signature}` string for known inputs is
self-derivable from the algorithm and need not come from a
provider-published vector). The credential material MUST stay out
of logs and `Debug`.

### 63.4.3 Response envelope (dictated)

Every response body is a JSON envelope with: an integer `code`, an
optional `reason`, a `message`, and an optional `data` payload. A
**business-success** response carries `code = 200` and a present
`data`; any other `code`, or a `data`-less success, is an error
even when the HTTP status is 200. Error envelopes (or non-2xx HTTP
statuses) map to typed errors by status class (§63.9). Provider
message text included in surfaced errors MUST be sanitized:
whitespace-collapsed and cut to at most **256 characters**
*(reference fact at the row `feat/tron-gasfree (as of
2026-05-04)`, re-verified 2026-11-08.
An earlier revision labeled the figure a Reloaded-only choice;
that label was wrong and is withdrawn.)*

### 63.4.4 Account info payload (dictated)

`GET .../address/{account}` returns, within `data`: the queried
`accountAddress`; the `gasFreeAddress` (the provider's view of the
custody address); an `active` flag (whether the custody account is
on-chain activated); a `nonce` (bound into the next
authorization); an `allowSubmit` flag; and an `assets` array, each
asset carrying `tokenAddress`, `tokenSymbol`, `activateFee`,
`transferFee`, `decimal`, and a `frozen` (in-flight-locked)
amount.

### 63.4.5 Supported-tokens payload (dictated)

`GET .../config/token/all` returns an array of entries each with
`tokenAddress`, `activateFee`, `transferFee`, `symbol`, `decimal`,
and a `supported` flag. Results may be cached for the session.

### 63.4.6 Submit request payload (dictated)

`POST .../gasfree/submit` sends the signed authorization as JSON.
Integer-valued fields are serialized as decimal **strings**;
addresses as base58 Tron addresses; the signature as a 65-byte
hex string (130 hex chars, no `0x`). Fields: an optional
`requestId` (a UUIDv4, rejected if not version 4); `token`;
`serviceProvider`; `user`; `receiver`; `value`; `maxFee`;
`deadline`; `version` (must equal 1); `nonce`; and `sig`.

### 63.4.7 Submit/trace response payloads (dictated)

The submit response and the trace response carry the provider's
view of the transfer: identifiers (`id`, and on trace the
`txnHash` once on-chain), the participating addresses
(`accountAddress`, `gasFreeAddress`, `providerAddress`,
`targetAddress`, `tokenAddress`), `amount`, fee estimates
(`estimatedActivateFee`, `estimatedTransferFee`, and on trace
`estimatedTotalFee`/`estimatedTotalCost` plus realized
`txn*Fee`/`txnTotalCost` fields), `nonce`, `version` (must be 1),
timing (`expiredAt`, `createdAt`, `updatedAt`), and lifecycle
state.

Transfer lifecycle `state` is one of: `WAITING`, `INPROGRESS`,
`CONFIRMING`, `SUCCEED`, `FAILED`. On-chain `txnState` (trace) is
one of: `INIT`, `NOT_ON_CHAIN`, `ON_CHAIN`, `SOLIDITY`,
`ON_CHAIN_FAILED`. A `state` or `txnState` value outside these
sets MUST fail deserialization of the payload; the response is then
an invalid provider response (§63.9.1), not a parsed one.

> **Dictated quirk.** The provider API spells the transfer-fee
> estimate inconsistently across endpoints: a client MUST accept
> **both** `estimatedTransferFee` and the variant spelling
> `estimateTransferFee` (without the trailing `d`) as the same
> field. Integer-valued provider fields may arrive as either JSON
> numbers or decimal strings, and a client MUST accept both forms;
> a negative or empty value is rejected. A signature or transaction
> hash carrying a `0x` prefix, or of the wrong length, is rejected.

## 63.5 GasFree Custody-Address Derivation -- Tron `CREATE2` (dictated interop)

The custody address is derived **locally and deterministically**
so the wallet can verify the provider's reported address and so a
user can compute their receive address without trusting the
provider. The derivation is dictated by the Tron `CREATE2` rule
and the published per-network GasFree contract artifacts; the
algorithm is reproduced because it must be byte-exact for the
address to be correct.

Per network the derivation uses three published constants: the
GasFree **controller** contract address, the **beacon** contract
address, and the proxy **creation bytecode**. All three are
published for every network in the **public GasFree JavaScript
SDK** (the `gasfree-sdk-js` repository of the GasFree project on
GitHub; see §63.14), which is a public specification for the
purposes of this chapter: this chapter references the bytecode by
its source rather than embedding it, and an implementation MUST use
the exact per-network proxy creation bytecode. *Sourcing rule
(Reloaded-original; the maintainer's decision of 2026-11-09; takes no
reference input):* Reloaded takes each network's creation bytecode
from the public chain -- the input of the transactions that deployed
the GasFree contracts on that network (chapter 01 R6) -- and uses the
bytecode the SDK publishes only as a cross-check; a test pins the hash
of the bytecode Reloaded embeds and compares it with the SDK's. The
SDK's vectors are used as cited data; no SDK code is used. The
published per-network constants (dictated interop, base58 Tron
addresses) are:

| Network | Controller (`verifyingContract`) | Beacon |
| --- | --- | --- |
| Mainnet | `TFFAMQLZybALaLb4uxHA9RBE7pxhUAjF3U` | `TSP9UW6FQhT76XD2jWA6ipGMx3yGbjDffP` |
| Nile | `THQGuFzL87ZqhxkgqYEryRAd7gqFqL5rdc` | `TLtCGmaxH3PbuaF6kbybwteZcHptEdgQGC` |
| Shasta | `TQghdCeVDA6CnuNVTUhfaAyPfTetqZWNpm` | `TQ1jvA3nLDMDNbJoMPLzTPoqAg8NvZ5CCW` |

The `initialize(address)` selector used in step 2 is `0xc4d66de8`
(the leading 4 bytes of keccak-256 of the ASCII function
signature). An implementation MUST bind exactly these published
constants and the SDK's per-network creation bytecode (the Shasta
bytecode differs from the mainnet and Nile bytecode); the T1
vectors of §63.11 are the acceptance check for that binding. The
steps:

1. **Salt** = the user's 20-byte EVM-form address, right-aligned
   (left zero-padded) into 32 bytes.
2. **init calldata** = the 4-byte selector of `initialize(address)`
   followed by the salt.
3. **init code** = creation bytecode concatenated with the
   ABI-encoding of (beacon address, init calldata).
4. **init-code hash** = keccak-256 of the init code.
5. **preimage** = the single byte `0x41` (Tron's `CREATE2`
   prefix, where Ethereum uses `0xff`) `||` controller address
   (20 bytes) `||` salt (32 bytes) `||` init-code hash (32 bytes).
6. **address** = the last 20 bytes of keccak-256(preimage),
   re-encoded as a Tron address.

The derivation MUST be validated against the custody-address
vectors the public GasFree SDK publishes: **five** user-to-custody
pairs for each of mainnet, Nile and Shasta (the same five user
addresses on every network; for the Nile pairs the SDK also
publishes the intermediate salt and init-code hash, which lets a
failing derivation be localised to a step). The controller, the
beacon and the creation bytecode are bound per network by the
implementation and are not caller-configurable.

## 63.6 `PermitTransfer` Signed Authorization -- TIP-712 (dictated interop)

The user authorizes a transfer by signing GasFree's
`PermitTransfer` structured data, Tron's TIP-712 (the
EIP-712-shaped) typed-data scheme. The domain and message layout
below are dictated by the GasFree protocol and are reproduced
because the signature is only valid if the typed data is
byte-exact.

- **Domain**: `name` = `GasFreeController`, `version` = `V1.0.0`,
  `chainId` = the network's EIP-712 chain id, `verifyingContract`
  = the per-network controller (§63.5). The dictated per-network
  chain ids are: Tron mainnet `728126428`, Shasta testnet
  `2494104990`, Nile testnet `3448148188`. (These are the
  well-known Tron network chain ids; they are also the values the
  signed authorization's relay payload reports.)
- **Primary type**: `PermitTransfer`, with fields in order:
  `token` (address), `serviceProvider` (address), `user`
  (address), `receiver` (address), `value` (uint256), `maxFee`
  (uint256), `deadline` (uint256), `version` (uint256, value 1),
  `nonce` (uint256).

Signing obligations (behavioral):

- the typed-data hash MUST be validated against the published
  GasFree `PermitTransfer` test vectors before the signer is
  trusted. The public GasFree SDK publishes them for the **Nile
  testnet only**: one domain separator and five message vectors
  (each giving the struct hash and the full typed-data hash). No
  mainnet or Shasta vector is published; there the domain
  separator follows from the same EIP-712 construction with the
  per-network chain id and controller, and a lab row MAY
  cross-check a mainnet or Shasta hash against the controller
  contract's public read-only message-hash function (the SDK's
  own test suite checks its Nile hashes that way);
- the signing key MUST correspond to the `user` address; a
  mismatch is refused (the wallet must not sign a transfer that
  debits an address it does not control);
- the `deadline` MUST be in the future at signing time; an expired
  deadline is refused;
- the result is a 65-byte secp256k1 signature; its recovery byte
  `v` MUST be normalized to the EIP-712 `27`/`28` convention;
- the signed material exposes addresses, amounts, fee cap,
  deadline, and nonce, but the raw signature MUST be redacted in
  `Debug`.

The signed authorization carries exactly the fields the provider
submit payload requires (§63.4.6).

## 63.7 Account Preflight and Availability Semantics

Before signing, the gasless rail performs a **preflight** that
fetches provider account state and the on-chain TRC20 balance of
the custody address, and decides whether the transfer can proceed.
The preflight is a behavioral contract; the decision categories
below are normative, their internal representation is not.

Inputs gathered: the provider's account info (§63.4.4); the
on-chain TRC20 `balanceOf` of the custody address (read via the
Tron node client of Chapter 21); and the locally derived custody
address (§63.5).

*(This subsection was re-established on 2026-11-08 in the project's
own words and structure.)*

**Outcome classes.** The preflight ends in exactly one of three
classes, and §63.8 and §63.9 consume nothing finer:

- *proceed* -- none of the conditions below holds; the rail goes on
  to sign;
- *wait* -- the provider's account record has its submit-permission
  flag clear. This class is decided from the account record alone
  and MUST NOT depend on the on-chain balance read;
- *refuse* -- one of the four refusal conditions below holds.

Only *token not enrolled* and *shortfall* count as *deterministic
unavailability* for the fallback of §63.8 step 6; the other two
refusal conditions and *wait* do not.

**Refusal conditions.** The listing order is not an evaluation
order.

| Condition | What was checked | Reported as (§63.9.2) |
| --- | --- | --- |
| *Shortfall* | one of the two funds tests below fails | the shared insufficient-balance error, with the custody account's figures |
| *Token not enrolled* | the asset list of the account record has no entry for the token's contract address | the rail unavailable |
| *Precision disagreement* | the asset entry's decimal count is not the activated token's decimals | an untrustworthy provider answer |
| *Custody mismatch* | the custody address in the account record is not the address §63.5 derives locally | an untrustworthy provider answer |

A refusal produces no signature. None of the four is a silent
fallback trigger except through §63.8 step 6, and only for the two
deterministic conditions named above.

**Funds tests.** Let *value* be the requested amount, *transfer
fee* and *activation fee* the figures the account record carries
for the token, *frozen* the amount it reports as locked, and
*balance* the on-chain TRC20 balance of the custody address. The
activation fee counts only while the account record says the
custody account is not yet activated on-chain; once activated it is
zero. Two tests are applied:

1. *balance* − *frozen* ≥ *value* + *transfer fee* + *activation
   fee*, for every account;
2. *balance* ≥ *value* + *transfer fee* + *activation fee* +
   *frozen*, for a not-yet-activated account only.

The two inequalities coincide arithmetically whenever *frozen* does
not exceed *balance*; they are kept apart because the figures
reported on failure differ (§63.9.2: the second test reports the
whole on-chain balance as *available* and includes *frozen* in
*required*), and because for a not-yet-activated account the second
test is the one evaluated first.

**Ordering properties (binding).** Two orderings are binding and
no other is: the custody-identity check comes before every other
check, so that no later figure is taken from an account record that
describes some other account; and the *wait* decision needs no
on-chain read.

## 63.8 Gasless Withdraw Behavior (sign-only today)

The withdraw pipeline gains a gasless branch selected by
`fee_method` (§63.3.4). Behavior:

1. **Rail selection.** `native` is unchanged. `gasless` requires
   the gasless rail and tries it first. `auto` tries the **native**
   rail first and turns to the gasless rail only when the native
   rail fails for a *funding* reason -- insufficient token balance,
   insufficient platform (TRX) balance for the resource fee, or an
   amount below the minimum -- and the request is not a max-amount
   request; a non-funding
   failure of the native rail is returned as is. Which error a
   failed `auto` request reports depends on whether the gasless
   rail was *attempted*: if the rail is not applicable to the coin
   (native TRX; no provider binding resolvable for the coin; a
   token activated without gasless enablement), the **native
   rail's** error is returned; if the rail was attempted and failed
   (a preflight refusal or wait, a fee-cap breach, a provider or
   transport failure), the **gasless rail's** error is returned.
   Supplying gasless options with the native rail is rejected. A
   max-amount ("withdraw everything") request is **not supported**
   on the gasless rail (the provider fee is taken from the same
   token balance, so "max" is ambiguous), and `auto` never routes
   one there. *(Reference fact at the row `feat/tron-gasfree (as
   of 2026-05-04)`, re-verified 2026-11-08. An earlier revision of this chapter described `auto` the
   other way round -- gasless first, native as the fallback -- and
   the previous candidate stated that a failed gasless attempt
   under `auto` reports the native error; neither matches the
   reference row and both are withdrawn. See decision D63.1.)*
2. **Eligibility.** The gasless rail applies only to a Tron TRC20
   token whose platform has a configured GasFree provider and
   whose token activation enabled gasless. For a token, the
   provider binding is resolved from its platform coin. Native TRX
   and non-Tron EVM coins never use the rail: `fee_method =
   gasless` on native TRX is refused as *unavailable* (§63.9), and
   on a non-Tron coin as §63.3.4 states.
3. **Quote.** Under the sender address's nonce lock (so that the
   provider nonce bound into the authorization cannot be raced by
   another send from the same address), the rail runs the
   preflight (§63.7), computes `total_token_fee = transfer_fee +
   activation_fee`, and applies the **effective fee cap** = the
   minimum of any present caps (the per-request `max_fee` and the
   activation-time `transfer_max_fee`); if neither is present the
   quoted total fee itself is used. If the quoted total exceeds
   the effective cap, the transfer is refused (max-fee-exceeded).
   The signed authorization's `maxFee` is set to the effective
   cap. The `nonce` is the one the provider's account record
   reports. The `deadline` is "now + window", where the window is
   the per-request `deadline_seconds` or, when omitted, a default
   of **300 s** *(reference fact)*;
   a `deadline_seconds` of zero is a fee-validation error (400).
   A requested amount of **zero** is refused as a fee-validation
   error (400) before any provider call *(reference fact)*.
4. **Sign.** The rail signs the `PermitTransfer` (§63.6) with the
   key of the sending address: under an HD wallet, the key at the
   request's `from` path; under the single-key policy, the
   activated key. A signing policy that holds **no local key**
   (hardware wallet, WalletConnect, browser-extension wallet) never
   reaches this step: the Tron withdraw path refuses it before any
   balance read, before any provider call and before either rail
   executes (R22).
5. **Result (sign-only).** The withdraw returns the signed
   authorization as a typed off-chain payload (carrying the
   signed permit, the participating addresses, the derived custody
   address, and a creation timestamp) wrapped as an *unsigned*
   transaction artifact -- i.e. it is **not** broadcast on-chain
   and **not** yet submitted to the provider -- together with the
   gasless fee details (§63.3.5). The off-chain payload's internal
   Rust type name is discretionary, but its serialized field set
   is part of the returned artifact and is informatively: a
   relay-type discriminator (a fixed wire string identifying the
   Tron GasFree off-chain rail; `tron_gasfree` at the reference
   row -- decision D63.2), the network chain id as a decimal
   string, the coin ticker, the optional HD `from` selector, the
   from address, the derived custody (GasFree) address, the
   per-network controller (`verifyingContract`), the creation
   timestamp (RFC 3339, whole seconds), and a nested **signed
   authorization** object. At the reference row (verified
   2026-11-08) that nested object carries the nine `PermitTransfer`
   values and the signature under **snake_case** member names --
   `token`, `service_provider`, `user`, `receiver`, `value`,
   `max_fee`, `deadline`, `version`, `nonce`, `sig` -- which is
   *not* the camelCase spelling of the provider submit payload
   (§63.4.6); the two are different documents. Integer members are
   serialized as decimal strings, `version` must read back as
   exactly 1, unknown members are rejected when the payload is read
   back, and the raw signature is redacted in `Debug` (R9). Whether
   Reloaded adopts these member names is part of decision D63.2.
6. **Fallback.** With `fee_method = gasless` and
   `fallback_to_native = true`, a *deterministic* unavailability
   (the rail is not configured for the coin, the token is not
   enrolled, or the custody balance is insufficient -- §63.7
   outcome classes) falls back to the native rail instead of
   erroring; a custody mismatch, a precision disagreement, an
   in-flight transfer and every provider/transport error still
   surface. Without `fallback_to_native` every unavailability
   surfaces as the §63.9 error.

**Not yet done: submission and settlement.** Today's withdraw
ends at step 5. It does not POST the authorization to the provider
and it does not poll for settlement; nothing past the signed
artifact is observable at any RPC or in any lab row of this
chapter. Submission and settlement tracking are deferred work
(D63.6). The wire shapes of the submit and trace exchanges
(§63.4.6, §63.4.7) and their tests (T5, T6) are binding now, so
that D63.6 adds a call and not a contract; whether an
implementation exercises those exchanges before D63.6 lands is not
observable and is not bound.

## 63.9 Error Taxonomy and Status Codes

*(This section was re-established on 2026-11-08: at the reference
row only the family of §63.9.1 is visible at the withdraw RPC;
every other provider-interaction failure collapses onto shared arms
as §63.9.3 states. Reference facts.)*

### 63.9.1 The public gasless error family (withdraw RPC)

The gasless rail introduces one public error family whose
**HTTP status mapping is the observable contract**. It is nested
under the shared withdraw error: the outer `error_type` names the
gasless family, and `error_data` is an object whose own
`error_type` names the member. The members and their statuses:

| Member (reference wire value) | Meaning | HTTP |
| --- | --- | --- |
| `Unavailable` | no gasless rail exists for the coin the request names: a non-Tron coin, native TRX, a platform without a provider binding, a token activated without gasless enablement, or a token the provider's account record does not list (§63.7 *token not enrolled*) | 503 |
| `PendingTransfer` | the preflight ended in the *wait* class of §63.7: the provider's account record withholds submit permission while an earlier transfer from this account is unsettled | 409 |
| `MaxFeeExceeded` | the quoted provider fee exceeds the effective cap | 400 |
| `QuoteExpired` | the quote's deadline passed before use | 400 |
| `ProviderRejected` | the provider refused the request (a provider 4xx other than authentication, authorization or rate limiting) | 502 |
| `InvalidProviderResponse` | the provider's answer could not be trusted or decoded; also the custody-mismatch and precision-disagreement outcomes of §63.7 | 502 |
| `TraceNotFound` | a trace id the provider does not know (deferred submission flow) | 404 |

The outer family name at the reference row is `Gasless`. Whether
Reloaded adopts the outer and inner wire values exactly is decision
D63.2 (§63.12A); the statuses are binding regardless.

### 63.9.2 Preflight outcome to error (binding)

| §63.7 outcome | Error at the withdraw RPC |
| --- | --- |
| token not enrolled | `Unavailable` (503) |
| custody mismatch; precision disagreement | `InvalidProviderResponse` (502) |
| transfer in flight | `PendingTransfer` (409) |
| shortfall (either funds test) | the **shared** insufficient-balance withdraw error of [Chapter 49](49-withdrawal-task-path.md) (`NotSufficientBalance`, 400), computed on the **custody** account: `available` = the custody spendable balance (or, for a not-yet-activated account, the custody on-chain balance) and `required` = value + fee (plus *frozen* for a not-yet-activated account), all in token units |

Reloaded MAY make the custody shortfall distinguishable from a
shortfall of the user's own account (decision D63.3), because the
balance a user sees in the wallet is not the one that was short.

### 63.9.3 Provider-interaction failures (collapse at the RPC)

An implementation will distinguish more failure kinds internally
than the withdraw RPC ever shows. Those finer kinds are **not a
public contract**, and this chapter does not enumerate them. What
is binding is the collapse: at the withdraw RPC a failed provider
interaction is reported through exactly one of four arms, chosen by
the observable condition. Where the provider answers with a JSON
envelope whose `code` is a status-like value (100–599) other than
200, that `code` takes the place of the HTTP status in the
classification below, whatever the HTTP status was.

| Withdraw RPC error | Conditions that produce it |
| --- | --- |
| `InvalidProviderResponse` (502) | the provider's answer cannot be used: an undecodable body; a lifecycle value outside the sets of §63.4.7; a malformed hash or signature member; a `version` other than 1; a success envelope (`code` 200) with no `data` |
| `ProviderRejected` (502) | a 4xx status (or status-like envelope `code`) other than 401, 403 or 429 |
| the shared withdraw `Transport` arm of Chapter 49, with that arm's own status | a 401, 403 or 429 status (or status-like envelope `code`); a 5xx status; a failed connection; the per-call timeout elapsing |
| the shared withdraw `InternalError` arm of Chapter 49 | a request the client could not form; an internal failure |

A 2xx answer whose envelope `code` is neither 200 nor a status-like
value is reported as `InvalidProviderResponse` (502), like any other
answer the client cannot use. *(Reloaded decision; takes no reference
input.)*

A caller therefore cannot tell an expired API key from a provider
outage at the withdraw RPC; the finer kind belongs in the daemon
log (with credentials redacted, R9), not on the wire.

### 63.9.4 Request-shape rejections

Gasless options with the native rail, a zero `deadline_seconds`,
an unparsable `max_fee`, and a zero amount are fee-validation
errors (HTTP 400). In Reloaded they are reported through the
shared fee-descriptor arm of Chapter 49 (`InvalidFeePolicy`); the
reference row reports them through its own fee-validation arm of
the same status. A max-amount gasless request is an
unsupported-operation refusal (Chapter 49 R49.27a, `UnsupportedError`,
400).

## 63.10 Binding Requirements

R1. **TRC20-only, Tron-only.** The gasless rail MUST apply only to
    Tron TRC20 tokens. It MUST refuse native TRX and any non-Tron
    EVM chain. A token gasless config without a platform provider,
    or on a non-Tron chain, MUST be rejected at activation.

R2. **The provider's custody address is checked, never trusted.**
    The implementation derives the custody address itself (§63.5)
    and requires the `gasFreeAddress` of the provider's account
    record to equal it before any other figure of that record is
    used; on inequality nothing is signed and the request fails as
    §63.9.2 states. The controller, the beacon and the creation
    bytecode are fixed per network inside the implementation; no
    caller input changes them.

R3. **Spec-faithful typed data.** The `PermitTransfer` domain and
    message (§63.6) and the `CREATE2` derivation (§63.5) are set
    by the GasFree protocol and the Tron rule; any divergence from
    the published vectors is a bug, not a design choice.

R4. **Signer is the user.** The signing key MUST correspond to the
    `PermitTransfer` `user` address; otherwise the rail refuses.

R5. **Version pinned to 1.** The authorization `version` MUST
    equal 1, on both the outbound submit payload and inbound
    provider payloads; any other value MUST be rejected.

R6. **Signature shape.** The signature MUST be 65 bytes with `v`
    normalized to `27`/`28`, serialized to the provider as 130 hex
    characters without `0x`.

R7. **Deadline discipline.** `deadline_seconds` MUST be greater
    than zero; the signed `deadline` MUST be in the future at
    signing time; an expired deadline MUST be refused.

R8. **Fee cap honored.** The accepted provider fee MUST NOT
    exceed the effective cap (the minimum of the present
    per-request and activation-time caps); a breach MUST refuse
    the transfer. The signed `maxFee` MUST equal the effective
    cap.

R9. **Secrets off every readable surface.** The provider
    credentials of §63.3.1 reach only the provider request headers
    of §63.4.2, and the raw bytes of a `PermitTransfer` signature
    reach only the signed-authorization artifact the withdraw
    returns (§63.8 step 5) and the provider submit payload
    (§63.4.6). Daemon log lines, diagnostic renderings of a
    configuration or payload, and every other RPC response carry
    neither.

R10. **Provider-text sanitization.** Any provider-supplied message
    folded into a surfaced error MUST be whitespace-collapsed and
    cut to at most 256 characters (§63.4.3). *(Reference fact.)*

R11. **Dictated-auth fidelity.** The request-auth header
    construction (§63.4.2) and the success-envelope semantics
    (§63.4.3) MUST match the provider API exactly; a 200 HTTP
    status with a non-200 envelope `code` or absent `data` is an
    error.

R12. **Strict provider-enum parsing.** A transfer or transaction
    lifecycle value outside the sets of §63.4.7 MUST fail
    deserialization and be reported as an invalid provider
    response (§63.9.1).

R13. **Host-only base URL.** The configured `base_url` MUST be
    host-only; the network path segment (§63.4.1) is derived, and
    a caller-supplied path MUST be rejected at activation.

R14. **`max` is unsupported on the rail.** A max-amount gasless
    withdraw MUST be rejected; `auto` MUST NOT route a max-amount
    request through the rail.

R15. **Public RPC field stability.** The request/response JSON
    field names of §63.3 are the public contract; changes must be
    additive and backward-compatible.

*R16–R24 were added in the re-establishment of 2026-11-08. Those
stating a reference fact are marked and await the KDF Dirty Gate;
R24 is Reloaded-original and takes no reference input.*

R16. **`auto` is native-first.** Under `fee_method = auto` the
    native rail MUST be attempted first; the gasless rail MUST be
    attempted only after a *funding* failure of the native rail
    (§63.8 step 1) and never for a max-amount request. When the
    gasless rail is not applicable to the coin, the native rail's
    error MUST be returned; when the gasless rail was attempted and
    failed, the gasless rail's error MUST be returned. *(Reference
    fact, re-verified 2026-11-08.
    Subject to decision D63.1.)*

R17. **Provider-failure collapse.** At the withdraw RPC the only
    gasless-specific errors are the members of §63.9.1; every other
    provider-interaction failure MUST be reported through the
    shared `Transport` or `InternalError` arm exactly as §63.9.3
    maps it. The finer provider categories MUST NOT be exposed as
    distinct wire discriminants. *(Reference fact.)*

R18. **Custody address at every address-with-balance site.** The
    `gasfree_address` field MUST be present at every placement
    §63.3.3 lists whenever the coin is a Tron coin whose platform
    has a configured provider, and omitted everywhere otherwise.
    *(Reference fact.)*

R19. **Preflight outcome mapping.** Each §63.7 outcome MUST map to
    the withdraw error §63.9.2 names; the shortfall figures MUST be
    the custody account's, computed as §63.9.2 states. *(Reference
    fact.)*

R20. **Bound defaults.** The provider request timeout defaults to
    15 000 ms, the settlement poll interval to 3 000 ms, and the
    authorization window to 300 s; a zero withdraw amount MUST be
    refused on the gasless rail. *(Reference facts.)*

R21. **Non-Tron refusal before processing.** A request naming
    `fee_method = gasless | auto` on a non-Tron coin MUST be
    refused as `Unavailable` (503), and gasless options with the
    native rail MUST be refused (400), before any other processing
    of the request. *(Reference fact.)*

R22. **Key-less policies refused before any provider call.** A
    withdraw on a Tron coin under a signing policy that holds no
    local private key (hardware wallet, WalletConnect,
    browser-extension wallet) MUST be refused with the
    unsupported-operation refusal of Chapter 49 R49.27a
    (`UnsupportedError`, 400) before any balance read, before any
    provider call and before either fee rail executes, so a
    `gasless` or `auto` request under such a policy never produces
    a quote. Whether that refusal precedes or follows the
    request-shape rejections of §63.9.4 is not established at the
    reference row and is not bound here. *(Reference fact at the
    row `feat/tron-gasfree (as of 2026-05-04)`, verified
    2026-11-08: the refusal precedes every balance read, every
    provider call and the execution of either rail. The previous
    candidate's note that the reference fetched a quote first and
    failed at signing with a 500 was wrong and is withdrawn.
    The refusal pattern matches
    [Chapter 47](47-metamask-integration.md) R47.5.13a.)*

R23. **Header-bearing JSON POST.** The cross-platform
    POST-JSON-with-headers transport capability (§63.2) MUST send
    the caller's headers and MUST set `Content-Type:
    application/json` in a way the caller cannot override; the
    browser transport MUST return the response headers the browser
    exposes, with lower-cased names. *(Reference fact; the chapter 26 transport table is to be
    extended accordingly.)*

R24. **No silent substitution without consent (Reloaded-original).**
    A rail switch -- native to gasless under `auto`, or gasless to
    native under `fallback_to_native` -- MUST be visible in the
    result: the returned fee details' `fee_method` (or the native
    fee-details shape) tells the caller which rail executed. The
    project MUST NOT switch rails under `fee_method = gasless`
    unless `fallback_to_native` is set.

> **Code-quality finding (informative; trigger: a comment at the
> reference row states the problem itself).** At the reference row the custody address is not produced
> by the balance-report builder but attached afterwards at each
> reporting site, and the generic single-address balance path leaves
> it unset for callers that do not run that attachment. A report
> produced through a path without the attachment therefore omits the
> field while the provider is configured -- a silent violation of
> R18. Reloaded should attach the field in one place (the
> balance-report construction for the EVM family) so that every
> consumer of a report carries it, and test R18 through each listed
> RPC rather than through the helper.

## 63.11 Tests

T1. **`CREATE2` address vectors.** Local derivation reproduces all
    fifteen custody-address vectors the public SDK publishes (five
    per network for mainnet, Nile and Shasta) and, for the Nile
    vectors, also the published intermediate salt and init-code
    hash.

T2. **TIP-712 vectors.** The `PermitTransfer` domain separator,
    struct hash and full typed-data hash reproduce the published
    Nile vectors of the public SDK (one domain separator, five
    messages). For mainnet and Shasta the test pins the domain
    separator computed from the per-network chain id and
    controller; a lab row checks one hash per network against the
    controller contract's public read-only message-hash function.

T3. **Auth vector.** The request-auth signature reproduces a fixed
    HMAC auth vector (self-derivable from the construction of
    §63.4.2 for known inputs); credentials are redacted in
    `Debug`.

T4. **Envelope semantics.** A 200 HTTP response carrying a
    non-200 envelope `code` is rejected; status classes map to the
    correct error categories; provider text is collapsed and bounded
    to 256 characters.

T5. **Submit payload contract.** The submit payload serializes
    integers as strings, the signature as 130-char `0x`-less hex,
    rejects a non-v4 `requestId`, rejects `version != 1`, and
    rejects malformed signatures.

T6. **Provider-payload drift.** Unknown lifecycle states and
    malformed hashes/signatures in provider payloads are rejected;
    both spellings of the transfer-fee estimate are accepted.

T7. **Preflight decisions.** Each outcome of §63.7 (custody
    mismatch, token not enrolled, precision disagreement, transfer in
    flight, either shortfall, proceed) is exercised against
    synthetic provider-plus-balance state, including that the
    in-flight outcome needs no on-chain read and that the
    activation fee counts only for a not-yet-activated account.

T8. **Rail selection and fallback.** `native`/`gasless`/`auto`
    selection, the gasless-options-with-native rejection, the
    max-amount rejection, the effective-cap computation, the
    deterministic fallback path, the native-first order of `auto`
    with both error cases of R16 (rail not applicable: the native
    error; rail attempted and failed: the gasless error), and the
    zero-amount and zero-deadline rejections are exercised.

T9. **Activation configuration.** The per-network request base of
    §63.4.1 formed from a host-only `base_url`, rejection of a
    path-bearing base URL, the defaults of
    R20, the `enabled: false` equivalence and the negative-cap
    rejection of §63.3.2, and the Tron-only/provider-required
    activation gates are exercised.

T10. **Custody address placements.** With a provider configured on
    a Tron platform, every RPC §63.3.3 lists carries
    `gasfree_address`, and none of them carries it without a
    provider or for a non-Tron coin (R18).

T11. **Error collapse.** Each provider-interaction condition of
    §63.9.3 reaches the withdraw RPC as the mapped arm and status, and no
    other discriminant; each §63.7 outcome reaches the RPC as
    §63.9.2 maps it, with custody figures in the shortfall case
    (R17, R19).

T12. **Key-less early refusal.** Under a hardware-wallet or
    WalletConnect policy a `gasless` or `auto` request is refused as
    `UnsupportedError` with no balance read and no provider call
    made (R22).

T13. **Non-Tron refusal.** On an EVM coin that is not Tron,
    `fee_method = gasless | auto` is refused with 503 and gasless
    options with the native rail with 400 (R21).

T14. **Nile end-to-end (lab row; deferred with D63.6).** A
    sign-and-submit round trip on the Nile testnet against the
    public test provider, verifying the custody address the provider
    reports, a successful preflight, and (once D63.6 lands)
    settlement tracking to a terminal state; plus a browser-target
    smoke test of the header-bearing transport. No end-to-end test
    exists at the reference row.

## 63.12 Deferred Work

*Decisions D63.1–D63.5 for the maintainer live in §63.12A below;
this section holds the deferrals D63.6 and D63.7.*

D63.6. **Submission and settlement tracking.** Wire the signed
    authorization to the provider submit endpoint and poll the
    trace endpoint for settlement, emitting the submit/wait
    progress phases of §63.3.6 and populating `trace_id`/tx-hash
    in the fee details and status. Today's withdraw is sign-only
    (§63.8).

D63.7. **Return path for tokens held at the custody address
    (Reloaded-only deferral).** What is deferred is a way for the
    user to move tokens out of the custody address other than by a
    provider-relayed transfer to a third party. Reloaded's position:
    the custody address is an account that only the GasFree
    controller spends from, so a balance that stays there -- a
    residue below the provider's fee after a transfer, a deposit
    made after the provider stopped serving the token, a balance
    left when an activation never completed -- is reachable today
    by no RPC of this project. The protocol places no constraint on
    the `receiver` of a `PermitTransfer`, so a gasless transfer
    whose receiver is the user's own address is a sufficient return
    path whenever the provider is operating. The surface is chosen
    (D63.4, option (b)): such a gasless withdraw, documented as the
    recovery procedure, as a first step. Deferred are that
    documentation, which lands with the rail, and the question
    whether any path is owed for a provider that no longer answers. (That the reference row has no return path
    either is a reference fact; the
    deferral itself takes no reference input.)

### 63.12A Decisions for the maintainer (Reloaded-only; take no reference input; resolved 2026-11-09)

D63.1. **Direction of `auto`.**
    *(Resolved: the maintainer's decision of 2026-11-09, option (a).)*
    R16 binds the native-first order
    the reference row has. The alternative -- gasless first with
    native as the fallback -- is what an earlier revision of this
    chapter described. Options: (a) keep R16 as written (parity;
    the ordinary route is tried first and the user's TRX is spent
    when it can be); (b) gasless-first (saves TRX whenever the
    custody account can pay, at the cost of a provider round trip
    on every `auto` withdraw). Recommended: (a), consistent with
    the project's preference on the EVM fee path to fail or use
    the ordinary route rather than substitute silently.

D63.2. **Wire-token parity.**
    *(Resolved: the maintainer's decision of 2026-11-09, option (a).)*
    Whether to adopt exactly the
    reference row's wire values for: the fee-details type tag
    (`TronGasless`), the outer and inner error discriminants of
    §63.9.1 (`Gasless`; `Unavailable`, `PendingTransfer`,
    `MaxFeeExceeded`, `QuoteExpired`, `ProviderRejected`,
    `InvalidProviderResponse`, `TraceNotFound`), the five task
    progress values of §63.3.6, and the relay-type string of the
    off-chain payload (`tron_gasfree`). Options: (a) adopt all
    (clients written against the reference's draft work unchanged);
    (b) adopt the statuses and semantics but choose Reloaded's own
    names. Recommended: (a) -- the values are public interface, cost
    nothing, and the statuses are bound either way.

D63.3. **Distinct custody-shortfall signal.**
    *(Resolved: the maintainer's decision of 2026-11-09, option (c).)*
    Whether the shortfall
    of §63.9.2 keeps the shared `NotSufficientBalance` shape
    (parity; the figures are the custody account's) or gains a
    distinguishing element (a distinct discriminant, or text naming
    the custody address) so a user whose wallet balance looks
    sufficient understands which account was short. Options: (a)
    parity; (b) a distinct member of the §63.9.1 family at 400; (c)
    parity shape with explanatory text. Recommended: (c) for the
    beta, (b) if client feedback asks for it.

D63.4. **Where the return path lives.**
    *(Resolved: the maintainer's decision of 2026-11-09, option (b).)*
    For D63.7: (a) a dedicated RPC;
    (b) a gasless withdraw whose `to` is the user's own address,
    documented as the recovery procedure; (c) documentation only.
    Recommended: (b) first (zero new surface), (a) later if needed.

D63.5. **Vector coverage threshold.**
    *(Resolved: the maintainer's decision of 2026-11-09, option (a).)*
    `docs/plans/tron-gasfree.md`
    asked for at least two published custody-address vectors per
    network. Established 2026-11-08 from the public SDK: it
    publishes five per network, Shasta included, so the threshold
    is met everywhere and the earlier idea of generating a second
    Shasta vector is moot. Options: (a) require every published
    vector per network (T1 as written). Recommended: (a); no
    alternative is needed.

## 63.13 Baseline Verifications

The following are verifiable from the baseline state of
[Chapter 02](02-baseline-state.md), row `baseline anchor (R1)`:

V1. The baseline tree contains **no** Tron coin support at all
    (per §21.1), and therefore no GasFree provider client, no
    `CREATE2` custody-address derivation, no TIP-712
    `PermitTransfer` signer, and no gasless withdraw rail. A
    directory listing of the baseline tree returns no
    GasFree-related path.

V2. The baseline withdraw request has no fee-rail selector and no
    gasless options; the balance response has no GasFree-address
    field. A tree-wide search of the baseline returns no such
    fields. The public surface of §63.3 is therefore material
    introduced after the baseline in its entirety.

## 63.14 External References

- The GasFree provider REST API (endpoint paths, the
  `{code, reason, message, data}` response envelope, the HMAC-SHA256
  request-auth header construction, the per-network path segments,
  the supported-token / account-info / submit / trace payloads), as
  published in the provider's developer documentation -- the
  binding source for §63.4.
- The GasFree `PermitTransfer` TIP-712 typed-data specification
  (domain `GasFreeController` / `V1.0.0`, the message field set and
  order) and its published test vectors -- the binding source for
  §63.6.
- The public GasFree JavaScript SDK: repository
  `gasfreeio/gasfree-sdk-js` at
  <https://github.com/gasfreeio/gasfree-sdk-js>, published by the
  GasFree project (GitHub organization `gasfreeio`) under the
  Apache License 2.0; cited at its release tag `v1.1.2` (commit
  `2f0a0318c4952030416aa4bafe7e1bca16b3aa15`, 2026-01-16), the
  latest release at the time of citation (2026-11-08). It carries
  the per-network contract artifacts (controller and beacon
  addresses, the proxy creation bytecode), the custody-address
  derivation reference and its per-network custody-address and
  typed-data vectors -- the binding source for the constants and
  vectors of §63.5 and §63.6, and the cross-check for the creation
  bytecode, which Reloaded takes from the public chain under the
  §63.5 sourcing rule (chapter 01 R30: URL, version and license as
  cited here). Cited as a public
  specification (chapter 01 R3); nothing in this chapter is taken
  from any project lineage's copy of it.
- The Tron `CREATE2` account-derivation rule (the `0x41` prefix in
  place of Ethereum's `0xff`) and keccak-256 -- the binding source
  for the address algorithm of §63.5.
- EIP-712 (typed structured data hashing and the `v` recovery-byte
  `27`/`28` convention) and secp256k1 (the 65-byte signature
  layout).
- The TRC20 token standard `balanceOf(address)` read used by the
  preflight (see [Chapter 21](21-tron-integration.md) §21.6).
- [Chapter 21](21-tron-integration.md) (Tron coin support: address
  type, signature/hash types, node client, native resource-fee
  model), [Chapter 26](26-cross-platform-and-wasm.md) (the
  cross-platform authenticated-POST HTTP capability),
  [Chapter 35](35-evm-v2-activation-rpcs.md) (the EVM platform and
  token activation requests this chapter extends),
  [Chapter 49](49-withdrawal-task-path.md) (the shared withdraw
  request, task statuses and error arms this chapter extends), and
  [Chapter 47](47-metamask-integration.md) (the early-refusal
  pattern R22 follows).

## 63.15 Provenance Footer

- *Inputs:* **forbidden-class input, consulted by the KDF Spec
  Reader under chapter 01 §1.4A** at the rows `feat/tron-gasfree
  (as of 2026-05-04)` and `dev (as of 2026-03-16)` of chapter 02
  §2.15A (the change set of the first over the second: the GasFree
  module group of the Tron portion of the EVM coin family, the Tron
  and EVM withdraw paths, the EVM platform and token activation
  requests, the balance-report and address-management RPC surfaces,
  the shared withdraw request, status and error types, the
  fee-details type, the HTTP transports and the EIP-712 encoder;
  49 files), for the facts of §63.3–§63.9 marked as reference facts, §63.4–§63.7 and §63.9 as re-established, R16–R23,
  the informative note under §63.10, the reference fact in D63.7
  and T14, and the 256-character figure of §63.4.3 and R10; the
  earlier revision of this chapter on this repository's own parked
  branch feat/tron-gasfree (Reloaded) -- a branch of this
  repository, not the reference row of the same name --
  (independent work of this project, chapter 01 R7, itself
  produced under §1.4A; the base text of every section not marked
  as added or re-established); the `baseline anchor (R1)` for §63.1 and
  §63.13; the public GasFree provider REST API documentation, the
  GasFree `PermitTransfer` TIP-712 specification, and the public
  GasFree JavaScript SDK (per-network contract artifacts, proxy
  creation bytecode, the fifteen custody-address vectors and the
  Nile typed-data vectors, read 2026-11-08) -- chapter 01 R3; the
  provider's wire shapes as dictated interop -- R4; the
  Tron `CREATE2` rule, EIP-712, secp256k1 and the TRC20 ABI -- R3;
  public facts about the historical record's repository for the two
  §2.15A rows (identifiers, dates, open/draft state) -- R6; this
  repository's own tree (shared withdraw error arms, EIP-712
  encoder, balance-report types) and chapters 21, 26, 35, 47, 49,
  55 for the "our side" statements -- R1/R2/R7.
- *Permitted-input classes used:* R1/R2/R7 (this project's tree,
  revision history and the parked-branch revision of this chapter);
  R3 (external public specifications: the GasFree provider API, the
  GasFree TIP-712 `PermitTransfer` spec and vectors, the public
  GasFree SDK artifacts and vectors, the Tron `CREATE2` rule,
  EIP-712, secp256k1, the TRC20 ABI); R4 (the provider's dictated
  wire shapes of §63.4, embedded as interop reuse under
  R29/R31/R33, and our own public RPC field names of §63.3); R6
  (public repository facts for the §2.15A rows); forbidden-class
  input through the §1.4A route as recorded above.
- *Sibling-allowlist consultations:* none. ([Chapter 21](21-tron-integration.md)
  and [Chapter 26](26-cross-platform-and-wasm.md) are cross-chapter
  contracts of this document set, not sibling repositories.)
- *Forbidden corpus:* **consulted**, through the two-team procedure
  of chapter 01 §1.4A, at the rows `feat/tron-gasfree (as of
  2026-05-04)` and `dev (as of 2026-03-16)` only, for the sections
  and items the *Inputs* item names; not consulted for §63.1,
  §63.13, §63.12A, R24, the Reloaded position of D63.7, or the
  §63.5 sourcing rule (a Reloaded-original addition of 2026-11-09,
  like the resolution markers of D63.1–D63.5), which take no
  reference input. Every reference fact is
  stated as externally observable behavior, public RPC interface
  or dictated interop. Wire-visible discriminants of the
  reference's RPC surface (fee-method values, task-status values,
  error discriminants, the fee-details tag, the relay-type value,
  the relay payload's member names) are recorded as the reference's
  public interface; whether Reloaded adopts them is decision D63.2,
  not a requirement. *Clearance record (chapter 01 R38–R40, T1):* PASS on 2026-11-08 (third pass), no findings. Extent: the Tron GasFree fee-delegation subsystem as chapter 63 scopes it — the gasless withdraw rail, provider REST client and typed payloads, custody-address derivation, typed-data signing, platform/token activation binding, custody-address reporting on balance and address-management RPCs, shared withdraw request/status/error/fee-details extensions, native and browser header-bearing HTTP transport, EIP-712 encoder changes, dependency manifests — compared as the change set of row feat/tron-gasfree (as of 2026-05-04) over row dev (as of 2026-03-16): 49 files (13 added, 36 modified), every added/changed line, plus the nine commit messages between the two rows; unchanged base context read where the chapter states an ordering fact. Method: reading of the full change set against every passage of the chapter and the pointer edits; a mechanical identifier intersection between the change set's added lines plus commit messages and the chapter text, each hit judged by the per-token forced-vs-free test; git reference metadata for the R13 check; the repository's clean-room gate tool was not run because the chapter contains no code blocks. Measure: 0.02 on the discretionary-expression basis, read against the standard that any single itemized discretionary-expression finding fails the candidate irrespective of the figure; with zero itemized findings the result is PASS (after two failed passes recorded by count: 0.05 with nine findings, 0.03 with twelve). History of record: the earlier
  revision of this chapter on this repository's own parked branch
  feat/tron-gasfree (Reloaded) carried a Dirty Gate PASS (measure 0.08;
  extent and method not recorded in this document set) recorded
  only in that branch's commit message of 2026-06-26, against a
  reference state that was not tabled at the time. Neither that
  verdict nor the two failed passes carry to this revision.
