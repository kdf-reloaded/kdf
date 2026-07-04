# Chapter 10 — Server-Sent-Events Streaming Substrate

**Status:** driving-spec.

A reusable in-process event-broker substrate plus a native-only HTTP
transport adapter, together exposing nine Server-Sent-Events streamers
under a dedicated RPC namespace, structurally replacing the polling-only
read model the baseline tree carried for live GUI updates.

## 10.1 Executive Summary

The baseline tree carries no first-class real-time event channel:
graphical consumers must poll the JSON-RPC surface for balances, swap
status, order status, and orderbook updates. The substrate bound by this
chapter introduces a structural split into two layers:

1. An in-process publish/subscribe broker exposed as a standalone crate
   substrate (chapter-bound identifier: `mm2_event_stream`), owning
   per-streamer task lifecycles and per-client bounded delivery channels.
2. A native-only HTTP handler at the bound path `GET /event-stream`,
   adapting the broker's per-client receiver into the
   `text/event-stream` wire format.

Activation is bound to a new RPC namespace prefix (`stream::`) with
exactly nine `<category>::enable` methods. Per-stream deactivation is
bound to the same namespace through `stream::disable`; full client
deactivation also occurs when an HTTP client connection drops. Slow
clients are bound to be individually back-pressured (events dropped per
slow client) and never block the broadcaster or other clients.

This chapter binds the broker's public crate surface, the streamer trait
contract, the wire-stable streamer origin tags, the HTTP endpoint shape,
the `stream::*` dispatcher routing, the nine concrete streamer identities
shipped by the substrate, and the runtime invariants the design relies
on.

## 10.2 Subsystem Shape

The substrate occupies a structural seam between three subsystems:

- the central-context substrate (the broker handle is owned as a field
  on the context and reachable from any subsystem holding the context);
- the JSON-RPC dispatcher (the `stream::*` namespace prefix is bound as
  a dispatcher branch routing to streamer activation and deactivation
  handlers);
- the native HTTP server (the `/event-stream` route is bound as an
  additional handler beside the JSON-RPC handler, gated to native-only
  targets).

The substrate is *not* a redesign of the JSON-RPC surface. The nine
streamers add a push channel beside the existing pull surface; no
pre-existing read RPC is removed, renamed, or repurposed. Bound rules
(R1–R6) constrain the broker; (R7–R14) constrain the streamer trait
contract and origin tags; (R15–R21) constrain the HTTP endpoint and
namespace; (R22–R26) constrain the shared activation envelope and
concrete streamers; (R29–R32) bind the sixth (Network) streamer;
(R33–R38) bind the seventh (fee-estimator) streamer; (R39–R45)
bind the eighth (tx-history) streamer; (R46–R51) bind the ninth
(shutdown-signal) streamer.

## 10.3 Bound Crate Surface

**R1.** The substrate MUST be a single crate. The chapter-bound
identifier is `mm2_event_stream`. Its public surface MUST be exactly the
following five names (any wider or narrower re-export set is a
substrate-shape violation):

- `Event`
- `StreamingManager`
- `Broadcaster`
- `EventStreamer` (trait)
- `NoDataIn`
- `StreamerId`

Plus a pass-through re-export of the asynchronous-channel primitives the
trait surfaces (`mpsc` and `oneshot`), so consumers can implement the
trait without a direct asynchronous-runtime dependency.

**R2.** The crate MUST preserve separate public API concerns for the
event payload, streamer identity, streamer implementation contract, and
broker coordination. No internal source-module names or file layout are
specified by this chapter.

**R3.** The broker MUST be cheap-to-clone (an inner shared handle behind
an interior-mutable lock). Cloning the broker MUST NOT copy its
registry; all clones MUST observe the same set of running streamers and
the same client map.

## 10.4 Bound Event Payload

**R4.** The `Event` payload MUST carry exactly three fields: an origin
tag (typed `StreamerId`), a JSON message body, and a boolean error
indicator. All other on-the-wire data (timestamps, ticker, payload
shape) MUST live inside the JSON message body.

**R5.** `Event` constructors MUST return a reference-counted handle
(`Arc<Event>`), so fan-out cloning across many clients is reference-count
increment only. Two constructor names are bound: `Event::new` (normal
event) and `Event::err` (error event). Three reader-side helpers are
bound: `is_error()`, `origin()` returning the wire-stable origin string,
`get()` returning the `(origin, message)` pair.

## 10.5 Bound Streamer Origin Tags

**R6.** The streamer origin tag (`StreamerId`) MUST be an enumeration
with exactly nine concrete variants and the following wire-stable display strings
(GUI-visible, treated as part of the SSE contract surface):

| Variant                                | Wire string             |
| -------------------------------------- | ----------------------- |
| Heartbeat                              | `HEARTBEAT`             |
| Balance(ticker)                        | `BALANCE:<ticker>`      |
| Network                                | `NETWORK`               |
| SwapStatus                             | `SWAP_STATUS`           |
| OrderStatus                            | `ORDER_STATUS`          |
| OrderbookUpdate { topic }              | `ORDERBOOK:<topic>`     |
| FeeEstimation(ticker)                  | `FEE_ESTIMATION:<ticker>` |
| TxHistory(ticker)                      | `TX_HISTORY:<ticker>`   |
| ShutdownSignal                         | `SHUTDOWN_SIGNAL`       |

These wire strings MUST be exact byte-for-byte: uppercase, colon
separator before the dynamic component, no whitespace, no padding. They
appear inside every SSE frame's JSON envelope as the `origin` field
(R17).

**R7.** The tag MUST derive equality, hashing, debug, serde, and clone.
The hashable property is load-bearing: it is the registry key under
which the broker deduplicates running streamers (R10).

## 10.6 Bound Streamer Trait Contract

**R8.** The `EventStreamer` trait MUST have the following shape (only
two associated items: an associated input-data type and a
`streamer_id()` accessor; one asynchronous method `handle`):

- An associated `DataInType` constrained `Send`. For self-driven
  streamers (timers, polls) this is the bound uninhabited type
  `NoDataIn`.
- An accessor `streamer_id()` returning `StreamerId`. It MUST be callable
  before `handle` runs; it is the registry key the broker looks up to
  decide spawn-vs-attach (R10).
- An asynchronous method `handle(self, broadcaster, ready_tx,
  shutdown_rx, data_rx)` consuming the streamer by value, taking a
  `Broadcaster` handle, a single-shot `ready_tx` returning
  `Result<(), String>`, a single-shot `shutdown_rx`, and the typed
  data-input receiver.

**R9.** Implementations MUST send exactly one value on `ready_tx`:
`Ok(())` after initialisation succeeds, or `Err(reason)` to abort. A
dropped `ready_tx` MUST be treated as failure by the broker.
Implementations MUST return when `shutdown_rx` resolves; the broker
fires shutdown once the last subscriber leaves.

**R10.** The `NoDataIn` type MUST be an uninhabited enumeration (zero
variants). The data-input receiver always exists for type-erasure
uniformity inside the broker, but for self-driven streamers it can
never yield a value.

## 10.7 Bound Broker State and Lifecycle

**R11.** The `StreamingManager` MUST track active streamer identities,
registered client identifiers, and each client's subscribed origin
strings. It MUST be able to answer whether a streamer is active, whether
a client is registered, and whether a registered client is subscribed to
a given streamer origin. It MUST also retain enough lifecycle state to
deliver events to subscribed clients and to stop a streamer when its
last subscriber leaves. No private storage layout is specified by this
chapter.

**R12.** The broker MUST expose the following methods with these exact
contracts:

| Method                           | Bound behaviour                                                                                                                                                                                                                                                  |
| -------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `new_client(client_id)`          | Allocates a bounded asynchronous channel with capacity 256, inserts a client entry with empty subscription set, returns the receiver wrapped as `ClientHandle`.                                                                                                  |
| `add(client_id, streamer)` async | Ensures the client is subscribed to the streamer's origin, reusing an already-active streamer for the same origin or starting one if none is active. The call completes only after the streamer reports ready or reports activation failure. |
| `stop(client_id, streamer_id)`   | Unsubscribes the client from the named streamer. If the streamer's subscriber set becomes empty, fires the shutdown signal and removes the registry entry.                                                                                                       |
| `remove_client(client_id)`       | Removes the client entirely, calling `stop` for every streamer the client was subscribed to.                                                                                                                                                                     |
| `send<T>(streamer_id, data)`     | Looks up the streamer, downcasts the type-erased data sender to the concrete `T` sender, forwards. Errors if the streamer is not running or the type does not match.                                                                                            |
| `send_fn<T>(streamer_id, fn)`    | As `send`, but constructs the payload only after the running-streamer check.                                                                                                                                                                                    |
| `is_active(streamer_id)`         | Pure registry lookup.                                                                                                                                                                                                                                          |

**R13.** The per-client delivery channel capacity MUST be exactly 256
entries. The fan-out path MUST use the non-blocking try-send variant: a
full client buffer MUST cause the event to be dropped for that client
only, with no effect on the broadcaster or any other client.

**R14.** Fan-out MUST deliver an event only to clients subscribed to the
event's origin string, and MUST deliver it to every such client that can
accept it without blocking. Broker state access MUST NOT hold
asynchronous suspension points inside critical sections.

## 10.8 Bound HTTP Endpoint and Wire Frame

**R15.** The HTTP transport MUST be gated to native targets only.
WebAssembly builds MUST instantiate the broker and accept subscriptions
(so the same streamer activations can drive a WebAssembly-native
delivery channel exposed elsewhere), but MUST NOT carry the HTTP
endpoint.

**R16.** The endpoint MUST be exactly `GET /event-stream` with a single
query parameter `id`, an unsigned 64-bit integer. Missing or
unparseable `id` MUST default to zero. The endpoint MUST respond with
HTTP status 200 and the following header set:

| Header                          | Bound value                                                                                       |
| ------------------------------- | ------------------------------------------------------------------------------------------------- |
| `Content-Type`                  | `text/event-stream`                                                                              |
| `Cache-Control`                 | `no-cache`                                                                                       |
| `Connection`                    | `keep-alive`                                                                                     |
| `Access-Control-Allow-Origin`   | Value read from a chapter-bound central-context accessor `event_stream_access_control()`. |

**R17.** Each event frame MUST be the byte sequence:

```
data: {"origin":"<wire string>","payload":<message JSON>,"error":<bool>}\n\n
```

where `<wire string>` is exactly the R6 display string of the event's
origin tag, `<message JSON>` is the event's JSON message body verbatim,
and `<bool>` is the event's error indicator. No SSE event identifiers,
no named SSE events, no retry directives, and no comment lines are part
of the bound surface.

**R18.** The response body MUST be a chunked stream produced by
unfolding over the per-client receiver returned by `new_client`. When
the underlying connection drops, the substrate MUST call
`remove_client` for the disconnecting identifier. This disconnect path
removes every subscription for that client; the explicit per-stream
unsubscribe path is the `stream::disable` RPC bound in R21.

## 10.9 Bound RPC Namespace

**R19.** A dedicated dispatcher branch MUST be added for the `stream::`
namespace prefix. Methods whose name begins with the four-byte prefix
`stream::` MUST be routed to streamer namespace handlers; methods
without the prefix MUST be routed unchanged through the existing v2
dispatcher.

**R20.** The streamer-activation table MUST contain exactly the
following nine activation entries (no aliases, no deprecated names, no
additional activation methods):

| Method name                  | Streamer key                              |
| ---------------------------- | ----------------------------------------- |
| `stream::heartbeat::enable`  | `Heartbeat`                              |
| `stream::balance::enable`    | `Balance(<request.coin>)`                |
| `stream::network::enable`    | `Network`                                |
| `stream::swap_status::enable`| `SwapStatus`                             |
| `stream::order_status::enable`| `OrderStatus`                            |
| `stream::orderbook::enable`  | `OrderbookUpdate { topic: <request.topic> }` |
| `stream::fee_estimator::enable` | `FeeEstimation(<request.coin>)`        |
| `stream::tx_history::enable` | `TxHistory(<request.coin>)`              |
| `stream::shutdown_signal::enable` | `ShutdownSignal`                    |

The `Network` origin tag reserved in R6 is now bound to its activation
method; its request shape, payload shape, cadence, and platform gate
are bound in §10.16 (R29–R32). This resolves D2. The fee-estimator
entry is constrained in detail by §10.17 (R33–R38). The tx-history
entry is constrained in detail by §10.18 (R39–R45). The shutdown-signal
entry is constrained in detail by §10.19 (R46–R51).

**R21.** The streamer-deactivation table MUST contain exactly one
generic deactivation entry:

| Method name       | Scope |
| ----------------- | ----- |
| `stream::disable` | Disable one streamer subscription for one client. |

The RPC MUST be routed on ALL targets, even though the HTTP SSE
transport remains native-only (R15). Its request parameters MUST be
exactly two required fields, with no defaults: `client_id` as an
unsigned 64-bit integer, and `streamer_id` as the wire string returned
by a successful enable RPC. The method MUST disable only the named
streamer subscription for the named client; it MUST NOT remove the
client's other subscriptions.

On success the method MUST return the standard mmrpc response envelope
whose `result` payload is exactly:

```json
{ "result": "Success" }
```

An unknown or unregistered client MUST fail with a disable-specific
HTTP 400 error. A syntactically valid streamer identifier that is not
registered or not running MUST fail with a disable-specific HTTP 400
error. If the client exists and the streamer is active, but that client
is not subscribed to that streamer, the call MUST be a successful
no-op. Missing or invalid `client_id` or `streamer_id` fields MUST fail
during request decoding or validation, before any subscription state is
changed.

**R22.** All activation handlers (the nine bound in R20) MUST share a
common request and response envelope:

- Request: a generic envelope carrying a `client_id` field (the same
  unsigned 64-bit integer the HTTP endpoint accepts) plus an
  inner per-streamer request flattened beside it.
- Response: the standard mmrpc-2.0 envelope (`mmrpc`, `result`, `id`)
  whose `result` is an object carrying a single string field
  `streamer_id` — the wire-stable identifier of the activated streamer
  (the same `StreamerId` display string bound in R6, e.g. `HEARTBEAT`,
  `BALANCE:<ticker>`, `SWAP_STATUS`, `ORDER_STATUS`,
  `ORDERBOOK:<topic>`, `TX_HISTORY:<ticker>`, `SHUTDOWN_SIGNAL`). The
  client MUST retain this string to later deactivate the streamer via
  `stream::disable`.
  Success is conveyed by the mmrpc `result` envelope itself; there is
  NO boolean field in the response.

**R23.** Activation handlers without a streamer-specific error surface
MUST use a single activation-failure category with HTTP status 500. Any
failure reported by the streamer's `ready_tx` MUST be wrapped into that
category verbatim. A later streamer-specific section MAY bind a more
specific public error/status mapping when required for upstream
interoperability; §10.18 does so for tx-history and §10.19 does so for
shutdown-signal.

## 10.10 Bound Concrete Streamers

**R24.** The substrate MUST ship exactly the following nine concrete
streamers:

| Activation method            | Streamer key                          |
| ---------------------------- | ------------------------------------- |
| `stream::heartbeat::enable`  | `Heartbeat`                          |
| `stream::balance::enable`    | `Balance(<ticker>)`                  |
| `stream::network::enable`    | `Network`                            |
| `stream::swap_status::enable`| `SwapStatus`                         |
| `stream::order_status::enable`| `OrderStatus`                        |
| `stream::orderbook::enable`  | `OrderbookUpdate { topic }`          |
| `stream::fee_estimator::enable` | `FeeEstimation(<ticker>)`         |
| `stream::tx_history::enable` | `TxHistory(<ticker>)`                |
| `stream::shutdown_signal::enable` | `ShutdownSignal`                |

The `network` streamer's request, payload, cadence, platform gate, and
peer-discovery integration are bound in §10.16 (R29–R32). The
tx-history streamer's request, payload, trigger, and coin-family support
are bound in §10.18 (R39–R45). The shutdown-signal streamer's request,
payload, trigger, platform gate, and shutdown interaction are bound in
§10.19 (R46–R51).

**R25.** The balance streamer's activation request MUST carry exactly
two fields: a coin ticker, and an interval in seconds defaulting to 30,
floored at construction time to a minimum of 10. The streamer's
`handle` MUST:

1. Resolve the coin via the central coin-registry accessor; report
   failure on `ready_tx` if not activated, otherwise report ready.
2. Loop, racing a timer against `shutdown_rx`.
3. On the timer branch, call the coin's balance accessor and broadcast
   a normal event only when the spendable or unspendable balance
   differs from the previous emission; broadcast an error event on
   failure.
4. On the shutdown branch, return.

The bound emit-on-change semantics MUST be observed: an unchanged
balance MUST NOT emit. Each emission's JSON message body MUST carry the
ticker, both balance components as decimal strings, and a timestamp in
milliseconds.

**R26.** The non-balance streamers MUST follow the shared activation
contract of R22 and the lifecycle contract of R8-R12, mapping activation
failures into the bound activation error category unless a
streamer-specific section binds a more specific public error surface.
The fee-estimator streamer's per-streamer specifics are bound in §10.17
(R33–R38); the tx-history streamer's per-streamer specifics are bound
in §10.18 (R39–R45); the shutdown-signal streamer's per-streamer
specifics are bound in §10.19 (R46–R51). The substrate MUST NOT expose
any streamer that is not on the R24 list.

## 10.11 Bound Central-Context Wiring

**R27.** The central context MUST carry exactly one new public field of
type `StreamingManager`, initialised to the broker's default. Cloning
the central context (which is itself a cheap shared handle) MUST
observe the same broker instance.

**R28.** The central context MUST expose one new accessor
`event_stream_access_control()` returning the configured CORS origin
string used in R16. The accessor MUST read a single named key
(`event_stream_access_control`) from the central configuration and fall
back to a substrate-defined default suitable for locally-hosted
graphical consumers.

## 10.12 Tests

**T1.** *Single-client delivery.* A test client subscribes to a
streamer that emits a single event; the test asserts the event is
delivered to the client's receiver with the bound origin string.

**T2.** *Last-unsubscribe shutdown.* Two clients subscribe to the same
streamer (registry deduplication path); both unsubscribe in sequence;
the test asserts the streamer task observed its `shutdown_rx` resolve
exactly once, after the second unsubscribe.

**T3.** *Multi-client fan-out.* Three clients subscribe; the streamer
emits one event; the test asserts every client's receiver yields the
same `Arc<Event>` (reference-count fan-out, not payload copy).

**T4.** *Slow-client back-pressure.* One client subscribes but does not
drain its receiver; a second client subscribes and drains. The streamer
emits more than 256 events. The test asserts that the slow client's
receiver caps at 256 and that the fast client receives every event.

**T5.** *Wire-frame literalness.* An end-to-end test connects to
`GET /event-stream?id=1`, activates the heartbeat streamer, and
asserts the response body matches the regular expression
`^data: \{"origin":"HEARTBEAT","payload":.*,"error":(true|false)\}\n\n`
for at least one frame.

**T6.** *Namespace routing.* A dispatcher unit test asserts that
`stream::heartbeat::enable` is routed through the streamer-activation
table and not through the v2 method table, and that a method
`stream::nonsense::enable` returns the dispatcher's "no such method"
error.

**T7.** *Explicit disable scope and response.* Two clients subscribe to
the same streamer. The first client calls `stream::disable` with the
`streamer_id` returned by the enable RPC. The test asserts a standard
mmrpc success response whose payload is `{ "result": "Success" }`, the
first client stops receiving that streamer's events, the second client
continues receiving them, and the streamer's task remains active.

**T8.** *Explicit disable no-op and last-subscriber shutdown.* A client
exists but is not subscribed to an active streamer. Calling
`stream::disable` for that client and streamer MUST succeed without
changing any other client's subscriptions. Separately, disabling the
last subscribed client MUST shut down that streamer exactly once.

**T9.** *Explicit disable error and validation surface.* A
`stream::disable` call for an unknown client MUST fail with a
disable-specific HTTP 400 error. A call for a syntactically valid but
not-running streamer identifier MUST fail with a disable-specific HTTP
400 error. Missing or invalid `client_id` or `streamer_id` fields MUST
fail at request decoding or validation.

**T10.** *All-target disable routing.* A target-matrix dispatcher test
asserts that `stream::disable` is routed through the `stream::`
namespace on native and WebAssembly targets. Native builds additionally
assert that this RPC is independent from the `GET /event-stream`
transport route.

**T11.** *Tx-history activation response and origin.* A supported,
activated coin is used to call `stream::tx_history::enable` through the
`stream::` namespace with a `client_id` and `coin`. The test asserts the
standard mmrpc success envelope whose result has exactly
`streamer_id: "TX_HISTORY:<coin>"`, then injects a new transaction
history record through the coin-history integration point and asserts
the SSE frame origin is the same `TX_HISTORY:<coin>` token. The
activation matrix MUST include every R41 family exposed by the test
target, including Tendermint-family and Z-coin-family coins when those
families are compiled into that target.

**T12.** *Tx-history reactive emission and disable interaction.* A
client enables tx-history for a supported coin, then two transaction
history records are delivered to the streamer as one update batch. The
test asserts two normal SSE events are emitted, one per record, with no
timer-driven empty event between them. After the same client calls
`stream::disable` with `TX_HISTORY:<coin>`, a later history update MUST
NOT be delivered to that client; another client subscribed to the same
streamer, if present, MUST continue receiving events.

**T13.** *Tx-history supported-family producer obligation.* For
Tendermint-family and Z-coin-family coins exposed by a target, a
conformance test MUST enable tx-history streaming, deliver a new
transaction through that family's public history-update path, and assert
that a `TX_HISTORY:<coin>` event is emitted with the R43 or R44 payload
contract. A build exposing either family MUST NOT satisfy this test by
returning HTTP 501 or by accepting activation while no reactive
history-update path can produce events.

**T14.** *Network streamer activation and payload.* A dispatcher test
MUST route `stream::network::enable` through the `stream::` namespace and
return the shared R22 success envelope with `streamer_id: "NETWORK"`.
Request-decoding tests MUST cover defaulted `client_id`, defaulted
network config, explicit `stream_interval_seconds` and `always_send`,
and rejection of unknown `config` fields. A streamer test MUST inject or
mock the peer-discovery snapshot source and assert that the first normal
event uses origin `NETWORK` and carries exactly the five R30 payload
fields. A cadence test MUST assert emit-on-change by default and
emit-every-cycle when `always_send` is true.

**T15.** *Fee-estimator activation, payload, cadence, and errors.* A
dispatcher test MUST route `stream::fee_estimator::enable` through the
`stream::` namespace and return the shared R22 success envelope with
`streamer_id: "FEE_ESTIMATION:<coin>"` for an activated EVM coin.
Request-decoding tests MUST cover required `coin`, required `config`,
defaults from an empty `config` object, `Simple` and `Provider`
estimator selection, and rejection of unknown `config` fields. A streamer
test MUST use a deterministic EIP-1559 estimator and assert normal-event
payload fields and gwei units per R36. A cadence test MUST assert that
two equal estimates still produce two normal events on successive cycles.
Error-path tests MUST assert the R38.1 activation status mapping and
that per-cycle estimation failures emit SSE error events without
stopping the streamer.

**T16.** *Shutdown-signal activation, platform gate, and response.* A
native non-Windows dispatcher test MUST route
`stream::shutdown_signal::enable` through the `stream::` namespace and
return the shared R22 success envelope with
`streamer_id: "SHUTDOWN_SIGNAL"`. Request-decoding tests MUST cover an
empty per-streamer request, omitted `client_id` defaulting to zero, an
explicit `client_id`, and invalid `client_id` validation. Target-matrix
tests MUST assert that the method is unavailable on WebAssembly and
Windows targets.

**T17.** *Shutdown-signal event and termination interaction.* A native
non-Windows integration test MUST enable the shutdown-signal streamer
for a registered client, cover each supported process-termination signal
through a test-controlled signal source, and assert one normal SSE event
for the delivered signal before runtime stop begins. Each frame MUST use
origin `SHUTDOWN_SIGNAL`, `error: false`, and a JSON string payload
carrying the public signal name. A second test MUST assert that if no
client has activated the streamer, receiving the same signal still
initiates runtime stop and does not require an SSE subscriber. Generic
`stream::disable` tests MUST cover removing the final `SHUTDOWN_SIGNAL`
subscriber before any signal arrives.

## 10.13 Deferred Work

**D1.** A WebAssembly-native delivery transport (the WebAssembly target
currently has the broker but no transport adapter; a future substrate
chapter is expected to bind an in-process callback adapter for
embedded WebAssembly consumers).

**D2.** *(Resolved by §10.16, R29–R32.)* Activation of the reserved
`Network` origin tag. Originally deferred (the tag was bound to fix the
wire string while no consumer existed); the consumer now exists, so the
activation method `stream::network::enable`, its request and payload
shapes, its timer-driven emit-on-change cadence, and its ALL-targets
platform gate are bound in §10.16.

**D3.** Per-client authentication and per-client rate limits. The
substrate currently relies on the bound CORS origin (R16) and the
existing JSON-RPC password gate for the activation methods; per-stream
gating is out of scope.

**D4.** Streamer-specific event-history replay (a reconnecting client
currently observes only events that occur after reconnect; replay would
require a per-streamer bounded backlog and a "last-event-id" handshake,
neither of which is bound).

## 10.14 Baseline Verifications

**V1.** The baseline tree MUST be confirmed to contain no
`mm2_event_stream` crate, no `/event-stream` HTTP route, no
`stream::` dispatcher prefix, and no broker field on the central
context. All graphical synchronisation in the baseline tree is
pull-mode through the existing JSON-RPC read surface.

**V2.** The nine activation method names bound in R20 and the
`stream::disable` method bound in R21 MUST be confirmed absent from the
baseline's v2 dispatcher method table. Adding them in the substrate is
a pure surface addition; no baseline method is renamed or repurposed.

**V3.** The nine `StreamerId` wire strings bound in R6 MUST be confirmed
absent from the baseline tree. They are introduced by the substrate
and become part of the GUI-visible contract surface on first release.

## 10.15 External References

- HTML Living Standard, *Server-sent events*,
  <https://html.spec.whatwg.org/multipage/server-sent-events.html> —
  the `text/event-stream` wire format bound in R17.
- WHATWG Fetch, *HTTP Access-Control-Allow-Origin*,
  <https://fetch.spec.whatwg.org/#http-access-control-allow-origin> —
  CORS header bound in R16.
- `tokio::sync` channels (`mpsc`, `oneshot`),
  <https://docs.rs/tokio/latest/tokio/sync/index.html> — the
  asynchronous-channel primitives the trait surfaces in R8 and R10.
- `parking_lot::RwLock`,
  <https://docs.rs/parking_lot/latest/parking_lot/> — the
  non-asynchronous lock bound in R14.
- `async-trait`, <https://crates.io/crates/async-trait> — used by the
  trait definition in R8.
- libp2p gossipsub, <https://docs.rs/libp2p-gossipsub/latest/libp2p_gossipsub/>
  — the peer/topic/mesh introspection surface that dictates the
  `NETWORK` event payload field set bound in R30.
- EIP-1559, *Fee market change for ETH 1.0 chain*,
  <https://eips.ethereum.org/EIPS/eip-1559> — the base-fee /
  priority-fee gas model whose estimate is carried by the fee-estimator
  streamer payload bound in R36.

## 10.16 Bound Network Streamer Activation

This section binds activation of the sixth concrete streamer, whose
origin tag (`Network`, wire string `NETWORK`) is already reserved in
R6. It resolves D2: a consumer for the tag now exists (a graphical
peer-connectivity view), so the previously-deferred activation handler
is bound here. The exact wire method name is `stream::network::enable`.

**R29.** A sixth entry MUST be added to the streamer-activation table
(R20) and routed through the `stream::` dispatcher branch (R19):

| Method name                | Streamer key |
| -------------------------- | ------------ |
| `stream::network::enable`  | `Network`    |

The activation handler MUST follow the shared R22/R26 contract:

- Request: the shared envelope carrying `client_id` (unsigned 64-bit,
  defaulting to 0) flattened beside a single per-streamer
  configuration object named `config`. The `config` object MUST carry
  exactly two optional fields, both supplying a default when omitted:

  | Field                     | Type                   | Default | Meaning                                                                                            |
  | ------------------------- | ---------------------- | ------- | -------------------------------------------------------------------------------------------------- |
  | `stream_interval_seconds` | number (float seconds) | `5.0`   | Delay between successive network-snapshot emissions.                                               |
  | `always_send`             | boolean                | `false` | When `true`, emit every cycle even if the snapshot is unchanged; when `false`, emit only on change. |

  Unknown fields inside `config` MUST be rejected. There is no minimum
  floor on `stream_interval_seconds` (unlike the balance streamer's
  10-second floor in R25).
- Response: the shared R22 envelope — the mmrpc-2.0 `result` carrying
  the activated streamer's `streamer_id` (here the fixed token
  `NETWORK`). Activation-time errors MUST use the streamer-specific
  mapping in R29.1 rather than the generic R23 mapping.

**R29.1.** The network streamer activation error surface is
streamer-specific and overrides the generic R23 status mapping for
activation-time failures: a valid request that cannot activate the
network stream MUST fail with HTTP 400. Missing or invalid request
fields MUST fail during request decoding or validation before any
subscription state is changed.

**R30.** The `NETWORK` event message body MUST be a JSON object with
exactly the following five fields, describing the node's current
gossipsub / peer-connectivity snapshot. The field names are wire-stable
(GUI-visible interop), byte-for-byte:

| Field                      | JSON value                                             | Semantics                                                                            |
| -------------------------- | ------------------------------------------------------ | ------------------------------------------------------------------------------------ |
| `directly_connected_peers` | object: peer-id string → array of multiaddress strings | Peers the node currently holds live transport connections to, with reachable addresses. |
| `gossip_mesh`              | object: topic string → array of peer-id strings        | Per-topic gossipsub mesh membership.                                                 |
| `gossip_peer_topics`       | object: peer-id string → array of topic strings        | Topics each known peer is subscribed to.                                             |
| `gossip_topic_peers`       | object: topic string → array of peer-id strings        | Peers subscribed to each known topic.                                                |
| `relay_mesh`               | array of peer-id strings                               | Peers in the relay mesh.                                                             |

These five values are dictated by the gossipsub introspection surface
of the peer-discovery substrate; this section binds their presence,
names, and JSON shape, not the internal traversal that produces them.

**R31.** The network streamer MUST be self-driven (input type
`NoDataIn`, R8/R10) and timer-paced:

1. On activation it MUST report ready (R9) after attaching to the
   peer-discovery substrate, then begin its emission loop.
2. Each cycle it MUST assemble the R30 snapshot from the peer-discovery
   substrate, then wait `stream_interval_seconds` before the next cycle.
3. Emit semantics MUST be emit-on-change by default: a cycle whose
   snapshot equals the previously broadcast snapshot MUST NOT emit. The
   first cycle always emits (there is no prior snapshot). When
   `always_send` is `true`, every cycle MUST emit regardless of change.
4. The streamer MUST return when its shutdown signal resolves (R9),
   i.e. when its last subscriber leaves (R12 `stop`).

**R32.** Platform gate: the network streamer activation MUST be bound on
ALL targets (native and WebAssembly). It carries no native-only `cfg`
gate, because the peer-discovery substrate it introspects is present on
every target. The implementation MUST be integrated with the
peer-discovery / p2p networking substrate that owns the gossipsub state;
no internal placement or file layout is specified by this chapter.

## 10.17 Bound Fee-Estimator Streamer Activation

This section binds the seventh concrete streamer: a continuous,
timer-paced EIP-1559 fee-per-gas estimate for an EVM coin. It reuses the
broker substrate (R1–R14), the HTTP/wire frame (R15–R18), the namespace
routing (R19–R21), and the shared activation envelope (R22), while
binding a streamer-specific activation error surface in R38.1. Only the
streamer-specific request shape, payload shape, cadence, platform gate,
and activation error mapping are bound here.

**R33.** The substrate MUST ship a seventh concrete streamer providing a
CONTINUOUS EIP-1559 fee-per-gas estimate for an EVM coin, activated by
the wire-stable method `stream::fee_estimator::enable`. Its origin tag
MUST be the `FeeEstimation` variant of R6 carrying the coin ticker as a
dynamic component, with the wire-stable display string
`FEE_ESTIMATION:<ticker>`. Exactly one such streamer runs per distinct
ticker (registry deduplication per R10/R11).

**R34.** The activation request MUST use the shared R22 envelope (a
`client_id` field plus a flattened inner request). The inner request
MUST have exactly the following shape:

| Field    | Type                       | Required | Default            | Notes                                                                 |
| -------- | -------------------------- | -------- | ------------------ | --------------------------------------------------------------------- |
| `coin`   | string (EVM coin ticker)   | yes      | —                  | Resolved via the central coin-registry accessor; a non-EVM or missing coin MUST fail activation. |
| `config` | object (estimator config)  | yes      | (all inner fields default) | Estimator configuration object (R35). Minimal accepted form is the empty object `{}`, which selects all defaults. |

**R35.** The `config` object MUST carry exactly two fields, both
optional with a default, and MUST reject unknown fields:

| Field            | Type                          | Allowed values      | Default  | Meaning                                                                                       |
| ---------------- | ----------------------------- | ------------------- | -------- | --------------------------------------------------------------------------------------------- |
| `estimate_every` | number (seconds, fractional)  | positive            | `15.0`   | Target cadence in seconds between successive re-estimations (R37).                            |
| `estimator_type` | string enum                   | `Simple`, `Provider`| `Simple` | `Simple` = internal historical estimator; `Provider` = external gas-API provider. The provider's name and base URL are taken from the coin's own configuration, NOT from this request. |

There is no request-level floor on `estimate_every`; the only cadence
guard is the effective-sleep gate of R37.

**R36.** Each emitted normal event's JSON payload MUST be the EIP-1559
fee estimate with the following exact field names and value types. All
fee magnitudes MUST be expressed in gwei as the numeric substrate's
decimal representation:

- `base_fee` — next-block base fee per gas.
- `low`, `medium`, `high` — three named priority tiers (low / medium /
  high), each an object carrying:
  - `max_priority_fee_per_gas` — the tip portion of the fee;
  - `max_fee_per_gas` — the total per-gas cap;
  - `min_wait_time` — optional integer milliseconds, nullable;
  - `max_wait_time` — optional integer milliseconds, nullable.
- `source` — string indicator of which estimator produced the values;
  the bound values are `empty`, `simple`, `infura`, `blocknative`.
- `base_fee_trend` — string trend indicator for the base fee.
- `priority_fee_trend` — string trend indicator for the priority fee.
- `units` — fee-unit enum whose only emitted value is `Gwei`.

There is deliberately NO separate `gas_price` field: the EIP-1559 model
expresses cost as `base_fee` plus the per-tier `max_priority_fee_per_gas`
/ `max_fee_per_gas`. On estimation failure the streamer MUST emit an
ERROR event (the R4 error indicator set true) whose JSON payload carries
the failure reason; a failed cycle MUST NOT terminate the streamer.

**R37.** The emit trigger MUST be timer-paced, NOT emit-on-change: on
each cycle the streamer re-estimates and broadcasts unconditionally,
then waits `estimate_every` seconds minus the elapsed estimation time of
that cycle; if the remaining wait falls below a small floor (0.1
seconds) the next cycle begins immediately. This differs from the
balance streamer's emit-on-change contract (R25): the fee-estimator
emits every cycle regardless of whether the estimate changed.

**R38.** The fee-estimator activation MUST be available on ALL targets
(it is an EVM-coin streamer, and EVM support is cross-platform).
Consistent with R15, WebAssembly builds instantiate the broker and
accept this activation even though the native HTTP transport is absent.
The activation handler MUST use the shared envelope of R22 (`client_id`
plus the flattened inner request) and MUST return the activated
streamer's `streamer_id` per the R22 response contract (the
`FEE_ESTIMATION:<ticker>` token). Activation-time errors MUST use the
streamer-specific mapping in R38.1 rather than the generic R23 mapping.

**R38.1.** The fee-estimator activation error surface is
streamer-specific and overrides the generic R23 status mapping for the
following activation-time failures. A missing or inactive `coin` MUST
fail with HTTP 404. An activated non-EVM coin MUST fail with HTTP 501.
A valid request that cannot activate the fee-estimator stream MUST fail
with HTTP 400. Unexpected activation failures MUST fail with HTTP 500.
Missing or invalid request fields MUST fail during request decoding or
validation before any subscription state is changed. Once the streamer
is active, per-cycle estimation failures MUST be emitted as SSE error
events per R36 and MUST NOT terminate the streamer.

## 10.18 Bound Tx-History Streamer Activation

This section binds the eighth concrete streamer: a reactive transaction
history event stream for coin families whose history subsystem can
publish newly discovered transaction records. It reuses the broker
substrate (R1–R14), the HTTP/wire frame (R15–R18), the namespace routing
(R19–R21), and the shared activation envelope (R22–R23) unchanged; only
the streamer-specific coin support, trigger, payload, and error surface
are bound here.

**R39.** The substrate MUST ship a tx-history streamer activated by the
wire-stable method `stream::tx_history::enable`. Its origin tag MUST be
the `TxHistory` variant of R6 carrying the coin ticker as a dynamic
component, with the wire-stable display string `TX_HISTORY:<ticker>`.
Exactly one such streamer runs per distinct ticker (registry
deduplication per R10/R11). This streamer is reactive: it emits only
when the corresponding coin-history subsystem reports newly discovered
transaction history records, not on a timer and not as a full history
snapshot replay.

**R40.** The activation request MUST use the shared R22 envelope (a
`client_id` field defaulting to zero when omitted, plus a flattened
inner request). The inner request MUST carry exactly one required field:

| Field  | Type   | Required | Default | Notes |
| ------ | ------ | -------- | ------- | ----- |
| `coin` | string | yes      | none    | Ticker of an already activated coin whose family supports tx-history streaming. |

On success the method MUST return the shared R22 response envelope: the
mmrpc-2.0 `result` object carrying exactly
`streamer_id: "TX_HISTORY:<ticker>"` and no additional success fields.

**R41.** Activation MUST resolve `coin` through the central coin
registry. The compatibility support set is UTXO-style coins, BCH-family
coins, Qtum-family coins, Tendermint-family coins, and Z-coin-family
coins. For every activated coin in this set, tx-history streaming support
means both accepting `stream::tx_history::enable` and providing the
family's reactive history-update producer required by R42; accepting the
activation as a dormant stream is nonconforming. A missing or inactive
coin MUST fail with HTTP 404. An activated coin outside those families
MUST fail with HTTP 501. A valid request that cannot activate the
tx-history stream MUST fail with HTTP 400. Unexpected activation errors
MUST fail with HTTP 500. Missing or invalid request fields MUST fail during
request decoding or validation before any subscription state is changed.

**R42.** The tx-history streamer MUST use the broker's typed data-input
channel rather than polling. The coin-history subsystem for every family
listed in R41 MUST call the broker's typed send path using the
`TX_HISTORY:<ticker>` streamer identity whenever it detects one or more
new transaction history records for that ticker. Tendermint-family and
Z-coin-family coins are not optional exceptions: if those activated coin
families are present on a target, their history subsystems MUST be wired
to the same reactive publication contract as the UTXO-style, BCH-family,
and Qtum-family paths. The streamer MUST report ready immediately after
the data-input receiver is registered. For each received update batch,
it MUST emit one normal event per transaction record in that batch. If
an update batch is empty, no event is emitted.

**R43.** Normal `TX_HISTORY:<ticker>` event payloads MUST be the same
JSON object shape used for that coin family's single transaction entry
in the public transaction-history RPC surface, not the paginated history
response wrapper. For UTXO-style, BCH-family, Qtum-family, and
Tendermint-family coins, this is the public `TransactionDetails` record:
the payload includes transaction data (`tx_hex`/`tx_hash` for signed
transactions, or the family-specific unsigned/Sia transaction data),
address arrays (`from`, `to`), decimal amount fields (`total_amount`,
`spent_by_me`, `received_by_me`, `my_balance_change`), `block_height`,
`timestamp`, optional `fee_details`, `coin`, `internal_id`, optional
`kmd_rewards`, `transaction_type`, and optional `memo`. For
Z-coin-family coins, the payload is the public Z-history transaction
detail record: `tx_hash`, `from`, `to`, `spent_by_me`,
`received_by_me`, `my_balance_change`, `block_height`,
`confirmations`, `timestamp`, `transaction_fee`, `coin`, and
`internal_id`.

**R44.** Z-coin-family tx-history support is part of the R41
compatibility set and therefore MUST include a producer that forwards
new wallet-visible transaction notifications to this streamer. Those
updates may require resolving wallet notifications into full transaction
detail records before emission. If that resolution fails for a received
update batch, the streamer MUST emit one SSE error event for
`TX_HISTORY:<ticker>` with the R4 error indicator set true and a JSON
object payload containing an `error` field. The failed batch MUST NOT
terminate the streamer. For all families, `stream::disable` MUST
interact with tx-history exactly as it does with every other streamer
under R21: it removes only the named client's subscription to
`TX_HISTORY:<ticker>`, shuts the streamer down only when the last
subscriber leaves, and does not remove other subscriptions for that
client.

**R45.** Platform gate: `stream::tx_history::enable` MUST be available
on ALL targets for every R41 family that the target exposes as an
activated coin. A target that exposes one of those families MUST also
expose the corresponding reactive history-update path required by R42.
Consistent with R15, WebAssembly builds instantiate the broker and
accept the activation even though the native HTTP SSE transport is
absent. Native builds additionally expose resulting events through
`GET /event-stream`; WebAssembly delivery is through the non-HTTP broker
receiver path bound outside this chapter.

## 10.19 Bound Shutdown-Signal Streamer Activation

This section binds the ninth concrete streamer: a reactive process
shutdown notification stream. It reuses the broker substrate (R1–R14),
the HTTP/wire frame (R15–R18), the namespace routing (R19–R21), and the
shared activation envelope (R22), while binding a streamer-specific
platform gate and activation error surface.

**R46.** The substrate MUST ship a shutdown-signal streamer activated by
the wire-stable method `stream::shutdown_signal::enable`. Its origin tag
MUST be the `ShutdownSignal` variant of R6, with the fixed wire-stable
display string `SHUTDOWN_SIGNAL`. Exactly one shutdown-signal streamer
runs per process (registry deduplication per R10/R11).

**R47.** Platform gate: `stream::shutdown_signal::enable` MUST be
available only on native non-Windows targets. It MUST NOT be routed on
WebAssembly targets or Windows targets; calls to the method on those
targets MUST fail through the normal missing-method dispatcher path. On
supported targets, native `GET /event-stream` delivery is available per
R15–R18.

**R48.** The activation request MUST use the shared R22 envelope. The
only accepted request field with streamer semantics is the shared
`client_id` unsigned 64-bit integer, defaulting to zero when omitted.
There are no shutdown-signal-specific request fields. On success the
method MUST return the shared R22 response envelope: the mmrpc-2.0
`result` object carrying exactly `streamer_id: "SHUTDOWN_SIGNAL"` and
no additional success fields.

**R49.** Activation-time failures for this streamer are
streamer-specific and override the generic R23 status mapping. A valid
request that cannot subscribe the client to the shutdown-signal streamer
MUST fail with HTTP 400. This includes an unknown client identifier and
a duplicate subscription by the same client. Missing or invalid request
fields MUST fail during request decoding or validation before any
subscription state is changed.

**R50.** The shutdown-signal streamer MUST be reactive, not timer-paced.
It MUST report ready immediately after its broker data-input receiver is
registered. When the process receives a supported operating-system
termination signal, the runtime MUST first publish that signal name to
the `SHUTDOWN_SIGNAL` streamer if it is active, then begin graceful
runtime stop. If the streamer is not active or has no subscribers, the
runtime MUST still begin graceful stop; absence of an SSE listener MUST
NOT block shutdown.

**R51.** Each normal `SHUTDOWN_SIGNAL` event payload MUST be a JSON
string carrying the public operating-system signal name. The supported
payload values are `SIGINT`, `SIGTERM`, and `SIGQUIT`. The SSE frame
MUST use origin `SHUTDOWN_SIGNAL` and `error: false`. This streamer MUST
emit only in response to an operating-system termination signal delivered
to the process; it MUST NOT emit periodic keepalive events and MUST NOT
replay earlier signals to clients that subscribe after the signal was
handled. Generic `stream::disable` semantics apply unchanged: disabling
the final `SHUTDOWN_SIGNAL` subscription removes the streamer, and a
later process signal is not delivered to SSE clients unless a client has
activated the streamer again before that signal is handled.

## 10.20 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit; chapter 01 (clean-room rules); chapter 31 (the central
  application-context substrate the broker handle of R27 and the
  `event_stream_access_control()` accessor are bound on); the
  chapter-bound identifier set for the broker substrate, the HTTP
  endpoint, the RPC namespace, and the nine concrete streamers;
  public protocol documentation (HTML Living Standard SSE, WHATWG
  CORS, EIP-1559 fee model); the libp2p gossipsub introspection surface
  (peer/topic/mesh enumeration) that dictates the `NETWORK` payload
  field set (R30); public documentation for the asynchronous-runtime and
  lock crates listed in 10.15; upstream clean-channel wire and platform
  facts for `stream::shutdown_signal::enable`.
- *Permitted-input classes used:* baseline source; bound substrate
  identifiers introduced with in-chapter justification; public
  protocol documentation; public crate documentation; dictated-interop
  wire facts (the `stream::network::enable`,
  `stream::fee_estimator::enable`, `stream::tx_history::enable`, and
  `stream::shutdown_signal::enable`, and `stream::disable` method
  strings, their request fields, and the
  `NETWORK` / `FEE_ESTIMATION:<ticker>` / `TX_HISTORY:<ticker>` event
  / `SHUTDOWN_SIGNAL` event field sets — all GUI/third-party-visible
  contract surface).
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* consulted only by the spec-reader role for C8
  clean-channel behaviour extraction; no protected expression is bound
  by this chapter.
