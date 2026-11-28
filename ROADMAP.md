# Roadmap

This page gives the direction of KDF Reloaded in short: what the releases so far brought, and what comes
next. It commits to no dates, and items may be reordered, deferred or dropped. What each release contains in
detail is in [`CHANGELOG.md`](CHANGELOG.md), the tests run before a release are in
[`docs/release-tests/`](docs/release-tests/), and the plans for single pieces of work are in
[`docs/plans/`](docs/plans/).

## Released so far

Versions below 1.0.0 are pre-releases: APIs, on-disk formats and behavior may still change between them.

### 0.1 series

The GPLv2-only continuation of the Komodo DeFi Framework from its 2022 baseline, with atomic swaps on
netids 8762 and 6133.

- A stable Rust toolchain, the project's own CI, and signed releases.
- 0.1.0-beta.1 (2026-07-05), the first published pre-release; 0.1.0-beta.2 (2026-07-07) fixed HD-wallet
  use with the Komodo wallet apps.

### 0.2.0-beta.1 (2026-08-05)

- Shielded coins (ARRR): activation in the same request and answer form as the public API reference, a
  key derived from an HD seed, control of where the sync starts, and support code for browser builds.
- Live swap and order events for graphical clients.
- Corrections to the version-two swap wire format so that it matches other KDF nodes.

### 0.3.0-beta.1 (2026-11-01)

- Lighter peer-to-peer networking: seeds, relays and busy makers need less memory and cope with slow
  peers, and orders are posted faster.
- ARRR swaps on the Ironwood transaction format, with refunds and recovery that hold up.
- Better handling of Electrum and EVM nodes that fail or fall behind.
- Many fixes to swaps and refunds, among them for merged-mined coins and Radiant (RXD) signing.

### 0.4 series

- 0.4.0-beta.1 (2026-11-14): Siacoin (SC) swaps; under an HD login each coin uses its own key and
  address; ARRR pays `pirate1` addresses; Bitcoin II (BC2) support; Electrum servers that fall behind or
  disagree are detected; a more robust order book; two price sources that check each other; more release
  builds, among them Linux ARM and the mobile libraries.
- 0.4.1-beta.1 (2026-11-21): the order book no longer shows ARRR or BC2 orders from nodes that cannot
  complete such a swap.
- 0.4.2-beta.1 (2026-11-26): ARRR can spend the notes on the wallet's change address.

### 0.5.0-beta.1

- Safer swaps: the chain reads that a refund or the use of a swap's secret rests on must agree between two
  nodes, or come from a node you trust; a swap starts only when that is possible; a step waits instead of
  acting on an unclear answer; and swap and order records survive a crash.
- Version-two swaps that were saved by an earlier run resume with the same care. New version-two swaps
  cannot be started yet.
- TRON and Solana are back as wallet-only coins, with transaction history; TRON also has gasless TRC20
  withdrawals through GasFree. Neither can be traded yet.
- ZEC can be sent again, in the version-5 transaction format.
- Tendermint coins gain a transaction history, watch-only activation and WalletConnect signing.
- Most RPC methods that had only the older request form also accept `mmrpc: "2.0"` requests.
- Many fixes for QTUM and QRC20 tokens, WalletConnect, HD wallets, transaction history and browser builds.

Read the breaking changes in the [`CHANGELOG`](CHANGELOG.md) before you upgrade.

## Next

These are the next larger pieces of work. No version or date is assigned to them here.

- **Starting new version-two swaps.** The node can resume a saved version-two swap, but it cannot start a
  new one. The work is the whole path that starts one. Read more: CRD
  [ch.52](docs/reloaded-rewrite/52-swap-v2-state-machine.md).
- **Solana swaps.** SOL and SPL tokens can be held and sent, not traded. See
  [`solana-support`](docs/plans/solana-support.md).
- **SLP tokens.** Finishing the support for SLP tokens and testing it on a live network; parts of the SLP
  changes so far rest on unit tests only.
- **Swap watchers.** A watcher that can finish or refund a swap while your node is offline does not work
  yet, so the node has to keep running until its swaps have finished. The work starts with the watcher
  architecture.
- **An operator quickstart.** A short guide from download to a running node and a first swap.

## Stable (v1.0.0)

Goal: long-term maintenance baseline.

- Public security disclosure process battle-tested through at least one full cycle.
- Reproducible signed builds for all release platforms.
- Deprecation policy for RPC methods documented and applied.
- Documented LTS branch policy.

## Out of scope (currently)

- New atomic-swap protocol designs (e.g. submarine swaps, taproot-based variants) are tracked as research, not committed work.
- A first-party GUI is not in scope for this repository; existing third-party GUIs continue to consume the JSON-RPC surface.

## How to influence the roadmap

Open a discussion or an issue describing the use case. Concrete pull requests against documented gaps move things faster than open-ended feature requests.
