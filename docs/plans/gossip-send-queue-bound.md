# Plan: bound the per-peer gossip send queue

**Status:** in progress — **Last reviewed:** 2026-10-28

> **Context.** The seed-node memory incident of 2026-10-28 prompted a survey of
> the queues in our gossip layer. Toward a single peer, our node queues every
> gossip message it sends, with no limit, until that peer reads it. This plan
> caps that queue. It is a Reloaded-original change; the reference leaves this
> queue unbounded. The binding rule is CRD ch.28 §28.9B RQ1. The policy was
> approved as proposed (decision AB, 2026-10-28).
>
> **Scope.** The cap protects a node against a peer that reads nothing, or reads
> more slowly than messages arrive. It does **not** explain or close the
> incident's growth: the lab saw no growth of this kind toward a throttled or
> paused peer, and it measured about 1.1–1.5 KB retained per received order
> update, held flat for 12 minutes after the load stopped. That points to
> retained state, which is a separate item.

## What was measured

An in-process test (`mm2src/mm2_p2p/tests/gossip_send_queue.rs`): the sender
is our production node, and the receivers use the same transport stack and
gossipsub configuration. It runs over the in-memory transport and counts heap
use with its own allocator.

| Finding | Evidence |
|---|---|
| **The per-peer queue has no bound.** With the receiver paused, the sender's heap grows linearly with the messages sent, and nothing is dropped. | 20,000 × 8 KiB: 366 MiB at the peak; all 20,000 delivered after resume. |
| **The memory sits in the connection handler's send queue** (`send_queue` in `vendor-patches/libp2p-gossipsub-0.45.0/src/handler.rs`). The behaviour's event queue is only transient. | Dropping the receiver frees the memory. The heap after the swarm goes quiet equals the heap right after publishing. |
| **Cost per queued message:** the payload plus about 0.4 KB. Each peer holds its own copy. A message published to K topics is K messages and K copies. | Scenarios C and D. |
| **The growth is per slow peer.** Healthy peers next to a paused one received everything. | 3 receivers, 1 or 2 of them paused. |
| **A slow reader behaves like a paused one, only more slowly.** | A reader at half the send rate had a backlog of 17,047 after 20 s. The queue took 19 s to empty. |
| **The queue sends newest first.** The handler pops from the end. Under a backlog, a peer receives messages in reverse order. | After resume, 19,985 of 19,999 adjacent pairs arrived in descending order. |
| **The ping safeguard covers only a peer that stops answering.** It drops such a peer after about 45 s. A peer that answers pings but does not read gossip is never dropped. | Scenario F. |
| **Emptying the queue does not make the process small again.** | After the drain, RSS stayed 85–118 MiB above its starting level for the 70–90 s observed, even after `malloc_trim`. |
| **Gossipsub keeps capacity after a burst.** About 1.3 KB per message at the burst's high-water mark is retained, independent of payload size and of whether the peer was healthy. It does not grow on repeated bursts of equal size. | 2,500 → 6.15 MiB, again 6.15 MiB after a second equal burst; 20,000 → 42.4 MiB. |

**Which cause the measurement supports.** The send queue is a real risk of its
own: a peer that reads nothing can grow a node's memory without limit. It is
not the incident's growth. That growth fits retained state:
- the lab's per-update retention, held flat;
- the two retention effects above, gossipsub capacity and allocator RSS, which
  also keep a node looking big long after any queue is empty.

The lab's post-stop decay curve can tell which kind of retention it is. Retention
that stays flat across repeated equal bursts is capacity; retention that grows
with each burst is state.

**Why the reverse order matters for the orderbook.** A received order update
is applied as it arrives, with no check of its age
(`process_maker_order_updated` → `apply_updated`). If a backlog reverses two
updates of one order, the peer keeps the older one until the keep-alive
synchronization repairs it. If it reverses a creation and its update, the update
is ignored. Each such repair is one more synchronization request.

## Proposed policy

All of this sits in the gossipsub connection handler we already vendor
(`vendor-patches/libp2p-gossipsub-0.45.0`), next to the existing backoff patch.

1. **Cap per connection.** At most **2,048 queued data messages or 4 MiB of
   their encoded size**, whichever is reached first. Data messages are the RPCs
   that carry published or forwarded messages.
   - Control-only RPCs (subscribe, unsubscribe, GRAFT, PRUNE, IHAVE, IWANT,
     relay-mesh notices) are small, are never dropped, and do not count toward
     the cap.
   - Why these numbers: at the lab's extreme rate of about 190 messages/s,
     2,048 messages is a pause of about 10 s, so a healthy peer with a short
     stall never reaches the cap. 4 MiB limits one stuck peer to a few MiB.
     If 500 peers stalled at once, the total would be about 2 GB, rather than
     unbounded.
2. **Order: first in, first out.** Send the oldest queued message first. A
   healthy peer normally has at most one message queued, so it sees no
   difference. A peer with a short backlog gets its messages in the order they
   were sent, which fixes the reordering above.
3. **When the cap is reached: drop the oldest data message for that peer.**
   - Other peers are not affected.
   - The newest messages are worth the most to an orderbook. What the dropped
     ones carried is repaired by the next keep-alive synchronization, which
     names the held root since #110/#111.
4. **When the cap stays reached: close the connection.** If the queue has been
   at its cap continuously for **60 s** (two keep-alive intervals), close that
   peer's connection. A period at the cap starts with the first drop. It ends
   only when the queue has drained to half of both limits, so a peer that
   frees one slot at a time still counts as stuck.
   - This releases the queue completely.
   - It also covers a peer that answers pings but reads no gossip.
   - The peer can reconnect and resynchronize.
5. **Logging.**
   - The first drop toward a peer: one warning with the peer ID, queued count
     and queued bytes.
   - After that, at most one summary line per peer per 60 s, with the dropped
     count.
   - Closing a connection: one warning with the peer ID, how long it was full,
     and the dropped count.
   - Peer IDs are public, and no message content is logged.
6. **Configuration.** The three numbers (count, bytes, 60 s) are constants in
   the vendored crate, settable through its config builder so tests can use small
   values. No `MM2.json` setting is proposed for now.

### Alternatives considered

| Option | Why not recommended |
|---|---|
| **Drop the newest message when full** (rust-libp2p 0.49 does this) | Under first-in-first-out order the delivered sequence stays in order either way; the choice is only which messages are lost. Dropping the oldest loses stale values. Dropping the newest would lose the latest updates and leave the peer with stale orders until the keep-alive synchronization. |
| **Drop only, never disconnect** | It leaves a peer that pings but never reads connected for ever, holding a full queue. |
| **Disconnect at once when full** | A short stall on a slow mobile link would cost a reconnect and a full resynchronization. |
| **Drop messages after a waiting time** (rust-libp2p: 5 s for own messages, 1 s for forwarded) | It drops messages for a healthy peer that only stalled briefly, which changes what a healthy peer receives. |
| **Upgrade to libp2p 0.54 or later**, which bounds the queue itself | That means redoing the ch.28 migration and porting the relay-mesh extension (see the vendor patch's README). Too large before the beta. |

## Compatibility

- **Wire:** no change. No message format changes and nothing new is sent.
- **Reference divergence:** the reference leaves this queue unbounded.
- **Default-side rule:** the change affects other nodes only when they cannot
  keep up. Such a peer may miss messages, or be disconnected after 60 s at the
  cap, where the reference would let the node's memory grow until it fails.
  That is not squarely arm 1 or arm 2, so it is a project-team decision (arm
  3).
- **Recommendation:** Reloaded by default, with **no switch**. Restoring an
  unbounded queue has no legitimate use, and the cap never acts on a peer that
  keeps up.

## Work packages

1. **CRD:** a Reloaded-original rule in chapter 28 (the gossip transport),
   coordinated with the chapter 28/32 edits in progress (`crd/seed-growth`).
   It covers the cap, the order, the drop rule, the disconnect, the logging and
   the divergence note, plus test items.
2. **Code, through the Coder:**
   - the handler queue becomes a FIFO with count and byte accounting;
   - the drop rule and the 60 s disconnect;
   - the rate-limited logging;
   - the vendor patch README gains a section for the change.
3. **Tests**, building on the measurement harness:
   1. A healthy peer receives every message of a 1,000-message burst, in order,
      with no drop logged.
   2. A paused peer: the sender's heap stops growing at the cap, the healthy
      peers still receive everything, and after resume the paused peer gets the
      newest messages.
   3. The byte cap acts first with 8 KiB payloads.
   4. Control messages are never dropped: a subscription sent while the cap is
      reached still arrives.
   5. A peer at the cap for the full time is disconnected (with a short test
      value), and one that drains in time is not.
   6. The order after a backlog is the order sent.
   7. The drop warning appears once, then at most once per interval.
4. **CHANGELOG:** in plain words, a peer that cannot keep up no longer makes a
   node's memory grow without limit; it may miss gossip messages, and after a
   minute at the limit it is disconnected.

## Follow-ups, not part of this plan

- **Gossipsub capacity retention:** find which structure keeps its high-water
  capacity (candidates: the message cache, the duplicate and time caches, the
  event queue), and shrink it after quiet periods.
- **Allocator retention:** decide whether a long-running seed should return
  freed memory, for example a periodic `malloc_trim` on Linux, or another
  allocator. That is its own decision.
- **Retained state, the incident's main suspect:** the trie store, the delta
  history and per-message tasks are tracked with the seed-growth work.

## Follow-ups (Decision AO, pending)

**Problem.** A lab run on 2026-10-28 put our maker at more than 90 order
updates/s on a CPU-saturated host. The traffic from the maker to the seed
nearly stopped, and the maker's keep-alives no longer arrived. After 90 s
without a keep-alive, the reference nodes dropped the maker's whole order book.
It came back only after a reconnect.

**Model.** A discrete model of one connection, run over 600 s: 90 updates/s plus
K keep-alives every 30 s (K = 1 or 20), toward a reader taking 1, 0.63 or
0.13 messages/s. It was run in two cases:
- the writer is the bottleneck, so the multiplexer window is empty;
- the reader is the bottleneck, so the window is full with about 256 earlier
  messages.

An in-process measurement against our production node is to confirm these
numbers.

| Queue discipline | Keep-alives delivered |
|---|---|
| Newest first, unbounded (dev before this plan) | 0 in every case. Each keep-alive is buried by the next update within 11 ms. |
| Oldest first, unbounded (the GPLv2 baseline's order) | latency grows without limit, about 450 s, so effectively none within 90 s |
| This plan: oldest first, capped, drop the oldest | at most 2 of 400. At the front of the queue a keep-alive is dropped by an incoming update far more often than it is sent. The 60 s close does not help under a constant slow reader, because the queue refills about 23 s after reconnecting. |
| Priority lane for keep-alives | writer-limited: 20 of 20 at 1–2 s (K = 1), 400 of 400 at about 11 s (K = 20). Reader-limited: 256–430 s latency, because the keep-alive still waits behind the window of earlier messages the reader has not taken. |

This plan does not get keep-alives through such a backlog. It bounds memory, and
its 60 s close forces a reconnect, which in the lab is what restored the flow.

**Where newest-first came from.** It is ours. The GPLv2 baseline's in-tree
gossipsub sent oldest first. Newest first arrived with the libp2p modernization
of chapter 28: the vendored crate matches gossipsub 0.45.0, whose handler pops
from the end. Whether the reference's gossipsub shares it is unverified.

**Options for the maintainer:**

1. **Priority lane for keep-alives and cancellations.**
   - The application marks the priority, both when it publishes and when it
     propagates a relayed message. Gossipsub cannot tell a keep-alive from an
     update, because they share the pair's topic.
   - The handler keeps a separate lane ahead of the data queue. It is never
     dropped and is capped by count only.
   - Cancellations belong there because a lost cancellation leaves a phantom
     order.
   - Limitation: it helps only when our writer is the bottleneck. When the
     reader is slow, earlier messages already fill its window, and nothing on
     our side can move ahead of them.
2. **Maker-side rate control.**
   - Coalesce per order: the latest value wins, keyed by the order's uuid.
   - Flush at a bounded rate. The rate comes either from a backpressure signal
     out of the handler, such as the data queue's depth or the stuck flag, or
     from a fixed budget per pubkey.
   - This works in both cases, because it never puts more into the connection
     than drains.
   - It changes maker semantics: peers see an order's latest value later, and
     not every intermediate value.

How to tell the two cases apart in the lab: during a stall, look at the send
queue of the maker-to-seed socket. A full send queue means the reader is the
bottleneck; an empty one means our writer is.
