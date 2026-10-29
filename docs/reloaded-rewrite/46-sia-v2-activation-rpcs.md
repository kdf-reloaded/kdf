# Chapter 46 -- Sia Coin V2 (Task-Based) Activation RPC Surface

**Status:** driving-spec. The task-based Sia activation layer is implemented in
reloaded; this revision binds its complete **wire contract** (request schema,
in-progress values, result shape, error discriminants, error-data shapes,
HTTP statuses, backend-failure mapping) and records where the current code
deviates (the `Reloaded status (informative)` notes). The Sia coin itself is
bound by ch. 20.

> **One-sentence claim:** the project shall expose the KDF **long-running,
> task-based standalone-coin** activation surface for the Sia coin -- the
> `task::enable_sia::{init,status,user_action,cancel}` family plus the flat
> `enable_sia` alias -- and the legacy v1 `enable` path for Sia-protocol coins,
> with exactly the wire behaviour of the reference lineage.

> **Treatment:** **T-PORT.** The Sia coin type and its builder, its activation
> request/config/protocol types, and the **shared standalone-coin
> task-activation framework** (the generic standalone `init`/`status`/
> `user_action`/`cancel` entry points already driving the UTXO, Qtum and Z-coin
> task families) are present in reloaded (§46.7). The activation integration for
> Sia (its standalone-activation trait implementation and the dispatcher routes)
> is also present. What remains is wire-parity correction (§46.5 status note)
> and the legacy v1 `enable` leg (§46.6.3).

> **Binding scope (R37).** Requirements bind observable behaviour, the public
> mmrpc-2.0 task method strings and their request/response JSON field names, and
> externally *dictated* interop (the Sia consensus/transaction wire formats and
> the walletd HTTP backend bound by ch. 20, and the client-configuration object
> of the public Sia Rust client library, §46.1.2). Private Rust types, helper
> decomposition, and internal module structure are informative and are **not**
> bound by this chapter.

> **Sources and reference version.** The public KDF API reference does **not**
> document Sia activation at all: there is no page for `task::enable_sia` or
> `enable_sia` (on either the stable or the dev documentation tree); the only Sia
> content is the error catalogue's list of Sia enum names (with placeholder
> variants for the Sia configuration enum). Per the maintainer's decision, where
> the public docs are silent the behaviour of the GLEEC KDF release tag
> **`v3.0.0-beta`** governs: it decides the method names, request and response
> shapes, the walletd-unreachable mapping, the error table and the HTTP
> statuses bound below. The Sia activation wire surface is identical at the
> legacy reference **`v2.6.0-beta`** (same routes, same request object, same
> client-library revision), so no per-reference split is needed.
>
> **Shared rules not restated here.** The mmrpc 2.0 envelope, the error
> envelope (`error`, `error_path`, `error_trace`, `error_type`, `error_data`),
> the HTTP-status policy, the task-status envelope
> (`{"status": "Ok"|"InProgress"|"UserActionRequired"|"Error", "details": ...}`,
> with an `Error` status carrying the full error object directly in `details`),
> the `init` success shape `{"task_id": <integer>}`, the
> `user_action`/`cancel` success shape `"success"`, the `forget_if_finished`
> semantics, the request-parse failure `InvalidRequest`, and every
> framework-level error of `status`, `user_action` and `cancel`
> (`NoSuchTask`, `TaskFinished`, the "task is not awaiting a user action"
> class, with their error-data shapes and HTTP statuses) are bound by
> [Chapter 55](55-coin-activation-wire-contract.md) §55.1--§55.2. The legacy v1
> `enable` envelope is bound by §55.4. The per-family unresponsive-backend
> summary is §55.3; §46.5.3 is the authoritative Sia row of that table.

---

## 46.0 Executive summary

Sia is a **standalone coin** (it is not a platform-with-tokens coin and has no
child tokens). It therefore activates through the project's shared
**standalone-coin task-activation** mechanism -- the same long-running task
family used by UTXO, Qtum and Z-coin -- rather than the **platform-coin
task-activation framework** of [Chapter 48](48-platform-coin-task-activation.md)
that EVM ([Chapter 35](35-evm-v2-activation-rpcs.md)) and Tendermint
([Chapter 36](36-tendermint-v2-activation-rpcs.md)) use for their
platform-with-tokens activation. Sia is a standalone-coin consumer of the
**sibling** substrate ch. 48 §48.9 itself names, not a ch. 48 consumer: it has no
platform/token split for ch. 48's framework to wrap. The surface specified here
is:

| Method string | Tier | Purpose |
| --- | --- | --- |
| `task::enable_sia::init` | mmrpc 2.0 (task) | begin a long-running Sia activation; returns a task id |
| `task::enable_sia::status` | mmrpc 2.0 (task) | poll an in-flight activation; ultimately yields the activation result |
| `task::enable_sia::user_action` | mmrpc 2.0 (task) | shared task user-action method; a Sia task never waits for one (§46.3) |
| `task::enable_sia::cancel` | mmrpc 2.0 (task) | abort an in-flight activation |
| `enable_sia` | mmrpc 2.0 (flat) | alias of `task::enable_sia::init` (returns a task id; §46.6) |
| `enable` (legacy v1) | legacy | one-shot legacy activation of a Sia-protocol coin (§46.6.3) |

All `task::enable_sia::*` methods and `enable_sia` are available on native and
WASM builds. Only the `error_type` discriminant, the `error_data` shape (and,
where stated, its content) and the HTTP status of an error are bound; the
human-readable `error` text is not part of the contract.

---

## 46.1 `task::enable_sia::init` -- begin activation

R46.1.1 The public RPC `task::enable_sia::init` shall begin a long-running Sia
activation and return a task handle. The mmrpc-2.0 `params` object carries:
- `ticker` (string, required) -- the Sia coin ticker to activate; and
- `activation_params` (object, required) -- the Sia activation parameters of
  R46.1.2.

R46.1.2 The `activation_params` object members:

| Field | Type | Required / default | Meaning |
| --- | --- | --- | --- |
| `client_conf` | object | **required** | walletd HTTP backend configuration, owned by the caller (rows below). No walletd URL is embedded in the project. |
| `tx_history` | boolean | optional; default **false** | enable transaction-history tracking for the coin ([Chapter 53](53-sia-transaction-history.md) §53.6) |
| `required_confirmations` | integer (unsigned) | optional; coins-config `required_confirmations` when absent | confirmations required for swap transactions on this coin |
| `gap_limit` | integer (unsigned 32-bit) | optional | single-address-mode HD gap-limit hint; accepted for forward compatibility, not acted upon by activation (ch. 20) |

The `client_conf` object is the configuration object of the public Sia Rust
client library (a permitted sibling source) and differs per build target:

| Target | Member | Type | Required / default | Meaning |
| --- | --- | --- | --- | --- |
| native | `server_url` | string (absolute URL) | required | walletd API base URL |
| native | `password` | string | optional | walletd API password, sent as HTTP Basic authentication with an empty user name |
| native | `timeout` | integer (seconds) | optional; default 30 | per-request HTTP timeout |
| WASM | `server_url` | string (absolute URL) | required | walletd API base URL |
| WASM | `headers` | object, string -> string | optional; default `{}` | extra HTTP headers sent with every request |

Unknown members of `activation_params` and of `client_conf` are ignored. A
`server_url` that is not a valid absolute URL, a missing `client_conf`, or a
wrongly typed member is a request-parse failure (`InvalidRequest`, §55.1).

> **Naming and credentials (pointer, 2026-10-28).** What a log line, an error
> text or the debug form of a value may show of `server_url`, `password` and
> `headers` is bound by [Chapter 20](20-siacoin-integration.md) §20.8 R-N1 and
> R-N2: the server in host-only form, and no password or header value. This
> includes the `error` member of the `CoinCreationError` and the `Transport`
> text of §46.5. Not yet implemented (ch. 20 D10).

R46.1.3 The signing policy is taken from the in-context wallet (the standard
private-key build policy shared by the other standalone coins); the request
carries no per-call key-policy selector. Only the single-key ("Iguana") policy
and the HD policy -- which yields the single derived address of ch. 20
§20.4.1 -- produce an activated Sia coin. Any other policy shall fail
activation with the task-level `CoinCreationError` of §46.5.2 (not a dedicated
key-policy discriminant). In practice the in-context policy is always one of
the two supported ones, so this branch is defensive.

R46.1.4 The activation flow shall: reject activation of an already-active coin
(§46.5.1); resolve the coin's static configuration and Sia protocol descriptor
from coin configuration; derive the key pair under the resolved signing policy;
construct the walletd HTTP client from `client_conf`, **including one
reachability probe of walletd (a consensus-tip request) during construction**
-- a failed probe fails the activation (§46.5.3); build the Sia coin via the
existing Sia coin builder; resolve and cache the per-network DEX-fee address
(ch. 20 §20.4.2); read the current height and the balance; register the
activated coin; and, when `tx_history` is set, start history tracking (the
observable meaning of that step is bound by
[Chapter 53](53-sia-transaction-history.md) §53.6).

R46.1.5 The success `result` of `init` shall carry a single integer `task_id`
used by `status`, `user_action` and `cancel` (§55.2).

---

## 46.2 `task::enable_sia::status` -- poll activation

R46.2.1 The public RPC `task::enable_sia::status` takes `task_id` (integer,
required) and `forget_if_finished` (boolean, optional, default **true**) and
returns the shared task-status union of §55.2.

R46.2.2 The `status` result is one of **in-progress** (R46.2.3), **ok**
(R46.2.4) or **error** (§46.5.2). A Sia task never reports
`UserActionRequired` (§46.3).

R46.2.3 In-progress `details` values are bare JSON strings:

| `details` value | When |
| --- | --- |
| `"ActivatingCoin"` | initial state; key derivation, walletd client construction and probe, coin construction |
| `"RequestingWalletBalance"` | coin built; reading height and balance |
| `"Finishing"` | permitted vocabulary name; the reference never emits it, and a client must not depend on seeing it |

Sia activation does not scan a chain, so it carries no scan-progress object
(cf. the Z-coin family, ch. 39 §39.10).

R46.2.4 The final **ok** `details` shall be:

```json
{
  "ticker": "<string: the activated ticker>",
  "current_block": <integer: walletd consensus height>,
  "wallet_balance": {
    "wallet_type": "Iguana",
    "address": "<string: the activated Sia address>",
    "balance": { "spendable": "<decimal string>", "unspendable": "<decimal string>" }
  }
}
```

When the wallet key context is single-address (Iguana), `wallet_balance` is the
single-address form above.

**Deliberate divergence (binding, maintainer decision 2026-10-11).** When the
wallet key context is HD, Reloaded reports `wallet_balance` as the HD
per-account report (`"wallet_type": "HD"`, the accounts and the single derived
address of ch. 20 §20.4.1 with its balance), so that a Sia coin reports its
balance in the same shape as the other coins of an HD wallet. The reference
always reports the single-address `"Iguana"` form, also under HD; both shapes
carry the same single address and balance. **Compatibility switch:** the
`MM2.json` key `sia_activation_single_address_balance` (boolean, default
`false`). Set it to `true` for the reference behavior: `wallet_balance` is then
always the single-address `"Iguana"` form above, with `address` being the single
derived address under HD. A client can tell the two shapes apart by
`wallet_type`. Balances are decimal
strings in whole siacoin (1 SC = 10^24 hastings, ch. 20 §20.5).

R46.2.5 Sia activation has **no activation deadline**. The `TaskTimedOut`
discriminant is produced only by a timed-out wait for a user action, which a
Sia task never performs (§46.3); it is listed in §46.5.2 for completeness with
its error-data shape.

---

## 46.3 `task::enable_sia::user_action` -- interactive confirmation

R46.3.1 The public RPC `task::enable_sia::user_action` exists with the shared
request shape (`task_id` integer, `user_action` object in the shared
hardware-wallet user-action shape, §55.2). Sia supports no hardware-backed
policy, so a Sia task never enters an awaiting-user-action state: a
`user_action` for a live Sia task fails with the framework's "task is not
awaiting a user action" error, and one for an unknown task with `NoSuchTask`
(both §55.2).

---

## 46.4 `task::enable_sia::cancel` -- abort activation

R46.4.1 The public RPC `task::enable_sia::cancel` takes `task_id` (integer,
required) and aborts an in-flight activation; success is `"success"`. If the
coin had already been registered when the cancellation takes effect, it is
removed again so that no half-activated coin remains. Cancel of an unknown or
finished task is bound by §55.2 (`NoSuchTask` / `TaskFinished`).

---

## 46.5 Error contract

### 46.5.1 `init` (mmrpc-level) errors

R46.5.1 `task::enable_sia::init` (and the `enable_sia` alias) returns these
synchronously as an mmrpc error response; everything else is reported later
through `status`:

| `error_type` | When | `error_data` | HTTP | Placement |
| --- | --- | --- | --- | --- |
| `CoinIsAlreadyActivated` | the ticker is already active when `init` is called | object `{"ticker": string}` | 400 | mmrpc |
| `CoinConfigIsNotFound` | no coins-config entry for the ticker | string: the requested ticker | 400 | mmrpc |
| `CoinProtocolParseError` | the coins-config `protocol` does not parse | object `{"ticker": string, "error": string}` | 400 | mmrpc |
| `UnexpectedCoinProtocol` | the coins-config protocol is valid but not the Sia protocol | object `{"ticker": string, "protocol": object}` where `protocol` is the coin's protocol object as written in the coins config | 400 | mmrpc |
| `Internal` | the activation context or task registry cannot be used, or the task cannot be spawned | string | 500 | mmrpc |
| `InvalidRequest` | `params` fail to deserialize (R46.1.1--R46.1.2) | per §55.1 | per §55.1 | mmrpc (dispatcher) |

### 46.5.2 Task-level errors

R46.5.2 A failed activation is reported in the `details` of a `status`
response whose `status` is `"Error"`; that `status` call itself is an HTTP
**200** response (§55.1). The HTTP column is the status the discriminant
carries if it is ever returned at mmrpc level; it never changes the 200 of the
`status` call.

| `error_type` | When (failure class) | `error_data` | HTTP if at mmrpc level |
| --- | --- | --- | --- |
| `CoinCreationError` | the coin could not be built: coins-config entry does not parse as a Sia config; key derivation fails; unsupported signing policy (R46.1.3); the walletd HTTP client cannot be created; **the walletd reachability probe fails** (walletd unreachable, refused, timed out, wrong password, non-success HTTP status, undecodable answer); any other build-time failure | object `{"ticker": string (the requested ticker), "error": string}` | 400 |
| `Transport` | the coin was built (walletd answered the probe) but the current height or the balance could not be read afterwards | string | 500 |
| `CoinIsAlreadyActivated` | while this task ran, a concurrent activation of the same ticker registered the coin first | object `{"ticker": string}` | 400 |
| `TaskTimedOut` | only from a timed-out user-action wait; not reachable for Sia (R46.2.5) | object `{"duration": {"secs": u64, "nanos": u32}}` | 408 |
| `Internal` | the wallet key context is not initialised; the activated address cannot be produced; history-tracking start-up or registration fails internally; task-handle failures; any otherwise unclassified failure | string | 500 |

R46.5.3 Discriminants that shall **not** be observable from any Sia activation
method: `PrivKeyNotAllowed`, `UnexpectedDerivationMethod`, `HwError`, and the
activation-internal names that the public error catalogue lists for the Sia
initialisation enum (`HardwareWalletsAreNotSupportedYet`, `CouldNotGetBalance`,
`CouldNotGetBlockCount`) or for the Sia build/config enums -- those are always
translated into the discriminants of §46.5.1--§46.5.2 before they reach the
wire.

### 46.5.3 Unresponsive walletd (Sia row of §55.3)

R46.5.4 The failure-to-discriminant mapping for an unreachable or failing
walletd backend is:

| Situation | Observable result |
| --- | --- |
| walletd unreachable / refusing / timing out / rejecting the credentials when the task starts | the reachability probe of R46.1.4 fails: terminal `status` `Error`, `CoinCreationError` `{ticker, error}`; no coin is registered |
| walletd answers the probe but fails on the following height or balance read | terminal `status` `Error`, `Transport` (string) |

The native client retries a request whose transport fails a small fixed
number of times before reporting the failure; each attempt is bounded by the
`client_conf.timeout` (default 30 s). No walletd failure is reported as an HTTP
5xx on the `status` call itself; the only 5xx responses come from `init`
(§46.5.1) or from the framework errors of §55.2.

> **Upstream divergence (informative) -- public catalogue.** The public error
> catalogue lists a Sia initialisation enum (with `CouldNotGetBalance`,
> `CouldNotGetBlockCount`, `HardwareWalletsAreNotSupportedYet`), a Sia build
> enum and a Sia configuration enum with placeholder variants. None of these
> names is observable on the activation wire (R46.5.3); the observable set is
> the shared standalone set of §46.5.1--§46.5.2.

> **Reloaded status (informative).** On `fix/activation-api-parity`: an
> unsupported signing policy is `CoinCreationError`; height and balance read
> failures after the probe are `Transport`; the HD report under an HD wallet is
> the default with the `sia_activation_single_address_balance` switch for the
> reference form (R46.2.4); legacy v1 `enable` activates Sia-protocol coins
> (§46.6.3); the shared standalone rules of §55.5 apply. Remaining: the HD and
> single-address result paths are tested through the shape selector only, as
> no walletd test double exists.

---

## 46.6 Method naming, flat alias and legacy `enable` (binding)

R46.6.1 The canonical activation surface is the four
`task::enable_sia::{init,status,user_action,cancel}` method strings of §46.0,
routed on native and WASM builds.

R46.6.2 A flat alias `enable_sia` shall route to the same `init` handler
(same request, returns a `task_id` -- *not* a synchronous one-shot result; same
errors and HTTP statuses as `task::enable_sia::init`). The reference lineage
routes this alias identically. There is no flat alias for `status`,
`user_action` or `cancel`, and no platform-with-tokens method for Sia.

R46.6.3 **Legacy v1 `enable`.** The legacy (non-mmrpc) `enable` method shall
activate a coin whose coins-config protocol is the Sia protocol, synchronously.
Its request carries the Sia activation members **at the top level of the
legacy request**, next to `coin`: `client_conf` (object of R46.1.2, required),
`tx_history` (boolean, default false), `required_confirmations` (integer,
optional) and `gap_limit` (integer, optional). Construction follows R46.1.3 --
R46.1.4 (including the walletd probe). The success response and the legacy
error envelope are those of the legacy `enable` method (§55.4); a missing or
malformed `client_conf`, an unreachable walletd, or an unsupported key policy
is reported through that legacy error envelope.

---

## 46.7 Implementation-substrate note (informative)

This note orients the implementer; it is not normative.

**Present in reloaded:**
- the Sia coin type -- the public `SiaCoin` (the `SiaCoinGeneric<SiaClient>`
  alias generic over its walletd HTTP client), already an `MmCoinEnum` variant
  with the coin, market, swap and withdraw operations bound by ch. 20;
- the Sia activation scaffolding alongside the coin: the Sia coin **builder**,
  the Sia **activation request** type (carrying `tx_history`,
  `required_confirmations`, `gap_limit` and `client_conf`), the Sia **config**
  type, and the Sia **protocol descriptor**;
- the **shared standalone-coin task-activation framework** with its shared
  request/response envelopes (the `{ticker, activation_params}` init request,
  the `{task_id, forget_if_finished}` status request, the
  `{task_id, user_action}` user-action request, and the `{task_id}` init
  response);
- the Sia standalone-activation trait implementation and the routes of
  §46.6.1--§46.6.2.

**What must change (scope of this revision):** the corrections listed in the
§46.5 `Reloaded status` note, and the legacy `enable` leg of R46.6.3. Coin
construction shall keep reusing the existing Sia coin builder.

---

## 46.8 Tests

T46.8.1 Request parsing: `activation_params` with only `client_conf` parses
with `tx_history` false; a missing `client_conf` and a non-URL `server_url`
fail with `InvalidRequest`; unknown members are ignored; on native
`password` and `timeout` parse, on WASM `headers` parses.

T46.8.2 Task-error mapping, each asserting `error_type`, the exact
`error_data` JSON type/fields, and that the carrying `status` response is
HTTP 200: (a) walletd unreachable at start -> `CoinCreationError` with
`{ticker, error}` and `ticker` equal to the request ticker; (b) height or
balance read failure after a successful probe -> `Transport` with a string;
(c) unsupported key policy -> `CoinCreationError`, never `PrivKeyNotAllowed`.

T46.8.3 Result shape: under the single-key context the `Ok` `details` carry
`wallet_balance.wallet_type == "Iguana"`. Under an HD wallet key context they
carry the HD report (`wallet_type == "HD"`) by default, and the `"Iguana"` form
with the single derived address when `sia_activation_single_address_balance` is
`true` (R46.2.4).

T46.8.4 `init` errors: `CoinIsAlreadyActivated` (object, 400),
`CoinConfigIsNotFound` (string = ticker, 400), `CoinProtocolParseError`
(object, 400), `UnexpectedCoinProtocol` (object with `protocol` object, 400),
asserting serialized `error_type`, `error_data` shape and HTTP status; the
same for the `enable_sia` alias.

T46.8.5 Legacy `enable` of a Sia-protocol coin with a reachable walletd stub
succeeds with the legacy enable response; with an unreachable stub it fails
through the legacy error envelope and leaves no coin registered.

T46.8.6 `user_action` on a live Sia task fails with the §55.2 not-awaiting
error.

---

## 46.9 Acceptance criteria

- `task::enable_sia::init` and `enable_sia` accept `{ticker,
  activation_params}` per R46.1.2 and return a `task_id` (§46.1, §46.6).
- `task::enable_sia::status` reports `"ActivatingCoin"` /
  `"RequestingWalletBalance"` and ultimately an `Ok` with `ticker`,
  `current_block` and a `wallet_balance` in the form R46.2.4 selects (§46.2).
- Errors map exactly as §46.5: init-time config/protocol/already-active errors
  at mmrpc level with HTTP 400; walletd unreachable at start ->
  `CoinCreationError {ticker, error}`; height/balance failure -> `Transport`
  string; unsupported key policy -> `CoinCreationError`; none of the
  discriminants of R46.5.3 observable (T46.8.1--T46.8.6).
- Legacy `enable` activates a Sia-protocol coin (R46.6.3).
- The activation reuses the existing Sia coin builder and the shared
  standalone-coin task framework (§46.7).

## 46.10 Provenance Footer

- *Inputs:* the published Komodo DeFi Framework API documentation (consulted
  to establish that it does **not** document Sia activation, and for its
  error-catalogue enum names); the GLEEC KDF release tags `v3.0.0-beta`
  (reference) and `v2.6.0-beta` (legacy cross-check), consulted **by the KDF
  Spec Reader role only**, for the externally observable activation wire
  contract (routes, request members, in-progress values, result shape, error
  discriminants, error-data shapes, HTTP statuses, walletd-failure mapping,
  legacy `enable` support) -- no implementation expression carried across,
  subject to KDF Dirty Gate clearance; the public Sia Rust client library at
  the revision both references pin, a permitted sibling source (the
  `client_conf` object members and defaults, the construction-time walletd
  probe, the transport retry); reloaded's existing standalone-coin
  task-activation framework and Sia activation code (for the `Reloaded status`
  notes); [Chapter 20](20-siacoin-integration.md) (the Sia coin type, builder,
  key derivation and walletd backend); [Chapter 48](48-platform-coin-task-activation.md)
  §48.9 (named only as the contrasting sibling substrate);
  [Chapter 53](53-sia-transaction-history.md) §53.6 (the `tx_history`
  activation parameter); [Chapter 55](55-coin-activation-wire-contract.md)
  §55.1--§55.4 (shared envelope, task framework errors, unresponsive-backend
  table, legacy `enable`).
- *Permitted-input classes used:* external public specification (the
  published KDF API documentation); dirty-side distillation of the reference
  lineage's observable wire behaviour under the two-team workflow; permitted
  sibling source (the public Sia Rust client library); baseline/as-built
  source (the existing standalone-coin task framework); cross-chapter
  contracts (Chapters 20, 48, 53, 55); Interop / wire-and-API-bound reuse
  (R29/R31) for the dictated method strings, JSON field names and error
  discriminants.
- *Sibling-allowlist consultations:* [Chapter 20](20-siacoin-integration.md),
  [Chapter 48](48-platform-coin-task-activation.md) §48.9,
  [Chapter 53](53-sia-transaction-history.md) §53.6; the public Sia Rust
  client library (client configuration and construction behaviour).
- *Forbidden corpus:* consulted by the KDF Spec Reader role for this revision
  (reference tags above); output limited to the clean-channel wire contract.
- *Clean-context addition (2026-10-28, forbidden corpus not consulted):* the
  pointer note after R46.1.2 is Reloaded-original. It rests on the maintainer
  decision of 2026-10-26 (host-only node naming everywhere) and binds nothing
  beyond chapter 20 R-N1 and R-N2.
