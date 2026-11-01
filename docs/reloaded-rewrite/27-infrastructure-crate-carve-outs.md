# Chapter 27 -- Infrastructure Subsystem Inventory

**Status:** driving-spec

> **One-sentence claim:** the codebase shall maintain a small,
> stable register of cross-cutting infrastructure subsystems
> (error handling, event streaming, typed RPC envelope, long-
> running task framework, state-machine runtime, metrics,
> exact numerics, I/O scaffolding, repository-driven
> configuration, libp2p proxy signing, hardware-wallet
> primitives, shared-reference debug helper, procedural-macro
> derives, the native SQLite persistence gateway, and the
> central per-process application context); each subsystem
> owns no user-visible feature on its own but is depended upon
> by feature-bearing subsystems, and each is bound by a single
> chapter or single binding rule in this chapter.

## 27.0 Executive Summary

The codebase's workspace is organised into two functional
populations:

1. **Feature-bearing subsystems** -- each owns a user-visible
   capability (peer-to-peer networking, coin support, swap
   protocols, wallet identity, RPC dispatch, etc.) and is
   bound by its own dedicated chapter elsewhere in this
   document set.
2. **Cross-cutting infrastructure subsystems** -- each owns
   no user-visible feature on its own but provides a uniform
   substrate that feature-bearing subsystems depend on. This
   chapter is the single register for the second population.

The register exists for two reasons. The first is to give a
reader a single starting point when an unfamiliar
infrastructure substrate is referenced from a feature-bearing
chapter. The second is to bind, in one place, the
architectural rules that apply to that population: what is
"infrastructure" vs what is "feature", how the populations
depend on each other, and what changes to an infrastructure
subsystem require coordinated changes elsewhere.

The fifteen subsystems registered in this chapter cover:

- error handling and its serialisation scaffolding (§27.1);
- server-sent-events fan-out (§27.2);
- typed JSON-RPC envelope (§27.3);
- long-running RPC task framework (§27.4);
- state-machine runtime with event-sourced persistence
  (§27.5);
- metrics surface (§27.6);
- exact-arithmetic numeric type (§27.7);
- filesystem and network I/O scaffolding (§27.8);
- the native SQLite persistence gateway (§27.8A);
- repository-driven configuration (§27.9);
- libp2p proxy signing (§27.10);
- hardware-wallet primitives (§27.11);
- shared-reference debug helper (§27.12);
- procedural-macro derives for boilerplate reduction (§27.13);
- the central per-process application context (§27.13A).

## 27.1 Error Handling and Serialisation

The error-handling substrate defines the codebase's standard
error envelope: a generic carrier wrapping a domain enum plus
optional structured payload, a result alias, a prelude of
conversion combinators (lift, propagate, map, and lift-into-
futures variants), and a JSON-error variant suitable for
logging. A companion procedural-macro substrate emits a
sealed marker gating the derive of the wire-format
adjacent-tagged JSON shape used by the RPC layer (the
`error_type` / `error_data` envelope keys).

R1. **Single error envelope.** Every public RPC handler in
    the codebase shall return either the standard error
    envelope of this substrate or an envelope structurally
    indistinguishable from it on the wire. The conventions
    are bound by [Chapter 4](04-error-aggregation-type-adaptation.md).

R2. **HTTP-status mapping at the handler boundary.** Each
    handler module declares an HTTP-status mapping for its
    error type as part of the handler's own surface.
    Infrastructure crates shall not declare such mappings.

## 27.2 Server-Sent-Events Streaming

The event-streaming substrate owns the SSE fan-out used by
every `stream::*` RPC method. Its bound surface includes:

- A small algebra for events (a typed enum with a serde tag
  set bound by the wire contract, an origin identifier whose
  textual form is part of the wire contract, a payload
  trait).
- A broadcaster handle that producers hold to publish events.
- A behaviour trait that streamers implement (an async
  worker loop with shutdown handling).
- A manager that registers, spawns, and shuts down streamers
  and that fans broadcast events out to subscribed clients
  with bounded per-client back-pressure channels.

R3. **Single fan-out path.** All SSE-shaped streaming surfaces
    in the codebase flow through this substrate. The contract
    is bound by [Chapter 10](10-sse-streaming.md).

R4. **Bounded per-client back-pressure.** Every per-client
    channel in the fan-out shall be bounded; a slow client
    cannot consume unbounded memory.

## 27.3 Typed JSON-RPC Envelope

The RPC-envelope substrate owns the typed wrappers around the
JSON-RPC 2.0-style envelope the codebase exposes: a version
tag, typed wrappers around the `params` / `result` / `error`
fields, and the WASM-only sender used by the cross-thread
bridging glue on the browser target.

R5. **Single envelope.** Every dispatcher entry in the
    codebase shall be typed against this substrate's
    envelope. The dispatcher's own contract is bound by the
    feature-bearing chapter that owns it.

R6. **`task::*` and `stream::*` namespaces are envelope
    extensions.** The typed envelopes for these namespaces
    are part of this substrate; the namespaces themselves are
    bound by §27.4 (tasks) and §27.2 (streams) respectively.

## 27.4 Long-Running RPC Task Framework

The long-running task substrate provides a four-call
lifecycle for any RPC operation that cannot complete inside
a single request:

| Lifecycle phase    | Effect                                                |
|--------------------|-------------------------------------------------------|
| Init               | Schedules the task; returns a task identifier         |
| Status             | Returns one of in-progress / awaiting-user-action /   |
|                    | ok / error (with structured payload)                  |
| User action        | Resumes a task awaiting interactive input (PIN,       |
|                    | passphrase, confirmation, etc.)                       |
| Cancel             | Aborts an in-flight task                              |

R7. **Behaviour trait per task type.** Each long-running RPC
    operation is implemented as a single behaviour trait
    generic over user-action input, awaitable item, error
    type, and intermediate progress payload.

R8. **Public namespace shall converge on `task::<op>::<verb>`.**
    The bound public namespace for long-running tasks shall be
    `task::<op>::<verb>` with `<verb>` drawn from
    {`init`, `status`, `user_action`, `cancel`}. At the time
    of writing many tasks are still exposed under historical
    unprefixed names; this is a deferred convergence (D1).

## 27.5 State-Machine Runtime

The state-machine substrate is the runtime that the V2 swap
state machines sit on top of. Its essence is a behaviour
trait that a state implements once per (current state, next
state) pair plus an auto-trait-and-negative-impl pattern that
makes self-transitions and transitions out of terminal
states into compile errors rather than runtime errors. A
second region of the substrate adds event-sourced persistence
so that an interrupted state machine can be reloaded from
its persisted last-event log and resumed at the boundary
where the interruption left it.

R9. **Compile-time enforcement of legal transitions.** The
    runtime shall use the type system to forbid self-
    transitions and transitions out of terminal states.
    Runtime-only enforcement of legal transitions is not
    acceptable.

R10. **Event-sourced resumption.** The persistent-runtime
     variant shall allow an interrupted state machine to be
     reloaded from its persisted event log and resumed at the
     last persisted boundary.

The runtime's contract is bound by
[Chapter 14](14-state-machine-runtime.md); its consumers are
the V2 swap paths
([Chapter 15](15-swap-v2-utxo-path.md),
[Chapter 16](16-swap-v2-pre-burn-output.md),
[Chapter 17](17-swap-v2-evm-path.md)) and other long-running
flows that require resumable persistence.

## 27.6 Metrics

The metrics substrate is a thin abstraction over a Prometheus
registry on the native build target and a no-op on the
browser build target. Its bound surface includes:

- A registry handle held by the central context.
- A clock handle for periodic counter/gauge/histogram
  updates.
- An initialiser pair: a plain initialiser and an
  initialiser-with-dashboard form that additionally starts an
  HTTP listener for scrape requests and a periodic-snapshot
  logger.
- A JSON-snapshot reader.
- A small set of macros for counter, gauge, and timing
  emission, each supporting both a bare form and a labelled
  form keyed by string label-key / label-value pairs.

R11. **Browser-target no-op.** Every macro in the substrate
     shall compile to a no-op on the browser target so that
     emission sites can be written once without per-target
     gating.

R12. **Dashboard opt-in.** The HTTP listener for scrape
     requests shall be enabled only when explicitly opted in
     via configuration; the default daemon does not expose a
     metrics endpoint.

## 27.7 Exact-Arithmetic Numeric Type

The exact-arithmetic substrate is the codebase's standard
type for prices, balances, and fee calculations. Its core is
a wrapper around an arbitrary-precision signed rational, with
serde adapters for several on-the-wire shapes: a
decimal-string representation, a `{numer, denom}` rational
pair, and a fixed-precision decimal form. Its public surface
includes the wrapper itself, an associated zero/one constant,
standard arithmetic operator implementations (Add, Sub, Mul,
Div, Neg, Pow), ordering implementations, and integer- and
decimal-conversion impls.

R13. **No floating-point for financial paths.** Every fee,
     price, and balance value crossing a wire boundary or
     entering a financial computation shall be expressed in
     this type. Floating-point arithmetic is reserved for
     non-financial code paths (display, logging, debugging).

R14. **Lossless wire shapes.** All three serde wire shapes
     (decimal string, rational pair, fixed-precision
     decimal) shall be lossless round-trips.

## 27.8 Filesystem and Network I/O Scaffolding

Three substrate regions carry the I/O and configuration glue:

- **Native filesystem and file-lock helpers** (native-only)
  -- bound by [Chapter 26](26-cross-platform-and-wasm.md).
- **Dual HTTP / WebSocket / gRPC-WEB transport** (native and
  browser) -- bound by [Chapter 26](26-cross-platform-and-wasm.md).
- **Network-configuration registry** -- bound by
  [Chapter 6](06-network-id-seed-node.md).

R15. **Target-aware indirection in transport.** The
     transport region shall present a single API to callers,
     dispatching to the native or browser implementation
     through compile-time target gating; callers do not
     branch on the build target.

R16. **Single source-of-truth for per-network constants.**
     All per-network constants flow through the network-
     configuration registry of §27.8 / Chapter 6. No
     per-network constant lives outside that substrate.

## 27.8A Native SQLite Persistence Gateway

The native SQLite persistence gateway is the codebase's single
point of contact with the on-disk SQLite engine on native
(non-WebAssembly) targets: an identifier-validation and
pragma-application toolbox, a typed query-builder substrate
covering the common single-table SQL shapes, and an
asynchronous connection facade backed by a dedicated worker
thread. Every native-side persistence consumer in the
workspace -- including the central application-context
substrate of §27.13A, the graphical-user-interface account-
state substrate, the transaction-history and per-protocol
history stores, the Lightning persister, the WalletConnect
session store, and the non-fungible-token table substrate --
reaches the SQLite engine through this gateway rather than
constructing a bare SQLite connection of its own.

R28. **Single native SQL gateway.** Every native-side
     persistence consumer in the codebase shall reach the
     SQLite engine through this substrate. The substrate's full
     contract -- the validation-and-pragma layer, the typed
     query-builder substrate, the asynchronous connection
     facade, and the consumer-routing discipline -- is bound by
     [Chapter 25](25-sql-query-builder.md); the parallel
     WebAssembly persistence substrate is bound by
     [Chapter 26](26-cross-platform-and-wasm.md).

## 27.9 Repository-Driven Configuration

The repository-driven-configuration substrate is the
codebase's read-only Git client. It abstracts a remote
repository (a Git host, an owner, a repo, a branch, a path),
fetches files, lists directories, and decodes JSON payloads
on the way out. Its purpose is to let coin lists, electrum-
server lists, faucet lists, and similar configuration tables
be served from a public Git repository instead of from a
centralised HTTP service: an operator changes a JSON file in
the configuration repository, pushes, and the daemon picks
up the change on its next refresh interval.

R17. **Read-only.** The substrate is strictly read-only. No
     write path through this substrate is acceptable; writes
     to configuration repositories happen out-of-band
     through normal Git tooling.

R18. **JSON payloads only.** The substrate's typed-decode
     surface handles JSON. Other formats (YAML, TOML, etc.)
     are not in scope for this substrate.

## 27.10 libp2p Proxy Signing

The libp2p-proxy-signing substrate authenticates clients to
libp2p proxy endpoints without a shared secret. A request is
wrapped in a typed envelope (a fixed magic prefix, the
target URI, a body-size hash, the libp2p-encoded public key,
and an expiry timestamp), signed with the client's libp2p
keypair, and validated on the proxy by re-deriving the
prefix and verifying the signature with the embedded public
key.

R19. **Magic-prefix constant.** The envelope's magic prefix
     is a bound constant on the wire; changing it requires
     coordinated change across every proxy participant.

R20. **Expiry-bounded validity.** Every envelope carries an
     expiry timestamp; the proxy shall reject envelopes
     whose expiry is in the past.

The substrate's wire-format contract is bound by
[Chapter 28](28-libp2p-modernization.md).

## 27.11 Hardware-Wallet Primitives

Three substrate regions cover the hardware-wallet stack:

- **Shared primitives and transport contracts** -- a
  derivation-path type, an ECDSA-curve enum, an abstract
  transport trait. Bound by
  [Chapter 26 §26.7](26-cross-platform-and-wasm.md#267-hardware-wallets-and-other-native-only-stacks).
- **Trezor protocol** -- protobuf protocol client, session
  abstraction, user-interaction trait (PIN and passphrase
  prompts), request/response types for the swap-relevant
  subset of Trezor messages, with both native and browser
  target tables.
- **Ledger protocol** -- present at the time of writing as a
  scaffold for an analogous Ledger client; not yet wired
  into the coin or swap paths.

R21. **Single derivation-path type.** All chain backends
     accept the same derivation-path type for hardware-
     wallet operations; per-backend types are not acceptable.

R22. **User-interaction trait at the hardware boundary.**
     The host UI implements the user-interaction trait once;
     hardware-wallet sessions consult it through that single
     surface for PIN and passphrase prompts.

## 27.12 Shared-Reference Debug Helper

The shared-reference debug substrate is a small wrapper
around the standard library's reference-counted shared-
ownership type. When its `enable` feature is on, every clone
and drop is instrumented with the caller's source location
and a leak summary is printed on process exit. When the
feature is off, the wrapper compiles down to the standard
type with no overhead.

R23. **Compile-time cost neutrality when disabled.** The
     wrapper shall add no runtime cost when its `enable`
     feature is off; this is what makes it usable as the
     default shared-ownership type for long-lived
     subsystems in the codebase.

## 27.13 Procedural-Macro Derives

The procedural-macro derives substrate carries the
boilerplate-reduction derives the codebase uses for its
tagged enums: a `From`-for-newtype derive, a `From`-via-
to-string derive for string-keyed variants, a custom-trait
delegation derive, and a unit-variant listing derive.
Companion derives for the wire-format error envelope live
alongside the error substrate (§27.1).

R24. **Derives are additive.** New derives in this substrate
     shall not change the semantics of any existing derive.
     Existing call sites do not need to be revisited when
     the substrate is extended.

## 27.13A Central Application Context

The central application-context substrate is the codebase's
single per-process shared-state handle: an owned-state record,
a reference-counted handle around it, a once-set lazy-
initialisation field substrate for values pinned once at
startup and read many times thereafter, and a sub-context slot
substrate through which every other feature-bearing subsystem
registers and fetches its own per-process shared state. Every
workspace consumer that needs shared state across the process
boundary -- the SQLite gateway of §27.8A, the SSE fan-out of
§27.2, the state-machine runtime of §27.5, the metrics surface
of §27.6, and every feature-bearing subsystem with per-process
state of its own -- fetches it through this substrate's single
handle.

R29. **Single central context.** Every workspace consumer that
     needs per-process shared state shall fetch it through this
     substrate's single handle rather than through process-
     global state, static mutable variables, or thread-local
     storage. The substrate's full contract -- the owned-state
     record and handle pair, the once-set lazy-initialisation
     field substrate, the sub-context slot substrate, and the
     construction-and-lifecycle discipline -- is bound by
     [Chapter 31](31-central-application-context.md).

## 27.14 Population Boundary and Cross-Population Rules

R25. **Single-population membership.** A subsystem is in
     exactly one of the two populations of §27.0. A
     subsystem that grows a user-visible feature is
     reclassified as feature-bearing and acquires its own
     chapter; a subsystem that loses its last user-visible
     feature is reclassified as infrastructure and acquires
     a section here.

R26. **Infrastructure does not depend on features.**
     Subsystems in the infrastructure population shall not
     depend on any feature-bearing subsystem. Feature-bearing
     subsystems may depend on infrastructure subsystems
     freely.

R27. **Each register entry has exactly one binding chapter.**
     Each section above either contains its own binding
     rules in full or names the chapter that contains them.
     Two chapters shall not both claim to bind the same
     infrastructure subsystem.

## 27.15 Deferred Work

D1. **Convergence of long-running RPC method names** under
    the `task::<op>::<verb>` namespace (R8). At the time of
    writing many tasks are still exposed under historical
    unprefixed method names.

D2. **Ledger protocol activation** (§27.11). The Ledger
    scaffold shall be wired into the coin and swap paths or
    explicitly removed from this register. Leaving it
    indefinitely as an inactive scaffold is not acceptable.

D3. **Dependency-graph rendering.** The cross-population
    rule R26 is currently verifiable only by reading the
    workspace's Cargo manifests. A rendered dependency
    diagram would make the rule directly inspectable.

D4. **Reclassification reviews.** R25 is enforced editorially
    at the time of writing. A periodic review of every
    subsystem's classification (feature-bearing vs
    infrastructure) is desirable, particularly when a new
    user-visible RPC handler lands.

## 27.16 External References

- The Prometheus exposition format (the wire shape the
  metrics substrate's HTTP listener produces).
- The libp2p signing primitives (the cryptographic substrate
  on which proxy signing of §27.10 is built).
- The arbitrary-precision rational and decimal types
  underlying the exact-arithmetic substrate of §27.7.
- The standard JSON-RPC 2.0 envelope shape extended by the
  typed-envelope substrate of §27.3.
- The async-runtime model on which the SSE fan-out, the
  long-running task framework, and the state-machine runtime
  are built.

## 27.17 Baseline Verifications

The following are verifiable from the baseline state defined
in [Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree carries a subset of the infrastructure
    subsystems registered here: error handling, dual
    transport (native side only), typed RPC envelope, native
    filesystem helpers, native SQL helpers, the long-running
    task framework, hardware-wallet primitives, the Trezor
    protocol, the Ledger scaffold, the central context, and
    the procedural-macro derives. Verifiable by directory
    listing of the baseline tree
    (`git ls-tree c1d46c0c1592faa0860f704008b2b2381bc3840f`).

V2. The baseline tree does not carry: the event-streaming
    substrate, the state-machine runtime, the metrics
    substrate, the exact-arithmetic type, the
    repository-driven-configuration substrate, the libp2p
    proxy-signing substrate, the network-configuration
    registry, or the shared-reference debug helper.
    Verifiable by the same directory listing plus tree-wide
    `git grep` for the bound surface identifiers of each
    against the baseline.

V3. R26 (infrastructure does not depend on features) holds
    against the baseline tree's infrastructure subset; the
    rule is therefore a continuation of an existing
    architectural posture, not a new constraint.

## 27.18 Provenance Footer

- *Status:* driving-spec.
- *Version:* v2.
- *Verified against:* baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`; baseline
  infrastructure inventory verified via
  `git ls-tree c1d46c0c1592faa0860f704008b2b2381bc3840f`
  and tree-wide `git grep` for the bound surface
  identifiers of each subsystem against the baseline; the
  publicly-documented Prometheus exposition format; the
  libp2p signing primitives; the arbitrary-precision
  rational and decimal numeric types; the JSON-RPC 2.0
  envelope shape; the async-runtime model on which the
  streaming, task, and state-machine substrates are built.
- *Forbidden corpus:* not consulted.
