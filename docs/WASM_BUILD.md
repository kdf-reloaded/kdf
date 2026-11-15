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

## Build output and the demo page

The package is named after the library target of `mm2_main`, so the build writes these files to
`wasm_build/deps/pkg/`:

- `kdflib.js`: the JavaScript glue, imported by the demo page `wasm_build/script.js`
- `kdflib_bg.wasm`: the WebAssembly binary
- `kdflib.d.ts` and `kdflib_bg.wasm.d.ts`: type declarations

The `wasm_build/deps/` directory is ignored by git. To try the build in a browser, serve the
repository root with a local HTTP server and open `wasm_build/index.html`; see
[`wasm_build/README.md`](../wasm_build/README.md).

## Compiling WASM binary with debug symbols

If you want to disable optimizations to reduce the compilation time, run `wasm-pack build mm2src/mm2_main` with an additional `--dev` flag:
```
wasm-pack build mm2src/mm2_main --target web --out-dir wasm_build/deps/pkg/ --dev
```

Please don't forget to specify `CC` and `AR` if you run the command on OSX.