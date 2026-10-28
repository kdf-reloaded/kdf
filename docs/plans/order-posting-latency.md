# Plan: order-posting latency

**Status:** in progress (pass 1 in review; maintainer Decisions P and R, 2026-10-27) — **Last reviewed:** 2026-10-28

> **Context:** a market maker found order posting on Reloaded slower than on GLEEC KDF, about 20 % on
> UTXO and EVM coins and about 100 % on DOGE. Orders carrying `timeout_in_minutes` expired in the
> middle of a price-update round. The governing rules are CRD ch.38 R38.6.3a, R38.6.4a and R38.6.6j,
> ch.57 R57.2.1 and R57.2.6, and ch.60 R60.4.5, D60.4 and R60.6.7 (amendment and item 8). All of them
> passed the KDF Dirty Gate on 2026-10-26 or 2026-10-27. This plan records only the measurements, the
> decisions and the packaging.

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

## Decisions (maintainer)

- **Decision R.** Do all of F1–F5, in the order below.
- **Decision P.** The pre-trade check runs on create and on volume or `max` changes. It is skipped on
  price-only, minimum-volume and confirmation updates. Its fee-rate reading is reused by the same request.
- **Balance watch on shared EVM RPC endpoints (maintainer, 2026-10-28).** F2 is parity: a base coin with maker
  orders costs one balance read per 30 s against its node or RPC endpoint, as the reference does (ch.60 R60.4.5,
  R60.4.6). Several such coins on one shared EVM endpoint each cost that one read; no change is planned.
- **F6 (operator, no code).** Raise the per-session limits on the operator's own Electrum servers for the
  market maker. This gives relief until the code lands.

## Work packages

| # | Content | Rules | State |
|---|---|---|---|
| 0 | CRD branch `crd/order-post-latency` (PR #95) | all of the above | merged 2026-10-27 |
| 1 | Coder pass 1: F1 P2PK batching; F4 concurrent gas-price queries plus reuse within one request; F3(a) one fee read per request; F3(b) skip on price-only updates | R38.6.3a, R57.2.1, R57.2.6 item 1, R38.6.6j item 1, R60.6.7 amendment and item 8 | implemented (PR #114) |
| 2 | Coder pass 2: F3(c) fee cache `fee_estimate_cache_secs`; F5 read spreading `electrum_spread_reads` | R38.6.6j items 2–4, R57.2.6, R38.6.4a | after pass 1 |
| 3 | F2 balance watch (order-bearing base coins, interval, restart seeding) | R60.4.5, R60.4.6 (D60.4 resolved, PR #96) | implemented (PR #116) |

Each code PR is checked end to end by the swaplab session's timing run once it is built.
