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

# Bounds on a value received from a peer (CRD ch.28 R16a, T4a)
cargo test -p common --lib msgpack_bounds::
cargo test -p mm2_p2p --features application --lib t4a_
cargo test -p coins --lib t4a_
cargo test -p mm2_main --lib t4a_
```

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
R39.8.9d2, R39.8.9d3, T39.8.9d1, T39.8.9d2, T39.8.9d3.

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
| `t39_8_9d3_a_payment_lists_the_script_address_and_nothing_for_the_data_carrier` | T39.8.9d3 (a) | A real `z_send_htlc` payment (two transparent outputs: the script and the data carrier): the row lists the script's pay-to-script-hash address in `to`, which parses as such with the coin's prefix and the script's hash, beside the own address for the change, and nothing else; unmined, after a restart, mined, and after another restart |
| `t39_8_9d3_a_fee_payment_lists_the_fee_address_in_the_sapling_form` | T39.8.9d3 (b) | A real `z_send_dex_fee` payment: the row lists the fee address in `to`, encoded with the coin's Sapling prefix and different from the own address, beside the own address for the change; unmined, mined, and after each restart |
| `t39_8_9d3_a_spend_and_a_refund_list_the_script_address_as_the_sender` | T39.8.9d3 (c) | A real spend (with the secret) and a real refund of the same payment built by `z_p2sh_spend`: neither has a row before it is mined (R39.8.9i); each mined row lists the script address in `from` and the own address in `to`; the same after a restart |
| `t39_8_9d3_a_failing_record_leaves_the_swap_step_whole_and_the_row_without_the_address` | T39.8.9d3, R39.8.9d2 (d) | The record file is replaced by a directory so every write fails: the payment and the spend are still built, broadcast and returned, and their rows list the own address only |

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
