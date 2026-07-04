# Upstream Parity Gaps — TODO Tracker

**Status:** active backlog.

This document tracks confirmed functional gaps between the reloaded tree and
the upstream Komodo DeFi Framework wire surface, as established by the changelog
parity audit (Jan 2022 → present) cross-referenced against
[`rpc-method-census.md`](./rpc-method-census.md) (the upstream wire-method
parity reference) and the reloaded dispatcher routing.

Each gap below has been confirmed at **both** the CRD-specification level and the
reloaded-code level (the audit method and evidence are recorded inline). Items
are numbered to match the parity report delivered to the maintainer.

Branch hierarchy for remediation: each item gets its own feature branch cut from
`dev`; branches are left local (not pushed/merged) pending review.

---

## #2 — Event streaming: missing `stream::` streamer implementation

**Resolved by C8.**

CRD reference: [`10-sse-streaming.md`](./10-sse-streaming.md). R6/R20/R24 and
§10.19 now bind `stream::shutdown_signal::enable` as the ninth streamer. The
remaining streaming parity gap in this tracker was the reloaded code
implementation for that method; C8 adds it on native non-Windows targets.

Reloaded implementation evidence:
- The public streamer-origin set includes `ShutdownSignal`.
- The `stream::` dispatcher routes `stream::shutdown_signal::enable` on native
  non-Windows targets and leaves it unavailable through the normal
  missing-method path elsewhere.
- A native non-Windows shutdown-notification producer publishes supported
  process signal names to the streaming manager before runtime stop begins.

Upstream `stream::` surface (census) vs reloaded:

| upstream method | reloaded | gap |
| --- | --- | --- |
| `stream::balance::enable` | yes | — |
| `stream::heartbeat::enable` | yes | — |
| `stream::order_status::enable` | yes | — |
| `stream::orderbook::enable` | yes | — |
| `stream::swap_status::enable` | yes | — |
| `stream::network::enable` | yes | implemented (D2 completion) |
| `stream::fee_estimator::enable` | yes | implemented (#3 completion) |
| `stream::tx_history::enable` | yes | C7 — `TX_HISTORY:<ticker>` reactive streamer |
| `stream::shutdown_signal::enable` | yes | C8 — native non-Windows `SHUTDOWN_SIGNAL` reactive streamer |
| `stream::disable` | yes | C6 — generic per-client unsubscribe |

- [x] **`stream::network::enable`** — implemented (D2 completion). The `Network`
  streamer, its activation request/payload, and the `stream::network::enable`
  route are bound by [`10-sse-streaming.md`](./10-sse-streaming.md) §10.16
  (R28–R31) and merged on `dev`.
- [x] `stream::tx_history::enable` — implemented by C7. The `TxHistory`
  streamer, activation request/response, supported-family contract, and producer
  obligations are bound by [`10-sse-streaming.md`](./10-sse-streaming.md) §10.18
  (R39-R45).
- [x] `stream::shutdown_signal::enable` — implemented by C8. The native
  non-Windows `ShutdownSignal` streamer, activation route, and
  shutdown-notification producer are bound by
  [`10-sse-streaming.md`](./10-sse-streaming.md) §10.19 (R46-R51).
- [x] `stream::disable` — implemented by C6 as the generic per-client
  unsubscribe RPC bound by [`10-sse-streaming.md`](./10-sse-streaming.md) R21.

## #3 — Fee estimator stream (`stream::fee_estimator::enable`) — **RESOLVED**

**Confirmed real.** The EIP-1559 fee estimator is exposed upstream as a streamer
(`stream::fee_estimator::enable`), not a legacy method. The on-demand
single-shot estimator `get_eth_estimated_fee_per_gas` **is** present in reloaded
(arms=1), and the gas-policy pair `get_swap_gas_fee_policy` /
`set_swap_gas_fee_policy` are present (arms=2 each). Only the **continuous
streaming** variant is missing.

CRD reference: [`08-fee-routing-engine.md`](./08-fee-routing-engine.md) covers
the swap fee-routing engine but does not bind a fee-estimator streamer;
[`10-sse-streaming.md`](./10-sse-streaming.md) now binds it as the seventh
concrete streamer (§10.17, R32–R37).

- [x] **`stream::fee_estimator::enable`** — implemented: a `FeeEstimation`
  `StreamerId` variant + activation module that periodically broadcasts EIP-1559
  fee estimates for a given EVM coin. Bound by
  [`10-sse-streaming.md`](./10-sse-streaming.md) §10.17 (R32–R37) and merged on
  `dev`.

## #1 — NFT activation (`enable_nft`) — **RESOLVED**

**Resolved.** Upstream activates the NFT subsystem via `enable_nft`; reloaded now
exposes that method directly.

CRD reference: [`19-nft-module-layout.md`](./19-nft-module-layout.md) binds the
NFT subsystem activation entry point in §19.6.2 and records the transition from
the earlier `update_nft` initialization role to the explicit `enable_nft`
activation contract.

Code evidence:
- `mm2src/mm2_main/src/rpc/dispatcher/dispatcher.rs` routes `enable_nft`.
- `mm2src/coins/nft/activation.rs` implements the mmrpc-2.0 `enable_nft`
  handler, request, response, and activation error surface.

- [x] **`enable_nft`** — implemented and bound by
  [`19-nft-module-layout.md`](./19-nft-module-layout.md) §19.6.2.

## #4 — Experimental liquidity routing (DOCUMENTED ONLY — NOT to be implemented)

**Confirmed real. Out of scope by maintainer decision.** Upstream exposes an
experimental multi-quote liquidity-routing namespace; reloaded has 0 routes for
it.

Upstream methods (census, all `experimental::liquidity_routing::`):
- `experimental::liquidity_routing::execute_routed_trade`
- `experimental::liquidity_routing::find_best_quote`
- `experimental::liquidity_routing::get_quotes_for_tokens`

No CRD chapter binds this namespace. Per maintainer instruction, this experimental
feature is **deliberately not implemented**; it is recorded here for completeness
only.

- [x] Documented as a known, intentional omission. No implementation planned.

## #5 — Streaming activation response carries an extra `active` field — **RESOLVED**

**Resolved. Reloaded wire contract now matches upstream.**

Upstream's `stream::*::enable` success response is the mmrpc-2.0 envelope whose
`result` is an object with a single string field `streamer_id` (the wire-stable
`StreamerId` display token, e.g. `HEARTBEAT`, `BALANCE:<ticker>`) — with **no
boolean field**. CRD reference: [`10-sse-streaming.md`](./10-sse-streaming.md)
R21 (corrected this round to bind the `streamer_id` response and drop the
previously-specified boolean `active`).

Code evidence:
- `mm2src/mm2_main/src/rpc/streaming_activations/mod.rs` — `EnableStreamingResponse`
  now serialises **only** `streamer_id: String`; the reloaded-only `active: bool`
  field was removed and the constructor takes just the `streamer_id`.
- No reloaded code reads `active`; the Komodo DeFi SDK's `BalanceManager` reads
  `streamer_id` (a captured run log shows `Key "streamer_id" not found in Map`
  failures from an older build that returned only `active`, confirming the SDK
  requires `streamer_id` and does not depend on `active`).

Three-way alignment: corpus `{streamer_id}` = reloaded code `{streamer_id}` = CRD
R21. The superset has been removed; all three shapes now agree.

- [x] **Drop `active`** from `EnableStreamingResponse` (and its constructor) so
  the success payload is exactly `{ "streamer_id": "<token>" }`, matching upstream
  and the corrected R21. Done on `dev` (commit `fd075e66e`); the shared struct and
  all activation handlers compile and the streaming_activations tests pass.

---

## Audit method (for reproducibility)

1. Extracted the 223 unique upstream wire-method strings from
   `rpc-method-census.md`.
2. Extracted reloaded's routed method strings from
   `mm2src/mm2_main/src/rpc/dispatcher/dispatcher.rs` and
   `dispatcher_legacy.rs`.
3. Normalised namespace prefixes on both sides and diffed.
4. Verified every candidate gap by direct source grep across `mm2src/`
   (eliminating routing-style false positives such as generic `::cancel` arms
   and task-lifecycle leaves).
