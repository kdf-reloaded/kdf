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

## Pre-existing test compilation fix

The `EthCoinImpl` struct gained a `derivation_method` field that was not present
in 9 existing test constructors, causing the entire `eth_tests` module to fail
to compile. All 9 sites now set
`derivation_method: DerivationMethod::Iguana(my_addr)`.
