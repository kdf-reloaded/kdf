# Chapter 49 -- Tron GasFree Fee Delegation (Gasless TRC20 Withdraw)

**Status:** driving-spec

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
> actual submission of the signed authorization to the provider
> and on-chain settlement tracking are scaffolded but deferred,
> so today's withdraw is **sign-only**.

## 49.0 Executive Summary

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
address derivation surfaced on the balance response; a typed
HTTP client for the provider's public REST API; an
account-preflight/eligibility layer; a TIP-712 `PermitTransfer`
signer; and a new gasless branch in the withdraw pipeline driven
by a new `fee_method` request field.

The integration is **TRC20-only and sign-only today**. Gasless
applies exclusively to Tron TRC20 tokens (never native TRX, never
non-Tron EVM chains). The withdraw path preflights, signs the
authorization, and returns it as a typed off-chain payload with a
gasless fee breakdown; it does **not** yet submit the
authorization to the provider or track on-chain settlement. The
provider client already models the submit and trace endpoints and
the task framework already reserves the submit/settlement
progress phases, so submission is scaffolded follow-on work.

> **Binding scope (R36).** Throughout this chapter, requirements
> bind observable *behaviour*, the *public* RPC request/response
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

## 49.1 Baseline Verification

Tron GasFree fee delegation is a **post-2022** feature. It is
absent from the baseline anchor of
[Chapter 02](02-baseline-state.md) (commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`, 3 June 2022): the
baseline has no Tron coin-support at all (per §21.1), and a
fortiori no GasFree provider client, no gasless withdraw rail, no
`CREATE2` GasFree address derivation, and no `fee_method`/gasless
withdraw request fields. The classification here is by *component
role and first-introduction epoch only*; it is derived from the
project's own revision history and does **not** transcribe code.

| Component (role) | Basis |
| --- | --- |
| GasFree provider HTTP client and typed API schema | Introduced after the baseline anchor. |
| `CREATE2` GasFree custody-address derivation | Introduced after the baseline anchor. |
| TIP-712 `PermitTransfer` signing | Introduced after the baseline anchor. |
| Gasless branch in the withdraw pipeline (`fee_method`, gasless options) | Introduced after the baseline anchor. |
| Platform/token GasFree activation config | Introduced after the baseline anchor. |

## 49.2 Subsystem Placement

GasFree support belongs **inside the Tron portion of the EVM coin
family**, as a self-contained module group adjacent to the Tron
fee/withdraw code of Chapter 21, not as a free-standing protocol
crate. It reuses the Tron address type, the Tron signature/hash
types, and the Tron node client (for the on-chain TRC20
`balanceOf` read) already specified by Chapter 21, and it reuses
the shared EVM key-policy and EIP-712 encoder.

Two pieces of cross-cutting infrastructure are extended in support
of GasFree and are specified by their own chapters, not here:

- a **cross-platform "POST JSON with custom request headers"**
  HTTP capability (native and WASM), required because the
  provider API needs per-request authentication headers; see
  [Chapter 26](26-cross-platform-and-wasm.md) for the
  HTTP-transport contract. This chapter only requires that such a
  capability exists and is used.
- the **shared EIP-712 encoder** is generalized so it can encode
  the GasFree `PermitTransfer` typed data (§49.6). The encoder is
  a public-spec implementation; this chapter requires only that it
  produces spec-correct typed-data hashes.

## 49.3 Public RPC and Activation Surface

This section specifies the *public contract* the workspace
exposes for GasFree. The JSON field names below are part of that
public API and are binding; the Rust types behind them are not.

### 49.3.1 Platform activation (dictated by our public API)

The EVM/Tron platform-activation request gains an optional
provider binding, valid only for Tron chains and rejected for
non-Tron EVM chains:

- `tron_gasless_provider` (optional object):
  - `base_url` -- a **host-only** provider URL (no path). The
    resolver appends the network path segment (§49.4); a
    caller-supplied path is rejected at activation.
  - `api_key` -- provider API key.
  - `api_secret` -- provider API secret (used for request signing,
    §49.4.2).
  - `service_provider` -- the provider's Tron address, bound into
    the signed authorization and validated as a parseable Tron
    address at activation.
  - `request_timeout_ms` (optional) -- per-call HTTP timeout.
  - `status_poll_interval_ms` (optional) -- settlement poll
    interval (reserved for the deferred submission flow).

  `api_key` and `api_secret` are credentials and MUST NOT appear
  in logs, `Debug` output, or serialized responses (R-credential).

### 49.3.2 Token activation (dictated by our public API)

A Tron TRC20 token-activation request gains an optional gasless
block:

- `gasless` (optional object):
  - `enabled` (bool) -- opt the token into the gasless rail.
  - `transfer_max_fee` (optional decimal) -- an activation-time
    cap, in token units, on the provider fee the user will accept;
    must be non-negative.

  A token gasless block is valid only on Tron chains, and only
  when the platform has a `tron_gasless_provider`; otherwise
  activation is rejected with an invalid-payload error.

### 49.3.3 Balance response (dictated by our public API)

The balance response gains an optional field:

- `gasfree_address` (optional string) -- the user's locally
  derived (§49.5) GasFree custody address, populated only for a
  Tron coin with a configured GasFree provider; omitted
  otherwise.

### 49.3.4 Withdraw request (dictated by our public API)

The withdraw request gains a fee-rail selector and gasless
constraints:

- `fee_method` (optional enum): `native` (default behaviour;
  unchanged), `gasless` (require the gasless rail), or `auto`
  (try gasless, fall back to native when appropriate).
- `gasless` (optional object), only meaningful with
  `fee_method = gasless | auto`; supplying it with `native`/absent
  `fee_method` is rejected:
  - `max_fee` (optional decimal) -- per-request cap, in token
    units, on the accepted provider fee.
  - `deadline_seconds` (optional integer) -- authorization
    validity window from signing time; must be greater than zero;
    a default window applies when omitted.
  - `fallback_to_native` (bool, default false) -- with
    `fee_method = gasless`, permit silently falling back to the
    native rail when the gasless rail is deterministically
    unavailable.

### 49.3.5 Withdraw response (gasless fee details, dictated by our public API)

When a withdraw is routed through the gasless rail, its fee
details object carries a distinct shape from the native Tron
resource-fee details of Chapter 21:

- `coin`, `fee_method` (value: `gasless`), `provider_name`
  (value: `gasfree`), `gasfree_address`, `transfer_fee`,
  `activation_fee`, `total_token_fee`, `signed_max_fee`
  (optional), `trace_id` (optional). All fee amounts are in token
  units; the fee is paid in the token, not in TRX.

### 49.3.6 Long-running withdraw progress phases

The long-running (`task::*`) withdraw status enumeration gains
new gasless phases reported to the caller as the operation
progresses: fetching the gasless quote; signing the gasless
authorization; submitting the authorization; waiting on the
provider (carrying a `trace_id` and a provider-reported state
string); and waiting for on-chain settlement (carrying a
`trace_id` and an optional transaction hash). The submit/wait
phases exist for the deferred submission flow (§49.8); the
sign-only path emits the quote/sign phases.

## 49.4 GasFree Provider REST API (dictated interop)

The provider API is an external, public REST service. The
fragments in this section are reproduced because they are the
bytes and calls any client must exchange to interoperate; their
authoritative source is the GasFree provider's published API, not
any project lineage.

### 49.4.1 Endpoints

All paths are relative to the resolved base URL and share the
prefix `api/v1`:

| Operation | Method + path |
| --- | --- |
| Supported tokens (with provider fees) | `GET api/v1/config/token/all` |
| Account info (custody address, nonce, assets, activation flag) | `GET api/v1/address/{account_address}` |
| Submit a signed transfer authorization | `POST api/v1/gasfree/submit` |
| Trace a submitted transfer by id | `GET api/v1/gasfree/{trace_id}` |

The base URL is **host-only at activation** and the resolver
appends a per-network path segment before the prefix: `tron` for
Tron mainnet, `nile` for the Nile testnet, `shasta` for the
Shasta testnet (so e.g. a mainnet supported-tokens call resolves
to `/tron/api/v1/config/token/all`).

### 49.4.2 Request authentication (dictated)

Each request carries two headers:

- `Timestamp` -- the current Unix time in seconds.
- `Authorization` -- of the form `ApiKey {api_key}:{signature}`,
  where `{signature}` is the Base64 encoding of an HMAC-SHA256,
  keyed by `api_secret`, computed over the ASCII concatenation of
  the HTTP method, the request path, and the timestamp (in that
  order, no separators).

An implementation MUST match this construction byte-for-byte to
authenticate; it MUST be validated against the provider's
published auth test vector. The credential material MUST stay
out of logs and `Debug`.

A second, proxy-mediated authentication transport (where a
trusted proxy supplies credentials instead of the client holding
`api_secret`) is **reserved and not implemented**; selecting it
yields a not-implemented error. See D-proxy.

### 49.4.3 Response envelope (dictated)

Every response body is a JSON envelope with: an integer `code`, an
optional `reason`, a `message`, and an optional `data` payload. A
**business-success** response carries `code = 200` and a present
`data`; any other `code`, or a `data`-less success, is an error
even when the HTTP status is 200. Error envelopes (or non-2xx HTTP
statuses) map to typed errors by status class (§49.9). Provider
message text included in surfaced errors MUST be sanitized
(whitespace-collapsed and length-bounded) so untrusted provider
text cannot bloat or distort diagnostics.

### 49.4.4 Account info payload (dictated)

`GET .../address/{account}` returns, within `data`: the queried
`accountAddress`; the `gasFreeAddress` (the provider's view of the
custody address); an `active` flag (whether the custody account is
on-chain activated); a `nonce` (bound into the next
authorization); an `allowSubmit` flag; and an `assets` array, each
asset carrying `tokenAddress`, `tokenSymbol`, `activateFee`,
`transferFee`, `decimal`, and a `frozen` (in-flight-locked)
amount.

### 49.4.5 Supported-tokens payload (dictated)

`GET .../config/token/all` returns an array of entries each with
`tokenAddress`, `activateFee`, `transferFee`, `symbol`, `decimal`,
and a `supported` flag. Results may be cached for the session.

### 49.4.6 Submit request payload (dictated)

`POST .../gasfree/submit` sends the signed authorization as JSON.
Integer-valued fields are serialized as decimal **strings**;
addresses as base58 Tron addresses; the signature as a 65-byte
hex string (130 hex chars, no `0x`). Fields: an optional
`requestId` (a UUIDv4, rejected if not version 4); `token`;
`serviceProvider`; `user`; `receiver`; `value`; `maxFee`;
`deadline`; `version` (must equal 1); `nonce`; and `sig`.

### 49.4.7 Submit/trace response payloads (dictated)

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
`ON_CHAIN_FAILED`. An unknown `state`/`txnState` MUST be rejected
on deserialization so provider API drift surfaces immediately
rather than being silently misread.

## 49.5 GasFree Custody-Address Derivation -- Tron `CREATE2` (dictated interop)

The custody address is derived **locally and deterministically**
so the wallet can verify the provider's reported address and so a
user can compute their receive address without trusting the
provider. The derivation is dictated by the Tron `CREATE2` rule
and the published per-network GasFree contract artifacts; the
algorithm is reproduced because it must be byte-exact for the
address to be correct.

Per network the derivation uses three published constants: the
GasFree **controller** contract address, the **beacon** contract
address, and the proxy **creation bytecode** (taken from the
public GasFree SDK; this chapter references them rather than
embedding the bytecode). The steps:

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

The derivation MUST be validated against the published GasFree
per-network address vectors. The controller and beacon are
hard-bound per network (not caller-configurable) precisely because
a wrong value would silently produce a wrong, user-visible receive
address.

## 49.6 `PermitTransfer` Signed Authorization -- TIP-712 (dictated interop)

The user authorizes a transfer by signing GasFree's
`PermitTransfer` structured data, Tron's TIP-712 (the
EIP-712-shaped) typed-data scheme. The domain and message layout
below are dictated by the GasFree protocol and are reproduced
because the signature is only valid if the typed data is
byte-exact.

- **Domain**: `name` = `GasFreeController`, `version` = `V1.0.0`,
  `chainId` = the network's EIP-712 chain id (e.g. the Nile
  testnet's chain id is `3448148188`), `verifyingContract` = the
  per-network controller (§49.5).
- **Primary type**: `PermitTransfer`, with fields in order:
  `token` (address), `serviceProvider` (address), `user`
  (address), `receiver` (address), `value` (uint256), `maxFee`
  (uint256), `deadline` (uint256), `version` (uint256, value 1),
  `nonce` (uint256).

Signing obligations (behavioural):

- the typed-data hash MUST be validated against the published
  GasFree `PermitTransfer` test vectors (domain separator and
  full typed-data hash) before the signer is trusted;
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
submit payload requires (§49.4.6).

## 49.7 Account Preflight and Availability Semantics

Before signing, the gasless rail performs a **preflight** that
fetches provider account state and the on-chain TRC20 balance of
the custody address, and decides whether the transfer can proceed.
The preflight is a behavioural contract; the decision categories
below are normative, their internal representation is not.

Inputs gathered: the provider's account info (§49.4.4); the
on-chain TRC20 `balanceOf` of the custody address (read via the
Tron node client of Chapter 21); and the locally derived custody
address (§49.5).

Decision outcomes:

- **Available** -- the rail may sign and (when submission is
  wired) submit.
- **Pending transfer** -- the provider enforces *one in-flight
  transfer per account*; the caller must wait for settlement and
  retry. This is a transient condition, not a hard error.
- **Disabled** -- the transfer cannot use the gasless rail, for a
  classified reason:
  - **Address mismatch** -- the provider's reported custody
    address disagrees with the local `CREATE2` derivation. This is
    a **safety stop**: the wallet MUST NOT sign against a custody
    it cannot independently verify (guards against a wrong
    controller, a derivation bug, or a malicious provider).
  - **Token unsupported** -- the token is not enrolled in the
    account; the caller may fall back to native or error.
  - **Token-decimal mismatch** -- the provider's reported token
    decimals disagree with the activated token's decimals; the
    caller MUST NOT sign or silently fall back (config/provider
    drift).
  - **Insufficient spendable balance** -- the spendable balance
    (on-chain minus frozen) is below `value + transfer_fee +
    activation_fee`.
  - **Inactive-account insufficient balance** -- for a not-yet
    activated custody account, the on-chain balance must cover
    `value + transfer_fee + activation_fee + frozen`; otherwise
    the rail rejects before signing to avoid a guaranteed on-chain
    failure.

The activation fee is charged only when the custody account is not
yet on-chain activated, and is zero otherwise.

## 49.8 Gasless Withdraw Behaviour (sign-only today)

The withdraw pipeline gains a gasless branch selected by
`fee_method` (§49.3.4). Behaviour:

1. **Rail selection.** `native` is unchanged. `gasless` requires
   the gasless rail. `auto` attempts gasless and falls back to
   native when appropriate. Supplying gasless options with the
   native rail is rejected. `auto` does not apply to a
   max-amount ("withdraw everything") request, and a max-amount
   request is **not supported** on the gasless rail (the provider
   fee is taken from the same token balance, so "max" is
   ambiguous).
2. **Eligibility.** The gasless rail applies only to a Tron TRC20
   token whose platform has a configured GasFree provider and
   whose token activation enabled gasless. For a plain coin
   (TRC20), the provider binding is resolved from its platform
   coin. Native TRX and non-Tron EVM coins never use the rail.
3. **Quote.** Under the address nonce lock, the rail runs the
   preflight (§49.7), computes `total_token_fee = transfer_fee +
   activation_fee`, and applies the **effective fee cap** = the
   minimum of any present caps (the per-request `max_fee` and the
   activation-time `transfer_max_fee`); if neither is present the
   quoted total fee itself is used. If the quoted total exceeds
   the effective cap, the transfer is refused (max-fee-exceeded).
   The signed authorization's `maxFee` is set to the effective
   cap. The `deadline` is "now + window" (per-request
   `deadline_seconds` or the default).
4. **Sign.** The rail signs the `PermitTransfer` (§49.6).
5. **Result (sign-only).** The withdraw returns the signed
   authorization as a typed off-chain payload (carrying the
   signed permit, the participating addresses, the derived custody
   address, and a creation timestamp) wrapped as an *unsigned*
   transaction artifact -- i.e. it is **not** broadcast on-chain
   and **not** yet submitted to the provider -- together with the
   gasless fee details (§49.3.5). The off-chain payload's wrapping
   envelope shape and its internal type discriminator are
   discretionary implementation detail.
6. **Fallback.** With `fee_method = gasless` and
   `fallback_to_native = true`, a *deterministic* unavailability
   (e.g. the rail is unavailable or balance is insufficient) falls
   back to the native rail instead of erroring; non-deterministic
   provider/transport errors still surface.

**Deferred (scaffolded):** the provider client already models the
submit and trace endpoints, and the task progress enumeration
already reserves the submit/wait-for-provider/wait-for-settlement
phases (§49.3.6), but the withdraw result is sign-only: it does
not POST the authorization or poll for settlement. Wiring
submission and settlement tracking into the withdraw result is
follow-on work (D-submit).

## 49.9 Error Taxonomy and Status Codes

The gasless rail introduces two public error families whose
**HTTP status mapping is the observable contract** (the variant
identities and message strings are not):

- **Withdraw-level gasless errors** map as: unavailable →
  `503`; an already-pending transfer → `409`; a fee-cap breach or
  an expired quote → `400`; provider rejection or an invalid
  provider response → `502`; a missing trace → `404`.
- **Provider-interaction errors** map as: invalid request →
  `400`; timeout → `504`; transport/invalid-response/upstream →
  `502`; provider bad-request → `400`; unauthorized → `401`;
  forbidden → `403`; rate-limited → `429`; not-implemented →
  `501`; internal → `500`. Provider HTTP status classes (4xx/5xx)
  and the envelope `code` are folded into these categories.

## 49.10 Binding Requirements

R1. **TRC20-only, Tron-only.** The gasless rail MUST apply only to
    Tron TRC20 tokens. It MUST refuse native TRX and any non-Tron
    EVM chain. A token gasless config without a platform provider,
    or on a non-Tron chain, MUST be rejected at activation.

R2. **Local custody verification is mandatory.** The custody
    address MUST be derived locally per §49.5 and MUST be compared
    against the provider's reported address; a mismatch is a
    safety stop that prevents signing. The per-network controller
    and beacon MUST be hard-bound, not caller-configurable.

R3. **Spec-faithful typed data.** The `PermitTransfer` domain and
    message (§49.6) and the `CREATE2` derivation (§49.5) are set
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

R8. **Fee cap honoured.** The accepted provider fee MUST NOT
    exceed the effective cap (the minimum of the present
    per-request and activation-time caps); a breach MUST refuse
    the transfer. The signed `maxFee` MUST equal the effective
    cap.

R9. **Credential confidentiality.** `api_key`, `api_secret`, and
    the raw signature MUST NOT appear in logs, `Debug`, or
    serialized responses.

R10. **Provider-text sanitization.** Any provider-supplied message
    folded into a surfaced error MUST be whitespace-collapsed and
    length-bounded.

R11. **Dictated-auth fidelity.** The request-auth header
    construction (§49.4.2) and the success-envelope semantics
    (§49.4.3) MUST match the provider API exactly; a 200 HTTP
    status with a non-200 envelope `code` or absent `data` is an
    error.

R12. **Strict provider-enum parsing.** Unknown transfer/tx
    lifecycle states (§49.4.7) MUST be rejected on deserialization
    so provider API drift is surfaced rather than misread.

R13. **Host-only base URL.** The configured `base_url` MUST be
    host-only; the network path segment (§49.4.1) is derived, and
    a caller-supplied path MUST be rejected at activation.

R14. **`max` is unsupported on the rail.** A max-amount gasless
    withdraw MUST be rejected; `auto` MUST NOT route a max-amount
    request through the rail.

R15. **Public RPC field stability.** The request/response JSON
    field names of §49.3 are the public contract; changes must be
    additive and backward-compatible.

## 49.11 Tests

T1. **`CREATE2` address vectors.** Local derivation reproduces the
    published GasFree per-network custody-address vectors
    (mainnet, Nile, Shasta).

T2. **TIP-712 vectors.** The `PermitTransfer` domain separator and
    full typed-data hash reproduce the published GasFree vectors
    across networks.

T3. **Auth vector.** The request-auth signature reproduces the
    provider's published HMAC test vector; credentials are redacted
    in `Debug`.

T4. **Envelope semantics.** A 200 HTTP response carrying a
    non-200 envelope `code` is rejected; status classes map to the
    correct error categories.

T5. **Submit payload contract.** The submit payload serializes
    integers as strings, the signature as 130-char `0x`-less hex,
    rejects a non-v4 `requestId`, rejects `version != 1`, and
    rejects malformed signatures.

T6. **Provider-payload drift.** Unknown lifecycle states and
    malformed hashes/signatures in provider payloads are rejected.

T7. **Preflight decisions.** Each availability/disabled outcome of
    §49.7 (address mismatch, unsupported token, decimal mismatch,
    insufficient/inactive-insufficient balance, pending transfer,
    available) is exercised against synthetic provider+balance
    state.

T8. **Rail selection and fallback.** `native`/`gasless`/`auto`
    selection, the gasless-options-with-native rejection, the
    max-amount rejection, the effective-cap computation, and the
    deterministic fallback path are exercised.

T9. **Config resolution.** Host-only base-URL resolution per
    network, rejection of a path-bearing base URL, and the
    Tron-only/provider-required activation gates are exercised.

## 49.12 Deferred Work

D-submit. **Submission and settlement tracking.** Wire the signed
    authorization to the provider submit endpoint and poll the
    trace endpoint for settlement, emitting the reserved
    submit/wait progress phases (§49.3.6) and populating
    `trace_id`/tx-hash in the fee details and status. Today's
    withdraw is sign-only.

D-proxy. **Proxy-mediated authentication transport.** The
    alternative transport that delegates credential handling to a
    trusted proxy (so the client need not hold `api_secret`) is
    reserved and currently returns not-implemented.

D-provider-fetch. **Provider auto-discovery.** The
    `service_provider` is currently supplied in activation config;
    fetching the provider set from the provider's config endpoint
    at activation time is desirable follow-on work.

## 49.13 External References

- The GasFree provider REST API (endpoint paths, the
  `{code, reason, message, data}` response envelope, the HMAC-SHA256
  request-auth header construction, the supported-token /
  account-info / submit / trace payloads) -- the binding source
  for §49.4.
- The GasFree `PermitTransfer` TIP-712 typed-data specification
  (domain `GasFreeController` / `V1.0.0`, the message field set and
  order) and its published test vectors -- the binding source for
  §49.6.
- The GasFree SDK's published per-network contract artifacts (the
  GasFree controller and beacon addresses and the proxy creation
  bytecode) -- the binding source for the constants of §49.5.
- The Tron `CREATE2` account-derivation rule (the `0x41` prefix in
  place of Ethereum's `0xff`) and keccak-256 -- the binding source
  for the address algorithm of §49.5.
- EIP-712 (typed structured data hashing and the `v` recovery-byte
  `27`/`28` convention) and secp256k1 (the 65-byte signature
  layout).
- The TRC20 token standard `balanceOf(address)` read used by the
  preflight (see [Chapter 21](21-tron-integration.md) §21.6).
- [Chapter 21](21-tron-integration.md) (Tron coin support: address
  type, signature/hash types, node client, native resource-fee
  model) and [Chapter 26](26-cross-platform-and-wasm.md) (the
  cross-platform authenticated-POST HTTP capability).

## 49.14 Baseline Verifications

The following are verifiable from the baseline state of
[Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree contains **no** Tron coin support at all
    (per §21.1), and therefore no GasFree provider client, no
    `CREATE2` custody-address derivation, no TIP-712
    `PermitTransfer` signer, and no gasless withdraw rail. A
    directory listing of the baseline tree
    (`git ls-tree -r c1d46c0c1592faa0860f704008b2b2381bc3840f`)
    returns no GasFree-related path.

V2. The baseline withdraw request has no fee-rail selector and no
    gasless options; the balance response has no GasFree-address
    field. A tree-wide search of the baseline returns no such
    fields. The public surface of §49.3 is therefore material
    introduced after the baseline in its entirety.

## 49.15 Provenance Footer

- *Inputs:* the project's own revision history (used for the
  epoch classification of §49.1 and the baseline verifications of
  §49.14, by component role and first-introduction epoch only --
  no code transcribed); the baseline anchor of
  [Chapter 02](02-baseline-state.md); the public GasFree provider
  REST API and its `PermitTransfer` TIP-712 typed-data
  specification and published vectors; the public GasFree SDK's
  per-network contract artifacts; the Tron `CREATE2`
  account-derivation rule; EIP-712 and secp256k1; the TRC20
  `balanceOf` read; and cross-chapter contracts
  (Chapters 21, 26).
- *Permitted-input classes used:* baseline source (epoch
  classification and absence verification only); external public
  specifications (the GasFree provider API, the GasFree TIP-712
  `PermitTransfer` spec and vectors, the GasFree SDK contract
  artifacts, the Tron `CREATE2` rule, EIP-712, secp256k1, the
  TRC20 ABI); cross-chapter contracts (Chapters 21, 26); Interop /
  wire-and-API-bound reuse (R29/R31/R33) for the dictated
  fragments embedded in §49.4 (the provider endpoint paths,
  response envelope, auth construction, and payload field names),
  §49.5 (the derivation algorithm and the `initialize(address)`
  selector), and §49.6 (the typed-data domain and message layout)
  -- whose authoritative source is the bytes and calls any
  conforming GasFree client must exchange for interoperability,
  not any historical lineage's discretionary expression; and our
  own public RPC contract (the request/response field names of
  §49.3).
- *Sibling-allowlist consultations:* [Chapter 21](21-tron-integration.md)
  (the Tron coin-support substrate this feature layers onto) and
  [Chapter 26](26-cross-platform-and-wasm.md) (the cross-platform
  HTTP transport contract).
- *Forbidden corpus:* consulted **only** to recover the
  externally-dictated GasFree interop embedded here under Interop
  reuse (R29/R31/R33) -- the provider API surface of §49.4, the
  `CREATE2` derivation rule and per-network artifact roles of
  §49.5, the `PermitTransfer` typed-data layout of §49.6, the
  dictated provider lifecycle enumerations, and the public/dictated
  semantics distilled into the behavioural requirements of
  §49.7--§49.9 -- all of which trace to the public GasFree
  protocol, the GasFree SDK, and the Tron derivation rule. No
  discretionary expression from the post-2022 code -- no struct
  field lists, internal type or enum variant definitions, private
  helper or method names or signatures or bodies, the relay/staging
  envelope's internal shape or type tag, the withdraw/preflight
  control flow, local variable names, per-method or per-field
  tables keyed to internal names, the module file tree, internal
  test-function names, discretionary constants, or diagnostic /
  error / log string literals -- crosses into this chapter. All
  other content is clean-room driving-spec stated by behaviour and
  public/dictated interface, governed by the R35 gate and the R36
  binding-scope note that heads this chapter.
