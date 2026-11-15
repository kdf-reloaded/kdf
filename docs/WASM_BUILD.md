# Building WASM binary

## Setting up the environment

To build WASM binary from source, the following prerequisites are required:

1. Install `wasm-pack`
   ```
   cargo install wasm-pack
   ```
2. OSX specific: install `llvm`
   ```
   brew install llvm
   ```

## Compiling WASM release binary

To build WASM release binary run one of the following commands according to your environment:

- for Linux users:
   ```
   wasm-pack build mm2src/mm2_main --target web --out-dir wasm_build/deps/pkg/
   ```
- for OSX users (Intel):
   ```
   CC=/usr/local/opt/llvm/bin/clang AR=/usr/local/opt/llvm/bin/llvm-ar wasm-pack build mm2src/mm2_main --target web --out-dir wasm_build/deps/pkg/
   ```
- for OSX users (M1):
   ```
   CC=/opt/homebrew/opt/llvm/bin/clang AR=/opt/homebrew/opt/llvm/bin/llvm-ar wasm-pack build mm2src/mm2_main --target web --out-dir wasm_build/deps/pkg/
   ```

Please note `CC` and `AR` must be specified in the same line as `wasm-pack test mm2src/mm2_main`.

## Compiling WASM binary with debug symbols

If you want to disable optimizations to reduce the compilation time, run `wasm-pack build mm2src/mm2_main` with an additional `--dev` flag:
```
wasm-pack build mm2src/mm2_main --target web --out-dir wasm_build/deps/pkg/ --dev
```

Please don't forget to specify `CC` and `AR` if you run the command on OSX.

## What a browser build cannot do

A page has no raw sockets, accepts no inbound connections, sends no UDP, and the browser owns TLS. A browser build
therefore refuses or ignores the following; use a native build where you need them:

- **Lightning**: its peers need raw TCP (only possible behind a WebSocket-to-TCP proxy).
- **Electrum over TCP or SSL**: use the coin's WS/WSS servers. Server key pinning (`spki_sha256`) and
  `disable_cert_verification` have no effect, because the browser checks certificates itself.
- **Native-daemon (coin daemon) mode** for UTXO coins.
- **Seed and relay node roles**: a browser node is a light node and uses WSS relays only.
- **DNS resolver settings**: the browser resolves names.
- **NTP clock check**: the `ntp_*` settings are ignored.
- **RPC server, configuration and log files**: configuration comes from JavaScript, and logs go to a JavaScript
  callback.

Shielded coins (ARRR) are refused in a browser build for now; this is missing work, not a browser limit. The
plans [`shielded-coins-in-browser`](plans/shielded-coins-in-browser.md) and
[`browser-build-gaps`](plans/browser-build-gaps.md) list the work that is planned.
