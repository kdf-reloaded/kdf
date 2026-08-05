# Plan: `librustzcash` upgrade with schema-safe rescan

> **Status (2026-08-05):** The operator selected the stable candidate:
> `zcash_client_backend` 0.23.0, `zcash_client_sqlite` 0.21.1, and
> `zcash_primitives`/`zcash_proofs` 0.28.0. The production port, shared
> `rusqlite` 0.37 migration, Reloaded-specific database namespace, strict
> legacy-schema rebuild/rescan path, and deterministic regression suite are
> implemented. Native package, WASM, and Windows GNU release checks pass, and
> the documentation records the selected schema and compatibility policy.
> Repeat Windows artifact analysis also identified and corrected the Pirate
> compact-wire metadata and prunable-frontier boundaries required by the
> modern scanner.
> Live Native-mode shielded activation is not claimed: the pre-existing native
> full-node-to-modern-compact-cache adapter remains an open chapter-39 gap.
> Pre-existing shielded-sync
> worktree changes were preserved and integrated rather than overwritten.

## Goal and non-negotiable policy

Upgrade the vendored, anchor-era Zcash stack to a coherent modern release so
that KDF Reloaded can use the current wallet and scanning contracts without
changing its public activation, transaction, or wire behavior.

Prefer published crates.io dependencies over repository-vendored copies. If a
dictated KDF network contract cannot be represented by an unmodified release,
keep any local patch as narrow, reviewable, and independently documented as
possible instead of retaining an entire modified upstream workspace by
default. The target-selection report must distinguish an unavoidable
compatibility patch from vendoring retained only for convenience.

Once the selected Zcash line is integrated, update directly related old
dependencies to newer stable releases where that can be done without widening
wire or persistence behavior. Prioritize dependencies reported by Rust/Cargo
as deprecated or future-incompatible. Record every independent blocker: do not
claim that `librustzcash` is the only blocker unless the dependency graph and
focused compile checks demonstrate it.

An existing wallet database that uses the old schema is **never migrated in
place**. Detection of that schema must cause a controlled wallet-database
rebuild followed by a rescan from the resolved start point. This is a
funds-safety boundary: no old witness or commitment-tree state may be treated
as valid under the new store.

## Hard acceptance gates

The selected target is not accepted until all applicable gates pass:

1. `coins` and `coins_activation` compile on the supported native target, and
   the affected code compiles for every required repository target. Final
   verification includes the Windows GNU release build; WASM is checked where
   the selected crates support the repository's existing WASM surface.
2. Deterministic shielded activation, scan, balance, and history tests pass.
3. An old-schema fixture proves that wallet open performs a rebuild and rescan,
   not an in-place migration or partial reuse.
4. Rescan start-point resolution and the post-rescan balance/history results
   remain correct.
5. `init_z_coin` keeps the existing RPC wire shape, including activation result
   fields and `init_z_coin_status` phase names, counters, and error shapes.
   Rebuild progress must use the existing `UpdatingBlocksCache` and
   `BuildingWalletDb` contract accurately.
6. Transaction construction, key/address encoding, and note recovery retain
   their dictated network behavior. Exact serialized transactions are checked
   where an affected API can change them.
7. The selected dependency graph passes licensing/provenance review and the
   repository's pinned format, focused test, check, clippy, and diff checks.

## Pre-upgrade stack

Before this upgrade, the repository consumed path dependencies under
`librustzcash-patched/` from `mm2src/coins`:

| Crate | Current version |
| --- | --- |
| `zcash_client_backend` | 0.5.0 |
| `zcash_client_sqlite` | 0.3.0 |
| `zcash_primitives` | 0.5.0 |
| `zcash_proofs` | 0.5.0 |
| `zcash_note_encryption` | local 0.0.0 component |

The old scan/store API includes `scan_cached_blocks`, `WalletDb`, `BlockDb`,
the `init_*_table` helpers, `get_balance`, and `get_update_ops`. Modern releases
replace this with wallet read/write traits, scan ranges, shard storage, and
batched scanning. The upgrade is therefore an API and schema port, not a Cargo
version-only change.

## Phase A candidates

Version discovery is pinned to 2026-08-04. Each line is coherent: KDF must not
mix independently newest crate versions when their public types belong to
different `zcash_primitives` releases.

### Candidate 1 — newest coherent stable crates.io line

| Crate | Version |
| --- | --- |
| `zcash_client_backend` | 0.23.0 |
| `zcash_client_sqlite` | 0.21.1 |
| `zcash_primitives` | 0.28.0 |
| `zcash_proofs` | 0.28.0 |
| `zcash_note_encryption` | 0.4.2 (resolved from the line's `0.4.1` constraint) |

The client crates declare Rust 1.85.1 or newer. This is the conservative
candidate: published stable artifacts, smaller provenance ambiguity, and less
pre-release dependency churn. It is one release line behind the newest
upstream APIs and may omit fixes or performance work only present in the RC
line. One published-manifest exception is now known: backend 0.23.0 pins
`time-core = 0.1.2`, which prevents Cargo from selecting the patched
`time >= 0.3.47` required by `RUSTSEC-2026-0009`. Candidate 1 is acceptable
only with a narrow audited removal of that obsolete pin or an equivalent
upstream-published stable fix; ignoring the advisory is not the default plan.

### Candidate 2 — newest coherent upstream-tag line

Start with official annotated tag `zcash_client_sqlite-0.22.0-rc.7` (tag object
`8069481c0ed2a9ab799e5e3cfd2d64160d057841`, peeled commit
`34f2b1e810ac8d00e671f0d254c7af7048c8985c`):

| Crate | Version at the tag |
| --- | --- |
| `zcash_client_backend` | 0.24.0-rc.7 |
| `zcash_client_sqlite` | 0.22.0-rc.7 |
| `zcash_primitives` | 0.30.0 |
| `zcash_proofs` | 0.30.0 |

This workspace declares Rust 1.88 or newer. It is closest to current upstream
and carries the newest scan/store work, but it is a release-candidate line with
a larger and partly pre-release dependency surface. If this tag fails before
KDF callsite compilation for a toolchain, target, or dependency reason that
cannot reasonably be ported, Candidate 2 moves backwards through official
tags until the newest viable upstream tag is found; every retreat is recorded.

### Stable-release refresh

The 2026-08-04 crates.io registry metadata and official upstream tags agree:

- `zcash_client_backend` has maximum stable version `0.23.0`; `0.24.0` exists
  only as release candidates through `0.24.0-rc.7`.
- `zcash_client_sqlite` has maximum stable version `0.21.1`; `0.22.0` exists
  only as release candidates through `0.22.0-rc.7`.
- `zcash_primitives` and `zcash_proofs` have stable `0.30.0` releases, but the
  stable client pair depends on their coherent `0.28` line. Substituting 0.30
  directly would duplicate incompatible public Zcash types; it is not a newer
  stable client candidate.
- Official upstream `main` at `257f63215567e87f653f8cce8eafc74c3fce5671`
  still declares backend `0.24.0-rc.7`, SQLite `0.22.0-rc.7`, and stable
  primitives/proofs `0.30.0`.

There is therefore no newer non-RC client stack than Candidate 1 at this
checkpoint.

### Objective comparison

Both candidates receive the same isolated `cargo check` and API inventory.
The human decision report will score:

| Dimension | Evidence |
| --- | --- |
| API churn | Distinct failing KDF files and removed/changed symbols |
| Store/scan risk | Required schema, scan-range, checkpoint, and trait changes |
| Platform fit | Native, Windows GNU, and applicable WASM dependency viability |
| Test risk | Existing deterministic tests affected or blocked |
| Provenance/licensing | New direct and transitive dependency burden |
| Release risk | Stable crates versus upstream pre-release components |

At this checkpoint no final target had been selected; the production port was
held until the operator reviewed the two reports. The operator subsequently
selected Candidate 1 after the additional corpus, stable-release, unvendoring,
and dependency-hygiene questions below were answered.

### Spike results

The current working-tree baseline passed `cargo check -p coins` natively and
`cargo check -p coins --target wasm32-unknown-unknown`. Each candidate was then
tested in an ignored copy of the current working tree.

Both candidates have the same graph-wide prerequisite: modern
`zcash_client_sqlite` requires `rusqlite 0.37`, while `db_common` pins 0.24.2.
Cargo refuses two crates with the `sqlite3` `links` key. Temporarily moving the
isolated copy to 0.37 exposed API updates in `db_common`,
`lightning_persister`, and `kdf_walletconnect` before `coins` could compile.
This shared database upgrade is production work required by either choice; it
must be a separately reviewed part of the final port, not hidden inside the
Zcash changes.

After temporary, spike-only adaptation of that shared API, both native builds
reached KDF Zcash callsites and reported the same 30 first-order errors:

| Local file | Candidate 1 | Candidate 2 | First-order removed/changed surface |
| --- | ---: | ---: | --- |
| `z_coin.rs` | 11 | 11 | account ID, consensus/memo/Sapling/ZIP32 paths, amount/output, tree visibility |
| `z_coin_wallet_db.rs` | 7 | 7 | account ID, balance/account/block init, consensus/ZIP32, compact spend/output types |
| `z_htlc.rs` | 6 | 6 | consensus/legacy/memo, amount/outpoint/output, generic builder error |
| `z_coin_sapling_cache.rs` | 2 | 2 | private/moved commitment tree and Sapling node |
| `z_coin_errors.rs` | 2 | 2 | generic builder error type |
| `get_private_keys.rs` | 2 | 2 | constants and ZIP32 paths |

These are first-order import/type errors; they prevent Rust from type-checking
several deeper callsites, so 30 is not the full port size. In particular, the
modern transaction builder, wallet traits, and scan driver have incompatible
signatures even where their names still exist.

On WASM, both candidates initially failed because the direct primitives
dependency enabled its default `multicore` feature, which `halo2_proofs` rejects
on `wasm32` without atomics. A target-specific dependency with
`default-features = false` and only `std` plus `transparent-inputs` removed that
platform failure. Both candidates then compiled through the modern Zcash
dependencies and stopped at the same 13 KDF relocation errors in `z_coin.rs`
and `z_coin_sapling_cache.rs`. This feature split is required whichever target
is selected.

The newest RC tag is therefore buildable on the repository's current Rust
1.93.1 toolchain and did not need a retreat to an older upstream tag. It does,
however, add deeper API work not visible in the identical first error set:
`ShieldedPool`/`LockFilter` note selection, anchor-retention state, pool
migration, PCZT-era types, and Ironwood/V6 builder paths.

### Equal-weight risk score

Scores run from 1 (low risk) to 5 (high risk). Each dimension has equal weight;
the score is a selection aid, not an acceptance substitute.

| Dimension | Candidate 1: stable | Candidate 2: RC tag | Evidence |
| --- | ---: | ---: | --- |
| KDF API churn | 4 | 5 | Same first 30 errors; RC adds `LockFilter`, `ShieldedPool`, Ironwood/V6 and larger builder changes |
| Store/scan risk | 4 | 5 | Both require shardtree and wallet traits; RC adds anchor-retention/pool-migration contracts |
| Platform fit | 3 | 3 | Both need shared `rusqlite 0.37` and the same WASM feature split |
| Regression-test risk | 4 | 5 | Both touch dictated tx bytes; RC has more selectable pool/version behavior to constrain |
| Provenance/licensing burden | 3 | 4 | Stable crates need one manifest-only security patch; RC uses an upstream Git tag with additional pre-release dependencies; full transitive audit remains required |
| Release stability | 2 | 5 | Published stable APIs with an obsolete dependency pin versus unreleased RC client crates and pre-release transitive components |
| **Total (lower is safer)** | **20** | **27** | Candidate 1 still has the smaller compatibility surface, but is not usable as a completely unmodified registry graph |

**Recommendation:** select Candidate 1 plus the narrow `time-core` manifest
fix, unless the clean-room reference comparison identifies a concrete later
behavior KDF must retain. The isolated fix allows `time 0.3.47` /
`time-core 0.1.8` to resolve and reaches the same 30 KDF port errors as the
unpatched stable spike; it introduces no additional source/API port. This
avoids taking unrelated unreleased pool and transaction-version work while KDF
is replacing its wallet schema and preserving legacy Sapling/ZHTLC behavior.
Candidate 2 avoids this particular manifest patch and resolves a fixed `time`
line, but is justified only if that unvendoring advantage or a concrete RC-only
correctness/performance change outweighs its larger pre-release surface.

### Crates.io-first integration assessment

The isolated Candidate 1 spike already resolves the four Zcash crates by exact
crates.io version, so the current full `librustzcash-patched/` subtree is not
needed merely to obtain or build the stable release. The current subtree's
recorded local changes—dependency bumps, generated-code lint allowances, and
an old timestamp-conversion fix—belong to the old API and do not justify
re-vendoring the full modern workspace.

Candidate 1 cannot be a literally registry-only graph today. The backend's
obsolete exact `time-core` pin is unconditional even when KDF does not enable
the Tor feature for which it was introduced. Official upstream removed the pin
when it raised its MSRV to 1.88, but has not published that change in a stable
0.23 patch release. A spike that removed only this dependency line resolved
`time 0.3.47` and `time-core 0.1.8`, cleared the Zcash-added advisory, and
stopped at exactly the same 30 KDF callsite errors. Production should carry
that as a small, explicit crate patch (or consume an equivalent future stable
release), not retain the complete upstream workspace.

The spike identified one compatibility question around KDF's
`ZcoinConsensusParams`, which accepts configuration-supplied coin type,
Sapling HRPs, transparent prefixes, and activation heights. Modern
`zcash_protocol::consensus::Parameters` also requires a closed `NetworkType`.
The production port resolved this without another crate patch:
`network_type_hint` supplies only the stock-network hint required by modern
internal unified-key persistence, while all externally visible derivation,
encoding, address, activation-height, and transaction behavior continues to
use the configured KDF values. Existing address/key tests and the new exact
transaction vectors guard that boundary.

Production retains the exactly pinned `zcash_keys 0.14.0` `unstable` feature
for one required symbol:
`UnifiedFullViewingKey::from_sapling_extended_full_viewing_key`. This converts
KDF's existing Sapling-only viewing key into the account form required by the
modern wallet store. No other unstable Zcash API was adopted; the exact pin
and focused account/open/rescan tests constrain this use until upstream
publishes a stable constructor for the same operation.

### Related dependency-hygiene assessment

The targeted Cargo audit distinguishes dependencies actually gated by this
upgrade from independent maintenance work:

| Dependency or warning | Current cause | Relationship to this upgrade |
| --- | --- | --- |
| `rusqlite 0.24.2` and suppressed deprecated `NO_PARAMS`-era APIs | Shared by `db_common` and vendored `zcash_client_sqlite 0.3`; SQLite's `links` key prevents adding modern `rusqlite` alongside it | **Directly gated and mandatory.** Both candidates require `rusqlite 0.37`, so the Zcash port must upgrade `db_common` and affected storage consumers together. |
| Stable backend's `time-core 0.1.2` pin | Published backend 0.23.0 retains a workaround that blocks fixed `time >= 0.3.47` | **Candidate-1-specific patch required.** Removing only the obsolete pin is validated at dependency-build level; Candidate 2 already removed it. |
| Old `bitvec 0.18.5` / `bellman 0.8.1` / `bls12_381 0.3.1` Zcash crypto chain | Pulled by the vendored Zcash 0.5-era crates | **Directly removed or modernized** by either candidate. Some similarly named old transitive crates can remain through unrelated chains and must be checked after resolution. |
| `protobuf 2.23` Zcash compact messages/codegen | Pulled by the old backend and also used directly by `coins` to convert compact blocks | **Partly gated.** Modern Zcash uses prost messages; the port should remove the old conversion/codegen path where the message contracts match, then re-check whether any direct protobuf 2 use remains. |
| `nom 6.1.2` future-incompatibility (`rust-lang/rust#79813`) | Pulled only through pinned `sia-rust`; its current upstream HEAD still declares `nom = "6.1.2"` | **Independent blocker.** A `sia-rust` update or narrow dependency patch is required; upgrading `librustzcash` cannot clear this compiler warning. |
| Deprecated `web_sys` builder methods in the WASM check | Local callsites use old setter names | **Independent callsite work.** It does not require a Zcash or crate-version change. |
| Vendored rust-lightning manifests defaulting to Rust 2015 | Separate `rust-lightning-patched/` subtree | **Independent vendoring work** and not a librustzcash acceptance blocker. |
| `ruint 1.18.0` advisory and yanked `crossbeam-channel 0.5.1` in the current lock | Existing Alloy/workspace dependency choices; also present before the Zcash spike | **Independent graph hygiene.** `ruint 1.20.0` resolves in a dry run; update and test separately. The shared crossbeam pin also needs a separately scoped lock/dependency update. |

Accordingly, `librustzcash` is the sole direct blocker for the shared
`rusqlite` upgrade and several old Zcash transitive crates, but it is not the
only blocker for every Rust/Cargo warning in the `coins` graph. Keep the latter
items separately scoped so the funds-sensitive Zcash port is not obscured by a
broad dependency rewrite.

## Phase B — API-impact matrix

Inventory every local use of the Zcash crates and classify it as:

- **Must change:** the old symbol or contract is absent or structurally
  incompatible in the candidate.
- **Must verify:** the symbol remains but semantics, feature gates, types, or
  serialization may have changed.
- **Likely unchanged:** both symbol and relevant contract remain compatible;
  this still receives compile/test coverage.

The matrix covers wallet open/store, block cache and scanning, transaction
builder/prover, keys and address encoding, note recovery/decryption, consensus
parameters, and the `mm2_main` activation/status response boundary.

### Symbol-level impact matrix

The classifications are the same for both candidates unless the final column
calls out an RC-only increment.

| Area and local files | Current symbols/contracts | Classification | Required port or verification |
| --- | --- | --- | --- |
| Configured consensus (`z_coin.rs`, `lp_coins_context.rs`) | `consensus::Parameters`, `NetworkUpgrade`, config-provided coin type/HRPs/prefixes | **Must change** | Modern consensus types moved to `zcash_protocol`; its `Parameters` obtains constants from a closed `NetworkType`. Preserve KDF's config authority with a narrow reviewed compatibility design instead of silently mapping every chain to stock Main/Test/Regtest. |
| Sapling keys (`z_coin.rs`, `get_private_keys.rs`, wallet DB/tests) | `ExtendedSpendingKey`, `ExtendedFullViewingKey`, `ChildIndex`, `PaymentAddress`, `OutgoingViewingKey` | **Must change** | Types moved to `sapling`/`zcash_keys`/`zip32`; rebind derivation and import a Sapling-capable unified viewing key into the new wallet account model. Verify every encoded key/address vector. |
| Encoding (`z_coin.rs`, `z_swap_ops.rs`, `get_private_keys.rs`, tests) | backend `encode_*`, `decode_*` | **Must verify** | Backend re-exports still exist but are deprecated; bind directly to `zcash_keys::encoding` and prove encoded bytes/strings remain identical for configured HRPs. |
| Amount/memo/transparent types (`z_coin.rs`, `z_htlc.rs`) | `Amount`, `MemoBytes`, legacy `Script`, `TxOut`, `OutPoint` | **Must change** | Use `zcash_protocol::value::Zatoshis`, `zcash_protocol::memo`, `zcash_script`, and `zcash_transparent` types; retain checked conversions, output order/value/script, locktime, and sequence. |
| Transaction serialization (`lp_coins.rs`, `z_coin.rs`, `z_coin_ops.rs`, `z_swap_ops.rs`, tests) | `Transaction::read(reader)`, `write`, `txid`, public fields | **Must change** | Modern read requires a consensus branch ID and bundle access is accessor-based. Add exact legacy transaction vectors so parsing/writing and branch selection cannot drift. |
| Transaction builder (`z_coin.rs`, `z_coin_ops.rs`, `z_htlc.rs`, `z_coin_errors.rs`) | `Builder::new(params,height)`, raw Sapling/transparent inputs, `build(branch, prover)` | **Must change** | Modern builder needs `BuildConfig`, full-viewing-key spends, typed transparent input/signing data, RNG, separate spend/output provers, and a fee rule. Ensure external fee subtraction is not applied twice. RC additionally exposes Ironwood/V6 paths. |
| Prover loading (`z_coin.rs`) | `LocalTxProver::new` / `from_bytes` | **Likely unchanged** | Constructors and parameter hashes remain compatible in both candidates; verify integration through the new builder and preserve existing typed pre-load/hash errors. |
| Manual transaction decryption (`z_coin_ops.rs`) | `decrypt_transaction(params,height,tx,Map<AccountId, ExtFVK>)` | **Must change** | Modern call takes optional mined/tip heights and unified full viewing keys. Verify output index, note value/rseed, and witness lookup semantics. |
| Fee output recovery (`z_swap_ops.rs`, `z_coin.rs`) | `try_sapling_output_recovery`, `DEX_FEE_OVK` | **Must change** | Recovery now uses Sapling-domain/note-encryption APIs and accessor-based outputs. Preserve the fixed OVK, destination, amount, memo, and validation failure behavior byte-for-byte. |
| Commitment cache (`z_coin.rs`, `z_coin_sapling_cache.rs`, `z_coin_ops.rs`) | `CommitmentTree`, `IncrementalWitness`, `Hashable`, `Node`, serialized per-block trees | **Must change** | Legacy exports are private/moved; decide whether the auxiliary cache can use the modern legacy API safely or should share the shard store. Never reuse old witnesses after the schema switch. |
| Wallet open/account setup (`z_coin_wallet_db.rs`, `z_coin_ops.rs`) | `WalletDb::for_path(path,params)`, `init_wallet_db`, `init_accounts_table` | **Must change** | New `WalletDb` is generic over connection/clock/RNG, `for_path` needs those capabilities, and initialization runs a migrator. Detect and rebuild a legacy DB before calling it; create/import the wallet account through `WalletWrite`. |
| Block cache/checkpoint (`z_coin_wallet_db.rs`) | `BlockDb`, `init_cache_database`, `init_blocks_table`, raw `sapling_tree` | **Must change** | Compact cache survives conceptually, but checkpoint seeding becomes `ChainState`/frontier plus shardtree state. Exact checkpoint height/hash/tree interpretation needs deterministic tests. |
| Scan driver (`z_coin_wallet_db.rs`, `z_coin_ops.rs`) | `get_update_ops`, four-argument `scan_cached_blocks` | **Must change** | Use `WalletWrite`, `ChainState`, from-height and bounded limit; integrate `suggest_scan_ranges` where required without changing activation progress semantics. RC error/runner types are broader. |
| Balance/note selection/history (`z_coin_wallet_db.rs`, `z_coin_ops.rs`) | `get_balance`, `get_target_and_anchor_heights()`, old `select_spendable_notes` result/witness | **Must change** | Use wallet summary and modern confirmation/target types. Stable selection already changes shape; RC additionally requires pool and lock filters. Preserve balance/history/internal-ID RPC semantics. |
| Compact protobuf conversion (`z_coin_wallet_db.rs`) | `CompactBlock/Tx/Spend/Output::new`, old top-level message fields | **Must change** | Modern prost messages nest spend/output types and use different construction/access. Verify compact-block bytes and heights before scanning. |
| Activation/RPC boundary (`coins_activation/z_coin_activation.rs`, `mm2_main` test structs) | `UpdatingBlocksCache`, `BuildingWalletDb`, `ZcoinActivationResult`, `first_sync_block` | **Likely unchanged** | No direct Zcash API dependency; freeze JSON snapshots and verify counters/status ordering during a forced rebuild and normal reopen. |

## Phase C — schema strategy and clean-room checkpoint

Before implementation, define deterministic legacy detection using only the
approved CRD contract: schema/version markers plus the required old
tables/columns. Detection must be conservative and distinguish:

- an absent database (create a new store);
- a recognized old database (move aside or replace safely, then full rescan);
- a current database (open normally);
- an unknown/corrupt database (typed failure; do not delete it silently).

The operator authorized a KDF Spec Reader to compare `v2.6.0-beta` with the
latest `dev` head, including their Zcash dependency lines and schema behavior.
The reader pinned `v2.6.0-beta` at
`475cdb49bc343a8fefdc2caaa1635d5ec426990b` and refreshed `dev` at
`e686ef3500585f01c9f0e89c8c01bc036c42253c`, then added sanitized
§39.8.0.4 to chapter 39. A fresh KDF Dirty Gate returned **PASS** with
discretionary-similarity score `0.00`, so the clean side may rely on
R39.8.0r--R39.8.0z, T39.8.0a--T39.8.0d, D39.8.0a, and
V39.8.0a--V39.8.0b.

The completed checkpoint answered:

1. The exact legacy wallet-DB and compact-cache table/column/index/foreign-key
   signatures created or accepted for Z-coin activation.
2. Every durable version or migration marker (`PRAGMA user_version`, migration
   tables/IDs, or the confirmed absence of such a marker).
3. Whether more than one legacy schema variant must be recognized, and the
   minimum positive fingerprint that distinguishes each from an empty,
   modern, unknown, or corrupt SQLite database.
4. The observable wallet-open behavior relevant to safely retaining the old DB
   before KDF Reloaded applies its deliberate **always rebuild/rescan** policy.

Both KDF references resolve the same public Komodo Zcash tag `k-1.4.2` at
`4e030a0f44cc17f100bf5f019563be25c5b8755f`: backend 0.5.0, SQLite 0.3.0,
primitives/proofs 0.5.0, and note-encryption 0.0.0. Their native wallet/cache
schemas and permissive open behavior are identical; neither reference uses a
modern stable or RC librustzcash generation. There is therefore no corpus-based
reason to prefer Candidate 2.

The approved chapter now binds the exact six-table legacy wallet and one-table
compact-cache fingerprints, `user_version = 0`, absence of a migration/version
table, conservative absent/legacy/current/unknown-or-corrupt classification,
and the requirement to intercept a legacy database before a modern initializer
can migrate it in place. The selected modern generation's exact migration-ID
set remains deferred by D39.8.0a until the operator chooses Candidate 1 or 2.

## Execution phases

1. **Phase A — lock target and gates:** run both isolated spikes, report pros,
   cons, scores, and a recommendation; wait for the human target choice.
2. **Phase B — API matrix:** map and classify all local callsites. This can run
   alongside the spikes.
3. **Phase C — schema checkpoint:** obtain the approved, sanitized legacy and
   current-dev signatures, bind the forced-rebuild contract in chapter 39, and
   record the reference dependency lines. This blocks implementation of schema
   detection and any corpus-derived target-selection claim.
4. **Phase D — dependency spike:** preserve reproducible manifests and compile
   logs for both candidates, grouped by local file and symbol.
5. **Phase E — store/scan port:** adapt `z_coin_wallet_db` to the selected
   wallet, shard store, checkpoint, and scanning contracts while preserving
   activation semantics.
6. **Phase F — wallet operations:** align builder/prover integration in
   `z_coin_ops.rs` and `z_htlc.rs`, then key/address/note-recovery callsites in
   `z_coin.rs`, `z_swap_ops.rs`, and `get_private_keys.rs`.
7. **Phase G — forced rebuild/rescan:** wire the four-way schema detector into
   wallet open, preserve the old database recoverably, create the new store,
   and rescan with accurate progress reporting.
8. **Phase H — regressions:** cover old detection, forced rescan, resolved scan
   starts, balance/history consistency, and RPC response parity.
9. **Phase I — dependency hygiene:** after the Zcash integration compiles,
   replace broad vendoring with crates.io dependencies wherever the compatibility
   design permits and update directly related deprecated/future-incompatible
   crates to stable releases. Prove or disprove that `librustzcash` was their
   only showstopper with dependency-tree and focused compile evidence.
10. **Phase J — verification/docs:** run scoped pinned formatting, tests,
    checks, clippy, native and Windows GNU release gates; update chapter 39,
    this plan, and `CHANGELOG.md` with the selected target and final policy.

## Proposed CI follow-up for shielded activation and synchronization

> **Status:** analysis and recommendation only. These jobs and additional
> fixtures have not yet been implemented.

The shielded CI should be layered so deterministic persistence and scanning
regressions block every pull request, while expensive proof construction and
unreliable public-service checks run at an appropriate cadence.

| Layer | Recommended cadence | Required result |
| --- | --- | --- |
| Modern/legacy database contract | every pull request, blocking | deterministic pass |
| Synthetic compact-block scanning | every pull request, blocking | deterministic pass |
| Full Light-mode `init_z_coin` with local services | every pull request, blocking once the harness exists | deterministic pass |
| Real Sapling prover/spend integration | Z-coin/dependency changes and nightly; promote to every pull request if runtime permits | deterministic pass |
| Live public ARRR service smoke | nightly, non-blocking | diagnostic signal |
| Windows database lifecycle | Z-coin/dependency changes or nightly | deterministic pass |

### Mandatory database and scan contract

1. Add a dedicated CI matrix cell for the existing focused suite:

   ```sh
   cargo test --locked -p coins --lib z_coin::z_coin_wallet_db::tests
   ```

   This is separate from the existing `coins_activation` library job; the
   current filtered `coins` cells do not execute the shielded store tests.
2. Pin the selected modern database semantically rather than hashing SQLite
   file bytes: assert `PRAGMA user_version`, the ordered migration-ID count and
   digest, and the canonical table/column/constraint/index/foreign-key record
   count and digest. Run `PRAGMA quick_check` after creation, scan, and reopen.
3. Make the legacy and compact-cache test goldens independent of the production
   classifier constants. Derive a test-owned semantic fixture from approved
   chapter 39 so a classifier constant and its fixture cannot drift together
   while tests still pass.
4. Verify the whole rebuild boundary: an exact legacy schema under a Reloaded
   filename is backed up byte-for-byte, replaced with the selected current
   schema, and fully rescanned to the expected balance and history. The
   GLEEC/pre-upgrade filenames remain byte-for-byte untouched. Unknown or
   corrupt Reloaded files remain untouched and return a typed error.
5. Expand deterministic compact-block cases to received notes, spent notes,
   shielded change, reopen/resume, tip-only incremental scanning, idempotent
   replay, and failure without state advancement for height gaps, wrong hash
   links, malformed payloads, and short streams. Exercise fetch boundaries
   around 500 blocks and scan-progress boundaries around 1,000 blocks; move
   only the largest boundary cases to nightly if measured runtime requires it.
6. Cover Iguana and supported HD shielded-key policies. Include a nonzero HD
   account only if the activation contract supports it. Progress callbacks
   must be monotonic and terminate at the exact target height.

Do not use a whole-file database digest: SQLite page layout, WAL state, and the
bundled SQLite version can change without semantic schema drift. The canonical
metadata contract is the stable assertion surface.

### Deterministic Light-mode activation harness

Add local, protocol-faithful test services on ephemeral loopback ports:

- a lightwalletd fixture implementing `GetTreeState` and streamed
  `GetBlockRange` from a small deterministic ARRR-compatible compact chain;
- a minimal Electrum fixture implementing only the transparent-backend calls
  made during activation.

Drive the public `init_z_coin` task and poll `init_z_coin_status`. The primary
blocking test shall assert ordered activation, `UpdatingBlocksCache`,
`BuildingWalletDb`, balance, and completion phases; monotonic counters; stable
wire fields; the resolved `first_sync_block`; recovered balance/history; and a
complete scan through the declared tip. A second activation against the same
files shall reuse current state without a full refetch. Advancing the fixture
tip shall fetch and scan only the delta.

Focused variants should cover explicit height, deterministic date resolution,
absent sync parameters, `skip_sync_params`, changed anchors, endpoint failover,
all-endpoint failure, invalid tree state, malformed block data, and a stream
that ends before the requested height. Use fixed test seeds and explicit
service counters, not wall-clock sleeps. The current protobuf build generates
client bindings only, so the implementation must add a test-only server
binding or a small fixture crate without widening the production server API.

### Prover, live-service, and platform layers

- After scanning a deterministic spendable note, construct a real shielded
  withdrawal or ZHTLC transaction with cached Sapling parameters. Assert note
  and witness selection, proof completion, transaction parsing, spend/change
  recognition after rescan, and no double spend. Reuse the existing CI Zcash
  parameter cache and measure runtime before making this an unconditional
  pull-request gate.
- Run an unfunded, bounded-range ARRR Light-mode activation against public
  Electrum and lightwalletd services nightly. Assert protocol interoperability,
  completion through the observed tip, current-schema recognition, and SQLite
  integrity, but do not assert an exact public tip. Keep this job non-blocking
  because endpoint availability and rate limits are external.
- Run the database classification, preservation, rebuild, and reopen cases on
  Windows because file locking, rename behavior, and WAL cleanup differ from
  Linux. Retain WASM compile coverage, but do not claim that native SQLite
  tests exercise browser persistence.
- Do not encode the current Native-mode compact-cache adapter gap as an ignored
  or expected-failure test. Add a positive Native activation/sync test when the
  adapter is implemented; until then, keep its compile gates and chapter-39
  limitation explicit.

Recommended implementation order: first make the existing shielded DB suite a
blocking CI cell; then add independent schema goldens and scan lifecycle cases;
then build the local lightwalletd/Electrum activation harness; then add the
real-prover, Windows, and nightly public-service layers.

## Working rules and log

- Modify only this repository. Never inspect the forbidden corpus paths or
  another KDF checkout from the clean-side context.
- Candidate manifests and build products live in ignored, isolated spike
  directories. They are not production edits and cannot overwrite the current
  dirty worktree.
- Preserve existing user changes. Stage or commit only files explicitly in
  scope if the operator later requests a commit.
- Continuously record candidate commands, failure summaries, any candidate-tag
  retreat, the human decision, and verification results below.

### Progress log

- **2026-08-04:** hard gates and forced-rebuild policy recorded. Candidate 1
  resolved to the stable `0.23/0.21.1/0.28` line. Candidate 2 was tested at
  upstream tag `zcash_client_sqlite-0.22.0-rc.7`; no tag retreat was needed.
  The baseline native and WASM checks passed. Both candidates require a shared
  `rusqlite 0.37` port, show the same 30 first-order native KDF errors, and reach
  the same 13 KDF errors on WASM after disabling WASM multicore defaults.
  Candidate 1 initially scored 18 versus Candidate 2's 27 and was recommended.
  The later advisory refresh adjusted Candidate 1 to 20; it remains lower risk
  only with the narrow `time-core` manifest fix. Waiting for the operator's
  target choice.
- **2026-08-04:** the operator authorized the Spec Reader and Dirty Gate
  checkpoint for both `v2.6.0-beta` and the latest `dev` head. Target selection
  remains paused while that result, the newest non-RC upstream release check,
  and the crates.io-first/dependency-hygiene assessment are prepared. The
  operator prefers the stable line but has not selected it yet.
- **2026-08-04:** registry and upstream-tag refresh found no stable client line
  newer than backend 0.23.0 / SQLite 0.21.1. The stable backend's exact
  `time-core 0.1.2` pin blocks the security-fixed `time` release; official
  upstream removed the workaround in `50f76b48474e50926f1b796ed943541b8ad630f6`
  as part of its Rust-1.88/next-line work. An isolated manifest-only removal
  resolved `time 0.3.47`, cleared the added advisory, and reproduced the same
  30 KDF compile errors. The dependency audit also showed that `nom 6.1.2` is
  independently pinned by `sia-rust`, so librustzcash is not the sole blocker
  for every current Rust/Cargo warning.
- **2026-08-04:** the Spec Reader pinned `v2.6.0-beta` at `475cdb49` and
  refreshed `dev` at `e686ef350`, finding the same Komodo `k-1.4.2` Zcash
  dependency revision and the same native persistence generation in both. It
  added the exact sanitized legacy/cache fingerprints and forced-rebuild
  boundary to chapter 39. The independent Dirty Gate passed the complete new
  subsection with score `0.00`. No authorized reference uses or requires the
  modern RC line; the only remaining Phase-C deferral is the selected modern
  line's exact migration-ID/current-schema fingerprint.
- **2026-08-04:** the operator selected Candidate 1. Production now resolves
  the exact stable `0.23.0/0.21.1/0.28.0` line and one shared
  `rusqlite 0.37.0`. The rusqlite audit found and ported native consumers in
  `db_common`, coins, Lightning persistence, WalletConnect storage, and
  `mm2_main`; 14 additional legacy dynamic-parameter calls in `mm2_main` were
  found only when that package was compiled and were corrected. All 33
  `db_common` tests and all four focused `mm2_main` migration tests pass.
- **2026-08-04:** the selected integration uses crates.io versions plus three
  narrow source patches. Backend 0.23.0 removes only its obsolete exact
  `time-core 0.1.2` dependency. Primitives 0.28.0 and transparent 0.8.0 expose
  only the raw-output, locktime, sequence, and KDF P2SH signing hooks needed for
  deployed transaction compatibility. Full serialized transaction bytes and
  output count/order/value/script are pinned by focused patch tests. The unused
  0.5-era `librustzcash-patched/` subtree was removed.
- **2026-08-04:** Reloaded now uses
  `<TICKER>_RELOADED_WALLET.db` and
  `<TICKER>_RELOADED_COMPACT_BLOCKS.db`, leaving the GLEEC/pre-upgrade names
  untouched. The read-only semantic classifier recognizes absent/empty, exact
  reference legacy, exact selected current, recognized cache, unknown, and
  corrupt states before initialization. Selected current is pinned at
  `user_version = 8`, 48 migration IDs (digest
  `7b506df8a2b119fb143664a1c0b5eddfa7de5fe8458f07829818c59e5b39ddbb`),
  and 467 semantic records (digest
  `dfba9135b6d5ae446e88d83d162e9458730b5b098051d8565fdd91fe6be5bea8`).
  D39.8.0a is resolved.
- **2026-08-04:** the initial 36 shielded schema/store/scan regressions pass. The final
  end-to-end fixture starts with the exact legacy schema at a Reloaded filename,
  rebuilds it rather than migrating it, scans a deterministically encrypted
  Sapling note, reconstructs the expected balance and transaction history, and
  proves both persist after reopening. Coverage also includes exact
  legacy/current fingerprints, recoverable backup, no mutation of GLEEC files,
  unknown/corrupt preservation, explicit scan anchors, bounded progress,
  first-block and sequential hash continuity, malformed compact data, durable
  reopen, balance, and history. Exact `init_z_coin` progress/result JSON is
  separately pinned and its focused activation tests pass. Native package
  checks, `coins` all-targets clippy with warnings denied, the
  `coins`/`coins_activation` WASM build, and the Windows GNU release build pass.
  The independent `nom 6.1.2`
  future-incompatibility remains attributable to pinned `sia-rust`; it is not a
  librustzcash blocker.
- **2026-08-04:** the feature-gated native ZHTLC test surface compiles and
  passes crate-scoped clippy against the selected APIs. The two direct
  transparent-transaction patch regressions also pass, covering preservation
  of KDF P2SH script-component order and of
  raw output count/order/value/script. Generated patch-crate build directories
  were removed from the source trees; only the published crate sources, narrow
  compatibility edits, patch records, and license files remain.
- **2026-08-04:** final source/CRD reconciliation confirmed a pre-existing
  Native-mode runtime gap: the native backend advances the legacy Sapling tree
  cache but does not populate the modern compact-block cache consumed by
  `zcash_client_sqlite`. Light-mode store/scan behavior is covered; Native-mode
  package and Windows builds pass, but no live Native activation is claimed.
  Implementing that source adapter is separate from the stable dependency and
  schema port and remains explicitly open in chapter 39.
- **2026-08-04:** first Windows Light-mode validation against uploaded Gleec
  Wallet and Desktop Wallet artifacts exposed three runtime defects beyond the
  original schema fixtures. A lightwalletd `TreeState` block ID was used in RPC
  display order against canonical little-endian compact hashes, so the first
  valid ARRR block failed continuity validation. An interrupted compact-cache
  rollback journal was then misclassified as corruption because read-only
  SQLite cannot perform recovery. Repeated same-ticker activation tasks also
  raced destructive rescan/rename work, producing Windows sharing violations
  and transient missing-table errors. The checkpoint boundary now reverses the
  validated 32-byte display hash once; sidecar recovery is fingerprinted only
  on a temporary copy; and same-ticker ZCoin activation is serialized across
  its full task. The Desktop log separately showed a requested range of 776,542
  blocks still downloading normally, so compact responses are now checked and
  persisted as one atomic transaction per 500-block batch instead of one
  connection/transaction per block. A final restart audit found that an
  unscanned wallet would otherwise start again at its checkpoint and download
  an already persisted compact range a second time. Restart now validates the
  complete cached segment from its chain-state anchor through the highest
  cached height at or below the current target and resumes at the following
  height; a discontinuous cache is preserved and rebuilt. All 38 shielded
  store/scan tests and all 69 `coins_activation` tests pass, as do native
  package checks, strict scoped clippy, the WASM package check, pinned
  formatting, and the complete current Windows GNU release build (22m33s;
  `kdf.exe` SHA-256
  `c6f0e7e784d1c45274b85de8d64b4db7fbace05cd880131aeea8b2d2dbd9ddd4`).
  The pre-existing `nom 6.1.2` future-
  incompatibility warning remains unrelated to this work.
- **2026-08-05:** repeat Windows validation showed that download and database
  recovery were no longer the blockers. Gleec Wallet fetched and atomically
  stored its roughly 2,900-block range in under a second; Desktop Wallet reused
  and completed a roughly 290,000-block cache in about 20 seconds. Both then
  failed at the first modern wallet scan because Pirate's dictated compact
  protobuf omits `ChainMetadata`, while the stable backend requires a known
  Sapling tree size at the scan boundary. A narrow in-memory block-source
  adapter now derives that first-block final size from the trusted preceding
  frontier plus the block's explicit Sapling outputs, preserving the cached
  wire record. A deterministic received-note regression reproduces the absent
  field and reconstructs its balance. The repeated `.checkpoint.*.bak` files
  were consequences of each new activation retry resetting the still-unscanned
  wallet, not schema-classification failures. Desktop's apparent 14% stall was
  a separate GUI rendering failure after it received the backend's string scan
  error; the KDF scan itself failed immediately. `<TICKER>_CACHE.db` remains the
  separate backward-compatible native Sapling-state cache, while modern Light
  mode uses the two Reloaded-specific database names. Post-fix verification
  passes all 39 shielded store/scan tests, all 69 `coins_activation` tests,
  native package checks, strict scoped clippy, pinned formatting, and diff
  hygiene. The Windows GNU release rebuild passes in 22m08s; `kdf.exe` SHA-256
  is `3c9978e86d1a3ca4ba18bc1a9209baa4b31d80ba91d291679e059a5e58a75007`.
- **2026-08-05:** the next Windows run confirmed a later, distinct scan failure.
  Both wallets reached the modern wallet-building phase and then returned
  `Unable to compute root; missing values for nodes`; Desktop's GUI rendered
  that backend string incorrectly and retained stale progress, while its KDF
  process had already stopped scanning. Gleec's short default-window retry
  succeeded only because it excluded the older deposit range. A clean public
  Pirate lightwalletd reproduction pinned the first failure to the ninth
  1,000-block wallet batch: stable `ShardTree` had legitimately compressed the
  completed rightmost subtree, so querying it for an exact leaf-level frontier
  at the next batch boundary failed. The scanner now advances an exact frontier
  from each validated compact block and carries it between bounded calls. The
  temporary public diagnostic then scanned the complete 50,333-block reported
  range through height 4,075,787 in about 148 seconds and was removed rather
  than retained as a network-dependent test. On a process restart, Reloaded
  reacquires `TreeState` at the local scanned height and accepts it only when
  height, block hash, and Sapling tree size match persisted wallet metadata. A
  deterministic metadata-free regression reproduces the public commitment-count
  boundary, rejects mismatched restart state, and continues scanning after
  reopen. All 40 shielded store/scan tests and all 69 `coins_activation` tests
  pass, as do native package checks, strict scoped clippy, the WASM package
  check, pinned formatting, and diff hygiene. The Windows GNU release rebuild
  passes in 22m53s; `kdf.exe` SHA-256 is
  `8ac63e9c155d1caf59de401d49dd67670f230fcfdddf9466eb2685a232e193e7`.
- **2026-08-05:** final acceptance logs from both Windows wallets contain no
  shielded schema, checkpoint, compact-chain, frontier, note-scan, or wallet-DB
  failure. Every observed wallet scan reached its target, including a Gleec
  Wallet `"earliest"` scan from Pirate Sapling activation through height
  4,075,935 and later sub-second validated resumes. Explicit starts at
  4,025,455, 3,938,542, 3,503,636, 4,000,000, 3,624,000, and 2,652,000 were
  applied exactly; changing an existing anchor rebuilt from the requested
  height, while unchanged state resumed. A roughly 3.9-million-block compact
  download completed in about 699 seconds, but its wallet scan took about
  8,781 seconds; smaller runs showed the same pattern, confirming that modern
  wallet scanning and SQLite work, rather than lightwalletd transfer, dominate
  a full rescan. The only ARRR transport incident was a short refusal from the
  configured public Electrum hosts; the clients kept retrying and reconnected
  without KDF intervention. Unsupported WSS candidates, Desktop calls to the
  generic `my_tx_history_v2` endpoint, and an inactive balance streamer were
  client/configuration diagnostics with working supported alternatives, not
  shielded scanner failures.

  Source tracing did find one real tuning defect: the modern Light wallet
  scanner hardcoded 1,000 blocks per batch and no pause even though the
  activation request had been parsed correctly. It now uses the effective
  `scan_blocks_per_iteration` and `scan_interval_ms` values; a deterministic
  fixture proves exact batch progress and the requested inter-batch pause
  without a wall-clock assertion. INFO records the effective policy and
  phase-level start/finish summaries; bounded batch timing and frontier detail
  are DEBUG/TRACE, and the normal Light bypass of the legacy Native cache is no
  longer a warning. The audit also recorded, without widening this port, that
  post-activation continuous Light sync remains deferred under D39.8.0b;
  re-activation currently performs the validated catch-up.

  Final verification passes all 40 shielded store/scan tests, all 69
  `coins_activation` tests, native `coins`/`coins_activation` checks, strict
  scoped clippy, the WASM package check, pinned formatting, and diff hygiene.
  The exact Windows GNU release build passes in 22m16s; the 65,933,312-byte
  `kdf.exe` has SHA-256
  `3d6b49bcb41e668791e4371809027ff304f8911eb95e90c4727f46382f2eac26`.
