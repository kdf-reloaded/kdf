# Chapter 19 -- NFT Module Layout

**Status:** driving-spec

> **One-sentence claim:** the project provides an in-tree NFT
> subsystem that covers five EVM chains, exposes eight JSON-RPC
> methods (activation, inventory, metadata, transfer history,
> withdrawal, wipe), embeds no third-party indexer hostname or
> default provider URL, speaks the deployed EVM NFT indexer wire
> contract behind a caller-selected provider profile, and
> abstracts both its outbound HTTP surface and its on-device
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
   third-party indexer hostname, no default or fallback provider
   base URL, and no vendor-operated endpoint of any kind.
   *Endpoint* is the bound term: a discriminant token that a
   caller sends inside its own request payload, and the path and
   query vocabulary a third-party API dictates, are interop
   surface rather than embedded endpoints and are not restricted
   by this principle (R1).
2. **Trait-abstracted persistence with two independent
   implementations.** Two narrow async traits cover the
   inventory and history surfaces; each is implemented twice
   (once for the native SQL backend, once for the browser
   IndexedDB backend) with no shared concrete type.
3. **Pluggable provider traits.** Two narrow async traits isolate
   the crawling and metadata-refresh logic from any specific HTTP
   wire shape. One wire profile is bundled behind those traits
   (§19.5.1): the **deployed-indexer profile**, whose request and
   response contract is dictated by the EVM NFT indexer API that
   KDF-family wallets and operators actually deploy behind the
   caller-supplied base URL. Further providers (signed-proxy,
   mock) can be wired in without touching either the RPC handlers
   or the persistence layer.

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

A **crawl provider** trait abstracts the owned-inventory and
transfer-history HTTP surface. Its operations are:

- **Owned inventory** for a given owner on a given chain,
  returning the full set of currently-owned inventory entries.
- **Transfers** for a given owner since a given block on a
  given chain, returning a list of transfer records.
- **Token detail** for a given owner / contract / token-id
  triple on a given chain, returning a single inventory entry,
  or absence when the provider has no record of the token.
- **Latest block** for a given chain, used only as a
  scan bookmark when a chain yields no transfers.

Every implementation is parameterised by a base URL supplied at
construction time and a boolean flag reserved for signed-proxy
operation (not yet wired). One wire profile is bundled.

The bundled profile does not serve every trait operation. It has
no latest-block operation, which degrades rather than bars: the
scan bookmark is left unadvanced when a chain yields no transfers.

#### The deployed-indexer profile (dictated interop)

Referred to below as **Profile A**; it is the only bundled profile.

This is the request/response contract of the EVM NFT indexer web
API that KDF-family wallets and node operators deploy behind the
caller-supplied base URL, whether as the vendor-hosted service or
as an operator-run compatible proxy in front of it. The contract
is fixed by that third party; the subsystem must produce and
consume it byte-for-byte to interoperate. Nothing about the
profile names or embeds a host: only the path and query
vocabulary appended to whatever base URL the caller supplied is
bound here.

| Operation         | Method and path template                      |
|-------------------|-----------------------------------------------|
| Owned inventory   | `GET {base}/api/v2/<owner>/nft`               |
| Transfers since   | `GET {base}/api/v2/<owner>/nft/transfers`     |
| Token detail      | `GET {base}/api/v2/nft/<contract>/<token_id>` |

Query parameters:

| Parameter    | Applies to                   | Value                                                        |
|--------------|------------------------------|--------------------------------------------------------------|
| `chain`      | all three operations         | The §19.3 upper-case chain discriminant.                     |
| `format`     | all three operations         | `decimal` -- selects decimal (not hexadecimal) token ids.    |
| `from_block` | transfers                    | Decimal block height; the lowest block to return. Absent-bookmark case sends `1`.     |
| `cursor`     | owned inventory, transfers   | Opaque page token echoed from the previous response.         |

Response contract:

- The two list operations return an object envelope carrying a
  `result` array of entries and a `cursor` string. Pagination is
  cursor-driven, not offset-driven: the caller re-issues the same
  request with `cursor` set to the previous response's value and
  stops when `cursor` is absent or null. A response whose
  `result` member is missing or is not an array terminates
  paging.
- The token-detail operation returns a single entry object
  directly, without the `result` / `cursor` envelope.
- Entry and transfer members carry the field names the
  subsystem's inventory and transfer models already use. Those
  names are model-level, not column-level: §19.4.2 persists most
  of them inside its `payload` column rather than promoting each
  to a column of its own. The encodings the profile dictates are:

| Member                                    | Wire encoding                                     |
|-------------------------------------------|---------------------------------------------------|
| `token_id`, `block_number`, `block_number_minted` | Decimal **string**, not a JSON number.    |
| `block_timestamp`                         | Timestamp **string**, not a Unix integer.         |
| `name`                                    | The collection name (the model's collection-name member). |
| `contract_type`                           | Optional. An entry lacking it is skipped rather than treated as an error. |

- A `404` on the token-detail operation means "no such token"
  (for example, burned) and is a normal absent result, not a
  transport failure.

Two shape notes follow from the third party's contract rather
than from this project's trait surface. Profile A's token-detail
endpoint is not owner-scoped, so the owner argument the crawl
trait carries is unused under this profile; for an ERC-1155 token
with several holders the endpoint therefore describes the token,
not the caller's holding, and the owned quantity must come from
the inventory or transfer path rather than from this response.
Profile A also has **no** latest-block operation: when a chain
yields no transfers, the scan bookmark is left at its previous
value rather than advanced from a provider-reported head.

#### No probing

The wire contract is fixed, never inferred from the base URL and
never probed at runtime. A wrong-path request against a live
indexer is indistinguishable from a legitimately empty result, so
the subsystem must never try one shape and retry with another.

### 19.5.2 Metadata Provider

A **metadata provider** trait abstracts the per-token
metadata-refresh HTTP surface. Its single operation refreshes
the metadata for a given chain / contract / token-id triple,
returning the freshly fetched inventory entry. Implementations
are parameterised by a base URL supplied at construction time
plus the same reserved signed-proxy flag, and speak the same
dictated contract as §19.5.1:

| Operation        | Request                                                                 |
|------------------|--------------------------------------------------------------------------|
| Metadata refresh | `GET {base}/api/v2/nft/<contract>/<token_id>` with `chain` and `format`  |

The response body is parsed leniently: every
member is optional, and a nested URL-metadata object may equally
be hoisted into the root object. Members the provider did not
supply leave the cached values untouched.

The RPC handler that drives a metadata refresh merges the
returned object into persistence through the inventory trait's
bulk register operation plus an in-place merge of the URL
fields.

**Upstream divergence (informative).** Before this revision the
subsystem spoke a generic REST shape of its own devising and
applied it to the base URL that SDK/GUI callers supply. Because
that URL addresses a deployed indexer, every crawl request
resolved to a path the service does not serve and returned `404`,
which activation surfaced as a provider failure. The chapter
previously specified that shape, so the implementation conformed
and the defect was a specification defect. The dictated contract
replaces it outright: the generic shape had no deployed consumer
and is not retained.

### 19.5.3 Spam and Phishing

The subsystem applies spam and phishing flags to inventory
entries and transfer records by inspecting the token's URI,
image, animation, and external domain fields. The subsystem
embeds and ships no spam-domain or phishing-domain list.

Two flag sources are in scope:

1. **Local heuristics**, applied unconditionally: a field that
   embeds a URL where none is expected, and a token URI whose
   host or shape matches the locally-evaluated suspicion rules,
   raise the corresponding flag. This source needs no network
   access and is the source presently wired.
2. **A caller-supplied blocklist service**, addressed by the
   separate anti-spam base URL that the operational RPC methods
   already carry (§19.6). Its wire contract is dictated by the
   deployed blocklist service and consists of two `POST`
   endpoints under the caller-supplied base: a contract-scan
   endpoint at path `api/blocklist/contract/scan`, taking a
   chain identifier plus a set of contract addresses, and a
   domain-scan endpoint at path `api/blocklist/domain/scan`,
   taking a set of domains. Each returns the subset judged spam
   or phishing. Wiring this source is deferred (D6); until then
   the anti-spam base URL is accepted, validated, and otherwise
   unused, and its absence or failure must not fail any call.

The flags are stored as scalar columns on each inventory row and
are used purely for client-side filtering and display masking.
A blocklist-service failure must never fail the operation whose
result it was annotating; the operation completes with
locally-derived flags only.

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

**No provider-profile selector.** Only one wire contract is
bundled, so the methods that reach an indexer (`update_nft`,
`refresh_nft_metadata`) carry no profile-selection member. A
request member that names a wire shape would have exactly one
legal value, and accepting one would invite callers to depend on a
selection axis the subsystem does not offer.

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

For wire compatibility with legacy token-activation envelopes,
`enable_nft` accepts and ignores top-level `requires_notarization`,
`priv_key_policy`, and `provider` fields when present. The canonical
provider for activation remains `activation_params.provider`.

`activation_params` carries a single required member:

| Field      | Type                       | Req? | Notes                                                   |
|------------|----------------------------|------|---------------------------------------------------------|
| `provider` | object (tagged union)      | yes  | The indexer provider descriptor (below).                |

For the same compatibility reason, `activation_params` accepts and
ignores `requires_notarization` and `priv_key_policy` when present.

`provider` is a tagged union with an externally-tagged shape:
a `type` discriminant string selecting the provider variant and
an `info` object carrying that variant's configuration. Every
variant carries the same `info` members:

| `info` field   | Type    | Req? | Default | Notes                                                                                   |
|----------------|---------|------|---------|-----------------------------------------------------------------------------------------|
| `url`          | string (URL) | yes | --    | Caller-supplied indexer base URL used for the initial inventory fetch. Consistent with R1: no default or embedded value -- the caller supplies it at RPC time. |
| `komodo_proxy` | boolean | no   | `false` | Signed-proxy flag (the same reserved per-provider signed-proxy flag described in §19.5 / D4). |

One discriminant value is bound:

| `type` value | Origin                                                                 |
|--------------|-------------------------------------------------------------------------|
| `Moralis`    | Fixed by the SDK/GUI clients that call the method. A client that sends any other value cannot activate, so the literal is functionally necessary and is stated here under the R1 carve-out and [`docs/INTEROP_NAMING_POLICY.md`](../INTEROP_NAMING_POLICY.md). It names a wire variant, not a host, and carries no endpoint. |

An unrecognised `type` is a client-input error. The method must
never substitute a contract it was not asked for, because a
wrong-path request against a live indexer is indistinguishable
from a genuinely-absent resource.

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
   platform coin and performs the **initial owned-inventory
   fetch** against the caller-supplied `url`, populating the
   owned-NFT snapshot returned in `nfts`.
4. The method is the activation counterpart to `update_nft`:
   `enable_nft` brings the subsystem into existence and performs
   the first inventory fetch; `update_nft` performs subsequent
   re-crawls (inventory plus transfer history) against a
   caller-supplied crawl-provider URL once the subsystem is
   active. A client that has called `enable_nft` does not need a
   separate `update_nft` to obtain the initial inventory.

**Activation scope (binding).** Activation is deliberately the
*narrowest* fetch that can satisfy the response contract:

| Work item                                            | At activation | Behind `update_nft` |
|------------------------------------------------------|---------------|---------------------|
| Ensure per-chain storage is ready                    | yes           | yes                 |
| Owned-inventory fetch for the active address, paged to completion | yes | yes            |
| Transfer-history crawl and transfer-log append       | **no**        | yes                 |
| Per-token detail lookups derived from transfers      | **no**        | yes                 |
| Metadata back-fill into the transfer log             | **no**        | yes                 |
| Scan-bookmark advance                                | **no**        | yes                 |
| Blocklist-service annotation (D6)                    | no            | yes                 |

Activation must therefore issue exactly the owned-inventory
operation of §19.5.1, repeated only to follow pagination. It must
not walk transfer history, because a transfer walk multiplies the
number of provider requests by the size of the account's history
and turns any one of them into an activation-blocking failure,
while contributing nothing the `nfts` response member needs. The
inventory the fetch observes is persisted so the operational read
methods see it immediately.

**Failure semantics (binding).**

- A **precondition** failure (items 1 and 2 above, unresolvable
  ticker, non-NFT protocol, non-EVM platform, declared/resolved
  platform mismatch, syntactically invalid `url`, unrecognised
  provider discriminant) fails activation. The
  ticker is not marked active and nothing is persisted.
- A **provider or transport** failure of the owned-inventory
  fetch fails activation. The ticker is not marked active, and
  any entries already written from earlier pages of the same
  fetch are not left behind as a partial snapshot. This preserves
  the contract SDK/GUI clients depend on: a successful
  `enable_nft` means `nfts` is the account's complete inventory,
  not a truncated prefix.
- A **per-entry** defect within an otherwise successful response
  -- an entry the profile says to skip, or an entry that fails to
  parse -- must not fail activation. Such entries are omitted from
  the snapshot and activation proceeds.
- A **blocklist-service** failure must not fail activation
  (§19.5.3).

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
- Caller-supplied provider URL syntactically invalid, or the
  provider discriminant unrecognised. The wire discriminant is
  `Internal` (HTTP 500), not a request-parse failure, as Chapter 35
  R35.2.8 binds.
- The initial inventory fetch failed to reach the indexer, or the
  indexer returned a non-success status or an unparseable
  envelope (upstream-dependency error). This condition is
  reported distinctly from a client-input error so an operator can
  tell a misconfigured request from a misbehaving indexer.
- The NFT protocol's declared platform does not match the
  resolved platform coin (configuration consistency error).

**Wire error contract (binding, cross-reference).** `enable_nft` is
dispatched to the same single-token activation handler as
`enable_erc20`, so its wire error contract -- the `error_type` set,
the `error_data` shape and the HTTP status of each failure class
listed above -- is the single-token contract of
[Chapter 35](35-evm-v2-activation-rpcs.md) R35.2.5, with the
NFT-specific failure mapping of R35.2.8 (for example: backing
platform not active → `PlatformCoinIsNotActivated` with the platform
ticker as a string; NFT already active → `TokenIsAlreadyActivated`
with the NFT ticker as a string; provider unreachable → `Transport`,
HTTP 500). Chapter 35 is authoritative for those wire values; this
section remains authoritative for the request/response shapes, the
activation scope and the graded failure semantics.

**Inline activation (cross-reference).** The NFT pseudo-coin of an
EVM platform can also be activated together with the platform, by
supplying `nft_req` (an `NftProvider`, bare or wrapped in
`provider`) to `enable_eth_with_tokens` or `task::enable_eth::init`
([Chapter 35](35-evm-v2-activation-rpcs.md) R35.1.4b). Inline
activation performs the same initial owned-inventory fetch for the
platform's single address and reports the snapshot in the platform
result's always-present `nfts_infos` map, keyed and shaped like the
`nfts` member above. It is unavailable when the platform address is
HD-derived; there, `enable_nft` is used after activation.

> **Reloaded status (informative).** Since `fix/activation-api-parity`,
> `enable_nft` is served by the single-token handler with the
> discriminants and shapes of Chapter 35 R35.2.5 / R35.2.8, and inline
> `nft_req` activation is supported; remaining deviations are listed in
> the status notes of Chapter 35 §35.1 and §35.2.

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
   NFT withdrawal for TRON-family chains ([Chapter 21](21-tron-integration.md)
   D5) and returns an explicit error to that effect.
2. Encode the calldata using a generic Ethereum ABI encoder
   over the embedded fragment for the selected method.
3. For the ERC-1155 variant, if the max-balance drain flag is
   set, query `balanceOf(owner, id)` via an `eth_call` and use
   the returned balance as the transfer amount.
4. Resolve gas using either the caller's explicit fee override
   or an estimate against the encoded calldata.
5. Resolve nonce using the platform coin's nonce-resolution
   path.
6. Build the transaction in the untyped legacy form, priced from
   the single gas price of step 4, and sign it with the
   keypair-derived legacy signing path. The form does not vary:
   the request accepts only the `EthGas` fee form (`gas_price`,
   `gas`) and rejects any other, and the coin's swap gas-fee policy
   ([Chapter 35](35-evm-v2-activation-rpcs.md) R35.6.3) is not
   consulted, so no NFT withdrawal is built as an EIP-1559
   transaction. An untyped transaction is type `0`, which no
   `max_eth_tx_type` cap (ch. 35 D35.7) can be below, and the
   legacy signing path does not consult the cap. The refusal of a
   form above the cap is enforced only on the EIP-1559 signing
   path, which this flow does not use.
7. Return transaction details with fee values denominated in
   the platform coin (`ETH`, `BNB`, `MATIC`, `AVAX`, `FTM`).

> **Upstream divergence (informative) — the reference builds a typed
> transaction at step 6.** Confirmed at release designations `v2.6.0-beta` and
> `v3.0.0-beta`: the reference resolves **both** EVM withdraw fee forms on
> this path, derives the transaction's envelope type from whichever was
> resolved (untyped for the legacy form, type `2` for the EIP-1559 form),
> checks that type against the coin's envelope-type cap **before** building,
> and then builds and signs in that type. Both non-fungible withdraw
> flavours go through the same shared fee resolution as the platform-coin
> and token withdrawals, so an NFT withdrawal can be and routinely is a
> type-`2` transaction there.
>
> The divergence is therefore a parity **gap** on a public request surface,
> not a deliberate choice: a caller that sends the EIP-1559 fee form to a
> reloaded node receives an expected-`EthGas` error where a reference node
> would honour it, and a reloaded NFT withdrawal always signs the untyped
> envelope even on a chain whose configuration entry caps the envelope type
> at `2` and whose other withdrawals the reference would send as type `2`.
> No funds are at risk — an untyped transaction remains valid on every EVM
> chain this project supports — but the fee the user pays is priced from the
> legacy gas price rather than from an EIP-1559 estimate.
>
> The contract to implement is bound once, for all four EVM withdraw
> flavours together, in [Chapter 57](57-evm-fee-inputs.md) §57.7
> (R57.7.1–R57.7.5); R57.7.5 names these flows explicitly. Step 6 above
> describes reloaded as built and is superseded by that contract when the
> gap is closed. The remaining work is tracked as §19.9 D8 below.

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
    `activation_params.provider`), accepts and ignores the documented
    legacy token-activation compatibility fields, and returns the §19.6.2
    success fields (`platform_coin` and `nfts`) on a successful native
    activation.

T2. **`enable_nft` activation failures.** A conformance test shall
    verify the functional failure categories listed in §19.6.2 for
    missing backing platform activation, already-active NFT support,
    invalid NFT ticker/protocol, unsupported platform, provider failure,
    and protocol/platform mismatch.

The provider layer carries its own request-shape coverage:

T3. **Profile A request construction.** A test shall assert, for each
    of the five chains and for each of the three Profile A
    operations, the exact path and the exact query-parameter set and
    values the provider emits for a given base URL, owner, contract,
    token id, and from-block, including the case where the supplied
    base URL carries a trailing slash and the case where it carries a
    non-empty path prefix. This test is the regression guard for the
    wrong-path defect described in the divergence note at the end of
    §19.5.2.

T4. **Profile A response decoding.** A test shall decode a recorded
    Profile A payload and assert that string-encoded block numbers and
    token ids, the timestamp-string transfer field, and the
    collection-name member all land in the model, that an entry
    lacking the contract-type member is skipped rather than failing
    the batch, and that a token-detail `404` decodes as absence rather
    than as an error.

T5. **Profile A pagination.** A test shall drive a stubbed provider
    that returns a page token on the first response and none on the
    second, and assert that both pages are requested, that the second
    request carries the first response's page token, and that the
    combined result is the union of both pages.

T6. **Provider discriminant.** A test shall assert that the bound
    discriminant on the activation provider descriptor is accepted,
    that an unrecognised value is a client-input error, and that no
    substitution or retry under a different wire contract occurs.

T7. **Activation issues no transfer request.** A test shall activate
    against a stubbed provider that fails every operation except the
    owned-inventory one, and assert that activation succeeds, that the
    returned snapshot matches the inventory response, and that no
    transfer-history, token-detail, or latest-block operation was
    invoked.

T8. **Activation atomicity.** A test shall fail the owned-inventory
    fetch on its second page and assert that activation reports the
    upstream-dependency error, that the NFT ticker is not marked
    active, and that no first-page entries remain persisted.

End-to-end integration tests against a live indexer are not in
the test set at the time of writing; they are named as
follow-on work once a deterministic local test fixture for the
EVM activation surface is available (D5). The fixture should serve
Profile A, since Profile A is the contract deployed callers exercise.

## 19.9 Binding Requirements and Deferred Work

The following are **binding rules** for this subsystem:

R1. **No embedded indexer endpoints.** The subsystem shall
    embed no third-party indexer hostname, no default or fallback
    provider base URL, and no vendor-operated endpoint of any
    kind. All HTTP base URLs used by the crawl, metadata, and
    blocklist providers are caller-supplied at RPC time. A
    discriminant token a caller sends inside its own request
    payload, and the path and query vocabulary a third-party API
    dictates for whatever base URL the caller supplied, are
    interop surface and are not endpoints for the purpose of this
    rule.

    **Carve-out — dictated tokens that happen to be proper nouns.**
    Where a third-party API dictates a literal path or query token,
    that token remains interop surface even when it is a vendor,
    product, or organisation name, and this chapter may state it
    verbatim. The test is functional necessity, not the token's
    spelling: if the service cannot be addressed without the token,
    withholding it would make the chapter unimplementable while
    protecting nothing. This carve-out covers the token only in its
    dictated position — it authorises no hostname, no default base
    URL, no branding, and no claim of affiliation, and it is
    conditional on the use not breaching the service's licence,
    terms of service, or trademark rights. Where any of those
    would be breached, the token stays out and the requirement is
    expressed without it. See [`docs/INTEROP_NAMING_POLICY.md`](../INTEROP_NAMING_POLICY.md),
    which generalises this decision beyond this chapter.

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

R6a. **Deployed-indexer wire contract.** The subsystem shall ship
     a crawl provider and a metadata provider implementing the
     dictated contract of §19.5.1 -- the request paths,
     query-parameter names and values, cursor pagination, response
     envelope, and member encodings dictated by the EVM NFT indexer
     API that deployed KDF-family callers address. It is the only
     bundled wire contract; the generic REST shape this project
     previously specified had no deployed consumer and shall not be
     retained. The contract shall carry no hostname (R1).

R6b. **No probing.** The subsystem shall not infer a wire contract
     from the shape of the base URL, shall not probe the provider to
     discover one, and shall not retry a failed request under a
     different contract. An unrecognised provider discriminant shall
     be a client-input error.

R7. **Standards-only ABI fragments.** The withdrawal path's
    embedded ABI fragments shall be the public ERC-721 and
    ERC-1155 on-chain interface definitions and nothing more.

R8. **First-class NFT activation entry point.** The subsystem shall
    expose `enable_nft` as the mmrpc-2.0 activation method for NFT
    support on the native target. The method shall accept the §19.6.2
    request shape (`ticker`, optional inline NFT `protocol`, required
    `activation_params.provider`) plus the documented legacy
    token-activation compatibility fields, and shall return the §19.6.2
    response shape (`nfts`, `platform_coin`). The provider base URL shall
    be caller-supplied at RPC time; the subsystem shall not embed a
    default indexer URL.

R9. **Activation vs refresh split.** `enable_nft` shall mark NFT
    support active for the requested NFT pseudo-coin ticker and, on
    the native target, perform the initial owned-inventory fetch.
    `update_nft` shall remain the refresh/re-crawl method for an
    already-active NFT subsystem and shall not be the activation
    substitute.

R9a. **Minimal activation scope.** Activation shall fetch the
     owned inventory for the active address and nothing else, per
     the §19.6.2 scope table. It shall not walk transfer history,
     shall not issue per-token detail lookups, shall not back-fill
     metadata into the transfer log, and shall not advance the
     scan bookmark. It shall follow provider pagination to
     completion so the returned snapshot is the account's whole
     inventory. All omitted work belongs to `update_nft`. Because
     this fetch is unconditional, activation is available only
     under a profile that provides the owned-inventory operation
     (R6a).

R10. **Activation preconditions and failures.** `enable_nft` shall
     require the resolved backing EVM platform coin to already be
     activated, shall reject an already-active NFT ticker, shall reject
     an invalid NFT ticker or non-NFT protocol, shall reject a
     non-EVM backing platform, and shall reject an inline NFT protocol
     whose declared platform disagrees with the platform resolved from
     the ticker. A failed precondition shall not mark the NFT ticker
     active and shall not persist a partial initial fetch result.

R10a. **Graded failure semantics.** A failure of the initial
      owned-inventory fetch -- transport, non-success status, or
      unparseable envelope -- shall fail activation, shall be
      reported as an upstream-dependency condition distinct from
      client-input conditions, shall leave the NFT ticker
      unmarked, and shall leave no partial snapshot persisted. A
      defect confined to a single entry of an otherwise valid
      response shall omit that entry and shall not fail
      activation. A blocklist-service failure shall never fail
      activation or any operational method.

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
    indexer fixture (§19.8). The fixture serves Profile A.

D6. **Blocklist-service annotation.** The contract-scan and
    domain-scan endpoints of §19.5.3 are specified but not wired.
    Until they are, the anti-spam base URL carried by the
    operational methods is accepted and validated but unused, and
    spam/phishing flags come from the local heuristics only.

D7. **Incremental activation snapshot.** Activation currently
    returns the whole owned inventory in one response, so a very
    large account pays the full page walk before activation
    completes. Returning early and continuing the walk in the
    background would require a progress/completion signal the
    activation response shape does not currently carry
    ([Chapter 10](10-sse-streaming.md)).

D8. **Typed (EIP-1559) NFT withdrawal.** The withdrawal path of
    §19.7 accepts only the legacy fee form and always signs the
    untyped envelope, where the reference resolves both fee forms
    and derives the envelope type from the resolved one (see the
    divergence note in §19.7). Closing it means routing both
    non-fungible flows through the shared EVM withdraw fee
    resolution bound in [Chapter 57](57-evm-fee-inputs.md) §57.7,
    which also brings the envelope-type-cap check
    ([Chapter 35](35-evm-v2-activation-rpcs.md) D35.7) onto this
    path. It is deferred to that chapter rather than specified
    here because the same change must land for the platform-coin
    and token withdrawals at the same time — a fee-form
    enumeration extended for one flavour and not the others would
    leave the public request surface inconsistent across
    withdrawals of the same coin.

## 19.10 External References

- The ERC-721 standard (the on-chain interface used by the
  withdrawal path's `transferFrom`).
- The ERC-1155 standard (the on-chain interface used by the
  withdrawal path's `safeTransferFrom` and `balanceOf`).
- The Ethereum JSON-RPC `eth_call` method (used for the
  `balanceOf` query on the ERC-1155 max-balance drain path).
- The EIP-1559 transaction format, named in two registers in this
  chapter: in §19.7 step 6 as the typed form this implementation's
  withdrawal path does not build, and in the upstream-divergence note
  that follows that step — with the deferred item §19.9 D8 that
  follows from it — as the fee form and envelope type the reference
  resolves and signs on the same path.
- The EVM chain ecosystems named in §19.3 (Ethereum, BNB Smart
  Chain, Polygon, Avalanche, Fantom) and their respective
  platform-coin tickers.
- The publicly documented EVM NFT indexer web API whose request
  and response contract Profile A of §19.5.1 reproduces: the
  owned-NFT-by-address, NFT-transfers-by-address, and
  NFT-metadata-by-contract-and-id endpoints of its version-2
  surface, together with its cursor pagination convention and its
  decimal-token-id request format. Cited as a counterparty-defined
  inter-operability shape (chapter 01 R4 / R15), not as an
  implementation. The chapter binds only the paths, parameters,
  and member encodings; the host is always caller-supplied (R1).
- The blocklist scan endpoints of §19.5.3, likewise cited as a
  counterparty-defined inter-operability shape.

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
  interop facts for `enable_nft`; the counterparty-defined request and
  response contract of the publicly documented EVM NFT indexer web API
  and of the blocklist scan endpoints (§19.10), consumed as
  inter-operability shapes under chapter 01 R4.
- *Permitted-input classes used:* R1 baseline source; R3 public
  specification documents; R4 counterparty-defined inter-operability
  shapes; R6 publicly observable endpoint behaviour; R7 independent
  work. Public ticker-symbol documentation under R3.
- *Sibling-allowlist consultations:* [Chapter 35](35-evm-v2-activation-rpcs.md)
  (the shared single-token wire error contract and inline `nft_req`
  activation cross-referenced from §19.6.2).
- *Forbidden corpus:* not consulted for this chapter's own rules; the
  two §19.6.2 cross-reference paragraphs restate Chapter 35, whose
  wire values the KDF Spec Reader established from the upstream
  reference version.
- *Clean-context additions (forbidden corpus not consulted):* the
  sentence at the end of step 6 of §19.7 (refusal of a transaction form
  above the coin's maximum) was added on 2026-10-16 by a context that
  never read the forbidden corpus. Later the same day, also by a
  context that never read it, step 6 was rewritten and the EIP-1559
  entry of §19.10 corrected to match. Since the chapter's first draft
  both had said the withdrawal signs legacy or EIP-1559 according to
  the platform coin's fee policy; the source consults no such policy
  on this path and never builds an EIP-1559 transaction there, so
  under chapter 01 R23 the chapter was corrected to the source. The
  rewritten step absorbs the earlier sentence. It states this
  implementation's own behavior as decided in chapter 35 D35.7 and
  observed in this repository's source: `coins::nft::withdraw` accepts
  only the `EthGas` fee form and builds and signs the untyped legacy
  form, whose signer in `coins::eth` does not consult the maximum; the
  EIP-1559 signer there enforces it. It describes no reference
  behavior and relies on no Dirty Gate clearance.
- *Scoped forbidden-corpus record for the §19.7 divergence note and
  §19.9 D8:* the "Upstream divergence (informative)" note appended to
  §19.7, and the deferred item D8 that follows from it, were authored
  under the chapter-01 §1.4A two-team workflow and do draw on
  forbidden-class input, scoped to one fact and one question only: that
  the reference's non-fungible withdraw flows resolve both EVM withdraw
  fee forms through the same shared resolution as its other EVM
  withdrawals, derive the transaction envelope type from the resolved
  form, and check it against the per-coin envelope-type cap before
  building. Established under the public release designations
  `v2.6.0-beta` and `v3.0.0-beta`; the record is no more precise than
  those designations. The contract a Coder implements from is bound in
  [Chapter 57](57-evm-fee-inputs.md) §57.7 and not here. No
  discretionary expression — no private type, field or function name, no
  body, control-flow transcription, local name, or error literal — was
  reproduced into this chapter.
- *Dirty Gate clearance (chapter 01 T1, R40, R43) for the §19.7
  divergence note and §19.9 D8:* the forbidden-class-sourced material in
  this chapter is exactly the two additions the scoped record above
  names — the "Upstream divergence (informative)" note that follows the
  step list of §19.7, and the deferred item §19.9 D8 that follows from
  it. Both were authored by the KDF Spec Reader under the two-team
  procedure of chapter 01 §1.4A and **passed the KDF Dirty Gate** on
  2026-10-24: PASS, discretionary-expression measure 0.00, finding list
  empty, and accordingly no remedy class for any line or for the
  candidate as a whole. That was the first gate pass on this material; no
  earlier verdict for it exists or was relayed, and the chapter's
  pre-existing content was outside the gated candidate and was not
  re-gated — it remains covered by the clean-context item above, which
  still holds for step 6 of §19.7 and for what the §19.10 entry states
  about this implementation. The clearance is therefore **complete and
  not pending**, and R40 withholds nothing here. The EIP-1559
  external-reference entry of §19.10 was brought into line with the note
  at the same time; it cross-references the note and D8 and carries no
  fact beyond what they state, so it is not a separately consulted
  section and required no pass of its own.

  *Why no section carries an inline pointer to this item.* The companion
  [Chapter 57](57-evm-fee-inputs.md) puts a short clearance pointer at
  the head of each of its six consulted sections; this chapter records
  the clearance here only. That is a scope judgement, written down so
  that it cannot be read as an oversight, and three things distinguish
  the two cases. First, the at-the-point duty of R40 attaches to a
  *pending* clearance; completing the record discharges that duty rather
  than relocating it. Second, what the duty guards against — a reader or
  an implementer mistaking a candidate for a cleared requirement — has
  no purchase on these two insertions: neither binds anything, the note
  is labelled informative in its own first line and names the reference
  designations it was established at, D8 is a deferral, and both state
  on their face that the contract to implement is bound in
  [Chapter 57](57-evm-fee-inputs.md) §57.7 and not here, so there is
  nothing here to implement from. Third, every substantive section of
  this chapter — the storage, provider, wire-surface and
  withdrawal-path rules — rests on the permitted-input classes alone,
  and a clearance notice repeated across them would suggest the converse
  of what the clean-context item above records. Chapter 57's shape
  answers a chapter whose six substantive sections are themselves
  forbidden-class-sourced and carry its binding rules; two short,
  clearly labelled, non-binding insertions in an otherwise clean chapter
  are the lighter case, and one record is the proportionate answer to
  it.

  The three elements of the R39 scope attestation, carried forward here
  in corpus-free terms as chapter 01 T1 requires:

  - *Extent.* The forbidden-class material corresponding to the subject
    matter the two additions address: the EVM withdraw fee-input surface
    and the non-fungible-token withdrawal flows of this project's coin
    subsystem. Reference designations: the publicly published release
    designations `v2.6.0-beta` and `v3.0.0-beta`, which the lineage's own
    releases carry and which chapter 01 §1.7 admits as public-release
    facts. **The record is no more precise than those two designations**
    — no narrower locator is given, by R11: no post-anchor commit
    identifier, no branch state, and no location within the compared
    material. Quantity examined: **ten file-versions**, five files at
    each of the two designations; within those, the withdrawal
    fee-resolution and non-fungible withdrawal regions were additionally
    examined by reading at both designations.

  - *Method.* **Combined.** Direct reading of the compared material at
    both reference designations, together with a mechanical comparison of
    the candidate's added lines against that material: a token-level pass
    over every code-like identifier in the added lines, and a
    word-five-gram overlap pass of the added prose against all ten
    file-versions. This repository's own clean-room gate tool under
    `tools/` was **not applicable and was not run:** it operates on files
    of source and the added material embeds no code block, so the
    instrument had no operand. The absence is stated so that it is
    visible rather than implied.

  - *Measure.* **0.00** on the discretionary-expression basis — the same
    figure the leak report carries, not a second one — read against the
    judgement standard R39 takes from R35: discretionary expression only,
    with dictated-interop and pinned-interface content excluded from the
    measured set before the judgement was made. Two observations recorded
    with the figure: of **653** word-five-grams in the added lines,
    **none** occurs in any compared file-version; and the added lines
    carry exactly **one** code-like token, a wire fee-form tag of the
    public withdraw request surface, which chapter 57 §57.7 binds and
    which this repository's own source already carries, so it is dictated
    and excluded from the measured set. Absent throughout the added
    material, as the verdict's classification records: private
    identifiers; function bodies; control-flow transcription; local
    names; error and log string literals; per-method behaviour tables
    keyed to internal names; internal module trees; and internal
    test-function names.

## 19.13 CoinProtocol::NFT Variant

NFT entries in the GLEEC coins config carry `{"type":"NFT","protocol_data":{"platform":"<EVM>"}}`.
Because `CoinProtocol` is a closed enum, an unknown `type` tag causes `from_conf_json` to
return a serde error, which propagates as a confusing message whenever a user calls `electrum`
or `enable` on an NFT ticker.

A permissive `NFT { platform: String }` variant was added to `CoinProtocol`
(in `mm2src/coins/lp_coins_context.rs`) that:
- Deserializes the GLEEC config shape without error.
- Is explicitly rejected by `lp_coininit` with the message
  *"NFT protocol is not supported by lp_coininit - use enable_nft instead"*.
- Returns a `CoinIsNotSupported` error from `orderbook_address` (not applicable to NFT).
- Is handled by the `get_private_keys` catch-all arm with a "not supported" message.

NFT activation itself does **not** go through `CoinProtocol` matching; it goes through
the dedicated `enable_nft` RPC handler which reads `NftProtocolData` directly from the
activation request (`mm2src/coins/nft/activation.rs`). Startup config loading never calls
`from_conf_json` on the coins array, so an NFT entry in the startup config is benign.
