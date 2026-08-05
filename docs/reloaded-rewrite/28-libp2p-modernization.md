# Chapter 28 — P2P Substrate Consolidation

**Status:** driving-spec.

This chapter binds the consolidated peer-to-peer substrate: the single
crate boundary, the composed network behaviour and its deliberately
constrained sub-behaviour set, the vendored relay-mesh-aware gossipsub
extension, the bound topic-naming and application-payload-signing surface,
the transport stack per build target, and the discovery / bootstrap /
mesh-maintenance discipline. It also binds, as **required ports**
(§28.9A), three post-baseline application-level P2P behaviours
reloaded must gain: the peer-connection health-check RPC, the
network time-synchronisation peer-admission check with legacy-compatible
fallback, and expirable pubkey bans.

## 28.1 Executive Summary

The baseline tree ships four cooperating P2P substrates: a glue crate
holding the composed behaviour, a vendored relay-mesh-aware
gossipsub crate, a vendored floodsub crate, and a separate peer-discovery
crate. Each contributes a piece of the composed network behaviour that the
application layer drives.

This chapter binds a single consolidated P2P substrate that exposes the
same composed behaviour through one crate boundary. The vendored
gossipsub and floodsub implementations and the peer-exchange primitives
all become submodules of the consolidated substrate. The application
layer and every other substrate that needs P2P facilities targets the
single consolidated crate.

This refresh **also** binds a libp2p dependency change. Reloaded's
baseline pins the *public* upstream `libp2p/rust-libp2p` git repository at
revision `ef2afcd4` (a ~0.45-era commit). That revision transitively
pulls a set of advisory-flagged crates (`webpki`, `rustls`,
`rustls-webpki`, `ring`, `mio`, `idna`, `remove_dir_all`, `owning_ref`).
The current corpus has moved off that pin entirely: it depends on the
`KomodoPlatform/rust-libp2p` **fork** at tag `k-0.52.12` (a ~0.52.x
lineage). This substrate binds that fork/tag as the target dependency for
reloaded, so the earlier chapter statement that the consolidation is
"*not* a libp2p version bump" is **corrected**: adopting the corpus's P2P
substrate is *both* a crate-layout consolidation *and* a libp2p version
bump to the fork. The exact dependency binding, the per-target feature
flags, and their consequences are bound in §28.1A (R0a–R0d).

Beyond the dependency change, the consolidation is a crate-layout
consolidation plus the small set of post-substrate-introduction additions
that hang off it: the proxy-signature substrate (Chapter 27), the
network-id scoping substrate (Chapter 06), and the helpers that used to
live in the glue crate (relay-address parser, peers-exchange wrapper,
ping-with-disconnect wrapper, request-response wrapper, swarm runtime
helpers, IP helpers).

> **Upstream divergence (informative).** In the current corpus the
> relay-mesh gossipsub extension and the flood-based pub/sub protocol are
> delivered **by the `KomodoPlatform/rust-libp2p` fork itself** — enabled
> through the fork's `gossipsub` and `floodsub` feature flags — rather
> than as in-tree vendored crates. The relay-mesh API additions
> (`i_am_relay` configuration, the relay-mesh accessor, and the
> `IAmRelay` control message) are part of that fork's public gossipsub
> surface. Reloaded currently vendors gossipsub in-tree because its
> ~0.45-era public pin does not carry the extension. When reloaded adopts
> the fork per §28.1A, the in-tree vendored gossipsub SHOULD be dropped in
> favour of the fork's `gossipsub` feature; whether to retain any in-tree
> gossipsub during a transition is an implementation choice (see open
> question OQ1 in §28.11A). The relay-mesh *functional* semantics bound in
> R10–R14 are identical either way.

## 28.1A Bound External libp2p Dependency

**R0a.** The substrate MUST depend on the `KomodoPlatform/rust-libp2p`
fork, pinned by the git tag `k-0.52.12`, with `default-features = false`.
This replaces reloaded's baseline pin of the public
`libp2p/rust-libp2p` repository at revision `ef2afcd4` (~0.45). The pin
MUST be declared once as a workspace dependency and referenced by member
crates with `workspace = true`, rather than repeated per crate.

**R0b.** The per-build-target feature set enabled on the fork is bound:

| Target | Bound libp2p feature set |
| ------ | ------------------------ |
| Native (`cfg(not(target_arch = "wasm32"))`) | `dns`, `identify`, `floodsub`, `gossipsub`, `noise`, `ping`, `request-response`, `secp256k1`, `tcp`, `tokio`, `websocket`, `macros`, `yamux` |
| WASM (`cfg(target_arch = "wasm32")`) | `identify`, `floodsub`, `noise`, `gossipsub`, `ping`, `request-response`, `secp256k1`, `wasm-ext`, `wasm-ext-websocket`, `macros`, `yamux` |

The runtime-transport feature naming follows the fork's 0.52.x scheme
(`dns` + `tcp` + `tokio`), *not* reloaded's baseline 0.45-era naming
(`dns-tokio` + `tcp-tokio`). The `gossipsub` and `floodsub` protocol
families are now supplied by the fork's feature flags (see the §28.1
divergence note). The `macros` feature is required for the
`#[derive(NetworkBehaviour)]` used by R6.

**R0c.** The `identify` feature is enabled at the crate level on both
targets even though Identify is deliberately absent from the composed
behaviour of R6/R7. Enabling the feature flag is not a violation of
R7: R7 constrains the *composed behaviour*, and Identify MUST NOT appear
as a sub-behaviour of the composed `NetworkBehaviour`. The feature is
enabled to satisfy other substrate consumers (peer-address bookkeeping /
the central-context substrate) that link the fork with `identify`.

**R0d.** The motivating outcome of R0a is bound as a requirement: moving
to the `k-0.52.12` fork MUST eliminate reloaded's transitive dependency
on the advisory-flagged crates pulled by the ~0.45 pin (`webpki`,
`rustls`, `rustls-webpki`, `ring`, `mio`, `idna`, `remove_dir_all`,
`owning_ref`). If adopting the fork leaves any of those advisories
unresolved, that residue MUST be recorded as an open question rather than
silently accepted.

## 28.2 Subsystem Shape

The consolidated substrate exposes a small public surface (R5 below) and
keeps everything else internal. The composed network behaviour is a
five-element `NetworkBehaviour` derive over the canonical libp2p protocol
families plus baseline-existing substrate wrappers named in R6. The wrapper
type spellings are retained only under the R11 baseline-existing identifier
carve-out; they are not wire bytes.

The libp2p protocol families *not* present in the composed behaviour are
themselves bound (R7): no Kademlia DHT, no mDNS, no Identify, no Relay
(v1 or v2), no DCUtR, no AutoNAT. Peer discovery is bootstrap-list-plus-
peers-exchange-only; NAT traversal is solved structurally (R24) by
always having a small set of globally-routable relay nodes in the
bootstrap list.

The gossipsub used by the substrate carries a relay-mesh extension that
the standard gossipsub specification does not have (R10–R13). This
extension is the structural reason the substrate cannot use a plain
upstream gossipsub: in reloaded's baseline it is supplied by an in-tree
vendored copy, and in the target fork (§28.1A) it is supplied by the
fork's `gossipsub` feature. Either way the relay-mesh semantics of
R10–R14 apply.

## 28.3 Bound Crate Boundary

**R1.** The substrate exposes exactly one crate boundary to all
downstream consumers. The composed behaviour, the vendored gossipsub
implementation, the vendored floodsub implementation, the peer-exchange
protocol, the ping-with-disconnect wrapper, the request-response
wrapper, the swarm runtime helpers, the relay-address parser, and the
IP-detection helpers all live inside this single crate.

**R2.** The substrate's submodule layout (one submodule per concern) is
not part of the contract; downstream consumers MUST consume the
re-exported public surface from the crate root (R5) and MUST NOT
import from submodule paths.

**R3.** Application crates that previously imported from any of the
four baseline P2P substrates MUST be updated to import from the
consolidated substrate. No application-side code path may retain a
direct import of a deleted baseline P2P crate.

**R4.** A single baseline P2P substrate (the peer-discovery substrate)
MAY be retained on disk as a workspace member with no active consumer.
Its retention is purely for repository-history continuity (`git log`
and `git blame` against pre-substrate-introduction code). No
post-substrate-introduction consumer MUST depend on it.

## 28.4 Bound Public Surface

**R5.** The consolidated substrate's crate root MUST re-export
exactly the following named surface:

- the swarm-spawning entry point, its associated error type, and its
  configuration enums — including the relay-vs-client distinction
  carried on a bound `NodeType` enum, and the optional TLS bundle type
  for WSS transports;
- the gossipsub event, message, and message-id types the consumer
  matches on;
- a small libp2p-identity re-export — `PeerId`, `Multiaddr`, and the
  secp256k1 public-key wrappers;
- the `PeerAddresses` type returned by the peer-exchange protocol;
- the `RelayAddress` parser used to decode bootstrap entries from
  configuration;
- the bound application-payload signing pair `encode_and_sign` /
  `decode_signed` defined in §28.7;
- the `pub_sub_topic` helper and the `TOPIC_SEPARATOR` constant defined
  in §28.7.

Every other symbol in the substrate MUST be private to the crate.

## 28.5 Bound Composed Behaviour

**R6.** The composed network behaviour MUST be a `NetworkBehaviour`
derive over exactly five sub-behaviours, with bound substrate role for
each:

| Sub-behaviour                  | R11 basis                        | Bound role                                                     |
| ------------------------------ | -------------------------------- | -------------------------------------------------------------- |
| `Gossipsub` (vendored)         | public libp2p / vendored surface | Mesh-based publish-subscribe for orderbook and swap traffic.   |
| `Floodsub`                     | public libp2p / vendored surface | Flood-based publish-subscribe for the bound peers topic.       |
| Generic request-response channel | baseline-existing capability     | Direct one-shot application RPC over the mesh; not required for baseline seed/relay admission. |
| Peer-exchange channel            | baseline-existing capability     | Request-response over a bound protocol identifier (R8).         |
| Disconnecting ping channel       | baseline-existing capability     | Ping wrapper that forces a disconnect on consecutive failures. |

**R7.** The composed behaviour MUST NOT include any of the following
libp2p protocol families: Kademlia DHT, mDNS, Identify, Relay (v1 or
v2), DCUtR, AutoNAT. Their absence is a substrate contract.

**R8.** The peer-exchange request-response channel MUST negotiate exactly
one protocol identifier: `/peers-exchange/2`. The negotiation offer set is
the singleton ordered list `["/peers-exchange/2"]`; this surface MUST NOT
offer `/peers-exchange/1` and MUST NOT require remote support for
`/peers-exchange/1`.

The peer-exchange wire payload contract is also bound:

- request payload: msgpack encoding of
  `PeersExchangeRequest::GetKnownPeers { num }`;
- responder behaviour: if `num > 20`, omit the response; otherwise answer
  with msgpack encoding of `PeersExchangeResponse::KnownPeers { peers }`;
- `peers` payload shape: map of peer-id to address-set, where peer-id is
  serialized as raw peer-id bytes and each address is a libp2p multiaddr;
- response bound: no more than 20 peer entries in `peers` per response.

A peer that does not negotiate `/peers-exchange/2` MUST NOT be disconnected
or excluded from the relay mesh solely for that reason; peer-exchange is an
address-discovery aid, while relay usefulness is determined by the transport
connection plus relay advertisement state in R10-R12.

**R8a.** The generic request-response channel MUST negotiate exactly one
protocol identifier: `/request-response/2`. The negotiation offer set is the
singleton ordered list `["/request-response/2"]`; this surface MUST NOT
offer `/request-response/1` and MUST NOT require remote support for
`/request-response/1`.

**R8b.** The two channels in R8 and R8a MUST use the same framing and
serialization contract: each request and response is one
length-prefixed libp2p request-response frame whose payload is msgpack
(`rmp-serde`) bytes for the channel's bound request/response type. The
maximum accepted or emitted frame payload is `1024 * 1024 - 100` bytes.

**R9.** The ping wrapper MUST disconnect a peer after a bounded number
of consecutive ping failures (the exact count is a substrate-internal
tuning parameter, not bound here), as opposed to merely logging the
failure.

## 28.6 Bound Gossipsub Relay-Mesh Extension

**R10.** The vendored gossipsub behaviour MUST carry, on top of the
standard gossipsub state, five additional pieces of state:

- a *connected-relays* set — peers that have advertised themselves
  with the substrate's `IAmRelay` control message;
- a *relay-mesh* map — the subset of connected relays the local node
  treats as its mesh peers, with per-peer counters;
- a *reverse-mesh* set — the relays that have included this node in
  *their* mesh;
- an *explicit-relay* list — relays pinned by configuration and never
  evicted from the mesh;
- a relay-mesh-maintenance timer running on a bound 10-second
  interval.

**R11.** The relay-mesh-maintenance tick MUST:

1. fill the relay-mesh up to the substrate's configured low watermark
   (`mesh_n_low`) by drawing from connected relays not currently in
   the mesh;
2. prune the relay-mesh back to the high watermark (`mesh_n_high`)
   when it exceeds it;
3. preserve every entry in the explicit-relay list against eviction.

**R12.** The substrate's `IAmRelay` control message is bound: nodes
that act as relays MUST emit it; nodes that act as clients MUST NOT.
The configuration flag that selects which role applies is bound as a
boolean `i_am_relay` field on the substrate's gossipsub configuration
surface.

**R13.** The substrate's gossipsub configuration surface MUST further
expose:

- a content-addressed message-id function that hashes payload-plus-
  sequence-number, so duplicate payloads collapse to a single
  message id;
- the three mesh-size watermarks (`mesh_n_low`, `mesh_n`,
   `mesh_n_high`), with substrate-level values differing between
   client and relay roles. The bound per-role triples are
   `(mesh_n_low, mesh_n, mesh_n_high) = (4, 8, 12)` for a relay node
   and `(2, 4, 6)` for a client node, selected on the `i_am_relay`
   flag of R12. Any watermark triple chosen MUST satisfy the gossipsub
   configuration invariant `mesh_n_low ≤ mesh_n ≤ mesh_n_high` and the
   outbound-mesh constraint that the effective `mesh_outbound_min`
   does not exceed `mesh_n_low` (the fork's config builder rejects
   triples that violate these);
- a manual-propagation flag — when set, the consumer is responsible
  for invoking the substrate's propagate-message method after
  validating a message, which gives the application the ability to
  drop invalid messages before forwarding;
- a maximum transmit size bound at slightly under 1 MiB.

**R14.** The substrate MUST NOT enable the peer-scoring / reputation
extension present in the standard gossipsub implementation. Abusive-peer
handling falls to the consumer's manual-disconnect logic and to R9's
force-disconnect. (The fork's gossipsub may expose peer scoring as an
optional capability; the substrate contract is that it stays
unconfigured.)

## 28.7 Bound Topic and Application-Payload Signing Surface

**R15.** Pub/sub topic strings MUST be constructed by the bound
`pub_sub_topic(prefix, topic)` helper. The topic separator MUST be
bound as the single byte `/`, exposed as the `TOPIC_SEPARATOR` constant
on the substrate's public surface. The resulting topic string is the
verbatim concatenation `<prefix><TOPIC_SEPARATOR><topic>`.

**R16.** Application payloads published on the mesh MUST be wrapped in
a secp256k1-signed envelope before publication. The substrate exposes
exactly two functions for this:

- `encode_and_sign<T: Serialize>(message: &T, secret: &[u8; 32]) ->
  Vec<u8>` — msgpack-encodes the payload, SHA-256 hashes the encoded
  bytes, signs the hash with the supplied secp256k1 secret, and packs
  `{ pubkey, signature, payload }` back into a msgpack envelope.
- `decode_signed<'de, T: Deserialize<'de>>(encoded: &'de [u8]) ->
  Result<(T, Signature, PublicKey), _>` — the inverse: parses the
  envelope, verifies the signature against the embedded public key,
  and returns the payload alongside the verified signature and public
  key.

**R17.** This application-payload signing surface is bound as *separate*
from the proxy-signature substrate of Chapter 27. The two operate on
different key spaces (raw secp256k1 here; libp2p-identity keypairs for
proxy signing) and address different threat models (mesh-message
authenticity here; HTTP-relay request-authentication for proxy signing).
Unification of the two signing surfaces is recorded in §28.10 as
deferred.

## 28.8 Bound Transport and Upgrade Stack

**R18.** The transport stack is target-dependent and is bound per
target:

| Target  | Bound transport                                                                                       |
| ------- | ----------------------------------------------------------------------------------------------------- |
| Native  | TCP plus DNS; optional WebSocket / WSS transport when a WSS port and TLS bundle are configured.        |
| WASM    | Browser WebSocket via the libp2p WASM FFI substrate.                                                  |
| Testing | An in-process memory transport for the substrate's own integration tests.                              |

**R19.** The native libp2p identity used to derive the node's
`PeerId` MUST be an Ed25519 libp2p identity. This identity is the
transport-level peer identity authenticated by the libp2p handshake
and is separate from the secp256k1 key material used for the
application-payload signing surface in §28.7.

**R20.** The libp2p connection upgrade pipeline is bound to be
uniform across all targets: multistream-select V1 negotiation,
Noise XX security, yamux stream multiplexing, and a 20-second
upgrade timeout. mplex MUST NOT be used for the GLEEC-compatible
handshake path.

**R21.** Bootstrap address input is source-dependent:

- native distributed seed-node entries are bare host strings, either
  IPv4 literals or DNS names, not full multiaddrs. The caller maps
  each host to the TCP P2P port derived from the active netid before
  handing addresses to the substrate;
- WSS is optional native transport support. When configured, WSS
  listeners and dials use the configured WSS port and TLS bundle
  rather than replacing the netid-derived TCP P2P port for bare
  native seed hosts;
- explicit operator bootstrap entries MAY use standard libp2p
  multiaddr forms for TCP, DNS, WebSocket, or WSS as supported by
  the configured target transport;
- `/memory/<port>` addresses are reserved for in-process test mode
  and MUST NOT be treated as production seed-node addresses.

## 28.9 Bound Discovery, Mesh Maintenance, NAT

**R22.** The substrate MUST NOT attempt open-internet peer discovery.
The bootstrap list supplied by the daemon is the bound source of truth
for initial peer addresses. In production that list is expected to come
from operator-provided `seednodes` in `MM2.json`; a compiled registry
fallback may contribute entries, but compiled production seed nodes are
not required by the substrate contract.

**R23.** At swarm-spawn time the substrate MUST:

1. normalise the configured bootstrap list into libp2p dial
   addresses using the address rules in R21;
2. dial up to `mesh_n` random entries; if the normalised list is
   empty, start the swarm without dialing any relay;
3. start a 10-second maintenance timer running R11 plus a periodic
   peers-exchange request to a random connected relay on a bound
   300-second interval after a bound 20-second initial delay.

Dial failures for configured seed addresses MUST be diagnosable by
the attempted address and a failure reason. A failed seed dial MUST
NOT by itself make swarm startup fail after the local transport and
listeners have been created.

**R24.** NAT traversal is bound to a *structural* solution: relay
nodes MUST be deployed at globally-routable addresses, and client
nodes that sit behind NAT reach the mesh exclusively via at least one
relay. The substrate MUST NOT advertise non-routable listener
addresses to peers; the bound `ip_helpers::is_global` predicate is
applied to listener announcements.

**R25.** A client node with no configured or reachable relay peers SHALL
remain in an empty relay-mesh state until at least one relay connection is
established. The relay-mesh maintenance loop may report that the mesh is
below its low watermark, but this condition is diagnostic rather than a
startup failure. While the relay mesh is empty, orderbook and swap pub/sub
traffic cannot reach the wider network through this substrate.

**R26.** A node configured as a relay may start with an empty bootstrap
list and act as the first reachable relay for a deployment. Other nodes
must receive that relay's address through `seednodes` or another
operator-controlled bootstrap channel before they can join its mesh.

**R27.** Connection handling MUST keep relay state in sync with the
connection lifecycle. When a connected peer advertises itself as a
relay, it is eligible for the connected-relays set and relay-mesh
maintenance of R10-R11. When a relay disconnects or is pruned, relay
state MUST be updated so later maintenance ticks do not keep a stale
mesh entry. Failed dials for seed addresses or peer-exchange addresses
MUST be logged or otherwise exposed to diagnostics with the attempted
address and failure reason; they MUST NOT be converted into a fatal
startup condition merely because no relay mesh has formed yet.
Peers that remain connected and advertise as relays remain eligible for
relay-mesh maintenance even when peers-exchange requests to them time
out, fail, or report unsupported protocol negotiation.

## 28.9A Required Port — Peer Health-check, Time-sync Admission, Expirable Bans (driving-spec)

**STATUS.** The three behaviours in this section are post-baseline
upstream additions that hang off the consolidated substrate. They
are **required ports in reloaded**. RP1 and RP3 are binding additions.
RP2 is binding as a reloaded enhancement, but it is **not** a baseline
seed/relay admission precondition: compatibility with legacy/GLEEC seed
or relay peers requires the non-fatal fallback specified in RP2.

### 28.9A.1 RP1 — Peer connection health-check RPC (implemented in reloaded)

**RP1.** A public top-level JSON-RPC v2 method
`peer_connection_healthcheck` MUST be added. It answers whether a
named peer is currently reachable on the mesh.

- **Request:** an object with a single field `peer_address` — the
  string rendering of the target peer's libp2p peer id.
- **Response:** a bare JSON boolean — `true` if the peer
  acknowledged within the timeout (or is the local node itself),
  `false` otherwise.
- **Behaviour:** if `peer_address` equals the local node's own
  peer id, return `true` immediately. For every other target, the
  externally observable protocol action MUST be a health-check
  probe signed with the §28.7 application-payload signing surface
  and published on the dedicated per-peer health-check pub/sub
  topic derived from the target peer address by the §28.7 topic
  construction rule. These are non-conditionable interoperability
  requirements: the signed envelope, target-peer binding,
  per-peer topic, bounded timeout, and acknowledgement semantics
  MUST NOT be made optional or replaced by another channel. The
  call returns `true` only if a valid acknowledgement for that
  probe arrives before the bound timeout (the health-check message
  expiry), and `false` otherwise.
- **Responder side:** a node receiving a health-check probe on its
  health-check topic MUST verify the signed envelope before
  responding. When a verified probe targets that node, the node
  MUST publish a health-check acknowledgement on the same topic.
  Invalidly authenticated probes or probes for another target MUST
  NOT produce a positive acknowledgement.
- **Errors:** failures MUST surface through the project's typed-
  error envelope (`error_type` / `error_data`) with wire tokens
  distinguishing a probe-generation failure, a probe-encoding
  failure, and a generic internal failure; all map to server-error
  (500). The human-readable message wording is NOT part of the
  contract.

**RP1 acceptance:** a caller can ask `peer_connection_healthcheck`
for a connected peer and get `true`, for an unreachable/unknown
peer get `false` after the timeout, and for its own peer id get
`true` immediately.

### 28.9A.2 RP2 — Network time-synchronisation peer admission (implemented in reloaded)

**RP2.** Reloaded nodes MUST implement a peer-clock check and SHOULD
attempt it immediately after a connection is established when the
application feature is enabled. The check guards swap timing assumptions
that depend on near-synchronised clocks, but it is a post-baseline
application behaviour rather than a legacy/GLEEC seed/relay
compatibility requirement.

- **Compatibility boundary:** GLEEC-compatible seed/relay peers are not
  required to support the generic request-response protocol used for
  this check. For the baseline-compatible reloaded surface, that generic
  request-response protocol identifier is `/request-response/2`, but
  support for it MUST NOT be treated as a condition for keeping a
  seed/relay connection.
- **Mechanism:** when the generic request-response protocol is
  available, the node issues a query over the substrate's
  request-response sub-behaviour (§28.5 R6) asking the newly-connected
  peer for its current UTC timestamp (Unix epoch seconds). The request
  payload MUST be msgpack encoding of the protocol's network-info
  UTC-timestamp query variant.
  A supporting peer replies with a msgpack-encoded unsigned epoch-seconds
  value (`u64`).
- **Admission rule:** when the peer returns a well-formed timestamp, the
  node compares it to its own UTC time. If the absolute difference is
  within the bound maximum gap, the peer is admitted. If the peer returns
  a well-formed timestamp outside the gap, the node MUST disconnect that
  peer. If the peer returns a successful response that is not a
  well-formed timestamp, the node MUST disconnect that peer as a failed
  reloaded clock check.
- **Unsupported / absent support:** if the request fails because the
  peer does not negotiate the generic request-response protocol, closes
  the request, times out, or otherwise reports a protocol-level failure
  without returning a timestamp, the result is inconclusive. The node
  MUST NOT disconnect the peer solely for that reason, and the peer MUST
  remain eligible as a seed/relay under R8, R10-R12, and R27.
- **Bound threshold:** the maximum acceptable gap is **20
  seconds**, exposed as a single named constant in the P2P layer.
  This value is depended on by swap-timing defaults and MUST NOT be
  changed casually.
- **Gating:** the admission check is gated behind the substrate's
  `application` build feature (it is part of the application-level
  P2P behaviour, not the bare transport).

**RP2 acceptance:** a peer whose reported UTC differs from local by
≤ 20 s stays connected; a peer that returns a well-formed timestamp
outside that gap is disconnected shortly after connection establishment;
a peer that lacks the generic request-response protocol, times out, or
reports an unsupported-protocol request failure remains connected unless
another independent rule disconnects it.

> **Upstream divergence (informative).** Post-baseline lineages have used
> different versioned protocol identifiers and stricter clock-check failure
> handling for newer network layers. This chapter binds the
> baseline-compatible reloaded request-response and peer-exchange identifiers
> to `/request-response/2` and `/peers-exchange/2`, and requires
> timestamp-check fallback so legacy/GLEEC seed and relay peers are not
> rejected merely because they do not support reloaded's post-connection
> clock query.

### 28.9A.3 RP3 — Expirable pubkey bans (partially present; expiry missing)

**RP3.** The pubkey-ban store MUST become **expirable**: a ban
entry MAY carry a time-to-live and, when it does, MUST auto-clear
once the TTL elapses without requiring an explicit unban. In
reloaded today the ban store is a plain map and every ban is
permanent until manually unbanned; the port adds expiry semantics
and a duration knob.

- **Manual ban — `ban_pubkey`.** The existing legacy RPC request
  `{ "pubkey": <pubkey hash>, "reason": <string> }` MUST gain an
  optional field `duration_min` (unsigned minutes). When
  `duration_min` is present, the ban is inserted with that expiry
  and auto-clears afterwards; when absent, the ban is **constant**
  (persists until an explicit unban). Banning an already-banned
  pubkey MUST be rejected. The response is the existing success
  acknowledgement (`{ "result": "success" }`).
- **Failed-swap auto-ban.** The automatic ban applied when a swap
  fails MUST become **time-limited** with a bound penalty of **one
  hour (3600 seconds)**, expiring automatically, rather than
  permanent.
- **List — `list_banned_pubkeys`.** Returns the current,
  non-expired ban set as `{ "result": <map of pubkey hash → ban
  reason> }`. The ban-reason wire shape is a `type`-tagged object:
  `{ "type": "Manual", "reason": <string> }` or
  `{ "type": "FailedSwap", "caused_by_swap": <uuid>,
  "caused_by_event": <swap-event> }`.
- **Unban — `unban_pubkeys`.** Request
  `{ "unban_by": { "type": "All" } }` or
  `{ "unban_by": { "type": "Few", "data": [ <pubkey hash>, … ] } }`.
- **Difference from the existing swap-failure ban:** in reloaded
  every ban is currently permanent; the port makes failed-swap
  bans self-expire after one hour and lets manual bans opt into a
  TTL via `duration_min`, while a manual ban with no `duration_min`
  remains permanent. Expired entries disappear from
  `list_banned_pubkeys` and stop being enforced without an explicit
  unban.

**RP3 acceptance:** a manual ban with `duration_min = N` disappears
from `list_banned_pubkeys` and stops being enforced after N
minutes; a manual ban with no `duration_min` persists until
unbanned; a failed-swap ban self-expires after one hour; the
`ban_pubkey` / `list_banned_pubkeys` / `unban_pubkeys` wire shapes
above are preserved.

## 28.10 Tests (test invariants)

**T1.** *Composed-behaviour shape.* A reflection-style test (or a
documentation-extracting audit) MUST confirm that the composed
`NetworkBehaviour` contains exactly the five sub-behaviours bound in
R6, and none of the seven libp2p protocol families excluded by R7.

**T2.** *Relay-mesh maintenance.* An in-process mesh test using the
testing memory transport MUST:

1. spawn one relay node and two client nodes;
2. observe that each client's connected-relays set includes the
   relay;
3. observe that the relay-mesh of each client includes the relay
   after one or two maintenance ticks (within roughly 30 seconds of
   simulated time);
4. force the relay to disconnect and observe that the maintenance
   tick removes it from each client's relay-mesh.

**T3.** *Pinned explicit relays.* A node configured with an explicit
relay MUST retain that relay in its relay-mesh across a
maintenance-tick cycle that would otherwise prune it when the mesh
exceeds the high watermark (R11 step 3).

**T4.** *Application-payload signing round-trip.* For arbitrary
serializable values, `decode_signed(encode_and_sign(value, sk))` MUST
return `Ok((value, signature, pubkey))` with `pubkey` matching the
public key derived from `sk`. Any single-byte mutation of the encoded
bytes MUST cause `decode_signed` to return an error and MUST NOT
return a parseable but invalid payload.

**T5.** *Topic construction.* `pub_sub_topic("orderbook",
"KMD:BTC")` MUST return exactly `"orderbook/KMD:BTC"`; the substrate
MUST NOT perform any URL-encoding or canonicalisation of the inputs.
A test or audit MUST confirm the helper's implementation reads
`TOPIC_SEPARATOR` (not a hard-coded `/`) so a future separator change
remains a one-symbol substrate edit.

**T6.** *Peer-exchange bound.* A peers-exchange request
`GetKnownPeers { num }` with `num <= 20` MUST receive a
`KnownPeers { peers }` response containing no more than 20 peer entries;
when `num > 20`, the responder MUST omit the response.

**T7.** *Empty bootstrap list.* A client spawned with no bootstrap
relays MUST start without a fatal P2P initialisation error, keep an
empty relay mesh, and report zero connected relays until a reachable
relay is introduced. A relay node spawned with no bootstrap relays
MUST still listen on its configured reachable address.

**T8.** *Handshake and address surface.* A compatibility audit MUST
confirm that native peer identities are Ed25519, connection upgrades
negotiate multistream-select V1, Noise XX, and yamux, native
distributed seed-node hosts map to the netid-derived TCP P2P port,
WSS uses the configured WSS port when enabled, and memory addresses
are limited to in-process tests.

**T9.** *Seed/relay compatibility fallback.* A compatibility test or
audit MUST cover a connected seed/relay peer that does not negotiate the
generic request-response timestamp-check protocol and/or does not
negotiate `/peers-exchange/2`. The peer MUST remain connected and relay-
eligible when it otherwise satisfies the transport and relay-advertisement
requirements. A separate test MUST confirm that a supporting peer whose
reported timestamp is more than 20 seconds away from local UTC is
disconnected.

## 28.11 Deferred Work

> **Note.** The §28.9A items (peer health-check RPC, time-sync
> peer admission with compatibility fallback, expirable pubkey bans)
> are **required ports**, NOT deferred work — they are binding
> driving-spec requirements an implementer MUST land. The items below
> are genuine deferrals.

**D1.** ~~A libp2p version bump is deferred.~~ **Superseded by §28.1A.**
The libp2p version bump is *no longer deferred*: adopting the
`KomodoPlatform/rust-libp2p` fork at tag `k-0.52.12` (R0a) is a binding
requirement of this refresh, driven by the advisory-clearing goal in
R0d. The accompanying touch on the sub-behaviours and the swarm-builder
code to compile against the fork's 0.52.x API is part of that required
work, not a deferral.

**D2.** Removal of the retained pre-substrate peer-discovery crate
(R4) from the workspace is deferred. Its retention costs build time
but preserves repository-history continuity.

**D3.** Unification of the two signing surfaces — application-payload
signing in R16 and proxy-signature signing in Chapter 27 — is
deferred. The two key spaces and threat models are currently kept
separate by design.

**D4.** Addition of peer-scoring / reputation extensions to the
gossipsub behaviour (whether the fork's optional peer scoring or a
separate mechanism) is deferred. The current substrate handles abusive
peers via manual-disconnect logic plus R9 force-disconnect.

**D5.** Addition of libp2p relay-v2 plus DCUtR fallback for clients
whose only path to a relay is blocked is deferred. The substrate
currently degrades to "no connection" for such clients.

**D6.** Automatic production seed-node discovery is deferred. Operators
remain responsible for supplying reachable relay addresses when the binary
does not carry a usable registry fallback for the selected netid.

## 28.11A Open Questions

**OQ1.** When reloaded adopts the fork (R0a), it MAY drop the in-tree
vendored gossipsub entirely in favour of the fork's `gossipsub` feature,
or keep an in-tree copy during a transition. This chapter binds the
relay-mesh *semantics* (R10–R14) but does not mandate which physical
source provides them. The chosen approach should be recorded when the
port lands.

**OQ2.** R0d requires the fork adoption to clear the enumerated
advisory-flagged transitive crates. **Resolved at adoption (2026-07):**
adopting the `k-0.52.12` fork did **not** clear the cluster. Only
`owning_ref` (RUSTSEC-2022-0040) was eliminated. The remaining crates —
`rustls`, `rustls-webpki`, `webpki`, `ring`, `mio`, `idna`,
`remove_dir_all` — still resolve, now pulled by the **fork's own
0.52-era transitive deps** (its WSS/TLS, trust-dns and tempfile stack)
instead of the old `ef2afcd4` pin; the WSS `rustls`/`ring`/`webpki` line
is additionally coupled to reloaded's `futures-rustls` transport binding.
Separately, `ed25519-dalek 1.x` / `curve25519-dalek 3.x` persist via
`solana-keypair` (Solana SDK), independent of libp2p. Per R0d this
residue is **not silently accepted**: it is recorded here and formally
accepted with per-advisory rationale in `deny.toml` (upstream-blocked —
awaiting KomodoPlatform fork modernization / Solana SDK bumps), and the
reloaded-owned roots (`libsqlite3-sys` via rusqlite; `metrics-util`) are
tracked as scheduled migrations.

## 28.12 External References

- *libp2p* — the underlying networking substrate. The target dependency
  bound by this chapter (§28.1A) is the `KomodoPlatform/rust-libp2p`
  fork at tag `k-0.52.12`, replacing reloaded's baseline pin of the
  public `libp2p/rust-libp2p` repository at revision `ef2afcd4` (~0.45).
- *libp2p gossipsub specification* — the basis the relay-mesh gossipsub
  extension extends with R10–R13.
- *libp2p floodsub specification* — the basis of the substrate's
  flood-based topic.
- *libp2p multistream-select specification* — the bound upgrade
  negotiation in R20.
- *libp2p noise handshake specification* — the bound security
  protocol in R20.
- *libp2p yamux stream-multiplexing protocol* — the bound stream
  muxer in R20.
- *libp2p multiaddr format* — the bound bootstrap-address grammar in
  R21.
- *secp256k1* — the bound signing primitive in R16.
- *msgpack serialization format* — the bound envelope serialization
  in R16.
- Chapter 06 (network-id and seed-node decoupling) — bound source of
  the network-identifier scoping that all mesh traffic carries.
- Chapter 27 (infrastructure substrate inventory), specifically the
  proxy-signature substrate row — the *other* signing surface, kept
  separate per R17.
- Chapter 11 (order-match cancellation race) and Chapter 09
  (watcher infrastructure) — primary consumers of the bound topic
  surface of R15.

## 28.13 Baseline Verifications

**V1.** The baseline workspace MUST be confirmed to contain four
distinct P2P-related substrate directories under the project's
crate root, with the layout bound in §28.1 (one glue substrate,
two vendored protocol substrates, one peer-discovery substrate). A
`git ls-tree` over the project crate root at the baseline commit
MUST list all four.

**V2.** The baseline composed network behaviour MUST be confirmed to
contain the five sub-behaviours of R6 (the bound substrate
preserves the shape, only consolidates its location). A `git grep`
for the bound sub-behaviour names against the baseline glue crate's
behaviour module MUST confirm all five.

**V3.** The baseline libp2p pin MUST be confirmed to be the *public*
`libp2p/rust-libp2p` repository at revision `ef2afcd4` (~0.45), with the
baseline 0.45-era per-target feature naming. This confirms the starting
point that R0a supersedes. The refreshed substrate is **not** identical
to the baseline pin: it is a deliberate move to the
`KomodoPlatform/rust-libp2p` fork at tag `k-0.52.12` with the feature
sets bound in R0b. A verification MUST show the baseline pin/flags, and
a separate check MUST confirm the adopted pin matches R0a/R0b once the
port lands.

## 28.14 Provenance Footer

- *Inputs consulted for this chapter:* the baseline tree at project
  baseline commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`,
  the restricted compatibility corpus for seed/relay protocol-version
  behaviour, Chapter 06 (network-id substrate), Chapter 09 (watcher topic
  conventions), Chapter 11 (order-match cancellation cache), Chapter
  27 (infrastructure substrate inventory and proxy-signature
  substrate row), Chapter 31 (the central application-context
  substrate constraints bound by Chapter 31 R6 / R7), and the
  external libp2p / cryptographic /
  wire-format specifications listed in §28.12.
- *Permitted-input classes used:* baseline source, including
  baseline-existing contract categories for composed-behaviour roles
  and for the crate-root public surface (`RelayAddress`, `PeerAddresses`,
  `encode_and_sign`, `decode_signed`, `pub_sub_topic`,
  `TOPIC_SEPARATOR`); interop / contract surface for the bound
  `/request-response/2` and `/peers-exchange/2` protocol identifiers,
  the `IAmRelay` control-
  message name, the `i_am_relay` configuration field, the
  `mesh_n_low`/`mesh_n`/`mesh_n_high` parameter names, the bound
  numeric constants 10 s / 300 s / 20 s / 20 / ~1 MiB); dictated
  compatibility behaviour for unsupported post-connection
  request-response checks; standard
  libp2p protocol names; standard cryptographic primitive names
  (Ed25519, secp256k1, Noise XX, SHA-256, msgpack).
- *Sibling chapters cross-referenced:* Chapter 06, Chapter 09,
  Chapter 11, Chapter 27, Chapter 31.
- *Author of this chapter:* clean-room round-2 driving-spec working
  set.
- *Forbidden corpus:* consulted only for seed/relay compatibility of the
  generic request-response timestamp check and peer-exchange protocol
  negotiation.
