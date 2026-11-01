# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@AGENTS.md

## Clean-room wall: read this before anything else

These rules are not optional. They apply to every Claude Code session and every subagent that
touches this repository, including sessions started in another directory that only
reach this repository through an added working directory.

1. **Never read the post-anchor upstream/GLEEC implementation** unless you are dispatched as the
   KDF Spec Reader or KDF Dirty Gate role. That covers the corpus under
   `/home/tomas_admin/kdf-analysis-2022/`, and equally any other checkout, clone, cache, web page,
   GitHub API response, issue discussion or excerpt of the GLEEC/Komodo KDF source
   (`GLEECBTC/komodo-defi-framework`, formerly `KomodoPlatform/komodo-defi-framework`, and local
   forks of it). Running their published release binaries as black boxes is allowed. It counts
   as a public-network observation.
2. **"What does upstream do?" is always a Spec Reader question.** For a bug, a compatibility
   question or a feature, investigate with this repository, public specifications, chain data and
   observed network behavior. Anything about upstream behavior goes to a KDF Spec Reader
   subagent that writes it into the governing CRD chapter. The chapter then passes KDF Dirty Gate,
   and the clean Coder implements from the gated chapter only. The role definitions are in
   `.github/agents/`.
3. **A context that has seen upstream implementation expression never implements**, not even a
   "small obvious fix". It may orchestrate: dispatch the Spec Reader, the Dirty Gate and the Coder,
   and review and commit the Coder's work. It must not pass upstream expression into the Coder's
   prompt.
4. **A suspected breach halts commits and pushes** and is reported to the maintainer, as
   `AGENTS.md` §2 says.

On the maintainer's machine, a Claude Code hook (`~/.claude/hooks/kdf-cleanroom-guard.py`)
enforces rules 1 and 3 mechanically. It is a safety net, not a substitute for following the
rules.

The imported `AGENTS.md` above is the governing contributor contract for this
repository: project mission, the clean-room wall and forbidden-corpus rules,
the two-team (Spec Reader / Dirty Gate / Coder) workflow, upstream-version and
netid-`8762`/`6133` compatibility policy, repository ownership map,
implementation discipline, formatting/test/verification commands, and
definition of done. Read it in full before doing substantial work — it is not
optional background, and this file does not restate it. If anything below
appears to conflict with `AGENTS.md`, `AGENTS.md` wins.

This file only adds Claude-Code-specific operational detail that `AGENTS.md`
doesn't cover: concrete build commands and a map of the workspace's crate
groupings.

## Build (not covered by AGENTS.md, which only specifies fmt/test/clippy)

Full manual: `docs/DEV_BUILD.md` (also see `docs/DEV_ENVIRONMENT.md`,
`docs/WASM_BUILD.md`). Toolchain channel is pinned by `rust-toolchain.toml`.

```sh
# Debug build (from repo root)
cd mm2src && cargo build --bin kdf --target x86_64-unknown-linux-gnu --profile dev

# Release build
cd mm2src && cargo build --bin kdf --target x86_64-unknown-linux-gnu --release

# Whole-workspace offline unit tests (AGENTS.md's focused per-package
# commands are preferred day-to-day; this is the full sweep)
cargo test --bins --lib
```

Windows uses the same commands with `--target x86_64-pc-windows-msvc`; macOS/WASM/iOS/Android are only partially documented in `docs/DEV_BUILD.md`.

### Build directories: one per worktree, never inside it

Parallel work here happens in worktrees under `local/worktrees/<name>/`. Each worktree
builds into its own directory `local/targets/<name>/` of the main checkout, set through
`CARGO_TARGET_DIR`, and never into a directory inside the worktree:

```sh
# Run from the worktree root; resolves to <main checkout>/local/targets/<worktree name>
export CARGO_TARGET_DIR="$(git rev-parse --path-format=absolute --git-common-dir)/../local/targets/$(basename "$(git rev-parse --show-toplevel)")"
```

- **One directory per worktree, never shared.** Two branches must not build into the same
  target directory. A branch that regenerates code at build time (for example `z_coin`'s
  protobuf module) leaves its output for the other branch to pick up, which breaks it in
  either direction. The dangerous case is a build that passes against the wrong generated
  code. Do not point two worktrees at one `local/targets/` entry, and do not reuse a
  removed worktree's entry for a new one. Delete the entry when you remove the worktree.
- **Outside the worktree, because git cannot see it there.** Git treats a worktree's root
  as the repository root, so the main checkout's `/local/` ignore rule does not apply
  inside a worktree. A build directory created at a worktree root shows up as an untracked
  top-level directory that a single `git add -A` would commit. That is how a 1.9 GB cargo
  output directory once ended up at a worktree root. `local/targets/` lies inside the main
  checkout's ignored `local/` and outside every worktree, so no worktree's git ever sees
  it, and it needs no `.gitignore` or `exclude` entry.
- **The main checkout keeps its own `target-private/`.** That directory at the repository
  root predates this convention. It is the main checkout's own private target directory,
  excluded through the local `.git/info/exclude`, and it follows the same
  one-directory-per-checkout rule. This convention covers worktrees only and does not
  move it.

### Before saying a change is verified

`AGENTS.md` §6 "Targets a plain `cargo check` does not cover" is binding here.
The short version: the host build compiles neither the WASM targets nor the
feature-gated test binaries, so passing it proves less than it appears to.

```sh
# WASM — required whenever coins, mm2_db or mm2_main changed at all,
# not only for work that is "about" WASM.
cargo check --target wasm32-unknown-unknown -p coins
cargo check --target wasm32-unknown-unknown -p mm2_db
cargo check --target wasm32-unknown-unknown -p mm2_main

# Feature-gated test binary — required for dependency/version/feature changes.
# --no-run builds it without needing a Docker daemon.
cargo test --no-run --bin docker_tests --features regtest-netid
```

Both have already caught defects that the host build accepted: a borrow
`wasm32-unknown-unknown` rejects, and a `rand`/`secp256k1` version mismatch
visible only in the Docker test binary. Read `.github/workflows/` for the
current authority; the gated-target set is not static.

## Workspace crate groupings

`Cargo.toml` lists ~40 workspace members under `mm2src/`. Beyond the
ownership map in `AGENTS.md` §4, it helps to know these fall into distinct
provenance/purpose groups:

- **`kdf_*` crates** (`kdf_chain`, `kdf_codec`, `kdf_crypto`, `kdf_keys`,
  `kdf_primitives`, `kdf_rpc_types`, `kdf_script`, `kdf_spv_validation`,
  `kdf_walletconnect`, `kdf_test_helpers`) — KDF Reloaded's own clean-room
  replacements for legacy `mm2_bitcoin`-derived UTXO/primitive code (see each
  crate's module doc comment and the relevant CRD chapter for its specific
  derivation). New UTXO/primitive work generally belongs here, not in
  `mm2_bitcoin`-style code.
- **`mm2_*` crates** — original AtomicDEX/KDF core infrastructure (RPC
  plumbing, networking, state machines, error handling, config, metrics,
  event streaming) inherited from the pre-anchor baseline and extended.
  `mm2_main` is the daemon entry point and orchestration layer;
  `mm2_net_config` is where per-netid (`8762`/`6133`) parameters live.
- **`coins` / `coins_activation`** — per-chain trait implementations,
  transaction construction/validation, and coin activation flows; the
  largest and most compatibility-sensitive area of the codebase.
- **Hardware wallet integration** — `trezor`, `ledger`, `hw_common`.
- **Vendored/adapted trees** — `rust-lightning-patched`, `core2-shim`,
  `testcontainers-vendored`, and patched dependencies noted in `Cargo.toml`;
  do not run bare workspace-wide formatting/lints over these (see
  `AGENTS.md` §6).

Session-scoped implementation notes for in-progress CRD chapters live under
`.memories/session/` (e.g. `ch28-impl-state.md`) — check there for
continuation context before re-deriving state on a chapter you're resuming.
