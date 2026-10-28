# Plan: keep a maker's book through a keep-alive stall, and rebuild it faster (Decision AN)

**Status:** planning — **Last reviewed:** 2026-10-28

> **Context.** In the lab on 2026-10-28, a reference seed and three reference
> subscribers held a Reloaded maker's full book of 600 orders.
> 1. The gossip stream from the maker to the seed stalled for about 150 s, and
>    no keep-alive arrived.
> 2. All four nodes dropped the whole book in the same second, 90 s after the
>    last keep-alive. Every later update was then logged as "Couldn't find an
>    order".
> 3. The subscribers' sync requests to the seed failed with
>    `InvalidStateRoot([0;8])`.
> 4. About a minute later, a full re-sync rebuilt the book.
>
> Our own relay behaves the same way:
> - It drops a pubkey's whole state 90 s after its last keep-alive (CRD ch.59
>   R59.6.1, R59.6.4). Order messages do not count as liveness.
> - On the first keep-alive afterwards, it requests the full set from upstream.
>   It has already forwarded that keep-alive at the gossip layer (ch.28 R11a,
>   ch.32 R-F3a).
> - Its subscribers therefore ask it while its own sync is still in flight.
>   They meet the all-zero placeholder and fail as a whole (ch.32 R-Q5 items 2
>   and 3), and ask again only at the next keep-alive (R-Q6).
> - Each wiped relay hop costs one advertisement interval, 30 s.
>
> The analysis is in `local/notes/2026-10-28-relay-wipe-after-keepalive-stall.md`.
>
> This plan covers what our nodes can do about it. All options are
> Reloaded-original and wire-compatible.

## One page for the maintainer

**Two separate problems.**
1. **The wipe.** A maker whose keep-alives stop arriving for 90 s loses its
   whole book on every node, even while its updates still arrive. The send
   queue is newest-first (`docs/plans/gossip-send-queue-bound.md`), so under a
   backlog the keep-alives are exactly what starves while updates get through.
2. **The slow rebuild.** After a wipe, subscribers of a relay fail their first
   sync request deterministically, because they always ask while the relay is
   still syncing. Each relay hop then adds 30 s.

**Options.**

| | What it does | Fixes | Who benefits | Departs from |
|---|---|---|---|---|
| **(c) Order messages count as liveness** | A signed created/updated/cancelled message from a maker refreshes that maker's liveness, as a keep-alive does. | Problem 1, when updates still flow | Our relays and their subscribers, reference ones included | ch.59 R59.6.1 (local effect only) |
| **(a) The relay defers its answer** | A sync request for a pair the relay is still syncing is answered when its own sync lands, within 5 s, instead of failing at once. | Problem 2 | Every subscriber of our relays, reference ones included | ch.32 R-Q5 timing (the outcome is unchanged) |
| **(b) Short requester retry** | After a failed sync, our node asks once more after 2–5 s instead of waiting 30 s. | Problem 2, against relays of any kind | Our own nodes only | ch.32 R-Q6 "no retry" |
| **(d) Keep-alive round after a relay (re)connect** | The maker sends one keep-alive per pair right after a relay connection comes up. | Shortens problem 1 after a reconnect | Everyone | — |

**Recommendation: (c) and (a); (b) not now; (d) as planned in Decision AO.**
- (c) prevents the wipe in the starvation case.
- (a) makes the rebuild after a real stall take seconds instead of 30–60 s. It
  does so for reference subscribers too, which nothing on the requester side
  can.
- (b) adds requests and helps only our own nodes. Revisit it if the lab shows
  that reference relays are the bottleneck once (a) is deployed.
- (d) belongs to the maker-side plan `docs/plans/maker-gossip-rate-control.md`,
  item (c)4, owned by tnn-miner-1c. It is **not** part of this plan.

**Neither (a) nor (c) helps** when the stream stalls completely, with neither
updates nor keep-alives arriving. Our relay then drops the book at 90 s, as the
reference does. That case is a matter of the send queue and of the AO rate
control, not of this plan.

**Decisions (maintainer, 2026-10-28).**
1. (c) and (a) are adopted.
2. The deferral bound is 5 s.
3. Both get compatibility switches: `order_messages_refresh_liveness` and
   `sync_answer_deferral_ms`.
4. (b) is deferred.

## (c) Order messages count as liveness

**Rule (Reloaded-original, ch.59 R59.6.1a).** When a node accepts a maker-order
message (created, updated or cancelled) whose signature verifies for pubkey P,
and the node already holds state for P, it records its local receipt time as
P's liveness instant, exactly as for a keep-alive. The rules for creating state
are unchanged:
- a created message creates P's record, as today;
- an updated or cancelled message for an unknown P creates nothing.

**Effect.**
- While updates from P keep arriving, P is never evicted, however long its
  keep-alives starve.
- A maker that is really gone sends nothing, and is evicted after 90 s as
  today. The dead-row defence of ch.59 §59.6 is unchanged.

**What a reference peer does differently.** It still evicts P at 90 s without
a keep-alive.
- Its subscribers that wiped then ask *our* relay at the next keep-alive. Our
  relay still holds the book and answers with the full set at once. Their
  rebuild gets faster, not slower.
- Our relay keeps forwarding P's updates, which a wiped reference node logs as
  "Couldn't find an order". That is unchanged from today.

**Replay.** A peer could re-publish an old signed order message of P after the
60 s duplicate window, to keep a dead P alive. The same is already possible
today with an old signed keep-alive, whose timestamp the receiver ignores
(R59.6.2). (c) therefore adds no new attack class. The divergence note records
this.

**Where in the code.**
- `process_msg` (ordermatch_orderbook.rs): on the three order-message arms,
  after a successful decode.
- A trie-store write of `last_keep_alive` for an existing record, done
  record-only, without get-or-init.
- The sweep in `lp_ordermatch_loop` is unchanged.

**Switch.** The top-level `MM2.json` boolean `order_messages_refresh_liveness`,
default `true`. A JSON `false` restores the reference rule.
- **Default-side rule:** arm 3. There is no wire change, but other nodes see
  our relay keep a book that a reference relay would drop. Proposed default:
  Reloaded.

## (a) The relay defers its answer for a pair it is still syncing

**Rule (Reloaded-original, ch.32 R-Q5b; amends the timing of R-Q5, never its
outcome).** A request `SyncPubkeyOrderbookState { P, trie_roots }` is
**deferrable** when, for every requested pair:
- the responder holds either a usable root, or the all-zero placeholder; and
- for every placeholder pair, the responder's **own** keep-alive-driven sync
  for (P, pair) is in flight, meaning sent and not yet answered and applied.

A deferrable request is parked. Its answer is built when the last of those
syncs has been applied by the trie worker, or when the bound expires,
whichever comes first.

**After the wait.**
- It is answered as R-Q5 answers it at that moment, all-or-nothing as before.
- If the responder's own sync failed or the bound expired, the requester gets
  the same error it would have got at once.

**Not deferrable, answered at once as today:**
- an unknown pubkey: "no data";
- a missing pair;
- a placeholder with no sync of ours in flight.

**Bound.**
- 5 s from the request's arrival. This is well under the requester's 10 s
  timeout and leaves room for the network.
- A setting, `sync_answer_deferral_ms`, default 5000. The value `0` turns
  deferral off and gives the reference timing.

**Off the event loop.** The inbound request path keeps its R-T3b item 3 rule:
nothing waits there.
- The dispatcher moves the response channel into a parked entry and returns.
- A completion hook on our own sync, plus a timer task, sends the answer
  through the same `SendResponse` command the dispatcher uses today
  (lp_network.rs `process_p2p_request`).
- The response channel is an owned value, so deferral needs no change in the
  transport.

**Limits.**
- At most 8 parked requests per peer and 256 in total. Past either, the request
  is answered at once with today's error.
- The libp2p request-response layer keeps a parked inbound stream open, so
  these caps must stay below its concurrent-stream limit. Implementation checks
  the configured value and asserts the relation in a test.
- Memory per parked request: the response channel and the requested roots.

**What the requester sees.**
- A subscriber of our relay, reference or Reloaded, gets the full set a few
  seconds later instead of an immediate error. It applies it, and on a relay
  forwards on success (R-F3a), so the next hop is served in the same round.
- The rebuild per hop drops from about 30 s to the upstream round trip plus
  the worker, usually under 2 s.

**Interaction with #122 (the gossip send-queue cap, ch.28 §28.9B RQ1).** None
directly. Sync answers travel on the request-response protocol, not in the
gossipsub send queue, so the cap neither drops nor delays them. A parked answer
costs an open inbound stream, not send-queue space.

**Where in the code.**
- A per-(pubkey, pair) "own sync in flight" record, set before
  `request_one_peer` in the keep-alive path (`process_orders_keep_alive`,
  ordermatch_orderbook.rs) and completed after the answer's trie ops are queued
  and applied. The
  applied-batch counter of R-T3b gives "applied".
- A parked-request table in `OrdermatchContext`.
- `process_peer_request` returns a "parked" outcome for
  `SyncPubkeyOrderbookState`, so `process_p2p_request` does not answer.
- The completion hook and the timer answer, using the normal answer builder.

**Switch.** `sync_answer_deferral_ms`, where 0 gives the reference.
- **Default-side rule:** arm 3. It changes what other nodes see, an answer
  instead of a fast error, and is wire-compatible. Proposed default: Reloaded
  (5000).

## (b) Short requester retry (not recommended now)

**What it would do.** After a failed keep-alive sync, our node would ask the
same peer once more after 2–5 s with jitter, but only for the race classes:
- a placeholder root;
- a missing pair.

It would not retry after a timeout or "no data", which point to a real
problem rather than a race.

**Why not now.**
- It departs from ch.32 R-Q6 ("no retry").
- It adds one request per failure.
- With (a) on our relays it is mostly redundant.
- It cannot help reference subscribers.
- Its only gain is against reference relays.

## (d) Keep-alive round after a relay (re)connect

This is owned by `docs/plans/maker-gossip-rate-control.md` (Decision AO,
tnn-miner-1c), item (c)4. It is maker-side, not relay-side, and it is not
designed or implemented here.

The two plans meet at one point: (d) resets the 90 s expiry at the relay right
after a reconnect, and (c) keeps it reset while updates flow.

## Acceptance scenario (the lab timeline, in-process first, then the swaplab)

Setup: a maker with 600 orders on K pairs, one relay (ours), and three
subscribers (ours, then reference in the swaplab).

| # | Scenario | Today | With (c) | With (a) |
|---|---|---|---|---|
| S1 | The maker's keep-alives to the relay are dropped for 150 s while its updates keep arriving | The relay wipes P at 90 s, subscribers fail, and the rebuild takes 30–60 s after keep-alives resume | **No wipe.** The relay's book stays equal to the maker's throughout. | — |
| S2 | The whole stream from maker to relay stalls for 150 s, then resumes | Wipe at 90 s (parity). After resumption, subscribers fail at the first keep-alive and succeed at the next, 30 s later. | Wipe at 90 s (parity) | Subscribers' first requests are **answered within 5 s** of the relay's own sync. The book converges at every subscriber within one round trip plus the worker after the first post-stall keep-alive. |
| S3 | A dead maker: everything stops | Wipe at 90 s | Wipe at 90 s | — |
| S4 | The relay's own upstream sync fails | Subscribers fail | — | Subscribers get the same error, after at most 5 s. Nothing hangs. |

Pass criteria:
- S1: no eviction of P on the relay for the whole 150 s.
- S2: all three subscribers converge within 5 s of the relay's sync after the
  first post-stall keep-alive.
- S3: eviction at 90 s ± one sweep.
- S4: the error arrives within the bound, and the parked table is empty
  afterwards.

## CRD rules to add (Reloaded-original, no forbidden-class input)

**ch.59:**
- R59.6.1a: order messages as liveness, with its switch.
- A divergence note: the reference uses keep-alives only; the replay remark.
- Test items for S1 and S3.

**ch.32:**
- R-Q5b: deferral (the deferrable condition, the bound, the limits, the
  outcome equal to R-Q5).
- An amendment to R-T3b item 3: a parked answer is not a wait on the inbound
  path.
- A divergence note: the reference fails at once.
- Test items for S2 and S4.
- A pointer from R-Q6 and R-F3a to R-Q5b.

**Not needed:** no Dirty Gate, since every input is our own code, the gated
chapters as merged, and black-box lab observations.

## Tests

1. (c), unit: an order message for a known P refreshes liveness; one for an
   unknown P creates nothing; a P silent for 90 s is evicted; a P sending only
   updates for 150 s is not; with the switch off, today's behaviour.
2. (a), unit:
   - a deferrable request is parked and answered after our sync is applied;
   - our sync fails, and the parked request gets the error;
   - the bound expires, and the parked request gets the error;
   - each non-deferrable case is answered at once;
   - a multi-pair request with one non-deferrable pair fails at once;
   - the per-peer and total caps hold;
   - the inbound dispatcher returns without waiting (timing assertion);
   - the parked table is empty after every path.
3. (a), in-process with the measurement harness of
   `ordermatch_tests::keep_alive_sync_measurement`: a chain of maker, relay
   and subscriber in which the relay is wiped, then S2. The subscriber
   converges within the bound.
4. S1 to S4 in the swaplab with reference subscribers, before and after.

## Work packages, once decided

1. CRD: ch.59 R59.6.1a, and ch.32 R-Q5b plus the R-T3b item 3 amendment, with
   the divergence notes and test items. This is a clean-room edit with no gate.
2. Code: (c), then (a), as separate commits, each with its switch, a
   `GLEEC_COMPATIBILITY.md` row and a plain-words CHANGELOG entry.
3. Tests 1 to 3.
4. A swaplab run of S1 to S4, before and after.
