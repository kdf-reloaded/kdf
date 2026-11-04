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

## Pre-existing test compilation fix

The `EthCoinImpl` struct gained a `derivation_method` field that was not present
in 9 existing test constructors, causing the entire `eth_tests` module to fail
to compile. All 9 sites now set
`derivation_method: DerivationMethod::Iguana(my_addr)`.
