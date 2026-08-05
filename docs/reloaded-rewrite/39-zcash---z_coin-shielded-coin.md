# Chapter 39 -- Zcash / z_coin Shielded Coin

**Status:** driving-spec (as-built baseline **plus** remaining
required-but-unimplemented extensions). Mixed treatment -- see §39.0.

> **One-sentence claim:** the project shall support Zcash-Sapling shielded coins
> (ARRR / ZOMBIE-style) as a first-class coin type that activates in either a
> full-node ("native") mode or a light-client mode backed by Electrum servers
> plus one or more lightwalletd gRPC endpoints, drives shielded balance/scan
> through a long-running task RPC, performs atomic swaps via shielded HTLCs, and
> (by required port) gains WASM support, activation-time sync tuning, and
> integrity verification of the Sapling proving/verifying parameters.

## 39.0 Treatment & scope split

- **§39.1--§39.5 (T-DOC, as-built):** verified present in reloaded -- native-only
  ZCoin type, dual activation modes (Native / Light), multi-lightwalletd light
  mode, the `init_z_coin` task-RPC trio with its progress states, shielded HTLC
  swap operations, and the activation result shape.
- **§39.6 (T-PORT, implemented):** WASM support, activation-time sync tuning /
  sync-from-date, Sapling parameter integrity verification, sourcing consensus
  parameters / checkpoint / HD path from `protocol_data`, and HD-derived
  shielded key-policy derivation are all implemented. One narrow platform gap
  remains: WASM shielded-transaction *building* (blocked by the absent
  `LocalTxProver` parameter files; §39.6.1). The transparent UTXO side of a
  shielded coin is iguana-derived even under an HD wallet; only the shielded
  Sapling key is HD-derived, per R39.6.4 §2.

> **Binding scope (R36).** Requirements bind observable behaviour, the public
> activation/task RPC surface and its JSON field names, and externally *dictated*
> interop: the Zcash Sapling protocol (shielded note/commitment-tree semantics,
> Sapling spend/output proving system) and the **lightwalletd gRPC** service
> contract. Sapling cryptography and the lightwalletd protocol are the source of
> truth, not this project's code. Private types and helper structure are
> informative.

---

## Part A -- As-built baseline (T-DOC)

## 39.1 Coin type & platform

R39.1.1 The shielded coin (`ZCoin`) is a UTXO-derived coin type that adds a
Sapling shielded layer. In reloaded it is built on the **native** target only;
the WASM build excludes it (see §39.6.1 for the required port).

R39.1.2 A shielded coin's `coins`-config `protocol` field is a tagged object
with `type` = `"ZHTLC"` **and a required `protocol_data` object**. The
coin-protocol type is an *adjacently tagged* union (the `type` string is the
tag; the `protocol_data` object is the content). The `ZHTLC` arm is a
**payload-carrying** variant — not a unit variant — whose content deserializes
into a shielded protocol-info structure with three members:

- `consensus_params` (object, **required**) — the Zcash consensus parameters
  for the coin (schema in R39.1.3);
- `check_point_block` (object, **optional**) — a sync-anchor block descriptor
  (schema in R39.1.4);
- `z_derivation_path` (string, **optional**) — a coin-level BIP32/ZIP32 HD path
  (e.g. `m/32'/133'`) used for shielded key derivation (R39.1.4).

Because the variant carries a required payload whose required member is
`consensus_params`, a **bare `{"type":"ZHTLC"}` with no `protocol_data` is
non-conformant** and shall fail coin-config deserialization (adjacently-tagged
serde has no content to populate the required fields). Every real config carries
`protocol_data`: production coins (ARRR/PIRATE) and the ZOMBIE test coin all
ship a full `protocol_data` block. See R39.6.4 for the required *consumption* of
these values by the shielded-coin builder.

R39.1.3 The `consensus_params` object is the Zcash network-parameter set and has
the following members (this is dictated config/wire interop — the field names
and JSON shapes are fixed):

| Field | JSON type | Required | Notes |
|-------|-----------|----------|-------|
| `overwinter_activation_height` | integer (unsigned 32-bit) | yes | Overwinter network-upgrade activation height. |
| `sapling_activation_height` | integer (unsigned 32-bit) | yes | Sapling activation height; also the lower floor for any sync start point. |
| `blossom_activation_height` | integer or `null` | no (nullable) | Blossom activation height, or `null` if not applicable. |
| `heartwood_activation_height` | integer or `null` | no (nullable) | Heartwood activation height, or `null`. |
| `canopy_activation_height` | integer or `null` | no (nullable) | Canopy activation height, or `null`. |
| `coin_type` | integer (unsigned 32-bit) | yes | SLIP-44 coin type used in shielded HD derivation. |
| `hrp_sapling_extended_spending_key` | string | yes | Bech32 human-readable prefix for extended spending keys. |
| `hrp_sapling_extended_full_viewing_key` | string | yes | Bech32 HRP for extended full-viewing keys. |
| `hrp_sapling_payment_address` | string | yes | Bech32 HRP for shielded payment addresses. |
| `b58_pubkey_address_prefix` | array of exactly 2 integers (each 0–255) | yes | Base58Check version prefix for transparent p2pkh addresses. |
| `b58_script_address_prefix` | array of exactly 2 integers (each 0–255) | yes | Base58Check version prefix for transparent p2sh addresses. |

R39.1.4 The optional `check_point_block` object, when present, is a sync-anchor
descriptor with all of the following members:

| Field | JSON type | Notes |
|-------|-----------|-------|
| `height` | integer (unsigned 32-bit) | Block height of the checkpoint. |
| `hash` | string | 32-byte block hash, hex-encoded. |
| `time` | integer (unsigned 32-bit) | Block timestamp (Unix seconds). |
| `sapling_tree` | string | Hex-encoded Sapling commitment-tree state as of this block. |

The optional `z_derivation_path` is a coin-level HD path string (ZIP32/BIP32
form, e.g. `m/32'/133'`). When the active key policy is HD-derived, the shielded
spending key is derived along this path with the activation `account` appended
as a hardened child (i.e. `m/<z_derivation_path>/account'`); it is required only
for the HD key policy (its absence is an error only in that policy). Both
`check_point_block` and `z_derivation_path` are consumed by the builder per
R39.6.4.

> **Upstream divergence (informative).** Reloaded currently defines the `ZHTLC`
> coin-protocol arm as a **unit** variant (no payload) and its shielded builder
> hardcodes Zcash-mainnet constants (see R39.6.4). Consequently reloaded's
> current behaviour is the inverse of upstream: it accepts a bare
> `{"type":"ZHTLC"}` and would *reject* a config that carries a `protocol_data`
> map (an adjacently-tagged unit variant cannot absorb content). Conforming to
> R39.1.2–R39.1.4 requires making the arm payload-carrying and updating the
> ZOMBIE test fixtures to ship full `protocol_data` (see R39.6.4 status note).

## 39.2 Activation modes

R39.2.1 Activation is a long-running task exposed as the public RPC trio
`init_z_coin` / `init_z_coin_status` / `init_z_coin_user_action`.

R39.2.2 The activation request carries a `mode` object (a tagged union with tag
field `rpc` and payload field `rpc_data`) selecting one of:
- **Native** -- talks to a full Zcash-family node; no `rpc_data` payload.
- **Light** -- a light client whose `rpc_data` object carries `electrum_servers`
  (the UTXO-side transparent backend) and `light_wallet_d_servers` (a **list** of
  lightwalletd gRPC endpoints) for the shielded side. The **same `rpc_data`
  object** is also where the optional shielded **sync starting point**
  `sync_params` (and its companion `skip_sync_params` boolean) live in the
  dictated wire (R39.6.2) — **not** at the top level of the activation params.

R39.2.3 The request also carries optional `required_confirmations` and
`requires_notarization` fields (top-level on the activation params). Sync-tuning
fields that sit at the top level (`scan_blocks_per_iteration`, `scan_interval_ms`,
`zcash_params_path`) are specified in R39.6.2.

R39.2.4 The light mode shall accept **more than one** lightwalletd endpoint so a
deployment can list several servers.

## 39.3 Activation progress, Trezor & result

R39.3.1 The activation task shall report progress through observable in-progress
states covering at least: activating the coin, scanning the shielded chain,
requesting the wallet balance, and finishing. On the dictated public task-status
wire these serialize (at minimum) as `ActivatingCoin`, the two scan phases
`UpdatingBlocksCache` and `BuildingWalletDb` (each carrying `current_scanned_block`
and `latest_block` unsigned counters), `RequestingWalletBalance`, and
`Finishing`; the hardware-wallet states of R39.3.2
(`WaitingForTrezorToConnect`, `WaitingForUserToConfirmPubkey`) appear on the same
status wire. These variant names are part of the externally observable task-RPC
surface. (Verified identical between the `v2.6.0-beta` reference and the
current upstream `dev` head.)

> **Status update (reloaded).** Implemented (convergence item #1). Activation
> reports the two dictated scan phases with live progress: `UpdatingBlocksCache`
> during the lightwalletd compact-block download and `BuildingWalletDb` during
> the wallet-database scan, each carrying `current_scanned_block` and
> `latest_block` unsigned counters. The wallet-DB scan runs in bounded batches
> so `current_scanned_block` advances incrementally.

R39.3.2 When the wallet is hardware-backed, the task shall additionally surface
states asking the user to connect the device and to confirm the pubkey, and shall
accept the confirmation via `init_z_coin_user_action`.

R39.3.3 On success the task result shall report `ticker`, `current_block`, and a
`wallet_balance` carrying the shielded balance. (`ticker` and `current_block`
are verified present on the dictated result of both `v2.6.0-beta` and the
current upstream `dev` head.)

> **Status update (reloaded).** Implemented (convergence item #1). Reloaded's
> activation result (`ZcoinActivationResult`) now serializes the dictated
> top-level `ticker` alongside `current_block`, `wallet_balance`, and
> `first_sync_block`.

## 39.4 Sapling parameters & scanning (R31 externally dictated)

R39.4.1 Shielded proving requires the Sapling spend/output parameters; the
project shall load them from the local parameter location and use them to build
and verify Sapling proofs.

R39.4.2 In light mode the project shall fetch compact blocks / shielded note data
from the configured lightwalletd endpoint(s) over gRPC, scan them to detect
incoming and spent notes, and maintain the shielded note set and witness data in
local storage.

R39.4.3 Unconfirmed (mempool / not-yet-mined) shielded notes shall be tracked
correctly so that the spendable shielded balance does not double-count or omit
in-flight notes.

## 39.5 Shielded atomic swaps (R31 externally dictated)

R39.5.1 The project shall perform atomic swaps for shielded coins using a
Sapling-based HTLC construction, fulfilling the same maker/taker payment,
spend-with-secret, and refund-after-timelock semantics required of every coin in
the swap protocol.

---

## Part B -- Required ports (T-PORT)

> **Status of Part B:** R39.6.1, R39.6.2, R39.6.3, and R39.6.4 are all
> implemented, including HD-derived shielded key-policy support (R39.6.4 §2).
> The transparent (UTXO) side of a shielded coin remains iguana-derived; only
> the shielded Sapling spending key is HD-derived, as R39.6.4 §2 requires.

## 39.6 Required shielded-coin ports

### 39.6.1 WASM support
### 39.6.1 WASM support
R39.6.1 The shielded coin shall be buildable and activatable on the WASM target,
with its shielded note/witness storage backed by IndexedDB (mirroring the
native storage contract). Acceptance: a light-mode shielded coin activates in a
WASM build and reports a shielded balance.

> **Status update (reloaded).** Implemented (commits 5514802de, f1a0a338e,
> 6cad05692). `zcash_primitives` and `zcash_client_backend` are now available
> on the WASM target. The sapling state cache is backed by a new
> `SaplingStateCacheOps` trait; `ZCoinSqliteSaplingCache` serves native builds
> and `ZCoinIdbSaplingCache` (mm2_db IndexedDB backend) serves WASM. The
> `MmCoinEnum::ZCoin` variant and the `z_coin` module are available on all
> targets. Transaction building (`gen_tx` / `send_outputs`) remains native-only
> because `LocalTxProver` (sapling parameter files) is absent in WASM.

### 39.6.2 Activation-time sync tuning / sync-from-date
R39.6.2 The activation request shall optionally accept sync-control parameters so
a fresh wallet need not scan from Sapling activation. This surface is
**externally dictated interop** (the public `task::enable_z_coin::init` /
`init_z_coin` request schema, as also emitted by KDF-family wallets); the JSON
field names, their nesting, and their value shapes below are fixed by that
contract. (The whole surface described here was verified **identical** between
the `v2.6.0-beta` reference and the current upstream `dev` head.)

**Sync starting point (`sync_params`) \u2014 nested, not top-level.** The sync start
point is a field named `sync_params` carried **inside the Light-mode `rpc_data`
object** (`activation_params.mode.rpc_data.sync_params`), alongside
`electrum_servers` and `light_wallet_d_servers`. It is **not** a top-level field
on `activation_params`. It is optional and, when present, is an **externally
tagged** union (lowercase variant names) taking one of exactly three JSON forms:

| Form | JSON shape | Meaning |
|------|-----------|---------|
| height | `{"height": <unsigned integer>}` | Start syncing from this block height. |
| date | `{"date": <unsigned integer>}` | Start from the block matching this Unix timestamp (seconds). |
| earliest | `"earliest"` (the bare JSON string) | Start from the coin's Sapling activation height. |

An adjacently-tagged `{"type": "height"|"date", "data": ...}` shape and a
top-level `sync_start` field are **both wrong** for this surface and shall not be
treated as the dictated wire.

A sibling optional boolean `skip_sync_params` (also inside the Light `rpc_data`)
requests resuming from the last locally-synced block; `sync_params` is consulted
only when no prior synced state exists.

**Scan-throughput tuning \u2014 top-level.** Two optional throughput fields sit at the
**top level** of `activation_params` (not inside `rpc_data`):

- `scan_blocks_per_iteration` \u2014 unsigned **non-zero** integer, blocks scanned per
  iteration (dictated default 1000);
- `scan_interval_ms` \u2014 unsigned integer, milliseconds to pause between scan
  iterations (dictated default 0 = no pause).

**Sapling parameter path \u2014 top-level, optional.** `zcash_params_path` is an
optional string on `activation_params` giving the filesystem directory of the
Sapling proving/verifying parameter files. It is part of the dictated request
schema (consumed on native targets; see §39.4 / R39.6.3).

Acceptance: activating with a `sync_params` `{"date": T}` begins scanning at the
block corresponding to `T`, materially reducing initial scan time; `"earliest"`
scans from Sapling activation; `{"height": N}` scans from height `N` (resolution
semantics in R39.8.0g).

> **Client/doc discrepancy (informative).** The dictated daemon wire and the
> public API reference name the pacing field `scan_interval_ms`. Some
> desktop-wallet clients emit `scan_interval` instead. The upstream daemon
> defines **no** alias for the shorter spelling, so against an upstream daemon a
> `scan_interval` field is silently ignored and the default applies; emit
> `scan_interval_ms` for the value to take effect. (Reloaded additionally accepts
> `scan_interval` as an alias \u2014 see the status note.)

> **Status update (reloaded).** Implemented, with documented divergences from the
> dictated wire:
>
> - **Sync start point:** conformant. Reloaded accepts `sync_params` nested in
>   the Light-mode `rpc_data` in all three dictated forms (`{"height": N}`,
>   `{"date": T}`, `"earliest"`). Height and earliest starts are applied directly;
>   a date start is resolved to the matching block height before the initial
>   shielded scan (R39.8.0g).
> - **Throughput field names:** reloaded's canonical field names are
>   `blocks_per_iteration` and `inter_iteration_interval_ms`, but it accepts the
>   dictated `scan_blocks_per_iteration` as an alias for the former and both
>   `scan_interval_ms` (dictated) and `scan_interval` (desktop-wallet spelling) as
>   aliases for the latter, so all dictated and client spellings deserialize.
> - **Throughput default — implemented (convergence item #4).** Reloaded now
>   defaults blocks-per-iteration to the dictated **1000** when neither the coin
>   config nor the request supplies it.
> - **Throughput behavior — implemented.** The effective blocks-per-iteration
>   and interval values control both the legacy Native commitment-tree loop and
>   the modern Light wallet-database scanner. The Light scanner processes at
>   most the configured block count per wallet batch and applies the configured
>   millisecond pause between non-final batches. It logs the effective policy at
>   INFO and keeps per-network-batch, per-wallet-batch, timing, and frontier
>   details at DEBUG/TRACE so normal INFO operation remains bounded.
> - **`skip_sync_params` — implemented (convergence item #3).** Reloaded accepts
>   the boolean in the Light `rpc_data`; when set and prior local sync state
>   exists, activation resumes from that state and ignores `sync_params`. With
>   no prior state, `sync_params` is still consulted.
> - **`zcash_params_path` — implemented (convergence item #2).** Reloaded accepts
>   the top-level request field and, on native targets, loads the Sapling
>   proving/verifying parameters from that directory, falling back to the fixed
>   platform default when absent.

### 39.6.3 Sapling-parameter integrity verification
R39.6.3 Before use, the loaded Sapling spend/output parameters shall be verified
against their known-good integrity digests; parameters that fail verification
shall be rejected (and, if a downloader is provided, re-fetched). Acceptance: a
corrupted parameter file is detected and refused rather than used to produce
invalid proofs.

> **Status update (reloaded).** This requirement is implemented: Sapling spend
> and output parameter files are integrity-checked against canonical digests
> before prover initialization, and mismatches are rejected with explicit
> read/hash-mismatch errors.

### 39.6.4 Consume `protocol_data` consensus parameters
R39.6.4 The shielded-coin builder shall source **all** of its Zcash
network parameters, its shielded HD derivation path, and its sync checkpoint
from the coin config's `protocol.protocol_data` (R39.1.2–R39.1.4), rather than
from hardcoded Zcash-mainnet constants. Concretely:

- **Consensus parameters.** The `consensus_params` object shall be the single
  authority for the coin's network-parameter lookups: activation heights for
  each supported network upgrade (Overwinter/Sapling required; Blossom /
  Heartwood / Canopy optional), the `coin_type`, the three `hrp_sapling_*`
  human-readable prefixes, and the two `b58_*` transparent-address version
  prefixes. These values shall feed every place the builder and the running coin
  need network parameters: encoding/decoding the wallet's own shielded payment
  address and the DEX fee/burn shielded addresses (via
  `hrp_sapling_payment_address`), Sapling note trial-decryption and output
  recovery during scanning, address handling, and shielded-swap construction.
  A coin whose `consensus_params` differs from Zcash mainnet shall derive and
  scan accordingly.
- **HD derivation.** When the key policy is HD-derived, the shielded spending
  key shall be derived along `z_derivation_path` with the activation `account`
  appended as a hardened child (R39.1.4); with a raw/iguana key policy the path
  is not required.
- **Sync checkpoint.** `check_point_block` shall be the sync-start anchor. In
  native mode the wallet database is anchored at `check_point_block.height`,
  falling back to `sapling_activation_height` when the checkpoint is absent. In
  light mode the anchor is the checkpoint corresponding to the resolved sync
  start height (a height/date sync parameter, or the earliest/default), floored
  at `sapling_activation_height`; the checkpoint's `sapling_tree` seeds the
  wallet's initial commitment-tree state so scanning need not replay from
  Sapling activation.

Acceptance: a ZHTLC coin whose `protocol_data` declares non-Zcash-mainnet
parameters (different HRP/b58 prefixes, `coin_type`, or activation heights)
produces addresses and derives keys under those declared parameters and begins
its shielded sync from the declared `check_point_block` (or from
`sapling_activation_height` when no checkpoint is given) rather than from
mainnet constants.

> **Status update (reloaded).** Implemented (commit f1a0a338e). The shielded
> builder now sources all Zcash network parameters from the coin config's
> `protocol.protocol_data` payload:
>
> - **Consensus parameters (R39.6.4 §1).** The builder reads
>   `consensus_params` from `protocol_data` and uses its `coin_type`, `hrp_sapling_*`
>   prefixes, `b58_*` prefixes, and activation-height policy throughout the running
>   coin (address encoding/decoding, key derivation, scanning, transaction
>   construction, and commitment-tree sync). A ZHTLC coin with non-mainnet
>   parameters (e.g., different HRP or `coin_type`) now derives keys and
>   addresses under those declared parameters, not Zcash-mainnet defaults.
> - **Sync checkpoint (R39.6.4 §3).** The builder seeds the wallet's
>   commitment-tree cache at `check_point_block.height`, deserialized from the
>   checkpoint's `sapling_tree`, falling back to `sapling_activation_height` when
>   absent (native mode). In light mode the wallet's initial commitment-tree
>   state is instead seeded from the lightwalletd `GetTreeState` at the resolved
>   sync start height (one below the first fetched compact block), which supports
>   an arbitrary resolved start rather than only the config checkpoint height
>   (see the R39.8.0h status note).
> - **HD derivation path (R39.6.4 §2).** Implemented. The shielded spending key
>   is selected by the active key policy before the builder runs: under the HD
>   (BIP39) policy it is derived from the wallet's BIP39 seed along the coin's
>   `z_derivation_path` (purpose' / coin_type', both hardened) with the
>   activation `account` appended as a hardened child
>   (`m/<z_derivation_path>/account'`); under the legacy Iguana policy it is the
>   ZIP32 master of the iguana secret. `account` is an optional `init_z_coin`
>   request field defaulting to `0`, ignored under the Iguana policy. Under the
>   HD policy a missing `z_derivation_path` is a build error. The transparent
>   UTXO side of the coin remains iguana-derived (out of R39.6.4 §2 scope).
> - **ZOMBIE fixtures.** Test fixtures updated to carry full `protocol_data` with
>   Zcash-mainnet parameters; bare `{"type":"ZHTLC"}` is now non-conformant per
>   R39.1.2.

---

## Part C -- Shielded transaction-history RPC (T-DOC)

## 39.8 `z_coin_tx_history` method

> **Source-of-truth note.** The wire contract below (method string, request and
> response field names/types, error variants) is the externally dictated public
> RPC interface; the authoritative reference is the Komodo DeFi Framework API
> documentation for `z_coin_tx_history`. Behaviour is specified abstractly.

### 39.8.0 Shielded history is activation-owned wallet state

R39.8.0a A successfully activated shielded coin shall have an initialized
shielded-wallet history store. The store is part of ZCoin activation, not the
generic v2 UTXO history background fetcher. A terminal activation result for
ARRR / ZCoin in Light or Native mode means the wallet database exists, contains
the tracked account/viewing key, has scanned through the activation tip selected
by the sync start policy, and can be queried by `z_coin_tx_history`.

R39.8.0b Activation shall not report shielded sync as finished merely because a
Sapling commitment-tree or block-state cache has reached the backend tip. The
terminal state is valid only after compact blocks have also been validated and
applied to the shielded wallet database so received notes, note nullifiers,
spend links, witnesses, and wallet transactions are available to the history and
balance paths.

R39.8.0c The generic `my_tx_history` v2 method is not the shielded transaction
history interface. For an activated ZCoin it shall not be used to synthesize
shielded history from the generic UTXO history store; it shall reject the coin as
unsupported for that method. Shielded callers, including Desktop, shall use
`z_coin_tx_history` for ARRR/ZCoin transaction display.

R39.8.0d After a successful Light activation of ARRR, an LTC-to-ARRR swap that
pays the wallet's shielded address shall become visible through
`z_coin_tx_history` once the ARRR transaction is mined and scanned. Returning
`StorageIsNotInitialized` for that activated ARRR coin is non-conformant unless
the local wallet database is genuinely unavailable or corrupt and the coin should
not have reached a normal terminal activation state.

### 39.8.0.1 Activation trigger conditions

R39.8.0e On every `init_z_coin` activation for a ZCoin, the implementation shall
create or open two per-coin local stores before the coin is returned active:

- a compact-block cache keyed by block height, storing the serialized compact
  block data fetched from the configured shielded backend;
- a shielded wallet database keyed by coin/account state, storing scanned
  blocks, tracked account viewing keys, wallet transactions, received notes,
  sent-note metadata, and Sapling witnesses.

R39.8.0f In Light mode, activation shall use the configured Electrum servers for
the transparent backend and the configured `light_wallet_d_servers` for compact
block and shielded note scanning. In Native mode, activation shall use the native
Zcash-family backend as the compact-block source. Both modes shall feed the same
shielded wallet database contract and the same `z_coin_tx_history` data path.

> **Status update (reloaded).** The Light-mode path is implemented. Native mode
> still updates the older commitment-tree state cache but does not yet convert
> full-node blocks into the modern Reloaded compact-block cache. Unless that
> cache already covers the requested range, Native activation therefore cannot
> yet satisfy R39.8.0f/R39.8.0m--R39.8.0n and fails the complete-through-tip
> wallet-scan gate. This gap predates the stable dependency selection; the
> native compile and Windows release-build gates below are not a claim that a
> live Native-mode shielded activation passed.

R39.8.0g The sync start point (supplied via `mode.rpc_data.sync_params`; R39.6.2)
is resolved before wallet scanning:

- an explicit `{"height": N}` starts from height `N`, floored at
  `sapling_activation_height`;
- an explicit `{"date": T}` starts from the backend block height resolved for
  the Unix timestamp `T`, floored at `sapling_activation_height`;
- `"earliest"` starts from `sapling_activation_height`;
- an omitted start point may continue from existing local wallet/cache state
  when that state exists; otherwise it shall use the implementation's default
  recent-start policy, floored at `sapling_activation_height`.

R39.8.0h If a caller supplies a start point that differs from existing local
scan state, activation shall rewind or recreate the compact-block cache and
wallet database to a safe height before rescanning. If the caller explicitly
requests reuse of previous sync state and a valid previous state exists,
activation may continue from that state. The activation result shall expose the
requested start, whether the request was below Sapling activation, and the actual
start height used.

> **Status update (reloaded).** Implemented. "Existing local scan state" is
> interpreted as the wallet's **sync anchor** — one block above the earliest
> block stored in the shielded wallet database, i.e. the height the current scan
> was actually started from. On activation with an explicit `sync_params`
> (R39.6.2):
>
> - If the requested start **differs from the current anchor in either
>   direction** (earlier to gain history, or later to narrow the window), the
>   compact-block cache and wallet database are rewound/recreated, the initial
>   commitment-tree state is re-seeded from the light backend at the resolved
>   start, and the wallet is rescanned from there. This comparison is evaluated
>   **before** the "already scanned through the tip" short-circuit, so a changed
>   sync start/date is honored even when the wallet was previously fully scanned
>   (rather than reusing the stale cache).
> - If the requested start **equals the current anchor**, local state is reused
>   and scanning resumes from the tip, so an unchanged re-activation (a client
>   that re-sends the same start every launch) does not rescan history.
> - A requested start **beyond the current tip** is clamped to the tip; if that
>   still differs from the anchor it rebuilds and scans the (empty) tip window,
>   matching "sync from a future point".
>
> Because the wallet is re-seeded at `requested_start - 1` on rebuild, the anchor
> afterwards equals the requested start, so the next unchanged activation matches
> and resumes. In light mode the re-seed uses the lightwalletd `GetTreeState` at
> the resolved start height (see the R39.6.4 §3 status note). The activation
> result exposes `first_sync_block` — an object `{ requested, is_pre_sapling,
> actual }` — where `requested` is the resolved start (height, or the block
> resolved from a requested date), `is_pre_sapling` is
> `requested < sapling_activation_height`, and `actual` floors `requested` at
> `sapling_activation_height`.
>
> **Status update (reloaded).** Implemented (convergence item #4). Reloaded now
> emits `first_sync_block` **unconditionally** in the activation result. When no
> `sync_params` start was supplied, `requested` falls back to the height the
> shielded scan was actually anchored at (the wallet's seed checkpoint + 1), and
> then to Sapling activation when the wallet has no stored blocks yet.



### 39.8.0.2 Storage and schema expectations

R39.8.0i The native wallet database shall be compatible with the public Zcash
light-client wallet schema used by `zcash_client_sqlite`, including at least the
logical tables `accounts`, `blocks`, `transactions`, `received_notes`,
`sent_notes`, and `sapling_witnesses`. The schema shall preserve the database's
monotonically increasing signed transaction row identifier, because
`z_coin_tx_history` exposes that identifier as `internal_id` and accepts it in
`FromId` paging.

R39.8.0j The WASM wallet database shall preserve the same logical data model in
IndexedDB. Its table names may be namespaced for the runtime, but it shall store
the same categories of data: accounts/viewing keys, scanned blocks, wallet
transactions, received notes with value and optional spent linkage, sent-note
metadata, and Sapling witnesses. It shall also provide stable per-transaction
integer identifiers for shielded history paging.

R39.8.0k The compact-block cache shall store compact blocks by height and support
querying the latest, earliest, and ranged block data, plus rewinding to a height.
It is not a replacement for the shielded wallet database: it is only the scanned
block source from which wallet notes, transactions, nullifiers, and witnesses are
derived.

R39.8.0l The wallet database shall be initialized with the wallet's extended full
viewing key and the configured checkpoint block when available. The checkpoint's
height, hash, timestamp, and Sapling tree seed the scanned-block state so the
wallet can start from the resolved sync point instead of replaying from Sapling
activation. In Light mode, the block-ID string in the lightwalletd `TreeState`
response is in RPC display order, while compact-block `hash` and `prev_hash`
byte fields are canonical little-endian. The implementation shall validate the
display value as exactly 32 bytes and reverse it once at that protocol boundary
before using it as the compact-chain anchor.

### 39.8.0.3 Scanner behavior

R39.8.0m The shielded scanner shall run during activation and continue after
activation as the coin's background shielded sync loop. During activation it
shall report observable progress for compact-block cache update and wallet-db
building, and activation shall wait until the wallet database is scanned through
the current activation tip before returning success. Concurrent activation tasks
for the same shielded ticker shall be serialized for the complete task, including
database construction, download, scan, result calculation, and registration, so
one task cannot rebuild or rename a database while another task is using it.
Different tickers shall remain independent, and cancellation shall release the
serialization guard.

> **Status update (reloaded).** The activation portion is implemented for
> Light mode, including complete-through-tip gating, two-phase progress,
> same-ticker serialization, and the R39.6.2 pacing controls. The
> **post-activation background Light scanner is not yet wired**: the modern
> compact-block downloader and wallet scanner currently run only from
> activation, while the older background Sapling-state loop is Native-only.
> Re-activation resumes and catches up from validated local state, but that does
> not satisfy the continuous-background clause above. D39.8.0b records the
> remaining work; this upgrade does not claim full R39.8.0m conformance.

R39.8.0n The scanner shall fetch compact blocks from the configured shielded
backend, cache them by height, validate that scanned heights are sequential and
that block hashes link to the previous scanned block, and rewind/rescan on chain
continuity failures instead of accepting inconsistent wallet history. Each
bounded network batch shall be checked for its exact sequential requested range
and then persisted in one atomic SQLite transaction. A short, out-of-order,
out-of-range, malformed, or interrupted batch shall not leave a partially
persisted batch in the compact cache. When activation restarts after compact
blocks were cached but before they were scanned into the wallet, the complete
cached segment from the resolved chain-state anchor through the highest cached
height at or below the current target shall be revalidated before reuse. A
valid segment shall resume network fetching at the following height instead of
downloading those blocks again. An invalid segment shall be preserved for
diagnosis, replaced with a fresh compact cache, and refetched; its reported
maximum height alone is never sufficient evidence for reuse.

The dictated Pirate compact-block protobuf does not carry the modern optional
`ChainMetadata` field. Before handing a bounded batch to a scanner that requires
that field at its boundary, the implementation shall adapt the first block in
memory by deriving its final Sapling tree size from the trusted preceding
`ChainState` frontier size plus the number of explicit Sapling outputs in that
block. Existing metadata, stored compact-block bytes, hashes, transactions, and
outputs shall remain unchanged. The adapter shall reject numeric conversion or
addition overflow and shall not weaken the height/hash-link validation above.
Pirate has no Orchard compact actions in this dictated interface, so the
synthetic Orchard tree size is zero.

Across bounded wallet-scan batches, the implementation shall advance and retain
the exact next `ChainState` directly from the already validated compact-block
commitments. It shall not reconstruct the next batch's frontier from the
prunable wallet commitment tree: a completed rightmost subtree may legitimately
have been compressed and no longer expose the leaf-level nodes needed for exact
frontier reconstruction. After a process restart with existing scanned blocks,
the implementation shall reacquire `TreeState` at exactly the persisted scanned
height before scanning more blocks. It shall accept that refreshed frontier only
when the response height, canonical block hash, and Sapling tree size match the
wallet's persisted block metadata; a mismatch shall fail without guessing or
overwriting the wallet state.

R39.8.0o The scanner shall trial-decrypt Sapling outputs with the wallet's
incoming viewing capability, record notes belonging to tracked accounts, advance
the Sapling commitment tree, maintain incremental witnesses, track note
nullifiers, and mark a received note as spent when a later scanned transaction
spends its nullifier. These records are the source of `received_by_me`,
`spent_by_me`, and `my_balance_change`.

R39.8.0p Generated shielded transactions shall coordinate with the scanner so the
same wallet note is not selected concurrently by multiple sends or swaps. After
broadcast, the scanner shall keep following the transaction until it is either
scanned into the wallet database or no longer available from the backend. Change
outputs and outgoing spends shall become reflected in balance and
`z_coin_tx_history` only through the wallet database scan state.

R39.8.0q Unconfirmed shielded outputs are not spendable merely because the local
process created or observed them. Spendable balance, transaction history, and
post-swap Desktop display shall reflect confirmed wallet-db scan results, with
the existing locked-note/change tracking used only to prevent unsafe local
double-spends while waiting for confirmation.

### 39.8.0.4 Native persistence generations and rebuild boundary

> **Dirty-side derivation record (sanitized).** This subsection was derived by
> the KDF Spec Reader from exactly two KDF references after a targeted ref
> refresh on 2026-08-04: `v2.6.0-beta` at
> `475cdb49bc343a8fefdc2caaa1635d5ec426990b`, and the then-current `dev` head at
> `e686ef3500585f01c9f0e89c8c01bc036c42253c`. Both references resolve their
> Zcash dependency family to the public Komodo librustzcash tag `k-1.4.2`,
> revision `4e030a0f44cc17f100bf5f019563be25c5b8755f`. The requirements below carry
> only dependency identity, public API generation, observable open behaviour,
> and dictated on-disk schema. They carry no private implementation expression.
> A clean-side implementation shall not consume this subsection until the KDF
> Dirty Gate has passed it.

R39.8.0r The two reference commits shall be treated as distinct compatibility
references but **not** as distinct native wallet-schema generations. Their
resolved Zcash packages are identical:

| Package | `v2.6.0-beta` | current `dev` | Source in both references |
|---|---:|---:|---|
| `zcash_client_backend` | `0.5.0` | `0.5.0` | Git tag `k-1.4.2`, locked revision `4e030a0f44cc17f100bf5f019563be25c5b8755f` |
| `zcash_client_sqlite` | `0.3.0` | `0.3.0` | same Git tag and revision |
| `zcash_primitives` | `0.5.0` | `0.5.0` | same Git tag and revision |
| `zcash_proofs` | `0.5.0` | `0.5.0` | same Git tag and revision |
| `zcash_note_encryption` | `0.0.0` | `0.0.0` | same Git tag and revision, transitively selected |
| `zcash_extras` | `0.1.0` | `0.1.0` | same Git tag and revision |

None of these Zcash packages is sourced from crates.io, a local path, or a
vendored directory in either reference. The native SQLite binding is instead
the crates.io `rusqlite` `0.28.0` package with bundled SQLite enabled. The two
references therefore provide no evidence that either netid used a newer
librustzcash generation.

R39.8.0s A newly created native reference wallet database has exactly the six
tables below. Column order is binding for schema recognition. `NN` means an
explicit `NOT NULL` declaration; `PK` means `INTEGER PRIMARY KEY` (a rowid alias,
with no `AUTOINCREMENT` and no separate explicit `NOT NULL` declaration); a
column without either marker is nullable.

| Table | Ordered columns | Uniqueness and foreign keys |
|---|---|---|
| `accounts` | `account INTEGER PK`; `extfvk TEXT NN`; `address TEXT NN` | no additional constraint |
| `blocks` | `height INTEGER PK`; `hash BLOB NN`; `time INTEGER NN`; `sapling_tree BLOB NN` | no additional constraint |
| `transactions` | `id_tx INTEGER PK`; `txid BLOB NN`; `created TEXT`; `block INTEGER`; `tx_index INTEGER`; `expiry_height INTEGER`; `raw BLOB` | `UNIQUE(txid)`; `block` references `blocks(height)` |
| `received_notes` | `id_note INTEGER PK`; `tx INTEGER NN`; `output_index INTEGER NN`; `account INTEGER NN`; `diversifier BLOB NN`; `value INTEGER NN`; `rcm BLOB NN`; `nf BLOB NN`; `is_change INTEGER NN`; `memo BLOB`; `spent INTEGER` | `UNIQUE(nf)`; `UNIQUE(tx, output_index)`; `tx` and `spent` reference `transactions(id_tx)`; `account` references `accounts(account)` |
| `sapling_witnesses` | `id_witness INTEGER PK`; `note INTEGER NN`; `block INTEGER NN`; `witness BLOB NN` | `UNIQUE(note, block)`; `note` references `received_notes(id_note)`; `block` references `blocks(height)` |
| `sent_notes` | `id_note INTEGER PK`; `tx INTEGER NN`; `output_index INTEGER NN`; `from_account INTEGER NN`; `address TEXT NN`; `value INTEGER NN`; `memo BLOB` | `UNIQUE(tx, output_index)`; `tx` references `transactions(id_tx)`; `from_account` references `accounts(account)` |

The foreign keys use SQLite's default non-cascading, non-deferrable action.
There are no explicit named indexes, views, or triggers. SQLite supplies only
the rowid primary-key access paths and automatic indexes required by the five
single/composite `UNIQUE` constraints listed above. Automatic index names are
SQLite implementation details and shall not be used as fingerprints; their
ordered indexed columns shall be checked instead.

R39.8.0t A newly created reference wallet database has
`PRAGMA user_version = 0`. Neither reference reads, validates, increments, or
overwrites that value, and neither creates a schema-version or migration table.
An existing database with another `user_version` is therefore not rejected on
that fact alone by the references, but it is not a positive reference-created
fingerprint. Native file opens request WAL journal mode and set connection-local
synchronous mode to `NORMAL`, temporary storage to memory, and foreign-key
enforcement on. WAL sidecars and those connection settings are not schema
generation markers.

R39.8.0u The reference compact-block cache is a separate SQLite database with
exactly one table:

| Table | Ordered columns | Uniqueness and foreign keys |
|---|---|---|
| `compactblocks` | `height INTEGER PK`; `data BLOB NN` | no additional constraint |

It has no explicit indexes, foreign keys, views, triggers, migration table, or
version table, and a newly created cache has `PRAGMA user_version = 0`. A row's
`height` is the key and `data` is the serialized public compact-block payload.
The same two-column cache schema is also exposed by the stable and release-
candidate public SQLite generations discussed in R39.8.0y; it is consequently
a positive compact-cache fingerprint but **not** a wallet-generation
fingerprint.

R39.8.0v Reference wallet open is permissive rather than versioned. An absent or
empty SQLite file acquires the six tables in R39.8.0s. Existing same-named
tables are retained without a complete semantic schema check; missing tables
are added, extra tables/columns and an arbitrary `user_version` are ignored,
and failures may occur only when a later account, block, note, witness, or
transaction operation reaches an incompatible column or constraint. Schema
creation is not an atomic whole-schema migration, so a failed open can leave a
partially initialized file. A compatible populated wallet is reused; a changed
sync anchor can rewind its contents, but does not change or replace its schema.
The compact cache follows the same create-if-absent/reuse-if-operable policy for
`compactblocks`. An unreadable or malformed SQLite file causes activation or
the first affected storage operation to fail; neither reference has a separate
unknown-schema recovery policy.

R39.8.0w A conservative pre-open classifier for the deliberate rebuild policy
shall use semantic SQLite metadata, not textual comparison of stored DDL and
not the permissive set of files that the old open path might happen to tolerate.
It shall distinguish the following classes without mutating the database:

| Class | Minimum positive fingerprint |
|---|---|
| absent/empty wallet | file absent, or valid SQLite with `user_version = 0` and no user-defined tables, indexes, views, or triggers |
| reference legacy wallet | valid SQLite; `user_version = 0`; no migration/version table; exactly the six tables, ordered columns, declared types/nullability, primary keys, uniqueness constraints, foreign keys, and permitted SQLite automatic indexes of R39.8.0s |
| selected current wallet generation | valid SQLite produced by stable `zcash_client_sqlite 0.21.1`; `user_version = 8`; `schemer_migrations` having the single ordered column `id BLOB PRIMARY KEY`; exactly 48 ordered migration IDs whose canonical digest is `7b506df8a2b119fb143664a1c0b5eddfa7de5fe8458f07829818c59e5b39ddbb`; and exactly 467 canonical semantic records whose version-prefixed digest is `dfba9135b6d5ae446e88d83d162e9458730b5b098051d8565fdd91fe6be5bea8` |
| recognized compact cache | valid SQLite; `user_version = 0`; exactly the `compactblocks` table of R39.8.0u and no other user-defined schema objects |
| unknown wallet/cache | a readable SQLite schema that does not match one of the positive fingerprints above, including an exact legacy-looking schema with a nonzero `user_version`, extra user objects, altered constraints, or only a partial table set |
| corrupt/unreadable | SQLite open, integrity validation, or schema introspection fails |

`v2.6.0-beta` and the examined `dev` head deliberately map to the same
`reference legacy wallet` class; no native database fingerprint can distinguish
which of those two binaries created it. The modern stable and RC families both
use `user_version = 8` plus `schemer_migrations`, so those two facts alone do not
distinguish their final schemas; the selected release's applied migration-ID set
and schema contract must do so.

The non-mutation requirement also applies when SQLite left a rollback/WAL
sidecar after interruption. Because SQLite cannot recover a hot rollback journal
through a read-only connection, a failed direct read-only fingerprint with a
present sidecar shall be retried only against a private temporary copy of the
database and all present sidecars. SQLite may recover that copy before semantic
fingerprinting. The source database and source sidecars shall remain byte-
preserved during classification. If copy recovery or fingerprinting fails, the
source remains `corrupt/unreadable`; a recoverable copy may classify only into
one of the exact positive fingerprints above.

For the selected-current row, the migration digest is SHA-256 over each
uppercase `hex(id)` selected in bytewise `id` order, followed by `\n`. The
semantic digest is SHA-256 over `user_version:8\n`, followed by the
lexicographically sorted canonical records and `\n` after every record. Records
cover every non-`sqlite_%` table/view/index/trigger identity, `table_xinfo`,
`foreign_key_list`, and semantic `index_list`/`index_xinfo` result, plus the
ordered migration IDs. SQLite values are type-tagged and text/blob bytes are hex
encoded, so DDL whitespace, automatic-index names, locale, and display quoting
cannot change the result. A deterministic regression pins this identity; a
dependency change that alters it requires an explicit new schema decision.

R39.8.0x Classification shall happen before a modern wallet initializer is
allowed to modify the file. The observable policy is:

- current Reloaded native storage uses `<TICKER>_RELOADED_WALLET.db` and
  `<TICKER>_RELOADED_COMPACT_BLOCKS.db`;
- the legacy/GLEEC names `<TICKER>_WALLET.db` and
  `<TICKER>_COMPACT_BLOCKS.db` are a separate compatibility namespace and are
  not opened, renamed, migrated, truncated, or deleted by Reloaded;
- `<TICKER>_CACHE.db` remains the shared, backward-compatible native Sapling
  commitment-tree cache. It is not a modern wallet or compact-block database;
  Light mode may initialize its empty legacy schema when absent but does not
  use it as the source of modern shielded balance, history, or scan state;

- an absent/empty wallet is initialized in the selected current generation and
  scanned from the resolved start of R39.8.0g--R39.8.0h;
- a positively recognized reference legacy wallet is preserved recoverably,
  replaced with a new current-generation wallet, and fully rescanned; it is
  never migrated in place, and no legacy note, nullifier, witness, commitment-
  tree, transaction-row identifier, or scan-tip state is reused;
- a positively recognized current wallet opens normally under the selected
  coherent release line;
- an unknown or corrupt file is preserved and produces a typed activation
  failure; it is not deleted, overwritten, or silently treated as empty.

A recognized compact cache may supply serialized compact blocks to the rescan
only after the existing height, payload-decode, sequential-height, and hash-link
checks of R39.8.0n succeed. Its presence never changes the requirement to build
all current wallet state anew, and it never constitutes evidence that the
wallet database is current. The rebuild shall report the existing
`UpdatingBlocksCache` and `BuildingWalletDb` phases according to the work
actually performed.

R39.8.0y The public modern candidates form two coherent, non-interchangeable
dependency generations as of the reference date:

| Candidate line | Backend | SQLite | Primitives | Proofs | Note encryption | Distribution |
|---|---:|---:|---:|---:|---:|---|
| stable | `0.23.0` | `0.21.1` | `0.28.0` | `0.28.0` | compatible `0.4.1` requirement (resolving to `0.4.2` in a fresh lock) | published crates.io releases |
| newer release candidate | `0.24.0-rc.7` | `0.22.0-rc.7` | `0.30.0` | `0.30.0` | compatible `0.4.1` requirement | published RCs / matching upstream release tag |

The stable client pair requires its coherent `0.28` primitives/proofs types;
substituting the separately newer `0.30` primitives/proofs line would create a
mixed public-type generation. Both modern SQLite candidates use the migration
table described in R39.8.0w and set `user_version` to the fixed value `8`; the
value is not advanced per migration. Their initial migration recognizes the
six-table schema of R39.8.0s as its historical starting shape, so invoking the
modern initializer directly on a reference database can initiate in-place
migration. R39.8.0x therefore requires the KDF classifier to intercept that
database first. Nothing in either authorized KDF reference requires an RC-only
wallet, pool, or transaction-generation feature.

> **Selection record (2026-08-04).** The operator selected the stable line:
> `zcash_client_backend 0.23.0`, `zcash_client_sqlite 0.21.1`, and
> `zcash_primitives`/`zcash_proofs 0.28.0`. KDF consumes exact crates.io
> versions and retains only narrow, documented patches: removal of backend
> 0.23.0's obsolete exact `time-core 0.1.2` resolver pin, plus the minimum
> transparent/builder hooks required to preserve KDF P2SH input and raw-output
> transaction bytes. The old broad `librustzcash-patched/` dependency is not
> part of the selected graph.

R39.8.0z The reference scan/store generation and both modern candidates differ
at their public behavioral boundary. The reference generation resumes scanning
implicitly from the greatest stored block (or Sapling activation for a new
wallet), tracks Sapling extended full viewing keys and per-height incremental
witness state, accepts an optional block-count bound, and stores each scanned
batch through the legacy wallet-write contract. The modern stable generation
requires an explicit starting height and preceding `ChainState`, a mutable
`WalletWrite` store with unified full viewing keys and shard-tree commitment
state, an explicit bounded count, and returns a `ScanSummary`; its normal sync
orchestration also exposes prioritized scan ranges. The RC generation retains
that basic scan contract but adds observable database/API obligations for newer
shielded-pool migration, note locking, anchor retention, Ironwood/V6-era state,
and additional transaction classification. These are generation changes, not
evidence that the KDF reference contract changed: both authorized references
remain on the same legacy Sapling generation of R39.8.0r.

#### Tests

T39.8.0a A deterministic SQLite fixture for each authorized reference shall
prove the exact R39.8.0s and R39.8.0u `table_info`, `index_list`/indexed-column,
and `foreign_key_list` results, `user_version = 0`, and the absence of any
migration/version table. The two fixtures shall classify identically.

T39.8.0b Detector tests shall cover absent, empty, exact reference legacy,
selected current, extra-object, missing/altered-column, altered-constraint,
nonzero-version legacy-looking, partial, and corrupt files. Classification shall
be read-only, and unknown/corrupt fixtures shall remain byte-preserved.

T39.8.0c Opening a recognized reference legacy fixture under the upgraded KDF
shall prove recoverable preservation, creation of a new current store, a rescan
from the resolved start, accurate cache/wallet progress phases, and reconstructed
balance/history. It shall also prove that the modern dependency's in-place
migration path was not invoked on the legacy file and that no old witness or
commitment-tree state was reused.

T39.8.0d A dependency-resolution check shall reject mixed Zcash public-type
generations and shall record whether the selected build resolves the stable or
RC matrix of R39.8.0y. Scan tests shall exercise explicit chain-state/start-
height agreement, bounded progress, sequential/hash-link rejection, and durable
wallet results after reopen.

T39.8.0e A non-palindromic checkpoint hash test shall prove the single
display-order-to-little-endian conversion before first-block link validation. A
locked/hot-journal fixture shall prove recovery-based classification through a
temporary copy while the source database and sidecar stay byte-identical. A
forced mid-batch SQLite failure shall prove atomic compact-cache persistence,
and same-/different-ticker lock tests shall prove the activation serialization
scope of R39.8.0m. A reopen fixture shall prove that a complete validated cache
resumes from its highest reusable height, clamps reuse to the current target,
and rejects a discontinuous cached segment. A compact-block fixture with the
dictated Pirate wire shape and no `ChainMetadata` shall prove that the adapter
of R39.8.0n reconstructs the first-block Sapling tree size and that a received
note is scanned into the expected balance. A deterministic metadata-free
fixture shall cross a bounded-scan boundary at which the wallet tree prunes a
completed rightmost subtree and prove that the compact-source frontier continues
the next batch. The same fixture shall reopen the wallet, reject refreshed
`TreeState` with a mismatched hash or tree size, accept the matching state, and
continue scanning. A pacing fixture shall configure a one-block wallet scan
batch and a nonzero inter-batch interval, prove incremental progress at every
batch boundary, and prove the scanned height remains durable after reopen.

#### Deferred Work

D39.8.0a *(Resolved 2026-08-04.)* The operator selected the stable line and the
selected-current row of R39.8.0w now binds its exact migration and semantic
schema identities. The implementation still must not classify a database as
current from `user_version = 8` or the migration-table name alone.

D39.8.0b The modern Light compact-block and wallet scanner shall be moved into
a coin-lifetime background task after activation without weakening the existing
activation-through-tip gate. The task needs an explicit poll policy, reorg and
endpoint-failover handling, cancellation on coin disable/context shutdown, and
serialization with re-activation or database rebuild. Until this is implemented
and tested, a running Light wallet catches up only when ARRR/ZCoin is activated
again.

#### Baseline Verifications

V39.8.0a The reference commits and the locked dependency revision in
R39.8.0r shall be re-resolved before implementation begins. The native wallet-
open, compact-cache, and scan/store artifacts and the root Zcash dependency
declarations used for this derivation were byte-identical between the two
examined commits; the relevant lockfile entries also resolve the same package
versions and Git revision. A changed `dev` head requires a fresh Spec Reader
comparison rather than silently moving this bound reference.

V39.8.0b The public package manifests for the selected modern line shall be
checked together, including the backend/SQLite/primitives/proofs/note-encryption
versions, before the current-generation fingerprint of D39.8.0a is closed.

#### External references for this subsection

- KomodoPlatform `librustzcash`, public tag `k-1.4.2`, revision
  `4e030a0f44cc17f100bf5f019563be25c5b8755f` (MIT OR Apache-2.0 package
  licensing): <https://github.com/KomodoPlatform/librustzcash/tree/4e030a0f44cc17f100bf5f019563be25c5b8755f>.
- Published stable package manifests for `zcash_client_backend` `0.23.0` and
  `zcash_client_sqlite` `0.21.1`: <https://crates.io/crates/zcash_client_backend/0.23.0>
  and <https://crates.io/crates/zcash_client_sqlite/0.21.1>.
- Published release-candidate package manifests for `zcash_client_backend`
  `0.24.0-rc.7` and `zcash_client_sqlite` `0.22.0-rc.7`:
  <https://crates.io/crates/zcash_client_backend/0.24.0-rc.7> and
  <https://crates.io/crates/zcash_client_sqlite/0.22.0-rc.7>.
- SQLite schema-table and application-version interfaces:
  <https://www.sqlite.org/schematab.html> and
  <https://www.sqlite.org/pragma.html#pragma_user_version>.

### 39.8.1 Envelope, method string & platform gate

R39.8.1 The project shall expose a dedicated shielded-coin transaction-history
method with the wire method string `z_coin_tx_history`, dispatched over the
**mmrpc 2.0** envelope (`{"mmrpc":"2.0","method":"z_coin_tx_history",
"params":{...}}`, with the usual `userpass`). The result is returned in the
standard v2 `{"mmrpc":"2.0","result":{...}}` success envelope; errors use the v2
error envelope (`error`, `error_path`, `error_trace`, `error_type`,
`error_data`).

R39.8.2 The method is available on every platform where an activated ZCoin has
the shielded wallet database required by §39.8.0. It is resolved against the
activated coin named by `coin` and shall succeed only when that coin is an
activated shielded (ZCoin) coin; any other activated coin type is rejected (see
R39.8.6).

### 39.8.2 Request parameters

R39.8.3 The request `params` object shall accept the following fields (this is
the shared v2 transaction-history request envelope, specialized to an
**integer** paging identifier for the shielded coin):

| Field | JSON type | Required | Default | Notes / bounds |
|-------|-----------|----------|---------|----------------|
| `coin` | string | yes | — | Ticker of an activated shielded coin. |
| `limit` | integer (unsigned) | no | `10` | Maximum number of transaction entries to return for the page. |
| `paging_options` | object (tagged union) | no | `{ "PageNumber": 1 }` | Selects the page; see R39.8.4. |
| `target` | object (tagged union) | no | `{ "type": "iguana" }` | Shared-envelope address-scope selector; accepted and echoed back in the response. Not used to scope shielded history results. |

R39.8.4 `paging_options` is a tagged union with exactly one of two shapes:
- `{ "PageNumber": <n> }` -- 1-based page number; `<n>` is a non-zero positive
  integer. This is the default when `paging_options` is omitted (page `1`).
- `{ "FromId": <id> }` -- continue paging from the entry whose internal
  identifier is `<id>` (a signed 64-bit integer matching the `internal_id`
  field of response entries; see R39.8.5). When `FromId` is supplied the page
  begins at the entries that follow that identifier in history order.

R39.8.5 `target` is a tagged union on field `type` with values `iguana`
(default), `account_id` (carrying an `account_id` integer), and `address_id`
(carrying an HD account/address path selector). It is part of the shared
request envelope; for the shielded method it is accepted for envelope
compatibility and reflected in the response unchanged, and does not alter which
shielded transactions are returned.

### 39.8.3 Success response

R39.8.6 On success the `result` object shall carry:

| Field | JSON type | Description |
|-------|-----------|-------------|
| `coin` | string | Echo of the requested ticker. |
| `target` | object | Echo of the request `target`. |
| `current_block` | integer | Current tip height known to the coin's backend at query time. |
| `transactions` | array of objects | The page of shielded transaction detail entries (see R39.8.7). |
| `sync_status` | object | History-sync state, tagged on field `state` with optional `additional_info`. For the shielded coin this is always the terminal `Finished` state, because a shielded coin is only active after its initial scan completes (§39.3). |
| `limit` | integer | Echo of the effective page limit. |
| `skipped` | integer | Number of entries skipped ahead of this page. |
| `total` | integer | Total number of known shielded transactions. |
| `total_pages` | integer | Total page count for `total` at the effective `limit`. |
| `paging_options` | object | Echo of the effective paging selector. |

R39.8.7 Each entry in `transactions` is a **shielded-coin transaction detail**
object whose shape differs from the generic v2 history entry. Its fields are:

| Field | JSON type | Description |
|-------|-----------|-------------|
| `tx_hash` | string | Transaction hash, hexadecimal. |
| `from` | array of strings | Source address set the coins were sent from. |
| `to` | array of strings | Destination address set the coins were sent to. |
| `spent_by_me` | decimal (string/number) | Amount spent from the wallet's own address. |
| `received_by_me` | decimal | Amount received by the wallet's own address. |
| `my_balance_change` | decimal | Net balance change for the wallet (received minus spent). |
| `block_height` | integer | Block height the transaction was mined at. |
| `confirmations` | integer | Confirmation count derived from `current_block` versus `block_height`. |
| `timestamp` | integer | Transaction timestamp (Unix seconds). |
| `transaction_fee` | decimal | Fee paid by the transaction. |
| `coin` | string | Ticker the transaction belongs to. |
| `internal_id` | integer (signed 64-bit) | Stable internal identifier used for `FromId` paging (R39.8.4). |

### 39.8.4 Error conditions

R39.8.8 The method shall report failures using the v2 error envelope with an
`error_type` drawn from the following set (functional descriptions; literal
operator-facing wording is not normative):

| `error_type` | HTTP status | Condition |
|--------------|-------------|-----------|
| `CoinIsNotActive` | 404 | The named `coin` is not an activated coin. |
| `NotSupportedFor` | 400 | The named coin is activated but is not a shielded (ZCoin) coin, so shielded history is unavailable for it. |
| `InvalidTarget` | 400 | The supplied `target` selector is invalid for the coin's wallet (e.g. an HD path/chain that does not apply). |
| `StorageIsNotInitialized` | 500 | The local transaction-history store for the coin has not been initialized. |
| `StorageError` | 500 | A failure occurred reading or building the local history store. |
| `RpcError` | 500 | A backend RPC error occurred while resolving tip height or fetching verbose transaction data. |
| `Internal` | 500 | An otherwise-unclassified internal error (e.g. address resolution). |

> **Shared error surface (corpus-faithful).** `z_coin_tx_history` uses the same
> public v2 transaction-history error discriminants and HTTP status mapping as
> the generic `my_tx_history` (v2) method. The HTTP statuses in the table above
> are therefore part of the shared public error contract
> (`CoinIsNotActive` → 404; `NotSupportedFor` and `InvalidTarget` → 400;
> `StorageIsNotInitialized`, `StorageError`, `RpcError`, and `Internal` → 500).
> The **published wire contract is the `error_type` discriminant names**; the
> HTTP integers are the shared public status mapping rather than separate
> per-method values.

> **Reloaded alignment.** Reloaded's shared transaction-history HTTP status
> mapping is aligned to the upstream values recorded in the table above
> (`CoinIsNotActive` → 404; `NotSupportedFor` → 400; `StorageIsNotInitialized`,
> `StorageError`, `RpcError` → 500). The `error_type` discriminant names are the
> published contract and remain unchanged.

### 39.8.5 Functional behaviour

R39.8.9 For an activated shielded coin with an initialized wallet database,
`z_coin_tx_history` shall return one requested page of shielded transaction
details derived from wallet scan state and confirmed transaction data. Each
entry shall expose the address sets, wallet-owned spent and received amounts,
net wallet balance change, fee, block height, timestamp, confirmation count
relative to the current backend tip, and the stable internal paging identifier.
The response shall also report `skipped`, `total`, and `total_pages` metadata
consistent with the stored shielded history and the effective paging request.

R39.8.9a The history page order shall be newest mined transaction first. For
transactions in the same block, ordering shall be stable by the wallet
database's transaction identifier. `PageNumber` paging skips
`(page_number - 1) * limit` entries in that order. `FromId` paging starts after
the entry identified by the supplied `internal_id` according to that same order;
an unknown `FromId` is a storage/history error rather than a silent empty page.

R39.8.9b The `received_by_me` amount is the sum of wallet-owned received notes
whose transaction is the history entry's transaction. The `spent_by_me` amount
is the sum of wallet-owned received notes whose spent-link points to the history
entry's transaction. `my_balance_change` is `received_by_me - spent_by_me`.

R39.8.9c The `from` address set shall include transparent input addresses that
can be recovered from previous transaction outputs and shall include the wallet's
own shielded address when the wallet spent shielded notes in the transaction. A
fully shielded incoming transaction from another wallet may have an empty `from`
set because the sender's shielded address is not generally recoverable.

R39.8.9d The `to` address set shall include transparent output addresses that
can be decoded from the transaction outputs, the wallet's own shielded address
when the wallet received shielded notes in the transaction, and shielded
addresses recoverable from outgoing viewing information for wallet-created
outputs such as change, swap, fee, or burn outputs.

R39.8.9e `transaction_fee` shall be computed from the transparent input value,
transparent output value, and the Sapling value balance of the full transaction.
`confirmations` shall be zero when the transaction height is above the backend
tip; otherwise it is `current_block + 1 - block_height`.

### 39.8.6 Relationship to the generic `my_tx_history` v2 method

R39.8.10 `z_coin_tx_history` reuses the **same v2 request/response envelope
types** as the generic v2 `my_tx_history` method (the same `coin`, `limit`,
`paging_options`, `target` request fields and the same `current_block`,
`transactions`, `sync_status`, `limit`, `skipped`, `total`, `total_pages`,
`paging_options` response framing). It differs in two contract-visible ways and
is therefore a distinct method rather than a branch of `my_tx_history`:
- **Paging identifier type.** The shielded method's paging identifier
  (`FromId` and the entry `internal_id`) is a **signed 64-bit integer**, whereas
  the generic v2 method keys paging on an opaque byte-string identifier.
- **Transaction entry shape.** The shielded method returns the shielded-specific
  detail object of R39.8.7 (shielded `from`/`to` address sets, integer
  `internal_id`), rather than the generic transaction-details entry returned by
  `my_tx_history`.

> **Upstream divergence (informative).** The `target` field is part of the
> shared v2 history-request envelope; for the shielded method it is accepted and
> echoed but does not scope the returned shielded transactions. Reloaded keeps
> this envelope-compatible acceptance to preserve the dictated wire contract.

R39.8.11 `StorageIsNotInitialized` remains a valid error discriminant for
history methods when the relevant local history store is genuinely absent. For
`z_coin_tx_history`, however, a normal successful ZCoin activation initializes
the shielded wallet database. Therefore an activated ARRR/ZCoin that reached the
terminal activation state after wallet-db scanning shall not return
`StorageIsNotInitialized` from `z_coin_tx_history`; if the store cannot be
opened, the activation path shall fail or the history path shall return a
storage error that reflects the store failure.

## 39.7 Acceptance criteria (chapter)

- Baseline: a light-mode shielded coin activates via `init_z_coin`, advances
  through the documented progress states, accepts multiple lightwalletd
  endpoints, and returns `current_block` + `wallet_balance` (§39.2--§39.3).
- A shielded HTLC swap completes maker and taker legs and a refund path (§39.5).
- R39.6.3 is implemented with integrity-check behaviour enforced before prover
  initialization.
- Config-loading (R39.1.2–R39.1.4): a `{"type":"ZHTLC","protocol_data":{...}}`
  config with a well-formed `consensus_params` (plus optional `check_point_block`
  and `z_derivation_path`) deserializes successfully; a bare
  `{"type":"ZHTLC"}` (no `protocol_data`) is rejected at coin-config parse time.
- Parameter sourcing (R39.6.4, implemented): a ZHTLC coin whose `protocol_data`
  declares non-mainnet HRP/b58 prefixes, `coin_type`, or activation heights
  produces addresses/keys under those declared values and begins its shielded
  sync from the declared `check_point_block` (or `sapling_activation_height`
  when absent), not from hardcoded mainnet constants.
- R39.6.4 is implemented, including HD-derived shielded key-policy support
  (the transparent UTXO side remains iguana-derived, which is outside R39.6.4 §2).
- Shielded history activation: a Light-mode ARRR activation initializes both the
  compact-block cache and shielded wallet database, reports compact-block and
  wallet-db scan progress, and reaches terminal activation only after wallet-db
  scanning is complete through the activation tip (§39.8.0).
- ARRR post-swap display: after an LTC-to-ARRR swap pays the activated wallet's
  shielded address and the ARRR transaction is mined and scanned,
  `z_coin_tx_history` returns a transaction entry with positive
  `received_by_me` and `my_balance_change`, includes the wallet shielded address
  in `to`, reports `sync_status: Finished`, and does not return
  `StorageIsNotInitialized` (§39.8.0d, §39.8.9).
- Generic history separation: for an activated ZCoin, generic `my_tx_history`
  v2 rejects the coin as unsupported for that method; Desktop uses
  `z_coin_tx_history` for shielded history (§39.8.0c).
- `z_coin_tx_history` returns a paginated page of shielded transaction detail
  entries for an activated shielded coin, honours `limit` and both
  `PageNumber`/`FromId` paging modes, echoes paging metadata, reports
  `sync_status: Finished`, and rejects non-shielded coins (`NotSupportedFor`)
  and inactive coins (`CoinIsNotActive`) (§39.8).
