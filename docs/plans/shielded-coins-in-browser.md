# Plan: shielded coins (ARRR) in the browser build

**Status:** planning (written 2026-11-15 from the WASM probe of November 2026; not scheduled) — **Last reviewed:** 2026-11-15

> **Context.** A browser (WASM) build of KDF Reloaded refuses to activate a shielded coin: CRD chapter 39
> R39.10.1, remaining deviation 1, and D39.6.a ("shielded coins in WASM — after the beta"). D39.6.a names three
> needs: a shielded wallet database in the browser, a Sapling prover that runs there, and a way to obtain the
> Sapling parameter files. A probe in November 2026 built and ran each piece in a real browser and found no
> blocker. This plan turns its findings into work packages. Nothing here is decided or scheduled; the decisions
> are proposals for the maintainer (section "Decisions").

## Goal

Lift the activation refusal of R39.10.1 deviation 1 / D39.6.a: a light-mode shielded coin (ARRR) activates in a
browser build, scans, reports its shielded balance, withdraws, and — last — takes part in swaps.

## Governing CRD

- Chapter 39 §39.6.1 (WASM support; its acceptance is not met today), §39.6.6 (reference behavior: Sapling
  parameter provisioning and shielded coins in the browser build; F39.6.6.1–F39.6.6.7, R39.6.6.1–R39.6.6.26,
  D39.6.6.a–D39.6.6.i, T39.6.6.a–T39.6.6.l), R39.8.0j (the logical data model of the shielded wallet database),
  R39.10.1 (the refusal this plan removes).
- Chapter 26 (cross-platform and WASM substrate) for the storage and runtime split.

## What the probe found

All timings come from one browser engine (Firefox 150, headless) on a shared host under heavy, permanent
background load. They are indicative, not benchmarks; absolute times on an idle machine were not measured. The
load-robust figure is the browser-to-native ratio on the same host, cores and load.

- **Wallet database.** The stock SQLite build cannot compile for `wasm32-unknown-unknown` (no C library for that
  target; no feature switch helps). With a pure-WASM SQLite (`sqlite-wasm-rs`) behind a small stand-in
  `libsqlite3-sys` crate, our patched `zcash_client_sqlite 0.22` and `rusqlite 0.37` build unchanged, create and
  migrate all 45 tables (0.5 s in memory, 2.2 s on OPFS), and the OPFS wallet survives a page reload and a
  browser restart. OPFS (the Origin Private File System) needs a dedicated Worker and a secure context. An own
  IndexedDB wallet store of the R39.8.0j model is therefore not needed.
- **Prover.** The Sapling and Ironwood provers build for wasm32 unchanged and run from parameter bytes (no
  file-system crate). Single thread: Sapling spend 65.8 s, output 10.2 s, Ironwood action 29.6 s, Ironwood key
  build 37 s per session. The browser costs about 4× the native single-thread time for every proof type.
- **Threads.** With a multi-threaded build (nightly, shared memory) in a cross-origin isolated page, 8 Web
  Workers bring a Sapling spend to 11.2 s and an Ironwood action to 6.0 s; 32 bring a spend to 7.0 s. The ~4×
  gap to native holds at equal thread count; Ironwood saturates at 8 threads.
- **Scan.** Trial decryption runs at about 1,000 blocks/s in the browser (about 4.4× native single-thread).
- **Transport.** The public gRPC-web endpoint of the coins file answers a page (`GetLightdInfo`,
  `GetBlockRange`) with CORS accepted. Incremental reading of a long stream was not demonstrated.
- **Parameter files.** Both files (51,551,256 bytes) are hash-checked in 0.32 s and parsed in 1.5 s in the
  browser. The digests are compiled into the library, so integrity never depends on the source.

## Work packages

Sizes are rough estimates for planning ("small" ≈ days, "medium" ≈ 1–2 weeks), not measurements.

1. **W1 — Wallet database on the SQLite route (medium).** A stand-in `libsqlite3-sys` crate over a pure-WASM
   SQLite, wired by one `[patch.crates-io]` line; OPFS as the persistent storage layer; a wallet clock read
   from JavaScript (the library's system clock panics on wasm32); `uuid` with its `js` feature. The SQLite-WASM
   crates force a workspace-wide `wasm-bindgen` bump (0.2.117 → 0.2.130, with `js-sys`/`web-sys` and the CI
   `wasm-bindgen` CLI); that bump is taken on purpose, as its own reviewed step. The stock and the WASM SQLite
   must never be linked together; the patch line guarantees it. Open: a real scan workload on OPFS, and how the
   synchronous wallet calls meet the daemon's single-threaded executor (in the hosting Worker of W4, or behind a
   message channel).
2. **W2 — Parameter source for both targets (small–medium).** One provider-based parameter source shared by
   native and browser (the probe's option P1): providers in a fixed order — local directory (native only),
   bytes or a same-origin URL supplied by the embedding app (browser), download from a configured host list,
   verified and stored (browser by default; native only if opted in), and the browser store of those. Every
   provider ends in the same BLAKE2b-512 check before a prover is built; a failure is a typed, reported error.
   The prover is built from bytes on both targets, so native becomes "read file → bytes → verify → parse".
   **Native default unchanged:** directory only, and `ZCashParamsNotFound` / `ZCashParamsDirNotFound` stay as
   today. Decisions D39.6.6.a–f apply.
3. **W3 — gRPC-web transport with bounded ranges (small–medium).** A server-streaming read for `GetBlockRange`
   on top of the existing unary gRPC-web client, requesting sequential ranges, each stored on arrival, with a
   stated memory bound and progress per range (D39.6.6.h), and a distinct report for an endpoint that does not
   serve gRPC-web (D39.6.6.i).
4. **W4 — Worker hosting of wallet and prover (cross-cutting).** A dedicated Worker hosts the wallet database
   (OPFS needs one) and the prover, so a proof never holds the node's thread (a single-thread Sapling spend
   holds it for about a minute). Where the page is cross-origin isolated (COOP `same-origin`, COEP
   `require-corp`), a second, multi-threaded bundle is used, with feature detection falling back to the
   single-thread bundle. In an isolated page every cross-origin resource — a parameter host, the lightwalletd
   and Electrum endpoints — must send CORS and CORP headers (CORP was not tested). Decision D39.6.6.g applies.
5. **W5 — Swap code last (medium).** Our shielded HTLC and swap operations are native-gated today. The cleared
   §39.6.6 records no swap refusal and no protocol difference at the reference in its browser build, only where
   proofs run. A payment with a few notes takes minutes single-thread; swap lock times are hours, so it fits,
   but only with proving off the node's thread (W4). This package depends on the decisions of the swap-process
   architecture work and comes after W1–W4.

The packages share one new requirement: the Worker of W4. W1, W2 and W5 depend on it.

## Decisions (proposals — to be decided by the maintainer)

Each row is a proposal from the probe's findings, not a decision. The options are those of chapter 39
D39.6.6.a–i.

| Decision | Proposal | Numbers / reason | Status |
| --- | --- | --- | --- |
| D39.6.6.a — source of the parameter bytes | (3) app-supplied (bytes or same-origin URL), with (2) a configured host list as fallback; digests compiled in; each source tried logged | Any HTTPS host must answer CORS (and CORP in an isolated page); 51.6 MB at first use; hashing 0.32 s | to be decided by the maintainer |
| D39.6.6.b — store scope | (2) one store per browser origin, shared by all wallets, keyed by digest | Public constants; per-wallet storage would download 51.6 MB per wallet | to be decided by the maintainer |
| D39.6.6.c — stored bytes failing the check | (2) hash on every load; on mismatch delete, obtain once more, else report an activation error | The re-check costs 0.32 s against ~1.5 s parse and ≥ 10 s per proof | to be decided by the maintainer |
| D39.6.6.d — refused storage or quota | (2) check before downloading that the store accepts ~52 MB, else fail at once naming storage | The wallet database needs persistent storage too; one up-front request and quota check serves both | to be decided by the maintainer |
| D39.6.6.e — when obtained, what `status` shows | (1) during activation as `ActivatingCoin`; (3) background loading later only if a wire addition is accepted | With the app-supplied source the wait is short; not (4), a swap step would stall on a 51.6 MB fetch | to be decided by the maintainer (a product choice) |
| D39.6.6.f — native: directory or download too | (1) directory only by default; an opt-in, verified download into the directory, visible in `status`, only if ever wanted | Keeps `ZCashParamsNotFound` where GUIs act on it; the provider of W2 makes the opt-in a setting, not new code | to be decided by the maintainer |
| D39.6.6.g — proving thread or worker | (2) a worker, threaded where the page is cross-origin isolated | Single thread: a Sapling spend holds the thread 65.8 s; with 8 workers 11.2 s; the ~4× gap to native holds | to be decided by the maintainer |
| D39.6.6.h — bounded block download | (2) sequential ranges, each stored on arrival, stated memory bound, progress per range | 10,000 blocks ≈ 1.84 MB compact; scan ~1,000 blocks/s, so ranges of a few thousand blocks keep memory small and progress visible | to be decided by the maintainer |
| D39.6.6.i — browser endpoints | (1) the caller passes gRPC-web endpoints in `light_wallet_d_servers`, with a distinct "endpoint does not serve gRPC-web" report; reading `light_wallet_d_servers_wss` from the coins entry only if callers ask | The public gRPC-web endpoint answered from a page with CORS; what the admission rule of R39.11.7a does with a two-entry list needs a look | to be decided by the maintainer |

## Running separately

Two small fixes found by the probe are handled outside this plan, as their own changes:

- the browser-store layer does not withdraw its event handlers when a pending request is discarded (chapter 39
  F39.6.6.1, confirmed in our code); W1 does not depend on it, but the browser build as a whole does;
- our vendored `zcash_primitives` breaks the wallet library's `orchard` feature on every target (a compile
  error, not a resolution conflict). Today's code keeps that feature off; any use of the library's
  Orchard/Ironwood scanning needs it reconciled first.

## Acceptance

- R39.6.1's acceptance holds: a light-mode shielded coin activates in a browser build and reports a shielded
  balance; R39.10.1 deviation 1 is closed with a dated status note.
- The tests of chapter 39 T39.6.6.a–l that apply to the chosen options pass; native activation, errors and
  timings are unchanged by W2.
- A withdraw proves in the Worker without blocking the node's RPC and P2P work.

## Not in this plan

- Lightning and the other browser limits by nature, and the remaining browser-build gaps: plan
  [`browser-build-gaps`](browser-build-gaps.md).
- Native-mode (coin daemon) shielded coins.
