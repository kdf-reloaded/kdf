# Plan: limit the memory allocator's arenas at startup

**Status:** in progress (CRD PR, then code PR) — **Last reviewed:** 2026-11-01

> **Context.** A maker that updates hundreds of orders many times a second holds more and more memory, although the data
> it holds stays the same. The lab traced this to the system allocator (glibc): each thread that allocates can get an
> arena of its own, memory freed inside an arena is mostly not reused by the others, and the daemon runs several hundred
> threads on a many-core host and about 30 on an 8-core host. Limiting the arenas to two removed the growth in the lab.
> This is a Reloaded-original change. It is not part of the compatibility contract, and it changes no order, swap, wallet
> or network behavior. The binding rules are CRD ch.45 §45.6A (R45.6A.1–7, T45.10–T45.13). Decision BJ, 2026-10-31:
> the limit goes in **before** the first beta, with minimal verification. The finding is recorded in issue #191.

## What was measured

One maker, 480 orders on 16 pairs, each order updated about every 6 seconds (about 80 updates per second), 40 minutes of
load, the same binary each time, `RUST_LOG=debug`.

| Run | Arenas | Memory growth from minute 10 | Data held by the daemon (system − free) |
|---|---|---|---|
| Allocator default, 256-core host | 778–782 | +1.17 MB/min | 46 → 63 MB, flat from minute 20 |
| Allocator default, maker limited to 8 cores | 36 | +1.02 MB/min | 37 → 55 MB, flat from minute 20 |
| `MALLOC_ARENA_MAX=2`, 256-core host | 2 | −0.04 MB/min | 44 → 62 MB, flat from minute 20 |

| Finding | Evidence |
|---|---|
| **It is not a leak and not the order book.** The order book, the trie store, the maker outbox, the send queue and the data directory stayed flat. | `orderbook.len` 480, `memory_db` 43 KB, `history.deltas` about 22 k, `send_queue` 0, data directory constant to the KB, file-backed memory constant. |
| **The growth is free memory the allocator keeps.** The live heap is the same in all runs, and the allocator's total grows only without the limit. | System heap 188 → 251 MB (default, 256 cores), 94 → 107 MB (limit 2). |
| **Trimming returns little.** The trim that runs after each 600-second purge gave back 3–12 % of the resident memory in the default runs; a manual trim after the load stopped gave back about 5 %. | Free memory sits in many small pieces spread over the arenas, mostly below a page. |
| **It does not depend on the host size.** The 8-core run shows nearly the same growth as the 256-core run. | 36 arenas still hold and grow the free memory. |
| **The limit costs nothing we could measure at this load.** | 81.8 against 81.3 updates per second; the same 6.0 s median round; 22.3 % against 21.7 % of one core. Per-call latency was not recorded. |
| **Threads.** About 776 on the 256-core host: two multi-threaded runtimes (`common/wio.rs`, `mm2_p2p/src/runtime.rs`) at one worker per core each, and the async-std executor at one thread per core. | Thread census by name from `/proc/<pid>/task`. |
| **Not known:** whether the growth levels off in a longer run (the free memory was still rising at minute 40 with 36 arenas), and how a limit of two behaves under swaps and activation. | See "Deferred". |

## The change (before the first beta)

Spec: CRD ch.45 §45.6A. Code, after the CRD PR is merged, by `kdf-coder`:

1. One allocator call, `mallopt(M_ARENA_MAX, n)`, in the start-up path of `mm2src/mm2_main/src/mm2.rs` after the
   configuration is read and before the daemon starts its runtimes (the new module `malloc_arena.rs` holds the
   decision and the call; `run_lp_main_impl` calls it only for the native daemon). Linux with glibc only; not in the
   embedded-library path, which calls `run_lp_main` inside the process of a host application.
2. The value: `MALLOC_ARENA_MAX` in the environment wins (no call from us); otherwise the key `malloc_arena_max` in
   `MM2.json` (`0` leaves the allocator's default); otherwise `2`. An invalid value gives a warning and `2`. A failed call
   gives a warning. Nothing refuses the launch.
3. One information line at launch states the outcome and its source.
4. A pure decision function with a table test, and a Linux test of the call itself.
5. CHANGELOG entry in plain words (references the finding's issue, does not close it), one README sentence for the key.

## Verification on the new head (minimal, Decision BJ)

- 40-minute soak with 480 orders and about 80 updates per second, default limit: memory growth below 0.3 MB/min from
  minute 10, at most 3 arenas in the allocator statistics, update rate and processor use as in the run with the
  environment variable.
- The smoke row: processor use and memory at idle.
- A 5-minute configuration check: key `0` shows as many arenas as before; the startup lines match the source in each
  case (default, key, environment, `0`).

## Deferred (post-beta)

Written down so the cost of the beta decision is visible. Decision BJ accepted that swaps under the limit stay
unmeasured for the beta.

- **Rows a–c** (D45.4): start-up and activation time with the limit and without it (a UTXO set, an EVM coin with
  tokens, a shielded coin); a short swap set (three UTXO pairs, one EVM cell, a shielded swap if the lab allows);
  latency of the query methods (orderbook, my orders, balance, withdraw) and of `update_maker_order` per call, while
  100 orders are updated at 20 updates per second. One build serves as both arms: the key `0` switches the limit off.
  Run the arms interleaved on the same host. Accept the limit when each measure of the limited arm lies within the spread
  of two repeats of the other arm, with a margin of 10 % on medians and 25 % on the 90th percentile. A single miss gets
  one repeat; two misses in the same direction on the same measure reject the default of `2` (then try `4` or `8`).
- **Plateau run** (D45.5): 3 hours at 20 updates per second on the 8-core setting, with and without the limit.
- **Executor sizing** (D45.3): size the three executors explicitly (a setting, or a cap such as the smaller of the core
  count and 16), instead of one worker per core each.
- If a larger limit performs better under swaps, change the default, not the mechanism.

## Not in scope

No change to the order book, the trie store, the outbox or the gossip layer; no memory limit on the process; no change on
other operating systems.
