# Chapter 62 — The Reference Price Service: Endpoints, Response Contract, Providers and Consumers

**Status:** driving-spec. **Passed the KDF Dirty Gate on 2026-11-07, measure
0.02** (see §62.13 for the extent and method). Authored by the KDF Spec Reader
on 2026-11-07 (round 0.4.0-4) under the two-team procedure of
[chapter 01](01-clean-room-rules.md) §1.4A.

> **One-sentence claim:** the project shall obtain fiat (USD) reference prices
> of coins from one or more HTTP price-aggregator endpoints that serve a
> per-ticker JSON document of the shape §62.4 binds; shall derive from them a
> cross rate for a pair, a per-swap fiat snapshot at swap completion, and the
> ETH-denominated price a watcher reward needs, exactly as §62.6–§62.8 bind;
> and shall treat the endpoint list, the fallback rule, the staleness rule and
> the set of recognized price providers as the maintainer's decisions of
> §62.10, because the reference's own values no longer work.

## 62.0 Executive Summary, subsystem shape, and treatment

*Executive summary.* Three things in the reference ask "what is this coin
worth in USD": the simple market-maker bot (to turn a USD-denominated pair
rate into a maker price and to size USD-denominated volumes), the swap
statistics store (to stamp a finished swap with the two coins' USD prices at
that moment), and the watcher-reward computation (to express an ETH-denominated
gas cost in another coin). All three read the same kind of document — one
JSON object keyed by ticker, each value a record with a last price, a
timestamp and the name of the exchange or aggregator the price came from — and
all three use the same lookup: find both tickers, require a *recognized* price
provider and a non-zero timestamp for each, divide. The bot also applies a
staleness window and price floors; the other two consumers do not. The
reference ships a fixed, release-bound list of three endpoint URLs and tries
them in order; the bot alone lets the caller replace the list in the start
request. At `v2.6.0-beta` two of the three built-in hosts no longer resolve
and the third times out; at `v3.0.0-beta` the list was moved to another
operator's hosts, which answer today. The two designations differ in nothing
else.

*Subsystem shape.* One price module in the coin crate owns the endpoint list,
the fetch, the response model, the provider vocabulary and the pair-rate
derivation; the order-match layer (the bot), the swap layer (completion
snapshot, statistics receive path) and the coin layer (watcher reward) consume
it. Nothing else does: no portfolio, no trade preview, no fee computation, no
GUI-facing RPC reads the price service at either designation.

*Treatment.* This chapter is new. [Chapter 42](42-price-service-and-makerbot.md)
binds the bot's order logic (its start/stop RPCs, per-pair configuration,
refresh loop and volume arithmetic) and was written without the corpus; its
§42.4–§42.6 state the price side only in outline, and §42.5's statement of the
provider set is superseded here (§62.9, finding F3). This chapter binds the
price side in full and leaves the bot's order logic to chapter 42.

- **§62.2–§62.8 (T-DOC, reference behavior).** What the reference does at
  `v2.6.0-beta` and `v3.0.0-beta`, stated as requirements where Reloaded must
  match and as reference facts where the maintainer has a decision to take.
- **§62.9 (Reloaded state).** What this repository does today, from its own
  tree, and where it departs from the reference.
- **§62.10 (D62.x).** The decisions the maintainer took on 2026-11-07
  (Decisions DO to DU, implemented in PR #287); every one is stated with the
  reference's answer and the observed network state.

> **Binding scope (R62.0).** This chapter binds observable behavior: the URLs
> requested and the order they are tried in, the HTTP request made, the JSON
> shape consumed (dictated by the aggregator API, chapter 01 R4), the public
> request keys of the bot's start RPC that select price sources, the
> arithmetic that produces a pair rate, the conditions under which a price is
> refused, the persisted snapshot values of chapter 44 §44.6.4, and the
> consumer-visible effects of a fetch failure. It does not bind private types,
> helper decomposition, log or error wording, or module layout (chapter 01
> R11, chapter 00 R6).

## 62.1 Reference designations

| Designation | Role in this chapter |
| --- | --- |
| *baseline anchor (R1)* | The project's own substrate. Has the bot with a single fixed endpoint and a single `price_url` request key; has neither the completion snapshot nor the watcher-reward conversion (§62.9). |
| `v2.6.0-beta` | The legacy behavioral reference for netid `8762` (AGENTS.md §3). Every rule in §62.2–§62.8 holds here. Its endpoint list is R62.2.1 (a). |
| `v3.0.0-beta` | The newest public release of the v3 lineage. **Identical to `v2.6.0-beta` in every rule of this chapter except the endpoint list**, which is R62.2.1 (b). No `NetConfig` selection is involved anywhere in this chapter. |
| `dev (as of 2026-03-27)` | Branch tip read for completeness: the price module, the bot and its state are byte-identical to `v3.0.0-beta`. Not a clearance target. |

Facts about the reference are stated against these rows of
[chapter 02](02-baseline-state.md) §2.15A only; no post-anchor commit
identifier is named in prose (chapter 01 R20).

## 62.2 Endpoints and how they are chosen

**R62.2.1** *Built-in endpoint list.* The reference carries a fixed list of
three endpoint URLs, in this order, compiled into the release:

(a) at `v2.6.0-beta`:
1. `https://prices.komodian.info/api/v2/tickers`
2. `https://prices.cipig.net:1717/api/v2/tickers`
3. `https://cache.defi-stats.komodo.earth/api/v3/prices/tickers_v2.json`

(b) at `v3.0.0-beta`:
1. `https://prices.gleec.com/api/v2/tickers`
2. `https://prices.cipig.net:1717/api/v2/tickers`
3. `https://defistats.gleec.com/api/v3/prices/tickers_v2`

The third entry of each list is, by the reference's own release notes, a
cache of the first that is refreshed about once a minute. The baseline anchor
has a single endpoint, `https://prices.komodo.live:1313/api/v2/tickers`, and
no list. *Rationale:* the URLs are the addresses of public services and are
dictated interop, not expression; they are recorded so that the maintainer's
D62.1 starts from what the reference ships.

> **Public-network observation (2026-11-07, informative).** From the
> development host: `prices.komodo.live` and `prices.komodian.info` have no
> DNS record (both failed in about 10 s); `cache.defi-stats.komodo.earth`
> resolves but a GET timed out at 12 s; `prices.cipig.net:1717`,
> `prices.gleec.com` and `defistats.gleec.com` each answered HTTP 200 in under
> 0.3 s with an 87–101 KB document of 254–277 tickers. So a `v2.6.0-beta`
> node with the built-in list reaches a working endpoint only on its second
> attempt, after two failures that each take the transport's full give-up
> time; a `v3.0.0-beta` node succeeds on its first.

**R62.2.2** *Who may change the list.* Only the simple market-maker bot
accepts a caller-supplied list, and only in the body of
`start_simple_market_maker_bot`, through exactly one of two optional keys at
the top level of the request, beside `cfg` and `bot_refresh_rate`:

- `price_url` — a single URL string (the baseline form; the reference marks
  it for deprecation but accepts it); or
- `price_urls` — an array of URL strings, tried in array order.

Neither key present → the built-in list of R62.2.1 for the running release.
The list is fixed for the life of the running bot; a new list needs a stop and
a start. No daemon-configuration key, no coin-configuration key and no
environment variable changes the list in a release build, for any consumer.
The completion snapshot (§62.7) and the watcher-reward conversion (§62.8)
always use the built-in list. *Rationale:* chapter 42 R42.5.1 already demands
that the endpoint be re-pointable and that no hostname be hard-required; the
reference meets that only for the bot.

> **Note (inferred, verify before relying on it).** The two keys are parsed
> as alternatives of one optional value. From the way such values are parsed,
> a request that carries both keys appears to take whichever the parser meets
> first, and a request whose value is malformed (for example a `price_urls`
> that is not an array of strings) appears to fall back to the built-in list
> rather than fail. Neither case is a documented contract of the reference;
> D62.6 asks the maintainer to define Reloaded's behavior for both.
> *(Decided 2026-11-07, Decision DT: Reloaded refuses a start request that
> carries both keys, and one with a malformed value of either key; PR #287,
> merged 2026-11-08.)*

**R62.2.3** *The request.* Each attempt is a plain HTTP GET of the URL as
given: no query parameter (no age or expiry filter, no ticker filter), no
request body, and no header the application adds itself — whatever the HTTP
library sends by default is all that is sent. A response is accepted only
with HTTP status 200; any other status is a failure of that endpoint. The
whole document is downloaded on every attempt; the reference keeps no cache
and sends no conditional-request header. No request-specific timeout is
applied: a connection that neither answers nor fails holds the fetch for as
long as the shared HTTP client and the operating system take to give up.
*Rationale:* the consumers' failure behavior (R62.6.6, R62.7.3) is bounded by
this.

**R62.2.4** *Fallback order and rotation — bot.* On each refresh cycle the bot
tries its list from the front and stops at the first endpoint that returns an
accepted, parseable document (§62.4). On success the list is *rotated* so
that the successful endpoint becomes the first entry and the entries that
failed before it move, in order, to the end; the next cycle therefore starts
with the endpoint that worked last. Every failed attempt is reported at
**error** level, once per endpoint per cycle, naming the endpoint; a success
is reported at **info** level once per cycle, naming the endpoint. When every
endpoint fails the cycle's fetch fails and R62.6.6 applies. *Rationale:* the
rotation keeps a dead first entry from costing a full transport timeout on
every cycle, but only after the first cycle; the per-cycle error line is
what an operator sees for a dead host.

**R62.2.5** *Fallback order — snapshot and watcher consumers.* These two
consumers try the built-in list from the front on every call, without
rotation, and stop at the first endpoint that both answers with an accepted
document **and** yields a pair rate for the two tickers asked for (R62.5.1).
An endpoint that answers but lacks one of the two tickers, or lists it with an
unrecognized provider or a zero timestamp, counts as a failed endpoint and the
next one is tried. Each failure is reported at **error** level; when all
endpoints fail the call yields no price and the consumer's own rule applies
(R62.7.3, R62.8.2). *Rationale:* this differs from the bot, which accepts the
first answering endpoint as a whole; a Coder must not fold the two into one
rule.

**R62.2.6** *Poll interval.* The bot fetches once per refresh cycle. The cycle
is `bot_refresh_rate` seconds, default 30, and a requested value below 30 is
raised to 30 (chapter 42 R42.3.1 binds the loop; this rule binds only how
often the price document is downloaded). The snapshot consumer fetches once
per terminal successful event of an own swap and once per received statistics
report (R62.7.1–R62.7.2). The watcher consumer fetches once per reward
computation (R62.8.1). There is no background price loop and no shared cache
between consumers: two consumers that need a price in the same second download
the document twice.

**R62.2.7** *Browser builds.* The bot's start and stop RPCs are served in the
browser build too, and the price document is fetched there with the browser's
fetch API in its default mode, which is subject to the same-origin policy; an
endpoint that does not send a permissive `Access-Control-Allow-Origin` header
cannot be read by a browser build. The snapshot consumer does not exist in
the browser build (R62.7.4). *Public-network observation (2026-11-07):* the
two `gleec.com` endpoints send `Access-Control-Allow-Origin: *`;
`prices.cipig.net:1717` sends no CORS header, so a browser build cannot use
it.

## 62.3 Transport failure classes (informative)

A failed attempt is one of: a transport or DNS failure; a timeout; a non-200
status; a body that is not UTF-8; a body that does not parse as the §62.4
document. All are failures of the endpoint and advance the fallback; the
consumers never distinguish them. This matters for D62.3: a host with no DNS
record fails fast, a host that accepts the connection and never answers fails
only at the transport's limit, and a host that answers with the wrong shape
fails after a full download.

## 62.4 The response document (dictated interop)

**R62.4.1** *Shape.* The response body, after trimming surrounding
whitespace, MUST be a JSON object whose keys are ticker symbols and whose
values are ticker records. A ticker record carries the following members;
every member except `sparkline_7d` is required, members not listed are
ignored, and a single record that is malformed or incomplete fails the whole
document (R62.3):

| Member | Form | Used by a consumer? |
| --- | --- | --- |
| `ticker` | string | no (the object key is used) |
| `last_price` | a number, or a decimal number in a string; parsed as an exact rational | **yes** — the only price value read |
| `last_updated` | string (an RFC 3339 instant in the live feeds) | no |
| `last_updated_timestamp` | unsigned integer, seconds since the Unix epoch | **yes** — freshness (R62.5.1 c, R62.6.3) |
| `volume24h` | number or decimal string | no |
| `price_provider` | provider name string (§62.5) | **yes** — recognition test (R62.5.1 b) |
| `volume_provider` | provider name string | no |
| `sparkline_7d` | `null` or an array of numbers; optional | no |
| `sparkline_provider` | provider name string | no |
| `change_24h` | number or decimal string (may be negative) | no |
| `change_24h_provider` | provider name string | no |

This shape is what the aggregator API serves at the §62.2 endpoints
(chapter 01 R4); chapter 42 R42.4.1 lists the same members and is consistent
with this table. *Rationale:* a Coder needs to know that nine of the eleven
members are parsed only to be dropped, and that a feed may omit none of them
except `sparkline_7d` without being rejected whole.

> **Public-network observation (2026-11-07, informative).** All three answering
> endpoints serve exactly these eleven members per record, prices and volumes
> as decimal strings, `sparkline_7d` as `null`, no record with a zero price and
> none with a zero timestamp. The two `gleec.com` feeds give `last_updated`
> with fractional seconds for some records and without for others.

**R62.4.2** *Numeric form.* `last_price` is read as an exact rational (the
project's `MmNumber` wire form: a JSON number, a decimal in a string, or the
project's ratio object), never as a binary float, and every derived value
(R62.5.1 d) is exact. A negative `last_price` is **not** rejected at this
layer; a zero `last_price` is rejected only where division makes it so
(R62.5.1 d) or where a consumer tests for zero (R62.6.2).

**R62.4.3** *Ticker lookup.* A consumer asks the document for a coin by the
coin's configured ticker. The lookup is: the exact ticker as a key; if that
key is absent and the ticker contains a hyphen that is not its last
character, the part before the *first* hyphen as a key (so `USDT-PLG20` and
`USDT-BEP20` both read the `USDT` record when the exact key is missing, and
`KMD-BEP20` reads `KMD`). There is no further fallback and no case folding.
*Rationale:* this is how a token on any chain inherits its asset's price
without the feed listing every chain variant.

## 62.5 Providers and the pair rate

**R62.5.1** *Pair rate.* Given a base ticker and a rel ticker, the pair rate
is defined when all of the following hold, and undefined otherwise:

(a) both tickers resolve to a record under R62.4.3;
(b) both records name a **recognized** `price_provider` (R62.5.2);
(c) both records carry a non-zero `last_updated_timestamp`;
(d) the division base `last_price` ÷ rel `last_price` is defined (the rel
    price is not zero).

When defined, the pair rate consists of: the base USD price, the rel USD
price, their exact quotient as the pair price, the two provider names, and
the pair timestamp = the **smaller** (older) of the two records' timestamps.
A zero base price yields a defined rate whose pair price is zero (the bot
rejects it under R62.6.2; the snapshot stores it). *Rationale:* a consumer
that takes the older timestamp as the pair's age never overstates freshness.

**R62.5.2** *Recognized providers.* The recognized provider names at both
designations are exactly, as they appear in the feed:
`binance`, `coingecko`, `coinpaprika`, `forex`, `livecoinwatch`. Any other
string — including the feed's own literal `unknown` and any name a feed adds
later — is read as *unrecognized*. The set is fixed in the release and
cannot be extended by configuration. Test builds of the reference recognize
one additional test-only name, which is not part of the contract. Only
`price_provider` is tested; `volume_provider`, `sparkline_provider` and
`change_24h_provider` are parsed and never evaluated.

> **Public-network observation (2026-11-07, informative).** `prices.gleec.com`
> names a provider `gleeccex` for some records; it is not in R62.5.2, so a
> reference node of either designation cannot price those tickers from that
> endpoint, and R62.2.5 then moves the snapshot consumer on to the next
> endpoint. `defistats.gleec.com` prices RIN from `livecoinwatch`, which the
> reference recognizes and Reloaded did not until PR #287, merged 2026-11-08
> (§62.9 F3; D62.4, Decision DR of 2026-11-07: `livecoinwatch` is in
> Reloaded's default set now).

**R62.5.3** *No cross-source check.* A consumer takes the first document that
satisfies it (R62.2.4, R62.2.5). No consumer fetches two endpoints to compare
them, and no consumer remembers the previous cycle's price to detect a jump.
*Rationale:* recorded because D62.2 asks whether Reloaded should.
*(Decided 2026-11-07, Decision DP: Reloaded fetches its sources in parallel
and drops a ticker whose prices differ by more than 5 %; PR #287, merged
2026-11-08.)*

> **Public-network observation (2026-11-07, informative).** Of the tickers both
> `prices.cipig.net:1717` and `prices.gleec.com` listed, 17 differed by more
> than 2 %, most with a different `price_provider` on each side; KMD was
> 0.00313 USD on one and 0.00680 USD on the other. Two of the three live feeds
> are therefore not interchangeable for every ticker, which is the fact behind
> D62.2.

## 62.6 Consumer (a): the simple market-maker bot

**R62.6.1** *What it fetches.* Once per cycle, before touching any order, the
bot downloads one document under R62.2.4 and keeps it for the whole cycle.
Every pair of the cycle is priced from that one document.

**R62.6.2** *Per-pair acceptance, in this order.* For each enabled pair the
bot derives the pair rate of R62.5.1 (an undefined rate is treated as a rate
with unrecognized providers) and then refuses the pair when the first of
these holds:

1. either provider is unrecognized;
2. the pair price is zero;
3. `min_base_price` is set and the base USD price is below it;
4. `min_rel_price` is set and the rel USD price is below it;
5. `min_pair_price` is set and the pair price is below it;
6. the pair has no timestamp (never true for a defined rate);
7. the pair's age exceeds the staleness window (R62.6.3).

A refused pair is **skipped for the cycle**; the reason is logged. *Rationale:*
chapter 42 R42.3.2 says a stale or missing price skips the pair; this rule
binds the complete test and its order.

**R62.6.3** *Staleness window.* The pair's age is the node's current time
minus the pair timestamp of R62.5.1, in seconds. The window is the pair's
`price_elapsed_validity` in seconds when set, else **300**. An age strictly
greater than the window refuses the pair. The window is the only staleness
test in the whole price system; the snapshot and watcher consumers have none.

**R62.6.4** *Maker price and volumes.* The maker price is the pair price
times the pair's `spread` (then optionally adjusted by chapter 42's
last-trades rule). A `usd`-denominated `max_volume` or `min_volume` is
converted with the **base USD price** of R62.5.1: a maximum volume in USD
becomes that amount divided by the base price (or "use the whole balance"
when the balance is worth less than the USD maximum); a minimum volume in
USD becomes that amount divided by the base price, and refuses the pair when
the balance is worth less than it. *Rationale:* this is why the feed must be
USD-denominated; a feed in another fiat would silently mis-size every
USD-volume pair.

**R62.6.5** *Effect of a refused pair on existing orders.* For a pair that
already has a maker order, the bot's cycle is an *update* of that order; any
refusal of R62.6.2 — and equally any other failure of the update, such as a
balance that cannot be read — makes the bot **cancel that order** in the same
cycle and log the reason at error level. For a pair with no order, a refusal
simply creates none. Consequently, a ticker that disappears from the feed, or
whose provider changes to an unrecognized name, takes the pair's order off the
book within one cycle. *Rationale:* an order left standing at a price nobody
can refresh is the hazard the design trades against; D62.5 asks whether
Reloaded keeps this. *(Decided 2026-11-07, Decision DS: kept, under
observation in v0.4.0; recorded with PR #287, merged 2026-11-08.)*

**R62.6.6** *Effect of a failed fetch.* When every endpoint fails in a cycle
(R62.2.4), the bot cancels **every order of every configured pair** — by pair,
so an order the operator placed by hand for the same base and rel is cancelled
too — logs the failure and the number of cancelled orders at error level, and
ends the cycle without creating anything. The bot stays running and tries
again on the next cycle. *Rationale:* same hazard as R62.6.5; the by-pair
cancellation is a side effect a Coder must reproduce or the maintainer must
decide away (D62.5). *(Decided 2026-11-07, Decision DS: kept; Reloaded's bot
already cancels by pair, §62.9.)*

**R62.6.7** *Logging frequency.* With a dead first endpoint in the list, the
reference's first cycle logs one error line for it; thanks to R62.2.4's
rotation later cycles start from the live endpoint and log one info line per
cycle. With all endpoints dead, every cycle logs one error line per endpoint
plus one for the cancellation. There is no warn-once or back-off (D62.3).

## 62.7 Consumer (b): the fiat snapshot of a finished swap

**R62.7.1** *When — own swap.* When the node saves the terminal event of one
of its own legacy swaps and the saved record is then a finished, successful
swap, it fetches the pair rate of its maker coin against its taker coin
(R62.5.1, via R62.2.5) **once** and sets two values on the record: the maker
coin's USD price and the taker coin's USD price. A failed swap gets neither.
The record is then persisted and later published and stored in the aggregate
statistics store with those values ([chapter 51](51-legacy-v1-swap-state-machine.md)
R50A items 2 and 5; [chapter 44](44-database-persistence-and-migrations.md)
§44.6.4 columns `maker_coin_usd_price`, `taker_coin_usd_price`). The
publication path does **not** fetch again; it reuses the values already on
the record.

**R62.7.2** *When — received report.* A native node that receives a peer's
statistics report (chapter 51 R50A item 4) fetches the pair rate for the
report's two coins at receive time, **whether or not the reported swap
succeeded**, replaces whatever prices the report carried with its own
fetch, and stores the record. *Rationale:* a collector prices every swap it
hears about at its own clock from its own endpoints; the publisher's values
are never trusted.

**R62.7.3** *On failure.* When no endpoint yields a pair rate (R62.2.5), the
record keeps **no** price — the two values stay absent and the two columns are
not written — and the swap's completion, publication and storage proceed
unchanged; nothing is retried later. There is no override: no request key,
configuration key or environment variable substitutes a price in a release
build.

**R62.7.4** *Scope.* Both values are **USD prices of the two coins**, not the
pair price, and both are stored as exact decimals. Browser builds have no
aggregate statistics store, publish without storing, and never fetch the
snapshot. The snapshot has no staleness test: a record with any non-zero
timestamp is accepted however old.

> **Code-quality finding F1 (informative; Reloaded's own tree).** *(The tree
> before PR #287, merged 2026-11-08; the line numbers are of that tree. The
> status update of §62.9 records the fix.)* Reloaded's
> snapshot path (`mm2src/mm2_main/src/lp_swap/swap_rpc.rs`, the constant at
> line 8 and the fetch at lines 20–47) downloads from the single baseline
> endpoint on the dead host `prices.komodo.live`, with no fallback, so every
> snapshot has failed since that host went dark: the two statistics columns
> are never written, and each own-swap termination and each received report
> first waits for a DNS failure. The same path also fetches on **publication**
> of an own swap (the statistics-store save routine, which performs the fetch,
> is called from the publication site at line 243), which the reference does
> not do (R62.7.1), and applies none of
> R62.5.1 (b)–(d): it reads `last_price` of the two exact-or-prefix keys with
> no provider or timestamp test. Proposed fix: route the snapshot through the
> same endpoint list and R62.5.1 derivation as the bot, under D62.1–D62.4.
> *(Done in PR #287, merged 2026-11-08, under Decisions DO to DU of
> 2026-11-07: the snapshot uses the bot's sources and R62.5.1, fetches once
> at the terminal report of a successful own swap, and no longer fetches on
> publication.)*

## 62.8 Consumer (c): the watcher-reward conversion

**R62.8.1** *When.* When a watcher reward's amount is not supplied by the
caller and the reward must be expressed in a coin other than ETH, the coin
layer obtains the coin's price **in ETH** as the quotient of the pair rate of
R62.5.1 for (that coin, `ETH`) — that is, the coin's USD price divided by
ETH's USD price, each from the same document — and divides the ETH-denominated
gas cost by it. This happens (i) for a UTXO taker coin whose counterpart is an
EVM coin, except when the counterpart is ETH itself and both trade amounts are
known (then the trade's own amounts give the rate and no fetch is made); and
(ii) for an ERC-20 maker coin whose counterpart is not ETH. An ETH maker or
taker never fetches. [Chapter 09](09-watcher-reward-infrastructure.md) owns
the reward; this rule binds only where its price comes from.

**R62.8.2** *On failure.* When no endpoint yields the rate, the reward
computation **fails** with an error in the RPC class, and the swap step that
asked for it fails with it; there is no default reward and no retry. The
fetch uses R62.2.5 (built-in list, no rotation, no staleness test). A test
build of the reference may read the rate from an environment variable
instead; that is a test facility, not a contract.

**R62.8.3** *Reloaded today.* Reloaded's swap watcher does not work yet in
the current release line (the project's release notes; chapter 09 R17 and the
GLEEC compatibility table), so this consumer is dormant here; the rule is
recorded so that a later watcher pass knows the price dependency and its
failure mode.

## 62.9 Reloaded's present state and divergences (from this repository's tree)

The following is from this repository's own code and release notes, not from
the corpus, and carries no clean-room restriction. The bullets describe the
tree before PR #287 (merged 2026-11-08); the status update after them records
what that PR changed.

- **Endpoints.** Two consumers, two copies of one dead URL: the bot's default
  (`mm2src/mm2_main/src/lp_ordermatch/simple_market_maker.rs` line 30) and the
  snapshot's constant (`swap_rpc.rs` line 8) are both the baseline's
  `https://prices.komodo.live:1313/api/v2/tickers`. Neither works (§62.2
  observation).
- **Bot list form.** Reloaded extends the baseline's single `price_url` to a
  **comma-separated list in that one string** (chapter 42 §42.5 status
  update), tries the entries in order, logs each failure at **warn** level,
  does not rotate, and returns the last error when all fail; an empty list is
  an error. The reference's `price_urls` array key is not accepted by
  Reloaded, so a GLEEC-style start request that uses it silently runs on the
  dead default (because the unknown key is ignored) — **finding F2**.
- **Provider set — finding F3.** Reloaded recognizes `binance`, `coingecko`,
  `coinpaprika`, `forex`, `coinmarketcap`; the reference recognizes
  `binance`, `coingecko`, `coinpaprika`, `forex`, `livecoinwatch` (R62.5.2).
  So Reloaded refuses every ticker priced by `livecoinwatch` — today that
  includes RIN on `defistats.gleec.com` — and accepts a `coinmarketcap` name
  that no reference build or live feed uses. Chapter 42 §42.5's statement
  that the provider set was "corrected" and "kept current" is therefore not
  accurate against either designation; it is superseded by R62.5.2 and D62.4.
- **Snapshot.** Finding F1 (§62.7).
- **Pair-rate derivation and bot checks.** Reloaded's bot applies R62.4.3,
  R62.5.1, R62.6.2 (same order), R62.6.3 (300 s default), R62.6.5 and
  R62.6.6 as the reference does; these are baseline-derived and need no
  change.
- **Name resolution.** Reloaded's `dns_resolver` setting (release notes)
  covers price sources, which the reference's price path does not have; it is
  a Reloaded extension and unaffected by this chapter.

> **Status update (reloaded, 2026-11-07).** Findings F1, F2 and F3 are fixed by
> PR #287 (merged 2026-11-08), with the decisions of §62.10 (DO to DU). Both
> consumers now use one list of sources: the default of D62.1 or the
> `MM2.json` key `price_urls`; the dead baseline URL is gone. F1 by the single fetch at the
> terminal report of a successful own swap, on the sources of D62.1 and the
> derivation of R62.5.1; F2 by the request key `price_urls` (D62.6); F3 by the
> reference provider set and the `price_providers` key (D62.4). The bot's
> per-pair acceptance, staleness window and cancellation stay as described above
> (parity). One latent defect of the bot is fixed with them: a zero `rel` price
> now gives no rate (R62.5.1 d) instead of a division by zero.

## 62.10 Decisions of the maintainer (taken 2026-11-07)

**D62.1** *Endpoint list and order.* Which endpoints Reloaded ships, and in
which order, for (i) the bot's default and (ii) the snapshot. Options:
(a) the `v3.0.0-beta` list of R62.2.1 (b) verbatim; (b) that list with
`prices.cipig.net:1717` first (the one host not operated by the party whose
hosts changed once already); (c) a Reloaded-operated endpoint first, the others
after; (d) different lists for the two consumers. The reference's answer is
(a). Observed: all three answer today; only the two `gleec.com` hosts send
CORS headers (R62.2.7), which decides the browser default. The maintainer's
list must also say whether the dead `komodian.info` / `komodo.earth` /
`komodo.live` hosts are dropped (recommended) or kept at the end.

**Decided 2026-11-07 (Decision DO).** Reloaded ships two *sources*, combined
under D62.2. A source is one URL or an ordered list of URLs; the first URL of a
source that answers is the source's answer, with no rotation. Source 1 is
`https://prices.gleec.com/api/v2/tickers?expire_at=259200`, followed in the same
source by its cache `https://defistats.gleec.com/api/v3/prices/tickers_v2?expire_at=259200`
(asked only if the first fails); source 2 is
`https://prices.cipig.net:1717/api/v2/tickers?expire_at=259200`. The query
parameter `expire_at` is the greatest age, in seconds (259200 = 72 hours), of
the tickers the service serves; without it the cipig service serves tickers
many months old. The dead `komodian.info`, `komodo.earth` and `komodo.live`
hosts are dropped. One list serves every consumer (the bot and the snapshot).
A top-level `MM2.json` key `price_urls` replaces the default list for every
consumer when present: an array whose items are each a URL string (one source)
or a non-empty array of URL strings (one source with in-order fallback). A
value that is not such an array, an empty array included, is ignored with one
WARN that names the key (not its value), and the default applies. The request
keys of the bot (D62.6) still override the list for that bot. Only the GLEEC
sources can be read by a browser build (R62.2.7). *Divergence:* the reference
rotates the bot's list after a success (R62.2.4); Reloaded does not, so T62.1 of
§62.12 does not apply. Reloaded's sources do not need it: the dead first URL of
the reference's list was the reason for the rotation.

**D62.2** *Fallback versus combining.* Whether Reloaded (a) keeps the
reference's first-success rule (R62.2.4/R62.2.5), (b) fetches two sources and
uses the first only when the second agrees within a tolerance, or (c) fetches
two and takes a defined combination. If (b) or (c): the tolerance, what to do
with a ticker whose two prices differ beyond it (refuse the pair, as the
reference refuses an unrecognized provider; or take the one with the newer
timestamp; or take the lower), and what to do when only one source answers
(accept it, or refuse everything). Observed: 17 of about 250 shared tickers
differed by more than 2 % between two live feeds, several by a factor of 2–60
(§62.5 observation), so (a) silently prices those pairs from whichever host
answered first. The reference's answer is (a).

**Decided 2026-11-07 (Decision DP).** Combine, with a 5 % cross-check. All
sources are fetched in parallel. From every answering source a record is dropped
if its `price_provider` is not recognized (D62.4) or its
`last_updated_timestamp` is zero. Then, per ticker: present in one source, it is
used; present in several sources with (highest price - lowest price) / lowest
price at most 5 %, the record with the newest timestamp is used, and on a tie
the earlier source's; above 5 %, the ticker is a *conflict* and is absent from
the combined table, which the consumers treat like a missing ticker (the pair
has no rate; the swap is stored without prices). The tolerance is a constant of
the code, not a setting. The ticker lookup of R62.4.3 applies to the combined
table. A source that fails is left out and the others are used alone; the fetch
fails only if no source answered. The figure rests on live data of 2026-11-07:
same-provider differences stayed below 3.6 % (timing), and the differences above
5 % that were seen were a factor of two or more with different providers on the
two sides (for example KMD at 0.00313 against 0.00680 USD).

**D62.3** *Warn-once and back-off.* The reference logs one error per failed
endpoint per attempt forever (R62.6.7). Reloaded already logs at warn level.
Options: (a) as the reference; (b) one warn line per endpoint per failure
*transition* (working → failing, failing → working), with a debug line
otherwise; (c) additionally skip an endpoint that failed N times in a row for
a back-off period and keep the rotation of R62.2.4. Whether to apply a
request-specific timeout (the reference applies none, R62.2.3) belongs here:
a 10 s per-attempt cap would bound a cycle with a dead first host to about 10 s
instead of the transport's limit.

**Decided 2026-11-07 (Decision DQ).** Warn once per transition, plus a cap of
10 seconds per attempt on a native build. The state is kept per endpoint, keyed by its host label
(scheme, host and port, never the path or query), and is shared node-wide by
every consumer. One WARN is logged when an endpoint goes from working (or from
unknown) to failing; DEBUG while it keeps failing; one INFO when it answers
again. A failure is a transport error, a timeout, a status other than 200, a
body that is not UTF-8 or a body that does not parse. The same rule applies per
ticker to conflicts (D62.2): one WARN naming the ticker, both prices, both
providers and the hosts; DEBUG while the conflict lasts; one INFO when the prices
agree again; the conflict state is kept per list of sources, so consumers with
different lists do not end or restart each other's episodes. The bot's own
"no source answers" case follows the same rule: one ERROR when it starts (with
the number of orders cancelled), DEBUG while it lasts, one INFO when a source
answers again; the cancellation itself is unchanged (D62.5). There is no
back-off. On a native build each attempt on a URL is capped at 10 s by the
transport's bounded request, which has its own tests. A browser build also
wraps the fetch in the executor's timer for the same 10 s; no test covers that
yet, so the cap is not claimed for the browser. The text of an error that
reaches a log line, the body of a server's answer included, is scrubbed of the
URL it was asked for, so that a key in a configured URL cannot be echoed into a
log.

**D62.4** *Known-provider list and its extension.* Options: (a) adopt the
reference set of R62.5.2 exactly (drop `coinmarketcap`, add
`livecoinwatch`); (b) adopt (a) and additionally recognize `gleeccex`, which
one live feed uses today; (c) make the recognized set configurable — a
top-level `MM2.json` array, or an extra key of the bot's start request, or
both — with (a) or (b) as the default; (d) invert the rule: accept any
provider name except the literal `unknown` and an optional deny-list. The
reference's answer is (a) and not extendable. Note that the test is a trust
decision: an unrecognized name means "the aggregator could not source this
price", and (d) removes that protection. Whichever is chosen, chapter 42 §42.5
must be rewritten to match (§62.11).

**Decided 2026-11-07 (Decision DR).** The reference set of R62.5.2 is the
default: `binance`, `coingecko`, `coinpaprika`, `forex`, `livecoinwatch`
(`coinmarketcap` is dropped, `gleeccex` is not added). A top-level `MM2.json` key
`price_providers`, an array of strings, replaces the default set when present;
a value that is not a non-empty array of strings is ignored with one WARN that
names the key, and the default applies. The literal `unknown` is never
recognized, even if listed (one WARN if it is). Only `price_provider` is tested,
against the active set. A wrong list disables pricing for every ticker whose
provider it leaves out: those records are dropped (D62.2), so the bot quotes no
pair and the snapshot stores no prices for them. Chapter 42 §42.5 is rewritten
accordingly.

**D62.5** *Order fate on a lost rate.* Whether Reloaded keeps the reference's
immediate cancellation of a pair's order when its rate is refused (R62.6.5)
and of every configured pair's orders — operator-placed ones included — when
the whole fetch fails (R62.6.6); or introduces a grace of N consecutive
failed cycles before cancelling; or restricts the all-pairs cancellation to
orders the bot itself created. The reference's answer is immediate, by pair.

**Decided 2026-11-07 (Decision DS).** Parity: the immediate cancellation of
R62.6.5 and R62.6.6 is kept, as it is in the reference and as Reloaded's bot
already does (§62.9). A conflict (D62.2) makes the pair's rate undefined, so the
pair is refused and its order cancelled in that cycle. **This behavior is under
observation in v0.4.0 and is to be revisited in the next version**: the known
side effects are that an order the operator placed by hand on a bot pair is
cancelled too, and that one failed cycle takes every order of every configured
pair off the book; a grace period and a restriction to the bot's own orders are
the options kept for that revisit.

**D62.6** *Config override key and request strictness.* (i) Whether the bot
accepts the reference's `price_urls` array in addition to Reloaded's
comma-separated `price_url` string, and which wins when both are present
(recommended: accept both, refuse a request that carries both, refuse a
malformed value instead of silently defaulting — see the R62.2.2 note).
(ii) Whether a top-level `MM2.json` key (proposed name `price_urls`, an array
of URL strings) sets the default list for *all* consumers, including the
snapshot, which the reference leaves unconfigurable (R62.2.2); and whether
the bot's request key then overrides it for the bot only. The GLEEC
compatibility table of `docs/GLEEC_COMPATIBILITY.md` would carry the new key
under the default-side rule, since no wire value changes.

**Decided 2026-11-07 (Decision DT).** The bot request accepts `price_urls`, an
array in the form of the `MM2.json` key of D62.1 (a flat array of URL strings
means independent sources, combined under D62.2; an item that is an array is one
source with in-order fallback), beside Reloaded's `price_url`, a string of
comma-separated URLs that is *one* source whose URLs are tried in order. A
request that carries both keys is refused; a malformed value of either key is
refused (the error names the key, not its value); neither silently falls back to
the default. Precedence for the bot: the request key, then the `MM2.json` key
`price_urls`, then the default. `price_urls` is the key the reference's start
request uses; `docs/GLEEC_COMPATIBILITY.md` carries the `MM2.json` keys under
the default-side rule (no wire value changes).

**D62.7** *Snapshot staleness and receive-path fetch.* Whether the snapshot
consumer applies the bot's 300 s window or a window of its own (the reference
applies none, R62.7.4), and whether a received report (R62.7.2) is priced at
receive time as the reference does or only when the report carries no
prices. The second question also appears in the parity-gaps page under
chapter 51 R50A item 5.

**Decided 2026-11-07 (Decision DU).** Parity with the reference. The snapshot
uses the sources of D62.1 and the combination of D62.2, and derives the two
prices under R62.5.1 (a)-(d) on the active provider set; it applies no age
window beyond `expire_at`. It fetches once, when the node makes the terminal
report of one of its own swaps, and only for a successful swap: a failed own swap
is stored without prices and causes no price request. The publication of the
report makes no price request of its own. A received report is priced at receive
time, whether or not the reported swap succeeded, and whatever the report
carried is not used. A conflict, a missing ticker or a failed fetch stores the
swap without prices and never fails the swap record.

## 62.11 Cross-chapter consequences

- [Chapter 42](42-price-service-and-makerbot.md): §42.4 is consistent with
  R62.4.1; §42.5's provider statements are superseded by R62.5.2 and D62.4;
  §42.5's status update on the comma-separated `price_url` is Reloaded's
  present form and is recorded in §62.9; §42.6 is consistent with §62.7 and
  now has the fetch moment and failure rule bound here. A pointer to this
  chapter was added to chapter 42 in this pass (its §42.0 note).
- [Chapter 44](44-database-persistence-and-migrations.md) §44.6.4 and R44.8.6:
  unchanged; the columns this chapter's snapshot writes are theirs.
- [Chapter 51](51-legacy-v1-swap-state-machine.md) R50A items 2, 4 and 5:
  consistent with R62.7.1–R62.7.3; no change needed. Item 4's receiver fetch
  is R62.7.2.
- [Chapter 09](09-watcher-reward-infrastructure.md): does not name where the
  reward's exchange rate comes from; R62.8.1 supplies it. No text of chapter
  09 was changed; a later watcher pass should add the pointer.
- [Chapter 02](02-baseline-state.md) §2.15A: every state read in this pass
  (`baseline anchor (R1)`, `v2.6.0-beta`, `v3.0.0-beta`,
  `dev (as of 2026-03-27)`) already has a row; no row added.
- The chapter index of [chapter 30](30-provenance-attribution.md) received a
  row for this chapter.

## 62.12 Verification notes

- **T62.1** With a local HTTP server serving a §62.4 document, start the bot
  with `price_urls` of [dead host, local server]: the first cycle logs one
  error for the dead host and prices from the local server; the second cycle
  contacts the local server first (R62.2.4).
  *Reloaded:* T62.1 does not apply, because Reloaded does not rotate (Decision
  DO); the equivalent test is that a source asks its first URL first on every
  fetch. The refusals of T62.2 (up to the missing rate), the lookup of T62.4 and
  the start-request cases of T62.7 are unit tests of the price service and the
  bot's start request (`price_service_tests.rs`, `simple_market_maker_tests.rs`);
  T62.6 is covered by `terminal_report_tests.rs`. The staleness window of T62.3
  and the cancellation of T62.2 and T62.5 are the bot's existing logic and need
  a running node; they are lab rows.
- **T62.2** Serve a document whose record for the rel coin has
  `last_price: "0"`: the pair is refused, and an existing order of the pair
  is cancelled in that cycle (R62.5.1 d, R62.6.5). Repeat with
  `price_provider: "gleeccex"` (unrecognized under R62.5.2 as shipped) and
  with `last_updated_timestamp: 0`.
- **T62.3** Serve a document whose timestamps are 301 s old with no
  `price_elapsed_validity`: refused; with `price_elapsed_validity: 600`:
  accepted (R62.6.3).
- **T62.4** Serve a document that lists `USDT` but not `USDT-PLG20`: a pair
  with `USDT-PLG20` is priced from `USDT` (R62.4.3).
- **T62.5** Stop the local server: the next cycle cancels every configured
  pair's orders, including one placed by hand for the same pair, and the bot
  keeps running (R62.6.6).
- **T62.6** Complete a swap in the lab with the local server as the only
  endpoint: the two statistics columns carry the two coins' USD prices
  (R62.7.1); with the server stopped they stay NULL and the swap still
  finishes and publishes (R62.7.3). Receive a peer's report: the columns are
  written from the receiver's own fetch (R62.7.2).
- **T62.7** Send a start request with both `price_url` and `price_urls`, and
  one with `price_urls: "x"`: the behavior must be whatever D62.6 decides,
  and tested, not left to the parser. *(D62.6 decided 2026-11-07, Decision
  DT: both requests are refused, the error naming the key; unit tests in
  `simple_market_maker_tests.rs`, PR #287, merged 2026-11-08.)*
- **V62.1** Record the probe of §62.2 (DNS, status, time, size for each
  endpoint) in the SwapLab notes before each release; D62.1 depends on it.

## 62.13 Provenance Footer

- *Inputs:* **forbidden-class input, consulted by the KDF Spec Reader under
  chapter 01 §1.4A** at the rows `v2.6.0-beta`, `v3.0.0-beta` and
  `dev (as of 2026-03-27)` of chapter 02 §2.15A, and the `baseline anchor
  (R1)` for the anchor facts of §62.1 and §62.9: the price module of the coin
  crate (endpoint list, request, response model, provider vocabulary,
  pair-rate derivation, two fallback rules), the bot's start request and
  refresh cycle, the swap-record completion and publication paths and the
  statistics receive path, the two coin-layer watcher-reward sites, the
  native and browser HTTP transports, and the reference's release notes for
  the facts about the cached third endpoint and the endpoint move. Public
  network observations of 2026-11-07 from the development host (DNS, HTTP
  status, response headers, response shape, provider names and price
  disagreement of the six endpoints named in §62.2) — permitted-input class
  R6. This repository's own tree and release notes for §62.9 and finding F1.
- *Permitted-input classes used:* R4 (the aggregator's response shape and the
  public service URLs, dictated interop); R6 (public-network observation);
  R1/R2 (the project's own tree).
- *Clean-channel discipline:* every reference fact is stated as externally
  observable behavior or dictated interface; no private identifier, function
  body, control-flow transcription, helper decomposition, log or error literal
  of the corpus was reproduced. The file:line citations in F1 and §62.9 are
  of this repository. The R62.2.2 note is marked as inferred.
- *Dirty Gate clearance (chapter 01 T1, R40).* **KDF Dirty Gate: PASS on
  2026-11-07, measure 0.02, no leaks.** The pass covered this chapter
  together with the chapter 42 §42.0 pointer note and the chapter 30 index
  row. *Extent:* the reference price-service subsystem as this chapter names
  it (built-in endpoint list, fetch request, response model, provider
  vocabulary, pair-rate derivation, the two fallback rules), the simple
  market-maker bot's start-request model, per-pair prerequisite checks and
  refresh cycle, the legacy-swap completion-snapshot sites and the
  swap-statistics receive path, the two coin-layer watcher-reward sites, and
  the reference's release notes; compared at `v2.6.0-beta`, `v3.0.0-beta` and
  `dev (as of 2026-03-27)` (nine source files and one release-notes file),
  and at the `baseline anchor (R1)` for the substrate check (two source
  files). *Method:* reading, with a mechanical comparison of every backticked
  token of the candidate against the corpus and against this repository's
  tree, each matching token classified under the forced-versus-free test; the
  gate script was not run because there are no code blocks. *Measure:* 0.02
  on the discretionary-expression basis; no private name, body, control-flow
  transcription, message wording or module tree was found. The four
  reference states named exist as rows of chapter 02 §2.15A.
- *Reloaded resolutions of 2026-11-07 (§62.9 status update, §62.10 "Decided"
  paragraphs, §62.12 note; and the clean-side decision-state pointers added
  2026-11-08 under R62.2.2, R62.5.2, R62.5.3, R62.6.5, R62.6.6, F1, §62.9 and
  T62.7, which cite those decisions and PR #287):* Reloaded-original maintainer decisions DO to DU,
  taken after the gated text above. They take no reference input and state no
  reference fact, so they need no Dirty Gate pass.
- *Authorship:* KDF Spec Reader, round 0.4.0-4, 2026-11-07; cleared by the
  KDF Dirty Gate on 2026-11-07.
