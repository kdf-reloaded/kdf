#!/bin/dash
# Run with `dash js/wasm-build.sh`.
set -ex

cargo build --bin kdf
ln -f target/debug/kdf js/kdf

cargo build --target=wasm32-unknown-unknown --release
ln -f target/wasm32-unknown-unknown/release/kdflib.wasm js/kdflib.wasm
