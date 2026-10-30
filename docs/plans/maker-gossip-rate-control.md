# Plan: maker-side gossip rate control

**Status:** in progress — **Last reviewed:** 2026-10-28

> **Context.** A maker that changes its orders faster than its relay can take
> them no longer gets its keep-alives through. After 90 s without a keep-alive,
> every node drops the maker's whole order book. This includes the reference
> nodes and ours (`MAKER_ORDER_TIMEOUT` = 3 × the 30 s keep-alive interval,
> `lp_ordermatch.rs:130`).
>
> The send-queue cap (CRD ch.28 §28.9B RQ1) bounds memory but cannot fix this.
> Measured in-process: 90 updates/s toward a reader taking about 1 message/s
> delivered **0 of 10 keep-alives** with the old queue and with the capped one.
> See `docs/plans/gossip-send-queue-bound.md`, "Follow-ups (Decision AO)".
>
> This plan designs the fix chosen there as the primary option: the maker
> limits what it publishes. A priority lane is covered as a secondary option.
> It is a Reloaded-original design.

## Decision (AO, 2026-10-28)

The maintainer adopted option A as written below:
- the outbox with merging;
- cancellations and swap-negotiation messages bypass the budget;
- a ceiling of 10 messages/s per pubkey with a burst of 100, halving under
  pressure down to 0.5/s, then a slow recovery;
- degraded mode stops pushing and lets the keep-alive synchronization pull;
- each keep-alive is sent after its pair's pending updates;
- the reconnect round;
- the ping safeguard unchanged.

The exact-parity switch `maker_update_coalescing` is kept because it costs
one branch.

The maintainer then chose **option C: A plus the priority lane of (f)**,
for cancellations and swap-negotiation messages only, never for
keep-alives. It is marked by the application, on publish and on relay
propagation, and capped at 256 RPCs per connection; past that cap the
connection is closed.

The change also adds aggregate send-queue and outbox metrics (R-MP5).

The binding rules are CRD ch.32 §32.9B (R-MP1–R-MP5, T-MP1–T-MP13) and
ch.28 §28.9B RQ1 items 8 and 9 (T-RQ1.8, T-RQ1.9).

**Amendment (2026-10-28, direction (a)).** The first in-process run of
T-MP8 failed: no keep-alive got through during the load. After measuring
the candidates, the maintainer adopted three changes:
- a keep-alive waits at most 10 s for its pairs' pending updates, and
  that wait respects the budget;
- two signals replace the halving under queue pressure: the send queue
  toward a relay staying non-empty for 2 s, or the last connection to a
  relay closing while maker updates are in flight. Either drops the rate
  to the floor in degraded mode for 5 minutes;
- after that the rate climbs by 0.5/s per minute, and stops below the
  rate at which the relay last failed.

The two signals cover the two stall modes seen in the field: a relay that
stops reading, and one whose bandwidth is limited. The accepted cost is
one synchronization request per receiver, pair and keep-alive while
degraded. The measurements and the residual first-signal gap are in the
ch.32 rationale, and the open recovery question is ch.32 D7.

## The one-page summary for the maintainer

**Problem.** Nothing on our side of the connection can get a message past a peer
that reads slowly. Once its window is full of our earlier messages, the next
keep-alive waits behind all of them. The only fix is not to fill the pipe.

**Options.**

| | What it does | Helps when our writer is slow | Helps when the relay is slow | Visible to peers |
|---|---|---|---|---|
| **A. Coalesce + budget** (recommended) | Per order, the latest value wins. Updates leave at a bounded rate that adapts to how fast the relay drains. Keep-alives and cancellations are never held back. | yes | yes | Peers see an order's latest value, sometimes a few seconds later. They may not see every intermediate value. |
| **B. Priority lane only** | Cancellations (and, possibly, keep-alives) jump ahead of updates in our send queue. | yes | **no** | None, but a keep-alive that jumps ahead of its own pair's updates causes a sync request from every receiver (below). |
| **C. Both** | A, plus a lane for cancellations and swap-negotiation messages | yes | yes | as A |
| **D. Nothing** | Rely on the 60 s close and the ping safeguard to force reconnects | partly | partly | The book is dropped and rebuilt when the stall lasts long enough. |

**Recommendation: A**, with three parts:
1. Per-order coalescing, where a cancellation is never coalesced away.
2. An adaptive budget: a fixed ceiling, lowered by a backpressure signal from the send queue.
3. Each pair's keep-alive sent right after that pair's pending updates.

Add the cancellation lane of C later, only if a measurement shows it is needed.
No compatibility switch. The ceiling is an `MM2.json` setting with a
conservative default.

**Decisions needed:**
1. A versus C versus D.
2. The default ceiling: proposed 10 messages/s per maker pubkey, with a burst
   of 100.
3. The reconnect keep-alive round.
4. That the ping safeguard stays as it is.

## (a) Coalescing per order

The three maker messages that carry order state go through one new **outbox**
per maker pubkey instead of straight to `broadcast_ordermatch_message`:
- `maker_order_created_p2p_notify` (`ordermatch_trading.rs:108`);
- `maker_order_updated_p2p_notify` (`:248`);
- `maker_order_cancelled_p2p_notify` (`:264`).

The re-announce of own orders missing from the index (the 0.777 s loop,
`:722`) uses the outbox too.

The outbox holds **at most one pending message per order**, keyed by uuid. A
new message merges with the pending one:

| Pending | New | Result |
|---|---|---|
| none | any | the new message |
| Created | Updated | Created, carrying the updated values. Peers never see the intermediate state. |
| Updated | Updated | Updated, merged field by field: the latest value of each field set in either |
| Created or Updated | Cancelled | **Cancelled**. The pending message is discarded. |
| Cancelled | anything | Cancelled stays. A cancellation is terminal and never coalesced away. |

Rules:
- **Cancellations are not budgeted.** They leave at once, ahead of the budget
  queue. A lost or delayed cancellation leaves a phantom order that takers try
  to match.
- **Swap-negotiation messages are untouched:** TakerRequest, MakerReserved,
  TakerConnect and MakerConnected. They are time-critical, one per swap, and
  never coalesced.
- **The local state is not delayed.** The maker's own order, its trie and the
  RPC result are updated at once, as today. Only the gossip message waits.
- **Order of flushing:** oldest pending order first, so no order starves.

## (b) The flush budget

**What a relay drains: measured and observed.**

| Situation | Drained |
|---|---|
| Lab, healthy host, one generator at about 190 updates/s | kept up; the seed's memory grew, but it stayed responsive |
| Lab, CPU-saturated host (2026-10-28) | 38, then 8 messages per minute (0.6 to 0.13 per s) from our maker |
| In-process, throttled reader | whatever the reader takes; at 90 updates/s published, every keep-alive was lost |

So the capacity ranges from about 0.1/s to over 190/s for the same software,
depending on host load. **A fixed budget alone cannot fit both ends.**

**Proposed: a fixed ceiling, adapted by backpressure.** The rate rises
additively and falls by halving.
- **Ceiling:** default 10 messages/s per maker pubkey, burst 100, configurable
  as `maker_update_rate_max` in `MM2.json`. At 10/s, a 600-order book refreshes
  fully in one minute.
- **Backpressure signal:** the send-queue handler already knows its data-queue
  depth and whether it is in a stuck period (RQ1 item 5). It reports a
  pressure level to the application when the depth crosses 256 RPCs, and again
  when it drops back under 64. The level is the highest over the connections to
  relays.
- **Adaptation:**
  - under pressure, halve the current rate every 5 s, down to 0.5/s;
  - after 30 s without pressure, add 1/s every 5 s, up to the ceiling.
- **Degraded mode:** while under pressure at the minimum rate, the maker stops
  pushing updates and relies on the keep-alive synchronization (pull). Each
  keep-alive whose root differs makes the relay request a delta from us,
  which paces itself to the relay's capacity.
  - Cost: subscribers that hold an older root request a sync from the relay.
    That is up to one request per subscriber per pair per 30 s, which is the
    amplifier the seed analysis found.
  - So degraded mode is a fallback, not the normal path.

**Alternative, fixed budget only:** simpler, with no handler signal. Rejected,
because no single value is both safe on a saturated host and useful on a
healthy one.

## (c) Keep-alive protection

1. **Never budgeted.** A keep-alive leaves on schedule whatever the budget
   state.
2. **Behind its own pair's updates.** For pair P, the keep-alive advertises the
   roots of the maker's *current* state. If updates for P were still pending,
   receivers would see a root they cannot reach, and every one of them would
   request a sync. So, before P's keep-alive:
   - flush P's pending outbox entries;
   - then send the keep-alive.

   This flush bypasses the budget. That is bounded: at most one message per
   order on P per 30 s, so a 600-order book adds at most 20 messages/s.
   - In degraded mode the flush is skipped, and the pull path of (b) repairs
     the difference.
3. **Not in a priority lane.** For the same reason as item 2, a keep-alive that
   overtakes its pair's updates triggers sync requests from every receiver.
4. **Lost in the transport window.** The in-process run showed that whatever
   sits in the window when a connection closes is lost. That includes a
   keep-alive sent just before the close. The 60 s close of RQ1 or the ping
   safeguard can both cause this.
   - **Proposed: a keep-alive round after (re)connecting to a relay.** Shortly
     after a relay connection is established and the relay mesh includes it,
     send one keep-alive per pair at once. Each one is preceded by its pair's
     pending flush.
   - Cost: K messages per reconnect.
   - Benefit: it resets the 90 s expiry at the relay before the book is
     dropped, and it starts the relay's sync of anything lost in the window.
     Since the budget keeps the new connection's pipe clear, this round
     arrives within seconds.
   - **It is worth it.** Today the next keep-alive can be up to 30 s away, and
     the 90 s expiry may already be close.
5. **Related, not part of this plan.** Decision AN (planned, seed-growth work)
   lets a signed order message refresh the maker's liveness on Reloaded
   receivers (ch.59). That keeps a maker whose updates still arrive from being
   dropped while its keep-alives queue. It complements this plan; it does not
   replace it, because other nodes still expire on keep-alives alone.

## (d) The ping safeguard

The in-process run showed our ping safeguard (`mm2_p2p/src/adex_ping.rs`, 2
consecutive failures, ping interval 15 s, timeout 20 s) closing a slow reader's
connection after about 45 s. That is earlier than RQ1's 60 s rule. The failures
were "protocol negotiation timed out": our new ping substream sat in the same
pipe behind the backlog. The maker's own log from the lab run shows the same
signature.

**Recommendation: leave the ping safeguard as it is.**
- **Lengthening it** would only delay the one recovery that works today, a
  reconnect, and would hide peers that are really gone.
- **Making the close conditional on the queue state** adds coupling for little
  gain. In both cases (a dead peer, or a pipe we saturated ourselves) closing
  is the right action.
- **With (a) to (c) in place,** the pipe does not saturate, so pings keep
  working and the safeguard no longer fires on a merely slow relay.

Revisit only if a field run shows ping closes under normal load.

## (e) What changes, rules, compatibility, tests

**Visible to peers.**
- An update may arrive later than today, by up to about 1/rate seconds. Under
  pressure, it arrives at the next keep-alive synchronization.
- Intermediate values of a quickly changing order may never be published.
- A cancellation is not delayed.
- After a reconnect, an extra keep-alive round goes out.
- The message formats are unchanged.

**Visible to the operator.**
- The RPC results of `setprice`, `update_maker_order` and `cancel_order` are
  unchanged and immediate.
- A log line when the rate is lowered and when it recovers, at most one per
  minute.
- The new `maker_update_rate_max` setting.

**CRD rules (Reloaded-original).**
- **ch.32:**
  - a new rule for the maker outbox: coalescing table, cancellation and
    swap-message exemptions, flush order;
  - a new rule for keep-alive ordering: flush the pair, then the keep-alive;
    never budgeted; the reconnect round;
  - a divergence note: the reference publishes every update at once.
- **ch.28:** an amendment to §28.9B: the send-queue handler reports a pressure
  level (thresholds 256 / 64) to the application.
- The budget's adaptation rule goes with the outbox rule in ch.32.

**Compatibility switch.** Under the default-side rule, other nodes see later
and fewer updates, so this is arm 3, a team decision.
- Recommendation: no compatibility switch. Setting `maker_update_rate_max` very
  high gives nearly the reference behavior on a healthy network, and the
  backpressure still protects a saturated one.
- If the maintainer wants exact parity available, the switch would be
  `maker_update_coalescing: false`, which bypasses the outbox entirely.

**Tests.**
1. The coalescing table, each row (unit).
2. A cancellation is never coalesced away and never delayed by the budget
   (unit, and in-process).
3. Swap-negotiation messages bypass the outbox (unit).
4. The budget: the ceiling and burst are respected; halving under pressure;
   recovery after 30 s without pressure (unit, with an injected clock).
5. The pressure signal: crossing 256 and back under 64, reported once per
   crossing (vendored handler unit test).
6. Keep-alive ordering: the pair's pending updates leave before its keep-alive,
   and a receiver that applies them in order reaches the advertised root
   without a sync request (in-process).
7. Reconnect round: after a forced close, one keep-alive per pair leaves within
   a few seconds of the relay rejoining (in-process).
8. **The lab scenario in-process.** 90 updates/s from 600 orders toward a reader
   at about 1 message/s, as in `keepalive_backlog.rs` (kept in
   `local/notes/gossip-send-queue/`). Every keep-alive arrives within 30 s, no
   gap exceeds 90 s, and the reader's book converges to the maker's.
9. A healthy relay: with the rate below the ceiling, every update is delivered
   at once, with no coalescing visible.

## (f) Secondary option: priority lane

If the maintainer wants it, in addition to or instead of A:
- **Placement:** a lane in the send-queue handler, ahead of the data queue.
  It is never dropped and is capped by count only, for example 256. Past that
  cap the connection is treated as stuck.
- **Marking:** gossipsub cannot tell message kinds apart, because they share
  the pair topic. The application marks priority on `PublishMsg`, and on the
  propagate command when our node relays.
- **Contents:** cancellations and the four swap-negotiation messages.
  **Not keep-alives,** because of (c) item 2.
- **Limitation:** it helps only when our own writer is the bottleneck. Toward
  a slow reader, the window already holds earlier messages, and nothing on our
  side can move ahead of them.
- **Size:** small in the handler. A new field on the publish and propagate
  commands, plus the classification at their call sites.

## Work packages, once decided

1. CRD: the ch.32 rules and the ch.28 §28.9B amendment.
2. Code, through the Coder:
   - the outbox and the budget in `mm2_main` ordermatch;
   - the pressure event in the vendored gossipsub handler and behaviour;
   - forwarding it through `mm2_p2p` to the application;
   - the reconnect round.
3. Tests 1–9.
4. A swaplab run on the lab setup of 2026-10-28, before and after.
5. CHANGELOG, in plain words.

## Deferred work

Recorded 2026-10-29, after the lab runs of the merged design. None of these is
bound by the CRD yet. The default ceiling and the climb step were measured and
decided the same day (CRD ch.32 D8: 20 per second, +0.5 per second every
30 seconds) and are not deferred.

**Opt-in since 2026-10-29.** After a field run in a mesh with reference relays
and makers, the outbox was made opt-in: `maker_update_coalescing` defaults to
`false`, and only `true` turns R-MP1 and R-MP2 on (CRD ch.32 D9). The items
listed there come before the outbox becomes the default again: a keep-alive
that advertises only released state, a close signal that counts only error
closes, and a synchronization request addressed to the keep-alive's author.

- **A budget that reads the TCP send queue.** Toward a relay whose bandwidth is
  limited, the backlog first builds in the kernel's send queue, below the
  handler queue that the queue signal watches. A signal from the send-queue
  depth would fire earlier in that mode. It needs a per-connection reading of
  the socket's send queue, which the transport does not expose today.
- **A slow start per new relay connection.** The first signal comes only after
  the load has started, so a burst can still cost one expiry of the maker's
  orders per new relay connection (CRD ch.32 D7, the part still open). A low
  starting rate for each new connection would close that gap. It was not
  chosen on 2026-10-28; decide after more field data.
