# Unit Tests

This document describes the unit tests added to improve coverage across the codebase.
All tests listed here are fully offline — they require no network, no running binary,
and no external services.

## Running Tests

```bash
# All unit tests (whole workspace)
cargo test --bins --lib

# Per-crate (faster iteration)
cargo test -p coins_activation --lib
cargo test -p mm2_main --lib ordermatch_tests
cargo test -p coins --lib -- eth::eth_tests::test_addr
cargo test -p coins --lib -- eth::eth_tests::test_gas_station_data
cargo test -p coins --lib -- eth::eth_tests::test_u256
cargo test -p coins --lib -- eth::eth_tests::test_wei
cargo test -p coins --lib -- rpc_response_tests coin_protocol_capability::

# The withdraw, QTUM, BCH and SLP tests of `coins`, offline (the CI row `withdraw-family`)
cargo test -p coins --lib -- withdraw qtum bch slp

# Bounds on a value received from a peer (CRD ch.28 R16a, T4a)
cargo test -p common --lib msgpack_bounds::
cargo test -p mm2_p2p --features application --lib t4a_
cargo test -p coins --lib t4a_
cargo test -p mm2_main --lib t4a_
```

## Tests that need public servers (`live-network-tests`)

Some tests connect to public servers: the DOC and MARTY Electrum servers of cipig, the Bitcoin, Bitcoin
Cash, Firo, Verus, Dogecoin and other Electrum servers, the Qtum testnet server (see below), the public
Ethereum dev node, a BCHD node, the Solana test networks, the public price service and `httpbin.org`. Such a test fails whenever a
server is down, slow or has changed, and that says nothing about the code. So these tests are off by
default. Each one carries this attribute right after `#[test]`:

```rust
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
```

A plain `cargo test` lists them as ignored. To run them, build with the cargo feature of the crate that
holds the test. The feature exists in `coins`, `mm2_main` and `mm2_net`, and does nothing else:

```bash
# Unit tests of the coins crate (UTXO, QRC20, BCH and SLP, Solana)
cargo test -p coins --lib --features live-network-tests utxo::utxo_tests::
cargo test -p coins --lib --features live-network-tests qrc20::

# The integration tests of mm2_main, which start a node and use public servers
cargo test -p mm2_main --lib --features regtest-netid,live-network-tests -- mm2::mm2_tests::test_my_balance

# The one test of mm2_net
cargo test -p mm2_net --lib --features live-network-tests test_slurp_req
```

The integration tests in `mm2_tests` need the `regtest-netid` feature (they start nodes on the test netid) and run
as library tests of `mm2_main`, with no Docker. The `docker_tests` binary is only for the tests that need the
local Docker chains: it pulls images and starts containers unless `_MM2_TEST_CONF` is set.

What carries the attribute:

- `coins`: the tests of `utxo/utxo_tests.rs` that build an Electrum client or activate a coin on a public
  server (`electrum_client_for_test` waits for a first connection, so even a test that only checks a
  signature needs the server), the tests of `qrc20/qrc20_tests.rs`, `qrc20/history.rs`, `utxo/bch.rs` and
  `utxo/slp.rs` (all through the testnet coin helpers), the BCHD tests of `utxo/bchd_grpc.rs`, and the
  Solana tests that read a balance or send a transaction.
- `mm2_main`: the tests of `mm2_tests.rs` and its `mm2_tests/` modules that enable a coin on a public
  server, `test_process_price_request` (it fetches the public price service), and three tests of `docker_tests.rs` that need a public server besides the Docker nodes
  (`test_get_max_taker_vol_with_kmd`, `test_match_utxo_with_eth_taker_sell`,
  `test_match_utxo_with_eth_taker_buy`).
- `mm2_net`: `test_slurp_req`.

What does not, and why:

- Tests that use a local stub, a mock or the Docker nodes only.
- The `wasm32` tests, which run in a browser.
- The tests behind `zhtlc-native-tests`, which has its own feature and set-up.
- `test_withdraw_and_send`, `test_withdraw_legacy` and the few other tests with a plain `#[ignore]`: they
  stay ignored with or without the feature. The first two run in the `external-network-tests` CI job with
  `--include-ignored`; see [DISABLED_TESTS.md](DISABLED_TESTS.md).
- `polygon_check_if_my_payment_sent`, which skips itself when `POLYGON_RPC_URL` is not set.

No CI job runs a gated test unless it passes the feature on purpose. The jobs that run unit tests select
their tests by name. Only the filter of the `withdraw-family` row (`withdraw qtum bch slp`, added in batch 4)
also matches gated tests; that row runs without the feature, so it skips them and runs the offline tests of
the family. The `docker-tests` job selects three gated tests by name
(`test_withdraw_segwit`, `test_withdraw_cashaddresses` and
`test_withdraw_to_different_cashaddress_network_should_fail`), so it passes the feature. A new test that needs
a public server should carry the attribute, and it joins a CI job only if someone adds the feature to that
job on purpose. If a gated test fails, look at the server before you look at the code.

## tQTUM tests and their Electrum server

Most of the tests that activate the Qtum testnet coin (`tQTUM`, and `QRC20` on it) read the
live testnet chain, so they are the exception to "fully offline" above: they need network access
to one server. The tests are in `mm2src/coins/utxo/utxo_tests.rs` (the `test_qtum_*` ones),
`mm2src/coins/qrc20/qrc20_tests.rs` and `mm2src/mm2_main/src/mm2_tests.rs`.

- **Server.** All of them list only `s6.electrum.qtum.site:51001` (plain TCP). It is the one
  public Qtum *testnet* Electrum server. The `qtum.info` hosts used earlier serve Qtum mainnet,
  so on them the test wallets were empty and no delegation or staker existed. Its TLS port
  presents an X.509 version 1 certificate, which this project refuses in every mode (CRD ch.38
  R38.6.4m item 5), so only the plain port is listed.
- **Slow answers.** The server can be slow to answer for a busy address. A request over an open
  connection gets a fixed 20 seconds (CRD ch.38 R38.6.4e item 2), which a test cannot raise; the
  `timeout_sec` of a server entry bounds only the connection set-up. If a test fails on a
  timeout, run it again before suspecting the code.
- **No broadcast.** The delegation tests (`add_delegation`, `remove_delegation`) only build and
  sign a transaction; none of these tests changes chain state.
- The list lives in one place per test crate: `tqtum_test_servers()` in `utxo_tests.rs` (used by
  `qrc20_tests.rs` too) and `TQTUM_TEST_SERVERS` in `mm2_tests/electrums.rs`. Add a second testnet
  server there if one appears.

## coins_activation (35 tests)

Location: `mm2src/coins_activation/src/tests.rs`

These tests cover error types, From impls, HttpStatusCode mappings,
Display formatting, and serde round-trips for every public request/error type
in the crate.

| Category | Count | What is tested |
|----------|-------|----------------|
| EnableTokenError | 7 | Display, HttpStatusCode, From (CoinConfWithProtocolError, BalanceError, UtxoRpcError), JSON serialization |
| EnableTokenRequest | 3 | Valid deser, missing ticker, missing params |
| EnableL2Error | 4 | Display, HttpStatusCode, From, JSON tag |
| EnableL2Request | 2 | Valid deser, missing ticker |
| EnablePlatformCoinWithTokensError | 5 | Display, HttpStatusCode, From (2), JSON tag |
| EnablePlatformCoinWithTokensReq | 2 | Valid deser, missing ticker |
| TokenActivationRequest | 1 | Serde |
| InitTokensAsMmCoinsError | 1 | From TokenProtocolParseError |
| InitStandaloneCoinError | 6 | Display, HttpStatusCode, From (2), JSON tag (2) |
| DerivationMethod | 2 | Serialize Iguana/HDWallet |
| CoinAddressInfo | 2 | Serialize with balances |

## ordermatch (17 new tests, 65 total)

Location: `mm2src/mm2_main/src/ordermatch_tests.rs` (appended to existing file)

Pure-logic tests for order matching functions, builder validation, and serde.

| Category | Count | What is tested |
|----------|-------|----------------|
| alb_ordered_pair | 3 | Reversed order, same coin, case sensitivity |
| parse_orderbook_pair_from_topic | 6 | Empty, wrong prefix, no colon, only prefix, trailing colon, valid round-trip |
| OrderConfirmationsSettings | 4 | reversed(), double reverse identity, serde round-trip, Default |
| TakerOrderBuilder validation | 4 | base==rel, zero pubkey, no conf settings, success |
| MakerOrderBuilder validation | 4 | base==rel, no conf settings, price too low, success |
| Error Display | 2 | TakerOrderBuildError, MakerOrderBuildError |
| TakerRequest serde | 1 | Round-trip serialization |
| MatchBy serde | 3 | Any, Orders, Pubkeys variants |

Helpers added: `make_test_coin_pair()`, `default_conf_settings()`, `nonzero_pubkey()`.

## ETH utility tests (12 new tests)

Location: `mm2src/coins/eth/eth_tests.rs` (appended to existing file)

Pure tests for ETH utility functions that don't need a node connection.

| Category | Count | What is tested |
|----------|-------|----------------|
| addr_from_pubkey_str | 3 | Valid compressed key, invalid hex, wrong length |
| wei_from_gwei_decimal | 3 | 1 gwei, fractional gwei, large value |
| GasStationData serde | 3 | Standard format, Matic format (alias), missing field |
| u256_to_big_decimal | 2 | Round-trip, zero |
| wei_to_gwei_decimal | 1 | Round-trip |

## Capability words in protocol information

Location: `mm2src/coins/coin_protocol_capability.rs` (module `tests`) and the module
`t14_capability_word_tests` of `mm2src/mm2_main/src/ordermatch_tests.rs`. The coins tests run in the
`rpc-response` cell of `.github/workflows/test.yml`; the order-match tests run with `ordermatch_tests`.
They cover CRD chapter 59 §59.4A, test T14.

| Category | What is tested |
|----------|----------------|
| T14 (a) writer | Fixed bytes for one and two keys, ascending order, no duplicates, no empty map, first value kept unchanged |
| T14 (b) reader | Every input of the chapter, plus the nesting bound, over-long declared lengths and counts, the reserved marker, every marker family as a first value |
| T14 (c) first value | Both readers of a UTXO address format give the same result with the word appended |
| T14 (d) opt-in | A UTXO coin's protocol information is exactly its encoded address format; a coin of another family sends nothing |
| T14 (e) carriage | The three order-match messages and the synchronization answers carry the bytes unchanged; the trie root is equal with and without the word |
| T14 (f) decisions | A test coin that requires `tx_v6`: reservations and requests without it are neither connected nor reserved, with it both go ahead, and no swap or penalty record appears |

## ARRR requires the version-6 capability word

Location: `mm2src/coins/z_coin.rs` (module `ironwood_v6_counterparty_tests`), `mm2src/coins/z_coin/z_note_lock_tests.rs`
and `mm2src/coins/coin_protocol_capability.rs` (module `tests`). The z_coin tests run in the `shielded`
cell (`cargo test -p coins --lib z_coin::`); the classifier test runs in the `rpc-response` cell. They
cover CRD chapter 39 R39.6.4f, test T39.6.4f. The pure functions take the clock as an argument.

| Test | What is tested |
|------|----------------|
| T39.6.4f (a) `t39_6_4f_a_protocol_info_is_the_address_format_then_the_word` | ARRR sends its address-format bytes followed by {`tx_v6`: `true`}; a configured time or height, or a discovered height, counts as an upgrade; a coin without one, and a build without version 6, send the bytes unchanged |
| T39.6.4f (b) `t39_6_4f_b_inside_the_window_a_counterparty_without_the_word_is_declined` | Absent, empty, first-value-only, malformed, truncated, `false` and empty-map information is declined with its cause; the word is accepted, with extra keys and trailing bytes ignored |
| T39.6.4f (c) `t39_6_4f_c_before_the_window_nothing_is_required_and_a_configured_height_opens_it` | Nothing is required one second before the window and with an activation time far ahead; a configured or discovered height alone opens it; a coin without an upgrade requires nothing; a time near the epoch does not wrap |
| T39.6.4f (d) `t39_6_4f_d_the_switch_and_the_gate_switch` | `require_v6_counterparty` defaults to `true`, `false` admits a counterparty without the word and still sends its own, and `ironwood_gate_disabled` changes no outcome |
| T39.6.4f (e) `t39_6_4f_e_logs_one_debug_per_decline_and_one_warning_per_hour`, `t39_6_4f_e_the_log_lines_do_not_carry_the_protocol_information` | One debug line per decline naming coin and cause; the first warning at once, then at most one per hour with the count since the previous one; state per coin instance; a clock stepping back does not warn; no protocol bytes in the lines |
| T39.6.4f (f) `t39_6_4f_f_the_freeze_still_declines_everything_for_a_build_without_v6` | The freeze predicate, consulted first and without the counterparty's bytes, declines throughout the window for a build without version 6 |
| T39.6.4f (a)-(c) `t39_6_4f_the_coin_methods_send_decline_and_accept` | On a real light-mode coin (needs the Sapling parameter files, else skipped): unchanged bytes and nothing required without an upgrade; after a height is discovered the word is sent, and `is_coin_protocol_supported` declines the first value alone and accepts the word |
| `t14_classifier_tells_the_three_causes_apart` | `classify_capability_word` separates no word, a malformed word (and a truncated first value) and a well-formed word, and agrees with the reader |

## Bounds on a value received from a peer (CRD ch.28 R16a, T4a)

Locations: `mm2src/common/msgpack_bounds.rs`, `mm2src/mm2_p2p/src/lib.rs`,
`mm2src/mm2_p2p/src/atomicdex_behaviour.rs`, `mm2src/coins/utxo/utxo_tests.rs`,
`mm2src/mm2_main/src/ordermatch_tests.rs`, `mm2src/mm2_main/src/lp_network.rs`,
`mm2src/mm2_main/src/lp_swap/swap_msg.rs` and `mm2src/mm2_main/src/lp_swap/swap_watcher.rs`.

The tests are named `t4a_<bullet>_...` after the bullets of T4a. All of them run in the `peer-decoding`
cell of the CI unit tests; the `ordermatch_tests` ones also run in the `ordermatch` cell.

| Bullet | Where | What is tested |
|--------|-------|----------------|
| (a) | `mm2_p2p`, `mm2_main` | Every message type the node sends passes the check and nests at most 32 levels |
| (b) | `common` | Nesting of exactly 64 levels passes; 65 and 10,000 are dropped (positional values, maps, nested maps, mixed, empty containers) |
| (c) | `common` | bin, str and ext headers declaring 0xFFFFFFF0 bytes, array and map headers declaring 0xFFFFFFFF elements, the reserved marker, every prefix of a valid value, every marker family walked to its exact end |
| (d) | `mm2_p2p` | An envelope beyond the bounds is dropped before its signature is examined and counted; a normal envelope and the peers-topic addresses still decode |
| (e) | `common` | Bytes after the first value are ignored |
| (f) | `mm2_main`, `coins` | The order record, the address format of a protocol-information value and the coin's own check drop or fall back beyond the bounds and are unchanged for the real formats |

## Pre-existing test compilation fix

The `EthCoinImpl` struct gained a `derivation_method` field that was not present
in 9 existing test constructors, causing the entire `eth_tests` module to fail
to compile. All 9 sites now set
`derivation_method: DerivationMethod::Iguana(my_addr)`.

## ARRR Ironwood transactions: reading and deposits (11 tests)

Locations: `mm2src/coins/z_coin/z_ironwood_read_tests.rs` (fixtures and the full-byte readers),
`mm2src/coins/z_coin/z_lightwalletd/tests.rs`, `mm2src/coins/z_coin/z_lightwalletd/htlc_index/tests.rs`,
`mm2src/coins/z_coin/z_coin_wallet_db.rs` (`tests` module). CI cell: `shielded`
(`cargo test -p coins --lib z_coin::`). CRD: chapter 39, T39.6.5a, R39.8.0bb and T39.8.0i.

The fixtures are two mainnet ARRR transactions fetched once with `GetTransaction` and stored as the
raw bytes the endpoint returned (`mm2src/coins/z_coin/test-fixtures/arrr-ironwood-*.hex`): a
withdrawal from an Ironwood wallet (height 4198585, one Sapling spend, two Sapling outputs, two
Ironwood actions) and a deposit into a Sapling address (height 4198378, no Sapling spend, two
Sapling outputs, two Ironwood actions). Building the Ironwood verifying key takes roughly a minute (54 to 80 seconds measured)
in a debug build and happens once per test binary; each bundle then verifies in about 0.3 seconds.
The Sapling parameter files are needed only by the spend test, which skips with a message when they
are absent (the same arrangement as the version-6 construction tests).

| Test | CRD | What is tested |
|------|-----|----------------|
| `t39_6_5a_the_fixtures_are_the_recorded_bytes` | T39.6.5a | SHA-256 of each fixture, version-6 header and version-group id |
| `t39_6_5a_the_project_reader_round_trips_and_names_each_mainnet_ironwood_transaction` | T39.6.5a | `z_tx_read::read_tx`: version, branch, shape, byte-identical re-serialization, display and internal identifier |
| `t39_6_5a_every_other_reader_of_full_bytes_accepts_the_mainnet_transactions` | T39.6.5a | The direct `Transaction::read` of `tx_enum_from_bytes`, `gen_tx` and the commitment-tree loop under two branch arguments; the resulting `TransactionEnum`; the Sapling commitments |
| `t39_6_5a_the_broadcast_record_reads_the_mainnet_transactions` | T39.6.5a, D39.8.0e, R39.6.5g | The broadcast record: a deposit has nothing to record; the withdrawal's fee is derived from the bytes (Sapling and Ironwood value balances: 10,000 base units), so the record goes on to the wallet database (unavailable here) |
| `t39_6_5a_send_raw_transaction_names_a_mainnet_ironwood_transaction_by_the_chain_identifier` | T39.6.5a | `send_raw_transaction` over the scripted transport: the identifier it returns is the chain's; the bytes reach the transport unchanged |
| `t39_6_5a_the_history_reader_accepts_a_mainnet_transaction_with_ironwood_actions` | T39.6.5a | The HTLC history reader accepts both transactions and names them |
| `t39_6_5a_the_ironwood_bundle_of_each_mainnet_transaction_verifies` | T39.6.5a | Halo2 proof, spend-authorization and binding signatures verify with the project's verifying key for the post-NU6.3 circuit; a signature hash off by one bit does not verify |
| `t39_6_5a_a_changed_proof_byte_does_not_verify` | T39.6.5a | One changed proof byte, with valid signatures, does not verify |
| `t39_8_0i_a_deposit_with_ironwood_actions_scans_to_the_note_the_tree_and_the_balance` | T39.8.0i, R39.8.0bb | A compact block built as wire bytes with field 6 and decoded as production does: the note is found, the Sapling tree holds exactly one commitment, the balance counts the note once |
| `t39_8_0i_a_following_build_spends_the_note_deposited_with_ironwood_actions` | T39.8.0i, R39.8.0bb | A build after that scan selects and spends the deposited note; the wallet records it as spent |
| `t39_8_0i_the_same_transaction_in_the_mempool_counts_as_pending` | T39.8.0i, R39.8.0bb | The same transaction as a `GetMempoolTx` compact transaction: a pending receipt with the right value, none for another key, and not counted again once its block is scanned |

## ARRR background sync: every pass refreshes the pending view (6 tests)

Locations: `mm2src/coins/z_coin.rs` (module `shielded_sync_pass_tests`, steps scripted, no network, no
wallet database) and `mm2src/coins/z_coin/z_note_lock_tests.rs` (a real Light-mode coin over a wallet
database; it needs the Sapling parameter files and says SKIPPED where they are absent). CI cell:
`shielded` (`cargo test -p coins --lib z_coin::`). CRD: chapter 39, R39.8.0aj1 and T39.8.0g1. The
mempool poll itself is a gRPC stream and stays covered by T39.8.0g.

| Test | CRD | What is tested |
|------|-----|----------------|
| `t39_8_0g1_a_pass_with_no_new_block_refreshes_once_with_the_tip` | T39.8.0g1, R39.8.0aj1 | Tip equal to the scanned height: no download, no scan, one refresh with the tip as its height |
| `t39_8_0g1_a_pass_whose_download_fails_still_refreshes` | T39.8.0g1, R39.8.0aj1 | New block, download fails: no scan, the failure recorded, one refresh |
| `t39_8_0g1_a_pass_whose_scan_fails_still_refreshes_after_the_attempt` | T39.8.0g1, R39.8.0aj1 | New block, scan fails: one refresh, after the scan attempt |
| `t39_8_0g1_a_pass_that_scans_refreshes_after_the_scan` | T39.8.0g1, R39.8.0aj1, R39.8.0ah | New block, scan succeeds: the refresh comes after the scan, once |
| `t39_8_0g1_a_pass_that_cannot_read_the_tip_refreshes_with_the_scanned_height` | T39.8.0g1, R39.8.0aj1 | Tip unreadable: the failure recorded, one refresh with the wallet's scanned height |
| `t39_8_0g1_a_mempool_output_with_the_tip_unchanged_reaches_the_unspendable_balance` | T39.8.0g1, R39.8.0aj1 | A wallet-owned output enters the (scripted) mempool while the tip stays put: unknown to the balance before the next pass, non-spendable balance after it; the spendable figure is untouched |

## ARRR withdrawals to a Pirate1 address (21 tests)

Locations: `mm2src/coins/z_coin/z_pirate1_tests.rs` (no coin needed),
`mm2src/coins/z_coin/z_note_lock_tests/pirate1_withdraw.rs` (a real Light-mode coin over a wallet
database and the scripted lightwalletd transport of the note-lock harness). CI cell: `shielded`
(`cargo test -p coins --lib z_coin::`). CRD: chapter 39, §39.6.5 (R39.6.5a to R39.6.5h, T39.6.5b to
T39.6.5e) and T39.5A.a.

The tests of the second and third file make real Sapling proofs and a real Halo2 Ironwood proof; they need
the Sapling parameter files and skip with a message where those are absent (the same arrangement as
the other wallet-database tests). The first Ironwood build of a test binary derives the Ironwood
proving key (once per binary, inside the builder) and takes about 100 seconds in a debug build when it
runs alone (96.5 s measured, 120 s with the other tests of this section, 321 s inside the whole
`z_coin::` cell, where the tests share the cores); the verifying key takes another 34 to 66 seconds,
also once per binary. Once the keys exist a built Ironwood bundle verifies in about 0.15 seconds. The test vectors are generated from a real
Orchard-type address (key `[7; 32]`); the Ironwood anchor of the mainnet withdrawal fixture is the
root of the empty tree and the bundle is output-only (spends disabled), which `t39_6_5d_the_mainnet_payment_into_ironwood_is_output_only_at_the_empty_tree_root` records.

| Test | CRD | What is tested |
|------|-----|----------------|
| `t39_6_5b_the_vectors_are_the_bech32_encoding_of_a_real_address` | T39.6.5b | The three pinned strings are the classic Bech32 encoding of a real address |
| `t39_6_5b_a_valid_address_is_accepted_on_a_coin_of_its_own_network` | T39.6.5b | Main, test and regtest address accepted on a coin of that network (also in upper case) |
| `t39_6_5b_each_address_is_refused_on_a_coin_of_another_network` | T39.6.5b | The six cross-network pairs are refused, with the reason naming both prefixes |
| `t39_6_5b_each_defect_is_refused_with_its_reason` | T39.6.5b | Bad checksum, Bech32m checksum, 42 and 44-byte data parts, 43 bytes that are no Orchard-type address, non-zero padding, mixed case |
| `t39_6_5b_a_configured_prefix_replaces_the_builtin_one` | T39.6.5b | `hrp_ironwood_payment_address` replaces the built-in prefix; the built-in one is then another network's |
| `t39_6_5b_a_coin_without_an_ironwood_pool_refuses_every_pirate1_address` | T39.6.5b | No pool: every address refused. A pool is a configured height or time, the built-in time of the real chain, or a discovered height; `ironwood_gate_disabled` changes nothing |
| `t39_6_5b_sapling_strings_keep_their_answers` | T39.6.5b | Sapling addresses and Sapling defects keep their answers; the withdrawal reason names both accepted forms on a coin with a pool |
| `t39_5a_a_the_divergence_from_the_reference_is_documented_where_it_is_tested` | T39.5A.a | Parity record (in the doc comment, not asserted): the reference answers `is_valid: false` and `InvalidAddress` for the same strings |
| `t39_6_5f_the_fee_is_fixed_for_an_ironwood_output_and_configured_otherwise` | R39.6.5f | 10,000 base units with an Ironwood output whatever `txfee` says; the configured fee otherwise |
| `t39_6_5d_the_mainnet_payment_into_ironwood_is_output_only_at_the_empty_tree_root` | T39.6.5d | The mainnet fixture that pays from Sapling into Ironwood: empty-tree anchor, spends disabled, outputs enabled. The mainnet deposit (spends an Ironwood note) is the other form: spends enabled, a real anchor |
| `t39_6_5c_a_target_height_below_the_activation_height_is_refused_before_note_selection` | T39.6.5c, R39.6.5d | The gate: activation height, unknown height, native mode, with the coin and the reason named |
| `t39_6_5g_the_fee_is_read_from_the_value_balances_of_the_bytes` | R39.6.5g | The mainnet withdrawal: Sapling plus Ironwood value balance, less transparent outputs |
| `t39_6_5g_a_transaction_with_transparent_inputs_has_no_derivable_fee` | R39.6.5g | The recorded HTLC refund (transparent input) has no derivable fee |
| `t39_6_5c_a_withdrawal_to_a_pirate1_address_builds_the_version_6_transaction` | T39.6.5c | Shape, fee, value balances, empty-tree anchor with spends off and outputs on, recipient output and memo, Sapling change to the wallet, byte-identical read-back, Ironwood proof verifies |
| `t39_6_5c_a_withdrawal_to_a_sapling_address_is_unchanged` | T39.6.5c, R39.6.5f | The configured fee, no Ironwood bundle, no tree-state read; `max` with the configured fee |
| `t39_6_5c_max_sends_the_spendable_balance_less_the_fixed_fee` | T39.6.5c | `max: true` sends the balance less 10,000 base units, no change |
| `t39_6_5c_a_target_height_below_the_activation_height_is_refused` | T39.6.5c | Refused before a note is selected, naming the coin and the reason; no tree-state read |
| `t39_6_5b_validateaddress_and_withdraw_answer_alike_on_a_coin` | T39.6.5b, R39.6.5c | `validateaddress` and the withdrawal on a real coin: accepted, refused, no pool |
| `t39_6_5d_the_built_anchor_and_flags_match_the_chain_wallets_form` | T39.6.5d | Built withdrawals, with and without change: empty-tree anchor, spends off and outputs on, exactly the fixture's form; two actions; everything entering Ironwood is the one payment (the change never does); no tree-state read |
| `t39_6_5e_a_submitted_pirate1_withdrawal_is_recorded` | T39.6.5e, R39.6.5g | Through `send_raw_tx`: the fee read from the bytes (10,000), the note excluded from the next selection, the unmined history row with the final amounts that survives the scan, the change as non-spendable balance, then spendable after the scan. Needs the vendored `zcash_client_sqlite`, which skips the Orchard and Ironwood nullifier marking instead of panicking |
| `t39_6_5a_the_recorder_stores_the_mainnet_withdrawal_with_its_derived_fee` | T39.6.5a, R39.6.5g | The recorded mainnet withdrawal (Sapling spend, two Ironwood actions) goes through the recorder into a real wallet database: stored with the derived fee 10,000; none of the wallet's notes are marked spent |

## ARRR history rows: the wallet's own address in one form, a withdrawal's recipient, and a swap's addresses (14 tests)

Locations: `mm2src/coins/z_coin/z_coin_wallet_db.rs` (module `tests`, the snapshot fixtures of the
history tests: a wallet database filled by hand, no proofs, no network) and
`mm2src/coins/z_coin/z_note_lock_tests/withdraw_recipient.rs` and `.../swap_addresses.rs` (a real
Light-mode coin over a wallet database and the scripted lightwalletd transport of the note-lock
harness). CI cell: `shielded` (`cargo test -p coins --lib z_coin::`). CRD: chapter 39, R39.8.9d1,
R39.8.9d2, R39.8.9d3, T39.8.9d1, T39.8.9d2, T39.8.9d3; the swap tests also cover the fee cases of
R39.8.9e1 and T39.8.9e1.

The recipient and a swap's addresses come from a small file of the node's own,
`<TICKER>_RELOADED_TX_ADDRESSES.db`, beside the wallet database. The tests of the second and third
file make real Sapling proofs (the Pirate1 one also
a real Halo2 proof, with the one-time key derivation described under the Pirate1 withdrawals) and skip
with a message where the Sapling parameter files are absent. The snapshot transaction used for the
withdrawal gets a transaction id that is not the same read forwards and backwards, so a missing byte
reversal between the wallet database's order and the displayed one would fail the test.

| Test | CRD | What is tested |
|------|-----|----------------|
| `t39_8_9d1_the_own_address_is_listed_only_in_the_form_my_balance_reports` | T39.8.9d1, R39.8.9d1 | Deposit rows and a withdrawal row with change: every entry of `from` and `to` is the address the caller passes (the `my_balance` form), and none is a string from the wallet-database crate's `addresses` table, which is checked to be present |
| `t39_8_9d2_the_recipient_is_listed_unmined_mined_and_after_a_restart` | T39.8.9d2 (a), (b) | A Sapling and a Pirate1 destination: the withdrawal's row lists it exactly as given, beside the own address for the change, unmined, after a restart, mined, and after another restart; no other row lists it; the row's hash is in display order |
| `t39_8_9d2_an_entry_without_a_row_adds_none_and_a_row_without_an_entry_lists_no_recipient` | T39.8.9d2 (b), (d) | An entry for a transaction the wallet database does not hold adds no row and no address; with no store file, and with a store file that cannot be read, a row lists no recipient and raises no error |
| `t39_8_9d2_an_expired_entry_is_removed_unless_its_transaction_is_mined` | T39.8.9d2 (c) | Expired and unmined: removed. Expired and mined, expiry not yet passed by the scan, and no expiry height (0): kept. A second pass removes nothing |
| `t39_8_9d3_entries_on_both_sides_join_the_row_and_survive_a_reopen` | T39.8.9d3, R39.8.9d2 (b) | Snapshot rows: two `to` entries beside the own address on a row that spends and receives; a `from` entry on a row that spends no wallet note, with the wallet as receiver; an entry of another transaction does not leak; the same after the store is reopened. Needs no proofs |
| `t39_8_9d2_a_completed_scan_pass_prunes_the_address_record` | T39.8.9d2 (c) | The prune point: after a completed scan pass the entry whose expiry the scanned height has passed is gone and the others stay |
| `t39_8_9d2_the_address_record_survives_a_wallet_database_rebuild` | T39.8.9d2 (a) | The file is named `<TICKER>_RELOADED_TX_ADDRESSES.db` beside the wallet database, and a rebuild of the wallet database leaves its entries |
| `t39_8_9d2_a_sapling_withdrawal_names_its_recipient_in_every_row` | T39.8.9d1, T39.8.9d2 | A real `withdraw` to a Sapling address, submitted through `send_raw_tx`, scanned into its block, and the coin rebuilt over the same directory: the recipient is in the unmined row, the mined row and after each restart; the deposit row lists the own address only; amounts unchanged |
| `t39_8_9d2_a_pirate1_withdrawal_names_its_recipient_in_every_row` | T39.8.9d2 | The same for a Pirate1 destination, which the wallet's Sapling keys cannot open: the recipient is the request's string |
| `t39_8_9d2_an_unsubmitted_withdrawal_adds_no_row_and_its_entry_goes_after_expiry` | T39.8.9d2 (b), (c) | A withdrawal that is built and never submitted adds no row and does not appear on the row of another withdrawal; once the scan passes the expiry height its entry is removed while the mined withdrawal's entry stays |
| `t39_8_9d3_a_payment_lists_the_script_address_and_nothing_for_the_data_carrier` | T39.8.9d3 (a) | A real `z_send_htlc` payment (two transparent outputs: the script and the data carrier): the row lists the script's pay-to-script-hash address in `to`, which parses as such with the coin's prefix and the script's hash, beside the own address for the change, and nothing else; unmined, after a restart, mined, and after another restart. T39.8.9e1: the row also reports the fee the build paid (read from the transaction: the value leaving the Sapling pool less the transparent outputs), in all four states |
| `t39_8_9d3_a_fee_payment_lists_the_fee_address_in_the_sapling_form` | T39.8.9d3 (b) | A real `z_send_dex_fee` payment: the row lists the fee address in `to`, encoded with the coin's Sapling prefix and different from the own address, beside the own address for the change; unmined, mined, and after each restart. T39.8.9e1: the row also reports the fee the build paid (read from the transaction), in all four states |
| `t39_8_9d3_a_spend_and_a_refund_list_the_script_address_as_the_sender` | T39.8.9d3 (c) | A real spend (with the secret) and a real refund of the same payment built by `z_p2sh_spend`: neither has a row before it is mined (R39.8.9i); each mined row lists the script address in `from` and the own address in `to`; the same after a restart. T39.8.9e1 (b): both rows report a fee of `0`, because neither spends a wallet note and neither is recorded at broadcast |
| `t39_8_9d3_a_failing_record_leaves_the_swap_step_whole_and_the_row_without_the_address` | T39.8.9d3, R39.8.9d2 (d) | The record file is replaced by a directory so every write fails: the payment and the spend are still built, broadcast and returned, and their rows list the own address only. T39.8.9e1: the payment row still reports its fee (held by the wallet database, not by the record) and the spend row reports `0` |

## ARRR history rows: the fee of an own send (8 tests)

Locations: `mm2src/coins/z_coin/z_coin_wallet_db.rs` (module `tests`, the snapshot fixtures of the
history tests: a wallet database filled by hand, no proofs, no network) and
`mm2src/coins/z_coin/z_note_lock_tests/history_fee.rs` (a real Light-mode coin over a wallet database
and the scripted lightwalletd transport of the note-lock harness). CI cell: `shielded`
(`cargo test -p coins --lib z_coin::`). CRD: chapter 39, R39.8.9e1, T39.8.9e1.

In Light mode `transaction_fee` is the fee the wallet database holds for the transaction, `0` when it
holds none. The tests of the second file make real Sapling spend proofs (the Pirate1 one also a real
Halo2 proof, with the one-time key derivation described under the Pirate1 withdrawals) and skip with a
message where the Sapling parameter files are absent. The swap payment, swap fee payment, swap spend
and refund cases of T39.8.9e1 are checked by the swap-address tests (the section on the wallet's own
address and a withdrawal's recipient), not here: there each of those tests also asserts the fee.

| Test | CRD | What is tested |
|------|-----|----------------|
| `t39_8_9e1_a_row_with_no_recorded_fee_reports_zero` | T39.8.9e1 (b), (c) | Snapshot with no fee stored (checked to be NULL, not a stored zero): the four deposits and the unmined own send all report `0` |
| `t39_8_9e1_a_recorded_fee_is_reported_unmined_mined_and_after_a_restart` | T39.8.9e1 (a) | A fee stored for the unmined own send is reported in the coin's decimals, after a restart, once the row is mined and after another restart; no other row reports a fee |
| `t39_8_9e1_a_fee_belongs_to_its_own_row_only` | T39.8.9e1 (a), (b) | Fees stored for an own send and for another row are each reported by that row alone; the other three rows report `0` |
| `t39_8_9e1_a_negative_stored_fee_refuses_the_page` | R39.8.9e1 | A negative stored fee is an unreadable row, like a negative amount: the page is refused, not served with a `0` |
| `t39_8_9e1_an_own_send_reports_its_fee_unmined_mined_and_after_a_restart` | T39.8.9e1 (a), (b) | A real `send_outputs` (the swap-payment path) with fee `F`: the row reports `F` unmined, after a restart, after the block that mines it is scanned and after another restart; the receipt of the funding note, a deposit, reports `0` throughout |
| `t39_8_9e1_a_sapling_withdrawal_reports_its_fee_in_every_state` | T39.8.9e1 (a) | A real `withdraw` to a Sapling address, submitted through `send_raw_tx`: the fee of the response is the fee of the row in all four states; before submission there is no row |
| `t39_8_9e1_a_pirate1_withdrawal_reports_its_fee_in_every_state` | T39.8.9e1 (a) | The same for a Pirate1 destination, whose fee is the fixed 10,000 base units whatever the configured fee is |
| `t39_8_9e1_a_rescanned_own_send_reports_zero_after_a_wallet_database_rebuild` | T39.8.9e1 (c) | After a real send is mined, the wallet database is rebuilt and the funding block and the mining block are scanned again: the own send is found (`spent_by_me` is the note) and reports `0`, the deposit too |

## ARRR withdraw response: `my_balance_change` is received minus spent (2 tests)

Location: `mm2src/coins/z_coin/z_note_lock_tests/withdraw_balance_change.rs` (a real Light-mode coin over
a wallet database and the scripted lightwalletd transport of the note-lock harness). CI cell: `shielded`
(`cargo test -p coins --lib z_coin::`). CRD: chapter 39, R39.5A.3, R39.6.5c, T39.5A.c.

The tests make real Sapling spend proofs (the Pirate1 one also a real Halo2 proof, with the one-time key
derivation described under the Pirate1 withdrawals) and skip with a message where the Sapling parameter
files are absent.

| Test | CRD | What is tested |
|------|-----|----------------|
| `t39_5a_c_a_sapling_withdrawal_reports_received_minus_spent` | T39.5A.c, R39.5A.3 | A real `withdraw` of `V` with fee `F` and change `C` from one note `N = V + F + C`: `spent_by_me = N`, `received_by_me = C`, `my_balance_change = -(V + F)` (negative, equal to received minus spent), `total_amount = N`, the tagged `Utxo` fee with the coin's ticker and `F`, block height and timestamp 0, no `kmd_rewards`. After `send_raw_tx` the history row of the same transaction carries the same `my_balance_change`, `spent_by_me` and `received_by_me`, unmined and after the block that mines it |
| `t39_5a_c_a_pirate1_withdrawal_reports_received_minus_spent` | T39.5A.c, R39.6.5c | The same for a Pirate1 destination, whose fee is the fixed 10,000 base units whatever the configured fee is |

## ARRR withdrawal refuses `fee` and `from` (2 tests)

Location: `mm2src/coins/z_coin/z_note_lock_tests/withdraw_unsupported.rs` (a real Light-mode coin over a
wallet database and the scripted lightwalletd transport of the note-lock harness). CI cell: `shielded`
(`cargo test -p coins --lib z_coin::`). CRD: chapter 49, R49.27, R49.27a (b) and (c), T49.27a (shielded
rows).

No refusal reads the backend, decodes the destination or selects a note, so the tests make no proof and
need no network. They need the Sapling parameter files only because the harness loads them, and skip with
a message where those are absent (the same arrangement as the other wallet-database tests). Every
assertion compares the discriminant, the HTTP status and the JSON type of `error_data`, never the text.
The requests carry a destination that decodes as no address, so a refusal that wins over the decoder was
decided before it ran.

| Test | CRD | What is tested |
|------|-----|----------------|
| `t49_27a_direct_withdraw_refuses_fee_then_from_with_the_shared_arm` | T49.27a, R49.27a (b), (c) | The direct `withdraw` with `fee`, then with `from`: HTTP 400, `error_type` `UnsupportedError`, string `error_data`. With both, the answer is the one for `fee` (the text is that of the `fee` refusal and differs from the `from` one). The backend is not asked anything; nothing is broadcast, locked or spent |
| `t49_27a_task_route_refuses_fee_and_from_with_the_shared_arm` | T49.27a, R49.27a | `task::withdraw::init` with `fee`, and another with `from`, over a registered coin: the task reaches `status: "Error"` with `details.error_type == "UnsupportedError"` and a string `details.error_data` |

## BC2 requires a counterparty that signs with the suffix

Location: the module `sighash_suffix_gate_tests` of `mm2src/coins/utxo/utxo_tests.rs` (writer, check,
config and logs) and the module `t14_capability_word_tests` of `mm2src/mm2_main/src/ordermatch_tests.rs`
(both roles). They cover CRD chapter 61 §61.5A, tests T10-T13.

| Test | CRD | What is tested |
|------|-----|----------------|
| `t10_a_coin_with_a_suffix_sends_the_first_value_then_the_word_as_fixed_bytes`, `t10_a_zero_suffix_sends_the_bytes_of_the_address_format_alone`, `t10_the_word_is_sent_whatever_the_switch_says`, `t10_the_bc2_and_bc2_segwit_entries_both_send_the_word` | T10, R61.5A.1 | A coin with a non-zero suffix sends its address-format bytes followed by the fixed bytes of {`sighash_suffix`: `true`} for the standard, segwit and cashaddress forms; the first value still decodes to the same format; a zero suffix sends today's bytes; the `BC2` and `BC2-segwit` entries both send the word |
| `t10_orderbook_address_format_is_the_same_with_the_suffix_word` (ordermatch) | T10, R59.4A.3 | The order book reports the same address format with and without the word |
| `t11_with_the_switch_on_the_word_is_accepted`, `t11_with_the_switch_on_everything_without_the_word_is_declined`, `t11_the_decline_names_the_cause`, `t11_with_the_switch_off_the_coin_judges_as_before`, `t11_with_a_zero_suffix_every_input_is_judged_as_before`, `t11_the_first_value_is_tested_before_the_word` | T11, R61.5A.2 | With the switch on, absent, empty, first-value-only, malformed and `false` information is declined and the word is accepted; with the switch off, or with a zero own suffix, every input is judged as before; the first-value test runs first |
| `t12_maker_*`, `t12_taker_*`, `t12_the_declining_coin_is_named_for_the_log_lines_of_the_order_match_layer` (ordermatch) | T12, R61.5A.3 | A maker declines a request and a taker declines a reservation whose information for the requiring coin lacks the word, and accepts one that carries it; the other coin of the pair does not trigger the decline; the declining coin is named |
| `t13_the_switch_is_true_when_absent_and_accepts_both_booleans`, `t13_a_switch_that_is_not_a_boolean_fails_activation_naming_the_key`, `t13_each_decline_writes_a_debug_line_that_names_the_coin_and_the_cause`, `t13_the_warning_comes_at_most_once_an_hour_with_the_count_since_the_previous_one`, `t13_the_warning_is_kept_per_coin_and_a_clock_that_goes_back_does_not_panic` | T13, R61.5A.4, R61.5A.5 | `require_suffix_counterparty` defaults to `true`, a non-boolean fails activation naming the key; one debug line per decline, one warning per hour with the count (injected clock) |

## Recovery of a maker swap with a sent spend of the taker payment (5 tests)

Location: the module `tests` of `mm2src/mm2_main/src/lp_swap/maker_swap.rs`, beside the other
`recover_funds` tests (a `TestCoin` pair with mocked chain searches). CRD: chapter 51, §51.6A, R75 item 2,
R76, T32 items 7-11. All five use one finished record that holds `TakerPaymentSpent` and not
`TakerPaymentSpendConfirmed`. Items 1-6 are covered by existing tests (see the chapter).
Run on 2026-11-07: `cargo test -p mm2_main --lib test_recover_funds` passed (42 tests, these five included).

| Test | CRD | What is tested |
|------|-----|----------------|
| `test_recover_funds_sent_spend_unspent_maker_payment_after_the_lock_is_not_refunded` | T32 item 7, R75 item 2, R76 | The maker payment is unspent and the refund time has passed: the call refuses with the text that the spend was sent and the maker payment belongs to the taker; the refund gate is not asked, no refund is built, and the record is as it was |
| `test_recover_funds_sent_spend_unspent_maker_payment_before_the_lock_gets_the_same_refusal` | T32 item 8, R75 item 2 | The refund time has not passed: the same refusal, not "Too early to refund"; no gate, no refund |
| `test_recover_funds_sent_spend_spent_maker_payment_still_spends_the_unspent_taker_payment` | T32 item 9, R75 item 2 | The maker payment is spent and the taker payment is not: the spend of the taker payment is built (`SpentOtherPayment`), no refund |
| `test_recover_funds_sent_spend_spent_maker_payment_and_spent_taker_payment_builds_nothing` | T32 item 10, R75 item 2 | Both payments spent: the call refuses ("Taker payment was already spent"); no spend and no refund is built |
| `test_recover_funds_sent_spend_refunded_maker_payment_is_refused_as_already_refunded` | T32 item 11, R75 item 2 | The maker payment is refunded on the chain: the call refuses ("Maker payment was already refunded"), not with the sent-spend text; nothing is built |

## EVM swap contract value derived from a 32-byte secret hash (14 tests)

Location: `mm2src/coins/eth/eth_secret_hash_tests.rs` (coin fixtures from
`eth_gas_limit_call_site_tests.rs`; payment building runs against a mock that captures the calldata, so no
node is needed). CRD: chapter 51 R72A, R72B, §51.9.1. The vectors are literals computed outside the code:
SHA-256 of 32 bytes of `0x42`, the RIPEMD-160 of that hash, and the swap id over the time lock and that hash.
Run on 2026-11-08: `cargo test -p coins --lib -- eth::` passed (425 tests, these 14 included).

| Test | CRD | What is tested |
|------|-----|----------------|
| `r72a_helper_keeps_a_20_byte_value_unchanged` | R72A | A 20-byte value comes back as it was |
| `r72a_helper_derives_ripemd160_of_a_32_byte_value` | R72A | A 32-byte value gives the literal RIPEMD-160 value |
| `r72a_helper_32_byte_derivation_equals_the_20_byte_default_of_the_same_secret` | R72A | For one secret, deriving from the 32-byte value equals the 20-byte default |
| `r72a_helper_rejects_other_widths_with_the_shared_text` | R65, R72A | 0, 19, 21 and 33 bytes give the same error text as the UTXO comparator |
| `r72a_eth_payment_embeds_the_derived_value_for_a_32_byte_secret_hash`, `r72a_erc20_payment_embeds_the_derived_value_for_a_32_byte_secret_hash` | R72A | The `ethPayment` and `erc20Payment` calldata carries exactly the derived 20 bytes in the `bytes20` slot |
| `r72a_eth_payment_keeps_a_20_byte_secret_hash_unchanged`, `r72a_erc20_payment_keeps_a_20_byte_secret_hash_unchanged` | R72A | A 20-byte secret hash is embedded as before |
| `r72a_payment_with_a_wrongly_sized_secret_hash_is_not_built` | R72A | A 33-byte value fails before any transaction is sent, for ETH and a token |
| `r72a_swap_id_is_computed_over_the_shared_value` | R72A | The swap id is still SHA-256 over the time lock and the shared value (literal) |
| `r72a_validation_accepts_the_derived_value_for_a_32_byte_secret_hash` | R72A | A slot holding the derived value is accepted |
| `r72a_validation_refuses_the_raw_leading_bytes_and_names_the_derived_value` | R72A | A slot holding the leading 20 bytes of the shared value is refused; the error says "is invalid" and names the derived value as expected |
| `r72a_validation_of_a_20_byte_secret_hash_is_unchanged` | R72A | The 20-byte comparison and error text are as before |
| `r72a_validation_of_a_wrongly_sized_secret_hash_is_an_error` | R72A | A 21-byte shared value is an error naming its width |

## QRC20 swap contract value derived from a 32-byte secret hash (9 tests)

Location: the end of `mm2src/coins/qrc20/qrc20_tests.rs`, using the coin fixture of that file (no node is
needed for these). CRD: chapter 51 R72A, R72B, §51.9.1. The vectors are the literals of the EVM tests above, plus
the SHA-256 of the secret of `test_extract_secret`. QRC20 is covered by unit tests only. The slot comparison of
`validate_payment` is tested through the helper it calls, because the validation itself needs a node. Run on
2026-11-08: `cargo test -p coins --lib -- r72a_qrc20` passed (9 tests).

| Test | CRD | What is tested |
|------|-----|----------------|
| `r72a_qrc20_payment_embeds_the_derived_value_for_a_32_byte_secret_hash` | R72A | The `erc20Payment` call bytes carry exactly the literal derived 20 bytes in the `bytes20` slot |
| `r72a_qrc20_payment_keeps_a_20_byte_secret_hash_unchanged` | R72A | A 20-byte value gives call bytes identical to encoding the raw value |
| `r72a_qrc20_payment_with_a_wrongly_sized_secret_hash_is_not_built` | R72A | 0, 19, 21 and 33 bytes give an invalid-params error naming the width, and no call bytes |
| `r72a_qrc20_swap_id_is_computed_over_the_shared_value` | R72A | The swap id is still SHA-256 over the time lock and the shared value (literal) |
| `r72a_qrc20_validation_accepts_the_derived_value_for_a_32_byte_secret_hash` | R72A | A slot holding the derived value is accepted |
| `r72a_qrc20_validation_refuses_the_raw_leading_bytes_and_names_the_derived_value` | R72A | The leading 20 bytes of the shared value are refused; the error says "is invalid" and names the derived value as expected |
| `r72a_qrc20_validation_of_a_20_byte_secret_hash_is_unchanged` | R72A | The 20-byte comparison is as before |
| `r72a_qrc20_extract_secret_accepts_a_32_byte_secret_hash` | R72A | The secret of a real spend transaction is found when the shared value is the SHA-256 of the secret |
| `r72a_qrc20_extract_secret_refuses_a_wrongly_sized_secret_hash` | R72A | A 33-byte value is an error naming its width |

## Stale-tip warning during a refund wait (15 tests)

Location: the module `tests` of `mm2src/mm2_main/src/lp_swap/stale_tip_watch.rs`. CRD: chapter 51, §51.6B, R79-R81, T33.
Time is faked: the clock moves only when a fake sleep ends, a sleep takes a few polls, and a fake read answers at
once, after one poll, or never, so no test waits. Run on 2026-11-08: all 15 passed.

| Test | CRD | What is tested |
|------|-----|----------------|
| `t33_no_warning_at_59_minutes_one_at_60_naming_60` | T33 item 1, R80 | 59 minutes of the same height give no warning; at 60 minutes one, naming the height and 60 minutes |
| `t33_no_second_warning_before_120_minutes_second_at_120` | T33 item 2, R80 | No second warning before 120 minutes; at 120 minutes a second one naming 120 minutes |
| `t33_height_change_ends_the_stall_and_starts_a_new_clock` | T33 item 3, R80 | A change of height gives no warning; 59 minutes after it none, 60 minutes after it one |
| `t33_height_change_before_the_first_warning_resets_the_clock` | T33 item 3, R80 | A change at 50 minutes means no warning at 109 minutes |
| `t33_first_observation_only_records_the_height` | T33 item 5, R80 | The first observation is a baseline; the stall counts from it |
| `t33_failed_reads_are_skipped_and_do_not_reset_the_clock` | T33 item 4, R79 | Failed reads give no warning and do not reset the clock; the warning still comes at 60 minutes |
| `t33_a_wait_with_only_failed_reads_gives_no_warning` | T33 item 4, R79 | A wait whose reads all fail writes no warning |
| `t33_a_read_that_does_not_finish_before_the_sleep_ends_is_skipped` | T33 item 4, R79 | Reads that never finish are dropped when their sleep ends: no warning from them, the clock is not reset (the warning still comes at 60 minutes), the sleeps add up to the wait |
| `t33_pending_reads_do_not_extend_the_wait` | T33 item 7, R79 | With every read pending, no warning, slices of at most 300 s, and the slices add up to the wait exactly |
| `t33_a_read_that_finishes_during_the_sleep_is_used` | T33 item 4, R79 | A read that answers before its sleep ends is observed (warning at 60 minutes) and the sleep is still awaited to its end |
| `t33_warning_text_has_the_ticker_and_no_address` | T33 item 6, R80 | The text contains the ticker, the height and the minutes, and no `http` or `://` |
| `t33_fixed_height_over_two_hours_warns_exactly_twice_in_slices_of_five_minutes` | T33 item 7, R79, R80 | A wait of 7500 s with a height that never changes: slices of at most 300 s that add up to 7500 s, exactly two warnings (60 and 120 minutes) |
| `t33_a_height_that_moves_every_half_hour_never_warns` | T33 items 3 and 7 | A height that moves every 30 minutes never warns; the sleeps add up to 7200 s |
| `t33_sleep_total_is_kept_for_waits_that_are_not_multiples_of_a_slice` | T33 item 7, R79 | Waits of 1000 s, 120 s and 0 s sleep exactly that long (a zero wait still sleeps once) |
| `t33_short_steps_read_the_height_once_per_sample_period` | T33 item 7, R79 | A wait that the caller cuts into steps of 5 s reads the coin once per 5 minutes, not at each step |

## Tendermint swap flow (50 tests)

Location: the module `tendermint_swap_flow_tests` of `mm2src/coins/tendermint/tendermint_swap_flow_tests.rs`
(native builds only). CRD: chapter 18, §18.3, §18.5 and §18.11. Run with
`cargo test -p coins --lib tendermint::tendermint_swap_flow_tests`.

Run on 2026-11-08: `cargo test -p coins --lib tendermint::` passed (67 tests, the 50 of this module included);
the swap-machine tests `cargo test -p mm2_main --lib -- lp_swap::maker_swap lp_swap::taker_swap` passed (138 tests).

The tests drive a `TendermintCoin` (an Iris coin with the `iaa` prefix, and a Nucleus coin with the `nuc`
prefix where the dialect matters) against a throwaway server on a loopback port. The server answers the
JSON-RPC requests the code under test makes (`health`, `abci_info`, `abci_query`, `tx_search`) and records them,
so the tests also assert what was asked. The keys are fixed throwaway keys.

Tendermint swaps stay off (`TENDERMINT_SWAPS_AVAILABLE` is `false`); the corrections tested here make the code
correct for when they are turned on (CRD ch.18 §18.11).

A test named `*_today_*` records a behavior that differs from the chapter and waits for a maintainer decision.
It carries a comment `// CRD ch.18: ... (gap G<n>)`, and the later fix flips that one assertion. The open gaps:

| Gap | What differs from the chapter today |
|-----|-------------------------------------|
| G1 | The payment check does not compare the time lock of the HTLC with the negotiated one (R18.11.5, D18.11.1 and D18.11.2 open) |
| G3 | A refunded HTLC is reported with an empty transaction, and the two refund methods answer a plain error, instead of an outcome the swap state machine recognizes as "the chain refunds by itself" (R18.11.17, D18.11.3 open) |

Two gaps of the first version of this table are closed. G5 (`check_if_my_payment_sent` matched by hash lock
only) was fixed by R18.11.22: the lookup is by the HTLC id, which covers the receiver and the amount. G6
(`wait_for_confirmations` ignores the confirmation count) was never a gap: R18.11.18 says a Tendermint coin
requires zero confirmations and ignores the count.

| Test | CRD | What is tested |
|------|-----|----------------|
| `tm_flow_coin_account_is_the_hash160_of_the_public_key` | §18.2.2 | The account of the test coin is the bech32 of the hash160 of the public key, with the dialect's prefix (checks the fixture) |
| `tm_flow_create_htlc_message_fields_iris`, `tm_flow_create_htlc_message_fields_nucleus` | §18.3.2 | The create message carries the sender, the receiver, one coin of the amount, the hash lock as the lower-case hex of the 32-byte secret hash, timestamp 0 and the time lock given; the Iris extras are empty and `transfer` is false; the type URL is the dialect's; the id returned with it is the HTLC id |
| `tm_flow_time_lock_blocks_follow_the_duration_within_the_chain_bounds` | §18.3.2 | The time lock in blocks is the duration divided by the block time, clamped to 50 .. 34560 |
| `tm_flow_claim_htlc_message_fields_iris`, `tm_flow_claim_htlc_message_fields_nucleus` | §18.3.2 | The claim message carries the HTLC id, the claimer and the secret as hex; the type URL is the dialect's |
| `tm_flow_htlc_id_is_the_upper_case_sha256_of_hash_from_to_and_amount` | §18.3.1 | The id is the upper-case SHA-256 of the secret hash, the sender, the receiver and `<amount><denom>`, computed independently in the test; another amount or receiver gives another id |
| `tm_flow_validate_payment_accepts_a_matching_payment` | §18.5 | A matching payment is accepted as a maker payment and as a taker payment, on both dialects, whatever the letter case of the hash lock |
| `tm_flow_validate_payment_rejects_a_wrong_hash_lock`, `..._a_wrong_sender`, `..._a_wrong_receiver`, `..._a_wrong_amount_or_denom` | §18.5 | A payment with another hash lock, sender, receiver, amount or denom is refused, with the matching reason |
| `tm_flow_validate_payment_today_accepts_any_time_lock` | §18.5, gap G1 | A payment with a time lock of 1 block, of the chain maximum or beyond it is accepted |
| `tm_flow_check_if_my_payment_sent_finds_the_payment_by_its_id` | R18.11.22 | Open, completed and refunded HTLCs, on both dialects: the module is asked for the id, then `create_htlc.id='<id>'` is searched (newest first, one result) and the create transaction is returned |
| `tm_flow_check_if_my_payment_sent_is_none_when_the_module_has_no_htlc` | R18.11.22 | No HTLC under the id gives `None` and no transaction search |
| `tm_flow_check_if_my_payment_sent_does_not_match_another_receiver_or_amount` | R18.11.22 | A payment with the same hash lock to another receiver, or of another amount, is not found (its id differs); the payment sent is found by its receiver and amount |
| `tm_flow_check_if_my_payment_sent_fails_on_a_non_zero_result_code` | R18.11.22 | A create transaction with a non-zero result code is an error naming the id |
| `tm_flow_check_if_my_payment_sent_fails_when_the_module_has_the_htlc_and_the_search_finds_nothing` | R18.11.22 | The module has the HTLC and the search finds nothing: an error naming the id |
| `tm_flow_check_if_my_payment_sent_compares_the_hash_lock_without_regard_to_the_letter_case` | R18.11.3, R18.11.22 | The recorded and the message hash locks are compared case-insensitively, in either direction |
| `tm_flow_check_if_my_payment_sent_refuses_a_create_transaction_with_another_hash_lock` | R18.11.22 | A create transaction whose hash lock differs from the recorded one is an error |
| `tm_flow_check_if_my_payment_sent_of_a_token_uses_the_token_denom_and_decimals` | R18.11.22, R18.11.11 | A token's payment is looked up with the token's denom and decimals, the platform's account and node |
| `tm_flow_check_if_my_payment_sent_without_the_amount_is_an_error` | R18.11.22 | The old method (no amount) answers an error on the coin and the token and asks the node nothing |
| `tm_flow_search_for_swap_tx_spend_of_an_open_htlc_is_none` | §18.3.3, §18.5 | An open HTLC gives `None` for both the "my" and the "other" search; the chain is asked for the HTLC under its id on the Iris query path |
| `tm_flow_search_for_swap_tx_spend_of_a_completed_htlc_is_the_claim` | §18.3.3, §18.5 | A completed HTLC gives `Spent` with the claim transaction, found by `claim_htlc.id='<id>'` (oldest first); the secret read from it is the one claimed |
| `tm_flow_search_for_swap_tx_spend_today_gives_an_empty_tx_for_a_refunded_htlc` | §18.3.1, gap G3 | A refunded HTLC gives `Refunded` with an empty transaction |
| `tm_flow_search_for_swap_tx_spend_of_a_nucleus_htlc_uses_the_nucleus_query` | §18.3.3 | A Nucleus coin asks the Nucleus query path and reports the open, completed and refunded states the same way |
| `tm_flow_refund_methods_today_answer_the_auto_refund_error` | §18.3.1, gap G3 | Both refund methods answer the plain error "Tendermint HTLCs auto-refund on chain; no broadcast required." and ask the node nothing |
| `tm_flow_extract_secret_returns_the_secret_of_a_claim_message` | §18.5 | The secret is read from the claim message of both dialects; bytes that are not a transaction give an error |
| `tm_flow_extract_secret_requires_exactly_32_bytes` | R18.11.14 | A secret of 1, 31, 33 or 64 bytes is an error naming the length, for the coin and the token; 32 bytes give the secret |
| `tm_flow_wait_for_confirmations_passes_on_an_ok_status_whatever_the_count` | R18.11.18 | A transaction with an OK status passes at once with 100 confirmations asked for (zero are required); the node is asked for it by the upper-case SHA-256 hex of its bytes |
| `tm_flow_wait_for_confirmations_fails_on_an_error_status` | R18.11.18 | A transaction with an error status fails the wait, naming the code |
| `tm_flow_validate_fee_accepts_the_standard_fee` | R18.11.21 | One `MsgSend` from the taker to the fee address with exactly the fee coin, the uuid memo and an equal node copy is accepted on both dialects; the node is asked by the upper-case SHA-256 hex of the bytes |
| `tm_flow_validate_fee_refuses_a_wrong_recipient`, `..._a_wrong_sender`, `..._two_coins_a_wrong_amount_and_a_wrong_denom`, `..._two_messages`, `..._a_wrong_memo` | R18.11.21 | Each wrong standard fee is refused with its reason, before the node is asked for a copy |
| `tm_flow_validate_fee_refuses_a_message_of_the_other_form` | R18.11.21 | A `MsgMultiSend` for a standard descriptor, and a `MsgSend` for a burn descriptor, are refused |
| `tm_flow_validate_fee_refuses_no_fee_descriptor` | R18.11.21 | `DexFee::NoFee` is refused on the validating side |
| `tm_flow_validate_fee_accepts_the_burn_fee` | R18.11.21 | One `MsgMultiSend` with one input of the taker and the fee output then the burn output, each with exactly its coin, is accepted on both dialects |
| `tm_flow_validate_fee_refuses_a_wrong_burn_fee` | R18.11.21 | A wrong burn amount, burn address, number of outputs or inputs, fee output (amount, coins, address, order) or sender is refused with its reason |
| `tm_flow_validate_fee_refuses_a_node_copy_that_differs`, `..._a_transaction_the_node_does_not_have` | R18.11.21 | A node copy that differs from the supplied bytes, and a missing copy, are validation errors naming the hash |
| `tm_flow_token_validate_fee_uses_the_token_denom` | R18.11.21, R18.11.11 | A token's fee is checked in the token's denom (standard and burn form) |
| `tm_flow_activation_refuses_an_avg_blocktime_of_zero`, `tm_flow_conf_refuses_an_avg_blocktime_of_zero_and_names_the_key` | R18.11.1 | `avg_blocktime` 0 is refused at activation with an error naming the key; positive values are accepted, the earlier refusals stay |
| `tm_flow_token_trade_fees_carry_the_platform_ticker_and_decimals` | R18.11.10, R18.11.11 | A token's sender-side trade fee and fee to send the dex fee carry the platform ticker and the platform-decimals amount; the receiver-side fee and the platform coin's own fee are the same |
| `tm_flow_a_chain_without_an_htlc_module_is_wallet_only` | R18.11.23 | A prefix other than `iaa` and `nuc` is wallet-only with the build switch on, whatever the coins file says; `iaa` and `nuc` follow the flag |
| `tm_flow_the_wallet_only_answer_of_the_coin_and_token_follows_the_prefix` | R18.11.23 | The coin and the token answer through the predicate (wallet-only while the build switch is off) |
| `tm_flow_ibc_routing_is_never_advertised_or_accepted` | R18.11.25 | The coin and the token advertise no protocol information and decline a match whose counterparty information is not empty |

Not covered, because they need the node to answer the account, simulation and broadcast requests: the
broadcast of the create, claim and fee transactions (`send_maker_payment`, `send_taker_payment`, the two
spend methods, `send_taker_fee`), and `wait_for_tx_spend`.
