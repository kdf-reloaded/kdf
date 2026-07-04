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
> This chapter documents that shipped capability.

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

R41.2.1 Lightning node activation shall be a long-running task exposed as the
public RPC quartet `init_lightning` / `init_lightning_status` /
`init_lightning_user_action` / `cancel_init_lightning`.

R41.2.2 A simplified single-call enable path shall also be exposed as the public
RPC `enable_lightning`.

R41.2.3 Activation shall bind the Lightning coin to its parent UTXO coin, load or
create the node's persistent state, and start the background node event loop.

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

> **Upstream divergence (informative).** Reloaded exposes the channel/payment
> operations under **flat** method strings (e.g. `open_channel`,
> `send_payment`). Upstream later regrouped several of these under a
> `lightning::` RPC namespace (e.g. `lightning::channels::open_channel`). This is
> a public-interface naming difference, not a behavioural one. **Recommended (if
> aligning to upstream):** introduce the namespaced aliases while keeping the
> flat names for backward compatibility, or document the flat names as the
> reloaded contract. No behavioural change is required.

## 41.7 Trusted-node management RPCs

> **Source of truth (informative).** The request/response field names and the
> namespaced method strings in this section are the **public** Komodo DeFi
> Framework API contract (the published Lightning RPC reference). Where this
> chapter and the public API docs disagree on a field name, the public API docs
> govern the wire contract.

> **Status:** these three RPCs are **not yet present** in reloaded-public and are
> specified here as a forward requirement (see the implementation-substrate note
> at the end of this section).

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

> **Status:** this RPC is **not yet routed** in reloaded-public. It is
> **deliverable** against the vendored Lightning substrate via a bounded patch
> (see the feasibility verdict at the end of this section); the contract below is
> the finished target.

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
> **Fallback (clean published failure, only if the user elects to route §41.8
> before the bounded patch lands):** the method may be routed to a documented
> failure rather than left unrouted -- it shall return a single documented
> `error_type` of a not-available class with HTTP **501 Not Implemented**, never a
> panic, never a partial/silent mutation. This fallback is a divergence from the
> public contract (which performs the update) and must itself be documented as
> such; it is *not* the preferred finish, which is verdict **A** above.

## 41.9 Acceptance criteria

- Lightning is absent from the WASM build and present on native (R41.1).
- A node activates via `init_lightning` and reaches a ready state (R41.2).
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
