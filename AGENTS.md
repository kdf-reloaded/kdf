# KDF Reloaded contributor instructions

These instructions apply to the entire repository. More-specific `AGENTS.md`
files may add local requirements, but they must not weaken the clean-room,
provenance, compatibility, or verification rules below.

## 1. Project mission and governing constraints

KDF Reloaded is a GPLv2 continuation of the Komodo DeFi Framework: a
multi-chain, peer-to-peer atomic-swap daemon intended to remain interoperable
with deployed KDF-family peers, wallets, nodes, and tooling.

Treat this as protocol and funds-handling software:

- Preserve wire, RPC, configuration, transaction, signing, and persistent-data
  compatibility unless an intentional exception is already documented or the
  task explicitly requires one.
- Prefer the smallest implementation that restores or extends the documented
  contract. Do not perform opportunistic refactors in compatibility-sensitive
  paths.
- Never weaken validation, key handling, transaction checks, or error handling
  merely to make a test pass.
- Support both production networks, netid `8762` and netid `6133`. Supporting
  both concurrently is a deliberate KDF Reloaded divergence; network-specific
  behavior must be selected by the active `NetConfig`, not by changing one
  network to behave like the other.
- The combined repository is distributed under GPL-2.0-only. Original
  post-anchor KDF Reloaded contributions are offered under
  GPL-2.0-or-later, while vendored/adapted components retain their own recorded
  licenses. Preserve existing headers and provenance records.

Read these sources before substantial work:

1. `README.md`
2. `LEGAL/LICENSING-POLICY.md`
3. `docs/reloaded-rewrite/00-overview.md`
4. `docs/reloaded-rewrite/01-clean-room-rules.md`
5. `docs/reloaded-rewrite/02-baseline-state.md`
6. The CRD chapter(s) governing the subsystem being changed
7. The relevant top-level document(s) under `docs/`
8. The role definitions under `.github/agents/`

The Clean-Room Documentation (CRD) under `docs/reloaded-rewrite/` is the
technical derivation record. When source and a governing chapter disagree,
investigate the mismatch and apply chapter 01's source-and-chapter consistency
rules; do not silently choose one.

## 2. Clean-room wall

The last unambiguously GPL-2.0-only baseline is commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f` dated 2022-06-03. The baseline,
eligible pre-anchor material, public specifications, dictated interfaces and
wire formats, permitted sibling sources, public-network observations, and
independent work are the allowed input classes described by CRD chapter 01.

The post-anchor upstream/GLEEC implementation corpus is forbidden input for a
normal contributor or implementation LLM. On the usual development host it is
located at:

```text
/home/tomas_admin/kdf-analysis-2022/
```

Unless you are explicitly running the `KDF Spec Reader` or `KDF Dirty Gate`
role:

- Do not read, list, search, index, diff, summarize, or otherwise inspect that
  path.
- Do not inspect post-anchor upstream/GLEEC source, comments, commits, diffs,
  issue discussions, or derived analyses through another checkout, remote
  website, cache, generated file, or user-provided excerpt.
- Scope every filesystem and text search to this repository. From the
  repository root, prefer `rg <pattern> <explicit-path>` and never search from
  `/home/tomas_admin`.
- If forbidden content appears accidentally, stop reading it, do not use it,
  and report the contamination risk.

Do not claim that every existing file is a clean-room rewrite. The repository
has a documented hybrid provenance: baseline-derived GPLv2 code,
clean-room-authored work, permissively licensed adaptations, generated or
interop-bound artifacts, and narrowly recorded lineage-derived components.
Classify and document provenance honestly according to chapter 01 and
`docs/reloaded-rewrite/34-provenance-ledger.md`.

### The two-team workflow

Use the role files under `.github/agents/` exactly as intended:

- `kdf-spec-reader.agent.md` is the dirty-side specification author. It is the
  only implementation-analysis role allowed to inspect the forbidden corpus.
  It writes clean-channel behavioral/interface requirements into the relevant
  CRD chapter and never edits implementation code.
- `kdf-dirty-gate.agent.md` compares a candidate chapter with the corpus and
  returns only its sanitized verdict. A Spec Reader change is not ready to
  cross the wall until this gate passes.
- `coder.agent.md` is the clean-side implementer. It reads the approved chapter
  and this repository, never the forbidden corpus, and implements only the
  chapter-bound behavior.

Do not combine the dirty and clean roles in one context. A dirty-side agent
must not implement code, and an agent that has seen forbidden implementation
expression must not become the clean-side implementer.

## 3. Upstream-version and network compatibility policy

Compatibility research must use an explicit reference version; never treat an
unqualified branch tip as universal truth.

### Stable legacy reference

The forbidden corpus has an important stable reference tag:
`v2.6.0-beta`.

- `v2.6.0-beta` is the primary behavioral reference for swaps on netid `8762`.
- Preserve its observable netid-8762 behavior, including fee arithmetic,
  discounts, transaction output count/order/scripts/values, serialization,
  validation tolerances, signing behavior, state transitions, and RPC/wire
  shapes.
- A newer branch must not silently overwrite the `v2.6.0-beta` contract for
  netid `8762`.

### Later v3 development

Features added after `v2.6.0-beta` may be specified from the upstream `dev`
branch or a more recent applicable branch in the unreleased v3 series, using
the two-team clean-room workflow above.

- The v3 lineage is the primary reference for v3 behavior and, where
  applicable, netid `6133` behavior.
- Add later features without breaking the legacy behavior retained from
  `v2.6.0-beta`.
- When stable and v3 behavior differ, encode the distinction explicitly
  (normally through network configuration, protocol/version negotiation, or
  an existing compatibility boundary) and test both sides.
- Do not blend constants or control decisions from different reference
  versions into a new, unverified hybrid.

### Compatibility target

The result should be wire compatible with the applicable original reference,
except for deliberate KDF Reloaded exceptions documented in the CRD and
operator-facing compatibility documents. Concurrent support for netids `8762`
and `6133` is one such exception.

"Wire compatible" includes more than matching a final numeric amount. It also
includes, as applicable:

- exact rational fee calculation and the point at which rounding occurs;
- transaction output count, order, value, and script;
- serialized field names, enum tags, byte order, and protocol identifiers;
- signature-hash modes and signed preimages;
- RPC request/response/error shapes;
- P2P negotiation and state-machine behavior;
- persisted schema and migration behavior.

For every swap or fee change, build a compatibility matrix covering at least:

| Dimension | Required cases |
| --- | --- |
| Reference | `v2.6.0-beta` legacy and applicable v3 behavior |
| Network | `8762` and `6133` |
| Coin role | taker and maker |
| Asset policy | discounted and non-discounted ticker |
| Fee form | standard, burn form, and no-fee case when applicable |
| Protocol | every affected negotiated swap version |

Test the exact structure peers validate, not merely aggregate values.

## 4. Repository map and ownership

- `mm2src/mm2_main/`: daemon startup, RPC dispatch, order matching, swap state
  machines, and orchestration.
- `mm2src/coins/`: coin traits and implementations, transaction construction,
  chain-specific validation, activation, and swap operations.
- `mm2src/mm2_net_config/`: compile-time per-netid parameters and the supported
  network registry. Network-specific fee policy belongs here.
- `mm2src/mm2_p2p/` and related networking crates: peer-to-peer transport and
  protocol behavior.
- `docs/reloaded-rewrite/`: CRD driving specifications and provenance record.
- `docs/`: developer/operator procedures and compatibility documentation.
- `LEGAL/`: operative license and attribution material.

Keep responsibilities separated:

- Network policy is selected in `mm2_net_config`.
- Pure fee arithmetic should remain deterministic and side-effect-free.
- Coin layers translate a fee descriptor into chain-specific transaction
  outputs and validate the same structure.
- Swap state machines carry negotiated values; they should not duplicate
  coin-layer transaction rules or network constants.

## 5. Implementation discipline

Before editing:

1. Inspect `git status` and preserve all pre-existing user changes.
2. Read the complete governing CRD chapter and relevant source/tests.
3. State the compatibility cases and observable failure being fixed.
4. Prefer a regression test that fails for the reported behavior before
   changing implementation.

While editing:

- Make surgical changes and follow surrounding style.
- Use typed errors and explicit propagation. Do not introduce `unwrap()`,
  `expect()`, or `panic!()` in production code except unavoidable constant
  initialization already justified by project convention.
- Do not log passphrases, private keys, mnemonics, session secrets, raw
  authorization headers, or unredacted RPC payloads.
- Keep public APIs documented, including `# Errors` and `# Panics` where
  relevant.
- Preserve externally dictated names and bytes exactly. Internal names and
  helper decomposition should be independently authored.
- Do not add speculative abstractions, global compatibility modes, or
  unrelated cleanup.
- If behavior intentionally diverges from an applicable upstream/GLEEC
  contract, follow `docs/COMPAT_SWITCHES.md`: provide the required scoped
  opt-back mechanism and update both its local documentation and
  `docs/GLEEC_COMPATIBILITY.md`, unless the divergence is an already documented
  project-wide exception.

For CRD edits:

- Keep the canonical status and R/T/D/V section shape from chapters 00 and 01.
- State observable behavior and public/dictated interfaces, not private
  upstream expression.
- Update a governing chapter in the same commit as the source behavior it
  describes.
- Do not write `Forbidden corpus: not consulted` unless that statement is true
  for the authoring context or the chapter has completed the documented
  Spec Reader and Dirty Gate workflow.

## 6. Formatting, tests, and verification

Start with focused checks for the affected crate and expand in proportion to
the risk. Bug fixes require deterministic regression coverage.

Use the repository's pinned formatter, scoped to packages you changed:

```sh
cargo +nightly-2026-05-08 fmt -p <package>
cargo +nightly-2026-05-08 fmt -p <package> -- --check
```

Do not run bare workspace-wide `cargo fmt`; patched vendor trees must not be
mechanically reformatted.

Typical verification:

```sh
cargo test -p <package> <focused-test-filter>
cargo check -p <package>
cargo clippy -p <package> --all-targets --no-deps -- -D warnings
git diff --check
```

Add `--all-features` only when it is relevant and supported by that package.
Use a separate dependency-wide lint only when the task changes dependencies or
patched vendor code; do not turn unrelated warnings in vendored trees into
opportunistic edits.
For WASM-only work, also check `--target wasm32-unknown-unknown`. Full
integration suites may require Docker, chain parameters, live endpoints, or
test passphrases; consult `docs/DEV_ENVIRONMENT.md`,
`docs/TEST_ENV_VARS.md`, and `docs/DISABLED_TESTS.md` before running them.
Never turn a required regression test into an ignored or network-dependent
test when a deterministic unit test can cover it.

For compatibility-sensitive swap changes, verify all affected crates and run
the focused fee/transaction/state-machine tests for both production netids.
Check exact rational values before base-unit conversion and exact transaction
outputs after conversion.

## 7. Documentation and release hygiene

Update every user- or operator-facing document made stale by the change. Common
locations include:

- `docs/NETWORK_CONFIG.md` for supported-netid policy;
- `docs/GLEEC_COMPATIBILITY.md` and `docs/COMPAT_SWITCHES.md` for deliberate
  divergences;
- the governing CRD chapter for implementation contracts;
- `README.md`, `CHANGELOG.md`, or `RELOADED_VS_GLEEC.md` when the public
  behavior changes.

Treat old operational notes carefully: some documents retain explicitly
superseded historical instructions. Verify live workflow files, manifests, and
toolchain pins before relying on commands that can drift.

Do not create releases, tags, push commits, alter remotes, or mutate external
services unless the user explicitly asks. When asked to commit, stage only the
in-scope files, review the staged diff, and use a concise imperative commit
message. Never discard or overwrite unrelated worktree changes.

## 8. Definition of done

A change is complete only when:

- the reported behavior is reproduced or otherwise evidenced;
- the correct reference-version/network contract is explicit;
- implementation and governing CRD agree;
- focused regression tests cover the failure and both sides of relevant
  compatibility branches;
- formatting and applicable checks pass;
- public/operator documentation is consistent;
- provenance and license requirements are preserved;
- the final diff contains no unrelated changes, secrets, generated junk, or
  forbidden-corpus expression.
