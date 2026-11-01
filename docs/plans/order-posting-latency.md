# Plan: order-posting latency

**Status:** in progress (all passes merged, Decision AM in PR #149; timing run done 2026-10-31; post-beta: read spreading by round trip, Decision BI) — **Last reviewed:** 2026-10-31

> **Context:** a market maker found order posting on Reloaded slower than on GLEEC KDF, about 20 % on
> UTXO and EVM coins and about 100 % on DOGE. Orders carrying `timeout_in_minutes` expired in the
> middle of a price-update round. The governing rules are CRD ch.38 R38.6.3a, R38.6.4a, R38.6.6j and
> R38.6.6k, ch.57 R57.2.1, R57.2.6 and R57.2.7, and ch.60 R60.4.5, R60.4.6 and R60.6.7 (amendment and
> item 8), and ch.12 R7a and R7b (Decision AM). Those of 2026-10-26 and 2026-10-27 passed the KDF
> Dirty Gate. R38.6.6k, R57.2.7, the rewritten R60.6.7 amendment of 2026-10-28, and ch.12 R7a and R7b
> are Reloaded-original and clean-side, with no forbidden-class input. This plan records only the
> measurements, the decisions and the packaging.

## What was measured (2026-10-26)

- **Per-session rate limit.** Public ElectrumX servers serve a session about 26 requests at about 15 ms
  each, then about one request message per second. A JSON-RPC batch counts as one message. Idle time does
  not reset the limit; only a new session does.
- **Consequence.** KDF keeps one long-lived session per server and sends to the first one, so every
  sequential request of an order post costs about a second.
- **Legacy addresses.** A balance or UTXO read of a legacy address sent the P2PK lookup as a second
  request. A DOGE `setprice` therefore made 4 sequential requests where 2 batched ones suffice.
- **Balance polling.** Every activated coin was polled every 10 s, whether or not any order needed it.
- **Dynamic-fee legs.** The pre-trade check re-read the fee estimate of a dynamic-fee leg on every create
  and every update, including price-only ones.
- **EVM.** The gas price was resolved twice per `setprice`, with its node queries made one after another.

## Acceptance target (field report, 2026-10-28)

A market maker runs 5 base coins against 200 or more coins each, about 1,000 orders per node. Each loop
re-posts every order with `setprice` (cancel plus create, not `update_maker_order`), with a `my_balance`
per coin before each order. On the implementation before this plan the full loop took 7 to 11 minutes.

- **Target.** A loop of 200 orders against one base coin is not slower than it was, and is faster once
  F1–F5 have landed.
- **Measured by** the swaplab session's timing run, against the same loop shape.
- **Result (2026-10-31).** Met by the code: with reads routed as before this plan, a 200-order RIN loop
  took 24.5 s against 41.8 s, 41 % faster. With the default switches, a coin whose server list holds one
  slow or distant server pays that server's round trip on one read in N, and the same RIN loop took
  61.8 s, 48 % slower. The defaults stay (Decision BI): they serve operators whose public servers limit
  each session, which is the case this plan was opened for. On DOGE, whose servers do, the loop was
  75 % faster. Detail in the next section.

## Timing run (swaplab, 2026-10-31)

The swaplab session ran the loop of the acceptance target against one base coin: 200 × `my_balance`
plus `setprice`, on the build before this plan (`d87cff376`) and on the beta head (`97f63a04d`), on
one host against the same Electrum servers. Each loop ran on a freshly started node. RIN ran twice,
the second time with the beta head first.

- **RIN, default switches.** The loop took 61.8 s against 41.8 s (first pair: 63.1 s against 42.8 s).
  Of the three servers on the RIN list, one answered a balance query in about 227 ms from the lab host,
  the other two in about 25 ms.
  - Each pass of the loop makes three RIN reads, and spreading sends them to the three servers in
    turn. Every pass therefore pays one round trip to the slow server: about 0.30 s per pass, against
    0.19 s before, when every read went to the first listed server.
  - The turn counter decides which request pays for it. In the first pair it was `setprice` for the
    first 30 s (median 0.27 s, `my_balance` 0.04 s), then `my_balance` (0.23 s, `setprice` 0.07 s)
    until about 60 s, then `setprice` again. The maker balance watch reads the balance every 30 s, and
    each of its reads moves the counter by one. In the second pair `setprice` paid in every block of
    20 calls, at an even 0.27 s from the first call: the slow part is not a warm-up.
- **RIN, reads routed as before.** With `electrum_spread_reads` `false` and `electrum_server_order`
  `listed`, every read goes to the first listed server, as before this plan. The loop took 24.5 s
  against 41.8 s, 41 % faster (`my_balance` median 0.031 s, `setprice` 0.073 s, flat over the run).
- **DOGE, default switches.** The loop took 298.4 s against 1193.8 s, 75 % faster. The `setprice`
  median fell from 4.03 s to 1.02 s and the `my_balance` median from 1.98 s to 0.91 s, flat over the
  run. Two of DOGE's three listed servers were reachable. The timings match the per-session limit
  measured on 2026-10-26, about one second per message. The old build sent six messages per pass, one
  after another, to one server. The beta head sends three, with the P2PK lookup batched, and spreads
  them over both servers.

## Decisions (maintainer)

- **Decision R.** Do all of F1–F5, in the order below.

- **Decision P (2026-10-27, superseded by AC).** The pre-trade check ran on create and on volume or `max`
  changes, and was skipped on price-only, minimum-volume and confirmation updates. Its fee-rate reading was
  reused by the same request. The skip was accepted only until a fee cache existed.
- **Decision AC (2026-10-28).** The pre-trade check runs on every order creation and every maker-order
  update, price-only included, and its fee-rate input may come from the fee cache (ch.60 R60.6.7 amendment).
  It lands in pass 2, together with the cache.
- **Decision AD (2026-10-28).** `fee_estimate_cache_secs` can be overridden per coin in the coins
  configuration (0–600 s), and an explicit top-level `MM2.json` value (0–30 s) ranks next. A top-level `0`
  therefore still restores GLEEC KDF behaviour in one switch. There is no unbounded mode. EVM gas-price
  readings are shared per chain, not per token (ch.57 R57.2.7).
- **Decision AI (2026-10-28).** A coin with neither a per-coin value nor an explicit top-level value gets an
  adaptive lifetime. It starts at the coin's `avg_blocktime` (2–600 s; 15 s without one), doubles each time a
  fresh reading repeats the previous one, caps at 600 s and resets on a change. A chain with a fixed gas
  floor therefore converges to one reading per 10 minutes without configuration (ch.38 R38.6.6k).
- **Relay fee and block count (2026-10-28).** The forced minimum relay fee is cached across requests like
  the fee estimate. `blockchain.headers.subscribe`, this client's block-count read, spreads like the other
  reads. An invalid `electrum_spread_reads` warns and is taken as `true`.
- **Lock scope (2026-10-28).** The reconciliation of own orders missing from the orderbook reads the
  balance and the maximum volume without holding the order's lock, and locks only to apply the result.
  The other lock holds across file and database writes are Decision AM.
- **Decision AM (maintainer, 2026-10-28): fix now, before the beta.** The balance-update handler, the
  periodic balance check, the timed-out match removal, an order's timeout expiry, the cancel paths and
  `update_maker_order` no longer hold an order's own lock across file or database writes. Each copies
  under a brief lock, writes unlocked, then re-locks and re-checks before it applies. Writes of one order
  keep the order of its changes, and nothing recreates a cancelled order's record (ch.12 R7a, R7b; PR #149).
  The match-negotiation paths keep their write under the lock and join the same write ordering.
- **Balance watch on shared EVM RPC endpoints (maintainer, 2026-10-28).** F2 is parity: a base coin with maker
  orders costs one balance read per 30 s against its node or RPC endpoint, as the reference does (ch.60 R60.4.5,
  R60.4.6). Several such coins on one shared EVM endpoint each cost that one read; no change is planned.
- **Decision BI (maintainer, 2026-10-31).** `electrum_spread_reads` `true` and `electrum_server_order`
  `random` stay the defaults for the beta. The cost is documented: spreading sends one read in N to each
  of N connected servers, a slow or distant one included. An operator who meets it removes distant
  servers from the coin's list, or sets `electrum_spread_reads` to `false` and `electrum_server_order`
  to `listed`. After the beta, reads are spread only over the connected servers whose recent round trip
  is close to the fastest one: a Reloaded-original amendment of ch.38 R38.6.4a, then code.
- **F6 (operator, no code).** Raise the per-session limits on the operator's own Electrum servers for the
  market maker. This gives relief until the code lands.

## Work packages

| # | Content | Rules | State |
|---|---|---|---|
| 0 | CRD branch `crd/order-post-latency` (PR #95) | all of the above | merged 2026-10-27 |
| 1 | Coder pass 1: F1 P2PK batching; F4 concurrent gas-price queries plus reuse within one request; F3(a) one fee read per request; F3(b) skip on price-only updates | R38.6.3a, R57.2.1, R57.2.6 item 1, R38.6.6j item 1, R60.6.7 amendment and item 8 | merged 2026-10-28 (PR #114) |
| 2 | Coder pass 2: F3(c) fee cache `fee_estimate_cache_secs` with per-coin lifetime and per-chain gas price; F5 read spreading `electrum_spread_reads`; spend check on every update again | R38.6.6j items 2–4, R38.6.6k, R57.2.6, R57.2.7, R38.6.4a, R60.6.7 amendment (AC) | implemented (PR #128) |
| 3 | F2 balance watch (order-bearing base coins, interval, restart seeding) | R60.4.5, R60.4.6 (D60.4 resolved, PR #96) | merged 2026-10-28 (PR #116) |
| 4 | Read spreading only over the connected servers whose recent round trip is close to the fastest (Decision BI) | R38.6.4a amendment, to be written | after the beta |

Each code PR is checked end to end by the swaplab session's timing run once it is built.
