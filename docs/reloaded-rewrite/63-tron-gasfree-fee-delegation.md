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

> *Status note (2026-11-20), §63.3.1 and §63.3.2 built (Reloaded-original
> where it states a choice; takes no reference input).*
> `enable_eth_with_tokens` and `task::enable_eth::init` read
> `tron_gasless_provider`. The entries of `erc20_tokens_requests`, and the
> `activation_params` of `enable_erc20` and `task::enable_erc20::init`, read
> `gasless`. TRON stays behind `TRON_ACTIVATION_AVAILABLE` (`false`), so no
> request reaches any of this yet.
>
> - *The provider object.* On an EVM platform it answers `InvalidPayload`
>   (HTTP 400) before any key is read and any node is contacted. On a TRON
>   platform a `base_url` that is not an `http` or `https` URL of a scheme and
>   a host alone, and a `service_provider` that is not a base58check TRON
>   address, answer `InvalidPayload`; the hex forms of an address are not
>   accepted for `service_provider`. Neither a node nor the provider is
>   contacted at activation. `base_url`, `api_key`, `api_secret` and
>   `service_provider` are required: a request without one of them fails
>   request decoding. The defaults are 15 000 ms and 3 000 ms (R20).
> - *The token block.* A block with `enabled: false` is treated as absent,
>   whatever else it holds. An enabled block is refused for a token that is
>   not a TRC20 token (an ERC-20 token, an NFT), for a TRON platform without a
>   provider binding, for a negative `transfer_max_fee`, and for a
>   `transfer_max_fee` that has more decimal places than the token or does not
>   fit the token's amount type. The first three need no token decimals and
>   are refused before any node is asked for the token; the cap is converted
>   once the token's decimals are known, which may take a read of the contract.
> - *Which error.* The platform routes, in both forms, and `enable_erc20`
>   answer `InvalidPayload` (HTTP 400). `task::enable_erc20` has no such
>   member in its error set: it reports the refusal in the task status as
>   `Transport`, the token chapter 35 R35.3A.6 gives an invalid-payload class
>   failure of the token construction.
> - *Where the settings live.* The validated provider binding (the host-only
>   base, the two credentials, the service provider, the timeout and the poll
>   interval) lives on the TRX platform coin; a TRC20 token resolves it from
>   its platform coin. The enabled token block, with the cap in the token's
>   smallest unit, lives on the token coin. The legacy `enable` and `electrum`
>   route has neither member. Nothing sends a provider request yet: the
>   withdraw rail reads both settings.
> - *Secrets (R9).* The debug form of the request and of the binding shows
>   neither credential and names the URL by its host; no refusal repeats a
>   value of the object; a `tron_gasless_provider` or `gasless` value that is
>   not an object, and a member of the wrong JSON type, fail request decoding
>   with a fixed text that repeats no part of the value.

> *Status note (2026-11-20, Reloaded-original), TRON switched on.* The note
> above says that TRON stays behind `TRON_ACTIVATION_AVAILABLE` (`false`) and
> that no request reaches §63.3.1 and §63.3.2. `TRON_ACTIVATION_AVAILABLE` is
> `true` in this tree (chapter 21 D6 and its status note of this date): the
> activation requests reach both, and the reports answer the
> `gasfree_address` of §63.3.3. The withdraw rail is still not built, so
> nothing sends a provider request.

> *Status note (2026-11-20, Reloaded-original), the withdraw rail.* The last
> sentence of the note above describes the tree before the sign-only rail of
> §63.8 and §63.8A was built. With that rail (the status note after §63.3.6) a
> gasless withdraw reads the provider's token list and account record; it
> still submits nothing to the provider.

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

> *Status note (2026-11-20), §63.3.3 built, and the report rule for R29
> (Reloaded-original; takes no reference input).*
>
> - *Placements.* `gasfree_address` stands:
>   - in the shared per-address entry of an HD report, hence in the HD form
>     of the platform-with-tokens result, in `get_new_address`,
>     `account_balance`, `task::account_balance::init`,
>     `task::scan_for_new_addresses::init` and in the HD forms of the token
>     results;
>   - in both address-information blocks of the single-address
>     platform-with-tokens result, also when `get_balances` is `false`;
>   - in `wallet_balance` of the single-address task-based token result,
>     beside `address`;
>   - in each entry of the `balances` map of the one-shot token result,
>     beside `spendable` and `unspendable` (the map is keyed by the address
>     and has no other member next to it);
>   - in the legacy `my_balance` answer, beside `address`.
>
>   `task::create_new_account::init` is refused for every TRON coin
>   ([Chapter 21](21-tron-integration.md) R21.10B.4), so that placement does
>   not arise. For every other coin, and for a TRON coin whose platform has no
>   provider binding, the member is omitted, never `null`.
> - *One place.* The value comes from one function of the EVM coin family.
>   Every HD report entry of that family is made by one constructor that calls
>   it, and the single-address results call the same function; no RPC attaches
>   the member afterwards (the finding in §63.10).
> - *Report rule for R29.* A report does not show a custody address that the
>   controller contradicts. Before an address is shown, the check of R29 runs
>   for it, with its 10-minute confirmation:
>   - the controller confirms the derived address, now or within the last
>     10 minutes: the member carries it;
>   - the controller reports another address: the member is omitted for that
>     address, and one warning line is logged per check;
>   - the controller cannot be read: the member carries the locally derived
>     address, as the reference row shows it (R18); nothing is remembered, so
>     the next report reads the controller again.
>
>   None of the three outcomes fails the balance read or the activation. The
>   confirmations are kept per platform coin and shared with its tokens; they
>   are gone after a restart and after the platform is activated again. With
>   a provider configured, every report reads the controller once per address
>   at most every 10 minutes, so an HD scan with gap N makes up to N reads,
>   empty entries included. The refusal of the gasless rail in R29 is
>   unchanged.

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

> *Status note (2026-11-20), §63.3.4 to §63.3.6 built (Reloaded-original where
> it states a choice; takes no reference input).* TRON stays behind
> `TRON_ACTIVATION_AVAILABLE` (`false`), so no request reaches the rail yet.
>
> - *The request.* `fee_method` and `gasless` are optional members of the
>   shared withdraw request, read by the direct `withdraw` method (legacy and
>   `mmrpc` 2.0) and by `task::withdraw::init`. `gasless.max_fee` is taken as a
>   JSON string or a JSON number; a value that does not read as a decimal
>   amount, a negative value and a value with more decimal places than the
>   token are answered with `InvalidFeePolicy` (HTTP 400), as §63.9.4 states,
>   and not as a request that does not decode. A `fee_method` other than the
>   three values, and a `deadline_seconds` or `fallback_to_native` of the
>   wrong JSON type, fail request decoding. `deadline_seconds` has no upper
>   bound: the caller owns the length of the authorization window.
> - *Where R21 sits.* One check, made by the direct method and by
>   `task::withdraw::init` right after the coin is found and before anything
>   else is done with the request. For a coin that is not a TRON coin,
>   `gasless` and `auto` answer `Gasless`/`Unavailable` (HTTP 503). For every
>   coin, a TRON coin included, `gasless` options without `fee_method`
>   `gasless` or `auto` answer `InvalidFeePolicy` (HTTP 400). On the task route
>   both are answered by the `init` call itself; no task is started.
> - *The error family (§63.9.1, D63.2 (a)).* The outer `error_type` is
>   `Gasless`; `error_data` is an object whose `error_type` is one of the seven
>   members and whose own `error_data` is free human-readable text. A client
>   classifies by the two `error_type` values and the HTTP status.
> - *The fee details (§63.3.5, D63.2 (a)).* The tag is `TronGasless`. `coin` is
>   the ticker of the token; `signed_max_fee` is always present on this path;
>   `trace_id` is always present and `null`.
> - *The progress phases (D63.2 (a)).* The sign-only path reports
>   `FetchingGaslessQuote` and `SigningGaslessAuthorization`.
>   `SubmittingGaslessAuthorization`, `WaitingForGaslessProvider` and
>   `WaitingForGaslessSettlement` are reserved for the submission flow (D63.6)
>   and are not part of the status vocabulary yet.

> *Status note (2026-11-20, Reloaded-original), TRON switched on.* The note
> above says that TRON stays behind `TRON_ACTIVATION_AVAILABLE` (`false`) and
> that no request reaches the rail. `TRON_ACTIVATION_AVAILABLE` is `true` in
> this tree ([Chapter 21](21-tron-integration.md) D6 and its status note of
> this date): a withdraw request for an activated TRC20 token reaches the
> rail. The rail is sign-only (§63.8A): the node does not hand the signed
> authorization to the provider, and no RPC does until the submission flow
> exists (D63.6). Until then a token at the custody address moves only when
> the caller submits the authorization to the provider.

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

> *Status note (2026-11-19; Reloaded-original, takes no reference input).*
> The provider's published API wraps the array: `data` is an object whose
> `tokens` member holds the entries, and each entry also carries `createdAt`
> and `updatedAt`. Source: the provider's public developer documentation
> (§63.14). The implementation decodes `data.tokens`.

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

> *Status note (2026-11-19), creation bytecode (Reloaded-original; takes no
> reference input).* Under the sourcing rule above, the bytes Reloaded embeds
> are taken from the chain: the input of the transaction that deployed the
> GasFree contracts on each network.
>
> | Network | Deploying transaction | Block | Length | keccak-256 of the creation bytecode |
> | --- | --- | --- | --- | --- |
> | Mainnet | `7f5e37bc9d78822d5b72bf90ea65444208c6a42b9b3b2f811666a992ba42f037` | 69944703 | 997 bytes | `28122b164158c3961d140d546ce24bb496fa91bff2b3cbc3f073d4eb5d998b22` |
> | Nile | `df2f6668a4b7dd086f34876f919db63afa1e1649c81a07594552c85540feae1d` | 52537691 | 997 bytes | `8a1613b444604b6dc63c348f90741c31aef84360be2e9aaa142422c90059fcd4` |
> | Shasta | `a6a20ec22185c82111f1ceceb70ccb144040f71a659c5e15643bcab7fe445aae` | 66317600 | 997 bytes | `52a8ac900543cec486e4d3c7bfd67d80b2a3e4b42e1b6a694382ee8498cdafc9` |
>
> The mainnet and Nile bytes are not equal: they differ in 32 bytes at offsets
> 954 to 985, the compiler's metadata hash. Each equals the bytes SDK v1.1.2
> publishes for its network. The Shasta controller was upgraded on 2026-07-04
> (transaction
> `c5ee6dcd2030b125b16309701b9d8046b14ae3ca294c017b2d734641a8067287`). The
> Shasta bytes of SDK v1.1.2 (1208 bytes, keccak-256
> `490791365cc571bb1659ec928507a7b5f69ab6c65a8cdf7ee9a4a8c61b2f3477`) and its
> five Shasta custody-address vectors describe the implementation that was
> live before that upgrade, and no longer match the live Shasta controller.
> Decision D63.5 (a) therefore holds as written for mainnet and Nile; for
> Shasta the source of the T1 vectors is the live controller's read-only
> `getGasFreeAddress(address)`. The sentences of this section about the SDK's
> Shasta bytecode and vectors are left as written.

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

   *Status note (2026-11-19).* The members around the nested signed
   authorization -- their names, JSON types, value sources, presence
   and position in the withdraw result -- are stated one by one in
   §63.8A (R25-R28, T15-T16, D63.8). Where this step and §63.8A
   differ in precision, §63.8A is the more precise statement. §63.8A
   passed the KDF Dirty Gate on 2026-11-19.
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

> *Status note (2026-11-20), §63.8 steps 1 to 6 and §63.8A built
> (Reloaded-original where it states a choice; takes no reference input).*
>
> - *Order of the refusals.* For a TRON coin: a sender without a local key
>   (R22); a `fee` descriptor and a `memo`
>   ([Chapter 21](21-tron-integration.md) R21.10B.7); the request-shape
>   rejections of §63.9.4 that need no amount (`gasless` options with the
>   native rail, an unusable `max_fee`, a zero `deadline_seconds`), for
>   `gasless` and for `auto` alike; a max-amount request under `gasless`
>   (R14). All of these come before any node or provider request. An amount of
>   zero and a `max_fee` finer than the token are refused when the gasless
>   rail is entered, before its first node or provider request.
> - *R22.* The TRON withdraw refuses a sender for which the node holds no
>   local key with `UnsupportedError`, as its first step. The earlier refusals
>   stay in front of it: the direct method and the task route refuse a coin
>   under the hardware-wallet policy, and a browser build refuses the
>   browser-extension policy ([Chapter 49](49-withdrawal-task-path.md) R49.9,
>   R49.27a). This repository has no WalletConnect signer for the EVM family
>   that a TRON coin could be activated with.
> - *The gasless rail and TRX.* The rail makes no TRX fee estimate: it reads
>   neither the sender's TRX balance nor its resources nor the chain's prices,
>   builds no transaction and makes no dry run of the transfer. The check that
>   the TRX balance covers the fee, which the native TRC20 path makes, is not
>   applied; a sender without any TRX is served. Its node reads are the
>   controller's `getGasFreeAddress` (R29) and the token balance of the custody
>   address, the latter only when the preflight needs it.
> - *Step 3.* The lock is one lock per sending address and platform coin,
>   held from the controller check to the signature. Nothing is submitted, so
>   the lock orders the node's own gasless withdraws of one address; it does
>   not see a transfer another client submits. Two sign-only results taken
>   back to back carry the same provider nonce, because nothing is submitted
>   between them (D63.6); the lock does not change that. The EVM family's
>   nonce lock is one lock for all EVM sends and is not used by the TRON
>   paths.
> - *Step 6.* "Not configured for the coin" covers native TRX, a token
>   activated without `gasless.enabled`, and a token whose platform coin has
>   no provider binding or is not active. With `fallback_to_native` the
>   native rail then runs with the request as it is, and its result or its
>   error is the answer.
> - *Errors (§63.9.2, §63.9.3, D63.3 (c)).* The custody shortfall is answered
>   with `error_type` `NotSufficientBalance` and the shared `error_data`
>   (`coin`, `available`, `required`, the custody account's figures in token
>   units); its human-readable text names the custody address and says that
>   the balance of the sending address is not used. A provider failure of the
>   shared `Transport` arm (401, 403, 429, 5xx, no connection, the timeout)
>   has one fixed text at the RPC; its kind is written to the log. A failed
>   node read (the controller, the custody balance) is the node's error, as
>   on the native rail.
> - *The result (D63.8 (a)).* The members of R26 without the selector of row
>   4, `coin` once, `from` the address array, the shared members of R28, no
>   `tx_hex` and no `tx_hash`. The payload's members follow the shared members
>   in the answer text, where R26 records them ahead; nothing depends on the
>   order. `memo` is omitted, as in every withdraw result of this repository
>   outside the Tendermint family, where R28 records `null`. `timestamp` is
>   the time of `created_at`. `spent_by_me` is the amount plus
>   `total_token_fee`.
> - *Amounts.* Every token amount of the rail is a 256-bit integer in the
>   token's smallest unit, and the signed `value` and `maxFee` are hashed as
>   their 32-byte big-endian words, over the whole 256-bit range. The shared
>   typed-data encoder takes integers up to 128 bits; within that range its
>   hash is required to equal the rail's own before anything is signed.

> *Status note (2026-11-21), submission and trace at the reference row.*
> What the reference row has of the submission flow, and what it does not
> implement, is stated point by point in §63.8B (R30-R35, the finding,
> D63.9, T18-T20). In short: no RPC of the reference row submits or traces;
> the flow that D63.6 defers takes no reference input beyond §63.4 and
> R31-R33. §63.8B passed the KDF Dirty Gate on 2026-11-21.

## 63.8A The Sign-Only Result: Members Around the Signed Authorization

*(Added 2026-11-19 by the KDF Spec Reader; reference facts at the
rows of chapter 02 §2.15A named below; passed the KDF Dirty Gate on
2026-11-19, measure 0.01, attestation in §63.15. It adds to §63.8 step 5
and replaces none of its sentences.)*

**Reference states.** At the row `dev (as of 2026-03-16)` there is
no GasFree withdraw: no fee-rail selector, no gasless rail and no
off-chain payload of any shape. Everything below is the row
`feat/tron-gasfree (as of 2026-05-04)`, the only cited state that
has the rail. No test at that row checks a complete withdraw answer
of the gasless rail; the facts below were established by reading how
the answer is assembled and serialized, not by running the rail.

R25. **No wrapper member; the payload sits at the top level
    (reference fact).** The off-chain payload is not carried under a
    member of its own. Its members are top-level members of the
    shared transaction-details object of
    [Chapter 49](49-withdrawal-task-path.md) R49.25, beside the
    shared members -- the same arrangement as the unsigned results of
    [Chapter 36](36-tendermint-v2-activation-rpcs.md) R36.14.17. The
    result carries **no `tx_hex` and no `tx_hash`**; the two members
    are omitted, not empty. This holds for the direct `withdraw`
    answer (legacy and `mmrpc` 2.0 form) and for the terminal payload
    of `task::withdraw::status` alike.

R26. **Member list (reference fact).** The payload contributes
    exactly the nine members below, emitted in this order and ahead
    of every shared member. A JSON object is unordered and nothing at
    the reference row depends on the order; it is recorded for
    byte-level comparison only.

    | # | Member | JSON type | Value | Present |
    | --- | --- | --- | --- | --- |
    | 1 | `relay_type` | string | the fixed text `tron_gasfree` | always |
    | 2 | `chain_id` | string | the chain id of the platform's Tron network -- the number the typed-data domain of §63.6 uses -- written as decimal digits (`728126428` mainnet, `2494104990` Shasta, `3448148188` Nile) | always |
    | 3 | `coin` | string | the ticker of the token being withdrawn (the request's `coin`) | always |
    | 4 | `from` | object | the request's own `from` selector, returned in the form it was sent: either `account_id` (integer), `chain` (`External` or `Internal`) and `address_id` (integer), or a single `derivation_path` (string) | only when the request carried `from`; otherwise the member is omitted, not `null` |
    | 5 | `from_address` | string | the sending address in Tron Base58Check form (`T...`); it equals the nested `user` | always |
    | 6 | `gasfree_address` | string | the custody (GasFree) address of the sender, Base58Check; the address the preflight of §63.7 settled on, equal to `gasfree_address` of the fee details (§63.3.5) | always |
    | 7 | `verifying_contract` | string | the GasFree controller contract of the platform's network (§63.5; the `verifyingContract` of §63.6), Base58Check | always |
    | 8 | `signed_authorization` | object | the nested object of §63.8 step 5 | always |
    | 9 | `created_at` | string | the UTC time the payload was built, RFC 3339 with whole seconds and the `Z` suffix | always |

    Inside `signed_authorization` the four address members (`token`,
    `service_provider`, `user`, `receiver`) are Base58Check strings;
    the five integer members are decimal strings, `deadline` being
    Unix seconds; and `sig` is the 65-byte signature as 130
    hexadecimal digits in lower case with no `0x` prefix, whose last
    byte is 27 or 28. Its ten members are emitted in the order §63.8
    step 5 lists them.

R27. **What the payload does not carry (reference fact).** There is
    no expiry member beside `created_at`: the only expiry in the
    result is the nested `deadline`. There is no member for the
    provider's name or address, no trace identifier (the fee details
    carry `trace_id`, `null` on this path), no numeric form of the
    chain id, no hexadecimal form of any address, and no sender
    public key. The token contract, the service provider, the
    receiver and the amount appear only inside
    `signed_authorization`; the account or path of the sender appears
    only as the optional `from` of row 4.

R28. **Shared members on this path, and two repeated names
    (reference fact).** The shared members follow the nine and carry:
    `from` (an array holding the sending address) and `to` (an array
    holding the receiver), both Base58Check; `total_amount` (the
    requested amount); `spent_by_me` (amount plus the total token
    fee); `received_by_me` (the amount when the receiver is the
    sender, otherwise zero); `my_balance_change` (received minus
    spent); `block_height` 0; `timestamp` (integer Unix seconds);
    `fee_details` (§63.3.5); `coin`; an empty `internal_id`; the
    default `transaction_type`; and `memo` `null`. `kmd_rewards` is
    absent.

    Two names of R26 are also shared names: `coin` (same value in
    both places) and `from` (row 4 is a selector object; the shared
    member is an array of strings). The answers named in R25 are
    written out member by member with no merging step, so the HTTP
    answer text contains `coin` twice and, when the request carried
    `from`, `from` twice -- the selector object first, the address
    array later. A client whose JSON reader keeps the last of two
    equal names therefore never sees the selector. On a route that
    first builds the result as a JSON tree, one member per name
    survives: the later value, at the earlier position. Which routes
    do so was not examined one by one in this pass.

    *Status note (2026-11-20, Reloaded-original).* Decided 2026-11-20:
    Reloaded's sign-only result has no `memo` member where this rule
    records `memo` `null`. Every withdraw result of this repository outside
    the Tendermint family omits an absent memo, and a TRON withdraw refuses
    a request that carries a memo, so the member would always be `null`. The
    other shared members are as this rule states them. The deviation is
    recorded in `RELOADED_VS_GLEEC.md`.

> **Reading the payload back (informative; not bound).** The
> reference row defines a typed reading of the nine members that
> refuses any member outside the nine and accepts a missing `from`;
> the nested object is read with `version` required to be exactly 1
> and does not itself refuse extra members. Nothing at the reference
> row performs that reading on any RPC path (submission is deferred,
> D63.6), so it is not observable today. Because the nine members are
> mixed with the shared ones, a consumer has to pick them out by
> name; the whole result object is not a valid payload under that
> reading.

> **Code-quality finding (informative; trigger: apparent while
> reading the payload and the shared result object side by side).**
> The repeated `coin` and `from` of R28 put two members of the same
> name into one JSON object, and a client that keeps the last of
> duplicate names does not see the selector the payload was given.
> Unless D63.8 chooses byte parity, Reloaded should emit each name
> once. See D63.8.

D63.8. **Shape of the sign-only result in Reloaded (for the
    maintainer; open).** Options:

    - (a) *Client sees:* the nine members at the top level as in R25
      and R26, `coin` once, and `from` always as the address array;
      no selector. *Reloaded must:* emit each name once and omit the
      selector of row 4.
    - (b) *Client sees:* as (a), plus the selector under a member
      name the reference does not have. *Reloaded must:* emit each
      name once and choose and document that Reloaded-only name.
    - (c) *Client sees:* the reference's answer text byte for byte,
      with `coin` twice and, when the request carried `from`, `from`
      twice. *Reloaded must:* reproduce the repeated names of R28 in
      the reference's order.

    *Recommendation:* (a).

    Under every option the result has no `tx_hex` and no `tx_hash`,
    which Chapter 49 R49.25 otherwise makes mandatory; the Chapter 49
    pointer of 2026-11-19 records that.

    *Status note (2026-11-19, clean side).* Decided 2026-11-19: option
    (a). Reloaded's sign-only result carries the members of R26 at the
    top level except the selector of row 4, emits `coin` once, and gives
    `from` always as the address array of R28; T16 applies in its (a)
    form.

T15. **Sign-only result members.** A sign-only gasless withdraw of a
    TRC20 token, once without and once with a `from` selector under
    an HD wallet, through the direct method and through the task
    path: the result has no `tx_hex` and no `tx_hash`; it carries the
    members of R26 with the stated JSON types; `chain_id` is the
    decimal text of the network's chain id; `from_address` equals the
    nested `user`; `gasfree_address` equals the fee details'
    `gasfree_address`; `verifying_contract` is the network's
    controller; `created_at` parses as RFC 3339 UTC with whole
    seconds and is earlier than the nested `deadline`; no member of
    R27 is present (R25-R27).

T16. **One member per name (conditional on D63.8).** Under D63.8
    (a) or (b): the serialized answer text of T15 contains each
    member name of the result object at most once, and its `from` is
    as the chosen option states. Under D63.8 (c) this test is
    replaced by a byte-parity check of the answer text against the
    repeated names and order of R28.

    *Status note (2026-11-20), T15 and T16.* The tests exist in their D63.8
    (a) form (`coins/eth/tron/gasfree/withdraw_tests.rs`, the `t15_t16_`
    tests), for a single-key login and for an HD login with both forms of
    `from`, on the direct method and on the task route. They also check that
    the signature recovers to the sending address for the typed-data hash.

## 63.8B Submission and Trace: What the Reference Row Has and What It Does Not

*(Added 2026-11-21 by the KDF Spec Reader, round 0.5.0-19; reference
facts at the rows of chapter 02 §2.15A named below; passed the KDF
Dirty Gate on 2026-11-21, measure 0.02, attestation in §63.15. It adds to
§63.8, §63.8A and D63.6 and replaces none of their sentences.)*

**Reference states.** The row `feat/tron-gasfree (as of 2026-05-04)`
is the only tabled state that has the gasless rail. The rows `dev (as
of 2026-03-27)`, `staging (as of 2026-03-27)`, `v3.0.0-beta`, `main
(as of 2026-10-07)` and `feat/tron-swap-support (as of 2026-04-23)`
were searched whole-tree on 2026-11-21 and carry no GasFree code at
all. The branch tips of the historical record were listed on the same
day: the tip of the GasFree branch is still the tabled one, and no
other branch carries the feature. Every statement below is therefore
the row `feat/tron-gasfree (as of 2026-05-04)`, and "not implemented"
means not implemented at that row, with no later state to consult.
The facts were established by reading; no test at that row submits or
traces a transfer.

R30. **No RPC submits and no RPC traces (reference fact).** The
    direct `withdraw` method and the `task::withdraw::*` methods end
    at the sign-only result of §63.8A; neither makes a submit or a
    trace request, automatically or on a later call. There is no
    additional method for submitting a signed authorization and none
    for tracing a `trace_id`. The raw-transaction broadcast method
    (`send_raw_transaction`) does not take the off-chain payload: for
    a Tron coin it accepts hexadecimal transaction text only, and a
    `tx_json` object is refused as text that is not hexadecimal,
    before any node or provider request. The provider client offers
    the submit and trace operations of §63.4.1 as two operations that
    nothing in the tree calls.

R31. **The two uncalled client operations (reference fact).** These
    are the only flow-related properties the row has; a caller built
    on top of them would observe:

    - *One attempt each.* Submit is one `POST`, trace is one `GET`,
      each under the per-call timeout of §63.3.1. Neither is repeated
      after a failed connection, a timeout or a 5xx answer; there is
      no retry count and no back-off.
    - *Refused before sending.* A submit payload whose `version` is
      not 1 is not sent. A trace identifier that does not read as a
      UUID is not sent. Both are the "request the client could not
      form" condition of §63.9.3.
    - *`requestId`.* The member is optional in the submit payload and
      is left out of the body when no value is given (§63.4.6). Nothing
      at the row generates a value, so there is no rule for how one is
      made or whether a repeated submit reuses it.
    - *No payload is built.* Nothing at the row turns the nested
      signed authorization of §63.8A into the submit payload of
      §63.4.6; the two documents exist side by side.
    - *Answer checks.* A submit or trace answer is checked while it
      is decoded, as §63.4.7 and §63.9.3 state (the envelope, the
      lifecycle sets, `version` 1, the hash and signature forms), and
      in no other way. Its `nonce`, addresses and amount are not
      compared with what was signed, and its `state` is not acted on.
    - *A 404.* A 404 status, or an envelope `code` of 404, from
      either operation falls under "a 4xx other than 401, 403 or 429"
      of §63.9.3, so it would be `ProviderRejected` (502).

R32. **Two declared errors that nothing raises (reference fact).**
    `TraceNotFound` (404) and `QuoteExpired` (400) are members of the
    family of §63.9.1 with the statuses stated there. No condition at
    the row produces either: no request path, no provider answer and
    no elapsed deadline leads to them. In particular the signed
    `deadline` is not looked at again after signing.

R33. **Three declared phases that nothing reports (reference fact).**
    `SubmittingGaslessAuthorization`, `WaitingForGaslessProvider` and
    `WaitingForGaslessSettlement` are part of the task status
    vocabulary and are never set. Their wire form follows from how
    the vocabulary is declared, in the place where
    `task::withdraw::status` carries the phase of a task in progress
    ([Chapter 49](49-withdrawal-task-path.md)):

    | Phase | Wire form |
    | --- | --- |
    | `SubmittingGaslessAuthorization` | the bare string, like the phases without members |
    | `WaitingForGaslessProvider` | an object with that one member name, whose value is an object with `trace_id` (string) and `provider_state` (string) |
    | `WaitingForGaslessSettlement` | an object with that one member name, whose value is an object with `trace_id` (string) and `tx_hash` (string, or `null` when there is none; the member is always present) |

    Which provider `state` or `txnState` value leads to which of the
    two waiting phases is not implemented, and neither is the text
    `provider_state` would carry or the form of `tx_hash`.

R34. **Lock and result at the row (reference fact).** The per-sender
    lock of §63.8 step 3 is taken before the preflight and held until
    the sign-only result is assembled; it is released when the
    withdraw returns. Nothing is submitted, so it is never held
    across a provider submit. The result is always the one §63.8A
    states: `trace_id` is `null`, there is no `tx_hash`, and no
    provider `state` or `txnState` appears at the top level or in the
    fee details. The terminal `Ok` payload of the task is that same
    result.

R35. **Not implemented at the row (reference fact).** Each of the
    following has no behavior at the row `feat/tron-gasfree (as of
    2026-05-04)`, so Reloaded decides it without reference input:

    - when a submitting call returns, and any bound on a wait (a
      count, a time, or the authorization deadline);
    - the result after a submission, and what the task's final `Ok`
      status carries beyond §63.8A;
    - which provider values count as settled;
    - how the failure endings map to errors: `state` `FAILED`,
      `txnState` `ON_CHAIN_FAILED`, a deadline that passes while
      waiting, a wait bound that is used up;
    - when `QuoteExpired` and `TraceNotFound` are raised (R32);
    - a trace of an existing `trace_id` outside the withdraw;
    - generation and reuse of `requestId`, and any retry (R31);
    - whether the lock is held through submission (R34);
    - what a submit answer in `state` `FAILED`, or one whose `nonce`,
      `version` or addresses differ from what was signed, leads to
      (R31 states the only existing check: `version` 1 at decoding);
    - cancellation during submit or wait: `task::withdraw::cancel` is
      the shared cancel of Chapter 49 R49.24 with no gasless-specific
      part, and with nothing submitted it can only stop a quote or a
      signature;
    - any record: a gasless withdraw writes nothing to the
      transaction history or to any store, `my_tx_history` gains no
      row from it, and the GasFree code of the row writes no log line
      at any level;
    - any use of `status_poll_interval_ms`: the value is read at
      activation (§63.3.1) and consumed by nothing.

> **Code-quality finding (informative; trigger: apparent while
> reading the error family and the client's status classification
> side by side).** The family declares `TraceNotFound` with HTTP 404
> for a trace identifier the provider does not know (§63.9.1), while
> the client classifies a 404 from the trace request with every other
> 4xx, which the collapse of §63.9.3 reports as `ProviderRejected`
> (502). Wired together as they stand, the two would never produce
> `TraceNotFound`. Reloaded's trace path should single out the 404 of
> the trace request (the HTTP status or the envelope `code`) and
> report it as `TraceNotFound`; D63.9 item 5.

D63.9. **The submission flow in Reloaded (for the maintainer; open;
    Reloaded-original, takes no reference input).** That the node
    submits and traces is decided. R35 lists what the reference
    leaves open. The points below follow from text of this chapter
    that is already cleared, or from the reason a cleared rule gives;
    they are recommendations of the author, not reference facts:

    1. *Lock.* Hold the per-sender lock of §63.8 step 3 until the
       submit answer is in. The lock exists so that the provider
       nonce bound into the authorization is not raced (§63.8 step
       3), and the provider's nonce moves only with a submission.
    2. *Submit payload.* Build it from the signed values themselves,
       so that every member of §63.4.6 equals the corresponding
       member of the nested signed authorization (R5, R6).
    3. *Submit answer.* Compare the answer's `nonce`, `version`,
       sender, receiver, token and amount with what was signed and
       report a difference as `InvalidProviderResponse` (502), the
       member §63.9.1 gives an answer that cannot be trusted.
    4. *`QuoteExpired`.* Raise it when the signed `deadline` has
       passed before the submit request is sent, the meaning §63.9.1
       states; R7 already requires an expired deadline to be refused.
    5. *`TraceNotFound`.* Raise it for a 404 of the trace request
       (the finding above).
    6. *`trace_id`.* Fill the fee details' `trace_id` (§63.3.5) with
       the `id` of the submit answer.
    7. *Wait bound.* Bound every wait; the signed `deadline` is the
       one bound the authorization itself carries.

    Left without a recommendation, for the maintainer: which call
    submits and when it returns; the settled rule and the mapping of
    the provider's values to the two waiting phases; the errors of
    the failure endings; `requestId` and retries (how the provider
    treats a repeated `requestId` has to be read from its public
    documentation, §63.14, first); cancellation; what is recorded and
    logged; whether a trace method exists outside the withdraw.

T18. **Client attempts and pre-send refusals.** Against a loopback
    provider: a submit and a trace are one request each; a failed
    connection, a timeout and a 5xx answer are followed by exactly as
    many further requests as Reloaded's decision under D63.9 states
    (none at the reference row, R31); a submit payload with a
    `version` other than 1 and a trace identifier that is not a UUID
    are refused with no request made to the provider.

T19. **Phase wire form.** If Reloaded reports the three phases, the
    task status carries them in the forms of R33: the bare string for
    `SubmittingGaslessAuthorization`; `trace_id` and `provider_state`
    as strings for `WaitingForGaslessProvider`; `trace_id` as a
    string and `tx_hash` as a string or `null`, never omitted, for
    `WaitingForGaslessSettlement`.

T20. **Trace 404 (conditional on D63.9 item 5).** A 404 from the
    trace request, as an HTTP status and as an envelope `code`,
    reaches the caller as `Gasless`/`TraceNotFound` with HTTP 404,
    and a 404 from the submit request stays `ProviderRejected` (502).

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

> *Status note (2026-11-21).* At the reference row `TraceNotFound` and
> `QuoteExpired` are declared with these statuses and raised by nothing
> (§63.8B R32); when each is raised is Reloaded's to decide (D63.9 items 4
> and 5).

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

    *Status note (2026-11-17).* A bounded accessor of this kind now exists
    (`mm2_net::transport::slurp_post_json_with_headers_bounded`, see
    [Chapter 26](26-cross-platform-and-wasm.md) §26.8). It sets
    `Content-Type: application/json` so that the caller cannot override it,
    does not follow redirects and never renders a header value. It does not
    yet return the response headers on the browser target (the map is empty
    there), so R23's browser sentence stays open until the GasFree rail
    needs it.

    *Status note (2026-11-19).* The GET-with-headers form now exists as a
    bounded accessor too:
    `mm2_net::transport::slurp_url_with_headers_bounded(url, headers,
    timeout_s)`, with the same signature on both targets. The whole exchange
    is bounded, a redirect is not followed and no header value is rendered;
    on the browser target the returned header map is empty, as for the POST
    accessor. The provider's read calls of §63.4.1 use it.

R24. **No silent substitution without consent (Reloaded-original).**
    A rail switch -- native to gasless under `auto`, or gasless to
    native under `fallback_to_native` -- MUST be visible in the
    result: the returned fee details' `fee_method` (or the native
    fee-details shape) tells the caller which rail executed. The
    project MUST NOT switch rails under `fee_method = gasless`
    unless `fallback_to_native` is set.

R29. **Custody derivation checked against the controller
    (Reloaded-original, 2026-11-19).** Before the gasless rail uses a
    custody address for a user on a network, whenever no confirmation of
    that address younger than 10 minutes exists (so after every restart
    of the node, and at least every 10 minutes while the rail is used),
    the implementation compares the address it derives (§63.5) with the
    controller's read-only `getGasFreeAddress(user)`. On inequality the
    gasless rail is refused for that network with `Unavailable` (HTTP 503)
    and one warning line, nothing is signed, and the native rail stays
    available. This refusal is not a deterministic unavailability for
    §63.8 step 6. *(Takes no reference input.)*

    *Status note (2026-11-20).* The same check runs before a report shows a
    custody address; what a report does with each outcome is stated in the
    status note after §63.3.3.

    *Status note (2026-11-20), the check in the withdraw rail and the switch
    (Reloaded-original; takes no reference input).* The gasless rail runs the
    check for the sending address before it asks the provider anything, with
    the confirmations the reports of the same platform coin use. A contradicted
    address refuses the rail with `Gasless`/`Unavailable` (HTTP 503) and one
    warning line; nothing is signed, the provider is not asked and nothing is
    remembered. A controller that cannot be read fails the withdraw with the
    node's error (`Transport`). Under `auto` either outcome is the answer of an
    attempted rail (R16).

    The refusal is not a deterministic unavailability, so `fallback_to_native`
    alone does not turn it into a native withdraw. An operator can change
    that with the top-level `MM2.json` boolean
    `tron_gasfree_guard_allows_fallback` (default `false`): with `true`, a
    request that carries `fallback_to_native: true` runs the native rail after
    the refusal and gets the native fee details (R24). The switch never causes
    a fallback for a request without `fallback_to_native`, and it changes
    nothing for a failed read of the controller. A value that is not a JSON
    boolean counts as `false`; the first time such a value is read, one
    warning line says so.

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

    *Status note (2026-11-19).* For Shasta the five vectors are the
    answers of the live controller's `getGasFreeAddress(address)`, because
    the Shasta vectors of SDK v1.1.2 predate the controller's upgrade of
    2026-07-04 (§63.5 status note); the test also requires the derived
    address to differ from the SDK's value.

T2. **TIP-712 vectors.** The `PermitTransfer` domain separator,
    struct hash and full typed-data hash reproduce the published
    Nile vectors of the public SDK (one domain separator, five
    messages). For mainnet and Shasta the test pins the domain
    separator computed from the per-network chain id and
    controller; a lab row checks one hash per network against the
    controller contract's public read-only message-hash function.

    *Status note (2026-11-19).* The per-network live check reads the
    controller's `getMessageHashForTransfer` for the first Nile message
    vector and compares the answer with the locally computed full hash.

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

    *Status note (2026-11-20).* The tests exist, through the direct `withdraw`
    method against a loopback node and a loopback provider
    (`coins/eth/tron/gasfree/withdraw_tests.rs`, the `t8_` tests): they also
    check that the gasless rail reads none of the inputs of the TRX fee
    estimate and serves a sender without TRX.

T9. **Activation configuration.** The per-network request base of
    §63.4.1 formed from a host-only `base_url`, rejection of a
    path-bearing base URL, the defaults of
    R20, the `enabled: false` equivalence and the negative-cap
    rejection of §63.3.2, and the Tron-only/provider-required
    activation gates are exercised.

    *Status note (2026-11-20).* The tests exist: the provider object and the
    token block at the coin level (the request base through a loopback
    provider, the path-bearing URL, the defaults, the malformed
    `service_provider`, the cap conversion), and the activation gates on the
    one-shot and task forms of the platform and token routes against a
    loopback node, for a TRON and for an EVM platform.

T10. **Custody address placements.** With a provider configured on
    a Tron platform, every RPC §63.3.3 lists carries
    `gasfree_address`, and none of them carries it without a
    provider or for a non-Tron coin (R18).

    *Status note (2026-11-20).* The tests exist, through the RPCs: the
    platform-with-tokens result in its single-address and HD forms, the
    one-shot and the task-based token result, `get_new_address`,
    `account_balance`, `task::account_balance::init`,
    `task::scan_for_new_addresses::init` and the legacy `my_balance`, each
    with the member equal to the local derivation for its address; without a
    provider, and for an EVM coin, the member is absent from the answer text.
    The report rule for R29 (§63.3.3 status note) is tested on the same
    answers: a contradicting controller omits the member, an unreadable one
    leaves the derived address, and a second report within 10 minutes makes
    no controller read.

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

    *Status note (2026-11-20), T11 to T13.* The tests exist in the same file.
    T11 covers every condition of §63.9.3, with one text for all conditions of
    the `Transport` arm, and every outcome of §63.7. T12 runs under the
    hardware-wallet policy, on the direct method, on the task route and at the
    TRON withdraw itself, and counts the node and provider requests; no
    WalletConnect signer exists for the EVM family here, so that policy is
    covered by the refusal of a sender without a local key. T13 runs for an
    EVM coin and for a UTXO coin, on the direct method and on
    `task::withdraw::init`.

T14. **Nile end-to-end (lab row; deferred with D63.6).** A
    sign-and-submit round trip on the Nile testnet against the
    public test provider, verifying the custody address the provider
    reports, a successful preflight, and (once D63.6 lands)
    settlement tracking to a terminal state; plus a browser-target
    smoke test of the header-bearing transport. No end-to-end test
    exists at the reference row.

T17. **Controller check (R29) (Reloaded-original, 2026-11-19).** A
    match is confirmed and reused for 10 minutes; after that, or after a
    restart, the controller is read again; a mismatch refuses with
    `Unavailable` (503), signs nothing and keeps nothing cached; a failed
    read is reported through the node error.

    *Status note (2026-11-20).* The tests exist for the check itself
    (`coins/eth/tron/gasfree/custody.rs`) and, through the withdraw, for the
    rail and for the switch `tron_gasfree_guard_allows_fallback`
    (`coins/eth/tron/gasfree/withdraw_tests.rs`).

## 63.12 Deferred Work

*Decisions D63.1–D63.5 for the maintainer live in §63.12A below;
this section holds the deferrals D63.6 and D63.7.*

D63.6. **Submission and settlement tracking.** Wire the signed
    authorization to the provider submit endpoint and poll the
    trace endpoint for settlement, emitting the submit/wait
    progress phases of §63.3.6 and populating `trace_id`/tx-hash
    in the fee details and status. Today's withdraw is sign-only
    (§63.8).

    *Status note (2026-11-21).* The reference row implements none of this
    flow; §63.8B (R30-R35) records what it has and what it leaves open, and
    D63.9 lists the points Reloaded decides.

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

    *Status note (2026-11-20), the recovery procedure (D63.4 (b);
    Reloaded-original; takes no reference input).* Tokens that stay at the
    custody address are moved back with a gasless withdraw whose `to` is the
    user's own address:

    ```json
    {
      "method": "withdraw",
      "mmrpc": "2.0",
      "params": {
        "coin": "USDT-TRC20",
        "to": "<the user's own TRON address>",
        "amount": "12.5",
        "fee_method": "gasless"
      }
    }
    ```

    The amount plus the provider's fee must be at the custody address, so the
    largest amount that can be returned is the custody balance less the fee the
    provider quotes; `max` is not taken on this rail (R14). The answer is the
    sign-only result of §63.8A: it is a signed authorization, not a
    transaction, and until the submission flow exists (D63.6) the node does
    not hand it to the provider. A residue below the provider's fee cannot be
    returned this way. What is owed for a provider that no longer answers
    stays deferred.

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
- *Scoped addition (2026-11-19, Spec Reader round 0.5.0-16) -- §63.8A
  (R25 -- R28, the read-back note, the code-quality finding, D63.8, T15
  and T16) and the status note under §63.8 step 5, cleared together with
  the pointer after chapter 49 R49.26; no other text of this chapter was
  changed.* **KDF Dirty Gate: PASS on 2026-11-19, measure 0.01, no
  findings** (chapter 01 R40; the attestation closes this item).
  *Inputs:* the forbidden corpus at the row `feat/tron-gasfree (as of
  2026-05-04)` of chapter 02 §2.15A, read read-only and consulted **only**
  to establish how the sign-only result of the gasless withdraw is
  assembled and serialized (its members, their JSON types, value sources,
  presence and order, and the nested signed authorization), and the row
  `dev (as of 2026-03-16)`, searched whole-tree, to establish that it has
  no GasFree withdraw. D63.8, T15 and T16 are original to this section. No
  row was added to chapter 02 §2.15A: both states already have rows.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-19, measure
  0.01, no findings, in a main pass and a delta pass after the options of
  D63.8 were reworded for neutrality. *Extent:* the Tron GasFree gasless
  withdraw surface of §63.8 and §63.8A -- the assembly and serialization
  of the sign-only withdraw result, the off-chain payload and the nested
  signed authorization -- at the row `feat/tron-gasfree (as of
  2026-05-04)`; the row `dev (as of 2026-03-16)` searched whole-tree (no
  GasFree withdraw). *Method:* reading plus mechanical comparison (a
  per-token lookup, an identifier scan, and word n-gram overlap at lengths
  4, 5 and 6); the repository's clean-room gate tool was not applicable
  because the additions embed no code blocks. *Measure:* 0.01, on the
  discretionary-expression basis.
- *Status note added 2026-11-19 -- after D63.8.* Reloaded-original, clean
  side: it records the maintainer's decision of 2026-11-19 (option (a));
  no forbidden-class input, chapter 01 R38 not engaged.
- *Additions of 2026-11-19:* R29, T17 and the status notes dated
  2026-11-19 (§63.4.5, §63.5, R23, T1, T2) are Reloaded-original and take no
  reference input.
- *Additions of 2026-11-20:* the status notes dated 2026-11-20 (after
  §63.3.2, §63.3.3, R29, T9 and T10) are Reloaded-original, clean side: they
  record what is built and the report rule for R29, and take no reference
  input.
- *Further additions of 2026-11-20:* the status notes dated 2026-11-20 after
  §63.3.6, before §63.8A, after R28, the second one after R29, and those
  after D63.7, T8, T13, T16 and T17 are Reloaded-original, clean side: they
  record the sign-only withdraw rail as built, the omitted `memo` of the
  sign-only result, the switch `tron_gasfree_guard_allows_fallback` and the
  recovery procedure, and take no reference input.
- *Addition of 2026-11-20:* the second status note of this date after
  §63.3.2 (TRON switched on) is Reloaded-original, clean side: it corrects
  the switch value the note before it names, from chapter 21 D6 and this
  repository's code, and takes no reference input. No cleared sentence was
  edited.
- *Additions of 2026-11-20 (TRON switched on, the withdraw rail):* the
  third status note of this date after §63.3.2 and the second one after
  §63.3.6 are Reloaded-original, clean side: they correct, from chapter 21
  D6 and this repository's code, what the notes before them say about the
  switch and about the withdraw rail not being built, and take no reference
  input. No cleared sentence was edited.
- *Scoped addition (2026-11-21, Spec Reader round 0.5.0-19) -- §63.8B
  (R30 -- R35, the code-quality finding, D63.9, T18 -- T20) and the three
  status notes of this date (before §63.8A, after §63.9.1, under D63.6),
  with the pointer after the sign-only pointer of chapter 49; no other text
  of this chapter was changed and no cleared sentence was edited.*
  **KDF Dirty Gate: PASS on 2026-11-21, measure 0.02, no findings** (chapter
  01 R40; the attestation closes this item). *Inputs:* the forbidden corpus
  at the row `feat/tron-gasfree (as of
  2026-05-04)` of chapter 02 §2.15A, read read-only and consulted **only**
  to establish whether and how a signed authorization is submitted and
  traced: the gasless withdraw flow, the provider client's submit and trace
  operations and its status classification, the submit and answer payload
  declarations, the gasless error family, the task status vocabulary, the
  raw-transaction broadcast entry for a Tron coin, and whole-tree searches
  for callers of the submit and trace operations, for the two unraised
  errors and the three unreported phases, and for log and history writes;
  the rows `dev (as of 2026-03-27)`, `staging (as of 2026-03-27)`,
  `v3.0.0-beta`, `main (as of 2026-10-07)` and `feat/tron-swap-support (as
  of 2026-04-23)`, searched whole-tree, to establish that they carry no
  GasFree code; and the historical record's branch list (reference
  metadata, chapter 01 R6). D63.9 and T18 -- T20 are original to this
  section. No row was added to chapter 02 §2.15A: every state read already
  has a row.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-21, measure
  0.02, no findings, in a single pass. *Extent:* the TRON GasFree
  fee-delegation subsystem as this chapter scopes it -- the gasless
  withdraw flow, the provider client with its submit and trace operations
  and its status classification, the submit and answer payload
  declarations, the gasless error family, the long-running withdraw task
  status vocabulary and the raw-transaction broadcast entry -- at the row
  `feat/tron-gasfree (as of 2026-05-04)`: six source files of that row
  read, four whole or nearly whole and two in the relevant part, and
  whole-tree searches of that row for callers of the submit and trace
  operations, the two unraised errors, the three unreported phases, the
  lock and the poll-interval setting; the rows `dev (as of 2026-03-16)`,
  `dev (as of 2026-03-27)`, `staging (as of 2026-03-27)`, `v3.0.0-beta`,
  `main (as of 2026-10-07)` and `feat/tron-swap-support (as of
  2026-04-23)` searched whole-tree for the feature only; on the candidate
  side, all 238 added lines of this chapter and chapter 49. *Method:*
  reading plus mechanical comparison (every identifier-shaped token of
  the additions searched in the compared row and judged forced or free,
  and a probe of the additions for private-name patterns); the
  repository's clean-room gate tool was not run because the additions
  embed no code blocks. *Measure:* 0.02, on the discretionary-expression
  basis (dictated-interop and public-surface tokens excluded).
