# Chapter 34 — Provenance Ledger

**Status:** driving-spec (meta-chapter).

> **One-sentence claim:** the project shall maintain a single, explicit
> provenance-ledger companion chapter that records file-level
> classification and source constraints for non-default-provenance
> artifacts, with all unlisted files treated as clean-room originals
> under chapter 01 rules.

This ledger records files in this repository whose provenance warrants
explicit documentation. It is a maintenance artifact for transparency and
consistency, not legal advice.

Files not listed here are clean-room original work governed by the rules
in [Chapter 01](01-clean-room-rules.md).

### Provenance model (hybrid, by design)

This project is openly **hybrid**. It does not claim — and has never claimed
— that every file is a clean-room original. Files fall into four provenance
classes:

1. **Clean-room originals** — independently authored under the
   [Chapter 01](01-clean-room-rules.md) rules. This is the default for any
   file not listed in this ledger.
2. **Constrained-expression fragments** — interop/wire-format, convergent
   idiomatic, generated, and third-party-API-bound files whose shape is
   dictated by an external spec, crate, ABI, or protocol. Byte/identifier
   identity here is expected and non-infringing under merger doctrine /
   de minimis / scènes à faire. Listed in §34.3.
3. **Adapted permissive-source** — code ported in place from a public,
   license-compatible upstream (recorded per CRD §1 R27). Listed in §34.3.
4. **GPLv2-inherited lineage content** — baseline code descending directly
   from the 2022 GPLv2 anchor, plus post-anchor modifications derived from
   the upstream/GLEEC KDF lineage, carried under GPLv2 copyleft. Basis in
   §34.5.

Because the project is explicitly hybrid, the Chapter 01 clean-room rules
— including R8's "forbidden input *when claiming clean-room derivation*" —
scope to class 1 only. Classes 2–4 are **not** clean-room claims and never
were; their similarity to upstream is expected and lawful under the basis
stated for each class. This scoping is the designed posture, not an
after-the-fact narrowing.

## 34.1 Schema

- **Destination** — path in this repository.
- **Classification** — one of:
  - `generated-artifact` — output of a code generator (CRD §1.11 R28).
  - `interop-reuse` — content whose bytes/identifiers must match an
    external specification or wire format (CRD §1.11 R29–R31).
  - `convergent-idiomatic` — short or canonical-pattern code where
    independent authors converge on the same shape (CRD §1.11 R32).
  - `third-party-api-bound` — shape dictated by an external crate, ABI,
    or protocol spec the file directly binds to (CRD §1.11 R33).
  - `baseline-carryforward` — predates the 2022 baseline anchor; governed
    by CRD §1.3 R3, not by the §1.11 categorisation.
  - `vendored-subtree` — a whole upstream project imported as a subtree
    under its own license.
  - `adapted-source` — code adapted or ported from a public,
    license-compatible upstream crate or project, recorded per CRD §1 R27.
    Distinct from `vendored-subtree`: the upstream is not imported wholesale
    but adapted in place.
  - `lineage-derived` — post-anchor code derived from the upstream/GLEEC
    KDF lineage (a derivative of GPLv2 mm2 code), carried under GPLv2
    copyleft per §34.5 basis (b). Explicitly **not** a clean-room original.
- **Source reference** — for `interop-reuse` and `third-party-api-bound`:
  the external spec, crate, or protocol that constrains the shape. For
  `generated-artifact`: the generator and its input. For
  `vendored-subtree`: the upstream project.
- **License basis** — for third-party-sourced content, the upstream
  license terms. For content derived from the upstream/GLEEC KDF *lineage*
  (not a third-party crate), the basis is the GPLv2 copyleft inheritance /
  constrained-expression analysis set out in §34.5, not a per-row license
  tag.
- **Notes** — operational context.

Per CRD §1.11 R34, a file's classification is the least-permissive
category of any fragment present in it.

## 34.2 Maintenance notes

### 2026-06-08 — repo-wide remediation pass and scope narrowing

- A repository-wide formatting/lint remediation attempt was run
  (`cargo fmt`; `cargo clippy --workspace --all-targets --all-features --fix --allow-dirty --allow-staged`).
- The pass touched a large unrelated surface; non-target edits were then
  rolled back via allowlist restore so only the intended provenance-remediation
  files remained modified.
- Additional strict local lint fixes were re-applied in
  `mm2src/mm2_main/src/lp_swap.rs`,
  `mm2src/mm2_main/src/lp_swap/maker_swap_v2.rs`,
  `mm2src/mm2_main/src/lp_swap/nft_maker_swap_v2.rs`, and
  `mm2src/mm2_main/src/lp_swap/taker_swap_v2.rs` to keep
  `cargo clippy --no-deps -p mm2_main -- -D warnings` green.
- Classification basis in this ledger is unchanged by this pass; no new
  classification category was introduced.

### 2026-06-12 — adapted-source reclassification and removed-dependency record

- Two `db_common` files previously treated as clean-room are reclassified
  `adapted-source`: `async_sql_conn.rs` and `async_conn_tests.rs` are an
  adaptation of `programatik29/tokio-rusqlite` (MIT). A new `adapted-source`
  classification (CRD §1 R27) is introduced in §34.1 to record them; see the
  *Adapted from public license-compatible source* table in §34.3. The MIT
  attribution is carried in `THIRDPARTY-LICENSES`, and both files carry a
  per-file header pointing to the upstream and that notice.
- `mm2_metamask` formerly depended on `GLEECBTC/rust-web3` (a fork of
  `tomusdrw/rust-web3`, MIT/Apache-2.0). Depending on a permissively-licensed
  crate is permitted regardless of who published the fork — a Cargo dependency
  pointer is a build-configuration reference, not source derivation. The
  dependency was removed in LP-17 Phase 4c and replaced by a clean-room
  EIP-1193 wrapper.

### 2026-06-13 — dependency-license confirmation and header correction

- WalletConnect SDK git dependencies (`pairing_api`, `relay_client`,
  `relay_rpc`, `wc_common`, pinned `tag = "k-0.1.3"`) point at
  `github.com/komodoplatform/walletconnectrust`, which GitHub now redirects to
  `GLEECBTC/WalletConnectRust` (a fork of `reown-com/reown-rust`). That upstream
  is **Apache-2.0**. As a Cargo git dependency it is a build-configuration
  reference, not source derivation.
- **Open license-compatibility finding (inherited from upstream KDF, not
  introduced here).** `LEGAL/LICENSE` distributes this project under
  **GPL-2.0-only** (the notice cites "version 2 ... as published by the Free
  Software Foundation" with no "or later" clause). Two components are in tension
  with GPL-2.0-only when combined into one distributed binary:
  1. The Apache-2.0 WalletConnect SDK crates above (Apache-2.0 is
     FSF-recognized one-way-incompatible with GPL-2.0-only).
  2. `mm2src/coins/eth/legacy_tx.rs`, a derivative of Parity's **GPL-3.0**
     `ethcore-transaction` (GPL-3.0 cannot be combined with GPL-2.0-only).
  Both conditions already existed in upstream Komodo DeFi Framework. Resolution
  is a licensing-policy decision for the maintainers/counsel — e.g. move the
  project to **GPL-3.0** or **GPL-2.0-or-later**, or invoke the exceptions
  mechanism noted in `LEGAL/LICENSE`. Flagged here; no code change made.
- `mm2_metamask` similarity sweep (audit follow-up to the rust-web3 removal):
  comment/whitespace-normalized similarity to the upstream lineage averages
  ~55% whole-file / ~26% body-only across its four files — below the rewritten
  set and below any clean-room concern threshold. `eip_1193_provider.rs` (which
  previously interfaced with the removed `GLEECBTC/rust-web3`) is now ~14% /
  ~11% similar (fully re-authored). No rewrite required.
- `mm2src/coins/eth/legacy_tx.rs` header corrected: removed a garbled
  "re-licensed by Parity under GPLv3" sentence. The file is a derivative work
  of Parity's GPL-3.0 `ethcore-transaction` and accurately remains under
  GPL-3.0.

### 2026-06-13 -- spec-provenance review of pinned code blocks (ch22-24)

- A review of the chapters whose blind implementations were rewritten this
  pass (22, 23, 24) checked whether any chapter relocates authorial
  expression into the CRD by over-pinning discretionary Rust shape rather
  than function or interface.
- **Chapter 23 §23.8** is the primary case: R11-D through R11-K pin the
  `UrlBuilder` private fields, the `build()` body step-by-step, the
  `SwapUrlBuilder`/`PortfolioUrlBuilder` marker names, local-variable names,
  and log-message wording. The URL grammar, provider constants, path tokens,
  header set, decode branches, and public method signatures are functionally
  dictated by the public 1inch v6.0 API (R33) / its wire envelope (R29); the
  surrounding identifiers and decomposition are discretionary.
- **Chapter 24 §24.7.3 (R-R6)** pinned statement-level control flow at the
  time of this review; its surrounding local naming is discretionary. (The
  Chapter 22 transaction-wording item originally noted here was removed
  entirely when Chapter 22 was clean-room rewritten — see the 2026-06-17
  note below.)
- **Resolution (no implementation rewrite).** Rule R36 (ch01) now states that
  CRD code blocks bind only functional and interface content; the
  discretionary identifiers and wording they show are informative and carry
  no clean-room obligation. A binding-scope note added to §23.8 records the
  dictated-vs-discretionary split explicitly. The shipped `client.rs` and
  `rpc_commands.rs` are not rewritten: parity with upstream is preserved, and
  their residual expressive similarity is deferred to the R35 gate (ch30 D1),
  under which a thin REST-path composer and a validate-then-store handler
  retain little protectable expression once dictated grammar and interface
  are excluded.

### 2026-06-13 — provenance-accuracy corrections (external-audit response)

Two provenance statements were tightened after an external audit:

- **`lp_ordermatch` orderbook.** The refactor-relocated entry previously
  described `ordermatch_orderbook.rs` as wholly "GPLv2 by descent". That
  overstated the facts: while the bulk is relocated pre-anchor body, the
  file also carries post-anchor additions absent at the anchor — notably the
  `recently_cancelled` logic (entered `de85bd5b4`, 2026-04-19), for which
  materially similar logic existed earlier in the GLEEC lineage (2024-10-04).
  The entry now records a **mixed basis** (descent for the relocated body,
  copyleft-inheritance basis (b) for the post-anchor additions). The
  `RELOADED_VS_GLEEC.md` and README wordings were corrected to match.
- **WalletConnect.** The substantive `mm2src/kdf_walletconnect/` files were
  not listed, so by the ledger default rule they implicitly read as
  clean-room originals. They are not: the crate is a post-anchor feature
  (introduced 2026-05-05) whose integration code is lineage-derived from the
  upstream/GLEEC KDF WalletConnect work product (same crate path, introduced
  in the lineage 2025-06-03). A new `lineage-derived` classification was
  added to §34.1 and the substantive crate files are now listed explicitly
  with GPLv2 copyleft basis (§34.5 b). No code changed; the legal basis is
  unchanged — only the documentation was made accurate. **(Superseded
  2026-06-17 — the WalletConnect crate was clean-room reimplemented and
  reclassified; see the next note.)**

### 2026-06-17 — WalletConnect clean-room reimplementation, reclassification, and companion CRD rewrites

- Chapter 22 was rewritten into a clean-room **driving-spec** (functional /
  interface / dictated-interop only), and the substantive
  `mm2src/kdf_walletconnect/` source files were **blind-reimplemented** from
  that spec — without consulting the forbidden corpus or any lineage source —
  and gated under R35 (residual-similarity) and R36 (binding-scope).
- The crate is therefore reclassified from `lineage-derived` to **clean-room
  original**. The substantive files are removed from the *WalletConnect
  integration* table (operating-rule §34.4(2)); being ledger-absent they are
  governed by the default clean-room rule. The genuinely thin files remain
  classified `third-party-api-bound` (`error.rs`, `session/rpc/extend.rs`) or
  `convergent-idiomatic` (`session/rpc/mod.rs`, `pairing.rs`) in §34.3.
- One Interop / wire-format reuse fragment (R29/R31) is retained and marked:
  the `open` on-disk session-record field names embedded in Chapter 22 §22.5.3,
  needed for byte-interop with GLEEC KDF session stores. No discretionary
  lineage expression accompanies it.
- **Basis shift.** For the WalletConnect crate the **primary** legal basis is
  now clean-room independence; the GPLv2 copyleft inheritance of §34.5(b) is
  retained only as a **backstop**. The `tools/clean_room_gate.py` manifest
  enrols the crate's source files against the pinned upstream reference so the
  R35 gate enforces this going forward.
- **Companion CRD rewrites (ch23, ch24, ch32).** Chapters 23 (trading-api
  client), 24 (GUI account-state), and 32 (orderbook P2P + trie) were rewritten
  as clean-room driving-specs in the same pass: verbatim bodies, private
  identifiers, helper-decomposition / call-chain tables, control-flow
  transcription, and diagnostic/format string literals were removed, leaving
  functional/behavioural contracts, public interface, and externally-dictated
  interop (R29/R31/R33) under R36 binding-scope notes. Each was independently
  Dirty-Gate verified. Chapter 32 additionally gained a *Baseline Verification*
  section epoch-classifying its components as pre-2022 baseline-carryforward
  (R3) or post-2022. These rewrites **supersede, at the CRD level**, the
  Chapter 23 §23.8 (R11-D…R11-K) and Chapter 24 §24.7.3 (R-R6) over-pinning
  items recorded in the 2026-06-13 *spec-provenance review* note above; the
  shipped code's residual expressive similarity remains deferred to the R35
  gate (ch30 D1) per the locked review order.

### 2026-08-04 — stable Zcash crates and narrow compatibility patches

- The obsolete `librustzcash-patched/` 0.5-era workspace dependency was
  replaced by exact published stable crates. The unused broad subtree was
  removed rather than retained as a second, stale implementation source.
- Three published crate sources are retained under `vendor-patches/` because
  Cargo's patch mechanism requires the patched package source in-tree. Each
  directory preserves its crates.io package metadata and MIT/Apache-2.0 license
  files and contains a `KDF-PATCH.md` describing the local delta.
- `zcash_client_backend 0.23.0` has a manifest-only change removing the obsolete
  exact `time-core 0.1.2` resolver workaround. `zcash_primitives 0.28.0` and
  `zcash_transparent 0.8.0` carry only the transaction-builder/P2SH extensions
  needed to preserve deployed KDF transaction bytes. Chapter 39 binds the
  resulting schema, scan, and transaction compatibility tests.

## 34.3 Entries

### Generated artifacts

| Destination | Classification | Source reference | License basis | Notes |
|---|---|---|---|---|
| `mm2src/coins/utxo/pb.rs` | generated-artifact | `prost-build` over `mm2src/coins/utxo/bchrpc.proto`, driven by `mm2src/coins/build.rs`. | Generator output; tracked file is regenerated from in-tree `.proto`. | Header begins `// This file is @generated by prost-build.`. Regeneration must be byte-identical; verify before each release. |

### Interop reuse — third-party authoritative source

| Destination | Classification | Source reference | License basis | Notes |
|---|---|---|---|---|
| `mm2src/coins/z_coin/service.proto` | interop-reuse | `PirateNetwork/lightwalletd` upstream `walletrpc/service.proto`. | MIT (Zcash developers; Pirate Chain developers). | Third-party MIT-licensed protocol spec. The `pirate.wallet.sdk.rpc` package is required for ARRR lightwalletd wire compatibility. |
| `mm2src/coins/z_coin/compact_formats.proto` | interop-reuse | `PirateNetwork/lightwalletd` upstream `walletrpc/compact_formats.proto`. | MIT (Zcash developers; Pirate Chain developers). | Same as above; compact block messages must share the same `pirate.wallet.sdk.rpc` package as the service. |
| `mm2src/coins/utxo/bchrpc.proto` | interop-reuse | `gcash/bchd` upstream `bchrpc/pb/bchrpc.proto` (Bitcoin Cash node RPC). | ISC (gcash/bchd project license). | Drives `mm2src/coins/utxo/pb.rs` generation. Upstream file does not carry a per-file copyright header; project license applies. |
| `mm2src/coins/eth/maker_swap_v2_abi.json` | interop-reuse | ABI of the deployed `EtomicSwapMakerV2` Solidity contract. | Derived from the deployed bytecode; ABI is a public derivation. | Bytes must match the deployed contract or `ethabi` calls fail at runtime. |
| `mm2src/coins/eth/taker_swap_v2_abi.json` | interop-reuse | ABI of the deployed `EtomicSwapTakerV2` Solidity contract. | Same as above. | Same as above. |

### Interop reuse — wire-format-only source

These files must match a wire format for which the only available concrete
source is the gleec/upstream KDF lineage. CRD §1.11 R31 applies: the
authoritative content is embedded (sanitized: identifiers preserved,
comments fresh) in the relevant CRD chapter at the point of use.

| Destination | Classification | Source reference | License basis | Notes |
|---|---|---|---|---|
| `mm2src/mm2_main/src/lp_swap/swap_v2.proto` | interop-reuse | KDF Swap V2 P2P wire format. No third-party spec exists; field numbers, message names, oneof variant tags must match for swap interop with upstream peers. | Wire-format identity required; the .proto declares the schema for P2P payloads. | R31 sanitization: upstream comments dropped, fresh commentary authored, all wire-identifying identifiers preserved exactly. Embedded in chapter 33. |

### Third-party-API-bound shape

Files whose shape is dictated by an external crate's API, an external
protocol specification, or an on-chain ABI path.

| Destination | Classification | Source reference | Notes |
|---|---|---|---|
| `mm2src/db_common/src/sql_create.rs` | third-party-api-bound | SQLite `CREATE TABLE` grammar + `sql_builder` crate API. | 270 lines; in-file comment documents the binding. |
| `mm2src/db_common/src/sql_update.rs` | third-party-api-bound | SQLite `UPDATE` grammar + `sql_builder` crate API. | 167 lines. |
| `mm2src/db_common/src/sql_value.rs` | third-party-api-bound | SQLite value-type taxonomy. | 149 lines. |
| `mm2src/db_common/src/sql_delete.rs` | third-party-api-bound | SQLite `DELETE` grammar + `sql_builder` crate API. | 118 lines. |
| `mm2src/db_common/src/sql_condition.rs` | third-party-api-bound | SQLite `WHERE` grammar + `sql_builder` crate API. | 187 lines. |
| `mm2src/db_common/src/sql_constraint.rs` | third-party-api-bound | SQLite `table-constraint` grammar. | 323 lines; in-file comment documents the binding. |
| `mm2src/coins/tendermint/ethermint_account.rs` | third-party-api-bound | Cosmos / Ethermint `BaseAccount` proto via `cosmrs::proto`, prost serde encoding. | 9 lines; prost-derived struct mirroring upstream Cosmos type. |
| `mm2src/coins/tendermint/htlc/mod.rs` | third-party-api-bound | Cosmos HTLC ABCI query paths (`/nucleus.htlc.Query/HTLC`, `/irismod.htlc.Query/HTLC`) and protocol state constants. | 302 lines; values fixed by the external Cosmos chains. |
| `mm2src/ledger/src/transport/apdu.rs` | third-party-api-bound | ISO 7816-4 APDU framing and standard response code values. | 76 lines. |
| `mm2src/trading_api/src/one_inch_api/classic_swap_types.rs` | third-party-api-bound | 1inch Swap API v6.0 public specification. | 425 lines. Magic-number bounds (`MAX_SLIPPAGE 50.0`, `MAX_FEE_SHARE 3.0`, `MAX_GAS 11_500_000`, `MAX_PARTS 100`, `MAX_MAIN_ROUTE_PARTS 50`, `MAX_COMPLEXITY_LEVEL 3`) match 1inch v6 documented limits. Query-parameter spellings (camelCase) are the 1inch URL contract. |
| `mm2src/trading_api/src/one_inch_api/portfolio_types.rs` | third-party-api-bound | 1inch Portfolio API public specification. | 99 lines. |
| `mm2src/kdf_walletconnect/src/error.rs` | third-party-api-bound | WalletConnect v2 specification error codes plus error variants from `pairing_api`, `relay_client`, `relay_rpc`. | 187 lines. |
| `mm2src/kdf_walletconnect/src/session/rpc/extend.rs` | third-party-api-bound | `relay_rpc::rpc::params::session_extend::SessionExtendRequest` + `ResponseParamsSuccess::SessionExtend` enum + internal `session_manager::extend_session` API. | 17 lines, three statements; reclassified from clean-room → third-party-api-bound during step 2.3 re-inspection — too thin to host substantive logic. |

### Convergent idiomatic shape

Files short enough or following a canonical pattern strongly enough that
independent authors converge on the same shape. The "Canonical pattern"
column identifies the convergent reference.

| Destination | Canonical pattern reference | Notes |
|---|---|---|
| `mm2src/db_common/src/lib.rs` | Rust module-organization (`pub mod` with cfg gates). | 24 lines. |
| `mm2src/mm2_err_handle/src/map_to_mm.rs` | `MmError` conversion pattern documented in repository `AGENTS.md`. | 41 lines. |
| `mm2src/mm2_err_handle/src/map_to_mm_fut.rs` | Same pattern, futures01 variant. | 69 lines. |
| `mm2src/mm2_err_handle/src/mm_json_error.rs` | Same pattern, JSON error wrapper. | 44 lines. |
| `mm2src/mm2_err_handle/src/map_mm_error.rs` | Same pattern, `MmError<E1> → MmError<E2>` adapter. | 64 lines. |
| `mm2src/mm2_err_handle/src/or_mm_error.rs` | Same pattern, `Option → MmResult` adapter. | 29 lines. |
| `mm2src/trading_api/src/lib.rs` | Rust module-organization. | 7 lines. |
| `mm2src/trading_api/src/one_inch_api.rs` | Rust module-organization. | 6 lines. |
| `mm2src/trading_api/Cargo.toml` | Cargo manifest format. | 32 lines; not Rust code. |
| `mm2src/common/shared_ref_counter/src/lib.rs` | Feature-gated re-export pattern. | 30 lines. |
| `mm2src/common/write_safe/fmt.rs` | Stdlib `fmt::Write` extension-trait pattern. | 68 lines. |
| `mm2src/common/write_safe/mod.rs` | Rust module-organization. | 4 lines. |
| `mm2src/common/write_safe/io.rs` | Stdlib `io::Write` extension-trait + macro pattern. | 31 lines. |
| `mm2src/mm2_rpc/src/mm_protocol.rs` | `MmRpc` protocol-types pattern. Builder + tagged enum result + `HttpStatusCode` mapping. | 253 lines; re-inspected in step 2.3, no substantive fragments found. |
| `mm2src/coins/build.rs` | Build-script driving `prost-build` / `tonic-build`. | 10 lines; the generator-driver, not the generated output. |
| `mm2src/coins/tendermint/ibc/mod.rs` | Module + constant declarations. | 6 lines. |
| `mm2src/coins/tendermint/htlc/nucleus/mod.rs` | Module declaration. | 7 lines. |
| `mm2src/coins/tendermint/rpc/mod.rs` | Cfg gates + tiny enum + `From` impl. | 24 lines. |
| `mm2src/coins/utxo/tx_cache/dummy_tx_cache.rs` | Canonical no-op trait impl returning empty results. | 20 lines. |
| `mm2src/coins/rpc_command/get_enabled_coins.rs` | Canonical RPC-handler + MmError pattern. | 48 lines. |
| `mm2src/coins/rpc_command/get_current_mtp.rs` | Same. | 73 lines. |
| `mm2src/hw_common/src/primitives.rs` | Re-export of external `bip32` crate types. | 11 lines. |
| `mm2src/ledger/src/error.rs` | Canonical error-enum pattern. | 14 lines. |
| `mm2src/kdf_walletconnect/src/session/rpc/mod.rs` | Module-organization. | 9 lines. |
| `mm2src/kdf_walletconnect/src/pairing.rs` | Thin async reply wrappers over `relay_rpc::rpc::params`. | 45 lines. |
| `mm2src/mm2_gui_storage/src/lib.rs` | Module-organization. | 3 lines. |
| `mm2src/mm2_gui_storage/src/context.rs` | `MmCtx`-style context-attachment pattern. | 24 lines. |
| `mm2src/mm2_main/src/for_tests/check_order_serde_payload.json` | Test fixture whose shape is the serde representation of the `Order` struct. | 29 lines; values arbitrary, shape dictated. |

### Adapted from public license-compatible source

Code adapted or ported in place from a public, license-compatible upstream
project, recorded per CRD §1 R27. High raw similarity to the upstream KDF
lineage for these files is explained by the shared upstream ancestor, not by
clean-room divergence.

| Destination | Classification | Source reference | License basis | Notes |
|---|---|---|---|---|
| `mm2src/db_common/src/async_sql_conn.rs` | adapted-source | `programatik29/tokio-rusqlite` — background-thread async `rusqlite` wrapper (`CallFn`, `Message::Execute`/`Close`, `call`/`call_unwrap`/`open_*` API, `Display` formatting, doc-comments). | MIT (GPLv2-compatible). | Adaptation: upstream `Connection` renamed `AsyncConnection`; `Internal(InternalError)` variant added. Upstream provenance (R14 closed): the `CallFn`/`Message` core originates at v0.1.0 (`a09c68af5617ce9f0a49668c196644d02a017f83`, 2022-04-25); the `Close((Connection, Error))` variant and `Result`-returning `call` at `d97e88dff22b54c88634f4ad80ecccaf946d2945` (2023-04-02); `call_unwrap` at `1c5a322d92f0a8d6c26f221fabc9fab3409bc1ab` (2023-06-22, `v0.4.0-2-g1c5a322`). The adapted surface therefore corresponds to the upstream **v0.4.x era** (state at `1c5a322`). Introduced here in `531634ebe` (2026-05-01). MIT attribution recorded in `THIRDPARTY-LICENSES`; per-file header added to both files. |
| `mm2src/db_common/src/async_conn_tests.rs` | adapted-source | `programatik29/tokio-rusqlite` test suite (`open_in_memory`/`call`/`call_unwrap` exercises). | MIT (GPLv2-compatible). | Same provenance and citation as `async_sql_conn.rs`. Uses the early rusqlite `NO_PARAMS` API. |

### Refactor-relocated baseline content (mixed: descent + post-anchor)

These files did not exist as separate files at the 2022 anchor. They were
created by an in-tree refactor (`284b66cd4`, 2026-05-13) that split the
`lp_ordermatch.rs` body (5,465 lines at the anchor) into smaller modules.
**Most** of their content is the pre-anchor GPLv2 `lp_ordermatch.rs` body
relocated unchanged — C-ported legacy code that predates the GLEEC fork
(GLEEC's own README describes `lp_ordermatch` as "parts ported from C
`as is`"); that portion is GPLv2 by direct descent from the anchor, and its
identity with the GLEEC fork reflects the shared pre-divergence ancestor.

**However, these files are not 100% anchor content.** They also carry
post-anchor additions that were not present at the anchor — most notably the
`recently_cancelled` order-tracking / stale-cancellation logic, which first
entered this tree on 2026-04-19 (`de85bd5b4`) and was later relocated into
`ordermatch_orderbook.rs` by the split. That logic is **not** "by descent";
it is a post-anchor modification of GPLv2 lineage code and is governed by
the copyleft-inheritance basis (b) in §34.5 (materially similar logic
existed in the GLEEC lineage earlier, from 2024-10-04). The earlier version
of this entry overstated the whole file as "GPLv2 by descent"; that is
corrected here.

| Destination | Classification | Source reference | Notes |
|---|---|---|---|
| `mm2src/mm2_main/src/lp_ordermatch/ordermatch_orderbook.rs` | baseline-carryforward (relocated) **+ post-anchor lineage additions** | Bulk: split of pre-anchor `lp_ordermatch.rs` body (basis a). Additions (e.g. `recently_cancelled`): post-anchor lineage-derived (basis b). | Orderbook propagation / cancellation logic. Mixed basis — see prose above. |
| `mm2src/mm2_main/src/lp_ordermatch/ordermatch_types.rs` | baseline-carryforward (relocated) | Split of pre-anchor `lp_ordermatch.rs` body. | Order / match type definitions; any post-anchor additions follow basis (b). |
| `mm2src/mm2_main/src/lp_ordermatch/ordermatch_trading.rs` | baseline-carryforward (relocated) | Split of pre-anchor `lp_ordermatch.rs` body. | Trading-side ordermatch logic; any post-anchor additions follow basis (b). |

### WalletConnect integration (post-anchor, clean-room reimplementation)

The entire `mm2src/kdf_walletconnect/` crate is a **post-anchor feature**
(introduced here 2026-05-05, `d34589ef7`); nothing in it exists at the 2022
anchor. The corresponding WalletConnect work also appeared in the
upstream/GLEEC KDF lineage. As of the 2026-06-17 clean-room pass (§34.2) the
crate's substantive files were **independently reimplemented** against
Chapter 22's clean-room driving-spec — a blind reimplementation that did not
consult the forbidden corpus or any lineage source — and were gated under the
R35 residual-similarity / R36 binding-scope rules (ch01, ch30). They are
therefore **clean-room originals** and, per operating-rule §34.4(2), are
**removed from this ledger**: being ledger-absent, they are governed by the
default clean-room rule in Chapter 01.

Three WalletConnect files remain explicitly listed elsewhere in §34.3,
because their shape is dictated rather than discretionary and is not a
clean-room claim:

- `mm2src/kdf_walletconnect/src/error.rs` and
  `mm2src/kdf_walletconnect/src/session/rpc/extend.rs` — *third-party-api-bound*
  (WalletConnect v2 / `relay_rpc` surface);
- `mm2src/kdf_walletconnect/src/session/rpc/mod.rs` and
  `mm2src/kdf_walletconnect/src/pairing.rs` — *convergent-idiomatic*.

One constrained-expression fragment is retained inside otherwise clean-room
files: the `open` on-disk session-record field names embedded in Chapter 22
§22.5.3 (an Interop / wire-format reuse fragment, R29/R31), required for
byte-interop with GLEEC KDF session stores. The fragment is marked at its
point of use and carries no discretionary lineage expression.

**License basis.** The primary basis for the WalletConnect crate is now
**clean-room independence** (R35/R36-gated reimplementation), enforced going
forward by the `tools/clean_room_gate.py` manifest, which enrols the crate's
source files against the pinned upstream reference. The GPLv2 copyleft
inheritance of §34.5(b) is retained only as a **backstop**: even were the
independence of any fragment disputed, the WalletConnect feature descends
from a GPLv2-licensed lineage and reaches us under GPLv2 regardless (§34.5).

### Baseline carryforward (pre-2022 anchor)

Files that predate the 2022 baseline anchor and are governed by CRD §1.3
R3, not by the §1.11 categorisation. Listed here only because the
inventory script flagged them.

| Destination | Classification | Notes |
|---|---|---|
| `mm2src/coins/utxo/tx_cache/mod.rs` | baseline-carryforward | Trait definition; would be third-party-api-bound if in scope. First-introduced 2022-05-10. |
| `mm2src/mm2_io/src/lib.rs` | baseline-carryforward | 2-line module declaration; would be convergent-idiomatic if in scope. First-introduced 2022-05-19. |
| `mm2src/crypto/src/bip32_child.rs` | baseline-carryforward | BIP32 typed wrappers; would be third-party-api-bound if in scope. First-introduced 2022-02-22. |
| `mm2src/coins/for_tests/ZOMBIE_CACHE.db` | baseline-carryforward | Binary SQLite fixture. First-introduced 2021-09-29. |

### Vendored subtrees

Whole upstream projects imported as subtrees under their own licenses.
Distinct from the in-tree fragment scheme.

| Destination | Classification | Source reference | License basis | Notes |
|---|---|---|---|---|
| `mm2src/ethabi-vendored/` | vendored-subtree | Upstream `rust-ethereum/ethabi` lineage (see in-tree crate metadata). | MIT/Apache-2 style upstream licensing. | Vendored for dependency control and compatibility. |
| `mm2src/testcontainers-vendored/` | vendored-subtree | Upstream `testcontainers-rs` lineage (see in-tree crate metadata). | Upstream permissive licensing per crate metadata. | Vendored for deterministic CI behaviour. |
| `vendor-patches/zcash_client_backend-0.23.0/` | vendored-subtree | Published crates.io `zcash_client_backend 0.23.0`. | MIT OR Apache-2.0; both license texts retained in-tree. | Manifest-only removal of the obsolete exact `time-core 0.1.2` dependency; see `KDF-PATCH.md`. |
| `vendor-patches/zcash_primitives-0.28.0/` | vendored-subtree | Published crates.io `zcash_primitives 0.28.0`. | MIT OR Apache-2.0; both license texts retained in-tree. | Narrow transaction-builder compatibility extension; see `KDF-PATCH.md`. |
| `vendor-patches/zcash_transparent-0.8.0/` | vendored-subtree | Published crates.io `zcash_transparent 0.8.0`. | MIT OR Apache-2.0; both license texts retained in-tree. | Narrow KDF P2SH/raw-output compatibility extension; see `KDF-PATCH.md`. |

## 34.4 Operating rules

1. Files not listed in this ledger are clean-room original work governed
   by the CRD. Do not describe a ledger entry as "clean-room-derived" in
   public documentation.
2. If a ledger entry is replaced by an independent reimplementation,
   remove its row and note the change in git history; the file then
   becomes ledger-absent (= clean-room).
3. Before public release, every entry classified `interop-reuse` whose
   source reference is the gleec/upstream KDF lineage must have a
   corresponding CRD chapter or appendix embedding its authoritative
   content per §1.11 R31.
4. Adding a new file to the tree without a CRD chapter covering it, or
   without a row here, is a policy violation.

## 34.5 License basis for upstream/GLEEC-lineage content

This section states *why* this project may distribute content that descends
from the upstream/GLEEC Komodo DeFi Framework lineage. It records the
project's licensing rationale; it is not legal advice and not a warranty.

### Anchor

KDF Reloaded is a **GPLv2-only continuation** anchored to upstream commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f` (2022-06-03), the last upstream
commit unambiguously distributed under GPL-2.0-only. Everything present at
the anchor is GPLv2 by direct descent.

### Two distinct bases (do not conflate)

**(a) Baseline / direct-descent content.** Files (or file bodies) that
exist at the anchor, or that are mechanical relocations of anchor content
(e.g. the `lp_ordermatch` refactor split in §34.3), are GPLv2 by direct
descent. Identity with the GLEEC fork for such content reflects the shared
pre-divergence ancestor, not a transfer from GLEEC. No further argument is
needed.

**(b) Post-anchor lineage-derived modifications.** Where post-anchor work
in this tree is a derivative of GPLv2 upstream/GLEEC-lineage code, GPLv2's
own terms govern the result:

- **GPLv2 §2(b):** a work that is a derivative of GPLv2 code, distributed
  as a whole, must be licensed under GPLv2 to all third parties. The mm2
  modifications in question are integrated derivative works of GPLv2 code,
  not separable independent works.
- **GPLv2 §6 (and equivalently GPLv3 §7 / §10):** each downstream recipient
  receives the GPL grant directly from the original licensors, and *no
  further restrictions* may be imposed on the exercise of those rights. On
  that basis we read a restrictive notice placed alongside GPL-covered code
  as an impermissible "further restriction" — in our view unenforceable /
  removable as to that code. This is the project's good-faith legal position,
  not a settled adjudication (see the disclaimer opening this section).

Consequently, GPLv2-derived lineage code reaches us under GPLv2 regardless
of any restrictive notice a downstream fork attaches to it, and we may
redistribute it under GPLv2.

### Per-file-class refinement

- **Trivial, short, convergent, or API-bound files** (e.g. 9-line module
  organisers, 24-line context-attachment glue, third-party-API type
  mirrors — see §34.3): byte-identity is legally innocuous under merger
  doctrine / de minimis / scènes à faire. The expression is constrained by
  the interface or is too thin to carry protectable original authorship.
  These are non-infringing independent of lineage.
- **Substantive lineage-derived files**: governed by basis (b) above
  (GPLv2 copyleft inheritance).

### WalletConnect crate (primary: clean-room; basis (b) as backstop)

The `mm2src/kdf_walletconnect/` crate is **not** carried under basis (b) as
its primary justification. Following the 2026-06-17 clean-room reimplementation
(§34.2), its primary basis is clean-room independence, R35/R36-gated. Basis
(b) is retained only as a fallback: the WalletConnect feature descends from a
GPLv2-licensed lineage, so even a disputed-independence fragment reaches us
under GPLv2 and is redistributable under GPLv2 regardless.

### Due-diligence finding on the GLEEC fork (public information)

As of this writing, the public `GLEECBTC/komodo-defi-framework` repository
(a fork of `jl777/SuperNET`) ships a GPL `COPYING` file under `LEGAL/`
— currently **GPL version 3** — alongside a restrictive copyright notice.
The presence of a GPL `COPYING` is what matters: under GPLv2 §6 (and GPLv3
§7/§10 alike) the side notice cannot strip GPL rights from GPL-covered
code. This project does **not** rely on pulling from GLEEC's current tree;
our lineage basis is direct descent from the 2022 GPLv2 anchor. The GLEEC
posture is recorded here only as corroboration that the upstream lineage is
GPL-licensed.

### What this basis does *not* cover

The GPLv2 copyleft theory does not resolve two genuinely open,
separately-sourced compatibility items (they are not GPLv2-lineage
derivatives):

- the Apache-2.0 WalletConnect SDK crates, and
- `mm2src/coins/eth/legacy_tx.rs` (a GPL-3.0 derivative of Parity
  `ethcore-transaction`).

Both pre-exist in upstream KDF and remain disclosed open items in §34.2 and
in `LEGAL/LICENSING-POLICY.md`; their resolution is a maintainer/counsel
licensing-policy decision, not a provenance question.
