# Chapter 42 -- Price Service & Makerbot

**Status:** driving-spec (as-built baseline **plus** implemented divergence
corrections). Mixed treatment -- see §42.0.

> **One-sentence claim:** the project shall provide an automated market-making
> ("makerbot") loop that periodically fetches reference prices from a configured
> price-aggregator endpoint and (re)places maker orders for a registry of trading
> pairs according to per-pair spread, volume, confirmation, and price-floor
> settings -- with the price endpoint treated as re-pointable configuration and
> the price-provider set kept current.

## 42.0 Treatment & scope split

- **§42.1--§42.4 (T-DOC, as-built):** verified present -- the
  `start_simple_market_maker_bot` / `stop_simple_market_maker_bot` RPC pair, the
  per-pair maker configuration registry, the periodic price-fetch-and-refresh
  loop, and the price-aggregator response shape consumed.
- **§42.5 (divergence corrections):** provider-set refresh and endpoint failover
  are implemented in reloaded.
- **§42.6 (T-PORT):** fiat price-at-completion snapshot persistence is
  implemented in reloaded.

> **Binding scope (R36).** Requirements bind observable behaviour, the public RPC
> method strings and request/response field names, configuration keys, and the
> externally *dictated* price-aggregator JSON response shape. The set of upstream
> price providers (CoinGecko, CoinMarketCap, etc.) and their APIs are external
> dictated interop. Private types and helper structure are informative.

---

## Part A -- As-built baseline (T-DOC)

## 42.1 Start / stop RPC

R42.1.1 The public RPC `start_simple_market_maker_bot` shall start the
market-making loop. Its request shall carry:
- `price_url` (optional) -- the price-aggregator endpoint to fetch from; when
  omitted it defaults to the project's public price-aggregator endpoint (a
  re-pointable value, see §42.5);
- `bot_refresh_rate` (optional) -- the loop period in seconds; and
- a registry mapping each trading-pair key to its per-pair configuration.

R42.1.2 The public RPC `stop_simple_market_maker_bot` shall stop the loop. The
bot shall expose an observable lifecycle of at least *stopped*, *running*, and
*stopping* states; starting an already-running bot or stopping an already-stopped
bot shall be reported as such rather than silently ignored.

## 42.2 Per-pair maker configuration

R42.2.1 Each registry entry shall be a per-pair configuration carrying at least
the following request fields (names are part of the contract):
- `base`, `rel` -- the pair;
- `spread` -- the multiplier applied to the reference price to set the maker
  price;
- `min_volume`, `max_volume` -- each expressed as either a `percentage` of
  balance or a `usd` amount;
- `max` -- use the maximum available volume;
- `base_confs`, `base_nota`, `rel_confs`, `rel_nota` -- per-order confirmation /
  notarization requirements;
- `enable` -- whether the pair is actively quoted;
- `price_elapsed_validity` -- maximum age (seconds) of a reference price before
  it is considered stale;
- `check_last_bidirectional_trade_thresh_hold` -- gate quoting on recent
  bidirectional trade activity;
- `min_base_price`, `min_rel_price`, `min_pair_price` -- price floors below which
  the pair is not quoted.

## 42.3 Refresh loop & price calculation

R42.3.1 The bot shall, every `bot_refresh_rate` seconds, fetch the reference
prices once from `price_url`, then for each enabled pair compute the maker price
as the reference price scaled by `spread`, enforce the price floors and staleness
check, compute the order volume from the volume settings, and create/update the
corresponding maker order (cancelling/replacing prior bot orders as needed).

R42.3.2 A reference price older than `price_elapsed_validity`, or missing for a
pair, shall cause that pair to be skipped for that cycle rather than quoting on
stale data.

## 42.4 Price-aggregator response shape (R31 externally dictated)

R42.4.1 The price-fetch shall consume the aggregator's per-ticker JSON, whose
fields include at least: `ticker`, `last_price`, `last_updated`,
`last_updated_timestamp`, `volume24h`, `change_24h`, an optional `sparkline_7d`,
and a per-metric provider attribution (`price_provider`, `volume_provider`,
`sparkline_provider`, `change_24h_provider`). This shape is dictated by the
aggregator API, not by this project.

---

## Part B -- Divergence corrections (§42.5) & required port (§42.6)

## 42.5 Price-provider set & endpoint configurability

> **Status update (reloaded).** The provider-set divergence is corrected:
> deprecated provider variants are removed, replacement provider variants are
> present, and unknown provider values remain tolerated for forward
> compatibility.
>
> **Port result (reloaded):** this recommendation is implemented.

> **Configurability requirement.** R42.5.1 The price endpoint shall remain fully
> re-pointable via `price_url`; no hostname shall be hard-required in normative
> behaviour. A deployment may point the bot at its own aggregator that serves the
> §42.4 response shape. The shipped default value is a convenience, not a
> contract.

> **Status update (reloaded):** `price_url` accepts a comma-separated endpoint
> list and the fetch logic falls back across endpoints on transport failures.

## 42.6 Fiat price at swap completion (T-PORT, implemented)

R42.6.1 When a swap completes, the project shall record the fiat (e.g. USD)
reference price of the swapped coins **as of the moment of completion** so that
both per-wallet swap history and aggregate swap stats can expose the
contemporaneous price rather than the current one. On native SQLite, the
GLEEC-compatible storage location is the aggregate `stats_swaps` row; per-wallet
history may read the same values by swap `uuid`.

> **Status of §42.6:** implemented in reloaded. At swap completion, fiat-price
> snapshots are fetched, stored in the GLEEC-compatible aggregate stats columns,
> and exposed through per-wallet swap-history RPCs by `uuid`.

## 42.7 Acceptance criteria (chapter)

- `start_simple_market_maker_bot` starts the loop with a pair registry; the loop
  quotes per-pair using `spread`, volume settings, floors, and staleness checks;
  `stop_simple_market_maker_bot` stops it (Part A).
- The price endpoint is overridable via `price_url` and defaults to the shipped
  value (R42.5.1).
- Provider attribution stays current and deserialization remains tolerant to
  unknown future provider values.
- Price fetch supports endpoint fallback via comma-separated `price_url`.
- A completed swap records its moment-of-completion fiat price (R42.6.1).
