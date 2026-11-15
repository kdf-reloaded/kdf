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
`mm2src/mm2_net` (the browser tests of `mm2src/coins` do not compile yet). The `wasm-bindgen-test` release in `Cargo.lock` has to match the locked
`wasm-bindgen` release, or the runner reports "no tests to run!". No CI job runs the browser tests yet; they
are run by hand.

PS If you notice that this guide is outdated, please submit a PR.
