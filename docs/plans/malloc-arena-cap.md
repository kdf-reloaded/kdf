# Plan: limit the memory allocator's arenas at startup

**Status:** parked (the arena limit shipped in PRs #189, #190; measured in PR #225; executor sizing and an allocator comparison wait for their triggers) — **Last reviewed:** 2026-11-08

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

Spec: CRD ch.45 §45.6A. Shipped: CRD PR #189 and code PR #190, both merged 2026-11-01:

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

*Run on 2026-11-01 on `9f0ec1c83` (`dev` after PR #190), all PASS (release tests 0.3.0-beta.1, rows
"Arena cap"): the configuration checks; the default soak, +0.26 MB/min over minutes 10–40 with 2
arenas, a plateau not yet measured; the smoke row. The plateau runs are in the next section
(PR #225, merged 2026-11-04).*

- 40-minute soak with 480 orders and about 80 updates per second, default limit: memory growth below 0.3 MB/min from
  minute 10, at most 3 arenas in the allocator statistics, update rate and processor use as in the run with the
  environment variable.
- The smoke row: processor use and memory at idle.
- A 5-minute configuration check: key `0` shows as many arenas as before; the startup lines match the source in each
  case (default, key, environment, `0`).

## Results of the measurements (2026-11-03)

Two plateau runs and the three rows of D45.4 were run on the lab host (256 cores, a permanent host CPU miner on cores 0–7)
with one binary (`9f0ec1c83`), the key `malloc_arena_max` set to `0` as the "limit off" arm. The acceptance rules were written
before the data; where the data forced a choice, the deviation is listed below. All times are client side. The raw files stay
in the lab tree.

**What these runs do and do not show.** The limit is **not** shown to be necessary for a plateau at 20 updates per second on
8 cores: without it memory also levels off. With the limit, resident memory is about 17 MB lower in the plateau runs (an
observation: the arms ran one after the other, and a level comparison was not part of the pre-registered rule) and the growth
measured at about 80 updates per second on 256 cores stays removed. 256 cores at plateau length and 80 updates per second at
plateau length were not run. No cost of the limit was resolved on start-up, activation, swaps or query latency.

### Plateau run (D45.5)

480 orders on 16 pairs, about 20 updates per second (19.79 and 19.72 per second measured over the whole runs), maker on cores
0–7 (the "8-core setting"), `RUST_LOG=debug`, 6.5 hours per arm, capped arm first, then the arm with `malloc_arena_max` `0`.
Slopes in MB/min, with the standard error, on the fixed-phase points (taken 71–121 s after each 600-second purge tick;
33 per run); window means in MB.

| | With the limit (2 arenas) | Without the limit (36–37 arenas) |
|---|---|---|
| Minutes 60–180, resident memory | +0.043 (first judgment: 0.049 on 1-minute points) | +0.073 ± 0.017 |
| Minutes 60–180, resident minus in-use | +0.083 | +0.095 ± 0.037 |
| Minutes 60–180, in-use (live data) | −0.050 (first judgment) | −0.022 ± 0.044 |
| Minutes 60–180, window difference (150–180 minus 60–90), RSS | +4.41 (1-minute points) | +6.58 |
| Verdict after 180 minutes | inconclusive → extended to 360 | inconclusive → extended to 360 |
| Minutes 180–360, resident memory | +0.004 ± 0.003 | +0.019 ± 0.009 |
| Minutes 180–360, resident minus in-use | +0.015 ± 0.024 | +0.025 ± 0.022 |
| Minutes 180–360, in-use | −0.012 ± 0.025 | −0.006 ± 0.024 |
| Minutes 180–240 → 300–360, resident memory | 88.06 → 88.37 (+0.31) | 104.59 → 106.83 (+2.24) |
| Minutes 180–240 → 300–360, resident minus in-use | 59.67 → 61.61 (+1.94) | 77.16 → 81.08 (+3.92) |
| Verdict (both slopes < 0.05 and difference < 6 MB) | **PLATEAU** | **PLATEAU** |

Reading. Both arms level off by the registered rule. With the limit, resident memory showed no measurable drift in hours 3
to 6 of a 6.5-hour run (0.004 ± 0.003 MB/min); this is not a proof of a bound: a drift of up to about 0.01 MB/min (about
14 MB/day) is not excluded. Without the limit, a small residual drift (0.019 ± 0.009 MB/min, about 27 MB/day as a point
estimate) is not excluded either. The two arms do not differ in drift at this resolution (0.015 apart at a combined standard
error of 0.0095). They differ in level and in arenas: resident memory was about 17 MB lower (16.5 MB at minutes 180–240,
18.5 MB at 300–360), entirely in memory the allocator keeps after it is freed (resident minus in-use: +17.5 and +19.5 MB;
in-use: −1.0 MB), with 2 arenas against 36–37. The one arena added without the limit in the middle of the run (36 → 37,
about minute 225) brought no measurable step in resident memory (+0.15 MB against about 0.7 MB scatter). The split of
resident memory into live data and retained memory scatters by about 5 MB per point, the two parts mirroring each other, so no
trend in either part is resolved.

Deviations and notes. (1) The live-heap test was registered on minutes 300–360; its standard error turned out to be 0.12–0.13
MB/min (5–6 points), so it is **unresolved**; the figures above on 180–360 are a **post-hoc** substitute. For the second arm
the window (the arm's own judging window, fixed-phase points, a finding needs a slope ≥ 0.05 and more than 2 standard errors)
was fixed before its data. (2) The primary series for the verdicts were fixed-phase points; 1-minute and 10-minute series
supported only, and the 1-minute standard error is not used because the purge saw-tooth autocorrelates it. (3) A sentence in
the first judgment that the retained memory had "stepped" was withdrawn. (4) One 1-minute RSS reading appears in both the
10-minute and the fixed-phase series (3 of 36 points); the verdicts rest on the fixed-phase series alone. (5) The arms ran
one after the other on the same host, not interleaved.

### Row a: start-up and activation

Warm runs from a copy of the plateau maker's data (n 3 per arm, interleaved C O C O C O), then one cold run per arm; the
whole set was repeated once (3 + 3, warm only) in a no-build window.

| Measure (seconds) | With the limit | Without the limit |
|---|---|---|
| Start to first `version` answer, first set | 10.60 / 25.78 / 10.59 | 9.33 / 10.06 / 9.83 |
| Start to first `version` answer, repeat | 11.10 / 5.94 / 9.99 | 9.68 / 26.24 / 9.61 |
| Pooled 6 per arm, median | 10.60 | 9.76 |
| Activation, UTXO set (RIN, DOGE, LTC), total | 0.37 / 0.34 / 0.34; repeat 0.338 / 0.324 / 0.319 | 0.34 / 0.34 / 0.37; repeat 0.419 / 0.358 / 0.337 |
| Activation, MON + USDC-MON (EVM) | median 2.48 (repeat 2.53) | median 1.87 (repeat 2.23) |
| Activation, ARRR (shielded) | 12.03 ×3 (3-second poll); repeat 10.083 / 10.583 / 9.835 (0.25-second poll) | 12.04 ×3; repeat 10.835 / 9.823 / 10.099 |
| End state memory (warm, 5 s after the last activation) | 117.2–118.7 MB, 2 arenas | 122.8–125.3 MB, 46–48 arenas |

Verdict: **PASS.** Medians are inside the off spread plus 10 %. The first set had one miss on the 90th percentile (the capped
run of 25.78 s); the repeat did not reproduce it, and an equal stall (26.24 s) occurred in an off run. In both, the extra
time lies between starting the container and the first KDF log line, not in KDF's own start-up, and both coincided with
disk-heavy work by other sessions (a build; a worktree checkout): the cause is probable, not proven. The off outlier widens
the off spread; nothing was excluded. The harness polled ARRR every 3 seconds in the first set, so that measure was not
informative there; at 0.25 seconds it shows no difference. Wording: no start-up or activation cost of the limit was resolved
(the MON + USDC-MON spread is 64 % of its median).

### Row b: swaps

One swap per pair and arm, the node under test as maker, alternating order: DOGE, KMD, LTC (three UTXO pairs) and USDC-MON
(one EVM cell). A shielded swap was not run.

| Swap | Wall s (with / without) | Peak maker memory MB | Arenas | Threads |
|---|---|---|---|---|
| DOGE | 692 / 449 | 41.1 / 41.3 | 2 / 33 | 30 / 30 |
| KMD | 1202 / 1207 | 41.3 / 41.7 | 2 / 32 | 30 / 30 |
| LTC | 260 / 521 | 41.0 / 42.1 | 2 / 33 | 31 / 30 |
| USDC-MON | 234 / 362 | 44.9 / 45.5 | 2 / 33 | 30 / 30 |

Verdict: **PASS.** 8 of 8 swaps completed; no panic and no fatal line in the 16 logs; peak memory within 1.1 MB. The warnings
are one message type (no known peers to dial; the lab node has none by design) and follow the log length. The error classes
are the same in both arms (a lookup of the swap's own transaction answered before it had propagated; a poll before the swap
exists; a saved-swap read once per LTC run); the only differences are one extra lookup error of that class in the taker of
the off DOGE and off LTC runs. Wall times are chain-bound (confirmations) and differ 1.5–2 times between equal swaps in both
directions: indicative only.

### Row c: query latency while 100 orders update at 20 per second

(The load was 100 maker orders on 10 pairs, `update_maker_order` at 20 calls per second for 2349 seconds, about 39 minutes,
47000 timed calls per run, 65 seconds of tail before the next run; a second client on other cores called orderbook, `my_orders`,
`my_balance` and withdraw (RIN, build only) 200 times each per run, one round about every 7 seconds from minute 15 to the end of
the load; default log level; the node on cores 0–7 beside the host miner.) Per-run
p50 / p90 in milliseconds, order C1 O1 C2 O2 C3 O3 (C = with the limit, O = without):

| Call | C | O |
|---|---|---|
| `update_maker_order` | 26.69/46.72, 26.86/45.58, 26.57/45.53 | 26.97/46.08, 26.61/45.12, 27.01/47.30 |
| orderbook | 4.00/4.39, 4.00/4.29, 3.98/4.38 | 4.03/4.57, 4.00/4.46, 4.07/4.39 |
| `my_orders` | 2.79/4.75, 2.79/5.15, 2.82/5.13 | 2.81/5.13, 2.84/4.94, 2.81/5.17 |
| `my_balance` | 61.64/240.24, 46.10/239.40, 36.78/240.05 | 42.70/242.14, 40.06/242.22, 36.86/239.94 |
| withdraw (build only) | 268.12/362.50, 267.38/344.50, 268.44/286.05 | 269.16/296.75, 269.62/301.95, 266.94/280.16 |

Rule and reading. The rule sentence ("each measure of the limited arm lies within the spread of two repeats of the other arm,
with a margin of 10 % on medians and 25 % on the 90th percentile") does not say whether the measure is the median of the
three per-run values or each run; it is ambiguous, so the stricter per-run reading binds. On the arm-level reading every
measure passes (the `my_balance` median p50 46.10 against the limit 46.97); on the per-run reading there is one miss
(`my_balance` p50 of C1, 61.64 against 46.97). The capped run was always first in each pair and the Electrum-bound calls
(`my_balance`, `withdraw`) sped up over the afternoon in both arms, so the original order cannot separate the two; the miss
therefore got its repeat in the reversed order (O then C), written down as a deviation before it ran.
Repeat (O4 18:06, then C4 18:48; same procedure; the judging was fixed before the run, with the off spread O1–O4): `my_balance`
p50 of C4 37.05 against the limit 1.10 × 42.70 = 46.97: **pass**; the other measures of C4 (p50 / p90, ms): `update_maker_order`
26.67 / 44.65 (off 26.48–27.01 / 44.60–47.30), orderbook 4.02 / 4.40, `my_orders` 2.82 / 5.29 (p90 0.05 above the off maximum,
inside the 25 % margin), `my_balance` p90 238.94, withdraw 267.25 / 279.66 (off 266.94–269.62 / 277.65–301.95): all within.
The C-minus-O difference of `my_balance` p50 over the five pairs in time order is +18.94, +6.04, −0.08 and, with the order
reversed, −1.73 ms; of withdraw p90 +65.75, +42.55, +5.89 and +2.01 ms: the difference shrinks over the day, the way the
decline over time in both arms predicts. The capped maximum excess of C1 and C2 did not recur (C4 `my_balance` maximum 253.9
against 359.9 in O4; withdraw 453.9 against 488.7). Verdict: **PASS** on the binding (per-run) reading: one miss (C1,
`my_balance` p50), repeated once in the reversed order, and not reproduced. Wording: per-run C1 miss on `my_balance` p50;
with the order reversed in one extra pair the capped run was within the off spread. In the reversed pair the off run had the higher maximum on both Electrum-bound calls. `withdraw` p90 is higher in the capped
run in all four pairs (+65.75, +42.55, +5.89 and, reversed, +2.01 ms; the last is 0.7 %, far inside the margin): one clause, not a
finding. Memory at the end of the runs: 70.1 MB with 40 arenas (O4) against 64.9 MB with 2 arenas (C4).
The calls the node serves itself (`update_maker_order`, orderbook, `my_orders`) agree per run within about 2 % in both arms.
The 99th percentile and the maximum are reported, not judged: the capped excess on `my_balance` and `withdraw` sits in C1
and C2 (maximum 605 and 1019 ms against 335 at most; 1157 and 756 against 533 at most), C3 equals the off level, and both
calls wait on Electrum servers. Wording: no latency cost was resolved on calls the node serves itself; on the two calls that
wait on Electrum servers, two of three capped runs had a heavier tail, which the run order cannot separate from a decline over
time seen in both arms.
End state of row c (observation): resident memory 63.4–65.5 MB with 2 arenas against 69.2–72.6 MB with 37–39 (5.7 MB, 8 %
lower with the limit), threads 28–32 and descriptors 32 in both arms.

## Still open after these runs

- Executor sizing (D45.3) and an allocator comparison stay deferred. The triggers for an allocator comparison are: a field
  report of memory growth with the limit in place, or a reject of 2, 4 and 8 in a repeat of these rows.
- Not measured: 256 cores or 80 updates per second at plateau length, a shielded swap, and a swap set larger than one per
  pair and arm.

## Deferred

Written down so the cost of the beta decision stays visible. The rows a–c (D45.4) and the plateau run (D45.5) that were
deferred past the beta were run on 2026-11-03; their results are above.

- **Executor sizing** (D45.3): size the three executors explicitly (a setting, or a cap such as the smaller of the core
  count and 16), instead of one worker per core each. It does not address the memory finding and adds a latency risk, so it
  waits for a reason of its own.
- **An allocator comparison** (jemalloc, mimalloc): only if a field report shows memory growth with the limit in place, or a
  repeat of the rows rejects `2`, `4` and `8`.
- If a larger limit performs better under swaps, change the default, not the mechanism.

## Not in scope

No change to the order book, the trie store, the outbox or the gossip layer; no memory limit on the process; no change on
other operating systems.
