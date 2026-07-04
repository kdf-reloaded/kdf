# Contributing to KDF-Reloaded

We welcome contributions in the form of suggestions, bug reports, pull requests, and feedback.
Please note we have a code of conduct, please follow it in all your interactions with the project.

See also: [docs/PR_REVIEW_CHECKLIST.md](./docs/PR_REVIEW_CHECKLIST.md),
[docs/GIT_FLOW_AND_WORKING_PROCESS.md](./docs/GIT_FLOW_AND_WORKING_PROCESS.md),
[docs/UNIT_TESTS.md](./docs/UNIT_TESTS.md).

## Submitting feature requests

Before uploading any changes, please make sure that the test suite passes locally before submitting a pull request with your changes.

```
cargo test --bins --lib
```

We also use [Clippy](https://github.com/rust-lang/rust-clippy) to avoid common mistakes
and we use [rustfmt](https://github.com/rust-lang/rustfmt) to make our code clear to everyone.

1. Format the code using rustfmt. **This project requires the pinned nightly toolchain** (see `rust-toolchain.toml` for the exact version) for formatting — plain `cargo fmt` will produce a different result and fail CI:
    ```shell
    # Determine the pinned nightly version:
    grep channel rust-toolchain.toml

    # Format only the crates you modified (never run on the whole workspace):
    cargo +nightly-2026-05-08 fmt -p <crate_name>

    # To format all non-patched KDF packages at once:
    pkgs=$(cargo metadata --no-deps --format-version 1 \
      | jq -r '.packages[] | select(.manifest_path | test("-patched/") | not) | .name')
    args=(); for p in $pkgs; do args+=(-p "$p"); done
    cargo +nightly-2026-05-08 fmt "${args[@]}"
    ```
    **Important**: never run `cargo fmt` without `-p <crate>` scoping — the workspace contains third-party patched vendor trees that must not be reformatted.
2. Make sure there are no warnings and errors. Run the Clippy:
    ```shell
    cargo clippy -- -D warnings
    ```
3. Make sure that no new dependencies duplicates appear. Run the following check
   ```shell
   cargo deny check bans
   ```
4. Make sure that dependencies do not have known vulnerabilities. If they do, update them.
   ```shell
   cargo deny check advisories
   ```

### Run WASM tests

1. Install Firefox.
1. Download Gecko driver for your OS: https://github.com/mozilla/geckodriver/releases
1. Run the tests
    ```
    WASM_BINDGEN_TEST_TIMEOUT=120 GECKODRIVER=PATH_TO_GECKO_DRIVER_BIN wasm-pack test --firefox --headless mm2src/mm2_main
    ```
