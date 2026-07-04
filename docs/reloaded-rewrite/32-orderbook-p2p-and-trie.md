# Chapter 32 -- Orderbook Patricia-Trie and P2P Surface

**Status:** driving-spec

> **One-sentence claim:** the order-matching subsystem shall
> maintain, per maker pubkey, a Patricia-Merkle trie over each
> alphabetically-ordered trading pair's open orders, hashed
> with a 64-bit-truncated BLAKE2b digest, and shall propagate
> order events plus periodic keep-alive trie-root
> advertisements over the libp2p gossip mesh so that subscribed
> peers detect divergence and pull verifiable deltas.

## 32.0 Executive Summary

The orderbook is not a flat set of open orders; it is a
two-level index layered over a Patricia-Merkle trie substrate.
Each known maker pubkey owns one trie per alphabetically-ordered
trading pair. The trie's leaves are that pubkey's open orders
for the pair, keyed by order UUID and serialised to a canonical
byte form; the trie's **root** is a 64-bit hash that a peer can
advertise in a periodic keep-alive message. A receiving peer
that already tracks the same pubkey for the same pair compares
roots: equal roots mean equal order sets; divergent roots mean
the receiver shall request a delta synchronisation and, failing
that, a full snapshot.

This chapter binds, by behaviour and public/dictated interface:

- the in-memory orderbook index and its consistency obligations
  (§32.3);
- the per-pubkey trie-root and keep-alive tracking state
  (§32.4);
- the order-record wire/in-memory split and the dictated wire
  shapes that carry orders between peers (§32.5);
- the dictated trie hashing scheme and the verifiable
  delta-sync machinery layered on it (§32.6);
- the dictated P2P request/response wire surface (§32.7);
- the public, cross-boundary entry points the libp2p ingestion,
  swap, and RPC layers call into the subsystem (§32.8);
- the lifecycle, keep-alive, and expiry semantics that govern
  all of the above (§32.9).

> **Binding scope (R36).** Throughout this chapter, requirements
> bind observable *behaviour*, the *public* cross-crate
> interface, and externally *dictated* interop (the P2P wire
> shapes and the trie hash algorithm, marked R29/R31). The
> private types, field layouts, helper decomposition, control
> flow, local names, and diagnostic wording an implementation
> uses to meet these requirements are informative, not mandated.
> Where a fragment is reproduced because the wire or hashing
> scheme dictates it, that is stated explicitly and
> distinguished from the discretionary Rust shape around it.

## 32.1 Baseline Verification

The order-matching subsystem long predates the clean-room
effort. Its components split across two epochs relative to the
baseline anchor of [Chapter 02](02-baseline-state.md) (commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`, 3 June 2022). The
classification below is by *component role and first-
introduction epoch only*; it is derived from the project's own
revision history and does not transcribe code.

**Pre-2022 baseline-carryforward** (governed by §1.3 R3; the
*code* descends from the GPLv2 baseline anchor and is **not**
subject to clean-room remediation — reported here and skipped):

| Component (role) | Basis |
| --- | --- |
| Orderbook multi-index storage (price-ordered set, pair reverse-indices, UUID set, full-order map) | Present at the baseline anchor. |
| Per-pubkey state tracking (keep-alive timestamp, per-pair trie roots, owned-UUID set, per-pair delta history) | Present at the baseline anchor. |
| Patricia-trie delta machinery (diff history keyed by predecessor root, full-or-delta response carrier, full-trie materialiser, empty-trie-root constant, `sp_trie` binding) | Present at the baseline anchor. |
| P2P propagation/sync messages (the inbound request enum, gossip ingestion entry, keep-alive handling, sync-state request/response, get-orderbook, best-orders, orderbook-depth) | Present at the baseline anchor. |
| Order-record wire/in-memory split (wire item, in-memory item, side-channel protocol-info / confirmation-settings maps, canonical trie-leaf encoding, byte-decode trait) | Present at the baseline anchor. |
| Order-request / keep-alive flow (periodic root advertisement, receipt-time recording, divergence-triggered sync) | Present at the baseline anchor. |
| Orderbook RPC adapters and peer-address derivation | Present at the baseline anchor. |

**Post-2022 modification** (in clean-room remediation scope;
classification only — no code-rewrite recommendation is made
here):

| Component (role) | Basis |
| --- | --- |
| Worker-owned trie-store separation: a dedicated mutation target holding the trie node store and per-pubkey state, fed by an asynchronous mutation-event queue drained by a single background task, replacing the baseline's in-line trie mutation held directly on the orderbook container | Introduced after the baseline anchor. |
| Cancellation-resurrection guard (a TTL-bounded record of recently-cancelled order UUIDs) | Introduced after the baseline anchor. |
| Per-order ephemeral self-pubkey set (gossip-loop self-recognition for fresh-keypair-per-order publication) | Introduced after the baseline anchor. |
| Event-streaming side effects on index mutation | Introduced after the baseline anchor. |
| Migration of the per-pair history and cancellation maps onto the workspace TTL-map helper (from the baseline time-cache helper) | Introduced after the baseline anchor. |

Per §1.3 R3, the pre-2022 rows are baseline-carryforward: their
*code* is out of clean-room remediation scope. Independently of
epoch, this chapter is a clean-room driving-spec and therefore
states all of the above by behaviour and contract only; it does
not reproduce the authorial expression of either epoch.

## 32.2 Subsystem Placement

The subsystem lives as order-matching logic inside the
application crate (`mm2_main`). Its mutable state is partitioned
across three concurrency domains so that the trie-mutation hot
path does not back-pressure the libp2p inbound queue of
[Chapter 28](28-libp2p-modernization.md):

- the **read index** — the in-memory orderbook used by matching,
  price-sorted iteration, and existence lookups (§32.3);
- the **trie store** — the Patricia-trie node store plus the
  per-pubkey trie-root/keep-alive/history state, owned by a
  single background mutation-applier task (§32.4, §32.6);
- the **subscription tracking** — the per-topic
  request/subscription state.

The read index and the trie store communicate through a
mutation-event queue: an index update enqueues one or more
trie-mutation events that the background task applies in
batches. The exact module layout, the names of these state
owners, and whether the subsystem is one file or several are
discretionary realisation concerns; a module split is deferred
work (§32.10 D1).

## 32.3 Orderbook Read-Index Behaviour

This section binds the behaviour of the in-memory read index.
How the index is decomposed into fields and helper methods is
discretionary (R36); the requirements below constrain only what
the index must guarantee.

**R-C1.** The read index MUST maintain, over the same logical
set of open orders, all of the following access paths, kept
mutually consistent:

- a **price-ordered view** per directed `(base, rel)` pair,
  yielding orders sorted by price and then by UUID;
- **reverse pair indices** that answer "which pairs does this
  coin participate in" without an O(N) scan of the whole order
  set;
- an **existence/membership view** per directed pair keyed by
  UUID;
- a **single authoritative store** of each order's full body,
  from which every other view borrows only the UUID (or, for
  the price-ordered view, a price+UUID sort key).

Any mutation MUST keep these views consistent as a unit;
partially-applied updates that leave the views disagreeing are
a class of bug an implementation MUST avoid (e.g. by routing all
mutation through a single logical update operation per order).

**R-C2.** The price-ordered view MUST be keyed by the *directed*
`(base, rel)` pair, not the alphabetically-ordered pair, so that
the buy-side and sell-side orderbooks for the same coin pair are
distinct. The ordering key MUST sort first by price and then by
UUID, so that two peers deriving an orderbook from the same set
converge on the same deterministic ordering of equal-priced
orders.

**R-C3.** The reverse pair indices (base→rels and rel→bases)
MUST exist so that pair-enumeration queries do not scan the
price-ordered view.

**R-C4.** The full-order store MUST be the single source of
truth for an order's complete body; the other views MUST carry
only the order's UUID (or price+UUID sort key).

**R-C5.** A separate cancellation-resurrection guard MUST exist:
a TTL-bounded record of recently-cancelled order UUIDs, consulted
on insert so that a `Cancel` message that races ahead of its
matching `Create` cannot resurrect an order. The guard's TTL is
bound by [§11.5](11-order-match-cancellation.md) and §32.9; this
chapter binds only the guard's presence and the obligation to
consult it on insert. *(Post-2022 component per §32.1.)*

**R-C6.** A set of this node's *own* ephemeral per-order
pubkeys MUST be tracked, so that orders this node published
under a fresh per-order keypair are recognised as self when they
reflect back through the gossip mesh (loop avoidance). This is
the mechanism the pre-burn-output path of
[Chapter 16](16-swap-v2-pre-burn-output.md) relies on. *(Post-
2022 component per §32.1.)*

**R-C7.** The read index MUST expose, to the rest of the
subsystem, the ability to: look up an order by UUID (optionally
constrained to a given maker pubkey); insert-or-update an order
and obtain the resulting trie-mutation events to forward to the
trie store; remove an order by UUID, obtaining its former body
and the matching removal event; and clear a whole
`(pubkey, pair)` slice of the index. The index-level operations
are *index-only*: they update the in-memory views and return the
trie-mutation events, but the authoritative trie reset is
performed by the trie store (§32.6). The exact method names,
signatures, and the split between public and private helpers are
discretionary (R36).

## 32.4 Per-Pubkey Trie-Root and Keep-Alive State

For each known maker pubkey, the subsystem MUST track the state
needed to detect divergence and answer sync requests. This
section binds that state behaviourally; its field layout and
the type used for the alphabetically-ordered-pair key are
discretionary (R36).

**R-P1.** Per maker pubkey, the subsystem MUST retain:

- the **last keep-alive receipt time**, in whole seconds of Unix
  epoch (to match the wire encoding of §32.5/§32.7);
- the set of **order UUIDs** the pubkey currently owns, tagged
  by alphabetically-ordered pair;
- the **trie root** the receiver currently holds for each
  alphabetically-ordered pair;
- a **per-pair delta history** (a TTL-bounded sequence of trie
  deltas keyed by predecessor root, §32.6) used to answer
  delta-sync requests.

The alphabetically-ordered pair is the order-side-independent
pair identifier (the same string regardless of which coin is
base and which is rel).

**R-P2.** Keep-alive comparisons MUST operate on second-
resolution timestamps to match the wire encoding. A freshly
created per-pubkey record MUST initialise its keep-alive time to
the current second.

**R-P3.** On processing a keep-alive, the receiver MUST record
its *own local receipt time* (in seconds) as the pubkey's last
keep-alive, not the sender-provided timestamp. The stored value
is "when this process last heard from the pubkey".

**R-P4.** The stored trie roots are the receiver's record of
each tracked `(pubkey, pair)` root. When a keep-alive advertises
a *different* root for a pair the receiver already tracks, the
receiver MUST request a sync (§32.7) targeting *only* the pairs
whose roots diverged.

**R-P5.** The per-pair delta history MUST let a peer that holds a
different but recent-enough root receive a compact delta list
rather than a full trie. The history is TTL-bounded: entries
older than the TTL are dropped, and a peer asking for a delta
against a dropped predecessor root MUST instead receive a full
snapshot (§32.6 R-T4).

**R-P6.** A per-pubkey record is evicted when no fresh keep-alive
arrives within the subsystem's pubkey-state TTL; eviction is
performed by the periodic maintenance described in §32.8/§32.9.

## 32.5 Order-Record Wire/In-Memory Split

The subsystem distinguishes the **wire** form of an order
(carried between peers and hashed into the trie) from the
**in-memory** form (which additionally caches per-protocol
metadata the local node may know). This section binds the wire
form as dictated interop (R29/R31) and the split as behaviour;
the in-memory record's exact Rust shape is discretionary (R36).

### 32.5.1 Dictated wire order record (R29/R31)

> **Binding scope (R36 / R29/R31, dictated interop).** The
> field set, types, and serialisation below are part of the P2P
> wire contract: any conforming peer must produce and consume
> exactly these bytes for orders to be exchanged and for trie
> roots to converge across implementations. They are dictated by
> the gossip protocol and the trie-leaf hashing scheme, **not**
> project expression. The commentary is freshly authored.

**R-W1.** The wire order record MUST carry exactly the following
eight fields, MessagePack-encoded in this order:

| Field | Type | Meaning |
| --- | --- | --- |
| `pubkey` | string | maker pubkey (hex) |
| `base` | string | base coin ticker |
| `rel` | string | rel coin ticker |
| `price` | rational (numerator/denominator) | price as an exact rational |
| `max_volume` | rational | maximum tradeable volume |
| `min_volume` | rational | minimum tradeable volume |
| `uuid` | UUID | order identifier |
| `created_at` | unsigned integer (seconds) | creation time |

Prices and volumes MUST be exact rationals (numerator and
denominator), not floating point, so that price comparison and
trie-root convergence are deterministic across peers.

**R-W3 (canonical trie leaf, dictated).** The bytes hashed into
the trie as an order's leaf MUST be the MessagePack encoding of
exactly these eight wire fields. The in-memory-only metadata of
§32.5.2 MUST NOT enter the trie leaf. The rationale is interop:
two peers tracking the same pubkey converge on the same root
only if they hash the same canonical record, and the side-
channel metadata is inherently asymmetric (a relay may not carry
the protocol metadata of a coin it does not support). How an
implementation projects its in-memory record down to these eight
fields before encoding is discretionary (R36); the *resulting
bytes* are dictated.

### 32.5.2 In-memory enrichment and the side channel (behaviour)

**R-W2.** The in-memory order record extends the eight wire
fields with locally-cached metadata that does **not** travel in
the order's own wire body:

- per-coin **protocol info** for the base and rel coins, and
- optional **confirmation/notarisation settings** for the order.

When orders are exchanged in bulk (the sync response of §32.7),
this metadata travels in two *side-channel maps* keyed by order
UUID, alongside the wire order records — not embedded in each
record. The receiver reconstitutes a full in-memory record from
a wire record plus the two side-channel lookups, defaulting the
metadata when the side channel lacks an entry. The map shapes
are dictated where they cross the wire (§32.7); the in-memory
reconstruction is behaviour.

**R-W2b (RPC-presentation enrichment, non-wire).** When an
in-memory order record is projected into the RPC orderbook
response shape returned to local callers, each entry is further
annotated with two presentation-only fields that never travel on
the P2P wire:

- `is_mine` (boolean) — true when the order's maker pubkey matches
  one of the node's own p2p pubkeys;
- `age` (signed 64-bit integer, seconds).

> **Upstream divergence (informative).** Despite its name, `age`
> does not carry an elapsed duration: in the current baseline it
> is populated with the UNIX timestamp (in seconds) at the moment
> the RPC entry is built, inherited unchanged from the upstream
> behaviour. Consumers that need a true elapsed age compute it
> locally. The field is part of the RPC response contract; its
> wire name and type MUST be preserved. Correcting its semantics
> would change observable RPC behaviour, so it is tracked here as
> a compatibility question rather than silently changed.
>
> **Decision (R-W2b.1).** The timestamp-as-`age` behaviour is
> **kept as-is** for RPC compatibility with existing GLEEC/upstream
> clients; it MUST NOT be changed in this CRD cycle.
>
> **Future reconsideration (R-W2b.2, deferred).** A later RPC
> revision SHOULD reconsider the semantics. The proposed fix is to
> add a *new* field carrying the true elapsed age in seconds
> (`order's creation timestamp` subtracted from `now`), leaving the
> legacy `age` field populated as today for backward compatibility,
> and to deprecate the legacy field on a documented schedule. This
> is intentionally **out of scope now** and recorded as a tracked
> to-do item.

**R-W2a (dictated side-channel record).** The base/rel protocol-
info side-channel value carried on the wire MUST be a record of
two opaque byte strings — one for the base coin, one for the rel
coin. This is part of the sync-response wire contract
(R29/R31).

**R-W4.** The subsystem MUST support, behaviourally:

- projecting an in-memory record down to the wire record;
- constructing an in-memory record from an inbound
  "maker-order-created" gossip message, attributing the
  originating pubkey from the *gossip envelope's signature*
  (the message body does not carry it).

**R-W5 (proof-carrying wire item, dictated).** For verifiable
delivery, the subsystem MUST define a wire form that wraps a wire
order record together with (a) the most recent signed pubkey-
state payload from which the trie root was derived and (b) a
trie proof, where a **trie proof is an ordered list of trie node
byte-strings** along the path to the order's leaf under the
pair's root. The in-memory counterpart of this wrapper MUST NOT
be serialisable; only the wire form crosses the network.
Signature verification of the signed payload flows through the
libp2p signed-envelope decoder of
[Chapter 28](28-libp2p-modernization.md), not through this
subsystem.

At the currently-bound behaviour level, proof population MAY be
deferred: the wrapper may be emitted with an empty payload and
an empty proof list while the wrapper shape is bound now and
proof generation remains future work (§32.10).

## 32.6 Bound Patricia-Trie Substrate

## 32.6 Patricia-Trie Substrate and Delta Sync

The subsystem builds its per-pubkey, per-pair tries on the
`sp_trie` Patricia-Merkle trie library under a project-specific
hash. This section binds the hash parameters as dictated interop
(R29/R31) and the surrounding machinery as behaviour; the Rust
shape of the mutation events, the store, the diff history, and
the helper functions is discretionary (R36).

### 32.6.1 Dictated trie hash (R29/R31)

> **Binding scope (R36 / R29/R31, dictated interop).** The hash
> function and its output width are part of the cross-peer
> contract: trie roots only converge across implementations if
> every peer hashes nodes identically. They are dictated by the
> divergence-detection scheme, not project expression.

**R-T1.** The trie node hash MUST be **BLAKE2b truncated to an
8-byte (64-bit) output**, used as the hash for an `sp_trie`
version-0 trie layout. The empty trie MUST hash to the constant
root that this layout assigns to the empty node set; the
subsystem MUST be able to recognise that empty-trie root without
materialising an empty trie (so that "this pubkey has no orders
for this pair" is detectable directly).

The 64-bit width is chosen for **wire compactness, not
cryptographic collision resistance**: the trie detects
divergence between cooperating peers, not adversarial tampering.
A collision merely triggers a redundant sync round-trip and is
harmless. An implementation MUST treat a failure to construct
the fixed-width hasher as a programmer error rather than a
recoverable condition (the width is a compile-time constant);
the specific diagnostic wording is not part of the contract.

### 32.6.2 Mutation events and the worker-owned store (behaviour)

> *Post-2022 component per §32.1: the asynchronous worker-owned
> store and the mutation-event queue were introduced after the
> baseline anchor.*

**R-T2.** Index updates MUST be expressed as discrete trie-
mutation events covering at least: clear a `(pubkey, pair)`
slice; insert/update an order's leaf; remove an order's leaf;
and drop an entire pubkey's tries. A test-only barrier event MAY
exist to let tests await the worker draining all preceding
events; if present it MUST be compiled out of production builds.
The event enum's name, variants, and payload layout are
discretionary (R36).

**R-T3.** A single worker-owned **trie store** MUST hold the
trie node store and the per-pubkey state of §32.4. It MUST apply
batches of mutation events, grouping consecutive events that
target the same `(pubkey, pair)` so each affected trie is opened
once, mutated, and closed once per group, avoiding per-event
open/close overhead. It MUST also be able to derive, from an
inbound keep-alive, the sync request (if any) needed to
reconcile a divergence. The store's struct shape and method
decomposition are discretionary (R36).

**R-T4.** A **delta history** MUST answer delta-sync requests.
Each stored delta is keyed by its **predecessor** root: "if you
hold root X and apply this delta, you reach root Y." This
inverse keying is deliberate — a peer announcing root X asks
"how do I get from X to your current root?", and the responder
looks up X and replies with the chained deltas (each delta
carries the next root so replays can chain). A full-or-delta
response carrier MUST be able to represent either a delta map or
a full key/value snapshot. When asked to bridge from a given
predecessor root to the current root, the subsystem MUST attempt
to chain successive deltas; if the chain is broken or stale, it
MUST fall back to a full snapshot. The history is TTL-bounded
per §32.4 R-P5.

**R-T5.** Trie failures MUST be surfaced as typed errors
distinguishing at least: an underlying trie-library error, a
byte-decode failure, and the invariant violation where a key
present in the trie has no retrievable value via the caller-
supplied value getter (indicating the trie nodes and the
surrounding index have diverged). Error variant names and
diagnostic wording are discretionary (R36).

**R-T6.** A materialiser MUST exist that, given a root, the node
store, and a caller-supplied "get value by key" closure,
returns every `(key, value)` pair under that root. The closure
indirection keeps the trie machinery agnostic of how values are
deserialised; callers resolve values against the read index (or
a wire-side equivalent).

**R-T7.** A single background task per process MUST drain the
mutation-event queue into the trie store and loop, isolating the
trie-mutation path from the libp2p inbound queue of
[Chapter 28](28-libp2p-modernization.md) so that blocking on the
trie-store lock does not back-pressure networking. The central
order-matching context MUST therefore expose handles to the read
index, the trie store, the mutation-event sender, and the
subscription tracking; a test-only drain barrier MAY exist. The
handle names and their container layout are discretionary (R36).

**R-T8.** The per-pubkey trie machinery MUST use a "get-or-init"
allocation discipline: a missing per-pubkey, per-pair, or per-
history entry is created with the default initial state and a
mutable reference returned, so callers never branch on absence.
The specific helper functions that realise this are private and
discretionary (R36).

## 32.7 P2P Request/Response Wire Surface

The subsystem answers a fixed set of peer-to-peer requests. The
request and response *shapes* are dictated interop (R29/R31);
the dispatch decomposition and the byte-decode trait that back
them are discretionary (R36).

### 32.7.1 Dictated request set (R29/R31)

> **Binding scope (R36 / R29/R31, dictated interop).** The
> request variant set, their tag names, and their field shapes
> are part of the P2P contract a conforming peer must speak.
> Commentary is freshly authored.

**R-Q1.** The inbound peer-request set MUST consist of exactly
these variants, serialised as a tagged enum:

| Variant | Payload | Purpose |
| --- | --- | --- |
| `GetOrderbook` | `base`, `rel` | request the full orderbook for a directed pair |
| `SyncPubkeyOrderbookState` | `pubkey`, map of alphabetically-ordered-pair → held root | reconcile divergence for the named pubkey's pairs |
| `BestOrders` | `coin`, action (buy/sell), `volume` (rational) | best orders satisfying a target volume |
| `OrderbookDepth` | list of `(base, rel)` pairs | order-count depth per pair |
| `BestOrdersByNumber` | `coin`, action, `number` | best N orders |

All five are *inbound* requests. Outbound replies are typed per
sub-handler and serialised separately.

> **RPC best-orders filter (informative).** The peer-to-peer
> `BestOrders`/`BestOrdersByNumber` request variants above carry
> only coin, action, and target volume/number. Separately, the
> node-local best-orders RPC request (the GUI-facing v2 request)
> carries an additional boolean field `exclude_mine` (default
> `false`); when `true`, the responder omits orders whose maker
> pubkey is the caller's own from the result set. The filter is
> applied node-side when building the RPC response and does not
> change the P2P request shape.

**R-Q2.** A single request dispatcher MUST route each variant to
its handler and return one of: an encoded reply payload; an
explicit "no data" outcome (delivered to the libp2p layer as an
empty reply); or a typed error. How the dispatcher is
decomposed into per-variant handlers is discretionary (R36).

**R-Q2a (dictated sync response, R29/R31).** The response to
`SyncPubkeyOrderbookState` MUST carry, on the wire:

- the most recent **signed pubkey-state payload** (opaque
  bytes);
- a map of alphabetically-ordered-pair → **full-or-delta** order
  set (§32.6 R-T4), the orders being wire order records
  (§32.5.1);
- the two **side-channel maps** (§32.5.2) — protocol info and
  confirmation settings — keyed by order UUID, each defaulting
  to empty when absent.

When building this response, the responder MUST populate the two
side-channel maps from exactly the orders present in the
delta/full branches, omitting entries for orders the delta
removes.

### 32.7.2 Byte-decode surface (behaviour)

**R-Q3.** Because the trie machinery operates over `Vec<u8>`
keys and values, the subsystem MUST provide a fallible
"decode-from-bytes" capability that re-types raw trie bytes into
the concrete key and value types when materialising a trie
(§32.6 R-T6). At minimum it MUST decode: a UTF-8 string; a wire
order record (from MessagePack); a 64-bit root (from its fixed
8-byte form); and a UUID (from its byte form). The trait name,
its error type, and the per-type implementations are
discretionary (R36); the *byte encodings* decoded are dictated
where they correspond to wire/trie content (R29/R31).

## 32.8 Cross-Boundary Entry Points and Internal Surface

This section binds the *behaviour* the subsystem exposes to
neighbouring layers. Function names, signatures, and the
public/private split are discretionary (R36) **except** for the
genuinely public, cross-crate entry points and the public RPC
types called out as such.

**R-F1 (topic and pair derivation).** The subsystem MUST derive,
deterministically: the alphabetically-ordered pair identifier
from a directed `(base, rel)` pair (the order-side-independent
sort-join of the two tickers); the gossip topic string for a
pair (from either the directed pair or the ordered pair); and
the inverse parse of a pair out of a topic. It MUST also decode a
peer-announced address format from opaque protocol-info bytes,
**degrading gracefully to the standard format on decode
failure** rather than dropping the peer (an unknown announced
format is treated as the default). The address-format decoder is
a public helper consumed across the crate boundary; its inputs
and the graceful-default behaviour are the contract.

**R-F2 (trie-driven order-set mutation).** The subsystem MUST be
able to (a) **replace** an entire `(pubkey, pair)` order set from
a full trie — first clearing the in-memory slice, emitting a
leading clear event, then rebuilding each order by reconstituting
it from the side-channel maps — and (b) **apply a delta**, where
a present order is an insert/update and an absent (removed) order
is a removal, emitting the matching mutation events. The
parameters these operations need (pubkey, ordered pair, and the
two side-channel maps) are passed together; their exact grouping
is discretionary (R36).

**R-F3 (gossip and request entry points).** The subsystem MUST
expose, as the public ingestion surface the libp2p layer of
[Chapter 28](28-libp2p-modernization.md) calls:

- a **gossip-message entry point** that accepts an inbound
  message (peer, raw bytes, relay flag) and returns whether the
  message advanced local state — the boolean drives gossip-relay
  (advance ⇒ relay, otherwise drop);
- the **peer-request dispatcher** of §32.7.

Internally these fan out to per-message handlers (keep-alive
processing, maker-order-updated processing, get-orderbook, sync-
state) each likewise reporting "did this advance our state?";
those handlers are internal and their signatures are
discretionary (R36).

**R-F4 (own-side mutation and queries).** The subsystem MUST
support, as internal surface: inserting/updating an order;
inserting/updating an order *as one of our own* (additionally
recording its pubkey in the self-pubkey set of §32.3 R-C6 so the
gossip reflection is recognised as self); deleting an order;
deleting an *own* order (additionally publishing the matching
cancel on the gossip topic when an own per-order key is
supplied); requesting and filling an orderbook from peers;
subscribing to a pair's orderbook topic; testing whether an
order is one of ours; resolving this node's internal pubkey;
gathering all maker pubkeys' orders for a directed pair
(collecting asks from `(base, rel)` and bids from `(rel, base)`,
grouping by maker pubkey and building the side-channel maps
alongside); and collecting orderbook metrics. Names and
signatures are discretionary (R36).

**R-F5 (confirmation-settings reconciliation).** The subsystem
MUST reconcile per-order confirmation-count and require-
notarisation settings against each coin's defaults, for both the
maker and taker sides, as consumed by the swap negotiation path
of [Chapter 13](13-swap-version-negotiation.md).

**R-F6 (peer-address derivation and RPC adapters).** The
subsystem MUST be able to derive the on-chain address a peer's
pubkey resolves to under a given coin's configuration, surfacing
a typed error for the failure modes (address-from-pubkey
failure, unsupported coin, malformed/absent config,
deserialisation failure). The address result is exposed through
a **public RPC type** that is a tagged union of a transparent
address (a string) and a shielded marker; this RPC shape
(tag/content discriminator and the two cases) is part of the
public RPC contract. The internal error type's variants and
wording are discretionary (R36). The subsystem MUST also adapt
an in-memory order into the RPC orderbook entry for both the ask
side (price as-is) and the bid side (price inverted and
confirmation settings reversed).

**R-F6a (subscription tracking).** The per-topic subscription
state MUST distinguish "already requested" from "subscribed but
not yet requested, since timestamp T". It is owned by the
subscription-tracking domain (§32.2), not by the read index. The
representation is discretionary (R36).

**R-F7 (empty-trie root).** The subsystem MUST be able to compute
the constant empty-trie root for its trie layout (§32.6 R-T1) to
detect "no orders for this pubkey/pair" without materialising an
empty trie.

## 32.9 Lifecycle, Keep-Alive, and Expiry Semantics

**R-K1 (cancellation guard TTL).** The cancellation-resurrection
guard of §32.3 R-C5 MUST retain a cancelled UUID for a fixed
window of **120 seconds**. Within that window, a late-arriving
create for the same UUID is suppressed. This value is fixed in
the current binding; making it operator-configurable is deferred
(§32.10 D2). *(Post-2022 component per §32.1.)*

**R-K2 (keep-alive advertisement).** Each node MUST periodically
advertise, per pubkey it makes orders under, its current per-pair
trie roots in a keep-alive message on the gossip mesh, so that
peers can detect divergence (§32.4 R-P4).

**R-K3 (pubkey-state expiry).** A tracked pubkey whose last
keep-alive receipt (§32.4 R-P3) has aged past the pubkey-state
TTL MUST be evicted, dropping its tries and per-pair state. The
per-pair delta history is independently TTL-bounded (§32.4
R-P5). Expiry sweeps are part of the subsystem's periodic
maintenance.

## 32.10 Deferred Work

D1. **Module split.** The subsystem is a single large module.
    A split into sub-modules (trie / wire / dispatch / module
    functions) is desirable but would multiply this chapter's
    cross-references; revisit after a split decision.

D2. **Cancellation-TTL configurability.** §32.9 R-K1 fixes the
    cancellation TTL to a compile-time value. Making it
    operator-configurable (per-coin or per-deployment) is
    deferred; the fixed value has held in production and the
    deferral is conservative.

D3. **Trie-hash width upgrade.** §32.6 R-T1 picks a 64-bit hash
    for wire compactness. If a future transport makes the saving
    irrelevant, widening to 128 or 256 bits would lower the
    collision rate further. This is a forward-compatibility
    item, not a defect, and is itself constrained by R29/R31:
    any width change is a wire-incompatible change all peers
    must adopt in lock-step.

D4. **Test-only mutation-barrier event.** The test-only drain
    barrier of §32.6 R-T2 lives on the production mutation-event
    type. A test-only worker wrapper posting a sentinel through
    a side channel would keep the production type clean; this is
    a refactor item.

## 32.11 External References

- The trie machinery is built on the Substrate Patricia-Merkle
  trie library ([`sp_trie`](https://docs.rs/sp-trie/)).
- The node hash is BLAKE2b in variable-output mode, truncated to
  8 bytes ([`blake2`](https://docs.rs/blake2/)).
- The order wire encoding is MessagePack
  ([`rmp_serde`](https://docs.rs/rmp-serde/)), chosen for
  compactness.
- The per-pubkey state TTLs use the workspace TTL-map helper,
  bound elsewhere in this document set.
- The libp2p ingestion that feeds the gossip-message entry point
  of §32.8 R-F3 is bound by
  [Chapter 28](28-libp2p-modernization.md).

## 32.12 Baseline Verifications

The order-matching subsystem is **present** at the baseline
anchor of [Chapter 02](02-baseline-state.md) (commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`); §32.1 classifies
which components are baseline-carryforward and which are post-
anchor modifications. The following are verifiable conformance
checks for the subsystem, stated behaviourally:

V1. **Trie-root advertisement detects divergence.** Apply the
    same order set to two independent orderbook instances and
    confirm their per-pubkey, per-pair trie roots match; mutate
    one and confirm the roots then differ.

V2. **Delta-sync round-trip.** Apply an order set to instance A,
    advertise A's root to instance B, have B pull and apply the
    delta via the sync path (§32.7), and confirm B's root then
    matches A's.

V3. **Cancellation guard.** Insert an order on A; on B, deliver
    the cancel ahead of the create (simulated reordering); and
    confirm the late create is suppressed by the cancellation
    guard of §32.3 R-C5 / §32.9 R-K1.

V4. **Wire/in-memory split is lossless.** Round-trip an order
    through the projection to its wire form plus side-channel
    maps and back, and confirm field-wise equality.

## 32.13 Provenance Footer

- *Inputs:* the project's own revision history (used for the
  epoch classification of §32.1, by component role and first-
  introduction epoch only — no code transcribed); the baseline
  anchor of [Chapter 02](02-baseline-state.md); the public
  `sp_trie` Patricia-Merkle trie library, the BLAKE2b hash
  specification, the MessagePack serialisation format, and the
  libp2p gossip/signed-envelope model — the external public
  specifications the wire and hashing contracts derive from;
  cross-chapter contracts (Chapters 11, 13, 16, 28).
- *Permitted-input classes used:* baseline source (epoch
  classification only); external public specifications (`sp_trie`
  trie layout, BLAKE2b, MessagePack, libp2p gossipsub);
  cross-chapter contracts (Chapters 11, 13, 16, 28); Interop /
  wire-and-algorithm-bound reuse (R29/R31) for the dictated
  fragments embedded in §32.5.1, §32.6.1, and §32.7.1 — namely
  the eight-field wire order record and its MessagePack
  encoding, the proof-carrying wire item and side-channel record
  shapes, the inbound request variant set and the sync-response
  shape, and the BLAKE2b-64 / `sp_trie` version-0 trie hash
  parameters and empty-trie root — whose authoritative source is
  the bytes any conforming peer must exchange for cross-peer
  interoperability and trie-root convergence, not the historical
  lineage's discretionary expression.
- *Sibling-allowlist consultations:* Chapter 28 (the libp2p
  ingestion and signed-envelope decoder feeding the gossip entry
  point); Chapter 13 (the swap version-negotiation path
  consuming the confirmation-settings reconciliation and the
  side-channel maps); Chapters 11 and 16 (the cancellation-TTL
  binding and the per-order ephemeral-pubkey mechanism).
- *Forbidden corpus:* consulted **only** to recover the
  externally-dictated wire-and-algorithm fragments of §32.5.1,
  §32.6.1, and §32.7.1, embedded as Interop reuse under R29/R31
  (only the field sets, encodings, request/response shapes, and
  hash parameters required for interoperability are reproduced;
  no upstream function bodies, private identifiers, helper
  decomposition, control-flow transcription, per-method tables
  keyed to internal names, or diagnostic / panic / log string
  literals accompany them). All other content is clean-room
  driving-spec stated by behaviour and public/dictated interface.
  Any residual similarity of a conformant realisation to the
  historical lineage — of either epoch — is governed by the R35
  gate and the R36 binding-scope notes that head every code-
  bearing section of this chapter.
