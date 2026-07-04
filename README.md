# KDF Reloaded

> **GPLv2 continuation of the Komodo DeFi Framework — peer-to-peer atomic swaps, no central authority.**

[![License: GPL v2](https://img.shields.io/badge/License-GPLv2-blue.svg)](LEGAL/LICENSE)
[![Status: Beta](https://img.shields.io/badge/status-beta-yellow.svg)](#beta-disclaimer)

KDF Reloaded is an open-source [atomic-swap](https://en.wikipedia.org/wiki/Atomic_swap) engine for trustless peer-to-peer trading across blockchains, derived from the Komodo DeFi Framework / AtomicDEX-API codebase as it stood under the GPLv2 license.

> **Release note:** This repository is published with a documented GPLv2 continuation posture and an explicit pre-release checklist; see [`SECURITY.md`](SECURITY.md) and [`RELEASE_CHECKLIST.md`](RELEASE_CHECKLIST.md) for release controls and operator guidance.

## Heritage

This project is a **continuation**, not a fork-of-current-upstream. It is anchored to the last commit of the upstream Komodo DeFi Framework that was unambiguously distributed under GPLv2-only (commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`, dated 2022-06-03). Post-anchor development is **hybrid**: it includes independently authored (clean-room) work, code adapted from permissively-licensed upstreams, and — for some subsystems — **lineage-derived code from the upstream/GLEEC KDF work product carried under GPLv2 copyleft** (for example, post-anchor additions in the relocated `lp_ordermatch` modules under `mm2src/mm2_main/src/lp_ordermatch/`). The right to redistribute rests on GPLv2 copyleft (GPLv2 §2(b)/§6), not on a clean-room claim for every file. Per-file provenance and the license basis for each class are tracked in [`docs/reloaded-rewrite/34-provenance-ledger.md`](docs/reloaded-rewrite/34-provenance-ledger.md). The repository as a whole is distributed under GPLv2-only; two inherited third-party license tensions (a GPL-3.0 file and the Apache-2.0 WalletConnect SDK dependencies) are openly tracked as deferred items in [`LEGAL/LICENSING-POLICY.md`](LEGAL/LICENSING-POLICY.md) §4.

For readers who want the shortest safe summary: this repository continues the
last clearly GPLv2-only upstream baseline, keeps the combined work under
GPLv2-only, records post-anchor original contributions as GPL-2.0-or-later,
and documents provenance file-by-file instead of claiming that every file is a
clean-room rewrite. For the full technical derivation record, read the 
[Clean-Room Documentation (CRD)](docs/reloaded-rewrite/) chapters linked below.

For the relationship to other downstream projects (notably the GLEEC fork), see [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md).

## Beta disclaimer

KDF Reloaded is currently in **public beta**. APIs, on-disk formats, and the network protocol may still change between pre-1.0 releases. Use only with funds you can afford to lose.

- The `kdf` binary is provided for evaluation, testing, and review.
- Mainnet swaps function on netid `8762` (AtomicDEX network) and netid `6133` (GLEEC network), but you are running unaudited pre-release software.
- Git commits and release tags are GPG-signed by the maintainer (`Takologi <takologi@proton.me>`, fingerprint `FEE1ACA52C65FF3EBF31818CB5595E1752BC2A82`); the public key is at [`docs/keys/takologi.asc`](docs/keys/takologi.asc). Release **binaries** ship with a GPG-signed `SHA256SUMS` manifest; see [`docs/RELEASE.md`](docs/RELEASE.md) and [`SECURITY.md`](SECURITY.md).

## What it does

- **Atomic swaps** between supported chains via Hash Time Locked Contracts (HTLCs) — no custodian, no proxy tokens, you keep your keys.
- **Multi-protocol coin support**: UTXO chains (Bitcoin family), EVM chains, Tendermint/Cosmos, Zcash (sapling), Lightning Network, and others — see [`mm2src/coins/`](mm2src/coins/).
- **Distributed orderbook** propagated over [libp2p](https://libp2p.io/) gossipsub.
- **JSON-RPC API** consumable from CLI, scripts, or third-party GUIs.

## Networks

| netid | Network | Status in beta |
|------:|---------|-----------------|
| 8762  | AtomicDEX (default upstream network) | Supported |
| 6133  | GLEEC                                | Supported |

Other netids (including 7777, 8100, 8999, 9000, 9998) are not part of the supported beta surface. Test-only netids exist in the codebase under the `regtest-netid` Cargo feature, off by default. See [`docs/NETWORK_CONFIG.md`](docs/NETWORK_CONFIG.md).

## Building from source

Requirements:

- Stable Rust toolchain (see [`rust-toolchain.toml`](rust-toolchain.toml))
- CMake ≥ 3.12
- A C/C++ toolchain (build-essential / Xcode CLT / MSVC)

```sh
cargo build --release --bin kdf
```

The binary is placed at `target/release/kdf`. For a development environment with full test infrastructure (Docker-based integration tests, electrum mocks, etc.) see [`docs/DEV_ENVIRONMENT.md`](docs/DEV_ENVIRONMENT.md).

For WebAssembly builds, see [`docs/WASM_BUILD.md`](docs/WASM_BUILD.md).

## Configuration

Two files drive runtime configuration:

- `MM2.json` — RPC credentials, mnemonic, `netid`, optional toggles. This project targets RPC/config compatibility with the Komodo DeFi Framework API; see the [Komodo DeFi Framework documentation](https://komodoplatform.com/en/docs/komodo-defi-framework/) for the full schema.
- `coins` — list of activatable coin definitions. A community-maintained registry lives at [github.com/KomodoPlatform/coins](https://github.com/KomodoPlatform/coins).

Minimal example:

```json
{
  "gui": "kdf-reloaded",
  "netid": 8762,
  "rpc_password": "Ent3r_Un1Qu3_Pa$$w0rd",
  "passphrase": "ENTER_UNIQUE_SEED_PHRASE_DO_NOT_REUSE"
}
```

> **WalletConnect session storage.** The optional `wc_session_persistence`
> setting controls whether and how WalletConnect v2 sessions are persisted.
> It defaults to `open` (GLEEC-compatible plaintext, byte-interchangeable with
> GLEEC KDF); set it to `none` to disable session persistence. See
> [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md) for details.

iOS builds are not currently part of the beta release matrix.

## Usage

Launch the daemon:

```sh
./kdf
```

It exposes a JSON-RPC server on `127.0.0.1:7783` by default. The RPC catalogue is identical to the upstream Komodo DeFi Framework where unchanged; differences are tracked in [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md). RPC namespaces include unprefixed stable methods, `task::*` for long-running operations, `stream::*` for SSE subscriptions, and others.

For method semantics and request/response shapes, see the upstream reference documentation: <https://komodoplatform.com/en/docs/komodo-defi-framework/>. KDF Reloaded aims for API compatibility with that reference; this link is provided for interoperability and does not imply endorsement by Komodo Platform.

## Project layout

```
mm2src/         Workspace crates (Rust)
docs/           Developer documentation
LEGAL/          License, contributor agreement, third-party notices
.github/        CI workflows
```

Notable crates: [`mm2src/mm2_main/`](mm2src/mm2_main/) (entry, RPC, swaps, ordermatch), [`mm2src/coins/`](mm2src/coins/), [`mm2src/mm2_p2p/`](mm2src/mm2_p2p/), [`mm2src/crypto/`](mm2src/crypto/).

## Documentation map

These are the most important documents to read before building, integrating,
auditing, or redistributing the project:

- [`docs/reloaded-rewrite/00-overview.md`](docs/reloaded-rewrite/00-overview.md) — what the chapter set is, who it is for, and the recommended reading order.
- [`docs/reloaded-rewrite/01-clean-room-rules.md`](docs/reloaded-rewrite/01-clean-room-rules.md) — the clean-room / provenance methodology and its explicit limits.
- [`docs/reloaded-rewrite/30-provenance-attribution.md`](docs/reloaded-rewrite/30-provenance-attribution.md) — the index into the chapter set, cited inputs, and subsystem coverage map.
- [`docs/reloaded-rewrite/34-provenance-ledger.md`](docs/reloaded-rewrite/34-provenance-ledger.md) — file-level provenance classifications, inherited exceptions, and open license items.
- [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md) — operator-facing differences and compatibility posture relative to the GLEEC fork.
- [`LEGAL/LICENSING-POLICY.md`](LEGAL/LICENSING-POLICY.md) — the repository-wide licensing posture: old combined work GPL-2.0-only, original post-anchor contributions GPL-2.0-or-later, plus disclosed inherited tensions.
- [`RELEASE_CHECKLIST.md`](RELEASE_CHECKLIST.md) — the gate used before any tagged release or public binary distribution.
- [`SECURITY.md`](SECURITY.md) — disclosure policy and release-signing expectations.
- [`docs/DEV_ENVIRONMENT.md`](docs/DEV_ENVIRONMENT.md) and [`docs/UNIT_TESTS.md`](docs/UNIT_TESTS.md) — build, test, and CI expectations.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) and the [PR review checklist](docs/PR_REVIEW_CHECKLIST.md). All contributors must agree to the [Developer Agreement](LEGAL/DEVELOPER-AGREEMENT) and abide by the [Code of Conduct](CODE_OF_CONDUCT.md).

For the project roadmap beyond the beta, see [`ROADMAP.md`](ROADMAP.md). For the change log, see [`CHANGELOG.md`](CHANGELOG.md).

## License

The repository as a whole is distributed under **GPL-2.0-only**, inherited from the GPLv2 upstream base this project continues (which we do not have the right to relicense).

Original code authored by this project **after** the anchor commit is additionally offered by its authors under **GPL-2.0-or-later**; this does not change the GPL-2.0-only terms of the combined work, but grants downstream the "or later" option for our own contributions. Vendored or adapted third-party components keep their own licenses (e.g. MIT, Apache-2.0, GPL-3.0).

See [`LEGAL/LICENSING-POLICY.md`](LEGAL/LICENSING-POLICY.md) for the full posture and known open items, plus [`LEGAL/LICENSE`](LEGAL/LICENSE), [`LEGAL/COPYING`](LEGAL/COPYING), and [`LEGAL/THIRDPARTY-LICENSES`](LEGAL/THIRDPARTY-LICENSES).
