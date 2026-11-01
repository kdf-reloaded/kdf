# Chapter 48 -- Platform-Coin Task-Activation Framework

**Status:** driving-spec (required port). This chapter specifies a **shared
task-activation substrate** for platform-coin-with-tokens activation, and the
sibling **token task-activation substrate** that a token family such as
`task::enable_erc20` runs on. It does not introduce a new coin or a new
activation result shape for platform coins; it provides the long-running task
machinery that the EVM ([Chapter 35](35-evm-v2-activation-rpcs.md) §35.3) and
Tendermint ([Chapter 36](36-tendermint-v2-activation-rpcs.md) §36.6)
task-activation families were recorded as blocked on, and the generic token
task that the EVM token family of Chapter 35 specialises.

> **One-sentence claim:** the project shall provide a generic
> platform-coin task-activation framework -- the `init`/`status`/`user_action`/
> `cancel` lifecycle, the standard task-status envelope, and the per-coin task
> registry -- so that platform-coin-with-tokens activation (EVM, Tendermint, and
> any future platform) can be exposed as a long-running `task::enable_*` family
> in addition to its one-shot call, **reusing the already-shipped one-shot
> activation routine unchanged** as the unit of work, together with a generic
> token task-activation framework for activating a single token on an already
> active platform coin as a long-running task.

> **Treatment:** **T-PORT.** Reloaded already ships (a) the one-shot
> platform-coin-with-tokens activation routine that EVM and Tendermint activation
> delegate to, and (b) two sibling task-activation substrates -- the
> standalone-coin family (backing `task::enable_utxo`, `task::enable_qtum`, the
> Z-coin task trio) and the l2 family (backing `task::enable_lightning`). What is
> required is the **third sibling**: a platform-with-tokens task substrate of the
> same shape, plus the per-coin task registration that routes the EVM and
> Tendermint task method strings to it; and the **fourth sibling**, a token task
> substrate (§48.11), which Reloaded does not ship.

> **Binding scope.** Requirements bind observable behaviour, the public mmrpc-2.0
> method strings and their request/response JSON field names, the task-status
> envelope shape (shared by every `task::` family), the bound `error_type`
> discriminants, their `error_data` shapes, HTTP statuses and placement, and the
> in-progress vocabularies. Those public contracts are the source of truth, not
> this project's code. Private Rust types, trait decomposition, and internal
> module structure are informative and are **not** bound by this chapter.

> **Source of truth (informative).** The method strings, the per-coin `init`
> parameters and success-result shapes, and the activation error discriminants
> are the published Komodo DeFi Framework API contract (the public KDF API
> documentation: the coin-activation `enable_eth_with_tokens` /
> `enable_tendermint_with_assets` sections, the task-managed activation pages and
> the error-types catalogue). Where the public documentation is silent,
> ambiguous or self-contradictory -- including every HTTP status and the
> question of which errors `init` reports synchronously -- the GLEEC KDF
> reference release `v3.0.0-beta` decides, per the maintainer decision recorded
> in [Chapter 55](55-coin-activation-wire-contract.md). The task-framework
> methods shared by every family (`status`, `user_action`, `cancel`, task ids,
> `forget_if_finished`, the cancel error contract) are bound once in
> [Chapter 55](55-coin-activation-wire-contract.md) §55.2; this chapter binds
> them by reference.

---

## 48.0 Executive summary

Reloaded models EVM and Tendermint coins as **platform coins with tokens**: a
native platform coin activated together with an inline batch of child tokens in a
single unit of work. That unit of work already exists as the one-shot RPCs
`enable_eth_with_tokens` (ch. 35 §35.1) and `enable_tendermint_with_assets`
(ch. 36 §36.1).

The published KDF surface additionally exposes a **long-running task variant** of
each platform activation:

| Method string | Tier | Targets | Purpose |
| --- | --- | --- | --- |
| `task::enable_eth::{init,status,user_action,cancel}` | mmrpc 2.0 (task) | all | long-running EVM platform activation (ch. 35 §35.3) |
| `task::enable_tendermint::{init,status,user_action,cancel}` | mmrpc 2.0 (task) | all | long-running Tendermint platform activation (ch. 36 §36.6) |

and a long-running **token** activation on an already active platform:

| Method string | Tier | Targets | Purpose |
| --- | --- | --- | --- |
| `task::enable_erc20::{init,status,user_action,cancel}` | mmrpc 2.0 (task) | all | long-running ERC20 token activation (ch. 35), on the token substrate of §48.11 |

Both platform families were blocked, in chapters 35 and 36, on **the same
missing piece**: a platform-coin task-activation substrate. This chapter
specifies that substrate. Once it exists, the per-coin families above become a
thin registration over it (one task per platform coin), and chapters 35 §35.3 /
36 §36.6 are unblocked.

The substrate is **behaviour-preserving with respect to the one-shot call**: a
task's unit of work is the existing one-shot activation routine, run inside the
task so that progress can be polled and the run can be cancelled. The `init`
parameters and the final success result are **identical** to the one-shot call's
(R48.2); no task-only request field is added beyond the `client_id` every task
`init` accepts ([Chapter 55](55-coin-activation-wire-contract.md) R55.2.2).

`task::enable_bch` is **not** a platform-with-tokens task: it is a
standalone-coin family member that activates BCH alone, bound by
[Chapter 55](55-coin-activation-wire-contract.md) §55.8 (R48.4.4).

> **Implementation status (informative).** The platform substrate specified in
> §§48.1--48.7 is implemented: a generic `init`/`status`/`user_action`/`cancel`
> task wrapper drives the existing one-shot platform-coin-with-tokens
> activation routine as its unit of work, and both required registrations of
> R48.4.1 are wired -- `task::enable_eth::*` over the EVM platform coin and
> `task::enable_tendermint::*` over the Tendermint platform coin -- each routed
> on native and WASM targets alike, satisfying R48.4.2's same-target-coverage
> requirement. This closes the dependency both
> [Chapter 35](35-evm-v2-activation-rpcs.md) §35.3 and
> [Chapter 36](36-tendermint-v2-activation-rpcs.md) §36.6 record on this
> substrate (§48.9). The token substrate of §48.11 is not implemented, and the
> wire details tightened by this revision (R48.3.3, R48.5.2, R48.5.4) are not
> yet met; see the *Reloaded status* notes.

---

## 48.1 Task lifecycle and status envelope

R48.1.1 The framework shall expose, for each registered platform coin, four
mmrpc-2.0 method strings following the `init`/`status`/`user_action`/`cancel`
pattern shared by every documented `task::enable_*` family
(`task::enable_utxo`, `task::enable_qtum`, the Z-coin task trio,
`task::enable_lightning`). The method strings for the two coins required here are
`task::enable_eth::{init,status,user_action,cancel}` and
`task::enable_tendermint::{init,status,user_action,cancel}`.

R48.1.2 `init` shall accept the platform activation parameters (R48.2.1), spawn a
task that performs the activation as a background unit of work, and return the
standard task-init response carrying a numeric `task_id`. `init` shall return
**before** activation completes, after performing only the synchronous checks
of R48.5.4.

R48.1.3 `status` shall accept a `task_id` (and the standard
`forget_if_finished` selector) and return the **standard task-status envelope**
of [Chapter 55](55-coin-activation-wire-contract.md) R55.2.7 used by every
`task::` family. That envelope distinguishes:

- an **in-progress** state carrying a coin-activation progress value (R48.3);
- a terminal **success** state carrying the activation result (R48.2.2);
- a terminal **error** state carrying the full error object of a bound
  `error_type` (R48.5);
- an **awaiting-user-action** state, when the active signing policy requires an
  interactive confirmation (R48.3.2).

The `forget_if_finished` semantics are those of Chapter 55 R55.2.8.

R48.1.4 `user_action` shall accept a `task_id` together with a `user_action`
payload and deliver it to a task that is in the awaiting-user-action state, with
the request, success and error contract of Chapter 55 R55.2.10–R55.2.12. For
signing policies that complete without interaction (R48.6), no `user_action`
call is required for a successful activation; the method is routed for wire
parity with the published surface and for the interactive policies of R48.3.2.

R48.1.5 `cancel` shall accept a `task_id` and abort an in-flight activation that
has not yet reached a terminal state, with the request, success and error
contract of Chapter 55 R55.2.13–R55.2.16. Cancelling shall not leave the
platform coin partially registered (R48.7.3).

R48.1.6 Each registered platform coin shall own an **independent task registry**,
so that an in-flight EVM activation and an in-flight Tendermint activation are
addressed by independent registries and neither can observe or cancel the
other's tasks; a task id issued by one family is `NoSuchTask` to the other
(Chapter 55 R55.2.4). Task ids themselves are drawn from the single
process-wide counter of Chapter 55 R55.2.3.

---

## 48.2 Request and result delegation (no new wire surface)

R48.2.1 `task::enable_eth::init` shall accept **exactly** the `enable_eth_with_tokens`
parameters of ch. 35 §35.1, and `task::enable_tendermint::init` shall accept
**exactly** the `enable_tendermint_with_assets` parameters of ch. 36 §36.1, in
both cases flat in `params` (no `activation_params` wrapper), plus the optional
top-level `client_id` of Chapter 55 R55.2.2. The task framework shall **not**
add, remove, or rename any other request field relative to the one-shot call.

R48.2.2 On success, the task-status terminal result for `task::enable_eth` shall
be the `enable_eth_with_tokens` success result of ch. 35 §35.1.3, and for
`task::enable_tendermint` shall be the `enable_tendermint_with_assets` success
result of ch. 36 §36.1.5 -- byte-for-byte the same result shape (including the
`get_balances`-dependent variants), so a caller can poll a task to completion and
receive what the one-shot call would have returned.

R48.2.3 The unit of work performed inside the task shall be the **same activation
routine** the one-shot call invokes (platform coin creation, token activation,
optional balance enumeration, and any background history fetch). The task wrapper
adds only progress reporting, cancellation, and the awaiting-user-action hook;
it shall not re-implement or fork the activation logic.

R48.2.4 The one-shot calls reject a hardware-wallet (`Trezor`) key policy with
the payload-less discriminant `UnexpectedDeviceActivationPolicy` (HTTP 400);
the task variants accept it. A hardware policy is therefore usable only through
`task::enable_eth` / `task::enable_tendermint`.

---

## 48.3 In-progress status surface

R48.3.1 While a task runs, `status` shall report observable in-progress states
covering at least: **activating the platform coin**, **requesting balances /
initialising tokens**, and **finishing**. The framework imposes no separate
activation wall-clock deadline of its own beyond the timeouts the wrapped
one-shot routine and the shared task plumbing already enforce; the only
timeout-class surface is the standard task-framework one (R48.5.2), matching the
standalone-coin and l2 sibling families.

R48.3.2 When the active signing policy requires interactive confirmation -- in
particular a hardware-backed (Trezor) policy -- `status` shall additionally
surface the hardware-interaction states of Chapter 55 R55.2.19:
`WaitingForTrezorToConnect`, `FollowHwDeviceInstructions`, and, as
awaiting-user-action states, `EnterTrezorPin` and `EnterTrezorPassphrase`; the
corresponding PIN or passphrase shall be supplied via `user_action` (R48.1.4)
with the hardware-wallet user-action vocabulary of Chapter 55 R55.2.10. These
states and payloads shall reuse the same hardware task contract already used by
the standalone-coin task family (Chapter 55 §55.5), not a new one.

R48.3.3 The in-progress `details` value of a platform task shall be drawn from
exactly this vocabulary, serialised per Chapter 55 R55.2.7 (payload-carrying
values as single-key objects):

| Value | Meaning |
| --- | --- |
| `ActivatingCoin` | initial value; platform coin being built or tokens activated |
| `SyncingBlockHeaders` with `{current_scanned_block: int, last_block: int}` | header-synchronisation progress, for a platform that synchronises headers |
| `TemporaryError` with a string | a transient synchronisation failure being retried |
| `RequestingWalletBalance` | reading balances |
| `Finishing` | finishing (also reported after a hardware-wallet connection attempt failed) |
| `WaitingForTrezorToConnect` | R48.3.2 |
| `FollowHwDeviceInstructions` | R48.3.2 |

The initial status is `ActivatingCoin`; `RequestingWalletBalance`, when
balances are requested, precedes `Ok`; no other ordering is bound.

> **Reloaded status (informative).** The device-confirmation state is
> `FollowHwDeviceInstructions` since `fix/activation-api-parity`.
> `SyncingBlockHeaders` and `TemporaryError` are not produced (no EVM or
> Tendermint producer exists).

---

## 48.4 Per-coin registration and targets

R48.4.1 The framework shall register one platform-activation task per supported
platform coin. The two required registrations are the EVM platform coin (backing
`task::enable_eth::*`) and the Tendermint platform coin (backing
`task::enable_tendermint::*`).

R48.4.2 Each task family shall be routed on the **same target set as its one-shot
counterpart**: wherever `enable_eth_with_tokens` is available,
`task::enable_eth::*` shall be available; wherever
`enable_tendermint_with_assets` is available, `task::enable_tendermint::*` shall
be available. The task variant shall not be narrower in target coverage than the
one-shot call it wraps.

R48.4.3 The dispatcher shall route the four method strings of each family through
the shared `task::`-prefix router used by the other `task::enable_*` families, so
that an unknown `task::enable_eth::<x>` / `task::enable_tendermint::<x>`
sub-method resolves to the standard no-such-method error (Chapter 55 R55.1.7)
rather than being silently accepted.

R48.4.4 `task::enable_bch` shall **not** be registered on this substrate. In the
reference surface it is a standalone-coin family member that activates the BCH
coin alone (no SLP tokens); it is bound by
[Chapter 55](55-coin-activation-wire-contract.md) §55.5 and §55.8. BCH
activation together with SLP tokens remains the one-shot
`enable_bch_with_tokens` (Chapter 55 §55.9), which has no task variant.

R48.4.5 The token task families are registered on the token substrate of
§48.11, not on this platform substrate. The one required registration is the
ERC20 token family (`task::enable_erc20::*`), whose token-specific contract is
bound by [Chapter 35](35-evm-v2-activation-rpcs.md).

---

## 48.5 Error contract

R48.5.1 The terminal (status-level) error of a platform-activation task shall be
reported with the **platform-activation `error_type` discriminants** of the
corresponding one-shot call -- the EVM platform discriminants of ch. 35 §35.1.5
for `task::enable_eth`, and the Tendermint platform discriminants of ch. 36
§36.1.6 for `task::enable_tendermint` -- with the same `error_data` shapes, so
that the failure surface of the task variant is the same as the one-shot variant
for the same fault (already-activated, config-not-found, protocol-parse,
transport, internal, etc.). That shared set (public catalogue set
`EnablePlatformCoinWithTokensError`) additionally contains `TaskTimedOut`
(`{duration: {secs, nanos}}`, Chapter 55 R55.2.18) for a task-framework
timeout and `NoSuchTask` (integer); these appear only in the task variant.

R48.5.2 The `status`, `user_action`, and `cancel` methods shall carry the
**task-framework discriminants** common to every `task::` family, bound in
[Chapter 55](55-coin-activation-wire-contract.md) §55.2: on `status`,
`NoSuchTask` (integer task id, HTTP 400) and `Internal` (string, 500); on
`user_action`, `NoSuchTask` (integer, 400) and `Internal` (string, 500, also for
a task that is not awaiting a user action); on `cancel`, `NoSuchTask` (integer,
**404**), `TaskFinished` (integer task id, **409**) for a task that already has
a terminal result, and `Internal` (string, 500). No new task-framework error
type is introduced, and no separate activation deadline is imposed by this
framework (R48.3.1).

R48.5.3 The HTTP status of each reused platform discriminant shall be the same
as in the one-shot chapters (ch. 35 §35.1.5, ch. 36 §36.1.6); it applies only
when the discriminant is returned at mmrpc level (by `init`, R48.5.4). A
`status` answer that reports a terminal error is HTTP 200 (Chapter 55 R55.1.5).

R48.5.4 *Synchronous `init` checks (settled).* `init` shall perform exactly two
synchronous checks and answer their failure at mmrpc level:

1. the platform ticker is already active -> `PlatformIsAlreadyActivated`
   (`error_data` the ticker as a string, HTTP 400);
2. the task cannot be created -> `Internal` (string, HTTP 500).

Every other failure -- platform configuration not found, platform or token
protocol unparsable or unexpected, token configuration not found, invalid
payload, missing nodes, platform creation, transport, private-key policy -- is
detected inside the task and reported **only at status level**, with the
discriminant and `error_data` the one-shot call would use.

> **Upstream divergence (informative).** The public documentation's Tendermint
> page shows platform-configuration, protocol and token-configuration errors as
> mmrpc-level answers, but those examples are captured from the one-shot path
> and the page does not say whether `init` or `status` returns them. The
> reference release reports them at status level; R48.5.4 follows it.

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`:
> the synchronous already-active check of R48.5.4, the cancel set of Chapter 55
> R55.2.15, and the shared platform error set with `TaskTimedOut`, `NoSuchTask`
> and `UnexpectedDeviceActivationPolicy`.

---

## 48.6 Signing-policy maturity (informative interface, bound behaviour)

R48.6.1 Reloaded's EVM signing policies are the local-keypair / context
(Iguana, HD) policy and, on the WASM target, the MetaMask delegate policy
(ch. 47). Under each of these policies platform activation **completes without
any interactive `user_action`**: the activation routine reads balances and
registers the coin using the context key (or, under MetaMask, the connected
account established by `task::connect_metamask`), and the task reaches its
terminal success state directly.

R48.6.2 The hardware (Trezor) policy of R48.3.2 is part of the **published**
task surface (it is the reason the published surface exposes `user_action` on
these families) and is the forward-looking consumer of the awaiting-user-action
hook. The EVM Trezor policy is bound by [Chapter 50](50-evm-trezor-signing.md)
and Chapter 35 §35.3.2; Reloaded does not currently ship a Trezor Tendermint
activation policy, and the awaiting-user-action machinery is provided so that
adding one is a policy addition, not a framework change. Under a policy that
needs no interaction, a successful activation never enters the
awaiting-user-action state.

R48.6.3 The presence of the `user_action` method on a coin whose shipped policies
never require it is **not** a stub: the method is routed, validates its
`task_id`, and behaves correctly (it has no awaiting task to satisfy and returns
the standard task-framework response of Chapter 55 R55.2.12). It shall not
panic, and it shall not be wired to a placeholder that fabricates a
confirmation.

---

## 48.7 Behavioural equivalence and side effects

R48.7.1 A platform activation driven to completion through the task family shall
leave the framework in the **same observable state** as the equivalent one-shot
call: the platform coin and its requested tokens are registered and enabled, the
same `current_block` / balance / address surface is reported, and any requested
background transaction-history fetch is started on the same terms.

R48.7.2 `init` of a platform coin that is already activated shall fail
synchronously with the mmrpc-level `PlatformIsAlreadyActivated` of R48.5.4. If
the platform becomes active while a task for it is running (a concurrent
activation registered it first), the task shall end with
`PlatformIsAlreadyActivated` at status level. Two concurrent activations of
the same platform coin both run; the first to register wins and the other
fails at registration without partially registering anything (Chapter 55
R55.5.7 states the same rule for the standalone family).

R48.7.3 A cancelled or failed task shall not leave the platform coin in a
half-registered state: either activation completed (success) or the coin is not
registered (cancel/error). Registration is the task's last step, so a task
cancelled before it completes registers nothing. Cancellation that races
completion shall resolve to one terminal outcome, not both: if the task
completed first, `cancel` answers `TaskFinished` (R48.5.2).

---

## 48.8 Acceptance checks (informative)

The following observable checks characterise a correct port; they are guidance,
not additional contract beyond §§48.1--48.7 and §48.11.

- A1. `task::enable_eth::init` with valid `enable_eth_with_tokens` params returns a
  `task_id`; polling `task::enable_eth::status` eventually yields the **same**
  result `enable_eth_with_tokens` would return for those params.
- A2. `task::enable_tendermint::init` with valid `enable_tendermint_with_assets`
  params returns a `task_id`; polling `task::enable_tendermint::status` eventually
  yields the **same** result `enable_tendermint_with_assets` would return.
- A3. `status` on an unknown `task_id` returns `NoSuchTask` (integer, HTTP 400);
  `cancel` on an unknown `task_id` returns `NoSuchTask` (integer, HTTP 404);
  `cancel` on a finished task returns `TaskFinished` (integer, HTTP 409).
- A4. `init` on an already-activated platform coin answers the mmrpc-level
  `PlatformIsAlreadyActivated` (string, HTTP 400) without creating a task
  (R48.5.4); `init` with a ticker missing from the configuration returns a
  `task_id`, and the task ends with status-level `PlatformConfigIsNotFound`.
- A5. `cancel` of an in-flight activation leaves the platform coin not enabled
  (R48.7.3); a subsequent one-shot or task activation of that coin succeeds.
- A6. `task::enable_eth::*` is reachable on every target where
  `enable_eth_with_tokens` is reachable, and likewise for Tendermint (R48.4.2).
- A7. The EVM and Tendermint task registries are independent: a `task_id` minted
  by one family is unknown to the other (R48.1.6).
- A8. A transport failure during activation surfaces, as the task's terminal
  error, the same discriminant and `error_data` shape the one-shot call uses for
  the same fault (R48.5.1), inside an HTTP 200 status answer (R48.5.3).
- A9. A platform task's in-progress values serialise from the vocabulary of
  R48.3.3 only.
- A10. `task::enable_erc20` follows §48.11: its `init` rejects an inactive
  platform synchronously with `PlatformCoinIsNotActivated`, and its terminal
  error uses the token task error set.

---

## 48.9 Relationship to other chapters

- [Chapter 35](35-evm-v2-activation-rpcs.md) §35.3 (`task::enable_eth`) and
  [Chapter 36](36-tendermint-v2-activation-rpcs.md) §36.6
  (`task::enable_tendermint`) define the **per-coin** wire contract (params,
  result, error discriminants) and record the dependency on this substrate. This
  chapter supplies the substrate; with it in place, those sections are
  **unblocked** and their task families are delivered (see the implementation
  status note in §48.0).
- Chapter 35 binds the ERC20-specific contract of `task::enable_erc20` on the
  token substrate of §48.11 (request `activation_params`, success fields beyond
  the generic ones, ERC20 failure mapping).
- [Chapter 55](55-coin-activation-wire-contract.md) binds the mmrpc envelope,
  the task-framework methods shared by every `task::` family (§55.2), the
  cross-family unreachable-backend outcomes (§55.3), and the standalone-coin
  family including `task::enable_bch` (§55.5, §55.8).
- Ch. 38 (UTXO coin maintenance) and the standalone-coin task family describe the
  **sibling** substrate and the hardware task plumbing that R48.3.2 reuses.
- Ch. 47 (MetaMask) defines the WASM MetaMask signing policy under which EVM
  platform activation (one-shot and task) completes without `user_action`
  (R48.6.1).

## 48.10 Reloaded status summary (informative)

The deviations of current Reloaded behaviour from this chapter, collected from
the *Reloaded status* notes above, in behavioural terms:

- platform `init` performs no synchronous already-active check (R48.5.4);
- platform `cancel` of a finished task answers `Internal` (500) instead of
  `TaskFinished` (409), and of an unknown task answers `NoSuchTask` with 400
  instead of 404 (R48.5.2);
- the shared platform error set lacks `TaskTimedOut`, `NoSuchTask` and
  `UnexpectedDeviceActivationPolicy` (R48.5.1, R48.2.4);
- the platform in-progress vocabulary differs from R48.3.3;
- the token task substrate of §48.11 and the `task::enable_erc20` family do
  not exist;
- `client_id` is accepted but ignored (Chapter 55 D55.1).

## 48.11 Token task-activation substrate

This section binds the generic substrate on which a **token** family --
activating one token on an already active platform coin as a long-running
task -- runs. The one required family is `task::enable_erc20` (R48.4.5);
[Chapter 35](35-evm-v2-activation-rpcs.md) binds what is ERC20-specific. The
substrate follows the shared task framework of
[Chapter 55](55-coin-activation-wire-contract.md) §55.2 for `status`,
`user_action` and `cancel`.

R48.11.1 *Methods and registry.* A token family exposes
`task::<family>::{init,status,user_action,cancel}`, routed on every target on
which the corresponding one-shot token method is routed, with its own task
registry (Chapter 55 R55.2.4).

R48.11.2 *`init` request* (`params`):

| Field | Type | Required / default | Effect |
| --- | --- | --- | --- |
| `ticker` | string | required | token ticker |
| `activation_params` | object (per family; Chapter 35 for ERC20) | required | token activation parameters |
| `protocol` | object | optional | custom-token protocol, with the custom-token rules of Chapter 35 R35.5 |
| `client_id` | integer | optional, default 0 | Chapter 55 R55.2.2 |

R48.11.3 *Synchronous `init` checks.* Before creating the task, `init`
performs exactly these checks, in this order, answering the first failure at
mmrpc level:

1. the token ticker is already active -> `TokenIsAlreadyActivated`;
2. the token configuration or protocol cannot be resolved ->
   `TokenConfigIsNotFound`, `TokenProtocolParseError`,
   `UnexpectedTokenProtocol`, or (for a custom-token request)
   `CustomTokenError`;
3. the token's platform coin is not active -> `PlatformCoinIsNotActivated`;
4. the active platform coin is not of the token's platform type ->
   `UnsupportedPlatformCoin`;
5. the task cannot be created -> `Internal`.

R48.11.4 *Error set* (public catalogue set `InitTokenError`), used for the
mmrpc-level `init` errors and for every status-level error of a token task;
family-specific failures are mapped into it before they are reported:

| `error_type` | Failure class | `error_data` | HTTP |
| --- | --- | --- | --- |
| `NoSuchTask` | (reserved; task lookups use the Chapter 55 §55.2 sets) | integer | 400 |
| `TaskTimedOut` | task-framework or device-connection timeout | object `{duration: {secs, nanos}}` | 408 |
| `TokenIsAlreadyActivated` | token already active at `init`, or registered concurrently when the task finishes | object `{ticker: string}` | 400 |
| `TokenConfigIsNotFound` | token absent from configuration | string: the token ticker | 400 |
| `TokenProtocolParseError` | token protocol unparsable | object `{ticker: string, error: string}` | 400 |
| `UnexpectedTokenProtocol` | token protocol not of this family | object `{ticker: string, protocol: <protocol descriptor>}` | 400 |
| `TokenCreationError` | the token cannot be created | object `{ticker: string, error: string}` | 400 |
| `CouldNotFetchBalance` | the token balance cannot be read | string | 500 |
| `PlatformCoinIsNotActivated` | the token's platform is not active | string: the **platform** ticker | 400 |
| `UnsupportedPlatformCoin` | the active platform is of the wrong type | object `{platform_coin_ticker: string, token_ticker: string}` | 500 |
| `PlatformCoinMismatch` | the token's protocol names a different platform | absent | 400 |
| `CustomTokenError` | a custom-token request is rejected | as bound in Chapter 35 | 400 |
| `HwError` | hardware-wallet failure | string (Chapter 55 R55.2.17) | 410 |
| `Transport` | a backend query failed | string | 500 |
| `Internal` | any other failure | string | 500 |

Note that `TokenIsAlreadyActivated` carries an **object** here, whereas the
one-shot token set (Chapter 55 R55.10.4) carries a string; each set's shape is
bound independently.

R48.11.5 *Progress values.* In-progress `details` values from exactly:
`ActivatingCoin` (initial), `TemporaryError` with a string,
`RequestingWalletBalance`, `Finishing`, `WaitingForTrezorToConnect`,
`FollowHwDeviceInstructions`; awaiting values `EnterTrezorPin`,
`EnterTrezorPassphrase`; user actions per Chapter 55 R55.2.10.

R48.11.6 *Success object.* The generic token task success object carries at
least:

| Field | Type |
| --- | --- |
| `ticker` | string |
| `platform_coin` | string |
| `token_contract_address` | string |
| `current_block` | integer |
| `required_confirmations` | integer |
| `wallet_balance` | object in the Iguana or HD form of Chapter 55 R55.5.13, with ticker-keyed balance maps (Chapter 35 binds which tickers appear) |

Chapter 35 binds the ERC20 values of these fields.

R48.11.7 *Registration and cancel.* The token is registered as an active coin
and its information recorded on the platform coin as the task's last step; a
concurrent registration of the same ticker ends the task with status-level
`TokenIsAlreadyActivated`. A cancelled token task deregisters the token only
if that task itself had registered it; a token registered by another
activation stays (Chapter 55 R55.2.14, R55.5.8).

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`
> with the ERC20 registration of Chapter 35 (`task::enable_erc20::*`).
> Remaining: the hardware progress values of R48.11.5 are never emitted (the
> token task performs no device exchange); a token can be enabled only at the
> platform's enabled address, and `scan_policy` / `min_addresses_number` are
> accepted and ignored.

## 48.12 Provenance Footer

- *Inputs:* reloaded's already-shipped one-shot platform-coin-with-tokens
  activation routine that EVM and Tendermint activation delegate to, and its
  two existing sibling task-activation substrates -- the standalone-coin
  family (`task::enable_utxo`, `task::enable_qtum`, the Z-coin task trio)
  and the l2 family (`task::enable_lightning`,
  [Chapter 41](41-lightning-network.md)) -- the precedent this chapter's own
  "Treatment" note names as the shape the third, platform-with-tokens
  substrate follows; the public mmrpc-2.0 task-status envelope shape shared
  by every `task::` family and the public error-types catalogue;
  [Chapter 35](35-evm-v2-activation-rpcs.md) §35.3
  and [Chapter 36](36-tendermint-v2-activation-rpcs.md) §36.6 (the per-coin
  wire contracts this substrate was recorded as blocking, per those
  chapters' own dependency notes); [Chapter 38](38-utxo-coin-maintenance-and-features.md)
  (the sibling standalone-coin substrate and hardware task plumbing R48.3.2
  reuses); [Chapter 47](47-metamask-integration.md) (the WASM MetaMask
  signing policy under which EVM platform activation completes without
  `user_action`, R48.6.1); [Chapter 50](50-evm-trezor-signing.md);
  [Chapter 55](55-coin-activation-wire-contract.md) (the shared task-framework
  contract this chapter references).
- *Permitted-input classes used:* baseline/sibling source (the existing
  one-shot activation routine and the standalone-coin/l2 task-substrate
  precedent); cross-chapter contracts (Chapters 35, 36, 38, 47, 50, 55); Interop
  / wire-and-API-bound reuse (R29/R31) for the dictated task-status envelope
  shape shared by every `task::` family.
- *Sibling-allowlist consultations:* [Chapter 35](35-evm-v2-activation-rpcs.md)
  §35.3, [Chapter 36](36-tendermint-v2-activation-rpcs.md) §36.6,
  [Chapter 38](38-utxo-coin-maintenance-and-features.md),
  [Chapter 41](41-lightning-network.md),
  [Chapter 47](47-metamask-integration.md).
- *Forbidden corpus:* the original revision did not consult it. The revision
  that added §48.11, R48.2.4, R48.3.3, R48.4.4–R48.4.5 and R48.5.4 and
  tightened R48.5.1–R48.5.2 and R48.7.2–R48.7.3 was authored by the KDF Spec
  Reader under the chapter-01 two-team clean-room workflow, consulting the
  GLEEC KDF release tag `v3.0.0-beta` (compared with `v2.6.0-beta`, identical
  on this surface) only to decide what the public documentation leaves silent
  or contradictory: which `init` errors are synchronous, the in-progress
  vocabulary, the cancel error contract, the token task's synchronous checks,
  error set, HTTP statuses and success fields. No source text, private
  identifier, helper structure, log or error string, or per-method internal
  table was copied; every rule is stated as an externally observable wire or
  behavioural requirement.
