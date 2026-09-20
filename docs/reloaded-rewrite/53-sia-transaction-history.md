# Chapter 53 -- Siacoin Transaction History

**Status:** driving-spec (required port, implemented). This chapter closes
[Chapter 20](20-siacoin-integration.md) §20.10 **D2 -- History persistence**
and gives the `tx_history` activation flag bound by
[Chapter 46](46-sia-v2-activation-rpcs.md) §46.1.2 an observable meaning.

> **One-sentence claim:** the project shall report Siacoin transaction
> history to a caller by reading the activated wallet address's event
> history from the walletd HTTP backend, mapping each supported event into
> the workspace's shared transaction-details record denominated in whole SC,
> caching those records in the coin-generic runtime history store, and
> serving them through the framework's transaction-history RPC surface with
> the shared paging, confirmation-count, and sync-status semantics every
> other coin already uses.

> **Treatment:** **T-PORT, implemented.** The Sia coin type, its walletd
> client, its `tx_history` activation flag, its history-sync-state cell, the
> coin-generic runtime history store, the coin-generic history RPC handler,
> and the Sia fee-details record were already present in reloaded before this
> port (see §53.7). The **history integration** this chapter specified --
> the event-to-record mapping, the background tracking task that populates
> the store, the three Sia transaction-type wire values, and the
> history-RPC acceptance of the Sia coin -- has been implemented against the
> contract distilled here.

> **Binding scope (R37).** Requirements bind observable behaviour, the public
> RPC method strings and their request/response JSON field names, the wire
> values of the shared transaction-record enumerations, and externally
> *dictated* interop (the walletd HTTP event endpoint and event object bound
> by ch. 20 §20.8, the Sia hastings unit ratio, and the public Sia Rust
> library's event API). Private Rust types, helper decomposition, task
> scheduling internals, module placement, and diagnostic wording are
> informative and are **not** bound by this chapter.

> **Source of truth (informative).** The transaction-history RPC envelopes,
> paging semantics, `sync_status` shape, and shared transaction-record field
> names are the published Komodo DeFi Framework API contract and are already
> realised by this repository's own baseline-derived history handler. The
> event endpoint, the event object's fields, and the event-type enumeration
> are the public walletd HTTP API and the public Sia Rust library API bound
> at the revision this workspace pins. Where this chapter and those public
> contracts disagree, the public contracts govern.

---

## 53.0 Executive summary

Siacoin holds no local chain state: walletd owns it (ch. 20 §20.0). Sia
transaction history is therefore not derived from a locally validated chain
or from a per-coin SQL index; it is a **projection of walletd's per-address
event log** into the workspace's shared transaction-details record, cached
locally so that a caller can page through it without re-querying walletd on
every request.

Three facts shape the whole design and are bound below:

1. Sia is **not** classified through the shared history-coin-type contract
   and does **not** use the shared per-coin SQL transaction-history schema.
   It uses the coin-generic **runtime history store** (the per-coin,
   per-address history cache: a file-backed store natively, an IndexedDB
   store under wasm) that the framework already provides for every coin, and
   the coin-generic history RPC path that reads it (§53.3).
2. The unit of history is a walletd **event**, not a transaction. Consensus
   events (miner and foundation payouts) have no inputs and no fee, and are
   represented alongside v1 and v2 transaction events (§53.5).
3. The bound walletd client revision exposes **no unconfirmed-event
   endpoint**, so Sia history is a confirmed-only view (§53.4, D53.2).

| Surface | Role |
| --- | --- |
| `my_tx_history` (tier-1 flat) | primary Sia history surface; shared paging/`sync_status` envelope (§53.2.1) |
| `my_tx_history` (mmrpc 2.0) | additive reloaded acceptance of Sia through the runtime-history path (§53.2.2) |
| `GET /api/addresses/:addr/events` | dictated walletd source of the event log (ch. 20 §20.8, §53.4) |
| `task::enable_sia::init` → `tx_history` | switch that starts tracking (ch. 46 §46.1.2/§46.1.4, §53.6) |

---

## 53.1 Baseline verification

Siacoin is a **post-2022** feature; so is its history path. Both are absent
from the baseline anchor of [Chapter 02](02-baseline-state.md) (commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`, 3 June 2022): the baseline tree
carries no Sia coin-support module, no Sia coin-enum variant, and therefore
no Sia history classification, no Sia event mapping, and no Sia entry in any
history dispatch. The classification here is by *component role and
first-introduction epoch only*.

| Component (role) | Basis |
| --- | --- |
| Sia event-to-history-record mapping | Introduced after the baseline anchor. |
| Sia history tracking task | Introduced after the baseline anchor. |
| Sia transaction-type wire values | Introduced after the baseline anchor. |

What *is* baseline-derived, and is therefore reused rather than
re-specified, is the surrounding machinery: the shared transaction-details
record, the coin-generic runtime history store and its RPC handler, the
paging and confirmation-count arithmetic, and the history-sync-state
enumeration. This chapter binds only the Sia-specific projection onto that
machinery.

---

## 53.2 RPC surface

### 53.2.1 `my_tx_history` (tier-1 flat) -- primary surface

**R53.2.1** An activated Sia coin shall be served by the framework's
coin-generic tier-1 `my_tx_history` method. No Sia-specific history method
string is introduced.

**R53.2.2** The request object shall accept, in addition to the method
envelope the tier-1 dispatcher dictates:

- `coin` (string, required) -- the Sia coin ticker;
- `limit` (integer, optional, default **10**) -- maximum records returned;
- `max` (boolean, optional, default **false**) -- when true the whole stored
  history is returned and `limit` is treated as the total record count;
- `from_id` (byte string, optional) -- from-id paging anchor, matched against
  a record's `internal_id`;
- `page_number` (integer ≥ 1, optional) -- page-number paging.

**R53.2.3** Paging semantics are the shared ones and shall not be
Sia-specialised:

- when `from_id` is supplied, the response begins with the record
  *following* the record whose `internal_id` equals it, and the number of
  records passed over (including the anchor) is reported as `skipped`;
- when `from_id` is absent and `page_number` is supplied, `(page_number - 1)
  * limit` records are passed over;
- when neither is supplied, paging starts at the first record;
- `from_id` takes precedence over `page_number` when both are supplied.

**R53.2.4** The success result shall carry the shared field set:
`transactions` (array), `limit`, `skipped`, `from_id`, `total`,
`current_block`, `sync_status`, `page_number`, and `total_pages`. `total` is
the count of stored records before paging; `current_block` is the Sia chain
tip height read from walletd (ch. 20 §20.8); `sync_status` is the value of
§53.6.3.

**R53.2.5** Each element of `transactions` shall be the shared
transaction-details record of §53.5 with a `confirmations` field added. The
confirmation count is derived, not stored: it is `0` when the record's
`block_height` is `0` or exceeds `current_block`, and otherwise
`current_block - block_height + 1`. A caller shall never observe a Sia
record whose `confirmations` disagrees with that arithmetic.

**R53.2.6** A `from_id` that matches no stored record shall be rejected
rather than silently returning the first page. This is the shared tier-1
behaviour and is not Sia-specific.

### 53.2.2 `my_tx_history` (mmrpc 2.0)

**R53.2.7** An activated Sia coin shall additionally be accepted by the
mmrpc-2.0 `my_tx_history` method and served from the same runtime history
store, through the same runtime-history path the method already uses for the
other runtime-store-backed coins. Consequently the `NotSupportedFor`
discriminant shall **not** be observable for an activated Sia coin.

**R53.2.8** In the mmrpc-2.0 envelope, the request carries `coin`, `limit`
(default **10**), and `paging_options` (the shared page-number / from-id
union keyed on the byte-string `internal_id`); the result carries `coin`,
`current_block`, `transactions`, `sync_status`, `limit`, `skipped`, `total`,
`total_pages`, and the echoed `paging_options`. The per-record content and
the `confirmations` derivation are those of R53.2.5.

> **Upstream divergence (informative).** The upstream lineage serves Sia
> history through the tier-1 method only and rejects Sia at the mmrpc-2.0
> method with the not-supported discriminant. R53.2.7 is a deliberate
> **additive** KDF Reloaded extension: it accepts a request upstream refuses
> and changes no response that upstream produces, so no existing caller
> observes a behaviour change. Implementers must not take R53.2.7 as licence
> to route Sia through the SQL-backed history storage -- that remains
> deferred (D53.1).

### 53.2.3 Observable error conditions

**R53.2.9** The bound error surface is the shared transaction-history error
contract; no Sia-specific discriminant is introduced. The discriminants a
caller can observe for a Sia request, and the only conditions that produce
them, are:

| Discriminant (mmrpc 2.0 `error_type`) | HTTP status | Condition for a Sia request |
| --- | --- | --- |
| `CoinIsNotActive` | 404 | the named Sia ticker is not an activated coin |
| `RpcError` | 500 | walletd could not be reached to read the chain tip |
| `StorageError` | 500 | the runtime history store could not be read |
| `NotSupportedInWasm` | 400 | wasm build, where the mmrpc-2.0 method is unavailable |

**R53.2.10** `StorageIsNotInitialized` shall **not** be observable for a Sia
coin, because Sia does not use the per-coin SQL history storage whose
initialization that discriminant reports (§53.3). `NotSupportedFor` shall not
be observable for an activated Sia coin (R53.2.7).

**R53.2.11** A Sia coin activated **without** `tx_history` is still a valid
history target: the request succeeds, `transactions` is empty, `total` is
`0`, and `sync_status` reports the not-enabled state (§53.6.3). Absence of
tracking is not an error.

---

## 53.3 History classification and storage substrate

**R53.3.1** Sia shall **not** be mapped into the shared history-coin-type
classification, and no Sia arm shall be added to it. Sia is neither a
platform coin with child tokens nor a token, and it does not participate in
the SQL-indexed history path that classification exists to key.

**R53.3.2** Sia history records shall be persisted in the **coin-generic
runtime history store**: the per-coin, per-address history cache the
framework already provides -- a file-backed store on native targets and the
IndexedDB transaction-history store under wasm. The store is keyed by the
pair (**coin ticker**, **wallet address**); the ticker key is the Sia coin's
configured ticker exactly as activated, and the address key is the activated
single address of ch. 20 §20.4.1.

**R53.3.3** No new on-disk schema is dictated by this chapter. The shared
per-coin SQL transaction-history and transaction-cache tables are **not**
created for Sia, are not extended for Sia, and carry no Sia rows; the
runtime store's existing serialised-record format is reused unchanged, so a
Sia record is a shared transaction-details record and nothing more.

**R53.3.4** The store's read/write granularity is the **whole record set for
the coin**: a tracking pass replaces the persisted set with its current
in-memory set. An implementation shall therefore never leave the persisted
set holding records the in-memory set has pruned (§53.6.5).

**R53.3.5** Persistence failure shall stop tracking for that coin and leave
the last successfully persisted set readable; it shall not truncate,
partially rewrite, or corrupt the stored set, and it shall not abort the
daemon.

> **Cross-reference.** [Chapter 44](44-database-persistence-and-migrations.md)
> governs the swap/order/statistics database and does **not** govern this
> store; no chapter-44 migration is required by, or permitted to be
> triggered by, Sia history.

---

## 53.4 Fetching from the walletd backend

**R53.4.1** The event source is the dictated walletd address-events endpoint
already bound by ch. 20 §20.8 (`GET /api/addresses/:addr/events`), reached
through the Sia HTTP client the coin was activated with. No walletd URL is
introduced by the history path; it is the caller-supplied backend of ch. 20
§20.4 / ch. 46 §46.1.2.

**R53.4.2** The address queried is the activated wallet's single address. A
coin whose signing policy does not yield a single address shall not start
tracking and shall report the error sync state rather than panicking;
multi-address history is deferred (D53.5).

**R53.4.3** The bound Sia client revision's address-events request carries
the address plus an **optional limit** and an **optional offset**, both
interpreted by walletd against walletd's own ordering. A tracking pass shall
retrieve the address's **complete** event set; when limit/offset are used to
retrieve it in parts, every part must be retrieved before the pass completes,
because record identity, pruning (§53.6.5), and the remaining-count status
(§53.6.3) are all defined over the complete set.

**R53.4.4** An implementation shall **not** depend on walletd's return
ordering for correctness. Records are identified by the event identifier
(R53.5.2), so an ordering change from walletd may reorder a response but
must never duplicate, drop, or misattribute a record.

**R53.4.5** The order in which records are **served** shall be deterministic
and stable for an unchanged stored set, so that from-id paging (R53.2.3) is
well defined: descending `block_height`, ties broken by ascending
`internal_id`. Two successive requests against an unchanged history shall
return identical pages.

**R53.4.6** Mempool and other unconfirmed activity is **not** represented.
The bound client revision exposes no unconfirmed-event endpoint, and the
address-events endpoint reports events that consensus has already recorded.
A broadcast-but-unmined Sia transfer is therefore absent from history until
it is mined; it must not be synthesised locally from the broadcast path
(D53.2).

**R53.4.7** A walletd failure during a pass shall leave the stored set
intact, leave the sync status unchanged or set to the error state, and be
retried on a later pass. It shall not clear history and shall not be
reported to the caller as an empty history.

---

## 53.5 Mapping a walletd event to a history record

**R53.5.1** Of the event types the public Sia library enumerates, exactly
four shall be represented in history: **v1 transaction**, **v2
transaction**, **miner payout**, and **foundation payout**. The remaining
types -- **siafund-claim payout**, **v1 contract resolution**, and **v2
contract resolution** -- shall be skipped silently: they are neither stored
nor counted in `total`, and skipping one is not an error (D53.3).

**R53.5.2** The record identifier is the **event's own identifier** (a
32-byte hash). `internal_id` carries its raw bytes; `tx_hash` carries its
lowercase hex encoding. For the two transaction event kinds this identifier
is the Sia transaction id; for the two payout kinds it is the identifier of
the created output. One event yields exactly one record, and the identifier
is the record's primary key in the store (§53.6.4). The Sia withdraw path
applies the same identity rule to the record it returns for a transaction it
has just signed (ch. 20 §20.9.2 R-W10), so a withdrawal result and the history
record that later covers the same transaction share one primary key.

**R53.5.3** All monetary fields shall be denominated in **whole SC** using
the dictated `1 SC = 10^24 hastings` ratio of ch. 20 §20.5 (R-U1). No field
of a history record is expressed in hastings.

**R53.5.4 (transaction events).** For a v1 or v2 transaction event:

- `from` -- the addresses of the outputs the transaction consumes;
- `to` -- the addresses of the outputs the transaction creates;
- `total_amount` -- the sum of the consumed outputs' values;
- `spent_by_me` -- the sum of the consumed outputs whose address is the
  wallet address;
- `received_by_me` -- the sum of the created outputs whose address is the
  wallet address;
- `my_balance_change` -- `received_by_me - spent_by_me`, which may be
  negative;
- `fee_details` -- present, in the Sia fee-details shape (the coin ticker, a
  fee-policy discriminant, and the total fee amount), with the fee equal to
  consumed value minus created value.

**R53.5.5** A v1 transaction event may create more value than it consumes
(consensus-funded output creation). The fee computation shall be **saturating
at zero**: such an event yields a reported fee of `0`. Numeric underflow,
wrapping, or a panic on this path is a defect.

**R53.5.6** The fee-policy discriminant reported for a history record shall
be the **unspecified/unknown** member of the Sia fee-policy enumeration. A
walletd event does not record which policy produced the fee, so no policy
may be inferred or fabricated (D53.7).

**R53.5.7 (payout events).** For a miner or foundation payout event:

- `from` -- empty (payouts are created by consensus, not spent from an
  address);
- `to` -- the single payout address;
- `total_amount` -- the payout value;
- `spent_by_me` -- `0`;
- `received_by_me` -- the payout value when the payout address is the wallet
  address, otherwise `0`;
- `my_balance_change` -- equal to `received_by_me`;
- `fee_details` -- absent (`null`); a payout pays no fee.

**R53.5.8** For every represented kind, `block_height` is the height of the
event's chain index and `timestamp` is the event's timestamp expressed as
whole Unix seconds. The event's maturity height and its walletd-reported
confirmation count are **not** stored: the confirmation count a caller sees
is derived from the chain tip at request time (R53.2.5), so a stored record
never carries a stale count.

**R53.5.9** `coin` is the activated Sia ticker. `kmd_rewards` is absent for
every Sia record.

**R53.5.10** The `transaction_type` field shall carry one of three wire
values reserved for Sia:

| Wire value | Assigned to |
| --- | --- |
| `SiaV1Transaction` | v1 transaction events |
| `SiaV2Transaction` | v2 transaction events |
| `SiaMinerPayout` | miner **and** foundation payout events |

These are additions to the shared transaction-type enumeration; adding them
shall not change the wire form, default, or meaning of any existing member,
and the default for a record that does not set the field remains the
standard-transfer member.

These wire values describe *what a record is*, not which subsystem produced
it. The Sia withdraw path therefore reports the same v2-transaction value on
the transaction-details object it returns for a freshly signed transfer
(ch. 20 §20.9.2 R-W9); the withdraw and history paths shall not disagree about
this field for one transaction.

**R53.5.11** The shared record's raw-transaction field carries **no data**
for a Sia record (an empty byte string). A walletd event is not a
rebroadcastable serialised transaction, and a caller shall not treat that
field as one for Sia. Carrying a typed Sia payload there is deferred
(D53.4).

This is the one field where a history record and a Sia *withdraw* record
differ by design. A withdraw response holds the signed transaction itself, so
it carries both a non-empty `tx_hex` and a top-level `tx_json` object
(ch. 20 §20.9.1 R-W6 / R-W7). A history record is projected from a walletd
event, which is not that transaction, so it carries neither: a caller shall
not expect `tx_json` on a Sia history record while D53.4 remains open.

**R53.5.12** Mapping is a pure function of the event and the wallet address:
no walletd call, no chain-tip read, and no clock read participates in it, so
mapping the same event twice yields byte-identical records.

**R53.5.13** A represented event that cannot be mapped shall surface through
the error sync state (§53.6.3) and shall not be stored as a partial or
placeholder record.

---

## 53.6 Tracking lifecycle

**R53.6.1** The `tx_history` boolean of ch. 46 §46.1.2 is the switch. When
it is **false** (the default), the coin is created with the not-enabled sync
state, no tracking task performs work, and the store is neither read nor
written for that coin. When it is **true**, the coin is created with the
not-started sync state and tracking begins once activation has completed and
the coin is registered -- this is the observable meaning of "start history
tracking" in ch. 46 §46.1.4.

**R53.6.2** Tracking is a **background** activity. Neither activation nor
any history request blocks on a tracking pass, and a history request never
triggers one.

**R53.6.3** The `sync_status` a caller observes in either history response
shall be one of the shared history-sync states:

| State | Meaning for Sia |
| --- | --- |
| not-enabled | activated without `tx_history`; no tracking (R53.2.11) |
| not-started | `tx_history` set, first pass has not yet produced a result |
| in-progress | a pass is populating the store; carries a `transactions_left` count of records identified but not yet stored |
| error | tracking stopped or a pass failed; carries diagnostic JSON |
| finished | the last pass completed and the store matches the fetched event set |

The in-progress count shall decrease monotonically within a pass as records
are stored, and reaching the finished state requires that the store contains
a record for every represented event of the fetched set.

**R53.6.4 (idempotency).** Records are keyed by the event identifier
(R53.5.2). A pass shall **insert only**: an identifier already present in
the store is left untouched rather than remapped or rewritten. Consequently
repeated passes over an unchanged event set leave the stored set unchanged
and end in the finished state, and history contains no duplicate
`internal_id`.

**R53.6.5 (pruning).** A stored record whose identifier is absent from the
fetched event set shall be removed, and the persisted set updated to match
(R53.3.4). This is how a reorganised-away event leaves history.

**R53.6.6 (cadence and quiescence).** A pass shall be skipped while the
wallet balance is unchanged since the previous successful pass and no stored
record requires updating; otherwise passes repeat on a fixed steady-state
interval. A backend failure retries on a shorter interval without clearing
the stored set (R53.4.7). The exact intervals are an implementation choice
and are not bound.

**R53.6.7** Tracking shall stop cleanly when the daemon is shutting down or
when the coin is no longer registered; a coin that briefly disappears from
the registry may be waited on for a bounded number of attempts before
tracking gives up. Stopping is not an error state in itself.

**R53.6.8** There is no all-or-nothing visibility guarantee: records become
visible to a caller as a pass stores them, which is exactly what the
in-progress status reports. A caller that needs a complete view polls until
`sync_status` is finished.

---

## 53.7 Implementation-substrate note (informative)

This note orients the implementer; it is not normative.

**Already present in reloaded:**

- the Sia coin type with its walletd client and its history-sync-state cell,
  and the Sia activation request's `tx_history` flag, which already selects
  the not-enabled vs. not-started initial sync state at build time;
- the coin-generic runtime history store (native file-backed and wasm
  IndexedDB) with its load/save accessors on the coin trait surface, and the
  framework hook that spawns a per-coin history task when a coin is
  registered with history enabled;
- the tier-1 `my_tx_history` handler with the paging, `confirmations`, and
  `sync_status` behaviour of §53.2.1 -- it is coin-generic and already
  reaches an activated Sia coin;
- the mmrpc-2.0 `my_tx_history` handler including its runtime-history branch
  for runtime-store-backed coins;
- the shared transaction-details record, the history-sync-state enumeration,
  the Sia fee-details record and its membership in the shared fee-details
  union, and the hastings/SC conversion helpers;
- the Sia HTTP client's address-events call.

**Added by this chapter's port (implemented):**

- the three transaction-type wire values of R53.5.10;
- the event-to-record mapping of §53.5;
- the tracking pass and its lifecycle of §53.6, replacing the previously
  inert history loop that ch. 20 §20.10 D2 recorded (now closed);
- acceptance of the Sia coin on the mmrpc-2.0 runtime-history branch
  (R53.2.7).

Module placement should follow the existing Sia coin-support layout; the
decomposition of the mapping and the tracking pass is the implementer's
choice. Because the mapping is pure (R53.5.12), it is directly unit-testable
from constructed events without a walletd instance, and that is the expected
regression surface for §53.5.

---

## 53.8 Deferred work

The following are explicit gaps, documented as deferred work rather than
defects. None is a correctness claim. This chapter **closes** ch. 20 §20.10
D2; the residue is narrower.

- **D53.1 -- SQL-indexed history storage.** Sia is not classified into the
  shared history-coin-type contract and creates no per-coin SQL history
  tables (§53.3). Server-side filtering and indexed paging over large Sia
  histories are therefore unavailable; paging is applied to the whole stored
  set.
- **D53.2 -- Unconfirmed/mempool entries.** The bound client revision
  exposes no unconfirmed-event endpoint, so history is confirmed-only
  (R53.4.6). Surfacing a pending transfer, and transitioning it to confirmed
  in place, is deferred.
- **D53.3 -- Contract and claim events.** Siafund-claim payouts and v1/v2
  contract resolutions are skipped (R53.5.1). Representing storage-contract
  activity in wallet history is deferred.
- **D53.4 -- Typed transaction payload.** The shared record's
  raw-transaction field is empty for Sia (R53.5.11); exposing the event's
  typed payload to callers is deferred. The withdraw path already binds the
  carrier shape such a payload would use if this were closed -- a top-level
  `tx_json` object alongside `tx_hex` (ch. 20 §20.9.1 R-W7) -- so closing
  D53.4 means populating those two fields from the event's transaction, not
  inventing a second carrier shape.
- **D53.5 -- Multi-address history.** Only the activated single address is
  queried (R53.4.2), matching Sia activation's account-0-only scope
  (ch. 20 §20.4.1; ch. 46 R46.1.3). Ch. 20 §20.10 D1 (multi-account HD) is
  now closed, so a wallet may hold additional discovered HD addresses beyond
  the activated one; aggregating history across that discovered address set
  remains deferred here.
- **D53.6 -- History streaming.** Sia history records are not published to
  the event-streaming surface; a caller polls (§53.6.8).
- **D53.7 -- Fee-policy attribution.** The reported fee policy is the
  unspecified member (R53.5.6); attributing a fee to a concrete policy is
  deferred.

---

## 53.9 Baseline verifications

The following are verifiable from the baseline state defined in
[Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree contains no Siacoin coin-support module and therefore
    no Sia history path: a tree-wide case-insensitive `git grep -li 'sia'`
    against the baseline returns no coin-support matches, and the baseline
    coin enum carries no Sia variant. Every Sia-specific requirement of this
    chapter is consequently a post-anchor addition, as ch. 20 §20.11 V1/V2
    already record.

V2. The machinery this chapter reuses **is** present at the baseline: the
    shared transaction-details record with its `tx_hex` / `tx_hash` /
    `from` / `to` / `total_amount` / `spent_by_me` / `received_by_me` /
    `my_balance_change` / `block_height` / `timestamp` / `fee_details` /
    `coin` / `internal_id` / `kmd_rewards` / `transaction_type` fields; the
    coin-generic runtime history store and the tier-1 `my_tx_history`
    handler with its `from_id` / `page_number` / `limit` / `max` request
    fields and its `transactions` / `limit` / `skipped` / `from_id` /
    `total` / `current_block` / `sync_status` / `page_number` /
    `total_pages` result fields; the confirmation-count arithmetic of
    R53.2.5; and the history-sync-state enumeration including the
    in-progress `transactions_left` count. §53.2, §53.3, and §53.6 bind
    Sia's use of that baseline contract, not a new one.

V3. The baseline transaction-type enumeration carries no Sia member, so the
    three wire values of R53.5.10 are additions; the baseline history-coin-
    type classification predates Sia entirely, which is why R53.3.1's
    non-classification changes nothing a baseline caller could observe.

V4. The walletd address-events endpoint, the event object's identifier /
    chain-index / timestamp / maturity-height / type / payload structure,
    the event-type enumeration, and the `1 SC = 10^24 hastings` ratio are
    properties of the public Sia protocol, the public walletd HTTP API, and
    the public Sia Rust library bound by this workspace -- not artefacts of
    any project lineage.

---

## 53.10 Acceptance criteria

- A Sia coin activated with `tx_history` set eventually reports a
  `sync_status` of finished, and `my_tx_history` returns one record per
  represented walletd event for the wallet address (§53.5.1, §53.6.3).
- A Sia coin activated without `tx_history` answers `my_tx_history`
  successfully with an empty `transactions` array, `total` of `0`, and the
  not-enabled sync status (R53.2.11).
- Amounts, fees, and balance changes are reported in whole SC; a v1 event
  that creates more value than it consumes reports a fee of `0` rather than
  underflowing (R53.5.3, R53.5.5).
- Payout records carry an empty `from`, a zero `spent_by_me`, and no
  `fee_details`; transaction records carry both address lists and a
  populated `fee_details` (R53.5.4, R53.5.7).
- `internal_id` equals the event identifier's raw bytes and `tx_hash` its hex
  form; from-id paging anchored on a returned `internal_id` yields the
  following records with a correct `skipped` count (R53.2.3, R53.5.2).
- `confirmations` on every returned record matches the chain tip arithmetic
  of R53.2.5, and two successive identical requests against an unchanged
  history return identical pages (R53.4.5).
- Re-running tracking over an unchanged event set produces no duplicate
  `internal_id` and no rewritten record; an event that disappears from the
  fetched set disappears from history and from the persisted set (R53.6.4,
  R53.6.5).
- A walletd outage during tracking leaves previously stored records readable
  and does not surface as an empty history (R53.4.7).
- `NotSupportedFor` and `StorageIsNotInitialized` are not observable for an
  activated Sia coin (R53.2.10).

---

## 53.11 Provenance Footer

- *Inputs:* the project's own revision history and current tree (for the
  epoch classification of §53.1, the baseline verifications of §53.9, and
  the already-present substrate inventory of §53.7 -- by component role and
  public contract only, no code transcribed); the baseline anchor of
  [Chapter 02](02-baseline-state.md), which supplies the shared
  transaction-details record, the runtime history store, the tier-1
  `my_tx_history` request/response field names and paging/confirmation
  semantics, the history-sync-state enumeration and its in-progress
  `transactions_left` count, and the shared history error discriminants; the
  public walletd HTTP API (the address-events endpoint of ch. 20 §20.8 and
  its optional limit/offset paging); the public Sia Rust library API bound
  at this workspace's pinned revision (the event object's identifier, chain
  index, confirmation count, timestamp, maturity height, event-type
  enumeration, and typed payload union, and the address-events request
  carrying an address plus optional limit and offset); the public Sia
  protocol's hastings unit and `1 SC = 10^24 hastings` ratio; and
  cross-chapter contracts (Chapters 20, 44, 46).
- *Permitted-input classes used:* baseline source (the shared history
  machinery this chapter reuses, and absence verification for Sia); external
  public specifications (the Sia protocol units, the walletd HTTP API, the
  public Sia Rust library API); cross-chapter contracts (Chapters 20, 44,
  46); Interop / wire-and-API-bound reuse (R29/R31/R33) for the dictated
  fragments embedded in §53.2 (the public history method strings, request
  and response field names, and error discriminants), §53.4 (the walletd
  address-events endpoint and its limit/offset paging), and §53.5 (the event
  object's fields, the event-type enumeration, the hastings ratio, and the
  transaction-type wire values a caller reads) -- whose authoritative source
  is the bytes and calls any conforming walletd instance, library consumer,
  or KDF-family API client must exchange for interoperability, not the
  historical lineage's discretionary expression.
- *Sibling-allowlist consultations:* [Chapter 20](20-siacoin-integration.md)
  (§20.4/§20.4.1 the activated single address, §20.5 the units contract,
  §20.8 the walletd endpoint set, §20.9.1-§20.9.2 the withdraw path's
  transaction carrier and its reuse of this chapter's record-identity and
  transaction-type rules, §20.10 D1/D2 the deferred boundary this
  chapter narrows); [Chapter 44](44-database-persistence-and-migrations.md)
  (to establish that the swap/order/statistics database is *not* this
  chapter's substrate); [Chapter 46](46-sia-v2-activation-rpcs.md) (§46.1.2
  the `tx_history` activation parameter and its default, §46.1.4 the
  activation step this chapter gives meaning to).
- *Forbidden corpus:* consulted, by the documented Spec Reader workflow, to
  answer four questions that no allowed source could answer: (a) whether the
  historical lineage implements Siacoin transaction history at all, and
  through which of the two history tiers and which storage substrate --
  establishing the facts bound in §53.3 and the divergence note in §53.2.2;
  (b) which walletd event kinds the lineage represents versus skips
  (R53.5.1); (c) the observable field-level projection of an event onto the
  shared transaction record -- the direction/amount/address/fee/height/
  timestamp/identifier assignments of §53.5, restated here as behavioural
  requirements; and (d) the observable lifecycle contract of the tracking
  activity -- its enablement switch, its sync-status progression, its
  insert-only keying, and its pruning rule (§53.6). Only the externally
  visible outcome of each was carried across, embedded as behavioural
  requirements or, where third-party- or API-dictated, as Interop reuse
  under R29/R31/R33. No discretionary expression from the post-2022 module
  crosses into this chapter: no private type, field, helper, or variable
  names; no helper decomposition; no control-flow transcription; no
  per-method tables keyed to internal names; no module trees or test names;
  and no diagnostic, log, or panic string literals. The three
  transaction-type values of R53.5.10 and the response/request field names
  of §53.2 are carried deliberately as *wire* values a third-party GUI reads
  and sends, not as internal expression. The ordering guarantee of R53.4.5,
  the mapping purity of R53.5.12, and the acceptance of Sia on the mmrpc-2.0
  method (R53.2.7) are independent KDF Reloaded strengthenings, marked as
  such. Any residual similarity of a conformant realisation to the
  historical lineage is governed by the R35 gate and the binding-scope note
  heading this chapter.
