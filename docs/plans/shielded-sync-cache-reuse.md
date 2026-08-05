# Plan: shielded (ZHTLC) sync — reuse the compact-block cache on a start change

> **Status:** deferred / not started. This is an optional performance
> optimization, not a correctness fix. The current behavior is correct and
> secure; this plan only removes avoidable re-downloading.

## Background

When a shielded coin (ARRR/ZHTLC) is activated with a `sync_start` that differs
from the wallet's current sync anchor, activation rewinds and recreates both the
compact-block cache and the shielded wallet database, then rescans from the
requested start (see the R39.8.0h status note in
[`docs/reloaded-rewrite/39-zcash---z_coin-shielded-coin.md`](../reloaded-rewrite/39-zcash---z_coin-shielded-coin.md)
and `reset_wallet_scan_state` in
[`mm2src/coins/z_coin/z_coin_wallet_db.rs`](../../mm2src/coins/z_coin/z_coin_wallet_db.rs)).

Today the rewind does `DELETE FROM compactblocks` and then re-fetches the entire
`start..tip` range from lightwalletd, even when part (or all) of that range is
already cached.

## Why the wallet DB rebuild itself cannot be skipped (do not "optimize" this)

The shielded wallet DB stores, per note, an **incremental Sapling witness**
(a Merkle authentication path) plus the per-block commitment-tree frontier, both
built **sequentially from the anchor**. Reusing note/witness state across a
changed anchor risks producing an **invalid witness → an unspendable or malformed
shielded transaction**. Therefore:

- The `start..tip` **scan is unavoidable** and *is* the wallet-DB build — they
  are the same operation, not two steps.
- `zcash_client_sqlite`'s only truncation primitive (`rewind_to_height`) removes
  *later* scan state, never earlier, so it cannot re-anchor an existing DB.

So the wallet database must be rebuilt on a start change. That part is correct
and must stay.

## What *can* be optimized safely: the compact-block cache

Compact blocks are immutable, height-keyed, and hash-link-validated by the
scanner as it consumes them. So on a start change we can keep already-downloaded
blocks and fetch only the gap:

- **Moving earlier** (E < current anchor R): keep cached `R..tip`, fetch only
  `E..R-1`.
- **Moving later** (R > E): the needed `R..tip` is already a cached subset —
  fetch nothing.

The scan still runs `start..tip` (irreducible). Realistic benefit: removes the
network fetch time only (the "~1 minute" in field reports), **not** the
multi-minute scan.

### Reorg safety (the one real hazard)

Preserving cached blocks near the tip is only safe if they are still on the
active chain. A finalized range (deep history) is effectively immutable, but the
last few blocks can reorg. A correct implementation must always re-fetch a small
confirmation window near the tip and rely on the scanner's hash-continuity check
at the join point, falling back to a full re-fetch on any mismatch.

## Proposed implementation

1. Split "reset wallet DB scan state" from "reset compact-block cache" so the
   wallet DB can be recreated without nuking `compactblocks`.
2. Fetch only heights in `[start, tip]` not already present in `compactblocks`.
3. Always re-fetch the last **N** blocks (configurable reorg window, default
   `100` for ARRR — confirm value) and fall back to a full re-fetch if the
   scanner reports a continuity break.
4. Regression tests: earlier start, later start (subset → no fetch),
   partial-overlap, and reorg-boundary mismatch → full-refetch fallback.

## Deferred bigger lever (separate item)

The dominant cost is the scan (trial-decryption + witness updates in the
vendored `zcash_client_backend::scan_cached_blocks`), not the fetch. A much
larger speedup would come from pipelining fetch with scan and/or upgrading the
vendored `librustzcash` to a version with batched note decryption and
`shardtree`. That is a large, higher-risk change tracked separately, not here.
