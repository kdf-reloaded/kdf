# Plan: Lightning activation features

**Status:** parked (deferred by maintainer decision 2026-10-11; nothing started) — **Last reviewed:** 2026-10-25

> **Context:** created 2026-10-11 on
> `fix/activation-api-parity`. That branch brings the Lightning task activation
> to wire parity: error names and `error_data` shapes, HTTP statuses, `init`
> error precedence including the `avg_blocktime` check, status-level failure
> mapping, and cancel cleanup (CRD ch. 41 §41.2, items 1-4 of its Reloaded
> status list). The features below were deferred by maintainer decision on
> 2026-10-11 and belong to a separate branch. Lightning's broader market, swap
> and history gaps are tracked separately in
> `docs/reloaded-rewrite/UPSTREAM-PARITY-GAPS.md`.

## Features to implement

References are to CRD ch. 41 (Dirty-Gate-passed).

1. **Coins-configuration forms** (R41.2.11, Reloaded status items 7-8). The
   reference reads `confirmation_targets` (integers) and
   `our_channels_configs`; Reloaded reads a `confirmations` member and
   `our_channels_config`. A coins file written for the reference fails
   Reloaded's protocol parsing today. Accept both forms rather than replacing
   Reloaded's, for the same backward-compatibility reason as the Solana plan.
   Do this first; it decides whether shared coins files work at all.
2. **Fee estimates at activation**: fetch fee estimates from the platform
   backend (the `GettingFeesFromRPC` phase) instead of only per-priority
   defaults.
3. **Progress reporting**: emit the R41.2.7 in-progress sequence
   (`ActivatingCoin`, `GettingFeesFromRPC`, `ReadingNetworkGraphFromFile`, ...)
   as the node starts. Reloaded reports a single value and never advances it.
4. **`user_action` payload**: accept the shared hardware user-action payload
   (R41.2.9). There is no awaiting state today, so this only matters together
   with hardware-wallet Lightning activation (D41.1).

## Defect: a discarded node keeps running (added 2026-10-29)

This is a real defect, kept for after the beta (CRD ch. 41 D41.3). When the
node starts, it binds its listening port and spawns loops that never stop. A
Lightning activation that fails after that point leaves them running, and so
does a disabled Lightning coin. A retry on the same port then fails, and the
loops of the old and new node can write the same persisted files. The fix:
give `LightningCoin` and each of its loops a stop handle, and fire it when an
unregistered node is dropped and when the coin is disabled. Then test a retry
on the same port after a failed activation and after a disable. This fix does
not depend on items 1-4.

## Order

1, then 2 and 3 together (they touch the same start-up sequence), then 4 with
D41.1 if hardware Lightning is ever in scope.

## Trading floors (added 2026-10-25)

Reloaded's two Lightning floor functions crash the request today (CRD ch. 60
finding F5). Plan `trading-floors-parity` fixes that before beta, in widened
PR #66: both report the reference value (ch. 41 §41.11A, ch. 60 row L1), and
the order builders refuse Lightning pairs until Lightning trading exists
(R60.6.5). When this plan's trading work starts, lift that refusal and keep the
values.
