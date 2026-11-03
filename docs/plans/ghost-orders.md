# Plan: cancelled orders that other nodes keep showing ("ghost orders")

**Status:** planning — **Last reviewed:** 2026-11-02

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
| B1 | After the forwarding relay's sync request fails, or its answer leaves a pair out of step with the maker's advertised root, ask one other connected relay once. The pair is never emptied. Switch `sync_request_alternate_relay`. | ch.32 R-Q5d (D12 item 1) | M | CRD and code PRs to follow |
| B2 | A cancel this node received keeps the order out of sync answers for a longer window (for example one hour), so a stale relay cannot bring it back. Memory-capped. | to be written (D12 item 2) | S | waits for one reference fact: whether a cancelled order can come back under the same identifier |
| B3 | Per-pair liveness on the receiver's clock: a pair the maker stops advertising is dropped after the liveness window (`maker_order_timeout`, 90 s) while the maker's other pairs live on. | to be written (D12 item 3) | M | after B1 |
| B4 | During start-up and re-post bursts, hold a sync request for a pair the relay is about to sync, instead of answering "missing pair" at once. | to be written (D12 item 4) | M | after B5 and B1 |
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
- **B1:** a chain in which one relay misses a cancel. Before: the cancelled order comes back on the
  nodes behind it. After: those nodes ask another relay and lose the entry in the same round. Also
  a timeout case and an error case. A fault-free run with a busy maker passes when the extra
  requests are at most 5 % of all sync requests.
- **B3:** the emptied-pair case leaves every Reloaded node within the liveness window plus one sweep.
  A run with a lossy link counts pairs dropped and refilled and the longest gap an order-book poll
  sees.
- **B2 and B4:** recipes follow with their CRD rules.

## Open points

- **B2 needs one reference fact first:** whether a maker can make an order live again under the same
  identifier after its cancel, and with which message. The answer decides whether the one-hour
  window is safe and when it has to be lifted. The window and the memory cap also need the rate of
  cancel messages on a busy seed, from 0.4.0-alpha logs.
- **B3:** the window assumes that an online maker advertises every non-empty pair at every tick
  (CRD ch.32 R-K2a). Any exception widens the window.
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
