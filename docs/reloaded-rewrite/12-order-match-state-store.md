# Chapter 12 — Maker-Order State Store

**Status:** driving-spec.

This chapter binds the maker-orders context substrate: a TTL-aware container
keyed by order identifier with two reverse indices for O(1) coin-presence
queries, the per-order optional auto-expiry parameter that surfaces on the
order-creation RPC, and the expiry-tick hook that routes timed-out orders
through the existing cancellation pipeline.

## 12.1 Executive Summary

The baseline tree stores live maker orders in a flat map keyed by order
identifier, held inside the order-matching central context behind an async
mutex. Two consequences follow:

- *No per-order TTL.* A maker order lives until the operator explicitly
  cancels it or the process exits. There is no in-process facility to
  express "keep this offer up for the next hour, then drop it." Graphical
  clients work around this by polling and re-issuing cancellations on a
  wall-clock schedule from outside the order-matching loop.
- *Linear queries by coin ticker.* Common predicates such as "does this
  wallet have any active maker orders for ticker T?" require iterating the
  whole map and locking each per-order mutex to read its base ticker. As
  several wallet-management codepaths perform this check on every coin
  enable / disable boundary, the cost is paid often even when most coins
  carry no orders.

This chapter binds a substrate that addresses both, with two design
constraints: no addition or removal of any RPC method; the wire-visible
change is confined to a single new optional request field on the existing
order-creation request type.

The substrate has four pieces:

- a **TTL-aware container** wrapping a third-party time-keyed map plus two
  reverse-lookup indices (uuid → ticker; ticker → live count);
- a **non-async lock discipline** on the container (the contained
  operations are I/O-free and never held across `await`);
- an **optional per-order timeout** plumbed from the order-creation
  request through the builder and the order value;
- an **expiry-tick hook** in the existing order-matching periodic worker
  that routes timed-out orders through the existing cancellation
  pipeline with a new dedicated cancellation reason.

## 12.2 Subsystem Shape

The container is owned exclusively by the order-matching central context,
behind a single non-async lock. Every mutator is a `&mut self` method on
the container, so the three internal data structures (the timed map and
the two reverse indices) cannot drift out of mutual consistency under
normal use.

The container is *not* `Clone`. Callers that need a snapshot of the live
order set use the explicit snapshot accessor, which returns a
plain map; the container is then free to mutate independently while the
snapshot is iterated.

Expiry is *collected* by the container (via a dedicated drain method) and
*routed* by the order-matching periodic worker. The container itself does
no P2P traffic and no on-disk persistence; both responsibilities remain
with the pre-existing cancellation pipeline.

## 12.3 Bound Container Surface

**R1.** The substrate introduces a container type, owned by the
order-matching central context, that internally maintains three
coordinated structures:

- a TTL-aware map keyed by order identifier, valued by a shared handle to
  the per-order mutex-wrapped order value;
- a reverse index from order identifier to base-ticker string;
- a reverse counter from base-ticker string to the current live-order
  count for that ticker.

The two reverse structures exist solely to satisfy the O(1) per-ticker
presence query (R5); they are an implementation detail of the container
and are never exposed to callers.

**R2.** The container MUST provide the following capabilities (method
names and signatures are an implementation choice; only the behaviour is
bound):

- *insert with auto-TTL selection* — insert an order, choosing a
  TTL-bearing backing when the order carries a per-order timeout and a
  non-expiring backing otherwise;
- *insert with explicit ticker and timeout* — insert using a
  caller-supplied ticker and an explicit optional timeout, for the
  kick-start and taker→maker-conversion paths;
- *remove by identifier* — remove an entry, return the previously stored
  handle, and decrement both reverse structures;
- *lookup by identifier* — read-through accessor returning the stored
  handle if present;
- *presence test by identifier*;
- *O(1) per-ticker presence query* (R5);
- *snapshot* — return a plain map copy for lock-free iteration by callers
  while the container mutates independently;
- *expired-entry drain* — remove and return every entry whose TTL has
  elapsed, for routing through the existing cancellation pipeline
  (R9/R14);
- *standard map-view accessors* — keys, length, and iteration.

Construction MUST yield an empty container.

**R3.** Every mutating operation (insert, remove, and expired-entry
drain) MUST update both reverse structures atomically with the
corresponding map mutation, within a single `&mut self` method.
Out-of-band mutation of any single structure is forbidden.

**R4.** Decrement operations on the ticker reverse counter MUST use
saturating arithmetic so a stray double-decrement during refactor cannot
underflow into a wrap-around large value.

**R5.** The O(1) per-ticker presence query MUST execute in O(1)
worst-case: it MUST consult the ticker reverse counter only and MUST NOT
iterate the order map.

## 12.4 Bound Lock Discipline

**R6.** The container MUST be stored on the order-matching central
context behind a non-async lock (a parking-lot-style mutex). The
substrate MUST NOT wrap the container in an async mutex.

**R7.** No method on the container performs I/O or holds the lock across
`await`. Callers that need to do `await`-bearing work on a snapshot of the
container's contents MUST first take the lock, copy out what they need
(typically via the snapshot accessor or by reading the specific Arc
handles they want), drop the lock, and then perform their `await`-bearing
work outside the critical section.

## 12.5 Bound TTL Semantics

**R8.** Insertion takes one of two backings based on whether a
per-order timeout is supplied:

- *Constant backing.* When the per-order timeout is absent, the order
  MUST be inserted with no TTL and MUST live until explicitly removed.
- *TTL backing.* When the per-order timeout is present, the order MUST
  be inserted with a TTL equal to the supplied minute count multiplied
  by 60 (interpreted as seconds). The substrate MUST NOT silently round
  or clamp the value below the operator-supplied number.

The per-order timeout type is bound as `Option<u16>`. The bound unit is
*minutes*; the bound upper extent is therefore approximately 45 days
(`u16::MAX` minutes). A future widening to `u32` or to a richer
duration type is recorded in Deferred Work.

## 12.6 Bound Wire-Field Extension

**R9.** The existing order-creation request type MUST gain a single new
optional field bound as `timeout_in_minutes: Option<u16>`. The field MUST
be marked with serde's default semantics so that pre-existing client
JSON omitting the field round-trips to `None` and produces the baseline
"never auto-expire" behaviour.

**R10.** The order-creation builder MUST surface this field via a
chainable builder accessor and MUST propagate the value verbatim into
the constructed order's per-order timeout field.

**R11.** The persisted on-disk order representation MUST tolerate the
absence of the per-order timeout field (legacy persisted records); the
substrate MUST default to `None` when reconstructing such a record.

**R12.** No RPC method name is added, removed or renamed by this
substrate. The wire-visible delta is confined to the single new
optional request field bound in R9.

## 12.7 Bound Cancellation-Reason Extension

**R13.** The pre-existing cancellation-reason enum that tags orders
moving out of the live set into the historical record MUST gain exactly
one new variant denoting auto-expiry. That reason is used by the
expiry-tick hook (R14) when a TTL-bounded order is drained.

## 12.8 Bound Expiry-Tick Hook

**R14.** At the start of each iteration of the existing order-matching
periodic worker, before any other per-order work, the substrate MUST
drain the container's expired entries under the non-async lock (releasing
the lock before any awaited work) and route each drained order through
the pre-existing maker-order cancellation pipeline tagged with the
auto-expiry reason (R13). Routing through that pipeline preserves its
established external effects: the P2P maker-order-cancelled broadcast,
the maker-orders history append (tagged with the auto-expiry reason), and
the removal of the on-disk active-order record.

**R15.** The substrate MUST NOT bypass the pre-existing cancellation
pipeline for expired orders; an expired order MUST receive the same
network, persistence and history treatment as an operator-initiated
cancellation, except for the cancellation-reason tag.

## 12.9 Bound External-Crate Surface

**R16.** A single third-party crate is bound as the TTL-aware-map
substrate. The substrate consumes exactly the documented methods of
that crate: TTL-bearing insertion, constant insertion, expired-entry
drain, removal, get, contains-key, iter, keys, len. No internal types
of the crate are referenced; the crate is treated as an opaque
black-box map with optional per-entry TTLs.

**R17.** The crate's WASM feature MUST be enabled for the WASM build
target so the time source uses the browser-clock equivalent rather than
the native-clock primitives. Native builds use the default time source.

## 12.10 Tests (test invariants)

**T1.** *Per-order TTL.* An order inserted with `timeout_in_minutes =
Some(N)` MUST be present in the container at any time strictly less than
`N` minutes after insertion, and MUST be returned by the
expired-entry drain at any time greater than `N` minutes after
insertion (modulo the granularity of the substrate's clock source).

**T2.** *Index consistency under remove.* For an order inserted with
ticker `T` and removed by identifier, the ticker reverse
counter for `T` MUST decrement by exactly one. Repeating the test for an
already-removed identifier MUST be a no-op on the counter (R4 saturating
arithmetic).

**T3.** *Index consistency under expiry.* For an order inserted with a
TTL that subsequently fires, the entry returned by the expired-entry
drain MUST no longer be present in the identifier reverse index nor
counted in the ticker reverse counter (the drain decrements *before*
returning the entries to the caller).

**T4.** *O(1) presence test.* The O(1) per-ticker presence query
MUST be implementable without iterating the order map. A test or audit
MUST confirm the query reads exactly the ticker
reverse counter.

**T5.** *Backward-compatible wire shape.* Order-creation JSON omitting
the new `timeout_in_minutes` field MUST round-trip to an order whose
per-order timeout is `None`. An order persisted under pre-substrate
schema (no `timeout_in_minutes` key in the on-disk record) MUST
reconstruct to an order with per-order timeout `None`.

**T6.** *Cancellation parity.* An expired order MUST traverse the same
external observable surface as an explicitly cancelled order: a
broadcast P2P cancellation notification, an appended history record
(differing only in the cancellation-reason tag), and removal of the
on-disk active-order record. The substrate-internal test or integration
test MUST observe all three.

## 12.11 Deferred Work

**D1.** A richer per-order duration type (replacing `Option<u16>`
minutes with a typed duration carrying its own units) is deferred. The
current substrate trades type richness for wire-shape minimality.

**D2.** A per-coin or per-pair *default* timeout policy (so an operator
can configure "all KMD orders default to one hour") is deferred. The
current substrate requires the per-order field on the order-creation
request when a timeout is desired.

**D3.** Migration of the per-order timeout into a typed `Duration` on
the wire (currently it is a bare integer with bound units) is deferred;
the wire shape is part of this chapter's contract.

**D4.** A more granular drain trigger (interrupt the periodic worker
immediately upon the next expiry, rather than waiting for the next
iteration boundary) is deferred; the current substrate accepts up to
one period of latency between TTL expiry and routed cancellation.

## 12.12 External References

- `timed-map` crate (the third-party TTL-aware-map substrate) at major
  version 1; bound feature `rustc-hash` for hash-map backing; bound
  feature `wasm` for WASM-target time source per R17.
- `rustc-hash` crate — bound hash implementation for the contained map.
- `parking_lot` crate — bound non-async mutex substrate per R6.
- `uuid` crate — bound order-identifier substrate.
- `serde` and `serde_json` — bound serialization substrate underlying
  the wire-shape compatibility in T5.
- Chapter 11 (order-match cancellation race) — orderbook-side
  cancellation cache; this chapter handles the seller-side (own-order)
  store. The two substrates do not share a data structure but operate
  on the same `uuid` identifier space.
- [Chapter 59](59-legacy-ordermatch-negotiation.md) (legacy order-match
  negotiation loop) — the loop that walks this store to answer a taker
  request (ch. 59 R59.4.3) and that prunes a stale in-flight match from
  a stored order to release its reserved volume (ch. 59 R59.4.4). Both
  operate on orders this chapter owns; ch. 59 binds when and why, this
  chapter binds the store and its TTL.

## 12.13 Baseline Verifications

**V1.** The baseline tree MUST be confirmed to hold maker orders in a
flat hash map keyed by `uuid` behind an async mutex on the order-
matching central context. No TTL machinery and no reverse-ticker index
is present in the baseline:

```
git -C <baseline> grep -nE 'my_maker_orders.*HashMap<Uuid, Arc<AsyncMutex<MakerOrder>'
```

**V2.** The baseline tree MUST be confirmed to lack any per-order
timeout field on the order-creation request type and on the maker-order
value:

```
git -C <baseline> grep -nE '\btimeout_in_minutes\b'
```

**V3.** The baseline cancellation-reason enum MUST be confirmed to lack
any auto-expiry variant. The baseline shape this substrate extends is a
closed set of operator-initiated and balance-driven reasons; the new
auto-expiry reason (R13) is purely additive.

## 12.14 Provenance Footer

- *Inputs consulted for this chapter:* the baseline tree at project
  baseline commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`, Chapter
  11 (orderbook-side cancellation cache), and the external crate
  documentation listed in §12.12.
- *Permitted-input classes used:* baseline source; the single wire-level
  field name introduced here as contract surface (`timeout_in_minutes`
  on the order-creation request); behavioural descriptions of the
  container substrate, its capabilities, and the new auto-expiry
  cancellation reason; third-party crate names; published documentation
  of the third-party time-keyed-map crate.
- *Sibling chapters cross-referenced:* Chapter 11.
- *Author of this chapter:* clean-room round-2 driving-spec working
  set.
- *Forbidden corpus:* not consulted.
