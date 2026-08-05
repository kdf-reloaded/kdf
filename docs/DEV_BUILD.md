# DEV_BUILD.md — Local build manual

> This guide is build-focused and complements existing docs:
> - [`DEV_ENVIRONMENT.md`](./DEV_ENVIRONMENT.md) for full test environment setup
> - [`WASM_BUILD.md`](./WASM_BUILD.md) for detailed WASM workflow

## 1) Preparation

### 1.1 Supported build platforms (current project scope)

- Linux (x86_64)
- Windows (x86_64, MSVC)
- macOS (x86_64 / ARM64 / Universal)
- WASM (`wasm32-unknown-unknown`)
- iOS (aarch64)
- Android (aarch64, armv7)

In this first version of the manual, Linux and Windows are fully documented. Other platforms are listed as placeholders for follow-up.

### 1.2 Common prerequisites

Install these first on your machine:

1. **Git**
2. **Rust toolchain (stable)** via `rustup`
3. **Rust components**: `rustfmt`, `clippy`
4. **Protobuf compiler** (`protoc`) available in `PATH`
5. **C/C++ build tooling** for your OS

Project toolchain baseline:

- `rust-toolchain.toml` pins channel to `stable`

Recommended setup commands:

```bash
rustup toolchain install stable --profile minimal
rustup default stable
rustup component add rustfmt clippy
```

### 1.3 Linux preparation

Install system packages (example for Debian/Ubuntu):

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev protobuf-compiler
```

Optional explicit target install:

```bash
rustup target add x86_64-unknown-linux-gnu
```

### 1.4 Windows preparation

1. Install **Visual Studio Build Tools 2022** with C++ workload (MSVC + Windows SDK).
2. Install **Git for Windows**.
3. Install `protoc` (for example via Chocolatey).
4. Install Rust stable + target.

PowerShell example:

```powershell
choco install protoc -y
rustup toolchain install stable --profile minimal
rustup target add x86_64-pc-windows-msvc
rustup component add rustfmt clippy
```

## 2) Debug build

## 2.1 Linux debug build

From repository root:

```bash
cd mm2src
cargo build --bin kdf --target x86_64-unknown-linux-gnu --profile dev
```

Expected artifact:

- `target/x86_64-unknown-linux-gnu/dev/kdf`

## 2.2 Windows debug build

From repository root (PowerShell):

```powershell
cd mm2src
cargo build --bin kdf --target x86_64-pc-windows-msvc --profile dev
```

Expected artifact:

- `target/x86_64-pc-windows-msvc/dev/kdf.exe`

## 2.3 macOS debug build

TODO: add macOS debug build instructions.

## 2.4 WASM debug build

TODO: add WASM debug build instructions (see `docs/WASM_BUILD.md` in the meantime).

## 2.5 iOS debug build

TODO: add iOS debug build instructions.

## 2.6 Android debug build

TODO: add Android debug build instructions.

## 3) Release build

### 3.1 General release build

For native host release build (host target):

```bash
cd mm2src
cargo build --bin kdf --release
```

Target-specific release commands are preferred for reproducible artifacts (see platform sections below).

### 3.2 Linux release build

```bash
cd mm2src
cargo build --bin kdf --target x86_64-unknown-linux-gnu --release
```

Expected artifact:

- `target/x86_64-unknown-linux-gnu/release/kdf`

### 3.3 Windows release build

```powershell
cd mm2src
cargo build --bin kdf --target x86_64-pc-windows-msvc --release
```

Expected artifact:

- `target/x86_64-pc-windows-msvc/release/kdf.exe`

### 3.4 macOS release build

TODO: add macOS release build instructions.

### 3.5 WASM release build

TODO: add WASM release build instructions (see `docs/WASM_BUILD.md` in the meantime).

### 3.6 iOS release build

TODO: add iOS release build instructions.

### 3.7 Android release build

TODO: add Android release build instructions.

## 4) Build features

Below are significant `mm2_main` build features (from `mm2src/mm2_main/Cargo.toml`).

### 4.1 `unsafe-rpc-wire-dump` (latest)

Enables debugging-only RPC request/response wire dump code paths. This feature is intended
for development and debugging; the compiled code is dormant until activated at runtime.

- **Compile-time guard:** Feature flag gates the code paths
- **Runtime gate:** `MM2_RPC_WIRE_DUMP=1` environment variable activates dumping
- **Secret redaction:** Default behavior redacts sensitive data (mnemonics, keys, etc.)
- **Full dump:** Set `MM2_RPC_WIRE_DUMP_SECRETS=1` to dump unredacted (use with caution)

**Release profile note:** See section 4.2 (`unsafe-rpc-wire-dump-release-override`) for using
this feature in release builds.

Example (dev profile):

```bash
cd mm2src
MM2_RPC_WIRE_DUMP=1 cargo build -p mm2_main --features unsafe-rpc-wire-dump
```

### 4.2 `unsafe-rpc-wire-dump-release-override` (latest)

Explicit override that permits release-profile builds when `unsafe-rpc-wire-dump` is enabled.

The `mm2_main` build script (in `build.rs`) enforces that this override flag must be set
whenever `unsafe-rpc-wire-dump` is used with a release profile, to prevent accidental
production builds containing debugging code. When building in dev profile, the override
is not required.

Example (release with override):

```bash
cd mm2src
cargo build -p mm2_main --release --features unsafe-rpc-wire-dump,unsafe-rpc-wire-dump-release-override
```

Example (dev profile, override not required):

```bash
cd mm2src
cargo build -p mm2_main --features unsafe-rpc-wire-dump
```

### 4.3 `regtest-netid`

Compiles in netid 9000 support for regtest/docker harness use.

```bash
cd mm2src
cargo build -p mm2_main --features regtest-netid
```

### 4.4 `custom-swap-locktime`

Testing/debugging feature for custom swap locktime behavior. Not for production release builds.

### 4.5 `ibc-routing-for-swaps`

Feature switch for IBC routing in swap paths.

### 4.6 `zhtlc-native-tests`

Enables native test functionality in `coins` for zHTLC-related coverage.

---

## 5) Code formatting

This project enforces formatting with a **pinned nightly toolchain** (currently `nightly-2026-05-08`).
Plain `cargo fmt` uses the stable toolchain and produces different output — it will fail CI.

**Rules:**
- Always use `cargo +nightly-2026-05-08 fmt`, never bare `cargo fmt`.
- Always scope to the crate(s) you modified with `-p <crate>`. Never format the whole workspace — it contains third-party patched vendor trees (`*-patched/`) that must not be touched.

```bash
# Format a single crate:
cargo +nightly-2026-05-08 fmt -p coins

# Format multiple crates you modified:
cargo +nightly-2026-05-08 fmt -p coins -p mm2_main

# Format all non-patched KDF packages (CI equivalent):
pkgs=$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.manifest_path | test("-patched/") | not) | .name')
args=(); for p in $pkgs; do args+=(-p "$p"); done
cargo +nightly-2026-05-08 fmt "${args[@]}"

# Check without modifying (CI mode):
cargo +nightly-2026-05-08 fmt "${args[@]}" -- --check
```

### 5.1) Pre-commit hook (project-local)

A pre-commit hook that runs the fmt check automatically lives in `.githooks/pre-commit`.
It is not active by default. Enable it once per clone:

```bash
git config core.hooksPath .githooks
```

This is a local `.git/config` setting — it only affects your clone and does not affect other developers.

---

## 6) Quick verification commands

Linux:

```bash
cd mm2src
cargo check -p mm2_main
cargo build --bin kdf --target x86_64-unknown-linux-gnu --release
```

Windows (PowerShell):

```powershell
cd mm2src
cargo check -p mm2_main
cargo build --bin kdf --target x86_64-pc-windows-msvc --release
```

---

## 7) Automated CI builds

The sections above cover building locally. CI produces the downloadable binaries
via GitHub Actions.

### 7.1) Per-platform build workflows

Each target has a dedicated workflow under `.github/workflows/` that accepts a
`profile` input (`release` / `dev`) and an optional `features` input (comma-separated
Cargo feature flags) that can be run on its own or reused by an umbrella workflow:

- `build-linux.yml`, `build-macos.yml`, `build-windows.yml`, `build-ios.yml`,
  `build-android.yml`, `build-wasm.yml`.

The `features` parameter is particularly useful for enabling debugging features like
`unsafe-rpc-wire-dump` across all platforms via a single workflow invocation.

**Linux is built inside a pinned Debian 11 container (glibc 2.31).** This gives
the shipped binary a deliberately low glibc floor so it runs on any host with
glibc >= 2.31 (Debian 11/12, Ubuntu 20.04+, RHEL/Rocky 9, …). Building on a
newer base would refuse to start on those still-common hosts. The dev, staging,
and release pipelines all reuse `build-linux.yml`, so **every snapshot and
release inherits the same backwards-compatible floor.**

### 7.2) Dev snapshots — `dev-build.yml`

Unsigned, all-platform snapshot builds, **manual only** (`workflow_dispatch`).
Use the "Run workflow" button to snapshot any ref on demand. Artifacts are
uploaded as GitHub Actions run artifacts; they are **not** checksummed, signed,
or published as a GitHub Release.

### 7.2a) Dev snapshots with RPC dump — `dev-build-rpc.yml`

Variant of `dev-build.yml` that builds all platforms with the `unsafe-rpc-wire-dump`
and `unsafe-rpc-wire-dump-release-override` features enabled. This produces debug
binaries capable of capturing and dumping RPC request/response wire traffic.

**Requires both features together:** The `build.rs` build script in `mm2_main` enforces
that `unsafe-rpc-wire-dump-release-override` must be enabled when building in release
profile with the `unsafe-rpc-wire-dump` feature, to prevent accidental production builds
with debugging code. The CI workflow automatically includes both features.

**Runtime activation:** The wire dump code is compiled in but inactive by default.
Enable it with environment variables:

```bash
MM2_RPC_WIRE_DUMP=1 ./kdf          # Dump RPC wire traffic (secrets redacted)
MM2_RPC_WIRE_DUMP_SECRETS=1 ./kdf  # Full unredacted dump (caution: sensitive data)
```

### 7.3) Staging snapshots — `staging-build.yml`

Unsigned, all-platform snapshot builds that run **automatically on every push to
`staging`** (also runnable via `workflow_dispatch`). `staging` is the
feature-frozen, stabilizing pre-release line, so testers/QA always have current
beta/rc binaries without a manual trigger. Same output posture as dev snapshots:
run artifacts only — **not** checksummed, signed, or published as a Release.

### 7.4) Signed releases — `release.yml`

Triggered by a `v*` tag. A **final** tag (`vX.Y.Z`) whose commit is on `main`
publishes the signed, latest GitHub Release; a **pre-release** tag
(`vX.Y.Z-alpha.N` / `-beta.N` / `-rc.N`) whose commit is on `staging` publishes
a signed GitHub pre-release. Both produce checksums + a GPG-signed manifest. See
[`RELEASE.md`](RELEASE.md) for the full runbook. `release.yml` reuses the same
per-platform build workflows (including the Debian 11 `build-linux.yml`).
