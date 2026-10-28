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
  all of the above (§32.9);
- the bounded and unbounded state of the subsystem and of the
  substrate beneath it, as facts of the reference every peer relies
  on (§32.9A).

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
bound by [§11.2](11-order-match-cancellation.md) and §32.9; this
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

The sender-provided timestamp remains part of the dictated wire
shape (§32.5) and MUST still be accepted, but the receiver MUST
make no use of it at all: it is not stored, not compared against
the local clock, not bounds-checked, and not a reason to reject
the message. [Chapter 59](59-legacy-ordermatch-negotiation.md)
R59.6.2 states the same rule from the negotiation side and
R59.6.3 draws the consequence this chapter's read paths depend
on — because every node on both sides of the wire expires peers
on its own receipt clock, a stale row cannot be inherited from a
peer by way of a mis-stated timestamp, so the orderbook read
index of §32.3 needs **no** consumer-side freshness filter and
adding one would be a divergence. R-P3 corrects the baseline,
which used the sender's timestamp here; see ch. 59 V3.

**R-P4.** The stored trie roots are the receiver's record of
each tracked `(pubkey, pair)` root. When a keep-alive advertises
a *different* root for a pair the receiver already tracks, the
receiver MUST request a sync (§32.7) targeting *only* the pairs
whose roots diverged. The exact trigger (including the "already
tracks" condition, which get-or-init turns into an all-zero
placeholder for a pair first seen in a keep-alive), the content of
the request, the responder's answer and the receiver's handling of
the outcome are bound by §32.7.3 R-Q4 to R-Q6; the shape of the
keep-alive that carries the advertisement is bound by §32.9 R-K2a.

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

**R-W1a (dictated order-message root member; sent as zeros,
never read).** *(Authored by the KDF Spec Reader 2026-10-28 from
the rows `v2.6.0-beta` and `v3.0.0-beta` of
[Chapter 02](02-baseline-state.md) §2.15A, identical at both.)*
The three per-order gossip messages — maker-order created,
maker-order updated (both generations of it) and maker-order
cancelled — each carry, besides the order fields and the
sender-side `timestamp`, a mandatory member `pair_trie_root` of
the trie-root type (the 8-byte root of §32.6 R-T1). It is part of
the dictated wire shape: a message lacking it does not decode.
Its *content* is not: the reference fills it with all zeros on
every one of these messages, and no receiver reads it — it is not
compared with any held root, it triggers no synchronization
request, and it is not stored. A sender therefore MUST emit the
member and MUST fill it with all zeros; a receiver MUST decode the
member and MUST make no use of its value, whatever it holds. The
only root a peer acts on is the one advertised by the keep-alive
(§32.9 R-K2a, §32.7.3 R-Q4). Reloaded sends zeros in all three
messages and ignores the member on receipt: **parity**, no
divergence to record.

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

When a maker sets no minimum volume, `min_volume` carries the per-coin
default of [Chapter 57](57-evm-fee-inputs.md) §57.7A R57.7A.4 (cleared
there on 2026-10-24 — see the second scoped-addition item of its §57.12);
a receiver validates it only for non-negativity. For every coin family,
the default is `max(b, q ÷ p)` over the two coins' minimum trading
volumes, and the maker answers no taker request below the order's
minimum volume ([Chapter 60](60-trading-floors-and-refusal-contract.md)
R60.4.2, R60.5.1–R60.5.3; cleared there on 2026-10-25).

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

**R-T3a (apply order).** *Reloaded-original (2026-10-27).* A batch of
mutation events MUST take effect exactly as if its events had been applied
one at a time, in queue order. The grouping of R-T3 is an optimization and
MUST NOT change the outcome. In particular, for events that concern one
order: a removal followed by an insertion of the same order leaves the order
present with the inserted value; an insertion followed by a removal leaves
it absent; two insertions leave the later value. For events that concern a
`(pubkey, pair)` slice: a clear followed by insertions leaves exactly those
insertions, and insertions followed by a clear leave the slice empty. A drop
of a whole pubkey's tries followed, in the same batch, by an insertion for
that pubkey leaves the insertion. The trie roots and the per-pubkey order
identifiers that result MUST be identical to those of applying the same
events as separate single-event batches.

> **Code-quality finding (informative).** A batch used to be applied in
> three passes per `(pubkey, pair)` group: all clears, then all insertions,
> then all removals, whatever the order the events arrived in. A removal
> queued before an insertion of the same order was therefore applied after
> it, and the order ended up missing from the trie while the read index held
> it. No path was found that queues such a pair for one order today, because a
> recently cancelled order is refused re-insertion, so this is a latent
> defect rather than an observed one.

**R-T3b (index and trie stay consistent where they are read).**
*Reloaded-original (2026-10-27).*

1. *Queue order.* A mutation event MUST enter the queue in the same order
   as the read-index operation that produced it. The event is therefore
   queued while that operation still holds the read index, so that two
   concurrent operations on one order reach the trie in the order they
   reached the index.
2. *No stale advertisement; the answer never refuses.* *(Amended
   2026-10-28 by the maintainer's decision on D5.)* A keep-alive
   advertisement MUST NOT reflect trie state older than the last completed
   read-index operation of this node: when a trie root is advertised, every
   mutation event produced by an index operation completed before that
   moment has been applied to the trie store. If the worker has not yet
   caught up, the keep-alive producer, which runs on its own task, waits for
   it for a short bounded time, short compared with the keep-alive interval
   and otherwise discretionary (R36), and if the bound is exceeded the
   keep-alive for that round is not sent (the next round carries the roots).
   The answer to a pubkey-state synchronization request MUST be built from
   the trie store as it stands when the request is handled, without waiting
   and without refusing, as the reference answers. A delta the answer
   reports is read from the delta history and ends at the trie store's
   current root. A full snapshot lists the orders under that root, each with
   the body the read index holds for it at that moment; an order the trie
   store still holds but the read index no longer does (its removal is
   queued for the worker) is left out of the snapshot. A lagging worker is
   therefore never a reason to fail the answer, and §32.6 R-T5's
   missing-value error remains reserved for a trie store and read index that
   have diverged with no queued event to explain it. A root an answer
   reflects that predates a completed index operation is tolerated: the
   requester's next keep-alive comparison (§32.7.3 R-Q4) corrects it one
   interval later, exactly as after any answer the maker has since moved
   past. *(Previously the answer checked once and returned a retryable error
   while the worker lagged; that outcome is withdrawn.)*
3. *Nobody else waits.* The producer of an index operation MUST NOT wait for
   the worker, and no wait of item 2 may happen on the inbound networking
   path (R-T7). *(Amended 2026-10-28, Decision AN, Reloaded-original.)* A
   synchronization request parked under §32.7.3 R-Q5b is not a wait on that
   path: the inbound handler hands the request's response channel over to
   the parked entry and returns at once; the answer is built and sent later
   from another task.
4. *The keep-alive receiver compares with a caught-up store.*
   *Reloaded-original (2026-10-28).* The comparison of an advertised root
   with the root this node holds (§32.7.3 R-Q4 steps 3 and 4) MUST use a
   trie store that reflects every index operation completed before the
   keep-alive is handled. The receiver records its receipt time first
   (§32.4 R-P3, as R-Q4 requires) and then MAY wait for the worker for the
   same short bound as the keep-alive producer; past the bound it compares
   with the store as it stands. A received keep-alive is handled on a task
   of its own, so this wait is not a wait on the inbound networking path of
   item 3. Without it, a worker that has not yet applied an operation this
   node completed makes the held root look older than it is, and the
   receiver sends a request a caught-up store would not have needed. The
   reference also applies its trie updates on a separate worker task fed
   by an unbounded queue (§32.10 D6; *corrected 2026-10-28, second pass,
   passed the KDF Dirty Gate on 2026-10-28 — the first wording of this item said it did not*)
   and performs no such wait, so its comparison can see a lagging store;
   this item makes Reloaded's comparison strictly less likely to request
   needlessly and is a Reloaded-original refinement, not a wire-visible
   divergence.

> **Measurement (informative).** On this project's own code, a cancelled order
> was still advertised in the pair's trie root, and could still be listed in a
> sync answer built from an older root the requester held, for the few
> milliseconds between the index update and the trie update (about 4 to 7 ms
> for a burst of 200 cancels with an idle worker in an optimized build). The
> queue between the two has no bound, and in an unoptimized build under load
> the lag grew to tens of seconds. Item 2 closes that window for the
> advertisement without bounding the queue; bounding it is a separate decision.
> Since the amendment of 2026-10-28 the window stays open for the
> synchronization answer, by decision (D5).

> **Upstream divergence / open decision (2026-10-28) — the refusing
> answer of R-T3b item 2.** *(Authored by the KDF Spec Reader from the
> rows `v2.6.0-beta` and `v3.0.0-beta` of
> [Chapter 02](02-baseline-state.md) §2.15A, identical at both.)* The
> reference never refuses a pubkey-state synchronization request: it
> answers from whatever its index and trie store hold at the moment the
> request is handled, with no notion of a lagging worker. R-T3b item 2 is
> Reloaded-original (gated 2026-10-27) and makes Reloaded's responder
> return a retryable error instead when its trie worker has not yet
> applied every completed index operation. The network consequence,
> stated from the requester's side under §32.7.3 R-Q5/R-Q6 and §32.8
> R-F3a: a requesting relay that receives an error keeps the all-zero
> placeholder it holds for the pair, withholds its application-level
> forward (R-F3b; its gossip-layer forward has already happened, R-F3a —
> *corrected in the second pass of 2026-10-28, passed the KDF Dirty Gate, which
> found the first pass's "does not forward" wrong*), and asks again only
> when the next keep-alive arrives (30 s later) — so one refusal by a busy
> Reloaded seed leaves every downstream peer that asks that relay with the
> placeholder failure of R-Q5 item 3 for at least one advertisement
> interval, and repeated refusals under load keep the maker's orders out
> of every peer behind that relay. A reference requester has no retry on the request itself
> (R-Q6). **Resolved 2026-10-28:** the maintainer adopted the
> alternative of §32.10 D5, and R-T3b item 2 now answers from the store
> as it stands, without refusing; this note records why.

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
| `SyncPubkeyOrderbookState` | `pubkey`, `trie_roots`: map of alphabetically-ordered-pair → a root the requester names as its predecessor (which root, see §32.7.3 R-Q4) | reconcile divergence for the named pubkey's pairs |
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

### 32.7.3 Keep-alive-driven synchronization (behavior, dictated at the wire)

> *Authored by the KDF Spec Reader 2026-10-28 from the rows
> `v2.6.0-beta` and `v3.0.0-beta` of
> [Chapter 02](02-baseline-state.md) §2.15A, identical at both, during
> the hotfix pass prompted by a live incident (public seed nodes
> overloaded while Reloaded makers ran). **Passed the KDF Dirty Gate on
> 2026-10-28** (PASS, no leaks; extent, method and measure in §32.13).*
> The request and response shapes are dictated (R-Q1, R-Q2a). The
> trigger, the root the request names, and the responder's reading of
> it are behavior every peer on the mesh relies on, so they are bound
> here as the interop contract; Reloaded's one permitted variation is
> stated as an explicit option (R-Q4a).

**R-Q4 (when a keep-alive produces a synchronization request, and what
it names).** On receiving a keep-alive (§32.9 R-K2a) from pubkey P via
forwarding peer F, the receiver MUST, in this order and before anything
else: record its local receipt time for P (§32.4 R-P3), creating P's
per-pubkey record if absent (§32.6 R-T8); then, for each `(pair, root)`
entry the keep-alive carries:

1. skip the pair if this node is not a relay and the pair's orderbook
   topic is not in its recorded subscription state (§32.8 R-F6a;
   [Chapter 59](59-legacy-ordermatch-negotiation.md) R59.6.7 item 1);
   a relay considers every pair;
2. skip the pair if the advertised root is all zeros or is the
   empty-trie root (§32.8 R-F7) — such a root is never requested;
3. otherwise look up the root this node holds for `(P, pair)`,
   **creating an all-zero placeholder entry if none exists** (get-or-init,
   §32.6 R-T8); a pair first heard of in a keep-alive is therefore
   tracked from that moment with an all-zero held root;
4. if the held root differs from the advertised root, add the pair to
   the request.

Recording the receipt time precedes every step, and steps 1 and 2 both
precede step 3. Steps 1 and 2 are skips without side effect, so their
order relative to each other is not observable and is not bound.

If the request is non-empty, the receiver MUST send exactly **one**
`SyncPubkeyOrderbookState { pubkey: P, trie_roots }` (R-Q1) to F — the
peer the keep-alive arrived from, not any relay — with one map entry
per diverging pair. The reference puts the **advertised** root into
each entry, so the request literally says "I hold the root you just
advertised; bring me from there to your current state"; the
consequences of that choice are drawn in R-Q4a. A matching root
produces no request and no other action.

**R-Q4a (Reloaded option: name the root actually held).**
*Reloaded-original, wire-compatible.* Because the responder (R-Q5)
treats every value in `trie_roots` only as the requester's claimed
predecessor root — it never compares it with the advertised root,
which it does not know — a requester MAY put its **own held root** for
the pair into the entry instead of the advertised one. The request
shape, the response shape and every responder on the mesh (reference
or Reloaded) are unaffected. The answer then is the delta from what the
requester really holds to the responder's current root, or a full
snapshot when the held root is the all-zero placeholder, the
empty-trie root, or a root the responder's history no longer covers.
**Decision (2026-10-28, maintainer): adopted.** Reloaded MUST name the
root it holds by default. This is a **deliberate, wire-compatible
Reloaded divergence**, recorded in the operator-facing compatibility
documents. The top-level `MM2.json` boolean
`orderbook_sync_names_held_root` (default `true`) selects it; only a
JSON `false` turns it off, and the request then names the advertised
root as R-Q4 describes.

> **Compatibility:** for behaviour matching GLEEC KDF, set
> `orderbook_sync_names_held_root` to `false`. *(Nothing a peer decodes
> changes either way; the setting only decides which root this node
> names as its predecessor.)*

> **Why the advertised-root form does not converge for a maker that
> changes state quickly (informative, behavioral).** Deltas are keyed by
> predecessor root (§32.6 R-T4). When the responder's current root for
> the pair *equals* the advertised root — the normal case, since the
> responder usually is the maker itself or a relay already in step with
> it — no delta starts at that root, so the responder answers with a
> **full snapshot** every time; the delta machinery is never used on
> this path. When the responder has already moved past the advertised
> root (the maker updated an order between its keep-alive and the
> request), the responder answers with the delta *from the advertised
> root* — a state the requester never held — and the requester applies
> insertions and removals meant for someone else's trie to its own,
> which leaves it with a root that matches neither the advertised nor
> the current one; the next keep-alive then differs again, and the
> cycle repeats once per advertisement interval for as long as the maker
> keeps changing. With the held-root form the requester receives the
> delta it can actually apply, converges after one round, and asks for
> a full snapshot only when it holds nothing (placeholder) or something
> too old. Fewer and smaller answers per keep-alive is also the effect
> the live incident needs.

**R-Q5 (the responder's answer).** On receiving
`SyncPubkeyOrderbookState { pubkey: P, trie_roots }`, the responder
MUST build the answer from its **own** held state for P, treating each
value in `trie_roots` only as the requester's predecessor root:

1. if the responder holds no state at all for P, the outcome is the
   explicit **"no data"** outcome of R-Q2 (an empty reply at the libp2p
   layer), not an error and not an empty snapshot;
2. the answer is **all-or-nothing** over the requested pairs: if any
   requested pair has no held-root entry for P at the responder, the
   whole request fails with a typed error that identifies the pair, and
   no pair is answered;
3. for each requested pair with a held root: if the responder keeps a
   delta history for `(P, pair)` and the history has an entry keyed by
   the requested root whose delta chain reaches the responder's current
   held root, the answer for the pair is the chained **delta**
   (§32.6 R-T4); otherwise — no history for the pair, no entry keyed by
   the requested root, or a chain that ends short of the current root —
   the answer for the pair is a **full snapshot** materialized from the
   responder's **own current held root** (§32.6 R-T6). The responder
   never opens a trie at the requested root. A held root that is itself
   the all-zero placeholder (the responder has heard P's keep-alive for
   the pair but has not yet synchronized) cannot be materialized: the
   open fails with the trie library's invalid-state-root error, which by
   item 2 fails the whole request;
4. the side-channel maps of R-Q2a are populated from exactly the orders
   present in the delta or snapshot branches, with entries removed for
   orders the delta removes; the signed-payload member is sent empty
   (§32.5.2 R-W5).

The answer is built from the index and the trie store under both locks,
synchronously, on the task that received the request (the reference
does so on its inbound networking event loop; §32.6 R-T7 lets Reloaded
place it elsewhere but R-T3b item 3 forbids waiting there).

**R-Q5a (how the responder's entries come and go; the three failure
classes).** *(Authored by the KDF Spec Reader 2026-10-28, second pass,
from the rows `v2.6.0-beta` and `v3.0.0-beta` of
[Chapter 02](02-baseline-state.md) §2.15A, identical at both.
**Passed the KDF Dirty Gate on 2026-10-28**, measure 0.02; §32.13.)* The outcomes of R-Q5 follow from how a node's
held state is created and removed:

1. a node creates a pubkey's record at the first trie insertion for that
   pubkey (an order-created message, or the application of an answer) or
   at the first keep-alive it handles from that pubkey — a non-relay
   creates the record too, before deciding whether any advertised pair is
   in its recorded subscriptions — and the record lives until the
   eviction of §32.9 R-K3;
2. a pair's held-root entry is created by an insertion, a removal or the
   placeholder of R-Q4 item 3, and is **removed** when a full snapshot for
   that pair is applied (the application first clears the slice) and when
   the record is evicted; it is **not** removed when the pair's last order
   is removed (the empty-trie root stays);
3. consequently a full snapshot that lists **no** orders — the
   responder's own current root for the pair was the empty-trie root when
   it answered, as after a maker cancelled everything on the pair between
   its keep-alive and the request — leaves the applying node with a record
   for the pubkey and **no entry for the pair**, and a later request to
   that node for that pair fails with the missing-pair error of item 2
   rather than succeeding with an empty answer.

The three failure classes a requester can meet are therefore: *no data*
(the responder holds no record for the pubkey: it has not yet handled any
message from it); *missing pair* (record present, entry absent: the
responder has not yet handled the keep-alive that advertised the pair, or
has applied a zero-order snapshot for it — the responder being always a
relay or the publishing maker, since a non-relay forwards nothing,
R-F3b; *correction re-checked by the KDF Dirty Gate on 2026-10-28*: a first wording also listed an
unsubscribed pair on a forwarding non-relay, which cannot occur);
*placeholder root* (entry present with all zeros: the responder's own
request for the pair is pending, has failed, or has not yet been applied
by its worker). A requester logs all three as a request error and retries
none (R-Q6). Reloaded's responder and applier behave the same:
**parity**.

**R-Q5b (Reloaded: a pair still being synchronized is answered when its
synchronization lands).** *Reloaded-original (2026-10-28, Decision AN); no
forbidden-class input.* A relay forwards a keep-alive at the gossip layer
before it handles it (§32.8 R-F3a), so its downstream peers ask it while its
own request for the same pair is still in flight, and R-Q5a's
placeholder-root failure is their certain outcome. To serve them instead,
Reloaded's responder MUST treat a `SyncPubkeyOrderbookState { pubkey: P,
trie_roots }` request as **deferrable** when, for every requested pair, the
responder holds a root for (P, pair) and, for every requested pair whose held
root is the all-zero placeholder, the responder's **own** keep-alive-driven
synchronization request for (P, pair) is in flight. An own request is in
flight from the moment it is sent until its answer's mutation events have
been applied by the trie worker, or until it fails; the wait for the worker
after a successful answer lasts at most the bound of item 3, so no record
outlives the longest parked request.

1. A deferrable request is **parked**. It waits for the own requests that
   are in flight when it arrives (a later own request for the same pair does
   not extend the wait). It is answered as soon as every own
   request it waits for has completed, or when its bound expires, whichever
   comes first. The answer is the one R-Q5 gives at that moment — the same
   outcomes, all-or-nothing as before. If an own request it waits for failed,
   or the bound expired, the requester receives the error it would have
   received at once.
2. A request that is not deferrable is answered at once, as R-Q5 binds:
   an unknown pubkey ("no data"), a missing pair, or a placeholder for which
   no own request is in flight.
3. The **bound** is the top-level `MM2.json` integer
   `sync_answer_deferral_ms`, default `5000` (5 s), counted from the
   request's arrival. It MUST stay well below the requester's 10 s timeout
   (R-Q6); a bound at or above that timeout gains nothing, because the
   transport abandons the request first. The value `0` turns deferral off:
   every request is answered at once, as in the reference.
4. **Limits.** At most 8 requests are parked per requesting peer and at most
   256 in total, a request counting once whatever the number of pairs it
   names; a request past either limit is answered at once. The per-peer
   limit MUST stay below the number of concurrent inbound streams the
   transport allows on one connection, because a parked request holds its
   stream open.
5. Parking does not wait on the inbound networking path (R-T3b item 3). It is
   the one case in which the answer is not built on the task that received
   the request (R-Q5's last paragraph).

> **Compatibility:** for behaviour matching GLEEC KDF, set
> `sync_answer_deferral_ms` to `0`.

> **Reloaded divergence (2026-10-28).** The reference answers every request
> at once. Against a relay that has just forwarded a keep-alive and is still
> synchronizing the pair, its subscribers fail, ask again only at the next
> keep-alive (R-Q6), and so wait one advertisement interval (30 s) per relay
> hop. Under this rule a Reloaded relay answers them within the bound, and on
> a relay they forward on success (R-F3b), so the next hop is served in the
> same round. The answer and its shape are unchanged; only its timing
> differs, and only for a request that would otherwise fail. Reference and
> Reloaded requesters alike accept a late answer within their timeout. Implemented 2026-10-28.

**R-Q6 (transport properties of the request, bound as facts the mesh
relies on).** The request is a single one-shot request-response
exchange with F: it times out after **10 seconds** with no answer; there
is **no retry** of a timed-out, failed or "no data" request — the next
keep-alive from P is the only thing that can trigger another; there is
**no de-duplication** of concurrent in-flight requests for the same
`(P, pair)` (two keep-alives processed concurrently produce two
requests); and there is **no rate limiting** of requests on either side.
What the requester does with each outcome is bound by §32.8 R-F3b
(its gossip-layer forward, on a relay, has already happened: R-F3a).
Reloaded today has the same four properties (parity); the hardening
candidates that would change them are recorded under §32.10 D6 and
are not bound here. As a responder, Reloaded answers a request for a pair
it is still synchronizing later rather than at once (R-Q5b).

**T-Q4** *Request content and target (R-Q4).* A keep-alive for P with a
single pair entry whose root differs from the held one, delivered as if
from peer F, produces exactly one `SyncPubkeyOrderbookState` to F
naming P and that one pair; a keep-alive whose root matches produces
none; an all-zero or empty-trie root produces none and leaves no held
entry; a pair not in the recorded subscription state produces none on
a non-relay node and one on a relay node; a pair never seen before
produces a request and leaves an all-zero held entry for `(P, pair)`.

**T-Q4a** *Held-root form (R-Q4a).* With the default setting, the request names the
requester's held root; against a responder holding a later root reached
by a recorded delta chain, the answer is a delta and the requester's
root afterwards equals the responder's; against a responder whose
history does not cover the named root, the answer is a full snapshot
with the same result; with the requester at the all-zero placeholder,
the answer is a full snapshot. With `orderbook_sync_names_held_root` set
to `false`, the request names the advertised root, as T-Q4 describes.

**T-Q5** *Responder outcomes (R-Q5).* For a responder holding P with
pairs X and Y: a request for X naming X's previous root yields a delta;
naming a root absent from the history yields a full snapshot of X;
naming X's current root yields a full snapshot of X; a request for X and
an untracked pair Z fails as a whole and names Z; a request for an
unknown pubkey yields the "no data" outcome; a request for a pair whose
held root is the all-zero placeholder fails as a whole.

**T-Q5a** *Entries come and go (R-Q5a).* Applying a full snapshot with no
orders for (P, X) to a node holding orders for (P, X) leaves P's record
and removes X's entry; a following request for (P, X) fails and names X.
Removing the last order of (P, Y) one at a time leaves Y's entry at the
empty-trie root, and a following request for (P, Y) yields an empty full
snapshot. A keep-alive handled on a non-relay for a pubkey never seen
creates the record even when no advertised pair is in its recorded
subscriptions.

**T-Q5b** *A pair still being synchronized is answered when its
synchronization lands (R-Q5b).* A relay holds the all-zero placeholder for
(P, X) and has its own request for (P, X) in flight. A request for (P, X) is
parked and the inbound handler returns without waiting; once the relay's own
answer is applied, the parked request is answered with the full set, well
within the bound. The same with the relay's own request failing: the parked
request receives the placeholder error, within the bound. With the relay's own
request never answering: the parked request receives the placeholder error
when the bound expires. A request for (P, Y) whose placeholder has no own
request in flight, a request naming a pair the relay does not track, and a
request for an unknown pubkey are each answered at once, as R-Q5 binds; a
request for (P, X) and (P, Y) together is answered at once with the
placeholder error. Past 8 parked requests from one peer, or 256 in all, a
further request is answered at once. With `sync_answer_deferral_ms` set to
`0`, every request is answered at once. After every path the parked table is
empty.

**T-Q6** *No retry, no de-duplication (R-Q6).* With the responder
silenced, one keep-alive produces exactly one request and nothing more
for at least two advertisement intervals; two keep-alives for the same
`(P, pair)` processed concurrently produce two requests.

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
  message advanced local state — the boolean drives the
  application-level forward of R-F3b (on a relay a second copy, whose
  first copy has already left at the gossip layer, R-F3a; on a
  non-relay no forward at all, R-F3b — *correction re-checked by the
  KDF Dirty Gate on 2026-10-28*);
- the **peer-request dispatcher** of §32.7.

Internally these fan out to per-message handlers (keep-alive
processing, maker-order-updated processing, get-orderbook, sync-
state) each likewise reporting "did this advance our state?";
those handlers are internal and their signatures are
discretionary (R36).

**R-F3a (a relay's gossip-layer forward precedes its own handling).**
*(Authored by the KDF Spec Reader 2026-10-28, second pass, from the
rows `v2.6.0-beta` and `v3.0.0-beta` of
[Chapter 02](02-baseline-state.md) §2.15A, identical at both, compared
with the baseline anchor (V5). **Passed the KDF Dirty Gate on 2026-10-28**,
measure 0.02; §32.13. Supersedes the
R-F3a cleared earlier the same day, which stated that a relay forwards a
keep-alive only after its own synchronization succeeded; that statement
was wrong for the reference and for Reloaded alike.)* On a node running
as a relay ([Chapter 28](28-libp2p-modernization.md) R12), the gossip
substrate forwards every message it accepts — a keep-alive included —
to the downstream topic peers that included this relay in their relay
mesh and to the relays of its own relay mesh (ch. 28 R10, R11a; *correction
re-checked by the KDF Dirty Gate on 2026-10-28*) **immediately on receipt, before the message is
delivered to the application** and independently of what the
application later decides. A relay therefore cannot withhold a
keep-alive, and nothing in this subsystem's handling of the message
delays its first hop. The behavior is post-baseline: the baseline's
substrate forwarded only on the application's report when manual
propagation was configured (V5). Reloaded's vendored substrate behaves
identically under the same relay flag: **parity**.

**R-F3b (the application-level forward).** The boolean of R-F3 is bound
for the keep-alive as follows. The receiver's own handling (§32.7.3
R-Q4) "advanced local state" exactly when either no synchronization
request was needed (every advertised pair was skipped or already
matched) or the request was answered within the timeout with a usable
answer — a decoded response whose per-pair order sets the receiver has
turned into index mutations and queued for the trie worker (§32.6
R-T7). The forward does not wait for the worker: at the moment of
forwarding the held root for the pair may still be the all-zero
placeholder. On a timeout, a "no data" outcome, a transport error or an
error answer the boolean is false, and the receiver MUST leave its held
root for each requested pair as it was (normally the all-zero
placeholder of R-Q4 item 3); nothing is retried (R-Q6), and the
placeholder stays until either the next keep-alive from that pubkey
repeats the cycle or the pubkey's state expires (§32.9 R-K3; 90 s by
default — a pubkey whose keep-alives keep arriving does not expire,
§32.9A R-B1 row 1). When the boolean is true, a relay forwards the
message **a second time** (the substrate de-duplicates none of its own
outbound copies; downstream peers drop the second copy by their
duplicate cache). A **non-relay forwards nothing it received, whatever
the boolean**: the application issues the forward only on a relay, and
the substrate under manual validation does not forward for a non-relay
either (ch. 28 R11a); on a non-relay the boolean's only effect is the
state update it reports. *(Correction re-checked by the KDF Dirty Gate on 2026-10-28: this
replaces a first wording that had a non-relay forward once, on success,
to its mesh peers.)* A keep-alive therefore reaches a peer only from a
relay or from the maker that published it, and those are the only
responders a synchronization request can be addressed to. Reloaded
forwards and withholds on the same conditions, with the same relay-only
gate in its dispatch: **parity**.

> **Consequence for the mesh (informative; rewritten in the second
> pass).** Because a relay's first forward precedes its own handling, a
> downstream peer that receives a keep-alive from relay S and finds a
> divergent root asks S (R-Q4: the peer the message arrived from)
> **while S may not yet have handled the same message**. S then answers
> with one of the three outcomes of R-Q5a, all of which the asking peer
> logs as a request error and none of which it retries (R-Q6): *no data*
> when S holds no record for the pubkey yet; *missing pair* when S holds
> a record but has not yet installed — or has just removed — the pair's
> entry; *placeholder root* when S has installed the placeholder but has
> not yet applied a usable answer of its own. The rate of these failures
> scales with the number of keep-alive messages per second times the
> number of downstream peers asking each relay, and the asking peers
> converge only on a later keep-alive. The two outbound copies per
> message per downstream peer double the relay's outbound load (§32.9A
> R-B1 row 17). An answer that fails for the whole request (R-Q5 item 2)
> or a request too large to be answered within 10 s (K-pair requests
> against K full tries, §32.9 R-K2a) leaves the asking peer at the
> placeholder for a full advertisement interval; the keep-alive itself
> has travelled on regardless.

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

**R-F6b (shielded outcome is a success, not a failure).** For a
coin family whose display address cannot, or by design should
not, be shown as a transparent string derived from the order's
P2P identity pubkey, address derivation MUST succeed with the
shielded marker of R-F6 rather than fail with the unsupported-coin
error. This applies for two distinct reasons, both currently
recognised:

- the coin's real receiving address is cryptographically
  unrelated to the order's P2P identity pubkey, so no derivation
  from that pubkey can ever produce it -- this is Siacoin's case:
  its address is derived from a per-coin ed25519 key ([Chapter
  20](20-siacoin-integration.md) R-T3, R-S9), which is not, and is
  not derived from, the secp256k1 key the order's P2P identity
  uses; or
- the coin's protocol deliberately keeps the payment-receiving
  identity private and showing it would leak information the
  protocol is designed to withhold -- this is Bitcoin Lightning's
  case ([Chapter 41](41-lightning-network.md)).

A privacy-shielded coin whose protocol itself defines a shielded
address class (for example a Zcash-Sapling-shielded coin) reaches
the same shielded outcome for the more direct reason that it has
no transparent address to show at all. In every one of these
cases the order MUST still appear in the orderbook / best-orders
response, carrying the shielded marker in place of an address
string; treating any of these coin families as an unsupported-coin
address-derivation failure is a conformance defect, because it
feeds R-F6c's drop path and removes the order from the response
entirely rather than displaying it without an address.

> **Code-quality finding (informative, resolved).** This
> repository's peer-address-derivation function previously
> answered the unsupported-coin failure, not the shielded success,
> for both the Siacoin and the Lightning coin families -- the two
> cases R-F6b binds to the shielded outcome. Combined with R-F6c's
> drop-on-failure behaviour, this meant every live Siacoin or
> Lightning order was absent from both the `orderbook` and the
> `best_orders` responses, in either query direction, rather than
> being shown with a shielded marker; this is the defect shape a
> live Siacoin maker order was observed to exhibit. This
> repository's code has since been corrected so that both arms
> return the shielded success value, matching the treatment already
> given to the privacy-shielded coin family R-F6b also names; the
> fix changed only these two arms' outcome and left R-F6c's general
> drop behaviour untouched, which correctly remains for a coin
> family that has no address-resolution path defined at all.
> *(Scope note, 2026-10-26: that correction reached the mmrpc-2.0
> generation only; the legacy generation is bound and its remaining
> defect recorded under R-F6d and R-F6e below.)*

**R-F6c (address-resolution failure drops only that order).**
When address derivation for a specific order's coin genuinely
fails -- because no derivation path is defined for that coin
family at all, as opposed to the shielded case of R-F6b -- the
response-assembly behaviour for both the `orderbook` and the
`best_orders` RPC surfaces MUST omit only that one order from the
response and continue assembling the rest; it MUST NOT abort the
whole response. This is the accepted, currently-shipped behaviour
for a coin family that has no address-derivation path implemented
at all (observed for Solana/SPL-token orders); it is not a design
goal to eliminate generally, and it is not the correct handling
for the R-F6b cases, which must not reach this failure path in the
first place. A future coin family that gains a real
address-derivation path moves from this rule to the transparent
case of R-F6; a future coin family recognised as address-
undisplayable-by-design moves to R-F6b's shielded case instead of
sitting on this rule.

**R-F6d (two RPC generations, one derivation, two renderings).**
The orderbook read surface is served in two RPC generations, and
an order entry's `address` member has a different shape in each:

- the legacy `orderbook` and `best_orders` methods (the
  envelope-less generation) carry `address` as a **plain
  string**;
- the mmrpc-2.0 `orderbook` and `best_orders` methods carry
  `address` as the tagged union of R-F6: an object whose
  `address_type` member is `Transparent`, with the address string
  in `address_data`, or whose `address_type` member is
  `Shielded`, with no `address_data` member.

Both generations MUST resolve the address through the single
derivation of R-F6 -- the same coin-family-to-outcome mapping,
the same shielded success of R-F6b, the same drop-only-that-order
rule of R-F6c -- and MUST differ only in how the outcome is
rendered. The derivation runs for the coin the entry is listed
under: the requested base coin for an ask, the requested rel coin
for a bid, and, for `best_orders`, the coin the entry is grouped
under in the response (the counterpart of the requested coin).

*Reference provenance.* At `v2.6.0-beta`, at `v3.0.0-beta` and
at `dev (as of 2026-03-27)`, the two generations do **not** share
a derivation. The mmrpc-2.0 methods behave as R-F6, R-F6b and
R-F6c describe: a shielded-coin order is rendered with the
`Shielded` marker, and an order whose coin family has no
derivation is omitted while the rest of the response is served.
The legacy methods instead route through an older, coin-generic
address-from-pubkey resolver that has no shielded outcome at all
and, at all three rows, refuses the shielded (`ZHTLC`), Lightning,
Siacoin and Solana/SPL families; the TRON family it refuses at
`v2.6.0-beta` only, while at `v3.0.0-beta` and `dev (as of
2026-03-27)` it derives a transparent TRON address from the
pubkey. The legacy `orderbook` then **fails the whole
response** on the first such order, so a pair carrying one
shielded-coin ask (or, queried in the other direction, one
shielded-coin bid) has no readable legacy orderbook at either
reference row; the legacy `best_orders` silently omits such
orders. A desktop client of the reference lineage consumes the
mmrpc-2.0 generation, which is consistent with the legacy
defect having persisted through both rows unnoticed. Reloaded
does not inherit it: the legacy-generation behavior bound by this
rule and by R-F6e is a deliberate Reloaded divergence from the
reference, confined to a node-local RPC response and invisible on
the peer-to-peer wire.

**R-F6e (legacy string form of the shielded outcome).** When an
entry's derivation yields the shielded outcome of R-F6b and the
response is of the legacy generation, the `address` string MUST
be the fixed placeholder `Shielded` -- the same spelling as the
mmrpc-2.0 `address_type` tag value, so that a consumer moving
between generations meets one vocabulary. It MUST NOT be an empty
string, the order's pubkey, a transparent address derived from
that pubkey, or any other value that could be mistaken for a real
address of the coin. The placeholder is display-only; nothing in
the order-matching or swap paths reads it back.

> **Reloaded divergence (2026-10-26).** Observed on the
> development line: with one maker order for a shielded-coin/UTXO
> pair on the book, the legacy `orderbook` request for that pair
> failed on every call from a second node with an internal error
> stating that address derivation is unsupported for the shielded
> protocol, while the maker's own `setprice` had succeeded.
> Reloaded's legacy `orderbook` path still resolves addresses
> through the coin-generic resolver described under R-F6d and
> treats its refusal as fatal for the whole response -- parity
> with a reference defect -- so a taker on the legacy generation
> cannot read any pair that carries a shielded-coin order on the
> derived side. Reloaded's legacy `best_orders` path uses the same
> resolver and drops such orders instead. The fix is to route
> both legacy methods through the single derivation of R-F6 and
> render the outcome per R-F6e (shielded) and R-F6c (drop only
> that order); the mmrpc-2.0 paths already conform.

> **Code-quality finding (informative).** Two things fall out of
> reading Reloaded's two generations side by side. First, the
> earlier finding under R-F6b, recorded there as resolved, was
> resolved for the mmrpc-2.0 generation only: the legacy
> generation never called the corrected derivation, so a live
> Siacoin or Lightning order still aborts the legacy `orderbook`
> and is still dropped from the legacy `best_orders` -- the very
> defect shape that finding describes. Second, Reloaded's
> coin-generic resolver refuses the Tendermint family (platform
> coin and IBC token alike), which the reference's equivalent
> derives transparently at both rows and which Reloaded's own
> mmrpc-2.0 derivation also derives; so the legacy `orderbook`
> for a pair with a Tendermint-coin order on the derived side
> fails in Reloaded where it succeeds in the reference. Routing
> the legacy generation through the R-F6 derivation, as R-F6d
> requires, cures both at once. The two legacy RPC paths are the
> coin-generic resolver's only callers in Reloaded today, so after
> the routing change it has none and may be removed as dead code;
> this rule does not require its removal.

**T-F6.1** *Legacy orderbook with a shielded-coin order
succeeds.* A test shall place a maker order selling a
`ZHTLC`-protocol coin for a UTXO coin on one node, then call the
legacy `orderbook` for that pair (shielded coin as base) and for
the reversed pair (shielded coin as rel) from a second node. Both
calls shall succeed; the first shall list the order among the
asks and the second among the bids, each with `address` equal to
the string `Shielded`. The same two calls over mmrpc 2.0 shall
list the same order with `address` equal to
`{"address_type":"Shielded"}`.

**T-F6.2** *Address shapes agree across generations.* For a
UTXO-coin order, the legacy entry's `address` shall be the
transparent address string, and the mmrpc-2.0 entry's `address`
shall be `{"address_type":"Transparent","address_data":<that
same string>}`. For a shielded-coin order, the pair of shapes is
the one T-F6.1 asserts. The two generations shall never disagree
on which R-F6 outcome an order received.

**T-F6.3** *One unresolvable order never fails a response.* A
test shall seed an orderbook with one well-formed UTXO-coin order
and one order whose coin's configured protocol family has no
address derivation at all (the R-F6c case; the Solana family is
such a case today), both on the same pair side, and call all
four methods -- legacy and mmrpc-2.0 `orderbook`, legacy and
mmrpc-2.0 `best_orders`. Every call shall succeed, shall contain
the UTXO-coin order, and shall omit the unresolvable order; none
shall return an error.

**T-F6.4** *Legacy best_orders includes shielded-coin orders.*
With the order of T-F6.1 on the book, a legacy `best_orders`
request that matches it shall list it under its coin with
`address` equal to the string `Shielded`, and the mmrpc-2.0
`best_orders` shall list it with `address` equal to
`{"address_type":"Shielded"}`.

> **Clearance (chapter 01 T1, R40) -- passed.** R-F6d, R-F6e,
> the Reloaded-divergence note, the code-quality finding and
> T-F6.1--T-F6.4 above were authored by the KDF Spec Reader on
> 2026-10-26 under the two-team procedure of chapter 01 §1.4A and
> **passed the KDF Dirty Gate** on 2026-10-26 (PASS, no leaks to
> scrub). The extent, method and measure are recorded once, in the
> clearance item of §32.13, and are not restated here.

**R-F6a (subscription tracking).** The per-topic subscription
state MUST distinguish "already requested" from "subscribed but
not yet requested, since timestamp T". It is owned by the
subscription-tracking domain (§32.2), not by the read index. The
representation is discretionary (R36). The recording of this state by
the own-order re-announcement path, and its gating effect on
keep-alive-driven synchronization requests on a non-relay node, are
bound by [Chapter 59](59-legacy-ordermatch-negotiation.md) R59.6.7.

**R-F7 (empty-trie root).** The subsystem MUST be able to compute
the constant empty-trie root for its trie layout (§32.6 R-T1) to
detect "no orders for this pubkey/pair" without materialising an
empty trie.

**T-T1** *Apply order within a batch (R-T3a).* One batch of mutation events
is applied to a trie store for each of these event sequences on one order and
one `(pubkey, pair)`: remove then insert; insert then remove; insert with
value A then insert with value B; clear then insert; insert then clear; and,
for a pubkey, drop then insert. After each batch the order is present with the
value, or absent, that sequential application gives, the pair root equals the
root of a trie built from only the surviving orders, and the pubkey's order
identifiers match.

**T-T2** *Batch equals sequence (R-T3a).* For a deterministic pseudo-random
mix of at least 200 events over several orders, two pairs and two pubkeys,
including removals, re-insertions, clears and drops, applying all events in one
batch and applying them as single-event batches yield identical pair roots and
identical per-pubkey order identifiers.

**T-T3** *The advertisement never runs ahead of the trie, the answer never
refuses (R-T3b item 2).* With the worker held back, an order is cancelled (or
its price changed) in the read index. While the trie still lacks that event, a
keep-alive is not produced for the affected pubkey. A sync request for that
pubkey is answered without waiting and without an error, for the request
roots of the old trie state and of the older state a peer might hold: a full
snapshot leaves the cancelled order out (and lists a changed order with its new
price), and a delta ends at the trie's current root. Once the worker has
applied the event, a keep-alive carries the new root and the sync answers
reflect the cancel (the cancelled order is absent, the changed price is the new
one). With the worker released from the start, nothing is withheld.

**T-T4** *Queue order under concurrency (R-T3b item 1).* Two threads each run
many rounds of operations on the same order: one updates its price, the other
cancels it and the first then finds it gone. After all events are applied, the
trie holds the order exactly when the read index holds it, with the same value,
for every round and for at least 200 rounds.

**T-T5** *The keep-alive receiver waits for the worker (R-T3b item 4).* A
node's index holds an order the trie worker has not yet applied, and a
keep-alive advertises the root that includes it. When the worker catches up
within the bound, no synchronization request results. When it stays behind
past the bound, the comparison uses the store as it stands and a request
results. In both cases the receipt time is recorded before the wait.

## 32.9 Lifecycle, Keep-Alive, and Expiry Semantics

**R-K1 (cancellation guard TTL).** The cancellation-resurrection
guard of §32.3 R-C5 MUST retain a cancelled UUID for a fixed
window of **120 seconds**. Within that window, a late-arriving
create for the same UUID is suppressed. This value is fixed in
the current binding; making it operator-configurable is deferred
(§32.10 D2). *(Post-2022 component per §32.1.)*

**R-K2 (keep-alive advertisement).** Each node MUST periodically
advertise, per pubkey it makes orders under, its current per-pair
trie roots in keep-alive messages on the gossip mesh, so that
peers can detect divergence (§32.4 R-P4). The granularity of that
advertisement — one message per pair, each carrying one root, each on
its own pair's topic — is bound by R-K2a; "its current per-pair trie
roots" in this rule is to be read as the union over those per-pair
messages, not as one message holding all of them.

**R-K2a (keep-alive granularity, dictated interop).** *(Authored by
the KDF Spec Reader 2026-10-28 from the rows `v2.6.0-beta` and
`v3.0.0-beta` of [Chapter 02](02-baseline-state.md) §2.15A,
identical at both, during the hotfix pass prompted by a live incident.
Passed the KDF Dirty Gate on 2026-10-28; see §32.13.)* On
every advertisement tick (the interval of
[Chapter 59](59-legacy-ordermatch-negotiation.md) R59.6.4, 30 s in a
production build), for each pubkey the node makes orders under — the
persistent one and each per-order one — and for **each pair** for which
that pubkey's held trie root is neither all zeros nor the empty-trie
root (§32.8 R-F7), the node MUST publish **exactly one** keep-alive
message (`trie_roots` plus `timestamp`, [Chapter 59](59-legacy-ordermatch-negotiation.md)
R59.6.2) whose `trie_roots` map holds **exactly that pair's single
entry**, signed by that pubkey, **on that pair's orderbook topic only**
(§32.8 R-F1). A pair whose root is all zeros or the empty-trie root is
not advertised at all; a pubkey with no state, or no advertisable pair,
produces no message. A keep-alive message with more than one entry in
`trie_roots`, or published on a topic other than the one entry's pair
topic, is non-conforming. The reference publishes exactly this shape;
every reference receiver processes a keep-alive as *one* independent
gossip message per pair (one handler task, one comparison, at most one
single-pair synchronization request, one forwarding decision), and
that per-pair independence is what the shape exists to guarantee.

> **Reloaded divergence (2026-10-28) — the incident shape; removed
> the same day, Reloaded now conforms to R-K2a (parity).** Until then
> Reloaded built, per pubkey and tick, **one** payload
> whose `trie_roots` holds **all** K advertisable pair roots and
> published that same payload once on **every** one of the K pair topics
> (K distinct gossip messages, each carrying K roots). This is a
> wire-decodable but non-conforming shape, and against reference
> receivers it multiplies work quadratically: a relay subscribed to the
> K topics receives K messages for one maker per tick; each is handled
> on its own task and compares all K roots; the first to run installs K
> all-zero placeholders (R-Q4 item 3) and sends one K-pair
> synchronization request to the forwarding peer; the other K−1,
> running concurrently before any answer has been applied, see the same
> K mismatches and send K−1 more K-pair requests (R-Q6: no
> de-duplication); the responder answers each with K full snapshots
> (R-Q4a note: the advertised-root form yields snapshots, not deltas) —
> K² tries built synchronously under the responder's locks, K² tries
> decoded and applied by the requester. A single pair the responder
> cannot answer (R-Q5 items 2 and 3, a placeholder root on the
> responder, or the 10 s timeout on a large answer) fails **all** K
> pairs of the request; on a relay the K copies have already been
> forwarded at the gossip layer (R-F3a), so every downstream peer sends
> its own K-pair requests to that relay and meets the same placeholders
> (R-Q5a); a non-relay forwards nothing it received in any case (R-F3b;
> *correction re-checked by the KDF Dirty Gate on 2026-10-28*). With one
> single-entry message per
> pair the same maker costs K requests of one pair each at most, and a
> failure is confined to its pair. The aggregated form was present on
> the network while the public seed nodes were overloaded; the swaplab
> control showed no growth attributable to the aggregated shape alone
> (*softened 2026-10-28, see §32.13*). Conforming to R-K2a removes its
> cost, and nothing in the receiving side of Reloaded
> depends on the aggregated form (T-K4 checks that the receiver ends in
> the same state either way).

**T-K1** *One message per pair per tick (R-K2a).* A maker with orders
under one pubkey on three pairs publishes, per advertisement tick,
exactly three keep-alive messages for that pubkey, each with a
single-entry `trie_roots`, each on its own pair's topic, and no message
on any other topic; a maker with per-order pubkeys publishes the same
pattern per pubkey.

**T-K2** *Single entry, correct topic (R-K2a).* Every published
keep-alive is decoded and checked: `trie_roots` has exactly one entry,
the message's topic is the orderbook topic of that entry's pair
(§32.8 R-F1), and the entry's root equals the pubkey's held root for
that pair at the time of the tick (subject to §32.6 R-T3b item 2).

**T-K3** *Zero and empty-trie roots are not advertised (R-K2a).* After
all of a pubkey's orders on a pair are cancelled, no keep-alive is
published for that pair while messages for the pubkey's other pairs
continue; a pubkey with no advertisable pair publishes nothing.

**T-K4** *A conforming receiver is unaffected by the change (R-K2a,
R-Q4).* Fed the per-pair messages of T-K1 as if from a peer, a
receiver ends with the same held roots and the same order set as when
fed the aggregated form, and issues at most one single-pair
synchronization request per message.

**R-K3 (pubkey-state expiry).** A tracked pubkey whose last
keep-alive receipt (§32.4 R-P3) has aged past the pubkey-state
TTL MUST be evicted, dropping its tries and per-pair state. The
per-pair delta history is independently TTL-bounded (§32.4
R-P5). Expiry sweeps are part of the subsystem's periodic
maintenance.

The TTL's concrete value, its operator override, the
advertisement interval R-K2 runs at, and the exemption of the
node's **own** pubkeys (both the persistent one and the
per-order ones) from this eviction are bound by
[Chapter 59](59-legacy-ordermatch-negotiation.md) R59.6.4 and
R59.6.5, which owns the liveness window because the same sweep
also drives that chapter's dead-order behaviour. The per-order
half of the own-pubkey exemption is **not** implemented here
today (ch. 59 R59.6.5), so this subsystem can currently evict a
pubkey the node itself makes orders under and drop those orders
from its own read index.

**R-K4 (memory maintenance of the synchronization state; Reloaded-original,
2026-10-28).** The delta history of R-P5 and R-T4 and the node store of R-T1
stay as they are on the wire and in their answers. What this rule adds is that
the memory they hold is released when it is due, not only when traffic happens
to touch it, and that nothing leaks when a slice is cleared:

1. *Expiry on a timer.* Every 30 seconds, independently of any traffic, each
   pubkey state drops the expired entries of its per-pair history cache (the
   4-hour bound) and of each pair's delta history (the 5-minute bound), and
   releases the spare capacity of those tables. Which entries are expired does
   not change: an entry whose expiry was refreshed is not dropped, and a delta
   answer for a root inside its window is unchanged by a sweep. A pair's history
   is removed only by its own 4-hour bound, never because its deltas expired.
2. *Refreshing an expiry is free.* Refreshing a cache entry's expiry does not
   add bookkeeping: each key has one expiry-queue element, and a queue element
   whose entry was refreshed is re-queued at its true expiry when it is reached.
3. *Node store.* After each ten-minute purge the node store releases the spare
   capacity of its table. Where the allocator keeps freed pages in the
   process (glibc), the node also asks it to return them to the system, outside
   every lock, and logs the resident size before and after at debug level.
4. *Clearing releases nodes.* Clearing a `(pubkey, pair)` slice, and removing a
   pubkey, dereferences the nodes of the cleared tries, so that the purge
   reclaims them. A node still referenced by another trie stays (the store
   counts references). This adopts the "orphan release" candidate of D6.
5. *Gauges.* The node reports, besides `orderbook.len`, the number of pair
   histories and of stored deltas, the number of tracked pubkeys and the number
   of queued trie-operation batches not yet applied (`orderbook.history.pairs`,
   `orderbook.history.deltas`, `orderbook.pubkeys`, `orderbook.trie_ops.pending`),
   on every sweep; and the size in bytes and entry count of the node store
   (`orderbook.memory_db`, `orderbook.memory_db.entries`), right after each
   purge only, because measuring the store walks all of it while the store is
   locked.

No item changes a wire shape. The tests are those of the implementing change
(in-process counts and capacities after a burst, the timer sweep, the clear and
the shared-subtree case, the gauges).

## 32.9A Bounded and Unbounded State

*(Authored by the KDF Spec Reader 2026-10-28, second pass, from the rows
`v2.6.0-beta` and `v3.0.0-beta` of [Chapter 02](02-baseline-state.md)
§2.15A, identical at both, and from the public gossipsub library both
rows and Reloaded pin; prompted by the memory growth of public seed
nodes under a Reloaded maker. **Passed the KDF Dirty Gate on 2026-10-28**,
measure 0.02; §32.13.)*

**R-B1 (reference bound inventory).** The following table binds, as
facts of the reference that every peer on the mesh relies on, which
state of this subsystem and of the substrate beneath it
([Chapter 28](28-libp2p-modernization.md)) grows with traffic from
other peers, and how each is bounded. "Receipt time" is the receiver's
own clock (§32.4 R-P3).

| # | State | Grows with | Created by | Removed or expired by | Bound |
| --- | --- | --- | --- | --- | --- |
| 1 | per-pubkey record (§32.4 R-P1) | pubkeys heard | first insertion for the pubkey or first keep-alive handled (R-Q5a item 1) | the sweep of §32.9 R-K3: receipt time older than the liveness window (90 s default), own pubkeys exempt | pubkeys heard within the window; **a pubkey whose keep-alives keep arriving never expires**, whatever its roots hold |
| 2 | per-pair held-root entry | pairs per pubkey | insertion, removal, placeholder (R-Q4 item 3) | snapshot application (the clear), record eviction; not by the last removal | pubkeys × advertised pairs |
| 3 | per-(pubkey, pair) delta history (§32.4 R-P5) | order changes | an insertion or removal whose predecessor root is non-zero | each delta 5 min after it was recorded; the pair's history 4 h after its last change; both swept on insertion and ignored once expired; removed by a clear | changes within the last 5 min per slice, each holding a full order record |
| 4 | cancellation guard (§32.3 R-C5, §32.9 R-K1) | cancel messages | every cancel message, known order or not | 120 s, swept on insertion | cancels within the last 120 s |
| 5 | read index (§32.3 R-C1) | live orders | insertion | removal, eviction | live orders (the reverse pair indices of R-C3 never shrink) |
| 6 | trie node store | every trie mutation | insertion, snapshot | the ten-minute purge releases nodes nothing references; **a cleared slice's nodes keep their references for the life of the process** | **unbounded**, monotone in the number of snapshot applications times the previous trie's size |
| 7 | mutation-event queue (§32.6 R-T7) | inbound order messages and answers | every index mutation | drained by the single worker under the store lock | **unbounded** |
| 8 | tasks handling inbound gossip messages | message rate | one task per delivered message | end of handling; a keep-alive's task awaits one request of up to 10 s | **no cap** on tasks in flight |
| 9 | outbound synchronization requests (R-Q6) | keep-alive copies with a divergent root | one per copy, no de-duplication, no rate limit | answer, error or the 10 s timeout | copies within the last 10 s |
| 10 | pending-request table | row 9 | request sent | answer or error; a sweep every few seconds against the 10 s timeout | requests within the last 10 s |
| 11 | channels between application and substrate | commands, events | — | capacity 8192 each; an item that finds its channel full is **dropped** with a diagnostic (a dropped gossip event is a keep-alive this node never handles, though a relay has already forwarded it, R-F3a) | 8192 |
| 12 | substrate message cache | messages accepted | every accepted message | shifted every heartbeat (1 s); kept five heartbeats, announced for three | 5 s of traffic |
| 13 | substrate duplicate cache | messages accepted | every accepted message id | 60 s, swept on insertion | 60 s of traffic |
| 14 | substrate IHAVE/IWANT bookkeeping (ch. 28 R13a) | announcements | per heartbeat | cleared every heartbeat | at most 10 IHAVE and 5000 requested ids per peer per heartbeat |
| 15 | per-message retransmission counts (inside row 12) | IWANT received | IWANT for a cached message | dropped with the message | at most 3 per message per peer |
| 16 | substrate peer-topic maps, mesh, fanout, backoffs | connected peers × topics | subscription announcements | disconnect, unsubscribe, backoff expiry | peers × topics |
| 17 | **per-peer outbound queue of the substrate's connection handler** (ch. 28 R11a, D7) | every forward, publish, IWANT answer or control message to that peer | push on send; each forward is serialized once and cloned per recipient; a relay sends every message twice (R-F3a, R-F3b) | popped only when that peer's outbound stream is writable; freed by disconnection | **unbounded**; the only row whose growth rate a *remote* peer sets, by reading slowly |
| 18 | connected relays, relay mesh (ch. 28 R10, R11) | relays | connection with relay advertisement | the 30 s maintenance trims connected relays above twice the high watermark and keeps the mesh within the watermarks | at most 24 relays on a relay node; mesh at most 12 |
| 19 | peer-exchange known peers (ch. 28 R8) | peers learned | exchange rounds | capped | 100 |

A relay subscribes to no orderbook topic of its own, so rows 12 to 15
hold for it only through forwarding, and it neither announces nor
requests message ids for those topics (ch. 28 R13a).

**R-B2 (Reloaded is at least as bounded).** For every row of R-B1 that
names a bound, Reloaded MUST keep a bound no weaker; it MAY be tighter.
For rows 6, 7, 8 and 17 Reloaded today shares the reference's lack of a
bound (**parity**); the Reloaded-original candidates that would add one
are recorded under §32.10 D6 and ch. 28 D7 and are not bound here.
Adopting any of them is a divergence to record in the operator-facing
compatibility documents; none changes a wire shape.
Since R-K4 (item 4) the cleared-slice part of row 6 is no longer
parity: Reloaded releases the nodes of a cleared slice, and row 3 and
the nodes of replaced leaves are released on a timer (R-K4 items 1 and
3).

**R-B3 (two growth modes an operator can tell apart).** Rows 6 and 7
grow with what *this* node applies: every synchronization answer in the
reference's advertised-root form is a full snapshot (R-Q4a note), so a
maker whose roots change at every advertisement makes every relay that
hears it orphan one trie per pair per advertisement interval — squared
with the number of pairs under the aggregated shape of the R-K2a
divergence note. Row 17 grows with what a *remote* peer fails to read,
at that peer's share of the forwarded traffic, and is released when the
peer disconnects (the ping disconnect of ch. 28 R9 fires only when the
peer stops answering pings altogether). Nothing else releases either.

**T-B1** *Orphaned nodes (R-B1 row 6).* On one node, apply N successive
full snapshots for one (pubkey, pair), each listing the same two orders;
the node store grows by about N times the size of one two-leaf trie and
does not shrink after the ten-minute purge. Applying N successive deltas
instead grows it by at most the size of the live trie plus the nodes the
next purge releases. *(Reloaded, since R-K4 item 4: after the purge the
node store is back to the live trie in both cases.)*

**T-B2** *Slow peer (R-B1 row 17).* A relay forwarding a steady message
stream to a peer whose inbound is throttled accumulates queued outbound
data for that peer in proportion to the shortfall; disconnecting the
peer releases it; a second, unthrottled peer on the same relay is
unaffected.

**T-B3** *Record lifetime (R-B1 row 1).* A pubkey that sends only
keep-alives advertising a root the node cannot synchronize (every
request fails) keeps its record and placeholders for as long as the
keep-alives arrive, and loses them 90 s after they stop.

## 32.9B Maker Publication Rate (Reloaded-original)

*Reloaded-original (maintainer decision AO, option C, 2026-10-28); no forbidden-class
input. The plan behind it is `docs/plans/maker-gossip-rate-control.md`.*

**R-MP1 (maker outbox).** The order messages a node publishes for its own
maker orders go through one *outbox* per maker pubkey: the persistent one
and each per-order one. These are the maker-order created, updated and
cancelled messages, including the re-announcement of an own order missing
from the index ([Chapter 59](59-legacy-ordermatch-negotiation.md) R59.6.7).

1. **One pending message per order.** The outbox holds at most one pending
   message per order uuid. A new message for an order that already has one
   pending merges with it:

   | Pending | New | Result |
   |---|---|---|
   | none | any | the new message |
   | created | updated | created, carrying the updated values |
   | updated | updated | updated, field by field: each field takes its latest value set in either message |
   | created or updated | cancelled | cancelled; the pending message is discarded |
   | cancelled | any | cancelled; a cancellation is terminal and never merged away |

2. **Not held back:**
   - A cancelled message is published at once and is not counted against
     the budget of R-MP2.
   - The swap-negotiation messages (taker request, maker reserved, taker
     connect, maker connected) do not go through the outbox at all.
   - Both are published with **priority** under
     [Chapter 28](28-libp2p-modernization.md) §28.9B RQ1 item 9, both when
     the node publishes them and when it lets a received one propagate.
   - No other ordermatch message is priority. Keep-alives in particular
     are not (R-MP3 item 3).
   - A cancellation that overtakes an earlier message for the same order
     is safe: the receiver's cancellation guard (§32.3 R-C5, §32.9 R-K1)
     suppresses a creation that arrives after it, and an update for an
     order the receiver no longer holds is ignored.
3. **Local state is not delayed.** The maker's own order record, its trie
   ([§32.6](#326-patricia-trie-substrate-and-delta-sync)) and the result
   of the request that changed the order are updated at once, as without
   the outbox. Only the gossip message waits.
4. **Order of release.** Pending messages are released oldest first,
   counted from when the order's slot was first filled, so that no order
   waits behind newer ones.
5. **Switch.** The top-level `MM2.json` boolean `maker_update_coalescing`
   (default `true`) selects this rule. Only a JSON `false` turns it off,
   and every message is then published at once, as without the outbox.

**R-MP2 (release budget).** The outbox releases created and updated
messages at no more than the pubkey's current *rate*.

1. **Ceiling.** The rate never exceeds `maker_update_rate_max` messages
   per second per maker pubkey (top-level `MM2.json` number, default 10),
   with a burst of 10 × that value (default 100).
2. **Signals** *(amended 2026-10-28, decision AO direction (a)).* Two
   signals, each judged for every connection to a relay, tell the node
   that a relay is not keeping up:
   - *Queue signal.* It is raised while the data queue of
     [Chapter 28](28-libp2p-modernization.md) §28.9B RQ1 toward the relay
     has held at least one data RPC for 2 seconds without becoming empty,
     or while the pressure level of RQ1 item 8 is raised. It clears once
     that queue has been empty for 5 seconds and the level of item 8 is
     cleared. It catches a relay that reads slowly while the connection
     stays up: the backlog then sits in that queue.
   - *Close signal.* It occurs when the node's last connection to a relay
     closes while an outbox holds a pending created or updated message, or
     has released one within the preceding 30 seconds. It catches a relay
     that stops reading: the backlog then sits below the queue, in the
     multiplexer and the transport, where the queue cannot see it, until
     the ping safeguard closes the connection. A close with no recent
     maker traffic is not a signal.

   The node is *under pressure* while the queue signal is raised for any
   relay connection.
3. **Adaptation** *(amended 2026-10-28).*
   - On a signal outside degraded mode -- a close signal, or the queue
     signal becoming raised -- the rate in effect is recorded as the
     *trip rate*. The rate then drops at once to the floor of 0.5
     messages per second, and the outbox enters degraded mode (item 4).
   - A signal while the outbox is degraded leaves the trip rate as it is.
   - The outbox leaves degraded mode once 5 minutes have passed since the
     last signal. A queue signal that stays raised counts as a signal at
     every moment it is raised.
   - It then releases at the floor, and the rate rises by 0.5 messages per
     second every 60 seconds. The climb stops one step below the trip
     rate, and never below the floor, so a rate at which a relay has
     already failed is not tried again.
   - After 30 minutes without a signal the trip rate is forgotten, and the
     climb continues up to the ceiling (maintainer decision 2026-10-28,
     D7).
   - The signals are node-wide and apply to every outbox; each pubkey
     keeps its own rate and trip rate.
4. **Degraded mode.** While degraded, the outbox stops releasing created
   and updated messages, and a keep-alive does not wait for its pair's
   messages (R-MP3 item 2). Peers then learn the changes through the
   keep-alive synchronization of §32.7.3. Cancellations are still released
   at once. *Accepted cost:* a keep-alive then advertises a root that its
   receivers cannot reach from the messages they hold, so each receiver
   sends one synchronization request per pair per keep-alive: at most one
   per 30 seconds per maker pubkey and pair.
5. **Logging.** Each signal with its kind, entering and leaving degraded
   mode, and each change of the rate are logged at most once per minute
   per pubkey, with the rate. The line that enters degraded mode is
   logged at once and is not held back by this limit *(added
   2026-10-29)*; it counts as the minute's line for the others. Order
   content is never logged.

**R-MP3 (keep-alive ordering)** *(amended 2026-10-28)*. The keep-alive
of R-K2/R-K2a is not counted against the budget, and it follows its
pairs' changes where the budget allows.

1. **Not budgeted.** A keep-alive is never counted against the budget,
   and it waits no longer than item 2 allows.
2. **Bounded wait.** On a tick, before the keep-alives of a maker pubkey
   are published, the node waits until every pending outbox message for
   an order on any of their pairs has been released within the budget,
   for at most 10 seconds in total. In degraded mode it does not wait.
   The keep-alives then leave, also while messages for their pairs are
   still pending; those stay in the outbox and are released later, in
   their order. The wait never releases a message beyond the budget.
3. **No overtaking in the send queue.** A keep-alive is never queued
   ahead of an order message already released, in any send-queue lane.
   It may leave while messages it accounts for are still pending in the
   outbox (item 2). A receiver then requests a synchronization, the cost
   that R-MP2 item 4 accepts.

**R-MP4 (keep-alive after reconnect).** When a connection to a relay is
established and that relay has joined the relay mesh (§28.6 of
[Chapter 28](28-libp2p-modernization.md)), the node publishes, within 5
seconds, one keep-alive per pair per maker pubkey as on a tick, after
the bounded wait of R-MP3 item 2 *(amended 2026-10-28)*. This round is in
addition to the regular ticks and does not move them.

**R-MP5 (metrics).** The metrics the node reports carry these aggregate
values, with no peer identity and no content:
- `p2p.send_queue.len`: queued data RPCs, summed over connections;
- `p2p.send_queue.max_per_peer`: the largest single connection's count;
- `p2p.send_queue.dropped.count`: data RPCs dropped under §28.9B RQ1
  item 4 since start;
- `p2p.outbox.pending`: pending outbox messages, summed over pubkeys;
- `p2p.outbox.rate`: the current rate of R-MP2, the lowest over pubkeys;
- `p2p.outbox.pressure`: 1 while the queue signal of R-MP2 item 2 is
  raised, else 0;
- `p2p.outbox.degraded`: 1 while any outbox is in degraded mode, else 0
  *(added 2026-10-28)*;
- `p2p.outbox.signals.queue.count` and `p2p.outbox.signals.close.count`:
  the signals of each kind since start *(added 2026-10-28)*.

*Rationale.* A relay can drain anything from more than 190 to fewer
than 0.2 messages per second, depending on its host's load. Toward a
slow reader, a sender cannot move a message ahead of those it has
already sent.
- An in-process measurement on 2026-10-28 published 90 updates per
  second toward a reader taking about one per second. It delivered 0 of
  10 keep-alives, both with a newest-first queue and with the capped queue
  of §28.9B.
- Every node drops a maker 90 seconds after its last keep-alive (R59.6.4).
  So a maker that publishes faster than its relay drains loses its whole
  book on every node.

The outbox makes the rate a choice of the maker rather than of the
bot driving it. The budget follows the relay's actual capacity, and the
keep-alive is never queued behind messages that need not exist.

R-MP3 item 2 keeps the advertised root reachable where the budget
allows. A keep-alive that overtook its own pair's updates would advertise
a root no receiver can reach yet, and every receiver would then request a
synchronization. The wait is bounded because the keep-alive matters more:
a late synchronization costs one request, a missing keep-alive costs the
maker's whole book.

*Amendment (2026-10-28).* The first in-process run of T-MP8 under the
rules as first written delivered 0 of 30 keep-alives during the load,
for three reasons:
- toward a relay that stops reading, the backlog sits below the send
  queue: the multiplexer grants each stream a 256 KiB window and the
  transport buffers more, so the queue saw nothing until about 1,050 to
  4,500 messages were in flight;
- the unbudgeted flush before each keep-alive and the reconnect round
  released whole backlogs at once;
- the ping safeguard's close discarded what was in flight.

Measured and not adopted: a ping round-trip signal (the round trip jumps
from milliseconds straight to timeouts, so it gives no earlier warning),
holding the ping close while degraded (it never engaged), and halving the
rate on each close with a slow ceiling growth (more gaps over 90 s). The
bounded wait with the close signal delivered, after the first close, 81
keep-alives with a median delay of 1.3 s, a maximum of 19.5 s and no gap
over 41 s toward a relay reading 1 message per second. At 0.3 messages
per second the median was 3.6 s, the maximum 36.8 s and the longest gap
68 s. A lab run against a relay whose inbound bandwidth was limited showed
the other stall mode: pings passed, the connection never closed, the
backlog grew in the send queue and the maker's book was dropped. The
close signal never occurs there; the queue signal does. One residual
stays: the first signal comes only after the load starts, about 50
seconds later in process, so a burst of order changes can still cost one
expiry of the maker's orders per new relay connection.

R-MP4 replaces what a closing connection loses. Data still in the
multiplexer's window when a connection closes is lost; §28.9B RQ1 item 5
and the ping safeguard can both close a connection. The round restarts
each relay's 90-second window and starts its synchronization, without
waiting up to 30 seconds for the next tick.

> **Reloaded divergence (informative).** The reference publishes every
> maker-order message at once. Under R-MP1 and R-MP2 a peer may receive an
> order's update up to about 1/rate seconds later, or, in degraded mode,
> only through the next keep-alive synchronization. It may never see an
> intermediate value of an order that changes quickly. Cancellations are
> not delayed, and may arrive ahead of messages published before it (the
> priority lane of §28.9B RQ1 item 9). In degraded mode, and when the
> bounded wait of R-MP3 item 2 runs out, a keep-alive may advertise a root
> that its receivers cannot yet reach; each receiver then sends one
> synchronization request per pair. R-MP4 publishes keep-alives beyond
> the ticks of R-K2a. Every
> keep-alive has the shape R-K2a binds, and a receiver processes each one
> on its own. No message format changes.
>
> Under `docs/COMPAT_SWITCHES.md`, "The default-side rule", this is arm 3.
> The project team decided that Reloaded's variant is the default (decision
> AO, 2026-10-28). **Compatibility:** to publish every order message at
> once, set `maker_update_coalescing` to `false`. R-MP3 and R-MP4 are not
> switched: they only order and add keep-alives of the bound shape.
>
> **Provenance.** Reloaded-original. Inputs: our own code, the in-process
> measurements of 2026-10-28 (`docs/plans/gossip-send-queue-bound.md`), a
> lab run of our maker against reference nodes run as black boxes, and the
> gated facts already in §32.9A and R59.6.4 (the 90-second liveness window).

**Tests (R-MP1–R-MP5).**

- **T-MP1** *Merging.* Each row of the R-MP1 item 1 table produces the
  stated pending message.
- **T-MP2** *Cancellation.* A cancelled message is never merged away, is
  released at once when the budget is exhausted, and leaves the
  node in degraded mode too.
- **T-MP3** *Swap messages bypass.* The four swap-negotiation messages are
  published at once, with the outbox full and the budget exhausted.
- **T-MP4** *Budget (amended 2026-10-28).* With an injected clock:
  - the ceiling and the burst are respected;
  - a close signal drops the rate to 0.5/s and enters degraded mode, and so
    does the queue signal becoming raised;
  - a relay close with no pending message and no release in the preceding
    30 s is not a signal;
  - the outbox leaves degraded mode 5 min after the last signal, not
    earlier while the queue signal stays raised;
  - the rate then rises by 0.5/s every 60 s and stops one step below the
    trip rate; with a trip rate of 3/s it stops at 2.5/s;
  - a close signal while degraded does not change the trip rate;
  - after 30 min without a signal it climbs on to the ceiling;
  - in degraded mode no created or updated message is released;
  - a signal 10 s after an earlier log line is logged at once, at warn
    level and with its kind *(added 2026-10-29)*.
- **T-MP5** *Signals (amended 2026-10-28).* §28.9B T-RQ1.8, and:
  - a data queue that stays non-empty for 2 s raises the queue signal,
    and one that empties within 2 s does not;
  - the signal clears after 5 s of an empty queue;
  - the close of the last connection to a relay with a pending message
    yields one close signal; the close of a connection to a non-relay
    peer yields none.
- **T-MP6** *Keep-alive ordering (amended 2026-10-28).*
  - With a budget that releases them within 10 s, a pair's pending
    messages are published before its keep-alive, and a receiver that
    applies them in order holds the advertised root and sends no
    synchronization request.
  - With more pending than the budget releases in 10 s, the keep-alives
    leave 10 s after the tick, give or take 1 s, and no message leaves
    beyond the budget.
  - In degraded mode the keep-alives leave at the tick.
- **T-MP7** *Reconnect round.* After a forced close and a reconnect, one
  keep-alive per pair leaves within 5 s of the relay rejoining the mesh,
  plus the bounded wait.
- **T-MP8** *Slow relay, in process (amended 2026-10-28).* A maker with
  600 orders changes them at 90 updates per second for at least 600 s,
  once toward a relay that reads about 1 message per second and once
  toward one that reads about 0.3.
  - After the first signal, the median delay from a tick to the arrival
    of its keep-alive is at most 10 s, and no keep-alive arrives later
    than 60 s after its tick. A keep-alive lost with a connection that
    closes never arrives; it is reported, and the gap criterion below
    bounds it.
  - After the first signal, no gap between delivered keep-alives exceeds
    90 s. Before it, one such gap is allowed (the residual of the
    rationale).
  - After the load stops, the relay's order set for the maker converges
    to the maker's within 120 s.
- **T-MP9** *Healthy relay.* Below the ceiling, every update is published
  as soon as it is made. A relay that keeps up receives every update, and
  no merging is visible.
- **T-MP10** *Metrics.* The R-MP5 values are present and reflect a
  queued backlog, drops, pending outbox messages, the rate and the
  pressure state.
- **T-MP11** *Switch.* With `maker_update_coalescing` set to `false`,
  every order message is published at once.
- **T-MP12** *Priority marking.*
  - A cancellation and each swap-negotiation message are published with
    priority, and so is one propagated by a relay.
  - A created, updated or keep-alive message is not.
  - Toward a paused relay with a full data queue, a cancellation arrives
    first after resume.
- **T-MP13** *Bandwidth-limited relay, lab (added 2026-10-28).* Not run
  in CI. A maker with the T-MP8 load runs against a relay whose inbound
  bandwidth is limited to 100 kbit/s, over a real network link, for at
  least 25 minutes:
  - the queue signal is raised, and the maker enters degraded mode;
  - the maker's memory stays bounded;
  - after the first 2 minutes, no node drops the maker's orders for a
    missing keep-alive.

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

D5. **The synchronization answer under a lagging worker (R-T3b item 2;
    added 2026-10-28; RESOLVED the same day: the maintainer adopted the
    alternative below, now in force as amended R-T3b item 2, with the
    full-snapshot rule for orders whose removal is still queued).** The open-decision note under
    §32.6 R-T3b records that the reference never refuses a
    synchronization request, while R-T3b item 2 makes Reloaded's
    responder return a retryable error when its trie worker lags,
    and that a refusal stops the asking relay's forwarding for an
    advertisement interval (§32.8 R-F3a — *as corrected in the second
    pass of 2026-10-28, a relay's gossip-layer forward has already
    happened; a refusal withholds only the application-level forward of
    R-F3b and leaves the asking peers at the placeholder of R-Q5a*). The
    alternative wording,
    as prepared for the maintainer's decision (kept for the record):

    > *R-T3b item 2 (alternative).* A keep-alive advertisement MUST NOT
    > reflect trie state older than the last completed read-index
    > operation of this node (the producer waits for the worker for a
    > short bounded time and otherwise skips the round, as today). The
    > answer to a pubkey-state synchronization request MUST be built
    > from the trie store as it stands when the request is handled,
    > without waiting and without refusing; a root the answer reports
    > that predates a completed index operation is tolerated, because
    > the requester's next keep-alive comparison (§32.7.3 R-Q4) corrects
    > it one interval later, exactly as it would after any answer that
    > the maker has since moved past. The retryable-error outcome is
    > withdrawn; T-T3's second expectation changes to "answered from
    > the trie state the store holds at that moment".

    Trade-off to decide: the alternative restores the reference's
    forwarding behavior under load and removes the cascade, at the price
    of re-opening, for the sync answer only, the few-millisecond window
    the measurement under R-T3b describes (a cancelled order may be
    listed in an answer built from the older root). Keeping item 2 keeps
    the window closed and keeps the cascade. A third option — answer
    from the current state but mark the answer so that the requester
    asks again sooner — would change the dictated response shape and is
    not available.

D6. **Reference unboundedness, and Reloaded-original hardening
    candidates (informative; added 2026-10-28).** *(Facts established
    by the KDF Spec Reader at the rows `v2.6.0-beta` and `v3.0.0-beta`
    of [Chapter 02](02-baseline-state.md) §2.15A, identical at both;
    stated as behavior, not as code. Passed the KDF Dirty Gate on
    2026-10-28; see §32.13.)* The reference's synchronization path has these
    **unbounded** resources: the trie-mutation queue between the inbound
    handlers and the trie worker has no capacity limit (the reference's
    own commentary acknowledges that rate limiting of orderbook
    messages is missing and that a bound would belong with it); trie
    nodes of a `(pubkey, pair)` slice that is cleared as a whole are
    never released — the clear drops the root and the history but does
    not dereference the nodes, and the periodic ten-minute store purge
    releases only unreferenced nodes, so they stay for the life of the
    process; every inbound gossip message is handled on its own spawned
    task with no cap on the number in flight; and the full-trie answer
    of §32.7.3 R-Q5 is built synchronously on the inbound networking
    event loop under both locks. **Bounded** are: outbound pending
    requests (10 s timeout, swept every few seconds), tracked pubkey
    state (the 90 s default window of
    [Chapter 59](59-legacy-ordermatch-negotiation.md) R59.6.4), the
    delta history (a pair's history expires after 4 hours without a
    change, each delta after 5 minutes), the two channels between the
    application and the libp2p swarm (8192 entries each; an event that
    finds the channel full is dropped), and the gossip library's
    message cache (library defaults). The inventory of §32.9A R-B1
    (second pass of 2026-10-28, passed the KDF Dirty Gate) restates these with
    their values and adds the one this list omitted: the substrate's
    per-peer outbound queue has no bound either (R-B1 row 17,
    [Chapter 28](28-libp2p-modernization.md) D7). Reloaded inherits the
    same profile (R-T3b's measurement already notes the unbounded
    queue).
    Candidates, all Reloaded-original and none bound here:

    - a **bounded or coalescing mutation queue** — Reloaded already has
      R-T3b item 1's queue ordering, which any bound must preserve; a
      coalescing design merges consecutive events for one
      `(pubkey, pair)` before they reach the worker and sheds load by
      dropping a full-snapshot batch superseded by a later one for the
      same slice, never by dropping single events out of order;
    - **in-flight synchronization de-duplication** — at most one
      outstanding request per `(pubkey, pair)` (or per pubkey), with a
      keep-alive that arrives while one is pending recorded as handled
      without a second request (its forwarding decision then follows
      the pending request's outcome, or is made as "no request needed"
      — to be decided together with R-F3b);
    - **orphan release** — dereferencing a cleared slice's nodes so the
      periodic purge reclaims them, or rebuilding the node store from
      the live roots at purge time (adopted: R-K4 item 4);
    - a **cap on concurrently handled inbound gossip messages** per
      peer or per topic, applied before the per-message task is spawned.

    Each is a divergence candidate to be recorded under the
    operator-facing compatibility documents when adopted, and none
    changes a wire shape.

D7. **Recovery of the maker publication rate (§32.9B R-MP2 item 3;
    resolved 2026-10-28, maintainer decision; added 2026-10-28).** The
    rise stops one step below the trip rate, so that a rate at which a
    relay already failed is not tried again. The rise was agreed without
    its end: without one, a maker whose relay recovers would stay at the
    lowered rate until restart. The alternatives were to forget the trip
    rate after 30 minutes without a signal, or a slow growth of the
    limit instead (+0.5 messages per second every 5 minutes, as in the
    measured candidate that halved the rate on each close). The
    maintainer chose the first, as R-MP2 item 3 states it: its worst
    case is one more trip per 30 minutes against a relay that is still
    slow. Still open: whether each new relay connection
    should start at a low rate, which would close the residual gap of
    the first signal. That option was not chosen on 2026-10-28; the
    lab's T-MP13 runs should show whether the residual matters.

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

V5. **Relay forward ordering at the baseline.** Confirm against the
    baseline anchor that its in-tree gossip substrate forwards a
    received message, when manual propagation is configured, only on
    the application's report; a relay's immediate forward on receipt
    (§32.8 R-F3a) and the per-peer announcement caps of
    [Chapter 28](28-libp2p-modernization.md) R13a are post-baseline,
    present at both reference rows through the pinned libp2p fork.

## 32.13 Provenance Footer

- *Inputs:* the project's own revision history (used for the
  epoch classification of §32.1, by component role and first-
  introduction epoch only — no code transcribed); the baseline
  anchor of [Chapter 02](02-baseline-state.md); the public
  `sp_trie` Patricia-Merkle trie library, the BLAKE2b hash
  specification, the MessagePack serialisation format, and the
  libp2p gossip/signed-envelope model — the external public
  specifications the wire and hashing contracts derive from;
  cross-chapter contracts (Chapters 11, 13, 16, 28); and, added
  in this revision, prompted by a reproduced order-visibility
  failure — the peer-address-derivation outcome mapping and the
  per-order response-assembly drop behaviour bound by R-F6b/R-F6c.
- *Permitted-input classes used:* baseline source (epoch
  classification only); external public specifications (`sp_trie`
  trie layout, BLAKE2b, MessagePack, libp2p gossipsub);
  cross-chapter contracts (Chapters 11, 13, 16, 20, 28, 41);
  Interop / wire-and-algorithm-bound reuse (R29/R31) for the
  dictated fragments embedded in §32.5.1, §32.6.1, and §32.7.1 —
  namely the eight-field wire order record and its MessagePack
  encoding, the proof-carrying wire item and side-channel record
  shapes, the inbound request variant set and the sync-response
  shape, and the BLAKE2b-64 / `sp_trie` version-0 trie hash
  parameters and empty-trie root — whose authoritative source is
  the bytes any conforming peer must exchange for cross-peer
  interoperability and trie-root convergence, not the historical
  lineage's discretionary expression; and, for R-F6b/R-F6c, the
  public RPC response-shape fact that a shielded outcome is a
  success case that keeps the order in the response, and that a
  drop-only-that-order (not abort-the-response) behaviour is the
  existing contract for a coin family with no address-derivation
  path at all — both are RPC response-shape facts within the
  compatibility surface, not internal expression.
- *Sibling-allowlist consultations:* Chapter 28 (the libp2p
  ingestion and signed-envelope decoder feeding the gossip entry
  point); Chapter 13 (the swap version-negotiation path
  consuming the confirmation-settings reconciliation and the
  side-channel maps); Chapters 11 and 16 (the cancellation-TTL
  binding and the per-order ephemeral-pubkey mechanism); and,
  added in this revision, Chapter 20 (Siacoin's ed25519 address
  key being cryptographically unrelated to the order's P2P
  identity pubkey, which R-F6b relies on) and Chapter 41
  (Lightning's deliberate non-disclosure of its payment identity,
  which R-F6b also relies on).
- *Forbidden corpus:* consulted **only** to recover the
  externally-dictated wire-and-algorithm fragments of §32.5.1,
  §32.6.1, and §32.7.1, embedded as Interop reuse under R29/R31
  (only the field sets, encodings, request/response shapes, and
  hash parameters required for interoperability are reproduced;
  no upstream function bodies, private identifiers, helper
  decomposition, control-flow transcription, per-method tables
  keyed to internal names, or diagnostic / panic / log string
  literals accompany them); and, in this revision, to recover
  which coin families the shielded-success outcome of R-F6b
  currently covers and to confirm the drop-only-that-order
  behaviour of R-F6c against both the `v2.6.0-beta` and current
  v3-lineage references, which agree. Only the outcome mapping
  (which coin protocol family reaches which of the three
  documented outcomes, and why) crosses into this chapter; no
  function name, argument list, match-arm structure, or comment
  text was reproduced. All other content is clean-room
  driving-spec stated by behaviour and public/dictated interface.
  Any residual similarity of a conformant realisation to the
  historical lineage — of either epoch — is governed by the R35
  gate and the R36 binding-scope notes that head every code-
  bearing section of this chapter.

  A **later pass** added two paragraphs — the keep-alive-timestamp
  clarification under §32.4 R-P3 and the liveness-window
  cross-reference under §32.9 R-K3 — whose substance is bound by
  [Chapter 59](59-legacy-ordermatch-negotiation.md) and whose
  forbidden-class scope is recorded there (ch. 59's own footer).
  What crosses into this chapter from that pass is only: that the
  reference likewise records local receipt time and makes no use
  of the sender's timestamp, and that the liveness window, its
  operator override and the own-pubkey exemption live with the
  negotiation chapter. Established under the public release
  designations `v2.6.0-beta` and `v3.0.0-beta`.
- *Dirty Gate clearance (chapter 01 T1, R40, R43) for the later
  pass:* the two paragraphs named immediately above — the
  keep-alive-timestamp clarification under §32.4 R-P3 and the
  liveness-window cross-reference under §32.9 R-K3 — together
  with the derivation record above and this item, were authored
  by the KDF Spec Reader under the two-team procedure of
  chapter 01 §1.4A and **passed the KDF Dirty Gate** on
  2026-10-24: PASS, discretionary-expression measure 0.00,
  finding list empty, and accordingly no remedy class for any
  line or for the candidate as a whole. That was the first gate
  pass on this material; no earlier verdict for it exists or was
  relayed. **Only the added material was gated:** the rest of
  this chapter was not re-examined in that pass, and its earlier
  clearance position — including the R35-gate and R36
  binding-scope position of the item above — is untouched by
  that verdict. The clearance for the later pass is therefore
  **complete and not pending**, and R40 withholds nothing here.

  *Why no section carries an inline pointer to this item.* The
  companion [Chapter 59](59-legacy-ordermatch-negotiation.md)
  puts a short clearance pointer at the head of each of its four
  consulted sections; this chapter records the clearance here
  only. That is a scope judgement, written down so that it
  cannot be read as an oversight, and three things fix it.
  First, the at-the-point duty of R40 attaches to a *pending*
  clearance; completing the record discharges that duty rather
  than relocating it, and the pending statement these two
  paragraphs previously relied on is what this item replaces.
  Second, what the duty guards against is a reader or an
  implementer mistaking a **candidate** for a cleared binding
  requirement, and neither paragraph can be mistaken that way
  now. Each is cleared, so reading it as cleared is accurate;
  and neither is the binding authority for what it states. The
  §32.9 paragraph says on its face that the window, the operator
  override and the own-pubkey exemption are bound by
  [Chapter 59](59-legacy-ordermatch-negotiation.md) R59.6.4 and
  R59.6.5, and the §32.4 paragraph's requirement on the
  sender-supplied member restates that chapter's R59.6.2 and
  cross-references it, so a reader who follows either paragraph
  to the rule that binds it arrives in a section that does carry
  a pointer. Third, this chapter's other sections rest on the
  inputs the item above records and were not part of this
  candidate; a fresh clearance notice inserted at two mid-chapter
  points of an otherwise unchanged chapter would imply a
  distinction in clearance position among its sections that this
  verdict did not draw. Two short cross-referencing paragraphs
  in an otherwise already-cleared chapter are the lighter case,
  and one record scoped to them is the proportionate answer to
  it.

  The three elements of the R39 scope attestation for that pass,
  carried forward here in corpus-free terms as chapter 01 T1
  requires:

  - *Extent.* The forbidden-class material corresponding to the
    subject matter the two added paragraphs address: the
    keep-alive processing path and the periodic tracked-pubkey
    eviction behaviour of this subsystem's peer-to-peer surface,
    in the narrow respects those paragraphs claim — the basis on
    which liveness is recorded, the treatment of the
    sender-supplied wire member, and the ownership of the
    liveness window, its operator override and the own-pubkey
    exemption. Quantity examined: **one file**, read in part.
    Reference designations: the publicly published release
    designations `v2.6.0-beta` and `v3.0.0-beta`. **The record
    is no more precise than those designations** — no
    post-anchor commit identifier, no branch state and no
    location within the compared material is given (chapter 01
    R11 and the reference-version limit of §1.7).

  - *Method.* By **reading**, on both sides. **No mechanical
    comparison was run:** the added material embeds no code
    block, so this repository's own residual-similarity gate
    tooling had no operand. The absence is stated so that it is
    visible rather than implied.

  - *Measure.* **0.00** on the discretionary-expression basis —
    the same figure the leak report carries, not a second one —
    read against the same judgement standard as the
    [Chapter 59](59-legacy-ordermatch-negotiation.md) pass of
    the same date: one identified item of genuine discretionary
    expression fails a candidate however much of the surrounding
    content is legitimately dictated. The figure reflects what
    the added material consists of: a negative behavioural claim
    about one dictated wire member, a cross-reference handing two
    bound values and one exemption to
    [Chapter 59](59-legacy-ordermatch-negotiation.md), a
    statement of this repository's own implementation state, and
    the two footer records. It carries no identifier outside
    R11's carve-out classes and no discretionary expression of
    any category — no private identifier, no function body, no
    control-flow transcription, no local name, no error or log
    literal, no per-method table keyed to internal names, no
    internal module tree and no test identifier.
- *Scoped addition (2026-10-26) -- legacy-generation orderbook
  surfaces (R-F6d, R-F6e, T-F6.1--T-F6.4 and the two notes beside
  them in §32.8):* prompted by a reproduced failure of the legacy
  `orderbook` method for a pair carrying a shielded-coin order.
  *Inputs:* this repository's own two orderbook RPC generations,
  read side by side; and the forbidden corpus, consulted at the
  rows `v2.6.0-beta`, `v3.0.0-beta` and `dev (as of 2026-03-27)`
  of the reference-state table of [Chapter 02](02-baseline-state.md)
  §2.15A, **only** to establish, for each of the four methods (two
  generations of `orderbook` and of `best_orders`), which coin
  families reach which R-F6 outcome and whether a per-order
  derivation failure aborts the response or omits the order. What
  crosses into this chapter is that outcome mapping and the
  abort-versus-omit fact per method, the public response shapes
  (`address` as a string on the legacy generation, the
  `address_type`/`address_data` object on mmrpc 2.0), and the
  observation that the three rows agree except for the TRON
  family, which the legacy resolver refuses at `v2.6.0-beta` only
  and derives at the two later rows; no function name,
  argument list, match-arm structure, error wording or comment
  text was reproduced. The legacy placeholder string of R-F6e has
  no reference counterpart and is a Reloaded decision.
- *Dirty Gate clearance for the 2026-10-26 scoped addition:*
  **PASS** (2026-10-26). The material named in the item above
  (R-F6d, R-F6e, the Reloaded-divergence note, the code-quality
  finding, T-F6.1--T-F6.4 and the scope note under R-F6b) passed the
  KDF Dirty Gate, with a file-level score of 0.02 against the
  threshold of 0.10 and no leaks to scrub. One Reference-State
  (R13) finding on this chapter was corrected by the Spec Reader
  and the corrected lines re-gated at 0.00. Every token that
  matches the reference is wire-visible (the method names
  `orderbook` and `best_orders`, the response members `address`,
  `address_type` and `address_data`, the values `Transparent` and
  `Shielded`, and the protocol tag `ZHTLC`); the residual 0.02 is a
  paraphrase of an error this project's own binary emits, not
  reference expression. *Extent:* the orderbook read surface at
  the three rows `v2.6.0-beta`, `v3.0.0-beta` and `dev (as of
  2026-03-27)` -- both generations of `orderbook` and of
  `best_orders` and the two address-derivation routines they
  call, read at the relevant routines with row-to-row
  differences -- plus the four files of this project's own tree
  that the Reloaded-side claims rest on and a full-tree search for
  callers of the coin-generic resolver. *Method:* reading,
  row-pinned search for the routines and call sites, inter-row
  differences, and a scan of the candidate's added lines for commit
  identifiers, pull-request numbers, reference paths, module names
  and code tokens (no hits); the mechanical gate tool was not run
  because the material contains no code blocks. *Measure:* 0.02
  for this chapter, 0.00 for the cross-reference in chapter 39
  R39.5.3, 0.00 for entry 17 of the upstream-parity gaps. The
  earlier clearance positions recorded above are untouched by this
  addition.
- *Reloaded-original addition (2026-10-27) -- R-T3a, T-T1, T-T2:* prompted by
  a measurement on this project's own tree (the batch applier). *Inputs:* this
  repository's own trie-store code and its tests; no reference input of any
  kind. The forbidden corpus was not consulted for this item, and nothing in it
  states a reference behavior.
- *Reloaded-original addition (2026-10-27) -- R-T3b, T-T3, T-T4:* prompted
  by a measurement on this project's own tree (the index and trie update
  windows). *Inputs:* this repository's own code and a throwaway measurement
  harness of it; no reference input of any kind. The forbidden corpus was not
  consulted for this item, and nothing in it states a reference behavior.
- *Scoped addition (2026-10-28) -- keep-alive shape and synchronization
  contract (hotfix pass): R-W1a in §32.5.1; the open-decision note under
  §32.6 R-T3b; §32.7.3 (R-Q4, R-Q4a, R-Q5, R-Q6, T-Q4, T-Q4a, T-Q5, T-Q6)
  and the R-Q1 table cell it governs; R-F3a and its consequence note in
  §32.8; the amended R-K2, R-K2a, its divergence note and T-K1 to T-K4 in
  §32.9; D5 and D6 in §32.10; the pointer appended to §32.4 R-P4.*
  Prompted by a live network incident: Reloaded makers overloading public
  seed nodes. *Inputs:* this repository's own keep-alive producer,
  keep-alive receiver, synchronization requester and responder, gossip
  dispatch and multi-topic publish path (every Reloaded-side parity and
  divergence statement rests on these); and the forbidden corpus,
  consulted at the rows `v2.6.0-beta` and `v3.0.0-beta` of
  [Chapter 02](02-baseline-state.md) §2.15A, which agree in every
  respect taken (their difference in the files read is confined to an
  unrelated coin-protocol arm, a network-identifier default and
  copyright lines), **only** to establish: the per-pair, single-entry,
  own-topic shape of the keep-alive and the zero/empty-root skip
  (R-K2a); that the order messages' `pair_trie_root` is sent as zeros
  and read by no receiver (R-W1a); the synchronization trigger
  (subscription gating, zero/empty skip, get-or-init placeholder,
  advertised root named in the request, one request to the forwarding
  peer) and the responder's semantics ("no data" for an unknown pubkey,
  all-or-nothing over pairs, delta-chain-or-own-snapshot, no trie opened
  at the requested root, failure at an all-zero held root, synchronous
  construction under both locks), together with the 10 s timeout and
  the absence of retry, de-duplication and rate limiting (R-Q4 to
  R-Q6); the forwarding condition (R-F3a); that the reference never
  refuses a synchronization answer (the R-T3b note, D5); and the bounded
  and unbounded resources D6 lists, including the existence of a
  commentary acknowledging missing rate limiting, which is paraphrased
  and not quoted. What crosses into this chapter is behavior,
  dictated wire members (`trie_roots`, `timestamp`, `pair_trie_root`,
  `pubkey`, the request tag of R-Q1) and numeric values peers can
  observe on the wire or in their timing (30 s, 90 s, 10 s, 4 h,
  5 min, 10 min, 8192); no function name, type or field name outside
  the dictated wire members, no helper decomposition, no control-flow
  transcription, no error, log or comment text was reproduced. The
  held-root option R-Q4a, the alternative wording of D5, the hardening
  candidates of D6 and the T-items are Reloaded-original; the
  behavioral explanation of non-convergence under R-Q4a is reasoning
  from the bound semantics, not a reference fact.
- *Dirty Gate clearance for the 2026-10-28 scoped addition (chapter 01 T1,
  R40).* Every item the addition names **passed the KDF Dirty Gate** on
  2026-10-28: PASS, discretionary-expression measure 0.03 against the 0.10
  threshold, no leaks and nothing to scrub. *Extent:* the orderbook
  peer-to-peer keep-alive and pubkey-state synchronization surface --
  keep-alive producer, receiver and request builder, synchronization
  responder and delta/snapshot builder, per-order gossip wire shapes, the
  peer-request enum, gossip dispatch and the one-peer request path,
  request-response timeout handling, the trie-store worker and the periodic
  purge -- at the rows `v2.6.0-beta` and `v3.0.0-beta` (the compared material
  is identical at both). *Method:* line-by-line classification of every added
  chapter line against the compared reference material, plus targeted
  mechanical searches for identifiers, literals and constants; the mechanical
  gate tool was not run (prose). *Measure:* 0.03. Only dictated wire members
  and timing constants match; the invalid-state-root error R-Q5 names is the
  trie library's public error variant; the one measured similarity is the
  fact-level paraphrase of a reference comment under D6. The clearance is
  complete and not pending, and the earlier clearance positions recorded
  above are untouched by it. *(Clean-side edits after the pass, Reloaded
  original, no forbidden-class input: the step-order sentence under R-Q4
  that leaves the order of its two skips unbound, as the gate's
  testability remark asked.)*
- *Reloaded-original additions and decisions (2026-10-28), no forbidden-class
  input:* R-T3b item 4 and T-T5 (the keep-alive receiver compares with a
  caught-up trie store); the amendment of R-T3b item 2 that resolves D5 (the
  synchronization answer never refuses, with the full-snapshot rule for an
  order whose removal is still queued) and the matching changes to T-T3, the
  R-T3b measurement note, the R-F3a consequence note and the resolution notes
  under R-T3b and D5; the decision recorded under R-Q4a (adopted by default,
  with the opt-back setting `orderbook_sync_names_held_root`) and the matching
  wording of T-Q4a. *Inputs:* this repository's own keep-alive producer and
  receiver, synchronization requester and responder and trie-store worker; a
  deterministic measurement of that code with two in-process nodes (a fresh
  peer named the advertised root and ended with 13 of 20 orders, and with the
  held root 20 of 20; a fresh relay kept the all-zero root after an exchange
  that counted as success and failed the next requester; one keep-alive's K
  topic copies produced K requests); and the maintainer's decisions on D5 and
  R-Q4a. The reference facts these items rely on are the gated ones of the
  scoped addition above; nothing else from the reference was used.
- *Scoped addition (2026-10-28, second pass) -- relay forward ordering,
  failure classes and the bound inventory: the rewritten R-F3a, the new
  R-F3b and the rewritten consequence note in §32.8; the amended R-F3
  bullet; R-Q5a and T-Q5a in §32.7.3; the corrected last sentence of
  §32.6 R-T3b item 4 and the corrected sentence of the open-decision note
  under R-T3b; the corrected sentence of the R-K2a divergence note; the new
  §32.9A (R-B1, R-B2, R-B3, T-B1 to T-B3); the pointer appended to D6; V5.
  Dirty Gate: PASS on 2026-10-28, recorded below.* Prompted by the continued memory growth of public
  seed nodes under a Reloaded maker after the first pass. *Inputs:* this
  repository's own gossip dispatch, propagation call, keep-alive producer
  and receiver, request-response wrapper and vendored gossip substrate
  (every Reloaded parity statement rests on these); the public gossipsub
  library of the libp2p fork both reference rows and Reloaded pin, for
  the substrate rows of R-B1 and for R-F3a's forwarding order (a
  permitted sibling source, not forbidden-class material); the baseline
  anchor's in-tree gossip substrate (V5); and the forbidden corpus,
  consulted at the rows `v2.6.0-beta` and `v3.0.0-beta` of
  [Chapter 02](02-baseline-state.md) §2.15A, which agree in every respect
  taken, **only** to establish: that the reference's application forwards
  a second copy on success and never withholds the substrate's first copy
  (R-F3a, R-F3b); the creation and removal of pubkey records and per-pair
  entries, the zero-order snapshot removing the entry, and the three
  failure classes with their requester-side logging (R-Q5a); that the
  reference's trie updates run on a separate worker fed by an unbounded
  queue (the R-T3b item 4 correction); and the per-row bounds of R-B1
  rows 1 to 11, 18 and 19 (90 s, 5 min, 4 h, 120 s, 10 min, 10 s, 8192,
  the relay-count trim at twice the high watermark, the known-peer cap of
  100), the absence of a bound for rows 6 to 8 and the per-message task
  spawn, and that a reference seed subscribes to no orderbook topic. What
  crosses into this chapter is behavior and numeric values a peer can
  observe in timing or in the wire; no function, type or field name
  outside the dictated wire members, no helper decomposition, no
  control-flow transcription, no error, log or comment text was
  reproduced. The failure-class names of R-Q5a are this chapter's own
  vocabulary; the invalid-state-root error it refers to is the trie
  library's public variant. R-B2, R-B3, T-B1 to T-B3 and T-Q5a are
  Reloaded-original. The earlier clearance positions recorded above are
  untouched by this addition, except that the first-pass R-F3a and its
  consequence note are superseded and the first-pass wording of R-T3b
  item 4's last sentence is withdrawn.
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
- *Clean-side correction (2026-10-28), Reloaded's own observation claim, no
  reference input and no gate needed:* the R-K2a divergence note and the
  authorship line of §32.7.3 no longer say that the aggregated keep-alive
  shape overloaded the public seed nodes. They now record only that it was
  present on the network while those nodes were overloaded: the swaplab
  control on the build before the fix showed no growth attributable to the
  aggregated shape alone. The gated content -- the two shapes and what each
  costs a receiver -- is unchanged.
- *Reloaded-original addition (2026-10-28), no forbidden-class input:* R-K4
  (timer-driven expiry and capacity release of the delta histories, free expiry
  refresh, node-store capacity release, node release on clear, gauges). *Inputs:*
  this repository's own history, node-store and metrics code, and an in-process
  measurement of it with a counting allocator (a 120,480-update burst for 30
  orders of one pubkey retained about 1,240 bytes per update, 54 % in the delta
  history, 28 % in unreferenced nodes, 14 % in table capacity, 4 % in
  expiry-queue elements; the history caches expire only when touched; a
  full-snapshot clear leaked about 6 KB per 30-order pair). The reference facts
  it relies on are the gated ones of D6 and of §32.9A R-B1 rows 3 and 6 (bounds,
  purge period, nodes of a cleared slice never released); nothing else from the
  reference was used.
- *Reloaded-original addition (2026-10-28, Decision AN): R-Q5b, its
  divergence note, the amendment of R-T3b item 3, the sentence appended to
  R-Q6, and T-Q5b.* A relay answers a request for a pair it is still
  synchronizing when that synchronization lands, within a bound, with the
  opt-back setting. *Inputs:* this repository's own keep-alive receiver,
  synchronization requester and responder and inbound request dispatch; the
  gated rules of this chapter as merged (R-F3a, R-Q5, R-Q5a, R-Q6), on which
  the rule builds without adding any reference fact; a lab run of reference
  binaries as black boxes (subscribers of a reference seed failed with the
  placeholder error at the first keep-alive after a wipe and were rebuilt
  about a minute later); and the maintainer's decision. No reference
  implementation was consulted for this item, and it needs no Dirty Gate pass.
