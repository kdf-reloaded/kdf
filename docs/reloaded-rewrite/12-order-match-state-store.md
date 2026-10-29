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

**R7a.** *A maker order's own lock is held for in-memory work only
(Reloaded; clean-room original; maintainer Decision AM, 2026-10-28).*
The per-order lock that the container hands out with each order MUST NOT
be held across an `await` of I/O -- a file write, move or delete, a
database statement, or a network read -- on these paths:

1. the balance-update handler of the order-matching layer;
2. the periodic check that cancels orders whose available amount fell
   below their minimum volume;
3. the periodic removal of timed-out matches;
4. the cancellation of an order whose own timeout elapsed (§12.8);
5. order cancellation: `cancel_order` in both of its forms, the
   cancellation of a pair's previous orders when a new order replaces
   them, and `cancel_all_orders`;
6. `update_maker_order`.

Each of these paths copies what it needs under a brief lock and releases
the lock. It then does its I/O with no order lock held. Where it applies
a change, it locks the order again, re-checks that the order is still in
the container and, where the change depends on it, that the order is
unchanged since the copy, and only then applies. A synchronous
announcement to peers (an order-updated or order-cancelled message) is
not I/O in this sense. It stays under the lock, so that peers receive an
order's announcements in the order of its changes. `update_maker_order`
announces only after its write has succeeded, so it locks the order
again to announce. That announcement takes every value it carries from
the order's state at the moment it is sent: the available volume, the
confirmation settings, and the price and minimum volume where the update
changed them. The last announcement of each value then matches the order
even when another path changed the order during the write. It is not
sent for an order that has left the container. A reader of the order
-- `my_orders`, `order_status`, the node's own orders in `orderbook`, the
keep-alive broadcast -- therefore never waits for an I/O operation of
these paths.

The paths that negotiate a match (a taker's request and a taker's
connect message) keep their writes under the order's lock and are outside
this rule, but they take part in the ordering of R7b.

**R7b.** *Storage order (Reloaded; clean-room original; Decision AM).*
Taking I/O out of the order's lock MUST NOT change what storage ends up
holding. For one order:

1. the stored active record and the order's history entry follow its
   state changes in the order in which they were made under the order's
   lock: a write that carries an older state never replaces one that
   carries a newer state;
2. once the order has been removed from the container, no write recreates
   its active record, and the history entry of the removal carries the
   order's last state;
3. a failing write is handled as before. For `update_maker_order` the
   RPC fails, and the order in memory returns to its state before the
   update unless it has changed again since. For the other paths the
   failure is logged.

> **As-built note (2026-10-28, clean-side record).** R7a and R7b are
> implemented in `lp_ordermatch/order_write_gate.rs` and the paths of
> `ordermatch_trading.rs`.
>
> *The gate.* Each maker order has a write gate (`OrderWriteGate`), kept
> in a side map of the maker-order store next to the order, and created
> when the order enters the store.
>
> - A writer publishes the order's state to the gate under the order's
>   lock (`PublishedState`). The gate keeps one unwritten snapshot, the
>   newest, stamped with a ticket. The flag that asks for the history row
>   to be updated as well is kept when a newer snapshot replaces an older
>   one.
> - The writer completes its ticket after releasing the lock. Completion
>   takes the gate's I/O lock and writes the snapshot unless an earlier
>   completion already wrote it or a newer one. A ticket covered by a
>   failed write reports that failure.
> - The gate is marked removed in the same step in which the order leaves
>   the store. From then on publishing returns nothing and completion
>   writes nothing.
> - The removal's deletion (`RemovedMakerOrder`) takes the I/O lock,
>   writes any snapshot still unwritten, and then deletes the order, so
>   the history entry carries the last state.
> - The gate never takes the order's lock.
> - The entry of a removed order is dropped from the side map at a later
>   insertion or removal, once no writer holds its gate. Only recorded
>   removals are visited.
> - The one storage function every changed state passes through is
>   `write_maker_order_state`. It replaces the earlier update-save
>   function.
>
> *The paths.* Besides the six of R7a, two more paths used to delete
> under the order's lock or remove an order outside it, and now follow
> the same pattern: `cancel_all_orders`, and the cancellation step of the
> reconciliation of chapter 60 R60.4.6 item 5. The expiry step moved
> unchanged into `cancel_expired_maker_orders`, so a test can run it.
> `update_maker_order` now also re-checks, after re-locking, that the
> order is still in the store, so an update that waited on a cancellation
> fails instead of recreating the record. The negotiation paths write
> through the gate with the order's lock held
> (`store_negotiated_maker_order`), and skip an order that has left the
> store.
>
> *Observable differences.* Two remain:
>
> - `cancel_all_orders` announces the cancellations of maker orders
>   before it removes the taker orders, not after.
> - When two updates of one order overlap and change the same value,
>   their announcements may interleave. Both carry the order's value at
>   the time they are sent.
>
> *Tests.* T7 and T8 are in `ordermatch_tests.rs`, module
> `t12_7_order_lock_io_tests`, together with a test of the side-map
> sweep.

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

**T7.** *No I/O under the order's lock (R7a).* For each path of R7a,
while its file or database operation is in progress (a test double holds
the operation open), the order's lock can be taken at once, so
`my_orders` and `order_status` answer without waiting. A mutation that
moves the operation back under the lock makes the test fail.

**T8.** *Storage order (R7b).* (a) Two updates of one order whose writes
complete in reverse order leave the newer state stored. (b) A
cancellation that runs while an update's write is in progress leaves no
active record, and its history entry carries the updated state. (c) A
timed-out match removed while the order is being cancelled does not
recreate the active record. (d) A failing write in `update_maker_order`
fails the RPC and restores the order's previous state in memory, and it
leaves alone a later change made by another path. (e) When the
balance-update handler lowers the order's volume while an update's write
is in progress, the update's announcement, sent after the write, carries
the lowered available volume. With no change during the write, the
announcement is the same message the update sent before R7a.

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

**D5.** *Match-negotiation writes under the order's lock (post-beta
candidate; Decision AM scope, 2026-10-28).* When a taker's request
reserves part of an order, and when a taker's connect message is
accepted, the order's active record is still written while the order's
lock is held. These are the paths R7a leaves out, and they take part in
the write ordering of R7b. Taking these writes out of the lock as well
would mean re-checking a reservation after the write, so it waits for a
separate maintainer decision.

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
- *Clean-side addition (2026-10-28), R7a, R7b, T7, T8, D5 and the as-built
  note after R7b:* written by a context that never read the forbidden
  corpus, from the maintainer's Decision AM and from this repository's own
  code (the per-order lock sites of the order-matching layer and its
  storage functions). They are Reloaded-original and state no reference
  behavior, so chapter 01 R38 is not engaged.
