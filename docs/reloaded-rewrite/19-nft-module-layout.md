# Chapter 19 -- NFT Module Layout

**Status:** driving-spec

> **One-sentence claim:** the project provides an in-tree NFT
> subsystem that covers five EVM chains, exposes eight JSON-RPC
> methods (activation, inventory, metadata, transfer history,
> withdrawal, wipe), embeds no third-party indexer hostnames,
> and abstracts both its outbound HTTP surface and its on-device
> storage behind narrow traits with native and browser
> implementations.

## 19.0 Executive Summary

An NFT subsystem provides on-device caching of ERC-721 and
ERC-1155 token inventories, transfer histories, and per-token
metadata across five EVM-compatible chain ecosystems
(Ethereum, BNB Smart Chain, Polygon, Avalanche, Fantom). The
subsystem is bounded by three architectural principles:

1. **No hardcoded indexer endpoints.** Every HTTP base URL used
   to crawl chain history or refresh per-token metadata is
   supplied by the caller at RPC time. The subsystem embeds no
   third-party indexer hostnames, vendor names, or default
   provider URLs of any kind.
2. **Trait-abstracted persistence with two independent
   implementations.** Two narrow async traits cover the
   inventory and history surfaces; each is implemented twice
   (once for the native SQL backend, once for the browser
   IndexedDB backend) with no shared concrete type.
3. **Pluggable provider traits.** Two narrow async traits
   isolate the crawling and metadata-refresh logic from any
   specific HTTP wire shape, so additional providers (self-
   hosted indexer, signed-proxy indexer, mock test indexer)
   can be wired in without touching either the RPC handlers or
   the persistence layer.

The subsystem at the time of writing supports keypair-derived
withdrawal signing only; HD-wallet and hardware-wallet
withdrawal, browser-side incremental crawling, and live
confirmation counts are explicitly named as deferred work in
§19.9.

## 19.1 Subsystem Shape

The subsystem is grouped into the following functional regions:

| Region                  | Responsibility                                  |
|-------------------------|-------------------------------------------------|
| Public surface          | Per-context handle, re-exports                  |
| Errors                  | Per-operation error enums                       |
| Models                  | Chain enum, NFT object, transfer record,        |
|                         | metadata, RPC request payloads, withdraw payload|
| Storage trait surface   | Inventory trait, history trait, error trait     |
| Storage -- native       | SQL backend, per-chain table pair               |
| Storage -- browser      | IndexedDB backend, per-chain object-store pair  |
| Providers -- crawl      | Trait + HTTP implementation                     |
| Providers -- metadata   | Trait + HTTP implementation                     |
| Providers -- spam       | Caller-supplied domain-list filtering           |
| RPC handlers            | Eight JSON-RPC entry points (native; selected   |
|                         | are stubbed on the browser target)              |
| Withdraw                | ERC-721 / ERC-1155 calldata encoding & signing  |

The subsystem must not contain a separate sibling persistence
crate; both backend implementations live as submodules of the
storage region within the subsystem itself. This is a binding
layout rule.

## 19.2 Public Handle

The subsystem exposes a single handle type obtained via the
per-context lazy-init pattern of the codebase's
central-context substrate. Call sites
acquire the handle from the central context; they do not
construct it directly. The handle is the only path through
which the RPC handlers, the withdrawal flow, and any future
external consumer reach the persistence layer.

The browser target compiles out the RPC handlers and the
withdrawal flow at the module level via target-architecture
guards; the rest of the public surface is identical across
targets.

## 19.3 Chains and Tickers

The chain set is modelled as a closed enum with five variants,
serialised as upper-case strings:

| Variant value (serialised) | Platform-coin ticker | Chain ecosystem            |
|----------------------------|----------------------|-----------------------------|
| `AVALANCHE`                | `AVAX`               | Avalanche C-Chain           |
| `BSC`                      | `BNB`                | BNB Smart Chain             |
| `ETH`                      | `ETH`                | Ethereum mainnet            |
| `FANTOM`                   | `FTM`                | Fantom Opera                |
| `POLYGON`                  | `MATIC`              | Polygon PoS                 |

A small chain-ticker trait maps each variant to:

- The platform-coin ticker used elsewhere in the codebase.
- A table-name prefix used by the native backend
  (`NFT_AVAX_`, `NFT_BNB_`, `NFT_ETH_`, `NFT_FTM_`, `NFT_MATIC_`).

Per-chain prefixes are a deliberate choice: a chain-scoped wipe
is two `DROP TABLE` statements and one row delete from the
chain-progress table.

Adding a new EVM chain is a closed additive operation:
appending an enum variant and a chain-ticker mapping is
sufficient; everything downstream (RPC dispatch, withdrawal
path, storage table layout) flows from the enum.

## 19.4 Storage Layer

### 19.4.1 Trait Surface

Two object-safe async traits abstract the backend:

- An **inventory** trait covering the set of tokens currently
  owned by the active address per chain. Its surface includes
  per-chain ready-state ensure/check, bulk register of newly
  observed tokens, paginated listing with optional filters,
  per-token fetch, per-token drop, per-chain purge, full purge,
  balance bookkeeping, last-scanned-block bookkeeping,
  per-contract enumeration, contract-level spam marking,
  external-domain enumeration, and domain-level phishing
  marking.
- A **history** trait covering every observed transfer touching
  the active address per chain. Its surface includes bulk
  append, paginated listing with filters, last-transfer-block
  lookup, since-block filtering, per-token transfer listing,
  per-log lookup, metadata-attachment bulk update,
  missing-metadata enumeration, per-contract transfer
  enumeration, contract-level spam marking, contract-address
  enumeration, domain enumeration, domain-level phishing
  marking, per-chain purge, and full purge.

Both traits carry an associated error type bounded by a shared
storage-error trait so that calling code can be backend-agnostic.

Inventory and history are deliberately on independent code
paths: a write to one cannot corrupt the other, and a clear of
one does not affect the other. The split is a binding rule.

### 19.4.2 Native Backend Schema

Per chain, the native backend maintains two tables. Schema
illustrated for the Ethereum variant; the other four chains
have identical column layouts under the chain-specific prefix.

```sql
CREATE TABLE IF NOT EXISTS NFT_ETH_inventory (
    token_address     TEXT    NOT NULL,
    token_id_str      TEXT    NOT NULL,
    block_number      INTEGER NOT NULL,
    possible_spam     INTEGER NOT NULL DEFAULT 0,
    possible_phishing INTEGER NOT NULL DEFAULT 0,
    contract_type     TEXT    NOT NULL,
    image_domain      TEXT,
    animation_domain  TEXT,
    external_domain   TEXT,
    payload           TEXT    NOT NULL,
    PRIMARY KEY (token_address, token_id_str)
);

CREATE TABLE IF NOT EXISTS NFT_ETH_transfers (
    transaction_hash  TEXT    NOT NULL,
    log_index         INTEGER NOT NULL,
    token_id_str      TEXT    NOT NULL,
    token_address     TEXT    NOT NULL,
    block_number      INTEGER NOT NULL,
    block_timestamp   INTEGER NOT NULL,
    possible_spam     INTEGER NOT NULL DEFAULT 0,
    possible_phishing INTEGER NOT NULL DEFAULT 0,
    status            TEXT    NOT NULL,
    token_domain      TEXT,
    image_domain      TEXT,
    payload           TEXT    NOT NULL,
    PRIMARY KEY (transaction_hash, log_index, token_id_str)
);
```

A single global table tracks per-chain crawl progress:

```sql
CREATE TABLE IF NOT EXISTS nft_chain_progress (
    chain              TEXT PRIMARY KEY,
    last_scanned_block INTEGER NOT NULL DEFAULT 0
);
```

Schema rationale:

- The `payload` column stores the full token or transfer object
  as JSON. Scalar columns are extracted only as far as is
  needed to drive pagination, filtering, and spam masks. New
  optional fields in the model can be added without a schema
  migration.
- The `*_domain` columns store the parsed host of the relevant
  URL field, allowing domain-list lookups to run as a single
  SQL filter rather than a per-row check.
- Per-chain table prefixes make a chain-scoped wipe two
  `DROP TABLE` statements plus a row delete from the progress
  table.

### 19.4.3 Browser Backend Schema

The browser backend mirrors the native schema in IndexedDB
object stores: each chain receives its own pair of stores
(inventory and transfers) carrying the same fields, with
indexes that mirror the native backend's scalar columns so the
same pagination and filtering queries can be expressed.

The two backends do **not** share a base implementation: the
abstraction lives entirely above them through the async traits.

## 19.5 Provider Layer

### 19.5.1 Crawl Provider

A **crawl provider** trait abstracts the inventory and
transfer-history HTTP surface. Its three operations are:

- **Latest block** for a given chain.
- **Transfers** for a given owner since a given block on a
  given chain, returning a list of transfer records.
- **Token detail** for a given owner / contract / token-id
  triple on a given chain, returning a single inventory entry.

The bundled HTTP implementation is parameterised by a base URL
supplied at construction time and a boolean flag reserved for
signed-proxy operation (not yet wired). Its endpoint shapes are:

| Operation        | Path template                                       |
|------------------|-----------------------------------------------------|
| Latest block     | `GET {base}/<chain>/block/latest`                   |
| Transfers since  | `GET {base}/<chain>/<owner>/transfers?from_block=<n>` |
| Token detail     | `GET {base}/<chain>/<owner>/<contract>/<token_id>`  |

Latest-block responses are objects shaped `{ "block": <u64> }`.

### 19.5.2 Metadata Provider

A **metadata provider** trait abstracts the per-token
metadata-refresh HTTP surface. Its single operation refreshes
the metadata for a given chain / contract / token-id triple,
returning the freshly fetched inventory entry. The bundled HTTP
implementation issues
`GET {base}/<chain>/<contract>/<token_id>` and is again
parameterised by a base URL supplied at construction time plus
the same reserved signed-proxy flag.

The RPC handler that drives a metadata refresh merges the
returned object into persistence through the inventory trait's
bulk register operation plus an in-place merge of the URL
fields.

### 19.5.3 Spam and Phishing

The subsystem applies spam and phishing flags to inventory
entries by checking the token's image, animation, and external
domain fields against **caller-supplied** URL lists. The lists
themselves are passed at RPC time on a per-call basis; the
subsystem embeds none and ships none. The flags are stored as
scalar columns on each inventory row and are used purely for
client-side filtering and display masking.

## 19.6 RPC Wire Surface

The subsystem registers eight JSON-RPC methods in the public
dispatcher. Seven (§19.6.1 onward) are the operational methods;
the eighth (`enable_nft`, §19.6.2) is the activation entry
point. The native target supports all eight. Browser-target
availability is bound only for the operational methods shown below.

| Method                  | Native | Browser | Returns                  |
|-------------------------|--------|---------|--------------------------|
| `enable_nft`            | yes    | -       | Owned-NFT snapshot       |
| `get_nft_list`          | yes    | yes     | Paginated inventory list |
| `get_nft_metadata`      | yes    | yes     | Single inventory entry   |
| `get_nft_transfers`     | yes    | yes     | Paginated transfer list  |
| `refresh_nft_metadata`  | yes    | yes     | empty success            |
| `clear_nft_db`          | yes    | yes     | empty success            |
| `update_nft`            | yes    | stub    | empty success (crawl)    |
| `withdraw_nft`          | yes    | -       | Transaction details      |

Request payloads (operational methods):

- `get_nft_list` -- chains, max-flag, page size, page number,
  spam-protection flag, optional filters.
- `get_nft_metadata` -- chain, token address, token id,
  spam-protection flag.
- `get_nft_transfers` -- chains, filters, max-flag, page size,
  page number, spam-protection flag.
- `refresh_nft_metadata` -- chain, token address, token id,
  metadata-provider base URL, spam-list base URL, signed-proxy
  flag.
- `update_nft` -- chains, crawl-provider base URL, spam-list
  base URL, signed-proxy flag.
- `clear_nft_db` -- chain set plus a `clear_all` flag; when
  `clear_all` is true the chain set is ignored and the wipe
  spans all chains.
- `withdraw_nft` -- a tagged union over the two NFT standards:
  - ERC-721 variant carries chain, recipient, token address,
    token id, optional fee override.
  - ERC-1155 variant additionally carries an optional amount
    and a max-balance drain flag.

The withdraw request is the only payload whose shape varies per
token standard.

### 19.6.1 Operational vs Activation Methods

The seven methods above operate on an NFT subsystem that is
*already active* for a platform coin. They neither create nor
tear down activation; they read, refresh, wipe, or withdraw
against active NFT support. Of these, `update_nft` carries the
crawl-provider base URL and is the method that drives a full
re-crawl of inventory and transfer history for the requested
chains.

`enable_nft` (§19.6.2) is distinct: it is the **activation**
entry point that brings the NFT subsystem into existence for a
platform coin and performs the initial inventory fetch. It is
the method the Komodo DeFi SDK and SDK-derived GUIs invoke when
a user turns NFT support on for an EVM platform coin; its
absence surfaces to those clients as an NFT-activation runtime
failure.

### 19.6.2 Bound `enable_nft` Activation

`enable_nft` is a **dictated-interop** method: its wire name,
envelope, request field shape, and response field shape are
fixed by the Komodo DeFi SDK / GUI clients that call it, and the
subsystem must honour that contract verbatim for those clients
to activate NFT support.

**Envelope and availability.** `enable_nft` is an **mmrpc 2.0**
method (the structured request/response envelope with top-level
`mmrpc`, `method`, `params`, and `id` fields). This chapter binds
the native activation surface.

**Request shape.** The `params` object is the standard
token-activation envelope specialised for the NFT protocol:

| Field               | Type                       | Req? | Notes                                                       |
|---------------------|----------------------------|------|------------------------------------------------------------|
| `ticker`            | string                     | yes  | The configured NFT pseudo-coin ticker whose coin-config protocol entry is of NFT type bound to an EVM platform. |
| `protocol`          | object (coin-protocol)     | no   | Optional inline protocol descriptor for a custom (non-config) NFT entry; of NFT type carrying the platform-coin ticker. When omitted the protocol is resolved from the coin config keyed by `ticker`. |
| `activation_params` | object                     | yes  | NFT activation parameters (below).                          |

`activation_params` carries a single required member:

| Field      | Type                       | Req? | Notes                                                   |
|------------|----------------------------|------|---------------------------------------------------------|
| `provider` | object (tagged union)      | yes  | The indexer provider descriptor (below).                |

`provider` is a tagged union with an externally-tagged shape:
a `type` discriminant string selecting the provider variant and
an `info` object carrying that variant's configuration. The
single variant in scope carries:

| `info` field   | Type    | Req? | Default | Notes                                                                                   |
|----------------|---------|------|---------|-----------------------------------------------------------------------------------------|
| `url`          | string (URL) | yes | --    | Caller-supplied indexer base URL used for the initial inventory crawl. Consistent with R1: no default or embedded value -- the caller supplies it at RPC time. |
| `komodo_proxy` | boolean | no   | `false` | Signed-proxy flag (the same reserved per-provider signed-proxy flag described in §19.5 / D4). |

The exact `type` discriminant literal is a dictated wire constant
emitted by SDK/GUI clients. The method must accept that wire value
for compatibility while keeping the provider `url` caller-supplied.

There is **no** chain field in the request: the platform/ticker
in `ticker` (and its resolved NFT protocol) identifies the
single EVM chain whose NFT support is being activated.

**Response shape.** On success the method returns an object with
two members:

| Field          | Type                         | Notes                                                                 |
|----------------|------------------------------|-----------------------------------------------------------------------|
| `nfts`         | object (map)                 | A map keyed by per-token identifier string; each value is an owned-NFT entry (below). Reflects the inventory observed during the initial crawl. |
| `platform_coin`| string                       | The platform-coin ticker the NFT subsystem was activated under.        |

Each owned-NFT entry carries the public fields:

| Field           | Type           | Notes                                                            |
|-----------------|----------------|-----------------------------------------------------------------|
| `token_address` | string (address) | The NFT contract address.                                     |
| `token_id`      | string         | The token id, serialised as a decimal string.                   |
| `chain`         | string         | The chain discriminant (the §19.3 upper-case chain values).     |
| `contract_type` | string         | The token-standard discriminant (ERC-721 / ERC-1155).           |
| `amount`        | string (decimal) | Owned quantity; meaningful for ERC-1155 multi-supply tokens.  |

**Behavioural contract.**

1. The platform coin named by the resolved NFT protocol (one of
   the five EVM platform coins of §19.3) **must already be
   activated**. If it is not, activation fails with a
   platform-coin-not-activated outcome.
2. The NFT subsystem must not already be active for that ticker.
   A second `enable_nft` for an already-active NFT ticker fails
   with an already-activated outcome.
3. On success the method marks NFT support active for the resolved
   platform coin and performs an **initial inventory crawl** against
   the caller-supplied `url`, populating the owned-NFT snapshot
   returned in `nfts`.
4. The method is the activation counterpart to `update_nft`:
   `enable_nft` brings the subsystem into existence and performs
   the first inventory fetch; `update_nft` performs subsequent
   full re-crawls (inventory plus transfer history) against a
   caller-supplied crawl-provider URL once the subsystem is
   active. A client that has called `enable_nft` does not need a
   separate `update_nft` to obtain the initial inventory.

**Error conditions (functional).** The wire surface distinguishes
at least:

- Platform coin for the requested NFT not activated
  (client-input error).
- NFT already activated for the requested ticker
  (client-input error).
- NFT ticker has no coin-config entry, or the resolved protocol
  is not an NFT protocol (client-input / configuration error).
- The resolved platform coin is not an EVM platform coin
  (unsupported-platform error).
- Caller-supplied provider URL invalid or the initial crawl
  fails to reach the indexer (transport / invalid-payload
  error).
- The NFT protocol's declared platform does not match the
  resolved platform coin (configuration consistency error).

## 19.7 EVM Withdrawal Path

The withdrawal module embeds minimal copies of the two on-chain
interface definitions (ABI fragments) needed to encode calldata:

```jsonc
// ERC-721 transferFrom
[{"inputs":[{"name":"from","type":"address"},
            {"name":"to","type":"address"},
            {"name":"tokenId","type":"uint256"}],
  "name":"transferFrom","outputs":[],
  "stateMutability":"nonpayable","type":"function"}]

// ERC-1155 safeTransferFrom + balanceOf
[{"inputs":[{"name":"from","type":"address"},
            {"name":"to","type":"address"},
            {"name":"id","type":"uint256"},
            {"name":"value","type":"uint256"},
            {"name":"data","type":"bytes"}],
  "name":"safeTransferFrom","outputs":[],
  "stateMutability":"nonpayable","type":"function"},
 {"inputs":[{"name":"account","type":"address"},
            {"name":"id","type":"uint256"}],
  "name":"balanceOf","outputs":[{"name":"","type":"uint256"}],
  "stateMutability":"view","type":"function"}]
```

These fragments are the public on-chain interface definitions
of ERC-721 and ERC-1155; they are deployed-contract
identifiers, not authorial material.

The flow is:

1. Resolve the EVM platform coin handle for the requested
   chain's ticker. Anything that is not a plain EVM platform
   coin is rejected; the codebase does not implement EVM-style
   NFT withdrawal for TRON-family chains and returns an
   explicit error to that effect.
2. Encode the calldata using a generic Ethereum ABI encoder
   over the embedded fragment for the selected method.
3. For the ERC-1155 variant, if the max-balance drain flag is
   set, query `balanceOf(owner, id)` via an `eth_call` and use
   the returned balance as the transfer amount.
4. Resolve gas using either the caller's explicit fee override
   or an estimate against the encoded calldata.
5. Resolve nonce using the platform coin's nonce-resolution
   path.
6. Build and sign with the keypair-derived signing path
   appropriate to the platform coin's fee policy (legacy or
   EIP-1559).
7. Return transaction details with fee values denominated in
   the platform coin (`ETH`, `BNB`, `MATIC`, `AVAX`, `FTM`).

The withdrawal path supports keypair-derived signing only.
HD-wallet and hardware-wallet signing for NFT withdrawal are
deliberately deferred (§19.9).

## 19.8 Tests

Unit tests are colocated with each region. The unit-test set at
the time of writing covers:

- Native storage: per-chain ensure, bulk register, pagination,
  filtering, chain-scoped clear.
- Browser storage: smoke tests over the IndexedDB backend.
- Pagination helper: boundary conditions on the shared
  pagination utility.
- Withdraw payload: serde round-trip over the tagged union,
  including optional fee and amount shapes.
- Metadata model: in-place merge semantics for URL fields.
- HTTP error classification.

The activation entry point has its own acceptance coverage:

T1. **`enable_nft` wire shape.** A conformance test shall verify that
    the mmrpc-2.0 method name `enable_nft` accepts the §19.6.2 request
    fields (`ticker`, optional inline NFT `protocol`, and required
    `activation_params.provider`) and returns the §19.6.2 success fields
    (`platform_coin` and `nfts`) on a successful native activation.

T2. **`enable_nft` activation failures.** A conformance test shall
    verify the functional failure categories listed in §19.6.2 for
    missing backing platform activation, already-active NFT support,
    invalid NFT ticker/protocol, unsupported platform, provider failure,
    and protocol/platform mismatch.

End-to-end integration tests against a live indexer are not in
the test set at the time of writing; they are named as
follow-on work once a deterministic local test fixture for the
EVM activation surface is available.

## 19.9 Binding Requirements and Deferred Work

The following are **binding rules** for this subsystem:

R1. **No embedded indexer endpoints.** The subsystem shall
    embed no third-party indexer hostnames, vendor names, or
    default provider URLs of any kind. All HTTP base URLs used
    by the crawl and metadata providers are caller-supplied at
    RPC time.

R2. **No embedded domain lists.** The subsystem shall embed no
    spam-domain or phishing-domain lists. The lists are
    caller-supplied at RPC time and apply per call.

R3. **Trait-abstracted storage with two independent
    implementations.** The two persistence backends (native SQL
    and browser IndexedDB) shall not share a base
    implementation; the abstraction shall live entirely above
    them via the inventory and history traits.

R4. **Inventory / history independence.** The inventory and
    history paths shall remain on independent code paths and
    independent schemas so that a write to one cannot corrupt
    the other and a clear of one does not affect the other.

R5. **Closed chain enum.** The five-element chain enum is the
    single registration point for an EVM chain to gain NFT
    support; everything downstream (RPC dispatch, table
    prefixes, withdrawal path) shall flow from the enum.

R6. **Provider pluggability.** The crawl and metadata HTTP
    surfaces shall remain behind their respective traits so
    that alternative providers (signed-proxy, self-hosted,
    mock) can be substituted without changes to the RPC
    handlers or the persistence layer.

R7. **Standards-only ABI fragments.** The withdrawal path's
    embedded ABI fragments shall be the public ERC-721 and
    ERC-1155 on-chain interface definitions and nothing more.

R8. **First-class NFT activation entry point.** The subsystem shall
    expose `enable_nft` as the mmrpc-2.0 activation method for NFT
    support on the native target. The method shall accept the §19.6.2
    request shape (`ticker`, optional inline NFT `protocol`, required
    `activation_params.provider`), and shall
    return the §19.6.2 response shape (`nfts`, `platform_coin`). The
    provider base URL shall be caller-supplied at RPC time; the
    subsystem shall not embed a default indexer URL.

R9. **Activation vs refresh split.** `enable_nft` shall mark NFT
    support active for the requested NFT pseudo-coin ticker and, on
    the native target, perform the initial owned-inventory crawl.
    `update_nft` shall remain the refresh/re-crawl method for an
    already-active NFT subsystem and shall not be the activation
    substitute.

R10. **Activation preconditions and failures.** `enable_nft` shall
     require the resolved backing EVM platform coin to already be
     activated, shall reject an already-active NFT ticker, shall reject
     an invalid NFT ticker or non-NFT protocol, shall reject a
     non-EVM backing platform, and shall reject an inline NFT protocol
     whose declared platform disagrees with the platform resolved from
     the ticker. A failed precondition shall not mark the NFT ticker
     active and shall not persist a partial initial crawl result.

The following are **deferred work** named explicitly in scope
of this chapter:

D1. **Browser-side incremental crawl.** The crawl operation is
    currently stubbed on the browser target. The crawl logic
    itself is target-agnostic; the missing piece is a
    long-running task harness on the browser target that does
    not block the host event loop.

D2. **HD-wallet and hardware-wallet NFT withdrawal.** The
    withdrawal path supports keypair-derived signing only.
    Adding HD-wallet and hardware-wallet signing requires
    threading the derivation path and address index through
    the withdraw request payload and the signing flow.

D3. **Live confirmation count.** The inventory stores the
    block at which each token was last observed; surfacing a
    live confirmation count requires either periodic
    re-observation or a streaming subscription
    ([Chapter 10](10-sse-streaming.md)).

D4. **Signed-proxy provider operation.** Both bundled HTTP
    providers carry a reserved signed-proxy flag intended to
    sign outbound HTTP with the codebase's signed-proxy scheme
    (keyed off the P2P identity, see
    [Chapter 28](28-libp2p-modernization.md) for the keying);
    wiring is deferred to that subsystem's integration step.

D5. **End-to-end integration tests** against a deterministic
    indexer fixture (§19.8).

## 19.10 External References

- The ERC-721 standard (the on-chain interface used by the
  withdrawal path's `transferFrom`).
- The ERC-1155 standard (the on-chain interface used by the
  withdrawal path's `safeTransferFrom` and `balanceOf`).
- The Ethereum JSON-RPC `eth_call` method (used for the
  `balanceOf` query on the ERC-1155 max-balance drain path).
- The EIP-1559 transaction format (one of the two signing
  policies selected by the platform coin's fee policy).
- The EVM chain ecosystems named in §19.3 (Ethereum, BNB Smart
  Chain, Polygon, Avalanche, Fantom) and their respective
  platform-coin tickers.

## 19.11 Baseline Verifications

The following are verifiable from the baseline state defined in
[Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree contains **no** NFT subsystem of the
    shape described in this chapter. A tree-wide
    `git grep -l '^pub.*nft\|NftListStore\|NftHistoryStore\|NftCrawlProvider'`
    against the baseline returns no matches; a directory
    listing of the baseline tree
    (`git ls-tree -r c1d46c0c1592faa0860f704008b2b2381bc3840f`)
    contains no path containing `nft` as a directory component.

V2. The baseline tree contains **no** sibling NFT storage
    crate. The single-subsystem layout rule in §19.1 is
    consistent with the baseline state: no NFT material exists
    at baseline at all.

V3. The five chain variants in §19.3 correspond to the
    publicly-documented EVM ecosystems of the same names. The
    platform-coin tickers (`AVAX`, `BNB`, `ETH`, `FTM`,
    `MATIC`) are the publicly-deployed mainnet ticker symbols
    of the respective ecosystems.

V4. The ABI fragments embedded in §19.7 are byte-identical to
    the publicly-published ERC-721 and ERC-1155 on-chain
    interface definitions for the methods named. They are
    on-chain interface identifiers, not authorial material.

## 19.12 Provenance Footer

- *Inputs:* baseline commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`;
  public ERC-721 and ERC-1155 interface standards; Ethereum JSON-RPC
  `eth_call`; EIP-1559; publicly documented mainnet ticker symbols of
  the five EVM ecosystems named in §19.3; dictated public SDK/GUI
  interop facts for `enable_nft`.
- *Permitted-input classes used:* baseline source; public
  specification documents; public protocol documentation; public
  ticker-symbol documentation; dictated-interop wire facts.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* not consulted.
