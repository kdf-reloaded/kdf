# Disabled / Ignored Tests

Inventory and remediation status of tests currently marked `#[ignore]` in the
workspace: why each is disabled, the effort to re-enable, and what coverage is
lost while it stays off.

Grouped by root cause. Line numbers drift; search by test name.

## Recently re-enabled (no longer `#[ignore]`)

These were disabled and have since been fixed / converted to deterministic or
env-gated tests:

- `test_limit_reached_true` (`mm2_main` order-requests rate limiter) — was flaky
  on macOS (sleep-based timing); rewritten to be deterministic.
- `test_get_fee_to_send_taker_fee_insufficient_balance` (`coins/eth`) — the code
  now maps an insufficient-balance `estimate_gas` revert to
  `NotSufficientBalance` (balance check, provider-agnostic); test runs offline
  via mocked `estimate_gas` + `my_balance`.
- `test_get_sender_trade_fee_dynamic_tx_fee` (`coins/utxo`) — dynamic-fee trade
  preimage made consistent for `UpperBound` vs `Exact`; test runs offline via
  mocked `get_tx_fee` + `get_unspent_ordered_list`.
- `polygon_check_if_my_payment_sent` (`coins/eth`) — reworked into a bounded,
  free-tier-friendly test; runs against a live RPC via `POLYGON_RPC_URL` and
  skips when unset (external-network-tests CI job).
- `send_and_refund_eth_payment`, `send_and_refund_erc20_payment` (`coins/eth`) —
  moved to `eth_swap_dev_tests` and rewritten to run against a throwaway
  `geth --dev` chain with a clean-room `EtomicSwap` HTLC + ERC20 deployed
  locally (see `for_tests/*.sol`). They exercise the real payment -> refund
  flow and assert the on-chain state transition. They skip themselves when the
  `geth` binary is not on `PATH`, so the default offline suite stays green; the
  `evm-geth-tests` CI job installs a pinned `geth` and sets `KDF_REQUIRE_GETH=1`,
  which turns that skip into a failure.
- `validate_dex_fee_invalid_sender_eth`, `validate_dex_fee_invalid_sender_erc`,
  `validate_dex_fee_eth_confirmed_before_min_block`,
  `validate_dex_fee_erc_confirmed_before_min_block`, `test_nonce_lock`,
  `test_nonce_several_urls` (`coins/eth/eth_tests.rs`, now in
  `eth_tests::scripted_node_ports`) — were not `#[ignore]`d but permanently failing: they
  were pinned to a decommissioned chain, a hosted project key and two RPC hosts that no
  longer answer, and no CI filter selected them, so the rot went unnoticed. They now run
  the same real functions (`validate_fee`, `sign_and_send_transaction_impl`,
  `get_addr_nonce`) against the scripted local node, with no network and no key. The
  fee tests sign an equivalent fee transaction instead of fetching two historical
  mainnet ones, and now also pin the `min_block` boundary and the accepting case;
  `test_nonce_lock` asserts the nodes saw nonces 0..5 once each; `test_nonce_several_urls`
  checks nonce agreement across a lagging, a leading and an unreachable node instead of
  broadcasting on a testnet. Still selected by no CI filter (see
  `docs/plans/evm-test-infrastructure.md`).
- `test_get_raw_transaction` (`mm2_main`) — its inline hosted-project key and Ropsten
  node are removed. The ETH raw-hex and "hash does not exist" checks were dropped (no
  node to serve them); the hash-format checks now run against DOC. Lost coverage: ETH
  `get_raw_transaction` retrieval.

## Group 1 — Genuine code failures worth fixing (inherited from the 2022 fork)

_None outstanding._ The fee-error mapping, dynamic-fee preimage consistency,
rate-limiter flakiness, and the ETH/ERC20 refund-path tests have all been fixed
and re-enabled (see above).

## Group 2 — Dead external endpoints

_None outstanding._ The last entry, `test_nonce_several_urls`, was ported to a scripted
node (see above).

## Group 3 — Live-infra integration tests (code is fine; ignored so offline `cargo test` stays green)

Run best-effort in the `external-network-tests` CI job.

- `test_withdraw_and_send` (`mm2_main/src/mm2_tests.rs`) — live cipig DOC/MARTY
  electrums + ETH dev chain.
- `test_withdraw_legacy` (`mm2_main/src/mm2_tests.rs`) — live cipig DOC/MARTY
  electrums.
- `test_electrum_tx_history` (`mm2_main/src/mm2_tests.rs`) — external electrum.
- `test_tx_details_kmd_rewards`, `test_tx_details_kmd_rewards_claimed_by_other`
  (`coins/utxo/utxo_tests.rs`) — live KMD electrum + specific historical txs.
- `firo_lelantus_tx_details` (`coins/utxo/utxo_tests.rs`) — stale. It fetched two Firo
  transactions from a live ElectrumX server and expected their fee from the `nFees` field of
  their Lelantus inputs. The server (ElectrumX 1.19.0) no longer sends `nFees` for these two
  transactions, so the fee path cannot be checked against them. The fee path of a Lelantus spend
  is covered offline by `firo_lelantus_tx_details_offline`, which runs `tx_details_by_hash` on
  a verbose answer of the old shape with the node call mocked. `firo_lelantus_tx` (the plain
  fetch of the verbose answer) stays, behind the feature below. Lost coverage: the check that a
  live Firo server still answers with `nFees`. Delete candidate.

Re-enable path: fresh electrum URLs + a network-gated CI job. Flaky by nature.

The other tests that need public servers are not in this list. They are not `#[ignore]` in the
source: each carries `cfg_attr(not(feature = "live-network-tests"), ignore = ...)`, so a plain
`cargo test` ignores them and `--features live-network-tests` runs them. See
[UNIT_TESTS.md](UNIT_TESTS.md#tests-that-need-public-servers-live-network-tests). The tests of
this group keep a plain `#[ignore]` because CI runs them on purpose in the
`external-network-tests` job with `--include-ignored`, or because they no longer pass.

## Group 4 — Docker swap tests (need a QTUM docker node)

- `segwit_address_in_the_orderbook`, `test_trade_qrc20_utxo`,
  `test_trade_utxo_qrc20` (`mm2_main/src/docker_tests/qrc20_tests.rs`) —
  QRC20/QTUM docker swaps. Effort: low to med (the fixture is hardened, see
  below; re-enable once docker runs show it stable). Lost coverage: QRC20
  atomic-swap regressions.

  The QTUM fixture (`docker_tests/docker_tests_common.rs`, `docker_tests/qrc20_tests.rs`)
  now mines a block on the regtest node when a test waits for a confirmation, instead
  of waiting for the node's own timer; mines while it waits for the fee estimator;
  waits until the node returns a new contract's code before the first call to it; and
  waits at least 90 s (was 30 s) for a QTUM-node confirmation or fee estimate. These
  are aimed at the intermittent `docker-tests` failures seen since 2026-10-30:
  confirmation and node timeouts, a missing fee estimate, and `sendtocontract` answering
  "contract address does not exist" for the QICK token. Whether they cure those failures
  shows only in CI runs. The tests stay ignored until a CI run shows them green.

  The tests' own confirmation waits in `docker_tests/qrc20_tests.rs` (they used a fixed
  40 s wait on the node's own timer) now mine a block first and wait up to 90 s; whether
  that removes the intermittent failures shows only in repeated CI runs.

  Nine of those tests paid to the shared QRC20 swap contract with a fixed secret (or, for
  three of them, a fixed secret hash) and a time lock of now minus 200 s: the three
  `test_search_for_swap_tx_spend_*` tests, `test_taker_spends_maker_payment`,
  `test_maker_spends_taker_payment`, `test_wait_for_tx_spend`, `test_maker_refunds_payment`,
  `test_taker_refunds_payment` and `test_check_if_my_payment_sent`. The QRC20 swap id is
  derived from the time lock and the secret hash. Each of the nine now draws its own random
  secret (or secret hash) per run.

The tQTUM tests (`coins/utxo/utxo_tests.rs`, `coins/qrc20/qrc20_tests.rs`,
`mm2_main/src/mm2_tests.rs`) are not disabled. They need the one public Qtum testnet
Electrum server, `s6.electrum.qtum.site:51001`, so like the live-infra tests of Group 3
they can fail when that server is down or slow, without any fault in the code. See [UNIT_TESTS.md](UNIT_TESTS.md#tqtum-tests-and-their-electrum-server).

## Group 5 — Hardware / WIP / delete candidates

- `emulator_trezor_withdraw_pin_user_action` (`coins/eth/eth_trezor_emulator_tests.rs`,
  T50.5) — needs a DebugLink PIN-matrix mapping; other Trezor-emulator tests
  cover the non-PIN paths. **Keep ignored** (high effort, low marginal value).
- `test_open_channel` (`mm2_main/src/mm2_tests/lightning_tests.rs`) — needs a
  two-node Lightning regtest; native Lightning is WIP.
- `test_one_unavailable_electrum_proto_version` (`coins/utxo/utxo_tests.rs`) —
  only exercised deprecated electrum protocol v1.2 negotiation. **Delete
  candidate** (no coverage value).
- `test_spam_doc` (`coins/utxo/utxo_tests.rs`) — manual repro, not an assertion.
  **Delete candidate.**

## Group 6 — Family switched off for the beta (re-enable when the family returns)

- `test_solana_and_spl_balance_enable_spl_v2`, `test_sign_verify_message_solana`,
  `test_sign_verify_message_spl` (`mm2_main/src/docker_tests/solana_tests.rs`) —
  they activate Solana through `enable_solana_with_tokens` / `enable_spl`, which
  refuse while `SOLANA_ACTIVATION_AVAILABLE` (`coins/lp_coins_ops.rs`) is `false`
  (Solana is postponed for the current beta). Effort to re-enable: flip the
  constant to `true` and remove the three `#[ignore]` attributes; they need the
  Solana docker node (`solana_supplied_node`). Lost coverage while off:
  Solana/SPL activation, balances and message signing against a live node.
  The refusal itself is covered by offline tests in `rpc/dispatcher/dispatcher.rs`
  and `lp_commands_legacy.rs`.

> Note: `coins/eth/abi_golden_tests.rs` mentions `#[ignore]` only in a doc
> comment (pending an ethabi 17 upgrade); those golden tests are **active**, not
> disabled.
