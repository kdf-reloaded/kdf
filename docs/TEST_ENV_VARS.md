# Test environment variables

This is the authoritative, source-verified reference for the environment
variables and `.env` files that the KDF Reloaded build and test suites read.
Upstream historically documented only the passphrase setup in
[`DEV_ENVIRONMENT.md`](./DEV_ENVIRONMENT.md); this file lists the full set.

> Runtime configuration (what an operator actually configures to run `kdf`)
> lives in `MM2.json`, **not** in environment variables or `.env` files. The
> variables below are for building and testing. See the project
> [`README.md`](../README.md) for runtime config.

## `.env.seed` and `.env.client`

These two files are **test fixtures**, not a general configuration mechanism.
They are read by the test helper `get_passphrase()` and parsed by
`from_env_file()` in
[`mm2src/mm2_test_helpers/src/for_tests.rs`](../mm2src/mm2_test_helpers/src/for_tests.rs).

The parser recognises **exactly two keys**, via the regex
`^(PASSPHRASE|USERPASS)=(\w[\w ]+)$`:

| Key | Meaning |
|-----|---------|
| `PASSPHRASE` | Wallet seed phrase for the test instance. |
| `USERPASS`   | RPC `userpass` for the test instance. |

Notes:

- The value charset is word-characters and spaces only (a BIP39 mnemonic fits;
  punctuation or raw hex does **not** parse).
- Both files are listed in [`.gitignore`](../.gitignore) (`.env.seed`,
  `.env.client`) and must never be committed.
- `get_passphrase()` reads the file first, then falls back to the process
  environment variable of the same logical name. A missing file is not an
  error (it returns empty), so exporting the env var alone is sufficient.

Conventional contents (see `DEV_ENVIRONMENT.md`):

```
# .env.seed
PASSPHRASE=also shoot benefit prefer juice shell elder veteran woman mimic image kidney
```

```
# .env.client
PASSPHRASE=spice describe gravity federal blast come thank unfair canal monkey style afraid
```

> ⚠️ The upstream/GLEEC `DEV_ENVIRONMENT.md` writes `BOB_PASSPHRASE=` /
> `ALICE_PASSPHRASE=` *inside* these files. The parser only matches
> `PASSPHRASE`/`USERPASS`, so those lines are silently ignored. In Reloaded,
> use `PASSPHRASE=` in the file and export `BOB_PASSPHRASE`/`ALICE_PASSPHRASE`
> as environment variables (see below).

## Swap counterparty passphrases

The integration and docker tests run two parties and read these directly from
the process environment (not from the `.env` files):

| Variable | Meaning |
|----------|---------|
| `BOB_PASSPHRASE`   | Maker / "seed" side passphrase. |
| `ALICE_PASSPHRASE` | Taker / "client" side passphrase. |

Constraints: both must be **non-empty** and **distinct** from each other.

For the **docker** suite the values are arbitrary: the harness funds the
addresses derived from them dynamically on a private regtest chain it spins up
(`fill_address` in
[`docker_tests_common.rs`](../mm2src/mm2_main/src/docker_tests/docker_tests_common.rs)).
The CI docker job therefore generates a fresh random pair per run via
[`scripts/with-test-passphrases.sh`](../scripts/with-test-passphrases.sh) and
commits no seed.

For tests that hit a **live testnet** and withdraw from a *pre-funded* address
(e.g. the DOC/MARTY withdraw tests in the `external-network-tests` CI job), the
specific funded seed is required — those keep their fixed, well-known public
testnet seeds and must **not** be randomised.

## Full variable catalogue (source-verified)

Generated from a grep of `var(...)` / `env::var(...)` / `option_env!(...)`
reads across `mm2src/`. If you add a new environment variable, please add a row
here.

| Variable | Scope | Default / source | Purpose |
|----------|-------|------------------|---------|
| `PASSPHRASE`, `USERPASS` | test `.env` files | — | seed / RPC userpass (only file keys) |
| `BOB_PASSPHRASE`, `ALICE_PASSPHRASE` | test env | — | swap counterparties |
| `MM_CONF_PATH` | runtime | `MM2.json` | path to the config file |
| `MM_COINS_PATH` | runtime | `coins` | path to the coins file |
| `MM_LOG` | runtime | — | log file path |
| `RUST_LOG` | runtime / test | — | log level filter |
| `TELEGRAM_API_KEY` | runtime | — | Telegram notifications |
| `LOCAL_THREAD_MM` | test | — | run MarketMaker in-thread vs. subprocess |
| `MYCOIN_FEE_DISCOUNT` | test | — | enable a test-only fee discount path |
| `TRACE_BLOCK_ON` | debug | — | trace `block_on` calls |
| `KEEP_CONTAINERS` | docker test | — | keep test containers after the run |
| `QTUM_REGTEST_DOCKER_IMAGE` | docker test | built-in default | override the QTUM regtest image |
| `ONE_INCH_API_TEST_AUTH` | test | empty | 1inch API auth token for trading-api tests |
| `WASM_BINDGEN_TEST_TIMEOUT` | wasm test | — | per-test timeout (seconds) |
| `GECKODRIVER` | wasm test | — | path to the geckodriver binary |
| `RUST_WASM_TEST_LOG` | wasm test | — | wasm log level |
| `MANUAL_MM_VERSION` | build | git-derived | override the embedded version string |
