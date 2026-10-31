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

> **Code-quality finding (informative).** The current implementation does
> not meet R1/R2/R5 as stated. `mm2_p2p` declares `atomicdex_behaviour`,
> `network_streamer`, `peers_exchange`, `relay_address`, and
> `request_response` as public submodules rather than keeping every
> non-re-exported symbol private, and several downstream call sites import
> directly from those submodule paths instead of the crate-root re-export
> surface — for example the connection command/event/channel types the
> application networking layer consumes, the gossipsub/peer-connectivity
> accessor functions backing the Chapter 10 §10.16 `NETWORK` streamer's
> snapshot fields (R30), and the `NetworkStreamer` type that streamer
> activates. None of these are on the R5 re-export list. Closing the gap
> means either widening R5's re-export list to cover the symbols downstream
> code actually needs, or moving those call sites onto crate-root
> re-exports and making the submodules private; either is a
> substrate-boundary change, not a change to any bound behaviour elsewhere
> in this chapter or in Chapter 10.

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

**R8c. Received peer addresses (Reloaded-original, maintainer decision
2026-10-30; amended 2026-10-31).** A node accepts a peer address it
received from another peer only in the form `/ip4/<address>/tcp/<port>`.
That holds for an address in a peer-exchange answer and for one in a
peers-topic announcement. The address MUST be globally routable (the
`ip_helpers::is_global` predicate of R24, unless item 8 is set), and the
port MUST be the network's TCP port. Every other form is not accepted,
with one exception: a native build looks up a hostname in a
peer-exchange answer under item 7 and stores the addresses it resolves
to, unless item 10 turns that off. The node makes no other name lookup
for a received address.

1. **Per address.** The node skips an address it does not accept and
   keeps the other addresses of the same answer or announcement. It
   skips a peer entry that has no accepted address left.
2. **The sender is kept.** Skipped addresses MUST NOT make the node
   forget the peer that sent them.
3. **Whole answer.** An answer with more than 20 peer entries (R8) is
   rejected as a whole. The node then forgets the peer that sent it and
   asks another peer.
4. **Logging.** The first time a distinct skipped address is seen, the
   node logs one INFO line. The line names the address, the peer that
   sent it and the accepted form. Later sightings log at DEBUG. The set
   of addresses already reported is bounded.
5. **Own identity** *(added 2026-10-30, maintainer decision BD).* The
   node never stores a peer entry that names its own peer ID, and never
   picks one as a dial candidate. Other peers hand out this node's own
   entry, and dialing it only reaches the node itself.
6. **Stale entry** *(added 2026-10-30, decision BD).* When a dial of an
   address stored for peer A reaches a peer with a different peer ID,
   the node removes that address from A's entry, and forgets A when no
   address is left. It does not store the address under the peer that
   answered.
7. **Hostnames in answers** *(added 2026-10-31, maintainer decision
   BH; a Reloaded-only extension: the reference rows keep no received
   hostname, see the note after the rationale).* On a native
   build, an address `/dns/<name>/tcp/<port>` or
   `/dns4/<name>/tcp/<port>` in a peer-exchange answer, with the
   network's TCP port, is queued for a lookup under the entry's peer ID
   instead of being skipped. The node still skips `/dns6` and
   `/dnsaddr`, a hostname with another port, and every hostname in a
   peers-topic announcement. A browser build skips every hostname.
   1. **When it looks up.** The node processes the accepted addresses
      of an answer before its hostnames. It makes no lookup while it is
      connected to the entry's peer. Otherwise it looks up the same name
      at most once per 30 minutes.
   2. **How it looks up.** The lookup goes through the operating
      system's resolver on a blocking worker thread, never on the
      swarm's own task.
      - At most one lookup runs at a time. The next one starts only when
        the previous one has returned, even if its result was given up.
      - A lookup that has not returned within 5 s stores nothing.
      - The queue holds at most 64 names. A name that finds it full is
        dropped and logged at DEBUG.
      - While the queue is empty, no lookup, timer or worker is pending.
   3. **Resolve, then check.** Each IPv4 result becomes
      `/ip4/<address>/tcp/<port>` and goes through the same check as a
      received address. The node stores an accepted one under the
      entry's peer ID, exactly like an accepted received address, and
      ignores IPv6 results. Items 5 and 6 apply to resolved addresses
      unchanged.
   4. **Names are never stored.** The node keeps no received hostname
      in a peer entry, so it never hands one out as a stored address.
   5. **Per-name record.** For at most 64 names, the node keeps the
      entry's peer ID, the time of the last lookup and the addresses
      that lookup stored.
      - A later lookup of the same name adds the new addresses. It
        removes from that peer's entry the addresses the name stored
        before and no longer returns.
      - When the record is full, the oldest entry is dropped.
      - The record is never handed out.
   6. **Reporting.** A hostname whose lookup fails or times out, or
      that resolves to no accepted address, is reported under item 4,
      with that reason. A name dropped from a full queue is logged at
      DEBUG only (sub-item 2), so a flood of names never reaches INFO.
      Skipping a lookup while connected or within 30 minutes is not
      logged.
8. **Private addresses for tests** *(added 2026-10-31, decision BH).* The
   configuration flag `p2p_allow_private_addresses` (default `false`)
   makes the check of this requirement accept any IPv4 address with the
   network's TCP port, routable or not. That applies to received and to
   resolved addresses. When the flag is `true`, the node logs a WARN line
   at start-up saying that the setting is not for production. The flag
   does not change the listener announcement of R24.
9. **Answers this node gives** *(added 2026-10-31, decision BH).* An
   answer this node gives carries, for each entry, only addresses in the
   accepted form of this requirement. The address of a connection this
   node dialed by hostname is left out, and so is an entry left with no
   address. *(The reference rows hand out a connection's dialed address
   as written before resolution, and so did Reloaded before this item:
   the note after the rationale, item 2. A reference receiver drops a
   whole answer that holds a hostname and forgets the sender: the same
   note, item 3. This item is a Reloaded-only divergence that changes
   no wire shape.)*
10. **Opt-back switch** *(added 2026-10-31, decision BH).* Items 7
    and 9 diverge from the reference rows, so they come with a way back
    (`docs/COMPAT_SWITCHES.md`). The top-level configuration flag
    `p2p_resolve_peer_hostnames` defaults to `true`. With `false`, the
    node skips every received hostname without a lookup, and its answers
    hand out a dialed connection's address as the reference rows do.
    Items 1 to 6 and 8 hold either way.

*Rationale.* A peer that does not run Reloaded may hand out the hostname
under which it dialed a seed. Rejecting a whole answer for one such
entry lost every valid address in it. Forgetting the sender removed a
working peer from the exchange. A hostname from a peer is still not
accepted, because it can resolve to an address that is not globally
routable, which R24 keeps out. It would also let a remote peer choose
the names this node looks up.

*Amended rationale (2026-10-31, decision BH).* The maintainer chose to
look these hostnames up after all, so that a node also reaches seeds
that other peers hand out only by name. Item 9 keeps this node's own
answers acceptable to every receiver. Item 7 keeps the two risks above out:
- **Routability.** The resolved address goes through the same check as
  a received address, so a name cannot steer the node to an address
  that is not globally routable.
- **Re-pointing.** Only the checked address is stored and dialed,
  never the name, so a later change of the name's records cannot
  re-point the node either.
- **Stalls and rate.** The lookup runs off the swarm's task, one at a
  time, from a bounded queue, so a remote peer can neither stall the
  node nor make it look names up at a high rate.
- **Source.** Only names in an answer to this node's own request are
  looked up.

What remains is that whoever runs a name's DNS servers can learn that
some node looked it up. Dialing a peer-supplied IPv4 address already
tells that peer more.

> **Reference behavior for received hostnames (informative; added
> 2026-10-31).** *(Authored by the KDF Spec Reader on 2026-10-31 from
> the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A and
> from the public libraries both rows pin. Cleared by a KDF Dirty Gate
> pass on 2026-10-31, §28.14.)* This note records what a reference
> node does. It changes nothing in R8c. Both rows behave the same in
> everything below, with one exception. A received announcement is
> ignored on each row's own default netid: 8762 at `v2.6.0-beta`,
> 6133 at `v3.0.0-beta`. `v3.0.0-beta` does not start on netid 8762
> at all (chapter 06 §6.6A.6 L1, L2).
>
> 1. **Own announcement.** Only a relay (seed) node builds one. A
>    client never does. The announcement carries at most **one**
>    address: one of the node's own listen addresses whose IPv4
>    component is globally routable. No hostname goes into it, no
>    external or observed address, and no port filter is applied. A
>    relay listens on one IP: the IP its operator configured, or else
>    the external IPv4 reported by public what-is-my-IP web services,
>    if the node can bind on it. If it cannot, the relay listens on all
>    interfaces, and its listen addresses are then its interface
>    addresses. The announcement is built 60 s after start and every
>    600 s after that, as a msgpack set of multiaddrs on the peers
>    topic (R6). So a seed operated by hostname puts in its public
>    IPv4 with the TCP port if it listens on that IPv4. It puts in
>    nothing if it sits behind NAT, and it never puts in its hostname.
>    With a secure-websocket listener configured, the one address may
>    be that listener's instead, which no receiver accepts (item 3).
>    **As read, the announcement reaches no peer at either row.** The
>    floodsub library sends a message, and the node's own
>    subscription, only to the peers the application has put on its
>    propagation list, and neither row puts any peer there. A reference
>    node therefore sends no peers-topic traffic.
> 2. **Answers it gives.** A reference node answers with up to the
>    requested number of entries (at most 20, R8). It picks them at
>    random among the peers it knows that have at least one address,
>    and gives every address it holds for each one. It holds addresses
>    from three sources:
>    - addresses it accepted from answers (item 3);
>    - addresses it accepted from announcements (item 3; on the rows'
>      default netids there are none);
>    - for a peer it is connected to over a connection **it dialed**,
>      the address it dialed, as written before name resolution. A
>      dial made to a known peer's ID carries that peer ID as a
>      trailing `/p2p/...` component.
>
>    Every connected relay joins the known peers at each 30 s
>    maintenance tick. So a seed the node dialed by hostname is handed
>    out as `/dns/<name>/tcp/<port>` for as long as that connection
>    lasts. The address of a peer that dialed this node is not
>    recorded and is not handed out. Nothing is filtered on the way
>    out: there is no form, routability or port check, and neither the
>    node's own entry nor the requester's is excluded.
> 3. **Addresses it receives.** An address is accepted only when its
>    first component is `/ip4` with a globally routable address and its
>    second is `/tcp` with the network's TCP port. Components after the
>    second are not examined. Addresses in the `/dns`, `/dns4`,
>    `/dns6`, `/dnsaddr` and `/ip6` forms are rejected, and so are
>    non-global `/ip4` addresses and other ports.
>    - *Answers are all-or-nothing.* The whole answer is dropped if it
>      is empty, has more than 20 entries, has an entry with no
>      address, or has a single rejected address. The sender is then
>      forgotten, together with the addresses held for it, and another
>      known peer is asked.
>    - *Announcements are all-or-nothing as well,* but the sender is
>      not forgotten. An announcement is processed only on a netid
>      other than the row's default netid.
>    - *No received hostname is resolved.* A received hostname is never
>      kept, so a reference node never resolves one, at receipt or at
>      dial. No resolved received address exists to be filtered.
> 4. **Dialing hostnames.** The only hostname addresses a reference
>    node dials are its configured seed entries (item 5). The DNS
>    transport resolves the name when the dial is made. It asks a fixed
>    public resolver (Google Public DNS), not the servers the operating
>    system is configured with, and uses the resolver library's default
>    options: a 32-entry cache that honors record lifetimes, and, for
>    `/dns`, IPv4 first and IPv6 only when there is no IPv4 record.
>    Every resolved address is tried in turn until one connects. No
>    routability or private-range check is made on a resolved address.
>    The limits are these:
>    - each seed address, as written, is dialed at most once per
>      5 minutes;
>    - at start-up at most `mesh_n` seeds are dialed, chosen at random,
>      all at once;
>    - after start-up, seeds are dialed only from the 30 s maintenance,
>      and only when known peers do not fill the shortfall (RQ1a
>      item 4);
>    - on each dial, the transport library caps the number of lookups
>      and connection attempts.
>
>    No limit on names exists per answer, per peer or per unit of time,
>    because no received name is kept.
> 5. **Configured seeds.** Neither row compiles in a seed list. The
>    list comes only from the operator's `seednodes` (chapter 06 §6.5,
>    the divergence note). Each entry is a bare IPv4 literal or a
>    lower-case DNS name. It is mapped to `/ip4/<ip>/tcp/<tcp port>` or
>    to `/dns/<name>/tcp/<tcp port>`; a browser build uses the
>    secure-websocket port with `/wss` instead. A full multiaddr is not
>    accepted, so R21's operator multiaddr forms are a Reloaded
>    extension. The seed list distributed for clients in the public
>    coins-configuration repository held only hostnames for netid 8762
>    at the `v2.6.0-beta` date. Today it holds only hostnames for netid
>    6133. A configured hostname is dialed through the transport's
>    resolution of item 4.
> 6. **Other behavior that limits discovery.**
>    - *Hostnames break reference-to-reference exchange.* Where the
>      configured seeds are hostnames, as in the distributed list, an
>      answer from a node connected to such a seed carries a hostname
>      whenever that seed is among the chosen entries (item 2). A
>      reference receiver then drops the whole answer and forgets the
>      sender (item 3). Reference-to-reference exchange therefore keeps
>      only the answers whose chosen entries include no seed dialed by
>      hostname.
>    - *Failed requests.* A timeout, or an asked peer that does not
>      support the protocol, also makes the node forget that peer and
>      ask another. Connected relays rejoin the known peers at the next
>      30 s tick, so a forgotten relay is back within 30 s while its
>      connection lasts.
>    - *Cap and deduplication.* The known peers are capped at 100. At
>      every 300 s request tick (the first at 20 s, R23), the excess is
>      removed at random, together with the addresses held for it.
>      Between ticks, accepted answers may exceed the cap. An address
>      already held for a peer is not added twice. An entry's address
>      count has no limit other than the frame size of R8b.
>    - *Which peer is asked, and how it is dialed.* The periodic request
>      goes to a random known peer, connected or not. To reach one that
>      is not connected, the request layer dials it by peer ID at its
>      held addresses, so a held address that now serves another peer
>      fails there. Relay maintenance instead dials held addresses
>      without an expected peer ID. A connection that reaches another
>      peer is kept as a connection to that peer, and the address stays
>      under the original entry (compare R8c item 6). Maintenance dials
>      every held address of a chosen peer except one it is already
>      connected to.
>    - *Own identity.* No entry is refused for naming the node's own
>      peer ID (compare R8c item 5). The transport library skips a dial
>      to an address equal to one of the node's own listen addresses,
>      and refuses a connection that reaches the node's own identity.
>
> *Code-quality findings (informative).* Each was apparent from
> reading the reference and this repository side by side.
> (a) *Inert announcement.* As read on 2026-10-31, this repository
> uses the same floodsub library the same way and puts no peer on its
> propagation list either, so its own announcement reaches no peer.
> R8c's announcement clause and the announcement bullet of T6a
> therefore cover a path that no peer of either implementation feeds.
> Proposal: keep the path for parity and confirm the absence of
> peers-topic traffic in the lab. Filling the list would be a
> Reloaded-only wire extension, and reference nodes on their default
> netids would ignore it.
> (b) *Routability predicate.* The reference treats all of
> 0.0.0.0/8 as not global and treats 192.0.0.9 and 192.0.0.10 as
> global, as the IANA special-purpose registry marks them. This
> repository's predicate of R24 treats only 0.0.0.0 in that block as
> not global, and treats all of 192.0.0.0/24 as not global. Proposal:
> align both ranges with the reference.
> (c) *Trailing components.* R8c's wording admits only the exact form
> `/ip4/<address>/tcp/<port>`. The reference, and this repository's
> check as read on 2026-10-31, examine only the first two components.
> Reference answers do carry the trailing `/p2p/...` form of item 2.
> Proposal: word R8c by its first two components, which matches the
> reference, or decide deliberately to reject a trailing component.
> (d) *Periodic request target.* R23 item 3 sends the periodic request
> to a random connected relay. Both rows and this repository pick a
> random known peer instead (item 6). Proposal: amend the wording of
> R23 item 3.
>
> *Open question.* The statement that a reference node sends no
> peers-topic traffic rests on reading at both rows, not on
> observation. A lab capture of a reference relay's traffic would
> confirm it.

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

**R11a (relay forward ordering).** *(Authored by the KDF Spec Reader
2026-10-28 from the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02
§2.15A, identical at both, and from the public gossipsub library both
rows pin; compared with the baseline anchor (V5). **Passed the KDF Dirty Gate
on 2026-10-28**, measure 0.02; §28.14.)* When `i_am_relay` (R12) is set, the vendored gossipsub
behaviour MUST, on receiving a message it accepts into its caches,
(1) deliver it to the consumer whether or not the node is subscribed to
the topic, and (2) forward it at once — before, and independently of,
any consumer validation — to the topic peers that included this node in
their relay mesh (R10) **and** to the relays of this node's own relay
mesh (R10), excluding in both sets the peer it arrived from, the
message's author and any peer the message is already known to have come
from. The consumer's later propagate call (R13, manual-propagation flag)
then forwards the same message a second time to the same recipients; the
substrate de-duplicates none of its own outbound copies, and recipients
drop the second copy by their duplicate cache. *(Correction of the
same day, re-checked by the KDF Dirty Gate on 2026-10-28, measure 0.02: the
next two sentences replace a first wording that said a
non-relay forwards once on the consumer's report.)* When `i_am_relay` is
not set, **nothing the node received is forwarded**: with manual
validation configured the substrate does not forward on receipt, and the
consumer issues the propagate call only when the node is a relay. A
non-relay sends only what it publishes itself. The behaviour is
post-baseline (V5) and identical in Reloaded's vendored copy and
dispatch: **parity**. Chapter 32 R-F3a and R-F3b state the consequence
for the orderbook keep-alive.

**R12.** The substrate's `IAmRelay` control message is bound: nodes
that act as relays MUST emit it; nodes that act as clients MUST NOT.
The configuration flag that selects which role applies is bound as a
boolean `i_am_relay` field on the substrate's gossipsub configuration
surface.

**R13.** The substrate's gossipsub configuration surface MUST further
expose:

- a content-addressed message-id function that hashes payload-plus-
  sequence-number, so duplicate payloads collapse to a single
  message id. The id is a wire value — it travels in the gossipsub
  IHAVE and IWANT control messages, whose message-id field the public
  gossipsub specification defines as a byte string — so both its
  preimage and its encoding are dictated interop. *Preimage:* the
  message's payload bytes followed by its sequence number, each fed
  through the Rust standard library's `Hash` implementation for the
  field's type into the standard library's default hasher (SipHash-1-3
  with zero keys, as the standard library implements it today; the
  library documents the algorithm as unspecified, so a change there
  would break id agreement with reference peers). Both trees pin the
  same libp2p fork (R0a), so the field types, and with them the
  preimage, are identical. *Encoding:* the 64-bit hash output
  serialized as exactly **8 bytes, big-endian**. *Per row (verified
  2026-10-27):* at the baseline anchor (R1) the same hash was emitted
  as its decimal ASCII string; `v2.6.0-beta` — the netid `8762`
  reference — and `v3.0.0-beta` — the netid `6133` reference — both
  emit the 8-byte big-endian form, and the public release history
  shows the change first at `v2.0.0-beta`. Adopting the 8-byte form
  therefore moves Reloaded toward, not away from, every reference peer
  on both supported netids;
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

> **Reloaded divergence (2026-10-27).** Reloaded today still emits the
> baseline's decimal-string id; its preimage and hasher already match
> R13. Consequence: a message id a Reloaded node announces in IHAVE
> never equals the id a reference node holds for the same message, and
> vice versa, so every announced message is re-requested with IWANT,
> delivered in full a second time and then dropped by the receiver's
> duplicate cache. The mesh still converges — IWANT echoes whatever id
> the announcer used, so the lookup on the announcer's side succeeds —
> but every gossip round between a Reloaded node and a reference node
> costs a full retransmission of every announced message in both
> directions. Closing it is an encoding change only (T10).
>
> *Scope and status (2026-10-28; the reference half passed the KDF Dirty
> Gate on 2026-10-28, measure 0.02).* The retransmission cost arises only between peers that share a
> **subscribed** topic and only within the per-peer, per-heartbeat caps
> of R13a: IHAVE is emitted for topics in the announcer's own mesh or
> fanout, to peers that announced a subscription, and is honored only for
> topics in the receiver's own mesh. A relay that subscribes to no
> orderbook topic — the reference seed's configuration — is not party to
> the exchange in either direction, so the id mismatch added no traffic
> and no state between a Reloaded maker and a reference seed; it did
> between the maker and reference makers or takers on its pairs.
> Reloaded's integration branch emits the 8-byte form as of 2026-10-28
> (the divergence is closed; the note is kept for the record and V4 is
> updated accordingly).

**R13a (control-message bounds the substrate provides).** *(Same
authorship, rows and status as R11a.)* The vendored gossipsub behaviour
MUST keep the library's defaults for the following bounds, which the
orderbook layer relies on (chapter 32 §32.9A R-B1): a heartbeat every
1 s; a message cache of five heartbeats of which three are announced; a
duplicate cache of 60 s; at most 10 IHAVE control messages honored per
peer per heartbeat and at most 5000 message ids requested per peer per
heartbeat, with the pending-request set and the per-peer counters
cleared every heartbeat; at most 3 retransmissions of one cached message
to one peer. IHAVE is emitted only for topics in this node's own mesh or
fanout and only to peers that announced a subscription to the topic, and
is honored only for topics in this node's own mesh; a relay that
subscribes to no orderbook topic therefore neither announces nor
requests message ids for them. Peer scoring stays off (R14), so reaching
any of these limits penalises nobody. The per-peer caps are
post-baseline hardening carried by the pinned fork (V5). The send queue
that carries these exchanges is bounded separately by §28.9B RQ1:
IHAVE and IWANT are control RPCs there, while messages sent in reply to
IWANT count as data.

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

## 28.9B Bound Per-Peer Gossip Send Queue (Reloaded-original)

**RQ1. Per-connection send queue (Reloaded-original, maintainer
decision AB, 2026-10-28).** The gossipsub connection handler keeps one
queue per connection of the RPCs waiting to be written to that peer.

1. **Data and control.** A *data RPC* is a queued RPC that carries at
   least one published or forwarded message. Every other queued RPC is a
   *control RPC*: subscribe, unsubscribe, GRAFT, PRUNE, IHAVE, IWANT, and
   the relay-mesh notices of §28.6. Control RPCs never count toward the
   cap of item 2 and are never dropped under item 4. The node does not
   combine control content and messages in one queued RPC. If it ever
   did, such an RPC would count as data, and dropping it would lose its
   control content too.
2. **Cap.** The queued data RPCs of one connection MUST NOT exceed
   **2,048 RPCs** or **4 MiB** of encoded size in total, whichever limit
   is reached first.
3. **Order.** The queue is first in, first out: RPCs are written to the
   peer in the order they were queued.
4. **Full queue.** When a new data RPC would exceed either limit, the
   handler MUST drop the oldest queued data RPCs of that connection until
   the new RPC fits, then queue the new RPC. Dropping affects only that
   connection; the same message is still queued for every other peer.
5. **Stuck peer** *(amended 2026-10-30, maintainer decision AY, D8).* A
   connection is *stuck* when either of these holds:
   - **At the cap.** Its data queue has been at its cap continuously for
     **60 seconds**. A period at the cap starts with the first drop under
     item 4. It ends when the data queue has drained to at most half of
     both limits (1,024 RPCs and 2 MiB). A peer that reads only a little
     at a time, freeing one slot at a time, therefore stays at the cap.
   - **Repeated drops.** In each of **3** consecutive 60-second windows,
     at least **25 %** of the data RPCs queued for the connection in that
     window were dropped under item 4. This catches a peer that catches
     up in bursts: its queue sometimes drains to half, which ends a
     period at the cap, while it keeps losing most of the stream. A
     window in which nothing was queued breaks the series.

   When a connection is stuck, the node MUST close **every** connection
   to that peer, not only the stuck one. Gossipsub writes a peer's data
   to one connection only. Closing just that connection would move the
   same stream to another one while the peer stays in every mesh (RQ1a
   items 1 and 6). After the close the peer leaves the meshes. It may
   reconnect, and it then resynchronizes through the keep-alive exchange
   of Chapter 32. RQ1a states what a reference peer does after the close:
   when it redials, what it sends, and that it holds nothing against the
   closer. The ping safeguard of R9 closes every connection to a peer in
   the same way.

   **Guard for relays** *(amended 2026-10-30, maintainer decision AY).*
   A shared cause can make several relay connections stuck at once, for
   example this node's own uplink being saturated. Closing them all would
   leave the node without relays. When the stuck peer is one of this
   node's connected relays, the node therefore holds back the close if
   either holds:
   - another relay was closed under this item within the last **30
     seconds**, the period of the relay-connection maintenance;
   - after the close, fewer than **2** relays would remain connected.

   A held-back connection stays open, and item 4 keeps dropping its
   oldest data RPCs, so its memory stays within the cap of item 2. The
   node starts a new stuck evaluation for it: a new period at the cap
   and a new series of windows. It closes the peer when that evaluation
   finds it stuck again and no guard condition holds. The node logs one
   INFO line per held-back close, naming the peer ID and the condition.
   A peer that is not a relay is closed without this guard.
6. **Logging.** The first drop toward a peer logs one warning naming the
   peer ID, the queued count and the queued bytes. Further drops toward
   that peer log at most one summary line per 60 seconds (the period of
   item 5), with the number
   dropped since the last line. Closing a peer under item 5 logs one
   warning with the peer ID, the criterion that applied (the time at the
   cap, or the share dropped in the last window), and the total dropped.
   No message content is logged.
7. **Settings.** The two limits, the 60-second period, the 3 windows
   and 25 % share of item 5, and the 30-second interval and 2 remaining
   relays of its guard are fixed values of the vendored gossipsub crate. Its configuration builder
   MAY override them, for tests. They are not an `MM2.json` setting.
8. **Pressure signal** *(amendment, maintainer decision AO,
   2026-10-28).* The handler reports a pressure level to the application
   for each connection:
   - *raised* when the data queue reaches 256 RPCs, or a period at the
     cap of item 5 starts;
   - *cleared* when the data queue has drained to 64 RPCs or fewer and no
     period at the cap is running.

   Each change is reported once. A closed connection counts as cleared.
   The handler also makes available, for metrics, its data-queue length
   and its count of dropped data RPCs. The application uses the signal
   for the maker publication budget of
   [Chapter 32](32-orderbook-p2p-and-trie.md) §32.9B R-MP2 and the counts
   for R-MP5. R-MP2 item 2 also reads whether the data queue is empty,
   for its queue signal *(added 2026-10-28)*. The thresholds are fixed values of the vendored crate,
   which its configuration builder MAY override for tests.
9. **Priority lane** *(amendment, maintainer decision AO option C,
   2026-10-28).* A published or forwarded message the application marks
   as *priority* is queued in a separate first-in-first-out lane of the
   same connection. The handler writes every RPC in the priority lane
   before any RPC of the queue of items 1 to 4.
   - **Marking.** The application marks priority when it publishes and
     when it lets a validated message propagate. Gossipsub does not tell
     message kinds apart itself. Which messages are priority is bound by
     [Chapter 32](32-orderbook-p2p-and-trie.md) §32.9B R-MP1 item 2.
   - **Never dropped.** A priority RPC does not count toward the cap of
     item 2 and is never dropped under item 4.
   - **Own cap** *(amended 2026-10-30, maintainer decision BC).* The
     priority lane holds at most 256 RPCs. When it is full and another
     priority RPC would be queued, that RPC is queued at the end of the
     queue of items 1 to 4 instead, as a data RPC: it counts toward the
     cap of item 2, and item 4 may drop it like any other data RPC. A
     full priority lane does not close the connection; a peer that is
     not reading at all is closed under item 5. The first time the lane
     is found full on a connection, the node logs one line at INFO level
     naming the peer ID and the lane size. Further overflows on that
     connection log at most one INFO line per 60 seconds, with the number
     of priority RPCs moved to the data queue since the last line. No
     message content is logged.
     *Reason:* a burst of more priority messages than the lane holds is
     normal. A maker that cancels all of its orders publishes several
     hundred cancellations at once, and a relay forwards the second copy
     of each through this lane (Chapter 32 §32.8 R-F3b). In a field run
     on a public network, closing on a full lane closed every slower
     relay link of a seed in the same second. On a relay the first copy
     of the same message is already in the data queue (§32.8 R-F3a), so
     moving the second copy there puts no message at a risk it was not
     already at.
   - **Order across lanes.** A priority message may reach the peer before
     a non-priority message published earlier. Messages within one lane
     keep their order.

*Rationale.* A peer that stops reading the gossip stream, or reads more
slowly than messages arrive, stalls the outbound stream at the
multiplexer's flow-control window, before the TCP send buffer fills.
Without a cap, the queue toward that one peer grows with every message
the node publishes or forwards. A measurement on 2026-10-28 found:

- About the payload plus 0.4 KB per queued message, separately for each
  slow peer; 20,000 messages of 8 KiB held 366 MiB toward one paused
  peer.
- A newest-first queue under a backlog delivered messages in reverse
  order. A reversed backlog can leave a peer with an older update of an
  order until the next keep-alive synchronization.
- The ping safeguard closes only a peer that stops answering pings. A
  peer that answers pings but reads no gossip was never closed.

The cap applies to the handler's queue only. RPCs already handed to the
multiplexer, up to its flow-control window, are outside it, so a stalled
peer may still receive about one window's worth of older messages ahead
of the queued ones.

At the highest message rate seen in the lab, about 190 messages/s, 2,048
messages is a stall of about 10 seconds, so a peer that keeps up never
reaches the cap. The cap does not change what a healthy peer receives.

*Why the oldest is dropped.* Under first-in-first-out order the delivered
sequence stays in order either way; the choice is only which messages
are lost. Dropping the oldest loses stale values. Dropping the newest
would lose the latest updates and leave the peer with stale orders until
the keep-alive synchronization.

> **Reloaded divergence (informative).** The reference leaves the
> per-peer send queue unbounded. The wire is unchanged: no message format
> changes and nothing new is sent. Only a peer that cannot keep up
> observes a difference: it may miss gossip messages, and after 60
> seconds at the cap it is disconnected. Under the reference the sending
> node's memory would instead grow until the node fails. Under
> `docs/COMPAT_SWITCHES.md`, "The default-side rule", this falls in arm
> 3. The project team decided (decision AB, 2026-10-28) that Reloaded's
> variant is the default and that **no switch** is provided, because an
> unbounded queue has no legitimate use.
>
> **Scope.** This rule protects a node against a peer that cannot keep
> up. It is not the cause of the seed-node memory growth of 2026-10-28,
> which a separate analysis attributes to retained state.
>
> **Provenance.** Reloaded-original. Inputs: our own code (the vendored
> gossipsub 0.45 handler and its README), our in-process measurement,
> one gated fact (the reference leaves this queue unbounded), and
> rust-libp2p gossipsub 0.49.4 (MIT), read for how it bounds its own
> queues. That design was not adopted: it drops the newest message and
> expires queued messages after a fixed time.

**RQ1a. A reference peer after every connection to it is closed.**
*(Authored by the KDF Spec Reader 2026-10-30 from the rows
`v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at both,
and from the public gossipsub library of the libp2p fork both rows pin.
**Passed the KDF Dirty Gate on 2026-10-30**, measure 0.03; §28.14.
Prompted by D8: closing every connection to a peer whose send queue is
stuck under RQ1 item 5, not only the stuck one.)* When a Reloaded node R closes every connection to a reference node
X that uses R as a relay, X behaves as follows. The facts are identical at
both rows.

1. **State is dropped at the last close, and only then.** When X's last
   connection to R closes, X forgets R from all of its gossip state at
   once: topic membership, every per-topic mesh and fanout, the
   connected-relays set, the relay mesh and the reverse mesh (R10, R27).
   While at least one connection to R remains, none of this changes: R
   stays in every mesh, X's traffic to R continues over a remaining
   connection, and only what was queued on the closed connection is
   lost. Closing one of several connections therefore does not remove R
   as X's relay; closing all of them does.
2. **A request in flight fails late.** A request-response exchange X has
   in flight to R when the connections close is not failed by the
   close: X's request layer does not act on the connection-closed
   failure the transport reports, and the request fails when the 3 s
   sweep finds it older than its 10 s timeout, between 10 and 13 s
   after it was sent (chapter 32 R-Q6, R-F3b). A synchronization request
   caught by the close therefore costs X as much as a timeout would.
3. **No ban, penalty or score.** X records nothing against R for
   closing. Peer scoring is off (R14); the gossip blacklist the library
   offers is never called by the application at either row; the pubkey
   ban of RP3 is keyed by the pubkey that signs gossip messages, not by
   peer, and nothing on the connection path feeds it. The only memory X
   keeps is address-level: an address X dialed within the last
   **5 minutes** is not dialed again during that time, whether at
   start-up or from maintenance (item 4).
4. **Reconnect only from maintenance, only when short of relays.** The
   transport never redials on its own. X redials from its **30 s**
   relay-connection maintenance tick (chapter 32 §32.9A R-B1 row 18),
   and only when its count of connected relays is below the low
   watermark of R13 (2 for a client, 4 for a relay). It then dials up to
   `mesh_n` minus that count candidates: first peers chosen at random
   from its known-peer list (addresses learned through peer exchange,
   R8; the peers-topic announcement of R6 adds none, see the note after
   R8c), then, if those are too few, addresses chosen at
   random from its configured bootstrap list (R23); in both sets it
   skips an address it is already connected to or dialed within the last
   5 minutes. The earliest redial of R is therefore the first tick at
   which X is both short of relays and past the 5-minute mark for R's
   address; a connection that lasted less than 5 minutes delays the
   redial to that mark; and an X that still has enough relays never
   redials R on its own. R redials X only if X is a relay whose listen
   address R holds (R8, R23): clients are never dialed. *(Two phrases
   of this item corrected 2026-10-31 and cleared by a KDF Dirty Gate
   pass the same day, §28.14.)*
5. **After reconnecting, only the gossip handshake and the clock
   check.** On the first new connection to R, X sends -- once -- either
   its relay notice (R12, if X is a relay) or one subscription for every
   topic in its own mesh (every orderbook and swap topic it is
   subscribed to, if X is a client); R does the same toward X; and each
   side issues the clock check of RP2 on the new connection. On R's
   relay notice X adds R to its connected relays and includes R in its
   relay mesh **at once** only if that mesh is below the low watermark,
   sending the inclusion notice of R10 that makes R forward to X again;
   otherwise R waits for X's 10 s relay-mesh tick (R11), which adds one
   relay per tick and only while the mesh is below the low watermark --
   so an X whose relay mesh is already at the low watermark does not
   re-include R until another relay drops, and receives nothing from R's
   forwards meanwhile (R11a). If X is a relay, R adds X to its own relay
   mesh on X's inclusion notice unless R's mesh is above the high
   watermark. Nothing at the application layer reacts to the close or to
   the reconnect: X sends no immediate keep-alive, no synchronization
   request and no orderbook request, and keeps its subscription state;
   the keep-alive cycle of chapter 59 R59.6.4 (30 s) and the pubkey
   liveness window (90 s) run on their own clocks, so a gap longer than
   the window evicts at X the pubkeys it heard only through R, and the
   next keep-alive rebuilds them through the exchange of chapter 32
   §32.7.3, addressed to the peer that forwarded it.
6. **More than one connection to a peer.** Neither row limits
   connections per peer. Two relays that dial each other hold two
   connections and keep both: the dial-side check compares only the
   remote addresses of existing connections, and an inbound connection's
   remote address is the dialer's ephemeral one, so it never matches the
   listen address being dialed. X sends its own publishes and its
   forwards to relay-mesh members on one fixed connection -- the
   earliest established that is still open -- and its forwards to the
   clients it serves and its IWANT answers on whichever connection can
   take the item. The ping safeguard of R9 closes **all** connections to
   a peer at once.

Reloaded's substrate, vendored from the same library, behaves as items
1, 5 and 6 state and keeps the 30 s maintenance, the low-watermark
condition and the watermark triples of item 4 (**parity** there).
Reloaded differs on two points: it fails an in-flight request at once
when the connection closes (item 2), and it has no 5-minute dial guard,
so it redials at the next maintenance tick at which it is short of
relays (item 3 and 4). For R, the consequence of item 1 is that closing
only the stuck connection would leave R in X's mesh, with X's traffic
moved to the other connection, while closing all of them makes X drop R
and redial under item 4; the consequence of items 4 and 5 is that a
reference client with one relay is back in at most 30 s once the
5-minute dial guard has passed, and a reference client with spare relays
does not come back until it is short of relays.

**RQ1 test items.**

- **T-RQ1.1** *Healthy peer unaffected.* A peer that keeps reading
  receives every message of a 1,000-message burst, in the order sent, and
  no drop is logged.
- **T-RQ1.2** *Order under a backlog.* After a paused peer resumes, it
  receives the queued messages in the order they were queued.
- **T-RQ1.3** *Drop oldest at the count cap.* With a paused peer and small
  payloads, the sender's queue for that peer stops at 2,048 data RPCs (or
  the test's configured value). The sender's memory stops growing with
  more messages. After resume the peer receives the newest messages and
  not the oldest. Healthy peers beside it receive every message.
- **T-RQ1.4** *Drop oldest at the byte cap.* The same with 8 KiB payloads.
  The 4 MiB limit (or the test's configured value) acts first.
- **T-RQ1.5** *Control messages never dropped.* A control RPC queued while
  the data queue is at its cap is neither dropped nor counted toward the
  cap, and is written in its turn. This item is verified at the handler
  queue level. A relay node ignores subscription changes, so an
  end-to-end subscription probe does not observe it.
- **T-RQ1.6** *Stuck peer closed (amended 2026-10-30, D8).* A peer at the
  cap for the full period (shortened by test configuration) is
  disconnected on every connection it holds to the node. A peer that
  drains before the period ends is not.
- **T-RQ1.7** *Rate-limited logging.* The drop warning appears once; later
  drops log at most one summary line per period.
- **T-RQ1.8** *Pressure signal.* Filling a paused peer's queue to 256 data
  RPCs raises the signal once. Draining to 64 clears it once. A period at
  the cap raises it, and closing the connection clears it.
- **T-RQ1.9** *Priority lane.*
  - With the data queue at its cap toward a paused peer, a priority
    message is not dropped, and after resume it arrives before the queued
    data messages.
  - *(Amended 2026-10-30, decision BC.)* With 256 priority RPCs queued
    toward a paused peer, a 257th is queued at the end of the data queue
    and counts toward its cap; the connection stays open. One INFO line
    names the peer, and a second overflow within the period logs nothing
    new until the period's summary line.
  - A burst of 600 priority RPCs toward a peer that reads slowly closes
    no connection. After it resumes, the first 256 arrive before the
    data RPCs queued earlier, and every other one that item 4 did not
    drop arrives in its order in the data queue.

- **T-RQ1.10** *Reference peer after a full close (RQ1a; black-box lab
  item).* A reference client whose only relay is the node under test has
  every connection closed by it: the client's relay mesh is empty at
  once; it redials no earlier than its next maintenance tick and no
  earlier than 5 minutes after its last dial of that address; on
  reconnecting it sends its subscriptions, re-includes the relay in its
  mesh at once, and sends no orderbook or synchronization request until
  its next keep-alive arrives; a request in flight at the close fails
  between 10 and 13 s after it was sent. A reference client with two
  other relays in its mesh does not redial within 10 minutes.
- **T-RQ1.11** *Repeated drops (item 5; added 2026-10-30, D8).* The
  windows are shortened by test configuration.
  - A connection is reported stuck, and every connection to the peer is
    closed, when it drops at least 25 % of the data RPCs queued in each
    of 3 consecutive windows, even though its queue drains to half within
    each window, so that no period at the cap lasts the full period.
  - A connection that drops 20 % in each window is not reported stuck.
  - Neither is one that drops 25 % in two windows and none in the third.
  - A window in which nothing was queued breaks the series.
- **T-RQ1.12** *Guard for relays (item 5; added 2026-10-30, decision AY).*
  The guard interval and the stuck periods are shortened by test
  configuration.
  - Two relays become stuck within the guard interval: the first is
    closed, the second is held back and stays connected.
  - With only 2 relays connected, a stuck relay is held back.
  - A held-back relay that is still stuck after a new evaluation, with no
    guard condition holding, is closed on every connection.
  - A stuck peer that is not a relay is closed at once.
  - While a relay is held back, its data queue stays within its cap.

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

**T6a.** *Received peer addresses (R8c).*
- An answer that mixes accepted addresses with `/dns/...` and
  non-global `/ip4/...` addresses keeps every accepted address. A peer
  entry with no accepted address is skipped. The sender stays a known
  peer.
- An answer with more than 20 entries is rejected, and the sender is
  forgotten.
- A peers-topic announcement with one `/dns/...` address and one
  accepted address keeps the accepted one.
- A skipped address is reported at INFO once, and at DEBUG after that.
- *(Added 2026-10-30, decision BD.)* An answer or announcement that
  carries this node's own peer ID stores nothing for it, and relay
  maintenance never dials it. A dial of A's address that reaches peer B
  removes the address from A's entry and stores nothing for B.
- *(Added 2026-10-31, items 7 to 10.)* With an injected resolver:
  - A `/dns` or `/dns4` name with the network's port that resolves to a
    global IPv4 address is stored as `/ip4/<address>/tcp/<port>` under
    the entry's peer ID. The name itself is not stored.
  - A name that resolves to a private, loopback or link-local address
    stores nothing and is reported with that reason. With
    `p2p_allow_private_addresses` set to `true` it is stored, and so is
    a received private `/ip4` address.
  - `/dns6`, `/dnsaddr`, a hostname with another port and a hostname in
    a peers-topic announcement are skipped without a lookup.
  - No lookup is made while the entry's peer is connected. A second
    sighting of the same name within 30 minutes makes no second lookup.
  - A later lookup that returns a different address adds it and removes
    the address the same name stored before.
  - The queue holds 64 names, and a 65th is dropped.
  - Lookups run one at a time. A lookup that times out stores nothing,
    and the next one starts only after it has returned.
  - With an empty queue, no lookup, timer or worker is pending.
  - An entry that names this node's own peer ID causes no lookup.
  - A browser build skips every hostname.
  - An answer this node gives carries no hostname, including for a peer
    connected through a dialed hostname, and no address outside the
    accepted form. An entry left with no address is left out.
  - With `p2p_resolve_peer_hostnames` set to `false`, no hostname is
    looked up, and an answer this node gives carries a dialed
    connection's address, as it did before item 9.

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

**T10.** *Message-id wire encoding (R13).* For one gossipsub message —
identical payload bytes and sequence number — the id a Reloaded node
announces in IHAVE is 8 bytes long and equals, byte for byte, the
big-endian serialization of the R13 hash; a decimal ASCII rendering of
the same hash MUST fail the test. A conformance variant records the
IHAVE ids a reference node at `v2.6.0-beta` or `v3.0.0-beta` announces
for a captured message and asserts byte equality with the Reloaded
node's id for the same message.

**T11.** *Relay forward ordering and control bounds (R11a, R13a).* With
`i_am_relay` set, a received message on a topic the node is not
subscribed to is forwarded to a downstream topic peer before the consumer
has been polled, and a second copy follows the consumer's propagate call;
with the flag clear, no copy of a received message leaves the node at
all *(correction re-checked by the KDF Dirty Gate on 2026-10-28)*. A peer
announcing 20 IHAVE messages of 1000 unknown ids each within one
heartbeat receives IWANT for at most 5000 ids drawn from at most 10 of
the announcements, and the next heartbeat starts the count afresh. A node
with no orderbook topic in its mesh emits no IHAVE for such topics and
ignores IHAVE for them.

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

**D7.** *Per-peer outbound queue bound (Reloaded-original candidate;
added 2026-10-28; the reference fact passed the KDF Dirty Gate on
2026-10-28).* The vendored
gossipsub connection handler's outbound queue has no bound at either
reference row or in Reloaded (chapter 32 §32.9A R-B1 row 17): every
forwarded message is cloned per recipient and held until that
recipient's stream is writable, so a peer that reads slowly makes this
node's memory grow at that peer's share of the forwarded traffic,
released only by disconnection — and the ping disconnect of R9 fires
only when the peer stops answering pings, not when it merely reads
slowly. A bound — dropping the oldest queued forwards past a per-peer
limit, or disconnecting a peer whose queue has exceeded a limit for
longer than a grace period — is a Reloaded-original hardening candidate;
the two copies per message of R11a are the other half of the same cost
and a candidate of their own (forward once, on the consumer's report).
Either is a divergence to record in the
operator-facing compatibility documents; neither changes a wire shape.

**D8.** *Close every connection of a stuck peer (Reloaded-original;
added 2026-10-30; the reference facts it rests on are RQ1a).* RQ1 item 5 closes the connection whose data queue is
stuck. Where a peer holds more than one connection to this node (RQ1a
item 6), the candidate closes every connection to that peer, so that
the peer drops this node as its relay and redials (RQ1a items 1 and 4)
instead of keeping this node in its mesh over a connection the stuck
one's traffic moves to. RQ1a states what the peer then does and that it
holds nothing against this node for the close. This is a divergence to
record in the operator-facing compatibility documents when adopted; it
changes no wire shape. *Decided 2026-10-30 by the maintainer (decision
AY), pre-beta: adopted, together with the repeated-drops criterion of
RQ1 item 5. A swaplab run gates the merge.* *Amended 2026-10-30 (decision
AY): a guard holds back the close of a relay when another relay was
closed within 30 s or fewer than 2 would remain (RQ1 item 5, T-RQ1.12).*

**D9.** *Peer-address follow-ups (deferred to after the beta; added
2026-10-31, maintainer decision BH).*
- **Asynchronous lookups.** An asynchronous, spaced lookup loop in place
  of the blocking worker of R8c item 7.
- **Transport resolver.** The transport dials configured hostnames
  through a fixed public resolver. A security evaluation should compare
  that with the system's resolver, possibly adding a compatibility
  switch that defaults to the fixed resolver. This is an evaluation, not
  a decision.
- **Skip logging.** R8c item 4 logs each skipped address at DEBUG and adds
  one INFO summary per hour, with counts per form.
- **Findings of the note after R8c's rationale:**
  - (b) align the routability ranges of R24 with the reference;
  - (c) word R8c by an address's first two components;
  - (d) correct the request target in R23 item 3.
- **Idle-close logging.** An idle close of a relay connection outside
  this node's relay mesh is logged at INFO.

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

**V4.** The baseline glue crate's gossipsub configuration MUST be
confirmed to derive the message id from the payload and the sequence
number with the standard library's default hasher and to emit it as a
decimal string. R13's 8-byte big-endian encoding is therefore a
post-baseline change on the reference side, which Reloaded followed on
its integration branch as of 2026-10-28 (R13 divergence note).

**V5.** The baseline in-tree gossipsub MUST be confirmed to forward a
received message, when manual propagation is configured, only on the
consumer's propagate call — a relay's immediate forward on receipt
(R11a) is post-baseline — and to request every unknown id of every
IHAVE without per-peer caps (the caps of R13a are post-baseline
hardening carried by the pinned fork).

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
  negotiation; and (addition of 2026-10-27, KDF Spec Reader, passed the KDF Dirty Gate
  on 2026-10-27) for the message-id preimage and wire encoding of R13 at the
  rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, compared
  with the baseline anchor (V4). The facts taken are the two hash
  inputs, the hasher family, the 8-byte big-endian encoding and the
  release at which it first appears; the per-row encoding statement
  also rests on the public release history. No private identifier,
  function body, helper decomposition, log or error literal was
  reproduced. Dirty Gate clearance (chapter 01 T1, R40): PASS on 2026-10-27, measure
  0.01, no leaks; every reference fact confirmed at both rows, including the
  per-row encoding. *Extent:* the gossipsub message-id derivation and encoding at
  both rows, the baseline anchor and the public releases `v1.0.7-beta` and
  `v2.0.0-beta` (five files). *Method:* reading of the compared regions plus a
  mechanical regex scan of every added line against private identifiers,
  literals and distinctive phrases; the mechanical gate tool was not run
  (prose). *Measure:* 0.01 against the 0.10 threshold.
- *Scoped addition (2026-10-28, second pass) -- R11a, R13a, the scope
  paragraph appended to the R13 divergence note, T11, D7, V5 and the V4
  status sentence. Dirty Gate: PASS on 2026-10-28, recorded below.* Prompted by the continued memory
  growth of public seed nodes under a Reloaded maker. *Inputs:* the public
  gossipsub library of the libp2p fork both reference rows and Reloaded
  pin (a permitted sibling source, not forbidden-class material) for the
  forward ordering, the announcement caps and the cache windows; the
  baseline anchor's in-tree gossipsub (V5); this repository's own vendored
  copy, configuration builder and propagate call (every parity statement);
  and the forbidden corpus, consulted at the rows `v2.6.0-beta` and
  `v3.0.0-beta` of chapter 02 §2.15A, identical in every respect taken,
  **only** to establish that the reference configures the relay flag and
  manual propagation as R11a describes, calls the propagate on the
  application's success, and that a reference seed subscribes to no
  orderbook topic. What crosses is behaviour and the library's public
  default values; no private identifier, function body, helper
  decomposition, log or error literal was reproduced. The hardening
  candidates of D7 are Reloaded-original.
- *Dirty Gate clearance for the 2026-10-28 second-pass addition (chapter 01
  T1, R40):* PASS on 2026-10-28, measure 0.02, no leaks; every reference fact
  confirmed at both rows. The non-relay-forwarding corrections marked in the
  text were re-checked the same day: PASS, measure 0.02, nothing held.
  *Extent:* the order-matching subsystem's keep-alive receiver,
  synchronization requester and responder, trie store, trie worker, periodic
  expiry sweep and memory purge; the network dispatch loop and command/event
  plumbing; the composed p2p behaviour's configuration, channel sizing and
  relay-connection maintenance; the request-response wrapper's pending table
  and timeout sweep; peer-exchange capacity; and the daemon start-up
  subscriptions -- at the rows `v2.6.0-beta` and `v3.0.0-beta` (nine source
  files at each row, identical in every respect taken); also compared, as
  permitted sibling and own-tree input, the MIT-licensed gossipsub crate both
  rows pin (in Reloaded's vendored copy, compared with the pinned fork) and
  the baseline anchor's in-tree gossipsub (V5). The re-check covered the
  dispatch loop's forwarding decision and the gossipsub configuration at both
  rows, the vendored crate's forwarding, propagate, validation gating, IHAVE
  emission and IWANT service, and Reloaded's own dispatch gate. *Method:*
  reading of the compared regions, plus mechanical comparison (inter-row diff,
  recursive diff of the vendored crate against the pinned fork, extraction of
  every backticked token and hexadecimal fragment from the added lines); the
  mechanical gate tool was not run (prose). *Measure:* 0.02 against the 0.10
  threshold. The clearance is complete and not pending.
- *Scoped addition (2026-10-30) -- R8c and T6a.* Reloaded-original,
  maintainer decision of 2026-10-30. It was prompted by peer-exchange
  answers carrying hostname addresses in a field run on a public network.
  *Inputs:* this repository's own peer-exchange and relay-address code
  only. The forbidden corpus was not consulted, and the addition needs
  no Dirty Gate pass.
- *Scoped amendment (2026-10-30) -- §28.9B RQ1 item 9 "Own cap" and
  T-RQ1.9.* Reloaded-original, maintainer decision BC of 2026-10-30. It
  was prompted by a seed's own log from a field run on a public network,
  in which a full priority lane closed three relay links in the same
  second. *Inputs:* that log and this repository's vendored gossipsub
  send queue only. The forbidden corpus was not consulted, and the
  amendment needs no Dirty Gate pass.
- *Scoped addition (2026-10-30, Spec Reader) -- RQ1a, the pointer appended
  to RQ1 item 5, T-RQ1.10 and D8. Dirty Gate: PASS on 2026-10-30, recorded
  below.* Prompted by the Reloaded candidate of closing every
  connection to a peer whose send queue is stuck. *Inputs:* the public
  gossipsub library of the libp2p fork both reference rows and Reloaded
  pin (a permitted sibling source, not forbidden-class material), read
  for the bookkeeping at a connection close with and without remaining
  connections, the handshake sent on a first connection, the relay-notice
  and inclusion handling, the relay-mesh fill rate and watermarks, the
  choice of connection for sends, and the address-based connected check;
  this repository's own vendored copy of that library, composed behaviour
  and request layer (the parity and difference statements); and the
  forbidden corpus, consulted at the rows `v2.6.0-beta` and `v3.0.0-beta`
  of chapter 02 §2.15A, identical in every respect taken, **only** to
  establish: the 30 s relay-connection maintenance and its conditions
  (below the low watermark; candidates from known peers, then bootstrap;
  skip connected and recently dialed addresses; trim above twice the
  high watermark), the 5-minute dial guard, that no per-peer connection
  limit and no blacklist call exist, that the request layer does not act
  on a connection-closed failure and relies on its 3 s sweep against the
  10 s timeout, that the ping wrapper closes all connections of a peer,
  that the clock check runs on every established connection, and that
  nothing at the application layer reacts to a connection close or a
  reconnect. What crosses is behaviour and numeric values a peer can
  observe in timing (30 s, 5 min, 10 s, 3 s, 10 to 13 s, the watermark
  triples of R13); no private identifier, function body, helper
  decomposition, log or error literal was reproduced. T-RQ1.10 and D8 are
  Reloaded-original. The earlier clearance positions recorded above are
  untouched by this addition.
- *Dirty Gate clearance for the 2026-10-30 Spec Reader addition (chapter 01
  T1, R40):* PASS on 2026-10-30, measure 0.03, no leaks.
  - *Extent:* the reference's composed peer-to-peer behavior at the
    published release designations `v2.6.0-beta` and `v3.0.0-beta`:
    - the bootstrap dialing and the 30 s relay-connection maintenance
      (chapter 32 §32.9A R-B1 row 18 scope);
    - the request-response layer and its pending-request timeout handling
      (R8a/R8b scope);
    - the ping wrapper's disconnect action (R9 scope);
    - the per-connection clock check (RP2 scope);
    - the application-layer dispatch's reaction to connection events
      (§28.9B scope).

    Seven corpus source files were compared at each row. The inter-row
    diff was taken, and no fact relied on changed between the rows. Also
    compared, as permitted inputs: this repository's vendored copy of the
    public gossipsub library (1 file), and this repository's own composed
    behavior, request layer and ping wrapper (3 files).
  - *Method:* reading, plus a mechanical extraction of every backticked
    token in the added lines, tested against both corpus rows. The
    mechanical gate tool was not run (prose, no code blocks).
  - *Measure:* 0.03 against the 0.10 threshold.
- *Scoped addition (2026-10-30, maintainer decision AY).* It covers the
  amended RQ1 items 5, 6 and 7, T-RQ1.6, T-RQ1.11, the relay guard of
  item 5 with T-RQ1.12, and the decision lines of D8. Its first wording
  also amended item 9 and T-RQ1.9; decision BC, above, replaced those.
  It is Reloaded-original.
  - *Inputs:* this repository's vendored send queue and connection
    handler, and the field run of 2026-10-30 on a public network. RQ1a
    supplies the reference facts.
  - No further corpus input was used, and the addition needs no Dirty
    Gate pass.
- *Scoped addition (2026-10-30) -- R8c items 5 and 6, and their T6a
  bullet.* Reloaded-original, maintainer decision BD of 2026-10-30. It
  was prompted by a seed's own log from a field run on a public network,
  which showed the seed dialing itself and a stored address answering
  under another peer ID. *Inputs:* that log and this repository's own
  peer-exchange and relay-maintenance code only. The forbidden corpus
  was not consulted, and the addition needs no Dirty Gate pass.
- *Scoped addition (2026-10-31, KDF Spec Reader) -- the note
  "Reference behavior for received hostnames" after the R8c rationale,
  and the matching correction of two phrases in RQ1a item 4.* It was
  prompted by the maintainer's pending decision on whether to accept
  hostname addresses received from peers. It is informative; R8c's
  normative text is unchanged.
  - *Inputs:* the forbidden corpus at the rows `v2.6.0-beta` and
    `v3.0.0-beta` of chapter 02 §2.15A. Seven source files were read
    at each row: the composed peer-to-peer behavior, the peer-exchange
    behavior, the seed-address parsing, the daemon's network start-up,
    its own-address detection and routability helper, the context's
    network flags, and the statistics peer registration. The inter-row
    diff was taken. The only difference in any respect taken is the
    default-netid value and the refused-netid list (chapter 06
    §6.6A.6). Also read, as permitted public sources and not
    forbidden-class material:
    - the libp2p fork both rows pin, for its floodsub, request-response,
      swarm, DNS and TCP crates;
    - the public DNS resolver library both rows pin, for its default
      options;
    - the public coins-configuration repository's seed-node list, at
      its last change before the `v2.6.0-beta` date and at its state
      on 2026-10-26.

    This repository's own peer-exchange behavior, composed behavior
    and routability helper (three files) were also read, for the
    findings.
  - *Method:* reading of the compared regions plus the inter-row
    diff. The mechanical gate tool was not run (prose). No private
    identifier, function body, helper decomposition, log or error
    literal was reproduced. The only literals the note carries are
    public multiaddr protocol names, numeric values a peer can observe
    in timing or in sizes, and public library defaults.
  - *Clearance:* a KDF Dirty Gate pass on 2026-10-31 cleared the
    note, the RQ1a item 4 correction (a re-clearance of the item
    cleared on 2026-10-30) and this bullet. *Extent:* the reference's
    address handling in the peer-to-peer subsystem at both rows: peer
    exchange, the composed behavior, the peers-topic announcement,
    seed configuration and dialing, own-address detection and the
    routability predicate, node-role flags, and relay-maintenance
    redial. *Method:* reading, mechanical extraction of every
    backticked and identifier-shaped token in the added lines tested
    against both rows, and a word 4-gram overlap; the mechanical gate
    tool was not run (prose). *Measure:* 0.02 against the 0.10
    threshold. The clearance is complete and not pending.
- *Scoped amendment (2026-10-31) -- R8c amended opening paragraph,
  items 7 to 10 and amended rationale, T6a's bullet for items 7 to 9,
  and D9.* Reloaded-original, maintainer decision BH of 2026-10-31.
  - *Inputs:*
    - the gated note above, for every reference fact;
    - this repository's peer-exchange behavior, request layer, composed
      behavior and swarm runtime;
    - the public libp2p fork's request-response crate;
    - a 25-minute run of a Reloaded node on the public netid-8762
      network on 2026-10-31.
  - The forbidden corpus was not consulted for this amendment, and it
    needs no Dirty Gate pass of its own.
