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

A fourth, Reloaded-original section, **capability words** (§59.4A), lets a
coin declare in its protocol information a capability that its
counterparty must have before a swap starts. It was added on 2026-11-04
so that a shielded-coin swap never starts with a node that cannot build
the transaction format the network now requires.

A fifth section, also Reloaded-original (§59.4C, 2026-11-20), keeps the
orders of such a node out of what the order book shows.

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

**R59.4.7** *Protocol-information bytes: what a coin publishes, where a
counterparty's value is judged, and how.* *(Passed the KDF Dirty Gate on 2026-11-04, measure 0.03; the provenance
footer records the clearance. Authored by the KDF Spec Reader on 2026-11-04 from the rows `v2.6.0-beta`
and `v3.0.0-beta` of chapter 02 §2.15A, identical at both.)* Every maker
order carries `base_protocol_info` and `rel_protocol_info`, each a byte
string that is empty when the coin supplies nothing, and both match messages
(`TakerRequest`, `MakerReserved`) carry the same two members as optional byte
strings, always present when this lineage sends them. Each is one opaque
per-coin value produced by the coin itself. At both reference rows:

1. *Content per coin family.* A UTXO-family coin -- the standard UTXO coin,
   QTUM, BCH, **and a shielded (`ZHTLC`) coin, which reuses the UTXO rule
   unchanged** -- publishes the MessagePack encoding of its **transparent
   address-format descriptor**: a map with one `format` key whose value is
   `standard`, `segwit` or `cashaddress`, the last with its `network` and two
   prefix members (the same object the coins configuration and the
   `address_format` activation member use, chapter 38 §38.3). The value is
   taken from the coin's own activated address format, so a shielded coin
   publishes the transparent `standard` descriptor. EVM, Tendermint, Sia and
   SLP-token coins publish an **empty** byte string.
2. *Judgment of a counterparty's value.* A UTXO-family coin, shielded coins
   included, accepts a counterparty's value exactly when **either** the value
   is absent and the coin's own format is not segwit, **or** the value decodes
   as an address-format descriptor. The decoder stops after the first complete
   MessagePack value and does **not** check that the input is exhausted, so a
   value that holds a valid descriptor **followed by further bytes is accepted
   and the trailing bytes are ignored**; a value that does not begin with a
   decodable descriptor -- an empty string included -- is refused. The amount,
   lock-duration and maker-side inputs of R59.4.5 do not enter the UTXO rule.
   EVM, Tendermint, Sia and SLP-token coins accept every value, absent or not.
3. *Where it is applied.* Exactly the two points R59.3.2 and R59.4.3 name:
   the taker judges the maker's two values in `MakerReserved` before sending
   `TakerConnect`, and the maker judges the taker's two values in
   `TakerRequest` before sending `MakerReserved`. Both are before any swap
   record exists and before any payment.
4. *A refusal is silent.* A refused value produces no message to the
   counterparty and no error to the operator: the maker sends no reservation
   (the taker's request then times out under R59.4.1), and the taker sends no
   connect (the maker's reservation ages out under R59.4.4). Nothing in either
   match message, in the negotiation payloads of chapter 51 R59, or in any
   later swap message carries a transaction-format or chain-upgrade
   capability; the only per-coin capability signal of the whole matching and
   negotiation exchange is this address-format descriptor.
5. *Orderbook presentation.* The `orderbook` and `best_orders` responses
   derive each entry's `address` from the order's public key under the same
   descriptor, decoded from the published value with the same tolerance; a
   value that does not decode is shown under the `standard` format. For a
   shielded coin the entry is the shielded outcome of chapter 32 R-F6b
   whatever the bytes say (chapter 39 R39.5.3).

> **Compatibility (this repository, 2026-11-04; not corpus-sourced).**
> Reloaded publishes the same descriptor for every UTXO-family coin and
> judges a counterparty's value by the same two-arm rule with the same
> decoder tolerance (both lineages pin the same MessagePack library release).
> Reloaded's shielded coin adds a refusal of its own during the Ironwood swap
> freeze (chapter 39 R39.6.4b), and chapter 39 R39.5.14 records what a
> reference counterparty does with a shielded-coin value that Reloaded
> extends beyond the descriptor. The amount- and lock-aware inputs of R59.4.5
> change no UTXO-family outcome; D1 stands for the other families.

## 59.4A Capability words in coin protocol information

*Reloaded-original (2026-11-04, Decisions BM and BO); no forbidden-class
input. The compatibility note relies only on the reference facts of R59.4.7
and Chapter 39 R39.5.14, which passed the KDF Dirty Gate.*

Every order and both negotiation messages that R59.3.2 and R59.4.3 check
carry each coin's **protocol information**:

- `MakerOrderCreated`, and the side channel of a synchronization answer
  ([Chapter 32](32-orderbook-p2p-and-trie.md) §32.5.2, R-W2a);
- `TakerRequest`;
- `MakerReserved`.

The order-match layer treats these bytes as opaque: the coin produces
them, and the coin judges the counterparty's. What each coin family
publishes today, and how a counterparty's value is judged, is bound in
R59.4.7. This section binds one
generic extension of the bytes, the **capability word**. Through it a
coin can declare a capability that its counterparty must have before a
swap starts. The first coin to use it is the shielded coin of
[Chapter 39](39-zcash---z_coin-shielded-coin.md), which declares that it
builds version-6 transactions after the Ironwood upgrade (Decision BM).
The rule that requires that capability, its time window and its switch
are bound in Chapter 39, not here.

**R59.4A.1** *Layout.* A coin that uses a capability word (R59.4A.4)
shall send as its protocol information:

1. first, exactly the bytes it sends without the word, unchanged: its
   *first value*. For a UTXO-family coin this is its encoded address
   format;
2. immediately after it, one MessagePack map with string keys and
   boolean values: the capability word.

A writer lists only the capabilities it declares, each with the value
`true`, with the keys in ascending byte order, so the same set always
gives the same bytes. Nothing after the word is defined.

**R59.4A.2** *Reading.* A reader shall determine the declared
capabilities in three steps:

1. skip exactly one complete MessagePack value from the start, whatever
   its type, without interpreting it;
2. decode the next value as a map with string keys;
3. a capability is declared exactly when its key is present with the
   boolean value `true`.

Steps 1 and 2 read under the bounds of
[Chapter 28](28-libp2p-modernization.md) R16a, which apply to every value
the node reads from a peer.

No capability is declared when:

- the protocol information is absent or empty;
- the first value is truncated or malformed, so the skip fails;
- nothing follows the first value;
- what follows is not a map with string keys, or is truncated.

A key whose value is `false`, or not a boolean, is not declared. A key that
appears more than once is declared when any of its occurrences has the
value `true`. A word that is an empty map declares nothing. Keys the
reader does not know are ignored, and so are any bytes after the word.
Reading never rejects the message itself: it yields the set of declared
capabilities, which may be empty. A coin that requires a capability
therefore fails closed: anything other than a well-formed word with that
key set to `true` counts as "not capable".

**R59.4A.3** *The first value reads the same.* Every reader of a coin's
first value shall read only that value and ignore what follows it:

- the coin's own acceptance check;
- the address format the order book reports for an order (Chapter 32).

The reported address format, and the decision of a coin that requires no
capability, are therefore the same with the word as without it.

**R59.4A.4** *Opt-in per coin.* A coin sends a capability word only if
its own chapter binds a capability for it. The protocol information of
every other coin stays byte for byte as it is without this section. A
further coin opts in only after a Spec Reader answer, passed by the Dirty
Gate, on how the reference release rows treat the longer bytes for that
coin; for the UTXO family that answer is R59.4.7 item 2 (the decoder ignores
what follows the first value), so a UTXO-family coin opts in through its own
chapter. The keys in use:

| Key | Coin family | Meaning | Required by |
| --- | --- | --- | --- |
| `tx_v6` | shielded coin (Chapter 39) | This node builds and spends the version-6 transaction format of the Ironwood upgrade. | Chapter 39 R39.6.4f |
| `sighash_suffix` | UTXO-family coin with a non-zero sighash suffix (Chapter 61) | This node signs every spend of the coin with the coin's configured sighash suffix. | Chapter 61 R61.5A.2 |

**R59.4A.5** *Who decides.* The order-match layer shall carry the
protocol information unchanged in all three messages and in the side
channel of Chapter 32 R-W2a. Every decision stays with the coin's
acceptance check, at the call sites of R59.3.2 and R59.4.3:

- a coin that requires a capability declines, through that check, a
  counterparty whose word lacks it;
- a declined reservation is not connected (R59.3.2), and a declined
  request is not reserved (R59.4.3).

Both happen before any swap starts. No swap record exists, nothing is
paid, and the penalty of R59.5.2 does not apply.

**R59.4A.6** *Logs.* When an acceptance check declines a message, the
order-match layer shall log one debug line. It names the order
identifier, the coin, and whether the message was a reservation or a
request. When a taker's selection pass (R59.3.2) ends without a connect
and an acceptance check declined at least one reservation in it, the
layer shall log one info line naming the taker order and the number of
reservations declined. An operator whose order times out can then see
why. The coin logs its own reason (Chapter 39).

**R59.4A.7** *No effect on the trie.* Protocol information is not part of
the trie leaf (Chapter 32 R-W3). The word therefore changes no root, no
synchronization and no order identity.

Rationale: the order-match layer has no other channel that reaches the
counterparty before the first payment.

- Makers and takers are light nodes. They connect to relays, not to each
  other.
- A peer-level signal therefore describes only relays: a peer
  identifier, a version announcement or a direct request.
- The coin's protocol information is the one per-order, per-coin channel
  that both sides already check before a swap starts.
- The word declares a capability, not the software a node runs. It
  judges an older Reloaded and any other implementation by the same test.

The sending side has no switch. A node that withheld the word would be
declined by every node that requires it, so the only switch is the
requirement's, which belongs to the coin (Chapter 39). The word is
visible to every reader of the order book, like the rest of an order.
While only Reloaded sends it, it also marks a node as Reloaded.

> **Compatibility.** At the reference release rows a counterparty judges
> a coin's protocol information by its first value alone (R59.4.7 item 2,
> Chapter 39 R39.5.14). A Komodo DeFi Framework 2.6.0-beta or GLEEC KDF
> 3.0.0-beta node therefore accepts a value that carries a capability word
> and matches as before. It never sends a word itself, and it shows the
> order under the right address. Because the reference refuses a value that
> does not begin with a decodable first value, the word must follow the
> first value and never precede or replace it (R59.4A.1). The protection
> therefore rests on the Reloaded side alone: a node that requires a
> capability declines a counterparty without it in both roles, before any
> swap starts (R59.4A.5). A reference taker sees a Reloaded maker's decline
> only as a request that times out, as with any refusal at the reference
> (R59.4.7 item 4).

> **Status note (2026-11-20).** What a relay at the reference rows returns
> for these bytes -- in the full-orderbook answer, the synchronization
> answer and both best-orders answers -- and how it treats a best-orders
> peer request with one more trailing member, is recorded in §59.4B
> (passed the KDF Dirty Gate on 2026-11-20). Nothing in this section is
> changed by it.

## 59.4B What a relay returns for an order's protocol information

*(Passed the KDF Dirty Gate on 2026-11-20, measure 0.01, attestation in
§59.11. Reference facts, KDF Spec Reader, 2026-11-20, round 0.5.0-18.
Rows `v2.6.0-beta` and `v3.0.0-beta` of
chapter 02 §2.15A, identical at both in everything stated here; the rows
`dev (as of 2026-03-27)` and `main (as of 2026-10-07)` do not differ from
`v3.0.0-beta` in the material read. The decoder properties used are those
of the public MessagePack library release both rows pin, confirmed in
that library's published source.)*

§59.4A relies on a relay carrying a coin's protocol information
unchanged (R59.4A.5). This section records what a reference relay does
with the bytes between receiving an order and answering a request about
it. "Relay" here means a node that holds other makers' orders and
answers the peer requests of [Chapter 32](32-orderbook-p2p-and-trie.md)
§32.7.1.

**R59.4B.1** *What a relay keeps.* A relay keeps, for each order it
holds, the order's two protocol-information byte strings exactly as it
received them. It does not decode, check, shorten or rewrite them, and
it applies no length rule of its own to them. It receives them in one of
two ways:

1. from the maker's `MakerOrderCreated` message, whose two members it
   stores as they are; a message without the two members gives two empty
   strings;
2. from the side-channel entry of an answer another relay gave it (a
   full-orderbook answer or a synchronization answer); an answer with no
   entry for the order gives two empty strings.

A maker's later update message carries no protocol information and
leaves the stored strings as they are. When a later answer carries the
order again, the relay rebuilds its record from that answer, protocol
information included.

**R59.4B.2** *What a relay returns.* In each of these four answers, the
side-channel map of protocol information holds **one entry for every
order the answer contains**, keyed by the order's identifier, and the
entry holds both strings whole -- every byte after the first value
included:

- (a) the answer to `GetOrderbook` (the full orderbook of a pair);
- (b) the answer to `SyncPubkeyOrderbookState`, which a keep-alive with
  an unexpected root triggers: an entry for every order the answer adds,
  changes or lists in a full set, and none for an order it only removes
  (Chapter 32 R-Q2a);
- (c) the answer to `BestOrders` (by volume) and the answer to
  `BestOrdersByNumber`.

A relay at these rows never leaves the entry out for an order the answer
contains. An order that a by-volume answer passes over because of its
minimum volume is in neither the order list nor the map. The map of
confirmation settings differs: it has an entry only for an order whose
settings the relay holds.

**R59.4B.3** *An entry with empty strings.* Apart from a coin whose
maker publishes an empty value, the entry is present with empty strings
exactly when the relay never received the bytes (R59.4B.1): the maker's
message lacked the two members, or the relay learned the order from an
answer that lacked the entry. No path at these
rows drops bytes a relay holds, and no relay at these rows produces an
answer without the entry. Empty strings for a coin whose makers always
publish a non-empty value (R59.4.7 item 1) therefore come from a maker
or a relay older than these rows, or from another implementation, not
from a relay at these rows shortening a longer value. A reader of the
answer cannot tell "the maker published nothing" from "a relay on the
way never had it".

**R59.4B.4** *The reader's side.* A node that reads one of these answers
treats a missing map, and a missing entry for an order, as two empty
strings. It keeps and shows the order (Chapter 32 R-W2; R59.4.7 item 5
for the address it then shows).

**R59.4B.5** *Wire form (dictated).* In `MakerOrderCreated` and in the
side-channel entry alike, each of the two strings travels as a
MessagePack **array of unsigned integers, one per byte**, not as a
MessagePack binary value. A relay re-encodes what it holds in the same
form, so the byte sequence a reader obtains equals the one the maker
sent.

**R59.4B.6** *A best-orders peer request with one more trailing member.*
`BestOrders` and `BestOrdersByNumber` each carry three members, encoded
by position. When a request carries a fourth member after the three, a
relay at these rows **ignores it**: it answers exactly as it would
without the member. It does not refuse the request, report an error or
treat the sender differently, and the answer has the same shape. This is
the decoder tolerance of Chapter 32 R-W2a applied to the request; it
holds because the request's members are the last thing in the request
message. It covers only a member appended at the end. A member inserted
earlier, a member of a changed type, or a request kind the relay does
not know is a different case, and this section does not bind it. The
same tolerance applies in the other direction: a reader at these rows
ignores a member appended at the end of a best-orders answer.

The local `best_orders` RPC request (the legacy method and the mmrpc 2.0
method) is equally lenient at these rows: a member of the request object
that the method does not know is ignored.

**R59.4B.7** *Version-6 transactions and the published value in newer
states.* The rows this chapter can read give this picture:

- At `v2.6.0-beta`, `v3.0.0-beta`, `dev (as of 2026-03-27)`,
  `staging (as of 2026-03-27)` and `main (as of 2026-10-07)` no source
  file names the Ironwood upgrade, the shielded coin has no setting for
  it, and the shielded-coin library pinned is the one of the row
  `librustzcash k-1.4.2` throughout. None of these states therefore
  builds a version-6 transaction (chapter 39 R39.6.4f, compatibility
  note). What the shielded coin publishes as protocol information is the
  bare descriptor of chapter 39 R39.5.14 at every one of them.
- On 2026-11-20 the public repository of the historical record listed no
  release tag newer than `v3.0.0-beta`, and its `main`, `dev` and
  `staging` branches still stood at the rows named above. No branch
  listed there on that date names the Ironwood upgrade in any source
  file.
- Whether a Komodo or GLEEC build exists that was made from source not
  published in that repository, whether such a build constructs
  version-6 transactions, and what it would publish as ARRR protocol
  information, **cannot be established** from the states read. A
  black-box observation of such a build's orders on the public network
  (chapter 01 R6) would be the way to learn it.

> **Compatibility (this repository, 2026-11-20; not corpus-sourced).** A
> Reloaded node that hides, at RPC presentation, an order whose protocol
> information lacks a capability the node requires can rely on a
> reference relay returning a capability word unchanged in all four
> answers (R59.4B.2). It must treat a missing entry and empty strings
> alike as "no capability declared", which R59.4A.2 already does. Such an
> order may come from a capable maker whose bytes an older relay never
> held (R59.4B.3); hiding it is then a false negative at presentation
> only, and the acceptance check of R59.4A.5 still decides at
> negotiation. A member appended to a best-orders peer request changes
> nothing at a reference relay (R59.4B.6), so it cannot be used to make
> such a relay filter its answer.

## 59.4C Orders that lack a required capability are not listed

*Reloaded-original (2026-11-20, by the maintainer's decisions of that day);
no forbidden-class input.*

§59.4A lets a coin decline, at the two match messages, a counterparty that
lacks a capability. The order book kept listing that counterparty's orders,
because the order-book answer showed no capability flag (D5). A user who
picked such an order got no swap: the reservation was declined (R59.4A.5),
the taker order timed out, and a good-till-cancelled order then became a
maker order (R59.4.1). This section removes such orders from what the node
shows, with the knowledge the node already has: the order's own protocol
information, which the order book stores with every order
([Chapter 32](32-orderbook-p2p-and-trie.md) R-W2a). What a reference relay
returns of that information is recorded in §59.4B.

**R59.4C.1** *Required set.* For a coin, the node's **required set** is the
set of capability keys (R59.4A.4) whose absence makes the coin's acceptance
check decline a counterparty at this moment:

- for a shielded coin, `tx_v6` while the requirement of
  [Chapter 39](39-zcash---z_coin-shielded-coin.md) R39.6.4f(b) applies and
  its switch (R39.6.4f(c)) is on;
- for a coin with a non-zero sighash suffix, `sighash_suffix` while the
  switch of [Chapter 61](61-utxo-sighash-suffix.md) R61.5A.4 is on;
- for every other coin, nothing.

The set is read from the activated coin when the coin is activated, and from
the coin's configuration entry otherwise, by the same rules. Only the
activated coin knows an Ironwood height the node discovered at run time
(Chapter 39 R39.6.4e(a)). A configuration entry whose protocol or capability
keys do not parse requires nothing; such a coin cannot be activated. Where a
coin trades under another order-book ticker, the set is that of the coin the
caller names, and that of the order's own ticker where the caller names none
(the event stream, and the counterpart coin in `best_orders`). Reading the
set has no side effect and writes no log line. The acceptance check and this
section read one source, so they cannot disagree.

**R59.4C.2** *Presentable.* An order is **presentable** exactly when, for
each of its two coins, every key of that coin's required set is declared in
the order's protocol information for that coin, read as R59.4A.2 reads it.
Absent, empty or malformed information declares nothing, so an order whose
information for such a coin is missing is not presentable. A coin with an
empty required set never makes an order unpresentable, whatever its bytes.
An order of the node's own is always presentable.

**R59.4C.3** *Where it applies.* The node shall list only presentable orders
in:

- the legacy and the mmrpc-2.0 `orderbook`;
- the legacy and the mmrpc-2.0 `best_orders`;
- the order-book event stream: no "new or updated" event for an order that
  is not presentable, and no "removed" event for one either. The order is
  judged at the moment the event would be sent. If a coin's required set
  changes between an order's arrival and its removal, which can happen
  where the window of Chapter 39 R39.6.4f(b) opens while the node runs, or
  where it rests on an Ironwood height that only the activated coin knows,
  a stream client can keep an entry whose removal it never receives, or
  receive the removal of an entry it never saw;
- the pair counts that `orderbook_depth` takes from the node's own book.

Counts, totals and running sums in these answers shall be computed over the
listed orders.

**R59.4C.4** *What is unchanged.* The stored order book, the trie,
synchronization and gossip propagation are unchanged: the node keeps and
forwards an order that is not presentable like any other (R59.4A.7). Every
answer the node gives to a peer -- the full order book, a synchronization
answer, best orders, depth -- is unchanged, so a relay serves all orders to
every node. The matching rules of R59.3.2 and R59.4.3 are unchanged. The
counts `orderbook_depth` obtains from a relay are not filtered, so for a
pair the node does not hold they can exceed what `orderbook` lists.

**R59.4C.5** *`best_orders` refill.* A relay selects the best orders by
price before this node can apply R59.4C.2, so the answer for a pair can lose
orders while presentable ones exist deeper in the book. When the relay's
answer for a pair loses at least one order under R59.4C.2, the node shall
fetch that pair's full order book once, with the request `orderbook` makes,
and shall build the pair's part of the answer from its own book: the
selection this node applies when it answers a best-orders request as a
relay, by volume or by number as requested, over presentable orders only. If
the fetch fails, or no relay answers it, the pair's part is the relay's
answer without the orders that are not presentable, and one warning is
logged. After the fetch the node stays subscribed to the pair, as it does
after `orderbook`. The books of at most four pairs are fetched at a time.

**R59.4C.6** *Early refusal.* When `buy` or `sell` names orders in
`match_by`, the node's own book holds every named order, and none of them is
presentable, the node shall refuse the request at once. No taker order is
created. The error text is `None of the orders named in match_by can be
matched: <coin> requires a counterparty that declares <capability>`. It names
the coin and the missing capability of the named order with the lowest
identifier (`match_by` carries the orders as a set), and within that order
the order's base coin is judged before its rel coin. On a surface with typed
errors the error type is `OrderNotMatchableMissingCapability`, with the
members `coin`, `capability` and `orders`; on a surface without them it is
the error text alone. Only an order the request could match by its coins is
judged: an order on the request's pair and on the side the request takes. In
every other case -- no `match_by`, `match_by` by public key, a named order
the node does not hold, a named order of the node's own, a named order of
another pair or of the side the request cannot match, or at least one named
order that is presentable -- the request proceeds as before.

**R59.4C.7** *Switch and logs.* There is no new switch. A requirement's own
switch removes its key from the required set (R59.4C.1), and with it the
effects of R59.4C.3, R59.4C.5 and R59.4C.6. A call that leaves out at least
one order logs one debug line for each pair concerned, naming the pair and
the number left out. No line is logged per order, and the event stream logs
none.

**R59.4C.8** *Coins that opt in later.* A coin that binds a capability under
R59.4A.4 supplies its required set through the source of R59.4C.1. This
section then applies to it without a change to the order-book code.

Rationale: the capability word is the only statement about a maker that
reaches a node before a swap, and it arrives with the order. Leaving the
order out where it is shown costs no message and needs nothing from the
maker. Dropping the order from the stored book instead would change the
maker's trie root on this node and start a synchronization that can never
succeed, so the rule acts on answers only.

> **Compatibility.** No message, trie leaf or relay answer changes, so a
> Komodo DeFi Framework 2.6.0-beta or GLEEC KDF node is not affected and
> cannot observe the rule. The answers of R59.4C.3 list fewer
> entries than the reference would for the same book; setting the
> requirement's switch to `false` (`require_v6_counterparty`,
> `require_suffix_counterparty`) restores the full list together with the
> matching behavior, and there is no way to list the left-out orders while
> the requirement is on. A node of another implementation still lists a
> Reloaded maker's order for such a coin, and its request still times out
> (R59.4A, compatibility note).

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
test of T4. *Status (2026-10-29):* closed. A failed negotiation now
penalises the maker like the other two outcomes, and T4 is implemented,
both halves included.

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
> 2026-10-28). **Exception (2026-11-01):** the cross-reference sentences
> appended to the divergence note of R59.6.1a, to R59.6.4 and to D2 were
> added by the KDF Spec Reader on 2026-11-01 and **passed the KDF Dirty
> Gate** on 2026-11-01 (PASS, measure 0.03) together with chapter 32 §32.9C
> (§59.11, addition of 2026-11-01).

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
> One consequence is recorded in [Chapter 32](32-orderbook-p2p-and-trie.md)
> §32.9C R-GH10: a stale entry of a maker whose keep-alives are lost but
> whose order messages arrive lives as long as those messages keep coming.

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
and R59.4.4. The window is per public key only: no pair and no order of
another maker expires on its own at the reference's release rows
([Chapter 32](32-orderbook-p2p-and-trie.md) §32.9C R-GH3); the per-pair
variant of the reference's later, unreleased change is recorded there as
R-GH6 item 7, and whether Reloaded adopts a form of it was ch. 32 §32.10
D12, decided 2026-11-01 (Decision BK, item (a)). Reloaded binds per-pair
liveness on the receiver's clock as ch. 32 §32.9C.2 R-GH14 (switch
`pair_liveness`, default on), with this window; PR #251 built it, merged
2026-11-05.

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
receipt clock. R59.6.5 is implemented in full (status as of 2026-10-29):
the eviction sweep exempts the persistent key and every per-order key
the node makes orders under. R59.6.7 is implemented (status as of
2026-10-29): the re-announcement of an own order missing from the read
index records the pair subscription, and keeps a subscription record
already present, as its tests show. R59.6.3 is a conclusion and carries
no implementation. R59.6.1a (added 2026-10-28) is implemented
(2026-10-28).

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

**T13.** *Protocol-information judgment (R59.4.7).* For a UTXO-family coin
with the `standard` format, the protocol-support judgment shall accept an
absent value, the encoded `standard` descriptor, and that descriptor followed
by arbitrary trailing bytes; it shall refuse an empty value and a value whose
first MessagePack item is not an address-format descriptor. For a coin with
the `segwit` format an absent value shall be refused. A refused `TakerRequest`
shall produce no `MakerReserved`, and a refused `MakerReserved` no
`TakerConnect`, with no outbound message of any other kind.

**T14.** *Capability words (§59.4A).*

- (a) *Writer.* For the set {`tx_v6`} the word is the one-entry map
  `"tx_v6"` → `true`, as fixed bytes. With two keys they are in
  ascending byte order. The first value's bytes come first, unchanged.
- (b) *Reader.* Each input yields the stated set:
  - the first value alone, as sent today: none;
  - the first value followed by {`tx_v6`: `true`}: {`tx_v6`};
  - the first value followed by {`tx_v6`: `false`}: none;
  - the first value followed by bytes that are not a map: none;
  - absent, and empty: none;
  - the first value truncated in the middle: none;
  - the word truncated in the middle: none;
  - {`tx_v6`: `true`, plus an unknown key} followed by trailing bytes:
    {`tx_v6`};
  - `tx_v6` with a non-boolean value: none;
  - `tx_v6` twice, once `true` and once `false`, in either order:
    {`tx_v6`};
  - an empty map as the word: none;
  - a first value that is an integer, a nested array, a map or an
    extension value is skipped whole, and the word after it is read.
- (c) *The first value reads the same (R59.4A.3).* With the word
  appended, the acceptance check of a coin that requires no capability,
  and the address format the order book reports, give the same results
  as without it. This holds for both of the node's readers of a UTXO
  address format.
- (d) *Opt-in (R59.4A.4).* The protocol information of a coin that does
  not opt in is byte for byte what it was before: a UTXO coin and one
  coin of another family.
- (e) *Carriage (R59.4A.5, R59.4A.7).* `MakerOrderCreated`,
  `TakerRequest` and `MakerReserved` carry the bytes unchanged through
  encoding and decoding, and so does the side channel of a
  synchronization answer. An order's trie root is the same with the word
  as without it.
- (f) *Decisions at the call sites.* With a test coin that requires a
  capability:
  - a reservation without it is not connected, and the info line of
    R59.4A.6 counts it;
  - a request without it is not reserved;
  - with the capability, both go ahead;
  - in no case is a swap started or a penalty record created.

**T15.** *Relay carriage of protocol information (§59.4B).* *(Passed
the KDF Dirty Gate with §59.4B on 2026-11-20. Reloaded-original test
obligations on the reference facts of that section.)*

- (a) *Unchanged in every answer (R59.4B.1, R59.4B.2).* A relay receives
  a `MakerOrderCreated` whose two protocol-information strings each hold
  a first value followed by further bytes. The answers to `GetOrderbook`,
  `SyncPubkeyOrderbookState` (as a full set and as a delta that adds the
  order), `BestOrders` and `BestOrdersByNumber` each hold an entry for
  the order with both strings byte for byte as sent.
- (b) *Entry for every order.* In each of the four answers, every order
  the answer contains has an entry, and an order a delta only removes
  has none.
- (c) *Missing entry on reading (R59.4B.4).* An answer that lacks the
  map, or lacks the entry for one order, is read without error; the
  order is kept with two empty strings, and no capability is declared
  for it (R59.4A.2).
- (d) *Update keeps the strings.* After an update message for the order,
  the relay's answers hold the same strings as before.
- (e) *Wire form (R59.4B.5).* The encoded entry holds each string as an
  array of integers, and a string sent in that form decodes to the same
  bytes.
- (f) *Trailing request member (R59.4B.6).* A `BestOrders` request and a
  `BestOrdersByNumber` request, each with one more member appended, are
  answered exactly as the same requests without it.

**T16.** *Orders that lack a required capability are not listed (§59.4C).*

- (a) *Required set (R59.4C.1).* A shielded coin inside the window of
  Chapter 39 R39.6.4f(b) requires {`tx_v6`}, and nothing with its switch
  off or without an Ironwood upgrade. A coin with a non-zero sighash
  suffix requires {`sighash_suffix`}, and nothing with its switch off. A
  plain UTXO coin and a coin of another family require nothing. For a
  coin whose Ironwood time or height is configured or built in, the
  activated coin and its configuration entry give the same set.
- (b) *Presentable (R59.4C.2).* For a coin with a required key: the first
  value followed by the word is presentable; the first value alone, empty
  information and a malformed word are not. For a pair whose coins require
  nothing, all four inputs are presentable. An order of the node's own is
  presentable with empty information.
- (c) *Parity with the acceptance check.* Over one table of coins and
  protocol-information values, an order is not presentable exactly when
  the coin's acceptance check declines that value for a capability cause.
- (d) *Surfaces (R59.4C.3).* With one presentable and two unpresentable
  orders on each side of a pair, the legacy and the mmrpc-2.0 `orderbook`
  list one ask and one bid, and their counts, totals and running sums are
  those of the listed orders. `best_orders` of both generations lists the
  presentable order only. The event stream emits nothing for the other
  two, on arrival, update and removal. `orderbook_depth` counts one ask
  and one bid for a pair the node holds.
- (e) *Unchanged (R59.4C.4).* For the same book, the answers to a peer's
  full-order-book, best-orders (by volume and by number) and depth
  requests contain all three orders, and the pair's trie root is the root
  of all three.
- (f) *Refill (R59.4C.5).* A relay answer that holds only unpresentable
  orders for a pair, while the node's book after the fetch holds a
  presentable one at a worse price, yields that order, by volume and by
  number. A relay answer that loses nothing triggers no fetch. A failed
  fetch yields the filtered relay answer.
- (g) *Early refusal (R59.4C.6).* `match_by` naming one held, unpresentable
  order is refused, and no taker order exists afterwards. It proceeds when
  the named order is not held, when it is the node's own, when it is an
  order of another pair or of the side the request cannot match, when one
  of two named orders is presentable, with `match_by` by public key, and
  without `match_by`. With two named orders that lack different
  capabilities, the refusal names those of the lower identifier.
- (h) *Switch (R59.4C.7).* With the requirement's switch off, (d), (f) and
  (g) give the results of a node without this section.

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
[Chapter 32](32-orderbook-p2p-and-trie.md) §32.7 and §32.9C: R-GH1 and
R-GH4 there bind the facts (a stale relay passes its view to every peer
that asks it, and no requester checks the root it reaches), and §32.10
D12 holds the decision (decided 2026-11-01).

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

**D5.** *A universal capability flag (Decision BO).* The capability word
is per coin, and for now it carries one capability of one coin family.
The maintainer regards it as temporary and decided not to show it in the
order-book answer (Decision BO, 2026-11-04). If capabilities keep being
needed, a node-level or message-level flag that holds for every coin is
the question to take up then, with its own analysis. The framing of
§59.4A is not meant to grow into that flag.

> **Status (2026-11-20).** The maintainer revised the decision on the
> order-book answer. The answer still carries no capability flag, but it no
> longer lists an order that lacks a capability the node requires (§59.4C).
> The universal flag stays the open question of this item.

**D6.** *Capability-aware best-orders request.* Under R59.4C.5 the node
repairs a thinned `best_orders` answer by fetching the pair's full book.
Letting the request itself carry the required capabilities, so that a relay
selects among presentable orders, would save that fetch. It changes a
peer-to-peer request, so it needs an established answer on how the
reference release rows treat the longer request, and is left for a later
release.

> **Status (2026-11-20, development line).** R59.4B.6 gives that answer: a
> relay at the reference rows ignores a member appended at the end of a
> best-orders request and answers as without it. A reference relay can
> therefore not be made to select among presentable orders, and the refill
> of R59.4C.5 stays necessary behind such a relay. What remains open is
> whether a Reloaded relay should honor such a member.

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
- *Status update (2026-10-29): R59.5.4 and R59.6.6.* R59.5.4 records that
  the failed-negotiation penalty and T4 are implemented. R59.6.6 is brought
  up to date with what this repository's integration branch implements for
  R59.6.5 and R59.6.7. *Inputs:* this repository's own code and tests only.
  No reference input: the rules themselves are unchanged, so no Dirty Gate
  pass is needed.
- *Scoped addition (2026-11-01) -- the cross-reference sentences appended to
  the divergence note of R59.6.1a, to R59.6.4 and to D2.* Authored by the
  KDF Spec Reader on 2026-11-01 as part of chapter 32 §32.9C; **KDF Dirty
  Gate: PASS on 2026-11-01, measure 0.03** (the inputs, the facts taken and
  the clearance are recorded in chapter 32 §32.13, scoped addition of
  2026-11-01). The sentences state no reference fact beyond those bound in
  chapter 32.
- *Scoped addition (2026-11-04) -- R59.4.7, T13 and the compatibility note
  under R59.4.7.* **KDF Dirty Gate: PASS on 2026-11-04, measure 0.03.** Prompted by the ARRR
  version-6 counterparty question of the 0.4.0 cycle (chapter 39 R39.5.14).
  *Inputs:* the forbidden corpus at the rows `v2.6.0-beta` and `v3.0.0-beta`
  of chapter 02 §2.15A, identical in every respect taken, consulted **only**
  to establish: what each coin family publishes as its per-coin protocol
  information; the two-arm acceptance rule of a UTXO-family coin and the
  decoder's tolerance of trailing bytes (a property of the pinned public
  MessagePack library release, confirmed in that library's published source);
  the two decision points; the silence of a refusal; the absence of any
  transaction-format capability in the matching and negotiation messages; and
  the orderbook's derivation of an entry's address from the same bytes. What
  crosses is behavior, dictated wire members (`base_protocol_info`,
  `rel_protocol_info`, the `format` key and its three values) and public RPC
  names; no private identifier, function body, helper decomposition, error,
  log or comment text was reproduced. The compatibility note rests on this
  repository's own code and on chapters 32 and 39. T13 is Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-04, measure
  0.03, no findings. *Extent:* the order-matching protocol-information
  publication and judgment and the orderbook's address derivation, at
  `v3.0.0-beta` with `v2.6.0-beta` as cross-check; for the whole batch, 30
  source files and one dependency manifest were read at `v3.0.0-beta`, 29 of
  them checked by hash against the tagged states of both rows. *Method:*
  side-by-side reading of the compared regions against every added or changed line,
  plus that hash check; the gate script was not run because the candidate has
  no code blocks. *Measure:* 0.03, read against the standard that any
  discretionary finding fails a file; dictated-interop and public-surface
  tokens were excluded from the measured set.
- *Scoped addition (2026-11-20, Spec Reader round 0.5.0-18) -- §59.4B
  (R59.4B.1--R59.4B.7 and its compatibility note), T15, and the status
  note at the end of §59.4A.* **KDF Dirty Gate: PASS on 2026-11-20, measure
  0.01, no findings** (chapter 01 R40; the attestation closes this item).
  Prompted by
  the plan to hide, at RPC presentation, orders whose protocol
  information lacks a required capability. *Inputs:* the forbidden corpus
  at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A,
  identical in every respect taken, with `dev (as of 2026-03-27)`,
  `staging (as of 2026-03-27)` and `main (as of 2026-10-07)` compared for
  R59.4B.7, consulted **only** to establish: how a relay stores the two
  protocol-information strings and from which messages; that the four
  answers carry an entry for every order they contain, with the strings
  whole; when the strings are empty; how a reader treats a missing map or
  entry; the wire form of the strings; the treatment of a member appended
  to a best-orders peer request and of an unknown member of the local
  `best_orders` request; and that no tabled state names the Ironwood
  upgrade, changes the pinned shielded-coin library, or publishes
  anything but the bare descriptor. The decoder tolerance and the array
  form of a byte string are properties of the pinned public MessagePack
  library release, confirmed in that library's published source. The
  statement about tags and branch positions on 2026-11-20 is reference
  metadata of the public repository (chapter 02 R13); no row was added,
  because every state read was already tabled. What crosses is behavior,
  dictated wire names (the four request kinds, `MakerOrderCreated`) and
  public RPC names; no private identifier, function body, helper
  decomposition, error, log or comment text was reproduced. The
  compatibility note and T15 are Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-20, measure
  0.01, no findings; the pointer in chapter 32 under R-W2a passed with this
  item. *Extent:* the order-matching peer-to-peer subject matter of
  §59.4B and chapter 32 §32.7.1: the peer request definitions, the maker
  order-creation message, the relay's storage of orders, the four answers
  (full orderbook, synchronization, best orders by volume and by number),
  the reading side and request decoding. The row `v3.0.0-beta` was read
  (the relevant parts of four of the six source files located);
  `v2.6.0-beta` by its full difference; `dev (as of 2026-03-27)`,
  `staging (as of 2026-03-27)` and `main (as of 2026-10-07)` by
  mechanical difference; all five rows by a whole-tree search for the
  upgrade name and the shielded-library pin record (row `librustzcash
  k-1.4.2`). The decoder statements of the public MessagePack library and
  the listing of the live public repository are outside the gate's
  extent; the listing was checked against the reading clone's remote
  references of 2026-11-18. *Method:* reading plus mechanical comparison
  (a per-token forced-versus-free inventory and version-control
  differences); the repository's clean-room gate tool was not applicable
  because the additions embed no code blocks. *Measure:* 0.01, on the
  discretionary-expression basis.
- *Reloaded-original addition (2026-11-04, Decisions BM and BO): §59.4A
  (R59.4A.1–R59.4A.7), T14 and D5.* The capability word in coin protocol
  information, read by the coin's acceptance check, with no switch on the
  sending side. *Inputs:*
  - this repository's own order-match messages, protocol-information
    producers and acceptance checks;
  - the source of the public MessagePack library this repository uses,
    for how values are skipped and decoded;
  - the maintainer's decisions.

  The forbidden corpus was not consulted for this item. Its compatibility
  note states no reference fact beyond R59.4.7 and chapter 39 R39.5.14,
  which the KDF Spec Reader established and which passed the KDF Dirty Gate
  (batch 0.4.0-2, 2026-11-04). It needs no Dirty Gate pass of its own.
- *Reloaded-original addition (2026-11-05) -- the pointer to Chapter 28
  R16a under R59.4A.2.* Maintainer decision CD. Written from this
  repository's own decode paths. The forbidden corpus was not consulted,
  and it needs no Dirty Gate pass.
- *Reloaded-original addition (2026-11-05) -- the pointer to ch. 32 R-GH14
  under R59.6.4.* No forbidden-class input; it needs no Dirty Gate pass.
- *Reloaded-original addition (2026-11-20, by the maintainer's decisions of
  that day): §59.4C (R59.4C.1–R59.4C.8), T16, D6, the status note under D5
  and the sentence in §59.0.* Orders that lack a capability the node
  requires are left out of the order-book answers, with the requirement's
  own switch as the opt-back. *Inputs:* this repository's own order-book,
  best-orders, event-stream and taker-order code, its acceptance checks, a
  lab run of this project's own node against a reference binary as a black
  box (reported as the node's own log lines), and the maintainer's
  decisions.
  The forbidden corpus was not consulted for this item. Its compatibility
  note states no reference fact beyond §59.4A. It needs no Dirty Gate pass.
- *Reloaded-original additions on the development line (2026-11-20): the
  status note under D6, the sentence on a changed required set in R59.4C.3,
  the pointer to §59.4B in the introduction of §59.4C, and the wording "the
  answers of R59.4C.3" in the compatibility note of §59.4C.* Written from
  this repository's own code and from the cleared R59.4B.6. No further
  forbidden-class input; they need no Dirty Gate pass.
