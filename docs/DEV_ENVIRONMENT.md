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
`mm2src/mm2_net` (the browser tests of `mm2src/coins` now compile; on the last run the `coins` test build ran 44 tests — 24 of `coins` itself (18 passed, 5 failed, 1 hung) and 20 linked in from `mm2_db` (16, all passed) and `mm2_net` (4: 3 passed, 1 failed), whose browser test modules are not limited to test builds. Failed, needing a public node: `eth::eth_wasm_tests::test_init_eth_coin` (node fetch fails), `utxo::utxo_wasm_tests::test_electrum_rpc_client` (server answers "use server.version to identify client") and `utxo::utxo_wasm_tests::test_electrum_display_balances` (cannot connect to the Electrum servers in 5 s). Failed, other: `wasm_ws::tests::test_websocket_invalid_url` (an `mm2_net` test, expects an error and gets none), `nft::store::idb::tests::attach_metadata_to_transfers_backfills_payload_and_columns` (IndexedDB ConstraintError while uploading an item) and `nft::store::idb::tests::append_transfers_and_lookup_by_log` (reads back `None` where a transfer is expected). Hung, needing a public node: `utxo::bchd_grpc::wasm_tests::test_check_slp_transaction_valid`, which stops the run unless it is skipped with `wasm-pack test --firefox --headless mm2src/coins -- --skip test_check_slp_transaction_valid`). Tests that only work natively are left out of the browser build. The `wasm-bindgen-test` release in `Cargo.lock` has to match the locked
`wasm-bindgen` release, or the runner reports "no tests to run!". No CI job runs the browser tests yet; they
are run by hand.

PS If you notice that this guide is outdated, please submit a PR.
