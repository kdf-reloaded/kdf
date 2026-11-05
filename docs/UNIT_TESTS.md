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

## ARRR withdrawals to a Pirate1 address (24 tests)

Locations: `mm2src/coins/z_coin/z_pirate1_tests.rs` (no coin needed),
`mm2src/coins/z_coin/z_note_lock_tests/pirate1_withdraw.rs` (a real Light-mode coin over a wallet
database and the scripted lightwalletd transport of the note-lock harness). CI cell: `shielded`
(`cargo test -p coins --lib z_coin::`). CRD: chapter 39, §39.6.5 (R39.6.5a to R39.6.5h, T39.6.5b to
T39.6.5e) and T39.5A.a.

The tests of the second file make real Sapling spend proofs and a real Halo2 Ironwood proof; they need
the Sapling parameter files and skip with a message where those are absent (the same arrangement as
the other wallet-database tests). The first Ironwood build of a test binary derives the Ironwood
proving key (once per binary, inside the builder) and takes about 100 seconds in a debug build when it
runs alone (96.5 s measured, 120 s with the other tests of this section, 321 s inside the whole
`z_coin::` cell, where the tests share the cores); the verifying key takes another 34 to 66 seconds,
also once per binary. Once the keys exist a built Ironwood bundle verifies in about 0.15 seconds. The test vectors are generated from a real
Orchard-type address (key `[7; 32]`); the Ironwood anchor of the mainnet withdrawal fixture is the
root of the empty tree, which is recorded in `t39_6_5d_the_mainnet_withdrawal_anchors_at_the_empty_tree_root`.

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
| `t39_6_5d_a_32_byte_value_is_the_root` | T39.6.5d | A 32-byte `ironwoodTree` value is the root |
| `t39_6_5d_a_legacy_serialization_yields_the_root_computed_from_it` | T39.6.5d | Trees of 1 to 13 leaves: the root computed from the legacy serialization |
| `t39_6_5d_an_unusable_value_is_refused` | T39.6.5d | Empty, blank, non-hex, all-zero, non-canonical, truncated, trailing-byte and one-byte values are refused with their cause |
| `t39_6_5d_the_mainnet_withdrawal_anchors_at_the_empty_tree_root` | T39.6.5d | The Ironwood anchor of the mainnet withdrawal fixture is the empty-tree root; a backend that reports it is used as reported |
| `t39_6_5c_a_target_height_below_the_activation_height_is_refused_before_note_selection` | T39.6.5c, R39.6.5d | The gate: activation height, unknown height, native mode, with the coin and the reason named |
| `t39_6_5g_the_fee_is_read_from_the_value_balances_of_the_bytes` | R39.6.5g | The mainnet withdrawal: Sapling plus Ironwood value balance, less transparent outputs |
| `t39_6_5g_a_transaction_with_transparent_inputs_has_no_derivable_fee` | R39.6.5g | The recorded HTLC refund (transparent input) has no derivable fee |
| `t39_6_5c_a_withdrawal_to_a_pirate1_address_builds_the_version_6_transaction` | T39.6.5c | Shape, fee, value balances, anchor, recipient output and memo, Sapling change to the wallet, byte-identical read-back, Ironwood proof verifies |
| `t39_6_5c_a_withdrawal_to_a_sapling_address_is_unchanged` | T39.6.5c, R39.6.5f | The configured fee, no Ironwood bundle, no tree-state read; `max` with the configured fee |
| `t39_6_5c_max_sends_the_spendable_balance_less_the_fixed_fee` | T39.6.5c | `max: true` sends the balance less 10,000 base units, no change |
| `t39_6_5c_a_target_height_below_the_activation_height_is_refused` | T39.6.5c | Refused before the anchor is read or a note selected, naming the coin and the reason |
| `t39_6_5b_validateaddress_and_withdraw_answer_alike_on_a_coin` | T39.6.5b, R39.6.5c | `validateaddress` and the withdrawal on a real coin: accepted, refused, no pool |
| `t39_6_5d_the_anchor_comes_from_the_backend_or_the_build_is_refused` | T39.6.5d | The anchor read at the Sapling anchor height; empty, zero, undecodable, wrong-height and missing answers refuse the build with the coin and the backend named, and nothing is broadcast |
| `t39_6_5e_a_submitted_pirate1_withdrawal_is_recorded` | T39.6.5e, R39.6.5g | Through `send_raw_tx`: the fee read from the bytes (10,000), the note excluded from the next selection, the unmined history row with the final amounts that survives the scan, the change as non-spendable balance, then spendable after the scan. Needs the vendored `zcash_client_sqlite`, which skips the Orchard and Ironwood nullifier marking instead of panicking |
| `t39_6_5a_the_recorder_stores_the_mainnet_withdrawal_with_its_derived_fee` | T39.6.5a, R39.6.5g | The recorded mainnet withdrawal (Sapling spend, two Ironwood actions) goes through the recorder into a real wallet database: stored with the derived fee 10,000; none of the wallet's notes are marked spent |
