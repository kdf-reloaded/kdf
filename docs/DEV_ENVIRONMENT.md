# Setting up the dev environment for KDF-Reloaded to run the full tests suite

For the curated set of fully-offline unit tests added in Reloaded, see
[UNIT_TESTS.md](./UNIT_TESTS.md). The instructions below cover the full
integration / docker / WASM suite inherited from upstream.

## Running native tests

1. Install Docker or Podman.
2. Install `libudev-dev` (dpkg) or `libudev-devel` (rpm) package.
3. Install protobuf compiler, so `protoc` is available in your PATH.
4. Download ZCash params files: [Windows](https://github.com/KomodoPlatform/komodo/blob/master/zcutil/fetch-params.bat),
   [Unix/Linux](https://github.com/KomodoPlatform/komodo/blob/master/zcutil/fetch-params.sh)
5. Create `.env.client` file with the following content
   ```
   PASSPHRASE=spice describe gravity federal blast come thank unfair canal monkey style afraid
   ```
6. Create `.env.seed` file with the following content
   ```
   PASSPHRASE=also shoot benefit prefer juice shell elder veteran woman mimic image kidney
   ```
   For docker / integration tests, also export them as environment variables:
   ```shell
   export BOB_PASSPHRASE="also shoot benefit prefer juice shell elder veteran woman mimic image kidney"
   export ALICE_PASSPHRASE="spice describe gravity federal blast come thank unfair canal monkey style afraid"
   ```
7. MacOS specific: run script (required after each reboot)
   ```shell
   #!/bin/bash
   for ((i=2;i<256;i++))
   do
       sudo ifconfig lo0 alias 127.0.0.$i up
   done
   sudo ifconfig lo0 inet6 -alias ::1
   sudo ifconfig lo0 inet6 -alias fe80::1%lo0
   ```
   Please note that you have to run it again after each reboot
8. Linux specific:
    - for Docker users:
       ```
       sudo groupadd docker
       sudo usermod -aG docker $USER
       ```
    - for Podman users:
       ```
       sudo ln -s $(which podman) /usr/bin/docker
       ```
9. Try `cargo test --bins --lib` for unit tests, or `cargo test --features native --all -- --test-threads=16` for the full suite.

## Running WASM tests

1. Set up [WASM Build Environment](../docs/WASM_BUILD.md#Setting-up-the-environment)
2. Install Firefox.
3. Gecko driver: if `geckodriver` is not on your `PATH` and `GECKODRIVER` is not set, `wasm-pack` downloads a copy into its cache (`~/.cache/.wasm-pack`) on the first run. To use your own, download it for your OS from the [releases](https://github.com/mozilla/geckodriver/releases).
4. Set environment variables required to run WASM tests
   ```shell
   # wasm-bindgen specific variables
   export WASM_BINDGEN_TEST_TIMEOUT=120
   # optional, only to use your own geckodriver instead of the one `wasm-pack` downloads
   export GECKODRIVER=PATH_TO_GECKO_DRIVER_BIN
   # MarketMaker specific variables
   export BOB_PASSPHRASE="also shoot benefit prefer juice shell elder veteran woman mimic image kidney"
   export ALICE_PASSPHRASE="spice describe gravity federal blast come thank unfair canal monkey style afraid"
   ```
6. Run WASM tests
   - for Linux users:
   ```
   wasm-pack test --firefox --headless mm2src/mm2_main
   ```
    - for OSX users (Intel):
   ```
   CC=/usr/local/opt/llvm/bin/clang AR=/usr/local/opt/llvm/bin/llvm-ar wasm-pack test --firefox --headless mm2src/mm2_main
   ```
    - for OSX users (M1):
   ```
   CC=/opt/homebrew/opt/llvm/bin/clang AR=/opt/homebrew/opt/llvm/bin/llvm-ar wasm-pack test --firefox --headless mm2src/mm2_main
   ```
   Please note `CC` and `AR` must be specified in the same line as `wasm-pack test mm2src/mm2_main`.

The same command runs the browser tests of a crate that has them, for example `mm2src/mm2_db` or
`mm2src/mm2_net` (the browser tests of `mm2src/coins` now compile). Each crate's test binary runs only the tests of that crate. On the last run: `mm2_db` had 17 tests, all passed; `mm2_net` had 5, of which 3 passed and 2 failed (`wasm_ws::tests::test_websocket_invalid_url` expects an error for an invalid address and gets none; `host_label::tests::wasm::a_browser_text_that_repeats_the_url_percent_encoded_loses_it` reports "the scrubbed text differs from the expected one"); `coins` had 24, of which 18 passed, 5 failed and 1 hung. The browser tests of `mm2_main` now compile; they are 12: `mm2::mm2_tests::trade_test_doc_and_marty` (fails: needs the `BOB_PASSPHRASE` environment variable and a trade setup, "No such 'BOB_PASSPHRASE' environment variable"), `mm2::mm2_tests::test_qrc20_tx_history` (hangs: it starts a node on network id 9998, the node start reports "UnsupportedNetId { netid: 9998, supported: [8762, 6133] }" inside a spawned task, and the test does not end; it stops the run unless it is skipped with `-- --skip test_qrc20_tx_history`), and ten tests of the browser storage code that all pass (`lp_ordermatch::my_orders_storage` 4: `test_delete_my_maker_order`, `test_delete_my_taker_order`, `test_load_active_maker_taker_orders`, `test_filtering_history`; `lp_swap::saved_swap` 1: `test_saved_swap_table`; `lp_swap::swap_lock` 3: `test_file_lock_should_create_file_and_record_timestamp_and_then_delete_on_drop`, `test_file_lock_should_return_none_if_lock_acquired`, `test_file_lock_should_acquire_and_update_timestamp_if_ttl_expired`; `lp_swap::my_swaps_storage` 2: `test_take_according_to_paging_opts`, `test_my_recent_swaps`). `mm2_gui_storage` has no browser tests, and its test build now compiles. `common`, `trezor`, `hw_common` and `kdf_walletconnect` build their browser tests and have none. The `coins` failures that need a public node are `eth::eth_wasm_tests::test_init_eth_coin` (node fetch fails), `utxo::utxo_wasm_tests::test_electrum_rpc_client` (server answers "use server.version to identify client") and `utxo::utxo_wasm_tests::test_electrum_display_balances` (cannot connect to the Electrum servers in 5 s). The other two `coins` failures are `nft::store::idb::tests::attach_metadata_to_transfers_backfills_payload_and_columns` (IndexedDB ConstraintError while uploading an item) and `nft::store::idb::tests::append_transfers_and_lookup_by_log` (reads back `None` where a transfer is expected). The test that hung, and needs a public node, is `utxo::bchd_grpc::wasm_tests::test_check_slp_transaction_valid`; it stops the run unless it is skipped with `wasm-pack test --firefox --headless mm2src/coins -- --skip test_check_slp_transaction_valid`. After the tests, `wasm-pack test` also runs the documentation examples of `mm2_db`; its 3 examples in `indexed_db/indexed_cursor.rs` are not compilable code and fail. Tests that only work natively are left out of the browser build. The `wasm-bindgen-test` release in `Cargo.lock` has to match the locked
`wasm-bindgen` release, or the runner reports "no tests to run!". No CI job runs the browser tests yet; they
are run by hand.

PS If you notice that this guide is outdated, please submit a PR.
