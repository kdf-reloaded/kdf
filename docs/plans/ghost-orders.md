# Plan: cancelled orders that other nodes keep showing ("ghost orders")

**Status:** planning — **Last reviewed:** 2026-11-03

> **Context.** On the public network on 2026-10-31, a maker re-priced its orders with `setprice` and
> `cancel_previous`, yet other nodes, Reloaded seeds among them, kept showing five of its orders on one
> pair where only two were valid. The extra entries left only when the maker restarted and posted
> again. They could not be matched or traded. CRD ch.32 §32.9C records how such an entry arises and
> what removes it. A node that missed a cancel keeps the entry until one of four things happens:
> - a later cancel reaches it;
> - a sync answer for that pair replaces the entry;
> - the maker goes offline for the liveness window;
> - a local order-book request refills the pair.
>
> A sync answer carries the responding node's own view, so a relay that missed the cancel passes the
> stale entry on, and a pair the maker has emptied is never advertised again. Reloaded behaves like
> the reference release rows here. The 0.3.0-beta.1 release lists the behavior as a known
> limitation (Decision BK, 2026-11-01, CRD ch.32 D12). This plan is the work after the beta, in the
> order D12 records, with the parking limits first because they are small and relieve busy seeds at
> once.

## Work packages

| ID | What | CRD | Size | State |
|---|---|---|---|---|
| B5 | Higher, configurable limits for sync requests a relay holds while its own sync is in flight: 32 per asking peer and 2,048 in all by default (were 8 and 256), with ceilings of 64 and 8,192. | ch.32 R-Q5b item 4 | S | PR #205 |
| B1 | After the forwarding relay's sync request fails, or its answer leaves a pair out of step with the maker's advertised root, ask one other connected relay once. The pair is never emptied. Switch `sync_request_alternate_relay`. | ch.32 R-Q5d (D12 item 1) | M | CRD PR #207, code PR #208 |
| B2 | A cancel this node received keeps the order out of every later order message and answer for an hour by default (`cancel_final_window_secs`, at least 120 s, at most one day), so a stale relay cannot bring it back. Memory-capped: the cancels of the last 120 s, as on Komodo and GLEEC KDF, plus at most `cancel_final_max_entries` older ones (100,000 by default, about 20 MB); past the cap the oldest fall back to 120 s. Decided 2026-11-03 (Decision 29). | ch.32 R-K1b (D12 item 2) | M | CRD PR #223, code PR #224 |
| B2a | Restore guard. At start, a maker skips and deletes a stored active order whose removal its order history already records, and it retries a failed deletion of a removed order's record (three tries, 200 ms apart, each failure logged once). Switch `maker_restore_skips_cancelled`, default on; off restores every record as Komodo and GLEEC KDF do. Limits: an order created with `save_in_history: false`, or one whose history write failed together with the deletion, still comes back; taker orders are not covered. Decided 2026-11-03 (Decision 29). | ch.12 R7c (D12 item 2) | S | CRD PR #223, code PR #224 |
| B3 | Per-pair liveness on the receiver's clock: a pair the maker stops advertising is dropped after the liveness window (`maker_order_timeout`, 90 s) while the maker's other pairs live on. A keep-alive naming the pair refreshes it whatever root it advertises. Switch `pair_liveness`, default on. | ch.32 §32.9C.2 R-GH14 (D12 item 3) | M | CRD PR #243, code PR #NNN |
| B4 | During start-up and re-post bursts, hold a sync request for a pair the relay is about to sync, instead of answering "missing pair" at once. | to be written (D12 item 4) | M | after B5 and B1 |
| B4a | Order-book answer under a lagging worker: when an order-book request lists a maker that has no trie record yet, the relay waits briefly (at most the bound a keep-alive already waits, CRD ch.32 R-T3b) for its trie worker and answers then; it fails as today only if the worker is still behind. Own switch, default on; off is the immediate error of Komodo and GLEEC KDF (CRD ch.32 R-GH12). Decided 2026-11-03 (Decision 28). | to be written with B4 (D12 item 4) | S | with B4 |
| L1 | Lab: a live trigger of the second request after a quick failure (CRD ch.32 R-Q5c) on a chain with 200 ms latency, with the setting on and off. | — | S | after the 0.4.0-alpha.1 bump |
| L2 | Lab: a chain with one heavily loaded reference relay, as the reproduction of the field case for B1; before and after on the same binary. | — | M | after B1 |

Each package changes no wire shape and has its own switch, so that a lab can run "before" and
"after" on one binary. Each one is recorded in `RELOADED_VS_GLEEC.md` and
`docs/GLEEC_COMPATIBILITY.md` when it lands.

## Verification

- **B5:** a burst case (a maker with 40 or more pairs restarts behind one Reloaded relay with three
  nodes behind it). Count the requests answered at once because the limits were full, and the
  relay's peak number of held requests and its memory, at the old and the new limits. Both are set
  through configuration.
- **B1:** a chain in which a relay and the node behind it both miss a cancel on a pair the maker still
  advertises. Before: the node clears the entry only after its relay has healed, one keep-alive
  interval later or more. After: it asks another relay and clears it in the first interval. Also a
  timeout case and an error case. A fault-free run with a busy maker passes when the extra requests
  are at most 5 % of all sync requests. *(Lab control on 0.4.0-alpha.1, 2026-11-03: in a fault-free
  chain a pair the maker still advertises heals hop by hop, one interval per hop, as CRD ch.32 R-GH4
  says, so the entry did not persist. It persists only when the forwarding relay's sync answers keep
  failing, as in the field; that case is the loaded-relay lab L2.)* *(Lab on H 66011cdcb, 2026-11-05: the case of a relay that
  missed a cancel could not be created with connection-level faults. After a connection reset the
  relays reconnected to the maker's seed only after 47 s and 119 s, so the maker's whole record
  expired on the node after 90 s without a keep-alive, as the liveness window says. With two healthy
  relays, the stale one never delivered the keep-alive first. No defect was seen. The rule is covered
  in-process by CRD ch.32 T-K8 (b).)*
- **B3:** the emptied-pair case leaves every Reloaded node within the liveness window plus one sweep.
  A run with a lossy link counts pairs dropped and refilled and the longest gap an order-book poll
  sees. The "before" baseline is a lab run on 0.4.0-alpha.1 in which the maker cancels its only
  order on a pair while one relay is cut off. B1 does not act on this case, because no keep-alive
  names an emptied pair. *(Baseline, 2026-11-03, on 0.4.0-alpha.1: the stale entry stayed
  for the whole 15 minutes on all three nodes that missed the cancel. It cleared only after the
  maker posted on that pair again. No sync request was sent in those 15 minutes, and no valid order
  was lost.)*
- **B2:** a chain of a maker, a relay that misses a cancel, and a node that receives it. More than
  120 s after the cancel, the node gets the pair from the stale relay (a sync answer or an order-book
  request). Before: the cancelled order comes back on the node. After: it stays out for the window,
  with the setting at its default and at `120`. A cancel-rate count on a busy seed checks the cap. *(Lab on H 66011cdcb, 2026-11-05: not constructible with
  connection-level faults. A node that received the cancel asks a relay only when a keep-alive
  shows a root other than its own, and its `orderbook` call requests the pair once per
  subscription. A relay that missed the cancel cannot stay stale for more than 120 s while staying
  alive: without the maker's keep-alives it drops the maker after 90 s, and with them it heals at the
  first one. The rule is covered in-process by CRD ch.32 T-K8 (a) and (b).)*
- **B2a:** unit tests (CRD ch.12 T9); no lab run.
- **B4:** the recipe follows with its CRD rule.

## Open points

- **B2's reference fact (recorded 2026-11-03, CRD ch.32 R-GH11):** whether a maker can make an
  order live again under the same identifier after its cancel, and with which message. At the
  reference release rows no path does so on purpose, and a new order never reuses a cancelled
  order's identifier. A cancelled order comes back in two ways:
  - An order update that runs into the cancel publishes an "updated" message for it. That message
    refreshes the entry at any node that missed the cancel, and changes nothing at a node that
    already dropped the order.
  - Its stored record, when it outlives the cancel, is restored at the maker's next start, which
    publishes an ordinary "created" message for it. The record outlives the cancel when its deletion
    fails, or when an order update or the match-pruning step writes it back after the cancel.

  KDF Reloaded never writes the record back (CRD ch.12 R7b). A failed deletion is logged and not
  retried, as on the reference; B2a adds the retry and the restore guard. Since no path revives a
  cancelled order on purpose, the one-hour window of B2 is safe and needs no message that lifts it.
- **B2, the cap is provisional.** The default of 100,000 older entries is to be checked against the
  rate of cancel messages on a busy seed, from 0.4.0-alpha logs.
- **B2a, what the restore guard cannot see.** It reads the order history, so an order created with
  `save_in_history: false`, or one whose history write failed together with the deletion, is still
  restored. The deletion retry narrows that case. Taker orders have the same restore path and are
  not covered; a stale taker record lives only until the taker order's timeout.
- **B3:** the window assumes that an online maker advertises every non-empty pair at every tick
  (CRD ch.32 R-K2a). Confirmed for the reference release rows on 2026-11-03 (CRD ch.32 R-GH13
  item 1): no pair with a non-empty root is skipped. A Reloaded maker does the same, except that it
  skips a round while its trie worker lags (CRD ch.32 R-T3b item 2); the window of three rounds
  absorbs two such skips. Any other exception would prune a live pair, which the next keep-alive
  refills with one sync request.
- **After 0.4.0, low priority: refresh the guard entry on a repeated cancel from the same
  publisher.** The reference restarts its window on such a repeat; Reloaded keeps the first cancel's
  expiry. It matters only with `cancel_final_window_secs: 120`; at the one-hour default the case
  hardly arises.
- **B1, an extra request after a skipped second request.** When a node's first sync request fails
  quickly and the pairs leave the placeholder before the second request (an order message filled
  them), no second request is sent, yet the request to another relay still follows, as CRD ch.32
  R-Q5d reads. It costs one redundant request. It stays as it is unless the lab or field counts show
  it matters.
- **B1, a second waiting task per sync.** After each applied sync answer, B1 starts a task that waits
  for the trie worker before it compares roots, next to the task that already waits to end the
  record of R-Q5b. Under a resync burst with a lagging worker, this doubles that polling of the trie
  store. If the large batch-1 run shows the cost (the latency rows or the B1 cost case), a follow-up
  merges the two into one task.
- **The known-limitations line** ("other nodes can keep showing a cancelled order for a while") stays
  until B1 and B3 have landed. B1 shortens the case of a pair the maker still advertises; B3 removes
  the case of an emptied pair.

## Not in scope

- Clearing a pair whose root does not match after an answer. D12 rejects this, because it would drop
  the maker's valid orders together with the stale ones.
- Ordering keep-alives by the sender's timestamp. D12 rejects this, because a maker with a wrong
  clock would lose all its keep-alives.
- Makers advertising an emptied pair, and receivers treating such an advertisement as "clear this
  pair" (D12 (c) and (d)). These come only if B3 is rejected.
