# Chapter 48 -- Platform-Coin Task-Activation Framework

**Status:** driving-spec (required port). This chapter specifies a **shared
task-activation substrate** for platform-coin-with-tokens activation. It does not
introduce a new coin, a new wire contract, or a new activation result shape; it
provides the long-running task machinery that the EVM (ch. 35 §35.3) and
Tendermint (ch. 36 §36.6) task-activation families were recorded as blocked on.

> **One-sentence claim:** the project shall provide a generic
> platform-coin task-activation framework -- the `init`/`status`/`user_action`/
> `cancel` lifecycle, the standard task-status envelope, and the per-coin task
> registry -- so that platform-coin-with-tokens activation (EVM, Tendermint, and
> any future platform) can be exposed as a long-running `task::enable_*` family
> in addition to its one-shot call, **reusing the already-shipped one-shot
> activation routine unchanged** as the unit of work.

> **Treatment:** **T-PORT.** Reloaded already ships (a) the one-shot
> platform-coin-with-tokens activation routine that EVM and Tendermint activation
> delegate to, and (b) two sibling task-activation substrates -- the
> standalone-coin family (backing `task::enable_utxo`, `task::enable_qtum`, the
> Z-coin task trio) and the l2 family (backing `task::enable_lightning`). What is
> required is the **third sibling**: a platform-with-tokens task substrate of the
> same shape, plus the per-coin task registration that routes the EVM and
> Tendermint task method strings to it.

> **Binding scope.** Requirements bind observable behaviour, the public mmrpc-2.0
> method strings and their request/response JSON field names, the task-status
> envelope shape (shared by every `task::` family), and the bound `error_type`
> discriminants. Those public contracts are the source of truth, not this
> project's code. Private Rust types, trait decomposition, the choice of
> per-coin versus generic status enums, and internal module structure are
> informative and are **not** bound by this chapter.

> **Source of truth (informative).** The method strings, the per-coin `init`
> parameters and success-result shapes, and the activation error discriminants
> are the published Komodo DeFi Framework API contract (the public KDF API
> documentation: the coin-activation `enable_eth_with_tokens` /
> `enable_tendermint_with_assets` sections and the task-method index). The
> task-status envelope is the same one every documented `task::` family uses.
> Where this chapter and the public API docs disagree, the public API docs
> govern.

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

Both families are blocked, in chapters 35 and 36, on **the same missing piece**:
a platform-coin task-activation substrate. Reloaded ships the standalone-coin and
l2 task substrates but not the platform-with-tokens one. This chapter specifies
that substrate. Once it exists, the per-coin families above become a thin
registration over it (one task per platform coin), and chapters 35 §35.3 / 36
§36.6 are unblocked.

The substrate is **behaviour-preserving with respect to the one-shot call**: a
task's unit of work is the existing one-shot activation routine, run inside the
task so that progress can be polled and the run can be cancelled. The `init`
parameters and the final success result are **identical** to the one-shot call's
(R48.2); no task-only request field is added, preserving 100% compatibility with
the published one-shot activation schema.

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
**before** activation completes.

R48.1.3 `status` shall accept a `task_id` (and the standard
`forget_if_finished` selector) and return the **standard task-status envelope**
used by every `task::` family. That envelope distinguishes at least:

- an **in-progress** state carrying a coin-activation progress value (R48.3);
- a terminal **success** state carrying the activation result (R48.2.2);
- a terminal **error** state carrying a bound `error_type` (R48.5);
- an **awaiting-user-action** state, when the active signing policy requires an
  interactive confirmation (R48.3.2).

R48.1.4 `user_action` shall accept a `task_id` together with a `user_action`
payload and deliver it to a task that is in the awaiting-user-action state. For
signing policies that complete without interaction (R48.6), no `user_action`
call is required for a successful activation; the method is routed for wire
parity with the published surface and for the interactive policies of R48.3.2.

R48.1.5 `cancel` shall accept a `task_id` and abort an in-flight activation that
has not yet reached a terminal state. Cancelling shall not leave the platform
coin partially registered (R48.7.3).

R48.1.6 Each registered platform coin shall own an **independent task registry**,
so that an in-flight EVM activation and an in-flight Tendermint activation are
addressed by independent `task_id` spaces and neither can observe or cancel the
other's tasks.

---

## 48.2 Request and result delegation (no new wire surface)

R48.2.1 `task::enable_eth::init` shall accept **exactly** the `enable_eth_with_tokens`
parameters of ch. 35 §35.1, and `task::enable_tendermint::init` shall accept
**exactly** the `enable_tendermint_with_assets` parameters of ch. 36 §36.1. The
task framework shall **not** add, remove, or rename any request field relative to
the one-shot call.

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
surface the hardware-interaction states reused from the existing hardware task
plumbing: at least **waiting for the device to connect**, **awaiting on-device
confirmation**, and, as an awaiting-user-action state, **awaiting a PIN entry**;
the corresponding confirmation/PIN payload shall be supplied via `user_action`
(R48.1.4). These states and their user-action payloads shall reuse the same
hardware task contract already used by the standalone-coin task family (ch. 35
§35.3.2, ch. 38), not a new one.

R48.3.3 The in-progress status values are part of the observable surface only at
the level of *which* progress phases are reported; the exact serialized
discriminant spelling of each in-progress value is informative and not bound by
this chapter, except where it coincides with the hardware task contract of
R48.3.2.

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
sub-method resolves to the standard no-such-method error rather than being
silently accepted.

---

## 48.5 Error contract

R48.5.1 The terminal error of a platform-activation task shall reuse the
**platform-activation `error_type` discriminants** of the corresponding one-shot
call -- the EVM platform discriminants of ch. 35 §35.1.5 for `task::enable_eth`,
and the Tendermint platform discriminants of ch. 36 §36.1.6 for
`task::enable_tendermint` -- so that the failure surface of the task variant is
the same as the one-shot variant for the same fault (already-activated,
config-not-found, protocol-parse, transport, internal, etc.).

R48.5.2 In addition, the `status`, `user_action`, and `cancel` methods shall
carry the **task-framework discriminants** common to every `task::` family:
unknown/`no_such_task` for an unrecognised `task_id`, and the shared
timeout-class discriminant the task plumbing already surfaces (e.g. when a
bounded `user_action` wait elapses) -- not a separate activation deadline
imposed by this framework (R48.3.1). These are the same task-framework
discriminants the standalone-coin and l2 families already surface; no new
task-framework error type is introduced.

R48.5.3 The HTTP status mapping of each reused discriminant shall be the same as
in the one-shot chapters (ch. 35 §35.1.5, ch. 36 §36.1.6) and the shared
task-framework chapters; this chapter does not re-map any status code.

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
hook. Reloaded does not currently ship a Trezor EVM/Tendermint activation policy;
the awaiting-user-action machinery is provided so that adding one is a policy
addition, not a framework change. Until such a policy exists, a successful
activation under the shipped policies never enters the awaiting-user-action state.

R48.6.3 The presence of the `user_action` method on a coin whose shipped policies
never require it is **not** a stub: the method is routed, validates its
`task_id`, and behaves correctly (it has no awaiting task to satisfy and returns
the standard task-framework response). It shall not panic, and it shall not be
wired to a placeholder that fabricates a confirmation.

---

## 48.7 Behavioural equivalence and side effects

R48.7.1 A platform activation driven to completion through the task family shall
leave the framework in the **same observable state** as the equivalent one-shot
call: the platform coin and its requested tokens are registered and enabled, the
same `current_block` / balance / address surface is reported, and any requested
background transaction-history fetch is started on the same terms.

R48.7.2 Re-`init` of a platform coin that is already activated shall fail with the
same already-activated discriminant the one-shot call uses (R48.5.1); the task
framework shall not allow a second concurrent activation of the same platform coin
to partially proceed.

R48.7.3 A cancelled or failed task shall not leave the platform coin in a
half-registered state: either activation completed (success) or the coin is not
registered (cancel/error). Cancellation that races completion shall resolve to one
terminal outcome, not both.

---

## 48.8 Acceptance checks (informative)

The following observable checks characterise a correct port; they are guidance,
not additional contract beyond §§48.1--48.7.

- A1. `task::enable_eth::init` with valid `enable_eth_with_tokens` params returns a
  `task_id`; polling `task::enable_eth::status` eventually yields the **same**
  result `enable_eth_with_tokens` would return for those params.
- A2. `task::enable_tendermint::init` with valid `enable_tendermint_with_assets`
  params returns a `task_id`; polling `task::enable_tendermint::status` eventually
  yields the **same** result `enable_tendermint_with_assets` would return.
- A3. `status` on an unknown `task_id` returns the task-framework
  unknown-task error; `cancel` on an unknown `task_id` returns the same.
- A4. `init` on an already-activated platform coin surfaces the
  already-activated discriminant (R48.7.2).
- A5. `cancel` of an in-flight activation leaves the platform coin not enabled
  (R48.7.3); a subsequent one-shot or task activation of that coin succeeds.
- A6. `task::enable_eth::*` is reachable on every target where
  `enable_eth_with_tokens` is reachable, and likewise for Tendermint (R48.4.2).
- A7. The EVM and Tendermint task registries are independent: a `task_id` minted
  by one family is unknown to the other (R48.1.6).
- A8. A transport failure during activation surfaces the `Transport` discriminant
  as the task's terminal error, with the same HTTP status as the one-shot call
  (R48.5.1, R48.5.3).

---

## 48.9 Relationship to other chapters

- Ch. 35 §35.3 (`task::enable_eth`) and ch. 36 §36.6
  (`task::enable_tendermint`) define the **per-coin** wire contract (params,
  result, error discriminants) and record the dependency on this substrate. This
  chapter supplies the substrate; with it in place, those sections are
  **unblocked** and their task families are delivered.
- Ch. 38 (UTXO coin maintenance) and the standalone-coin task family describe the
  **sibling** substrate and the hardware task plumbing that R48.3.2 reuses.
- Ch. 47 (MetaMask) defines the WASM MetaMask signing policy under which EVM
  platform activation (one-shot and task) completes without `user_action`
  (R48.6.1).
