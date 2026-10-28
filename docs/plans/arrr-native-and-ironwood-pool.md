# Plan: ARRR native mode and the Ironwood pool (v0.4.0)

**Status:** planning — **Last reviewed:** 2026-10-28

> **Context:** decided by the maintainer on 2026-10-28 (Decision X). Release 0.3.0-beta stays
> Sapling-only. It builds version-6 transactions that carry a Sapling bundle and an empty Ironwood
> bundle (CRD ch.39 R39.6.4), and it refuses native-mode activation of a shielded coin
> (ch.39 R39.11.1a). This plan collects what has to exist before native mode and the Ironwood pool
> can ship, targeted at v0.4.0. Nothing here is settled design yet. Each item needs its own analysis
> pass, and the CRD rules come first.

## Scope

### 1. Native mode (a shielded coin served by its own full node)

What exists: the activation route, a native RPC client, and the older commitment-tree state cache.

What is missing:

- **Compact-block cache from full-node blocks.** Native activation does not convert the node's
  blocks into the compact-block cache the wallet database scans. A fresh native activation
  therefore fails the complete-through-tip scan gate (ch.39 R39.8.0f, its status update).
- **One height authority for native mode**, matching what R39.11.9 to R39.11.11 bind for light
  mode: tip, confirmations, expiry and the builder's target height. For confirmations and expiry,
  use the fresh tip, as R39.11.10a requires in light mode.
- **Ironwood activation discovery from the node** (design drafted on 2026-10-26, never implemented):
  1. A configured `ironwood_activation_height` is used as is.
  2. Otherwise, before a transaction is built, read the node's `getblockchaininfo`. An Ironwood
     entry among its network upgrades that carries an activation height is recorded as the
     discovered height.
  3. Otherwise, the node's consensus branch for the next block decides as the light-mode server
     branch does (ch.39 R39.6.4(c)). Ironwood's branch means active from the tip; another known
     branch means not yet active.
  4. Anything unreadable or not understood refuses the build with
     `BuilderHeightError::IronwoodActivation`. Native mode never guesses, and never builds
     version 4 once the node reports Ironwood's branch.
- **Lifting the refusal of R39.11.1a**, with a live activation, a send, and a swap on mainnet as
  evidence.

### 2. The Ironwood pool

Today the project builds no Ironwood bundle and does not scan for Ironwood-pool notes. To be
specified from public Pirate Chain documentation and the dictated transaction format:

- **Scanning:** detecting and decrypting notes this wallet receives in the Ironwood pool, and
  keeping their witnesses. Balance and history then include them.
- **Spending:** building Ironwood spends and outputs, including change, fees, and moving funds
  between the Sapling and Ironwood pools.
- **Key derivation:** the Ironwood spending and viewing keys from the wallet seed, for both
  iguana and HD wallets.
- **Swaps:** whether HTLC payments, fees and refunds stay transparent P2SH funded from Sapling
  (as today), or may be funded from the Ironwood pool.

### 3. Pirate1 addresses

The address form that goes with the new pool: parsing, validation, display, and use as a
withdraw destination and as the wallet's own receiving address. To be specified from public
Pirate Chain documentation.

## Open questions

- Which reference behavior, if any, applies? The reference builds no version-6 transactions.
  Native-mode parity questions go to the Spec Reader before any CRD rule is written.
- Do the light-mode backend (lightwalletd) and its compact format already carry Ironwood-pool
  outputs, or does scanning that pool need a backend change?
- Can native mode and light mode share one wallet database for the same seed, or must they stay
  separate?

## Not in this plan

- Detecting a second instance on the same seed, and showing that instance's change while it is
  unmined. That stays with the two-daemon item of
  [`shielded-in-flight-note-locking`](shielded-in-flight-note-locking.md).
