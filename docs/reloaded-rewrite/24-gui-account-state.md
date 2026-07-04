# Chapter 24 -- GUI-Facing Account-State Persistence

**Status:** driving-spec

> **One-sentence claim:** the project carries a dedicated
> workspace crate for persisting GUI-visible "wallet account"
> state -- one record per Iguana / HD / hardware-wallet
> account, with display metadata and a per-account activated-
> ticker set -- behind a single async trait with a native
> backend and a browser-target stub, and exposes an eleven-
> method JSON-RPC namespace as its public surface.

## 24.0 Executive Summary

A GUI consumer of the chapter-bound substrate needs a stable
concept of "the current account" that is distinct from the
in-process wallet identity used by the coin support modules and
the swap engine.
A dedicated workspace crate provides that surface: it stores
per-user account records, per-account display metadata (name,
description, fiat balance), and the set of activated coin
tickers each account has selected, all persisted between
daemon runs.

The crate is bounded by the following architectural rules:

1. **Three account-identity variants.** Iguana (legacy single-
   account mode), HD (BIP-44-style indexed accounts on a
   single seed), and HW (hardware-wallet device-keyed
   accounts) coexist under one identity enum. The crate does
   not invent additional account kinds.
2. **One trait, two backend implementations.** A single async
   trait abstracts persistence. The native build target carries
   a fully-functional native-SQL backend; the browser build
   target carries a stub backend that returns an explicit
   "not implemented" error from every method.
3. **Eleven JSON-RPC handlers** form the public surface a GUI
   uses to manage accounts and their activated-coin sets.
4. **Validation at the trait boundary.** Length and shape
   validation runs in the trait/handler layer before any
   storage call; backends do not re-check inputs.

In the chapter-bound substrate the native backend, the type surface,
the trait, and the handler module are complete; the browser
backend is the stub of (2); and the eleven handlers are not
yet registered in the public RPC dispatcher.

**Port status.** The `mm2_gui_storage` crate (backend, type
surface, trait, and the eleven typed handlers) is present in
reloaded as a **library**, but its public `gui_storage::` JSON-RPC
namespace is **required but NOT yet registered in reloaded's
dispatcher** (and `mm2_gui_storage` is not yet a dependency of the
application crate). Per the project's PORT decision the dispatcher
registration is a **binding driving-spec requirement**, not
optional deferred work; the required public method surface is
specified normatively in §24.9A and §24.7.

## 24.1 Subsystem Shape

The crate is organised into the following functional regions:

| Region                     | Responsibility                                |
|----------------------------|-----------------------------------------------|
| Public re-exports          | Account identity, metadata, error types       |
| Per-context handle         | Lazy-init handle pattern; owns the backend    |
| Account identity & metadata| The identity enum and the metadata record     |
| Storage trait              | Async CRUD + activation surface               |
| Storage -- native          | Three-table backend over the native SQL layer |
| Storage -- browser         | Stub returning "not implemented" everywhere   |
| RPC handlers               | Eleven typed handlers, one per surface method |

The per-context handle is obtained through the lazy-init
pattern of the central-context substrate of
[Chapter 31](31-central-application-context.md); call sites
acquire the handle from the central context and never construct
the backend directly.

## 24.2 Account Identity

The wire-level account identity is a tagged enum with three
variants:

| Variant | Carries                          | Purpose                                |
|---------|----------------------------------|----------------------------------------|
| Iguana  | (no payload)                     | Legacy single-account mode             |
| HD      | 32-bit account index             | BIP-44-style indexed HD account        |
| HW      | 160-bit hardware-device pubkey   | Hardware-wallet-keyed account          |

R1. **Closed identity set.** The three-variant identity enum
    is the single registration point for an account kind. New
    account kinds (for example, a future MPC-keyed account)
    require an explicit additional variant; the storage
    layout (§24.4) shall accommodate any new variant through
    its composite primary key without schema change.

R2. **Enabled-account restriction.** Only Iguana and HD
    variants are eligible to be marked as the **enabled**
    (active) account. Hardware-wallet accounts can be added,
    enumerated, renamed, and removed, but cannot be selected
    as the active context through this surface. (The
    rationale: hardware-wallet activation has a different
    lifecycle managed elsewhere; this surface intentionally
    does not own it.)

## 24.3 Account Metadata

Each account record carries:

| Field         | Constraint                                                    |
|---------------|---------------------------------------------------------------|
| Display name  | Up to 255 characters (`MAX_ACCOUNT_NAME_LENGTH = 255`)        |
| Description   | Up to 600 characters (`MAX_ACCOUNT_DESCRIPTION_LENGTH = 600`) |
| Fiat balance  | Decimal value stored as a string, up to 255 characters,       |
|               | required at storage but omittable on the wire (defaults to "")|

The ticker-length cap MUST be `MAX_TICKER_LENGTH = 255`
(applied by the boundary ticker validation of §24.7 R-R7 and by
the coins-table `coin` column of §24.5/§24.6.1).

R3. **Validation at the trait boundary.** Name length,
    description length, ticker length, and balance shape are
    validated by the trait or handler layer **before** any
    storage call is issued. Backends do not re-check; they
    rely on the trait-layer guarantee.

R4. **Balance is opaque.** The fiat balance is stored as a
    decimal-string scalar; it is not interpreted as currency,
    not used for any in-tree calculation, and not bound to
    any specific currency code. The field is a pass-through
    cache from whatever oracle the GUI consults.

R5. **`AccountId` wire shape.** The identity enum of §24.2
    MUST serialise as a serde adjacently-tagged JSON object
    with the discriminator key `type` and lowercase variant
    names (`#[serde(tag = "type", rename_all = "lowercase")]`).
    Wire forms: `{"type":"iguana"}`, `{"type":"hd","account_idx":N}`,
    `{"type":"hw","device_pubkey":"<hex>"}`. `EnabledAccountId`
    MUST use the same shape with no `hw` variant. These JSON
    forms and the discriminator key are the dictated wire
    contract (R29); the Rust identifiers shown are interface,
    and any other expression alongside them is informative
    (R36).

R6. **`account_type` discriminator integers.** The native
    backend round-trips `account_type` as an integer column of
    the on-disk schema (§24.6.1 R-N2). The integer encoding
    MUST be fixed at:
    `Iguana = 0`, `HD = 1`, `HW = 2`. Changing these values is
    a breaking change to stored data and is not in scope for
    this chapter.

R7. **Identity traits.** `AccountId` MUST derive `Clone`,
    `Debug`, `Eq`, `Ord`, `Hash`, `Serialize`, `Deserialize`
    so that it can be used as an ordered-map key (§24.6.2)
    and as both an RPC-request field and a serialised
    response field. The three value types `AccountInfo`,
    `AccountWithCoins`, `AccountWithEnabledFlag` MUST derive
    `Clone`, `Debug`, `Serialize`, `Deserialize`.

The activated-coins set hangs off each account as a separate
set of (account-key, ticker) pairs (§24.4).

## 24.4 Storage Trait

The single async storage trait exposes the following
behavioural operations:

| Category            | Operations                                                   |
|---------------------|--------------------------------------------------------------|
| Lifecycle           | `init` (idempotent schema/connect bootstrap)                 |
| Account CRUD        | upload one, delete one, load all (with-enabled-flag)         |
| Enabled-account     | Load enabled-id, load enabled with coins, set enabled        |
| Metadata setters    | Set name, set description, set balance                       |
| Activated coins     | Activate tickers, deactivate tickers, load tickers           |

All trait methods are async and return the project's standard
`MmError`-wrapped result type, parameterised by the crate's
own storage-error enum.

R5. **Single trait surface.** All persistence operations on
    GUI account state flow through this trait. The handlers
    in §24.7 are the only public surface above the trait; no
    callers reach into the backend directly.

R6. **No single-account loader on the trait.** The trait MUST
    NOT expose a standalone single-account loader. Single-
    account access is covered instead by the operation that
    loads the enabled account together with its coins, and by
    the operation that loads an account's activated coin set;
    the latter returns that coin set and surfaces the
    no-such-account condition when the set is empty and the
    account does not exist (§24.6.2 R-N8). Whether a
    backend keeps an internal single-account helper is an
    implementation detail; the trait surface above the
    backends does not expose such a method.

R7. **Boxed per-context handle.** The crate MUST expose a
    public boxed-trait-object alias for the async storage trait
    — a single per-context storage handle — together with a
    target-gated constructor that returns one such boxed handle
    per `MmArc`:

    - Native target: a constructor over the SQLite backend,
      boxed as the storage-handle alias.
    - WASM target: the IndexedDB backend boxed equivalently.

    The per-context handle of §24.1 holds a lazily-initialised
    cell of that boxed storage handle (or equivalent) and its
    storage accessor returns the boxed trait object on first
    call, reusing the cached instance thereafter.

> **Binding scope (R36).** The async storage trait, its public
> boxed-handle alias, and the per-context constructor signatures
> bind as the crate's internal persistence interface. The
> cache-cell type and any other realisation detail shown are
> informative.

## 24.5 Native Backend Schema

The native backend owns three tables, created idempotently on
first connect. This section states the schema's *roles and
relationships*; the concrete table names, column names, and
column types are the dictated on-disk format and are given in
§24.6.1 (R29/R31).

| Table                    | Role                                                |
|--------------------------|-----------------------------------------------------|
| Accounts                 | One row per account (composite primary key + metadata) |
| Account-coin assignments | (account-key, ticker) pairs, cascade-deleted        |
| Enabled-account marker   | Single row identifying the active account           |

The accounts table's composite primary key is `(account_type,
account_idx, device_pubkey)`, so the three identity variants
of §24.2 coexist without collision: Iguana uses a sentinel
value in `account_idx` and `device_pubkey`; HD uses a real
`account_idx` and a sentinel `device_pubkey`; HW uses a
sentinel `account_idx` and a real `device_pubkey`.

The account-coin table and the enabled-account table each
carry foreign keys back to the composite primary key with
`ON DELETE CASCADE`, so deleting an account also removes its
activated tickers and the enabled marker if it was active.

R6. **Composite-key invariant.** The three-column composite
    primary key shall accommodate any future identity variant
    by appropriate use of sentinels for the columns that
    variant does not populate. Schema-level changes shall be
    additive; column removals are not permitted.

R7. **Cascade-on-delete.** Deletion of an account row shall
    cascade to its activated-coin rows and to the enabled-
    account marker if applicable. The cascade is enforced at
    the schema level, not in the handler.

R8. **No schema migrations in the chapter-bound substrate.** The
    schema is whatever the initialiser creates on first
    connect. Any future shape change requires adding a
    migrations layer; this is a deferred item (§24.10 D3).

## 24.6 Native Backend — On-Disk Schema and Realisation Contract

> **Binding scope (R36).** The table names, column names, and
> column SQL types in §24.6.1 are the **on-disk storage schema**
> and bind as externally-dictated interop (R29/R31): any reader
> or writer of an existing database — a later application
> version, a migration, or an out-of-process inspector — depends
> on them, so differing bytes break interoperability. The Rust
> item names, struct layout, private-helper decomposition,
> builder call chains, local-variable names, statement ordering,
> and diagnostic/log wording used to realise the schema are NOT
> part of the contract; they are informative, and a conformant
> backend MAY use different internal naming and decomposition.

The native backend realises the three regions of §24.5 over the
chapter-25-bound typed SQL-builder substrate
(`db_common::sql_build`). It MUST create the schema idempotently
(create-if-not-exists) on first connect and MUST NOT carry a
migrations path in the chapter-bound substrate (§24.10 D3).

### 24.6.1 On-disk schema (R29/R31, dictated)

The on-disk format is three tables. Their names, column names,
and column types are the storage contract; a conformant backend
MUST emit exactly these:

| Table                 | Column          | SQL type       | Null     |
|-----------------------|-----------------|----------------|----------|
| `gui_account`         | `account_type`  | `INTEGER`      | not null |
| `gui_account`         | `account_idx`   | `INTEGER`      | not null |
| `gui_account`         | `device_pubkey` | `VARCHAR(20)`  | not null |
| `gui_account`         | `name`          | `VARCHAR(255)` | not null |
| `gui_account`         | `description`   | `VARCHAR(600)` | nullable |
| `gui_account`         | `balance_usd`   | `VARCHAR(255)` | not null |
| `gui_account_coins`   | `account_type`  | `INTEGER`      | not null |
| `gui_account_coins`   | `account_idx`   | `INTEGER`      | not null |
| `gui_account_coins`   | `device_pubkey` | `VARCHAR(20)`  | not null |
| `gui_account_coins`   | `coin`          | `VARCHAR(255)` | not null |
| `gui_account_enabled` | `account_type`  | `INTEGER`      | not null |
| `gui_account_enabled` | `account_idx`   | `INTEGER`      | not null |
| `gui_account_enabled` | `device_pubkey` | `VARCHAR(20)`  | not null |

Schema constraints (also dictated by the on-disk format):

- `gui_account` carries a composite **primary key** over
  (`account_type`, `account_idx`, `device_pubkey`).
- `gui_account_coins` carries a composite **foreign key** over
  the same three identity columns referencing `gui_account`,
  configured `ON DELETE CASCADE`, plus a **unique** constraint
  over (`account_type`, `account_idx`, `device_pubkey`, `coin`).
- `gui_account_enabled` carries the same `ON DELETE CASCADE`
  foreign key over the three identity columns and holds at most
  one row.

**R-N1.** **Schema names and types are the contract.** A backend
MUST emit exactly the table names, column names, column types,
and constraints above; they are the on-disk format. The
`VARCHAR` widths (`device_pubkey` = 20, `name` = 255,
`description` = 600, `balance_usd` = 255, `coin` = 255) are part
of the schema. The crate's public length caps
(`MAX_ACCOUNT_NAME_LENGTH` = 255,
`MAX_ACCOUNT_DESCRIPTION_LENGTH` = 600, `MAX_TICKER_LENGTH` =
255; §24.3 R3) coincide with the corresponding column widths.

**R-N2.** **Identity-to-row encoding (dictated).** Each identity
variant of §24.2 occupies one composite-primary-key row:

- `account_type` encodes the variant discriminator as an
  integer: `Iguana = 0`, `HD = 1`, `HW = 2` (§24.3 R6).
- `account_idx` holds the HD account index; the Iguana and HW
  rows use a sentinel.
- `device_pubkey` holds the hardware-device public key as
  lowercase hexadecimal without an `0x` prefix; the Iguana and
  HD rows use a sentinel. The enabled-account table does not
  carry a distinct device pubkey and uses the sentinel for its
  stored row.

This encoding is the on-disk representation; a reader MUST
interpret stored rows by it.

### 24.6.2 Realisation contract (behavioural)

> **Binding scope (R36).** This sub-section states observable
> behaviour only. The private helpers, row-mapper functions,
> builder call chains, local variables, transaction
> bookkeeping, and diagnostic wording a backend uses to achieve
> it are informative, not mandated.

**R-N3.** **Construction.** The backend is constructed per
`MmArc` from the central context's shared SQLite connection slot
(Chapter 31). If that slot is not initialised, construction MUST
fail with the crate's internal-error condition rather than
panic. The diagnostic message wording is not part of the
contract.

**R-N4.** **Schema bootstrap ordering.** `init` MUST create the
three tables within a single transaction and in foreign-key
dependency order — the parent `gui_account` table before the two
child tables that reference it — then commit. The ordering is
observable because the child tables' foreign keys require the
parent table to exist first.

**R-N5.** **Mutation atomicity.** Any operation that changes more
than one row (schema bootstrap, switching the enabled account,
activating a batch of tickers) MUST be transactional, so a
partial failure leaves no partially-applied state.

**R-N6.** **Enabled-account replacement.** Switching the enabled
account MUST clear the at-most-one enabled-marker row and record
the new identity atomically. Recording an identity that does not
correspond to an existing account MUST surface the
no-such-account condition (enforced through the foreign key).

**R-N7.** **Ticker activation semantics.** Activating tickers
MUST be idempotent: re-activating an already-active ticker for an
account is a no-op, not an error. Activating tickers for an
account that does not exist MUST surface the no-such-account
condition. Deactivating tickers MUST remove the named
(account, ticker) rows; deactivating against a non-existent
account MUST surface the no-such-account condition, while
deactivating a ticker set that does not intersect the stored set
of an existing account is a no-op success.

**R-N8.** **Single-account access.** The backend exposes no
single-account loader on the trait surface (§24.4). Loading the
coin set for one account MUST return its activated-ticker set,
and MUST surface the no-such-account condition when that set is
empty *and* the account does not exist.

**R-N9.** **Enabled-flag enumeration.** Enumerating all accounts
with the enabled flag MUST mark exactly the account whose
identity equals the stored enabled identity. If a stored enabled
identity matches no account row, that is a storage-invariant
violation and MUST surface as an internal-error condition, not
silently.

**R-N10.** **Error classification.** At the trait boundary the
backend MUST distinguish the domain conditions
(account-exists-already, no-such-account, no-enabled-account)
from generic storage failures, and MUST classify serialisation
versus deserialisation failures into the crate's corresponding
storage-error categories (§24.7.1 R-R2). The classification is
behavioural; the message wording is not part of the contract.

**R-N11.** **Encapsulation.** The backend's only surface above
the async storage trait is its per-context constructor and the
trait implementation. Every other item in the module is private.

## 24.7 JSON-RPC Surface

The crate's public surface is **eleven** typed JSON-RPC
handlers. Each carries its own typed request and response
struct and its own validation error variants.

| Handler                     | Effect                                       |
|-----------------------------|----------------------------------------------|
| Add account                 | Insert a new account record                  |
| Delete account              | Remove an account (cascades coins + enabled) |
| Get accounts                | Enumerate all accounts                       |
| Get account coins           | List activated tickers for one account       |
| Get enabled account         | Return the active account, if any            |
| Enable account              | Mark account active (Iguana or HD only)      |
| Set account name            | Update display name                          |
| Set account description     | Update description                           |
| Set account balance         | Update fiat balance figure                   |
| Activate coins              | Append tickers to an account                 |
| Deactivate coins            | Remove tickers from an account               |

R9. **Public namespace.** When wired into the public RPC
    dispatcher (D1) these eleven handlers shall live under
    the `gui_storage::` namespace, with method names matching
    the table above (`gui_storage::add_account`,
    `gui_storage::delete_account`, ..., `gui_storage::
    deactivate_coins`).

R10. **Standard error mapping.** Each handler returns a typed
     error that implements the project's standard
     `MmError` and `HttpStatusCode` conventions. The error
     surface is owned by the handler module; the storage trait
     contributes its storage-error enum as one of the
     constituent variants.

R11. **Enabled-account validation.** The `enable_account`
     handler shall enforce R2 (no hardware-wallet variant can
     be enabled) at the handler boundary and return a typed
     validation error if the caller passes a hardware-wallet
     account identity.

### 24.7.1 Bound Error Type

**R-R1.** The handler module MUST expose an error enum with
exactly the following variant set and payload shapes. The
variant names double as the wire `error_type` tokens (dictated
by the typed-error envelope), and the payload fields are the
`error_data` shapes:

```rust
#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum AccountRpcError {
    NameTooLong { max_len: usize },
    DescriptionTooLong { max_len: usize },
    TickerTooLong { max_len: usize },
    NoSuchAccount(AccountId),
    NoEnabledAccount,
    AccountExistsAlready(AccountId),
    ErrorLoadingAccount(String),
    ErrorSavingAccount(String),
    Internal(String),
}
```

> **Binding scope (R36).** The serde `error_type`/`error_data`
> envelope, the variant names (which are the wire `error_type`
> tokens), and the payload field shapes bind as interface. The
> human-readable `Display` message wording each variant carries
> is a diagnostic string, NOT part of the contract; it is
> informative and a conformant implementation MAY word it
> differently.

The serde tag/content shape is the project's standard
typed-error envelope per `AGENTS.md`.

**R-R2.** **Storage-to-RPC error mapping.** Every failure of the
async storage trait MUST be converted into a public
`AccountRpcError` wire variant according to the failing
*condition*, preserving any account-identity or diagnostic
payload the source failure carried:

- a no-such-account condition maps to `NoSuchAccount`, carrying
  the offending account identity;
- a no-enabled-account condition maps to `NoEnabledAccount`;
- an account-already-exists condition maps to
  `AccountExistsAlready`, carrying the offending identity;
- any load-side failure, including deserialisation failures,
  maps to `ErrorLoadingAccount`, carrying its diagnostic string;
- any save-side failure, including serialisation failures, maps
  to `ErrorSavingAccount`, carrying its diagnostic string;
- any other generic or internal failure maps to `Internal`,
  carrying its diagnostic string.

**R-R3.** A `HttpStatusCode for AccountRpcError` impl MUST
return:

| Variant set | Status |
| --- | --- |
| `NameTooLong`, `DescriptionTooLong`, `TickerTooLong`, `NoSuchAccount`, `NoEnabledAccount`, `AccountExistsAlready` | `BAD_REQUEST` |
| `ErrorLoadingAccount`, `ErrorSavingAccount`, `Internal` | `INTERNAL_SERVER_ERROR` |

### 24.7.2 Bound Request/Response Types

**R-R4.** The following request and response types MUST exist.
Their public names, generic parameters, serde attributes
(`flatten`, `default`, `tag = "policy"`, `rename_all`), and
field shapes are the JSON-RPC wire contract:

```rust
#[derive(Deserialize)]
pub struct NewAccount<Id> {
    account_id: Id,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    balance_usd: BigDecimal,
}

impl<Id> From<NewAccount<Id>> for AccountInfo
where AccountId: From<Id> { /* projection */ }

#[derive(Deserialize)]
pub struct EnableAccountRequest { #[serde(flatten)] policy: EnableAccountPolicy }

#[derive(Deserialize)]
#[serde(tag = "policy")]
#[serde(rename_all = "snake_case")]
pub enum EnableAccountPolicy {
    Existing(EnabledAccountId),
    New(NewAccount<EnabledAccountId>),
}

#[derive(Deserialize)]
pub struct AddAccountRequest { #[serde(flatten)] account: NewAccount<AccountId> }

#[derive(Deserialize)]
pub struct DeleteAccountRequest { account_id: AccountId }

#[derive(Deserialize)]
pub struct SetAccountNameRequest { account_id: AccountId, name: String }

#[derive(Deserialize)]
pub struct SetAccountDescriptionRequest { account_id: AccountId, description: String }

#[derive(Deserialize)]
pub struct SetBalanceRequest { account_id: AccountId, balance_usd: BigDecimal }

#[derive(Deserialize)]
pub struct CoinRequest { account_id: AccountId, tickers: Vec<String> }

#[derive(Deserialize)]
pub struct GetAccountsRequest;

#[derive(Deserialize)]
pub struct GetAccountCoinsRequest { account_id: AccountId }

#[derive(Serialize)]
pub struct GetAccountCoinsResponse { account_id: AccountId, coins: BTreeSet<String> }

#[derive(Deserialize)]
pub struct GetEnabledAccountRequest;
```

`SuccessResponse` is the project's standard empty success
response type imported from `common`.

> **Binding scope (R36).** The type names, serde attributes, and
> field shapes above are the request/response wire contract and
> bind as interface. The `From` projection comment, private
> field naming, and any helper decomposition shown are
> informative.

### 24.7.3 Bound Handler Signatures

**R-R5.** The eleven handlers MUST have exactly these
signatures:

```rust
pub async fn enable_account(ctx: MmArc, req: EnableAccountRequest) -> MmResult<SuccessResponse, AccountRpcError>;
pub async fn add_account(ctx: MmArc, req: AddAccountRequest) -> MmResult<SuccessResponse, AccountRpcError>;
pub async fn delete_account(ctx: MmArc, req: DeleteAccountRequest) -> MmResult<SuccessResponse, AccountRpcError>;
pub async fn get_accounts(ctx: MmArc, _req: GetAccountsRequest) -> MmResult<Vec<AccountWithEnabledFlag>, AccountRpcError>;
pub async fn get_account_coins(ctx: MmArc, req: GetAccountCoinsRequest) -> MmResult<GetAccountCoinsResponse, AccountRpcError>;
pub async fn get_enabled_account(ctx: MmArc, _req: GetEnabledAccountRequest) -> MmResult<AccountWithCoins, AccountRpcError>;
pub async fn set_account_name(ctx: MmArc, req: SetAccountNameRequest) -> MmResult<SuccessResponse, AccountRpcError>;
pub async fn set_account_description(ctx: MmArc, req: SetAccountDescriptionRequest) -> MmResult<SuccessResponse, AccountRpcError>;
pub async fn set_account_balance(ctx: MmArc, req: SetBalanceRequest) -> MmResult<SuccessResponse, AccountRpcError>;
pub async fn activate_coins(ctx: MmArc, req: CoinRequest) -> MmResult<SuccessResponse, AccountRpcError>;
pub async fn deactivate_coins(ctx: MmArc, req: CoinRequest) -> MmResult<SuccessResponse, AccountRpcError>;
```

> **Binding scope (R36).** The handler names, parameter types,
> and result types above are the public surface and bind as
> interface. The parameter binding names and any body shown
> elsewhere are informative.

**R-R6.** **Handler lifecycle (behavioural).** Every handler
MUST observe the following ordering:

1. **Validate at the boundary first.** A handler whose request
   carries a display name, a description, or a ticker list MUST
   validate it per R-R7 and reject before any storage access.
   Balance updates carry an already-typed decimal and skip this
   step; the read handlers and the delete handler validate no
   further than their request struct.
2. **Obtain the per-context storage handle** (Chapter 31).
3. **Invoke the corresponding storage-trait operation.**
4. **Return** the project's standard empty success response,
   or — for the four read handlers — the loaded value
   (accounts enumerated in `AccountId` order; coin sets
   returned as ordered sets).

Storage-init and storage-call failures are converted into
`AccountRpcError` via the §24.7.1 R-R2 mapping. The
statement-level decomposition that achieves this lifecycle is
informative (R36).

**R-R7.** **Boundary validation (behavioural).** Before any
storage interaction a mutating handler MUST enforce the length
caps and return the matching typed error:

- a display name longer than `MAX_ACCOUNT_NAME_LENGTH` (255)
  yields `NameTooLong`;
- a description longer than `MAX_ACCOUNT_DESCRIPTION_LENGTH`
  (600) yields `DescriptionTooLong`;
- any ticker longer than `MAX_TICKER_LENGTH` (255) yields
  `TickerTooLong`, reported on the first offending ticker.

Each too-long error's `max_len` field reports the limit that
was checked. How the validation is decomposed into helper
functions is informative (R36).

**R-R8.** **Enable-account policy (behavioural).** The
enable-account request carries a policy union with two cases:
enable an already-stored account identified by id, or create a
new account from an inline record and then enable it. Both
cases converge on the same enable operation. The hardware-
wallet restriction (§24.2 R2) is enforced at the type level:
the policy carries an `EnabledAccountId`, whose variant set
excludes the hardware-wallet identity, so a hardware-wallet
identity is rejected at deserialisation before the handler
runs.

## 24.8 Browser Backend Stub

R12. **Explicit not-implemented stub on the browser target.**
     On the browser build target every trait method shall
     return an explicit "not implemented" error variant rather
     than silently succeed, panic, or no-op. The stub exists
     so the crate compiles cleanly for the browser target and
     so any caller hitting it gets a clear, typed error.

The browser backend is the natural location for a future
IndexedDB port (D2); the port shall mirror the three-region
shape of the native backend.

## 24.9 Tests

Unit tests are colocated with the storage module. The unit-
chapter-bound test set covers the native backend
end-to-end against an in-memory database built through a
test-helper context constructor:

- Account lifecycle: upload, enable, load, delete.
- Metadata updates: name, description, balance.
- Coin activation and deactivation, including idempotence.
- Cascade-delete behaviour: deleting an account removes its
  activated-coin rows and clears the enabled-account marker
  if applicable.

The browser-target stub has no functional tests at the time
of writing (every call is expected to error). The RPC
handlers do not have integration tests in the chapter-bound substrate
because they are not reachable through the dispatcher (D1).

## 24.9A Required Port — `gui_storage::` Dispatcher Surface (driving-spec)

**STATUS.** The capability in this section is **required but NOT
yet implemented in reloaded; the `mm2_gui_storage` crate (backend,
trait, type surface, and the eleven typed handlers of §24.7) is
present** as a library. The crate is not yet a dependency of the
application crate, and its handlers are not yet wired into the
public dispatcher. Per the PORT decision the registration is a
binding requirement, not optional deferred work.

**RP1.** The application crate MUST take `mm2_gui_storage` as a
dependency and register the eleven handlers of §24.7 under the
`gui_storage::` JSON-RPC v2 namespace. The method strings are the
wire contract and MUST be exactly:

| Method                          | Handler (§24.7.3)            | Request type (§24.7.2)            | Success result (§24.7.3)        |
|---------------------------------|-----------------------------|-----------------------------------|---------------------------------|
| `gui_storage::enable_account`   | `enable_account`            | `EnableAccountRequest`            | empty success                   |
| `gui_storage::add_account`      | `add_account`               | `AddAccountRequest`               | empty success                   |
| `gui_storage::delete_account`   | `delete_account`            | `DeleteAccountRequest`            | empty success                   |
| `gui_storage::get_accounts`     | `get_accounts`              | `GetAccountsRequest`              | `Vec<AccountWithEnabledFlag>`   |
| `gui_storage::get_account_coins`| `get_account_coins`         | `GetAccountCoinsRequest`          | `GetAccountCoinsResponse`       |
| `gui_storage::get_enabled_account` | `get_enabled_account`    | `GetEnabledAccountRequest`        | `AccountWithCoins`              |
| `gui_storage::set_account_name` | `set_account_name`          | `SetAccountNameRequest`           | empty success                   |
| `gui_storage::set_account_description` | `set_account_description` | `SetAccountDescriptionRequest` | empty success                   |
| `gui_storage::set_account_balance` | `set_account_balance`    | `SetBalanceRequest`               | empty success                   |
| `gui_storage::activate_coins`   | `activate_coins`            | `CoinRequest`                     | empty success                   |
| `gui_storage::deactivate_coins` | `deactivate_coins`          | `CoinRequest`                     | empty success                   |

**RP2.** The dispatcher MUST route the `gui_storage::`-prefixed
method (with the prefix stripped) to the matching handler. Each
handler's request/response wire shapes are the §24.7.2 contract
and its error envelope is the §24.7.1 `AccountRpcError` contract;
both bind unchanged by the registration.

**RP3.** Registration does not alter the persistence semantics of
§24.4–§24.6 or the validation discipline of §24.7. The native
backend serves the surface on native targets; on the browser
target the §24.8 stub returns the explicit not-implemented error
for every method until the §24.10 D2 IndexedDB backend lands.

**RP4 — acceptance criteria.**

- AC1. All eleven `gui_storage::*` methods are reachable through
  the public dispatcher and exercise the §24.7 handlers.
- AC2. A round trip — `add_account` → `get_accounts` →
  `activate_coins` → `get_account_coins` → `enable_account` →
  `get_enabled_account` → `delete_account` — succeeds on the
  native target with the §24.7.2 wire shapes and the §24.6
  persistence/cascade semantics.
- AC3. Validation failures (over-long name/description/ticker,
  no-such-account, enabling a hardware-wallet identity) surface as
  the matching `AccountRpcError` wire token with the §24.7.1 R-R3
  HTTP status.

## 24.10 Binding Requirements and Deferred Work

R1-R12 above are binding.

The following are **deferred work** named explicitly in scope
of this chapter:

D1. **[REQUIRED PORT — §24.9A]** Public dispatcher
    registration. The eleven handlers of §24.7 shall be
    registered in the public RPC dispatcher under the
    `gui_storage::` namespace per R9, and `mm2_gui_storage`
    shall become a dependency of the application crate. In
    reloaded the handler module exists but is not registered;
    landing this is a binding requirement, not optional, and
    is the single biggest blocker to consumer adoption.

D2. **Browser-target persistence.** The browser-target stub
    of §24.8 shall be replaced by an IndexedDB-backed
    implementation that mirrors the three-region shape of
    the native backend (one object store per region;
    cascade-on-delete enforced in code at the trait
    boundary if not at the store level).

D3. **Schema migrations.** A migrations layer shall be added
    so that schema evolutions can land without breaking
    existing deployed databases. In the chapter-bound substrate the
    initialiser is the only schema-creation path.

D4. **Account-record versioning.** A schema-version field on
    the accounts table shall be added so future record-
    shape changes can be staged behind a per-row version
    discriminator without requiring a full data migration.

D5. **Hardware-wallet active-account model.** The R2
    enabled-restriction is a deliberate scope limit. A
    future revision shall either relax the enabled-account
    set to include hardware-wallet accounts (with the
    appropriate confirmation flow) or introduce a parallel
    "active hardware device" concept; this chapter does not
    bind which.

D6. **Bulk import / export.** A GUI-facing backup flow
    requires the ability to export all accounts and their
    activated-coin sets in one operation and import them in
    one operation. Not present in the chapter-bound substrate.

## 24.11 External References

- BIP-44 (the public derivation-path standard that backs the
  HD account index used by the HD identity variant of §24.2).
- The codebase's standard error and HTTP-status mapping
  conventions referenced by R10.
- The codebase's per-context handle pattern (lazy-init
  under the central-context substrate) referenced by the
  lazy-init rule of §24.1.
- The codebase's cross-platform persistence approach
  ([Chapter 26](26-cross-platform-and-wasm.md)) referenced by
  the browser-target stub of §24.8.
- The native SQL abstractions
  ([Chapter 25](25-sql-query-builder.md)) over which the
  native backend is built.

## 24.12 Baseline Verifications

The following are verifiable from the baseline state defined
in [Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree contains **no** GUI account-state
    persistence crate. A directory listing of the baseline
    tree (`git ls-tree c1d46c0c1592faa0860f704008b2b2381bc3840f`)
    contains no `mm2_gui_storage` entry; a tree-wide
    `git grep -l 'gui_storage'` against the baseline returns
    no matches, confirming the absence of both the crate and
    its public `gui_storage::` RPC surface. To confirm the
    subsystem in the reloaded tree, verify instead that the
    crate exposes the eleven public `gui_storage::` methods of
    §24.7 and persists to the dictated on-disk tables of
    §24.6.1 (`gui_account`, `gui_account_coins`,
    `gui_account_enabled`).

V2. The baseline public RPC dispatcher carries no
    `gui_storage::` namespace. The library-only posture of
    the chapter-bound crate (D1 deferred) is
    consistent with the baseline's complete absence of this
    surface.

V3. The three-variant identity enum of §24.2 corresponds to
    the three wallet identity styles the project already
    supports elsewhere: legacy single-key (Iguana), BIP-44
    HD account indexing, and hardware-wallet device-keyed
    identity. These are not new identity styles invented by
    this chapter.

## 24.13 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`; absence of the
  GUI account-state crate at baseline verified via
  `git ls-tree c1d46c0c1592faa0860f704008b2b2381bc3840f`
  and tree-wide `git grep` for the trait, context, and
  namespace identifiers against the baseline; BIP-44 (the
  public derivation-path standard that backs the HD identity
  variant of §24.2); the project's standard error and HTTP-
  status mapping conventions; chapter 31 (the central
  application-context substrate the `account_ctx` sub-context
  slot is registered on per chapter 31 R7 / R8); the project's
  per-context handle pattern and cross-platform persistence
  approach (Chapters 8 and 26); the native SQL abstractions of
  Chapter 25.
- *Permitted-input classes used:* baseline source; external public
  specifications (BIP-44; the SQLite SQL grammar and the public
  `db_common::sql_build` / `rusqlite` builder APIs; the public
  `gui_storage::` JSON-RPC method strings); cross-chapter
  contracts (Chapters 8, 25, 26, 31); Interop / on-disk-schema-
  bound reuse (R29/R31) for the dictated storage schema embedded
  in §24.6.1 — the three table names (`gui_account`,
  `gui_account_coins`, `gui_account_enabled`), their column names
  and `VARCHAR`/`INTEGER` types, the composite-primary-key /
  foreign-key-cascade / unique-constraint layout, and the
  identity-to-row encoding (integer discriminator values and
  lowercase-hex device-pubkey form) — whose authoritative source
  is the on-disk format read by any other version of the
  application, not the historical lineage's discretionary
  expression.
- *Sibling-allowlist consultations:* Chapter 25 (the typed
  SQL-builder substrate the native backend consumes); Chapter 31
  (the central-context handle the per-context storage is
  registered on); Chapter 26 (the cross-platform persistence
  approach the browser stub defers to).
- *Forbidden corpus:* not consulted for clean-room derivation. No
  discretionary expression — no function bodies, private
  identifiers, helper decomposition, control-flow transcription,
  per-method tables keyed to internal names, or diagnostic /
  `Display` / log string literals — from the historical lineage
  crosses into this chapter. The single fragment whose
  authoritative source includes the historical record is the
  dictated on-disk storage schema of §24.6.1; it is embedded as
  Interop / wire-format reuse under R29/R31 (only the bytes
  required for interoperability — table names, column names,
  column types, and constraint layout — are reproduced; no
  upstream comments, private Rust identifiers, builder call
  chains, or helper decomposition accompany it). Any residual
  similarity of a conformant realisation to that lineage is
  governed by the R35 gate and the R36 binding-scope notes that
  head every code-bearing section of this chapter.
