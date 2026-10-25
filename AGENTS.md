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
- A suspected or confirmed clean-room wall breach halts all `git commit` and
  `git push` activity in this repository immediately — every in-flight
  change, not only the one that triggered it — until the situation is
  investigated and cured. Report it to the maintainer privately and
  promptly; do not commit or push while remediation is pending.

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
  CRD chapter and never edits implementation code. It may also update facts
  and step premises in `docs/plans/*.md` under the same clean-channel
  discipline — never a plan's `**Status:**` line — as its own "Plan documents"
  section bounds.
- `kdf-dirty-gate.agent.md` compares a candidate chapter with the corpus and
  returns only the sanitized verdict CRD chapter 01 R39 bounds: a leak
  report, a scope attestation every verdict must carry, and optionally a
  labelled non-leak section. A Spec Reader change is not ready to
  cross the wall until this gate passes.
- `coder.agent.md` is the clean-side implementer. It reads the approved chapter
  and this repository, never the forbidden corpus, and implements only the
  chapter-bound behavior.

Do not combine the dirty and clean roles in one context. A dirty-side agent
must not implement code, and an agent that has seen forbidden implementation
expression must not become the clean-side implementer.

### Requesting code-quality analysis from KDF Spec Reader

KDF Spec Reader's job is distilling behavior into CRD chapters, not auditing
code for correctness — do not turn every dispatch into a code review. By
default it only surfaces a finding that falls out of its normal reading with
little extra effort, or one a code comment states outright (see its own
"Incidental code-quality findings" section for the exact triggers).

When dispatching it, decide deliberately whether this run also needs an
explicit request to look harder, based on what kind of task it is:

- Routine chapter authoring or a scoped rewrite: leave the default alone: do
  not ask for extra scrutiny.
- Chasing a bug where the relevant chapter already exists, already passed the
  Dirty Gate, and accurately describes what the code does — and the bug
  persists anyway: this shape means the chapter is faithfully documenting
  code that was already wrong at the source, not that the chapter or the
  implementation drifted from it. Always explicitly ask Spec Reader to
  analyze the original (corpus) code for correctness in this specific area,
  not merely to re-confirm the chapter still matches it — this is the one
  case where a deeper look is mandatory, not optional.
- A user-reported bug in an area with no chapter yet, or one being rewritten
  from scratch: reasonable to ask for it too, since the authoring pass is
  already reading the relevant code closely and a second pass would be pure
  overhead.

Either way, a finding is informative, not a mandate — decide what to do with
it the same way you would any other CRD content: verify it against this
repository's actual code before acting, and gate any chapter change it
produced through KDF Dirty Gate before treating it as final.

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
- Never substitute a placeholder value — an empty/default/zero stand-in, or a
  fallback branch on a conversion that looks fallible but your data's actual
  type makes infallible — for data you do not have at a call site, in a
  funds-moving or state-machine transition path. Propagate a typed error or
  abort instead. If the real fix needs a design decision you cannot make in
  this change, stop and follow the stub policy (`unimplemented!()` + a
  recorded reason) rather than shipping a value that lets the code compile
  but cannot do its job.
- For a new or touched state-machine transition, check every state that can
  actually reach it and confirm each one's field set carries what the
  transition needs — do not infer completeness from the code compiling and
  the happy-path test passing. A compiler and a happy-path test cannot see
  that a downstream abort path needs data an upstream state already dropped.
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
- The provenance footer's *Forbidden corpus* item is a statement about the
  chapter's inputs, not about what an authoring context did. Write it in one of
  the two forms CRD chapter 01 R20 binds — the blanket `not consulted` trailer,
  or a scoped record naming what was taken, for which sections, and under which
  reference versions — and only where the chapter contains no R8–R10 material.
  Where forbidden-class input was taken, it must have been taken through the
  two-team procedure of CRD §1.4A, which R38 requires.
- Before finishing any edit to a CRD chapter — whether authored via `KDF
  Spec Reader` or edited directly in a session — identify every other
  chapter that references it, is referenced by it, or states a binding
  requirement the changed subsystem must satisfy. Check each for continued
  accuracy; fix or cross-reference as needed in the same pass. See
  `kdf-spec-reader.agent.md`'s "Cross-chapter consistency" section for the
  full procedure when the Spec Reader role is doing the authoring.

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

**Do not run bare `cargo clippy --workspace`.** It fails outright — the
vendored, patched `rust-lightning-patched/lightning` crate trips 13
deny-level clippy lints against its own source (confirmed 2026-08-26:
`cargo clippy -p lightning --no-deps` exits nonzero, "could not compile
`lightning` (lib) due to 13 previous errors"). This does **not** affect the
per-package pattern above — `cargo clippy -p <package> --all-targets
--no-deps -- -D warnings` never clippy-invokes `lightning` as its own primary
target, so it lints your package's first-party code correctly regardless
(confirmed the same day: `cargo clippy -p coins --no-deps -- -D warnings`
exits 0 and reports only `coins`' own diagnostics). The only thing that
breaks is a literal `--workspace` sweep, or a manual `-p lightning`/
`-p lightning-invoice` invocation. If a task genuinely needs a
workspace-wide sweep (a periodic audit, not routine per-crate work), use
`RUSTFLAGS="--cap-lints=warn" cargo clippy --workspace --all-targets`
instead of the bare form.
Full integration suites may require Docker, chain parameters, live endpoints, or
test passphrases; consult `docs/DEV_ENVIRONMENT.md`,
`docs/TEST_ENV_VARS.md`, and `docs/DISABLED_TESTS.md` before running them.
Never turn a required regression test into an ignored or network-dependent
test when a deterministic unit test can cover it.

### Targets a plain `cargo check` does not cover

The default host build compiles neither the WASM targets nor the
feature-gated test binaries, so a change can pass every local check and still
break CI. Two distinct failures have reached `dev` this way: a borrow the host
accepts but `wasm32-unknown-unknown` rejects, and a dependency-version mismatch
that only surfaced when the `docker_tests` binary was built.

**Check the target when your change could plausibly affect it — not only when
the work is "about" that target.** The wasm break came from ordinary streaming
code; the rand break came from a `secp256k1` upgrade. Neither author was
working on wasm or on Docker.

| Trigger | Also run |
|---|---|
| Anything in `coins`, `mm2_db`, or `mm2_main` | `cargo check --target wasm32-unknown-unknown -p <coins\|mm2_db\|mm2_main>` |
| Dependency, version, or feature changes | the wasm checks above **and** `cargo test --no-run --bin docker_tests --features regtest-netid` |
| Anything touching swap, key, or RNG code | as above, plus the focused CI jobs listed below |

`--no-run` is enough to catch build breakage in the Docker binary without a
running Docker daemon; the suite itself still needs the environment described
in `docs/DEV_ENVIRONMENT.md`.

The cheap focused jobs CI runs, worth mirroring before pushing a broad change:

```sh
cargo test -p coins_activation --lib
cargo test -p mm2_main --lib ordermatch_tests
cargo test -p coins --lib rpc_response_tests
cargo test -p kdf_spv_validation
```

`.github/workflows/` is the authority on what CI builds. Read it rather than
relying on this list when a change is wide or touches the build itself; other
gated targets exist (for example `trezor-emulator-tests`) and the set moves.

Where a test suite has known-failing cases in your environment — several
require network access — establish the baseline before judging your own change:
run the suite on an unmodified checkout, then compare failing test *names*, not
counts. A differing name that passes in isolation is environment flakiness; a
new name that fails in isolation is a regression.

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
- formatting and applicable checks pass, **including the targets a host
  `cargo check` does not build** — see §6 "Targets a plain `cargo check` does
  not cover". Claiming a change is verified on the strength of the host build
  alone is how the two known CI breakages reached `dev`;
- public/operator documentation is consistent;
- provenance and license requirements are preserved;
- the final diff contains no unrelated changes, secrets, generated junk, or
  forbidden-corpus expression;
- passing `cargo check`/`test`/`clippy` is a floor, not evidence of
  completeness — it was true of both real gaps a 2026-08-25 external review
  found in `lp_swap/` (CRD ch.52 D7, D8). Before calling a change verified,
  grep your own diff for placeholder/fallback patterns (`unwrap_or(`,
  `::default()` used as a stand-in, empty-value substitutions, `TODO`) in
  funds-moving or state-machine code; any hit ships either fixed, or as an
  explicit stub paired with a same-commit CRD Deferred Work entry — never as
  a silent comment only.
