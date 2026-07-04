# Chapter 23 -- External Trading-API Client

**Status:** driving-spec

> **One-sentence claim:** the project carries a self-contained
> binding crate for external trading-API providers; its first
> provider binds the publicly-documented 1inch Swap API v6.0 and
> the matching portfolio price-history endpoint, exposes typed
> request and response shapes for both, and never embeds an
> API key, default base URL, or production rate-limit policy.

## 23.0 Executive Summary

A dedicated workspace crate carries bindings to external
trading-API providers (price oracles, swap aggregators, route
indexers). The chapter-bound crate contains exactly
one provider binding: a typed client for the publicly-
documented **1inch Swap API v6.0** plus the matching portfolio
price-history endpoint. The crate is structured so that a
second provider would live as a sibling module rather than
replace the first.

The crate is **a library only**. It does not register any
public JSON-RPC handler, does not depend on any coin support
module, and is not consumed by the daemon's runtime path at the
time of writing.

**Port status.** The 1inch binding crate is present in reloaded as
a **library**. The integration boundary — the public classic-swap
JSON-RPC surface, plus the EVM allowance/approval and
transaction-submission wiring — is **required but NOT yet
implemented in reloaded**. Per the project's PORT decision this is
a **binding driving-spec requirement**, not optional deferred
work; the required public method surface and its request/response
shapes are specified normatively in §23.8A.

The bound surface for the 1inch provider covers:

- A typed HTTP client that holds a caller-supplied base URL
  and routes requests to the correct path-template per
  endpoint and per chain id.
- Builders for the **classic-swap quote** and **classic-swap
  create** (transaction-build) requests, the **liquidity-
  sources** and **tokens** discovery requests, and the
  **portfolio cross-prices** OHLC request.
- Typed response records for each of the above, including a
  shared classic-swap response shape covering both quote and
  create returns.
- An error enum that distinguishes invalid-parameter, out-of-
  bounds, transport, body-parse, generic API, and the
  provider-specific `AllowanceNotEnough` failure (the last
  carrying the required allowance and the current allowance as
  256-bit integer values).

## 23.1 Subsystem Shape

The crate is organised by **provider**: each provider gets its
own submodule containing its client, error type, URL builder,
and typed request/response records. The crate's public surface
re-exports one provider submodule per binding.

| Region                  | Responsibility                                  |
|-------------------------|-------------------------------------------------|
| Crate-level public API  | Re-exports one provider submodule per binding   |
| Provider submodule      | Client, URL builder, errors, typed records      |
| Provider client         | Stateless HTTP entry point bound to a base URL  |
| Provider URL builder    | Endpoint path + query-parameter composition     |
| Provider error type     | Provider-specific error enum                    |
| Provider request types  | Builder structs for each endpoint               |
| Provider response types | Typed records for each endpoint                 |

The crate **does not** define a provider-agnostic trait. A
second provider added later would live as a sibling submodule
(`<provider>`) with its own client, its own error type, and
its own URL builder. A provider-agnostic abstraction is an open
question (§23.9 D1), not a binding rule of the chapter-bound substrate.

## 23.2 1inch Provider -- Endpoints

The 1inch provider binds the following routes of the public
1inch v6.0 API:

| Path template                                                            | Purpose                       |
|--------------------------------------------------------------------------|-------------------------------|
| `GET /swap/v6.0/{chainId}/quote`                                         | Indicative swap quote         |
| `GET /swap/v6.0/{chainId}/swap`                                          | Build executable swap tx      |
| `GET /swap/v6.0/{chainId}/liquidity-sources`                             | Enumerate router protocols    |
| `GET /swap/v6.0/{chainId}/tokens`                                        | Enumerate supported tokens    |
| `GET /portfolio/integrations/prices/v1/time_range/cross_prices`          | OHLC token-pair price history |

Chain ids accepted on the four `/swap/v6.0/{chainId}/...`
endpoints correspond to the publicly-documented v6.0-supported
EVM chains:

| Chain id   | Chain ecosystem        |
|-----------:|------------------------|
| 1          | Ethereum               |
| 10         | Optimism               |
| 56         | BNB Smart Chain        |
| 100        | Gnosis                 |
| 137        | Polygon                |
| 250        | Fantom                 |
| 324        | zkSync Era             |
| 8217       | Klaytn                 |
| 8453       | Base                   |
| 42161      | Arbitrum               |
| 43114      | Avalanche              |
| 1313161554 | Aurora                 |

The chain id is interpolated into the path for the four swap
routes; any chain id outside the set above is rejected with
the `InvalidParam` variant of the provider error type.

The portfolio route does **not** carry the chain id in the
path; the chain id travels as a query parameter on the
portfolio request type.

## 23.3 Configuration and Authentication

R1. **No embedded base URL.** The 1inch base URL shall be
    sourced from the daemon's JSON configuration field
    `1inch_api`. If the field is absent, the client shall fail
    fast at construction with the provider's `InvalidParam`
    error variant. The crate shall not carry a compiled-in
    default URL.

R2. **No embedded API key.** Production builds shall not carry
    any API key for any provider. The public 1inch endpoints
    do not require authentication; the binding shall therefore
    issue unauthenticated requests on production builds.

R3. **Test-only authentication path.** A test build path,
    gated behind the **`test-ext-api`** Cargo feature, may
    additionally:
    - Read an authentication token from the environment
      variable `ONE_INCH_API_TEST_AUTH` and attach it as an
      `Authorization` header on every request.
    - Serialise outbound requests through a one-request-per-
      second lock so that test runs stay inside the test-tier
      rate limit of the provider.
    Neither behaviour shall be active in release builds.

R4. **Standard headers.** Every request carries the headers
    `Accept: application/json` and `Content-Type:
    application/json`. The conditional `Authorization` header
    of R3 is added only on the test-only build path.

## 23.4 Request and Response Shapes

The 1inch classic-swap **quote** request carries:

- *Required:* source token address, destination token address,
  amount (as a wei-denominated decimal string).
- *Optional:* fee, protocols filter, gas-price hint,
  complexity-level hint, parts count, main-route parts count,
  gas-limit hint, include-tokens-info flag, include-protocols
  flag, include-gas flag, connector-tokens list.

The 1inch classic-swap **create** request extends the quote
request with:

- *Required additionally:* sender address (`from`), slippage
  percentage in the range 0..=50.
- *Optional additionally:* excluded-protocols filter, permit
  payload, compatibility flag, alternative receiver address,
  referrer, disable-estimate flag, allow-partial-fill flag,
  use-permit2 flag.

Both endpoints return a shared classic-swap response shape
carrying:

| Field           | Type                            | Populated by             |
|-----------------|---------------------------------|--------------------------|
| `dst_amount`    | decimal string                  | quote and create         |
| `src_token`     | optional token-info record      | quote and create         |
| `dst_token`     | optional token-info record      | quote and create         |
| `protocols`     | optional triple-nested protocol-info list | quote and create |
| `tx`            | optional transaction-fields record | create only           |
| `gas`           | optional 128-bit gas estimate   | quote only               |

The transaction-fields record on a create response carries the
fields needed to sign and broadcast the swap transaction
(sender, recipient, calldata, value, gas price, gas limit).
Signing and broadcast are out of scope for this crate.

The portfolio cross-prices request carries chain id, token-0
address, token-1 address, optional granularity, and optional
limit. The response is a series of OHLC records keyed by
timestamp; numeric fields use a big-decimal type so the
response can be deserialised without precision loss.

R5. **Numeric precision.** Provider response shapes shall use
    big-decimal or 256-bit-integer types for any field
    representing an on-chain amount, an allowance, an OHLC
    price, or a wei-denominated value. Floating-point types
    shall not be used for any such field.

## 23.5 Error Model

The provider error enum distinguishes the following failure
modes:

| Variant               | Carries                                                    |
|-----------------------|------------------------------------------------------------|
| Invalid parameter     | Description of the violated invariant                      |
| Out-of-bounds         | Parameter name, value, declared minimum, declared maximum  |
| Transport             | Inner transport-layer error                                |
| Parse body            | Body-decode message                                        |
| General API           | Provider's `error` message, description, HTTP status code  |
| Allowance not enough  | The provider's error/description/status code **plus**     |
|                       | required allowance and current allowance, each as a       |
|                       | 256-bit unsigned integer                                   |

R6. **Allowance shortfall carries machine-actionable data.**
    The provider's 400-with-meta body for an
    `allowance is not enough` failure shall be parsed into the
    `AllowanceNotEnough` variant with the required and current
    allowance promoted to the typed 256-bit unsigned integer
    used by EVM coin support. The variant is the surface
    through which an allowance-approval flow (deferred D5
    below) reads the amounts it needs.

R7. **No HTTP-status mapping in the crate.** The crate shall
    not implement the project's
    `HttpStatusCode` mapping trait. Mapping provider errors
    to RPC-layer HTTP status codes is the responsibility of
    the future RPC handlers (D2).

## 23.6 Networking

R8. **Transport-layer indirection.** All outbound HTTP traffic
    shall flow through the workspace's cross-platform HTTP
    transport (see [Chapter 26](26-cross-platform-and-wasm.md))
    rather than calling a concrete HTTP-client crate directly.
    This is what allows the same client code to compile and
    run on both native and browser targets.

R9. **GET-only wire surface.** Every endpoint bound by this
    crate is a `GET` with query parameters. No `POST` body and
    no streaming endpoint is in scope.

The URL builder for each provider is responsible for:

- Prepending the configured base URL.
- Interpolating the chain id into the path component where the
  endpoint template requires it.
- Serialising typed request parameters into URL query
  parameters.
- Validating that numeric parameters lie within their declared
  bounds; out-of-bound values surface as the out-of-bounds
  error variant of §23.5 before any network call is made.

## 23.7 Error Parsing and the Allowance-Shortfall Wire Envelope

The error model of §23.5 is produced by parsing the provider's
HTTP error bodies. The provider dictates the wire shapes below;
the field spellings are the provider's documented camelCase JSON
and are reproduced here as externally-dictated interop (R29 wire-
format / R33 third-party-api-bound), not as project expression.

**R10-A — Error wire envelope (R29/R33, externally dictated).**
A 1inch error response on an HTTP 400 carries a JSON object with
these fields:

| Wire field    | JSON type        | Meaning                       |
|---------------|------------------|-------------------------------|
| `error`       | string           | short error token             |
| `description` | string, optional | human-readable description    |
| `statusCode`  | integer          | echoed HTTP status            |
| `meta`        | array, optional  | typed metadata entries        |
| `requestId`   | string, optional | provider request id (ignored) |

Each `meta` entry is an object with a `type` string and a
`value` string. The binding consumes `requestId` only so that
decode does not reject the field; it is never surfaced to
callers.

**R10-B — Recognised `meta.type` tokens (R33).** The two
`meta.type` tokens the binding acts on are the provider's
documented values `allowance` and `amount`. Any other token is
treated as unknown, and the body falls through to the general-
API-error path.

**R10-C — Allowance-shortfall promotion.** When a 400 body's
`meta` array contains an entry of type `allowance`, the binding
MUST:

- read the `value` of that entry as the current allowance;
- read the `value` of a sibling entry of type `amount` as the
  required allowance;
- decode both decimal strings into the workspace's 256-bit
  unsigned integer type;
- produce the allowance-not-enough variant of §23.5 carrying the
  provider `error`, `description`, echoed status code, and the
  two decoded 256-bit values.

A 400 body with no `allowance` meta entry MUST instead produce
the general-API-error variant.

**R10-D — Other error bodies.** A non-400 error response MUST be
reported as the general-API-error variant, reading the top-level
`error` string from the body (empty when absent) together with
the echoed status code. A 400 body that fails to decode against
the envelope of R10-A MUST be reported as the body-parse-error
variant of §23.5.

**R10-E — Lenient amount decode.** An `allowance`/`amount` value
that does not parse as a decimal 256-bit integer MUST decode to
zero rather than aborting the parse. This is a deliberate
functional choice: the downstream allowance-approval consumer
(deferred D5) treats a zero current allowance as "no approval on
record" and is not broken by the substitution, whereas surfacing
a parse error here would mask the actionable allowance shortfall.

**Binding scope of §23.7 (R36).** The wire field spellings and
`meta.type` tokens above are dictated by the public 1inch API and
bind as interop (R29/R33). The error-variant *shapes* are the
§23.5 contract. Any Rust type names, private helper or
deserialisation structs, helper decomposition, field
identifiers, and the Display/diagnostic wording used to realise
this parsing are informative under R36: a re-derivation that
decodes the same wire envelope and produces the same §23.5
variants with different internal naming or decomposition is
conformant.

## 23.8 HTTP Client and URL Composition

The networking contract of §23.6 is realised by a stateless
client namespace plus a URL composer. This section states the URL
grammar, request behaviour, and dictated interop the client MUST
produce. The grammar, path tokens, provider constants, supported-
chain set, and header set are dictated by the public 1inch v6.0
API and bind as interop (R29/R33); the internal Rust shape used to
realise them is informative (R36).

### 23.8.1 Provider constants (R33, externally dictated)

R11-A. The binding carries the following 1inch v6.0 protocol
values — published provider constants, not project choices — and
MUST expose them to callers:

- **Aggregation router (v6.0) contract address:**
  `0x111111125421ca6dc452d289314280a0f8842a65`.
- **Native-asset sentinel address**, used by the provider to
  denote the chain's native coin in token positions:
  `0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee`.
- **Supported chain set:** the twelve `(ecosystem, chain id)`
  pairs enumerated in §23.2.

The binding MUST additionally expose a predicate that answers
whether a given chain id is in the supported set above (used to
reject unsupported chains per R1 of §23.2).

### 23.8.2 URL grammar (R33, externally dictated)

R11-B. The composed request URL MUST follow the 1inch path
grammar:

```
<base-url>/<endpoint-prefix>/<chain-id>/<method-token>?<query>
```

where, for the four swap routes, `<endpoint-prefix>` is
`swap/v6.0` and `<method-token>` is one of `quote`, `swap`,
`liquidity-sources`, or `tokens`; and for the portfolio route the
prefix is `portfolio/integrations/prices/v1`, the chain-id path
segment is omitted (the chain id travels as a query parameter per
§23.2), and the method token is `time_range/cross_prices`. The
path segments MUST be joined in the order prefix → chain id (swap
routes only) → method token, so the chain id is interpolated
between the version-pinned endpoint prefix and the method token;
query parameters are appended last. These prefixes and method
tokens are 1inch URL grammar, not project expression.

### 23.8.3 Base URL and headers

R11-C. **Base URL resolution.** On production builds the client
MUST resolve the base URL from the daemon configuration field
`1inch_api` (per R1); absence MUST surface as the invalid-
parameter error variant of §23.5 before any network call is
made. A test build path MAY substitute a fixed provider test
host. A base URL that fails to parse MUST surface as the invalid-
parameter variant.

R11-D. **Header set (R33 / standard content negotiation).** Every
request MUST carry `accept: application/json` and
`content-type: application/json`. On the `test-ext-api` build
path only, an `Authorization` header sourced from the
`ONE_INCH_API_TEST_AUTH` environment variable (per R3) MUST be
added. These are the standard JSON content-negotiation headers
and, for the authorization header, the provider's documented
test-tier authentication scheme; none shall be present in
release builds beyond the two content-negotiation headers.

### 23.8.4 Request execution

R11-E. **Call sequence.** A typed request call MUST:

1. on the `test-ext-api` build path only, acquire the test-tier
   rate-limit guard (R11-G) for the duration of the call;
2. issue a `GET` for the composed URL through the chapter-26
   cross-platform HTTP transport with the header set of R11-D,
   mapping any transport failure to the transport error variant
   of §23.5;
3. decode the response body once into a generic JSON value,
   mapping a decode failure to the body-parse-error variant;
4. on a non-`200` status, route the generic value through the
   error-parsing contract of §23.7 and return the resulting
   error variant;
5. on a `200` status, decode the same generic value into the
   caller's requested typed response shape, mapping a decode
   failure to the body-parse-error variant.

The decode-once-to-value-then-branch-on-status shape is a
functional requirement: success and error bodies share the
provider's JSON envelope at the transport layer but deserialise
to different typed shapes, so the status code selects which shape
the already-decoded value is interpreted as.

R11-F. **Diagnostics.** The binding MAY emit debug-level
diagnostics around the outbound URL and the response body.
Diagnostic wording is not part of the contract.

### 23.8.5 Test-tier rate limiting

R11-G. **One-request-per-second guard (test builds only).** On
the `test-ext-api` build path the client MUST serialise outbound
requests so that no two requests issue within one second of each
other, keeping test runs inside the provider's test-tier rate
limit (R3). The guard MUST NOT be present in release builds. The
mechanism used to realise it (a process-wide async lock plus a
one-second delay held across the request) is informative under
R36.

**Binding scope of §23.8 (R36).** The URL grammar, path tokens,
provider constants, supported-chain set, and header set above are
dictated by the public 1inch v6.0 API and bind as interop
(R29/R33). The error-routing and decode-branch behaviour are the
functional contract. All Rust type names, private struct and
field names, helper/marker types, local variables, control-flow
decomposition, and diagnostic wording used to realise this
section are informative: a re-derivation that emits the same
URLs, headers, and decode/error behaviour with different internal
naming or structure is conformant. Residual similarity of the
realisation to the historical lineage is governed by the R35
gate, under which a thin REST-path composer of this kind retains
little discretionary expression once the dictated grammar and
interface are excluded.

## 23.8A Required Port — Classic-Swap RPC Surface and EVM Wiring (driving-spec)

**STATUS.** The capabilities in this section are **required but
NOT yet implemented in reloaded; the 1inch binding crate is
present** as a library. Per the PORT decision these are binding
requirements, not optional deferred work. An implementer MUST land
the public classic-swap RPC surface (§23.8A.1) and the EVM
allowance/approval wiring (§23.8A.2).

### 23.8A.1 Public RPC method surface

**RP1.** The following **five JSON-RPC v2 methods** MUST be
registered under the `experimental::1inch_v6_0::` namespace. The
method strings are the wire contract:

| Method (`experimental::1inch_v6_0::…`) | Binds endpoint (§23.2)                       | Purpose                              |
|----------------------------------------|----------------------------------------------|--------------------------------------|
| `classic_swap_contract`                | (no call) provider constants (§23.8.1)       | Resolve the aggregation-router and native-asset-sentinel addresses for a chain |
| `classic_swap_quote`                   | `GET /swap/v6.0/{chainId}/quote`             | Indicative swap quote                |
| `classic_swap_create`                  | `GET /swap/v6.0/{chainId}/swap`              | Build an executable swap transaction |
| `classic_swap_liquidity_sources`       | `GET /swap/v6.0/{chainId}/liquidity-sources` | Enumerate router protocols           |
| `classic_swap_tokens`                  | `GET /swap/v6.0/{chainId}/tokens`            | Enumerate supported tokens           |

**RP2 — request/response shapes.** Each handler's request
identifies the target chain, and that identification differs by
method:

- `classic_swap_quote` and `classic_swap_create` select the chain
  **indirectly**, through two coin-ticker request fields — `base`
  (source coin) and `rel` (destination coin). The handler resolves
  each ticker to its activated EVM coin and derives the numeric
  chain id from the `base` coin; both coins MUST resolve to the
  same supported chain. There is **no** explicit chain-id field on
  these two requests.
- `classic_swap_liquidity_sources` and `classic_swap_tokens` select
  the chain **directly**, through an explicit numeric `chain_id`
  request field.
- `classic_swap_contract` takes an **empty** request (no chain
  field): it returns provider constants only.

Each handler's request also carries the typed parameters of §23.4
for the bound endpoint; each handler's response is the typed
record of §23.4/§23.5 for that endpoint (the shared classic-swap
response for quote and create, the liquidity-sources list, the
token map, or the router/sentinel address record for
`classic_swap_contract`). The numeric-precision rule R5 (§23.4)
and the dictated 1inch wire field spellings (§23.7, §23.8) bind
unchanged.

**RP3 — error mapping.** The handlers MUST map the crate's
provider error enum (§23.5) onto the project's typed-error
envelope and HTTP status codes; this is the `HttpStatusCode`
mapping that R7 (§23.5) deliberately keeps OUT of the library and
assigns to the RPC layer. The `AllowanceNotEnough` provider error
(§23.5 R6) MUST surface to the caller carrying the required and
current allowance as 256-bit unsigned integers.

### 23.8A.2 EVM allowance / approval and submission wiring

**RP4.** A `classic_swap_create` flow against an ERC-20 source
token MUST integrate with EVM coin support so that:

- the current ERC-20 allowance of the aggregation-router
  (§23.8.1) over the source token is observable, and
- an ERC-20 `approve` raising that allowance to at least the
  required amount can be issued, before the swap transaction is
  submitted.

The top-level allowance methods `get_token_allowance` and
`approve_token` are the intended interface for these two steps.
These two methods **do not exist in reloaded yet** (only internal
EVM-coin helpers do); they are themselves **part of this required
port** and their full request/response/error wire contract is
specified in §23.8A.4. The `AllowanceNotEnough` condition (RP3) is
the machine-actionable trigger that tells a caller (or an
orchestration layer) the approval is required and by how much.

**RP5.** Signing and broadcasting the transaction-fields record
returned by `classic_swap_create` (§23.4) is performed by EVM coin
support, NOT by the trading-API library (R11 / D7 hold: the
library stays handler-free and coin-free). The port adds the RPC
handler and the coin wiring around the library, leaving the
library's library-only posture intact.

**RP6 — separate from liquidity routing.** The `find_best_quote`
method belongs to a **distinct** `experimental::liquidity_routing::`
namespace (a separate routing feature) and is NOT part of this
1inch classic-swap surface; it is out of scope for this chapter.
This chapter binds only the five `1inch_v6_0::classic_swap_*`
methods above.

### 23.8A.3 Acceptance criteria

- AC1. All five `experimental::1inch_v6_0::classic_swap_*` methods
  are reachable through the public dispatcher and return the
  §23.4/§23.5 typed shapes.
- AC2. A chain id outside the supported set (§23.2) is rejected
  with the invalid-parameter error variant before any network
  call (consistent with §23.2 / §23.8.1).
- AC3. A `classic_swap_create` against an under-approved ERC-20
  source token surfaces `AllowanceNotEnough` carrying the required
  and current allowance, and an approval issued via `approve_token`
  followed by a retry succeeds.
- AC4. The trading-API library remains handler-free and coin-free
  after the port (R11 / R12 still hold).

### 23.8A.4 Public RPC wire contract (request/response/error field tables)

This subsection pins the **binding wire contract** — the exact
public JSON-RPC v2 request, response, and error field names a GUI
client exchanges with the daemon — for all seven required-port
methods. These RPC-layer field spellings are distinct from the
1inch upstream HTTP query-parameter spellings of §23.7/§23.8
(which the handler emits onward to the provider); the names below
are what a client sends to, and receives from, the daemon. Every
request/response shape in this subsection is **binding wire
contract**. Field types are JSON types; "optional" means the field
may be omitted (a serde default applies where noted). Where a
request reuses a §23.4 parameter, the exact RPC field spelling is
listed here rather than re-derived.

**Method strings (as registered in the dispatcher / sent on the
wire).** The first five are dispatched under the
`experimental::1inch_v6_0::` prefix; the last two are top-level
(unnamespaced) methods:

| Capability | Wire method string |
|------------|--------------------|
| Router-address resolution | `experimental::1inch_v6_0::classic_swap_contract` |
| Classic-swap quote | `experimental::1inch_v6_0::classic_swap_quote` |
| Classic-swap create | `experimental::1inch_v6_0::classic_swap_create` |
| Liquidity-sources discovery | `experimental::1inch_v6_0::classic_swap_liquidity_sources` |
| Tokens discovery | `experimental::1inch_v6_0::classic_swap_tokens` |
| ERC-20 allowance read | `get_token_allowance` |
| ERC-20 approval | `approve_token` |

#### Shared numeric representation

- Coin-denominated **input** amounts (request `amount`) are sent as
  a decimal value (JSON number or decimal string, with fraction).
- The destination amount in the classic-swap response (`dst_amount`)
  is returned in coin units as the workspace detailed-decimal
  representation (a decimal value with its fraction/rational
  companions per the workspace numeric convention).
- Per-token raw amounts on the allowance/approval path are
  coin-unit big-decimal values (with fraction).
- Allowance figures carried inside the allowance-insufficient error
  are 256-bit unsigned integers (the workspace U256 JSON encoding).

#### `classic_swap_contract` — request / response

- **Request:** an **empty JSON object** (`{}`); no chain field.
- **Response:** a bare JSON **string** equal to the 1inch
  aggregation-router contract address for v6.0,
  `0x111111125421ca6dc452d289314280a0f8842a65` (the on-chain
  address a caller approves against). The native-asset sentinel
  address used elsewhere by the provider is the dictated constant
  `0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee` (§23.8.1).

#### `classic_swap_quote` — request

| Field | JSON type | Req? | Bounds / notes |
|-------|-----------|------|----------------|
| `base` | string | required | source coin ticker (resolved to an EVM coin; supplies the chain id) |
| `rel` | string | required | destination coin ticker (must resolve to the same chain) |
| `amount` | decimal (number or string) | required | sell amount in `base` coin units (with fraction) |
| `fee` | number | optional | partner fee share, min 0, max 3 |
| `protocols` | string | optional | comma-separated liquidity-source allow-list |
| `gas_price` | string | optional | network gas price in Gwei |
| `complexity_level` | integer | optional | min 0, max 3 |
| `parts` | integer | optional | max 100 |
| `main_route_parts` | integer | optional | max 50 |
| `gas_limit` | integer | optional | max 11500000 |
| `include_tokens_info` | boolean | optional | serde default `true` |
| `include_protocols` | boolean | optional | serde default `true` |
| `include_gas` | boolean | optional | serde default `true` |
| `connector_tokens` | string | optional | comma-separated token-connector list |

Unknown request fields are rejected (strict deserialization).

#### `classic_swap_create` — request

All `classic_swap_quote` request fields above (identical spellings,
types, optionality, and bounds) **plus**:

| Field | JSON type | Req? | Bounds / notes |
|-------|-----------|------|----------------|
| `slippage` | number | required | allowed slippage, min 0, max 50 |
| `excluded_protocols` | string | optional | comma-separated exclude-list, max 5 |
| `permit` | string | optional | EIP-2612 permit blob |
| `compatibility` | boolean | optional | exclude the Unoswap method |
| `receiver` | string | optional | recipient address; defaults to caller address |
| `referrer` | string | optional | partner-fee recipient address |
| `disable_estimate` | boolean | optional | |
| `allow_partial_fill` | boolean | optional | |
| `use_permit2` | boolean | optional | Permit2 auto-approval |

Unknown request fields are rejected (strict deserialization).

#### Shared classic-swap response (`classic_swap_quote` and `classic_swap_create`)

| Field | JSON type | Presence |
|-------|-----------|----------|
| `dst_amount` | detailed-decimal amount | always |
| `src_token` | object (token-info) | present when source token info is requested/available; omitted otherwise |
| `src_token_kdf` | string or null | source coin ticker as named in the coins config, when resolvable |
| `dst_token` | object (token-info) | present when destination token info is requested/available; omitted otherwise |
| `dst_token_kdf` | string or null | destination coin ticker as named in the coins config, when resolvable |
| `protocols` | array (3-level nested array of route-hop objects) | present when route protocols are requested/available; omitted otherwise |
| `tx` | object (transaction-fields) | present only for `classic_swap_create`; omitted otherwise |
| `gas` | integer or null | estimated gas; populated chiefly for `classic_swap_quote` |

The `tx` object (returned by `classic_swap_create`, to be signed
and broadcast by EVM coin support) carries:

| `tx` field | JSON type | Notes |
|------------|-----------|-------|
| `from` | string (address) | |
| `to` | string (address) | |
| `data` | string (0x-prefixed hex bytes) | call data |
| `value` | decimal | native value in coin units |
| `gas_price` | decimal | gas price in Gwei |
| `gas` | integer | gas limit |

The `src_token` / `dst_token` objects and the route-hop objects
inside `protocols` carry the **1inch-dictated** field spellings
(e.g. `address`, `symbol`, `name`, `decimals`, `eip2612`, `isFoT`,
`logoURI`, `tags` for token info; `name`, `part`,
`fromTokenAddress`, `toTokenAddress` for a route hop) — these are
provider-sourced interop, not RPC-layer-coined (§23.7/§23.8).

#### `classic_swap_liquidity_sources` — request / response

- **Request:** `chain_id` — integer, required (explicit numeric
  chain id).
- **Response:** object with one field `protocols` — an array of
  liquidity-source descriptor objects, each carrying `id` (string),
  `title` (string), `img` (string URL), and `img_color` (string
  URL). Image URLs are validated against the provider domain before
  being surfaced (anti-phishing, §23.x).

#### `classic_swap_tokens` — request / response

- **Request:** `chain_id` — integer, required.
- **Response:** object with one field `tokens` — a JSON object
  (map) keyed by coin ticker string, each value a token-info object
  using the 1inch-dictated token-info field spellings listed above.

#### `get_token_allowance` — request / response

| Request field | JSON type | Req? | Notes |
|---------------|-----------|------|-------|
| `coin` | string | required | EVM coin ticker |
| `spender` | string (address) | required | 0x-prefixed spender address (e.g. the aggregation router) |

- **Response:** a bare JSON **decimal** (big-decimal, with
  fraction) — the current ERC-20 allowance expressed in `coin`
  units.

#### `approve_token` — request / response

| Request field | JSON type | Req? | Notes |
|---------------|-----------|------|-------|
| `coin` | string | required | EVM coin ticker |
| `spender` | string (address) | required | 0x-prefixed spender address |
| `amount` | decimal | required | allowance to set, in `coin` units (with fraction) |

- **Response:** a bare JSON **string** — the 0x-prefixed hash of
  the broadcast ERC-20 `approve` transaction.

#### Error envelope and HTTP-status mapping (RP3)

Errors use the project's standard tagged error envelope: a
discriminator field `error_type` and a payload field `error_data`.
The allowance-insufficient payload carries the current and required
allowances as 256-bit unsigned integers under `error_data` field
names `allowance` (current) and `amount` (required); the
out-of-bounds payload carries `param`, `value`, `min`, and `max`
(all strings).

**Classic-swap handlers** — condition → HTTP status:

| Condition (behavioural) | HTTP status |
|-------------------------|-------------|
| Unknown / not-activated coin ticker | 404 |
| Coin is not an EVM coin; protocol unsupported; chain unsupported; both coins not on the same chain; address-derivation failure; invalid parameter; parameter out of bounds; numeric-conversion failure; allowance insufficient | 400 |
| Provider transport / body-parse / general provider-API failure; provider-data conversion failure | 502 |
| Internal failure | 500 |

**`get_token_allowance` / `approve_token` handlers** — condition →
HTTP status:

| Condition (behavioural) | HTTP status |
|-------------------------|-------------|
| Unknown coin ticker; coin is not an EVM coin; invalid parameter / numeric-conversion failure | 400 |
| Transaction failure; underlying EVM-RPC failure | 500 |

> **Note (status divergence, informative).** An unknown coin
> ticker resolves to **404** on the classic-swap handlers but to
> **400** on the allowance/approval handlers; an implementer MUST
> preserve this per-surface difference for GUI compatibility.

#### Numeric bases and edge cases (informative)

These notes pin the unit conventions the wire tables above rely on.
They are realization guidance, not new wire fields:

- **`tx.value` decimals.** The `value` field of the `tx` object is
  a native-coin amount and uses the EVM native-coin precision of
  **18 decimals** (the chain's base unit). It is not token-scaled.
- **`tx.gas_price` unit.** The provider returns the raw gas price
  in wei; the response `gas_price` field is that value converted to
  **Gwei** (the unit named in the `tx` table). The inbound 1inch
  value is wei.
- **`dst_amount` / token decimals.** `dst_amount` is expressed in
  `rel`-coin units using the **`rel` coin's own token decimals**
  (which for an ERC-20 token is the token's declared `decimals`,
  not a fixed 18); likewise an ERC-20 `base` amount is scaled by
  the `base` coin's own decimals on the request path. Floating
  point is never used for any of these on-chain amounts (R5).
- **`src_token_kdf` / `dst_token_kdf` on quote/create.** Because
  `classic_swap_quote` and `classic_swap_create` resolve both coins
  before any provider call, both companion ticker fields are always
  populated (non-null) on those two methods. The `null` case in the
  table covers re-use of the shared response shape by a future
  caller that has not resolved a config ticker, not the two methods
  bound here.
- **Native (non-token) EVM coin on the allowance/approval surface.**
  `get_token_allowance` and `approve_token` are ERC-20 token
  operations. A coin that is an EVM **native** coin (not a token)
  has no ERC-20 allowance to read or set; such a request is out of
  scope for these two methods and surfaces the underlying EVM
  operation's failure (a 500-class transaction/EVM-RPC error per
  the table above) rather than a distinct 400 "not a token"
  condition. An implementer MAY refine this to a dedicated 400
  condition, but is not required to.

## 23.9 Binding Requirements

R1-R9 above are binding. In addition:

R10. **Provider isolation.** Each provider's client, error
     type, URL builder, and request/response types shall live
     in a single submodule of the crate. No provider's types
     shall depend on another provider's types.

R11. **Library-only.** The crate shall not register any
     JSON-RPC handler and shall not depend on any coin
     support module. RPC integration and coin wiring live
     outside the crate (D2, D5).

R12. **No vendored provider source.** The crate shall consume
     each provider's public HTTP API only. No provider's
     source code shall be vendored into the crate.

## 23.10 Tests

The crate ships unit tests colocated with each region. The
chapter-bound unit-test set covers:

- Anti-phishing URL validation on the provider client.

End-to-end tests against the live provider API are not in the
chapter-bound test set; they require both the
deferred RPC handlers (D2) and the test-only authentication
build path (R3) configured with a valid test-tier token.

## 23.11 Deferred Work

D1. **Provider-agnostic abstraction.** A trait covering the
    common client surface across providers is an open
    question. The current per-provider-submodule layout
    leaves room for one but does not bind one.

D2. **[REQUIRED PORT — §23.8A.1]** JSON-RPC handler
    registration. The public RPC surface for the 1inch
    provider is the set of five handlers under
    `experimental::1inch_v6_0::` covering router-address
    resolution, classic-swap quote, classic-swap create,
    liquidity-sources discovery, and tokens discovery. These
    handlers are not yet registered in reloaded; landing them
    is a binding requirement, not optional.

D3. **1inch Fusion mode.** Only the classic-swap surface is
    bound in the chapter-bound substrate. The intent-based, resolver-
    filled Fusion variant of the provider's API is not
    bound.

D4. **Portfolio endpoint integration.** The portfolio cross-
    prices request and response types are defined but no
    consumer in the project calls them.

D5. **[REQUIRED PORT — §23.8A.2]** Allowance-approval flow.
    The `AllowanceNotEnough` condition carries enough
    information (R6) for a consumer to issue an ERC-20
    `approve` call before retrying. The `get_token_allowance`
    and `approve_token` methods that this flow relies on are
    **not present in reloaded yet** and are themselves part of
    the port; their wire contract is specified in §23.8A.4.
    Wiring this flow is a binding requirement of the port, not
    optional.

D6. **Production rate-limit policy.** Only the test-only
    build path of R3 serialises requests. A production rate-
    limit policy (per-provider, per-chain, or global) is a
    deferred decision; the crate does not currently impose
    one.

D7. **[REQUIRED PORT — §23.8A.2 RP5]** Transaction signing
    and broadcast. The transaction-fields record returned by
    the classic-swap create endpoint is delivered to the
    caller. The crate does not sign or broadcast; that wiring
    belongs in the integrating RPC handler and the EVM coin
    support module, and is part of the required port (the
    library itself stays handler-free and coin-free).

## 23.12 External References

- The 1inch Swap API v6.0 specification (the public HTTP API
  bound by the first provider).
- The 1inch Portfolio Cross-Prices API specification (the
  public OHLC endpoint bound by the same provider).
- The publicly-documented EVM chain ids of the twelve chains
  enumerated in §23.2.
- The ERC-20 `approve`/`allowance` standard (the basis for
  the typed allowance amounts of §23.5 and the deferred
  approval flow of D5).
- The big-decimal and 256-bit-integer numeric types of R5
  (the workspace's standard numeric substrates for on-chain
  amounts and allowance values).

## 23.13 Baseline Verifications

The following are verifiable from the baseline state defined
in [Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree contains **no** trading-API binding
    crate. A directory listing of the baseline tree
    (`git ls-tree c1d46c0c1592faa0860f704008b2b2381bc3840f`)
    contains no `trading_api` entry; a tree-wide
    `git grep -l '1inch\|one_inch\|trading_api'` against the
    baseline returns no matches.

V2. The baseline tree contains no JSON-RPC handler for any
    1inch endpoint. A tree-wide `git grep` for
    `one_inch_v6_0` against the baseline returns no matches.
    R11 of §23.7 ("library-only") is therefore consistent
    with the baseline state and not a regression from it.

V3. The twelve chain ids enumerated in §23.2 correspond to
    the publicly-documented v6.0-supported EVM chains. The
    list is the provider's published support set; chain ids
    outside the list are out of scope by binding rule R1 of
    §23.2.

V4. The provider's HTTP API is a public specification.
    No provider source code is vendored anywhere in the
    workspace; the binding is via the public HTTP surface
    only (R12).

## 23.14 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`; absence of the
  trading-API binding crate at baseline verified via
  `git ls-tree c1d46c0c1592faa0860f704008b2b2381bc3840f`
  and tree-wide `git grep` for the provider keywords against
  the baseline; the public 1inch Swap API v6.0 specification;
  the public 1inch Portfolio Cross-Prices API specification;
  the publicly-documented EVM chain ids of the twelve chains
  enumerated in §23.2; the ERC-20 `approve`/`allowance`
  standard.
- *Permitted-input classes used:* baseline source; external public
  specifications (1inch Swap API v6.0; 1inch Portfolio Cross-Prices
  API; ERC-20 `approve`/`allowance`); behavioural observation of
  public networks (the publicly-documented EVM chain ids);
  Interop / third-party-API-bound reuse (R29 wire-format / R33
  third-party-api-bound) for the dictated 1inch interop embedded in
  §23.7 and §23.8 — the error wire-envelope field spellings
  (`error`/`description`/`statusCode`/`meta`/`requestId` and the
  `meta` `type`/`value` keys), the `allowance`/`amount` `meta.type`
  tokens, the URL grammar and path tokens, the aggregation-router
  and native-asset-sentinel contract addresses, the supported-chain
  set, and the content-negotiation header names — whose authoritative
  source is the public 1inch v6.0 API, not the historical lineage;
  **public RPC interface recovery / interop wire-format** (the
  daemon-facing JSON-RPC v2 request, response, and error field
  names, the seven public method strings, and the error→HTTP-status
  mapping pinned in §23.8A.4) — these are the public wire contract a
  GUI client relies on and are required for GUI compatibility.
- *Forbidden-corpus consultation (interface recovery only):* the
  historical-lineage corpus was consulted **solely to recover the
  public RPC wire contract** of §23.8A.4 — public method strings,
  public request/response JSON field names and types, dictated
  bounds/enums, and the error-condition→HTTP-status mapping. Only
  clean-channel interface facts crossed into the chapter. No
  protected expression — no function bodies, private identifiers,
  internal error-variant or struct/handler names, helper
  decomposition, control-flow transcription, or diagnostic/Display
  string literals — was reproduced. This consultation is an
  interface/interop recovery, **not** a clean-room derivation of
  protected expression.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* consulted for **interface/interop recovery
  only** (see the *Forbidden-corpus consultation* entry above) — the
  public RPC wire contract of §23.8A.4. Not consulted for clean-room
  derivation of protected expression. The dictated 1inch interop
  fragments enumerated above (R29/R33) are sourced from the public
  1inch v6.0 API documentation; no discretionary expression — no
  function bodies, private identifiers, internal error-variant or
  struct/handler names, helper decomposition, control-flow
  transcription, or diagnostic/Display string literals — from the
  historical lineage crosses into this chapter. The realisation's
  residual similarity to that lineage for the thin REST-path composer
  is governed by the R35 gate (see §23.8 binding-scope note).
