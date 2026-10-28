# Plan: make `my_orders` fast at 2,000 orders

**Status:** planning (approach decided 2026-10-28: speed up the call instead of adding a filter; evaluation and implementation pending) — **Last reviewed:** 2026-10-28

> **Context:** an operator runs a maker bot with about 2,000 live maker orders
> (1,000 pairs, a bid and an ask each: 5 base coins x 200 rel coins). The bot
> cannot use `my_orders` to find the order to update, because the call "was
> very slow and has no filters", so it issues `setprice` (cancel and create)
> per pair every 10 minutes instead. The operator wants `my_orders` filtered by
> base and rel, because it would be called about 1,000 times per loop. No code
> is written until the maintainer decides.

> **Decision (2026-10-28, Tomáš):** make `my_orders` fast instead of adding a
> filter, pre-beta, so the operator's bot stays identical on both flavors of the
> framework (the filter of section 3 would exist on one flavor only). The
> approach to evaluate is to cache the decimal renderings of an order's numbers
> and reuse them wherever the same values are rendered, with the output digits
> unchanged and a target under 10 ms for 2,000 orders. Section 3 is kept as the
> rejected alternative; section 8 is the plan that replaces it.

## 1. Measured cost (dev `4ea11d5f1`, 2026-10-28)

In-process, release build, AMD EPYC 7702 (shared, busy), 2,000 `MakerOrder`
values over 1,000 pairs inserted into the maker-order context, 60 calls after 3
warm-ups. Two value profiles: **A**, volumes and prices that are non-terminating
rationals (what a price derived from a cross rate or a CEX feed produces); **B**,
values born from 8-decimal strings (what a user types into `setprice`).

| Call | A | B |
|---|---|---|
| `my_orders`, 2,000 orders: median (p95) | 191 ms (203) | 47 ms (52) |
| `my_orders`, 200 orders | 18 ms | not measured |
| `my_orders`, 1 order | 0.094 ms | not measured |
| response body, 2,000 orders | 1.83 MB (915 bytes/order) | 1.14 MB |
| `order_status` for one live uuid | 89 us | 24 us |

Cost is linear in the order count (about 90 us per order in A). It is an
in-memory read, as expected, but it is not milliseconds.

**Where the time goes** (2,000 orders, A, medians of one pass): converting the
orders to their RPC form 149 ms (78%), building the JSON value 22 ms,
serializing it 6 ms, cloning every order under its mutex 3.3 ms, cloning the
order map 0.5 ms. The conversion is slow because each order's `price`,
`max_base_vol`, `min_base_vol` and `available_amount` is turned from a rational
into a decimal by a real big-decimal division to 100 digits
(`rational_to_dec`, `mm2src/mm2_number/src/lib.rs:143`), about 18 to 20 us per
number on a non-terminating rational, four numbers per order. Values born from
decimal strings divide quickly, hence B. Locking and serde are not the problem.

**Lock contention is real, but not the average cost.** `my_orders` awaits every
order's mutex in turn, and does all conversion after the loop, so one order held
for 50 ms adds the full 50 ms to the call (measured: 191 to 240 ms at 2,000
orders, 18 to 68 ms at 200). Orders are held across slow awaits in several
places, the longest being the maintenance loop for orders missing from the
orderbook index, which keeps the order locked across a remote balance query
and the maximum-volume calculation (`lp_ordermatch/ordermatch_trading.rs`
738 to 765, repeating every 0.777 s). Others hold across file and sqlite
writes: balance-update handling (318 to 346), cancel and timeout paths (676 to
688, 889 to 935), taker-request and taker-connect handling (1082 to 1197), the
final write of `update_maker_order` (2126 to 2141), cancellation (1982 to 1998,
2402 to 2463). The synchronous `maker_orders_ctx` lock is held only for the 0.5
ms map clone and never across an await.

So the operator's "very slow" is plausible but not extreme: roughly 0.05 to 0.2
seconds per call, on one runtime thread, growing linearly, plus any wait behind
a held order. 1,000 such calls per loop would cost 50 to 190 seconds of
runtime-thread time, which is the real problem with the plan as stated.

## 2. What already gives a per-pair path to an own order uuid

- **`order_status {uuid}`**: 24 to 89 us, independent of the order count. Needs
  the uuid.
- **The uuid comes back from the calls that create and change orders.**
  `setprice` and `update_maker_order` return the order, including its uuid. A bot
  that keeps `pair -> uuid` from those responses needs no search at all.
- **`orders_history_by_filter`** already accepts `base`, `rel`, `order_type`
  and `status` (also price, volume, time and paging). It reads the sqlite
  `my_orders` table, whose live orders carry the status `Created` or `Updated`,
  so a pair lookup is two calls. The table has no index on `base`, `rel` or
  `status`, so each call is a full table scan that grows with the order history;
  not measured here.
- **`orderbook` / `best_orders`**: not a cheaper path. `orderbook` for a pair
  returns every maker's orders, marking ours with `is_mine`; `best_orders`
  searches other makers' orders.

## 3. The filter that was proposed (rejected 2026-10-28)

1. **Request:** `my_orders` takes two optional string members, `base` and `rel`
   (exact, case-sensitive ticker). Absent or `null` means "no constraint".
2. **Semantics:** `base` alone returns the orders whose base is that coin; `rel`
   alone those whose rel is that coin; both return the orders with that base and
   that rel (direction as stored: the ask `X/Y` is not the bid `Y/X`, which needs
   `base: Y, rel: X`). The filter applies to the maker and the taker order map
   alike. A coin nobody trades returns empty maps, not an error.
3. **Response:** unchanged in shape: `{"result": {"maker_orders": {...},
   "taker_orders": {...}}}`, with the same per-order fields. A call without the
   members is byte-identical to today's (field order within the maps follows the
   hash map, as now). Nodes that predate the filter ignore the unknown members,
   so a bot can send them safely and re-check the pair in the answer.

The handler is today `my_orders(ctx)` and ignores the request body
(`rpc/dispatcher/dispatcher_legacy.rs:103`); the filter changes it to read the
request.

Two ways to implement it:

- **Filter inside the existing loop.** Lock each order (about 1.6 us), compare
  base and rel, clone only matches. About 3 ms per call at 2,000 orders (the
  measured lock loop), plus the conversion of the few matches. 1,000 calls would
  cost about 3 seconds per loop. It still touches every order's mutex, so it
  still waits behind a slow holder.
- **Index by pair.** `MakerOrdersContext` already keeps `uuid -> base` and a
  count per base without locking any order; adding `(base, rel) -> uuids`
  (maintained in `add_order`, `remove_order`, `drop_expired` beside them) lets the
  call lock and convert only the matches, in microseconds, and be unaffected by
  other orders being held. It adds state that must stay in step with the three
  mutators, notably the expiry path.

## 4. The bot-side alternative (recommended regardless)

One unfiltered `my_orders` per loop (every 10 minutes), indexed by pair in the
bot, then `update_maker_order` per uuid. Measured cost of that one call: about
190 ms and 1.8 MB for 2,000 orders with non-terminating values, about 47 ms and
1.1 MB with decimal-born values: negligible once per ten minutes. Better still,
keep the uuid from each `setprice` / `update_maker_order` response and use
`my_orders` only at start-up and as a periodic reconciliation. This also avoids
the cancel-and-create of `setprice` on every loop.

## 5. Recommendation

1. Tell the operator the bot-side pattern (section 4); no node change is needed
   for it.
2. If Tomáš wants the filter anyway, do the **index** version: the call becomes
   independent of the order count and of held orders, which are the two real
   problems; the in-loop version still costs 3 ms and shares the lock waits. It
   is a small, additive change: one request type, one index, tests for
   unfiltered byte identity, base only, rel only, both, unknown coin, expiry
   keeping the index consistent.
3. Separately, and not needed for the operator: the conversion cost (78% of the
   unfiltered call) could be cut without changing the output, for example by not
   dividing to 100 digits for rationals that are already terminating, but the
   printed digits of a non-terminating value must stay identical, so this is a
   performance task with its own wire check, not part of the filter.

## 6. What changes in the docs

No CRD chapter binds `my_orders` in detail (the census lists it as an own-order
read). The filter would be an additive Reloaded extension: a line in
`docs/GLEEC_COMPATIBILITY.md` (additive request members, response unchanged),
the CHANGELOG, and the RPC reference. Whether the reference's `my_orders` takes
parameters is not known here; if parity with the reference matters for the
member names, a Spec Reader question comes first.

## 7. Open points for the maintainer

1. Filter at all, or the bot-side pattern only?
2. Index version or in-loop version?
3. Is the conversion cost (section 5, item 3) worth a separate task?

## 8. Approach: cache the decimals, drop the intermediate JSON value

**What the measured phases allow.** At 2,000 orders (profile A) the call is
about 191 ms: conversion 149, building a `serde_json::Value` 22, serializing it
6, cloning every order under its mutex 3.3, cloning the map 0.5. Caching the
decimals alone removes most of the 149 ms and leaves about 35 ms; the 10 ms
target also needs the 22 ms `Value` step gone (serialize the response structs
straight to bytes) and the number formatting inside serialization cheap. A
realistic floor is the 3.3 ms lock-and-clone loop plus a few ms of serialization,
so under 10 ms is reachable for decimal-born values; the non-terminating profile is held to 50 ms (section 9). Profile B (decimal-born
values, 47 ms today) shows the same shape at a quarter of the cost.

**Stage 1: memoize the rational-to-decimal step, content-addressed.** A bounded
memo from the exact rational (numerator and denominator) to the decimal that
`rational_to_dec` produces today (`mm2src/mm2_number/src/lib.rs:143`), used by
the order-to-RPC conversions for `price`, `max_base_vol`, `min_base_vol` and
`available_amount`. A miss computes with the existing function, so the digits are
identical by construction. Content addressing is the point: a cache stored in the
order itself or keyed by uuid must be invalidated at every mutation of those
fields, and orders are mutated in many places under their mutex (about 35 lock
sites in `ordermatch_trading.rs`); a stale decimal would be a silent wrong price.
A content-keyed memo cannot be stale. `available_amount` without matches equals
`max_base_vol`, so it hits the same entry. Cost per lookup is hashing two big
integers, well under a microsecond. Bound it (a few tens of thousands of entries,
cleared or aged out when full) so that a market maker whose prices change every
second does not grow it without limit, and keep it out of `rational_to_dec`
itself, which is used in arithmetic paths where a shared memo would add lock
contention for nothing. The same memo serves `order_status`, `setprice`,
`update_maker_order` and the orderbook entries, which render the same values.

**Stage 2: serialize the structs directly.** The handler builds
`json!({...})` from the map of converted structs, which materializes a full
`Value` tree (22 ms) and then writes it out (6 ms). Writing the same structure
with `serde_json::to_vec` of the typed response removes the intermediate tree.
The key order of the output must stay the same as today's (the maps are hash
maps and already unordered, so only the order inside one order object and the
two top-level keys is fixed).

**Stage 3 (only if still above 10 ms): pre-rendered number strings.** Memoize
the decimal's printed string as well as its value, and serialize it as a raw
string, so serialization stops formatting 8,000 big decimals per call
(about 9 of the 13.5 ms of struct serialization at profile A).

**Regression check, which is the acceptance test.** Keep the measurement test
(a copy is in the scratchpad of the session that measured it) as the harness:
2,000 orders in both value profiles, the old and the new handler on the same
orders. Because the maps are hash maps, whole bodies differ in key order from
call to call, so "byte for byte" is checked per order: for every uuid the
object's bytes are equal between old and new, and the top-level structure is
equal after sorting the map keys. A mismatch of a single digit fails the test.
Add the matched-order case (with `matches`, so `available_amount` differs from
`max_base_vol`) and a taker order. Timing targets: under 10 ms median at 2,000
orders for decimal-born values and under 50 ms for non-terminating ones,
measured after each stage so the stage that is enough is the last one done. Other RPCs that adopt the memo get their own
byte-identity checks.

**Risks.** A wrong memo key (for example hashing only the numerator) returns a
wrong decimal: the key is the full rational and the test compares against the old
output. The memo is shared state across tasks: use a small lock held only for the
lookup, or a per-thread cache, and show with a contended run that `my_orders` and
the order loop do not slow each other. The lock-wait problem of section 1 (an
order held for 50 ms adds 50 ms to the call) is not fixed by any of this;
that needs the pair index of section 3, or reading under a short lock without
awaiting each order in turn, and is outside this decision.

## 9. Decisions on the open points (2026-10-28, orchestrator, Tomáš informed)

1. The memo is content-addressed and bounded, keyed by the full rational; no
   per-order cached field.
2. Targets at 2,000 orders: under 10 ms median for decimal-born values (profile
   B), under 50 ms median for the non-terminating profile (A). Stage 3 (cached
   rendered strings) is done only if the second target is missed after stages 1
   and 2.
3. Every RPC that renders the same order values adopts the memo in the same
   change (`order_status`, `setprice`, `update_maker_order`, the orderbook
   entries), as long as that stays a mechanical substitution; anything beyond
   that is a follow-up.
4. The lock-wait problem of section 1 stays out of this change; another session
   is looking at the lock held across a remote call in the maintenance loop.

## 10. Outcome of the implementation (2026-10-29)

Built as sections 8 and 9 describe: a bounded, content-addressed memo of the decimal
rendering (keyed by the exact numerator and denominator, 32,768 entries, emptied when full),
typed serialization of the `my_orders` answer in place of the `json!` tree, and cached
rendered strings (stage 3). One step beyond the stage list was needed: the available amount
of an order without matches is a subtraction that cost about 8 ms of the 2,000-order call, so
it has its own memo keyed by the exact maximum volume. The orderbook entries, the match
amounts and the taker request amounts were left out: they are other makers' derived or
one-off values and would push the operator's own orders out of the memo.

Release build, 2,000 orders, interleaved old and new calls on the same orders, warm memo,
machine load average 260 to 600:

| Profile | Old | New, first call | New, repeated call | Target |
|---|---|---|---|---|
| B, decimal-born values | 56.0 ms | 39.4 ms | 12.3 ms (10.3 to 33.7) | under 10 ms: missed by about 2 ms |
| A, non-terminating values | 210.1 ms | 188.1 ms | 17.5 ms (14.9 to 75.2) | under 50 ms: met |

The first call after a restart, or after the memo empties, costs what it cost before. What
remains in profile B is the lock-and-clone loop (about 3 ms) and the serialization of three
rational arrays per order (about 6 ms); caching those would be a further stage and is not part
of this change. The maintainer accepted the profile-B miss on 2026-10-29 (12.3 ms against the 10 ms target,
about 4.5 times faster than before), so no such cache is added. The answer is byte for byte the old one for every order with at most one
match; with two or more matches the order of the `matches` entries was already random (a
`HashMap` with a per-instance hasher), so the test compares the parsed documents there.
`my_orders` now returns an error instead of panicking if the answer cannot be serialized.
