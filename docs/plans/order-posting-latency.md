# Plan: order-posting latency

**Status:** in progress (pass 1 merged; F2 and pass 2 in review; maintainer Decisions R, AC and AD) — **Last reviewed:** 2026-10-28

> **Context:** a market maker found order posting on Reloaded slower than on GLEEC KDF, about 20 % on
> UTXO and EVM coins and about 100 % on DOGE. Orders carrying `timeout_in_minutes` expired in the
> middle of a price-update round. The governing rules are CRD ch.38 R38.6.3a, R38.6.4a, R38.6.6j and
> R38.6.6k, ch.57 R57.2.1, R57.2.6 and R57.2.7, and ch.60 R60.4.5, R60.4.6 and R60.6.7 (amendment and
> item 8). Those of 2026-10-26 and 2026-10-27 passed the KDF Dirty Gate. R38.6.6k, R57.2.7 and the
> rewritten R60.6.7 amendment of 2026-10-28 are Reloaded-original and clean-side, with no forbidden-class
> input. This plan records only the measurements, the decisions and the packaging.

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

## Decisions (maintainer)

- **Decision R.** Do all of F1–F5, in the order below.
- **Decision P (2026-10-27, superseded by AC).** The pre-trade check ran on create and on volume or `max`
  changes, and was skipped on price-only, minimum-volume and confirmation updates. Its fee-rate reading was
  reused by the same request. The skip was accepted only until a fee cache existed.
- **Decision AC (2026-10-28).** The pre-trade check runs on every order creation and every maker-order
  update, price-only included, and its fee-rate input may come from the fee cache (ch.60 R60.6.7 amendment).
  It lands in pass 2, together with the cache.
- **Decision AD (2026-10-28).** `fee_estimate_cache_secs` can be overridden per coin in the coins
  configuration, and a coin with an `avg_blocktime` defaults to that block time, bounded to 2–600 s. There
  is no unbounded mode. An explicit top-level `MM2.json` value ranks above the block time, so a top-level
  `0` still restores GLEEC KDF behaviour in one switch (ch.38 R38.6.6k). EVM gas-price readings are
  shared per chain, not per token (ch.57 R57.2.7).
- **F6 (operator, no code).** Raise the per-session limits on the operator's own Electrum servers for the
  market maker. This gives relief until the code lands.

## Work packages

| # | Content | Rules | State |
|---|---|---|---|
| 0 | CRD branch `crd/order-post-latency` (PR #95) | all of the above | merged 2026-10-27 |
| 1 | Coder pass 1: F1 P2PK batching; F4 concurrent gas-price queries plus reuse within one request; F3(a) one fee read per request; F3(b) skip on price-only updates | R38.6.3a, R57.2.1, R57.2.6 item 1, R38.6.6j item 1, R60.6.7 amendment and item 8 | implemented (PR #114) |
| 2 | Coder pass 2: F3(c) fee cache `fee_estimate_cache_secs` with per-coin lifetime and per-chain gas price; F5 read spreading `electrum_spread_reads`; spend check on every update again | R38.6.6j items 2–4, R38.6.6k, R57.2.6, R57.2.7, R38.6.4a, R60.6.7 amendment (AC) | in progress |
| 3 | F2 balance watch (order-bearing base coins, interval, restart seeding) | R60.4.5, R60.4.6 (D60.4 resolved, PR #96) | ready; after pass 1 |

Each code PR is checked end to end by the swaplab session's timing run once it is built.
