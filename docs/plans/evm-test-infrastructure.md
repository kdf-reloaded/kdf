# EVM test infrastructure: a real local chain as the default

**Status:** planning (design pass not started; two tests ported 2026-10-16/17; `geth --dev` tests and the scripted-node ports wired into CI 2026-10-24) — **Last reviewed:** 2026-10-24

> **Context:** this document records what already exists, what has been decided, and what
> has been excluded. It deliberately contains **no architecture** — designing the next
> generation is the first task of a dedicated session, and this is that session's input.

## Why this exists

EVM tests in this workspace have been rotting against external endpoints for years,
and nobody noticed because none of them sits inside a CI test filter. As of
2026-10-16 the known damage is:

| test | depends on | state |
| --- | --- | --- |
| `test_search_for_swap_tx_spend_was_refunded` | Ropsten via Infura | ported 2026-10-16 to a scripted node |
| `test_search_for_swap_tx_spend_was_spent` | Ropsten via Infura | ported 2026-10-16 to a scripted node |
| `validate_dex_fee_erc_confirmed_before_min_block` | `eth1.cipig.net:8555` | fails, unported |
| `validate_dex_fee_invalid_sender_erc` | `eth1.cipig.net:8555` | fails, unported |
| `test_nonce_lock` | `195.201.0.6:8565` | fails, unported |
| `test_nonce_several_urls` | Ropsten + ethgasstation.info | `#[ignore]`d, see `DISABLED_TESTS.md` group 2 |

Ropsten was decommissioned. `ethgasstation.info` is gone. The two Ropsten tests
asserted a frozen historical fact on a chain that no longer exists, so no provider
could have rescued them — the defect was the pinning, not the endpoint.

An Infura project key is also inline at **four** places: three in `eth_tests.rs` (lines 338,
1263, 1341) and one in `mm2_main/src/mm2_tests.rs:4757`. `git log -S` dates it to
**2019-07-01**, so it arrived with the original fork and has been public in a public
repository for roughly seven years. It is not ours to rotate, but it should not remain in
our tree; all four occurrences sit in tests that need porting anyway.

## What already exists — read this before designing anything

Three tiers of EVM test infrastructure are already in the tree. The next generation
is mostly a matter of adopting and wiring what is here, not building from nothing.

### 1. A real local chain, already working — and never run

`mm2src/mm2_test_helpers/src/geth_dev.rs` starts a throwaway `geth --dev` node and
tears it down on drop. It carries:

- `ETOMIC_SWAP_BYTECODE` — the clean-room `EtomicSwap` v1 HTLC contract
  (`for_tests/EtomicSwap.sol`)
- `TEST_ERC20_BYTECODE` — a minimal ERC20 minting its supply to the deployer
  (`for_tests/TestErc20.sol`)
- `GethDev::start/rpc/wait_receipt/deploy/fund_eth/send_call`

`mm2src/coins/eth/eth_swap_dev_tests.rs` uses it for the real v1 ETH and ERC20
payment→refund flows and asserts the on-chain state transition. One test in
`mm2_main/src/mm2_tests.rs` uses it too.

**CI status (updated 2026-10-24).** Until this date `geth` was installed in no workflow and
no CI filter reached `eth_swap_dev_tests`, so EVM HTLC coverage on a real chain was nil.
Now the `evm-geth-tests` job in `test.yml` installs a pinned, checksum-verified `geth`
(1.17.2) and runs `cargo test -p coins --lib eth::eth_swap_dev_tests::`, and it is in
`tests-passed`'s `needs:`. Because `GethDev::start()` returns `None` when geth is missing
and the tests then *pass*, the job sets `KDF_REQUIRE_GETH=1`, which makes the harness panic
instead (see `geth_dev::REQUIRE_GETH_ENV`), and it asserts that exactly three tests ran.
Any job copied from it must keep that variable. The fourth user,
`test_withdraw_and_send` (`mm2_main/src/mm2_tests.rs`), is deliberately not in that job: it
is `#[ignore]`d because its DOC/MARTY half needs live cipig electrums, so it stays in
`external-network-tests` (`continue-on-error`), where `KDF_REQUIRE_GETH` is not set and it
still skips when geth is absent. `scripted_node_ports` needs no binary and now runs as the
`eth-scripted-node` leg of the `unit-tests` matrix.

### 2. The env-gated live-RPC pattern, with the free-tier problem already solved

`polygon_check_if_my_payment_sent` (`coins/eth/eth_tests.rs`) reads `POLYGON_RPC_URL`
and **skips itself** when unset, so it is a no-op in the offline suite and runs only
in the `external-network-tests` job where the secret is provided. Introduced
2026-07-08 (`d61f67146`, `ca22cf82b`, `37e59f21b`); catalogued in
`docs/TEST_ENV_VARS.md`.

It is engineered for free tiers, and the technique is the part worth keeping:

- `logs_block_range: 5` in the test's coin config, so the very first `eth_getLogs`
  chunk is tiny and stays inside free-tier range limits (Alchemy and Ankr reject wide
  ranges).
- A fixed historical block (`PAYMENT_BLOCK = 22_185_152`, Polygon, Dec 2021) chosen so
  the event is found in that first bounded chunk, with no crawl toward chain head.
- `from_block` is a fixed constant, deliberately **not** derived from the coin's own
  `current_block()` — the method under test — so the fixture stays independent of the
  code it exercises.
- The history is immutable, so the fixture cannot go stale.

A free-tier **archive** RPC is sufficient for it: no funds, no writes. That is why a
key is needed at all, and why the URL lives in a CI secret rather than in source.

### 3. A scripted local JSON-RPC node

`scripted_evm_node` (added 2026-10-16 on `investigate/swap-tx-format`) spawns a
loopback JSON-RPC server from a `method, params -> result` closure; any unscripted
method panics inside the node. It is shared by `swap_send_path_wire_form` (asserts the
broadcast wire form per swap gas-fee policy) and `search_for_swap_tx_spend_scripted`
(the two ported tests). It needs no binary, no network and no key.

### 4. The inventory

`docs/DISABLED_TESTS.md` groups disabled tests by root cause and records what coverage
is lost while each stays off. Keep it current — it is the only place the picture is
assembled.

## Measured facts about free public endpoints (2026-10-16)

Probed directly, not recalled:

| endpoint | result |
| --- | --- |
| `ethereum-rpc.publicnode.com` | works, keyless |
| `ethereum-sepolia-rpc.publicnode.com` | works, keyless |
| `1rpc.io/eth` | works, keyless |
| `eth.llamarpc.com` | TLS error 525 |
| `cloudflare-eth.com` | refuses: "Cannot fulfill request" |
| `polygon-rpc.com` | refuses: "API key disabled, tenant disabled" |

Keyless endpoints serve **old block headers**, but refuse archive requests:
`eth_getLogs` over a historical range and state at an old block both return
*"Archive requests require a personal token"*. A log scan from a historical height is
exactly what `search_for_swap_tx_spend` and `check_if_my_payment_sent` do, so keyless
endpoints cannot serve that class of test. Two of the six probed are already dead or
gated — the same decay that killed the Ropsten tests, on a slower clock.

## Decided (do not relitigate without a reason)

- **Unit tests are deterministic, offline and may gate merges.** Scripted node or
  mocks. No network, no keys, no binaries.
- **Live-RPC tests are env-gated, self-skipping and never gate merges.** They run
  best-effort in `external-network-tests`, which is `continue-on-error: true` by
  design.
- **A required status check must be** deterministic given the commit, independent of
  third-party availability, incapable of legitimately skipping, and stable in name.
  Anything failing one of those blocks merges for reasons unrelated to the change,
  which teaches people to ignore red. (Settled in the CI policy work, 2026-10-14; see
  the `clippy` and `tests-passed` discussion in `GIT_FLOW_AND_WORKING_PROCESS.md` and
  `.github/workflows/test.yml`.)
- **A test pinned to a historical fact is acceptable** only while the chain still
  exists and the query stays inside free-tier limits — the
  `polygon_check_if_my_payment_sent` technique above.
- **A clean-authored change to a CRD chapter records its own provenance.** See
  `CLAUDE.md` and ch.35 §35.9.

## Excluded, with the reason

- **Pointing dead tests at Sepolia or another live testnet.** Testnets are retired on
  a schedule (Ropsten, Goerli), so this re-creates the decay; write tests would also
  need a funded key in CI.
- **Deleting the dead tests.** They are the only EVM coverage of their functions.
  `search_for_swap_tx_spend` is covered for UTXO coins by `docker_tests.rs` and
  `utxo_tests.rs`, and CI's `docker-tests` job runs those — the EVM side had nothing
  else.
- **Making live-network tests gate merges.** Someone else's outage would block an
  unrelated change.
- **Adopting `anvil` from the vendored WalletConnect crate.** It appears in
  `vendor-patches/relay_rpc-0.1.0` as that crate's own fixture, not as first-party
  infrastructure, and `geth_dev` already exists first-party with our own HTLC and
  ERC20 contracts. (Note that `alloy`, which we already depend on, ships anvil
  support — so anvil is not excluded as a *technology*, only as a reason to ignore
  `geth_dev`.)

## Open questions for the architecture session

1. Does the next generation standardise on `geth --dev`, on `anvil`, or on both
   behind one trait? What decides it — startup cost, EIP-1559 fidelity, fork-mode
   support, or something else?
2. ~~How does CI get a node, and how do the tests get selected?~~ Answered 2026-10-24: a new
   job, `evm-geth-tests`, with a pinned and checksummed release tarball and an explicit
   test filter; the three `eth_swap_dev_tests` are merge-gating via `tests-passed`.
   Still open: whether a later generation moves to a service container or `anvil`.
3. Fork mode is the interesting question for the ported tests: a local node forked
   from a public archive endpoint at a fixed height would give a *real* node serving
   *real* historical state deterministically — which is what both ported tests
   actually want. It needs a keyed archive endpoint (we already have the
   `POLYGON_RPC_URL` secret pattern) and a caching story so CI does not refetch.
   Is that worth it, or is the scripted node sufficient for this class?
4. What is the minimum fidelity a test needs before a scripted node stops being
   honest? The scripted node asserts our handling of responses we wrote ourselves;
   it cannot catch a wrong assumption about what a real node returns.
5. Should `logs_block_range` and similar free-tier accommodations remain test-local,
   or become a documented testing profile?
6. Does the `external-network-tests` job earn its keep in its current form, or should
   it be split into "live third-party" (best-effort) and "local real node" (gating)?

## Work queued behind the architecture

- ~~Port the three dex-fee/nonce tests and remove the four inline Infura URLs~~ -- done on
  `chore/evm-test-port` (also ported the two ETH dex-fee siblings and `test_nonce_several_urls`).
  The CI selector for `eth::eth_tests::scripted_node_ports` is done (`eth-scripted-node`
  matrix leg).
- ~~Install `geth` in CI **and** add a test selector that reaches `eth_swap_dev_tests`~~ --
  done (`evm-geth-tests` job, 2026-10-24). Open: no live send/refund test yet exercises the
  typed-transaction and base-fee changes beyond what those three tests cover; a
  `test_withdraw_and_send` ETH/JST half that does not need the DOC/MARTY electrums would let
  it join the gating job.
- `cargo check --target wasm32-unknown-unknown -p coins --tests` fails with 17
  pre-existing errors (missing `sql_tx_history_storage`, `native_client_for_test`, no
  `tokio` in scope). CI does not build test targets for wasm32, so it is not a gate —
  but the test target genuinely does not compile for that platform, and the maintainer
  asked for it to be resolved after the current queue. Related infrastructure; decide
  whether it belongs in this plan's scope.
