# Chapter 55 -- Coin Activation Wire Contract

**Status:** driving-spec.

This chapter binds the cross-family wire contract of every coin-activation RPC
-- the mmrpc 2.0 error envelope and its HTTP-status policy, the four
task-framework methods shared by every `task::` activation family, and the
observable outcome of every activation family when the coin's backend is
unreachable or unresponsive -- and the complete per-method contracts of the
baseline-inherited activation families that no other chapter owns: the legacy
`enable` and `electrum` methods, `task::enable_utxo`, `task::enable_qtum`, the
new `task::enable_bch`, `enable_bch_with_tokens`, `enable_slp`, and
`task::init_trezor` including its new `cancel` method.

> **Binding scope.** Requirements bind the public method strings, the request
> field names, types, defaults and accepted alternative forms, the success
> shapes, the closed set of `error_type` discriminants each method can emit,
> the mapping from failure class to `error_type`, the JSON shape of
> `error_data` per `error_type`, the HTTP status per `error_type` and per
> placement, and the numeric encoding. The human-readable `error` text,
> `error_path` and `error_trace` are present on the wire but their content is
> **not** bound. Private Rust types, helper decomposition and module layout are
> not bound.

> **Sources of truth and precedence (informative).**
> 1. The public Komodo DeFi Framework API reference governs wherever it is
>    explicit: its field table and its example JSON agree, or only an example
>    exists and nothing on the site contradicts it.
> 2. Where the public reference is silent, ambiguous or contradicts itself --
>    including every HTTP status code, which the reference states nowhere --
>    the maintainer's decision of 2026-10-08 applies: Reloaded does what the
>    GLEEC KDF reference release does. The reference release for this chapter
>    is the GLEEC KDF tag **`v3.0.0-beta`**, the most recent release tag
>    available to the Spec Reader.
> 3. Where a rule below departs from the public reference because the
>    reference is wrong or self-contradictory, the rule is marked
>    *public-docs erratum*.

> **Upstream divergence (informative).** For every family this chapter owns
> (§55.4–§55.11), for the task framework of §55.2 and for the envelope of
> §55.1, the netid-8762 legacy reference `v2.6.0-beta` exposes the same wire
> shapes, discriminants, `error_data` shapes and HTTP statuses as `v3.0.0-beta`.
> The only activation-surface change between the two tags that touches this
> chapter is that the later tag additionally rejects one more token protocol
> (TRC20) on the legacy `enable`/`electrum` path (§55.4, R55.4.7); the earlier
> tag had no such protocol at all. No per-network split is therefore required
> by this chapter.

---

## Executive Summary

A wallet that activates coins against Reloaded parses three things from every
answer: the `error_type` discriminant, the JSON type and fields of
`error_data`, and the HTTP status. A difference in any of them is a breaking
change for that wallet even when the human-readable message is equivalent.
The CRD previously bound, for most activation methods, only the discriminant
names and their HTTP statuses; it did not bind which failure produces which
discriminant, nor the `error_data` shape, nor where a task error is placed.
This chapter closes that gap for the shared surface and for the families with
no chapter of their own.

The chapter is organised in three layers:

1. **Cross-cutting envelope** (§55.1): the success and error envelope, how
   `error_data` is encoded for each kind of discriminant, the HTTP-status
   policy, the two error placements (mmrpc-level and task-status-level),
   numeric encoding, and the legacy error body.
2. **Task framework** (§55.2): the `init` / `status` / `user_action` /
   `cancel` contract shared by every `task::` activation family -- task ids,
   the status envelope, `forget_if_finished`, the `NoSuchTask` /
   `TaskFinished` / `Internal` cancel contract and its HTTP statuses, the
   timeout and hardware-wallet discriminants, and the hardware-wallet
   user-action vocabulary. Per-family chapters reference §55.2 instead of
   restating it.
3. **Per-family contracts**: a single cross-family table of
   unresponsive-backend outcomes (§55.3, a summary whose details the owning
   chapters bind), and the full contracts of the families owned here
   (§55.4–§55.11).

## Subsystem Shape

| Activation family | Method strings | Substrate | Governing chapter |
| --- | --- | --- | --- |
| Legacy UTXO / QTUM / QRC20 / BCH / SLP / EVM | `enable`, `electrum` (legacy, non-mmrpc) | legacy coin initialisation | this chapter §55.4 |
| UTXO standard | `task::enable_utxo::{init,status,user_action,cancel}` | standalone-coin task family | this chapter §55.5, §55.6 |
| QTUM | `task::enable_qtum::{init,status,user_action,cancel}` | standalone-coin task family | this chapter §55.5, §55.7 |
| BCH (task) | `task::enable_bch::{init,status,user_action,cancel}` (**new**) | standalone-coin task family | this chapter §55.5, §55.8 |
| BCH with SLP tokens | `enable_bch_with_tokens` | platform-with-tokens one-shot | this chapter §55.9 |
| SLP token | `enable_slp` | one-shot token | this chapter §55.10 |
| Hardware-wallet context | `task::init_trezor::{init,status,user_action,cancel}` (`cancel` **new**) | init task | this chapter §55.11 |
| EVM platform / ERC20 / NFT | `enable_eth_with_tokens`, `task::enable_eth::*`, `enable_erc20`, `task::enable_erc20::*`, `enable_nft` | platform-with-tokens, token, token task | [Chapter 35](35-evm-v2-activation-rpcs.md), [Chapter 48](48-platform-coin-task-activation.md), [Chapter 19](19-nft-module-layout.md) |
| Tendermint platform / token | `enable_tendermint_with_assets`, `task::enable_tendermint::*`, `enable_tendermint_token` | platform-with-tokens, token | [Chapter 36](36-tendermint-v2-activation-rpcs.md), [Chapter 48](48-platform-coin-task-activation.md) |
| Z-coin | `task::enable_z_coin::*` | standalone-coin task family | [Chapter 39](39-zcash---z_coin-shielded-coin.md) |
| Sia | `task::enable_sia::*` | standalone-coin task family | [Chapter 46](46-sia-v2-activation-rpcs.md) |
| Solana | Solana platform / token methods (`enable_solana_with_tokens`, `enable_spl`; switched off in this beta, see §55.3 and [Chapter 40](40-solana-coin.md)) | platform-with-tokens, token | [Chapter 40](40-solana-coin.md) |
| Lightning | `task::enable_lightning::*` | layer-2 task | [Chapter 41](41-lightning-network.md) |
| MetaMask connection | `task::connect_metamask::*` | init task | [Chapter 47](47-metamask-integration.md) |

The four task substrates (standalone-coin, platform-with-tokens, token, and
layer-2) and the hardware-wallet and MetaMask init tasks share one task
framework; §55.2 binds its observable contract once.

---

## 55.1 mmrpc envelope and HTTP-status policy

**R55.1.1** *Success envelope.* A successful mmrpc 2.0 activation call shall
answer HTTP 200 with the body `{"mmrpc":"2.0","result":<value>,"id":<id>}`.
`<id>` is the request's `id` when the request carried a non-negative integer
`id`; otherwise the key is present with the value `null`.

**R55.1.2** *Error envelope.* A failed mmrpc 2.0 call (an *mmrpc-level* error)
shall answer with the body
`{"mmrpc":"2.0","error":<string>,"error_path":<string>,"error_trace":<string>,"error_type":<string>,"error_data":<value>,"id":<id>}`
with no `result` key. `error_type` and `error_data` form an adjacently tagged
pair: `error_type` names the discriminant, `error_data` carries its payload.

**R55.1.3** *Encoding of `error_data`.* The JSON shape of `error_data` is fixed
per discriminant by the tables of this chapter and of the owning chapters, and
is one of:

| Discriminant kind | `error_data` on the wire |
| --- | --- |
| no payload | the `error_data` key is **absent** (not `null`) |
| a single text payload | a JSON string |
| a task identifier | a JSON integer (the task id) |
| a record | a JSON object with exactly the bound field names |
| a hardware-wallet error name | a JSON string holding one name of the vocabulary of R55.2.17 |
| a duration | a JSON object `{"secs": <integer>, "nanos": <integer>}` |
| a duration inside a record | a JSON object `{"duration": {"secs": <integer>, "nanos": <integer>}}` |
| a coin-protocol descriptor inside a record | a JSON object with a string `type` and, when the protocol carries parameters, an object `protocol_data` |

**R55.1.4** *Human-readable fields.* `error`, `error_path` and `error_trace`
shall always be present as strings on both placements (R55.1.6). Their text is
not part of the contract and clients must not parse it. A client-visible
difference in these strings is not a wire-compatibility defect.

**R55.1.5** *HTTP-status policy.* The HTTP status of an mmrpc-level error is
the status bound for that `error_type` in the error table of the method that
produced it. The same discriminant name may carry different statuses in
different error sets, and each set's status is bound independently; in
particular `Transport` is 502 in the platform-with-tokens set (§55.9) but 500
in the standalone-coin (§55.5), token (§55.10, [Chapter 48](48-platform-coin-task-activation.md)
§48.11) and layer-2 sets, and `NoSuchTask` is 400 on `status` and
`user_action` but 404 on `cancel` (§55.2). A task-status response is HTTP 200
whatever the outcome it reports (R55.2.7).

**R55.1.6** *Two placements.* An activation error is reported in exactly one of
two placements, and the governing table of each method states which:

- *mmrpc-level*: the envelope of R55.1.2, with the HTTP status of R55.1.5;
- *status-level*: inside a successful `status` answer as
  `{"status":"Error","details":<error object>}`, where `<error object>` is
  `{"error","error_path","error_trace","error_type","error_data"}` -- the
  error envelope without `mmrpc` and `id` -- placed **directly** as the value
  of `details`, with no `result` wrapper, and with `error_data` absent for a
  payload-less discriminant. *Public-docs erratum:* several task pages'
  response tables describe a `details.result` wrapper; every published example
  places the error object directly in `details`, and so does the reference
  release.

**R55.1.7** *Dispatcher errors seen by activation callers.* The dispatcher
errors are bound by [Chapter 43](43-daemon-rpc-dispatcher-and-misc-maintenance.md);
restated here because every activation method can return them: parameters
that cannot be decoded into the method's request shape answer `InvalidRequest`
(string, 400); an unknown method string -- including an unknown
`task::<family>::<sub-method>` and a known family name on a target where it is
not routed -- answers `NoSuchMethod` (no `error_data`, 400); authentication and
access-control failures answer the 403 discriminants of Chapter 43.

**R55.1.8** *Unknown request keys.* The activation request objects of the
methods bound in this chapter (the `params` object and every nested
activation-parameter object) shall accept and ignore keys they do not
recognise, unless the method's own table binds a key as rejected. The outer
mmrpc envelope keeps the strictness bound by Chapter 43.

**R55.1.9** *Numeric encoding.* In every activation request and response: coin
amounts and balances are JSON decimal strings; no `*_rat` or `*_fraction`
companion field appears; block heights, task ids, confirmation counts, account
and address indices, `secs` and `nanos` are JSON integers; flags are JSON
booleans.

**R55.1.10** *Legacy error body.* An error from a legacy (non-mmrpc) activation
method (§55.4) shall answer HTTP 500 with the body `{"error":<string>}` and no
other key, for every failure class including caller errors. The string content
is not bound.

**R55.1.11** *Closed discriminant sets.* For every method and placement bound
in this chapter, the `error_type` values a caller can observe are exactly the
values listed in the governing table; a failure class the table maps to a
discriminant shall produce that discriminant and no other.

**R55.1.12** *Serialized same-ticker activation (maintainer decision,
2026-10-11).* Reloaded shall serialize activations of the same ticker across
every family and path (task, one-shot and legacy): an activation that starts
while another activation of the same ticker is still running waits until that
one has finished, then re-checks whether the ticker is registered. If it is,
the waiting activation ends with its family's already-activated discriminant in
that family's usual placement (`CoinIsAlreadyActivated` at status level for the
standalone-coin family, `PlatformIsAlreadyActivated` / `TokenIsAlreadyActivated`
for the platform and token families, the legacy error for `enable` /
`electrum`); if it is not, it proceeds with its own activation. The reference
runs such activations in parallel and the loser reports the same discriminant
(R55.5.7), so this is not a wire divergence: only the timing differs. It avoids
two activations building the same coin at once; for a shielded coin, which
writes its wallet database during activation, that would be a corruption
risk.

> **Reloaded status (informative).** The success and error envelopes, the
> `id` echo, the adjacently tagged encoding and the dispatcher errors already
> match. Two cross-cutting differences sit with
> [Chapter 43](43-daemon-rpc-dispatcher-and-misc-maintenance.md) rather than
> with activation: Reloaded HTML-escapes `<`, `>` and `&` inside response
> bodies (so an error string containing a node URL with a query string is
> altered), which the reference release does not do; and a legacy request
> whose method the legacy table does not know is answered as a legacy error,
> whereas the reference release retries such a request on the mmrpc 2.0
> dispatcher. Neither affects a bound field of this chapter.

---

## 55.2 Task-framework methods (shared by every `task::` activation family)

### 55.2.1 Routing and identity

**R55.2.1** Every task activation family exposes the four mmrpc 2.0 methods
`task::<family>::init`, `task::<family>::status`,
`task::<family>::user_action` and `task::<family>::cancel`, where `<family>` is
one of `enable_utxo`, `enable_qtum`, `enable_bch`, `enable_z_coin`,
`enable_sia`, `enable_eth`, `enable_erc20`, `enable_tendermint`,
`enable_lightning` and `init_trezor`. (`task::connect_metamask` follows the
same contract without a `user_action` method; see
[Chapter 47](47-metamask-integration.md).) Each family is routed on every
target on which its governing chapter routes it.

**R55.2.2** *`init` request.* The `init` `params` object carries the family's
activation request (bound per family) and, at its top level, an optional
`client_id` (non-negative integer, default 0). `client_id` shall be accepted by
every family's `init`, including `task::init_trezor::init`; it names the
event-stream client to which the reference release pushes the task's status
changes (see D55.1).

**R55.2.3** *`init` success.* A successful `init` answers
`{"task_id": <integer>}`. Task ids come from one process-wide counter that
starts at 0 and increases by one for every task spawned by any family; a task
id is never reused within one process lifetime.

**R55.2.4** *Per-family registries.* Each family keeps its own task registry. A
task id issued by one family is unknown to the `status`, `user_action` and
`cancel` methods of every other family, which answer `NoSuchTask` for it.

**R55.2.5** *`init` errors.* `init` answers an mmrpc-level error only for the
synchronous checks bound for the family (§55.5 R55.5.3, §55.11 R55.11.3,
[Chapter 48](48-platform-coin-task-activation.md) §48.5 and §48.11,
[Chapter 41](41-lightning-network.md)), and `Internal` (string, 500) when the
task cannot be created. A failure that occurs after the task was created is
reported only at status level.

### 55.2.2 `status`

**R55.2.6** *Request.* `task_id` (integer, required); `forget_if_finished`
(boolean, optional, default `true`).

**R55.2.7** *Response.* A known task answers HTTP 200 with
`result = {"status": <S>, "details": <D>}`:

| `status` | `details` |
| --- | --- |
| `InProgress` | the family's current in-progress value: a JSON string, or -- for a value that carries data -- a single-key object whose key is the value's name and whose value is its data (for example a key `TemporaryError` with a string, or a key `SyncingBlockHeaders` with an object) |
| `UserActionRequired` | the family's awaiting value, a JSON string (for hardware-wallet interaction `EnterTrezorPin` or `EnterTrezorPassphrase`) |
| `Ok` | the family's success object, with no wrapper |
| `Error` | the error object of R55.1.6 |

No other `status` value is emitted; in particular a cancelled task is never
reported (R55.2.8).

**R55.2.8** *`forget_if_finished` semantics.*

- (a) When the task is terminal (`Ok` or `Error`) and `forget_if_finished` is
  `true`, the terminal answer is returned once and the task is then removed;
  any later `status`, `user_action` or `cancel` for that id answers
  `NoSuchTask`.
- (b) When the task is terminal and `forget_if_finished` is `false`, the
  terminal answer is returned and the task is kept; repeated reads return the
  same terminal answer, and a later read with `true` removes it.
- (c) A non-terminal task is never removed by `status`, whatever the flag.
- (d) From the moment a `cancel` for the task succeeds, the task is not
  observable: `status` answers `NoSuchTask`.

**R55.2.9** *mmrpc-level errors of `status`* (public catalogue set
`RpcTaskStatusError`):

| `error_type` | When | `error_data` | HTTP |
| --- | --- | --- | --- |
| `NoSuchTask` | the id was never issued by this family, was forgotten (R55.2.8 a), or was cancelled | integer: the requested task id | 400 |
| `Internal` | the task registry is unavailable | string | 500 |

### 55.2.3 `user_action`

**R55.2.10** *Request.* `task_id` (integer, required); `user_action` (object,
required) in the family's user-action vocabulary. The hardware-wallet
vocabulary, used by every activation family that supports a hardware policy and
by `task::init_trezor`, is `{"action_type":"TrezorPin","pin":<string>}` (the PIN
as entered on the device's scrambled matrix) or
`{"action_type":"TrezorPassphrase","passphrase":<string>}` (an empty string
selects the standard wallet). A payload outside the vocabulary answers
`InvalidRequest` (R55.1.7).

**R55.2.11** *Success.* When the task is awaiting a user action the action is
delivered and the method answers `result = "success"`, whether or not the
action is correct; a wrong PIN, for example, still answers `"success"` and its
consequence is reported by later `status` reads. An action of the wrong kind
(a passphrase while a PIN is awaited, or the reverse) is likewise delivered and
answered `"success"`; the task then terminates with a status-level error -- for
`task::init_trezor` `UnexpectedUserAction` (§55.11), for the standalone-coin
and token families `Internal`, and for the platform-with-tokens and layer-2
families the error bound by their governing chapter.

**R55.2.12** *mmrpc-level errors of `user_action`* (public catalogue set
`RpcTaskUserActionError`):

| `error_type` | When | `error_data` | HTTP |
| --- | --- | --- | --- |
| `NoSuchTask` | unknown id (as in R55.2.9) | integer: the requested task id | 400 |
| `Internal` | the task exists but is not awaiting a user action (in progress, terminal, or being cancelled), or the task stopped before it could accept the action, or the registry is unavailable | string | 500 |

### 55.2.4 `cancel`

**R55.2.13** *Request.* `task_id` (integer, required). Other keys -- in
particular a `forget_if_finished` copied from a `status` request -- are
ignored.

**R55.2.14** *Success.* When the task is in progress or awaiting a user action,
`cancel` aborts it and answers `result = "success"`. The cancelled activation
shall leave no trace: no coin, token or hardware-wallet context that the task
was creating remains registered or initialised. The standalone-coin and token
families deregister a coin the task had already registered; `task::init_trezor`
returns the hardware-wallet context to the not-initialised state; the
platform-with-tokens families register nothing before completion
([Chapter 48](48-platform-coin-task-activation.md) R48.7.3).

**R55.2.15** *mmrpc-level errors of `cancel`* (public catalogue set
`CancelRpcTaskError`):

| `error_type` | When | `error_data` | HTTP |
| --- | --- | --- | --- |
| `NoSuchTask` | unknown id: never issued by this family, already forgotten, or already cancelled | integer: the requested task id | 404 |
| `TaskFinished` | the task has a terminal result (`Ok` or `Error`) that has not been forgotten; the result stays readable by `status` | integer: the task id | 409 |
| `Internal` | the registry is unavailable, or a cancellation of the same task is still completing its cleanup | string | 500 |

*Public-docs erratum:* the cancel pages' response tables type `error_data` as a
string; every published example, and the reference release, carry an integer.
The pages also head the `TaskFinished` example as a success; it is an
mmrpc-level error.

**R55.2.16** A repeated `cancel` of a task whose cancellation already succeeded
shall answer `NoSuchTask` once the cleanup of R55.2.14 has completed. During
the short window in which that cleanup is still running the reference release
answers `Internal`; an implementation that completes cleanup before answering
the first `cancel`, and therefore answers `NoSuchTask` immediately, conforms.

### 55.2.5 Shared discriminants and vocabularies

**R55.2.17** *Hardware-wallet error vocabulary.* The discriminant `HwError`
carries as `error_data` a JSON string holding exactly one of:
`NoTrezorDeviceAvailable`, `FoundMultipleDevices`, `FoundUnexpectedDevice`,
`InvalidPin`, `UnexpectedMessage`, `ButtonExpected`, `DataError`,
`PinExpected`, `InvalidSignature`, `ProcessError`, `NotEnoughFunds`,
`NotInitialized`, `WipeCodeMismatch`, `InvalidSession`, `FirmwareError`,
`FailureMessageNotFound`, `UserCancelled`, `PongMessageMismatch` (the public
error-types catalogue's hardware RPC error list). Where `HwError` is
mmrpc-level its HTTP status is 410. A device that is absent, was disconnected,
or to which access was denied maps to `NoTrezorDeviceAvailable`; more than one
connected device maps to `FoundMultipleDevices`; a device whose public-key hash
differs from the one the caller required maps to `FoundUnexpectedDevice`. A
failure to connect to a device within the connection timeout is reported by
the family's timeout discriminant (R55.2.18), never by `HwError`.

**R55.2.18** *Timeout discriminants.* The standalone-coin, platform-with-tokens,
token and layer-2 error sets report a task timeout as `TaskTimedOut` with
`error_data = {"duration": {"secs": <integer>, "nanos": <integer>}}`;
`task::init_trezor` reports it as `Timeout` with
`error_data = {"secs": <integer>, "nanos": <integer>}` directly. These occur
at status level; their HTTP status (408, except 500 in the platform-with-tokens
set) applies only if they are ever returned at mmrpc level.

**R55.2.19** *Hardware-wallet progress vocabulary.* Every family that supports a
hardware policy uses the in-progress values `WaitingForTrezorToConnect`
(waiting for a device to be connected) and `FollowHwDeviceInstructions` (the
device asks the user to confirm on its screen), and the awaiting values
`EnterTrezorPin` and `EnterTrezorPassphrase`, answered by the user actions of
R55.2.10.

### 55.2.6 Placement summary

| Method | Success | mmrpc-level errors | Status-level errors |
| --- | --- | --- | --- |
| `init` | `{"task_id": n}`, 200 | the family's synchronous checks; `Internal` | -- |
| `status` | `{"status","details"}`, 200 | R55.2.9 | the family's error set, in `details` of an `Error` status |
| `user_action` | `"success"`, 200 | R55.2.12 | -- |
| `cancel` | `"success"`, 200 | R55.2.15 | -- |

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`:
> the cancel set of R55.2.15 (`NoSuchTask` 404, `TaskFinished` 409, `Internal`
> 500) in every family including the Lightning task, the cleanup of R55.2.14
> (a coin the task registered is deregistered; `task::init_trezor` resets the
> hardware-wallet context it owned), `task::init_trezor::cancel`, `client_id` on
> every `init`, and wire-shape tests for the status envelope and every cancel
> error. Remaining: task-status events are not streamed (D55.1); the first
> `cancel` answers before the cleanup has run, so a caller listing enabled coins
> in the following moment may still see the coin (R55.2.16 permits the repeated
> `cancel` to answer `NoSuchTask` at once).

---

## 55.3 Unreachable or unresponsive backends: cross-family summary

This section states once, for every activation family, what a caller observes
when the coin's backend cannot be reached or does not answer usefully (all
nodes or servers down, connection refused, HTTP 5xx from a node, timeouts,
malformed answers). It is a summary: the chapter named in the last column binds
the details, and where this table and that chapter differ the owning chapter is
the defect to be reported. "Status" placement means a `task::<family>::status`
answer with HTTP 200 and `status = "Error"` (R55.1.6); the HTTP column then
gives the status the discriminant would carry if it were mmrpc-level.

| Family / method | Failure point | `error_type` | `error_data` | HTTP | Placement | Owner |
| --- | --- | --- | --- | --- | --- | --- |
| legacy `electrum` (UTXO family) | no server reachable, or protocol-version negotiation fails with every server, observed when the balance is read; or the balance read fails or exceeds its bound | none (legacy); the coin is deregistered | -- (`{"error": string}`) | 500 | legacy | §55.4 |
| legacy `electrum` | unusable connection settings | none (legacy) | -- | 500 | legacy | §55.4 |
| legacy `enable` (native UTXO) | daemon unreachable when the balance is read | none (legacy) | -- | 500 | legacy | §55.4 |
| legacy `enable` / `electrum` (EVM) | no node answers the client-version probe | none (legacy) | -- | 500 | legacy | §55.4 |
| `task::enable_utxo`, `task::enable_qtum`, `task::enable_bch` | Electrum: no server reachable, or protocol-version negotiation fails with every server (Electrum connections are established in the background, so this is first observed by the block-height query) | `Transport` | string | 500 | status | §55.5 |
| same | native daemon unreachable (first observed by the block-height query) | `Transport` | string | 500 | status | §55.5 |
| same | native wallet configuration unreadable at build | `CoinCreationError` | object `{ticker, error}` | 400 | status | §55.5 |
| same | balance query (transport failure or malformed answer) | `Transport` | string | 500 | status | §55.5 |
| `enable_bch_with_tokens` | Electrum servers not responding | `PlatformCoinCreationError` | object `{ticker, error}` | 500 | mmrpc | §55.9 (public reference governs; see R55.3.2) |
| `enable_bch_with_tokens` | native daemon unreachable, or the Electrum connection lost after the platform was built (block-height or unspent-output query) | `Transport` | string | 502 | mmrpc | §55.9 |
| `enable_slp` | token balance query through the BCH backend | `Transport` | string | 500 | mmrpc | §55.10 |
| `enable_eth_with_tokens` | no node answers the client-version probe; later block or balance queries fail | `Transport` | string | 502 | mmrpc | [Ch. 35](35-evm-v2-activation-rpcs.md) |
| `task::enable_eth` | same as the one-shot call | `Transport` | string | 502 | status | Ch. 35, [Ch. 48](48-platform-coin-task-activation.md) |
| `enable_erc20` | balance or contract query through the platform node | `Transport` | string | 500 | mmrpc | Ch. 35 |
| `task::enable_erc20` | balance query | `CouldNotFetchBalance` | string | 500 | status | Ch. 35, Ch. 48 §48.11 |
| `task::enable_erc20` | other node transport failure | `Transport` | string | 500 | status | Ch. 35, Ch. 48 §48.11 |
| `enable_nft` | NFT indexer or provider unreachable | `Transport` | string | 500 | mmrpc | [Ch. 19](19-nft-module-layout.md), Ch. 35 |
| `enable_tendermint_with_assets`, `task::enable_tendermint` | block-height or balance query (building the coin contacts no node) | `PlatformCoinCreationError` | object `{ticker, error}` | 500 | mmrpc / status | [Ch. 36](36-tendermint-v2-activation-rpcs.md) |
| `enable_tendermint_token` | balance query | `CouldNotFetchBalance` | string | 500 | mmrpc | Ch. 36 |
| `task::enable_z_coin` | every configured light-wallet (lightwalletd) endpoint unreachable at activation (`LightwalletdUnreachable`) | `Transport` | string | 500 | status | [Ch. 39](39-zcash---z_coin-shielded-coin.md) R39.11.6 |
| `task::enable_z_coin` | every configured light-wallet endpoint fails admission, or a fresh wallet finds no strict-majority branch (`LightwalletdExcluded`); an empty `light_wallet_d_servers` list; the first chain-state or tree-state query to the admitted endpoints fails, before the sync starts; or the coin cannot be built for another reason | `CoinCreationError` | object `{ticker, error}` | 400 | status | Ch. 39 R39.11.5, R39.11.6 |
| `task::enable_z_coin` | light-wallet server unreachable during block-cache sync | none: the task stays `InProgress` with `TemporaryError` (string) while it retries | -- | 200 | status (in progress) | Ch. 39 |
| `task::enable_z_coin` | block-height or balance query after sync fails (the height comes from the admitted light-wallet endpoints; Electrum is not used for a shielded coin, and `electrum_servers` is accepted and ignored, R39.11.5) | `Transport` | string | 500 | status | Ch. 39 |
| `task::enable_sia` | wallet daemon unreachable at build | `CoinCreationError` | object `{ticker, error}` | 400 | status | [Ch. 46](46-sia-v2-activation-rpcs.md) |
| `task::enable_sia` | block-height or balance query after build | `Transport` | string | 500 | status | Ch. 46 |
| Solana platform activation | in this beta, every request, before any node is contacted (`SOLANA_ACTIVATION_AVAILABLE` is `false`; `error` is "Solana support is not available in this build") | `PlatformCoinCreationError` | object `{ticker, error}` | 500 | mmrpc | [Ch. 40](40-solana-coin.md) |
| Solana token activation | in this beta, every request, before any node is contacted (same switch and text) | `Internal` | string | 500 | mmrpc | Ch. 40 |
| Solana platform activation | block-height or balance read | `PlatformCoinCreationError` | object `{ticker, error}` | 500 | mmrpc | [Ch. 40](40-solana-coin.md) |
| Solana platform activation | inline token initialisation finds no healthy node | `Transport` | string | 502 | mmrpc | Ch. 40 |
| Solana token activation | no healthy node, or a node error reading the token or its balance | `Transport` | string | 500 | mmrpc | Ch. 40 |
| `task::enable_lightning` | platform-coin backend failure while starting or reading balance | `Transport` | string | 500 | status | [Ch. 41](41-lightning-network.md) |
| `task::init_trezor` | no device reachable | `HwError` | string `NoTrezorDeviceAvailable` | 410 | status | §55.11 |
| `task::init_trezor` | device not connected within the connection timeout | `Timeout` | object `{secs, nanos}` | 408 | status | §55.11 |

**R55.3.1** Each activation family shall report an unreachable or unresponsive
backend with exactly the discriminant, `error_data` shape and placement listed
above; the owning chapter binds the precise failure points.

**R55.3.2** Two rows are documented explicitly by the public reference and
therefore governed by it: the EVM platform activation with unresponsive nodes
(`Transport`, string `error_data`; the reference release instead answers
`Internal` with HTTP 500, see Chapter 35 R35.1.6); and the BCH platform
activation with unresponsive Electrum servers
(`PlatformCoinCreationError`, object `{ticker, error}`, HTTP 500). For every
other row the public reference is silent and the reference release decides.

> **Upstream divergence (informative).** The reference release contradicts the
> public reference on the BCH row. It no longer connects to Electrum servers
> while building a UTXO-family coin (connections are opened in the background),
> so for `enable_bch_with_tokens` an unresponsive Electrum server set surfaces
> only at the first block-height query, as `Transport` (string, 502). The
> published example -- `PlatformCoinCreationError` with an object -- reflects
> an earlier connect-while-building behaviour. Per the precedence rule the
> public reference governs this row; the same lazy-connection behaviour is why
> the task families above report `Transport` (the public reference is silent
> there).

> **Reloaded status (informative).** Matching on `fix/activation-api-parity`:
> the UTXO, QTUM and BCH task families, the EVM platform, the ERC20 token and the
> NFT provider (Chapters 35, 19), Tendermint platform and token (Chapter 36),
> Z-coin and Sia (`Transport` after the build; Z-coin `TemporaryError` while the
> sync runs; Chapters 39, 46), the Lightning task (Chapter 41) and the legacy
> methods (§55.4). Z-coin matches as rebuilt by the Light-mode backend work
> (Chapter 39 §39.11): lightwalletd is the sole backend, a non-empty
> `light_wallet_d_servers` list is mandatory, `electrum_servers` is accepted and
> ignored, and the backend failures are `LightwalletdUnreachable` (`Transport`)
> and `LightwalletdExcluded` (`CoinCreationError`). Remaining deviation: the
> Solana platform reports `Internal` (Chapter 40, deferred); in this beta
> Solana is switched off and every Solana request ends at the refusal rows
> above instead.

---

## 55.4 Legacy `enable` and `electrum`

**R55.4.1** *Surface.* `enable` and `electrum` are legacy (non-mmrpc) methods.
The request is a flat JSON object carrying `method`, `userpass` and the
parameters at the top level, with no `mmrpc` key and no `params` wrapper. Both
methods are routed on every target. `electrum` connects a UTXO-family coin
(UTXO, QTUM, QRC20, BCH) through Electrum servers; `enable` connects a UTXO
family coin through a local native daemon. For EVM coins and their tokens both
methods behave identically (the method name selects nothing for them).

**R55.4.2** *Common request fields.*

| Field | Type | Required / default | Effect |
| --- | --- | --- | --- |
| `coin` | string | required | ticker to activate; must be present in the coins configuration |
| `mm2` | integer 0 or 1 | required unless the coin's configuration carries it | declares swap support |
| `required_confirmations` | integer | optional; default from the coin's configuration | confirmations awaited during swaps |
| `requires_notarization` | boolean | optional | wait for dPoW notarisation (no effect for EVM coins) |
| `tx_history` | boolean | optional, default `false` | start background transaction-history collection |

**R55.4.3** *UTXO-family request fields* (`electrum` and `enable`):

| Field | Type | Required / default | Effect |
| --- | --- | --- | --- |
| `servers` | array of server objects (R55.5.10) | required for `electrum`; not used by `enable` | Electrum servers |
| `min_connected` | integer | optional, default 1 | minimum live Electrum connections |
| `max_connected` | integer | optional, default all servers | maximum live Electrum connections |
| `utxo_merge_params` | object (R55.5.10) | optional | automatic UTXO consolidation |
| `address_format` | object | optional | overrides the configured address format |
| `check_utxo_maturity` | boolean | optional | spend only mature outputs |
| `scan_policy`, `min_addresses_number`, `path_to_address` | as R55.5.10 | optional | HD-wallet parameters |
| `priv_key_policy` | as R55.5.10 | optional, default `ContextPrivKey` | validated, but the legacy methods always build the coin with the policy of the wallet context; a hardware policy is not supported on this path |
| `bchd_urls` | array of strings | required for BCH | SLP validation servers |
| `allow_slp_unsafe_conf` | boolean | optional, default `false` (BCH) | permit an empty `bchd_urls` |
| `swap_contract_address`, `fallback_swap_contract` | string | QRC20 only | swap contract addresses |

**R55.4.4** *EVM request fields* (both methods): `urls` (array of strings,
required, at least one), `swap_contract_address` (string, required, not the
zero address), `fallback_swap_contract` (string, optional, not the zero
address), `gas_station_url`, `gas_station_decimals` (integer, default 8) and
`gas_station_policy` (`{"policy": "MeanAverageFast" | "Average"}`), all
optional; and `swap_gas_fee_policy` (string, optional, one of the four values of
[Chapter 35](35-evm-v2-activation-rpcs.md) R35.6.3), which sets the coin's
initial swap gas-fee policy on this route exactly as it does on the v2 and task
routes (ch. 35 R35.6.7). Absent it, the coin's configuration entry and then the
built-in default decide, again as on the other routes. EVM activation semantics
are governed by [Chapter 35](35-evm-v2-activation-rpcs.md).

> **EVM swap gas-fee policy on this route (informative; no remaining
> divergence from the reference).** Reloaded reads both the request field and
> the coin configuration on this route, with the precedence of ch. 35 R35.6.7
> (request field, else the coin-configuration key, else the built-in `Legacy`),
> exactly as the v2 and task routes do (ch. 35 D35.6, resolved). It also parses
> the field *leniently* here: an unrecognised value is ignored in favour of the
> configured or default policy rather than rejected, where the v2 and task
> routes reject it (ch. 35 R35.1.1). This leniency is the specified behavior of
> this route and the two parses are deliberately not unified. The field belongs
> in this table because the reference accepts it here and Reloaded now does too.

> **Upstream divergence (informative).** The public reference documents the
> three gas-station fields for legacy `enable`; the reference release no longer
> reads them on this path. Because the reference is explicit, the documented
> fields govern: Reloaded accepts them and may honour them.

**R55.4.5** *Success.* HTTP 200 with a bare JSON object (no mmrpc envelope):

| Field | Type | Presence |
| --- | --- | --- |
| `result` | string `"success"` | always |
| `address` | string | always |
| `balance` | decimal string (spendable) | always |
| `unspendable_balance` | decimal string | always |
| `coin` | string (ticker) | always |
| `required_confirmations` | integer | always |
| `requires_notarization` | boolean | always |
| `mature_confirmations` | integer | only when the coin defines a coinbase maturity (UTXO family); absent otherwise |

No other field is emitted. *Public-docs erratum:* some published examples show
a `locked_by_swaps` field or omit `coin` / `unspendable_balance`; the field
table and the reference release emit exactly the fields above.

**R55.4.6** *Activation order and atomicity.* The coin is built, registered,
and then its balance is read. For `electrum` the balance read is bounded to 20
seconds; if it fails or exceeds the bound the coin shall be deregistered before
the error is returned, so a retry can succeed. For `enable` the reference
release leaves the coin registered when the balance read fails; Reloaded
shall deregister it as `electrum` does (see the code-quality finding below).

**Deliberate divergence (binding, maintainer decision 2026-10-11).** Reloaded
deregisters on a failed balance read in both methods; the reference does so in
`electrum` only. No wire field changes; a retry of a failed `enable` succeeds
instead of failing as already active. **Compatibility switch:** the `MM2.json`
key `legacy_enable_keeps_coin_on_balance_failure` (boolean, default `false`).
Set it to `true` for the reference behavior: `enable` then leaves the coin
registered when its balance read fails (`electrum` is unaffected). The switch
exists to make the reference's inconsistency reproducible; it is not
recommended.
After success, `enable` subscribes the node to the coin's transaction-helper
topic when the coin is a UTXO coin in native mode, and to the coin's watcher
topic when the node runs as a watcher ([Chapter 09](09-watcher-reward-infrastructure.md));
`electrum` subscribes to neither.

**R55.4.7** *Errors.* Every failure answers per R55.1.10 (HTTP 500,
`{"error": string}`), including: missing `coin`; coin already active; coin
absent from configuration or `mm2` unset in both request and configuration;
invalid parameter values; a backend that cannot be reached (§55.3); a failed
balance read. Coins whose protocol requires an mmrpc 2.0 activation are
rejected on this path: Tendermint and Tendermint tokens, Z-coin, NFT,
Lightning, Solana and Solana tokens. TRX and TRC20, which the reference also
rejects here, are activated by Reloaded on this path as a documented extension
(D55.5); in this beta that extension is suspended and both are rejected too
(D55.5). While a family is switched off for the beta, the rejection's `error`
text is "TRON support is not available in this build" (TRX, TRC20; build
constant `TRON_ACTIVATION_AVAILABLE`) or "Solana support is not available in
this build" (Solana, Solana tokens; `SOLANA_ACTIVATION_AVAILABLE`), and nothing
is registered.

**R55.4.8** *Sia on the legacy path.* A coin whose configured protocol is the
Sia protocol is activated synchronously by the legacy path, with the request
members, construction and failure classes bound by
[Chapter 46](46-sia-v2-activation-rpcs.md) R46.6.3, and the success body and
error body of R55.4.5 and R55.1.10.

**R55.4.9** *Concurrent legacy activation of the same ticker.* The legacy
methods check only whether the ticker is already registered when the call
starts. Two calls for the same ticker that overlap are both accepted and build
their coins in parallel; the first to register the coin proceeds to the
balance read, the other fails at registration with the legacy error of
R55.1.10 (HTTP 500). The failing call shall not deregister the coin that the
other call registered (the deregistration of R55.4.6 applies only to a coin
this call registered). No call waits for or reuses another. The same
first-registration-wins rule holds between a legacy call and a task or
one-shot activation of the same ticker (R55.5.7).

> **Code-quality finding (informative).** In the reference release the two
> legacy methods disagree on failure cleanup: `electrum` removes a coin whose
> balance read failed, `enable` leaves it registered, so a retry of `enable`
> fails as already-active although the first call reported failure. Reloaded
> currently leaves the coin registered on both paths. Proposed fix: deregister
> on balance failure in both methods (R55.4.6). This changes no wire field; it
> only makes a retry succeed.

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`:
> the 20-second bound on `electrum`'s balance read, deregistration on a failed
> read in both methods (R55.4.6) with the `legacy_enable_keeps_coin_on_balance_failure`
> switch, cleanup limited to the coin the call registered (R55.4.9), and Sia
> activation (R55.4.8). TRX and TRC20 are accepted when TRON is switched on;
> in this beta they are refused (D55.5). Remaining: the
> cleanup removes the coin by ticker, because the coin registry has no instance
> identity, so if another call disabled and re-registered the same ticker while
> this call's balance read was still pending, that other coin would be removed.
> Closing it needs an instance identity on every coin type and a
> compare-and-remove in the registry.

---

## 55.5 Standalone-coin task family: shared contract

**R55.5.1** *Members.* The standalone-coin task family comprises
`task::enable_utxo` (§55.6), `task::enable_qtum` (§55.7), `task::enable_bch`
(§55.8), `task::enable_z_coin` ([Chapter 39](39-zcash---z_coin-shielded-coin.md))
and `task::enable_sia` ([Chapter 46](46-sia-v2-activation-rpcs.md)). Each
member follows §55.2 and this section; its own section binds only the protocol
it accepts, its `activation_params`, its progress values and its failure
mapping.

**R55.5.2** *`init` request.*

| Field | Type | Required / default |
| --- | --- | --- |
| `ticker` | string | required |
| `activation_params` | object (per member) | required |
| `client_id` | integer | optional, default 0 (R55.2.2) |

**R55.5.3** *`init` synchronous checks.* Before creating the task, `init`
performs exactly these checks, in this order, and answers the first failure at
mmrpc level; no backend is contacted:

1. a coin with this ticker is already active -> `CoinIsAlreadyActivated`;
2. the ticker is absent from the coins configuration -> `CoinConfigIsNotFound`;
3. the configured protocol cannot be parsed -> `CoinProtocolParseError`;
4. the configured protocol is not the member's protocol -> `UnexpectedCoinProtocol`;
5. the task cannot be created -> `Internal`.

The list is exhaustive, so it contains no check that the coins-configuration
entry carries only recognized keys. How an entry's unrecognized keys are
treated is bound by
[Chapter 45](45-startup-configuration-and-environment-tolerance.md) R45.3.7,
which is the authority for it and carries both the derivation record and the
clearance record for it; this paragraph states no outcome of its own and binds
nothing.

**R55.5.4** *Error set* (public catalogue set `InitStandaloneCoinError`). Used
for the mmrpc-level errors of `init` and for every status-level error of every
member; each member's internal failures are mapped into this set before they
are reported.

| `error_type` | Failure class | `error_data` | HTTP |
| --- | --- | --- | --- |
| `NoSuchTask` | (reserved; task lookups use the §55.2 sets) | integer | 400 |
| `TaskTimedOut` | a task-framework or device-connection timeout | object `{duration: {secs, nanos}}` | 408 |
| `CoinIsAlreadyActivated` | coin already active at `init`, or registered concurrently by another activation when this task finishes | object `{ticker: string}` | 400 |
| `CoinConfigIsNotFound` | ticker absent from configuration | string: the ticker | 400 |
| `CoinProtocolParseError` | configured protocol unparsable | object `{ticker: string, error: string}` | 400 |
| `UnexpectedCoinProtocol` | configured protocol is not the member's | object `{ticker: string, protocol: <protocol descriptor>}` (R55.1.3) | 400 |
| `CoinCreationError` | the coin could not be built (member-specific causes) | object `{ticker: string, error: string}` | 400 |
| `HwError` | a hardware-wallet failure classified per the member's mapping | string (R55.2.17) | 410 |
| `Transport` | a backend query failed after the coin was built | string | 500 |
| `Internal` | any other failure | string | 500 |

No other discriminant is emitted by a standalone-coin member.

**R55.5.5** *Completion order and registration.* A task builds the coin, runs
any member-specific synchronisation phase, computes the success object, starts
transaction-history collection when requested, and registers the coin as its
last step. If another activation registered the same ticker in the meantime the
task ends with `CoinIsAlreadyActivated` at status level; it shall not report
success.

**R55.5.6** *Cancel.* A cancelled task deregisters the coin if it had already
registered it (R55.2.14).

**R55.5.7** *Concurrent activation of the same ticker.* No activation path
keeps a record of activations still in progress; the only duplicate check is
"is a coin with this ticker already registered". Therefore, in every
standalone-coin member (`enable_utxo`, `enable_qtum`, `enable_bch`,
`enable_z_coin`, `enable_sia`):

1. a second `init` for a ticker whose first activation is still running is
   **not** rejected: it passes the synchronous checks of R55.5.3 and answers a
   new `task_id`;
2. in the reference the two tasks run **in parallel**; neither waits for the
   other (Reloaded serializes them, R55.1.12, without changing what either
   caller observes);
3. whichever task registers the coin first succeeds; the other ends at status
   level with `CoinIsAlreadyActivated` `{ticker}` (HTTP 200 status answer; 400
   if it were mmrpc-level) -- it never reports success and never replaces the
   registered coin;
4. no member deduplicates: a second `init` never returns, joins or reuses the
   first task.

A request issued after the first activation has registered the coin is
rejected synchronously at `init` with `CoinIsAlreadyActivated` (R55.5.3). The
same rules apply across paths that activate the same ticker, for example a
task and a legacy `enable`: the first registration wins and the other path
reports its own already-active error. Both reference tags (`v2.6.0-beta`,
`v3.0.0-beta`) behave identically.

**R55.5.8** *Cancelling the losing task.* Cancelling a task shall deregister
only a coin that this task itself registered. A task that lost the race of
R55.5.7, or that has not registered anything, shall leave a coin registered by
another activation untouched when it is cancelled.

> **Code-quality finding (informative).** In the reference release the cancel
> step of the standalone-coin and token tasks removes whatever coin is
> currently registered under the task's ticker, without checking that the task
> registered it. Cancelling the second of two concurrent activations (R55.5.7)
> while it is still running therefore deregisters the coin that the first
> activation registered successfully. R55.5.8 binds the corrected behaviour;
> it changes no wire field.

> **Reloaded status (informative).** Since `fix/activation-api-parity` the
> losing standalone task ends with `CoinIsAlreadyActivated`, and a cancelled
> task removes only a coin it registered itself (R55.5.8). The serialization of
> R55.1.12 is a decided change, not yet implemented outside the Z-coin, whose
> activation already holds a per-ticker lock; elsewhere concurrent activations
> still run in parallel, with the same observable outcome.

> **Numbering (informative).** `R55.5.9` is deliberately unused. Requirements
> from `R55.5.10` onwards are cited by those identifiers from source comments
> and tests, from `docs/API_REFERENCE_DEVIATIONS.md` and from chapters 37, 39
> and 48, so the gap is kept rather than closed by renumbering.

### 55.5.1 UTXO-family members (`enable_utxo`, `enable_qtum`, `enable_bch`)

**R55.5.10** *`activation_params` common to the UTXO-family members.*

| Field | Type | Required / default | Effect |
| --- | --- | --- | --- |
| `mode` | object `{"rpc": "Native"}` or `{"rpc": "Electrum", "rpc_data": {"servers": [...], "min_connected"?: int, "max_connected"?: int}}` | required | backend selection |
| `mode.rpc_data.servers[]` | object `{url: string (required), protocol?: "TCP" \| "SSL" \| "WS" \| "WSS", disable_cert_verification?: boolean (default false), timeout_sec?: number}` | at least one for Electrum | Electrum server; `protocol` defaults to `TCP` on native targets and `WSS` on WASM |
| `utxo_merge_params` | object `{merge_at: int (required), check_every?: number (default 10), max_merge_at_once?: int (default 100)}` | optional | automatic UTXO consolidation |
| `tx_history` | boolean | optional, default `false` | start history collection on success |
| `required_confirmations` | integer | optional; default from configuration | swap confirmations |
| `requires_notarization` | boolean | optional; default from configuration | dPoW notarisation wait |
| `address_format` | object | optional | overrides the configured address format |
| `gap_limit` | integer | optional | HD gap limit ([Chapter 05](05-hd-wallet-support.md)) |
| `scan_policy` | string `do_not_scan` \| `scan_if_new_wallet` \| `scan` | optional, default `scan_if_new_wallet` | HD address scanning |
| `min_addresses_number` | integer | optional | HD minimum address count ([Chapter 38](38-utxo-coin-maintenance-and-features.md) R38.8.5) |
| `priv_key_policy` | `ContextPrivKey` or `Trezor` (R55.5.11) | optional, default `ContextPrivKey` | key source |
| `check_utxo_maturity` | boolean | optional | spend only mature outputs |
| `path_to_address` | object `{account_id: int, chain: "External" \| "Internal", address_id: int}` | optional, default account 0, `External`, address 0 | which HD address the coin uses for swaps |

The SPV configuration accepted by UTXO activation is bound by
[Chapter 37](37-utxo-spv-and-block-header-validation.md).

**R55.5.11** *`priv_key_policy` forms.* The UTXO-family members shall accept
the policy in both forms in which it reaches the wire: the bare string
(`"ContextPrivKey"`, `"Trezor"`), which is the form the reference release
accepts, and the object `{"type": "ContextPrivKey"}` / `{"type": "Trezor"}`,
which is the form the public reference's field tables and examples use. The
two forms are equivalent. The WalletConnect policy is governed by
[Chapter 22](22-walletconnect-v2.md).

> **Upstream divergence (informative).** The public reference documents only
> the object form; the reference release accepts only the string form and
> rejects the documented object. Accepting both satisfies both sources.

**R55.5.12** *Progress values.* In-progress `details` values, from this
vocabulary only:

| Value | Meaning |
| --- | --- |
| `ActivatingCoin` | initial value; coin being built or finalised |
| `SyncingBlockHeaders` with `{current_scanned_block: int, last_block: int}` | SPV header synchronisation progress (only when SPV is configured, Chapter 37) |
| `TemporaryError` with a string | a transient SPV synchronisation failure that is being retried |
| `RequestingWalletBalance` | reading balances |
| `Finishing` | finishing after a hardware-wallet connection attempt failed |
| `WaitingForTrezorToConnect` | R55.2.19 |
| `FollowHwDeviceInstructions` | R55.2.19 |

Payload-carrying values serialise as single-key objects (R55.2.7). Awaiting
values are those of R55.2.19. The initial status is `ActivatingCoin`;
`RequestingWalletBalance` is reported before the task reaches `Ok`; no other
ordering is bound.

**R55.5.13** *Success object* (`Ok` details):

| Field | Type |
| --- | --- |
| `ticker` | string |
| `current_block` | integer |
| `wallet_balance` | object, one of the two forms below |

- Single-address (Iguana) form:
  `{"wallet_type": "Iguana", "address": <string>, "balance": {<ticker>: {"spendable": <dec>, "unspendable": <dec>}}}`.
- HD form:
  `{"wallet_type": "HD", "accounts": [{"account_index": <int>, "derivation_path": <string>, "total_balance": {<ticker>: {...}}, "addresses": [{"address": <string>, "derivation_path": <string>, "chain": "External" | "Internal", "balance": {<ticker>: {...}}}]}]}`.

In both forms balances are maps keyed by the activated ticker
([Chapter 38](38-utxo-coin-maintenance-and-features.md) R38.8.5 binds the HD
account semantics).

**R55.5.14** *Failure mapping* (all status-level):

| Failure | `error_type` | `error_data` |
| --- | --- | --- |
| the coin cannot be built: native wallet configuration unreadable or RPC port unknown, fee-method detection fails, hardware policy requested but the hardware-wallet context is not initialised, the coin's configuration does not support the hardware wallet, any other non-internal build failure | `CoinCreationError` | `{ticker, error}` |
| an internal build failure, including Electrum connection settings that cannot be used (`min_connected` below 1 or above `max_connected`, unusable server entries) | `Internal` | string |
| no Electrum server can be reached, or protocol-version negotiation fails with every server: building does not wait for a connection, so the failure is first observed by the block-height query below | `Transport` | string |
| SPV synchronisation reports a permanent failure, or its progress feed ends unexpectedly | `CoinCreationError` | `{ticker, error}` |
| the block-height query after build fails | `Transport` | string |
| balance query: transport failure or malformed answer | `Transport` | string |
| balance query: internal failure | `Internal` | string |
| balance query: any other failure | `CoinCreationError` | `{ticker, error}` |
| HD address derivation fails | `CoinCreationError` | `{ticker, error}` |
| HD account creation: no device or device disconnected | `HwError` | `NoTrezorDeviceAvailable` |
| HD account creation: several devices | `HwError` | `FoundMultipleDevices` |
| HD account creation: unexpected device | `HwError` | `FoundUnexpectedDevice` |
| HD account creation: device connection timed out, or the user did not answer a PIN / passphrase request in time | `TaskTimedOut` | `{duration}` |
| HD account creation: a failure reported by the device itself (wrong PIN, user cancelled on device, firmware or session failure, and the other device-side codes of R55.2.17 except the three above) | `CoinCreationError` | `{ticker, error}` |
| hardware-wallet extended-key reader unavailable when balances are read | `HwError` | `NotInitialized` |
| registration conflict (R55.5.5) | `CoinIsAlreadyActivated` | `{ticker}` |
| history storage or wallet context unavailable; any other task-framework failure | `Internal` | string |

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`:
> the bound error set (with `HwError`, without the private-key and
> derivation-method discriminants), the success object with `ticker` and
> ticker-keyed balances, `FollowHwDeviceInstructions`, the R55.5.14 failure
> mapping (`Transport` for unreachable Electrum servers and a failed
> block-height read; device and timeout failures during HD account creation
> mapped separately), the registration conflict, `path_to_address`, per-server
> `timeout_sec`, the WASM default `WSS` protocol and both forms of
> `priv_key_policy`. Remaining: `SyncingBlockHeaders` and `TemporaryError`
> exist on the wire but are never emitted (D55.4); `path_to_address` is honored
> for a software global-HD account only, not under the Trezor policy;
> unreachable Electrum servers are detected while building the coin rather than
> lazily (the observable discriminant is the bound `Transport`).

---

## 55.6 `task::enable_utxo`

**R55.6.1** The member accepts a coin whose configured protocol is `UTXO`;
any other protocol answers `UnexpectedCoinProtocol` at `init`.

**R55.6.2** `activation_params` is exactly the object of R55.5.10. The success
object, progress values, user actions, error set and failure mapping are those
of §55.5.

**R55.6.3** The flat aliases `init_utxo`, `init_utxo_status` and
`init_utxo_user_action` inherited from the baseline are not part of the
reference surface. Reloaded keeps them as a documented extension (maintainer
decision, 2026-10-11; see the census disposition note); they shall behave
exactly as `task::enable_utxo::init`, `::status` and `::user_action`.

*Public-docs erratum:* the reference's `task::enable_utxo` page shows an Iguana
success example whose `balance` is ticker-keyed and an HD example; both agree
with R55.5.13. Its error section names `TaskTimedOut`, `CoinCreationError` and
`HwError` only; the complete set is R55.5.4.

## 55.7 `task::enable_qtum`

**R55.7.1** The member accepts a coin whose configured protocol is `QTUM`. Its
`activation_params`, success object, progress values, user actions, error set
and failure mapping are those of §55.5 and §55.6, built with the QTUM coin
builder. The flat aliases `init_qtum`, `init_qtum_status` and
`init_qtum_user_action` follow R55.6.3.

*Public-docs erratum:* the QTUM page defers its status response formats to an
overview page that contains none; §55.5 is authoritative. Its table marks
`gap_limit`, `min_addresses_number` and `scan_policy` as required; they are
optional (R55.5.10).

## 55.8 `task::enable_bch` (new)

**R55.8.1** *Routing.* `task::enable_bch::init`, `::status`, `::user_action`
and `::cancel` shall be routed on every target as a **standalone-coin** family
member (§55.5) with its own task registry. It is not a platform-with-tokens
task: it activates the BCH coin only and activates no SLP token (SLP tokens are
activated afterwards with `enable_slp`, §55.10).

**R55.8.2** *Protocol.* The member accepts a coin whose configured protocol is
`BCH` (whose configuration carries the SLP address prefix); any other protocol
answers `UnexpectedCoinProtocol` at `init`.

**R55.8.3** *`activation_params`.* The object of R55.5.10 plus:

| Field | Type | Required / default | Effect |
| --- | --- | --- | --- |
| `bchd_urls` | array of strings | required | SLP validation servers |
| `allow_slp_unsafe_conf` | boolean | optional, default `false` | permit an empty `bchd_urls` |

Other keys, including `slp_tokens_requests` and `get_balances`, are accepted
and ignored (R55.1.8).

*Public-docs erratum:* the `task::enable_bch` field table lists the activation
fields at the top level of `params`; its example and the reference release nest
them in `activation_params`, which governs. The table's duplicate `tx_history`
rows are resolved by R55.5.10 (default `false`). The deprecation banner on the
page links to the UTXO page; the method itself is current in the reference
release.

**R55.8.4** *Failure mapping.* §55.5 R55.5.14 applies, plus, at status level:
an empty `bchd_urls` without `allow_slp_unsafe_conf: true` ->
`CoinCreationError` `{ticker, error}`; a configured SLP address prefix that
cannot be parsed -> `CoinCreationError` `{ticker, error}`.

**R55.8.5** *Success.* The success object is R55.5.13, keyed by the BCH ticker,
with addresses in the coin's configured or requested address format.

> **Reloaded status (informative).** Routed on `fix/activation-api-parity`
> with its own registry, on all targets. Remaining deviation: BCH activates for
> a single address only (Reloaded's BCH coin has no HD or hardware-wallet
> paths); the `Trezor` policy ends with status-level `CoinCreationError`.

---

## 55.9 `enable_bch_with_tokens`

**R55.9.1** *Surface.* `enable_bch_with_tokens` is a one-shot mmrpc 2.0 method,
routed on every target, that activates the BCH platform coin together with a
list of SLP tokens. The public reference marks it deprecated; it remains routed
in the reference release and in Reloaded.

**R55.9.2** *Request* (`params`, flat -- no `activation_params` wrapper):

| Field | Type | Required / default | Effect |
| --- | --- | --- | --- |
| `ticker` | string | required | the BCH platform ticker |
| all fields of R55.5.10 | | as there | platform activation parameters |
| `bchd_urls` | array of strings | required | SLP validation servers |
| `allow_slp_unsafe_conf` | boolean | optional, default `false` | permit an empty `bchd_urls` |
| `slp_tokens_requests` | array of `{ticker: string (required), required_confirmations?: int, protocol?: object}` | required (may be empty) | SLP tokens to activate with the platform |
| `get_balances` | boolean | optional, default `true` | whether balances are returned |

A token's `required_confirmations` takes precedence over the confirmations in
the token's protocol configuration, which take precedence over the platform
coin's. The optional token `protocol` object follows the custom-token rules of
[Chapter 35](35-evm-v2-activation-rpcs.md) R35.5 (see D55.3). The platform is
always built with the wallet context's key policy; `priv_key_policy` is read
only to reject a hardware policy (R55.9.4).

*Public-docs erratum:* the reference marks `utxo_merge_params` required and
`requires_notarization` defaulting to `true`; the former is optional and the
latter defaults from the coin's configuration (R55.5.10) -- the reference
behavior, confirmed by maintainer decision (2026-10-11). The duplicate
`tx_history` rows resolve to default `false`.

**R55.9.3** *Success:*

| Field | Type |
| --- | --- |
| `current_block` | integer |
| `bch_addresses_infos` | map: cash address -> address info |
| `slp_addresses_infos` | map: SLP address -> address info |

An address info is `{"derivation_method": {"type": "Iguana"}, "pubkey": <hex string>, ...}` plus:

- when `get_balances` is `true`: in `bch_addresses_infos` a `balances` object
  `{"spendable": <dec>, "unspendable": <dec>}`; in `slp_addresses_infos` a
  `balances` map `{<token ticker>: {"spendable": <dec>, "unspendable": <dec>}}`;
- when `get_balances` is `false`: the `bch_addresses_infos` entry carries
  neither `balances` nor `tickers`; the `slp_addresses_infos` entry carries
  `tickers`, a JSON array of the activated SLP tickers (order not bound) and no
  `balances`.

A key that is not present is omitted, not `null`.

**R55.9.4** *Errors* (all mmrpc-level; public catalogue set
`EnablePlatformCoinWithTokensError`, whose full membership and statuses are
shared with [Chapter 35](35-evm-v2-activation-rpcs.md),
[Chapter 36](36-tendermint-v2-activation-rpcs.md) and
[Chapter 40](40-solana-coin.md); the rows below are the subset this method can
emit):

| `error_type` | Failure class | `error_data` | HTTP |
| --- | --- | --- | --- |
| `UnexpectedDeviceActivationPolicy` | `priv_key_policy` requests a hardware wallet (only task methods accept one) | absent | 400 |
| `PlatformIsAlreadyActivated` | the ticker is already active, or a concurrent activation registered it first | string: the ticker | 400 |
| `PlatformConfigIsNotFound` | ticker absent from configuration | string: the ticker | 400 |
| `CoinProtocolParseError` | platform protocol unparsable | object `{ticker, error}` | 500 |
| `UnexpectedPlatformProtocol` | platform protocol is not `BCH` | object `{ticker, protocol: <protocol descriptor>}` | 400 |
| `TokenConfigIsNotFound` | a requested token is absent from configuration | string: the token ticker | 400 |
| `TokenProtocolParseError` | a token's protocol unparsable | object `{ticker, error}` | 500 |
| `UnexpectedTokenProtocol` | a token's protocol is not an SLP token protocol | object `{ticker, protocol: <protocol descriptor>}` | 400 |
| `PlatformCoinMismatch` | a requested token belongs to a different platform | absent | 400 |
| `CustomTokenError` | a custom-token request is rejected (Chapter 35) | as bound in Chapter 35 | 400 |
| `PlatformCoinCreationError` | the platform cannot be built: Electrum servers not responding (public reference governs, R55.3.2; the implementation shall detect this before reporting success), native configuration unreadable, empty `bchd_urls` without `allow_slp_unsafe_conf: true`, any other builder failure | object `{ticker, error}` | 500 |
| `UnexpectedDerivationMethod` | the platform is in HD mode (this method supports a single address only) | string | 500 |
| `Transport` | the block-height or unspent-output query fails after build | string | 502 |
| `Internal` | the configured SLP prefix cannot be parsed; the SLP address cannot be encoded; history storage or wallet context unavailable | string | 500 |

**R55.9.5** *Side effects.* On success the platform and its tokens are
registered together, each token's information is recorded on the platform, and
transaction history starts when `tx_history` is `true`. On failure nothing is
registered.

**R55.9.6** *Concurrent calls for the same platform ticker.* A second
`enable_bch_with_tokens` for a ticker whose first call is still running is not
rejected; both calls build the platform in parallel without waiting for each
other, the first to register wins, and the other answers mmrpc-level
`PlatformIsAlreadyActivated` (string: the ticker, HTTP 400) without
registering anything. No call is deduplicated or joined. A call made after the
platform was registered is rejected at once with the same discriminant.

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`:
> `get_balances` with `tickers`, the token-to-platform mismatch check, the
> token `protocol` field (custom SLP tokens to the extent D55.3 binds them), and
> the rejection of a hardware `priv_key_policy` with
> `UnexpectedDeviceActivationPolicy`. The Electrum-down row stays
> `PlatformCoinCreationError`, as the public docs show.

---

## 55.10 `enable_slp`

**R55.10.1** *Surface.* `enable_slp` is a one-shot mmrpc 2.0 method, routed on
every target, that activates one SLP token on an already active BCH platform
coin.

**R55.10.2** *Request:*

| Field | Type | Required / default |
| --- | --- | --- |
| `ticker` | string | required |
| `activation_params` | object `{required_confirmations?: int}` | required (may be `{}`) |
| `protocol` | object | optional; custom-token request (D55.3) |

`required_confirmations` precedence is as in R55.9.2.

**R55.10.3** *Success:*

| Field | Type |
| --- | --- |
| `balances` | map `{<SLP address>: {"spendable": <dec>, "unspendable": <dec>}}` |
| `token_id` | string: the token id, 64 hexadecimal characters |
| `platform_coin` | string: the platform ticker |
| `required_confirmations` | integer: the effective value |

**R55.10.4** *Errors* (mmrpc-level; public catalogue set `EnableTokenError`,
shared by every one-shot token method -- `enable_erc20` and `enable_nft`
(Chapter 35 / Chapter 19), `enable_tendermint_token` (Chapter 36), the Solana
token method (Chapter 40)). Full set with HTTP statuses:

| `error_type` | `error_data` | HTTP | Emitted by `enable_slp` when |
| --- | --- | --- | --- |
| `TokenIsAlreadyActivated` | string: the token ticker | 400 | the ticker is already active, or a concurrent activation registered it first |
| `TokenConfigIsNotFound` | string: the token ticker | 400 | ticker absent from configuration |
| `TokenProtocolParseError` | object `{ticker, error}` | 500 | token protocol unparsable |
| `UnexpectedTokenProtocol` | object `{ticker, protocol: <protocol descriptor>}` | 400 | protocol is not an SLP token protocol |
| `PlatformCoinIsNotActivated` | string: the **platform** ticker | 400 | the BCH platform is not active |
| `UnsupportedPlatformCoin` | object `{platform_coin_ticker, token_ticker}` | 500 | the configured platform is active but is not a BCH coin |
| `PlatformCoinMismatch` | absent | 400 | the token's platform does not match |
| `CustomTokenError` | as bound in Chapter 35 | 400 | a custom-token request is rejected |
| `Transport` | string | 500 | the token balance query fails in transport or returns a malformed answer |
| `Internal` | string | 500 | any other failure (balance internal failure, address derivation, context) |
| `UnexpectedDerivationMethod` | a derivation-method error name as a string, or a single-key object for a name that carries text (Chapter 35) | 500 | not emitted by `enable_slp` |
| `CouldNotFetchBalance` | string | 500 | not emitted by `enable_slp` |
| `InvalidConfig` | string | 500 | not emitted by `enable_slp` |
| `InvalidPayload` | string | 400 | not emitted by `enable_slp` |
| `PrivKeyPolicyNotAllowed` | string | 500 | not emitted by `enable_slp` |

**R55.10.5** *Side effects.* On success the token is registered as an active
coin and its information is recorded on the BCH platform coin, so that the
platform's SLP-aware operations (balances, history, swaps) know the token
exactly as they would after `enable_bch_with_tokens`.

**R55.10.6** *Concurrent calls for the same token ticker.* A second
`enable_slp` for a token whose first call is still running is not rejected;
both run in parallel, the first to register the token wins, and the other
answers mmrpc-level `TokenIsAlreadyActivated` (string: the token ticker, HTTP
400) without registering anything or recording token information on the
platform. No call is deduplicated or joined.

> **Reloaded status (informative).** Request (without `protocol`) and success
> shapes match, the shared token error set has every bound discriminant, and
> since `fix/activation-api-parity` the token's information is recorded on the
> platform coin after registration. Remaining: the top-level `protocol` of
> `enable_slp` (custom SLP tokens, D55.3).

---

## 55.11 `task::init_trezor` (including the new `cancel`)

**R55.11.1** *Surface.* `task::init_trezor::init`, `::status`, `::user_action`
and `::cancel` initialise the node's hardware-wallet context with a Trezor
device, on every target. A successful task leaves the context initialised for
the rest of the session; activations with a `Trezor` key policy require it.
The baseline flat aliases `init_trezor`, `init_trezor_status` and
`init_trezor_user_action` follow R55.6.3.

**R55.11.2** *`init` request.* `params` may be absent, `null`, or an object
with these optional keys:

| Field | Type | Effect |
| --- | --- | --- |
| `device_pubkey` | string, 40 hexadecimal characters (a 20-byte public-key hash, no prefix) | when present, the connected device must match it |
| `client_id` | integer | R55.2.2 |

All three forms of an empty request -- no `params`, `"params": null`,
`"params": {}` -- shall be accepted. A malformed `device_pubkey` answers
`InvalidRequest`.

**R55.11.3** *`init` result and errors.* Success answers `{"task_id": n}`. The
only mmrpc-level `init` error is `Internal` (string, 500), when the task cannot
be created; a concurrent initialisation is reported at status level
(R55.11.6).

**R55.11.4** *Progress values.* In-progress: `Initializing` (initial value, and
again after the device connects), `WaitingForTrezorToConnect`,
`FollowHwDeviceInstructions`. Awaiting: `EnterTrezorPin`,
`EnterTrezorPassphrase`. The device-connection wait is bounded to 300 seconds;
each PIN or passphrase wait is bounded to 600 seconds.

**R55.11.5** *Success object* (`Ok` details), flat:

| Field | Type |
| --- | --- |
| `type` | string `"Trezor"` |
| `model` | string or `null` |
| `device_name` | string or `null` |
| `device_id` | string or `null` |
| `device_pubkey` | string, 40 hexadecimal characters |

*Public-docs erratum:* the status page's example wraps these fields in a
`result` object inside `details`; its field table and the reference release
place them directly in `details`.

**R55.11.6** *Error set* (status-level; public catalogue set `InitHwError`):

| `error_type` | Failure class | `error_data` | HTTP |
| --- | --- | --- | --- |
| `HwContextInitializingAlready` | another hardware-wallet initialisation is still running | absent | 400 |
| `HwError` | device failure, mapped per R55.2.17 -- including `FoundUnexpectedDevice` when the device does not match `device_pubkey` | string | 410 |
| `UnexpectedUserAction` | the user action delivered is of the wrong kind (R55.2.11) | object `{expected: string}` holding the expected `action_type` name (`TrezorPin` or `TrezorPassphrase`) | 400 |
| `Timeout` | device not connected within 300 s, or a PIN / passphrase not supplied within 600 s | object `{secs, nanos}` | 408 |
| `Internal` | any other failure, including the task being stopped while waiting for a user action | string | 500 |

No other discriminant is emitted. A failed task leaves the context
not-initialised.

**R55.11.7** *`status`, `user_action`, `cancel`.* As §55.2, with the
hardware-wallet user-action vocabulary of R55.2.10. `cancel` (**new**) is
routed and, on success, returns the hardware-wallet context to the
not-initialised state (R55.2.14); its errors are those of R55.2.15.

**R55.11.8** *Activations that need the context.* A standalone-coin activation
with `priv_key_policy` `Trezor` started before the context is initialised fails
at status level with `CoinCreationError` `{ticker, error}` (R55.5.14); the
public reference shows this case.

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`:
> every request form, the device-object `Ok` details, the error set with
> `HwError` and `UnexpectedUserAction`, the progress vocabulary, and `cancel`
> with the context reset. Remaining: the `device_pubkey` check runs after the
> device is initialised, so the context is briefly ready for the wrong device
> before the task resets it and reports `FoundUnexpectedDevice`; `device_name`
> is taken from the device label.

---

## 55.12 Tests

Each test below names a wire-shape unit or integration test the implementation
must have. Every error test asserts the serialised `error_type`, the JSON type
of `error_data` (and its field names for objects, or its absence) and the HTTP
status.

**T55.1** For each task family, `cancel` of a task that finished (both `Ok` and
`Error`) answers `TaskFinished`, `error_data` the integer task id, HTTP 409;
a subsequent `status` with `forget_if_finished: false` still returns the
terminal result.

**T55.2** For each task family, including `task::enable_lightning` and
`task::init_trezor`, `cancel` of an unknown id answers `NoSuchTask`, integer,
HTTP 404; `status` of an unknown id answers `NoSuchTask`, integer, HTTP 400;
`user_action` of an unknown id answers `NoSuchTask`, integer, HTTP 400.

**T55.3** A task id issued by one family is `NoSuchTask` for every other
family's `status` and `cancel`.

**T55.4** A status-level error serialises as `status = "Error"` with
`details` holding `error`, `error_path`, `error_trace`, `error_type` and
`error_data` directly (no `result` key), HTTP 200; a payload-less discriminant
omits the `error_data` key.

**T55.5** A payload-carrying in-progress value serialises as a single-key
object (`SyncingBlockHeaders` with an object, `TemporaryError` with a string).

**T55.6** `forget_if_finished` semantics of R55.2.8 (a)–(d), including that a
cancelled task answers `NoSuchTask` to `status`.

**T55.7** Every `init` accepts a top-level `client_id`; `task::init_trezor::init`
accepts no `params`, `null`, `{}`, and `{"device_pubkey": <40 hex>}`.

**T55.8** `task::init_trezor` success details serialise as the flat object of
R55.11.5; each discriminant of R55.11.6 serialises with its bound `error_data`
shape and HTTP status (`HwContextInitializingAlready` without `error_data`,
400; `HwError` string, 410; `UnexpectedUserAction` object with `expected`, 400;
`Timeout` `{secs, nanos}`, 408; `Internal` string, 500).

**T55.9** `task::init_trezor::cancel` is routed; cancelling an in-progress
initialisation answers `"success"` and leaves the hardware-wallet context
not-initialised.

**T55.10** The standalone-coin error set serialises exactly the discriminants
of R55.5.4 with their `error_data` shapes and HTTP statuses, including `HwError`
(string, 410) and `TaskTimedOut` (`{duration: {secs, nanos}}`, 408).

**T55.11** A UTXO-family success object contains `ticker`, and its Iguana
`balance` is a map keyed by the ticker.

**T55.12** The UTXO-family progress vocabulary serialises exactly the values of
R55.5.12, including `FollowHwDeviceInstructions`.

**T55.13** UTXO-family failure mapping: a block-height failure after build
yields `Transport` (string); a registration conflict yields
`CoinIsAlreadyActivated` `{ticker}` and not success; unreachable Electrum
servers yield `Transport` (string); an unreadable native configuration yields
`CoinCreationError` `{ticker, error}`.

**T55.14** `priv_key_policy` is accepted as `"Trezor"` and as
`{"type": "Trezor"}`, with identical effect, and likewise for
`ContextPrivKey`.

**T55.15** `task::enable_bch::{init,status,user_action,cancel}` are routed;
`init` reads its parameters from `activation_params`; an empty `bchd_urls`
without `allow_slp_unsafe_conf` ends with status-level `CoinCreationError`;
the success object is R55.5.13.

**T55.16** `enable_bch_with_tokens` with `get_balances: false` returns the
shapes of R55.9.3 (`tickers` array on SLP entries, neither key on BCH entries);
a `Trezor` policy answers `UnexpectedDeviceActivationPolicy` without
`error_data`, 400; a token of another platform answers `PlatformCoinMismatch`
without `error_data`, 400; an unreachable native daemon answers `Transport`
(string, 502); unreachable Electrum servers answer `PlatformCoinCreationError`
(object, 500).

**T55.17** After `enable_slp` succeeds, the BCH platform reports the token among
its known SLP tokens; `enable_slp` errors serialise per R55.10.4.

**T55.18** Legacy `electrum`: when the balance read fails, the coin is not
active afterwards and a retry can succeed; legacy `enable` likewise; every
legacy error answers HTTP 500 with only an `error` string.

**T55.19** Legacy success bodies carry exactly the fields of R55.4.5 and no
`locked_by_swaps`; `mature_confirmations` appears only for UTXO-family coins.

**T55.20** `UnexpectedCoinProtocol` and `UnexpectedPlatformProtocol` carry a
`protocol` object with a string `type`.

## 55.13 Deferred Work

**D55.1** *Task-status event streaming.* The reference release pushes every
task status change to the event-stream client named by `client_id`. Reloaded
accepts `client_id` (R55.2.2) but streams nothing. The streaming contract
belongs with [Chapter 10](10-sse-streaming.md) and is deferred to it.

**D55.2** *(Withdrawn.)* Sia activation through the legacy methods is bound by
[Chapter 46](46-sia-v2-activation-rpcs.md) R46.6.3 and referenced from
R55.4.8; nothing is deferred.

**D55.3** *Custom SLP tokens.* The reference release lets a token request carry
a `protocol` object that activates a token absent from the coins configuration
(rejecting it when the ticker is configured). The shared custom-token rules are
bound for EVM by Chapter 35 R35.5; their application to SLP tokens
(`enable_bch_with_tokens`, `enable_slp`) is deferred until Chapter 35's
custom-token surface is implemented.

**D55.4** *SPV progress values.* `SyncingBlockHeaders` and `TemporaryError`
(R55.5.12) are emitted only when SPV block-header synchronisation is configured
([Chapter 37](37-utxo-spv-and-block-header-validation.md)); their emission
waits on Chapter 37's progress surface.

**D55.5** *TRX and TRC20 through the legacy methods -- closed (maintainer
decision, 2026-10-11).* Reloaded activates TRX and TRC20 through legacy
`enable`/`electrum` ([Chapter 21](21-tron-integration.md)); the reference release
rejects both there and requires an mmrpc 2.0 activation. Reloaded has no mmrpc
2.0 TRON activation, so rejecting them here would remove TRON activation
entirely. Reloaded keeps them on the legacy path as a documented extension: a
client written for the reference never sends such a request, so it is
unaffected. Adding the mmrpc 2.0 TRON activation the reference uses is future
work (the RPC wire-parity plan).

> **Suspended for the beta (maintainer decision, 2026-10-26).** TRON support
> is not finished, so for the coming beta the extension is switched off: while
> the build constant `TRON_ACTIVATION_AVAILABLE` (`coins/lp_coins_ops.rs`) is
> `false`, legacy `enable` and `electrum` refuse TRX and TRC20 with "TRON
> support is not available in this build" (R55.1.10, HTTP 500) and register
> nothing, so in this beta Reloaded activates TRON on no path. The decision
> above stands for when TRON returns, planned for the next beta.

**D55.6** *Re-activation of a passive platform coin.* The reference release
treats a platform coin that was disabled while dependent tokens stayed active
(a passive coin) as re-activatable by the platform activation methods
instead of answering `PlatformIsAlreadyActivated`. Whether Reloaded has passive
coins at all is a [Chapter 43](43-daemon-rpc-dispatcher-and-misc-maintenance.md)
question (`disable_coin`); the activation side is deferred to it.

## 55.14 Baseline Verifications

**V55.1** The pinned baseline (Chapter 02) contains the task framework with
`init`, `status` and `user_action` for the UTXO, QTUM and Z-coin families and
for Trezor initialisation, under flat method names only; the `status` and
`user_action` error sets and their HTTP statuses already match R55.2.9 and
R55.2.12. It has no `task::` prefix, no `cancel` method in any family, no
`task::enable_bch`, and no token task substrate.

**V55.2** The baseline contains `enable_bch_with_tokens` and `enable_slp` with
the request and success shapes of R55.9.2–R55.9.3 except `get_balances`, the
`tickers` field and the token `protocol` field.

**V55.3** The baseline's Trezor initialisation request is a payload-less
structure and its success value is the bare string `"success"`; R55.11.2 and
R55.11.5 are therefore post-baseline requirements.

**V55.4** The baseline's legacy `enable` and `electrum` emit the success body of
R55.4.5 and the error body of R55.1.10.

## 55.15 External References

- Public Komodo DeFi Framework API reference, mmrpc 2.0 protocol page (request,
  success and error envelope).
- Public reference, legacy coin-activation page (`enable`, `electrum`).
- Public reference, task-managed coin-activation pages (`task::enable_utxo`,
  `task::enable_qtum`, `task::enable_bch`, and the overview).
- Public reference, `enable_bch_with_tokens` page.
- Public reference, `task::init_trezor` pages (`init`, `status`,
  `user_action`, `cancel`).
- Public reference, common-structures pages (activation, wallet, enums) and the
  error-types catalogue (the sets `RpcTaskStatusError`,
  `RpcTaskUserActionError`, `CancelRpcTaskError`, `InitStandaloneCoinError`,
  `EnablePlatformCoinWithTokensError`, `EnableTokenError`, `InitHwError`, and
  the hardware RPC error list).
- Electrum protocol (server version negotiation), as the external dictate for
  the Electrum connection failure classes.
- Trezor device protocol (PIN matrix, passphrase, device features), as
  referenced by [Chapter 05](05-hd-wallet-support.md) and
  [Chapter 50](50-evm-trezor-signing.md).

## 55.16 Provenance Footer

- *Inputs:* the public Komodo DeFi Framework API reference pages listed in
  §55.15; Chapter 01 (rules and canonical shape); Chapter 02 (pinned baseline,
  for §55.14); Chapters 05, 09, 10, 19, 21, 22, 35, 36, 37, 38, 39, 40, 41, 43,
  46, 47, 48 and 50 (cross-chapter contracts); the present Reloaded workspace (for
  the *Reloaded status* notes).
- *Permitted-input classes used:* R1 (baseline source, for §55.14); R3/R4
  (public API reference and dictated wire formats); R7 (independent work).
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* consulted by the KDF Spec Reader, under the chapter-01
  two-team clean-room workflow, at the GLEEC KDF release tags `v3.0.0-beta`
  (reference) and `v2.6.0-beta` (legacy comparison), only to decide what the
  public reference leaves silent, ambiguous or contradictory: HTTP statuses;
  `error_data` shapes; the failure-to-discriminant mappings of §55.3, §55.5,
  §55.8–§55.11; the placement of errors; the task-framework lifecycle of §55.2;
  the request forms and defaults where the reference contradicts itself. No
  source text, private identifier, helper structure, log or error string, or
  per-method internal table was copied; every rule is stated as an externally
  observable wire or behavioural requirement. A later pass on 2026-10-16, at the
  same two reference tags, added the EVM `swap_gas_fee_policy` request field to
  R55.4.4 and the divergence note after it (the field's acceptance on this
  route, its precedence relative to the coin configuration, and the lenient
  parsing of an unrecognised value here). On 2026-10-16, after that pass, the
  divergence note after R55.4.4 was rewritten by a context that never read the
  forbidden corpus to record that Reloaded now implements the behavior the
  earlier note described as missing (ch. 35 D35.6); it states Reloaded's own
  decided behavior and adds no reference content, and the clearance recorded
  below does not cover that rewrite. The chapter as a whole, that addition
  included, **passed the KDF Dirty Gate** on 2026-10-16 (PASS, 0.02, no
  findings); [Chapter 35](35-evm-v2-activation-rpcs.md)'s record of the same
  Spec Reader pass does not cover this chapter, which carries its own record:
  - *Extent.* The whole subject matter this chapter claims: the mmrpc error
    envelope and its HTTP-status policy, the four shared task-framework
    methods, the standalone-coin task family and its UTXO-family members, the
    BCH platform-with-tokens and SLP token one-shot methods, the hardware-wallet
    initialisation task, and -- the pass's new material -- the legacy EVM
    activation request and the route by which an initial swap gas-fee policy
    reaches the coin, with its precedence against configuration and the
    strictness of value parsing there compared with the v2 and task routes.
    Reference: `v2.6.0-beta` and `v3.0.0-beta`; the pinned baseline was also
    checked, to settle which of the chapter's named error-set identifiers are
    baseline-era (six of seven; the seventh is post-anchor and is a public-API
    type sourced to the public error-types catalogue). Quantity: about twenty
    files across the activation, task-framework, hardware-wallet and EVM coin
    areas, plus the two sweeps below over four subsystem areas.
  - *Method.* By reading, combined with mechanical comparison; both sweeps ran.
    One extracted every backtick-delimited or quoted identifier from the
    chapter (about 240) and put each match individually to the forced-versus-free
    test. The other extracted every string literal of 18 or more characters from
    the four areas, normalised placeholders and matched them against the
    chapter's full text; it returned only wire field names and no error or log
    wording. `tools/clean_room_gate.py` was not applicable: the chapter embeds
    no fenced code block.
  - *Measure.* 0.02 on the discretionary-expression basis, after excluding
    dictated-interop and public or serialized interface content, and read
    against the rule that one instance of genuine discretionary expression
    fails the chapter whatever the figure. No such instance was found.
- *A later addition, and why it needs no clearance of its own (chapter 01 T1,
  R40, §1.7).* The paragraph that follows R55.5.3's check list was added after
  the pass recorded above and is **not** covered by it. As first drafted it
  stated, in its own words, a positive outcome for an activation request whose
  coins-configuration entry carries an unrecognized key, which is substantive
  content however accurately it deferred authority elsewhere; a paragraph that
  states an outcome is not a pointer in form, whatever it says about itself.
  It has accordingly been **reduced to a genuine cross-reference**: it now
  states only that the check list above it is exhaustive, which is a statement
  about this chapter's own text, and it hands the treatment of such a key to
  [Chapter 45](45-startup-configuration-and-environment-tolerance.md) R45.3.7,
  which is the authority for it and carries both the derivation record and the
  completed clearance for it. The paragraph therefore asserts no behavior of
  its own and opens no new forbidden-class-sourced surface in this chapter, so
  the clearance item above is left exactly as it stood -- not re-dated, not
  widened, not reopened -- and no section of this chapter carries an inline
  clearance pointer for the addition: a cross-reference that binds nothing
  gives an implementer nothing to implement from, and the at-the-point notice
  R40 asks for sits where the rule is, at Chapter 45 R45.3.7.

  The route is recorded even so, because the record is what makes that
  position checkable rather than merely asserted. The paragraph as first
  drafted was authored by the KDF Spec Reader under the two-team procedure of
  chapter 01 §1.4A and **passed the KDF Dirty Gate** on 2026-10-24: PASS,
  discretionary-expression measure 0.00, finding list empty, and accordingly
  no remedy class for any line or for the candidate as a whole. The reduction
  removes material rather than rewording it, so it can only narrow what that
  pass cleared. This item is written on the clean side from that verdict,
  which reached the clean side as relayed text rather than as a gate-written
  file; that is record-keeping and adds no chapter content. The three elements
  of the pass's R39 scope attestation, in corpus-free terms as chapter 01 T1
  requires, with the limits the gate stated left visible:

  - *Extent.* The same coin-registry-entry tolerance surface compared for
    Chapter 45 R45.3.7, in its bearing on the synchronous pre-task checks of a
    standalone-coin activation request. Quantity examined: no file beyond
    those compared for that rule. Reference designation: the publicly
    published release designation `v3.0.0-beta`, and **the record is no more
    precise than that designation** -- no post-anchor commit identifier, no
    branch state and no location within the compared material is given
    (chapter 01 R11 and the reference-version limit of §1.7). **Two things
    this element does not attest, stated rather than papered over.** First,
    the enumeration of synchronous checks that the addition characterizes was
    **not** re-compared in this pass: that enumeration is pre-existing
    material resting on the clearance recorded above, and this pass neither
    reopens nor re-confirms it. Second, the pass reached material at the
    `v3.0.0-beta` designation only, so it attests nothing about the legacy
    designation the earlier record names beside it; that designation's half of
    the earlier record continues to rest on the earlier record alone, and
    nothing here supports or contradicts it.

  - *Method.* By **reading**. No token on an added line was quoted material,
    so no mechanical token comparison was applicable. **This repository's own
    clean-room gate tooling under `tools/` was not applicable and was not
    run:** it operates over source-file bodies and the addition embeds no code
    block, so the instrument had no operand. The absence is stated so that it
    is visible rather than implied.

  - *Measure.* **0.00** on the discretionary-expression basis -- the same
    figure the leak report carries, not a second one -- read against the same
    standard the pass above was read against: one item of genuine
    discretionary expression fails a candidate whatever the figure. The
    addition quoted no identifier and reproduced nothing, and the single
    behavioral statement it then carried was a restatement of material gated
    with Chapter 45 R45.3.7 -- the statement the reduction has since removed.
