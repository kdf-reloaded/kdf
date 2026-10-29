# Plan: bound the trie mutation queue and each pair's delta history

**Status:** planning — **Last reviewed:** 2026-10-29

> **Context.** The seed-node memory incident of 2026-10-28 showed that a seed
> keeps about 1.2 KB per received order update. The low-risk part (timer expiry,
> capacity release, node release on clear, gauges, `malloc_trim`) is merged or
> in review (PRs #131, #134, #137). This plan is the second part, decision AL
> of 2026-10-29: two of its three items were approved before the beta, one was
> rejected. Both approved items are Reloaded-original; the reference bounds neither
> (CRD ch.32 §32.9A R-B1 rows 3 and 7).

## Decisions

| Item | Decision | Rule |
|---|---|---|
| AL-2: drain and bound the mutation queue | **Approved**, pre-beta | ch.32 R-K5 |
| AL-3: cap on each pair's delta history | **Approved**, pre-beta | ch.32 R-K6 |
| AL-1: store deltas as `(uuid, change)` and rebuild the values at answer time | **Rejected** (2026-10-29, see below) | none |

## AL-2: what is bound, and what is not

The queue between the order handlers and the trie worker has no limit and
costs about 1.9 KB per queued update while the worker lags. A hard bound is not
possible without a wait on the producer (forbidden by ch.32 R-T3b item 3) or the
loss of an event (the trie and the read index would diverge). So the worker is
made fast instead: it takes everything waiting, up to 1,024 batches or 2,048
events, and applies it as one sequence (R-K5 item 1). The result equals
one-by-one application (R-T3a); only the delta history gets fewer entries, and a
requester holding a root that fell inside a merged sequence gets a full snapshot.

The second half of AL-2 is the unreferenced nodes: about 350 bytes per update
until the ten-minute purge, which is 100 MB per purge window at 500 updates a
second and gigabytes at the incident burst rate. R-K5 item 2 adds an early purge
after 100,000 applied leaf updates (about 35 MB), at most every 5 seconds; the
ten-minute purge stays as the floor.

## AL-3: the cap

2,048 deltas per `(pubkey, pair)`, the oldest dropped first (R-K6). Past the cap a
requester gets the full snapshot, as it does after the 5-minute expiry. Memory
per slice is then bounded by about 1.4 MB, whatever the rate. Measured basis:
about 680 bytes per stored delta (516 bytes of order record in 12 allocations
plus map slot and queue element), measured in process with a counting allocator.

| Scenario | Delta history without the cap | With the cap (per active pair) |
|---|---|---|
| A maker with 1,000–2,000 orders refreshing every 10 minutes | 0.35–0.7 MB | the same (below the cap) |
| Lab load, 500 updates/s on one pair | about 100 MB | 1.4 MB |
| Incident burst, 27,000 updates/s on one pair | rate × 300 s: GB scale if sustained | 1.4 MB |

A node with many active pairs holds up to 1.4 MB per active pair (about 35 MB
for 25 pairs). Figures are arithmetic on the measured bytes, not lab runs.

## AL-1: rejected, with the reason

Storing a delta as `(uuid, change)` and rebuilding the order values when an
answer is built would cut a stored delta from about 680 to about 170 bytes (4×,
not 30×: the map slot and queue element stay). With AL-3 in place the history
per pair is already bounded to 1.4 MB, so AL-1 would save about 1 MB per busy
pair. It is rejected for these risks; it is not to be reopened without numbers that
answer each of them:

1. **Worker lag against the read index.** The read index is updated first and the
   trie operations are applied later by the worker. An answer rebuilt from the
   current read index can carry newer values than the trie root it names. The
   requester then applies it and ends at a root different from the advertised
   one, and falls back to a full resync. A stored-item delta is consistent with
   its root by construction.
2. **An order removed and added again inside the window** differs between the two
   builds.
3. **A lock to make the rebuild consistent** would be held across the read index
   and the trie store, which is the contention the trie worker was introduced to
   remove.
4. The claim that a chain of deltas always merges to the latest value per order
   and ends at the current root is untested.

## Tests and acceptance

T-K5, T-K6, T-K7 in ch.32 (bulk drain equals one-by-one, early purge, history
cap). In addition, in the implementing change: a measurement before and after on
the 120,480-update burst of the retention work (peak live bytes, and bytes 330 s
after the burst) and a sustained-rate run showing the node store stays below the
early-purge bound.

## Review finding and measurement of the lock hold (2026-10-29)

The review of the first implementation noted that readers take the orderbook lock
and then wait for the trie-store lock, so a long hold by the worker stalls every
orderbook user. Measured in a release build on a loaded host (30,000 stored
orders, 65,536 new events on distinct uuids, one sequence timed):

| Events per sequence | One pair, median | 100 pubkeys x 25 pairs, median |
|---|---|---|
| 16,384 | 374 ms | 232 ms |
| 4,096 | 90 ms | 52 ms |
| 2,048 (chosen) | 45 ms | 26 ms |
| 1,024 | 20 ms | 13 ms |

The bound was lowered from 16,384 to 2,048 events so that a sequence stays under
about 50 ms at the median. The one-batch-at-a-time hold of the old worker was
shorter, which is the price of the bulk drain; a time-based bound (apply in
chunks and release the lock after about 25 ms) is feasible, is about 30 lines,
and was not built.

## Open points for the maintainer

1. The early purge runs inside the worker's lock hold, after the apply. Measured
   in the same build: 79 ms with 30 live orders and about 300,000 stored nodes,
   280 ms with 30,000 live orders and about 640,000 stored nodes. It happens at
   most once per 5 seconds and only under sustained load; the ten-minute purge
   has the same cost today. Options: leave it; lower the 100,000-update threshold
   (roughly a quarter of the cost per purge at 25,000, but four times as many
   purges); or make the purge incremental (a larger change).

Moving the status of this plan is the maintainer's.
