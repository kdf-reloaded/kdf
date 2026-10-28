# Chapter 11 -- Order-Match Cancellation Race Mitigation

**Status:** driving-spec

> **One-sentence claim:** the codebase shall maintain a
> short-lived per-orderbook cache of recently-cancelled
> maker-order identifiers (keyed by cancelling publisher),
> consulted on the insert path so that out-of-order
> peer-to-peer delivery of a `create-then-cancel-then-create`
> sequence cannot resurrect an order the publisher has
> explicitly cancelled.

## 11.0 Executive Summary

The codebase's orderbook treats inbound peer-to-peer maker-
order messages as authoritative: a create message inserts or
refreshes; a cancel message removes. Because the underlying
publish-subscribe mesh delivers messages without ordering
guarantees, two scenarios race in a way a strictly-stateful
orderbook cannot recover from:

1. A publisher cancels an order and immediately republishes
   (for example, a price update). A third-party node may
   receive `(old create) → cancel → (new create)` or
   `cancel → (stale create that was queued for retransmit)`.
2. A network partition heals after a cancellation; buffered
   create messages from peers that did not learn of the
   cancellation in time replay onto recovering peers.

In both cases a strictly-stateful orderbook would re-create
a maker order that the publisher has explicitly removed,
where it would linger until its keep-alive timer expires --
seconds-to-tens-of-seconds during which a taker could match
against an order the maker would refuse to honour.

This chapter binds the mitigation: a per-orderbook short-
lived cache of (recently-cancelled identifier → cancelling
publisher) entries, consulted at insert time and used to
drop publisher-self-resurrection inserts silently.

## 11.1 Subsystem Shape

The mitigation has three behavioural surfaces, each bound
by this chapter:

| Surface                  | Effect                                        |
|--------------------------|-----------------------------------------------|
| Recently-cancelled cache | Time-bounded map (identifier → publisher)     |
| Cancellation recording   | Cache write on every cancellation             |
| Insert-time guard        | Cache lookup on every insert; drop on match   |

The cache lives on the orderbook value, not on a global; one
instance per orderbook so per-network-id orderbooks do not
share state.

## 11.2 Recently-Cancelled Cache

R1. **Time-bounded.** The cache shall be a time-keyed
    structure that returns absent for entries whose age
    exceeds a fixed time-to-live. The chapter binds the
    time-to-live as **120 seconds**. The rationale is
    coverage of realistic worst-case mesh delivery skew
    without permanently blocking legitimate later
    re-use of the same identifier by the same publisher.

R2. **Key is the maker-order identifier.** The cache key is
    the identifier the wire protocol uses to refer to a
    maker order (the version-4 random universally-unique
    identifier the publisher generates and gossips).

R3. **Value is the cancelling publisher's identity.** The
    cache value is the same publisher identity the wire
    protocol uses on create and cancel messages (the
    publisher's persistent peer-identity string).

R4. **Per-orderbook isolation.** Each orderbook instance
    carries its own cache. Per-network-id orderbooks do not
    share recently-cancelled state.

## 11.3 Cancellation Recording

R5. **Unconditional record.** The cancellation handler shall
    insert into the cache **before** performing any local
    state removal. The record shall be written regardless
    of whether the local node had previously seen the
    order: a node that learns of the cancellation
    before the create still records the identifier and
    therefore still suppresses a late-delivered create.

R6. **Single-publisher attribution.** The cancellation
    record carries the cancelling publisher's identity per
    R3; this is what scopes the insert-time guard (R8) to
    publisher-self-resurrection only.

R7. **Insertion under the same lock as removal.** The
    record-then-remove pair shall execute under the same
    orderbook lock that the removal already holds, so a
    concurrent insert cannot interleave between the record
    and the removal.

## 11.4 Insert-Time Guard

R8. **Drop on identifier-and-publisher match.** On every
    maker-order insert, the handler shall consult the
    recently-cancelled cache. If the cache contains the
    inserted identifier **and** the cached publisher
    matches the inserted publisher, the handler shall
    return without modifying the orderbook.

R9. **Publisher-scoped, not identifier-only.** A cache hit
    with a **different** publisher than the inserted one
    shall not suppress the insert. The mitigation
    explicitly targets self-resurrection only; identifier
    collisions across publishers are governed by the
    pre-existing collision-handling behaviour of the
    orderbook (and are negligible under version-4 random
    identifiers).

R10. **Time-out lets legitimate reuse through.** A cache
     miss because the entry's age exceeds the time-to-live
     (R1) shall proceed normally. The 120-second window is
     long enough to cover mesh skew and short enough that
     a publisher who legitimately wishes to re-use the
     same identifier later is not blocked indefinitely.

     *Informative (2026-10-27, KDF Spec Reader; passed the KDF Dirty Gate
     on 2026-10-27).* The time-to-live is a contract on lookups, not
     on storage: a lookup of an entry older than the
     time-to-live returns absent, while the memory the entry
     occupies may be reclaimed lazily, no later than the next
     write into the cache. The reference behaves this way at
     `v2.6.0-beta` and `v3.0.0-beta`: its time-keyed map
     filters expired entries on read and reclaims them on
     writes. *Reloaded divergence:* Reloaded's cache reclaims
     on write only and its lookup does not filter by age, so an
     entry older than 120 s still suppresses an insert until
     the next cancellation is recorded. This is peer-visible
     only to a publisher re-using an identifier after the
     window, which version-4 identifiers make negligible; the
     remedy is an age check in the lookup.

R11. **Drop shall be logged at warn level.** A suppressed
     insert shall emit a single warn-level log entry
     identifying the dropped identifier; this is the only
     externally-observable signal of the guard firing.

R12. **No effect on the publisher's own orderbook.** The
     guard runs on the receiving side of the publish-
     subscribe mesh; a publisher's local outgoing
     orderbook is unaffected by R8.

## 11.5 Default Construction

R13. **Explicit default constructor.** The orderbook value's
     default constructor shall initialise the recently-
     cancelled cache with the time-to-live of R1 and shall
     otherwise initialise every pre-existing field to its
     own default. The codebase shall not use any default-
     derivation mechanism that would silently skip the
     cache field; the cache must be present and live in
     every orderbook instance.

## 11.6 Tests

The mitigation shall be covered by two unit tests:

T1. **Self-resurrection is dropped.** Insert an order;
    cancel it; attempt to re-insert an order with the same
    identifier and the same publisher; assert the order is
    not present in the orderbook afterwards.

T2. **Cross-publisher resurrection is not dropped.** Insert
    an order; cancel it; attempt to re-insert an order
    with the same identifier and a **different** publisher;
    assert the order **is** present in the orderbook
    afterwards.

The test pair shall live in the orderbook's own test module
and shall not require fixtures beyond the in-process
orderbook setup used elsewhere in that module.

## 11.7 Runtime Invariants

| Invariant                                              | Bound by  |
|--------------------------------------------------------|-----------|
| Time-to-live is exactly 120 seconds                    | R1        |
| Cancellation is recorded before local removal          | R5, R7    |
| Guard fires only on identifier-and-publisher match     | R8, R9    |
| Cross-publisher identifier collisions behave unchanged | R9        |
| Default constructor produces a live cache              | R13       |

## 11.8 Deferred Work

D1. **Persistent recently-cancelled cache.** The cache is
    in-memory and per-process. A daemon restart loses the
    recently-cancelled record; a late-delivered create
    received after restart can therefore resurrect an
    order cancelled shortly before restart. Persisting the
    cache to durable storage with a matching time-to-live
    would close this gap; it is not in scope at the time
    of writing.

D2. **Time-to-live as configuration.** The 120-second
    constant of R1 is a literal. Exposing it as a tunable
    (per-network-id or daemon-wide) would let operators
    trade memory for tolerance of larger mesh skews; it is
    not in scope at the time of writing.

D3. **Cross-instance attribution.** The guard distinguishes
    by publisher identity (R3), not by signing key. If the
    wire protocol grows a key-rotation mechanism, the
    cache attribution shall be revisited to follow whatever
    canonical publisher-identity field the protocol adopts.

## 11.9 External References

- The publish-subscribe mesh protocol the codebase uses
  for order propagation, whose "no delivery ordering"
  property motivates the mitigation (referenced via
  [Chapter 28](28-libp2p-modernization.md)).
- The version-4 random universally-unique-identifier
  scheme (RFC 4122) under which cross-publisher
  identifier collisions are negligible.
- The orderbook substrate whose insert and cancellation
  paths the mitigation extends (referenced via
  [Chapter 32](32-orderbook-p2p-and-trie.md)). Chapter
  12 binds a distinct store -- the node's own live maker
  orders -- and is not the orderbook substrate this
  mitigation extends.
- The negotiation loop that consumes the orderbook this
  mitigation protects, and the separate liveness eviction
  that removes a silent peer's rows entirely --
  [Chapter 59](59-legacy-ordermatch-negotiation.md). The
  two mechanisms are independent and must not be
  conflated: this chapter's cache suppresses a *late
  create* for a *cancelled* order within a fixed window,
  whereas ch. 59 §59.6 drops *all* rows of a peer that
  stopped advertising. An order whose maker went silent
  without cancelling is ch. 59's case, not this one.

## 11.10 Baseline Verifications

The following are verifiable from the baseline state defined
in [Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline orderbook carries no recently-cancelled
    cache or equivalent. Verifiable by tree-wide
    `git grep -E 'recently_cancelled|RECENTLY_CANCELLED'`
    against the baseline; matches are zero.

V2. The baseline orderbook's cancellation handler removes
    state without recording cancellation history;
    re-creation of a cancelled identifier by the same
    publisher therefore proceeds at the baseline. Verifiable
    by inspection of the baseline orderbook's deletion path.

V3. The publish-subscribe mesh protocol the codebase uses
    is present at the baseline and provides no delivery-
    ordering guarantees; the race the mitigation closes
    is therefore a pre-existing wire-level property, not
    one introduced after the baseline.

## 11.11 Provenance Footer

- *Status:* driving-spec.
- *Version:* v2.
- *Verified against:* baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`; absence of
  the recently-cancelled cache and its constant at
  baseline verified via tree-wide `git grep`; the
  publish-subscribe mesh protocol's published "no delivery
  ordering" property; RFC 4122 (version-4 random
  universally-unique-identifier collision properties); the
  orderbook substrate's pre-existing time-keyed-map
  primitive that the cache instantiates.
- *Forbidden corpus:* not consulted, except (addition of
  2026-10-27, KDF Spec Reader, passed the KDF Dirty Gate on 2026-10-27) for the
  informative note under R10: the read-time filtering and
  write-time reclamation of the reference's cache at the rows
  `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, which is
  the documented behavior of the public time-keyed-map crate the
  reference uses there. No private identifier, body or literal
  was reproduced. Dirty Gate clearance (chapter 01 T1, R40): PASS on 2026-10-27, measure
  0.00, no leaks. *Extent:* the dependency lockfiles at both rows and the one
  use site of the cache, with the primitive read from the public crate. *Method:*
  reading plus a mechanical scan of the added lines. *Measure:* 0.00 against the
  0.10 threshold.
