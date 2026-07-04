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

> **Upstream divergence (informative).** The active RELOADED tree's legacy
> event parser may be narrower than the historical persisted event family. This
> chapter binds the broader persisted JSON family so existing saved swaps can
> be loaded, replayed, or retained without data loss.

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
- Native live swap/order/stats persistence is exercised only after migration
  completion and succeeds against the state-15 schema.
- Completion-fiat snapshots are stored in `stats_swaps`; `my_swaps` does not
  contain RELOADED-only `maker_coin_usd_price` or `taker_coin_usd_price`
  columns.
