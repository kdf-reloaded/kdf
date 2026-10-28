# Chapter 59 — Legacy Order-Match Negotiation Loop and Dead-Order Defences

**Status:** driving-spec.

> **One-sentence claim:** the project shall negotiate a legacy (V1) match
> over the four-message `TakerRequest` → `MakerReserved` → `TakerConnect`
> → `MakerConnected` exchange with the bound selection, timeout and
> peer-penalty semantics specified here — a taker that collects every
> reservation for a short window and connects to the cheapest viable one,
> a maker that reserves at most once per taker request, a one-hour
> penalty on a counterparty whose negotiation fails, and a liveness
> record kept on the receiver's own clock — so that a maker that has gone
> away cannot repeatedly absorb a taker's attempts to trade.

## 59.0 Executive Summary

Before this chapter the document set covered the order-match surface only
in pieces: [Chapter 11](11-order-match-cancellation.md) binds the
cancellation-resurrection guard, [Chapter 12](12-order-match-state-store.md)
the maker-order store, [Chapter 13](13-swap-version-negotiation.md) the
version tag the first two messages carry, and
[Chapter 32](32-orderbook-p2p-and-trie.md) the orderbook trie and its
peer-to-peer surface. The negotiation loop itself — who answers whom, in
what order, with what deadline, and what happens when the other side goes
silent — had no chapter. This chapter is that chapter, scoped to the
legacy (V1) loop.

Its practical subject is a class of user-visible failure reported from
the field: an orderbook entry whose maker is gone still absorbs a taker's
trade attempt. Three distinct mechanisms bear on that, and the chapter
separates them deliberately because they fail independently:

1. **Selection** (§59.3) — whether the taker, having asked the whole
   mesh, picks well among the makers that answered.
2. **Penalty** (§59.5) — whether a maker that answers and then fails to
   complete the handshake is kept out of the taker's way afterwards.
3. **Liveness** (§59.6) — how a node decides an absent peer's rows are
   stale, and whose clock that decision runs on.

## 59.1 Reference designations

| Designation | Role |
| --- | --- |
| `v2.6.0-beta` | The legacy behavioural reference for netid `8762`. Every rule below holds at this designation. |
| `v3.0.0-beta` | The newest public release of the v3 lineage; unchanged from `v2.6.0-beta` for everything this chapter binds. |

The penalty extension of R59.5.2 — the field report's "bad maker" fix —
first appears at `v2.6.0-beta`. The record is no more precise than that
designation.

## 59.2 Subsystem Shape

The loop runs entirely over the pair's orderbook gossip topic
([Chapter 32](32-orderbook-p2p-and-trie.md) §32.2) and carries four
message kinds:

| Message | Sender | Meaning |
| --- | --- | --- |
| `TakerRequest` | taker | "I want to trade this volume of this pair at this price or better." Broadcast, not addressed. |
| `MakerReserved` | maker | "I will honour it at these exact amounts, against this maker order." Addressed to the taker's public key. |
| `TakerConnect` | taker | "I accept your reservation." Addressed to that one maker. |
| `MakerConnected` | maker | "Confirmed; the swap is starting." Addressed to the taker. |

Both sides keep a per-order map of in-flight *matches* keyed by the other
side's order identifier, each carrying the messages seen so far and a
last-updated instant. A match that reaches `MakerConnected` becomes a
running swap and leaves the negotiation surface.

Every incoming message of all four kinds is subject to the peer-penalty
filter of R59.5.1 before any of the rules below see it.

## 59.3 Bound taker-side selection contract

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §59.11, and are not restated here.

**R59.3.1** *Collect before choosing.* A taker shall not act on the first
`MakerReserved` it receives. On receiving one for a live taker order of
its own it shall append the message to a per-taker-order pending
collection and, **only if it is the first message in that collection**,
start a single deferred selection pass; a later message that finds the
collection non-empty is appended and nothing more is started. The
deferred pass shall begin after a fixed delay of **3 seconds**.

**R59.3.2** *Price-ascending selection.* The deferred pass shall take the
whole pending collection for that taker order, sort it by the maker's
quoted price in **ascending** order, and walk it in that order. It shall
connect to the **first** entry that satisfies all of:

- the reservation's amounts match the taker order's own request;
- the taker order's match map is **empty**;
- both coins accept the counterparty's protocol information for the
  negotiated lock time
  ([Chapter 13](13-swap-version-negotiation.md), §59.4).

On connecting it shall broadcast `TakerConnect` addressed to that maker,
insert a match into the taker order's match map keyed by the maker's
order identifier, persist the updated taker order, emit the
order-status stream event, and **stop walking**. If no entry satisfies
the conditions, no connect is sent and the collection is discarded.

**R59.3.3** *Single-shot by construction.* The empty-match-map condition
of R59.3.2 makes the loop single-shot: once any match is inserted for a
taker order, no later `MakerReserved` for that order can be acted on,
within the same pass or in any later one. This is the bound contract and
**not** a defect to be fixed: the taker has committed its funds
accounting to one maker at that point
([Chapter 51](51-legacy-v1-swap-state-machine.md) §51.8), and a second
connect would reserve the same volume twice.

**R59.3.4** *No retry against the next maker.* When the chosen maker
never answers the `TakerConnect`, the taker shall **not** fall back to
the next-best reservation. The taker order instead ages out under
R59.4.2. Reloaded behaves this way and so does the reference; this is
therefore **parity**, not a gap, and a Reloaded retry would be a
divergence needing a `docs/COMPAT_SWITCHES.md` entry. The mitigation the
reference actually applies to the same problem is the penalty of
R59.5.2, not a retry.

> **Operator consequence, stated plainly because it is what a user
> sees.** A taker whose chosen maker vanishes loses up to the taker-order
> timeout of R59.4.1 and must issue the order again. The second attempt
> is the one that succeeds, because by then the vanished maker is under
> the R59.5.2 penalty and its reservation is dropped before selection.
> That two-attempt shape is the reference's behaviour, not a reloaded
> defect.

## 59.4 Bound timeout and maker-side contract

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §59.11, and are not restated here.

**R59.4.1** *Taker-order timeout.* A taker order shall carry a timeout of
**30 seconds** from its creation. The periodic order-match maintenance
pass shall, for each taker order past that deadline:

- if the order's match map is **non-empty**, or its type is not
  good-till-cancelled: delete the order with a timed-out cancellation
  reason. The partially negotiated match is abandoned, not retried;
- otherwise (no match and good-till-cancelled): delete it with a
  converted-to-maker cancellation reason, create the corresponding maker
  order, persist it, mark the order's history row as having been a taker
  order, and broadcast the maker-order-created notification.

**R59.4.2** *No taker-side match pruning.* There is deliberately **no**
pass that prunes an individual stale match from a taker order's match
map. A taker order with a stuck match reaches R59.4.1's first arm and the
whole order is cancelled. A Coder shall not add taker-side match pruning
as an "obvious" symmetry with R59.4.4: doing so would re-open the order
to a second connect and defeat R59.3.3.

**R59.4.3** *Maker reservation contract.* On a `TakerRequest` a maker
shall walk its own live maker orders in its store order and, for the
**first** order that matches the request, reserve once and return —
whether or not the reservation is actually sent. It shall reserve only if
all of:

- the request's maker-public-key constraint, if set, names this maker;
- the request's order-identifier constraint, if set, names this order;
- the order's match map does **not** already contain this taker
  request's identifier;
- both coins accept the taker's protocol information for the maker's own
  lock duration (§59.4.5).

The reservation names exact base and rel amounts computed from the
order's price and the request's volume, carries the maker's confirmation
and notarisation settings (filled in from the two coins' own
requirements when the order sets none), carries both coins' protocol
information, and carries the order's negotiated swap version
([Chapter 13](13-swap-version-negotiation.md)). The maker then inserts a
match keyed by the taker request's identifier, persists the order, and
emits the order-status stream event.

> **Pointer (informative; Spec Reader 2026-10-25, cleared by the Dirty
> Gate with chapter 60 on 2026-10-25).** "Matches the request" includes a volume test: the matched
> base volume must be at least the order's minimum base volume and at most
> its available volume, at a price no worse than the order's. A request
> failing it gets no message at all
> ([Chapter 60](60-trading-floors-and-refusal-contract.md) R60.4.3,
> R60.5.2).

**R59.4.4** *Maker-side match pruning.* The periodic maintenance pass
shall drop from each maker order's match map every match whose
last-updated instant is older than **30 seconds**, **unless** that match
has already reached `MakerConnected`. Dropping a match releases the
volume it reserved. A connected match is never dropped by this pass; it
belongs to a running swap.

**R59.4.5** *Lock-duration asymmetry.* The two sides compute the lock
duration they check protocol support against differently, and both
computations are bound: the taker checks against the negotiated atomic
lock time for the pair; the maker checks against that same value scaled
by its coin's maker lock-time multiplier and rounded **up**. The maker
additionally passes the matched base amount into the base coin's support
check and a maker-side flag that the taker does not set. These are not
interchangeable: using the taker's value on the maker side would accept
a protocol the maker cannot actually honour for the longer maker leg.

> **Code-quality finding (informative).** Reloaded's coin
> protocol-support check takes neither an amount, nor a lock duration,
> nor the maker-side flag — it is a one-argument check on the protocol
> information alone, at every one of the four call sites R59.3.2 and
> R59.4.3 name. Reloaded therefore cannot implement R59.4.5 at all, and
> cannot refuse a match on the grounds the reference refuses it on (a
> counterparty protocol that is unsupported *for this amount* or *for
> this lock duration*). This is a distinct parity gap from anything else
> in this chapter; it belongs to the coin-trait surface rather than to
> the negotiation loop, and it is recorded as D1 rather than bound here.
> *(Trigger: contradiction apparent between the reference's call shape
> and reloaded's, seen side by side while establishing R59.3.2.)*

**R59.4.6** *An update that changes nothing is not broadcast.*
*Reloaded-original (2026-10-28); no forbidden-class input; outside the
clearance of this section's reference-sourced material.* When an
operator's update of one of the node's own maker orders (the
`update_maker_order` request) leaves the order's price, maximum volume,
minimum volume and confirmation settings all as they were, with amounts
compared as numbers, the node MUST NOT broadcast a maker-order updated
message for it. The update is otherwise handled as before: it is
validated, the order is saved, and the response is the same. Updates the
node makes itself (a volume cut after a balance change, the remaining
volume after a match) always change the volume and are not affected.

Rationale: such a message changes no peer's order set and no trie root
([Chapter 32](32-orderbook-p2p-and-trie.md) §32.6), yet every relay
forwards it to every subscriber of the pair. A maker driven by a bot that
re-submits all of its orders on every refresh multiplies the gossip load
for nothing. Observable effect: fewer messages, and no change to any
peer's state. A peer that missed an earlier update is no longer repaired
in passing by a later re-send that changes nothing; repair stays with the
keep-alive synchronization of ch. 32 §32.7.3.

The top-level `MM2.json` boolean `suppress_unchanged_order_updates`
(default `true`) selects the rule; only a JSON `false` turns it off, and
every update is then broadcast.

> **Compatibility:** to broadcast every update, as the GPLv2 baseline
> does, set `suppress_unchanged_order_updates` to `false`. This chapter
> does not establish what GLEEC KDF does for an update that changes
> nothing; the setting restores the baseline behavior.

## 59.5 Bound peer-penalty contract

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §59.11, and are not restated here.

**R59.5.1** *The filter.* A node shall drop, without processing, every
order-match and orderbook peer-to-peer message whose sender's public key
is currently penalised. The check is at the message-dispatch boundary, so
a penalised maker's `MakerReserved` never reaches the selection of
§59.3 and a penalised peer's orderbook rows are never ingested.

**R59.5.2** *What earns a penalty.* A taker shall penalise its
counterparty's public key for **one hour** when the taker-side swap
reaches any of these three outcomes:

| Outcome | Meaning |
| --- | --- |
| negotiation failed | The handshake or the negotiation message exchange did not complete. |
| maker-payment validation failed | The maker's payment did not satisfy the taker's checks. |
| waiting for the taker payment to be spent failed | The maker never spent the taker's payment. |

The penalty record shall name the swap identifier and the outcome that
caused it, and shall be visible through, and removable by, the existing
penalty-listing and penalty-clearing RPC methods. It expires on its own
after the hour; nothing re-arms it.

**R59.5.3** *The field-reported extension.* The first row of R59.5.2 —
penalising on a **failed negotiation** — is exactly what the reference's
"bad maker" change added, and it is the whole of that change on the
taker-swap side. It did **not** alter keep-alive or expiry semantics
(§59.6), and it did **not** alter the selection contract of §59.3: the
collect-sort-connect behaviour of R59.3.1 and R59.3.2 predates it
unchanged, and the single-shot property of R59.3.3 and the absence of
retry in R59.3.4 were not touched.

**R59.5.4** *Implementation status.* Reloaded already implements R59.5.1,
the one-hour duration, the record shape and the RPC surface, and
penalises on the second and third rows of R59.5.2. It does **not**
penalise on a failed negotiation. That single missing trigger is a parity
**gap**, and it is the entire delta between reloaded and the reference
for the field-reported problem. Closing it is a one-line widening of the
set of taker-swap outcomes that trigger the penalty, plus the regression
test of T4.

> The narrowness matters for expectations: closing R59.5.4 does **not**
> make a taker retry a different maker within one order (R59.3.4 forbids
> that by parity), and does not make dead rows disappear from the
> orderbook faster (§59.6 governs that). It makes the *second* attempt
> work, by keeping the maker that just failed out of the second
> attempt's candidate set for an hour.

## 59.6 Bound liveness and keep-alive-freshness contract

> **Clearance (chapter 01 T1, R40).** The forbidden-class-sourced
> material in this section was authored by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS, finding list empty).
> Its clearance is therefore *complete* and not pending: nothing in this
> section is a candidate awaiting a gate. The extent, method and measure
> of that pass are recorded once, in the *Dirty Gate clearance* item of
> §59.11, and are not restated here. **Exception (2026-10-27):** R59.6.7
> and the status update in R59.6.6 were added after that pass and passed the
> KDF Dirty Gate on 2026-10-27 (§59.11, addition of 2026-10-27).
> **Exception (2026-10-28):** the second paragraph of R59.6.2 and the
> cross-reference sentence of R59.6.7 item 1 were added after those passes
> and passed the KDF Dirty Gate on 2026-10-28 (§59.11, addition of
> 2026-10-28).

**R59.6.1** *Expiry runs on the receiver's clock.* A tracked maker public
key's liveness instant shall be the receiver's **own local receipt time**
of that key's most recent keep-alive, never the timestamp the keep-alive
carries. This restates [Chapter 32](32-orderbook-p2p-and-trie.md) R-P3
from the negotiation side and agrees with it exactly.

The rule **corrects baseline behaviour** (V3): the baseline recorded the
sender-supplied timestamp as the liveness instant, so a peer could hold
its own rows alive indefinitely by advertising a future timestamp, or
have them evicted on arrival by advertising a past one. The reference
also records local receipt time, so R59.6.1 is a parity **fix** and not a
divergence.

**R59.6.1a** *Order messages also count as liveness.* *Reloaded-original
(2026-10-28, Decision AN); no forbidden-class input; outside the clearance
of this section's reference-sourced material.* When a node accepts a
maker-order created, updated or cancelled message whose signature verifies
for public key P, and the node already holds a record for P, it MUST record
its own local receipt time as P's liveness instant, exactly as R59.6.1 does
for a keep-alive. The rules that create a record are unchanged: a created
message creates P's record as before, and an updated or cancelled message for
a P the node holds no record for creates nothing. An updated message that
names an order the node does not hold is ignored as before (it is not
accepted) and refreshes nothing; a created or cancelled message is always
accepted. The eviction of R59.6.4
then removes P only when neither a keep-alive nor an order message from P has
arrived within the window.

The top-level `MM2.json` boolean `order_messages_refresh_liveness` (default
`true`) selects the rule; only a JSON `false` turns it off, and the liveness
instant is then the last keep-alive alone, as in the reference.

> **Compatibility:** for behaviour matching GLEEC KDF, set
> `order_messages_refresh_liveness` to `false`.

> **Reloaded divergence (2026-10-28).** The reference takes the liveness
> instant from keep-alives alone. A maker whose keep-alives stop arriving
> while its order updates still do — the case a backlogged, newest-first
> gossip send queue produces — therefore loses its whole book on every
> reference node after 90 s, and every later update is unknown there. Under
> this rule a Reloaded relay keeps such a maker's book, and the reference
> subscribers that wiped it are answered with the full set by that relay at
> their next keep-alive (chapter 32 §32.7.3). A maker that has really stopped
> sends neither, so the dead-row defence of this section is unchanged. A
> peer could re-publish an old signed order message of P after the gossip
> duplicate window to keep a stopped P alive; the same is already possible
> with an old signed keep-alive, whose timestamp the receiver ignores
> (R59.6.2), so the rule adds no new exposure. Nothing on the wire changes.

**R59.6.2** *The wire timestamp is carried, ignored, and unvalidated.*
The keep-alive message carries two members: `trie_roots`, a map from
alphabetically-ordered pair to the sender's trie root for that pair,
and a `timestamp` holding the sender's own time in whole seconds of
Unix epoch. The receiver shall accept the member (it is part of the
dictated wire shape and a message lacking it is malformed) and shall
make **no** use of it: it is not stored, not compared against the
receiver's clock, not bounds-checked, and not a reason to reject the
message. A sender's clock being hours wrong, or its timestamp being
replayed, changes nothing a receiver does.

The map type of `trie_roots` admits any number of entries, but the
conforming message carries **exactly one**: a keep-alive is sent per
pair, on that pair's orderbook topic, holding that single pair's root,
and pairs whose root is all zeros or the empty-trie root get no message
([Chapter 32](32-orderbook-p2p-and-trie.md) §32.9 R-K2a, added
2026-10-28, passed the KDF Dirty Gate on 2026-10-28). The receiver-side rules
of this section hold for each such message independently.

**R59.6.3** *Consequences for consumer-side freshness — the question
answered.* Because every node on both sides of the wire derives staleness
from its own receipt clock (R59.6.1) and ignores the sender's claim
(R59.6.2), a reloaded node **cannot** inherit stale rows from a
reference peer by way of a mis-stated timestamp. A peer that has stopped
sending keep-alives ages out of a reloaded node's state on the reloaded
node's own clock, on the same schedule as on any reference node.

Therefore **no consumer-side freshness filter is required** in the
best-orders or orderbook read paths for parity, and adding one would be a
divergence: it would hide from a reloaded client rows that every
reference client still shows, at the same instant. Reloaded's correction
of its own expiry basis to receipt time is a parity **fix**, and it is
sufficient. Two residual exposures are real but are **not** solved by a
freshness filter and are recorded as D2 and D3 instead: a relaying peer
that keeps forwarding a dead maker's rows while itself staying alive, and
the window between a maker going silent and its state ageing out.

**R59.6.4** *Liveness window.* The keep-alive advertisement interval
shall be **30 seconds** in a production build, and a tracked public key's
state shall be evicted — dropping its tries, its per-pair state and its
order identifiers from the read index — once its liveness instant
(R59.6.1) is older than a window defaulting to **three** advertisement
intervals, i.e. 90 seconds. The window shall be overridable by the
daemon's `maker_order_timeout` configuration member, in whole seconds.
The eviction sweep is part of the same periodic maintenance as R59.4.1
and R59.4.4.

**R59.6.5** *Own public keys are exempt.* The eviction of R59.6.4 shall
never apply to the node's own persistent public key or to any
per-order public key the node itself makes orders under, however long it
has been since the node last heard its own advertisement back. Evicting
one's own key would silently delete one's own live orders from one's own
read index.

The **per-order** half of this exemption is post-baseline on both sides:
the baseline exempted only the persistent key (V3). Reloaded is in the
baseline state — its eviction sweep exempts the persistent key and not
the per-order keys, although it already tracks the per-order key set and
already uses it in the read paths to mark an order as the caller's own.
That is a parity **gap** with a self-inflicted failure mode: a node that
makes orders under a per-order key and is quiet for the window of
R59.6.4 drops its own orders out of its own read index while they are
still live on the mesh. Closing it reuses the existing own-order
predicate at the eviction site.

**R59.6.6** *Implementation status of §59.6.* At the time of writing:
R59.6.2 and R59.6.4 are as-built in reloaded. R59.6.1's correction is
merged into this project's integration branch (status as of
2026-10-27), so a reloaded binary built from it expires peers on its own
receipt clock. R59.6.5's per-order half is unimplemented. R59.6.7 is
unimplemented: the re-announcement path subscribes to the topic without
recording the subscription state (its divergence note). R59.6.3 is a
conclusion and carries no implementation. R59.6.1a (added 2026-10-28) is
implemented (2026-10-28).

**R59.6.7** *Re-announcement records the pair subscription.* (Authored
by the KDF Spec Reader 2026-10-27 from the rows `v2.6.0-beta` and
`v3.0.0-beta` of chapter 02 §2.15A, identical at both; passed the KDF Dirty Gate on 2026-10-27, see §59.11.) Two facts combine:

1. *Synchronization requests are gated on recorded subscriptions.* On
   receiving a keep-alive, a node compares the advertised per-pair roots
   with the roots it holds for that public key and requests a
   synchronization for a pair whose root differs — but a **non-relay**
   node does so only for pairs whose orderbook topic is in its recorded
   subscription state ([Chapter 32](32-orderbook-p2p-and-trie.md)
   R-F6a); a relay node does so for every pair. A pair the node is
   subscribed to on the mesh but has not recorded is, for this purpose,
   not subscribed. This gating is baseline behavior (V4). The full
   trigger, the content of the request and the answer are bound by
   [Chapter 32](32-orderbook-p2p-and-trie.md) §32.7.3 R-Q4 to R-Q6, and
   because a conforming keep-alive carries one pair (R59.6.2, ch. 32
   R-K2a), the comparison concerns one pair per message.
2. *Re-announcement records before it broadcasts.* When the periodic
   maintenance of R59.4.1 finds one of the node's own maker orders
   absent from the local orderbook — after a restart, or after the
   node's own public-key state was evicted (R59.6.4, R59.6.5) — it
   records a subscription for the order's pair in the subscription state
   (as "subscribed, not yet requested" if no record exists; an existing
   record is kept), subscribes to the topic on the mesh if it was not
   subscribed, and only then re-broadcasts the order's created
   notification ([Chapter 60](60-trading-floors-and-refusal-contract.md)
   R60.4.6 item 5 binds the balance reconciliation that precedes it). No
   orderbook request is made at this point. Order creation records the
   subscription the same way; the re-announcement path is bound here
   because it runs without any operator request.

> **Reloaded divergence (2026-10-27).** Reloaded's re-announcement path
> subscribes to the topic on the mesh without recording the subscription
> state — the baseline's behavior (V4). On a non-relay node whose maker
> orders were restored at start, item 1 then never fires for those pairs
> until something else — an orderbook request, a new order on the pair —
> records the subscription: a peer's keep-alive with a differing root
> brings no synchronization request, and the node's view of the other
> makers on its own pairs goes stale.

## 59.7 Tests

**T1.** *Collect, sort, connect.* Deliver three `MakerReserved` messages
for one taker order within the window of R59.3.1, in a non-ascending
price order, all three viable. Exactly one `TakerConnect` is broadcast,
and it names the **cheapest** maker. Exactly one match is inserted.

**T2.** *Fall-through within the pass.* Same as T1 but with the cheapest
reservation made non-viable (mismatched amounts). The connect names the
**second**-cheapest, and the non-viable entry is not retried.

**T3.** *Single-shot.* With one match already inserted for a taker order,
a later viable `MakerReserved` for the same order inserts nothing and
broadcasts nothing (R59.3.3). The test MUST fail if a second connect is
sent.

**T4.** *Penalty on failed negotiation (the parity guard).* A taker-swap
outcome of "negotiation failed" penalises the maker's public key, and a
subsequent `MakerReserved` from that key for a fresh taker order is
dropped before selection (R59.5.1, R59.5.2). The test MUST assert both
halves: the record's creation **and** the message being dropped. It MUST
also assert that the other two outcomes of R59.5.2 still penalise, so the
widening does not accidentally replace the existing set.

**T5.** *Penalty expiry.* A penalty record created at a known instant is
absent once an hour has passed and present just before.

**T6.** *Taker-order timeout, both arms.* A good-till-cancelled taker
order past the deadline with an empty match map converts to a maker order
and broadcasts the created notification; one with a non-empty match map
is cancelled with the timed-out reason and does **not** convert
(R59.4.1).

**T7.** *Maker-side match pruning.* A maker match older than the
order-match deadline and not connected is dropped and its reserved
volume released; a connected one of the same age is kept (R59.4.4).

**T8.** *Keep-alive timestamp is inert.* A keep-alive whose `timestamp`
member is far in the past, and one whose `timestamp` is far in the
future, are both processed identically to one carrying the current
second, and the receiver's recorded liveness instant equals its own
receipt time in all three cases (R59.6.1, R59.6.2). A keep-alive missing
the member is rejected as malformed.

**T9.** *Eviction window and own-key exemption.* A tracked foreign public
key whose last receipt is older than the configured window is evicted
along with its rows; the node's own keys are not, at any age
(R59.6.4, R59.6.5). The configured override is honoured in place of the
three-interval default.

**T10.** *Kick-start on a non-relay node (R59.6.7).* Start a non-relay
node with one stored maker order and no operator orderbook request.
After its first maintenance pass, deliver a keep-alive from another
public key whose root for that pair differs from the one the node holds.
Exactly one synchronization request for that pair is sent. The same
keep-alive for a pair the node has no order on and never requested sends
nothing; on a relay node it sends a request in both cases.

**T11.** *An update that changes nothing is not broadcast (R59.4.6).* An
own maker order is updated through `update_maker_order` once with its
current price, volume, minimum volume and confirmation settings, and once
with a new price. The first update is saved and answered but publishes no
message; the second publishes one maker-order updated message on the
pair's topic. A price given as an equal fraction written differently
(2/4 for 1/2) counts as unchanged. With `suppress_unchanged_order_updates`
set to `false`, both updates publish a message.

**T12.** *Order messages keep a maker alive (R59.6.1a).* A node holds a
record and orders for public key P. For 150 s it receives order-updated
messages from P every few seconds and no keep-alive: P is not evicted, and its
orders stay in the read index throughout. When P then falls silent
altogether, P is evicted one window (90 s by default) after its last message,
within one maintenance sweep. An order-updated message from a P the node holds
no record for creates no record, and one that names an order of P the node does
not hold leaves P's liveness instant unchanged. With `order_messages_refresh_liveness` set to
`false`, P is evicted 90 s after its last keep-alive although its updates keep
arriving.

## 59.8 Deferred Work

**D1.** *Amount-and-lock-time-aware protocol support.* The finding under
R59.4.5 records that reloaded's coin protocol-support check cannot carry
the amount, the lock duration, or the maker-side flag the reference's
carries, so R59.4.5 is unimplementable today. Closing it changes a
coin-trait signature used by four negotiation call sites and by the
per-coin implementations behind them, which is why it is deferred rather
than bound here. Until it is closed, both sides accept some matches the
reference would refuse; the refusals concerned are protocol-capability
refusals, not fund-safety ones, but a match accepted here can still fail
later in the swap.

**D2.** *Relayed rows of a dead maker.* A relaying peer that is itself
alive continues to serve a dead maker's rows to anyone who synchronises
from it until the relay's own eviction of that maker fires. R59.6.3
explains why a consumer-side freshness filter is the wrong remedy; the
right one is a question about relay-side eviction and
synchronisation-response construction, which belongs to
[Chapter 32](32-orderbook-p2p-and-trie.md) §32.7 and is unresolved.

**D3.** *The silence window.* Between a maker going silent and its
eviction under R59.6.4 there is a window of up to the configured window
in which its rows are still offered and a taker can still spend an
attempt on it. Shortening the window trades mesh traffic against dead-row
exposure and is a network-policy decision for both supported netids, not
a code change; it is deferred and must not be made unilaterally, because
the window is also what tolerates a mobile peer's connectivity gaps.

**D4.** *The V2 negotiation loop.* This chapter is scoped to the legacy
(V1) loop. The V2 loop's negotiation messages and its own refusal
contract are bound by
[Chapter 52](52-swap-v2-state-machine.md); whether V2 inherits R59.3's
selection contract unchanged, and whether its failures feed the penalty
of R59.5.2 at all, is not established here.

## 59.9 Baseline Verifications

**V1.** *The four-message loop and the collect-sort-connect selection are
baseline behaviour.*

- *Claim.* The negotiation loop's four message kinds, the pending
  reservation collection, the deferred pass, the price-ascending sort and
  the empty-match-map condition all exist in the pinned baseline tree, so
  §59.3 documents inherited behaviour rather than a port.
- *Verification.* In the baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`, the order-match module
  defines the four message kinds, holds a per-taker-order pending
  reservation collection in its order-match context, defers for a fixed
  delay, sorts the collection by price before walking it, and gates the
  connect on the taker order's match map being empty.
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md).

**V2.** *The penalty mechanism is baseline; its negotiation trigger is
not.*

- *Claim.* The one-hour penalty, the record shape, the dispatch-boundary
  filter and the two non-negotiation triggers are baseline behaviour; the
  failed-negotiation trigger of R59.5.2 is the only post-baseline
  element of §59.5.
- *Verification.* The baseline commit's swap module penalises on the
  maker-payment-validation and waiting-for-spend outcomes only, with the
  same one-hour duration and the same record shape, and the order-match
  module already drops messages from a penalised sender.
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md).

**V3.** *The keep-alive wire shape is baseline; the receipt-time basis
and the per-order own-key exemption are not.*

- *Claim.* The keep-alive wire shape of R59.6.2 — trie roots plus a
  sender timestamp — is baseline. The baseline **used** that timestamp as
  the liveness instant, so R59.6.1 corrects it. The baseline's eviction
  sweep exempted only the node's persistent public key, so the per-order
  half of R59.6.5 is post-baseline. The advertisement interval, the
  three-interval default window and its configuration member name are
  baseline and unchanged.
- *Verification.* In the baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`, the order-match module
  defines the keep-alive message with exactly those two members; the
  handler that processes a received keep-alive assigns the message's own
  timestamp to the tracked key's liveness instant; the eviction sweep's
  keep condition names only the node's persistent public key beside the
  window comparison; and the interval, the three-interval default and the
  `maker_order_timeout` configuration member are all present with the
  values of R59.6.4.
- *Cross-reference.* [Chapter 32](32-orderbook-p2p-and-trie.md) R-P3 and
  §32.5.

**V4.** *The re-announcement's subscription recording is post-baseline;
its gating is not.*

- *Claim.* The baseline re-announces an own order missing from the local
  orderbook after a plain mesh subscription that records no subscription
  state, so R59.6.7 item 2 is a post-baseline correction on the
  reference side that Reloaded has not followed. The non-relay gating of
  R59.6.7 item 1 is baseline.
- *Verification.* In the baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`, the order-match loop's
  missing-order branch subscribes to the topic directly before
  broadcasting the created notification, while the keep-alive handler
  already skips, on a non-relay node, every pair outside the recorded
  subscription set.
- *Cross-reference.* [Chapter 32](32-orderbook-p2p-and-trie.md) R-F6a.

## 59.10 External References

- libp2p gossipsub, for the broadcast transport all four messages of
  §59.2 travel on.
- [Chapter 11 — Order-Match Cancellation Race Mitigation](11-order-match-cancellation.md)
  for the recently-cancelled guard that interacts with a maker order
  disappearing mid-negotiation.
- [Chapter 12 — Maker-Order State Store](12-order-match-state-store.md)
  for the maker-order store R59.4.3 walks and the expiry it owns.
- [Chapter 13 — Atomic-Swap Version Negotiation](13-swap-version-negotiation.md)
  for the swap-version tag the first two messages carry and the
  element-wise-minimum rule.
- [Chapter 32 — Orderbook Patricia-Trie and P2P Surface](32-orderbook-p2p-and-trie.md)
  §32.2 (the topic), §32.4 R-P3 (the receipt-time rule R59.6.1
  restates), §32.5 (the order-record wire shape), §32.7 (the
  synchronisation surface D2 concerns; §32.7.3 R-Q4 to R-Q6 bind the
  keep-alive-driven request and answer R59.6.7 item 1 relies on),
  §32.8 R-F3a and R-F3b (a relay's gossip-layer forward of a keep-alive,
  which precedes its own handling, and the application-level forward;
  pointer updated 2026-10-28 with the second-pass amendment of chapter 32,
  which passed the KDF Dirty Gate on 2026-10-28) and §32.9 (the
  keep-alive and expiry semantics R59.6.4 refines; R-K2a binds the
  one-pair-per-message shape R59.6.2 names).
- [Chapter 51 — Legacy (V1) Atomic-Swap State Machine](51-legacy-v1-swap-state-machine.md)
  §51.6 (the refusal and abort contract the outcomes of R59.5.2 come
  from) and §51.8 (the reserved-funds semantics R59.3.3 protects).
- [Chapter 52 — Version-Two Atomic-Swap State Machine](52-swap-v2-state-machine.md)
  for the V2 loop D4 defers.
- [Chapter 57 — EVM Fee and Amount Inputs](57-evm-fee-inputs.md) §57.7A
  for the per-coin minimum trading volume the maker and taker order builders
  enforce before any message of §59.2 is sent (cleared there on 2026-10-24 —
  see the second scoped-addition item of its §57.12).
- [Chapter 60 — Per-Coin Trading Floors](60-trading-floors-and-refusal-contract.md)
  for every family's floors, the order-builder arithmetic, and the one
  volume effect on §59.2's exchange: a maker sends no reserved message for
  a request below its order's minimum volume (R60.5.2; cleared there on
  2026-10-25).

## 59.11 Provenance Footer

- *Inputs:* the pinned baseline tree (V1, V2, V3, and the whole of §59.2,
  §59.3 and §59.4 except the lock-duration asymmetry of R59.4.5); the
  present reloaded workspace (every implementation-status statement, and
  the finding under R59.4.5 as it concerns reloaded's own call shape);
  the dictated keep-alive and order-match message shapes the project must
  produce and consume on the gossip mesh (§59.2, R59.6.2); a field report
  from an operator of a KDF-derived wallet, as the statement of the
  problem the chapter addresses (§59.0) — a third-party report of
  observed behaviour, carrying no implementation material.
- *Permitted-input classes used:* R1 (the baseline tree), R4 (wire
  formats the project must inter-operate with — the four message kinds,
  their addressed/broadcast nature, and the keep-alive member set), R6
  (behavioural observation of the public mesh, for the dead-row problem
  statement), R7 (independent work, for the chapter's own organisation
  and its test obligations).
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* consulted **only** to establish, under the
  reference designations of §59.1: that the field-reported "bad maker"
  change consists of adding the failed-negotiation outcome to the
  existing penalty trigger set and nothing else on the taker-swap side
  (R59.5.3), leaving selection, single-shot insertion, the absence of
  retry, and keep-alive and expiry semantics untouched; that the
  reference's selection contract is the collect-sort-connect behaviour of
  R59.3.1–R59.3.2 with the same 3-second window and the same
  empty-match-map gate reloaded already has; that there is no taker-side
  match pruning (R59.4.2) and the two timeout arms of R59.4.1 and the
  maker-side pruning rule of R59.4.4 are as stated, with the 30-second
  deadlines; that the maker reserves at most once per request under the
  conditions of R59.4.3; that the maker's protocol-support check carries
  an amount, a scaled-and-rounded-up lock duration and a maker-side flag
  where the taker's does not (R59.4.5); that liveness is recorded from
  local receipt time and the wire timestamp is neither stored nor
  validated (R59.6.1, R59.6.2); and the advertisement interval, the
  three-interval default window, its configuration member name, and the
  own-key exemption of §59.6.4–§59.6.5. No discretionary expression — no
  private type, field or function name, no function body, control-flow
  transcription, helper decomposition, local name, error or log literal,
  and no test identifier — was reproduced; the message kind names and the
  keep-alive member names are dictated wire identifiers under chapter 01
  R11's inter-operability carve-out, and the configuration member name is
  a dictated configuration key. (Addition of 2026-10-27, KDF Spec Reader,
  passed the KDF Dirty Gate on 2026-10-27: R59.6.7, T10 and V4 rest on the rows `v2.6.0-beta`
  and `v3.0.0-beta` of chapter 02 §2.15A, identical at both, compared
  with the baseline anchor. The facts taken are the non-relay gating of
  keep-alive-driven synchronization requests on the recorded subscription
  state, and the re-announcement path's recording of that state before
  it re-broadcasts, with no orderbook request. The R59.6.6 status update
  rests on this project's own tree only. No private identifier, function
  body, helper decomposition, log or error literal was reproduced. Dirty Gate clearance (chapter 01 T1, R40): PASS, measure 0.02, no leaks;
  every reference fact confirmed at both rows. *Extent:* the keep-alive
  synchronization gating and the re-announcement path, one file per row and
  the baseline. *Method:* reading plus a mechanical regex scan of every added
  line against private identifiers, literals and distinctive phrases; the
  mechanical gate tool was not run (prose). *Measure:* 0.02 against the 0.10
  threshold.) (Addition of 2026-10-28, KDF Spec Reader, hotfix pass: the
  second paragraph of R59.6.2 — the one-entry-per-message shape of
  `trie_roots` — the cross-reference sentence appended to R59.6.7 item 1,
  and the §59.10 pointers to ch. 32 §32.7.3, R-F3a and R-K2a. The one
  reference fact taken, the per-pair single-entry own-topic keep-alive
  shape, rests on the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02
  §2.15A, identical at both, and is bound in ch. 32 R-K2a; this chapter
  only names it. No private identifier, function body, helper
  decomposition, log or error literal was reproduced. These three
  passages **passed the KDF Dirty Gate** on 2026-10-28: PASS, measure 0.00
  against the 0.10 threshold, no leaks; extent, method and measure are those
  recorded for the same pass in chapter 32 §32.13. The clearance is complete
  and not pending.)
- *Dirty Gate clearance (chapter 01 T1, R40, R43).* The whole of this
  chapter's forbidden-class-sourced material — the scoped item above,
  which is the selection contract of §59.3, the timeout and maker-side
  contract of §59.4 including the lock-duration asymmetry of R59.4.5,
  the penalty trigger set and its field-reported extension in §59.5, and
  the liveness, keep-alive-freshness and eviction contract of §59.6 —
  was authored by the KDF Spec Reader under the two-team procedure of
  chapter 01 §1.4A and **passed the KDF Dirty Gate** on 2026-10-24:
  PASS, discretionary-expression measure 0.05, finding list empty, and
  accordingly no remedy class for any line or for the candidate as a
  whole. The whole file was gated. This was the chapter's first gate
  pass; no earlier verdict exists or was relayed, so nothing was
  re-gated. The clearance is therefore **complete and not pending**, and
  R40 withholds nothing here.

  *Which sections carry a pointer to this item, and how that set was
  fixed.* §59.3, §59.4, §59.5 and §59.6 each carry a short clearance
  pointer, so that a reader or an implementer entering at the material
  passes the notice where the material appears (R40) and not only on
  reaching this footer. That set was **derived** from the scoped item
  above rather than chosen: the scoped item already fixes which sections
  rest on forbidden-class input, and those four sections are exactly the
  ones it names, so taking the pointer set from the record makes the two
  agree by construction and leaves no scoped section without the notice.
  A check of where a reader is most likely to enter, ranked by inbound
  references within the document set rather than by taste, picks out the
  same four and so adds no judgement to the derivation: §59.6 is the only
  section another chapter deep-links by rule number (the
  [Chapter 32](32-orderbook-p2p-and-trie.md) cross-references into
  R59.6.2–R59.6.5) and carries its own implementation-status rule;
  §59.5 is where a reader dispatched at the parity gap lands first and
  may never read §59.0; §59.3 is the densest inbound-link target inside
  the chapter (§59.0, R59.4.2, R59.5.3 and T1–T3 all point into it); and
  §59.4 is entered from §59.0, from T6 and T7, from D1, and from the
  chapters §59.10 names for the maker-order store and the version tag.
  The remaining sections carry no pointer, and that is a scope judgement
  rather than an oversight. §59.0 states the chapter's own framing and
  problem statement, §59.1 its reference-designation discipline and
  §59.2 the dictated wire shape and the inherited match-map structure,
  none of them a forbidden-class-sourced fact. §59.7's tests, §59.8's
  deferrals and §59.10's external references are derivative: each tests,
  defers or hands on material the four sections already bind, so
  repeating the notice there would multiply it without putting it nearer
  the material it concerns. §59.9's baseline verifications cite R1
  baseline material and fall outside a candidate-against-corpus
  comparison altogether. §59.11 is the record itself.

  The three elements of the R39 scope attestation, carried forward here
  in corpus-free terms as chapter 01 T1 requires:

  - *Extent.* The forbidden-class material corresponding to the subject
    matter this chapter scopes: the legacy (V1) order-match negotiation
    surface — the four-message exchange, taker-side collection and
    selection, both taker-order timeout arms, the maker-side reservation
    contract and match pruning — together with the counterparty-penalty
    surface, the taker-side legacy-swap outcome set that arms it and its
    listing and clearing request surface, and the keep-alive liveness and
    tracked-public-key eviction surface. Quantity examined: **five
    files**, one read in full and four in part. Reference designations:
    the publicly published release designations `v2.6.0-beta` and
    `v3.0.0-beta`, which the lineage's own releases carry and which
    chapter 01 §1.7 admits as public-release facts. **The record is no
    more precise than those designations** — no post-anchor commit
    identifier, no branch state and no location within the compared
    material is given (chapter 01 R11 and the reference-version limit of
    §1.7), and that limit is left visible rather than papered over.

  - *Method.* By **reading**, on both sides. **This repository's own
    residual-similarity gate tooling was not run:** it compares code
    blocks against a pinned reference and this chapter embeds no code
    block, so the instrument had no operand and its figure would not
    have been informative. No other mechanical comparison was used. The
    absence is stated so that it is visible rather than implied.

  - *Measure.* **0.05** on the discretionary-expression basis — the same
    figure the leak report carries, not a second one — with
    dictated-interop and pinned-interface content excluded from the
    measured set before the judgement was made. Excluded on that basis,
    so that the figure's coverage is visible: the four negotiation
    message kinds (§59.2, and their re-use as state labels through
    §59.3–§59.6) and the keep-alive member set (R59.6.2, T8), as
    dictated wire identifiers; the dictated operator configuration key of
    R59.6.4; the numeric protocol and liveness constants whose values
    are part of the counterparty-visible contract (R59.3.1, R59.4.1,
    R59.4.4, R59.5.2, R59.6.4); the argument meaning and arity of a
    cross-crate public trait method, stated behaviourally and without
    naming it (R59.4.5 and the informative finding beneath it); the
    abstract behavioural, lifecycle, ordering and timeout requirements of
    §59.3, §59.4 and §59.6; the externally observable serialized outcome
    labels the R59.5.2 table restates in prose rather than quoting as
    identifiers; and the pre-anchor baseline anchor commit named in
    §59.9 (V1, V3), which is a permitted input under R1 and not
    forbidden-class material. Every identifier this chapter quotes
    verbatim is paired, in the scoped item above, with the R11 carve-out
    class that admits it, so no `name-carve-out` remedy is owed.
    **Judgement standard read against:** zero tolerance — one identified
    item of genuine discretionary expression fails a candidate however
    much of the surrounding content is legitimately dictated; the figure
    also sits well below the 0.50 discretionary-similarity ceiling this
    repository's gate tooling carries as its default cap. Absent
    throughout, as the verdict's classification records: verbatim and
    near-verbatim function bodies; step-by-step branch, loop or
    statement-order transcription; private non-wire identifiers;
    discretionary error, log, panic and diagnostic wording; per-method
    behaviour tables keyed to internal names; internal module trees; and
    internal test identifiers. The residual the figure reports is two
    requirements only — the start condition of the deferred selection
    pass (R59.3.1) and the at-most-once-then-stop property of the maker
    reservation (R59.4.3) — each of which a counterparty can see violated
    on the wire, which is the clean-channel class R39 admits rather than
    an internal arrangement.
- *Reloaded-original addition (2026-10-28): R59.4.6 and T11.* The rule
  that an own maker-order update that changes nothing is not broadcast,
  and its opt-back setting. *Inputs:* this repository's own
  `update_maker_order` path and maker-order update messages, and a
  measurement of this repository's keep-alive and synchronization code.
  No reference input of any kind: the forbidden corpus was not consulted
  for this item, and the rule states no reference behavior (its
  compatibility note says so). It needs no Dirty Gate pass.
- *Reloaded-original addition (2026-10-28, Decision AN): R59.6.1a, its
  divergence note, the R59.6.6 status sentence and T12.* Order messages count
  as liveness, with the opt-back setting. *Inputs:* this repository's own
  maintenance sweep and gossip handlers; a lab run of reference binaries as
  black boxes (a public-network observation: a reference seed and its
  subscribers dropped a maker's whole book 90 s after its keep-alives stopped
  arriving); and the maintainer's decision. No reference implementation was
  consulted for this item, and it needs no Dirty Gate pass.
