# Chapter 41 -- Lightning Network

**Status:** driving-spec (as-built; documents shipped reloaded behaviour).
**Native target only.**

> **One-sentence claim:** the project shall support Bitcoin Lightning as a
> native-only Layer-2 coin built on a BOLT-conformant Lightning implementation,
> activating a node through a long-running task RPC, managing peers and channels,
> issuing and paying BOLT-11 invoices, and participating in atomic swaps whose
> on-chain HTLC timelock semantics align with the swap's lock-duration contract.

> **Treatment:** **T-DOC.** Reloaded ships a Lightning coin (`LightningCoin`)
> with a complete node/channel/payment RPC surface, gated to the native target.
> This chapter documents that shipped capability, except §41.2 (node
> activation), which is **T-PORT**: it binds the activation wire contract of the
> reference named in R41.2.0 and lists where reloaded still deviates.

## 41.0 Executive Summary

Lightning is integrated as a Layer-2 coin layered on a parent UTXO coin (BTC-
family). A node is brought up by a task-based activation, after which the wallet
can connect to peers, open and close channels, and route BOLT-11 payments. The
underlying protocol and node engine (the LDK-style Lightning library and the BOLT
specifications) are externally dictated; this chapter binds the project's public
surface and the swap-relevant timelock contract, not the engine internals.

> **Binding scope (R36).** Requirements bind observable behaviour, the public RPC
> method strings and their request/response field names, and externally
> *dictated* interop: the **BOLT** specifications (BOLT-11 invoices, channel and
> HTLC semantics) and the underlying Lightning library's network behaviour. The
> Lightning library's internal API and version are an implementation dependency
> (§41.6), not a wire contract. Private types and helper structure are
> informative.

## 41.1 Platform guard

R41.1.1 The Lightning coin shall be compiled and exposed on the **native** target
only. It shall be absent from the WASM build.

## 41.2 Node activation (task RPC)

> **Source of truth (informative).** The method strings, request fields, result
> fields, in-progress values and the init error examples of this section follow
> the public Komodo DeFi Framework "Lightning Network Initialization Tasks" page
> and the `LightningActivationParams` / error-type catalogue entries. Where they
> are silent or contradict themselves (including every HTTP status), this
> section binds the reference implementation of R41.2.0, by maintainer decision.
> The mmrpc-2.0 envelope, the task-status object and the task-framework errors of
> `status` / `user_action` / `cancel` (`NoSuchTask`, `TaskFinished`, ...) are
> bound once by [Chapter 55](55-coin-activation-wire-contract.md) §55.1-§55.2
> and are not restated here.

R41.2.0 **Reference version.** The reference implementation is the GLEEC KDF
release tag **`v3.0.0-beta`**; the netid-8762 legacy reference `v2.6.0-beta`
exposes an identical Lightning activation surface (methods, fields, errors,
statuses), so no reference-version distinction applies.

R41.2.1 **Methods.** Lightning node activation shall be the native-only
task-managed family `task::enable_lightning::{init,status,user_action,cancel}`
(the shared task-lifecycle substrate is described in
[Chapter 48](48-platform-coin-task-activation.md)). The reference routes **no**
one-shot `enable_lightning` method and **no** flat `init_lightning` /
`init_lightning_status` / `init_lightning_user_action` / `cancel_init_lightning`
aliases; neither form is part of the parity contract (see R41.2.2).

R41.2.2 **Reloaded-only activation methods (informative).** Reloaded also routes
the one-shot `enable_lightning` (inherited from the GPLv2 baseline) and the flat
`init_lightning` quartet (a reloaded addition that gives Lightning the same flat
`init_<coin>*` names as the other task families) as aliases of the same
activation. They are not parity surfaces. By maintainer decision
(2026-10-11) they stay routed as documented Reloaded extensions (backward
compatibility with baseline-era clients, uniform naming); see the census
disposition note. They shall not be described to clients as KDF-compatible
methods.

R41.2.3 Activation shall bind the Lightning coin to its parent UTXO coin, load or
create the node's persistent state, and start the background node event loop.

### `task::enable_lightning::init`

R41.2.4 **Request** (`params`):

| Field | JSON type | Required / default | Effect |
| --- | --- | --- | --- |
| `ticker` | string | **required** | the Lightning coin ticker (a `LIGHTNING`-protocol coins-configuration entry) |
| `activation_params` | object | **required** | node parameters below; unknown members ignored |
| `activation_params.name` | string | **required** | node alias announced to the network; at most 32 bytes (UTF-8 length), space-padded to 32 bytes on the wire |
| `activation_params.listening_port` | integer (0-65535) | optional, default `9735` | P2P listening port; a non-integer (e.g. a string) is a request-parse failure (`InvalidRequest`, ch. 55 §55.1) |
| `activation_params.color` | string | optional, default `"2b6680"` | node RGB colour as exactly 6 hex digits |
| `activation_params.payment_retries` | integer | optional, default `5` | payment retry attempts |
| `activation_params.backup_path` | string | optional | directory for channel backups |

> **Upstream divergence (informative).** The public `LightningActivationParams`
> table states a colour default of `2b6680`; the reference defaults to
> `000000`. By maintainer decision (2026-10-11) the documented `2b6680` is
> bound, because the docs are explicit and the value is cosmetic (the node's
> colour in the network graph). The docs page also carries a second
> `LightningActivationParams` definition (`backup_password`, `node_pubkey`) that
> is not part of the activation request; that point is resolved in favour of
> the reference.

R41.2.5 **Success:** `{"task_id": <integer>}` (HTTP 200).

R41.2.6 **`init` errors** (mmrpc-level; synchronous, before the task is
spawned). The observable precedence is the row order:

| # | `error_type` | Returned when | `error_data` | HTTP |
| --- | --- | --- | --- | --- |
| 1 | `L2IsAlreadyActivated` | a coin with `ticker` is already active | string: the Lightning ticker | 400 |
| 2 | `L2ConfigIsNotFound` | no coins-configuration entry for `ticker` | string: the Lightning ticker | 400 |
| 3 | `L2ProtocolParseError` | the entry's `protocol` does not parse | `{ticker: string, error: string}` | 500 |
| 4 | `UnexpectedL2Protocol` | the protocol is not `LIGHTNING` | `{ticker: string, protocol: object}` (protocol in canonical coin-configuration form, ch. 36 R36.0.3) | 400 |
| 5 | `L2ConfigParseError` | the rest of the Lightning coin entry does not parse (R41.2.11) | string | 500 |
| 6 | `PlatformCoinIsNotActivated` | the `platform` named by the protocol is not active | string: the **Lightning** ticker (as in the public-docs example), not the platform ticker | 400 |
| 7 | `UnsupportedPlatformCoin` | the platform is active but is not a standard UTXO coin | `{platform_coin_ticker: string, l2_ticker: string}` | 500 |
| 8 | `Internal` | the platform is not activated in segwit address format | string | 500 |
| 9 | `InvalidPlatformConfiguration` | the platform's coin configuration lacks `avg_blocktime` | `{platform_coin_ticker: string, err: string}` | 500 |
| 10 | `Internal` | `name` longer than 32 bytes, or `color` not 6 hex digits | string | 500 |
| 11 | `Internal` | the task could not be created | string | 500 |

The full set of `error_type` values that `init` and the task status can carry is:
`L2IsAlreadyActivated`, `L2ConfigIsNotFound`, `L2ProtocolParseError`,
`UnexpectedL2Protocol`, `PlatformCoinIsNotActivated`, `UnsupportedPlatformCoin`,
`InvalidPlatformConfiguration`, `L2ConfigParseError`, `TaskTimedOut` (object
`{duration: {secs: integer, nanos: integer}}`, HTTP 408), `Transport` (string,
500), `Internal` (string, 500). HTTP statuses: 400 for
`L2IsAlreadyActivated`, `PlatformCoinIsNotActivated`, `L2ConfigIsNotFound`,
`UnexpectedL2Protocol`; 408 for `TaskTimedOut`; 500 for all others.

### `task::enable_lightning::status`

R41.2.7 **Request:** `{task_id: integer, forget_if_finished: boolean (default
true)}`. Responses are HTTP 200 task-status objects (ch. 55 §55.2):

- **InProgress** -- `details` is one of the strings `ActivatingCoin` (initial),
  `GettingFeesFromRPC`, `ReadingNetworkGraphFromFile`,
  `InitializingChannelManager`, `InitializingPeerManager`,
  `ReadingScorerFromFile`, `InitializingBackgroundProcessor`,
  `ReadingChannelsAddressesFromFile` (the public-docs list), reported in that
  order as the node starts; no other in-progress value is emitted.
- **UserActionRequired** -- never reported (no hardware policy exists for
  Lightning).
- **Ok** -- `details` is `{platform_coin: string (the platform ticker), address:
  string (the node public key, 33-byte compressed, hex), balance: {spendable:
  string, unspendable: string}}`; `spendable` is normally `"0"` at activation
  (channel balance becomes spendable once counterparties are online).
- **Error** -- `details` is the error object with one of:

| `error_type` | Returned when | `error_data` |
| --- | --- | --- |
| `Transport` | the platform coin's backend (electrum / native node) cannot be reached when fetching fee estimates at the `GettingFeesFromRPC` step, or a transport failure reading the final balance | string |
| `Internal` | any other node-start failure: platform activated in HD-wallet mode, I/O error (e.g. listening port in use -- the public-docs example), persistence / database / backup-path errors, address or non-transport balance errors | string |
| `L2IsAlreadyActivated` | the coin was registered by a concurrent activation before this task finished | string: the Lightning ticker |
| `TaskTimedOut` | the task framework timed the task out | `{duration: {secs, nanos}}` |

**Unresponsive platform backend.** A Lightning activation whose platform UTXO
backend is unreachable shall end with a status-level `Error` carrying
`Transport` (string); the `status` call itself returns HTTP 200.

R41.2.8 **Completion delay (informative).** After the node has started, the
reference waits roughly ten seconds before reading the address and balance and
completing the task; clients must not assume a bound on activation time.

### `task::enable_lightning::user_action` and `cancel`

R41.2.9 `task::enable_lightning::user_action` is routed; its request carries the
shared hardware user-action payload of ch. 55 §55.2. No Lightning task ever
awaits an action, so a call yields the task-framework error of ch. 55 §55.2.

R41.2.10 `task::enable_lightning::cancel` takes `{task_id: integer}` and returns
`"success"`; cancelling a running activation stops it and, if the Lightning coin
had already been registered, de-registers it. Task-framework errors
(`NoSuchTask`, `TaskFinished` -- the public-docs example shows the latter at
mmrpc level with an integer `error_data`) are bound by ch. 55 §55.2.

### Lightning coin configuration (dictated config contract)

R41.2.11 A Lightning coins-configuration entry carries `coin`, `decimals`,
`protocol`, and optionally `accept_inbound_channels`,
`accept_forwards_to_priv_channels`, `channel_options`, `our_channels_configs`,
`counterparty_channel_config_limits` and `sign_message_prefix`. Its `protocol`
is `{"type": "LIGHTNING", "protocol_data": {"platform": <platform ticker>,
"network": "mainnet" | "testnet" | "regtest", "confirmation_targets":
{"background": <integer>, "normal": <integer>, "high_priority": <integer>}}}`;
the confirmation targets are block-count targets used to fetch fee estimates
from the platform backend at activation (the `GettingFeesFromRPC` phase). A
`protocol_data` without `confirmation_targets` is a protocol parse failure
(`L2ProtocolParseError`).

> **Upstream divergence (informative).** The public Lightning page spells the
> per-channel defaults key both `our_channels_config` (table, first example) and
> `our_channels_configs` (second example); the reference reads
> `our_channels_configs`, which is bound. Accepting the other spelling as an
> alias is permitted.

### Reloaded status of §41.2 (informative)

> **Maintainer decision (binding, 2026-10-11).** This branch implements the
> wire contract of §41.2: items 1-4 below. Items 5-8 (progress sequence,
> `user_action` payload, fee estimates and the `confirmation_targets` /
> `our_channels_configs` coins-configuration forms) are deferred to
> `docs/plans/lightning-activation-features.md`. Item 9 is decided separately
> (D41.2).

> **Implemented on `fix/activation-api-parity`:** items 1-4 below (the bound
> error names and shapes, the `avg_blocktime` check, the status-level mapping).
> Until item 7 lands, the status-level `Transport` of an unreachable platform
> backend is raised at the node's first chain read rather than at the fee
> fetch. Remaining: items 5-8 (deferred to the Lightning plan).

Current reloaded behaviour that deviates from §41.2:

1. **Error names.** Reloaded's task errors are `AlreadyActivated`,
   `ConfigNotFound`, `ProtocolParseError`, `UnexpectedProtocol`,
   `PlatformNotActivated`, `IncompatiblePlatform`, `InvalidPlatformConfig`,
   `ConfigParseError`; bound: `L2IsAlreadyActivated`, `L2ConfigIsNotFound`,
   `L2ProtocolParseError`, `UnexpectedL2Protocol`, `PlatformCoinIsNotActivated`,
   `UnsupportedPlatformCoin`, `InvalidPlatformConfiguration`,
   `L2ConfigParseError` with the shapes and statuses of R41.2.6.
2. **`UnexpectedL2Protocol` payload.** Reloaded omits the `protocol` member;
   bound: `{ticker, protocol}`.
3. **`avg_blocktime` check.** Reloaded does not check the platform's
   `avg_blocktime` at `init`; bound: `InvalidPlatformConfiguration` (row 9).
4. **Status-level mapping.** Reloaded maps every node-start failure (incl. an
   unreachable platform backend) to `Internal`, every balance failure to
   `Transport`, and a registration conflict to `Internal`; bound: R41.2.7.
5. **In-progress values.** Reloaded reports its own value set (starting with an
   initialising value) and never advances it; bound: the R41.2.7 sequence.
6. **`user_action` payload.** Reloaded expects a placeholder confirmation
   action; bound: the shared hardware user-action payload (R41.2.9).
7. **Fee estimates.** Reloaded does not fetch fee estimates from the platform
   backend at activation and reads per-priority default fees from a
   `confirmations` protocol member; bound: `confirmation_targets` integers and
   the `GettingFeesFromRPC` phase (R41.2.11). A coins file written for the
   reference therefore fails reloaded's protocol parsing today.
8. **`our_channels_configs`.** Reloaded reads `our_channels_config` only; bound:
   R41.2.11.
9. **Extra methods.** `enable_lightning` and the flat `init_lightning` quartet
   (R41.2.2). Kept as documented extensions by maintainer decision; not a
   deviation to fix.

> **Code-quality finding (informative; trigger: apparent with little extra
> effort).** When the platform coin is not active, the reference's
> `PlatformCoinIsNotActivated` names the *Lightning* ticker where the error's
> meaning calls for the platform ticker (the public-docs example shows the same).
> R41.2.6 binds the reference behaviour for parity; a future deliberate
> deviation could carry the platform ticker instead.

> **Decision recorded (informative).** The reloaded-only `enable_lightning`
> and `init_lightning` quartet stay routed as documented extensions (R41.2.2).

## 41.3 Peer & channel management

R41.3.1 The project shall expose the following public RPCs with the stated roles:
- `connect_to_lightning_node` -- connect to a peer by node-pubkey@host:port;
- `open_channel` -- open a channel to a peer with a requested capacity and
  options (e.g. announced/unannounced, push amount);
- `close_channel` -- cooperatively or force-close a channel;
- `get_channel_details` -- return the details of one channel;
- `list_open_channels_by_filter` -- list open channels matching a filter;
- `list_closed_channels_by_filter` -- list historically-closed channels matching
  a filter;
- `get_claimable_balances` -- report balances claimable from channels (including
  pending close states).

## 41.4 Invoices & payments (R31 dictated by BOLT-11)

R41.4.1 The project shall expose the following public RPCs:
- `generate_invoice` -- create a BOLT-11 invoice for a requested amount and
  description;
- `send_payment` -- pay a BOLT-11 invoice (or a keysend/spontaneous payment to a
  node), returning a payment identifier;
- `get_payment_details` -- return the status/details of one payment;
- `list_payments_by_filter` -- list payments matching a filter.

R41.4.2 Invoice encoding/decoding and payment routing shall conform to the BOLT
specifications; those specifications are the source of truth.

## 41.5 Swap timelock contract (R31 dictated by swap protocol)

R41.5.1 When Lightning participates as a swap leg, the on-chain HTLC timelock
used for that leg shall be derived from the swap's lock-duration contract so that
the Lightning final-CLTV expiry is consistent with the counterpart chain's
HTLC timelock (the maker lock duration governs the final CLTV expiry delta).
This preserves the atomicity guarantee: neither side can claim without revealing
the secret, and refunds become possible only after the agreed timeout.

## 41.6 Engine dependency

R41.6.1 The Lightning engine is an external library dependency pinned to a
specific version; upgrading it is an implementation concern. Requirements in this
chapter bind the project's public behaviour and the BOLT-level wire contract, not
the engine's internal API. The implementation shall track a maintained engine
version.

> **Upstream divergence (informative).** Upstream regrouped the channel/payment
> operations under a `lightning::` RPC namespace (e.g.
> `lightning::channels::open_channel`). Reloaded exposes **both** forms: the
> original **flat** method strings (e.g. `open_channel`, `send_payment`) remain
> routed for backward compatibility, and the namespaced `lightning::channels::*`
> / `lightning::nodes::*` / `lightning::payments::*` aliases (including
> `lightning::channels::update_channel` and the §41.7 `lightning::nodes::*`
> trusted-node RPCs) are routed alongside them to the same handlers. This is a
> public-interface naming difference, not a behavioural one; no request differs
> in outcome by which form of the method string it uses.

## 41.7 Trusted-node management RPCs

> **Source of truth (informative).** The request/response field names and the
> namespaced method strings in this section are the **public** Komodo DeFi
> Framework API contract (the published Lightning RPC reference). Where this
> chapter and the public API docs disagree on a field name, the public API docs
> govern the wire contract.

> **Status:** these three RPCs are **implemented and routed** in reloaded-public
> (see the implementation-substrate note at the end of this section for the
> persistence substrate that backs them).

R41.7.1 The project shall expose three native-only mmrpc-2.0 RPCs for managing the
set of *trusted nodes* of an activated Lightning coin:
- `lightning::nodes::add_trusted_node`
- `lightning::nodes::list_trusted_nodes`
- `lightning::nodes::remove_trusted_node`

Each request is an mmrpc-2.0 envelope (`mmrpc: "2.0"`, a `method` string from the
list above, a `params` object, and an optional client-supplied `id` echoed in the
response). Each is resolved against the **activated** Lightning coin named by the
`params.coin` field; if no such activated Lightning coin exists the call fails
(R41.7.7). All three are absent from the WASM build.

R41.7.2 **Definition of a trusted node.** A trusted node is a counterparty peer,
identified by its Lightning node public key, from which the local node is willing
to accept **zero-confirmation** inbound channel funding -- i.e. to treat a
channel that peer opens as usable before its funding transaction has confirmed on
the parent chain. Membership of the trusted-node set is the sole gate on that
zero-confirmation acceptance for inbound channels; a peer not in the set is
subject to the node's normal confirmation requirement.

R41.7.3 **`add_trusted_node` request / response.**
- Request `params`:
  - `coin` -- string, **required** -- ticker of the activated Lightning coin.
  - `node_id` -- string, **required** -- the trusted peer's Lightning node public
    key (compressed-pubkey hex, as used elsewhere in the Lightning surface).
- Success `result`:
  - `added_node` -- string -- the node public key that the request asked to add
    (echo of `node_id`).

R41.7.4 **`remove_trusted_node` request / response.**
- Request `params`:
  - `coin` -- string, **required**.
  - `node_id` -- string, **required** -- the node public key to drop from the set.
- Success `result`:
  - `removed_node` -- string -- the node public key that the request asked to
    remove (echo of `node_id`).

R41.7.5 **`list_trusted_nodes` request / response.**
- Request `params`:
  - `coin` -- string, **required**.
- Success `result`:
  - `trusted_nodes` -- array of strings -- the node public keys currently in the
    coin's trusted-node set (order unspecified; may be empty).

R41.7.6 **Idempotent-reporting mutation semantics.** `add_trusted_node` and
`remove_trusted_node` shall be idempotent in effect: adding a node already present
leaves the set unchanged and still succeeds; removing a node not present leaves
the set unchanged and still succeeds. The implementation shall determine and may
surface whether the call actually changed the set (added a new member / removed an
existing member), so that a no-op add or remove is distinguishable from one that
mutated the set.

R41.7.7 **Persistence and reload.** The trusted-node set is durable state of the
Lightning coin. A successful `add`/`remove` shall be persisted through the coin's
existing Lightning persister before the call reports success, and the set shall be
re-loaded from persistent storage at coin activation so that trusted-node
membership survives a restart. `list_trusted_nodes` shall reflect the current
persisted set.

R41.7.8 **Error conditions (public `error_type` + HTTP status).**
- The `coin` does not name an activated Lightning coin (unknown coin, or a coin
  that is not a Lightning coin) -- `error_type` reporting an unsupported/unknown
  coin -- HTTP **400 Bad Request**.
- `node_id` is missing or not a well-formed Lightning node public key -- an
  invalid-request / parse `error_type` -- HTTP **400 Bad Request**.
- A failure to persist the updated set through the persister -- an internal/save
  `error_type` -- HTTP **500 Internal Server Error**.

## 41.8 Live channel configuration update (`lightning::channels::update_channel`)

> **Source of truth (informative).** As in §41.7, the field names and method
> string below are the **public** Komodo DeFi Framework Lightning RPC contract;
> the published API docs govern on any discrepancy.

> **Status:** this RPC is **implemented and routed** in reloaded-public, delivered
> against the vendored Lightning substrate via the bounded patch described in the
> feasibility verdict at the end of this section; the contract below is the
> shipped behaviour.

R41.8.1 The project shall expose a native-only mmrpc-2.0 RPC
`lightning::channels::update_channel` that mutates the configurable parameters of
a single **live** channel on the running channel manager of the activated
Lightning coin named by `params.coin`. It is absent from the WASM build.

R41.8.2 **Channel identity.** The target channel is named by its **RPC channel
identifier** (`rpc_channel_id`) -- the stable per-channel numeric handle assigned
when the channel was opened and surfaced by the channel-listing / channel-details
RPCs of §41.3. It is **not** the on-chain funding outpoint. The implementation
resolves this identifier to the live channel's counterparty node public key and
channel id before applying the update.

> **Wire-identifier divergence (informative).** The published reference material
> names the per-channel identifier `uuid` across the Lightning channel RPC
> surface. The reloaded Lightning module instead identifies channels by the
> numeric `rpc_channel_id` across **all** of its §41.3 channel RPCs; there is no
> UUID handle anywhere in the reloaded substrate. `update_channel` therefore takes
> `rpc_channel_id` to remain consistent with the identifier the §41.3 RPCs return
> -- the only handle a caller can actually obtain. Migrating the whole Lightning
> channel surface to a `uuid` wire name is a separate module-wide decision and is
> out of scope for this method.

R41.8.3 **Request `params`.**
- `coin` -- string, **required** -- ticker of the activated Lightning coin.
- `rpc_channel_id` -- unsigned integer (64-bit), **required** -- the RPC channel
  identifier of the channel to update (R41.8.2).
- `channel_options` -- object, **required** -- the mutable per-channel
  configuration to apply. Every field is **optional**; a field that is present
  overrides the corresponding parameter, and a field that is absent leaves that
  parameter to be carried from the merge base (R41.8.4). The publicly exposed
  configurable fields are:
  - `proportional_fee_in_millionths_sats` -- unsigned integer (32-bit) -- the
    proportional outbound forwarding fee charged for routing through this channel,
    in millionths of the forwarded amount.
  - `base_fee_msat` -- unsigned integer (32-bit) -- the flat per-forward base fee
    charged for outbound forwarding, in millisatoshis.
  - `cltv_expiry_delta` -- unsigned integer (16-bit) -- the CLTV expiry delta this
    channel advertises/enforces for forwarded HTLCs (a block count).
  - `max_dust_htlc_exposure_msat` -- unsigned integer (64-bit) -- the cap on total
    in-flight dust HTLC exposure on this channel, in millisatoshis.
  - `force_close_avoidance_max_fee_sats` -- unsigned integer (64-bit) -- the
    additional fee, in satoshis, the node is willing to absorb on a cooperative
    close to avoid waiting on the counterparty's locktime (a force-close
    avoidance ceiling).

> **Field-name note (informative).** The on-wire option name for the force-close
> ceiling is `force_close_avoidance_max_fee_sats`. The underlying Lightning
> channel-config field is spelled `force_close_avoidance_max_fee_satoshis`; the
> RPC surface deliberately exposes the shorter `_sats` form, so the wire contract
> uses `_sats` even though some published reference material mirrors the internal
> `_satoshis` spelling.

R41.8.4 **Behaviour and merge base.** The call shall:
1. resolve `coin` to an activated Lightning coin (else error per R41.8.6);
2. resolve `rpc_channel_id` to a live channel on that coin (else error per
   R41.8.6), obtaining its counterparty node public key and channel id;
3. form the **effective** option set by taking the coin's configured
   channel-option defaults as the merge base and overlaying each option present in
   the request `channel_options`; where the coin carries no configured defaults,
   the request `channel_options` is itself the base;
4. apply the effective option set to the channel's configuration on the
   **running** channel manager so it takes effect on the live channel without
   re-activation, which includes re-advertising the channel's forwarding policy to
   peers (a fresh channel-update gossip message reflecting the new fees / CLTV
   delta);
5. persist the channel-manager state so the new configuration survives a restart;
6. return the effective option set (R41.8.5).

> **Merge-base note (informative; Upstream divergence).** The merge base in step 3
> is the **coin-level configured channel-option defaults**, not a read-back of the
> channel's *current* live runtime config. The vendored Lightning substrate does
> not expose reading a live channel's `ChannelConfig` back out, so a parameter
> omitted from the request is filled from the coin's defaults rather than from a
> value that may have been set by a prior `update_channel` call. Callers that want
> deterministic results should send the full `channel_options` set they intend.

R41.8.5 **Success `result`.**
- `channel_options` -- object -- the effective configuration applied to the
  channel, carrying the same fields enumerated in R41.8.3 with their resulting
  values.

R41.8.6 **Error conditions (public `error_type` discriminant + HTTP status).**
- The named `coin` exists but is not a Lightning coin -- `error_type`
  **`UnsupportedCoin`** -- HTTP **400 Bad Request**.
- No coin with the given ticker is activated -- `error_type` **`NoSuchCoin`** --
  HTTP **404 Not Found**.
- No channel with the given `rpc_channel_id` exists on the coin -- `error_type`
  **`NoSuchChannel`** -- HTTP **404 Not Found**.
- The channel manager rejects the configuration mutation or the updated state
  cannot be persisted -- `error_type` **`FailureToUpdateChannel`** -- HTTP
  **500 Internal Server Error**.

> **Implementation-substrate note + FEASIBILITY VERDICT (informative; status).**
> §41.7 is **IMPLEMENTED** (persisted `trusted_nodes` set with save/load and
> activation reload, R41.7.7).
>
> §41.8 (`update_channel`) is **IMPLEMENTED** via a BOUNDED vendored-LDK patch
> (verdict **A**) and is **routed** in the dispatcher (native-only). Substrate
> reality that the patch addressed:
> - The vendored `rust-lightning-patched` is LDK **0.0.106**, but its
>   per-channel `ChannelConfig` is **already a public struct with all five
>   configurable fields public** (proportional fee, base fee, CLTV delta, max
>   dust HTLC exposure, force-close avoidance ceiling), and the **dust-exposure
>   cap is already enforced** by the channel state machine. The `coins` crate can
>   therefore already *construct* the target `ChannelConfig`.
> - The **only** missing substrate capability is the *runtime* mutation entry
>   point on the channel manager (the public live-config-update method that LDK
>   added upstream in 0.0.107). 0.0.106 only sets channel config at open time via
>   `UserConfig` and exposes no public method to mutate an open channel's config
>   and re-gossip its policy.
> - **Bounded patch shape (role level, no upstream code transcribed):** add one
>   public method on the vendored channel manager that takes a counterparty node
>   public key, a list of channel ids, and a `ChannelConfig`, and for each matched
>   live channel (a) overwrites its stored per-channel config, (b) regenerates and
>   enqueues a broadcast channel-update gossip message reflecting the new
>   forwarding policy, and (c) marks the manager for persistence. The handler in
>   the `coins` crate then resolves coin + `rpc_channel_id` -> live channel, builds
>   the merged `ChannelConfig` per R41.8.4, calls this method, persists, and
>   returns the merged options. Route the method in the dispatcher.
> - **Risk: MODERATE.** The patch touches the channel manager's gossip/broadcast
>   and persistence paths; the upstream 0.0.107 change is the well-scoped
>   reference for correctness. Failure modes to guard in implementation/tests:
>   stale or mis-sequenced channel-update gossip (peers routing on old fees), and
>   failure to persist (config lost on restart). No funds-custody surface is
>   touched -- only this node's own forwarding-policy / fee parameters -- so a
>   correctly bounded patch introduces no security hole.
> - A full LDK uplift (0.0.106 -> 0.0.113, matching corpus) is **not required**
>   for §41.8 because the field set and dust enforcement are already present; an
>   uplift remains a larger, separate effort tracked elsewhere and is out of scope
>   here.
>
> **Historical fallback (superseded).** Before the bounded patch above landed,
> this chapter allowed routing `update_channel` to a documented not-available
> failure (HTTP 501) rather than leaving it unrouted, as an interim divergence
> from the public contract. That fallback no longer applies: verdict **A** has
> shipped and `update_channel` performs the update as specified in R41.8.3-
> R41.8.5. Recorded here only so a reader of prior chapter history understands
> why an interim 501 path may have existed.

## 41.9 Acceptance criteria

- Lightning is absent from the WASM build and present on native (R41.1).
- A node activates via `task::enable_lightning::init` and reaches the `Ok`
  status with `{platform_coin, address, balance}`; every `init` failure class of
  R41.2.6 and every status-level failure class of R41.2.7 yields the bound
  `error_type`, `error_data` shape, HTTP status and placement; an unreachable
  platform backend ends in a status-level `Transport` error (R41.2).
- A channel can be opened to a peer, listed, its details fetched, and closed
  (R41.3).
- A BOLT-11 invoice can be generated and paid; the payment is then listed and its
  details fetched (R41.4).
- A swap leg using Lightning derives its final-CLTV expiry from the swap lock
  duration (R41.5).
- A node can be added to, listed in, and removed from a Lightning coin's
  trusted-node set; the set survives a restart; repeated add/remove report
  whether the set actually changed (R41.7).
- A live channel's configurable parameters (fees, CLTV delta, dust cap,
  force-close fee ceiling) can be updated by `rpc_channel_id` and the effective
  configuration is echoed back and persisted (R41.8), delivered by the bounded
  vendored-LDK runtime-config-mutation patch described in §41.8.

## 41.10 Tests

T41.1 *Init error names and shapes.* For each of rows 1-7 and 9 of R41.2.6 a
wire-shape test asserts the serialized `error_type`, the `error_data` type and
member names, and the HTTP status; row 6 asserts that `error_data` is the
Lightning ticker.

T41.2 *`UnexpectedL2Protocol` payload.* A `ticker` whose protocol is `UTXO`
yields HTTP 400 with `error_data` `{ticker, protocol: {"type": "UTXO"}}`.

T41.3 *Missing `avg_blocktime`.* A segwit platform without `avg_blocktime`
yields HTTP 500 `InvalidPlatformConfiguration` with
`{platform_coin_ticker, err}`.

T41.4 *Unreachable platform backend.* With the platform's electrum servers
down, `init` returns a `task_id` and `status` eventually returns HTTP 200
`{"status": "Error", "details": {..., "error_type": "Transport",
"error_data": <string>}}`.

T41.5 *In-progress values.* Every in-progress `details` observed during a
successful activation is one of the strings of R41.2.7, starting with
`ActivatingCoin`.

T41.6 *Coin configuration.* A `LIGHTNING` entry with `confirmation_targets`
integers and `our_channels_configs` parses; one with only the baseline-era
`confirmations` member fails with `L2ProtocolParseError`.

T41.7 *Activation parameters.* `color` absent yields colour `2b6680`;
`listening_port` absent yields 9735; a 33-byte `name` yields HTTP 500
`Internal` at `init`.

## 41.11 Deferred Work

D41.1 Hardware-wallet Lightning activation (no `UserActionRequired` state
exists today).

D41.2 *Closed (2026-10-11).* The reloaded-only `enable_lightning` and
`init_lightning` methods stay routed as documented extensions (R41.2.2).

D41.3 *A discarded node keeps running (post-beta defect, 2026-10-29).*
Starting the node binds its listening port and spawns long-lived loops, each
holding the node:
- network-graph and scorer persistence;
- peer connection and node announcement;
- the peer-to-peer listener;
- best-block updates;
- the background processor.

No stop signal reaches them, so they survive two cases. One is an activation
that fails after the node has started, for example at the address or balance
read. The other is a Lightning coin that is disabled. Both leave the port bound
and the loops running. A later activation on the same port then fails with an
I/O error, and the loops of both nodes can write the same persisted files. A
fix needs stop handles in the Lightning coin and its loops; see plan
[`lightning-activation-features`](../plans/lightning-activation-features.md).

## 41.11A Trading floors (Spec Reader 2026-10-25; cleared by the Dirty Gate with chapter 60 on 2026-10-25)

R41.11A.1 In both reference designations (`v2.6.0-beta` and `v3.0.0-beta`
agree) a Lightning coin's minimum transaction amount is the largest, over
the node's open channels, of the counterparty's minimum outbound HTLC
amount in millisatoshi — one millisatoshi for a channel that states none,
and one millisatoshi when there are no channels — expressed in the coin's
configured decimals; its minimum trading volume equals that amount. It is
the only family whose floors depend on node state (channels) rather than
configuration alone ([Chapter 60](60-trading-floors-and-refusal-contract.md)
R60.2.2, row L1).

R41.11A.2 Reloaded implements neither accessor: both are placeholders
that end the request abnormally, so `min_trading_vol` for an active
Lightning coin, and any order or trade preview naming one, gets no orderly
response (chapter 60 finding F5). Required: report R41.11A.1's value, or,
while Lightning trading is unsupported, refuse the order or preview with
an error (chapter 60 R60.6.5). This contradicts this chapter's "as-built"
status line for these two accessors only; the activation and channel
surfaces it documents are unaffected.

> **As-built note (2026-10-25, clean-side record).** Implemented in PR
> #66. Both accessors report R41.11A.1's value; a channel whose
> counterparty has not yet stated a minimum counts as 1 msat, and a stated
> minimum of zero counts as zero. The vendored LDK fork gained the
> counterparty minimum in its channel details as a local patch, recorded in
> plan `lightning-ldk-upgrade`. Orders and trade previews naming a
> Lightning coin are refused with an error while Lightning trading is
> unsupported (chapter 60 R60.6.5). The cleared rule text above is
> unchanged.

## 41.12 Baseline Verifications

V41.1 The GPLv2 baseline routes the one-shot `enable_lightning` and parses the
`LIGHTNING` protocol with a `confirmations` member; §41.2's reloaded-status
list records the resulting deviations.

## 41.13 Provenance Footer

- *Inputs:* the project's own revision history and current tree (the
  shipped `LightningCoin` node/channel/payment RPC surface this chapter
  documents as-built -- by public behaviour and wire-contract shape
  only, no code transcribed); the GPLv2 baseline dispatcher and coin protocol;
  the published Komodo DeFi Framework Lightning activation page, Lightning coin
  configuration page and error-type catalogue; the published BOLT
  specifications (BOLT-11 invoices, channel and HTLC semantics) and the vendored
  LDK-style Lightning library's public network behaviour;
  [Chapter 48](48-platform-coin-task-activation.md) (the task-lifecycle
  substrate `task::enable_lightning` is delivered through);
  [Chapter 55](55-coin-activation-wire-contract.md) (envelope and
  task-framework errors).
- *Permitted-input classes used:* baseline/as-built source (the shipped
  Lightning coin and its vendored LDK dependency); external public
  specification (the KDF API documentation, the BOLT specifications);
  cross-chapter contracts (Chapters 36, 48, 55); interop / wire-and-API-bound
  reuse (R29/R31) for the dictated method strings, field names,
  coin-configuration keys and error discriminants.
- *Sibling-allowlist consultations:* [Chapter 36](36-tendermint-v2-activation-rpcs.md),
  [Chapter 48](48-platform-coin-task-activation.md),
  [Chapter 55](55-coin-activation-wire-contract.md).
- *Forbidden corpus:* (addition of 2026-10-25, §41.11A, cleared by the
  Dirty Gate with [Chapter 60](60-trading-floors-and-refusal-contract.md)
  on 2026-10-25, attestation in its §60.12:
  also consulted at the same two tags, only to establish R41.11A.1's
  channel-derived floor; no discretionary expression reproduced.)
  Otherwise consulted, under the chapter-01 two-team clean-room
  workflow, at the GLEEC KDF tags `v3.0.0-beta` (reference) and `v2.6.0-beta`
  (legacy comparison), for the node-activation contract of §41.2 only --
  specifically the routed method set of R41.2.1, the activation-parameter
  defaults of R41.2.4, the `init` error precedence, shapes and HTTP statuses of
  R41.2.6, the in-progress values and status-level error mapping of R41.2.7,
  the cancel side effect of R41.2.10 and the coin-configuration keys of
  R41.2.11. The remaining sections were not derived from the corpus. No source
  text, private identifier, internal decomposition, or error/log string literal
  was carried into this chapter.
- *Clean-context addition (2026-10-29, forbidden corpus not consulted):*
  D41.3 is Reloaded-original. It records a behavior of this repository's own
  Lightning node start-up, found while R55.1.12 of
  [Chapter 55](55-coin-activation-wire-contract.md) was implemented. It adds
  no reference content, so chapter 01 R38 is not engaged and the corpus
  record above is unchanged.
