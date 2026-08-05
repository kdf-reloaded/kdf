# Changelog

All notable changes to KDF Reloaded are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Pre-1.0 releases use the `0.MAJOR.MINOR-PRERELEASE.N` convention; expect breaking changes between alpha and beta.

## [0.2.0-beta.1] - 2026-08-05

### Added

- **Known beta limitation: shielded Zcash/ARRR is not supported in the web-wallet WASM bundle yet.** The WASM CI safety net now uses a direct `cargo check --target wasm32-unknown-unknown -p mm2_main` check because the packaging step hit a duplicate `secp256k1-sys` link collision from the Zcash/ARRR stack. Native and desktop builds remain the supported targets for shielded Zcoin activation and sync until the web bundle is normalized.

- **Z-coin (ARRR/ZHTLC) WASM support — R39.6.1.** The shielded coin module is now available on the `wasm32-unknown-unknown` target. `zcash_primitives` and `zcash_client_backend` are added as WASM dependencies. The sapling state cache is abstracted behind a `SaplingStateCacheOps` trait with a SQLite backend for native and an IndexedDB backend (mm2_db) for WASM. `MmCoinEnum::ZCoin` and the `z_coin` module are now gated for all targets. Transaction building (`gen_tx`/`send_outputs`) remains native-only pending WASM delivery of the sapling parameter files. Code: `mm2src/coins/z_coin/`, `mm2src/coins/Cargo.toml`, `mm2src/coins/lp_coins.rs`, `mm2src/coins/lp_coins_context.rs`.
- **Z-coin sync-parameter control — R39.6.2.** The `task::enable_z_coin::init` activation request now honours the externally-dictated sync controls (confirmed against the public komodoplatform API reference and KDF-family wallets): a `sync_params` field **inside the Light-mode `rpc_data`** (`activation_params.mode.rpc_data.sync_params`), shaped as the externally-tagged union `{"height": <uint>}` / `{"date": <unix-seconds>}` / `"earliest"`; plus top-level `scan_blocks_per_iteration` (u32) and `scan_interval_ms` (u64, `scan_interval` accepted as an alias). Height starts pass through, date starts resolve to a block height via a binary search over backend block timestamps, and `earliest` maps to Sapling activation. (Supersedes the earlier, incorrect top-level `sync_start` / `{"type","data"}` shape.) Code: `mm2src/coins_activation/src/z_coin_activation.rs`, `mm2src/coins/z_coin.rs`.
- **Z-coin HD-derived shielded key policy — R39.6.4 §2.** Under an HD (BIP39) wallet, the shielded Sapling spending key is now derived from the wallet seed along the coin's `z_derivation_path` with the activation `account` appended as a hardened child (`m/<z_derivation_path>/account'`); the legacy Iguana policy keeps deriving the ZIP32 master from the iguana secret. A new optional `account` field (u32, default 0) on `task::enable_z_coin::init` selects the account (ignored under the Iguana policy). Only the shielded key is HD-derived; the transparent UTXO side remains iguana-derived. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins/z_coin/z_coin_ops.rs`, `mm2src/coins_activation/src/z_coin_activation.rs`.
- **Z-coin activation reports the resolved shielded sync start — R39.8.0h.** When a `sync_params` is supplied, the activation result now includes a `first_sync_block` object `{ requested, is_pre_sapling, actual }`, where `requested` is the resolved start height (from a height or a date), `is_pre_sapling` flags a request below Sapling activation, and `actual` floors the start at Sapling activation. Code: `mm2src/coins_activation/src/z_coin_activation.rs`.
- **Z-coin (ARRR/ZHTLC) activation-wire conformance — R39.3 / R39.6.2 / R39.8.0h.**
  `task::enable_z_coin` now matches the dictated public activation wire on the
  remaining points: the status task reports the two dictated scan phases with
  live progress (`UpdatingBlocksCache` during the compact-block download and
  `BuildingWalletDb` during the wallet-DB scan, each with `current_scanned_block`
  / `latest_block`); the activation result includes the top-level `ticker` and
  emits `first_sync_block` unconditionally; the request honours
  `zcash_params_path` (native Sapling parameter directory) and `skip_sync_params`
  (resume from local state, consulting `sync_params` only when no prior state
  exists); and blocks-per-iteration now defaults to the dictated `1000`. The
  modern Light wallet-DB scanner now applies the effective
  `scan_blocks_per_iteration` and `scan_interval_ms` values instead of using a
  fixed 1,000-block batch with no pause. Activation logs the effective policy at
  INFO; network and wallet batch timing/frontier details are bounded DEBUG/TRACE
  diagnostics, while the normal Light-mode bypass of the legacy native cache is
  no longer reported as a warning. Code:
  `mm2src/coins_activation/src/z_coin_activation.rs`, `mm2src/coins/z_coin.rs`,
  `mm2src/coins/z_coin/z_coin_ops.rs`, `mm2src/coins/z_coin/z_coin_wallet_db.rs`.
- **Tendermint `denom` / `decimals` / `ibc_channels` in `CoinProtocol`.** `CoinProtocol::TENDERMINT` now carries optional `denom`, `decimals`, and `ibc_channels` fields (R36.3.3), aligned with the komodo-coins config schema for ATOM-family coins. Code: `mm2src/coins/tendermint/`.
- **V1 swap and taker order-status SSE.** The SSE streaming infrastructure now emits live maker/taker swap and order-status events under the existing `stream::*` namespace. Code: `mm2src/mm2_main/`.
- **SSE streamer parity endpoints.** The CRD and implementation now cover `stream::disable`, `stream::fee_estimator::enable`, `stream::tx_history::enable`, and native non-Windows `stream::shutdown_signal::enable`, completing the currently bound `stream::*` parity surface. Code: `docs/reloaded-rewrite/10-sse-streaming.md`, `mm2src/mm2_main/`.
- **RPC-dump development build workflow.** A manual `dev-build-rpc.yml` workflow builds with the RPC dump feature set, and the cross-platform build workflows pass feature flags consistently on Linux, macOS, Windows, iOS, and Android. Code: `.github/workflows/`.

### Fixed

- **ARRR Light activation checkpoint, recovery, and concurrency.** Lightwalletd
  `TreeState` display-order block IDs are now converted to the canonical
  little-endian compact-block representation before first-block continuity
  validation, fixing valid scans that failed immediately with a hash
  discontinuity and consequently reported a zero balance. A recoverable hot
  SQLite journal is classified through a temporary recovery copy without
  mutating the source, rather than being mislabeled as schema corruption after
  logout or interruption. Same-ticker ZCoin activation tasks are serialized
  across the full activation lifecycle to prevent concurrent rebuild/rename
  races on Windows. Compact-block network batches are validated and committed
  atomically in one SQLite transaction, reducing long historical-sync write
  overhead and preventing partial batches. After a restart, a complete cached
  segment is chain-validated and resumed at its next height, avoiding another
  download of already persisted history; a discontinuous segment is preserved
  and rebuilt instead of being trusted by its maximum height. Code:
  `mm2src/coins/z_coin/`, `mm2src/coins_activation/`.

- **ARRR Light scans with Pirate compact-block metadata.** Pirate's dictated
  compact-block protobuf predates the modern optional `ChainMetadata` field.
  Reloaded now supplies the missing first-block Sapling tree size in memory
  from the trusted checkpoint frontier and the block's explicit outputs, so
  the stable `zcash_client_backend` scanner no longer aborts with an unknown
  tree-size error. Reloaded also carries the exact compact-source frontier
  between bounded wallet-scan batches instead of reconstructing it from the
  prunable wallet tree, fixing later failures near completion with `Unable to
  compute root; missing values for nodes`. After a restart it refreshes the
  frontier at the persisted scan height and validates its block hash and tree
  size before continuing. Scan failures are also logged explicitly instead of
  appearing only in the activation task response. Code:
  `mm2src/coins/z_coin/`.

- **Z-coin (ARRR/ZHTLC) shielded sync honours the requested sync start.**
  The activation request's shielded sync point (`mode.rpc_data.sync_params`)
  was being dropped on the wire — the daemon expected a top-level `sync_start`
  with a different shape, so the parameter deserialized to nothing and the
  fetch plan always saw `requested_start_height=None`. On top of that, once a
  shielded wallet had scanned to the chain tip, activation short-circuited on
  the already-scanned state and reused the stale local cache, so the wallet
  displayed a balance almost instantly regardless of the requested start. The
  request now parses the dictated wire (`sync_params` as
  `{"height"|"date":N}`/`"earliest"` inside the Light `rpc_data`), and
  activation compares the requested start against the wallet's sync anchor and,
  when they differ (earlier or later), rewinds/recreates the compact-block
  cache and wallet database and rescans from the requested start, re-seeding
  the commitment tree from the light backend — matching upstream behaviour. An
  unchanged start still resumes from the tip without a rescan, and a start
  beyond the tip is clamped. Fixes #2. The remaining ZHTLC activation-wire
  divergences (two-phase scan progress, result `ticker`, `zcash_params_path`,
  `skip_sync_params`, unconditional `first_sync_block`, and the `1000`
  blocks-per-iteration default) are addressed by the activation-wire conformance
  entry above. Code:
  `mm2src/coins_activation/src/z_coin_activation.rs`,
  `mm2src/coins/z_coin/z_coin_wallet_db.rs`.

- **Small KMD direct-burn DEX fees retain the legacy wire shape.** Netid 8762
  KMD taker-fee construction now permits the positive 75% fee-collection
  output selected by the `v2.6.0-beta` policy even when that split component
  is below KMD's generic spendable-output dust threshold. The exception is
  scoped to that protocol-defined output; ordinary outputs and change retain
  the existing dust checks. Code: `mm2src/coins/utxo/`.
- **HD UTXO activation honours `min_addresses_number`.** API-v2 UTXO
  activation now persists and returns the requested minimum number of external
  addresses for each HD account, so an empty account activated with the
  wallet-standard value `1` has external address `0` available to balance,
  order, and swap-preimage paths. Empty HD accounts also retain the activated
  ticker's zero-valued entry in `total_balance` instead of returning an untyped
  empty object. Code: `mm2src/coins/`, `mm2src/coins_activation/`.
- **DEX-fee wire compatibility on both production netids.** Netid 8762 KMD
  takers now use the `v2.6.0-beta`-compatible discounted fee and two-output
  75/25 fee/OP_RETURN structure, while non-KMD takers remain single-output.
  Netid 6133 follows the v3/dev single-output fee structure. Both networks now
  use only the taker coin's minimum transaction amount as the fee floor,
  removing the erroneous additional `0.0001` floor. Fixes #1. Code:
  `mm2src/mm2_net_config/`, `mm2src/mm2_main/src/lp_swap/`,
  `mm2src/coins/utxo/`.
- **`CoinProtocol::NFT` variant accepted.** A permissive NFT variant is added to `CoinProtocol` so NFT-typed coin configs no longer fail deserialization. Code: `mm2src/coins/lp_coins.rs`.
- **NFT subsystem activation RPC parity.** `enable_nft` is routed as the explicit NFT subsystem activation method, with CRD coverage for the activation contract. Code: `docs/reloaded-rewrite/19-nft-module-layout.md`, `mm2src/mm2_main/`, `mm2src/coins/nft/`.
- **SSE activation response wire shape.** `stream::*::enable` success payloads now expose the upstream-compatible `streamer_id` field without the reloaded-only `active` boolean. Code: `docs/reloaded-rewrite/10-sse-streaming.md`, `mm2src/mm2_main/`.
- **ZHTLC `protocol_data` deserialization.** Bare `{"type":"ZHTLC","protocol_data":{...}}` configs now deserialize correctly; previously missing `protocol_data` support caused ARRR activation to fail. Code: `mm2src/coins/z_coin_activation.rs`.
- **Zcash consensus parameters sourced from `protocol_data` — R39.6.4.** The shielded builder now reads all network parameters (`consensus_params`, `check_point_block`, `z_derivation_path`) from `protocol.protocol_data` rather than hardcoded Zcash-mainnet constants. A ZHTLC coin with non-mainnet HRP/b58 prefixes or activation heights uses its declared parameters end-to-end. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins_activation/src/z_coin_activation.rs`.
- **`{"type":"ETH"}` protocol config accepted again.** A regression caused standard ETH coins using the `{"type":"ETH"}` protocol object to fail activation; restored. Code: `mm2src/coins/lp_coins.rs`.
- **UTXO dynamic-fee trade preimage is consistent** for `UpperBound` vs `Exact` fee policies. Code: `mm2src/coins/utxo/`.
- **ETH `estimate_gas` insufficient-balance revert mapped to `NotSufficientBalance`** instead of a generic transport error. Code: `mm2src/coins/eth/`.
- **getrandom 0.3 `wasm_js` backend enabled on wasm32** so entropy sources compile correctly on the WASM target. Code: `Cargo.toml`.
- **sia-rust bumped to `e1c0725`** (nom 7.1.3 and WASM bindgen refresh).
- **ARRR/ZCoin activation and light-mode scanning.** ZHTLC activation no longer panics through the task manager when a failing Electrum candidate is encountered; activation fails over across available Electrum servers. Light-mode activation creates the required Sapling cache, bounds shielded-history scanning, handles stale checkpoints, uses the Pirate-compatible lightwalletd gRPC package, supports TLS lightwalletd endpoints, and reports shielded wallet DB balances for ARRR instead of returning zero. Code: `mm2src/coins/`, `mm2src/coins_activation/`.
- **ARRR/ZCoin and direct-withdraw coin previews through `task::withdraw`.** ZCoin/ARRR shielded withdraws, plus BCH, QRC20, SLP, Solana/SPL, Sia, and Tendermint native/token withdraws, are routed through the task-withdraw API instead of failing preview/status with `CoinDoesntSupportInitWithdraw` when clients use the task path. Lightning remains intentionally unsupported by withdraw because invoices are the payment entrypoint. Code: `mm2src/coins/rpc_command/init_withdraw.rs`, `mm2src/coins/z_coin.rs`.
- **ARRR/ZCoin light-mode shielded withdrawal construction.** Shielded ARRR withdraw previews in light mode now build spends from the scanned `<TICKER>_RELOADED_WALLET.db` note/witness data instead of entering the native zcashd `ZRpcOps` path, preventing preview-time KDF crashes on Electrum-backed activations. Code: `mm2src/coins/z_coin/`.
- **Orderbook address handling for Tendermint/Sia/Solana-family configs.** Orderbook and best-orders rendering no longer panic when a P2P order references non-UTXO protocol configs encountered by multi-coin wallet sessions. Tendermint addresses are derived from the order pubkey where possible; unsupported address families now return structured per-order errors and are skipped instead of crashing KDF. Code: `mm2src/mm2_main/src/lp_ordermatch/`.
- **Legacy maker-swap refund event compatibility.** Data-less historical
  `MakerPaymentRefundStarted` and `MakerPaymentRefundFinished` milestones now
  deserialize as their distinct unit events, so old refund-path swaps no longer
  disappear from swap history/status with `missing field data`. The known
  payload-bearing refund-start form remains accepted and preserves its
  `wait_until` deadline as a wait-refund milestone. Code:
  `mm2src/mm2_main/src/lp_swap/`.
- **Legacy swap instruction-event compatibility.** Saved maker/taker swap files whose `MakerPaymentInstructionsReceived` or `TakerPaymentInstructionsReceived` event omitted the optional `data` field now deserialize as `None`, preventing repeated `missing field data` errors in swap history/status polling. Code: `mm2src/mm2_main/src/lp_swap/`.
- **Legacy swap-history resilience.** `my_recent_swaps` now logs and omits missing or corrupt saved-swap rows instead of returning `null` entries or failing typed recent-swap consumers, preventing wallet swap pages and order processing from being destabilized by incompatible historical swap files. Code: `mm2src/mm2_main/src/lp_swap/`.
- **Ordermatch trie-delta removal test made deterministic.** The orderbook sync test no longer depends on nondeterministic ordering when asserting a delta after removed orders. Code: `mm2src/mm2_main/src/ordermatch_tests.rs`.
- **Coin activation/runtime hardening for multi-asset wallets.** ERC20/BEP20 tokens with missing or zero config decimals now fall back to on-chain `decimals()`, NFT activation lazily opens the native async SQLite store instead of failing with `async_sqlite_connection is not initialized`, and Tendermint RPC node selection falls back from `/health` to `abci_info` before declaring all nodes unavailable. Code: `mm2src/coins/`, `mm2src/coins_activation/`.
- **Runtime stubs converted to explicit no-op/errors where safe.** Unsupported TRON V1 swap paths now return structured errors, L2 SQL transaction-history queries return an unsupported-history error instead of panicking, wasm crash-report initialization is an intentional no-op, and public-key trait methods for enabled coin families no longer panic when called by production paths. Code: `mm2src/coins/`, `mm2src/common/`.

## [Unreleased]
- **Background retry and diagnostic noise hardening.** Persistent QRC20 history
  failures now back off from 10 seconds to a bounded five-minute retry interval
  and reset after recovery. Native Electrum reconnect loops retain the first or
  changed endpoint error at `ERROR` while identical retries move to `DEBUG`;
  redundant rustls handshake/alert internals are omitted from the application
  log while the endpoint-specific KDF error remains. Optional
  `eth_feeHistory`, late request-response completions, routine peer exchange,
  and duplicate peer-exchange dial diagnostics no longer appear as operator
  errors. Code: `mm2src/coins/`, `mm2src/common/`, `mm2src/mm2_p2p/`.

### Changed / dependencies

- **Security and yanked dependency refresh.** Transitive `ruint` is upgraded
  from 1.18.0 to the security-fixed 1.20.0 for RUSTSEC-2026-0220. Compatible
  patch updates also replace the yanked `ahash 0.7.6`,
  `crossbeam-channel 0.5.1`, and `rmp-serde 0.14.3` releases; MessagePack stays
  on the wire-compatible 0.14 line. Obsolete `libsqlite3-sys` and `rand`
  advisory exceptions are removed from the audit policy.
- **Stable modern `librustzcash` stack with schema-safe shielded rescan.**
  Z-coin now uses the coherent stable crates.io line
  (`zcash_client_backend 0.23.0`, `zcash_client_sqlite 0.21.1`, and
  `zcash_primitives`/`zcash_proofs 0.28.0`) instead of the legacy vendored 0.5
  API. Reloaded stores the modern wallet and compact cache as
  `<TICKER>_RELOADED_WALLET.db` and
  `<TICKER>_RELOADED_COMPACT_BLOCKS.db`, leaving GLEEC/pre-upgrade files
  untouched. Exact legacy schemas are preserved and rebuilt/rescanned rather
  than migrated in place; unknown or corrupt schemas fail without mutation.
  The selected-current migration/schema fingerprint is pinned, cached-chain
  continuity is validated before scanning, and KDF P2SH transaction bytes are
  protected by full serialization regressions. The broad old Zcash workspace
  dependency is replaced by exact registry versions plus three narrow,
  documented crate patches for the obsolete `time-core` pin and required KDF
  transaction-builder compatibility. See CRD chapter 39 and
  [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md). This port completes the
  Light-mode modern store/scan path; the pre-existing Native full-node adapter
  does not yet populate that modern compact cache and remains an explicit CRD
  chapter-39 gap.
- **Shared SQLite binding upgraded from `rusqlite 0.24.2` to `0.37.0`.** All
  native SQLite consumers now share the version required by the stable Zcash
  wallet store. Legacy `NO_PARAMS`/named-query APIs and dynamic parameter calls
  were ported without changing their SQL contracts. The upgrade also exposed
  and removed one deprecated Solana keypair accessor in tests; the independent
  `nom 6.1.2` future-incompatibility remains owned by the pinned `sia-rust`
  dependency rather than Zcash.
- **`rand` 0.7 → 0.8** across all direct reloaded usages (RUSTSEC-2026-0097; upstream-blocked advisory). Code: multiple crates.
- **`mm2_metrics` rewritten** with a hand-rolled Prometheus registry; drops the dead `metrics-runtime 0.13` / `metrics-util` stack, clearing RUSTSEC-2021-0113. Code: `mm2src/mm2_metrics/`.
- **`anyhow` bumped to 1.0.103, `crossbeam-epoch` to 0.9.20** (advisory clears). Code: `Cargo.toml`.
- **CI test failures now include Rust backtraces.** `tests.yml` sets `RUST_BACKTRACE=1` for test jobs so panics provide actionable stack traces in CI logs. Code: `.github/workflows/tests.yml`.

## [0.1.0-beta.2] — 2026-07-07

Stability release focused on HD-wallet interoperability with the Komodo DeFi wallet (Flutter SDK). Continues from `0.1.0-beta.1` under GPLv2-only.

### Fixed

- **HD-wallet login no longer hangs on a spinner.** `RpcTaskStatus` now serialises the terminal state as `status: "Ok"` / `status: "Error"` (tagged `status`/`details`) instead of the previous `Ready` envelope, matching what the SDK's task-status polling expects. Applies to `task::enable_utxo`, `task::account_balance`, `task::create_new_account`, `task::withdraw` and other long-running task-status responses. Code: `mm2src/rpc_task/`.
- **Concurrent coin activation no longer throws `CoinIsAlreadyActivated`.** A coin already registered under a racing activation request is now treated as an idempotent success rather than surfacing an uncaught exception to the client. Code: `coins_activation/src/standalone_coin/init_standalone_coin.rs`.
- **HD balance responses are ticker-keyed.** `HDAccountBalance.total_balance` and `HDAddressBalance.balance` are now `{ "<TICKER>": { spendable, unspendable } }` maps, fixing the wallet's `PUBKEY_ACTIVATION_ERROR` / `"String is not a subtype of Map"` on HD activation. The legacy `my_balance` response is unchanged (remains a flat object). Code: `coins/coin_balance.rs` and HD balance/activation paths.

### Changed / divergences

- **Withdraw with an omitted HD `from` defaults to the enabled address** (account 0 / external / index 0) instead of failing with `FromAddressNotFound`. This is a deliberate divergence from GLEEC KDF, documented in [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md); there is no compatibility switch in this release. An explicit but invalid `from` is still rejected.
- **Legacy `<db_root>/wallets/*.wallet` wallets are read (login/list/delete) but never written**; new wallets are always the canonical `<name>.json` record. Documented in [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md) and CRD chapter 07 §7.5/§7.7 (R9A). The storage format does not by itself select HD vs single-address signing.

## [0.1.0-beta.1] — 2026-07-05

Initial public beta (first published pre-release). Continues the Komodo DeFi Framework codebase from the GPLv2 anchor commit `c1d46c0c1592faa0860f704008b2b2381bc3840f` (2022-06-03) under GPLv2-only.

For the full feature delta against the GLEEC fork of the upstream codebase, see [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md).

Highlights:

- Mainnet support on netid 8762 (AtomicDEX) and netid 6133 (GLEEC).
- Stable Rust toolchain (no nightly pin).
- Self-hosted CI with split format / unit-test / docker-test jobs.
- On-demand release builds for Linux (additional platforms via the umbrella dev-build workflow).
- Documentation-only compatibility convention for GLEEC-equivalent operation (developer rule in [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md), central admin chapter in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md)); no divergent settings active in this release.
- Test-only netids (8100, 8999, 9000, 9998) gated behind the `regtest-netid` Cargo feature, off by default in production builds.
- EVM ABI encoding/decoding migrated from `ethabi` to `alloy` (`alloy-dyn-abi` / `alloy-json-abi` / `alloy-primitives`), guarded by a byte-identity golden regression suite.
- Tag-driven, GPG-signed release pipeline: signed `SHA256SUMS` manifest and drafted GitHub Releases — pre-releases from `staging` (`-alpha/-beta/-rc` tags), finals from `main`. Linux binaries build inside a Debian 11 container (glibc 2.31) for broad backwards-compatibility; unsigned all-platform snapshots via manual `dev-build.yml` and automatic `staging-build.yml`.

[Unreleased]: https://github.com/kdf-reloaded/kdf/compare/v0.2.0-beta.1...HEAD
[0.2.0-beta.1]: https://github.com/kdf-reloaded/kdf/compare/v0.1.0-beta.2...v0.2.0-beta.1
[0.1.0-beta.2]: https://github.com/kdf-reloaded/kdf/compare/v0.1.0-beta.1...v0.1.0-beta.2
[0.1.0-beta.1]: https://github.com/kdf-reloaded/kdf/releases/tag/v0.1.0-beta.1
