# Chapter 46 -- Sia Coin V2 (Task-Based) Activation RPC Surface

**Status:** driving-spec (required port). This chapter specifies a **task-based
activation RPC layer** over the Sia coin code that already exists in reloaded; it
does not introduce a new coin (the Sia coin itself is bound by ch. 20).

> **One-sentence claim:** the project shall expose the standard Komodo DeFi
> Framework **long-running, task-based standalone-coin** activation surface for
> the Sia coin -- the `task::enable_sia::{init,status,user_action,cancel}` family
> -- on top of the Sia coin type, builder, and shared standalone-coin task
> framework that reloaded already ships.

> **Treatment:** **T-PORT.** The Sia coin type and its builder, its activation
> request/config/protocol types, and the **shared standalone-coin
> task-activation framework** (the generic `init_standalone_coin` entry points
> and the `InitStandaloneCoinActivationOps` trait already driving the UTXO,
> Qtum, and Z-coin task families) are all present in reloaded (see §46.7). What
> is required is the **activation integration**: the standalone-coin activation
> trait implementation for the Sia coin (reusing the existing builder) plus the
> dispatcher routes for the method strings named below. The wire contract
> distilled here is the source of truth for that port.

> **Binding scope (R37).** Requirements bind observable behaviour, the public
> mmrpc-2.0 task method strings and their request/response JSON field names, and
> externally *dictated* interop (the Sia consensus/transaction wire formats and
> the walletd HTTP backend bound by ch. 20). Those public contracts are the
> source of truth, not this project's code. Private Rust types, helper
> decomposition, and internal module structure are informative and are **not**
> bound by this chapter.

> **Source of truth (informative).** The method strings, JSON field names, and
> error discriminants below are the published Komodo DeFi Framework API contract
> (the public KDF API documentation; Sia is activated through the
> `task::enable_sia` namespace). Where the public API docs leave a behavioural
> gap, the standalone-coin task family pattern that reloaded already implements
> for `task::enable_z_coin` and `task::enable_utxo` governs, since Sia shares
> that pattern. Where this chapter and the public API docs disagree, the public
> API docs govern.

---

## 46.0 Executive summary

Sia is a **standalone coin** (it is not a platform-with-tokens coin and has no
child tokens). It therefore activates through the project's shared
**standalone-coin task-activation** mechanism -- the same long-running task
family used by UTXO, Qtum, and Z-coin -- rather than the platform-coin activator
used by EVM (ch. 35) and Tendermint (ch. 36). The surface specified here is:

| Method string | Tier | Purpose |
| --- | --- | --- |
| `task::enable_sia::init` | mmrpc 2.0 (task) | begin a long-running Sia activation; returns a task id |
| `task::enable_sia::status` | mmrpc 2.0 (task) | poll an in-flight activation; ultimately yields the activation result |
| `task::enable_sia::user_action` | mmrpc 2.0 (task) | supply an interactive user action to an in-flight activation |
| `task::enable_sia::cancel` | mmrpc 2.0 (task) | abort an in-flight activation |

All four use the mmrpc-2.0 envelope (`{"mmrpc":"2.0","method":...,"params":
{...},"id":...}`) and, on success, return `{"mmrpc":"2.0","result":{...},"id":
...}`. On error they return the standard mmrpc-2.0 error envelope carrying
`error`, `error_path`, `error_trace`, `error_type`, and `error_data`; only the
**`error_type` discriminant** and its HTTP status are bound below (the human-
readable `error` text is not part of the contract).

The four methods follow the same init/status/user_action/cancel lifecycle as the
other `task::enable_*` standalone families: `init` spawns the activation task and
returns a `task_id`; `status` polls that task; `user_action` feeds an interactive
confirmation to a task awaiting one; `cancel` aborts it.

---

## 46.1 `task::enable_sia::init` -- begin activation

R46.1.1 The public RPC `task::enable_sia::init` shall begin a long-running Sia
activation and return a task handle. The mmrpc-2.0 `params` object carries:
- `ticker` (string, required) -- the Sia coin ticker to activate; and
- `activation_params` (object, required) -- the Sia activation parameters of
  R46.1.2.

R46.1.2 The `activation_params` object shall carry:
- `client_conf` (object, required) -- the walletd HTTP backend configuration
  (the walletd base URL and its authentication), owned by the caller. No walletd
  URL is embedded in the project; the backend is entirely caller-supplied. The
  shape of this object is dictated by the Sia walletd HTTP API and the external
  Sia client library (ch. 20 §20.4--§20.5).
- `tx_history` (boolean, optional, default **false**) -- whether to enable
  transaction-history tracking for the coin.
- `required_confirmations` (integer, optional) -- confirmations required for swap
  transactions on this coin; when absent the coin-configuration default applies.
- `gap_limit` (integer, optional) -- single-address-mode HD gap-limit hint;
  currently accepted for forward compatibility (ch. 20 records it as accepted but
  not yet acted upon).

R46.1.3 The signing policy is taken from the in-context wallet (the standard
private-key build policy shared by the other standalone coins); the request does
not carry a separate per-call private-key-policy selector. Per ch. 20, only the
single-key and single-address HD-account policies reach a successfully activated
Sia coin; any other policy shall fail activation with a typed error
(R46.5) rather than panicking.

R46.1.4 The activation flow shall: reject activation of an already-active coin
(R46.5); resolve the coin's static configuration and Sia protocol descriptor from
coin configuration; construct the walletd HTTP client from `client_conf`; build
the Sia coin via the existing Sia coin builder under the resolved signing policy;
resolve and cache the per-network DEX-fee address (ch. 20 §20.4.2); register the
activated coin; and, when `tx_history` is set, start history tracking.

R46.1.5 The success `result` of `init` shall carry a single `task_id`
(the long-running-task identifier) used by `status`, `user_action`, and `cancel`.

---

## 46.2 `task::enable_sia::status` -- poll activation

R46.2.1 The public RPC `task::enable_sia::status` shall report the state of an
in-flight or finished activation. Its `params` carry:
- `task_id` (integer, required) -- the identifier returned by `init`; and
- `forget_if_finished` (boolean, optional, default **true**) -- when true a
  finished task is dropped from the task registry after this poll returns it.

R46.2.2 The `status` result is the standard task-status union: it reports one of
- **in-progress** -- carrying an in-progress status value from the enumeration of
  R46.2.3;
- **awaiting user action** -- when the task needs an interactive confirmation
  (R46.3);
- **ok** -- carrying the final activation result of R46.2.4; or
- **error** -- carrying an `error_type` from R46.5.

R46.2.3 The in-progress status enumeration shall report at least the observable
states: **activating the coin**, **requesting the wallet balance**, and
**finishing**. (Sia activation does not scan a shielded chain, so it carries no
scan state; cf. the Z-coin family, ch. 39.)

R46.2.4 The final **ok** activation result shall report:
- `ticker` (string) -- the activated Sia coin ticker;
- `current_block` (integer) -- the current Sia chain height at activation; and
- `wallet_balance` -- the wallet balance report. For the single-address
  ("Iguana") wallet this carries the activated `address` and a `balance` object
  with `spendable` and `unspendable` decimal fields; for an HD wallet it carries
  the per-account/per-address balance report. The balance objects use the shared
  `{spendable, unspendable}` coin-balance shape used by every coin.

R46.2.5 A task that exceeds its activation deadline shall be reported as an
**error** result carrying the timeout-class `error_type` of R46.5.

---

## 46.3 `task::enable_sia::user_action` -- interactive confirmation

R46.3.1 The public RPC `task::enable_sia::user_action` shall deliver an
interactive user action to an activation task whose `status` is awaiting one
(e.g. a hardware-wallet confirmation, when a hardware-backed policy is in use).
Its `params` carry `task_id` (integer, required) and `user_action` (object,
required) in the shared hardware-wallet user-action shape used by the other
standalone task families.

R46.3.2 On success the `result` is the shared task success acknowledgement.
Supplying a `user_action` for an unknown task shall fail with the no-such-task
error of R46.5.

---

## 46.4 `task::enable_sia::cancel` -- abort activation

R46.4.1 The public RPC `task::enable_sia::cancel` shall abort an in-flight
activation. Its `params` carry `task_id` (integer, required). On success the
`result` is the shared task success acknowledgement. Cancelling an unknown task
shall fail with the no-such-task error of R46.5.

---

## 46.5 Error contract

R46.5.1 The bound error surface for all four methods is the project's shared
**standalone-coin** activation error contract -- the same `error_type`
discriminants and HTTP statuses the framework already exposes for
`task::enable_utxo`, `task::enable_qtum`, and `task::enable_z_coin`. The
Sia-specific activation errors (coin-creation failure, balance/height retrieval
failure, unsupported signing policy, already-activated, timeout) map into this
shared contract; the discriminants the caller observes shall include at least:

| `error_type` | HTTP status | Condition |
| --- | --- | --- |
| `CoinIsAlreadyActivated` | 400 | the named coin is already active |
| `CoinConfigIsNotFound` | 400 | no coin-configuration entry for the ticker |
| `CoinProtocolParseError` | 400 | the coin's protocol descriptor failed to parse |
| `UnexpectedCoinProtocol` | 400 | the configured protocol is not the Sia protocol |
| `CoinCreationError` | 400 | the Sia coin could not be built (e.g. walletd client / config error) |
| `PrivKeyNotAllowed` | 400 | the active signing policy is not permitted for Sia |
| `UnexpectedDerivationMethod` | 400 | the wallet derivation method is unsupported for this call |
| `NoSuchTask` | 400 | `status`/`user_action`/`cancel` referenced an unknown `task_id` |
| `TaskTimedOut` | 408 | the activation exceeded its deadline |
| `Transport` | 500 | a transport failure occurred reaching the walletd backend |
| `Internal` | 500 | an otherwise-unclassified internal error |

R46.5.2 A request for an already-active coin shall be rejected with
`CoinIsAlreadyActivated`; an unknown or mistyped Sia entry in coin configuration
shall be rejected with the corresponding config-not-found / protocol-parse /
unexpected-protocol discriminant rather than partially activating the coin; an
unsupported signing policy shall be rejected with `PrivKeyNotAllowed`.

---

## 46.6 Method-naming & legacy alias (binding)

R46.6.1 The canonical activation surface is the four `task::enable_sia::{init,
status,user_action,cancel}` method strings of §46.0. These are the namespaced
wire names a caller uses.

R46.6.2 In addition, a **legacy flat alias** `enable_sia` shall route to the same
`init` handler (returning a `task_id`, *not* a synchronous one-shot result). This
mirrors the legacy flat aliases the framework keeps for the other standalone
families (e.g. the `init_utxo` / `init_qtum` flat aliases alongside their
`task::enable_*` names). Sia does **not** expose a distinct `init_sia`-style flat
trio for status/user_action/cancel; those lifecycle calls are reached only
through the `task::enable_sia::*` names. There is no platform-with-tokens
`enable_sia_with_assets`-style method, because Sia is a standalone coin.

> **Source-of-truth note (informative).** The published KDF API documents Sia
> activation under the `task::enable_sia` namespace. The legacy flat `enable_sia`
> alias is bound here for backward compatibility with the existing dispatcher
> aliasing convention; if the public API docs and this alias ever diverge, the
> public API docs govern (§46.0).

---

## 46.7 Implementation-substrate note (informative)

This note orients the implementer; it is not normative.

**Already present in reloaded:**
- the Sia coin type -- the public `SiaCoin` (the `SiaCoinGeneric<SiaClient>`
  alias generic over its walletd HTTP client), already an `MmCoinEnum` variant
  with the coin, market, swap, and withdraw operations bound by ch. 20;
- the Sia activation scaffolding already alongside the coin: the Sia coin
  **builder** (`SiaCoinBuilder`), the Sia **activation request** type
  (`SiaCoinActivationRequest`, carrying `tx_history`, `required_confirmations`,
  `gap_limit`, and `client_conf`), the Sia **config** type (`SiaCoinConf`), and
  the Sia **protocol descriptor** (`SiaCoinProtocolInfo`);
- the **shared standalone-coin task-activation framework** -- the generic
  `init_standalone_coin` / `..._status` / `..._user_action` / `cancel_...`
  entry points and the `InitStandaloneCoinActivationOps` trait -- already
  implemented and already driving the UTXO, Qtum, and Z-coin task families, with
  its shared request/response envelopes (the `{ticker, activation_params}` init
  request, the `{task_id, forget_if_finished}` status request, the
  `{task_id, user_action}` user-action request, and the `{task_id}` init
  response) and its shared standalone error contract (§46.5).

**What must be added (scope of this chapter's port):**
- implement the **standalone-coin activation trait**
  (`InitStandaloneCoinActivationOps`) for the Sia coin, **reusing the existing
  Sia coin builder** for coin construction -- do **not** reinvent coin
  construction. The trait implementation binds the Sia activation request type as
  its activation request, the Sia protocol descriptor as its protocol, the
  in-progress status set of R46.2.3, and an activation-result type carrying the
  fields of R46.2.4; its activation error maps into the shared standalone error
  contract of §46.5;
- add the **dispatcher routes** for the four `task::enable_sia::*` method strings
  (and the legacy flat `enable_sia` alias, §46.6) so the mmrpc-2.0 task layer
  reaches the generic standalone entry points parameterised over the Sia coin,
  exactly as the existing UTXO/Qtum/Z-coin routes do.

The placement of the Sia activation trait implementation (its crate/module)
should follow the existing standalone-coin activation layout used by the other
standalone coins (the coins-activation crate); its internal decomposition is the
implementer's choice.

---

## 46.8 Acceptance criteria

- `task::enable_sia::init` accepts `{ticker, activation_params}` (with
  `client_conf` required and `tx_history` / `required_confirmations` / `gap_limit`
  optional) and returns a `task_id` (§46.1).
- `task::enable_sia::status` advances through the in-progress states (activating,
  requesting balance, finishing) and ultimately returns an ok result carrying
  `ticker`, `current_block`, and a `wallet_balance` with `{spendable,
  unspendable}` balance(s) (§46.2).
- `task::enable_sia::user_action` and `task::enable_sia::cancel` feed and abort an
  in-flight task respectively, and reject an unknown `task_id` with `NoSuchTask`
  (§46.3--§46.4).
- An already-active coin is rejected with `CoinIsAlreadyActivated`, an unsupported
  signing policy with `PrivKeyNotAllowed`, and a timed-out activation with
  `TaskTimedOut` (408), using the shared standalone error contract of §46.5
  without partially activating the coin.
- The legacy flat `enable_sia` alias reaches the same `init` handler and returns a
  `task_id` (§46.6).
- The activation reuses the existing Sia coin builder and the shared
  standalone-coin task framework; no coin construction is reinvented (§46.7).
