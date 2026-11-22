# Chapter 44 -- Database Persistence and Schema Migrations

**Status:** driving-spec.

> **One-sentence claim:** the native legacy SQLite database shall remain
> interchangeable with GLEEC-era nodes by preserving the migration-ledger
> semantics, the version-1 bootstrap schema, the version-by-version
> migrations through state 15, and the rule that RELOADED-local persistence
> changes shall not claim future shared `MM2.db` migration numbers.

This chapter binds the native legacy SQLite database used for swap, order,
and stats persistence. It complements [Chapter 25](25-sql-query-builder.md),
which binds the SQLite gateway substrate, and [Chapter 26](26-cross-platform-and-wasm.md),
which binds the native/WebAssembly persistence split. This chapter does not
redefine query-builder APIs or IndexedDB storage; it binds the on-disk
SQLite compatibility contract.

> **Binding scope (R36).** Table names, column names, SQL type declarations,
> index names, migration numbers, and file-format behavior are dictated
> interoperability facts and are normative here. Private helper names,
> diagnostic text, module decomposition, and internal test names are
> non-normative and are intentionally omitted.

## 44.1 Scope and Database Files

R44.1.1 The migration contract in this chapter applies to the native legacy
SQLite database file named `MM2.db` under the node database directory. The
startup path may also open other native SQLite database files, including
`MM2-shared.db` and `KOMODEFI.db`, but those files are outside this chapter
unless a later chapter explicitly binds their schemas.

R44.1.2 The chapter-bound native SQLite path shall use the chapter-25 SQLite
gateway and pragma discipline. A native process shall initialize the SQLite
connections before normal swap/order/stat persistence is allowed to run.

R44.1.3 WebAssembly builds shall not use `MM2.db`, the `migration` table, or
the native SQL migrations in this chapter. Browser persistence uses the
IndexedDB-backed paths bound by chapter 26. Cross-platform code that has both
native and WebAssembly storage implementations shall treat this chapter as
native-only.

## 44.2 Migration Ledger Semantics

R44.2.1 The database shall contain a ledger table named `migration` with the
following schema:

| Column | SQLite declaration |
| --- | --- |
| `current_migration` | `INTEGER NOT_NULL UNIQUE` |

The declaration spelling is part of the compatibility contract. The ledger
does not use an integer primary key; the unique constraint on
`current_migration` is the only explicit table constraint.

R44.2.2 The current schema state is the highest numeric value stored in
`migration.current_migration`. Implementations shall append a new row after
each successful migration step rather than updating the existing highest row.

R44.2.3 State `1` is the bootstrap state. Applying the migration keyed by
state `N` produces state `N + 1` and appends `N + 1` to the ledger. The
GLEEC-era interchange baseline is state `15`.

R44.2.4 A correctly migrated state-15 database shall contain the contiguous
ledger values `1` through `15`. Runtime migration selection is still keyed by
the highest value; implementations shall not infer the current state from
column existence or table existence.

R44.2.5 Each migration step shall be atomic with its ledger append. If any
statement in a step fails, neither the schema/data changes for that step nor
the `N + 1` ledger row may be committed.

R44.2.6 Column-adding migrations are intentionally not self-idempotent. The
ledger is what prevents a column-add statement from being run twice. A
duplicate-column error means the database schema and ledger are inconsistent
or a later schema was retroactively folded into an earlier creation path.

## 44.3 Bootstrap Behavior

R44.3.1 On a fresh native database, bootstrap shall create only:

- the `migration` ledger table of §44.2;
- a ledger row with `current_migration = 1`;
- the version-1 `my_swaps` table of §44.5.1.

R44.3.2 Bootstrap shall not create `stats_swaps`, `my_orders`, `nodes`, or
`stats_nodes`; those tables appear only through their numbered migrations.

R44.3.3 Bootstrap shall not pre-create any `my_swaps` column introduced by
states 10, 13, or 14. In particular, the bootstrap schema shall not include
`swap_type`, `is_finished`, `events_json`, `other_p2p_pub`, `dex_fee_burn`,
or `swap_version`.

R44.3.4 After bootstrap records state `1`, the normal migration loop shall run
from state `1` until no state-15-era migration remains. A fresh interoperable
database therefore reaches state `15` before normal native persistence paths
are used.

R44.3.5 For an existing database with highest ledger state at least `1`,
startup shall skip bootstrap and resume migrations from that highest state.

R44.3.6 If the current ledger state cannot be read, the compatibility path may
treat the legacy database as uninitialized. The cleanup behavior for this
legacy path is narrow: it targets the `migration` and `my_swaps` tables before
attempting bootstrap again. Other tables are not part of that cleanup contract.

## 44.4 Version State Table

The following table binds the state numbers. "Input state" is the value read
from the highest ledger row before the step begins; "recorded state" is the
value appended after the step commits.

| Input state | Recorded state | Schema/data effect |
| ---: | ---: | --- |
| bootstrap | 1 | Create `migration`; create version-1 `my_swaps`; record state `1`. |
| 1 | 2 | Import historical "my swaps" JSON records into the version-1 `my_swaps` columns only. |
| 2 | 3 | Create `stats_swaps` with its base schema and import historical maker/taker stats JSON. |
| 3 | 4 | Create index `timestamp_index` on `stats_swaps(started_at)`. |
| 4 | 5 | Add split ticker/platform columns to `stats_swaps` and backfill them from the coin strings. |
| 5 | 6 | Create `my_orders`. |
| 6 | 7 | Create `nodes` and `stats_nodes`. |
| 7 | 8 | Add USD price columns to `stats_swaps`. |
| 8 | 9 | Add maker/taker public-key columns to `stats_swaps`. |
| 9 | 10 | Add V2/live-state columns to `my_swaps`. |
| 10 | 11 | Mark already-finished legacy swaps in `my_swaps` using the historical JSON records. |
| 11 | 12 | Add GUI and daemon-version columns to `stats_swaps`. |
| 12 | 13 | Add the counterparty P2P public-key and dex-fee-burn columns to `my_swaps`. |
| 13 | 14 | Add `swap_version` to `my_swaps` and backfill rows present at migration time with legacy version `1` where unset. |
| 14 | 15 | Backfill maker/taker public-key values in `stats_swaps` from historical maker/taker stats JSON. |

No state-15-era migration exists after state `15`; a database whose highest
ledger value is `15` is fully migrated for the contract in this chapter.

## 44.5 `my_swaps` Schema

### 44.5.1 State 1 Bootstrap Columns

The state-1 `my_swaps` table shall have exactly these columns, in this order:

| Column | SQLite declaration |
| --- | --- |
| `id` | `INTEGER NOT NULL PRIMARY KEY` |
| `my_coin` | `VARCHAR(255) NOT NULL` |
| `other_coin` | `VARCHAR(255) NOT NULL` |
| `uuid` | `VARCHAR(255) NOT NULL UNIQUE` |
| `started_at` | `INTEGER NOT NULL` |

R44.5.1 Historical import at state `1 -> 2` shall write only `my_coin`,
`other_coin`, `uuid`, and `started_at`. It shall not require `swap_type` or
any later column to exist.

### 44.5.2 State 10 Additions

The state `9 -> 10` migration shall append these columns to `my_swaps`, in
this order:

| Column | SQLite declaration |
| --- | --- |
| `is_finished` | `BOOLEAN NOT NULL DEFAULT 0` |
| `events_json` | `TEXT NOT NULL DEFAULT '[]'` |
| `swap_type` | `INTEGER NOT NULL DEFAULT 0` |
| `maker_volume` | `TEXT` |
| `taker_volume` | `TEXT` |
| `premium` | `TEXT` |
| `dex_fee` | `TEXT` |
| `secret` | `BLOB` |
| `secret_hash` | `BLOB` |
| `secret_hash_algo` | `INTEGER` |
| `p2p_privkey` | `BLOB` |
| `lock_duration` | `INTEGER` |
| `maker_coin_confs` | `INTEGER` |
| `maker_coin_nota` | `BOOLEAN` |
| `taker_coin_confs` | `INTEGER` |
| `taker_coin_nota` | `BOOLEAN` |

R44.5.2 The decimal/rational amount fields stored in `maker_volume`,
`taker_volume`, `premium`, and `dex_fee` shall be persisted as text strings to
avoid binary floating-point loss.

R44.5.3 The `events_json` column shall default to the empty JSON array string
for rows that predate event persistence or are inserted without an explicit
event list.

### 44.5.3 State 13 Additions

The state `12 -> 13` migration shall append these columns to `my_swaps`, in
this order:

| Column | SQLite declaration |
| --- | --- |
| `other_p2p_pub` | `BLOB` |
| `dex_fee_burn` | `TEXT` |

R44.5.4 `dex_fee_burn` shall use text storage for the same precision reason
as the amount fields in §44.5.2.

### 44.5.4 State 14 Addition

The state `13 -> 14` migration shall append:

| Column | SQLite declaration |
| --- | --- |
| `swap_version` | `INTEGER` |

R44.5.5 During the same state `13 -> 14` step, rows already present whose
`swap_version` is unset shall be updated to `1`. The column remains nullable
and has no default constraint; live V2 persistence shall provide an explicit
value when it inserts a V2 swap row.

### 44.5.5 Final State-15 Shape

At state `15`, `my_swaps` shall consist of the state-1 columns followed by
the state-10 columns, the state-13 columns, and the state-14 column, preserving
the append order specified above. No additional index is bound for this table
other than SQLite's implicit structures for the primary-key and unique
constraints.

## 44.6 `stats_swaps` Schema

### 44.6.1 State 3 Base Table

The state `2 -> 3` migration shall create `stats_swaps` with exactly these
columns, in this order:

| Column | SQLite declaration |
| --- | --- |
| `id` | `INTEGER NOT NULL PRIMARY KEY` |
| `maker_coin` | `VARCHAR(255) NOT NULL` |
| `taker_coin` | `VARCHAR(255) NOT NULL` |
| `uuid` | `VARCHAR(255) NOT NULL UNIQUE` |
| `started_at` | `INTEGER NOT NULL` |
| `finished_at` | `INTEGER NOT NULL` |
| `maker_amount` | `DECIMAL NOT NULL` |
| `taker_amount` | `DECIMAL NOT NULL` |
| `is_success` | `INTEGER NOT NULL` |

R44.6.1 Historical maker/taker stats import at state `2 -> 3` shall insert
only these base columns. If maker-side and taker-side historical stats contain
the same swap UUID, the importer shall keep a single stats row for that UUID.

### 44.6.2 State 4 Index

The state `3 -> 4` migration shall create this explicit index:

| Index | Table | Columns |
| --- | --- | --- |
| `timestamp_index` | `stats_swaps` | `started_at` |

### 44.6.3 State 5 Split Coin Columns

The state `4 -> 5` migration shall append these columns to `stats_swaps`, in
this order:

| Column | SQLite declaration |
| --- | --- |
| `maker_coin_ticker` | `VARCHAR(255) NOT NULL DEFAULT ''` |
| `maker_coin_platform` | `VARCHAR(255) NOT NULL DEFAULT ''` |
| `taker_coin_ticker` | `VARCHAR(255) NOT NULL DEFAULT ''` |
| `taker_coin_platform` | `VARCHAR(255) NOT NULL DEFAULT ''` |

R44.6.2 Backfill shall split `maker_coin` and `taker_coin` at the first
hyphen. The ticker column receives the text before the first hyphen, or the
whole coin string when no hyphen exists. The platform column receives the text
after the first hyphen, or the empty string when no hyphen exists.

### 44.6.4 State 8 Price Columns

The state `7 -> 8` migration shall append:

| Column | SQLite declaration |
| --- | --- |
| `maker_coin_usd_price` | `DECIMAL` |
| `taker_coin_usd_price` | `DECIMAL` |

### 44.6.5 State 9 Public-Key Columns

The state `8 -> 9` migration shall append:

| Column | SQLite declaration |
| --- | --- |
| `maker_pubkey` | `VARCHAR(255)` |
| `taker_pubkey` | `VARCHAR(255)` |

> **Cross-reference (added 2026-10-27; passed the KDF Dirty Gate on 2026-10-28 with
> chapter 51 R50A).** How these two columns are filled at live time is bound by
> [Chapter 51](51-legacy-v1-swap-state-machine.md) R50A item 5: each
> party's published terminal report supplies only its **own** persistent
> public key — a maker record fills `maker_pubkey`, a taker record
> `taker_pubkey` — so a swap's two columns are complete only once both
> parties' reports have arrived, and a privacy-coin party's column stays
> empty. The state-15 backfill of R44.6.3 applies the same own-role
> derivation to historical records.

### 44.6.6 State 12 GUI and Version Columns

The state `11 -> 12` migration shall append:

| Column | SQLite declaration |
| --- | --- |
| `maker_gui` | `VARCHAR(255)` |
| `taker_gui` | `VARCHAR(255)` |
| `maker_version` | `VARCHAR(255)` |
| `taker_version` | `VARCHAR(255)` |

### 44.6.7 State 15 Backfill

R44.6.3 The state `14 -> 15` migration shall update `maker_pubkey` and
`taker_pubkey` in `stats_swaps` from the historical maker/taker stats JSON
records where those values can be derived. This step does not add, remove, or
rename any column.

### 44.6.8 Final State-15 Shape

At state `15`, `stats_swaps` shall consist of the state-3 base columns followed
by the state-5 split coin columns, the state-8 price columns, the state-9
public-key columns, and the state-12 GUI/version columns, preserving the append
order specified above. The only explicit bound index is `timestamp_index` on
`started_at`.

## 44.7 Order and Node Tables

### 44.7.1 `my_orders`

The state `5 -> 6` migration shall create `my_orders` with exactly these
columns, in this order:

| Column | SQLite declaration |
| --- | --- |
| `id` | `INTEGER NOT NULL PRIMARY KEY` |
| `uuid` | `VARCHAR(255) NOT NULL UNIQUE` |
| `type` | `VARCHAR(255) NOT NULL` |
| `initial_action` | `VARCHAR(255) NOT NULL` |
| `base` | `VARCHAR(255) NOT NULL` |
| `rel` | `VARCHAR(255) NOT NULL` |
| `price` | `DECIMAL NOT NULL` |
| `volume` | `DECIMAL NOT NULL` |
| `created_at` | `INTEGER NOT NULL` |
| `last_updated` | `INTEGER NOT NULL` |
| `was_taker` | `INTEGER NOT NULL` |
| `status` | `VARCHAR(255) NOT NULL` |

### 44.7.2 `nodes`

The state `6 -> 7` migration shall create `nodes` with exactly these columns,
in this order:

| Column | SQLite declaration |
| --- | --- |
| `id` | `INTEGER NOT NULL PRIMARY KEY` |
| `name` | `VARCHAR(255) NOT NULL UNIQUE` |
| `address` | `VARCHAR(255) NOT NULL` |
| `peer_id` | `VARCHAR(255) NOT NULL UNIQUE` |

### 44.7.3 `stats_nodes`

The state `6 -> 7` migration shall create `stats_nodes` with exactly these
columns, in this order:

| Column | SQLite declaration |
| --- | --- |
| `id` | `INTEGER NOT NULL PRIMARY KEY` |
| `name` | `VARCHAR(255) NOT NULL` |
| `version` | `VARCHAR(255)` |
| `timestamp` | `INTEGER NOT NULL` |
| `error` | `VARCHAR(255)` |

## 44.8 Historical Imports and Live Persistence

R44.8.1 Historical JSON import steps are migration-owned data migrations. They
shall run only at their bound migration states:

- state `1 -> 2`: import historical "my swaps" records into version-1
  `my_swaps`;
- state `2 -> 3`: import historical maker/taker stats records into
  `stats_swaps`;
- state `10 -> 11`: mark already-finished legacy swaps after `is_finished`
  exists;
- state `14 -> 15`: backfill maker/taker public keys after the public-key
  columns exist.

R44.8.2 Normal live native swap persistence shall run only after the startup
migration path has completed. The V1/live-filter path may rely on
`swap_type`, `is_finished`, and `events_json` existing at state `10`; V2 swap
persistence may rely on all `my_swaps` columns through state `14`; stats
persistence may rely on all `stats_swaps` columns through state `12` and the
state-15 public-key backfill.

R44.8.3 V2 native live persistence shall store the following `my_swaps` fields
explicitly for a V2 record: `my_coin`, `other_coin`, `uuid`, `started_at`,
`swap_type`, `maker_volume`, `taker_volume`, `premium`, `dex_fee`,
`dex_fee_burn`, `secret`, `secret_hash`, `secret_hash_algo`, `p2p_privkey`,
`lock_duration`, `maker_coin_confs`, `maker_coin_nota`, `taker_coin_confs`,
`taker_coin_nota`, `other_p2p_pub`, and `swap_version`.

R44.8.4 Swap event persistence shall update `events_json` by `uuid`; completion
persistence shall set `is_finished` by `uuid`; unfinished-swap recovery shall
select rows by `is_finished = 0` and `swap_type`. These paths require the
state-10 schema and are not valid against a lower native migration state.

R44.8.5 Recent-swap filtering shall use `my_swaps.my_coin`,
`my_swaps.other_coin`, and `my_swaps.started_at`; paging by UUID shall operate
against `my_swaps.uuid`. The result for each row shall include both `uuid` and
`swap_type`, so the state-10 schema is required for normal operation.

R44.8.6 RELOADED shall not add completion-fiat snapshot columns to `my_swaps`.
Native per-wallet swap-history RPCs that expose completion-fiat values shall
read those values from the state-8 `stats_swaps` price columns by swap `uuid`.
The aggregate stats row is the canonical `MM2.db` storage location for those
price snapshots in the GLEEC-compatible schema.

## 44.8A Legacy Saved-Swap Event JSON

R44.8A.1 The legacy V1 saved-swap JSON surface is a public persisted
compatibility contract. A saved swap is role-tagged with a string `type` field
whose accepted values are `Maker` and `Taker`. Each record carries `uuid`,
optional order UUID, coin tickers and amounts when known, optional GUI/version
metadata, `success_events`, `error_events`, and an ordered `events` array.

R44.8A.2 Each legacy saved event in the `events` array shall be a JSON object
with:

- `timestamp`: an unsigned millisecond timestamp;
- `event`: a role-specific event object encoded with a string `type` field and
  a `data` field when the event carries payload data.

Unit events shall be accepted without a `data` field. Events with payloads
shall preserve their existing JSON payload shape; transaction identifiers,
payment instructions, swap errors, negotiation data, secret material, and
refund deadlines are dictated by the legacy swap RPC/history surface and are
not renamed by this chapter.

R44.8A.3 The maker-side event parser shall accept these persisted event
`type` values:

`Started`, `StartFailed`, `Negotiated`, `NegotiateFailed`,
`MakerPaymentInstructionsReceived`, `TakerFeeValidated`,
`TakerFeeValidateFailed`, `MakerPaymentSent`,
`MakerPaymentTransactionFailed`, `MakerPaymentDataSendFailed`,
`MakerPaymentWaitConfirmFailed`, `TakerPaymentReceived`,
`TakerPaymentWaitConfirmStarted`, `TakerPaymentValidatedAndConfirmed`,
`TakerPaymentValidateFailed`, `TakerPaymentWaitConfirmFailed`,
`TakerPaymentSpent`, `TakerPaymentSpendFailed`,
`TakerPaymentSpendConfirmStarted`, `TakerPaymentSpendConfirmed`,
`TakerPaymentSpendConfirmFailed`, `MakerPaymentWaitRefundStarted`,
`MakerPaymentRefundStarted`, `MakerPaymentRefunded`,
`MakerPaymentRefundFailed`, `MakerPaymentRefundFinished`, and `Finished`.

R44.8A.4 The taker-side event parser shall accept these persisted event
`type` values:

`Started`, `StartFailed`, `Negotiated`, `NegotiateFailed`, `TakerFeeSent`,
`TakerFeeSendFailed`, `TakerPaymentInstructionsReceived`,
`MakerPaymentReceived`, `MakerPaymentWaitConfirmStarted`,
`MakerPaymentValidatedAndConfirmed`, `MakerPaymentValidateFailed`,
`MakerPaymentWaitConfirmFailed`, `TakerPaymentSent`, `WatcherMessageSent`,
`TakerPaymentTransactionFailed`, `TakerPaymentDataSendFailed`,
`TakerPaymentWaitConfirmFailed`, `TakerPaymentSpent`,
`TakerPaymentWaitForSpendFailed`, `MakerPaymentSpent`,
`MakerPaymentSpendConfirmed`, `MakerPaymentSpendConfirmFailed`,
`MakerPaymentSpentByWatcher`, `MakerPaymentSpendFailed`,
`TakerPaymentWaitRefundStarted`, `TakerPaymentRefundStarted`,
`TakerPaymentRefunded`, `TakerPaymentRefundFailed`,
`TakerPaymentRefundFinished`, `TakerPaymentRefundedByWatcher`, and
`Finished`.

R44.8A.5 Backward-compatible parsing shall be tolerant across the known legacy
event family. In particular, loading legacy JSON MUST NOT fail solely because
it contains `WatcherMessageSent` or `MakerPaymentSpendConfirmed`. Event
variants that represent watcher notification, watcher spend/refund outcomes,
post-spend confirmation, refund-start, or refund-finish milestones shall be
treated as valid historical milestones even when the current runtime no longer
emits all of them on new swaps.

R44.8A.6 Legacy event storage is append-only per swap. When a new V1 event is
persisted, the implementation shall load the existing saved swap by `uuid`,
append exactly one timestamped event to the end of its `events` array, and
replace the saved record for that same `uuid`. The storage backend may be a
legacy JSON file, a browser object store, or the native `my_swaps.events_json`
column after import; the role tag, event tag, order, and payload JSON contract
remain the same.

R44.8A.7 Replay shall apply saved events in array order to reconstruct the
role-specific swap state. The first event is expected to be `Started`; if the
first event is absent or is not a start event, replay cannot reconstruct coin
identity, amounts, or secret/key material and the swap shall not be
kickstarted. Error events shall restore the failed status and accumulated
error state; success milestone events shall restore observed transaction
identifiers, confirmation milestones, refund deadlines, watcher milestones, and
finished status as applicable.

R44.8A.8 After replay, unfinished swaps shall resume from the command implied
by the last accepted event. Terminal `Finished` events shall not schedule a
continuation. Recovery entry MUST NOT append a duplicate copy of the event it
is resuming from; only new events produced after recovery may be appended.

R44.8A.9 Fund-recovery eligibility is derived from the saved event sequence,
not from string matching on human-readable status text. A finished swap with a
known successful counterparty spend is not recoverable by the local refund
path. A finished swap that failed after the local payment may be recoverable
when replay preserved enough payment, secret/hash, locktime, and coin data to
construct the refund or spend transaction. If any required coin is not active
or required event payload data is missing, recovery shall fail cleanly without
rewriting the saved history.

R44.8A.10 Historical import into `MM2.db` shall preserve legacy event order and
known event tags. Unknown future event tags outside the family enumerated in
R44.8A.3-R44.8A.4 may prevent automatic replay of that swap, but they shall
not justify deleting the saved record or marking the swap finished. The
preferred failure mode is to retain the raw saved history and surface that the
swap cannot be replayed by this binary.

> **Implementation note (informative).** The RELOADED compatibility parser
> accepts the broader historical persisted event family bound above, including
> unit refund-start and refund-finish milestones. It also tolerates the known
> maker refund-start form carrying a `wait_until` payload, preserving that
> deadline as the wait-refund milestone.

## 44.9 Creation/Migration Interaction

R44.9.1 The version-1 creation SQL and the numbered migrations are a single
contract. A fresh database shall not be created directly at the final schema
while still recording ledger state `1`, because the later column-add
migrations would then run against columns that already exist.

R44.9.2 `CREATE TABLE IF NOT EXISTS` may be used for table creation at the
bound state where a table first appears. It shall not be used as permission to
move future columns into that earlier creation statement.

R44.9.3 Implementations shall not use schema introspection to skip a numbered
migration in the normal GLEEC-compatible path while still appending its ledger
row. In that path, the ledger state is authoritative.

R44.9.4 A node may recognize and repair the old RELOADED development ledger
states `8` and `9` when `my_swaps.swap_type` is already present. That
combination is impossible in the GLEEC-compatible state table and indicates
that the old RELOADED branch prematurely stored state-10+ `my_swaps` columns
under lower migration numbers. The repair shall not allocate a new shared
migration state. It shall convert the database to the state-15 schema, append
only the missing ledger rows through `15`, preserve existing V2 `my_swaps`
values, remove RELOADED-only fiat columns from `my_swaps`, and preserve fiat
snapshot values in `stats_swaps` by swap `uuid` where such values exist.

R44.9.5 RELOADED shall not allocate state `16` or any later state in the shared
`MM2.db` migration ledger for project-local schema additions. The next numeric
states after `15` are reserved for GLEEC-compatible upstream evolution. A
RELOADED-only persistence need shall use the existing state-15 schema, a
non-`MM2.db` storage location, or a deliberately documented incompatible
lineage with a conversion rule.

R44.9.6 Future migrations shall not retroactively alter the state-1, state-3,
state-6, or state-7 creation schemas, nor any state-15-era column-addition
lists, unless the project intentionally defines a new incompatible migration
lineage and a conversion rule for existing `MM2.db` files.

> **Upstream divergence (informative).** The active reloaded tree has carried
> draft migration work that collapses some post-bootstrap columns into lower
> state numbers and predefines columns that are also added later. That shape can
> produce a duplicate-column failure on a fresh SQLite database. This chapter
> treats the state-1-through-state-15 ledger above as the interchange baseline;
> RELOADED-local additions must not claim future GLEEC migration numbers in this
> shared ledger.

## 44.9A Swap Storage Shared With the Reference in One Data Directory

> **Status note (2026-11-22).** This section was added by the KDF Spec Reader
> in round 0.5.0-24. **KDF Dirty Gate pass: PASS on 2026-11-22, measure
> 0.00** (attestation in the provenance footer).
> It adds rules and tests only; no sentence of §§44.1-44.9 or §44.10 is
> changed by it.

This section records what the reference does with three things a user's data
directory carries when the user moves between a reference build and RELOADED
on the same directory: the `MM2.db` migration ledger, the per-swap lock file,
and the saved-swap JSON files. "The reference rows" below means the rows
`v2.6.0-beta` and `v3.0.0-beta` of [Chapter 02](02-baseline-state.md) §2.15A.
Every reference fact in R44.9A.1 through R44.9A.9 was read at both rows and is
the same at both. Paths are relative to the node database directory of
R44.1.1, the directory that holds `MM2.db`. File names, file content formats
and ledger values are on-disk interoperability surface (R36) and are normative;
nothing else about the reference is.

### 44.9A.1 Migration ledger above the known state

R44.9A.1 *Highest reference state.* At both reference rows the highest state
the reference can record is `15`, as §44.4 binds. The same holds at the rows
`dev (as of 2026-03-27)`, `staging (as of 2026-03-27)` and
`main (as of 2026-10-07)`. No release tag of the historical record defines a
migration step that would record state `16` or higher; all 255 tags were
checked for this. For builds published without source after `v3.0.0-beta`
this is **not established**.

R44.9A.2 *State 16 has no single meaning.* Two unmerged development branches of
the historical record, the rows `lr-swap-wip-3 (as of 2025-12-03)` and
`tpu-status (as of 2026-01-08)`, each define one further step that records
state `16`. The two steps are different: one adds columns to `my_swaps`, the
other adds columns to `stats_swaps`. Neither branch is part of a release, and
their column lists are not bound here. The consequence for this chapter is
that a ledger value of `16` in a user's database does not identify a schema.

R44.9A.3 *Reference start-up with a ledger above its own latest state.* The
reference does not compare the ledger's highest value with the highest state it
knows. When the highest value is `1` or more, it skips bootstrap and looks for
the step keyed by that value. For any value of `15` or more it finds no step,
changes nothing, appends no ledger row, and continues start-up exactly as for a
fully migrated database. It does not refuse to start and gives no warning or
error; the only trace is its ordinary informational start-up report of the
state it read. It does not lower or remove ledger rows it does not know.

R44.9A.4 *What follows after such a start.* After R44.9A.3 the reference uses
the database on the assumption that it has the state-15 shape. Whether its
normal reads and writes then succeed depends only on what the unknown steps
changed and on SQLite's own rules; no reference check is involved. Which
changes it would survive is **not established** beyond that.

R44.9A.5 *Consequence for RELOADED.* A refusal by RELOADED to open a database
whose ledger is above the highest state the build knows is a RELOADED-only
behavior with no reference counterpart, and it is compatible with the
reference: a database that RELOADED leaves at state `15` (R44.9.5) is fully
migrated for both reference rows. The reference gives no protection in the
other direction, so R44.9.5 remains the rule that keeps a shared database
readable by both.

> **Reloaded status update (2026-11-22, Reloaded-original; informative).**
> With PR #486 RELOADED refuses at start-up a database whose highest ledger
> value is above `15`, the highest state it records: the start-up error names
> both numbers, no migration statement runs, and no ledger row is written,
> lowered or removed. The repair of the old RELOADED shape still runs first.
> This answers the last sentence of the Reloaded status note under R44.9A.13
> for this repository.

### 44.9A.2 The per-swap lock file

R44.9A.6 *Name and location.* The reference uses one lock file per swap,
named `<uuid>.lock`, where `<uuid>` is the swap identifier in its hyphenated
lowercase text form. The file is in `SWAPS/MY/`, the same directory as the
node's own saved-swap files of R44.9A.10. Missing parent directories are
created. The same file and the same rules serve legacy maker swaps, legacy
taker swaps and version-two swaps. The reference source carries a build option,
off by default, that places the file elsewhere; whether any released binary was
built with it is **not established**.

R44.9A.7 *Content format.* The whole content of the file is the Unix time in
whole seconds, written as ASCII decimal digits and nothing else: no line
break, no white space, no surrounding structure, and no identification of the
holder (no process identifier, host name or random token). The content is
written when the lock is taken and written again at every refresh
([Chapter 51](51-legacy-v1-swap-state-machine.md) R5: every thirty seconds).
Each write replaces the content of the existing file in place; it is not a
write to a temporary file followed by a rename.

R44.9A.8 *Acquisition and foreign content.* The reference first tries to
create the file exclusively. If the file already exists, it reads the content
as text and decides as follows:

| Content of the existing file | Reference behavior |
| --- | --- |
| The whole content is an unsigned decimal integer, and the current time is at most 40 seconds after it | The lock is held by another runner. Chapter 51 R5 applies: wait 40 seconds, try once more, then abandon the run. A value in the future counts as held for as long as this condition is true. |
| The whole content is an unsigned decimal integer, and the current time is more than 40 seconds after it | The lock is stale. The reference takes it over by writing the current time into the existing file. |
| Anything else that can be read as text: empty content, digits followed by a line break or space, a signed or fractional number, JSON, or any other format | The lock is treated as free. The reference takes it over at once by writing the current time into the file, without waiting and without a message. |
| The content cannot be read as text (for example it is not valid UTF-8), or the file cannot be read | Acquisition fails with an error. The swap run is abandoned without the retry of chapter 51 R5; a legacy swap emits no event. |

R44.9A.9 *Lifetime and removal.* The reference takes no operating-system
file lock on the file; staleness is decided only by comparing the number in
the file with the wall clock, with the 40-second limit of R44.9A.8. The file's
modification time is not used. For a legacy swap the reference removes the
file when the swap's run ends, on every way the run can end, and ignores a
failure to remove it. Removal does not check that the file still carries the
remover's own time. A process that is stopped abruptly leaves the file behind;
the next runner takes it over under R44.9A.8 once it is stale. The reference
has no start-up step that clears leftover lock files. Whether an orderly
shutdown removes the files of swaps still running, and the exact point at
which a version-two swap releases its lock, are **not established**.

> **Code-quality finding (informative).** Three gaps in the reference's lock
> follow directly from R44.9A.7 through R44.9A.9. (1) A refresh rewrites the
> file in place, and empty content counts as a free lock, so a second runner
> that reads the file during a refresh can take a lock that is held. (2) Two
> runners that both find a stale lock can both take it, because the take-over
> is a plain write and not an exclusive step. (3) A runner that has lost its
> lock to a take-over keeps refreshing the file and removes it when its run
> ends, although another runner now holds it. A proposed remedy for an
> implementation that wants more than this is to keep the holder's identity
> outside the lock file's content and to verify it before each refresh and
> before removal; R44.9A.13 bounds how far that can go while the reference
> shares the directory.

### 44.9A.3 Saved-swap files

R44.9A.10 *File names.* The reference keeps one JSON file per swap in three
directories: `SWAPS/MY/<uuid>.json` for the node's own swaps,
`SWAPS/STATS/MAKER/<uuid>.json` and `SWAPS/STATS/TAKER/<uuid>.json` for swap
statistics. The content is the saved-swap JSON of §44.8A.

R44.9A.11 *Which files the reference reads.* A single swap is read by its
exact path; an absent file means "no such record", and a file that does not
parse is an error for that one read. A whole directory is read only by the
migration steps of R44.8.1. Such a read picks only entries that are not
directories and whose name has the extension `json`, compared without regard
to letter case; no other entry is opened. A picked file that does not parse
is skipped and reported in the log; it does not stop the step. A directory
that cannot be listed is treated as holding no records. A leftover file with
any other extension, including `.tmp` and `.lock`, is therefore ignored. A
leftover file whose name ends in `.json` is picked, and is imported if it
parses.

R44.9A.12 *How the reference writes.* The reference writes a saved swap, in
all three directories, through a temporary file. It serializes the whole
record, writes it to a file in the same directory whose name is the final
name with `.tmp` appended (`<uuid>.json.tmp`), and then renames that file over
the final name. The temporary name is fixed per record; it has no random or
per-process part. The reference does not ask the operating system to force
the file or the directory to stable storage before or after the rename. A
failed write can leave the temporary file behind; the reference never clears
such leftovers, but its next save of the same record overwrites the leftover
and renames it.

### 44.9A.4 Rules for RELOADED

R44.9A.13 *Interoperability rules.* Where RELOADED shares a data directory
with a reference build, the following hold.

1. A temporary file that RELOADED creates in a saved-swap directory shall not
   have the extension `json` in any letter case (R44.9A.11). The name
   `<uuid>.json.tmp` is the one the reference itself uses and overwrites
   (R44.9A.12); using the same name leaves no file that the other
   implementation does not already handle.
2. For a reference build to respect a lock that RELOADED holds, the lock shall
   be the file of R44.9A.6 and its whole content shall have the form of
   R44.9A.7, refreshed more often than every 40 seconds. Any additional
   content in that file, such as a fencing token or a holder identifier, makes
   the reference treat the lock as free and take it at once (R44.9A.8, third
   row). Content that is not valid text makes the reference abandon the swap
   run (fourth row).
3. Data that RELOADED uses to fence its lock shall therefore be kept outside
   the content of `<uuid>.lock`, and, if it is a file in `SWAPS/MY/`, under a
   name that item 1 allows. The reference neither reads nor preserves such
   data, and it may overwrite or remove `<uuid>.lock` without regard to it
   (R44.9A.9).
4. RELOADED shall read a `<uuid>.lock` file of the R44.9A.7 form by the
   40-second rule of R44.9A.8, so that it does not start a second runner for a
   swap that a reference build on the same directory is running.
5. Fencing protects RELOADED runners from each other. Against a reference
   build running at the same time on the same directory, the protection is
   the timestamp rule of R44.9A.8 and no more.

> **Reloaded status (informative, 2026-11-22).** At the base of this round's
> branch, this repository keeps its lock at the R44.9A.6 path with the
> R44.9A.7 content, and writes saved swaps in place, without a temporary
> file. The reference rows write through `<uuid>.json.tmp` and a rename
> (R44.9A.12), so a change of RELOADED to that form brings it to the
> reference's behavior; it is not a divergence. This repository's start-up
> behavior for a ledger above state `15` was not examined in this round.

> **Reloaded status update (2026-11-22, Reloaded-original; informative).**
> With PR #480 the native build writes a saved swap, its statistics copy and a
> saved order through a temporary file in the same directory, named `<file
> name>.<process id>.<counter>.tmp` (extension `tmp`, R44.9A.13 item 1), and
> renames it over the record. Beyond the reference (R44.9A.12) it forces the
> temporary file to stable storage before the rename and, on Unix, the
> directory after it; if only the directory sync fails, the save succeeds with
> a warning, because the record is already replaced. On Windows a rename
> refused because another process holds the file is retried briefly. The
> temporary name is unique per write, so two writers of one record never share
> it; a leftover from an interrupted write is therefore not overwritten by a
> later save, and both implementations' directory reads ignore it (R44.9A.11).
> Browser builds save each record in one object-store transaction.
>
> The same PR fences the swap lock between RELOADED runners (R44.9A.13 items 2
> to 5). The content of `<uuid>.lock` keeps the R44.9A.7 form. A new lock is
> created by linking a fully written temporary file to the lock name, which
> fails if the name exists, so no reader sees an empty lock (on a file system
> without hard links the earlier create-then-write is used, and a new lock is
> empty for a moment); a refresh and a take-over write through a temporary
> file and a rename (gap 1 of the code-quality finding). The runner keeps the
> value it wrote last in memory; before each refresh it reads the file, and a
> missing file, a different value, content that is not text, or refreshing
> that has failed for longer than 40 seconds makes the lock lost: the run is
> stopped where it stands, like an abrupt stop, starts no further step and
> leaves the file (gap 3), and the swap is handed to the kick-start of a saved
> swap, which takes the lock by chapter 51 R5; if the lock file keeps being
> removed, the swap is resumed from its record after each loss. A late refresh
> that finds its own value continues with a warning. After a take-over the
> file is read back once, which narrows gap 2 between RELOADED runners; two of
> them that take a stale lock within the same second write the same value and
> are not told apart. Against a reference build on the same directory the
> timestamp rule of R44.9A.8 remains the only protection (item 5).

### 44.9A.5 Tests

T44.9A.1 *Lock content form.* After RELOADED takes a swap lock and after each
refresh, the content of `SWAPS/MY/<uuid>.lock` consists of one or more ASCII
decimal digits only, with no other byte, and its value is the current Unix
time in seconds within the test's tolerance.

T44.9A.2 *Reading a reference-form lock.* With a `<uuid>.lock` file whose
content is the current time in the R44.9A.7 form, RELOADED does not start a
runner for that swap before the 40-second limit has passed. With a value more
than 40 seconds old, it takes the lock.

T44.9A.3 *Temporary files are invisible to a directory read.* With a file
`<uuid>.json.tmp` and a file `<uuid>.lock` present in `SWAPS/MY/`, and the
same temporary name present in both statistics directories, the migration
steps of R44.8.1 import exactly the records of the `*.json` files and report
no error for the other files.

T44.9A.4 *An interrupted save leaves a readable record.* When a save of a swap
record is interrupted after the temporary file is written and before the
rename, the final `<uuid>.json` still holds the previous complete record, and
the next save of that record succeeds.

T44.9A.5 *Ledger stays at 15.* After RELOADED has run on a state-15 database,
including a run that uses the lock and the saved-swap changes of this
section, the highest ledger value is still `15` (R44.9.5).

## 44.10 Acceptance Criteria

- A fresh native `MM2.db` initializes at state `1`, runs migrations through
  state `15`, and contains contiguous ledger rows `1` through `15`.
- A database already at state `N` where `1 <= N < 15` runs exactly the
  migrations from input state `N` through input state `14`, appending each
  recorded state atomically.
- A database in the old RELOADED state-8/state-9 shape with premature
  `my_swaps.swap_type` is repaired to state `15` without duplicate-column
  errors, without recording state `16`, and without retaining fiat columns in
  `my_swaps`.
- A database already at state `15` performs no state-15-era schema migration.
- The `my_swaps`, `stats_swaps`, `my_orders`, `nodes`, and `stats_nodes`
  schemas match §§44.5-44.7 by state `15`, including the nullable/default
  declarations and the `timestamp_index` index.
- Fresh-database bootstrap creates only the state-1 objects. Running the
  full migration sequence on a fresh database shall not hit a duplicate-column
  error.
- Historical JSON imports run only at their bound migration states and do not
  require columns that are introduced by later migrations.
- Legacy saved-swap JSON containing `WatcherMessageSent` or
  `MakerPaymentSpendConfirmed` loads without an unknown-variant failure; replay
  either resumes from the last accepted event or preserves the raw history when
  replay is not possible.
- Legacy maker saved-swap JSON containing data-less `MakerPaymentRefundStarted`
  or `MakerPaymentRefundFinished` milestones loads without a missing-`data`
  failure. The historical payload-bearing refund-start form preserves its
  `wait_until` deadline.
- Native live swap/order/stats persistence is exercised only after migration
  completion and succeeds against the state-15 schema.
- Completion-fiat snapshots are stored in `stats_swaps`; `my_swaps` does not
  contain RELOADED-only `maker_coin_usd_price` or `taker_coin_usd_price`
  columns.

## 44.11 Provenance Footer

- *Inputs:* the project's own revision history and current tree (the
  shipped `MM2.db` migration ledger, version-1 bootstrap schema, and
  version-by-version migrations through state 15 -- by table/column/
  migration-number shape only, no code transcribed); the GLEEC-compatible
  shared schema baseline (state 15) and the dictated-interop rule that
  RELOADED-local persistence changes must not claim future shared
  `MM2.db` migration numbers (both explicitly named as this chapter's
  binding-scope source of truth); [Chapter 25](25-sql-query-builder.md)
  (the SQLite gateway substrate this chapter's native path uses);
  [Chapter 26](26-cross-platform-and-wasm.md) (the native/WebAssembly
  persistence split this chapter is scoped native-only against).
- *Permitted-input classes used:* baseline/as-built source (the shipped
  migration ledger and schema); dictated interop (the GLEEC-compatible
  shared schema/migration-number contract, R29/R31); cross-chapter
  contracts (Chapters 25, 26).
- *Sibling-allowlist consultations:* [Chapter 25](25-sql-query-builder.md),
  [Chapter 26](26-cross-platform-and-wasm.md).
- *Forbidden corpus:* not consulted.
- *Addition of 2026-11-22 (KDF Spec Reader, round 0.5.0-24; **KDF Dirty Gate
  pass: PASS on 2026-11-22, measure 0.00, no findings**):* §44.9A
  (R44.9A.1 through R44.9A.13, T44.9A.1 through
  T44.9A.5, its status notes and its code-quality finding). The *not
  consulted* statement above continues to describe every other section of
  this chapter. For §44.9A the forbidden corpus was consulted under the
  two-team procedure of chapter 01 §1.4A, at the rows `v2.6.0-beta` and
  `v3.0.0-beta` of chapter 02 §2.15A, identical at both for every fact
  stated; the migration step list alone was also read at the rows
  `dev (as of 2026-03-27)`, `staging (as of 2026-03-27)`,
  `main (as of 2026-10-07)`, `lr-swap-wip-3 (as of 2025-12-03)` and
  `tpu-status (as of 2026-01-08)`, and searched for across every release tag.
  The facts taken are: the highest migration state and the start-up outcome
  for a ledger above it; the lock file's name, place, content form, the
  outcome for each kind of existing content, its staleness limit and its
  removal; and, for saved-swap files, their names, which directory entries a
  directory-wide read picks, and the temporary-file-and-rename form of a
  write. File names, content formats and ledger values are dictated on-disk
  surface (R36). No private identifier, function body, helper decomposition,
  control-flow transcription, log or error literal was reproduced. R44.9A.13,
  the tests and the Reloaded status note are Reloaded-original; the status
  note rests on this repository's own lock and saved-swap code.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-22, measure 0.00,
  no findings, in a single pass; all seven chapter 02 §2.15A rows read were
  verified, including the two rows added in this round, `lr-swap-wip-3 (as of
  2025-12-03)` and `tpu-status (as of 2026-01-08)`. *Extent:* the reference's
  node-database migration ledger, the per-swap lock file (the primitive and
  its acquisition by the legacy maker and taker runs and by the version-two
  runs) and the reading and writing of saved-swap files, at the rows
  `v2.6.0-beta` and `v3.0.0-beta` for the whole subject (eight source files);
  the migration step list at the rows `dev (as of 2026-03-27)`, `staging (as
  of 2026-03-27)`, `main (as of 2026-10-07)`, `lr-swap-wip-3 (as of
  2025-12-03)` and `tpu-status (as of 2026-01-08)`; and a search of all 255
  release tags; on the candidate side, the 250 added lines. *Method:* reading
  plus a mechanical comparison of the added lines against 141 reference source
  files at the row `v2.6.0-beta` (identifier-token intersection and five-word
  shingles); the repository's clean-room gate tool was not applicable because
  the additions embed no code blocks. *Measure:* 0.00, on the
  discretionary-expression basis.
- *Clean-side addition of 2026-11-22 (PR #480):* the Reloaded status update
  under R44.9A.13. It is Reloaded-original, rests only on this repository's own
  write code and the cleared text of §44.9A, and changes no sentence of the
  chapter.
- *Clean-side addition of 2026-11-22 (PR #486):* the Reloaded status update
  under R44.9A.5. It is Reloaded-original, rests only on this repository's own
  start-up code and the cleared text of §44.9A, and changes no sentence of the
  chapter.
