# AtomicDEX-API WASM example

**wasm_build** is an example of using **MarketMaker2** in webpages
via [WebAssembly](https://developer.mozilla.org/en-US/docs/WebAssembly)

1. Build WASM binary according to the [WASM Build Guide](../docs/WASM_BUILD.md). The build writes
   `kdflib.js` and `kdflib_bg.wasm` (plus type declarations) to `wasm_build/deps/pkg/`; the page
   loads `./deps/pkg/kdflib.js`. The `wasm_build/deps/` directory is not tracked by git.
2. Run a local HTTP server from the repository root (the page URL below is relative to it)
    - if you use Python 3, run:
   ```
   python3 -m http.server 8000
   ```
    - if you use Python 2, run:
   ```
   python -m SimpleHTTPServer 8000
   ```
   Read more about
   [running a simple local HTTP server](https://developer.mozilla.org/en-US/docs/Learn/Common_questions/set_up_a_local_testing_server#running_a_simple_local_http_server)
3. Open webpage in your browser http://localhost:8000/wasm_build/index.html
