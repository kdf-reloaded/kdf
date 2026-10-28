# Chapter 60 — Per-Coin Trading Floors: Minimum Trading Volume, Minimum Transaction Amount, and Where They Refuse or Fail a Trade

**Status:** driving-spec (mixed treatment — see §60.0). **Gated by the
KDF Dirty Gate in five passes on 2026-10-25, PASS** (see §60.12 for the
extent and stated gaps).

> **One-sentence claim:** every coin the project can trade shall report
> two exact per-coin floors — a *minimum trading volume* that bounds the
> orders the node itself creates and becomes the default minimum volume it
> advertises, and a *minimum transaction amount* that floors the DEX fee
> both swap peers compute independently — with the per-family values of
> §60.3, consumed exactly where §60.4 says and nowhere else; and the
> project shall know, and test, every point (§60.5) at which a floor, a
> dust limit, or a fee deducted from a payment makes a peer refuse a trade
> or makes a started swap fail.

## 60.0 Executive Summary, subsystem shape, and treatment

*Executive summary.* Every coin reports two floors. The minimum trading
volume limits the orders the node builds and becomes the default
`min_volume` it advertises; a maker answers no request below an order's
minimum volume. The minimum transaction amount floors the DEX fee, which
each peer computes on its own and the maker validates, so two nodes must
agree on it for the taker coin. No protocol refuses a swap at negotiation
for its size. A trade too small for the fees taken out of its payments
fails later, when the coin layer builds a spend. For a fixed-fee UTXO
coin the reference sets the floor at ten times the spend fee, so this
cannot happen. For a dynamic-fee UTXO coin, and for Sia, it still can.

*Subsystem shape.* The values belong to the coin layer, one pair per
coin family (`MarketCoinOps`-level accessors of each family's coin type).
The order-match layer (order builders, the maker's request handling,
balance-driven cancellation) and the swap layer (the DEX fee pipeline of
chapter 08, the maximum-volume computations) consume them. Nothing else
does.

*Treatment.* This chapter is new. It binds a contract that spans every coin family and
the order-match and swap layers, so it does not fit any one coin
chapter. Each family's *values* are stated in the chapter that owns that
coin layer; this chapter collects them in one table (§60.3), binds how the
order-match and swap layers consume them (§60.4), and enumerates the
peer-visible effects (§60.5).

- **§60.2–§60.4 (T-DOC with bound corrections).** The two accessors and
  their consumption are as-built baseline behaviour in reloaded, except
  for the per-family value gaps of §60.3 and §60.6.
- **§60.5 (T-DOC, normative).** The refusal and failure map. Most points
  are as-built; §60.6 lists the ones that are not.
- **§60.6 (T-PORT).** The gaps reloaded must close, each stated as a
  requirement.

> **Binding scope (R60.0).** This chapter binds observable values (what
> the `min_trading_vol` RPCs report; the default `min_volume` a maker
> advertises; the DEX fee amount a peer validates), the order and swap
> refusals and failures a peer or the operator can observe, and the
> public coins-configuration keys that feed them. It does not bind
> private types, helper decomposition, error wording or module layout
> (chapter 01 R11, chapter 00 R6).

## 60.1 Reference designations

| Designation | Role in this chapter |
| --- | --- |
| `v2.6.0-beta` | The legacy behavioural reference for netid `8762` (AGENTS.md §3). Every reference rule here holds at this designation unless stated otherwise. |
| `v3.0.0-beta` | The newest public release of the v3 lineage, the reference for netid `6133`. **Every rule in this chapter is identical at `v2.6.0-beta` and `v3.0.0-beta`**; no `NetConfig` selection is involved anywhere in this chapter. No later unreleased v3 state is covered by this chapter's clearance (§60.12). |
| `v2.0.0-beta` | First designation at which the EVM and QRC20 minimum trading volume is one smallest unit (chapter 57 §57.1). |
| `v2.4.0-beta` | First designation whose UTXO HTLC-spend fee estimate uses a 496-byte reference size (chapter 15 R40, chapter 16 R14A). |
| `v2.5.0-beta` | First designation without the baseline's fixed Bitcoin minimum trading volume (§60.3 row U1, chapter 38 R38.6.6f). |
| `v2.6.0-beta` (again) | First designation with the fee-derived UTXO floor, the whole-kilobyte fixed-fee option and proportional plain fixed fees (chapter 38 R38.6.6a–e). |

Facts about the reference are stated against these public designations
only; no post-anchor commit identifier or branch state is named
(chapter 01 R20).

## 60.2 The two accessors

**R60.2.1** *Two floors, two purposes.* Every tradable coin exposes two
per-coin values, each an exact rational number of whole coin units
(never floating point):

- the **minimum trading volume** (the public `min_trading_vol` coin
  method, and the legacy and version-2 `min_trading_vol` RPCs that report
  it) — the smallest base or rel amount the node's own order builders
  accept, and the source of the default minimum volume of the node's own
  orders (§60.4);
- the **minimum transaction amount** (the public `min_tx_amount` coin
  method) — the floor of the DEX fee of a trade in which this coin is the
  taker coin ([Chapter 08](08-fee-routing-engine.md) R6 step 3, R11), and
  the floor the maximum-volume computations subtract and compare against.

The two are not interchangeable. A family may set them equal (SLP,
Tendermint, Sia, Lightning) or not (UTXO: ten dust or more against one
dust; EVM: one smallest unit against zero).

**R60.2.2** *Deterministic for the life of an order and a swap.* Both
values shall be functions of the coin's configuration (and, for Lightning
only, of its open channels, R60.3 row L1), not of a fee estimate or any
other network state that changes while an order is open or a swap runs.
The reference chose a dust-based floor for dynamic-fee UTXO coins for
exactly this reason; §60.5 finding F1 records its cost.

**R60.2.3** *Always answered.* The `min_trading_vol`
RPCs and every order builder shall return a value or an orderly error for
every coin that can be activated. A coin family that cannot trade shall
refuse order creation with an error, never terminate the request
abnormally (finding F5, F6).

## 60.3 Per-family values

The owning chapter is the binding home of each row; this table is the
index. "Both" means `v2.6.0-beta` and `v3.0.0-beta`, which agree on every
row. *D* is the coin's dust amount and *r* its configured fixed fee per
kilobyte, both in base units, as chapter 38 R38.6.6a and R38.6.6d define
them.

| Row | Family | Min trading volume (both designations) | Min transaction amount (both) | Owner | Reloaded today |
| --- | --- | --- | --- | --- | --- |
| U1 | UTXO standard, BCH, Qtum platform | `max(10·D, 10·F)` base units, F the 496-byte spend fee under the coin's fixed-fee policy, zero for dynamic-fee coins; no ticker exception | `D` | ch. 38 R38.6.6 | Partly: ticker `BTC` fixed at 0.00777; F rounded up; no whole-kB option (ch. 38 R38.6.6i) |
| U2 | SLP tokens | one smallest token unit, 10^-decimals | one smallest token unit | ch. 38 R38.6.6g | Matches |
| Z1 | ZHTLC shielded (ARRR family) | the U1 rule over the coin's transparent fee and dust fields (ARRR: 0.0001) | `D` (ARRR: 0.00001) | ch. 39 R39.5.2 | Matches (value); the fee-option gap of U1 applies only if a ZHTLC entry ever declares one |
| Q1 | QRC20 tokens | 10^-decimals | 0 | ch. 57 R57.7A.6 | No: 10^-(decimals ÷ 3, integer) — PR #66 pending |
| E1 | EVM platform coins and ERC-20 tokens | 10^-decimals | 0 | ch. 57 R57.7A.1, R57.7A.3 | No: as Q1 — PR #66 pending |
| T1 | TRON (TRX) and TRC20 | 10^-decimals (the E1 rule; TRON coins are cases of the EVM coin family in both trees) | 0 | ch. 21 §21.11A | No: as Q1 (TRX 10^-2 against 10^-6); swap participation itself is deferred (ch. 21) |
| C1 | Tendermint platform coins and their tokens | one base unit, 10^-decimals of the coin or token | one base unit | ch. 18 §18.5.2 | Matches |
| S1 | Sia | one hasting, 10^-24 SC | one hasting | ch. 20 R-S11 | Matches (finding F2 applies to both trees) |
| L1 | Lightning | equal to the minimum transaction amount | the largest, over the node's open channels, of the counterparty's minimum outbound HTLC amount (one millisatoshi where a channel does not state one, and one millisatoshi with no channels), in the coin's own decimals | ch. 41 §41.11A | No: both accessors terminate abnormally (finding F5) |
| P1 | Solana and SPL tokens | none: both designations terminate the request abnormally (finding F6) | one lamport, 10^-9, for SOL **and** for every SPL token regardless of the token's decimals | ch. 40 §40.6A | Differs, with no parity target: 0.00777 and 0 |

**R60.3.1** *Where the rows meet the wire.* Rows whose family can swap
in both trees (U1, U2, Z1, Q1, E1, C1, S1) are parity data: a reloaded
node shall report exactly the reference value. Rows for families that
cannot swap in one or both trees (T1, L1, P1) bind only R60.2.3 and the
value given; no peer can observe them through a swap.

> **As-built note (2026-10-25, clean-side record).** After PR #66 the
> "Reloaded today" column reads as follows. U1: the `BTC` exception is
> gone and the fee term is truncated; the whole-kilobyte option is still
> missing. Q1, E1 and T1 match. L1 matches, and Lightning pairs are refused
> while Lightning trading is unsupported. P1 keeps its values, and Solana
> and SPL pairs are refused the same way (R60.6.5). The cleared table above
> is unchanged. After PR #72, U1 matches as well, including the
> whole-kilobyte option (R60.6.2).

## 60.4 Consumption contract

**R60.4.1** *One accessor, every path.* The minimum trading volume of a
coin is consumed on exactly the paths of chapter 57 R57.7A.4 — maker order
creation and update, taker order creation, `trade_preimage`, and the two
`min_trading_vol` RPCs — for every family, not only EVM. No path carries
its own family-specific floor.

**R60.4.2** *Default-and-refusal arithmetic (order builders).* With
`b` the base coin's and `q` the rel coin's minimum trading volume and `p`
the order price (rel per base), the node's own order builders shall:

1. refuse a maker order whose maximum base volume is below `b`, or whose
   maximum base volume times `p` is below `q`;
2. refuse a taker order whose base amount is below `b` or whose rel amount
   is below `q`;
3. give an order with no operator-set minimum volume the default
   `max(b, q ÷ p)` (for a taker order, `p` is its rel amount over its base
   amount), and refuse an operator-set minimum below that default;
4. refuse a maker price that is not strictly positive (the reference
   since `v2.0.0-beta`; reloaded still refuses below 10^-8, chapter 57 D7).

All arithmetic is exact-rational. This is behaviour at the baseline anchor (R1),
unchanged in the reference at every designation of §60.1 (chapter 57 V4);
chapter 57 D7 named "whichever order-match chapter comes to own order
validation" as its home, and this chapter is that home for the floors.

**R60.4.3** *The order's minimum volume is what governs everything after
creation.* Once an order exists, the coin floors are no longer consulted
for it; its own minimum base volume (operator-set, or the default of
R60.4.2 step 3) is. A maker shall:

- match an incoming taker request only if the matched base volume is at
  least its order's minimum base volume (and within its available
  volume, at an acceptable price);
- cancel its order, announcing the cancellation to the network, when its
  spendable balance falls below the order's minimum base volume;
- close its order when, after a fill, the available volume is below the
  order's minimum base volume and no match is in progress.

**R60.4.4** *The minimum transaction amount.* It is consumed by the DEX
fee pipeline of the taker coin ([Chapter 08](08-fee-routing-engine.md) R6
step 3, R8, R11; [Chapter 16](16-swap-v2-pre-burn-output.md) R9, R10) and
by `max_taker_vol` / `max_maker_vol`, and nowhere else on the trading
path. It is **not** consulted at order creation and **not** by a peer's
order validation.

**R60.4.5** *Balance watch for maker orders (passed the KDF Dirty Gate on
2026-10-26; see §60.12).* The node watches a coin's balance only to keep its own maker
orders' maximum volume honest.
1. *Which coins.* A coin's balance is polled only while the node has at
   least one maker order with that coin as base. The watch starts when the
   first such order is created, seeded with the balance read at creation,
   and stops when the last one is cancelled, matched away or expires. A
   coin that is only activated, or only appears as rel, is not polled for
   this purpose.
2. *Interval.* The poll interval is the 30 s of R60.4.6 item 1, and no
   shorter.
3. *Work per poll.* A poll reads the balance only. The maximum maker volume,
   and the fee preview it needs, are recomputed only when the balance
   differs from the last value seen, and only if a maker order with that
   base still exists.
4. *Peer effect.* None beyond the existing R60.5.1 order updates, which are
   sent only when an order's volume actually changes.

*Rationale.* Each Electrum session is served about one request message per
second once its burst is spent (chapter 38 R38.6.3a). Polling every
activated coin every 10 s spends a fifth of a legacy-address coin's budget
on balance reads that no maker order needs, and it competes with order
posting on the same session.

**R60.4.6** *Balance-watch interval and restored orders (closes D60.4;
authored by the KDF Spec Reader 2026-10-27, passed the KDF Dirty Gate on 2026-10-27; see
§60.12).* Every statement here holds identically at `v2.6.0-beta` and
`v3.0.0-beta`.

1. *Interval.* The watch of R60.4.5 sleeps **30 seconds** between polls.
   The value is fixed in the build: it is not read from the daemon
   configuration and no operator override exists. The sleep comes first,
   so a watch started at time *t* performs its first balance read at
   *t* + 30 s. (D60.4's own lower bound of 10 s is met with margin.)
2. *Failed poll.* A poll whose balance read fails changes nothing: the
   last-seen value is kept, no recomputation runs, no order is touched,
   and the next poll follows after the ordinary interval.
3. *Seed at creation.* A watch started by maker-order creation is seeded
   with the balance the creation itself read while checking the order
   (R60.4.5 item 1); its first poll recomputes only if the balance has
   moved since.
4. *Restored and converted orders are unseeded.* A maker order restored
   from storage at node start, and a maker order produced by converting a
   timed-out taker order (chapter 59 R59.4.1), start the watch for their
   base coin — if none is running — **without** a seed: no balance is
   read at insertion. The watch's first poll, 30 s later, treats the
   value it reads as a change and runs the recomputation of R60.4.5
   item 3 once; where the balance has not changed since the reconciliation of item 5
   (restored orders) or since the order was sized (converted orders), that
   recomputation finds nothing to lower, so it changes no order and sends
   nothing; this follows from the reconciliation having run and is not a
   separately observed behavior. From then on the watch behaves as for any
   other order.
5. *Fresh read on re-announcement.* Independently of the watch, when the
   periodic order-matching maintenance finds one of the node's own maker
   orders absent from the local orderbook — the state of every restored
   order on the first maintenance pass after start, and of every order
   under a public key evicted by chapter 59 R59.6.4 — it reads the base
   coin's spendable balance afresh and recomputes the maximum maker
   volume for it; lowers the order's maximum volume to its reserved
   amount plus that maximum when the maximum is below the order's
   available amount (it never raises it); cancels the order with the
   insufficient-balance reason when the read or the recomputation fails,
   or when the available amount is then below the order's minimum
   volume; and otherwise re-announces the order (chapter 59 R59.6.7). A
   restored order is thus reconciled with the live balance on the first
   maintenance pass, before the watch's first poll. This path is
   baseline behavior (V60.2); items 1–4 are post-baseline.

## 60.5 Refusal and failure map — what a peer observes

This section answers whether a floor is a wire parameter. It is, in two
places, and neither is a refusal message: the advertised minimum volume
(W1, W2) and the DEX fee floor (W4). Every other effect is local or
appears only as a failed swap.

**R60.5.1 (W1) — advertised minimum.** The default minimum volume of
R60.4.2 step 3 travels in the order record's `min_volume` field
([Chapter 32](32-orderbook-p2p-and-trie.md) R-W1) and is what every peer
sees in `orderbook` and `best_orders`. It embeds the maker's floors of
**both** coins of the pair.

**R60.5.2 (W2) — silent non-reservation.** A taker request whose matched
base volume is below the maker order's minimum base volume (R60.4.3)
receives **no reply**: the maker sends no reserved message and no refusal
of any kind. The taker observes only the absence of a reservation; its
order then follows its own time-in-force (a good-till-cancelled taker
order becomes a maker order after its matching window, a fill-or-kill
order is cancelled). This is the only way a node's floors reach a peer's
*request*. A peer that respects the advertised minimum of W1 is never
refused on volume.

**R60.5.3 (W3) — no floor check on a received order.** A node shall not
apply its own floors to an order record received from a peer. The only
volume-related validation a received record undergoes is that its
minimum volume is non-negative and its price and maximum volume are
positive ([Chapter 32](32-orderbook-p2p-and-trie.md) §32.5). A taker does
not compare a reservation against its own coin floors either: it accepts
a reservation whose volumes match its request. Consequently a node whose
floor for a coin is *higher* than its peer's refuses, locally, to build a
taker order that the peer's advertised minimum would accept; a node whose
floor is *lower* advertises minima its peers accept.

**R60.5.4 (W4) — the DEX fee floor is negotiated implicitly.** Both peers
compute the DEX fee of a trade from the taker coin's minimum transaction
amount on their **own** node (chapter 08 R6). When the fee rate times the
trade amount is below that amount, the fee *is* that amount. The maker
validates the taker's fee against its own figure:

- *legacy protocol:* for a UTXO-family taker coin the fee output must be
  at least the maker's figure; for an SLP, Tendermint or Sia taker coin
  the fee amount must equal it exactly. A taker whose figure is lower —
  or, for the exact-match families, differs at all — fails the check: the
  maker records the taker-fee validation failure and ends the swap, and
  the taker has already paid the fee (chapter 51 R40 penalises the
  taker's key for one hour). For a UTXO-family taker coin, a taker whose
  figure is higher overpays and the swap proceeds;
- *version-two protocol (UTXO taker coin):* the taker-funding output must
  equal the dex fee plus premium plus trading volume exactly
  ([Chapter 15](15-swap-v2-utxo-path.md) R17). (For an EVM taker coin the
  minimum transaction amount is zero on both sides, so the question does
  not arise.) Any difference aborts the swap
  on the maker side before the maker pays; the taker reclaims its funding
  through the funding refund path of [Chapter 52](52-swap-v2-state-machine.md).

Two nodes therefore agree on a small trade only if they agree on the taker
coin's minimum transaction amount. For every family that can swap
(§60.3), reloaded's value equals the reference's; the requirement is
that it stays so (R60.6.6).

**R60.5.5 (W5) — no volume refusal at negotiation.** Neither protocol
refuses a swap at negotiation or start because its volume, or its volume
net of any fee, is below a floor or a dust limit. The legacy maker's
seven refusal conditions ([Chapter 51](51-legacy-v1-swap-state-machine.md)
R34) contain none, and the version-two state machines have no
volume-based abort. A swap whose volume is too small to complete is
detected only by the coin layer, when a transaction is built.

**R60.5.6 (W6) — coin-layer failure points.** The coin layer refuses to
build or spend in these cases, at both designations; the outcome column
is what the parties observe:

| # | Who, when | Condition that refuses | Outcome |
| --- | --- | --- | --- |
| 1 | Taker, sending the DEX fee | an output (fee leg, or a burn leg that is not an `OP_RETURN`) below the coin's dust | Cannot happen for a fee the chapter-08 pipeline produced: every leg other than an `OP_RETURN` burn leg is at least the minimum transaction amount, which is the dust for UTXO coins. |
| 2 | Either side, sending its payment | the payment output below the coin's dust | Cannot happen for an order at or above the floor of U1 (ten dust or more). |
| 3 | Taker (legacy), spending the maker payment | the estimated spend fee is at least the payment value | Taker records a maker-payment spend failure and enters its refund chain, but its own payment has already been spent by the maker, so its refund fails; the maker's swap completes normally. The maker payment stays in its HTLC: only the maker can reclaim it after its lock time, and its completed swap does not do so by itself. **The taker loses the trade.** |
| 4 | Maker (legacy), spending the taker payment | as row 3 | Maker records a taker-payment spend failure and refunds its own payment after its lock time; the taker, seeing no spend, refunds — but row 5 refuses the taker's refund for the same reason, so the taker payment stays locked. |
| 5 | Either side, refunding its own payment | as row 3 | The refund is refused; the payment stays locked. |
| 6 | Version-two maker, generating the funding-spend preimage after validating the funding ([Chapter 52](52-swap-v2-state-machine.md) R17 step 3) | the funding value is below the spend fee plus dust (chapter 15 R21) | The maker aborts before paying, with nothing of its own committed; the taker reclaims its funding through the funding refund path. |
| 7 | Version-two maker, finalising the taker-payment spend | `Standard` fee and the payment is below spend fee plus dust plus DEX fee (chapter 16 R18) | The maker cannot spend; refund paths of chapter 52 apply. |
| 8 | Sia (both protocols), any HTLC spend or refund | the fixed 0.00002 SC miner fee every Sia HTLC spend and refund deducts is not smaller than the HTLC value | No valid spend or refund can be built (finding F2). |

The spend fee in rows 3–7 is the 496-byte estimate of chapter 16 R14A
under the coin's fee policy of chapter 38 R38.6.6b; a dynamic-fee coin's
legacy spend uses its current estimate without any volatility increase.
For a fixed-fee UTXO coin the floor of row U1 guarantees rows 3–7 cannot
trigger for an order at or above the floor (the payment is at least ten
spend fees), unless the coin forces a minimum relay fee above its
configured rate, which the floor does not take into account. For a
dynamic-fee coin it guarantees nothing (finding F1).

> **Code-quality finding F1 (informative; explicit look requested).** In
> both reference designations the floor of a dynamic-fee UTXO coin is ten
> dust and does not depend on fees. For `BTC` and `BTC-segwit` (dynamic
> fee, no configured dust) that is 0.0001 BTC = 10,000 satoshi, while the
> legacy spend fee estimate is the fee rate times 496 bytes. At a rate of
> about 19,100 satoshi per kilobyte (roughly 19 sat/vB) the spend of a
> floor-sized payment leaves less than standard dust and is not relayed;
> from about 20,200 satoshi per kilobyte it is refused outright (row 3).
> A floor-sized BTC trade therefore loses the taker's side whenever fees
> are moderately high. Reloaded is shielded for the exact ticker `BTC` by
> its baseline 0.00777 floor (about 2.5 million satoshi per kilobyte
> before row 3, with its own 305-byte estimate) but not for `BTC-segwit`,
> whose floor is the same 0.0001 as the reference's — so reloaded
> carries the two Bitcoin tickers at floors 77.7 times apart. The
> reference, and reloaded once PR #66 lands, also put a Bitcoin floor of
> 0.0001 (reloaded: 0.00777) beside a wrapped-Bitcoin EVM token's 10^-8
> (row E1); reloaded today has the inverse oddity, a wrapped-Bitcoin floor
> of 0.01 above its native-Bitcoin floor. *Proposed remedy (maintainer's choice; none
> needs a compatibility switch, because W2 and W3 make any floor local
> policy):* (a) keep parity (R60.6.1) and add a local pre-trade check that
> refuses, at order creation, `trade_preimage` and swap start, a volume
> whose current legacy spend estimate would leave less than the dust; or
> (b) let the operator raise the floor through the coin's `dust` value,
> which the public coins file can already carry. *(Trigger: explicit
> request to look for a floor below what fee deductions allow.)*

> **Code-quality finding F2 (informative; explicit look requested).** In
> both reference designations and in reloaded, every Sia HTLC spend and
> refund deducts a fixed 0.00002 SC miner fee from the HTLC value, while the
> Sia floors (row S1) are one hasting. A Sia payment of 0.00002 SC or less —
> accepted by every order builder — can be neither spent nor refunded,
> and the arithmetic does not refuse cleanly beforehand. *Proposed
> remedy:* refuse the spend or refund with an orderly error when the
> value does not exceed the fee, and treat a parity-preserving local
> pre-trade check as in F1(a). Raising reloaded's Sia floor would be a
> one-sided local divergence (W3), not a wire break, but it is a
> maintainer's decision. *(Trigger: explicit request.)*

> **Code-quality finding F3 (informative; this repository's code).**
> Reloaded's legacy HTLC spend and refund builders, and its version-two
> maker-payment spend and refund builder, subtract the spend fee from the
> HTLC value without first checking that the fee is smaller; with the
> workspace's overflow checks off the result wraps, and the node
> broadcasts a transaction carrying an out-of-range output value, which
> the coin daemon rejects. The reference refuses these spends cleanly
> when the fee is not smaller than the value (since `v1.0.0-beta`). The
> visible outcome of rows 3–5 is the same failure, but reloaded reaches it
> by broadcasting garbage. *Proposed fix:* refuse with a spend-failure
> error when the fee is not smaller than the HTLC value, on all of these
> builders (R60.6.4). Separately, reloaded's version-two funding-spend
> builder refuses only when the funding value does not exceed the fee,
> where chapter 15 R21 requires fee plus dust. *(Trigger: apparent while
> reading side by side.)*

> **Code-quality finding F4 (informative; this repository's code).**
> Reloaded's legacy HTLC spend estimate uses a 305-byte reference size
> and rounds a fixed-fee coin's estimate up to a whole kilobyte, so a
> plain fixed-fee coin pays its whole per-kilobyte fee on every spend
> (KMD: 1,000 base units against the reference's 496), and a dynamic-fee
> coin's estimate is 305/496 of the reference's. Neither is validated by a
> peer in the legacy protocol, so this is overpayment and a
> slightly lower dynamic-fee margin, not a wire break. Chapter 38
> R38.6.6c binds the reference rule.

> **Code-quality finding F5 (informative; this repository's code).**
> Reloaded's Lightning coin implements neither accessor: both are
> placeholders that terminate the calling request abnormally. Lightning
> activates in reloaded ([Chapter 41](41-lightning-network.md)), so a
> `min_trading_vol` request for an active Lightning coin, or any
> `setprice`, `buy`, `sell` or `trade_preimage` naming one, ends without
> an orderly response. *Proposed fix:* R60.6.5. *(Trigger: a placeholder
> stating its own incompleteness, read while establishing row L1.)*

> **Code-quality finding F6 (informative; explicit look requested).** In
> both reference designations the Solana platform coin and the SPL token
> type leave the minimum trading volume unimplemented, so the
> reference's own `min_trading_vol` RPC and order builders terminate
> abnormally for them; and an SPL token's minimum transaction amount is
> the platform's one lamport expressed in SOL's nine decimals, whatever
> the token's own decimals. Neither tree can swap Solana today, so there
> is no parity target; reloaded's baseline 0.00777 for both is
> decimal-unaware but harmless. *(Trigger: explicit request; the
> placeholder is also self-describing.)*

## 60.6 Requirements for reloaded (gaps)

**R60.6.1** *UTXO floor parity.* Reloaded shall compute the UTXO minimum
trading volume exactly as chapter 38 R38.6.6e binds: no ticker exception
(remove the 0.00777 `BTC` value), truncation inside the fee term (not
rounding up), and the whole-kilobyte option's fee term. No compatibility
switch (W3). The maintainer decides whether finding F1's remedy ships
with it.

**R60.6.2** *Fixed-fee arithmetic parity.* Reloaded shall implement the
`dingo_fee` whole-kilobyte option and price plain fixed-fee coins
proportionally (chapter 38 R38.6.6a–c). This is peer-visible in one place:
a version-two swap whose taker coin carries the whole-kilobyte option,
where both peers must compute the same spend fee (chapter 16 R14A, R17).

**R60.6.3** *EVM, QRC20 and TRON.* Chapter 57 R57.7A.1 and R57.7A.6 (PR
#66) close rows E1 and Q1; because TRON coins are cases of the EVM coin
family, the same change closes row T1's value.

**R60.6.4** *Clean spend refusal.* Every UTXO-family HTLC spend and refund
builder shall refuse, with an orderly error and without broadcasting,
when the spend fee is not smaller than the HTLC value; the version-two
funding-spend builder shall refuse when the funding value is below the
fee plus dust (finding F3). The Sia spend and refund builders shall
likewise refuse when the fixed miner fee is not smaller than the HTLC
value ([Chapter 20](20-siacoin-integration.md) R-S11, finding F2).

**R60.6.5** *No abnormal termination.* Lightning (row L1) shall report the
reference value; until Lightning trading exists the order builders shall
refuse a Lightning pair with an error (R60.2.3). Reloaded's Solana values
(row P1) may stay as they are, since no tree can swap Solana, but no
Solana request shall terminate abnormally.

**R60.6.6** *Minimum transaction amount is frozen parity data.* For every
family that can swap, reloaded's minimum transaction amount shall equal
the reference's (§60.3). Changing it for any such family is a wire change
(W4) and needs the compatibility-switch treatment of
`docs/COMPAT_SWITCHES.md`.

**R60.6.7** *Pre-trade spend-safety check (clean-room-original;
maintainer decision of 2026-10-25, resolving D60.1 and D60.2).* This is
reloaded's own local policy, not reference behaviour: the reference
accepts the orders this rule refuses, and the swap then fails in the
coin layer (§60.5 R60.5.6 rows 3–5 and 8, findings F1 and F2).

1. *Scope.* The rule covers every coin whose HTLC spend and refund pay
   their fee out of the HTLC value: the UTXO coins of chapter 38
   R38.6.6g except SLP and QRC20 tokens (that is, the standard UTXO
   coin, the BCH platform coin, the Qtum platform coin and the
   transparent HTLCs of a ZHTLC coin), and Sia. Other families pay their
   spend fee outside the HTLC value and are not covered.
2. *Safe amount.* For a covered coin, the safe amount `S` is the
   smallest HTLC value whose spend leaves a valid output:
   - for a UTXO-family coin, the HTLC spend-fee estimate of chapter 38
     R38.6.6c under the coin's fee policy at the moment of the check —
     the current estimated rate for a dynamic-fee coin, the minimum relay
     rate where the coin forces it — plus the coin's dust `D` of R38.6.6d;
   - for Sia, the fixed HTLC miner fee of R60.5.6 row 8 plus one hasting.
3. *Where it is checked.* When the node creates or updates an order of
   its own (`setprice`, `update_maker_order`, `buy`, `sell`) and when it
   answers `trade_preimage`, the smallest trade the order can produce
   shall be safe on both legs: the order's minimum volume of the base
   coin shall be at least the base coin's `S`, and that volume times the
   order's price at least the rel coin's `S`, for each leg whose coin is
   covered.

   **Amendment (Reloaded-original, maintainer Decision P of 2026-10-27; passed the KDF Dirty Gate on 2026-10-27).**
   Item 3 applies when an order is created (`setprice`, `buy`, `sell`)
   and in `trade_preimage`. On `update_maker_order` it applies only when
   the update changes the order's volume (a volume delta, or `max`). An
   update that changes only the price, the minimum volume or the
   confirmation settings is not re-checked. *Accepted cost:* such an update
   can leave the smallest trade below `S` without a refusal, in the same way
   as item 5's long-lived order whose fees rose after it was placed.
   *Rationale:* a price-only update needs no server call otherwise. Each
   Electrum session is served about one request message per second once its
   burst is spent (chapter 38 R38.6.3a), so every fee-rate read the check
   adds costs a market maker about a second per update.
4. *Refusal.* A request that fails item 3 is refused with the same typed
   volume-too-low refusal the floors produce (R60.4.2), naming the coin
   and carrying `S` as the threshold; `trade_preimage` reports the same
   refusal. An order with no operator-set minimum volume is refused, not
   silently raised: this rule never changes the default minimum volume a
   maker advertises (R60.5.1, W1).
5. *Nothing a peer can observe.* The check never runs on a received
   order or request, never makes a maker withhold a reservation, and
   never aborts a started swap; W1–W5 are unchanged, and the floors of
   §60.3 stay at parity. A long-lived maker order is not re-checked when
   fees rise after it was placed.
6. *Estimate unavailable.* If the current fee estimate of a dynamic-fee
   coin cannot be obtained, the request is refused with a typed error; no
   placeholder estimate is substituted.
7. *Switch.* The MM2.json boolean `pre_trade_spend_check`, default
   `true`, enables the rule; `false` restores the reference behaviour
   (no check). Under `docs/COMPAT_SWITCHES.md`, "The default-side rule",
   this is arm 2: it has no wire effect, affects only the node's own
   orders, and improves fund safety. The setting is documented next to
   itself and in `docs/GLEEC_COMPATIBILITY.md`, as that document
   requires.

   > **Compatibility:** for behaviour matching GLEEC KDF, set
   > `pre_trade_spend_check` to `false`. GLEEC KDF has no such check: it
   > accepts these orders, and the swap then fails in the coin layer.

8. *Fee-rate reuse (Reloaded-original, Decision P; passed the KDF Dirty Gate on 2026-10-27).* Where the
   check reads a coin's current fee rate, that same reading is used by the
   trade-fee calculation of the same request. A request reads each coin's
   fee rate at most once (chapter 38 R38.6.6j, chapter 57 R57.2.6).

> **As-built note (2026-10-27, clean-side record).** The Amendment under item 3
> and item 8 are implemented in PR #114. `update_maker_order` runs the check
> only for a volume delta or `max`. The check's fee reading is shared with the
> request's trade-fee calculation through the request scope of chapter 38
> R38.6.6j.

> **As-built note (2026-10-25, clean-side record).** R60.6.4 and R60.6.7
> are implemented in PR #73, together with T60.5 and T60.6. The UTXO
> legacy and version-two spends and refunds, the ZHTLC transparent spend
> and the Sia spends and refund refuse a value not above the fee without
> broadcasting. The version-two funding spend requires fee plus dust. The
> pre-trade check runs in `setprice`, `update_maker_order`, `buy`, `sell`
> and both trade previews. Only a JSON `false` for `pre_trade_spend_check`
> disables it. For a ZHTLC coin the safe amount uses the R38.6.6c estimate,
> which is conservative, because its transparent spend actually charges a
> fixed 1,000 base units.

## 60.7 Tests

**T60.1** *UTXO values.* For coins configured as the public coins file
configures them, the reported minimum trading volume shall be: KMD
0.0001; LTC 0.000546; DOGE 0.1; RVN 0.0496; ZEC 0.00496 at `txfee`
100,000 and 0.003472 at `txfee` 70,000; QTUM 0.01984; DINGO 10; BTC and
BTC-segwit 0.0001; ARRR 0.0001. Synthetic: `txfee` 1,001, no dust, gives
10 × max(1,000, 496) = 0.0001 and `txfee` 30,001, no dust, gives
10 × 14,880 base units = 0.001488 (truncated, not 0.0014881 rounded up);
whole-kilobyte `txfee` 300,000, dust 1,000, gives 0.03; dynamic fee, dust
5,460, gives 0.000546.

**T60.2** *Minimum transaction amounts.* UTXO equals the dust (KMD
0.00001, LTC 0.0000546); SLP and Tendermint one base unit; EVM, ERC-20,
QRC20 and TRON zero; Sia 10^-24.

**T60.3** *W2.* A maker order with minimum base volume `m`: a taker request
of `m` is reserved; a request below `m` receives no message at all.

**T60.4** *W4.* With the taker coin's minimum transaction amount set to
different values on two test nodes, a trade whose fee is floored is
refused at taker-fee validation in the legacy protocol — for a UTXO taker
coin only when the maker's value is higher, for a Tendermint taker coin
in both directions — and is aborted in the version-two protocol in both
directions.

**T60.5** *R60.6.4.* A legacy spend of an HTLC whose value equals the
spend fee is refused without broadcasting.

**T60.6** *R60.6.7.* With `pre_trade_spend_check` at its default:
(a) a dynamic-fee UTXO coin with dust 1,000 and a current estimated rate
of 20,000 base units per kilobyte has `S = 9,920 + 1,000 = 10,920` base
units; a maker order whose minimum volume is 0.0001 (eight decimals) is
refused with threshold 0.0001092, and one at 0.00011 is accepted;
(b) a Sia leg whose amount equals the fixed HTLC miner fee is refused
with the miner fee plus one hasting as threshold; (c) a KMD order at its
floor of 0.0001 is accepted (`S` = 496 + 1,000 base units);
(d) `trade_preimage` reports the refusal of (a) with the same threshold;
(e) with `pre_trade_spend_check` set to `false`, the order of (a) is
accepted; (f) an incoming order or taker request below `S` is handled
exactly as without the rule.

**T60.7** *R60.4.6.* (a) With one maker order on base coin X and a
balance that does not change, exactly one balance read of X occurs per
30 s and none within the first 30 s after the watch starts; the interval
is unaffected by any configuration member. (b) A watch seeded at
creation with balance *b* performs no recomputation on a first poll
reading *b*, and exactly one on a first poll reading *b′* ≠ *b*. (c)
After a restart with one stored maker order, the first maintenance pass
reads the balance once and re-announces the order; the watch's first
poll at +30 s runs one recomputation even though the balance equals the
pass's reading, and that recomputation changes no order and sends no
message. (d) A poll whose read fails leaves the last-seen value and
every order untouched, and the next poll occurs 30 s later. (e) A
restored order whose live balance supports less than its available
amount is announced with its maximum volume lowered to the reserved
amount plus the supported maximum; one whose supported maximum is below
its minimum volume is canceled with the insufficient-balance reason and
not announced.

## 60.8 Deferred Work

**D60.1** *Finding F1 remedy* — **resolved 2026-10-25.** The maintainer
chose the local pre-trade check, now bound as R60.6.7; the floors stay at
parity (R60.6.1).

**D60.2** *Finding F2 remedy* for Sia — **resolved 2026-10-25.** The Sia
floor stays at parity; the orderly spend and refund refusal of R60.6.4
and the pre-trade check of R60.6.7 together keep a Sia payment at or
below the miner fee from being created.

**D60.3** *Order-validation home.* Chapter 57 D7's maker price floor is
coin-agnostic and is now bound here as R60.4.2 step 4; the one-line fix
itself remains tracked by chapter 57 D7 and the parity tracker's #12.

**D60.4** *Balance-watch interval and restored orders* — **resolved
2026-10-27** (passed the KDF Dirty Gate on 2026-10-27). Both open points are bound by
R60.4.6: the parity interval (item 1) and the start and seeding of the
watch for restored orders (items 4 and 5). This project's own budget
arithmetic — one request message per poll (chapter 38 R38.6.3a), a
budget of about one message per second, polling kept under a tenth of a
session's budget — gives a lower bound of 10 s per watched coin, which
the bound value satisfies.

## 60.9 Baseline Verifications

- **V60.1** The tree at the baseline anchor (R1) carries: the UTXO floor as ten dust
  with a fixed 0.00777 for the exact ticker `BTC` only; the UTXO minimum
  transaction amount as the dust (configured `dust`, else 1,000 base
  units); whole-kilobyte rounding for every fixed-fee coin; a 305-byte
  legacy spend estimate; no refusal when the spend fee reaches the HTLC
  value; the order-builder arithmetic of R60.4.2 steps 1–3 with a 10^-8
  price floor. Reloaded inherits all of these except where chapter 38's
  as-built note records a change.
- **V60.2** The tree at the baseline anchor (R1) carries the
  re-announcement path of R60.4.6 item 5 — fresh balance read, downward
  volume reconciliation, insufficient-balance cancellation — for own
  maker orders absent from the local orderbook, and carries **no**
  per-base-coin balance watch: no poll interval, no watch started at
  order creation, no seed. R60.4.5 and R60.4.6 items 1–4 are therefore
  post-baseline on both sides.

## 60.10 External References

- Bitcoin Core relay policy (dust threshold for standard outputs), the
  public basis of the "standard dust" figure in finding F1.
- Sia: the `sia-rust` public crate's default miner-fee constant
  (0.00002 SC), shared by both trees (finding F2).
- Public coins configuration file of the Komodo DeFi Framework coins
  registry (the `txfee`, `dust`, `dingo_fee` and `decimals` values used in
  T60.1).

## 60.11 Cross-references

[Chapter 08](08-fee-routing-engine.md) R6, R8, R11;
[Chapter 15](15-swap-v2-utxo-path.md) R21, R40;
[Chapter 16](16-swap-v2-pre-burn-output.md) R10, R14A, R17, R18;
[Chapter 18](18-tendermint-ibc-htlc.md) §18.5.2;
[Chapter 20](20-siacoin-integration.md) R-S11;
[Chapter 21](21-tron-integration.md) §21.11A;
[Chapter 32](32-orderbook-p2p-and-trie.md) R-W1, §32.5;
[Chapter 38](38-utxo-coin-maintenance-and-features.md) §38.6.6;
[Chapter 39](39-zcash---z_coin-shielded-coin.md) R39.5.2;
[Chapter 40](40-solana-coin.md) §40.6A;
[Chapter 41](41-lightning-network.md) §41.11A;
[Chapter 51](51-legacy-v1-swap-state-machine.md) R34, R40;
[Chapter 52](52-swap-v2-state-machine.md);
[Chapter 57](57-evm-fee-inputs.md) §57.7A, D7, D8;
[Chapter 59](59-legacy-ordermatch-negotiation.md).

## 60.12 Provenance Footer

- *Inputs:* the tree at the baseline anchor (R1) (V60.1, R60.4.2 steps 1–3,
  R60.4.3, the baseline column of §60.3); the present reloaded workspace
  (every "Reloaded today" entry, findings F3, F4 and F5); the public
  third-party coins-configuration file in two published copies (the
  `txfee`, `dust`, `dingo_fee` and `decimals` values behind T60.1, and
  the absence of `txfee` and `dust` on the ARRR and Bitcoin entries); the
  public `sia-rust` crate shared by both trees (the 0.00002 SC miner fee of
  F2); public Bitcoin relay policy (the standard-dust figure of F1); the
  public release history of the historical lineage (the designations of
  §60.1).
- *Permitted-input classes used:* R1 (the baseline tree), R3 (public
  relay policy), R4 (the coins-configuration key schema and the
  `min_volume` wire field cited from chapter 32), R5 (`sia-rust`), R6
  (public-release facts), and the project's own tree and revision
  history.
- *Sibling-allowlist consultations:* `sia-rust` (the default miner-fee
  constant only).
- *Two-team workflow record, and its Dirty Gate clearance (chapter 01
  T1, R40, R43).* Authored by the KDF Spec Reader (AGENTS.md §2) on
  2026-10-25 under the two-team procedure of chapter 01 §1.4A, and
  gated by the KDF Dirty Gate in **five passes on 2026-10-25**, each
  returning PASS, discretionary-expression measure 0.02, finding list
  empty, and accordingly no remedy class for any line or for the
  candidate as a whole. The pass-1 candidate was this chapter together
  with the companion material it was authored with: chapter 38 §38.6.6
  and its footer item, the chapter 57 R57.7A.5 amendment and pointers,
  and the per-family rules and pointers in chapters 08, 15, 16, 18, 20,
  21, 32, 39, 40, 41, 51, 52 and 59 and in the parity tracker. The pass-2
  candidate was the one rule-text change made after pass 1 (the
  `v3.0.0-beta` row of §60.1, which no longer states anything about a
  later unreleased v3 state) and all clearance-record text. The pass-3
  candidate was the whole current candidate (below). The pass-4
  candidate was the regions changed after pass 3: one minimal rule-text
  change made at the gate's request (the last sentence of chapter 38
  R38.6.6e, which now names the releases that had ten times the dust for
  every UTXO coin) and the record text that reports pass 3. The
  clearance covers the combined extent below, subject to stated gaps
  (a)–(c); gap (d) is closed. The scope attestation, recorded in
  corpus-free terms:
  - *Extent, pass 1.* The trading-floor subsystem as this chapter
    describes it: the minimum-trading-volume and
    minimum-transaction-amount accessors of every family of §60.3; the
    UTXO fee-policy classification and fee arithmetic, including the
    change-output allowance; the UTXO HTLC spend-fee estimate and the
    spend and refund refusal conditions; and the maker's test of an
    incoming taker request. Compared in full at `v3.0.0-beta`, and
    cross-checked at `v2.6.0-beta` for the floor rule and the
    configuration classification — thirteen source files at the first
    designation, two of them also at the second, plus a symbol search of
    the whole first-designation tree — and four public issue-tracker
    items.
  - *Extent, pass 2 (first-appearance history).* The UTXO HTLC spend
    reference size at `v1.0.0-beta`, `v2.0.0-beta`, `v2.3.0-beta`,
    `v2.4.0-beta` and `v2.5.0-beta`; the UTXO minimum-trading-volume rule
    and its Bitcoin exception at `v2.4.0-beta`, `v2.5.0-beta`,
    `v2.5.1-beta` and `v2.5.2-beta`; the whole-kilobyte option and the
    fixed-fee arithmetic at `v2.5.2-beta` and `v2.6.0-beta`; and the
    UTXO spend-refusal condition at the baseline anchor (R1), `v1.0.0-beta`,
    `v1.0.1-beta`, `v2.0.0-beta` and two public releases of the older
    lineage, `v1.1.0` and `v1.0.1172` — three source files across those
    designations, plus symbol searches at `v2.4.0-beta` and
    `v2.5.2-beta`. Every first-appearance statement within this extent
    was found consistent: `v2.4.0-beta` for the 496-byte size;
    `v2.5.0-beta` for the removal of the Bitcoin exception, with ten
    times the dust for every UTXO coin, Bitcoin included, in the
    `v2.5.0-beta`, `v2.5.1-beta` and `v2.5.2-beta` releases; `v2.6.0-beta` for the fee-derived floor, `dingo_fee` and
    proportional fixed fees, which `v2.5.2-beta` lacks; and
    `v1.0.0-beta` for the dating in finding F3.
  - **Stated gaps.** (a) No later unreleased v3 state was compared in
    any pass, so no statement about one is cleared, and this chapter
    makes none. (b) The per-family legacy taker-fee validation (R60.5.4's
    legacy bullet), the version-two funding check (R60.5.4's version-two
    bullet), the Sia and shielded spend construction (R60.5.6 row 8;
    chapter 20 R-S11; chapter 39 R39.5.2) and the order-builder refusals
    (R60.4.2) were checked by token inspection only, not read side by
    side with the compared material. (c) The `v2.0.0-beta`
    first-designation row of §60.1 was not compared in any pass; it
    rests on the earlier clearance of chapter 57 §57.1, which this
    clearance does not repeat. (d) That the §60.1 `v3.0.0-beta` row was
    the only rule-text change after pass 1 was not independently
    verified in pass 2; closed by pass 3. Gaps (a) and (c) hold for all
    five passes.
  - *Pass 3 (final sweep, 2026-10-25; closes gap (d)).* PASS, measure
    0.02, finding list empty, standard unchanged. The candidate was the
    whole current candidate: all of this chapter (594 lines at the time)
    and every added line of the companion diff in the eighteen modified
    files, 1,159 lines in all. It was compared with the comments and
    string literals of the whole compared source tree at `v3.0.0-beta`
    and at `v2.6.0-beta`, and with that tree's prose documentation at
    `v3.0.0-beta`. A word six-gram overlap of the whole candidate against
    that material found no overlap at all, and a word five-gram
    near-match check left only non-discretionary residue. Every
    identifier-shaped or code-formatted token went through the
    forced-versus-free test; the only tokens new since pass 1 are public
    release designations. The rule text of this chapter, of chapter 38
    §38.6.6 and of tracker entry #14, and every added line elsewhere
    that is not record text, was re-read against the pass-1 reading. The
    pass confirmed that the §60.1 `v3.0.0-beta` row is the only rule-text
    change since pass 1 and that everything else is record text, which
    **closes gap (d)**. Gaps (a)–(c) stand: pass 3 compared no later
    unreleased v3 state and did not compare the `v2.0.0-beta` row; its
    mechanical sweep also covered the four areas of gap (b), which
    otherwise remain token-inspected only.
  - *Pass 4 (2026-10-25; the post-pass-3 changes).* PASS, measure 0.02,
    finding list empty. The candidate was the regions changed after pass
    3, as listed by the commissioning context; the gate did not
    re-derive that list. The compared side was the UTXO floor rule and
    its Bitcoin exception at each of the thirteen beta releases from
    `v1.0.0-beta` to `v2.4.0-beta` (twelve distinct commits;
    `v1.0.0-beta` and `v1.0.1-beta` share one), together with the comments and string
    literals of the whole compared tree at `v3.0.0-beta` and
    `v2.6.0-beta` and that tree's changelog at `v3.0.0-beta`. The gate
    read the reworded R38.6.6e sentence side by side with it, counted
    mechanically where the rule and the exception are present across the
    thirteen releases, ran a word five-gram overlap test of the changed
    regions against those comments, literals and changelog (no overlap),
    and applied the forced-versus-free test to the new tokens. It found
    the sentence consistent: its `v2.5.0-beta` to `v2.5.2-beta` statement
    lies within the pass-2 extent, and its statement that every UTXO
    coin except the ticker `BTC` had ten times the dust before
    `v2.5.0-beta` lies within the pass-4 extent. Releases of the older
    lineage that predate the baseline anchor (R1) were not compared for
    that statement; for the baseline anchor (R1) itself it rests on this document's
    baseline verification (V60.1).
  - *Pass 5 (2026-10-25; the reference-state names).* PASS, leak measure
    0.02, finding list empty. Pass 5 had two checks. The leak check
    covered only the lines changed in that pass: the fifteen rows added
    to the chapter 02 reference-state table (§2.15A), that table's
    closing note, and the conversion of this candidate's baseline
    references to the table's row name. The reference-state check of
    chapter 02 R13 covered the whole candidate. Every state name the
    candidate uses has a row. Each state compared in passes 1–4 (21 names
    and 20 distinct commits) was resolved read-only to its full
    identifier and date, and every row identifier is the commit actually
    read. Each comparison read the tag-resolved content or the baseline
    anchor (R1) itself, never a checked-out working tree. Method: the
    forced-versus-free test on the changed lines; a mechanical
    extraction of every state name, looked up in §2.15A; read-only
    resolution of each compared name; and a cross-check of the new
    pull-request rows against the public merge metadata. Those rows, and
    the branch-tip row added in the same pass, meet chapter 02 V4, but
    no pass compared their content and no chapter prose names them. The
    reference-state check had zero findings. Gaps (a)–(c) stand.
  - *Post-pass metadata normalization (2026-10-25, after pass 5).* On
    the orchestrator's decision, two convention sentences were added to
    the discipline list of chapter 02 §2.15A. One says the short forms
    *the baseline* and *the anchor* refer to the row *baseline anchor
    (R1)* and need not be rewritten. The other says every date in the
    table is a UTC date: the commit date for the anchor, release tags and
    branch tips, and the merge date for pull-request merges. Every row
    was then checked against read-only reference metadata. One date
    changed, on the row `v1.0.4-beta` (2023-05-23 → 2023-05-22, the UTC
    date of its commit). Two time-zone remarks became redundant under the
    stated rule and were removed from the kind column of the rows
    `PR #2112 (merged 2025-03-24)` and `PR #2454 (merged 2025-07-30)`. No
    name, identifier or other kind entry changed. Dates are reference
    metadata, not expression and not a state's identity (the full
    commit identifier is), so no gate pass was run for this
    normalization.
  - *Method.* Pass 1: side-by-side reading of the compared material
    against the candidate; a mechanical token search of the compared
    material; and a per-token forced-versus-free test of every
    code-formatted token against the compared material, the baseline
    anchor (R1) and this repository. Pass 2: side-by-side reading; a
    mechanical token search and count across the designations named
    above; and a per-token forced-versus-free test of the new text.
    Passes 3, 4 and 5: as stated in their items above. The gate's script
    was not run in any pass, since the candidate contains no code blocks.
  - *Measure.* **0.02** in each pass, on the discretionary-expression
    basis of chapter 01 R39 and R35, with dictated and pinned content
    excluded; pass 5 also had zero findings against chapter 02 R13. All
    five passes read against the same leak standard: any single
    free discretionary match, any transcribed body or control flow, or
    any discretionary message or comment wording fails the candidate.
    None was found.
- *Forbidden corpus:* consulted **only** under the two-team procedure of
  chapter 01 §1.4A, under the reference designations of §60.1
  (`v2.6.0-beta` and `v3.0.0-beta`, with `v2.4.0-beta`, `v2.5.0-beta` and
  `v2.6.0-beta` named only as the first designations of the behaviours
  §60.1 lists), to establish: the per-family values of the "both
  designations" columns of §60.3; that the reference consumes the two
  accessors on the paths of R60.4.1 and R60.4.4 and applies R60.4.2 step
  4 and R60.4.3; the peer-visible effects W1–W6 of §60.5, including the
  maker's silence below an order's minimum volume, the per-family legacy
  fee checks (at least for UTXO, exact for SLP, Tendermint and Sia) and
  the version-two exact funding check, the absence of any volume-based refusal
  at negotiation, the spend refusals of rows 3–7 and the Sia miner-fee
  deduction of row 8; findings F1, F2 and F6; and the designation in F3.
  The reference's public issue-tracker discussion of the original 0.01
  wrapped-Bitcoin floor, of the swap failures at a ten-dust floor, and of
  the fee-derived floor change was read under the same procedure, only to
  locate behaviour; nothing of it is quoted. No discretionary expression —
  no private type, field or function name, no function body,
  control-flow transcription, helper decomposition, local name, error or
  log literal — was reproduced. The numeric values are bound as parity
  data: each is observable at the external boundary of a reference node
  through its `min_trading_vol` response, its advertised `min_volume`, or
  the DEX fee it pays or validates.
- *Clean-room-original addition (2026-10-25).* R60.6.7, T60.6 and the
  resolutions recorded under D60.1 and D60.2 were written by a clean-side
  session from the maintainer's decision of 2026-10-25 and this
  chapter's cleared text. The design is this project's own; the reference
  has no such check. *Forbidden corpus:* not consulted for this
  addition.
- *Clean-side correction (2026-10-25).* The Sia miner fee was given as
  0.02 SC in R60.5.6 row 8, finding F2, §60.10 and this footer. That figure
  came from the `sia-rust` constant's own comment. The constant is
  2 × 10^19 hastings, and one SC is 10^24 hastings, so the fee is
  0.00002 SC. The figures now say so. The finding stands, at that size.
  This correction uses no forbidden-class input.
- *Addition of 2026-10-26, cleared by the KDF Dirty Gate.* R60.4.5 and D60.4 were
  written by a session that never read the corpus. Its rationale comes from
  this project's own Electrum measurements and code reading. Before writing
  them, that session had received a KDF Spec Reader's behavioural summary of
  the reference, relayed before any gate pass, and items 1 and 3 coincide
  with it, so its provenance is recorded here rather than claimed as
  clean-room-original. No value from that summary is stated.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-10-26, measure
  0.03, no leaks, with the pre-gate summary declared as an input. *Extent:*
  the maker-order balance-watch surface of the order-matching subsystem
  within the scope of §60.4, at `v2.6.0-beta` and `v3.0.0-beta`. *Method:*
  reading plus mechanical text search for identifiers, phrases and figures;
  the gate script was not run because there are no code blocks. *Measure:*
  0.03 against the 0.10 threshold. The gate confirmed that the 10 s lower
  bound in D60.4 is this project's own arithmetic, and that the rationale
  is independent of the reference.
- *Reloaded-original additions (2026-10-27), cleared by the KDF Dirty Gate.* The R60.6.7 amendment and item 8 were
  written by a session that never read the corpus, from the maintainer's
  Decisions P and R of 2026-10-27 and this project's own Electrum measurements and code reading. The informative notes saying
  the reference reads its fee estimate or gas price per consumer and caches
  nothing rest on a KDF Spec Reader summary relayed before its gate pass, and
  are declared here as that input. *Dirty Gate clearance (chapter 01 T1,
  R40).* PASS on 2026-10-27, measure 0.00, no leaks; the declared input
  was verified accurate at both rows, and the switch names do not occur in the
  reference. *Extent:* the fee-rate acquisition surface used by the pre-trade check and the trade-fee consumers of §60.6, at `v2.6.0-beta` and `v3.0.0-beta`.
  *Method:* reading plus mechanical text search against both rows; the gate
  script was not run because there are no code blocks. *Measure:* 0.00
  against the 0.10 threshold.
- *Addition of 2026-10-27 (KDF Spec Reader; passed the KDF Dirty Gate on 2026-10-27).*
  R60.4.6, T60.7, V60.2, the resolution of D60.4 and the pointer change
  in R60.4.5 item 2 were authored by the KDF Spec Reader under the
  two-team procedure of chapter 01 §1.4A. Forbidden-class input,
  consulted at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02
  §2.15A (identical for everything stated) and compared with the baseline
  anchor (V60.2): the fixed 30 s interval and its non-configurability;
  the sleep-before-read ordering; the behavior on a failed read; which
  insertion paths seed the watch and which do not; and the
  fresh-read-and-reconcile behavior of the re-announcement path. Every
  fact is stated as externally observable behavior; no private
  identifier, function body, helper decomposition, log or error literal
  was reproduced. Dirty Gate clearance (chapter 01 T1, R40): PASS, measure 0.02, no leaks;
  every reference fact confirmed at both rows. *Extent:* the order-matching
  balance-watch and re-announcement surface, one file per row and the
  baseline, with the row-to-row difference. *Method:* reading plus a
  mechanical regex scan of every added line against private identifiers,
  literals and distinctive phrases; the mechanical gate tool was not run
  (prose). *Measure:* 0.02 against the 0.10 threshold.
