# Chapter 39 -- Zcash / z_coin Shielded Coin

**Status:** driving-spec (as-built baseline **plus** remaining
required-but-unimplemented extensions). Mixed treatment -- see §39.0.

> **One-sentence claim:** the project shall support Zcash-Sapling shielded coins
> (ARRR / ZOMBIE-style) as a first-class coin type that activates in either a
> full-node ("native") mode or a light-client mode backed by one or more
> lightwalletd gRPC endpoints as its **only** backend, drives shielded
> balance/scan through a long-running task RPC, performs atomic swaps via
> shielded HTLCs, takes every chain height from a single admitted-endpoint
> authority, and
> (by required port) gains WASM support, activation-time sync tuning, and
> integrity verification of the Sapling proving/verifying parameters.

## 39.0 Treatment & scope split

- **§39.1--§39.5 (T-DOC, as-built):** verified present in reloaded -- native-only
  ZCoin type, dual activation modes (Native / Light), multi-lightwalletd light
  mode, the Z-coin activation task RPCs (`task::enable_z_coin::*`, plus the
  baseline flat `init_z_coin*` aliases) with their progress states, shielded
  HTLC swap operations, and the activation result shape. The full activation
  wire contract (request schema, in-progress values, result, error mapping,
  HTTP statuses) is bound in §39.10 (Part D).
- **§39.6 (T-PORT, implemented):** WASM support, activation-time sync tuning /
  sync-from-date, Sapling parameter integrity verification, sourcing consensus
  parameters / checkpoint / HD path from `protocol_data`, and HD-derived
  shielded key-policy derivation are all implemented, except on WASM: the
  WASM build compiles the shielded coin but refuses its activation, because
  it has no shielded wallet database, and shielded-transaction *building*
  would also need the `LocalTxProver` parameter files it lacks (§39.6.1,
  R39.10.1 remaining deviation 1; corrected 2026-10-29). Under an HD wallet the shielded
  Sapling key is HD-derived per R39.6.4 §2; since maintainer Decision D39.5.a
  (2026-10-28) the coin's transparent secp256k1 key is HD-derived too, at the
  coins-file `derivation_path` (R39.6.4 §2, R39.5.9); implemented by
  `z_coin::transparent_secret_for_policy`.
- **§39.6.4 Ironwood (plan Step E, implemented):** the builder constructs the
  version-6 transaction format the Ironwood upgrade requires, selected by the
  coin's Ironwood activation height, which is configured or discovered at runtime
  (R39.6.4e). The swap freeze (R39.6.4b) and the build refusal (R39.6.4c) are
  therefore inert in this build. What remains unverified without a broadcast is
  stated in R39.6.4e's status note.
- **§39.11 (Part E -- implemented in part):** `lightwalletd` as the **sole**
  backend of a Light-mode shielded coin, with Electrum support for ZHTLC removed.
  **Implemented (ARRR Ironwood plan Step C, the wallet half):** the function table
  (R39.11.1--R39.11.4g) except the swap-path consumers named below, the activation
  requirements (R39.11.5--R39.11.7a, (j) staleness included), the single height authority
  (R39.11.8--R39.11.12), and the tests T39.11.a--T39.11.d and T39.11.f,
  T39.11.h--T39.11.j (T39.11.a--T39.11.d in part; each test's note says what it
  does not cover). **Implemented (plan Step D):** the swap-path parser routing
  (R39.11.16--R39.11.16a, with the carve-outs its status note names) and
  T39.11.e. **Implemented (plan Step C, the swap half):** the swap-path HTLC
  spend and already-sent detection (R39.11.4d, the swap consumer of R39.11.4b(f),
  and the named conditions of R39.11.17) and T39.11.g, in part -- each status
  note says what it does not cover; the address-index workaround of R39.11.4d is
  proved on a funded HTLC (D39.11.d). Three kinds of statement in that Part are **not**
  required changes and are marked as such where they appear: R39.11.13--R39.11.15
  describe the as-built parsing contract and the pinned dependencies' published
  version coverage; several requirements record verified facts about the
  deployed backend, the serving node and the chain's consensus rules rather than
  claims about this project's code. §39.11 carries a per-requirement status note
  drawing that boundary -- read it before treating any statement there as current
  behaviour.

> **Binding scope (R36).** Requirements bind observable behaviour, the public
> activation/task RPC surface and its JSON field names, and externally *dictated*
> interop: the Zcash Sapling protocol (shielded note/commitment-tree semantics,
> Sapling spend/output proving system), the **lightwalletd gRPC** service
> contract -- which, for a Light-mode shielded coin, is the whole of its backend
> surface, transparent side included (§39.11.1) -- and the Zcash
> transaction-format specifications its parsers implement. Sapling cryptography,
> the lightwalletd protocol and the transaction-format ZIPs are the source of
> truth, not this project's code. Private types and helper structure are
> informative.

---

## Part A -- As-built baseline (T-DOC)

## 39.1 Coin type & platform

R39.1.1 The shielded coin (`ZCoin`) is a UTXO-derived coin type that adds a
Sapling shielded layer. This was originally built on the **native** target
only; §39.6.1's required port has since made it buildable on the WASM target
as well, but a WASM build refuses its activation, because that build has no
shielded wallet database, and shielded transaction *building* stays
native-only (see §39.6.1 for the exact boundary and status, and R39.10.1 for
the activation error).

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

R39.2.1 Activation is a long-running task exposed as the public task RPCs
`task::enable_z_coin::{init,status,user_action,cancel}`; Reloaded also keeps the
baseline flat aliases `init_z_coin` / `init_z_coin_status` /
`init_z_coin_user_action`. Routing, request schema and error contract are bound
in §39.10.

R39.2.2 The activation request carries a `mode` object (a tagged union with tag
field `rpc` and payload field `rpc_data`) selecting one of:
- **Native** -- talks to a full Zcash-family node; no `rpc_data` payload.
- **Light** -- a light client whose `rpc_data` object carries
  `light_wallet_d_servers` (a **list** of lightwalletd gRPC endpoints) and
  `electrum_servers`. The **same `rpc_data` object** is also where the optional
  shielded **sync starting point** `sync_params` (and its companion
  `skip_sync_params` boolean) live in the dictated wire (R39.6.2) — **not** at
  the top level of the activation params.
  `light_wallet_d_servers` is **mandatory and non-empty** for Light mode and is
  the coin's only backend; `electrum_servers` is **accepted and ignored**, and
  Electrum support for a ZHTLC coin is removed. See §39.11.2 for what activation
  requires of the backend, and §39.11.1 for the functions it serves.

> **Status update (reloaded, Step C).** Implemented. `light_wallet_d_servers` is
> mandatory and non-empty (an empty list fails activation as a
> `CoinCreationError` naming the coin, before any shielded work), and
> `electrum_servers`, `min_connected` and `max_connected` are accepted, may be
> absent or empty, and are ignored. See R39.11.5 and R39.11.7.

R39.2.3 The request also carries optional `required_confirmations` and
`requires_notarization` fields (top-level on the activation params). Sync-tuning
fields that sit at the top level (`scan_blocks_per_iteration`, `scan_interval_ms`,
`zcash_params_path`) are specified in R39.6.2.

R39.2.4 The light mode shall accept **more than one** lightwalletd endpoint so a
deployment can list several servers.

## 39.3 Activation progress, Trezor & result

R39.3.1 The activation task shall report progress through observable in-progress
states covering at least: activating the coin, scanning the shielded chain,
and requesting the wallet balance. On the dictated public task-status wire these
serialize as `ActivatingCoin`, the two scan phases `UpdatingBlocksCache` and
`BuildingWalletDb` (each carrying `current_scanned_block` and `latest_block`
unsigned counters), and `RequestingWalletBalance`, plus the retriable-failure
value `TemporaryError` (R39.10.7). The further vocabulary names `Finishing` and
the hardware-wallet states of R39.3.2 (`WaitingForTrezorToConnect`,
`WaitingForUserToConfirmPubkey`) are never entered by a Z-coin activation
(R39.10.8). These variant names are part of the externally observable task-RPC
surface. (Verified identical between the `v2.6.0-beta` and `v3.0.0-beta`
references.)

> **Status update (reloaded).** Implemented (convergence item #1). Activation
> reports the two dictated scan phases with live progress: `UpdatingBlocksCache`
> during the lightwalletd compact-block download and `BuildingWalletDb` during
> the wallet-database scan, each carrying `current_scanned_block` and
> `latest_block` unsigned counters. The wallet-DB scan runs in bounded batches
> so `current_scanned_block` advances incrementally.

R39.3.2 The hardware-wallet status names of R39.3.1
(`WaitingForTrezorToConnect`, `WaitingForUserToConfirmPubkey`) are part of the
status vocabulary, but the shielded coin does not support hardware-backed or
WalletConnect key policies: a Z-coin activation never enters these states,
never reports `UserActionRequired`, and a `user_action` sent to a live Z-coin
task fails as bound in R39.10.8. The retriable-failure in-progress value
`TemporaryError` and the remaining wire details are bound in R39.10.7.

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
the swap protocol. Because the HTLC itself is a transparent P2SH output on a
shielded coin, several of these steps are answered by the coin's backend's
transparent surface and read the coin's transaction bytes through the shared UTXO
parser; which backend answers them is §39.11.1, and the parser split and
version-coverage requirement are §39.11.4.

R39.5.2 *Trading floors.* A shielded coin's minimum trading volume and
minimum transaction amount are the UTXO values of
[Chapter 38](38-utxo-coin-maintenance-and-features.md) R38.6.6d–e, computed
from the coin entry's transparent `txfee`, `dingo_fee` and `dust` fields,
in both reference designations (`v2.6.0-beta` and `v3.0.0-beta` agree).
The published ARRR entry declares neither `txfee` nor `dust`, so it takes
the fixed default of 1,000 base units per kilobyte and the default dust of
1,000 base units: minimum trading volume 0.0001 ARRR, minimum transaction
amount 0.00001 ARRR. The fee a shielded HTLC spend deducts is a fixed
1,000 base units in both trees, so a payment at the floor is always
spendable. Reloaded matches. The cross-family consumption and refusal
contract is [Chapter 60](60-trading-floors-and-refusal-contract.md)
(row Z1). *(Spec Reader addition 2026-10-25; cleared by the Dirty Gate
with chapter 60 on 2026-10-25.)*

R39.5.3 *Orderbook display of a shielded coin's orders.* An order whose
coin is a shielded (`ZHTLC`-protocol) coin has no transparent address to
show in the `orderbook` and `best_orders` responses: the entry's
`address` is rendered as the shielded outcome bound by
[Chapter 32](32-orderbook-p2p-and-trie.md) R-F6b -- the
`{"address_type":"Shielded"}` object on mmrpc 2.0 and the fixed string
`Shielded` on the legacy generation (ch. 32 R-F6d, R-F6e) -- and the
order MUST appear in both generations of both methods; a shielded-coin
order never makes an orderbook response fail or go missing. *(Spec
Reader addition 2026-10-26; cross-reference only, the binding rules and
their clearance record live in chapter 32.)*

R39.5.4 *Recovery uses the swap's own HTLC key.* A shielded coin's HTLC key
pair is generated for each swap and saved with the swap; it is not the
coin's persistent key. Every later step that rebuilds the swap's HTLC
script — searching for the spend of this node's own payment, refunding it,
and `recover_funds_of_swap` on either the maker or the taker side — shall
try the HTLC public key this node negotiated for that swap first, read
from the swap's saved record. A saved swap that carries no negotiated key
falls back to the saved key pair, as the swap restore does
([Chapter 51](51-legacy-v1-swap-state-machine.md) R63). If that key does not
rebuild the script the payment pays, recovery shall try the coin's
persistent key next: a swap saved by an older build, or by another build,
may have negotiated with it. The first key that rebuilds the paid script is
used for the rest of the recovery. A key here is a key pair: the private key
that refunds or spends is the one that belongs to the public key that rebuilt
the script. When the private key saved with the swap does not belong to its
saved public key, the coin's persistent key pair is used if its public key is
that key. Otherwise recovery stops with an error naming the public key for which
no private key is held, before anything is built. "Belongs to" means the public key
the coin derives from the pair for its HTLC: the pair's own public key on a
secp256k1 coin, but a different key on a coin whose HTLC field uses another curve.
A saved public key with no private key held is still tried against the payment;
when it does not rebuild the paid script, the persistent key is tried next as
usual. Which key matched, "per-swap key" or
"persistent key", is stated in a log line, and in the error text whenever
recovery stops with an error (for example "refunded already by <txid>"). The
response of a successful `recover_funds_of_swap` keeps its shape: no field is
added, so the method stays wire-compatible with the reference. With a
matching key:

- a payment already refunded is reported as refunded, naming the refund
  transaction;
- a payment already spent by the counterparty yields its secret and the
  swap continues as for any coin;
- an unspent payment past its lock time is refunded.

A payment whose output pays neither rebuilt script is still a hard stop
(it is not this swap's payment, or the record is inconsistent). The error
names the payment by its transaction id in the usual display byte order and
says that both keys were tried.

> **Provenance and reason (2026-10-26).** Clean-room original; this is a
> Reloaded defect, not a reference question. Reloaded's shielded spend
> search rebuilt the script with the persistent key, so recovery of every
> shielded swap stopped with "does not pay the HTLC script this swap
> negotiated". Observed on a September taker swap that had in fact been
> refunded on chain; the message also printed the transaction id byte-reversed.

T39.5.4 *Recovery with a per-swap key.* For a shielded taker payment and a
shielded maker payment built with a per-swap HTLC key different from the
persistent key, tests shall show that recovery:

- reports "refunded already" with the refund transaction id once the
  payment has been refunded;
- refunds an unspent payment past its lock time;
- recovers the secret from a counterparty spend.

The same holds for a payment built with the persistent key, which matches on
the fallback; the log line and any error text name the key that matched,
and a successful response has the same fields as before. A payment of
another swap still hits the hard stop; its message prints the transaction id
in display order and says both keys were tried. A UTXO-coin case shows
recovery unchanged there.

A swap record whose saved HTLC public key is not the public key of its saved
private key is refunded with the persistent key pair when the saved public key is
the persistent one. When it is neither, recovery stops with the named "no private
key" error; this is checked on the real payment of a September swap, whose refund
key is `032d2807…`.

> **Amendment (2026-10-28).** Clean-room original. A maker swap saved by
> another build carried an HTLC public key that was not the public key of the
> private key saved with it. Recovery matched the public key and then built the
> refund with the saved private key, so the redeem script no longer matched the
> payment, and the transaction builder rejected it as "Invalid address".

R39.5.5 *Recovery returns within a bound and names its stage.*
`recover_funds_of_swap` shall return within 10 minutes, for every coin and
on both the maker and the taker side. Recovery keeps track of the stage it is
in and logs one line at `info` as each stage starts, naming the coin and the
payment. The stages are:

- resolving the HTLC key for a payment (R39.5.4);
- checking whether this node's own payment was sent;
- searching for a spend of a payment;
- spending the counterparty's payment;
- refunding this node's payment.

When the bound is reached, the work in flight is dropped and the call
returns an error naming the stage and the coin. If the stage was a shielded
coin's HTLC detection, the error also names the endpoint and address form
that detection was reading last (R39.11.4h). If the stage was a spend or a
refund, the error also says that the transaction may already have been
broadcast, so the chain has to be checked before the outcome is assumed.

A shielded coin's HTLC spend and refund log their own steps at `info`: the
builder's target height, the build, and the broadcast with the transaction
id. Shielded spend detection is bounded per endpoint by R39.11.4h, so on a
shielded coin this bound is a backstop. The response of a successful call
keeps its shape (R39.5.4).

> **Provenance and reason (2026-10-28).** Clean-room original; this is a
> Reloaded defect, not a reference question. A field test of R39.5.4
> recovered a September shielded swap: the key-match line was logged, and
> then the call never returned while the process sat idle. Nothing after the
> key match had a total bound or progress output, so there was no way to
> tell which stage was waiting.

T39.5.5 *Bounded recovery.* With a deliberately short bound, tests shall show
that:

- a recovery whose spend search never completes returns the error naming
  the search stage and the coin; on a shielded coin it also names the
  endpoint and address form read last;
- a recovery that stalls while refunding names the refund stage and says the
  transaction may already have been broadcast;
- a recovery that completes within the bound returns the same response as
  before.

> **Candidate order with a derived key (amendment to R39.5.4, 2026-10-28).** Once a
> shielded coin's HTLC key is derived as R39.5.6--R39.5.9 bind, recovery tries, in order:
> (1) the key re-derived from the saved Started event (R39.5.11, parity); (2) a private key
> saved with the swap that belongs to the saved HTLC public key, which recovers swaps a
> Reloaded build made with a random per-swap key before the derivation; (3) the persistent
> key. The named "no private key held" stop of R39.5.4 is kept as a Reloaded divergence
> from R39.5.11(d), which has no such error class. It is reported only when a public key
> that matched the payment has no private key among the candidates, and the swap wire is
> unchanged.

> **Derivation record (sanitized).** R39.5.6--R39.5.12, the code-quality finding and
> T39.5.a--T39.5.e were authored by the KDF Spec Reader under the two-team procedure
> (chapter 01 §1.4A) from the `v2.6.0-beta` and `v3.0.0-beta` designations, which agree
> on everything stated here. The KDF Dirty Gate passed them on 2026-10-28 (PASS, measure
> 0.01; the clearance item in §39.9 records the extent). The gate's candidate numbered
> them R39.5.2--R39.5.9; they are placed here as R39.5.6--R39.5.13 because R39.5.2--R39.5.5
> were already taken (gated R39.5.*n* = chapter R39.5.*n*+4). R39.5.13, D39.5.a, T39.5.f
> and the informative pre-check under R39.5.11 are original to this design.

R39.5.6 **Per-swap HTLC key of a shielded coin.** *(binding, parity.)* For every legacy (v1)
swap in which a shielded coin is the maker coin or the taker coin, the shielded leg's HTLC
key pair — the key whose 33-byte compressed public key is negotiated as that leg's
`maker_coin_htlc_pub` / `taker_coin_htlc_pub` (ch. 51 R59 shape 3, R63) and saved as
`maker_coin_htlc_pubkey` / `taker_coin_htlc_pubkey` in the Started event — shall be
**derived deterministically** from (a) the coin's activated transparent secp256k1 private
key and (b) the swap-unique input of R39.5.7, by the algorithm of R39.5.8. It shall not be
the node's persistent key, shall not be random, and shall not be persisted anywhere: every
later use (payment, spend, refund, already-sent check, spend search, recovery) re-derives
it from the same two inputs.

R39.5.7 **Swap-unique input.** *(binding, parity.)* The swap-unique input is role-dependent:
- when this node is the **maker**, the swap's secret-hash bytes exactly as negotiated (the
  Started event's `secret_hash`; when a saved swap predates that member, the hash of the saved
  `secret` under the swap's secret-hash algorithm of ch. 51 §51.9.1);
- when this node is the **taker**, the 16 raw bytes of the swap `uuid`.
The same input is used for both legs of the swap; a non-shielded UTXO leg ignores it
(its HTLC key is the activated key itself, ch. 51 R63 note), which is why the two legs'
HTLC public keys differ exactly when one leg is shielded.

R39.5.8 **Derivation algorithm.** *(binding, parity — cryptographic derivation, must be bit-exact for seed-compatible
recovery.)* Given base private key *d* (secp256k1 scalar) and
swap-unique input *u*:
1. *m* = SHA-256(SHA-256(*u*)).
2. *s* = DER-encoded ECDSA signature of *m* under *d* with the deterministic nonce of
   RFC 6979 and low-S normalisation (the secp256k1 library default).
3. *h* = SHA-256(SHA-256(*s*)) as 32 bytes.
4. *k* = *h* with byte 0 cleared of its three least significant bits, byte 31 cleared of its
   most significant bit and set in bit 6 (the Curve25519-style clamp; it yields a non-zero
   scalar below the secp256k1 group order).
5. HTLC private key = *k*; HTLC public key = compressed point of *k* (33 bytes).

These interop facts make the derivation reproducible bit for bit, and both reference
states agree on each:

- (i) the ECDSA signature uses the RFC 6979 deterministic nonce with **no** extra entropy
  and **no** low-R grinding, low-S normalisation, and bare DER encoding;
- (ii) *m*, the double SHA-256 of *u*, is the 32-byte ECDSA message as it is, with no
  signed-message prefix;
- (iii) the DER signature bytes are hashed as they are, with no sighash byte appended;
- (iv) the clamp works on the digest in its natural order: byte 0 is the first digest
  byte (`&= 0xF8`), byte 31 the last (`&= 0x7F`, `|= 0x40`); the 32 bytes are then read
  as a **big-endian** scalar, with no reversal. "Curve25519-style" names the bit
  pattern only; it does not mean little-endian;
- (v) the taker's input is the 16 raw UUID bytes in canonical RFC 4122 order (the
  hyphen-less hex of the uuid), not a mixed-endian GUID layout;
- (vi) the maker's input is the decoded secret-hash bytes as saved (20 bytes), not the
  hex string; when the Started event has no secret-hash member, it is the hash of the
  saved secret.

R39.5.9 **Base key selection.** *(binding, parity.)* The base private key *d* of R39.5.8 is the
shielded coin's activated transparent secp256k1 key under the node's global private-key
policy: under the seed (iguana) policy the seed's secp256k1 key — the key behind
`my_persistent_pub`; under the HD-wallet policy the key the UTXO builder activates for the
coin at the coins-file `derivation_path` with the default account/change/index of the
activation request. The Light-mode activation request (§39.10) carries no member that
influences *d*; its `account` member selects only the Sapling spending-key account.
Hardware-wallet policies are refused for this coin type.

Under the HD-wallet policy this rests on the transparent derivation of R39.6.4 §2, adopted by
maintainer Decision D39.5.a (2026-10-28).

R39.5.10 **Where the key is used.** *(binding, parity.)* The derived key enters:
- the payment: only as this node's key inside the HTLC redeem script whose P2SH is output 0
  of the payment (output 1 is an OP_RETURN carrying the redeem script); the payment has no
  transparent input and no transparent signature;
- the spend and the refund: as the signer of the single transparent input (the payment's
  output 0) and, with the counterparty's saved HTLC key, the locktime and the secret hash,
  as a component of the rebuilt redeem script;
- the already-sent check and both spend searches (R39.5.11).
The DEX-fee transaction is fully shielded and involves no secp256k1 key; the swap `uuid` is
carried as the memo of each fee output.

R39.5.11 **Recovery obtains the key by re-derivation.** *(binding, parity.)* `recover_funds_of_swap`
and any restart-time resumption shall obtain the shielded leg's HTLC private key only by
re-applying R39.5.7–R39.5.9 to the saved Started event. Consequently:
(a) both coins of the swap must be activated at the time of the call; otherwise the call fails
    with a per-coin "not activated" class error;
(b) the swap must be finished, its own payment not already refunded, and the counterparty's
    payment not already spent-and-confirmed; each violation is a distinct error. On the taker
    side the counterparty's payment must also be saved;
(c) when the own payment transaction is not saved, it is searched through the coin's
    already-sent check using the P2SH address built from the *derived* key — with a key that no
    longer matches, the search finds nothing and recovery fails with a "payment not found"
    class error;
(d) the spend search shall rebuild the expected P2SH output script from (locktime, secret
    hash, derived own key, saved counterparty key) and compare it with output 0 of the saved
    payment; a mismatch is an error (script-mismatch class) and recovery stops there. There is
    **no** separate "saved public key does not match a held key" check and no such error class;
    a base-key mismatch is observable only as (c) or (d);
(e) on success the response is `{"result":{"action":"RefundedMyPayment"|"SpentOtherPayment",
    "coin":<ticker>,"tx_hash":<hex>,"tx_hex":<hex>}}`; on a broadcast failure the built
    transaction is still published on the swap P2P transaction-helper topic and the node's
    rejection text is returned as the error.
> **Reloaded-original (informative, proposed).** Reloaded may add an explicit pre-check that
> the re-derived public key equals the saved `*_coin_htlc_pubkey` and report a dedicated
> error naming the mismatch before any chain query. This is additive and does not change the
> parity behaviour above when the keys match.

> **Status update (reloaded, 2026-10-28).** R39.5.6--R39.5.11 are implemented, with the HD
> base key of R39.6.4 ("Transparent key under HD").
> - *Derivation:* `coins::z_coin::derive_swap_htlc_key_pair` (R39.5.8), from the coin's
>   activated transparent key (`ZCoin::derived_htlc_key_pair`, through
>   `SwapOps::derive_swap_htlc_key_pair`). The maker's input is the saved secret-hash bytes
>   (`maker_htlc_key_input`), the taker's the 16 raw uuid bytes.
> - *Never saved:* `start_htlc_key_pair` writes no private key for the shielded leg. The key is
>   derived again on restart (`restored_htlc_key_pair`, in `apply_event(Started)`) and in
>   recovery (`recovery_key_pairs`, `resolve_recovery_htlc_key`) in the candidate order of the
>   R39.5.4 amendment: derived, saved pair, persistent. A swap saved by an earlier Reloaded
>   build keeps its saved pair when that pair belongs to the saved public key. Key resolution
>   reads nothing from the network.
> - *Pre-check:* `check_held_htlc_key`. It applies to **resumption only**: a kick-started
>   swap whose re-derived key is not its saved public key is not resumed, and the log names
>   the swap and the coin. In `recover_funds_of_swap` it only logs a warning, so the parity
>   outcomes of (c) and (d) and T39.5.c stay as stated.
> - *Tests:* T39.5.a passes on the reference vector, plus two low-S cases computed from the
>   rule; T39.5.b and T39.5.c pass, plus in-flight, candidate-order and policy-switch cases.

R39.5.12 **Refund gate and refund transaction of a shielded leg.** *(binding, parity.)*
(a) The refund of a shielded leg becomes permitted when the node's wall clock exceeds the HTLC
    locktime; until then the refund stage waits (locktime − now + 1) seconds and re-checks, and
    on a check error retries after 30 seconds. No median-time-past or block-count condition is
    consulted (contrast: the UTXO coins' gate also requires locktime < median-time-past and
    bounds a single wait to 3600 s).
(b) The refund transaction: nLockTime = the HTLC locktime exactly; one transparent input
    spending the payment's output 0 with sequence 0xFFFFFFFE and the refund branch of the
    redeem script; one Sapling output to the wallet's own shielded address of value HTLC value
    minus a fixed 1000 zatoshi; expiry height = chain height at build time + 20 blocks.
    The secret-spend differs only by sequence 0xFFFFFFFF and the secret branch.
(c) A broadcast rejection is **terminal for the swap**: the built transaction is published on
    the P2P transaction-helper topic, the leg's refund-failed event is recorded with the node's
    text, and the swap finishes; it stays `recoverable`, and a later `recover_funds_of_swap`
    builds a fresh refund (fresh expiry) under the same gate.
(d) Event sequence (maker leg; taker mirrors): `MakerPaymentWaitRefundStarted` →
    `MakerPaymentRefundStarted` → gate (a) → broadcast → `MakerPaymentRefunded` →
    `MakerPaymentRefundFinished`; or … → `MakerPaymentRefundFailed` → finish.
> **Code-quality finding (informative).** Zcash-family nodes judge a time-locked transaction
> final against median-time-past (BIP 113 semantics), which lags the wall clock by several
> block intervals. Under (a)+(b) the first refund broadcast is therefore expected to be rejected
> as non-final on every chain, and the swap ends in the failed state of (c) although nothing
> is wrong with the funds; a manual recovery attempted soon after may fail for the same reason.
> **Reloaded change: R39.5.13 (maintainer Decision AP).**

R39.5.13 **Median-time-past-aware shielded refund.** *(binding, Reloaded-original;
maintainer Decision AP, 2026-10-28; implemented, see the status update below. Supersedes R39.5.12(a) and (c) for
this project while keeping (b) and (d) wire-identical.)* The shielded
leg's refund gate shall require both that the wall clock has passed the HTLC locktime and
that the chain's median-time-past (computed from the `time` field of the last 11 compact
blocks served by the admitted `lightwalletd` endpoint of §39.11, or from the serving node's
headers in Native mode) exceeds the HTLC locktime, bounding any single wait to 3600 s. A
broadcast rejection whose node text indicates non-finality shall be retried on the next gate
pass rather than terminating the swap, until the leg's refund deadline (locktime + 3700 s,
ch. 51 R18/R30); other rejections keep R39.5.12(c). `recover_funds_of_swap` shall apply the
same gate and report a "too early" error naming the earliest permitted instant.

> **Status update (reloaded, 2026-10-28).** Implemented. The gate is `z_refund_gate::refund_gate`,
> reached through `SwapOps::can_refund_htlc` of the shielded coin (`z_swap_ops.rs`): wall clock first
> (no chain read while it has not passed the lock time), then the median time past of
> `get_current_mtp` (the admitted backend's tip and its compact-block times, R39.11.4f and R39.11.8),
> every wait capped at 3600 s through the UTXO coins' `capped_refund_wait`, a read failure an `Err`.
> The retry is the coin's `SwapOps::retriable_refund_rejection` (the shielded coin answers for a
> rejection text containing "non-final", any case; every other coin keeps the default `None`) used by
> `refund_maker_payment` and `refund_taker_payment` through `retried_refund_rejection` until
> locktime + 3700 s; a retried rejection is logged once at warn level and not published on the P2P
> transaction-helper topic. Recovery asks `SwapOps::refund_gate_closed_note` after its own deadline
> check, in the stage "checking whether the chain allows the refund yet"; the default answers `None`
> without a read. The Native path (refused at activation in this release) reads its node's headers
> through the same `get_current_mtp`; WASM, which cannot build a shielded swap, is unchanged.

> **Divergence (informative).** Against the reference behavior of R39.5.12, three things
> change: the refund of a shielded leg is broadcast once median time past, not only the wall
> clock, has passed the locktime; a refund rejected as non-final is retried instead of ending
> the swap in the refund-failed state; and `recover_funds_of_swap` called too early reports
> "too early" instead of the node's rejection. The refund transaction, the event types and
> the swap wire are unchanged, so a counterparty sees only when the refund appears. The
> operator-facing entry is in `docs/GLEEC_COMPATIBILITY.md`, "ZHTLC backend".

Tests for R39.5.6--R39.5.13:

T39.5.a *Derivation vector.* *(parity.)* For a fixed base private key and the two swap-unique
inputs (a 32-byte secret hash; a 16-byte UUID), the derived public key equals a vector taken
from a swap file produced by the reference (2.6.0-beta) with the same seed (a throwaway,
non-HD test wallet; until that vector exists the test runs on public Started fields as a
structure check only, and the derivation code is not merged before it passes); the derived key
for the UTXO leg equals `my_persistent_pub`, the shielded leg's does not.
T39.5.b *No persistence.* *(parity.)* A saved swap contains no HTLC private key; the
shielded leg's key is re-derived on restart and after re-activation and signs a valid refund.
> **Amendment (2026-10-28) to T39.5.a.** "A 32-byte secret hash" means the secret-hash bytes
> as saved: 20 bytes under the default algorithm (the reference vector's case), 32 when
> ch. 51 R71/R72 selects SHA-256.

> **Amendment (2026-10-28) to T39.5.c and the R39.5.4 candidate order.** T39.5.c concerns a
> swap record that saved no HTLC private key, which every derived key is. Such a record has
> no match-only candidate, so with a different seed recovery ends in the script-mismatch or
> not-found class, as T39.5.c says. The named "no private key is held" stop of R39.5.4
> applies only when the record saved a private key that does not belong to the saved
> public key.

T39.5.c *Mismatch outcomes.* *(parity.)* With a different seed, recovery of a saved shielded
swap fails with the script-mismatch class when the payment is saved and with the
not-found class when it is not; no other error class is produced.
T39.5.d *Refund shape.* *(parity.)* The refund has nLockTime = locktime, input sequence
0xFFFFFFFE, expiry = height + 20, output value = HTLC value − 1000 zatoshi, to the own
shielded address.
T39.5.e *Refund-failed is terminal and recoverable.* *(parity.)* A rejected broadcast yields
the refund-failed event, finish, `recoverable` = true, and a later recovery builds a fresh
transaction.
T39.5.f *MTP gate.* *(Reloaded.)* Under R39.5.13 the refund is not broadcast while
median-time-past ≤ locktime even though wall clock > locktime; a simulated non-final
rejection is retried, a script-failure rejection is terminal, and a non-final rejection
after the leg's refund deadline is terminal as in R39.5.12(c). `recover_funds_of_swap`
before the gate opens reports "too early" with the earliest permitted instant and
broadcasts nothing.

#### Deferred Work (§39.5)

D39.5.a **Decided 2026-10-28 (maintainer): follow the reference.** Under the HD-wallet policy
the shielded coin's transparent key, and with it the base key of R39.5.9, is the key at the
coins-file `derivation_path` with the default account, change and address index; R39.6.4 §2
binds it, with no divergence and no operator-facing exception. Under the seed policy nothing
changes. A swap saved by an earlier Reloaded build with a random per-swap key keeps that saved
key (R39.5.4, candidate (2)). The question as it was put:

**HD-wallet base key for the shielded HTLC key.** §39.0 and §39.6 record that Reloaded derives the transparent
side of a shielded coin from the seed key even under an HD wallet. That satisfies R39.5.9
only for seed-policy nodes. For HD-policy nodes the reference's base key differs, so a swap
started by the reference under HD cannot be recovered by Reloaded with the same seed (the
script-mismatch outcome of R39.5.11(d)) and vice versa. Either R39.6.4 §2 is extended to
HD-derive the transparent key as the reference does, or the divergence is recorded as a
deliberate Reloaded exception in §39.0 and in the operator-facing compatibility document.

D39.5.b **A dedicated build error for the transparent key under HD** *(after the beta;
cosmetic; 2026-10-29).* Since D39.5.a, a failure to derive the shielded coin's transparent
key under the HD-wallet policy (the crypto context unavailable, or the coins-file
`derivation_path` absent or invalid) is reported through the build error of the shielded
key, whose text begins "HD shielded key derivation failed". The key that failed is the
transparent one, so the prefix misleads; in the coins-file cases the detail after it names
`derivation_path`, so the cause stays clear. A dedicated error, or a prefix that names no key kind, changes only the
text of `error_data.error`: the activation still ends in `CoinCreationError` `{ticker,
error}` (R39.10.12), and no behavior changes.

---

## Part B -- Required ports (T-PORT)

> **Status of Part B:** R39.6.1, R39.6.2, R39.6.3, and R39.6.4 are all
> implemented, including HD-derived shielded key-policy support (R39.6.4 §2).
> The transparent (UTXO) side of a shielded coin remains iguana-derived; only
> the shielded Sapling spending key is HD-derived, as R39.6.4 §2 required until
> 2026-10-28. Maintainer Decision D39.5.a extended R39.6.4 §2 to the transparent
> key under HD ("Transparent key under HD"), implemented on 2026-10-28.

## 39.6 Required shielded-coin ports

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
> targets. Shielded transaction *building* remains native-only, because the
> Sapling local prover (and the parameter files it needs) is unavailable in WASM.
>
> **Status update (2026-10-29).** The note above is out of date on activation, and
> the acceptance of R39.6.1 is not met. Since 2026-10-11 a WASM build has refused
> every shielded activation with `CoinCreationError` `{ticker, error}`, stating that
> shielded activation is not available in the WebAssembly build (R39.10.1, remaining
> deviation 1). That build has no shielded wallet database, so it has no real balance
> to report, and the coin is never built or registered there. Shielded transaction
> building would also stay native-only, as above. This is a known limitation of the
> beta, listed in the release notes; shielded coins in WASM are deferred (D39.6.a).

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

R39.6.4c A shielded coin that declares `ironwood_activation_time` and is built
without the ability to construct Ironwood-era transactions shall refuse to build
**any** transaction from that activation time onwards, with an error naming the
coin, the activation time, and the need to upgrade.

The refusal shall reach callers as an *unsupported operation*, not as an internal error: nothing has failed inside the process, and the remedy is the user's. `WithdrawError::UnsupportedAfterNetworkUpgrade` carries it and maps to HTTP 400, following R47.6.7/R50.20 for the MetaMask and Trezor policies.

The refusal shall be applied at the single point every shielded transaction is
constructed, so that it covers withdrawals as well as swap payments: a
withdrawal passes none of the swap gates of R39.6.4b, so without this it would
still build a transaction the network no longer accepts and fail only at
broadcast, as an opaque rejection.

It shall be evaluated before any blocking wait in that path, so that a caller is
refused promptly rather than made to wait first and refused second.

This cut-off is **later** than the swap freeze of R39.6.4b and the two shall not
be merged. The freeze starts earlier and stops only the entering of new swaps;
between the two, a swap begun before the freeze must still be able to spend or
refund itself, which requires building transactions. A build refusal starting at
the freeze would strand exactly the swaps the freeze exists to protect.

Receiving, balance, address derivation and history shall remain unaffected in
both windows.

> **Status update (reloaded, Step E).** This build can construct Ironwood-era
> transactions (R39.6.4e), so this refusal never engages in it. The rule and its
> tests are kept for a build that cannot.

R39.6.4b A shielded coin that declares `ironwood_activation_time` and is built
without the ability to construct Ironwood-era transactions shall refuse to enter
**new** swaps from a cut-off preceding that activation, and shall report itself
as wallet-only for the purposes of order placement.

The cut-off shall precede activation by at least the longest time a swap payment
can remain unspendable-and-unrefunded: the longest maker payment lock this
framework produces, plus the refund grace the swap machines add before acting,
plus an allowance for the refund to be mined. A payment funded at the last
tradeable instant must therefore still be refundable, and confirmed, before
activation.

The refusal shall be applied at **both** of:

- the locally initiated order paths, via the coin's wallet-only report, which
  `buy`, `sell` and `setprice` already gate on; and
- the incoming peer-match predicate, which the wallet-only report does not
  reach — a remote taker can otherwise match an order already posted and pull
  the coin into a new swap regardless.

The refusal shall not affect balance, address derivation, withdrawal, history,
or any swap already in progress: nothing re-checks these once a swap has begun,
and a swap already under way must be allowed to finish or refund normally.

A coin that declares no Ironwood activation time shall never be frozen, and the
freeze shall lift entirely once the build can construct Ironwood-era
transactions. "Can construct Ironwood-era transactions" means more than the
builder emitting them: every swap step that *reads* the coin's transaction bytes
must also understand that version, per R39.11.16. A build whose builder can emit
the new version while its swap-path parsers cannot read it shall remain frozen.

R39.6.4a `consensus_params` may additionally carry two optional Ironwood
network-upgrade members, both defaulting to absent:

- `ironwood_activation_time` -- the wall-clock timestamp (Unix seconds) from
  which the coin's Ironwood upgrade activates;
- `ironwood_activation_height` -- the activation height, once the network has
  derived and published it.

Both are optional in both directions: a configuration omitting them shall parse
on a build that understands them, and a configuration carrying them shall parse
on a build that does not (this payload is deliberately not
`deny_unknown_fields`, per R36.3.1/R36.3.3).

> **Compatibility:** no published coin configuration declares
> `ironwood_activation_time` or `ironwood_activation_height`, and GLEEC KDF applies
> no upgrade gating. Set `ironwood_gate_disabled` to `true` to retain GLEEC-equivalent behaviour for
> a coin; with the default `false`, the swap freeze of R39.6.4b and the build
> refusal of R39.6.4c apply to any coin with a known Ironwood activation.
> Disabling them on a coin that does upgrade accepts the risk those gates exist
> to prevent -- a counterparty's HTLC left neither spendable nor refundable
> across the upgrade.
>
> **Status update (reloaded, Step E).** With version-6 construction supported
> (R39.6.4e) the gates never engage, so `ironwood_gate_disabled` no longer changes
> any behaviour of the gates. It keeps its meaning for a build that cannot construct
> the format, and is still accepted. It does **not** disable version-6 construction
> or activation-height discovery: GLEEC KDF cannot build for a post-Ironwood chain at
> all, so there is no GLEEC behaviour on that side for the switch to reproduce.
>
> **Omitting `ironwood_activation_time` is not an opt-out**, because a built-in
> activation time applies where the configuration supplies none (R39.6.4d).

R39.6.4d `consensus_params` shall additionally accept an optional
`ironwood_gate_disabled` boolean, defaulting to `false`, which suppresses both
R39.6.4b and R39.6.4c for that coin including any built-in activation time.

A build shall carry a built-in Ironwood activation time for each coin whose
upgrade is known to it, keyed on the coin's ticker, and shall apply it when the
coin configuration supplies none. For ARRR that value is **1 791 054 000**
(3 Oct 2026 19:00:00 UTC), read from the dictated chain's own
`KOMODO_IRONWOOD_ACTIVATION`.

**This reverses an earlier requirement of this chapter**, which forbade a
per-ticker default on the grounds that a compiled-in date could not follow a
moved activation. Two facts overrode it. First, both members are an extension
introduced by this project, and no published coin configuration carries either
member (in `GLEECBTC/coins` no coin mentions Ironwood, and ARRR is the only coin
carrying `consensus_params` at all) -- so gating driven purely by configuration
armed on no deployment whatsoever, making the safety net decorative. Second, the dictated
chain's maintainer confirmed on 2026-09-28 that there will be no standardness
grace period and recommended that ARRR swaps stop from 2 Oct, so the gates could
not wait on a third party's configuration change. An activation time is a fact
about the network, not an operator preference.

The original concern is answered by precedence rather than by abstention:

- A configured `ironwood_activation_time` shall override the built-in value, so a
  moved activation remains a data change and never requires a release.
- The built-in value shall be withheld unless the parameters describe the chain it
  was read from. That covers the test and regtest networks, which activate Ironwood
  at a height with no timestamp at all, so a mainnet timestamp describes neither --
  **and Pirate's own Ironwood test chain, which cannot be recognised by its address
  prefixes**: it carries the mainnet prefixes (`zs`, `pirate`) and reports
  `chainName` `main`, so a rule keyed on the prefixes treats it as the real chain.
  The built-in value is therefore keyed on the ticker **and** the chain's Sapling
  activation height (for ARRR, 152 855), which a chain that diverges from genesis
  does not share. A configured `ironwood_activation_time` still applies to any
  chain: the withholding is of the default, not of the feature.
- A coin whose ticker the build does not know shall be unaffected.

A build that gains v6 transaction support (R39.6.4c) makes both gates inert
regardless of any of the above.

They are two members rather than one because the dictated chain does not fix an
Ironwood activation height in advance: each node derives it at runtime from the
first block whose time exceeds the timestamp, plus a settling margin, so only
the timestamp can be published ahead of the upgrade and only the height is
usable as a consensus-parameter lookup.

Carrying `ironwood_activation_time` shall not by itself change any
network-upgrade activation lookup: it is data for discovery, not a height. The
Ironwood upgrade is bound to `NetworkUpgrade::Nu6_3` by R39.6.4e; every other
post-Sapling upgrade (Nu5 .. Nu6_2) shall continue to report no activation height,
because the shielded transaction builder derives the consensus branch ID -- and
therefore the transaction version it signs -- from exactly those lookups.

R39.6.4e **Version-6 construction and activation-height discovery.** A shielded
coin whose Ironwood activation height is known shall construct version-6
transactions from that height on, and version-4 transactions below it; a coin with
no Ironwood height anywhere shall keep constructing version 4 at every height.

- (a) **Binding.** `NetworkUpgrade::Nu6_3` reports the configured
  `ironwood_activation_height` when present, otherwise the height discovered at
  runtime, otherwise none. The discovered height lives in a cell shared by every copy
  of the consensus parameters (the coin, the transaction builder and the wallet
  database each hold a copy), is never serialized, and a configured height always
  wins. With the height reported the builder selects consensus branch `0x37A5165B`
  and version 6; without it, the Sapling branch and version 4.
- (b) **Shape.** The format is the one the dictated chain mines: header
  `0x80000006`, version-group id `0xD884B698`, branch id `0x37A5165B`, `nLockTime`,
  `nExpiryHeight`, the transparent inputs and outputs, the Sapling bundle, **a single
  `0x00` for the Orchard slot, and an empty Ironwood bundle**. Sapling survives inside
  version 6; this project builds neither an Orchard nor an Ironwood bundle, so the build
  configuration carries no Orchard or Ironwood anchor. The transaction identifier is the
  ZIP-244 digest, not a double-SHA256 of the bytes; a Sapling signature is made over the
  shielded signature hash (equal to the identifier for a transaction with no transparent
  inputs).
- (c) **Discovery.** The dictated chain publishes no activation height; each node
  derives it as the height of the first block whose time exceeds the activation time,
  plus 60, and trusts it only once that block is 30 deep. Before a transaction is built
  (the single height authority of R39.11.8 is the one place this happens), a coin that
  has neither a configured nor a discovered height shall read the server's consensus
  branch at its tip and, when the coin has an activation time (configured, or built-in
  under R39.6.4d), search the compact blocks' times for that first block, and decide:
  the server on Ironwood's branch means Ironwood is active, from the derived height when
  one exists at or below the tip and otherwise from the tip; the server not on Ironwood's
  branch means not yet active, unless the derived height lies ahead of the tip, in which
  case that height is recorded as upcoming. A discovered height is recorded and not
  searched for again.
- (d) **Refusal, never a guess.** If the server and the timestamp rule disagree -- the
  rule puts Ironwood at or below the tip and the server is not on Ironwood's branch -- or
  a read needed to decide fails, the build shall be refused with a typed error
  (`BuilderHeightError::IronwoodActivation`) rather than choose a version: the wrong
  version is rejected by the network as an opaque failure after the fact. The one
  exception is a failed timestamp search once the server has itself reported Ironwood:
  the exact height then refines only *past* heights, and costs nothing about a new
  transaction's version.
- (e) **Native mode** builds nothing in this release: its activation is refused
  (R39.11.1a), so no native-mode transaction reaches the builder and no version is ever
  chosen for one. Native mode returns in v0.4.0 together with discovery of the
  activation from its own daemon (`docs/plans/arrr-native-and-ironwood-pool.md`).

  > **Provenance of (e) (2026-10-28).** Clean-room original Reloaded policy, decided by
  > the maintainer. A Pirate Chain developer reported that native mode kept building
  > version 4 when no Ironwood height was configured. The fix chosen for this release is
  > the refusal of R39.11.1a, not discovery. The earlier text allowed version 6 only from
  > a configured height.
- (f) **Build capability.** With (a)-(d) in place `IRONWOOD_V6_SUPPORTED` is true, which
  makes R39.6.4b and R39.6.4c inert by their own terms.

> **Status update (reloaded, Step E).** Implemented: `z_coin.rs`
> (`ZcoinConsensusParams::activation_height`, `effective_ironwood_activation_height`,
> the chain-identity guard `builtin_ironwood_activation_time`), `z_coin/z_ironwood.rs`
> (the decision `decide`, the search `first_block_after`, `discover`),
> `ZCoin::ensure_ironwood_activation` called from `builder_target_height`, and
> `z_coin/z_v6_construction_tests.rs`. Tested without a broadcast: the header, version
> group, branch id, the empty Orchard slot and Ironwood bundle, the round trip through the
> Zcash reader the swap path uses, the ZIP-244 identifier, version 4 below the height and
> for a coin without one, a Sapling spend + transparent P2SH output + Sapling change
> funding verifying under the version-6 sighash, and -- the strongest check -- a real
> mined version-6 transaction (one Sapling spend, two outputs, two Ironwood actions, no
> transparent inputs) whose Sapling proofs, spend-authorization and binding signatures
> verify under the signature hash this library computes. **Not verified without a
> broadcast:** acceptance by the live network (policy, `IsStandardTx`, the HTLC
> `ac_private` exemption, fee policy); a version-6 shielded signature over a transaction
> *with transparent inputs*, whose digest also commits to the spent outputs' amounts and
> scripts (those are not in the recorded corpus; this is upstream's ZIP-244 code,
> unchanged for version 6); discovery against a mainnet server, in particular that
> Pirate's "first block with time over the threshold" agrees with the search's
> jitter window (16 blocks); and the 24-hour look-back Pirate's own derivation applies.
> **Known limits:** the discovered height is not persisted (it is re-derived after a
> restart, about thirty block reads); the pre-activation quiet window of the plan (refuse to
> build within three blocks of the boundary) is not implemented, so a transaction built
> in that window is rejected by the network rather than refused locally; on Pirate's test
> chain, whose activation does not follow the timestamp, the derived height for *past*
> blocks is not the chain's, which affects no new transaction.
>
> **Status update (2026-10-29): verified by broadcast.** On 2026-10-26 and 2026-10-27 the
> main network accepted version-6 transactions built by this project: a shielded send, and
> both transactions of a completed swap between two Reloaded nodes -- the HTLC payment
> mined at height 4,185,655 (version 6, no transparent input, two transparent outputs) and
> its spend mined at height 4,185,666 (version 6, one transparent input, no transparent
> output). Acceptance by the live network is therefore observed, and the spend settles the
> item above on a version-6 signature over a transaction with transparent inputs. The
> versions and shapes were read back from the chain on 2026-10-29. Still not verified:
> discovery against a mainnet server for a coin that configures no activation height. Of
> the known limits, the pre-activation quiet window is obsolete -- Ironwood has been active
> on the main chain since height 4,161,185, so no new transaction is built near the
> boundary -- and the unpersisted discovered height is deferred (D39.6.b).

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
- **Transparent key under HD.** *(binding, parity; maintainer Decision D39.5.a,
  2026-10-28.)* When the key policy is HD-derived, the coin's transparent
  secp256k1 key shall be the key the UTXO builder activates for a UTXO coin's
  default address: the coins-file `derivation_path` with account 0 (hardened),
  the external chain 0 and address index 0, i.e. `m/<derivation_path>/0'/0/0`.
  The activation request carries no member that changes it; its `account` member
  selects only the Sapling account. This key is the base key of R39.5.9 and the
  key of every transparent use of the coin. With a raw/iguana key policy it is
  the seed's secp256k1 key, as before. Under the HD policy a missing
  `derivation_path` is a build error, as a missing `z_derivation_path` is (a
  Reloaded choice: what the reference does without the member is not
  established). *Status (2026-10-28):* implemented by
  `z_coin::transparent_secret_for_policy`, reusing `derive_activated_hd_secret`,
  which the UTXO builder's HD activation also uses. A missing `derivation_path`
  fails the build with `HdDerivationError`.
- **Sync checkpoint.** `check_point_block` shall be the sync-start anchor. In
  native mode the wallet database is anchored at `check_point_block.height`,
  falling back to `sapling_activation_height` when the checkpoint is absent. In
  light mode the anchor is the checkpoint corresponding to the resolved sync
  start height (a height/date sync parameter, the earliest, or the default
  recent window of R39.8.0av), floored
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
>   UTXO side of the coin remains iguana-derived (out of R39.6.4 §2 scope at the
>   time; history since Decision D39.5.a, 2026-10-28, extended §2 to the
>   transparent key under HD, now implemented).
> - **ZOMBIE fixtures.** Test fixtures updated to carry full `protocol_data` with
>   Zcash-mainnet parameters; bare `{"type":"ZHTLC"}` is now non-conformant per
>   R39.1.2.

#### Deferred Work (§39.6)

D39.6.a **Shielded coins in WASM -- after the beta.** *(Recorded 2026-10-29.)* A WASM
build needs a shielded wallet database in the browser (the logical data model of
R39.8.0j), a Sapling prover that runs there, and a way to obtain the Sapling parameter
files (§39.6.1). Until then a WASM build refuses to activate a shielded coin (R39.10.1).

D39.6.b **The discovered Ironwood activation height is not persisted -- after the beta.**
*(Recorded 2026-10-29.)* A height found by discovery (R39.6.4e(c)) is kept in memory only
and is derived again after a restart, at a cost of about thirty block reads. Persisting
it saves those reads; it changes no outcome, because the derivation gives the same height
each time and its result is trusted only once its block is 30 deep.

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

R39.8.0f In Light mode, activation shall use the configured
`light_wallet_d_servers` for every backend function of the coin, including the
chain tip, compact-block download, shielded note scanning, and the transparent
surface the swap path needs; the Electrum server list is not used. In Native
mode, activation shall use the native Zcash-family backend as the compact-block
source. Both modes shall feed the same shielded wallet database contract and the
same `z_coin_tx_history` data path. The function table, what activation requires
of the backend, and the single height authority are specified in §39.11.

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
  when that state exists; otherwise it shall use the default recent-start
  policy of R39.8.0av, floored at `sapling_activation_height`.

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
>   This applies to a start given as a **height**. A start given as a **date**
>   in the future is rejected instead (R39.10.11, maintainer decision
>   2026-10-11), before any wallet state is touched; this is implemented (§39.10
>   Reloaded status, item 2).
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

> **Dirty-side derivation record (sanitized).** R39.8.0av, the two notes under
> it and T39.8.0av were authored by the KDF Spec Reader under the chapter-01
> two-team workflow (§1.4A), from the GLEEC KDF public release designations
> `v3.0.0-beta` (reference) and `v2.6.0-beta` (legacy cross-check), which agree
> on every fact bound below. They carry only the resolved start height's
> external contract, the configuration keys it consumes, and the activation
> outcomes it produces; no implementation expression was carried across.
>
> **Clearance (chapter 01 T1, R40).** That material -- R39.8.0av, the two
> informative notes under it, and T39.8.0av -- **passed the KDF Dirty Gate** on
> 2026-10-24: PASS, discretionary-expression measure 0.02, finding list empty,
> and accordingly no remedy class for any line or for the candidate as a whole.
> Its clearance is therefore *complete* and not pending, and R40 withholds
> nothing in it. One limit of that pass belongs here and not only in the
> footer: the comparison reached material at the `v3.0.0-beta` designation
> only, so the agreement between the two designations that the record above
> asserts **is not attested by the clearance** and rests on the derivation
> record alone. The extent, method and measure of the pass are recorded once,
> in the *Dirty Gate clearance* item of §39.9, and are not restated here.

R39.8.0av **The default sync start when the caller supplies none.** When
activation carries no `sync_params` and no prior local shielded state exists,
the start height shall be derived from a **recent window below the current
chain tip**, floored at `sapling_activation_height`. In the reference the
window is a *wall-clock* quantity and not a block count: it is **one day
before the present moment**, converted to a height by the same block-time
estimate the reference uses for an explicit `{"date": T}` start -- the coin
configuration's `avg_blocktime` seconds per block, from which a blocks-per-day
figure is computed and subtracted from the tip. That estimate never reads a
block timestamp. Three consequences are bound:

- the window's size in blocks is **per coin**, not global: a 60-second coin
  gets 1,440 blocks, a 150-second coin 576. For ARRR the reference's default
  start is therefore `tip - 1440`, i.e. one day of history;
- a coin configuration carrying no `avg_blocktime` cannot resolve the default
  at all, and activation fails with `CoinCreationError` (R39.10.11) rather
  than falling back to Sapling activation or to the configured checkpoint;
- the elapsed interval is taken in whole days and one further day's blocks are
  always added, so the smallest window the estimate can produce is one day's
  blocks even when the start moment lies inside the current day.

In Light mode the resolved height is **not** read from the coin
configuration's `check_point_block`. The anchor is fetched from the backend:
the commitment-tree state at the resolved height (§39.11.1) supplies the
height, hash, time and Sapling tree that seed the wallet (R39.8.0l). The coin
configuration's checkpoint is the **Native-mode** anchor only, and Native mode
uses it unconditionally -- Native applies no recent window and never
re-anchors, so its `first_sync_block` reports the configured checkpoint
height, or Sapling activation when the entry declares none (§39.10.4).

The default start is **not** derived from an account-birthday field, and no
coin-configuration key supplies one. The wallet's birthday *is* the resolved
start: it is written into the wallet database when the account is created and
is read back afterwards as the wallet's anchor (R39.8.0h's status note, and
R39.8.0al's rebuild rule, which re-anchors at exactly that height).

> **Upstream divergence (informative) -- the size of the default window.**
> Reloaded's default window is a single built-in constant of **2,880 blocks**
> applied to every shielded coin, which at ARRR's 60-second blocks is **two
> days** -- double the reference's one day, and for any other block time a
> different interval entirely. The *shape* of the policy is parity: a recent
> window below the tip, floored at Sapling activation, consulted only when the
> caller supplies no start and no prior local state exists. The *number* is
> not traceable to the reference and is Reloaded's own; nothing in the
> reference expresses the window as a fixed block count. Reloaded also does
> not consume `avg_blocktime` and so never fails activation for its absence.
> The observable consequences are confined to the initial scan range and to
> `first_sync_block.requested`; no wire shape changes. Converging would mean
> reading `avg_blocktime`, computing one day's blocks per coin, and adopting
> the activation failure when the key is missing.
>
> **The zero-balance outcome is not a Reloaded defect.** A caller that
> activates a shielded coin with no `sync_params` against a wallet database
> that has never been scanned gets a recent-window scan in *both*
> implementations, and funds older than that window are invisible in both --
> in the reference sooner, because its window is half as long for ARRR. A
> wallet whose funds predate the window must be activated with an explicit
> `sync_params` (R39.8.0g); that is the operator remedy for a migrated or
> restored wallet, and no code change is required to obtain the reference's
> behaviour.

> **Code-quality finding (informative).** Trigger: a contradiction apparent
> between two conditions read side by side while establishing R39.8.0av. In
> the reference, "prior local shielded state exists" is decided from the
> **compact-block cache** alone -- whether that cache holds any block at all
> -- and not from the shielded wallet database. A wallet database carrying
> years of scanned history whose compact-block cache has been cleared (a
> disk-space cleanup, a cache format migration, an interrupted cache rewind)
> therefore reads as having no prior state: the default recent window is
> resolved, the wallet's own earliest scanned block no longer matches it, and
> the wallet database is truncated and re-anchored at the recent window. The
> scanned history is destroyed and the reported balance drops to zero with no
> caller request to rescan and no explicit `sync_params` in play. The two
> stores have independent lifetimes -- the compact cache is a disposable fetch
> buffer and the wallet database is the durable record (R39.8.0k) -- so the
> disposable one must not decide whether the durable one is re-anchored.
> Proposed fix: decide "prior local state exists" from the wallet database's
> own earliest scanned block, and treat an empty compact cache as nothing more
> than a fetch that must resume from the wallet's existing anchor. Reloaded
> already decides it that way, keying its fetch plan on the wallet's scanned
> height and anchor rather than on the cache, so this is a finding against the
> reference and **not** a convergence target: a Coder shall not adopt the
> cache-keyed test.
>
> *Status (2026-10-29): nothing open for Reloaded; the proposed fix is addressed to
> the reference.*

T39.8.0av *Default-window shape.* A test shall pin, for a wallet database with
no scanned block and an activation carrying no `sync_params`: that the resolved
start equals the tip minus the implementation's default window, that it is
floored at `sapling_activation_height` when the tip lies within one window of
Sapling activation, and that `first_sync_block.requested` reports it. A second
test shall pin that a wallet database **with** scanned blocks and an activation
carrying no `sync_params` resumes from the wallet's own scanned height and does
not re-resolve the default window, whatever the state of the compact-block
cache -- including an empty cache (the finding above). *(Cleared with
R39.8.0av -- see the clearance statement above that rule and the record in
§39.9.)*



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
activation as the coin's background shielded sync loop (§39.8.0.5). During
activation it shall report observable progress for compact-block cache update and
wallet-db building, and activation shall wait until the wallet database is scanned
through the current activation tip before returning success. Concurrent activation
tasks for the same shielded ticker shall be serialized for the complete task,
including database construction, download, scan, result calculation, and
registration, so one task cannot rebuild or rename a database while another task
is using it. Different tickers shall remain independent, and cancellation shall
release the serialization guard.

> **Status (reloaded, 2026-08-09).** Implemented for Light mode: activation with
> complete-through-tip gating, two-phase progress, same-ticker serialization, and
> the R39.6.2 pacing controls; plus the post-activation background shielded sync
> task of §39.8.0.5. Native mode is still served only by the legacy
> commitment-tree catch-up loop and does not yet maintain the modern wallet
> database after activation.

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
downloading those blocks again. An invalid segment shall be discarded, replaced
with a fresh compact cache, and refetched; its reported maximum height alone is
never sufficient evidence for reuse. The cache is deleted, not kept as a backup:
it holds only blocks the backend can serve again, and a failure that repeats on
every background pass (R39.8.0aa) would otherwise leave one more copy behind each
time. Wallet-database backups of the rebuild paths (R39.8.0x, R39.8.0al) are
unaffected; a wallet database that has not scanned a block yet is deleted like the
cache when it is replaced (R39.8.0ax).

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
`z_coin_tx_history` only through the wallet database scan state. The observable
contract for that coordination -- its ordering relative to broadcast, its release
conditions, its durability and its serialization scope -- is bound by §39.8.0.7
(R39.8.0ao--R39.8.0at); this rule states only that the coordination exists.

R39.8.0q Unconfirmed shielded outputs are not spendable merely because the local
process created or observed them. Spendable balance, transaction history, and
post-swap Desktop display shall reflect confirmed wallet-db scan results, with
the existing locked-note/change tracking used only to prevent unsafe local
double-spends while waiting for confirmation. Reporting an unconfirmed receipt as
a *pending* (non-spendable) amount is required separately by §39.8.0.6 and does
not relax this requirement. One exception concerns the history only: with the
switch `shielded_history_unmined_sends` at its default, an own shielded send is
listed in the transaction history from broadcast on, first and with its final
amounts (R39.8.9i, decided in D39.8.0h). That row is display only; it makes
nothing spendable, and with the switch set to `false` the history again reflects
scanned results alone. Likewise, the change of an own unmined send is reported as
non-spendable balance straight from the wallet database (R39.8.0ag1). It becomes
spendable only when the scan reaches its block.

> **Status update (reloaded, 2026-08-09).** Light mode satisfies the spendable
> half of this requirement: its balance is read from the scanned wallet database,
> which by construction contains only mined, scanned notes. It does not yet
> satisfy §39.8.0.6, because it reports a zero pending amount unconditionally, so
> an incoming shielded transaction stays invisible until its block is mined *and*
> scanned rather than appearing as a pending amount first. Native mode already
> distinguishes the two, counting zero-confirmation notes as the non-spendable
> portion of the balance. Until §39.8.0.6 is implemented, the expected
> user-visible latency for an incoming shielded payment in Light mode is one
> block plus up to one background sync period (§39.8.0.5).
>
> **Status update (2026-10-29).** The note above is out of date: Light mode has
> implemented §39.8.0.6 since 2026-08-09 (D39.8.0c). An incoming shielded payment
> seen in the mempool is reported in the non-spendable portion of the balance
> before its block is mined, and the change of an own unmined send is reported
> there too (R39.8.0ag1). The latency stated above no longer applies.

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
| selected current wallet generation | valid SQLite produced by `zcash_client_sqlite 0.22.0`; `user_version = 8`; `schemer_migrations` having the single ordered column `id BLOB PRIMARY KEY`; exactly 71 ordered migration IDs whose canonical digest is `1142e14a2d3103f1cabbbcbc2d686000e021bf88bcbcce2d34f7d0a040e2d08b`; and exactly 727 canonical semantic records whose version-prefixed digest is `4ebc85f7ea3516d019b3d0d4680a6cb75ec980bfe81437abb4913d50b3407350` |
| previous Reloaded wallet generation (upgradeable) | valid SQLite produced by `zcash_client_sqlite 0.21.1`, the schema of the previous Reloaded build; `user_version = 8`; `schemer_migrations` with exactly 48 migration IDs whose canonical digest is `7b506df8a2b119fb143664a1c0b5eddfa7de5fe8458f07829818c59e5b39ddbb`; and exactly 467 canonical semantic records whose version-prefixed digest is `dfba9135b6d5ae446e88d83d162e9458730b5b098051d8565fdd91fe6be5bea8`. Handled by R39.8.0ba |
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
- a positively recognized previous Reloaded wallet is upgraded under R39.8.0ba;
- an unknown or corrupt file is preserved and produces a typed activation
  failure; it is not deleted, overwritten, or silently treated as empty.

A recognized compact cache may supply serialized compact blocks to the rescan
only after the existing height, payload-decode, sequential-height, and hash-link
checks of R39.8.0n succeed. Its presence never changes the requirement to build
all current wallet state anew, and it never constitutes evidence that the
wallet database is current. The rebuild shall report the existing
`UpdatingBlocksCache` and `BuildingWalletDb` phases according to the work
actually performed.

R39.8.0ba **Upgrade of a previous Reloaded wallet.** A wallet database that
classifies as the previous Reloaded generation of R39.8.0w (it was created by an
earlier Reloaded build on an older `zcash_client_sqlite` that applied fewer
migrations) shall be brought to the current schema by the dependency's own
migrator, under these steps, all inside the wallet-database serialization of
R39.8.0ad:

1. **Back up first.** A byte-for-byte copy of the database (after folding its WAL
   into the main file) is written to a fresh `.pre-upgrade.<ms>.bak` name before
   any other step. If the copy cannot be made, activation fails with the typed
   schema error and the file is left unchanged.
2. **Migrate a working copy.** The migrator runs on a private working copy, never
   on the original.
3. **Verify.** The migrated copy shall classify as the selected current
   generation of R39.8.0w. Only then does it replace the original (atomic
   rename), with the original's sidecars removed so they cannot be replayed into
   it. The account and scan state carried by the wallet are kept.
4. **Fall back.** On any failure at any step -- working-copy creation, migration
   error, post-migration classification other than selected current, or the
   final replacement -- the working copy is discarded, the original is moved
   aside as `.upgrade-failed.<ms>.bak` exactly as the rebuild path of R39.8.0x
   does, a fresh wallet is created, and the wallet is rescanned. The outcome is
   therefore never a half-migrated wallet; it degrades to the rebuild behaviour.

An unknown or corrupt file is not covered by this rule and keeps the typed
failure of R39.8.0x. This rule does not extend to wallets older than the
previous Reloaded generation; the reference legacy generation still follows the
never-migrated rule of R39.8.0x.

R39.8.0y The public modern candidates form two coherent, non-interchangeable
dependency generations as of the reference date:

| Candidate line | Backend | SQLite | Primitives | Proofs | Note encryption | Distribution |
|---|---:|---:|---:|---:|---:|---|
| stable (superseded 2026-10-12; see the selection record below) | `0.23.0` | `0.21.1` | `0.28.0` | `0.28.0` | compatible `0.4.1` requirement (resolving to `0.4.2` in a fresh lock) | published crates.io releases |
| newer release candidate (closed 2026-10-12: not a live option) | `0.24.0-rc.7` | `0.22.0-rc.7` | `0.30.0` | `0.30.0` | compatible `0.4.1` requirement | published RCs / matching upstream release tag |

Both rows are the 2026-08-04 evaluation, kept as history. Neither is the stack
now in use; the current stack is the 2026-10-12 selection record below.

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

> **Selection record, current (2026-10-12).** The maintainer's decision is
> published stable releases only: no prerelease crate and no git dependency is
> selected. The stack in use since the re-vendoring is `zcash_primitives 0.30.1`
> (vendored and patched), `zcash_transparent 0.10.0` (vendored and patched),
> `zcash_client_backend 0.24.0` (consumed unpatched from crates.io; its vendor
> copy is dropped), `zcash_client_sqlite 0.22.0`, `zcash_proofs 0.30.0`,
> `zcash_protocol 0.10.6`, and `sapling-crypto 0.7.0` (unchanged). This is the
> published-stable successor of the `-rc.7` candidates of the table above, which
> are therefore no longer evaluated as an option: the `0.24.0-rc.7` /
> `0.22.0-rc.7` row is closed. The patches retained are the narrow ones of the
> earlier record (the minimum transparent/builder hooks that preserve KDF P2SH
> input and raw-output transaction bytes). One upstream wrinkle is unavoidable:
> `zcash_primitives 0.30.1` itself declares exact version pins on two prerelease
> crates, `block-buffer` and `crypto-common` release candidates. That is
> upstream's choice for this release; it resolves, and KDF selects no prerelease
> crate of its own, but the resolved tree is not free of prerelease packages. The
> `zcash_client_sqlite 0.22.0` migrator applies 71 migrations (23 more than
> 0.21.1) and creates Ironwood/Orchard tables unconditionally, which is why the
> wallet-schema identity of R39.8.0w changed and R39.8.0ba exists.

> **Selection record, superseded (2026-08-04; superseded 2026-10-12 by the record
> above).** Kept as the record of why the stable line was chosen at the time.
> The operator selected the stable line:
> `zcash_client_backend 0.23.0`, `zcash_client_sqlite 0.21.1`, and
> `zcash_primitives`/`zcash_proofs 0.28.0`. KDF consumes exact crates.io
> versions and retains only narrow, documented patches: removal of backend
> 0.23.0's obsolete exact `time-core 0.1.2` resolver pin, plus the minimum
> transparent/builder hooks required to preserve KDF P2SH input and raw-output
> transaction bytes. The old broad `librustzcash-patched/` dependency is not
> part of the selected graph. (Under the current record `zcash_client_backend`
> is no longer vendored or patched, and the primitives line is `0.30.x`.)

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

T39.8.0f A wallet database captured from the previous Reloaded build (a real
`zcash_client_sqlite 0.21.1` file with one imported account, not a DDL
reconstruction) shall classify as the previous Reloaded generation without being
modified, and opening it shall run the dependency's migrator over it, leave a
byte-identical `.pre-upgrade` backup, and yield a database that classifies as the
selected current generation with 71 applied migrations and its account intact. A
second test shall force the migration attempt to fail and prove the original is
preserved byte-identically, a `.pre-upgrade` backup also exists, and a fresh empty
current-generation wallet is created. The post-migration verification-failure
branch is not separately exercised.

#### Deferred Work

D39.8.0a *(Resolved 2026-08-04; the pinned identities re-pinned 2026-10-12 for the current stack.)* The operator selected the stable line and the
selected-current row of R39.8.0w now binds its exact migration and semantic
schema identities. The implementation still must not classify a database as
current from `user_version = 8` or the migration-table name alone.

D39.8.0b *(Specified 2026-08-09; see §39.8.0.5.)* The modern Light compact-block
and wallet scanner shall be moved into a coin-lifetime background task after
activation without weakening the existing activation-through-tip gate. The task
needs an explicit poll policy, reorg and endpoint-failover handling, cancellation
on coin disable/context shutdown, and serialization with re-activation or
database rebuild.

This item is now superseded by normative requirements rather than by a note:
R39.8.0aa (coin-lifetime task, resume-from-local-state, no re-anchoring),
R39.8.0ab (bounded poll policy and its latency budget — the period itself is the
implementation's choice, not a dictated value), R39.8.0ac (termination on coin
disable/shutdown in bounded time), R39.8.0ad (wallet-database-scoped
serialization against activation rebuild/rename), and R39.8.0ae (endpoint
failover; reorg recovery inherited from R39.8.0n). Tests are T39.8.0f.

Residual: unconfirmed shielded receipts are still not represented in the balance,
because the background pass consumes only mined compact blocks. That gap is now
specified by §39.8.0.6 and tracked as D39.8.0c.

D39.8.0i **Native mode returns after the beta.** *(Recorded 2026-10-29.)* The Native-mode
gaps named in the status notes of R39.8.0f and R39.8.0m -- full-node blocks are not
converted into the compact-block cache, and the wallet database is not maintained after
activation -- and the Native-mode question of D39.11.j are not worked on before the beta:
a Native-mode activation of a shielded coin is refused in this release (R39.11.1a). They
return with Native mode in v0.4.0 (`docs/plans/arrr-native-and-ironwood-pool.md`).

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
- Published stable package manifests for the stack in use (selection record of
  2026-10-12): <https://crates.io/crates/zcash_client_backend/0.24.0>,
  <https://crates.io/crates/zcash_client_sqlite/0.22.0>,
  <https://crates.io/crates/zcash_primitives/0.30.1>,
  <https://crates.io/crates/zcash_transparent/0.10.0>,
  <https://crates.io/crates/zcash_protocol/0.10.6>, and
  <https://crates.io/crates/sapling-crypto/0.7.0>.
- Historical (superseded 2026-08-04 selection): `zcash_client_backend` `0.23.0`
  and `zcash_client_sqlite` `0.21.1`:
  <https://crates.io/crates/zcash_client_backend/0.23.0> and
  <https://crates.io/crates/zcash_client_sqlite/0.21.1>.
- Historical (closed release-candidate row): `zcash_client_backend` `0.24.0-rc.7`
  and `zcash_client_sqlite` `0.22.0-rc.7`:
  <https://crates.io/crates/zcash_client_backend/0.24.0-rc.7> and
  <https://crates.io/crates/zcash_client_sqlite/0.22.0-rc.7>.
- SQLite schema-table and application-version interfaces:
  <https://www.sqlite.org/schematab.html> and
  <https://www.sqlite.org/pragma.html#pragma_user_version>.

### 39.8.0.5 Post-activation background shielded sync (Light mode)

R39.8.0aa A Light-mode shielded coin shall keep its shielded wallet database
current for the whole time the coin is active, not only up to the tip observed
during activation. On successful completion of a Light activation the
implementation shall start exactly one coin-lifetime background sync task per
activated shielded coin, whose only job is to advance the compact-block cache and
then the shielded wallet database toward the current backend tip. The task shall
resume from the wallet's own local sync state and shall never re-anchor,
rewind-by-policy, or otherwise change the sync start point established by
R39.8.0g--R39.8.0h; changing the sync anchor remains an activation-only
operation. The task shall not weaken the activation-through-tip gate of R39.8.0m:
activation still returns only after its own scan is complete, and the background
task exists to cover chain growth afterwards. A pass that finds the backend tip
at or below the wallet's already-scanned height shall do no work.

R39.8.0ab The background task shall have an explicit, bounded poll policy. **No
poll period is dictated by any external contract**; the exact value is the
implementation's choice, constrained as follows:

- the period shall be strictly positive and finite — neither a busy loop nor an
  unbounded wait — and it shall be a named, documented constant or configuration
  value rather than an incidental literal;
- passes shall not overlap: a new pass shall begin only after the previous pass
  has finished or failed, so a slow catch-up cannot fan out concurrent scans of
  the same wallet database;
- the period shall be short enough that it is not the dominant term in
  incoming-payment visibility latency. The steady-state latency budget for a
  mined shielded receipt is *one block interval plus at most one poll period*, so
  the poll period shall not exceed the coin's nominal block interval;
- a failed pass (backend unreachable, fetch error, scan error) shall be treated
  as transient: it shall not terminate the task, shall not tighten the poll
  cadence into a hot retry, and shall be retried on a subsequent scheduled pass;
- a failed scan shall leave the wallet conservatively marked as not
  scan-complete, so spending and shielded-transaction construction stay blocked
  until a later pass succeeds (consistent with R39.8.0q).

> **Rationale (informative).** The GPLv2 baseline's Native shielded catch-up loop
> already establishes the same shape — a periodic catch-up pass followed by a
> fixed sleep, terminating when the coin is dropped — and its period is an order
> of ten seconds. Any period from a few seconds up to the coin's block interval
> satisfies R39.8.0ab; picking one exact number is an implementation decision and
> is deliberately not specified here.

R39.8.0ac The background task shall terminate when the coin is disabled or the
application context shuts down. It shall not keep the coin alive: holding only a
non-owning reference to the coin and returning when that reference can no longer
be upgraded is a sufficient termination condition, and an explicit cancellation
handle is equally acceptable. Termination shall be observable in bounded time —
at most one poll period plus the duration of an in-flight pass — and shall
release every resource the task holds, including the serialization guard of
R39.8.0ad. A pass still running at the moment of disable shall not be able to
corrupt or resurrect state for a coin that has been disabled.

R39.8.0ad Background scanning and activation-time database construction shall be
mutually exclusive per shielded wallet database. The implementation shall
serialize on the identity of the wallet database itself (its on-disk location,
not merely the ticker), and both the background scan entry point and the
activation rebuild/replace/rename entry points shall take that guard. The
guarantee is that no activation may create, rebuild, replace, rename, or remove a
wallet database file while a scan of that same database is in flight, and no scan
may run against a database whose files a concurrent activation is in the middle
of replacing. Distinct wallet databases — including different tickers, and the
legacy versus current namespaces of R39.8.0x — shall remain independent, and the
guard shall be released on completion, failure, and cancellation alike. This
guard is additional to, not a substitute for, the per-ticker activation
serialization of R39.8.0m: it also covers the window in which a background pass
started before coin disable is still running while a re-activation begins.

R39.8.0ae A pass shall tolerate an individual unavailable shielded backend. When
several `light_wallet_d_servers` are configured (R39.2.4), the implementation
shall attempt the *admitted* endpoints (R39.11.7a) in turn and shall declare the
pass failed only after every admitted endpoint has failed; an endpoint excluded
by admission is not attempted and is not counted as a failure. The height a pass
scans *towards* is the single height authority of R39.11.9, subject to the
monotonicity rule of R39.11.10. Chain-continuity recovery is not
restated here: a background pass uses the same fetch/validate/scan path as
activation and therefore inherits the sequential-height and hash-link validation
and the rewind/refetch-on-discontinuity behaviour of R39.8.0n, including the
handling of a reorg that invalidates already-cached blocks. A reorg of the block
the wallet last scanned is detected on every pass and repaired by the rewind or
sync-start rebuild of R39.8.0al, so a failing pass can never repeat the same
failure on every later pass.

> **Status update (reloaded, Step C).** Implemented. The background pass and
> activation fetch only the endpoints admission lets through (R39.11.7a), in
> configured order; the height a pass scans towards is the single authority of
> R39.11.9 with the monotonic floor of R39.11.10. The inherited R39.8.0n
> continuity behaviour is unchanged.

R39.8.0au **A failed sync shall be reported as failed, not as progress.** When a
background pass fails — the chain tip cannot be reached, every configured
shielded backend has failed under R39.8.0ae, or the scan itself errors — the
history-sync state the coin reports shall be the error state, carrying a message
describing the failure and the height scanned so far. It shall not be reported as
a sync still in progress.

A failed pass and a pass still running both leave the wallet database short of
the chain tip, so a state derived from scan completion alone cannot tell them
apart, and a wallet that can no longer sync is then indistinguishable from one
that is merely slow — the two call for opposite responses from the operator. The
reported failure shall be cleared by the next successful pass, so that the state
tracks the current condition rather than latching on the first failure, and shall
be reported whether or not the initial scan had already completed, because a
background pass can fail after activation succeeded.

Four further conditions shall be reported through this same channel rather than
as generic RPC failures: an endpoint excluded by admission, with its reason
(R39.11.7a(f)); a transparent history window that failed part-way, or completed
with a skipped height, rather than completing empty (R39.11.4b); a backend whose
address index returns neither encoding of the P2SH output being watched
(R39.11.4d); and a backend that cannot serve the bytes of a transaction with no
transparent output (R39.11.4b, D39.11.g). Each leaves the wallet or a swap
decision short of an answer while nothing has strictly "failed", which is exactly
the case this rule exists to make visible.

> **Status update (reloaded, Step C).** The channel is implemented: a failed
> background pass is reported as the error state with a message and the height
> scanned so far, cleared by the next successful pass. A failed chain-tip read, and
> an admission failure (whose message lists every excluded endpoint with its
> reason, R39.11.7a(f)), now populate it. **Swap half implemented:** the three
> swap-path conditions of the last paragraph -- a history window that failed
> part-way or skipped a height, a backend whose index returns neither encoding of
> the P2SH output being watched, and a backend that cannot serve a transaction with
> no transparent output -- are written to the channel by the swap-path detection
> when it cannot answer (R39.11.4b(f), R39.11.4d, R39.11.17). The last of them is
> not distinguishable from the first: a node without its transaction index
> surfaces as skipped heights, and the message says so. They share the channel's
> one message slot with background sync failures, and the next successful
> background pass clears them. Endpoints excluded
> by an admission that still succeeded are logged and available through
> `ZCoin::excluded_lightwalletd_endpoints`, but are not written into the sync-state
> channel unless a later pass fails.

### 39.8.0.6 Pending (unconfirmed) shielded receipts

R39.8.0af A **pending shielded receipt** is a wallet-owned Sapling output that
the wallet has observed in a transaction the backend reports as present in the
mempool, and that is not yet present in the shielded wallet database as a scanned
received note. Its amount is the note value recovered by trial-decrypting that
output with the wallet's incoming viewing capability; an output that does not
trial-decrypt to a tracked account is not a receipt and shall be ignored.

R39.8.0ag A pending shielded receipt shall be reported to the caller **only** as
part of the non-spendable portion of the coin balance (the `unspendable` field of
the balance object), aligning Light mode with the Native-mode behaviour of
counting zero-confirmation notes as non-spendable. It shall never contribute to:

- the spendable balance;
- any tradable/available-volume figure, order maximum, or swap-funding decision;
- note selection or input selection for building any shielded transaction,
  including swap payments, fee/burn outputs, and change.

Pending receipts shall likewise not appear in `z_coin_tx_history`, which remains
derived from wallet-database scan state (R39.8.9). R39.8.0q continues to govern:
observing a receipt never makes it spendable; only a wallet-database scan does.

R39.8.0ag1 **The change of an own unmined send is non-spendable balance.** When
this node broadcasts a shielded transaction and records it in the wallet
database (§39.8.0.7), the value of every wallet-owned output of that
transaction, its change included, shall be reported in the `unspendable` field
of the balance from the moment of the record. It stays there until the block
that mines the transaction is scanned, when it moves to the spendable balance.
If the transaction expires unmined (R39.8.0ar), it leaves the balance. It is
counted once. A pending receipt (R39.8.0af) for the same transaction output
does not add to it, so the two sources never sum for one output. R39.8.0ag's
limits apply unchanged: this value never enters the spendable balance, any
tradable volume, or note selection. The wallet database is the source, so the
value is shown at once after the broadcast, without waiting for a mempool
poll, and survives a restart.

> **Provenance and reason (2026-10-26).** Clean-room original. The mempool poll
> of R39.8.0ai trial-decrypts with the external incoming viewing key. A wallet's
> own change goes to its internal (change) address, which that key cannot
> decrypt, so an own send's change was never a pending receipt: right after a
> send, `unspendable` read 0 and the change appeared only once mined (observed
> live 2026-10-26). The reference reports the change of its own unmined sends
> as non-spendable balance (Spec Reader, 2026-10-26), so this rule restores the
> observable behavior. Change of a send made by another instance on the same
> seed is not covered, because it is not in this node's wallet database. That
> case is deferred with the two-daemon item of the in-flight note-locking plan.

R39.8.0ah A pending shielded receipt shall be invalidated — removed from the
pending set and no longer added to the non-spendable balance — as soon as any of
the following holds:

- its transaction has been scanned into the shielded wallet database (the same
  value then appears through normal scanned-note accounting);
- its transaction is no longer reported by the backend as being in the mempool
  and has not been mined (dropped or replaced);
- its transaction's expiry height has been passed by the wallet's scanned tip.

**Double-counting constraint.** The same value shall never be counted twice. An
amount shall not appear simultaneously in the spendable balance (from the wallet
database) and in the pending non-spendable amount, and the transition between the
two shall be atomic from the caller's point of view: a balance query shall never
observe a state in which a receipt has been counted from both sources, nor a
state in which a receipt that is still in flight has been dropped from the
pending set before the scan that supersedes it is durable. Repeated observation
of the same mempool transaction across successive polls shall contribute its
value at most once, keyed by transaction identifier and output index.

R39.8.0ai The pending-receipt source is the lightwalletd streaming mempool
method, which is dictated interop:

| Element | Contract |
|---|---|
| method | `GetMempoolTx` |
| request | `Exclude`, carrying a repeated `txid` byte field holding identifiers (optionally shortened to a prefix) the client already holds |
| response | a **stream** of `CompactTx`, each carrying the transaction `index`, `hash`, optional `fee`, its `CompactSpend` nullifiers, and its `CompactOutput` entries |
| recovery | a `CompactOutput` carries the note commitment, ephemeral key, and the truncated note ciphertext, which is sufficient for trial decryption of the note value by the wallet's incoming viewing capability; the memo is not recoverable from a compact output |

The implementation shall treat the mempool view as advisory and possibly stale by
seconds, shall pass already-known transaction identifiers in `Exclude` so a poll
does not re-stream what it already holds, and shall degrade gracefully: an
endpoint that errors on, or does not support, this method shall cause the pending
amount to be reported as zero rather than failing the balance query or the
background sync pass of §39.8.0.5.

R39.8.0aj The pending-receipt view shall be refreshed on the same coin-lifetime
schedule as the background sync of R39.8.0ab, or on balance query, or both; the
choice is the implementation's, subject to the constraint that the reported
pending amount shall not be older than one poll period and shall not be computed
from a mempool snapshot taken before the wallet-database height it is combined
with.

#### Tests

T39.8.0f Background-sync tests shall prove that, after a Light activation
completes, a block mined afterwards becomes visible in balance and
`z_coin_tx_history` without re-activation and within the stated latency budget of
R39.8.0ab; that the background task does not change the wallet's sync anchor
across passes; that a pass failure is transient and a later pass recovers; that a
failed scan leaves the coin blocked from spending until a pass succeeds; that
disabling the coin terminates the task within one poll period plus one in-flight
pass; and that a re-activation which rebuilds the wallet database cannot proceed
while a background pass is scanning that same database, nor a pass begin against
a database mid-rebuild (R39.8.0ad). A multi-endpoint fixture shall prove that a
pass survives the failure of any proper subset of the configured endpoints and
fails only when all of them fail.

Reorg-recovery tests (R39.8.0al, R39.8.0n) shall run without a network, with the
backend's tree state supplied by a fixture, and shall prove: (a) a wallet scanned
through height H with a checkpoint for every block and a note of its own, whose
in-session chain state is at H and whose tip the chain replaced, detects the
divergence on the next pass, rewinds below H without rebuilding, clears the cached
compact blocks above the rewound height, scans the new chain past H, keeps its
account and balance, and creates no backup file, with the rewind landing on the
oldest retained checkpoint (inside the window) and the fixture pinning the
backend's 100-checkpoint window; (b) the same for a sparse-checkpoint wallet,
where the rewind lands directly, and for a wallet whose first note is mined after
its first scanned block, where the rewind must still land inside the window rather
than on the block before that note; (c) a rewind
the backend refuses below the oldest retained checkpoint is redirected to that
checkpoint; (d) a divergence beyond the
retained window rebuilds the wallet at its sync start, asks the backend for no
height below it (in particular never 0), recreates the account at the original
birthday, scans the new chain, and leaves exactly one wallet backup and no
compact-cache backup; (e) a wallet with no recorded sync start, or a backend that
cannot supply the rebuild anchor, fails the pass and leaves the wallet unmodified;
(f) an unchanged chain keeps the anchor; (g) a compact cache that fails validation
is deleted, not renamed to a backup; (h) passes that keep failing after a rebuild,
and a restarted interrupted first scan, add no wallet backup beyond the rebuild's
own one, while a wallet that has scanned blocks is refused and left untouched
(R39.8.0ax).

T39.8.0g Pending-receipt tests shall use a mempool fixture to prove that a
wallet-owned unconfirmed shielded output is reported in the non-spendable balance
and never in the spendable balance or any tradable volume; that it is not offered
for note selection; that it does not appear in `z_coin_tx_history`; that once the
transaction is mined and scanned the total balance is unchanged across the
transition (no double count, no gap) and the value has moved to spendable; that a
dropped or expired mempool transaction removes the pending amount; that the same
transaction observed on repeated polls contributes once; and that an endpoint
which does not serve `GetMempoolTx` yields a zero pending amount rather than a
balance error.

#### Deferred Work

D39.8.0c *(Resolved 2026-08-09.)* Pending shielded receipts (§39.8.0.6) are
implemented. The background sync task polls the dictated `GetMempoolTx` stream
after each wallet scan, trial-decrypts every compact output with the wallet's
incoming viewing key, excludes any transaction the wallet database has already
scanned, and rebuilds the pending set wholesale so a mined, dropped or expired
transaction disappears without bespoke invalidation. The total surfaces only in
the non-spendable balance field. Backend failure degrades the pending view to
zero rather than failing the balance.

### 39.8.0.7 In-flight note exclusion for shielded sends

> **Dirty-side derivation record (sanitized).** This subsection was authored by
> the KDF Spec Reader, which was explicitly asked (per `AGENTS.md` §2) to assess
> the upstream approach in this area for correctness rather than only to restate
> it, because R39.8.0p already described the behaviour and the defect persisted.
> It carries only the observable send-path contract -- ordering relative to
> broadcast, release conditions, durability, and serialization scope -- plus two
> behavioural code-quality findings. It carries no private implementation
> expression. The public-crate facts about the selected dependency generation's
> spendable-note exclusion (R39.8.0aq) come from the published package sources
> named in R39.8.0y, not from the corpus. A clean-side implementation shall not
> consume this subsection until the KDF Dirty Gate has passed it.

R39.8.0ao **The contract.** A shielded note spent by a transaction this wallet has
broadcast shall not be offered to note selection for any later shielded
transaction until either the effect of that broadcast is durably recorded in the
shielded wallet database, or the broadcast transaction can no longer be mined. A
second transaction that reuses such a note re-presents the same nullifier and is
rejected by the network, so this exclusion is a correctness condition of the send
path rather than an optimisation.

The exclusion shall cover every path that constructs a shielded transaction --
withdrawal, swap payment, DEX fee or burn output, and refund alike -- because all
of them draw from the same wallet note set. It applies to Light mode
specifically: in Native mode the full node accounts for its own mempool spends
when it reports unspent shielded value, so a wallet-side exclusion is not
required there and shall not be assumed to exist.

Unconfirmed change created by such a transaction remains governed by R39.8.0q and
§39.8.0.6: it is never spendable before it is scanned, and it is surfaced only as
a non-spendable amount.

R39.8.0ap **Ordering: the exclusion is established after a successful broadcast,
not before it.** The in-flight record shall be written only once the backend has
accepted the transaction. Selecting, building and signing shall establish nothing
durable: a build that fails, or a broadcast the backend refuses for any reason,
shall leave the wallet in exactly the state it held before the attempt, with every
selected note immediately available to the next send and the reported balance
unchanged.

This ordering is the deployed contract and it is also the only ordering that is
safe under R39.8.0aq. Recording the exclusion before broadcast obliges the
failure path to release it; in the selected dependency generation of R39.8.0y
there is no supported explicit release, so every rejected or failed send would
instead strand its inputs until their expiry height. Recording after broadcast
converts that failure mode into an ordinary retryable error.

The window this ordering opens -- between choosing notes and recording the
broadcast, during which nothing marks them -- is closed by the serialization
requirement of R39.8.0at, not by moving the record earlier.

R39.8.0aq **Release.** The release in the normal case is the block scan: once the
block containing the transaction is scanned into the shielded wallet database, the
notes it spends are recorded as spent there and the separate in-flight exclusion
for that transaction shall no longer be applied. There shall be no explicit
unlock operation on the send path, and none shall be assumed to exist.

In the selected current generation (R39.8.0y) the wallet database's own
spendable-note exclusion is derived from a spending transaction's mined height,
its expiry height, and the height at which the wallet first observed it.
Re-describing such a transaction as unrecognised or as not being on the main chain
does not release its inputs in that generation. Consequently exactly two releases
exist and both are automatic: the containing block is scanned, or the wallet's
scanned tip passes the transaction's expiry height.

R39.8.0ar **A bounded release is mandatory.** Whatever mechanism records the
in-flight spend, the note shall become selectable again within a bounded height
window even if the transaction is never mined. A record that can only be cleared
by scanning the very transaction that created it is non-conformant: a transaction
that is broadcast and then dropped, evicted, or replaced never arrives, and its
inputs would then be excluded for the remaining life of the wallet with no
operator-visible way to recover them. Expiry-height-bounded exclusion satisfies
this requirement with no additional sweep; any other mechanism shall carry an
equivalent bound of its own.

> **Code-quality finding (informative).** The deployed upstream approach does not
> satisfy R39.8.0ar. It maintains its own durable record of the notes a broadcast
> transaction spends, separate from the shielded wallet database, and that record
> is removed at exactly one place: when a scanned block yields a wallet
> transaction with the same identifier. No expiry, no startup reconciliation, and
> no mempool-disappearance path removes it -- and the scanner's own handling of a
> transaction that vanishes from the backend clears only its internal follow-up
> state, not the spend record. A transaction that is accepted by the backend and
> then never mined therefore removes its inputs from the wallet permanently and
> across restarts. The user-visible failure is a later send that waits, and then
> fails, while the balance reports value that can never be spent; nothing short of
> discarding wallet state restores it. Deriving the exclusion from the
> transaction's expiry height instead -- as the selected dependency generation
> already does -- removes this failure mode entirely, because the exclusion then
> lapses exactly when the transaction itself becomes unmineable.

R39.8.0as **Durability.** The in-flight exclusion shall be durable across a
restart of the daemon, not merely held for the process lifetime, and shall be
scoped to the wallet/account whose notes it describes. Because every release is
height-driven rather than event-driven (R39.8.0aq), an in-memory-only record lost
on restart would silently re-offer a note whose spend is still in flight, and the
resulting second transaction would be rejected with no indication of why. On
native targets this means on-disk storage; on the WASM target it means the
browser-persistent store used for the wallet's other shielded state (R39.8.0j).

R39.8.0at **Serialization.** Note selection, transaction construction, broadcast,
and the recording of the in-flight spend shall together be one critical section
per shielded wallet. A note selected for one send shall not be selectable by
another send until the first has either recorded its broadcast or failed.

Serializing only construction-through-record is insufficient and shall not be
treated as satisfying this rule: two sends that both complete selection before
either broadcasts will choose the same notes, and the second broadcast is then
rejected by the network as a duplicate nullifier. The observable symptom is a
swap payment or withdrawal that fails at broadcast for no reason visible in the
request.

The section shall not be held across a wait for an in-flight spend to clear. That
wait depends on the scanner advancing, and the scanner cannot advance while the
section is held, so holding it would deadlock. A send that finds insufficient
unexcluded value shall therefore wait outside the section and re-select inside it
on each attempt. The wait shall be bounded and shall terminate in a typed
failure rather than blocking indefinitely, and a request that cannot be funded
even with every note unexcluded shall fail immediately with an
insufficient-balance error rather than consuming the wait budget.

> **As built (2026-10-26).** The wait budget is 600 seconds in total, with a
> re-selection attempt every 15 seconds, aligned with the reference's budget
> (Spec Reader triage 2026-10-26). It was 300 and 10 seconds before. The rule
> that a send waits only while this process's in-flight total could cover the
> shortfall is unchanged.

> **Code-quality finding (informative).** The deployed upstream implementation
> does not satisfy R39.8.0at either: its exclusive section covers construction,
> broadcast and recording, but the notes are chosen just before that section is
> entered, deliberately so that the scanner can keep running while a send waits
> for an earlier spend to clear. Two sends that reach selection before either
> broadcasts therefore select the same notes, and the second is rejected at
> broadcast. In practice the exposure is narrowed by a second, coarser property
> of the same design -- after a broadcast the scanner refuses to hand control to
> the next transaction-generation request until that transaction has been scanned
> or has disappeared from the backend, so only one shielded send is genuinely in
> flight at a time -- but that gate is armed by the broadcast, which is precisely
> after the racing window has closed. Re-selecting inside the section, rather than
> before it, closes the window without reintroducing the deadlock the split was
> made to avoid.

> **Upstream divergence (informative).** Upstream keeps the in-flight record in a
> store of its own, outside the shielded wallet database, and reduces the reported
> spendable balance by consulting it. This project instead records the broadcast
> transaction in the shielded wallet database itself and lets that database's own
> spendable-note exclusion do the work (R39.8.0aq). The two are behaviourally
> equivalent for R39.8.0ao, R39.8.0ap and R39.8.0as, and the divergence is
> deliberate: it is what supplies the bounded release R39.8.0ar requires and
> upstream lacks. Its cost is that the exclusion cannot be revoked explicitly,
> which R39.8.0ap's ordering already accommodates.

> **Status update (reloaded).** As of this writing the Light-mode send path
> records the broadcast transaction in the shielded wallet database immediately
> after a successful broadcast, best-effort, so a failure to record degrades to
> the prior behaviour rather than turning a sent transaction into a caller-visible
> error. R39.8.0ap, R39.8.0aq, R39.8.0ar and R39.8.0as are satisfied by that
> arrangement; the best-effort degradation is a narrow, logged residual gap
> against R39.8.0ao, bounded by the fact that the transaction is already on the
> network and the scan corrects the wallet once its block lands.
>
> R39.8.0at is satisfied on the send path: note selection, construction,
> broadcast and recording are performed under one per-wallet exclusive section,
> the sapling-state gate and the confirmation wait are both taken outside it, and
> a shortfall that this process's in-flight spends could still cover releases the
> section, waits, and re-selects on the next attempt under a bounded budget that
> ends in a typed failure. A shortfall that no in-flight spend could cover fails
> immediately rather than consuming that budget.
>
> A withdrawal is built and returned by this daemon but broadcast by its caller
> through `send_raw_transaction`. That second call is handled by this daemon, so the
> broadcast is observable after all: the coin's raw-transaction submission records
> the accepted transaction exactly as the send path does (D39.8.0e, resolved
> 2026-10-19). A withdrawal that is built and never submitted records nothing.

#### Tests

T39.8.0h In-flight exclusion tests shall prove, for a Light-mode shielded coin:
that after a successful broadcast the notes it spends are not offered to a second
send, and the second send reports an insufficient-balance or bounded-wait failure
rather than producing a transaction that reuses them; that a broadcast which the
backend rejects leaves every selected note immediately reusable and the reported
balance unchanged, so an immediate retry succeeds; that restarting the daemon
between the broadcast and the containing block's scan does not re-offer the
excluded notes; that scanning the containing block makes the change spendable and
the spent notes permanently absent, with no double counting across that
transition (jointly with T39.8.0g); and that a transaction which is broadcast and
never mined stops excluding its inputs once the wallet's scanned tip passes its
expiry height, with the recovered value spendable afterwards. A concurrency
fixture shall start two shielded sends whose funding requirements overlap and
prove that exactly one selects each note -- the other either waits and re-selects
different notes or fails with a typed error -- and that neither is rejected at
broadcast for reusing a nullifier.

> **Coverage status (2026-09-22; superseded by the update of 2026-10-25 below, which
> covers every case for the Light-mode send path).** T39.8.0h is **not** yet satisfied. One
> deterministic case is covered: a recorded but unmined transaction is not
> reported as scanned, which is the predicate the in-flight exclusion and the
> pending-receipt accounting both depend on. Every other case the rule names --
> the second send declining the excluded notes, a rejected broadcast leaving them
> reusable, survival across a restart, the mined-and-scanned transition, expiry
> lapse, and the two-send concurrency fixture -- remains unwritten, because each
> needs a harness able to drive a shielded wallet database through
> broadcast/scan/expiry transitions, which this tree does not yet have. Tracked
> as D39.8.0f.
>
> **Update (2026-10-19).** Three withdrawal-side cases are now covered by deterministic
> tests against a real shielded wallet database (a scanned note, a real spend proof, a
> transaction submitted as raw bytes): once a withdrawal's bytes have been accepted, a
> second build before any scan is not handed its note and is told it is short; a
> withdrawal that is built and never submitted strands nothing, and the next build
> reuses its note; and a submitted withdrawal's note is held while its expiry height
> can still be mined in and is free once the chain passes it. The tests need the
> Sapling parameter files and say so and return where they are absent. The
> broadcast-rejection, restart, mined-and-scanned and two-concurrent-send cases remain
> unwritten.
>
> **Update (2026-10-25).** The remaining cases are covered. A harness drives a real Light-mode
> shielded coin over a real wallet database with a scripted backend, so the send path itself is
> exercised: the record is written only once the backend has accepted the bytes, and a rejected
> broadcast records nothing, leaves the balance unchanged and lets an immediate retry reuse the note; a
> second send is refused the committed note and ends in the typed bounded-wait failure, while a
> withdrawal names the cause; a restart (a new coin over the same wallet database) keeps the
> exclusion and, as D39.8.0e records, loses only the explanation; scanning the containing block
> makes the change spendable and the spent note permanently absent with the balance counted exactly
> once; a never-mined transaction releases its note once the scanned tip passes its expiry height and
> not a block earlier; two concurrent sends over two notes select disjoint notes and both broadcast;
> and a send that is waiting re-selects as soon as a concurrent scan frees the change. The only
> production change made for this is that the send path's wait budget and polling periods are now a
> field installed with the former values, pinned by a test, so the bounded wait can be driven in
> milliseconds. The tests need the Sapling parameter files and say so and return where they are
> absent. Native mode is not covered, by design: it needs no wallet-side exclusion.

#### Deferred Work

D39.8.0d Serializing note selection with construction, broadcast and recording
(R39.8.0at) — **resolved.** The send path now selects inside the same exclusive
section that covers construction, broadcast and recording, and keeps both the
sapling-state gate and the wait for an in-flight spend outside it, so the scanner
can still advance while a send waits and each attempt re-selects.

D39.8.0e The exclusion did not cover a withdrawal, which is built and returned as a
signed transaction for the caller to submit. **Resolved 2026-10-19.** The caller's
submission is a `send_raw_transaction` request that this daemon handles and
forwards itself, so the shielded coin observes the broadcast: after the backend
accepts the bytes, the transaction is read back from them (its Sapling spends carry
the nullifiers; its fee is the value leaving the Sapling pool less the transparent
outputs it pays) and recorded under R39.8.0ap, with the same release (R39.8.0aq,
R39.8.0ar). Nothing is recorded at build time, so a withdrawal the user cancels
strands no notes, and a second build made before any broadcast may reuse the first
build's notes -- if both are then submitted, the network rejects the second as a
duplicate nullifier, which is the same outcome as any two conflicting spends and not
a defect of this daemon. Not covered: two daemons running on one seed with separate
wallet databases, which cannot see each other's records, and a transaction whose fee
cannot be derived from its bytes (it has transparent inputs or a non-Sapling shielded
bundle -- none this daemon builds), which is logged and not recorded.

*Naming the cause (2026-10-21).* A withdrawal that is short only because its notes are
committed to such a recorded, unconfirmed transaction is refused with a dedicated
`BalanceLockedByUnconfirmedTransaction` (HTTP 400) naming the coin, the available
amount, the required amount and the amount committed, rather than a bare
`NotSufficientBalance` that contradicts the balance on display. The name is applied only
when the committed amount would have covered the request, so a real lack of funds still
reports as one. The committed amount is read from this process's in-memory record of
in-flight spends, not from the wallet database; after a restart that record is empty and
the same shortfall reports as the bare `NotSufficientBalance`. This is a limit of the
explanation only -- the exclusion itself is durable (R39.8.0as). The swap send path is
unaffected: it waits out such a shortfall (R39.8.0at) and never surfaces this error.

D39.8.0g A built but unsubmitted withdrawal is invisible to the user. The history
(`z_coin_tx_history`) is derived from the scanned chain (R39.8.0q, §39.8.0.6), so a
transaction that has been built and not broadcast appears nowhere. One that was
broadcast appears at once as the first history row, with its final amounts (R39.8.9i,
decided in D39.8.0h), and its change counts as non-spendable balance until mined
(R39.8.0ag1). With the switch `shielded_history_unmined_sends` set to `false` it is not
a history row until mined, as in the reference, and its change is still shown as
non-spendable balance. Showing built-but-unsent
transactions is a user-experience gap, deliberately out of scope here: it would need
a record of unbroadcast builds, which is exactly the state R39.8.0ap declines to
create.

D39.8.0f T39.8.0h is only partially covered; see the coverage status under that
rule for what exists and what does not. The missing cases share one prerequisite:
a test harness that can put a shielded wallet database into a chosen state --
notes received, a transaction recorded but unmined, a block scanned, a scanned
tip advanced past an expiry height -- and re-run note selection against it. The
existing fixtures build such a database only for history and scan-state
assertions and cannot express a broadcast, a restart, or two concurrent sends.
Until this is closed the in-flight exclusion rests on the single predicate test
plus the type checker, which is weaker coverage than a funds-moving path in this
project is expected to carry; the shortfall is in test coverage only and does not
describe a known defect in the implemented behaviour.

*As built (2026-10-25).* The harness exists and every case T39.8.0h names is covered for the
Light-mode send path (see the coverage update under T39.8.0h); this item is closed.

D39.8.0j **Two instances on one seed, and a sync right after a send -- after the beta.**
*(Postponed 2026-10-25.)* Two daemons running on one seed with separate wallet databases
cannot see each other's in-flight records (D39.8.0e), so each may select a note the other
has already spent; nothing detects that case yet (one cheap signal would be a nullifier on
chain that this wallet did not create). Running one instance per seed is a known limitation
of the release. Starting a sync right after a broadcast, so that the change becomes
spendable sooner, is not implemented either; it changes latency only, because the
exclusion is recorded in the wallet database. Both items are tracked in
`docs/plans/shielded-in-flight-note-locking.md`.

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
| `sync_status` | object | History-sync state, tagged on field `state` with optional `additional_info`. Normally the terminal `Finished` state, because a shielded coin is only active after its initial scan completes (§39.3). It is not always `Finished`: a background pass that fails after activation reports the error state, with a message and the height scanned so far, and one still catching up reports the in-progress state (R39.8.0au, §39.8.0.5). |
| `limit` | integer | Echo of the effective page limit. |
| `skipped` | integer | Number of entries skipped ahead of this page. |
| `total` | integer | Total number of known shielded transactions. |
| `total_pages` | integer | Total page count for `total` at the effective `limit`. |
| `paging_options` | object | Echo of the effective paging selector. |

R39.8.7 Each entry in `transactions` is a **shielded-coin transaction detail**
object whose shape differs from the generic v2 history entry. Its fields are:

| Field | JSON type | Description |
|-------|-----------|-------------|
| `tx_hash` | string | Transaction hash, hexadecimal, in **big-endian display order** (R39.8.7a). |
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

R39.8.0an A caller-supplied shielded sync start shall survive subsequent
activations that do not repeat it.

Activation may skip forward to the recent scan window rather than replay long
-dead history, but only for a wallet still anchored where it was born -- at the
coin configuration's checkpoint, or Sapling activation when none is declared. A
wallet deliberately re-anchored by a caller-supplied sync start shall keep that
anchor.

The two states are otherwise indistinguishable: a wallet part-way through a deep
rescan is empty and anchored far behind the recent window, exactly like a fresh
one, until the rescan reaches its first transaction. Skipping forward on that
basis discards the rescan silently, and the case it damages is the one that
needs it most -- restoring a seed whose funds predate the recent window, where
the discarded rescan is the only thing that would have found them and the
balance simply stays at zero.

R39.8.0aw When a shielded coin is activated without `sync_params` and the
shielded scan start is decided by the default recent scan window -- because the
wallet has no scanned state yet, or because the empty-wallet skip-forward of
R39.8.0an applied -- the node shall log one warning for that activation. The
warning names the ticker; states that no `sync_params` were supplied; gives the
start height and the window size in blocks; states that funds received before
that height will not be visible; and states that an existing wallet should be
re-activated with `sync_params` (`{"height": N}`, `{"date": T}` or `"earliest"`)
preceding its first funds. No warning is logged when `sync_params` were supplied,
when `skip_sync_params` is set, when an already-scanned wallet resumes, or for the
periodic post-activation top-up passes. The warning changes no behavior; the
activation result's `first_sync_block` (R39.8.0h) remains the machine-readable
signal.

> **Provenance (informative).** R39.8.0aw is clean-room-original hardening written
> without reference to the upstream implementation; it is not a parity item and
> needed no Dirty Gate pass.

> **Upstream parity position (informative).** The skip-forward rule above is
> **unconstrained** by the reference: the reference has no counterpart to it.
> The reference decides whether to re-resolve the default recent window from
> the compact-block cache's contents alone (R39.8.0av's code-quality finding),
> never from whether the wallet found any note, so it has no
> "scanned-but-empty" state to skip forward from and draws no distinction
> between "never scanned" and "scanned and empty". Reloaded's rule is
> therefore an addition, not a divergence from a stated contract -- and it is
> the *narrow* form of the same guard: it fires only for a wallet still
> anchored where it was born, where the reference's cache-keyed test would
> re-anchor a deliberately re-anchored wallet too. The window that the rule
> skips forward to is bound by R39.8.0av, where the size of that window is
> recorded as a Reloaded-specific value.

R39.8.0am Shielded note decryption shall accept both the pre- and post-ZIP-212
note plaintext versions at every height, for every shielded coin whose network
accepts both.

The dictated chain states this rule explicitly in its own plaintext-version
check: both lead bytes are valid at all heights. It has no Canopy upgrade, and
the reference library derives ZIP-212 enforcement solely from Canopy activation
-- so with Canopy absent the reference library reports enforcement *off*, which
accepts only the pre-ZIP-212 lead byte. A note carrying the post-ZIP-212 byte
then fails to trial-decrypt, and because a failed trial decryption is
indistinguishable from a note that simply is not ours, the payment does not
error: it never appears at all. Every payment from a current wallet on that
network is invisible.

The wallet shall therefore derive its decryption parameters separately from the
coin's consensus parameters, reporting the enforcement state that accepts both
versions, and shall apply them at every trial-decryption site: the compact-block
scanner, mempool pre-confirmation detection, full-transaction decryption, and
outgoing-output recovery.

Those decryption parameters shall **never** reach transaction construction. The
consensus branch ID is resolved as the branch of the last *active* upgrade, so
parameters that report Canopy active would move every constructed transaction
off the Sapling branch and invalidate it. A test shall pin both halves: that the
coin's real parameters still resolve to the Sapling branch, and that the
decryption parameters do not.

R39.8.0al A wallet whose scanned chain no longer agrees with the network shall
rewind past the divergence and re-anchor, not fail.

A light wallet that syncs to the chain tip records the tip block. An ordinary
one-block reorg then leaves it holding a block the network has dropped, and the
tree state the backend reports for that height no longer matches what the wallet
stored. Treating that as an error is unrecoverable by construction: the stored
state is never repaired, so every later pass repeats the same comparison and the
wallet can never sync again.

The comparison shall therefore be made on **every** fetch, against the tree state
the backend reports now. The chain state a scan leaves in memory is what the scan
derived from cached blocks, not what the chain says, and shall never stand in for
the backend's answer: trusting it skips the comparison for the rest of the session
and no divergence is ever detected after a scan.

On a mismatch between the stored block hash and the one the backend reports at
the anchor height, the wallet shall:

- rewind the wallet database to the height `D` blocks below the anchor and
  re-anchor there, repeating at most `N` times, where `N` is 3 and `D` is one less
  than the number of commitment-tree checkpoints the wallet database retains
  (100), i.e. 99. Checkpoints sit at distinct heights at or below the scan tip, so
  a wallet holding all of them has its oldest at or below `tip - 99`, and a rewind
  of `D` from the tip always lands on a retained checkpoint: it never leaves the
  retained window, below which the backend falls back to the block before the
  wallet's earliest note (next bullet). A depth equal to the window (100) lands
  exactly one block below the window of a wallet kept current by the background
  sync, which holds a checkpoint for nearly every block, and would make every
  one-block reorg rescan that wallet's whole note history. Observed ARRR reorgs
  were one block deep, and rescanning at most `D` compact blocks is cheap;
- accept whatever height at or below the request the backend rewinds to. The
  backend truncates to the newest scanned block at or below the request at which
  its commitment trees can be cut: a block with a retained checkpoint or, below
  the retained checkpoints, a block before the wallet's earliest note with a
  recorded tree position. For a wallet holding notes that can be far below the
  request; the range between is rescanned like any rewound range;
- when the backend refuses a rewind because no block at or below the request
  qualifies, and reports the oldest checkpoint it does hold, rewind to that
  checkpoint instead of giving up. The first rewind cannot be refused (it lands
  inside the retained window by construction), but a later attempt, or a young
  wallet whose window has not filled yet, can ask for a height below everything
  retained; without this redirect such a refusal went straight to the rebuild
  below;
- stop rewinding as soon as a rewind cannot move below the divergence; the loop
  is bounded by `N` and never unbounded;
- delete, in the same step, every compact block cached above the height the wallet
  rewound to. They were fetched for the chain being abandoned; left in place they
  feed the orphaned block back to the scanner and the wallet fails the same way on
  every pass;
- if the divergence persists after `N` rewinds, or the rewind is refused beyond
  repair, or it cannot move below the divergence, **rebuild** the wallet database.
  A divergence deeper than the retained window cannot be repaired locally;
- resume fetching from the height actually anchored at, never from the height
  originally planned, so the rewound range is rescanned rather than skipped. The
  anchor height the check returns is always the height of a real anchor; no value,
  including 0, signals "rebuilt".

The rebuild shall re-anchor the wallet at its **sync start** and never at height
0. The sync start is the birthday of the wallet's account: the height from which
the wallet originally began scanning (the coin's checkpoint, the activation's
`sync_params`, or the default recent window, whichever created the wallet,
R39.8.0g/h/av). It is read before anything is replaced. The rebuild shall fetch and
validate the tree state at the block below the sync start before it touches the
wallet database, so an unreachable or malformed answer leaves the existing wallet
as it was; then it shall replace the wallet database with a fresh one holding the
account at that birthday and delete the compact cache, and the next scan resumes
at the sync start. Notes below the sync start are out of scope, exactly as for any
wallet anchored there. No shielded value in the covered range is lost, because
every note in it is recoverable from the chain with the wallet's own viewing key.
A wallet that records no sync start, or whose sync start leaves no block below it
to anchor on, shall fail the pass with an error rather than be rebuilt at a
guessed height; it is not modified. A rebuild shall keep exactly one backup of the
previous wallet database (`.chain-divergence.<ms>.bak`), never one per pass, and
shall keep no backup of the compact cache (R39.8.0n).

The divergence shall be reported with both the stored and the reported block
hash, in display byte order, so the two can be distinguished and looked up.

R39.8.0ax **A wallet database that has not scanned a block shall be replaced
without a backup.** *(binding; Reloaded-original -- it bounds the backup files
of Reloaded's own rebuild paths.)* A wallet database can
hold its account and nothing else: after the rebuild of R39.8.0al, and after a
process stopped between importing the account and scanning the first block. The
anchor check of R39.8.0al replaces such a database with a fresh one before it
fetches the checkpoint again, and does so on every pass until a scan succeeds. The
replaced database holds nothing the wallet cannot recreate -- no scanned block, no
transaction and therefore no note, only an account derived from the wallet's
viewing key -- so it shall be deleted together with its SQLite sidecar files, not
kept as a backup. Keeping one per pass grows without bound while the scans keep
failing, the same growth R39.8.0n removed for the compact cache. The check that
the database holds no scanned block and no transaction shall be made under the
same per-database guard as the replacement (R39.8.0ad), so a concurrent scan
cannot add data between the check and the delete; a database that fails the
check shall be left untouched and the pass shall fail. A rebuild's own backup of
the previous, scanned wallet database (R39.8.0al) is unaffected and stays the only
backup a rebuild leaves.

R39.8.0ak The Sapling commitment tree carried by a lightwalletd `TreeState`
shall be validated before it is accepted as the wallet's sync anchor. The
wallet shall reject, without mutating any persisted or in-memory chain state:

- a payload whose length equals a Sapling commitment root (32 bytes), which is
  not a serialized commitment tree;
- a payload with bytes remaining after a complete commitment tree has been
  read;
- an empty payload at a height above Sapling activation.

An empty payload at or below Sapling activation denotes the empty tree and
shall be accepted.

The first two rejections are load-bearing rather than defensive. The dictated
server fills this field through `preferredTreeState(finalState, finalRoot)`,
which substitutes the commitment **root** whenever the node cannot supply a
frontier, and it does so identically in `GetTreeState` and
`GetBridgeTreeState` — so no alternative RPC avoids it. A root is
indistinguishable from a tree by inspection, and the reference tree reader
returns as soon as it has read its three members without requiring the buffer
to be exhausted: a root whose leading bytes are zero therefore parses
*successfully* as the **empty** tree while the remainder is discarded,
anchoring the wallet on a tree of size zero. Length and full-consumption checks
are the only things that separate the two cases.

Where the server also populates a Sapling frontier field, a value differing
from the tree field shall be recorded as a warning and the tree field shall
remain authoritative.

**The substituted root is told apart by the Sapling frontier field, not by
length.** The server's protocol definition says the frontier field exists to
cross-check the tree field. Measured on mainnet (Sapling activation 152855) and
on the Pirate test chain (activation 61), the frontier is **empty exactly when
the tree field is the substituted root** and populated whenever the tree is
genuine: at `activation - 1` the tree is 64 hex characters and the frontier is
empty; at the activation height the tree is `000000` and the frontier is
`000000` (a real empty tree); at height 4 100 000 both are the same 1028
characters. A 32-byte tree with an empty frontier is therefore the
substitution, and a short tree with a matching frontier is a legitimate empty
tree. A server that sends no frontier at all but a tree of a length other than
32 bytes is still validated on the tree alone, as above.

The anchor for a scan starting at the Sapling activation height (which is where
`"earliest"`, a `date` before activation, and a `height` below activation all
land; R39.8.0g) is the tree at `activation - 1`. No Sapling commitment exists
below activation, so that tree is the empty tree by definition; the server
returns the substituted root there on mainnet and on the test chain. A
substituted root at a height **below** Sapling activation shall therefore be
read as the empty tree, only its block hash being used. A substituted root at
or above activation cannot be resolved the same way and shall be rejected; the
error shall say that the server substituted the root, and shall tell the
operator to use `"earliest"` (naming the activation height), a height at or
above it, or another server.

R39.8.7a `tx_hash` shall be rendered in the **big-endian display byte order** used
by block explorers, by the transparent-coin history of `my_tx_history`, and by
this coin's own `withdraw` and `send_raw_transaction` responses -- **not** in the
internal little-endian order the shielded wallet database stores.

The shielded wallet database (`zcash_client_sqlite`) persists
`transactions.txid` little-endian, so the stored column shall be byte-reversed
before it is serialized. The two orders are byte reversals of one another and
are therefore indistinguishable by length or charset: a wrong order is not a
malformed value, it is a valid-looking hash that resolves to nothing. A
regression fixture shall use transaction IDs that are **not** byte-order
symmetric, because a symmetric fixture (for example a repeated single byte)
cannot distinguish the two orders at all.

`internal_id` is unrelated to `tx_hash` and is not a transaction hash: it is the
wallet database's own row identifier, used only for `FromId` paging (R39.8.2).


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
tip; otherwise it is `current_block + 1 - block_height`. The `current_block`
used here is the single lightwalletd tip (R39.11.8(f), R39.11.9), which is also the
source of the scanned blocks, so an entry cannot be ahead of the height used to
count its confirmations.

#### 39.8.5.1 Unmined transactions, same-block order, and amounts of an own send

> **Dirty-side derivation record (sanitized).** R39.8.9f--R39.8.9h, the notes
> under them, T39.8.9a--T39.8.9d and D39.8.0h were authored by the KDF Spec
> Reader under the chapter-01 two-team workflow (§1.4A), from the reference
> states `v2.6.0-beta` and `v3.0.0-beta` (chapter 02 §2.15A), which agree on
> every fact bound below: the shielded history query, its paging, and the
> post-broadcast handling of an own send are identical at the two states. They
> carry only the externally observable contract of the history page; no
> implementation expression was carried across. The KDF Dirty Gate passed
> this subsection on 2026-10-26 (discretionary-expression measure 0.02, no
> leaks to scrub); the provenance footer records its scope attestation.

R39.8.9f **Unmined transactions are not history rows.** A shielded transaction
this wallet has broadcast, and a shielded transaction addressed to this wallet
that the backend reports only as a mempool member, shall not appear in
`z_coin_tx_history` at any position until the transaction has been mined and
its block has been scanned into the shielded wallet database. The history is
derived from scanned blocks alone (R39.8.0q, §39.8.0.6, D39.8.0g): the
post-broadcast state of an own send is held by the in-flight exclusion record
of §39.8.0.7 and by the pending-receipt balance path of §39.8.0.6, neither of
which feeds the history query. Consequently every history row carries a
positive `block_height` (the height of the block that mined it), a `timestamp`
equal to that block's time, and a `confirmations` value computed by R39.8.9e
that is at least 1 whenever the backend tip is at or above the row's height.
There is no row with `block_height` 0, no row with a zero `timestamp`, and no
row with zero `confirmations` other than one whose height exceeds a stale tip.

> **Reloaded divergence (2026-10-26).** Reloaded records an own shielded send in
> the wallet database at broadcast time and serves it as a history row before it
> is mined, ordered by the mined height with an absent height sorting below every
> real height, so the unmined row appears **last** on the final page. The
> reference has no such row. Either outcome of D39.8.0h cures the divergence:
> removing the unmined row restores reference parity; keeping it as a deliberate
> Reloaded feature requires it to sort **ahead of** every mined row (the
> unconfirmed-first convention the generic histories use, chapter 38 §38.5A
> R38.5A.1 and R38.5A.6) and to carry the amounts R39.8.9g binds. Sorting an
> unmined row last is non-conformant under either choice.

> **Scope since D39.8.0h (2026-10-26).** The maintainer kept the row. R39.8.9f
> states the reference contract and binds a Reloaded node only with
> `shielded_history_unmined_sends` set to `false`; at the default, an own send
> is governed by R39.8.9i. A mempool-only transaction addressed to this wallet
> is never a row under either setting.

R39.8.9g **Amounts of an own send.** Because an own send becomes a history row
only once mined (R39.8.9f), the row's amounts are computed once, from scanned
wallet state, by R39.8.9b: `spent_by_me` is the total value of the wallet notes
the transaction consumed, `received_by_me` is the total value of the wallet
notes the transaction created -- the change note included -- and
`my_balance_change` is their difference, which equals the negated sum of the
value paid to other parties and the fee. No earlier figure for the same
transaction is ever served: in particular the history never reports the whole
value of the consumed notes as the wallet's loss while the transaction is
unmined and the net figure afterward. A client that merges successive pages by
position therefore never sees a row whose amounts change between two reads.

> **Reloaded divergence (2026-10-26).** Reloaded's unmined row (see the note
> under R39.8.9f) reports the full value of the consumed notes as the loss
> (observed: `-0.00198` for a send whose true net change is `-0.00051`) and the
> correct net figure only after the transaction is mined. The mined figure
> matches R39.8.9b and R39.8.9g; the unmined figure matches nothing in the
> reference. If D39.8.0h keeps the row, its amounts shall be the mined-row
> amounts from the start: the change the wallet pays itself is known when the
> transaction is constructed (it is the value of the change output) and shall
> be counted in `received_by_me`.

R39.8.9h **Same-block tie-break and platform note.** The stable order R39.8.9a
requires within one block is **ascending** wallet-database transaction
identifier (the `internal_id` of R39.8.7): among transactions mined in the same
block, the one the wallet recorded first is served first. The complete key is
therefore (block height descending, `internal_id` ascending); `FromId` paging
locates the supplied `internal_id` in that same key order, and `total` counts
every row of the wallet's transaction table. The order is a total order over
distinct `internal_id` values and is therefore deterministic across repeated
requests and across restarts.

> **Reference platform difference (informative).** The reference's native
> (SQLite) history path serves the key above. Its browser (WASM) path orders by
> descending wallet-database row identifier alone, without grouping by block,
> and its `FromId` paging is keyed to that row order. The native order is the
> one R39.8.9a and
> R39.8.9h bind; a Reloaded WASM build shall serve the native order too, so the
> same wallet produces the same page on both platforms.

D39.8.0h **Whether to keep a visible unmined own send is a maintainer
decision.** The reference shows nothing between broadcast and mining
(R39.8.9f, D39.8.0g); Reloaded currently shows a row, in the wrong position and
with the wrong amounts (notes under R39.8.9f and R39.8.9g). The two conforming
outcomes are stated in those notes. Whichever is chosen, the row -- if any --
shall never be served last, and the amounts of a transaction shall never change
between its unmined and mined states.

**Decision (maintainer, 2026-10-26): keep the row.** By default a Reloaded node
serves an own shielded send from broadcast on, under R39.8.9i; the reference
behaviour of R39.8.9f stays reachable through the switch of R39.8.9i (f). This
is a deliberate divergence placed by the second arm of the default-side rule
(`docs/COMPAT_SWITCHES.md`): it changes no wire format and no other node, and
only shortens the time before a user sees their own send. It is recorded in
`docs/GLEEC_COMPATIBILITY.md`.

R39.8.9i **Visible unmined own send (Reloaded divergence, default).** This rule
is clean-room-original and applies while the switch of (f) is on:

- (a) *Which rows.* A transaction this wallet built and recorded in the shielded
  wallet database at broadcast (§39.8.0.7) shall be served by
  `z_coin_tx_history` while it is unmined. A transaction addressed to this
  wallet that is known only from the mempool is still not a row (R39.8.0ag):
  only own sends are.
- (b) *Position.* Unmined rows shall be served ahead of every mined row; several
  unmined rows are ordered by ascending `internal_id`. Mined rows follow in the
  order of R39.8.9h. `FromId` paging and `total` use this same sequence.
- (c) *Fields.* An unmined row reports `block_height` 0, `timestamp` 0 and
  `confirmations` 0.
- (d) *Amounts.* From its first appearance the row reports the amounts
  R39.8.9g binds for the mined row: the value of the change output the wallet
  pays itself is known when the transaction is built and shall be counted in
  `received_by_me`. Recording that change at broadcast shall not make it
  spendable before the block is scanned (R39.8.0q) and shall not count it twice
  in the balance (the pending-receipt amount of R39.8.0af and the recorded
  change are one amount, never added together).
- (e) *Life of the row.* When the block that mines the transaction is scanned,
  the same row -- same `internal_id`, same amounts -- moves to its block's
  position under R39.8.9h; at no point are two rows served for one
  transaction. A transaction that expires unmined (the chain passes its expiry
  height, R39.8.0ar) shall no longer be served.
- (f) *Opt-back switch.* The top-level `MM2.json` boolean
  `shielded_history_unmined_sends` selects this rule. It defaults to `true`;
  only the JSON value `false` turns it off, and then `z_coin_tx_history`
  follows R39.8.9f: no unmined row is served and `total` counts mined rows
  only. The switch changes nothing in the balance, in note selection or in any
  other method.

> **Compatibility:** for behavior matching GLEEC KDF, set
> `shielded_history_unmined_sends` to `false`; an own shielded send then
> appears in `z_coin_tx_history` only after it is mined.

T39.8.9a *Unmined own send is absent.* A test shall broadcast a shielded send
from a wallet whose database is in a known scanned state, then query
`z_coin_tx_history` before any further block is scanned: the response shall not
contain the broadcast transaction at any position, `total` shall be unchanged,
and the pending change shall be visible only through the balance path of
§39.8.0.6. After the block containing the send is scanned, the same query shall
return the row first (newest block), with `block_height` equal to that block's
height, `timestamp` equal to that block's time, `confirmations` at least 1, and
the amounts of R39.8.9g. This form of the test applies with the switch of
R39.8.9i (f) set to `false`. With the switch at its default (D39.8.0h keeps the
row), the first half shall instead assert that the row is served first, reports
`block_height`, `timestamp` and `confirmations` 0, carries the mined-row
amounts, and keeps those amounts and its `internal_id` after mining; a further
case shall assert that a transaction which expires unmined is no longer served.

T39.8.9b *Same-block tie-break.* A test fixture with three wallet transactions
mined in one block and one in the following block shall return the later block's
row first and then the three same-block rows in ascending `internal_id`, both
for `PageNumber` paging and for `FromId` paging that starts inside the block;
the fixture shall assign `internal_id` values whose ascending order differs from
the ascending order of their transaction hashes, so a hash-ordered tie-break
fails the test.

T39.8.9c *Deterministic order.* Two consecutive `z_coin_tx_history` requests
with the same arguments against an unchanged wallet database, and the same
request after closing and reopening the database, shall return identical row
sequences.

T39.8.9d *Amounts never change.* For a send of amount `A` with fee `F` from a
single note of value `V > A + F`, the only history row ever served for that
transaction shall report `spent_by_me = V`, `received_by_me = V - A - F`, and
`my_balance_change = -(A + F)`; a test shall assert that no response at any
point in the transaction's life reports `my_balance_change = -V`.

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

---

## Part D -- Activation wire contract (T-PORT)

## 39.10 `task::enable_z_coin` activation wire contract

> **Dirty-side derivation record (sanitized).** This section was authored by
> the KDF Spec Reader under the chapter-01 two-team workflow, at the GLEEC KDF
> release tags `v3.0.0-beta` (reference) and `v2.6.0-beta` (legacy
> cross-check), for the cases where the public KDF API reference is silent,
> ambiguous or self-contradictory. It carries only the externally observable
> activation wire contract (method routing, request field names, types and
> defaults, in-progress values, result shape, error discriminants, error-data
> shapes, HTTP statuses, failure-to-discriminant mapping) and behavioral
> code-quality findings. It carries no private implementation expression. It
> passed the KDF Dirty Gate before clean-side implementation.

> **Sources and reference version.** The public KDF API reference (the
> task-managed `enable_z_coin` page, the `ActivationRpcData` and
> `ZcoinRpcModeEnum` common structures, and the error-type catalogue) governs
> where it is explicit. Where it is silent, ambiguous, or self-contradictory, the
> maintainer's decision is to match the GLEEC KDF release tag **`v3.0.0-beta`**
> (the most recent release tag of that lineage). The activation wire surface
> bound in this section -- method names, request schema, in-progress values,
> result shape, error discriminants, error-data shapes and HTTP statuses -- is
> identical at the netid-8762 legacy reference **`v2.6.0-beta`**, so no
> per-reference split is needed.
>
> **Shared rules not restated here.** The mmrpc 2.0 envelope, the error
> envelope (`error`, `error_path`, `error_trace`, `error_type`, `error_data`),
> the HTTP-status policy, the task-status envelope
> (`{"status": "Ok"|"InProgress"|"UserActionRequired"|"Error", "details": ...}`,
> with an `Error` status carrying the full error object directly in `details`),
> the `init` success shape `{"task_id": <integer>}`, the
> `user_action`/`cancel` success shape `"success"`, the `forget_if_finished`
> semantics, the request-parse failure `InvalidRequest`, and every
> framework-level error of `status`, `user_action` and `cancel`
> (`NoSuchTask`, `TaskFinished`, the "task is not awaiting a user action"
> class, and their error-data shapes and HTTP statuses) are bound by
> [Chapter 55](55-coin-activation-wire-contract.md) §55.1--§55.2. The
> per-family unresponsive-backend summary is in §55.3; this section is the
> authoritative Z-coin row of that table.

### 39.10.1 Method strings and routing

R39.10.1 The canonical activation methods are the four mmrpc 2.0 methods
`task::enable_z_coin::init`, `task::enable_z_coin::status`,
`task::enable_z_coin::user_action` and `task::enable_z_coin::cancel`. They
shall be routed on **every build target on which the shielded coin can be
activated** -- native and WASM (§39.6.1) -- not on native only.

R39.10.2 The reference lineage routes **no** flat Z-coin activation aliases.
Reloaded keeps the three flat names it inherits from the GPLv2 baseline anchor
(`init_z_coin`, `init_z_coin_status`, `init_z_coin_user_action`) as a
documented backward-compatibility extension. Each flat name shall behave
exactly like its `task::enable_z_coin::*` counterpart (same request, same
success and error shapes, same HTTP statuses, same task registry). There is no
flat cancel alias. The flat names shall be reachable on the same targets as the
canonical names.

### 39.10.2 `init` request

R39.10.3 `task::enable_z_coin::init` takes `params` with:

| Field | Type | Required / default | Meaning |
| --- | --- | --- | --- |
| `ticker` | string | required | coin to activate; must have a ZHTLC entry in the coins config |
| `activation_params` | object | required | the Z-coin activation parameters of R39.10.4 |

R39.10.4 `activation_params` members (all at the top level of
`activation_params` unless stated otherwise):

| Field | Type | Required / default | Meaning |
| --- | --- | --- | --- |
| `mode` | object `{ "rpc": string, "rpc_data": object }` | **optional**; when absent it behaves as Light mode with empty server lists (which then fails at status level, R39.10.13) | backend selection (R39.2.2) |
| `mode.rpc` | string: `"Native"` or `"Light"`; `"Electrum"` is accepted as a synonym of `"Light"` | required inside `mode` | `"Native"` exists only on native builds: on a WASM build it is an unknown variant and the request fails to parse (`InvalidRequest`, §55.1) |
| `mode.rpc_data` | object | required for Light; absent for Native | Light-mode backend settings (rows below) |
| `rpc_data.electrum_servers` | array of Electrum server objects (the same server object as the UTXO Electrum activation, §55.5 R55.5.10); `servers` is accepted as a synonym | required in Light | transparent-side backend (see the supersession note below) |
| `rpc_data.min_connected` | integer (unsigned) | optional; default 1 | minimum number of Electrum connections to keep (see the supersession note below) |
| `rpc_data.max_connected` | integer (unsigned) | optional; default = number of listed Electrum servers | maximum number of Electrum connections (see the supersession note below) |
| `rpc_data.light_wallet_d_servers` | array of strings (lightwalletd gRPC endpoint URLs) | required in Light | shielded-side backends (R39.2.4) |
| `rpc_data.sync_params` | `{"height": u64}`, `{"date": u64}` or `"earliest"` | optional | sync start point (R39.6.2, R39.8.0g) |
| `rpc_data.skip_sync_params` | boolean | optional; default false | resume from prior local sync state (R39.6.2) |
| `required_confirmations` | integer (unsigned) | optional; coins-config default | swap confirmations |
| `requires_notarization` | boolean | optional; coins-config default | accepted; no effect on a ZHTLC coin, which has no notarized confirmation count (R39.11.4e) |
| `zcash_params_path` | string | optional; platform default directory | Sapling parameter directory on native builds; ignored on WASM (parameter availability on WASM is governed by §39.6.1) |
| `scan_blocks_per_iteration` | integer, unsigned 32-bit, **non-zero** | optional; default 1000 | blocks per scan batch; the value `0` is a request-parse failure (`InvalidRequest`), not a silent clamp |
| `scan_interval_ms` | integer (unsigned 64-bit) | optional; default 0 | pause between scan batches |
| `account` | integer (unsigned 32-bit) | optional; default 0 | HD shielded account index (R39.6.4 §2); ignored under the single-key policy |

> **Supersession (implemented, Step C).** The `rpc_data.electrum_servers` row
> and the `rpc_data.min_connected` / `rpc_data.max_connected` rows are superseded
> by §39.11: the members are parsed and ignored, as are the `"Electrum"` /
> `servers` synonyms, so the request stays wire compatible. `electrum_servers`
> may be absent.


R39.10.5 Fields that the public `ActivationRpcData` table lists but that have
no Z-coin meaning -- `tx_history`, `get_balances`, `gap_limit`,
`scan_policy`, `path_to_address`, `min_addresses_number` -- and any other
unknown member shall be **accepted and ignored** (no strict-field rejection).
In particular `get_balances: false` does not remove `wallet_balance` from the
result, and Z-coin activation never starts the generic transaction-history
machinery (shielded history is served by `z_coin_tx_history`, §39.8). A
`ws_url` member that some public-docs examples place on an Electrum server
object is not a recognised member of that object and is ignored.

R39.10.6 The signing key is never taken from the request: it comes from the
wallet key context the daemon was started with -- the single-key ("Iguana")
policy or the HD policy (R39.6.4 §2). Hardware-wallet and WalletConnect
policies are not supported for the shielded coin (R39.10.11).

> **Reloaded extension (informative).** Reloaded additionally accepts the
> spellings `blocks_per_iteration` (for `scan_blocks_per_iteration`) and
> `scan_interval` / `inter_iteration_interval_ms` (for `scan_interval_ms`), see
> R39.6.2. These are a superset of the reference wire and stay permitted.

### 39.10.3 `status` in-progress values

R39.10.7 While the task runs, `status` returns `"status": "InProgress"` with
`details` set to one of the following values. Unit values serialize as a bare
JSON string; the data-carrying values serialize as a single-key JSON object
keyed by the value name:

| `details` value | JSON form | When |
| --- | --- | --- |
| `ActivatingCoin` | `"ActivatingCoin"` | initial state, coin construction |
| `UpdatingBlocksCache` | `{"UpdatingBlocksCache": {"current_scanned_block": u64, "latest_block": u64}}` | compact-block download (R39.3.1) |
| `BuildingWalletDb` | `{"BuildingWalletDb": {"current_scanned_block": u64, "latest_block": u64}}` | wallet-database scan (R39.3.1) |
| `TemporaryError` | `{"TemporaryError": <string>}` | a retriable shielded-backend or scan failure during the initial sync (R39.10.14); the string is human-readable and not bound |
| `RequestingWalletBalance` | `"RequestingWalletBalance"` | shielded sync finished; reading height and balance |

R39.10.8 The names `Finishing`, `WaitingForTrezorToConnect` and
`WaitingForUserToConfirmPubkey` belong to the Z-coin status vocabulary but a
Z-coin activation never enters them; the awaiting-user-action values
`EnterTrezorPin` / `EnterTrezorPassphrase` are likewise never entered, so
`status` never reports `UserActionRequired` for a Z-coin task. A
`user_action` sent to a live Z-coin task (in the shared hardware-wallet
user-action shape of §55.2) therefore always fails with the framework's
"task is not awaiting a user action" error of §55.2. The public-docs
in-progress list for this method also names `FollowHwDeviceInstructions`; that
value is not part of the Z-coin vocabulary and shall not be emitted.

### 39.10.4 `status` success (`Ok`) shape

R39.10.9 On success `status` returns `"status": "Ok"` with `details`:

```json
{
  "ticker": "<string: the activated ticker>",
  "current_block": <integer: transparent-backend chain height>,
  "wallet_balance": {
    "wallet_type": "Iguana",
    "address": "<string: the wallet's Sapling payment address>",
    "balance": { "spendable": "<decimal string>", "unspendable": "<decimal string>" }
  },
  "first_sync_block": { "requested": <integer>, "is_pre_sapling": <bool>, "actual": <integer> }
}
```

- `wallet_balance` is **always** the single-address form with
  `"wallet_type": "Iguana"`, also when the shielded key is HD-derived
  (R39.6.4 §2); there is no per-account HD balance report and no ticker-keyed
  balance map. Balances are decimal strings.
- `first_sync_block` is **always** present (R39.8.0h). In Native mode the
  reference reports `requested` = `actual` = the configured checkpoint height
  (or the Sapling activation height when the coin config has no checkpoint) and
  `is_pre_sapling: false`.

### 39.10.5 Error contract

R39.10.10 **`init` (mmrpc-level) errors.** `task::enable_z_coin::init`
returns these synchronously as an mmrpc error response; everything else is
reported later through `status`:

| `error_type` | When | `error_data` | HTTP | Placement |
| --- | --- | --- | --- | --- |
| `CoinIsAlreadyActivated` | the ticker is already active when `init` is called | object `{"ticker": string}` | 400 | mmrpc |
| `CoinConfigIsNotFound` | no coins-config entry for the ticker | string: the requested ticker | 400 | mmrpc |
| `CoinProtocolParseError` | the coins-config `protocol` does not parse (including a ZHTLC entry without a valid `protocol_data`, R39.1.2) | object `{"ticker": string, "error": string}` | 400 | mmrpc |
| `UnexpectedCoinProtocol` | the coins-config protocol is valid but not ZHTLC | object `{"ticker": string, "protocol": object}` where `protocol` is the coin's protocol object as written in the coins config | 400 | mmrpc |
| `Internal` | the activation context or task registry cannot be used, or the task cannot be spawned | string | 500 | mmrpc |
| `InvalidRequest` | `params` fail to deserialize (R39.10.3--R39.10.4) | per §55.1 | per §55.1 | mmrpc (dispatcher) |

R39.10.11 **Task-level errors (`status` with `"status": "Error"`).** A failed
activation is reported in the `details` of a `status` response, which itself
is an HTTP **200** response (§55.1). The HTTP column below is the status the
same discriminant carries if it is ever returned at mmrpc level; it never
changes the 200 of the `status` call that reports a task error.

| `error_type` | When (failure class) | `error_data` | HTTP if at mmrpc level |
| --- | --- | --- | --- |
| `CoinCreationError` | the coin could not be constructed: Sapling parameters missing, unreadable or failing their integrity digest (R39.6.3); local block-cache / wallet-database / locked-note storage cannot be opened; shielded key derivation fails (incl. HD policy without `z_derivation_path`); **no lightwalletd endpoint can be connected, or the initial chain-tip / tree-state query to the shielded backend fails**, or `light_wallet_d_servers` is empty; the requested sync date (a `date` start) is in the future -- later than both the wall clock and the tip block time -- or cannot be resolved; the key policy is not supported for the shielded coin (hardware wallet, WalletConnect); any other build-time failure | object `{"ticker": string (the requested ticker), "error": string}` | 400 |
| `Transport` | the coin was built and the shielded sync finished, but the transparent backend could not deliver the current block height, or the wallet balance could not be read (e.g. **every admitted lightwalletd endpoint unreachable** at the height read -- Electrum no longer exists for a shielded coin, §39.11 --, native daemon unreachable at the result step) | string | 500 |
| `CoinIsAlreadyActivated` | while this task ran, a concurrent activation of the same ticker registered the coin first | object `{"ticker": string}` | 400 |
| `TaskTimedOut` | only produced by a timed-out wait for a user action; a Z-coin task never waits for one (R39.10.8), so this is not reachable in practice | object `{"duration": {"secs": u64, "nanos": u32}}` | 408 |
| `Internal` | the wallet key context is not initialised; the shielded sync process stopped before reporting completion; task-handle or registration failures; any otherwise unclassified failure | string | 500 |

R39.10.12 For the "Sapling parameters not found" case the public-docs example
binds the content of the object: `error_data.error` shall be the bare token
`ZCashParamsNotFound` and `error_data.ticker` the requested ticker. For every
other `CoinCreationError` cause the `error` string is human-readable and not
bound.

R39.10.13 Discriminants that shall **not** be observable from any
`task::enable_z_coin::*` method: `HwError` (there is no hardware-wallet path),
`PrivKeyNotAllowed`, `UnexpectedDerivationMethod`, and the activation-internal
names that the public error catalogue lists for the Z-coin initialisation enum
(`HardwareWalletsAreNotSupportedYet`, `CouldNotGetBalance`,
`CouldNotGetBlockCount`) -- those are always translated into the discriminants
of R39.10.11 before they reach the wire. A request whose `mode` is absent
(R39.10.4) parses, starts a task, and ends with `CoinCreationError`.

### 39.10.6 Unresponsive and failing backends (Z-coin row of §55.3)

R39.10.14 The failure-to-discriminant mapping for unreachable or failing
backends is:

| Situation | Observable result |
| --- | --- |
| Light mode, **every** lightwalletd endpoint unreachable at admission, before anything is read (row superseded and implemented, §39.11, R39.11.6) | terminal `status` `Error`, `Transport` (string); an admission-excluded set or a fresh wallet with no strict-majority branch is `CoinCreationError` `{ticker, error}` instead |
| Light mode, the `light_wallet_d_servers` list is empty | terminal `status` `Error`, `CoinCreationError` `{ticker, error}`, raised before the coin is built (R39.11.5) |
| Light mode, the sync cannot start although an endpoint was admitted (connecting to it, the local resets, or the first chain-state / tree-state query fails, or local storage cannot be read) | terminal `status` `Error`, `CoinCreationError` `{ticker, error}` |
| Light mode, lightwalletd fails **after** the shielded sync has started (download or scan error) | the task does **not** fail: `status` reports `InProgress` with `{"TemporaryError": <string>}`, the sync retries after a pause on the order of ten seconds, and progress values resume once the backend answers; the task ends only on success, on `cancel`, or on a non-transient failure |
| Light mode, **every** Electrum server unreachable (row superseded and implemented, §39.11: the state cannot occur for a shielded coin; its lightwalletd analogue is R39.11.6) | not applicable |
| Native mode, full-node daemon unreachable | shielded sync reports `TemporaryError` and retries as above; if the height step fails: `Transport` (string) |
| Shielded sync process stops without reporting completion | terminal `Error`, `Internal` (string) |

> **Resolved and implemented (maintainer decision, 2026-10-11; Step C).**
> Electrum is removed for shielded (ZHTLC) coins entirely; the transparent side
> is served by lightwalletd and `electrum_servers` is accepted and ignored
> (§39.11; `docs/plans/arrr-ironwood-compatibility.md` §10). Light mode no longer
> builds any Electrum connection, so the "every Electrum server unreachable"
> state cannot occur; an unreachable lightwalletd endpoint set and an
> admission-excluded one end the task with distinct typed errors (R39.11.6).


No Z-coin backend failure is reported as an HTTP 5xx on the `status` call
itself; the only 5xx responses come from `init` (R39.10.10) or from the
framework errors of §55.2.

> **Upstream divergence (informative) -- public docs vs reference.** The
> public page shows the "no Zcash params" `CoinCreationError` once as a bare
> error object without any envelope and once inside a `status` `Error`; the
> reference only ever emits it inside a `status` `Error` (R39.10.11). The
> public catalogue's Z-coin initialisation names (R39.10.13) are never on the
> wire. The public in-progress list omits `UpdatingBlocksCache`,
> `BuildingWalletDb` and `TemporaryError` (the examples show the first two as
> object-valued `details`) and wrongly includes `FollowHwDeviceInstructions`.
> The public `get_balances` remark ("structure may vary") does not apply:
> the reference ignores `get_balances` for this coin.

> **Upstream divergence (informative) -- start-point resolution.** When no
> `sync_params` is given and no prior local state exists, the reference starts
> one day of blocks before the current tip, computed from the coin config's
> `avg_blocktime` (a missing `avg_blocktime` then fails activation with
> `CoinCreationError`). It resolves a `{"date": T}` start by the same
> block-time estimate rather than by looking up block timestamps. Reloaded
> resolves dates against the backend's block timestamps (R39.8.0g), and uses a
> fixed 2,880-block window for the no-`sync_params` default; the observable
> difference is limited to the initial scan range and to the value of
> `first_sync_block.requested`. Wire shapes are unaffected. The full contract
> for the defaulted start, including the parity position on the window size,
> is bound at R39.8.0av.

> **Code-quality finding (informative).** Trigger: a contradiction visible
> between the reference's own description of `skip_sync_params` and its
> effect, read side by side. The described intent is "resume from the last
> synced block; consult `sync_params` only when nothing was synced yet". In
> the reference, however, an explicit `sync_params` combined with
> `skip_sync_params: true` and an existing local block cache still rewinds and
> rescans from `sync_params`, i.e. the flag is effectively honoured only when
> the local cache is empty. Reloaded implements the described intent
> (R39.6.2) and shall not copy the reference's effective behaviour.
> Proposed upstream fix: treat "skip requested and a prior synced height
> exists" as the resume condition.
>
> *Status (2026-10-29): nothing open for Reloaded; the proposed fix is addressed to
> the reference.*

> **Reloaded status (informative).** On `fix/activation-api-parity` all twelve
> deviations previously listed here are resolved: WASM routing (but see item 1
> below), optional `mode`, the `"Electrum"` / `servers` synonyms and the
> connection bounds, the `scan_blocks_per_iteration` parse failure, `"Native"`
> unknown on WASM, `Transport` for post-build height and balance failures,
> `CoinCreationError {ticker, error}` for an unresolvable sync date, the
> R39.10.14 outage mapping with the `TemporaryError` in-progress value and an
> uncapped retry about every 10 s (maintainer decision: follow the reference),
> `CoinCreationError` for the hardware-wallet class, the bare
> `ZCashParamsNotFound` token, the registration race and the cancel set.
> Remaining deviations:
>
> 1. **WebAssembly.** The family is routed on WASM, but a WASM activation ends
>    with `CoinCreationError {ticker, error}` stating that shielded activation
>    is not available in the WebAssembly build. The WASM build has no shielded
>    wallet database, so it cannot compute a balance, and reporting a zero
>    balance instead would misstate the user's funds.
> 2. **Future sync date.** Implemented. The reference rejects a sync date in
>    the future with `CoinCreationError` (R39.10.11) and Reloaded does the same;
>    this is no longer a deviation. A date is "in the future" when it is later
>    than both the wall clock and the block time of the current tip; a date
>    after the tip's time but not after the clock resolves to the tip (the chain
>    simply has no later block yet). A start given as a height beyond the tip is
>    unaffected and still clamps to the tip (R39.8.0h).
> 3. **Electrum in Light mode.** Removed (§39.11, Step C): Light mode is served by
>    lightwalletd alone, and the build-time Electrum `CoinCreationError` no longer
>    exists. A Light coin with no usable lightwalletd endpoint ends the task as
>    R39.11.6 specifies.
> 4. A retry after `TemporaryError` re-runs the fetch with the original
>    `sync_params`, so a re-anchored wallet can rescan once per retry.
> 5. In Native mode a local wallet-database scan failure still ends the task
>    with `Internal`.

### 39.10.7 Tests

T39.10.1 Routing: on both the native and the WASM build, each of the four
`task::enable_z_coin::*` names and each flat alias of R39.10.2 resolves to a
handler (not "method not found").

T39.10.2 Request parsing: (a) `activation_params` without `mode` parses;
(b) `"rpc": "Electrum"` parses as Light; (c) `servers` parses as
`electrum_servers`; (d) `min_connected` / `max_connected` parse;
(e) `scan_blocks_per_iteration: 0` fails with `InvalidRequest`;
(f) on WASM, `"rpc": "Native"` fails with `InvalidRequest`; (g) unknown
members such as `get_balances`, `tx_history` and an Electrum `ws_url` are
ignored.

T39.10.3 In-progress serialization: `TemporaryError` serializes as
`{"TemporaryError": "<text>"}`; the object-valued and unit values serialize as
in R39.10.7 (extends the existing scan-progress wire test).

T39.10.4 Task-error mapping, each asserting `error_type`, the exact
`error_data` JSON type/fields, and that the carrying `status` response is
HTTP 200: (a) height or balance failure -> `Transport` with a string;
(b) unresolvable or future sync date -> `CoinCreationError` with
`{ticker, error}` and `ticker` equal to the request ticker; (c) all
lightwalletd endpoints unreachable at start -> `CoinCreationError`;
(d) missing Sapling parameters -> `CoinCreationError` with
`error_data.error == "ZCashParamsNotFound"`; (e) unsupported key policy ->
`CoinCreationError`; (f) residual hardware-wallet class -> `Internal`
string, never `PrivKeyNotAllowed`.

T39.10.5 Mid-sync backend failure: with a lightwalletd stub that fails after
the first successful batch, `status` reports `{"TemporaryError": ...}`, the
task is not terminal, and it reaches `Ok` once the stub recovers.

T39.10.6 `init` errors: `CoinIsAlreadyActivated` (object, 400),
`CoinConfigIsNotFound` (string = ticker, 400), `CoinProtocolParseError`
(object, 400), `UnexpectedCoinProtocol` (object with `protocol` object, 400),
asserting the serialized `error_type`, `error_data` shape and HTTP status.

T39.10.7 `user_action` on a live Z-coin task fails with the §55.2
not-awaiting error; the flat aliases return byte-identical results to their
canonical counterparts for the same task.
## Part E -- The single Light-mode backend, height authority and swap-path parsing

> **Dirty-side derivation record (sanitized).** §39.11 was authored by the KDF
> Spec Reader and **materially reversed on 2026-10-11** by a second Spec Reader
> pass. It carries only the observable backend contract, the dictated
> lightwalletd-gRPC method surface, public trait method names belonging to this
> project's own code, public dependency-crate facts, and public-network
> observations. It carries no private implementation expression. A clean-side
> implementation shall not consume this subsection until the KDF Dirty Gate has
> passed the reversed text.
>
> **What reversed, and why.** The first pass documented the as-built split in
> which a Light-mode shielded coin is served by *two* independent backends -- an
> Electrum transparent backend and a lightwalletd shielded backend -- and
> required a non-empty Electrum server list, declaring shielded-backend-only
> operation unsupported. Three facts arriving after that pass make the
> requirement unsatisfiable and the design wrong:
>
> 1. **The transparent provider is gone.** On 2026-10-08 both publicly reachable
>    ARRR ElectrumX servers were stalled at height 4161184 -- the last block
>    before the chain's first post-Ironwood transaction -- while Pirate
>    `lightwalletd` served roughly 6000 blocks further with the new consensus
>    branch active. A 2026-10-11 port scan of all ten hosts published for that
>    chain found **no ElectrumX service on any of them**: they serve lightwalletd
>    or plain HTTP only. The mandatory dependency has no healthy provider, and
>    nothing to wait for.
> 2. **The chain's own maintainer states the dependency is wrong in principle**:
>    a shielded Zcash-derived chain is served by lightwalletd, not by Electrum,
>    because the shielded protocol requires the former's compact-block and
>    commitment-tree surface.
> 3. **`lightwalletd` serves the transparent side too.** Verified live on
>    2026-10-11 against the chain's test network rather than inferred from the
>    service definition: every transparent function this coin needs is answered
>    by the same gRPC service, with the constraints recorded in R39.11.4a
>    --R39.11.4d.
>
> The decided design is therefore **one backend**. Electrum support for a
> ZHTLC/ZCoin Light-mode coin is **removed**, not retained behind a
> compatibility switch. The governing statement of intent is
> `docs/plans/arrr-ironwood-compatibility.md` §10.
>
> **Two live-probe findings were later corrected, and this subsection carries
> the corrected form.** A first reading of the probe results concluded that the
> dedicated transparent-balance method was unusable and that P2SH index coverage
> was merely unproven. Both were wrong. The balance method works -- the
> rejections came from a malformed request encoding on the probing side, not from
> the server (R39.11.4a). P2SH is not unproven but *defective*, by a reproducible
> defect in the serving node's own published source, with a specific consequence
> and a specific workaround (R39.11.4d). Where a requirement below rests on a
> live observation, it states which, so that a later correction lands in one
> place.

## 39.11 The Light-mode backend

> **Status update (reloaded, Step C) -- how to read §39.11's implementation
> status.** The wallet half of §39.11 is implemented, and (plan Step C, swap half) so is
> the swap-path half. The subsection carries three kinds of statement, and a reader shall be able to
> tell them apart:
>
> 1. **Verified facts about the external backend and the chain.** Where a
>    requirement records what the deployed `lightwalletd` service, the serving
>    node, the chain's consensus rules, or a pinned dependency crate actually do
>    -- the live probes dated 2026-10-08 and 2026-10-11, the node-source-derived
>    index defect, the dictated gRPC method surface -- the fact is established, and
>    the requirement says which observation it rests on. These are **not** claims
>    about this project's code.
> 2. **Descriptions of the present as-built contract.** R39.11.13--R39.11.15
>    describe what this repository did **before Step D**, and say so in their own
>    text. They are T-DOC, as-built, and were unchanged by Step C. Step D (the parser
>    routing of R39.11.16/R39.11.16a) has since changed the parser-per-call-path
>    split they record; the status note after R39.11.16a states what it is now.
> 3. **Requirements on this project's code.** Each carries a status note. In short:
>    implemented -- one backend, the function table, the activation requirements,
>    endpoint admission, the single height authority, `UtxoRpcClientOps` over
>    lightwalletd, and (Step D) the parser routing (R39.11.16/R39.11.16a);
>    and (Step C, swap half) the swap-path spend and already-sent detection with the
>    P2SH workaround (R39.11.4d) and the named conditions of R39.11.17. The
>    detection of a *spend* was observed live on 2026-10-29 (R39.11.4d's status
>    note). The naming, rejection and suspension rules of §39.11.6
>    (R39.11.29 -- R39.11.31) carry their own status note just before
>    the tests of §39.11.6; that note, not this summary, records whether they
>    are implemented.
>
> **Where it lives.** `mm2src/coins/z_coin/z_lightwalletd.rs` (admission, the
> failover client, the height authority, the windowed history reader, the
> `UtxoRpcClientOps` implementation), `z_coin/z_lightwalletd/htlc_index.rs` (the HTLC
> address-index probe and its decisions), carried on the coin as
> `UtxoRpcClientEnum::LightWalletD`; `ZCoin::builder_target_height` is the one
> height every construction path reads. The tests are in
> `z_coin/z_lightwalletd/tests.rs`, `z_coin/z_lightwalletd/htlc_index/tests.rs`,
> `z_coin.rs` (`light_backend_tests`) and
> `coins_activation/src/z_coin_activation.rs`.

### 39.11.1 Division of work: `lightwalletd` serves the whole coin

R39.11.1 A Light-mode shielded coin is served by **exactly one** backend: the
configured `light_wallet_d_servers` endpoints, speaking the lightwalletd gRPC
compact-transaction-streamer service. Every function below shall be obtained
from that service. No function of a ZHTLC coin is served by an Electrum server,
and no Electrum connection shall be opened on behalf of a ZHTLC coin, with one
exception: the fallback of R39.11.25 for three swap-path questions, which opens an
Electrum connection only on its first use and serves nothing else.

The method names in the second column are dictated interop -- the lightwalletd
service contract, which this repository already carries as the generated service
definition -- and are fixed by that specification, not by this project.

| Function | Dictated gRPC method | Derivation and failure policy |
|---|---|---|
| Chain tip / current block height | `GetLatestBlock` | the admitted endpoints are tried in turn (R39.11.3); the read fails only when every admitted endpoint fails |
| Compact blocks carrying shielded note data | `GetBlockRange` | as above |
| Commitment-tree state used as the wallet's sync anchor | `GetTreeState` | as above; the tree bytes are validated per R39.8.0ak before use |
| Mempool membership of a shielded transaction, and pending shielded receipts | `GetMempoolTx` | the pending amount degrades to zero (R39.8.0ai) |
| Block timestamp at a height (resolving a `{"date": T}` sync start) | `GetBlock` | read from the compact block's block-time field (R39.11.4f) |
| Median time past over a window | `GetBlock` per height in the window | computed locally from the same field (R39.11.4f) |
| Raw transaction bytes and mined height by txid | `GetTransaction` | only the hash-selector form is served, in internal byte order (R39.11.4c) |
| Confirmation count for a transaction | `GetTransaction` + `GetLatestBlock` | derived locally; no server-side decode exists and none is required (R39.11.4e) |
| Transaction broadcast (every shielded transaction: swap payment, DEX fee / burn, HTLC spend, HTLC refund, withdrawal) | `SendTransaction` | R39.11.4 |
| Transparent address history -- spend detection, and detection that this wallet's own payment was already sent | `GetTaddressTxids`, bounded by height window, then a local decode of each streamed transaction to match the previous-output reference | R39.11.4b (windowing and stream failure) and R39.11.4d (which address encoding reaches the HTLC's index rows) both bind this row |
| Transparent UTXO listing for one address | `GetAddressUtxos` | one address per call; the service definition carries no multi-address form, so a multi-address listing is a loop over single-address calls. R39.11.4d binds the P2SH case |
| Transparent address balance | `GetTaddressBalance` | several addresses in one call, summed by the server; the reply carries a single value amount and no breakdown. The request encoding is load-bearing (R39.11.4a), and R39.11.4d binds the P2SH case |
| Notarized-confirmation depth | none | not obtainable, so `requires_notarization` has no effect (R39.11.4e) |
| Fee rate | coin configuration | the coin's fixed per-kilobyte fee (an undeclared fee keeps the shared fixed default, as the published ARRR entry relies on); this service exposes no fee-estimation method, so a Light-mode shielded coin that explicitly requests estimation (`txfee: 0`) shall be refused |
| Shielded balance, spendable note selection, shielded transaction history | the local shielded wallet database (§39.8.0.2) | -- |

> **Status update (reloaded, Step C).** Implemented for every row; the three that the
> swap path owns were completed in its swap half (R39.11.4d). Chain tip (`GetLatestBlock`), block time and median
> time past (`GetBlock`), raw transaction and mined height (`GetTransaction`),
> confirmations (derived locally), broadcast (`SendTransaction`), transparent
> balance (`GetTaddressBalance`), UTXO listing (`GetAddressUtxos`, one call per
> address) and transparent history (`GetTaddressTxids`, R39.11.4b) are served by
> the admitted endpoints; the compact blocks, tree state and mempool rows were
> already. The fee-rate refusal is implemented at build time (a Light coin with
> `txfee: 0` is not built; an absent `txfee` keeps the shared fixed default). The two prohibitions hold structurally: the
> Light build constructs no Electrum client. The transparent
> history reader (`read_history`) is consumed by the swap-path detection, which also
> serves `find_output_spend` and `check_if_my_payment_sent`; the generic balance, UTXO
> and history methods still refuse a script-hash (P2SH) address with a typed error
> instead of an empty success, and only the detection's dual-form probe issues
> script-hash queries (R39.11.4d). `get_verbose_transaction(s)`
> return a typed unsupported error: nothing here decodes a transaction, and the
> one caller that needs bytes and height (`validate_fee`) reads them from
> `GetTransaction`. Notarized depth: see R39.11.4e.

R39.11.1a **Native mode is refused in this release.** An activation request for a
shielded coin with `mode.rpc` `Native` shall fail before any shielded work begins.
It fails as a configuration error naming the coin, in the same error shape the
activation already uses for an empty `light_wallet_d_servers` list. The message
says native mode is not supported in this release and that light mode should be
used. No backend, wallet database, or scan is created for the refused request.
Light mode is unaffected. The native-mode text elsewhere in this chapter describes
code that stays in the tree for a later release. It binds nothing that a node can
reach while this rule stands.

> **Provenance and reason (2026-10-28).** Clean-room original Reloaded policy,
> decided by the maintainer. In this repository the native path cannot build a
> working wallet for a fresh activation, because a full node's blocks do not feed
> the compact-block cache (the status update under R39.8.0f). It also could not tell which transaction
> version to build after Ironwood. Native mode, the Ironwood pool, and the
> daemon-side activation discovery are planned for v0.4.0 in
> `docs/plans/arrr-native-and-ironwood-pool.md`. The reference accepts native
> mode; the divergence is listed in `docs/GLEEC_COMPATIBILITY.md`.

T39.11.1a *Native mode refused.* An activation request with `mode.rpc` `Native`
fails with the configuration error naming the coin and the light-mode advice,
and leaves no backend, wallet database or scan state behind. A Light-mode request
for the same coin still activates.

R39.11.2 **Unconfirmed evidence remains asymmetric, and shall stay so even
though one service now answers both sides.** The *only* source of mempool
membership for a shielded transaction is the streaming mempool method
(R39.8.0ai). A zero-or-absent mined height on a transparent-side answer -- a
transaction returned by the history stream with no mined height, or an entry in
the UTXO listing -- is consumed only as "not yet mined" by the confirmation
wait, and shall never be treated as evidence that a shielded note exists, is
spendable, or has been received. R39.8.0q continues to govern.

> **Status update (reloaded, Step C).** Holds. Shielded mempool membership still
> comes only from `GetMempoolTx`; a transparent-side answer feeds no shielded note
> state, and a zero or absent mined height is read only as "not yet mined" by the
> confirmation arithmetic.

R39.11.3 **Endpoint multiplicity is a plain ordered list, admitted before use.**
The configured endpoints shall be tried in configured order per operation, and an
operation shall be declared failed only after every *admitted* endpoint has
failed (R39.8.0ae). Admission is the check of R39.11.7a: an endpoint that fails
admission is not tried at all, for any operation, and is not counted as a
failure.

A ZHTLC coin has no Electrum connection pool, so the maintained-connection floor
and ceiling of [chapter 38](38-utxo-coin-maintenance-and-features.md) §38.6.4
no longer bind it, and a requirement elsewhere that speaks of "all endpoints
failed" for a shielded coin means all admitted lightwalletd endpoints.

> **Status update (reloaded, Step C).** Implemented. Every operation tries the
> admitted endpoints in configured order and fails only after all have failed; an
> endpoint that passes admission and then fails a method (a 502 on the method
> actually needed, D39.11.g) is skipped for that call, and a total failure forces
> admission to be re-evaluated on the next call. A Light coin has no Electrum
> connection pool, so chapter 38 §38.6.4 does not bind it.
>
> **Pointer (2026-10-28).** R39.11.31 adds suspension: the configured order
> stays, and an endpoint suspended after a transport-level failure is tried
> after the unsuspended ones. Not yet implemented.

R39.11.4 **Broadcast is served by the same backend**, which removes the
single-point dependency the first form of this subsection recorded: a deployment
whose shielded backend is healthy and current can also submit. The submission
result carries a service-level success/failure indication rather than a transport
error, so a rejection shall be surfaced with the backend's reported failure
distinguished from an unreachable endpoint, and a rejection shall not be retried
against further endpoints as though it were a transport failure -- a
consensus-level rejection will be reproduced by every honest endpoint, and
retrying it hides the reason.

> **Status update (reloaded, Step C).** Implemented, unverified against the
> deployed node (D39.11.e). A service-level rejection (`SendResponse.errorCode`
> non-zero) is reported as `BroadcastRejected` with the backend's code and message
> and is not retried on further endpoints; a transport failure moves on. The
> transaction identifier is computed locally from the bytes (the response carries
> none), with the Zcash reader.
>
> **Pointer (2026-10-28).** R39.11.30 adds that the error and a `warn` log
> line name the endpoint that rejected, in host-only form, and R39.11.31(h)
> that a rejection never suspends an endpoint. Not yet implemented.
>
> **Status update (2026-10-29).** Submission is observed against the deployed
> service: on 2026-10-26 and 2026-10-27 it accepted version-6 transactions this project
> built and submitted -- a shielded send, and the payment and spend of a completed swap
> (R39.6.4e's status note) -- and each was mined. A service-level rejection has not
> been observed live; that path is still covered only by the scripted tests (D39.11.e).

R39.11.4a **Transparent balance is served by the dedicated balance method, and
its request encoding is load-bearing.** The method works against the deployed
node: verified 2026-10-11, it accepts several addresses in one call and returns
their summed value. Its reply carries a single value amount in the chain's
smallest unit and **no breakdown** -- no received/sent split, no per-address
split, no confirmed/unconfirmed split -- so a caller that needs any of those
shall derive them elsewhere rather than expecting them here.

The dictated request message carries the addresses as a **flat repeated string
field**. An encoding that instead nests a submessage inside that field is
accepted by the transport and then rejected by the node as an invalid address
list, for every address, including addresses the same server answers correctly on
its other transparent methods in the same session. Therefore:

- the implementation shall encode the address list as the dictated flat repeated
  field, and shall have a test that pins the encoded request bytes rather than
  only the decoded structure, because a malformed nesting round-trips cleanly
  through most generated code and fails only on the wire;
- an invalid-address-list rejection from this method shall **not** be treated as
  evidence that the supplied addresses are invalid. It is at least as likely to
  indicate a malformed request, and the two shall be reported differently. An
  implementation that reads it as "bad address" will report every valid address
  as invalid.

*(This requirement replaces an earlier form that declared the method unusable and
specified a sum over the UTXO listing as a workaround. The premise was a
malformed request on the probing side, not a server defect; the workaround is
withdrawn, and no independent reason to prefer it was found. The UTXO listing is
still used where the individual outputs are themselves needed.)*

For a P2SH address this method is subject to R39.11.4d exactly like the other two
transparent address methods: it is the index, not the method, that fails there.

> **Status update (reloaded, Step C).** Implemented. `GetTaddressBalance` is called
> with the flat repeated address field, several addresses in one call; the
> byte-level test (T39.11.j) pins the encoding. A per-address result
> (`display_balances`) takes one call per address, because the reply has no
> breakdown. Not implemented: the "invalid address list" rejection is surfaced as
> an ordinary endpoint failure carrying the node's text; it is not reclassified as a
> request-or-index condition, and nothing treats it as "these addresses are
> invalid". A P2SH address is refused before any request (R39.11.4d).

R39.11.4b **The transparent history stream aborts on the first transaction the
node cannot serve, and drops its last buffered height group. The implementation
shall window, bisect, and resume accordingly, and shall never report a partial
stream as "no history."** Probed on 2026-10-11: the history call for an address
with heavy history delivered 8661 transactions and then terminated in failure.
The cause is known from the serving node's and the server's own published
behaviour rather than inferred, and it is not a result-size cap or a range limit
-- there are none:

- the node cannot serve the raw bytes of a transaction that is **fully spent or
  has no transparent output** unless it is running with its transaction index
  enabled, and the streaming server aborts the whole stream on the first such
  failure rather than skipping that entry;
- the server buffers the transactions of one height and flushes the buffer only
  when a higher height appears, so the **last height group of any stream is
  always dropped**. That is why the observed count was one short of the index's
  own count rather than arbitrary.

The implementation shall therefore:

- (a) request **bounded height windows** rather than one open-ended range;
- (b) send both range bounds as values of at least 1. The node applies a
  requested range only when both bounds are numeric and greater than zero, so a
  zero start bound silently means "the whole chain" -- a request that looks
  narrow and behaves unbounded;
- (c) on the serve-failure of this requirement, **bisect the failing window and
  skip the offending height** rather than failing the whole pass. A skipped
  height shall be recorded as a gap, not forgotten;
- (d) **resume from the last received height, not from one past it**, because the
  final height group is always dropped; consequently the implementation shall
  deduplicate by transaction identifier across windows, since resuming at the
  same height re-delivers what it did receive;
- (e) retain and use the transactions already delivered when a stream terminates
  in failure;
- (f) distinguish, in every decision that consumes this surface, *the window
  completed with no gap and contained no match* from *the window failed, or
  completed with a skipped height*. **Only the first may be reported as a
  negative answer.** The second shall be surfaced through the sync-error channel
  of R39.8.0au and shall leave the consuming decision undetermined.

(c) and (f) are deliberately different rules for different consumers, and shall
not be collapsed: skipping a height is acceptable for keeping *history and sync*
moving, and is never acceptable as the basis for a negative *swap-path* answer
over a range that contains the skipped height. The skipped height may be exactly
the one holding the spend.

This is funds-affecting on the swap path: spend detection and already-sent
detection are both negative-answer decisions, and a failed or gapped stream read
as "not spent" or "not sent" is precisely the error that loses a secret or
double-funds a payment.

**The shape of an HTLC spend is the shape the node cannot serve.** An HTLC spend
on this coin consumes a transparent P2SH input and pays a shielded output, so it
has transparent inputs and **no transparent outputs** -- exactly the case the
first bullet describes. Spend detection on a node running without its transaction
index can therefore be structurally unable to retrieve the one transaction whose
script signature carries the swap secret, and the same limitation applies to
fetching it directly by identifier. The implementation shall treat this as a
named, reportable condition rather than as absence of a spend, and the serving
node's transaction index is a dictated operational precondition (R39.11.17).
See D39.11.g.

> **Status update (reloaded, Step C).** (a)--(e) are implemented in `read_history`
> and tested (T39.11.h): bounded windows, both bounds at least 1, bisection down to
> a single height recorded as a gap, resumption from the last received height with
> deduplication, retention of what a failing stream delivered. A window that
> delivers nothing beyond what the read holds is widened (up to 16 times the base
> window) from the same cursor, because the dropped group can only be delivered by a
> window that also contains a later group. Two residues cannot be repaired from the
> client and are documented on the function: the newest height group of the whole
> range, and a group isolated from any later group by more than the widest window.
> (f) is implemented as `HistoryRead::is_clean()` and **its consumer is built**: the
> swap-path detection reads every history window through `read_history` (one
> endpoint at a time, both address forms), treats a gap, a stream that ended in
> failure or an undecodable delivered transaction as leaving the decision
> undetermined, and writes the condition to the sync-error channel. A skipped
> height does not stop the read from delivering the transactions around it, and a
> delivered spend is reported as a spend even when another window skipped a height.
> **One consequence the live probe has not yet measured:** the newest height group of a
> stream is the one the service drops, and an HTLC's spend is always the newest group
> of its address. The detection therefore never reads "the history shows the funding and
> no spender" as "not spent" (R39.11.4d).

R39.11.4c **The transaction lookup accepts only the hash selector, in internal
byte order.** The dictated transaction filter offers a block-and-index selector
and a hash selector; the deployed node serves only the hash selector, and the
hash shall be supplied in the internal (compact-block) byte order, not the
big-endian display order of R39.8.7a. A lookup issued in display order fails
rather than returning a wrong transaction, so the failure mode is loud; the
requirement exists because the same wallet surfaces the opposite order to callers
(R39.8.7a), and the conversion point shall be explicit and tested in both
directions.

This method is served by the node's raw-transaction lookup, so it inherits that
lookup's limitation: a transaction that is fully spent or has no transparent
output cannot be served unless the node's transaction index is enabled
(R39.11.4b). A failure of this method shall therefore not be reported as "no such
transaction" (R39.11.12).

> **Status update (reloaded, Step C).** Implemented. `GetTransaction` is called with
> the hash selector only; the display-to-internal and internal-to-display
> conversions are the two functions `txid_display_to_internal` /
> `txid_internal_to_display`, and both directions are tested with a non-symmetric
> value. A failed lookup is reported as a failure; only an explicit gRPC `NotFound`
> is reported as "no such transaction" (R39.11.12).

R39.11.4d **A P2SH address query never matches this chain's transparent address
index, by a defect in the serving node. The implementation shall query the
redeem-script hash under the pubkey-hash address encoding instead, shall probe
both encodings at runtime rather than hard-coding either, and shall fail loudly
rather than report a negative answer.** The shielded HTLC is a transparent P2SH
output, so spend detection and already-sent detection query the HTLC's address.

**The defect, as a behavioural fact.** The chain's published node source
classifies every indexed transparent output under the *pubkey-hash* index key
type, because the classification's guard condition is unconditionally true after
a port of the destination type from one variant library to another; the
script-hash and cryptocondition key types are unreachable. The reverse mapping,
from an address to an index key, is correct: a script-hash address (this chain's
version byte 85) maps to the script-hash key type. The key type is the leading
byte of the index key, so the two types occupy disjoint key ranges. Consequently
**all three transparent address methods -- history, UTXO listing and balance --
return an empty success for a script-hash address forever, however much activity
that P2SH has.** The earlier observation that a valid P2SH address is "accepted
and returns empty" was therefore evidence of nothing.

The resolution belongs to the chain, not to this project; this requirement
specifies how to operate until it lands, and the workaround shall be written so
that it can be removed when it does.

**The workaround, and the two constraints on it.** The rows exist; only the key
they are filed under is wrong. The same twenty bytes -- the redeem script's hash
-- re-encoded under the chain's *pubkey-hash* address version byte (60) maps to
the pubkey-hash key type, which is where the rows actually are. Therefore:

- (a) **This follows from the node's published source and is unverified at
  runtime**, because no P2SH on this chain with known activity was reachable. The
  implementation shall prove it on first use rather than trust it, by the
  obligation in (d) below.
- (b) **Which key type a row carries depends on which binary indexed the block.**
  A data directory indexed by an older build may hold P2SH rows under the
  script-hash key type. The implementation shall therefore **probe both
  encodings** -- the script-hash form and the pubkey-hash re-encoding of the same
  twenty bytes -- and shall not hard-code either. Which one answered shall be
  remembered per endpoint and re-probed rather than assumed for the coin's
  lifetime, since endpoints are upgraded independently.
- (c) **A returned row shall be validated before it is trusted.** The
  pubkey-hash key type is keyed by twenty bytes alone, so a row could in
  principle belong to a different output with the same hash. The implementation
  shall check the returned output's script against the expected P2SH script
  before treating the row as the HTLC's, and shall discard a row that does not
  match rather than inferring from the key alone.
- (d) **Proof obligation and loud failure.** The implementation already holds the
  fact it needs: the payment transaction that funded the HTLC is itself a
  transaction involving that output, at a height the wallet knows. A
  spend-detection or already-sent query over an HTLC address, windowed from the
  funding height, shall be *required* to return that known funding transaction
  under at least one of the two encodings. If neither encoding returns it -- while
  the funding transaction is known to be mined inside the queried window and the
  window completed without a gap per R39.11.4b(f) -- the implementation shall
  report an index-coverage failure through the sync-error channel of R39.8.0au as
  a distinct, named condition, and **fail the decision loudly**. It shall under no
  circumstances report "not spent", "not sent", or "no such transaction" on the
  strength of an empty result.

A silent "not spent" on a spend that happened is the worst available outcome: the
counterparty's HTLC spend is the only source of the swap secret, and a wallet
that cannot see it waits out the lock and refunds a swap it had already won.

> **Dictated consensus fact in this project's favour (informative, and safe to
> rely on).** On this chain a transparent output in a non-coinbase transaction is
> a consensus-blessed special case reserved for exactly this HTLC: the chain's own
> transaction check admits it only when the transaction has no transparent
> inputs, has exactly two outputs, the first is pay-to-script-hash, and the second
> is a reveal of that script's redeem script -- and the reveal check is pinned to
> the fixed 110-byte atomic-swap redeem-script shape. Two consequences the
> implementation may depend on: the **redeem script is always present on-chain**
> in the funding transaction's second output, so its twenty-byte hash can be
> derived from chain data rather than only from local state; and the redeem-script
> **shape cannot drift**, because a transaction carrying any other shape is
> invalid on that chain. This is dictated interop, not a convention.

> **Status update (reloaded, Step C, swap half).** **Implemented, and the workaround is
> proved.** A real version-6 HTLC (100000 zat, mined at height 71535 on the test
> chain) was queried on both test endpoints on 2026-10-12 with identical results:
> under the `b...` script-hash form `GetTaddressTxids`, `GetAddressUtxos` and
> `GetTaddressBalance` each answered OK and empty (0 transactions, 0 UTXOs, 0 zat); under
> the `R...` pubkey-hash re-encoding of the same twenty bytes each answered OK with the
> row (1 transaction with the matching identifier, 1 UTXO, 100000 zat). The
> defect is therefore confirmed and the workaround resolves it, and the redeem-script
> block-scanning fallback is not needed. What is built, in
> `coins/z_coin/z_lightwalletd/htlc_index.rs`:
>
> - (b) **Both forms, always.** Every detection queries, on each endpoint it tries, the
>   unspent listing and the windowed history of both forms, pubkey-hash first. Which
>   form last returned rows is remembered per endpoint (and logged when it changes) but
>   never narrows the next probe.
> - (c) **Row validation.** A listing row counts only when its script equals the
>   expected pay-to-script-hash script. A delivered transaction counts as the funding
>   only when the watched output of that identifier carries the expected script, and as
>   the spend only when one of its transparent inputs names the watched output.
> - (d) **The decision.** A spend is returned whenever a delivered transaction spends
>   the output, whatever else failed. Otherwise the only negative answers are: the
>   unspent listing, under either form, names the output with the expected script; or
>   the funding transaction is reported unmined by the endpoint and a healthy endpoint
>   (every read answered, every window complete) returned nothing. Every other case is an
>   error and none is "not spent": a failed read or a stream that ended in failure, a
>   skipped height, an undecodable delivered transaction, an index that returned
>   neither form of a funding transaction that is mined or whose state could not be
>   read (the index-coverage failure), and an output the index knows but no longer
>   lists whose spender was not delivered. **This departs from the first form of (d)**,
>   which let "the funding transaction returned, no spender" answer "not spent": the
>   service drops the newest height group of a stream (R39.11.4b) and a spend is the
>   newest group of its address, so that reading is wrong exactly when it matters. The
>   unspent listing is not subject to the drop and is the corroboration R39.11.4b's
>   residue note asks for.
> - **Already-sent detection** has no known funding transaction to prove coverage
>   against. It returns the payment when a delivered transaction has a transparent
>   output of exactly the HTLC script, or the listing names one; it returns "not sent"
>   only from a healthy endpoint; it errors otherwise. The residual risk is
>   stated rather than closed: a payment that is broadcast and not yet mined is not in
>   the index, and a healthy empty index is not proof of coverage.
> - **Search window.** From the caller's start height (floored at the Sapling
>   activation height, below which no block is read), widened to take in the funding
>   height the endpoint reports, to the tip.
> - **Endpoints.** Tried in configured order; an endpoint that fails a needed method
>   contributes failure evidence and the next is tried; the first conclusive answer
>   is returned.
>
> The generic address methods still refuse a script-hash address (they have no
> corroboration to apply); only this probe issues script-hash queries.
>
> **Routing.** `ZCoin`'s `search_for_swap_tx_spend_*`, `wait_for_tx_spend` and
> `check_if_my_payment_sent` call the detection directly over lightwalletd and keep the
> spend as the Zcash transaction it is, because the shared UTXO transaction type cannot
> read version 5 or 6; the spend is classified by the same rule as the shared search
> (`classify_swap_spend`, extracted), applied to the input that spends the output. The
> `find_output_spend` trait method over lightwalletd runs the same detection and returns a
> loud error if the spend it found cannot be read by the shared parser.
>
> **Not covered.** A spend on a node without its transaction index (D39.11.g)
> surfaces as skipped heights; whether the test endpoints serve it is measured by the
> opt-in live test `live_spend_detection_of_a_redeemed_htlc` (`ARRR_TESTCHAIN_LIVE_DIR`),
> which funds a fresh HTLC, redeems it in the same run and asserts the detection and
> `extract_secret`. Until that has been run, detection of a *spend* on a live chain is
> unverified; detection of an *unspent* HTLC and the P2SH defect are proved.
>
> **Observed live (2026-10-29).** Detection of a *spend* is now observed on the main
> chain, read-only, against an HTLC that a swap between two Reloaded nodes funded at
> height 4,185,655 and spent at height 4,185,666. The production functions behind
> `check_if_my_payment_sent` and `search_for_swap_tx_spend` were run against two
> lightwalletd endpoints that keep the transaction index, each alone and both together.
> In every run the payment was found as sent and the spend was found under the
> pubkey-hash form, input 0, in about 230 ms, while the script-hash form returned
> nothing, as the index defect predicts; `extract_secret` recovered a secret whose
> hash matches the script. The opt-in test above, which also funds and redeems, has
> still not been run on the test chain.

R39.11.4e **Confirmation depth shall be derived locally; a ZHTLC coin has no
notarized depth, and `requires_notarization` has no effect on it.** *(Amended
2026-10-30, maintainer Decision AT.)* The lookup method returns the
transaction bytes and its mined height (or an explicit not-mined indication), and
the tip comes from the tip method, so confirmation depth is `tip + 1 - mined
height`, and zero when the transaction is unmined or its height exceeds the tip
-- the same arithmetic R39.8.9e already states for history entries, now the only
form available. Two consequences bind:

- **No server-side transaction decode is required any more.** The first form of
  this subsection depended on a transparent server that could decode the
  transaction to report a mined height; this one does not, which removes an
  entire class of failure for a chain whose transaction version the servers do
  not yet understand (see R39.11.17).
- **Notarization depth has no source, so `requires_notarization` has no effect.**
  This service exposes no notarized-depth field, and a ZHTLC coin has no
  notarized confirmation count for the flag to select. Asserting
  `requires_notarization` for a ZHTLC coin -- in the coins file, in the
  activation request, by `set_requires_notarization`, or as an order's
  `base_nota` / `rel_nota` -- shall be accepted and shall not change the wait:
  the confirmation wait counts plain confirmation depth against the required
  confirmations. The coin shall report `requires_notarization` as `false`, and a
  `warn` line naming the coin shall state that the flag has no effect because no
  notarized confirmation count exists: once at activation, once per
  `set_requires_notarization` that asserts it, and once per wait that asks for it.
  Nothing shall report a notarized depth. See D39.11.f.

> **Status update (reloaded, Step C).** Implemented. The confirmation wait fetches
> the bytes and mined height with `GetTransaction` and the tip with `GetLatestBlock`
> and computes `tip + 1 - mined height` locally (zero when unmined or above the tip);
> no server-side decode is asked for. The notarization prohibition is implemented
> at the point of use (D39.11.f, second form): a wait that requires notarization
> fails with an error saying notarized depth is not obtainable, and plain depth is
> not substituted. Asserting `requires_notarization` at activation is still accepted;
> the maintainer decision on its observable form (D39.11.f) is open.
>
> **Known issue (maintainer Decision AT, 2026-10-29).** The assertion stays accepted at
> activation and by `set_requires_notarization`; the refusal this rule asks for is not
> made there. A swap that requires notarization for a ZHTLC coin fails at the
> confirmation wait, after its payment is on chain. See D39.11.f.
>
> **Status update (2026-10-30, amended rule).** Implemented; the two notes above
> describe the earlier form and are superseded. The lightwalletd confirmation wait no
> longer takes the flag. The ZHTLC wait counts plain confirmations and, when a swap
> asks for notarization, says so in one warning. `requires_notarization` reports
> `false`, `set_requires_notarization(true)` warns and stores nothing, and activation
> warns when the coins entry or the request asserts it. Tests:
> `a_wait_that_asks_for_notarization_counts_plain_confirmations` and
> `requires_notarization_has_no_effect_on_a_shielded_coin`.

R39.11.4f **Block timestamps and median time past shall be read from the compact
block's block-time field.** The compact-block message carries the block time, so
both the `{"date": T}` sync-start resolution of R39.8.0g and the
median-time-past-derived locktime of the transparent P2SH path are served by
per-height compact-block reads. Two constraints follow:

- the window for a median-time-past computation shall be bounded and its size
  explicit, because each height is a separate request on this service rather
  than one batched header call;
- the header-family selection rule of
  [chapter 37](37-utxo-spv-and-block-header-validation.md) §37.5 R37.5.1a no
  longer binds this resolution for a ZHTLC coin, because no block header is
  parsed: the time is a field of the dictated compact-block message. Chapter 37
  §37.5 continues to bind every Electrum-backed UTXO coin.

> **Status update (reloaded, Step C).** Implemented. The `{"date": T}` resolution
> and the median time past read the compact block's time field per height; the
> median window is bounded (`MEDIAN_TIME_WINDOW_MAX`, 64). Chapter 37 §37.5
> R37.5.1a no longer binds a ZHTLC coin.

R39.11.4g **The Light-versus-Native discriminator shall not be the identity of
the UTXO RPC client.** In this repository a ZHTLC coin's mode is currently decided
by matching on which arm of the shared UTXO RPC client enumeration the coin
holds, with the Electrum arm standing for "Light". Removing Electrum from this
coin type removes that signal, so the implementation shall carry the coin's
activation mode explicitly on the coin and shall decide every mode-dependent
branch from it. This is a correctness requirement, not a tidying one: every
Light-mode-only behaviour in this chapter -- the shielded balance source, the
background sync task, the pending-receipt surface, the skipping of the native
commitment-tree maintenance -- is selected by that discriminator today, and a
discriminator that silently stops matching selects the Native path for a Light
coin.

The lightwalletd-backed implementation of this project's own UTXO RPC operations
trait (`UtxoRpcClientOps`) shall satisfy that trait for every function a ZHTLC
coin reaches, and shall return an explicit typed unsupported error -- never a
zero, empty, or default stand-in -- for any trait method this chapter does not
assign it. Where the module lives is an implementation decision; that it is a
first-class implementation of that trait, rather than a set of special cases
scattered through the shielded coin, is a requirement, because the shared UTXO
swap helpers reach it through the trait.

> **Status update (reloaded, Step C).** Implemented. The coin carries `ZcoinMode`
> (`Native` / `Light`), set at construction from the requested backend; the shielded
> balance source, the background sync task, the pending-receipt surface, the
> in-flight spend recording and the skipping of the commitment-tree loop are selected
> by it. `LightwalletdClient` is a first-class `UtxoRpcClientOps`; the methods this
> chapter assigns it are served, and the rest (`get_verbose_transaction(s)`,
> `estimate_fee_sat`, `get_relay_fee`, `find_output_spend`) return an explicit typed
> unsupported error, never a zero or empty stand-in.

R39.11.4h **HTLC detection is bounded per endpoint and reports its progress.**
Spend detection and already-sent detection (R39.11.4d) read the transparent
history through the windowed reader of R39.11.4b. That reader's bisection
(R39.11.4b(c)) assumes that a serve failure belongs to one transaction. An
endpoint whose failure is not tied to one transaction breaks that assumption,
for example a server whose node has no address index and refuses every history
request. Such an endpoint would be bisected down to every single height of the
range, at about two requests per block and without a word: tens of thousands
of requests for a swap a few weeks old. The implementation shall therefore:

- (a) start the spend-detection window at the funding transaction's mined
  height when the endpoint reports one, never below the Sapling floor. A spend
  cannot precede its funding, so a caller's search start below that height is
  not read. When the funding height is not known, the caller's start applies
  as before.
- (b) end a history read with a named failure, instead of bisecting further,
  once one window holds more than 4 heights the endpoint could not serve. The
  failure states how many heights of which window could not be served, carries
  the endpoint's last error text as received, and says that the failure is not
  confined to single transactions. A read also ends
  with a named failure after 1,024 requests. Either failure leaves that
  endpoint's answer undetermined for this detection (R39.11.4b(f)), and the
  next admitted endpoint is tried. A read answered with the node's
  no-address-index message never reaches this bound: it ends at its first
  request (R39.11.4i(e)).
- (c) give each endpoint a time budget of 120 seconds per detection, covering
  both address forms. An endpoint that exceeds it counts as failed for this
  detection, with an error naming the endpoint, the address form and the read
  it was in. The next admitted endpoint is tried.
- (d) log one line at `info` for each of these:
  - the start of a detection: the coin, the funding transaction and its state,
    the height range, and the number of admitted endpoints;
  - each endpoint and address form: transactions and unspent rows found, gaps,
    requests made, elapsed time, and the failure if the read stopped;
  - the decision, or the failure of every endpoint.

  A detection run by a polling wait (waiting for the counterparty to spend)
  repeats every few seconds, so it logs these lines at `debug`.

None of this changes when a decision may be negative. An endpoint stopped under
(b) or (c) is undetermined, never "not spent" or "not sent".

> **Provenance and reason (2026-10-28).** Clean-room original; this is a
> Reloaded defect, not a reference question. Recovery of a September shielded
> swap logged its key match and then did not return, with the process idle
> (R39.5.5). The windowed reader had no bound on its number of requests and
> produced no progress output. On that day, two endpoints of the coin's
> published server list answered every transparent-history and UTXO request,
> for both address forms, with status Unknown and the node's message "-5: No
> information available for address": their node keeps no address index. They
> pass admission, because server information and transaction lookup work. Each
> refusal was read as a serve failure and bisected, so a range of about 33,500
> blocks cost about 67,000 requests per address form on each of them. That is
> roughly 40 minutes per endpoint, with nothing logged. Under (b), such an
> endpoint stops after about 20 requests. The other reachable endpoints of the
> list served the same reads over the HTLC of another September swap fast and
> clean: both address forms, the funding and the refund delivered in one
> window, and each 1,000-block window answered in under a fifth of a second.
> That probe also saw the newest height group delivered, which R39.11.4b's
> status update had left unmeasured. The detection still does not depend on it.

T39.11.4h *Bounded HTLC detection.* Against scripted endpoints, tests shall show
that:

- an endpoint that answers every history request with a serve failure is
  stopped after a small, asserted number of requests over a range of 30,000
  blocks, with the named failure. A second endpoint that answers cleanly then
  decides the detection;
- a read that keeps advancing one height per request stops after 1,024
  requests, with the named failure;
- an endpoint that exceeds its time budget counts as failed, with an error
  naming the endpoint and the address form, and the next endpoint decides;
- a funding transaction mined far above, and one far below, the caller's
  search start is read from its own height, and a spend after it is found;
- an endpoint stopped under (b) or (c), with no other endpoint, leaves the
  detection undetermined, never "not spent".

R39.11.4i **An endpoint without an address index is marked and skipped.** An
endpoint keeps no address index if it answers a transparent-history or UTXO read
of an HTLC detection with the node's message "No information available for
address", and no HTLC detection can succeed on it. The implementation shall
mark such an endpoint "no address index" for the rest of the coin's activation:

- (a) the first time, it logs one line at `warn` naming the endpoint and
  carrying the node's message;
- (b) every later detection skips the endpoint without sending it a request.
  When a detection is left undetermined, its error text lists the endpoint as
  skipped for having no address index;
- (c) when every admitted endpoint is marked, a detection fails at once. The
  error names those endpoints and says that HTLC detection needs a server
  whose node keeps an address index (R39.11.17);
- (d) the mark is cleared only when the coin is activated again. Re-evaluating
  admission (R39.11.7a(g)) does not clear it, because a server-information read
  cannot show whether the index exists;
- (e) a transparent-history or UTXO read answered with that message ends at
  once. It is not a serve failure of R39.11.4b(c): the window is not bisected
  and no height is recorded as a gap. Its failure says that the server's node
  keeps no address index and carries the node's message. It is never explained
  as a missing transaction index, because the node's transaction index has
  nothing to do with it.

A marked endpoint is undetermined, as in R39.11.4h. The mark never makes a
detection negative, and it does not affect any other use of the endpoint: tip,
transactions, compact blocks and broadcast.

> **Provenance and reason (2026-10-28).** Clean-room original. The endpoints
> described under R39.11.4h refuse every address read in the same way, so under
> R39.11.4h alone each detection would still spend a few requests and a log line
> on them. Detection runs on every poll of a live swap.
>
> **Amendment (2026-10-28), item (e).** A fresh swap's already-sent check reads
> a range of one block. A refusal there produced a single gap, below the stop
> of R39.11.4h(b), so the gap was reported with the transaction-index
> explanation of R39.11.4b. An operator whose node runs with its transaction
> index on, and without the address index, saw a maker payment fail with that
> explanation, which pointed at the wrong setting.

T39.11.4i *No address index.* Against scripted endpoints, tests shall show
that:

- an endpoint answering the address reads with that message is marked during
  the first detection, and the next endpoint decides;
- a second detection sends the marked endpoint no request;
- with every admitted endpoint marked, a detection fails at once with the
  named error and sends no request;
- a backend built anew (a new activation) starts with no endpoint marked;
- the marked endpoint still serves the tip and transaction lookups;
- (e) a one-block history read and a UTXO read answered with that message
  each cost one request and record no gap. The detection's error says that the
  server's node keeps no address index, carries the node's message, and does
  not mention a transaction index;
- (e) over a range of many windows, the same refusal ends the read after its
  first request;
- a genuine per-transaction serve failure, one not carrying that message,
  is still bisected and reported as a gap with the transaction-index
  explanation.

### 39.11.2 What activation requires of the backend

R39.11.5 **A non-empty `light_wallet_d_servers` list is mandatory for Light mode;
`electrum_servers` is not.** *(This requirement is the reverse of its first form,
which made the Electrum list mandatory.)*

- The Light-mode `rpc_data` object's `light_wallet_d_servers` array shall be
  present and carry at least one entry. An empty array is non-conformant and
  shall fail activation at coin construction, before any shielded work begins,
  reported as a configuration error naming the coin -- not as a transient
  backend failure.
- `electrum_servers` shall remain **accepted and optional**. It is not required,
  an empty array is accepted, and its absence is accepted. Deployed KDF-family
  wallets send it unconditionally and a parse rejection would break every one of
  them, so the field stays in the accepted wire shape. Its servers are used for
  one thing only: the fallback of R39.11.25. The Electrum connection-count members
  the activation wire accepts alongside it are accepted and ignored.
- Activation shall not open, attempt, probe, or report on any Electrum
  connection for a ZHTLC coin, and shall not fail for any Electrum-related
  reason. The fallback connects on its first use.

> **Amendment (2026-10-28, maintainer Decision AK).** The first bullet's "accepted and
> ignored" became "accepted and optional, used only by the fallback of R39.11.25".
> Implemented: the list is kept and handed to the fallback, and activation still never
> connects to it (the status update below describes the rest unchanged).

> **Reference behavior (informative, black box, 2026-10-28).** The reference's 3.0.0-beta
> release binary rejects a Light-mode request without `electrum_servers` as a parse error
> naming the missing field, in line with the "required in Light" row of the §39.10 table.
> Reloaded accepts both shapes, so a request that carries the field activates on either
> implementation and one without it on Reloaded only. `docs/GLEEC_COMPATIBILITY.md`
> ("ZHTLC backend") tells operators.

> **Status update (reloaded, Step C).** Implemented and tested (T39.11.b): an empty
> `light_wallet_d_servers` is a `CoinCreationError` naming the coin, raised before
> the coin is built; `electrum_servers` and the connection counts are accepted, may
> be absent or empty, and are ignored; activation never opens an Electrum connection.

R39.11.6 **Activation does not require a live connection at construction time,
but it does require a usable admitted endpoint before it can return a result.**
*(Reversed in subject: the binding backend is now lightwalletd.)* Activation
reads the chain tip twice -- once to bound the shielded scan (R39.11.8(b)) and
once to populate the activation result's `current_block` (R39.3.3) -- and both
reads are hard requirements. A coin whose every configured endpoint is
unreachable, and a coin whose every configured endpoint fails admission under
R39.11.7a, shall each fail activation with a typed error, and the two causes
shall be distinguishable. Neither shall report a terminal success carrying a
placeholder, zero, or last-known tip.

> **Status update (reloaded, Step C).** Implemented. Activation runs admission
> before it reads anything: an unreachable endpoint set ends the task with
> `LightwalletdUnreachable` (wire: `Transport`), an excluded set or a fresh wallet
> with no strict-majority branch with `LightwalletdExcluded` (wire:
> `CoinCreationError`), and neither reports a tip. The tip is read from the admitted
> endpoints and a failed read is an error.

R39.11.7 **Light-mode operation on the shielded backend alone is the supported
design, and the only one.** *(This is the reverse of its first form, which
declared this configuration unsupported.)* The three capabilities the first form
named as missing are all present and are specified above: the chain tip comes
from the backend's own tip method (R39.11.1, R39.11.9), transaction submission
comes from the same service (R39.11.4), and spend detection comes from that
service's transparent address history under the proof obligation of R39.11.4d.

Electrum support for a ZHTLC coin is **removed**, not deprecated and not retained
behind a compatibility switch. There shall be no code path, configuration value,
or build feature by which a ZHTLC coin is served by an Electrum server. See the
Compatibility note at the end of §39.11.3 for what this means against GLEEC KDF.

> **Status update (reloaded, Step C).** Implemented. Electrum is removed for a ZHTLC
> coin: there is no code path, configuration value or build feature by which a
> ZHTLC coin is served by an Electrum server. The spend-detection capability the
> requirement lists as present in the service is present in the service, not yet
> in this project's code at Step C's wallet half, and is since (R39.11.4d). The Compatibility note at the end of §39.11.3
> is now the description of a real divergence and is recorded in
> `docs/GLEEC_COMPATIBILITY.md` and `RELOADED_VS_GLEEC.md`.

R39.11.7a **Endpoint admission: an off-branch endpoint shall be excluded from
use, not merely reported.** *(New requirement.)* Before any endpoint is used for
compact blocks, tree state, or a tip read, the implementation shall read that
endpoint's server-information method and admit the endpoint only if all of the
following hold:

- (a) the reported network name matches the network the coin is configured for;
- (b) the reported Sapling activation height matches the coin's configured
  Sapling activation height;
- (c) the reported consensus branch identifier satisfies (d) or (e) below.

**Why exclusion and not reporting.** Endpoints published for one ticker can be
serving *different forks of that chain*: three of the eight published ARRR
lightwalletd entries belong to the operator whose node never upgraded, and on
2026-10-11 they reported the pre-upgrade branch identifier against a chain that
had activated a new one. A reported-but-still-used endpoint is not merely stale.
Per the session that fixed the stale-anchor condition, an endpoint whose tip is
*above* the wallet's anchor while serving a different chain presents as a
divergence, so the wallet rewinds -- on the observed configuration by
approximately 99 blocks -- and then continues from that endpoint's chain. That is
an integrity failure: the wallet's chain selection has been captured by the
endpoint. A warning the operator may not read does not prevent it; only exclusion
does.

**How the expected branch identifier is determined, and the inversion hazard
this requirement exists to avoid.** The branch identifier expected at a height is
derived from the coin's configured network-upgrade heights. When the chain has
activated an upgrade whose height the coin's configuration does not declare --
which is exactly the ARRR state this chapter documents, because the Ironwood
members of R39.6.4a are optional and no published configuration carries them --
the locally derived expectation is the *pre-upgrade* value. A check that simply
compares each endpoint's reported identifier against that local expectation
therefore **admits the stale fork and excludes the healthy chain**, which is the
precise opposite of the intent. The rule shall therefore be:

- (d) **Where the coin's configuration determines the expected identifier at the
  endpoint's reported height** -- that is, the reported height is below the first
  upgrade height the configuration does not declare -- a mismatch excludes the
  endpoint.
- (e) **Where it does not** -- the reported height is at or above a height for
  which the configuration cannot name the active upgrade -- the local expectation
  is not authoritative and shall not be used to exclude. Instead the reachable
  endpoints shall be partitioned by reported identifier, and:
  - if the wallet database already holds scanned blocks, the implementation shall
    admit only the partition whose identifier matches the one recorded with that
    scanned state, and shall persist that identifier alongside the wallet's sync
    state so the binding survives restart. An endpoint in any other partition is
    excluded regardless of its tip height. This is the rule that prevents the
    rewind-and-follow capture above.
  - if the wallet database holds no scanned state, the implementation shall admit
    the partition reported by a strict majority of the reachable endpoints,
    record its identifier with the new sync state, and exclude the rest. When no
    partition holds a strict majority -- including the single-reachable-endpoint
    case -- activation shall fail with a typed error reporting every observed
    identifier with its endpoint count, rather than silently picking one.
- (f) The admission result shall be observable: the set of excluded endpoints,
  each with its reason, shall be reportable through the coin's sync-state channel
  (R39.8.0au), because "activation failed, and three of eight endpoints are on
  another fork" is diagnosable where "activation failed" is not.
- (g) Admission shall be re-evaluated rather than cached for the coin's lifetime:
  an endpoint can be upgraded, and a previously admitted endpoint can fork away.
  The re-evaluation interval shall be an explicit named constant or configuration
  value. Re-evaluation shall never *widen* the binding of (e): once a wallet's
  scanned state carries an identifier, only that partition is ever admitted until
  the configuration gains the upgrade height that makes (d) decidable again.
- (h) **A pre-upgrade endpoint is stale once the upgrade's activation time has
  passed, and is excluded regardless of any majority.** When the coin has an
  Ironwood activation *time* -- the configured `ironwood_activation_time`, else the
  built-in time of the real ARRR chain (R39.6.4d), irrespective of
  `ironwood_gate_disabled` -- and the wall clock is at or past it, an endpoint
  reporting the branch identifier in force immediately before Ironwood (that of the
  last declared pre-Ironwood upgrade) shall be excluded before (d) and (e) are
  applied, with a reason that names the activation time. This is positive
  information, not a vote: the stale endpoints neither count in the majority of (e)
  nor can they form one. Before the activation time the pre-upgrade identifier is the
  correct answer and shall not be excluded. The time is the only authority: a reported
  *height* cannot serve, because a frozen node's tip lies below the activation
  height. Where no activation time is known (a coin without a configured or built-in
  one, a test chain) the rule is silent and (e) alone decides. If every reachable
  endpoint is stale, admission fails (all excluded) and the wallet shall not
  transact. **Release of a superseded binding:** a wallet whose recorded identifier is
  that same pre-Ironwood identifier is not bound by it once the activation time has
  passed -- every wallet that scanned before activation carries it, and honouring it
  would exclude every current endpoint (the exception to the last sentence of (g)).
  The wallet is decided like one with no recorded identifier (strict majority of the
  endpoints remaining after the exclusion) and the winner is recorded. A window of
  the order of the chain's median-time-past lag follows the activation time during
  which current endpoints still report the old identifier and are excluded too; the
  wallet fails closed for it.
- (i) **Admission shall be logged.** Every evaluation shall log, at a level a
  deployment captures by default, each endpoint probed with the chain, branch
  identifier and height it reported or the reason it was unreachable, each exclusion
  with its reason, and the admitted set; a failed evaluation shall log its error,
  and that error shall itself name every endpoint that was excluded or unreachable
  and why -- including the failure that reports observed identifiers (R39.11.7a(e)),
  so that "one endpoint reports branch X" is never the only thing a reader learns.
- (j) **A stale endpoint shall be excluded.** *(New requirement; original to this
  design.)* `GetLightdInfo` reports the height of the endpoint's *node*; the
  compact-block cache that actually serves blocks is reported by a different method
  (`GetLatestBlock`), and the two can disagree arbitrarily -- on 2026-10-23 an ARRR
  endpoint reported the consensus tip and the right branch while its cache stood at
  height 840,761, so it could serve nothing above it. Admission shall therefore also
  read the tip method of each endpoint that answered the server-information read, and
  exclude it, with a distinct reason, when:
  - (j1) the tip method fails or the cache is empty while the server-information read
    succeeded (the cache is "not yet ready");
  - (j2) the cache height trails the endpoint's own reported node height by **more
    than** the tolerance; or
  - (j3) the lower of the two (what the endpoint can actually serve) trails the
    *consensus height* by more than the tolerance. The consensus height is the lower
    median of the node heights reported by the endpoints on this coin's network
    (matching network name and Sapling activation height), so a minority -- up to
    just under half of them -- cannot move it, and with two endpoints it is the lower
    one, so a single stale endpoint is never the reference that condemns a healthy
    one. An endpoint ahead of the consensus height is never excluded by this rule.

  **The tolerance** is a named constant equal to the transaction-builder's expiry
  delta (R39.11.8(e), 40 blocks): the monotonic-tip rule (R39.11.11) already refuses a
  tip more than that far behind a height the activation has observed, so a tighter
  bound would exclude endpoints the wallet can use, and a looser one would admit
  endpoints the wallet would then refuse. An endpoint trailing by exactly the
  tolerance is admitted. An endpoint a few blocks behind -- a cache that is
  ingesting the block just found, a node one block behind its peers -- is healthy
  and shall be admitted. The rule is applied after (h) and before (d) and (e),
  so a stale endpoint neither counts in a majority nor hides how few current
  endpoints remain. The reasons are reported through the same channels as the
  others ((f), (i)).

> **Status update (reloaded, Step C).** Implemented in `decide_admission` and tested
> (T39.11.f). (a), (b), (d) and (e) are as written. The configuration determines the
> expected branch only below the *horizon* -- the activation height of the last
> declared upgrade, when any later upgrade this build knows of is undeclared -- so
> for ARRR (no published configuration declares an upgrade after Sapling) the local
> expectation is never applied at a real tip height and (e) decides. Since Step E
> `Nu6_3` is bound to the Ironwood height (R39.6.4e): a configuration that *declares* the
> height determines Sapling's branch below it and Ironwood's from it, and an Ironwood
> height *discovered* after the policy was built does not re-arm the expectation (its
> horizon was fixed while the upgrade was undeclared), so it can never exclude the
> endpoints it was learned from. The recorded branch identifier is a sidecar file
> of the wallet database (`<wallet db>.lightwalletd-branch`): the wallet database's
> schema is fingerprinted and cannot carry it. A wallet that has scanned blocks but
> has no recorded identifier (it predates the binding) is decided like a fresh one,
> by strict majority, and the winner is recorded. A fresh wallet needs at least two
> agreeing endpoints; a single reachable endpoint fails with the observed
> identifiers. (g) is implemented with `ADMISSION_REEVALUATION_INTERVAL` (300 s); the
> recorded binding is never rewritten while it is in force, so a legitimate network
> upgrade the configuration does not declare stops a bound wallet with
> `AllExcluded` until the configuration gains the upgrade height. (f) is partial:
> the exclusions are logged, listed in the message of an admission failure and
> available from `ZCoin::excluded_lightwalletd_endpoints`, but a successful
> admission does not write them into the sync-state channel.
>
> **Status update (reloaded, staleness, (j)).** Implemented in `decide_admission_at` and
> tested (T39.11.k). Admission now reads the cache height through a separate transport
> method (`cache_height`) so a fixture can make it disagree with the server-information
> read. Three exclusion reasons: cache unavailable (j1), cache behind its own node (j2),
> behind the consensus height (j3); `STALENESS_TOLERANCE_BLOCKS` is `BUILDER_EXPIRY_DELTA`.
> The probe log line now shows the cache height. The failure with observed identifiers
> (`NoBranchMajority`) carries the excluded and unreachable endpoints, and the activation
> error for an all-excluded or no-majority admission no longer says that "no endpoint is on
> this coin's chain" -- it was wrong for a lone endpoint on the right branch and for stale
> ones. Not covered: the tolerance is not configurable; a live run of the Rust transport
> against a stale-cache endpoint has not been done (the criterion was evaluated against the
> real endpoint set with the monitor script's readings, not with the daemon).
>
> **Pointer (2026-10-28).** Under R39.11.31(e) admission probes every
> configured endpoint whether or not it is suspended, and R39.11.29 binds how
> its logs and errors name an endpoint. Neither is implemented yet.

R39.11.7b **Each endpoint votes once.** Before admission (R39.11.7a), the
configured `light_wallet_d_servers` list shall be normalized and
de-duplicated, keeping the configured order and the first spelling of each
endpoint. Two entries name the same endpoint when they agree after
normalization: scheme (an entry without one takes the scheme the client would
use), lowercase host, port (the scheme's default when none is given), and no
trailing slash. Each dropped duplicate is logged once as a warning naming
both spellings. The branch vote, the "at least two endpoints" condition, and
every other count in admission then see each endpoint once, so the list
`[A, A, B]` is two endpoints and cannot produce a 2-to-1 majority. An empty
list after de-duplication is refused as an empty list is today.

Normalization cannot recognize one server published under two different host
names, or behind two addresses. Such a server still votes twice; the
configuration is responsible for listing independent endpoints.

The warning names each spelling in its host-only form (R39.11.29, added
2026-10-28; not yet implemented).

> **Provenance (2026-10-26).** Clean-room original Reloaded policy, written
> from this repository's code and a report by a Pirate Chain developer; no
> forbidden-corpus input. It adds to R39.11.7a without editing its gated
> text.

T39.11.7b *Duplicate endpoints.* `[A, A, B]` with A and B on different
branches fails admission with no majority (one vote each), not a 2-to-1 win
for A. `[https://a.example:443/, a.example, B]` keeps two endpoints, in
configured order. A list made only of one endpoint's spellings is a single
endpoint and is treated as one.

### 39.11.3 Chain-height authority

> **Pointer (2026-10-28).** The median time past that the shielded refund gate R39.5.13
> (Decision AP) consults is read from compact-block times as R39.11.4f binds; it is not a
> second height authority.

R39.11.8 **Every consumer of the chain height.** The admitted backend's tip is
the coin's only chain-height authority, and it is consumed by all of the
following. This list is normative: a change to the height authority shall be
evaluated against every entry.

- (a) **The activation result.** `current_block` on the `init_z_coin` result
  (R39.3.3) is that tip verbatim.
- (b) **The shielded scan target at activation.** The same value bounds *both*
  activation phases: the compact-block download and the shielded wallet-database
  scan. The complete-through-tip gate of R39.8.0m is satisfied against this
  bound.
- (c) **The shielded scan target of every post-activation background pass**
  (§39.8.0.5), and the bound passed to the pending-receipt refresh.
- (d) **The background pass's "nothing to do" short-circuit**, which compares
  this value against the wallet's scanned height and skips the pass when the
  value is not higher.
- (e) **The transaction-builder target height for every shielded construction
  path**, including the HTLC spend and HTLC refund path. The builder derives a
  transaction's expiry height from its target height plus a fixed delta -- 40
  blocks in the selected dependency generation (`zcash_primitives`' published
  `DEFAULT_TX_EXPIRY_DELTA`).
- (f) **Confirmation arithmetic** in `z_coin_tx_history` (R39.8.9e) and in the
  confirmation wait (R39.11.4e): `confirmations` is `current_block + 1 -
  block_height`, and zero when the entry's height exceeds the tip.
- (g) **The give-up condition of the confirmation wait**: when the transaction
  cannot be fetched and declares a non-zero expiry height, the wait declares the
  transaction expired once the tip passes that expiry height -- subject to
  R39.11.12.
- (h) **Resolution of a `{"date": T}` sync start to a height** (R39.8.0g), which
  searches heights relative to the tip and reads block times per R39.11.4f.
- (i) **Median-time-past-derived locktime** for transparent P2SH spending in the
  shared UTXO path, likewise computed per R39.11.4f.
- (j) **Height-range bounds for the transparent history stream** (R39.11.4b).

> **Status update (reloaded, Step C).** Implemented for (a)--(f), (h) and (i): each
> reads the single lightwalletd tip (the activation result, the activation and
> background scan targets, the "nothing to do" short-circuit, the builder target
> height of every construction path, confirmations, the date resolution and the
> median time past). (g): the give-up condition follows R39.11.12. (j): the consumer
> is the swap-path detection (R39.11.4b, R39.11.4d).

R39.11.9 **One height authority, and no second height source.** *(This replaces
the first form's rule that the shielded scan target be taken from a different
backend's tip than the transparent consumers'. With one backend the question
dissolves: the backend that serves the data is the backend that reports the
height.)*

The tip read of R39.11.1 is the single authority for every consumer of R39.11.8.
The implementation shall not introduce a second height source for a ZHTLC coin,
shall not derive a height from a cached or configured value when the read fails,
and shall not let any consumer of R39.11.8 take its height from anywhere else.
The two defects the first form existed to describe were both consequences of
there being two authorities; the requirement that survives is that there be one.

> **Status update (reloaded, Step C).** Implemented. `GetLatestBlock` on the admitted
> endpoints is the only height source of a Light coin. The Light-mode ordinary send
> path no longer takes its target from the wallet database (the database's height
> still selects notes and the anchor), and the HTLC spend and refund path no longer
> takes it from the UTXO client; both call `ZCoin::builder_target_height`. A failed
> read returns an error and never a cached, configured or scanned height. Native
> mode keeps its full-node block count as its authority.

R39.11.10 **Admitted endpoints can still lag one another, and the height the
coin uses shall not go backwards.** *(This is the residual of the withdrawn
cross-backend divergence tolerance; see D39.11.b for what it replaces and why
the rest is gone.)*

Admission under R39.11.7a establishes that every admitted endpoint is on the same
chain; it does not establish that they are at the same height. Ordinary
propagation lag, and an endpoint still catching up after a restart, both produce
a lower tip from an honest endpoint. Therefore:

- the height the coin reports and scans towards shall be **monotonic
  non-decreasing** within one activation: a tip read lower than the highest
  previously observed tip shall not lower it, and shall not be reported as the
  coin's current block;
- a tip below the wallet's already-scanned height shall never trigger a rewind, a
  re-anchor, or a discontinuity recovery. Only a *hash-link* discontinuity at a
  height both the wallet and the endpoint claim may do that, per R39.8.0n;
- an endpoint whose tip is materially below the highest observed tip may still
  serve ranges it covers, but shall not be the source of the coin's reported
  height;
- no tolerance constant is required for any of this. The withdrawn tolerance
  existed to decide when two *different backends* disagreed enough to warrant a
  report; monotonicity and the hash-link rule together cover intra-list lag
  without one. If an implementation finds it needs a lag threshold -- for example
  to stop using an endpoint that has fallen far behind -- it shall be an explicit
  named constant whose stated purpose is endpoint selection, never chain
  selection, which R39.11.7a owns.

> **Status update (reloaded, Step C).** Implemented and tested (T39.11.i, in part).
> The reported height is `max(read, highest observed)`; a lower read neither lowers
> it nor is reported as it. A tip below the scanned height triggers no rewind (the
> scanner's only rewind trigger remains the hash-link rule of R39.8.0n). No lag
> threshold constant exists. The test covers the monotonic floor; the hash-link
> half is covered by the existing R39.8.0n tests.

R39.11.10a **Confirmations and expiry are judged against a fresh tip.** The
monotonic height of R39.11.10 serves reporting, scanning and the builder
target height (R39.11.11) only. Two decisions shall instead use the tip just
read from an admitted endpoint (the fresh read, not the highest height ever
observed):

- the confirmation count of a transaction (`tip + 1 - mined height`, zero
  when the height is above that tip), and so every wait for a number of
  confirmations;
- the decision that a transaction has expired because the tip has passed its
  expiry height.

A lagging endpoint then delays both decisions, which is the safe direction:
a payment is never counted as confirmed, and a transaction never declared
expired, on the strength of a height the chain no longer has. Under the
monotonic height, a reorganization that lowered the tip would overcount a
payment's confirmations by the difference.

> **Provenance (2026-10-26).** Clean-room original Reloaded policy, written
> from this repository's code and a report by a Pirate Chain developer; no
> forbidden-corpus input. It refines R39.11.10 without editing its gated
> text.

T39.11.10a *Fresh tip for confirmations.* With a highest observed tip of 110
and a fresh read of 105, a transaction mined at 105 has 1 confirmation, not 6.
A wait for 2 confirmations does not finish until a fresh read reaches 106.
A transaction with expiry height 107 is not declared expired while the fresh
read is 105, although the highest observed tip is 110. The reported height
stays 110.

R39.11.11 **Every shielded transaction construction path shall take its builder
target height from the one authority of R39.11.9.** *(Retained unchanged in
substance; it is the requirement that fixes the live refund defect, and the
reversal does not weaken it.)*

The builder's expiry height is its target height plus a fixed delta
(R39.11.8(e)). A target height more than that delta behind the true chain tip
therefore produces a transaction that is *already expired at the instant it is
broadcast*. The network rejects it, and the rejection surfaces as an opaque
broadcast failure rather than as the stale-height condition it actually is. With
the delta at 40 blocks and the divergence observed on 2026-10-08 at roughly 6000
blocks, every HTLC spend and every HTLC refund built from the stale source was
born expired.

This is funds-affecting rather than cosmetic: the refund path is the only thing
that returns a maker's or taker's locked value when a swap does not complete, and
it is exactly the path that took its target height from the stale source.

The requirement is **uniformity as well as currency**: the ordinary shielded send
path (swap payment, DEX fee and burn outputs, withdrawal) and the HTLC
spend/refund path shall not use two different height authorities for the same
coin. A divergence between them is invisible in testing -- each path passes in
isolation -- and shows up only as one of the two refusing to confirm.

Additionally, a construction path shall refuse to build rather than build against
a height it cannot currently corroborate: if the tip read fails, the path shall
return a typed error, and shall not fall back to the wallet database's own target
height, to the last known tip, or to the scanned height.

> **Status update (reloaded, Step C).** Implemented and tested (T39.11.c, in part).
> `builder_target_height` is `tip + 1` from a fresh read and refuses with
> `StaleTip` when the read is more than the builder's expiry delta (40,
> `DEFAULT_TX_EXPIRY_DELTA`) behind the highest tip seen or the wallet's scanned
> height; a failing read is `TipUnavailable`. Neither falls back. The test pins the
> shared arithmetic and the refusals; it does not drive the two full builders from
> one injected tip, because those need the Sapling parameters and a scanned wallet
> database.

R39.11.12 **A stale or unreachable tip shall not be allowed to fabricate a
terminal outcome.** *(Retained; restated for one backend.)* Specifically:

- The confirmation wait shall not conclude "expired" on the strength of a tip it
  cannot corroborate against a successful transaction fetch. A tip that has
  advanced past the expiry height while the transaction itself cannot be
  retrieved is at least as likely to indicate a backend that has not indexed the
  transaction as a genuine expiry, and the two shall be reported differently.
- "The backend returned no such transaction" shall be distinguished from "the
  backend failed", and neither shall be reported as "the transaction does not
  exist". A partial or failed history stream shall be treated per R39.11.4b and
  an empty P2SH result per R39.11.4d; neither is a negative answer.
- A tip read that fails shall leave the coin's reported sync state in the error
  state of R39.8.0au, carrying the height scanned so far, rather than leaving the
  previous state in place.

> **Status update (reloaded, Step C).** Implemented and tested (T39.11.d, in part).
> A transaction that cannot be fetched while the tip is past its expiry height is
> declared expired only when the service answered an explicit `NotFound`; otherwise
> the error says the expiry is not proved. "No such transaction" is distinguished
> from "the backend failed" in the same way. A failed tip read in the background
> pass populates the sync-error channel with the height scanned so far.

> **Compatibility.** Two distinct divergences from GLEEC KDF are recorded here.
>
> 1. **Electrum support for a ZHTLC coin is removed** (R39.11.5, R39.11.7). The
>    applicable GLEEC contract requires a non-empty Electrum server list in the
>    Light-mode activation payload and serves the coin's whole transparent side
>    from it. Reloaded accepts the field and ignores it. Per
>    `docs/COMPAT_SWITCHES.md` a deliberate divergence normally ships with a
>    scoped opt-back mechanism; **there is none to offer here, and the
>    documentation shall say so plainly rather than name a switch that does not
>    exist.** On 2026-10-11 the removed path had no working provider: no host
>    published for this chain ran an ElectrumX service at all, and the two that
>    did were serving a fork that stopped at the first post-upgrade block
>    containing transactions. An opt-back switch would re-arm the chain-capture
>    and born-expired defects this subsection exists to close. On 2026-10-28 one
>    published host served the chain's tip, and its block hashes matched the
>    lightwalletd endpoints' at the Ironwood activation height and near the tip.
>    The maintainer therefore restored Electrum (Decision AK) for three
>    swap-path questions only, as a fallback behind a chain-identity check
>    (R39.11.25), never for heights, broadcast or the wallet. The observable
>    change for a caller is that `electrum_servers` matters only for that
>    fallback.
> 2. **Endpoint admission and single-authority height discipline**
>    (R39.11.7a, R39.11.9--R39.11.12) have no GLEEC equivalent: that contract
>    performs no server-information admission check, excludes no endpoint for
>    being on another fork, and has two builder height authorities in the one
>    coin. These are additive for a deployment whose endpoints are all on the
>    chain they claim: such a deployment observes no behavioural change except
>    that the two height authorities become one, which is the defect fix.
>
> Both entries belong in `docs/GLEEC_COMPATIBILITY.md` and `RELOADED_VS_GLEEC.md`
> in the same commit as the behaviour.

> **Code-quality finding (informative) -- two builder height authorities in one
> coin.** In this repository the ordinary Light-mode shielded send path takes its
> builder target height from the shielded wallet database's own target height,
> while the HTLC spend and refund construction path takes it from the UTXO RPC
> client's block-count read. The two are the same number only while both sources
> are current. With the RPC-side height behind by more than the builder's expiry
> delta, the ordinary send path still produces broadcastable transactions while
> every HTLC spend and refund is born expired -- which is why a wallet in this
> state appears to work for withdrawals and fails only on the swap paths.
> *Proposed fix:* give both paths the single height authority R39.11.11 requires,
> and add a regression that drives both paths from one injected tip and asserts
> the same expiry height. *(Trigger: contradiction between two construction paths
> read side by side.)*

> **Code-quality finding (informative) -- the mode discriminator is the RPC
> client's identity.** In this repository every Light-mode-only behaviour of the
> shielded coin is selected by matching on which arm of the shared UTXO RPC
> client enumeration the coin holds, with the Electrum arm standing for "Light"
> and the native arm for "Native". Removing Electrum from this coin type makes
> that match stop selecting the Light path, and the fall-through is the *Native*
> path -- which for a Light coin means native-only commitment-tree maintenance
> runs, the shielded balance is not read from the wallet database, and the
> pending-receipt surface is bypassed. Nothing in the type system notices.
> *Proposed fix:* R39.11.4g -- carry the activation mode explicitly on the coin
> and branch on it. *(Trigger: a rule the code plainly violates once the backend
> changes, visible at the call sites being specified.)*

> **Status (2026-10-29): both findings above are resolved.** The ordinary send path
> and the HTLC spend and refund path now take their target height from one
> function, `ZCoin::builder_target_height` (`z_coin_ops.rs`, called from `gen_tx` and
> from `z_htlc.rs`), and the test
> `every_construction_path_derives_the_same_target_and_expiry_from_one_tip` is the
> proposed regression. The coin carries its mode explicitly as `ZcoinMode`
> (R39.11.4g), and every Light-only behavior branches on it.

> **External defect (informative) -- the serving chain's transparent address
> index files every output under the pubkey-hash key type.** This is a defect in
> the chain's own node, not in this project, and it is recorded here because two
> requirements are shaped entirely by it. The output-classification guard is
> unconditionally true after a port of the destination type between two variant
> libraries, so the script-hash and cryptocondition branches are unreachable and
> every indexed output is filed under the pubkey-hash key type; the
> address-to-key mapping still sends a script-hash address to the script-hash key
> type, and the key type is the leading byte of the key. The ranges are disjoint,
> so every P2SH address query returns an empty success permanently. The input-side
> path in the same node still carries the correct guard form, which is what makes
> this read as a port regression rather than a design choice. *What this project
> can do:* R39.11.4d -- query both encodings, validate the returned script, prove
> coverage against the known funding transaction, and fail loudly. *What it
> cannot:* fix the index. The correct resolution is a node fix, and R39.11.4d's
> workaround is specified so that it can be removed when one ships.
> *(Trigger: reported for this area from the chain's published source; the
> defect's own contradiction -- a guard that cannot be false next to the correct
> form of the same guard elsewhere in the same node -- is visible without a
> correctness audit.)*

### 39.11.4 Swap-path transaction parsing contract

R39.11.13 **Two different parsers read the same coin's transaction bytes, and
which one reads them is determined by the call path, not by the coin.** This is
the present contract and it shall be stated explicitly, because planning
version-6 support requires knowing every site that must change. The backend
reversal does not change which parser reads what; it only changes where the bytes
come from.

The **Zcash library transaction reader** -- the public
`zcash_primitives::transaction::Transaction` reader, which takes a consensus
branch identifier -- reads:

- `MarketCoinOps::tx_enum_from_bytes` for a shielded coin;
- the counterparty's payment transaction, before an HTLC spend or refund is
  constructed from it, in all four of `send_maker_spends_taker_payment`,
  `send_taker_spends_maker_payment`, `send_maker_refunds_payment` and
  `send_taker_refunds_payment`;
- the DEX fee transaction in `validate_fee`.

The **project's shared UTXO transaction parser** reads the same coin's bytes in
every swap step the shielded coin delegates to the shared UTXO swap helpers:

- `validate_maker_payment` and `validate_taker_payment`;
- `check_if_my_payment_sent`;
- `search_for_swap_tx_spend_my` and `search_for_swap_tx_spend_other`;
- `extract_secret`;
- `wait_for_confirmations`;
- `wait_for_tx_spend`;
- and each candidate transaction streamed from the backend's transparent address
  history during spend detection, before its previous-output references can be
  matched.

R39.11.14 **The type crossing the swap-layer boundary is not uniform for a
shielded coin.** The swap layer's transaction carrier is a tagged union with both
a UTXO-transaction arm and a Zcash-transaction arm, and a shielded coin produces
**both**, depending on the path:

- payment construction -- maker payment, taker payment, DEX fee and burn --
  yields the Zcash-transaction arm;
- `tx_enum_from_bytes` yields the Zcash-transaction arm;
- the HTLC spend and refund construction path re-reads the bytes it has just
  built through the shared UTXO parser and yields the **UTXO-transaction** arm;
- spend detection yields the UTXO-transaction arm, because the candidate was
  decoded by the shared UTXO parser.

A plan for version-6 support shall therefore not assume a single carrier type for
the coin. Any path that converts between the two arms by re-parsing the
serialized bytes acquires the version coverage of the parser it converts
*through*, and is the narrowest point of the whole chain.

> **Upstream divergence (informative).** The upstream shielded HTLC spend/refund
> path returns the Zcash-transaction arm. Reloaded's returns the
> UTXO-transaction arm, because it re-reads its own freshly built bytes through
> the shared UTXO parser before returning. Observable swap behaviour is identical
> while every transaction is a version the shared parser understands; it diverges
> the moment one is not, and Reloaded's variant fails one step earlier -- at
> construction rather than at validation.

R39.11.15 **Version coverage of the two parsers (public facts, for planning).**

- The **shared UTXO parser** recognises the Overwinter-era layout for versions 3
  and 4 and applies the version-4 field layout to *any* overwintered version at
  or above 4. It implements neither the ZIP-225 (version 5) nor the ZIP-230
  (version 6) layout, and it computes transaction identifiers by double-SHA256
  only. A version-5 or version-6 transaction therefore either fails to
  deserialize or is mis-parsed against the wrong field layout, and its
  identifier is wrong by construction, because versions 5 and above take the
  ZIP-244 identifier.
- The **Zcash library reader** accepts versions 3, 4 and 5, and in the dependency
  generation currently pinned recognises version 6 only when the build sets that
  dependency's unstable next-upgrade configuration predicate; without it a
  version-6 header is rejected as an unknown transaction format. In the published
  stable release named by the plan's crate-bump step the version-6 arm is
  unconditional. It computes the identifier by the rule each version dictates.

> **Status update (reloaded; describes the state before Step D -- see the status note
> after R39.11.16a for what the routing changed).** **As-built (T-DOC) for R39.11.13 and R39.11.14;
> R39.11.15 is a pair of published-dependency facts.** The parser-per-call-path
> split, the non-uniform carrier type and each parser's version coverage were
> read off this repository's own current code and its pinned dependencies. They
> describe the present state and require no change in themselves; they are stated
> here because R39.11.16 and R39.11.16a change them, and because the enumeration
> is what makes the set of sites that must change knowable. The one
> forward-looking sentence -- that the backend reversal does not change which
> parser reads what -- is a property of a change not yet made and shall be
> re-checked when it is.

R39.11.16 **Requirement for Ironwood-era support.** Before a shielded coin may
transact on a network whose standardness rule requires a transaction version the
shared UTXO parser does not implement, every swap step of R39.11.13 shall read
that coin's bytes through a parser that:

- accepts every transaction version the coin's network accepts;
- computes the transaction identifier by the rule that version dictates; and
- exposes the transparent inputs, transparent outputs and script data the HTLC
  checks of the shared path require.

The validation *semantics* shall be preserved exactly: the same P2SH
script-pubkey check against the same expected redeem script, the same output
value check, the same script-signature inspection for the spend-versus-refund
discrimination, and the same byte-for-byte comparison of the locally held
payment against the bytes the backend returns. That byte comparison is why a
re-serialization round trip through a version-unaware parser cannot serve as a
bridge: it would make a well-formed payment fail validation.

Until this holds, the coin shall be frozen rather than allowed to transact; the
freeze and build-refusal contract is R39.6.4b/R39.6.4c and is not restated here.

R39.11.16a **The parser that satisfies R39.11.16 shall be the Zcash library
reader, reached by routing a ZHTLC coin's swap-path reads to it. The shared UTXO
parser shall not be extended to cover versions 5 and 6.** *(New requirement; it
resolves D39.11.c in favour of routing rather than extension.)* The reasons are
properties of the two parsers, not preferences:

- The Zcash library reader already handles Sprout, version 3, version 4, version
  5 and -- in the published stable release -- version 6 **in one parser**, each
  with the identifier rule its version dictates. The version coverage a ZHTLC
  coin needs is therefore acquired, not written.
- The shared UTXO parser is *wrong by construction* for the newer versions rather
  than merely incomplete: it applies the version-4 field layout to any
  overwintered version at or above 4 and computes a double-SHA256 identifier, so
  a version-6 transaction is mis-parsed against the wrong layout and assigned the
  wrong identifier. Extending it would mean implementing ZIP-225, ZIP-230 and
  ZIP-244 inside the parser every UTXO coin in this project shares.
- That parser cannot be replaced either: its transaction type (`UtxoTx`) is the
  transaction type for every UTXO coin in the workspace and is referenced across
  the order of forty files. Changing it is not a shielded-coin change.
- Routing is therefore the only option that confines the change to the coin that
  needs it, and it removes a duplication rather than adding one.

Three constraints bind the routing:

- (a) **The old version coverage shall be retained, not replaced.** A wallet
  restoring from a pre-upgrade birthday still reads version-4 history, so the
  routed parser shall be exercised against version-4 fixtures and shall produce
  the same decisions the shared parser produced for them. The Zcash reader
  accepts version 4, so this is a test obligation, not a compatibility hazard.
- (b) **The carrier non-uniformity of R39.11.14 shall be resolved in the same
  change, not left in place.** Routing the reads while the HTLC spend/refund path
  still re-serializes through the shared parser to produce the UTXO-transaction
  arm leaves the narrowest point exactly where it was.
- (c) **Identifier byte order shall be pinned at the boundary.** The two parsers
  compute identifiers by different rules, this chapter requires display order on
  the caller-facing surface (R39.8.7a), and the backend requires internal order
  on the lookup method (R39.11.4c). The routing change shall pin all three with a
  fixture whose identifier is not byte-order symmetric.

> **Status update (reloaded, Step D).** **R39.11.16 and R39.11.16a are implemented
> for every swap step of R39.11.13, with the carve-outs below.** Every read of a
> ZHTLC coin's transaction bytes on the swap path goes through one module,
> `z_coin/z_tx_read.rs`, which holds the only parse of those bytes: the Zcash
> library reader, called with a fixed consensus-branch argument that matters only
> to the pre-version-5 layouts (versions 5 and 6 carry their own). It serves
> `validate_maker_payment`/`validate_taker_payment` (same P2SH script-pubkey and
> output-value decisions, plus a byte-for-byte comparison of the supplied payment
> with the backend's bytes; there is no SPV step, because the shared validator does
> none for a node-served or lightwalletd-served coin either),
> `check_if_my_payment_sent`, `search_for_swap_tx_spend_my`/`_other`,
> `extract_secret`, `wait_for_confirmations` (expiry height from the routed
> reader) and `wait_for_tx_spend`. The decisions that do not depend on the parser
> are the shared ones, reached through three small shared helpers that take the
> facts the routed reader produced (the secret-extraction rule over input scripts,
> the spend search over the payment's identifier and output scripts, and the
> output-spend wait loop); the shared UTXO parser's own entry points call the same
> helpers, so other coins are unchanged. The shielded commitment-tree loop of a
> Native-mode coin, which parsed each block transaction with the shared parser to
> read its Sapling output commitments, reads them from the Zcash reader as well.
> Constraint (b): the HTLC spend/refund builder returns the transaction it built
> as the Zcash-transaction arm instead of re-reading its bytes through the shared
> parser, and `check_if_my_payment_sent` yields that arm too. Constraint (c): the
> internal-order identifier is what the spend search and the lightwalletd lookup
> take, display order is the caller-facing form, and one reversal converts between
> them, pinned on a recorded identifier that is not byte-order symmetric.
>
> **Carve-outs, not covered by this change.** (1) A spend found by a *Native-mode*
> node client still arrives as the shared UTXO transaction type, because that
> client belongs to every UTXO coin; it is read by the shared parser inside the
> client and is therefore not routed. Over lightwalletd the spend-detection tail, the
> swap half of plan Step C, has since been built and is routed too (R39.11.4d): the
> spend stays the Zcash transaction it is. (2) `sign_raw_tx` on a ZHTLC coin deserializes a
> caller-supplied transparent transaction with the shared parser; it is not a swap
> step, and a version-5 or version-6 input is refused there. (3) The WASM build
> keeps its own stub file, which does not parse shielded-coin swap transactions
> here. (4) Over lightwalletd the wait loop of `wait_for_tx_spend` still retries
> until its deadline rather than failing at once, as before; each undetermined
> round is logged with its reason and the named conditions are written to the
> sync-error channel.
>
> **Observed defect, corrected from the earlier description.** R39.11.15 and
> R39.11.16a say the shared parser "mis-parses" a version-6 transaction. On every
> one of 18 recorded version-6 transactions (all bundle shapes) it does not
> mis-parse but **refuses the bytes** (a malformed-data or unexpected-end error),
> so there is no wrong identifier to compare and the swap step fails rather than
> proceeding on a wrong one. The identifier rule it owns, double-SHA256 of the
> bytes, is still demonstrably not the version-6 identifier (T39.11.e asserts both).
> The routing is required either way. The unstable-configuration and
> published-stable-release properties R39.11.15 records are dependency facts, not a
> claim about this build's selection; the vendored reader in this build reads
> version 6.

R39.11.17 **Dictated external preconditions that no change on this side can
satisfy.** *(Restated: the backend reversal removes the largest of the three and
leaves two.)*

- **Removed by the reversal: server-side transaction decoding.** The first form
  of this subsection depended on a transparent server decoding the transaction to
  report a mined height and a confirmation count. Under R39.11.4e nothing on this
  coin's path asks the backend to decode a transaction; the backend relays bytes
  and a height. A backend that does not understand the chain's current
  transaction version can therefore still serve confirmation counting -- which is
  precisely the condition that made the chain unusable on 2026-10-08, when both
  reachable transparent servers stalled at the last block before the chain's
  first post-upgrade transaction.
- **Remaining: the address index must be reachable for the output being watched.**
  On this chain a P2SH address query never matches, by a node defect, so spend
  detection depends on the pubkey-hash-encoding workaround of R39.11.4d landing
  on rows a particular endpoint's data directory actually holds. Neither the
  defect nor which binary indexed a given endpoint's chain is within this
  project's control, which is why R39.11.4d requires a per-query proof, a probe
  of both encodings, and a loud failure rather than an assumption.
- **Remaining: the serving node's transaction index must be enabled.** Without
  it the node cannot serve the raw bytes of a transaction that is fully spent or
  has no transparent output, and an HTLC spend is exactly that shape
  (R39.11.4b). This is an operator/provider configuration on a third-party host.
  The implementation cannot require it, cannot read it from the
  server-information method, and shall therefore detect it from the failure and
  report it as its own named condition. See D39.11.g.
- **Remaining: broadcast depends on the node's own relay and mempool policy.** A
  transaction the node will not relay is refused regardless of this project's
  behaviour, and R39.11.4 requires that refusal be reported distinguishably from
  an unreachable endpoint.

All three remaining conditions shall be treated as first-class, reportable
conditions, distinct from an unreachable endpoint and from a transaction that
does not exist, and shall be surfaced through the sync-error channel of R39.8.0au
rather than as a generic RPC failure.

> **Status update (reloaded, Step C swap half).** **The preconditions are established
> external facts; the reporting requirement is implemented for the swap path.** All four
> bullets state properties of third-party components -- the chain's node, its
> transparent address index, an operator's configuration, a node's relay and mempool
> policy -- and the observation that at least one live endpoint lacked a transaction
> index was made on 2026-10-11. The final paragraph's reporting is built for the
> swap-path detection as follows, each as a distinct error of
> `HtlcIndexError` that is written to the sync-error channel (R39.8.0au) and is never an
> answer: the **address index** condition is the index-coverage failure (neither form
> returned a funding transaction known or presumed mined) and, when the index lists
> nothing for an output it knows, the spender-not-delivered error; the **transaction
> index** condition (D39.11.g) cannot be read from the server and is detected from the
> failure, where it surfaces as skipped heights in the incomplete-read error, whose
> message names the cause; the **broadcast** condition was already reported
> distinguishably by R39.11.4 and is unchanged. A lookup failure of the detection itself
> (no admitted endpoint, no tip) is a plain failed call and is not written to the
> channel. The first bullet's "removed by the reversal" now describes present
> behaviour for the transparent side: nothing on this coin's path asks the backend to
> decode a transaction.

### 39.11.5 Swap-path transparent queries: reference contract and the Light-mode failure policy

> **Derivation record (sanitized).** This subsection was authored by the KDF Spec
> Reader from the reference behaviour of the `v2.6.0-beta` and `v3.0.0-beta`
> releases, which are identical for everything stated here, and from public
> network-configuration data. It carries the observable swap contract, dictated
> protocol method names, public event names of the saved-swap format, and
> public-network facts; no private implementation expression. The KDF Dirty
> Gate passed it on 2026-10-28 (PASS, measure 0.02; the clearance item in §39.9
> records the extent). *binding* rules bind this project's code; *informative*
> statements record facts about the reference or third parties and bind nothing.
> A **parity** rule restates the observable contract of `v2.6.0-beta` (identical
> in `v3.0.0-beta`) that netid 8762 behaviour must keep; a **Reloaded-original**
> rule has no reference counterpart and says so.

R39.11.18 **An already-sent query precedes every HTLC payment, and its three
outcomes are fixed.** *(binding; parity.)* Before the maker constructs its payment,
and before the taker constructs its payment, the swap shall ask the paying coin
whether a payment to the HTLC of this swap already exists on chain (R39.11.27
records the one Reloaded exception, for a shielded coin in a fresh swap). The
question is defined entirely by the swap's own data: the HTLC time lock, the secret hash,
the wallet's own swap-derived HTLC public key and the counterparty's, from which
the redeem script, its hash and the P2SH output script follow; the swap start
block and the amount are available to the coin as hints. The answer is one of
three, and the swap acts on each as follows:

- **sent** — the coin returns the payment transaction; the swap adopts it as its
  payment and does **not** broadcast a second one. (The reference adopts the
  returned transaction without re-validating it; R39.11.4d's already-sent rule
  already requires this project to return only a transaction that carries an output
  of exactly the HTLC script, which is a strict tightening with no observable
  difference in normal operation.)
- **not sent** — the swap checks that the step's send deadline has not passed
  (maker: one third of the lock duration after the swap started; taker: the
  maker-payment wait deadline) and then constructs and broadcasts the payment.
- **undetermined** (the coin returned an error) — R39.11.19.

For a shielded coin the HTLC is a transparent P2SH output, so the query is a
transparent-address question; which backend answers it is §39.11.1 (lightwalletd,
R39.11.4d; the wallet's own send record first, R39.11.26; Electrum as the fallback,
R39.11.25) and the reference's answer to the same question is recorded in the
*Upstream divergence* note below.

R39.11.19 **An undetermined already-sent answer fails the payment step closed.**
*(binding; parity.)* When the already-sent query returns an error, the payment step
shall end with the payment-failed event of the role (`MakerPaymentTransactionFailed`
for the maker, `TakerPaymentTransactionFailed` for the taker), the swap shall
finish, and **nothing shall be broadcast**. The step shall not retry the query
internally, shall not fall back to "not sent", and shall not substitute another
source silently. The coin's error text is carried in the event. (For the taker the
DEX fee is already spent at this point; that is the reference's behaviour too and
is not changed here.)

R39.11.20 **Over lightwalletd, the already-sent query shall classify an
address-index-less backend as undetermined, never as "not sent", and shall try
every admitted endpoint before concluding.** *(binding; Reloaded-original — the
reference never asks lightwalletd an address question, so it has no counterpart;
see the divergence note.)* The serving node's address-index methods, which the
transparent-address gRPC methods of R39.11.1 are served by, reject every request
with a "no information available for the address" class of error when the node
runs without its address index; lightwalletd relays it as a failed call. The
implementation shall:

- (a) recognise that rejection as a distinct **index-unavailable** condition of the
  endpoint, separate from an unreachable endpoint, a stream that ended in failure
  (R39.11.4b) and the index-coverage failure of R39.11.4d(d); R39.11.4i is that
  recognition (the endpoint is marked "no address index");
- (b) on index-unavailable, move to the next admitted endpoint (R39.11.3) and
  remember the condition per endpoint as R39.11.4i(d) binds it (until the coin is
  activated again), so that the swap path does not re-ask an endpoint known to lack
  the index while wallet use of the endpoint is unaffected;
- (c) when every admitted endpoint is index-unavailable, ask the Electrum
  fallback of R39.11.25; only when that cannot answer either, return
  **undetermined** with the named conditions of both sources (so that R39.11.19
  fails the step), and write them to the sync-error channel of R39.8.0au;
- (d) never return "not sent" from an index-unavailable endpoint, from an empty
  answer of such an endpoint, or from a mixture of index-unavailable and
  unreachable endpoints.

The wallet's own send record is asked first (R39.11.26, which D39.11.h proposed and
the maintainer adopted); (a)–(d) govern the index query that follows it.

R39.11.21 **The spend watch tolerates per-iteration failures until its deadline and
never ends early on an error.** *(binding; parity, with a Reloaded-original
reporting addition.)* The taker's wait for the maker's spend of the taker payment
shall poll the paying coin for a spender of the payment's HTLC output at its check
interval (10 s) until the taker payment lock time; a failed poll is logged and the
next poll proceeds; the wait ends only with a spender found (then the secret is
extracted from the spender's input script, and an extraction failure finishes the
swap with the wait-failed event) or with the deadline reached (`TakerPaymentWaitForSpendFailed`,
followed by the refund path). An error shall not shorten the wait. Over
lightwalletd each failed poll is classified per R39.11.4b, R39.11.4d and
R39.11.20(a) and the named conditions are written once to the sync-error channel
per change of condition rather than per poll; an index-unavailable condition on
every admitted endpoint is reported at once but, per the first sentence, does not
end the wait, because an endpoint may recover before the deadline. (This is the
contract R39.11.16a's status note records as carve-out (4); it is normative here.)

R39.11.22 **Recovery queries are one-shot and fail closed.** *(binding; parity, with
a Reloaded-original reporting addition.)* In the manual recover-funds path, when the
saved swap carries no record of the wallet's own payment, the already-sent query of
R39.11.18 is run once: an error is returned to the caller as the recovery error,
"not sent" is returned as a not-recoverable error, and only "sent" lets recovery
continue. The spend searches of recovery (own payment, counterparty payment) are
likewise one-shot: an error aborts recovery with an error; "spent" or "refunded"
abort it with that reason; only "not spent" lets it proceed to spend or refund.
Nothing is retried inside the call and nothing is broadcast on an error. Over
lightwalletd the returned error shall name the condition (index-unavailable,
index-coverage failure, incomplete stream, unreachable) so that the operator can
change endpoint and re-invoke.

R39.11.23 **Reference lightwalletd method surface, for comparison.** *(informative;
reference fact.)* Over the whole swap lifecycle, activation included, the reference
calls four methods of the compact-transaction-streamer service: the tip method (as a
health probe before every client use, for the activation tip and at the start of
every sync pass), the tree-state method (once, at activation), the block-range
method (every sync pass), and the transaction-lookup method with the hash selector
(only as a post-broadcast existence poll of six attempts thirty seconds apart;
each failed attempt is logged, and the poll has no effect on the swap or its
status). None of the four needs the serving node's address index;
the lookup inherits the node's transaction-index limitation (R39.11.4c) but its
failure has no swap effect in the reference. The reference calls no other method of
the service: not the submission method, not the three transparent-address methods,
not the mempool, server-information or single-block methods. Every transparent
function of the reference's shielded coin — already-sent, spend search, spend
watch, confirmations, fee-transaction validation, block count and **every
broadcast** including the HTLC payment, the DEX fee and the HTLC spend
and refund — is served by the Electrum protocol (`blockchain.scripthash.get_history`,
`blockchain.transaction.get` plain and verbose, `blockchain.transaction.broadcast`,
`blockchain.headers.subscribe`) or, in Native mode, by
the node's wallet RPCs. The reference's shielded-coin swap path therefore has no
dependency on the serving node's address index at all; this project's single-backend
design (R39.11.1) acquires that dependency, which is why R39.11.4d and R39.11.20
exist. A lightwalletd outage affects the reference's swaps only indirectly: every
shielded send first waits for the shielded sync loop to hand over, so while all
endpoints fail the send blocks until the step's deadline fails it; the HTLC spend
and refund do not pass that gate.

R39.11.24 **Published ARRR endpoint list and the reference's endpoint semantics.**
*(informative; public data and reference fact.)* The reference binary ships no
default list; endpoints arrive in the activation request's `light_wallet_d_servers`
and wallet applications take them from the public coins repository, whose
`light_wallet_d/ARRR` file (master at its head dated 2026-03-05,
fetched 2026-03-07) lists, in order:
`https://lightd1.pirate.black:443`, `https://piratelightd1.cryptoforge.cc:443`,
`https://piratelightd2.cryptoforge.cc:443`, `https://piratelightd3.cryptoforge.cc:443`,
`https://piratelightd4.cryptoforge.cc:443`, `https://electrum1.cipig.net:9447`,
`https://electrum2.cipig.net:9447`, `https://electrum3.cipig.net:9447`. The
reference requires **any one** endpoint: at activation each URL is connected and a
URL that cannot be parsed, configured for TLS or connected is skipped, activation
failing only when none connected (an empty list is a distinct configuration error;
the WASM build connects lazily); per operation the list is walked in its current
order with a tip read as the probe, the first endpoint that answers is used and
moved to the front, and an operation fails only when none answers. The reference
performs no server-information read, no network, branch or height admission, and
no exclusion. This project's admission (R39.11.7a) and its strict-majority
requirement for a fresh wallet are the recorded divergence (Compatibility note,
item 2, §39.11.3); R39.11.3's try-in-order-fail-when-all-fail rule is the
behaviour the two have in common. *(Editor's note, public data, 2026-10-28:* the
same file at the repository head dated 2026-10-26 lists nine entries:
`https://lightwalletd1.cryptoforge.cc:443`, `https://lightwalletd2.cryptoforge.cc:443`,
`https://pirate.mathnodes.com:443`, `https://arrr.qortal.link:443`,
`https://arrr2.qortal.link:443`, `https://arrr3.qortal.link:443`,
`https://lightd1.pirate.black:443`, `https://electrum1.cipig.net:9447`,
`https://electrum2.cipig.net:9447`. On 2026-10-28 the last two answered every
transparent-address request with the index-unavailable rejection of R39.11.20.*)*

> **Upstream divergence (informative) — which backend answers the swap-path
> transparent questions.** For a Light-mode shielded coin the reference answers
> already-sent, spend search, spend watch, confirmations, fee validation and every
> broadcast from its Electrum servers, and never asks lightwalletd a transparent
> question. Reloaded answers them from lightwalletd (R39.11.1, R39.11.4,
> R39.11.4d), because Electrum was removed as the coin's backend (§39.11.3,
> Compatibility note item 1, and `docs/GLEEC_COMPATIBILITY.md`). For already-sent,
> spend search and spend watch it falls back to the activation's Electrum servers
> when no admitted lightwalletd endpoint can answer (R39.11.25), and for
> already-sent it first consults its own send record (R39.11.26). Observable
> consequences:
>
> 1. The address-index-less-node failure mode of R39.11.20 stops a payment step
>    only when neither source can answer; in the reference the same lightwalletd
>    configuration fails nothing, because the question is never asked of it.
> 2. Reloaded's already-sent answer verifies the returned transaction's output
>    script; the reference's does not.
> 3. For a shielded coin's own payment in a swap started fresh, Reloaded skips the
>    already-sent question (R39.11.27), and a taker whose own payment is such a
>    coin is refused at start when no source can answer (R39.11.28).
>
> None of these changes the swap wire. Item 3 ends a taker swap earlier than the
> reference would, and only when the taker could not have learned the secret. All
> three are recorded in `docs/GLEEC_COMPATIBILITY.md` under the existing
> Electrum-removal entry.

> **Code-quality finding (informative) — the reference's already-sent check does
> not verify what it found.** In the reference the transaction returned as the
> wallet's own payment is whichever appears first in the HTLC address's history;
> its outputs are not compared with the expected script or amount. The history is
> height-ordered with unconfirmed entries last, so the first entry is normally the
> funding transaction, but nothing enforces it. *Proposed handling:* keep
> R39.11.4d's requirement that the already-sent answer carry an output of exactly
> the HTLC script, and add the output-script assertion to T39.11.l. *(Trigger:
> apparent with little extra effort — the check was read side by side with the
> spend search, which does verify the script.)*
>
> **Status (2026-10-29): adopted.** T39.11.l carries the assertion ("a candidate
> transaction without such an output is not adopted as sent"), and the test
> `sent_detection_ignores_a_transaction_paying_another_script_and_a_listing_with_another_script`
> (`z_lightwalletd/htlc_index/tests.rs`) proves it over lightwalletd.

> **Code-quality finding (informative) — the reference's Native-mode already-sent
> check depends on an import made at send time.** The reference imports the HTLC
> address into the node wallet only when it sends the HTLC, and answers "not sent"
> whenever the node wallet does not know the address. A rebuilt node wallet, or a
> different node, therefore answers "not sent" for a payment that exists, and the
> swap funds a second HTLC. Native mode is secondary for this chapter; this
> project's Native-mode shielded coin should either prove the import before
> answering or answer undetermined when the address is unknown and the swap start
> block is below the node wallet's import height. *(Trigger: apparent — the import
> at send time and the unknown-address rule are in the two functions read
> together.)* Left as D39.11.j.

R39.11.25 **Electrum is the fallback for three swap-path questions of a Light-mode
shielded coin.** *(binding; Reloaded-original, maintainer Decision AK of
2026-10-28.)* Already-sent (R39.11.18), the one-shot spend search (R39.11.22) and each
poll of the spend watch (R39.11.21) are asked of the admitted lightwalletd endpoints
first, under R39.11.4d, R39.11.4h, R39.11.4i and R39.11.20. When none of them gives
an answer, the question shall be asked of the servers in the activation's
`electrum_servers`, in the order and under the time budget of chapter 38 R38.6.4.
Nothing else of the coin uses them: tip, confirmations, the builder height,
broadcast and the whole wallet half stay on lightwalletd (R39.11.9--R39.11.11).

- (a) **Chain identity first.** Before an Electrum server answers any of the three
  questions, the implementation reads its block header at the height of the
  single height authority's fresh tip minus 10. It computes the block hash as the
  double SHA-256 of the full header, and compares it with the hash an admitted
  lightwalletd endpoint reports for that height. A server whose hash differs,
  or which cannot serve the header, is not used, with a named reason. The check
  is repeated after each admission re-evaluation interval (R39.11.7a(g)).
- (b) **Tip lag.** A server whose tip is more than 10 blocks below the fresh tip
  of the single height authority is not used for that question.
- (c) **Reads.** The script history of the HTLC's P2SH output script and the
  transactions it names are read and decoded with the Zcash reader
  (R39.11.16a), never with the shared UTXO parser. Already-sent answers "sent"
  only with a transaction that has an output of exactly the HTLC script. A
  spend is a history transaction that spends the HTLC output; it is classified
  by the shared rule. "Not spent" needs the server's unspent listing for the
  script to list the HTLC output, and "not sent" needs a complete, empty script
  history, each from a server that passed (a) and (b).
- (d) **Failures.** A failed connection, a failed read or an undecodable
  transaction is undetermined, never a negative answer. When neither
  lightwalletd nor Electrum gives an answer, the question is undetermined with
  the named conditions of both sources (R39.11.19, R39.11.20(c)).
- (e) **Activation and logging.** Activation never opens, probes or fails for an
  Electrum server (R39.11.5). The first use of each server logs one line at
  `info` with its identity result; each answer from the fallback is logged at the
  level of the detection that asked (R39.11.4h(d)).

> **Status update (reloaded, 2026-10-28).** Implemented (T39.11.p).
>
> - **Construction.** The shared Electrum client is built on the fallback's first use,
>   through the same builder every UTXO coin uses. It is kept for the coin afterwards.
> - **Order and budget.** Servers are asked one at a time, in the chapter-38 order, within
>   one 60-second budget per question.
> - **Checks.** The identity check (a) is cached for the admission re-evaluation interval;
>   the tip-lag check (b) runs for every question.
> - **Errors.** When neither source answers, the error and the sync-error channel name
>   the lightwalletd conditions and each Electrum server with its own.
> - **Recorded check.** On 2026-10-28 a recovery of the September test swap ran against
>   two `lightwalletd` servers without an address index plus one published ARRR ElectrumX.
>   It reported the payment refunded, from Electrum.

R39.11.26 **The wallet's own send record answers "sent" first.** *(binding;
Reloaded-original, maintainer Decision AK of 2026-10-28; D39.11.h adopted.)* Before
any server is asked, the already-sent question shall consult the wallet database's
record of this wallet's own sends. That record is written when the backend accepts
the broadcast (§39.8.0.7) and survives a restart. A recorded send counts when its
transaction has an output of exactly the HTLC script and it is mined, or unmined
and not past its expiry height against the fresh tip read (R39.11.10a). Such a send
answers "sent": the transaction is returned and no address or transaction query
is made. A mined record needs no request at all; judging an unmined record's
expiry takes one tip read. An unmined record past its expiry is disregarded.
Without a counting record, the question goes to the servers as before, and so it
does when the record or the tip cannot be read. The local record never answers
"not sent".

Any recorded transaction that pays exactly the HTLC script counts, without a
separate check that it spends this wallet's notes: the script is unique to the
swap (a random per-swap key and a fresh secret hash, R39.5.4), so only this
wallet's own payment can carry it. A mined record is trusted as the wallet
database records it; a reorganization that the database's rewind handles clears
its mined height.

R39.11.27 **A fresh swap skips the already-sent question for a shielded coin's own
payment.** *(binding; Reloaded-original, maintainer Decision AK of 2026-10-28;
a divergence from R39.11.18's parity contract.)* When the paying coin is a shielded
coin, the payment step of a swap that this process started skips the already-sent
question and goes straight to the step's deadline check and the send. That is a
swap not restored from its saved log, whose log holds no payment event of this role.
A shielded coin's HTLC key is random per swap (R39.5.4) and the secret hash is
fresh, so no payment to that script can exist unless this swap sent it, and a swap
started in this process cannot have sent it before this step. A restored swap
always asks the question. Every other coin keeps R39.11.18 unchanged.

R39.11.28 **A taker whose own payment is a Light-mode shielded coin is refused at
start when no source can watch its payment.** *(binding; Reloaded-original,
maintainer Decision AK of 2026-10-28.)* The spend watch (R39.11.21) is the taker's
only way to learn the secret. A taker that pays while no source can see the
maker's spend loses its payment, and the maker later refunds its own. Before the
taker fee is sent, such a swap shall therefore establish that at least one source
can answer the swap's address questions:

- an admitted lightwalletd endpoint, not marked "no address index" (R39.11.4i),
  that answers a transparent-address read without that rejection; or
- an Electrum server that passes (a) and (b) of R39.11.25 and answers a
  script-history read.

If none can, the swap fails at start with `StartFailed`, and nothing is sent. The
event's error names the coin, each lightwalletd endpoint with its condition and each
Electrum server with its condition, and says that the taker could not watch its
payment for the maker's spend. A maker, and a taker paying with any other coin,
are not affected.

> **Status update (reloaded, 2026-10-28).** R39.11.27 and R39.11.28 are implemented
> (T39.11.r, T39.11.s).
>
> - **"A shielded coin" in R39.11.27.** It is decided by the property the rule rests on:
>   the coin's HTLC key is random per swap (only the shielded coin has it).
> - **"Started in this process".** Recorded when the swap is started new, never when it
>   is restored from its saved log.
> - **The address read of R39.11.28.** It is the transparent-address listing of the
>   wallet's own transparent address on each admitted lightwalletd endpoint, then the
>   script history of that address on each identity-checked Electrum server.
>   - An endpoint that answers with the no-address-index rejection is marked
>     (R39.11.4i).
>   - The check runs in the taker's start step, after the balance check and before
>     anything is sent.
> - **Not exercised end to end.** No test drives the whole start step: its balance check
>   has no test double. The refusal step is tested directly, with the fee step made to
>   fail if reached.

### 39.11.6 Endpoint naming, the endpoint that rejected, and suspension

> **Provenance (2026-10-28).** R39.11.29 -- R39.11.31 are clean-room original
> Reloaded policy, written from this repository's code, chapter 38 R38.6.4d
> and the maintainer's decisions: host-only node naming everywhere
> (2026-10-26), and naming the rejecting endpoint plus a shared suspension
> backoff for lightwalletd (2026-10-28). They take no forbidden-corpus input
> and add to §39.11 without editing its gated text. **None of them is
> implemented yet.**

R39.11.29 **Host-only naming of lightwalletd endpoints (Reloaded; clean-room
original; maintainer decision 2026-10-26; no switch).** Every log line, error
text and status message the shielded coin emits that names a
`light_wallet_d_servers` endpoint shall name it in the host-only form of
[chapter 35](35-evm-v2-activation-rpcs.md) R35.1.14: scheme, host and, when
present, port. The form is taken from the URL the client connects with, so an
entry without a scheme takes the scheme the client would use (as in
R39.11.7b). This covers at least:

- the admission logs and errors of R39.11.7a, including the excluded set of
  (f), the evaluation log of (i) and the failure that reports observed
  identifiers;
- the duplicate warning of R39.11.7b, which names both spellings, each in its
  host-only form;
- the per-endpoint failure logs and the all-failed error of R39.11.3, the
  compact-block download of an activation or background pass (R39.8.0ae) and
  its errors, and the mempool poll;
- the logs and errors of HTLC detection (R39.11.4h, R39.11.4i, R39.11.20) and
  of the taker refusal of R39.11.28;
- the rejection of R39.11.30 and the suspension log of R39.11.31;
- any lower-layer text (gRPC, HTTP/2, TLS, DNS) embedded in one of these. A
  full URL that arrives from a lower layer is replaced by its host-only form.

An entry that cannot be parsed into a scheme and a host is named by a fixed
placeholder and its position in the configured list, never by its raw string.
Chapter 35 R35.1.17 applies to the lightwalletd backend, its transport and the
endpoint list of the activation request, with the same scope limit for the RPC
wire dump.

R39.11.30 **A rejection names the endpoint that rejected (Reloaded;
clean-room original; maintainer decision 2026-10-28).** When an endpoint
answers a broadcast with a service-level failure (R39.11.4, reported as
`BroadcastRejected`):

- the error returned to the caller shall name that endpoint in the form of
  R39.11.29, together with the code and message the endpoint sent; and
- the coin shall log one line at `warn` level naming the endpoint in that
  form, the identifier of the transaction (computed locally, as R39.11.4's
  status update says), and the endpoint's code and message.

The rejection still ends the broadcast: it is not retried on further
endpoints (R39.11.4), and it does not suspend the endpoint (R39.11.31(h)). The
endpoint's name is part of the human-readable message only; it changes no
`error_type`, `error_data` shape or HTTP status.

R39.11.31 **A failing endpoint is suspended with a doubling backoff (Reloaded;
clean-room original; maintainer decision 2026-10-28; no switch).**

- (a) **What suspends.** A transport-level failure of an attempt on an
  endpoint: the connection cannot be made (name resolution, a refused
  connection, the TLS handshake); the attempt exceeds its time bound (the
  per-call bound, or the admission probe's bound); the channel breaks while
  the call runs; or the service answers with one of the gRPC status codes
  `UNAVAILABLE`, `DEADLINE_EXCEEDED`, `CANCELLED`, `ABORTED` or
  `RESOURCE_EXHAUSTED` (D39.11.l).
- (b) **The schedule** is the Electrum schedule of
  [chapter 38](38-utxo-coin-maintenance-and-features.md) R38.6.4d item 1. The
  first suspension lasts 20 s. Each further transport-level failure before the
  endpoint's next successful answer doubles it, up to 12 hours. A successful
  answer of any method resets the endpoint, so that its next suspension is
  again 20 s. A service-level answer neither suspends nor resets. Whether the
  implementation shares the Electrum code is not bound; the figures and the
  behavior are.
- (c) **Order of attempts.** Every walk over the admitted endpoints tries
  those that are not suspended, in the configured order of R39.11.3, and
  skips the suspended ones. This covers every function of R39.11.1's table
  that asks an endpoint, the compact-block download of a pass (R39.8.0ae), the mempool poll and HTLC
  detection (R39.11.4h, R39.11.4i). The configured order stays the order;
  suspension adds no rotation and no preference for an endpoint that
  answered.
- (d) **Suspension never decides an outcome alone.** When every admitted
  endpoint that is not suspended has failed, or none is unsuspended, the same
  operation then tries the suspended admitted endpoints, in configured order,
  before it fails. So an operation fails as "every admitted endpoint failed"
  only after it has attempted every admitted endpoint. An attempt on a
  suspended endpoint that fails at transport level suspends it again under
  (b).
- (e) **Admission.** The probe of R39.11.7a asks every configured endpoint,
  suspended or not, as chapter 38 R38.6.4k-R item 1 does for Electrum. A
  suspension therefore never changes the branch vote, the strict majority,
  the consensus height of R39.11.7a(j), or which admission failure is
  reported (R39.11.6). The probe feeds (b): a transport-level failure of its
  server-information read or its tip read, or the probe's own time bound,
  suspends the endpoint, and a successful server-information read resets it.
  Exclusion by admission and suspension are separate states: an excluded
  endpoint is not suspended for being excluded, and a suspended endpoint is
  not excluded for being suspended (D39.11.k).
- (f) **Height authority.** Suspension changes only which admitted endpoint
  answers a tip read. It adds no height source and keeps no last-known tip of
  a skipped endpoint (R39.11.9). R39.11.10 and R39.11.10a are unchanged.
- (g) **The "every endpoint unreachable" rows.** R39.11.6, the
  `LightwalletdUnreachable` (`Transport`) and `LightwalletdExcluded`
  (`CoinCreationError`) rows of §39.10.6 R39.10.14 and of
  [chapter 55](55-coin-activation-wire-contract.md) §55.3 keep their meaning.
  By (d) and (e), each such report rests on an attempt made in that operation
  or evaluation; a suspended endpoint is never reported unreachable without
  having been tried. The all-failed error names each endpoint it tried once,
  in the form of R39.11.29.
- (h) **What never suspends.** A service-level answer: a broadcast rejection
  (R39.11.4, R39.11.30), an explicit "not found", the no-address-index answer
  (R39.11.4i keeps its own mark), any gRPC status outside (a), and an answer
  that cannot be decoded. Exclusion by admission does not suspend either, nor
  does a stop of HTLC detection under R39.11.4h(b) or (c); a request inside a
  detection that fails at transport level does suspend.
- (i) **Logging.** Each suspension logs one line at `debug` level naming the
  endpoint in the form of R39.11.29, the failure and the length of the
  suspension.
- (j) **Lifetime.** The state is held in the running coin instance and is not
  persisted. A new activation starts with no endpoint suspended, and
  re-evaluating admission clears no suspension.
- (k) **The Electrum fallback.** The servers of R39.11.25 are suspended under
  chapter 38 R38.6.4d, as before. Lightwalletd suspension does not change when
  the fallback is asked: only after no admitted lightwalletd endpoint has
  given an answer under (d).

*Why the figures are shared.* One schedule for both backends gives an
operator one behavior to reason about: a server that keeps failing is asked
less and less often, and a server that answers is back at once.

> **Compatibility.** R39.11.29 -- R39.11.31 change no wire shape and reach no
> peer: they change only what a text says and which of the operator's own
> endpoints is asked first. By the second arm of the default-side rule
> (`docs/COMPAT_SWITCHES.md`) the Reloaded form is the default, and no switch
> is offered. They are recorded in `docs/GLEEC_COMPATIBILITY.md` with "none --
> no switch exists".

> **Status (2026-10-28).** Implemented. The lightwalletd backend names every
> endpoint host-only; `BroadcastRejected` carries the endpoint and is logged
> once at warn level; and an endpoint that fails at transport level is
> suspended on the schedule Electrum uses (`coins/endpoint_suspension.rs`).
> Admission still probes every endpoint, and an operation tries suspended
> endpoints before it fails. T39.11.t–T39.11.v cover it.

#### Tests

> **Status update (reloaded, Step C).** T39.11.a--T39.11.d, T39.11.f and
> T39.11.e (Step D), T39.11.g (Step C swap half) and T39.11.h--T39.11.j exist, in part.
> They run against a
> scripted in-process transport, with no network:
> `z_coin/z_lightwalletd/tests.rs` (admission, failover, monotonic height, builder
> height, confirmation wait, byte order, balance request bytes, history reader),
> `z_coin.rs` `light_backend_tests` (the Light build is lightwalletd whatever the
> request says about Electrum; fixed-fee refusal) and `z_coin_activation.rs`
> (the activation wire). Not covered: T39.11.a's "a shielded receipt mined after
> activation becomes visible" and "every function answered from the fixture" are
> covered per function, not by one end-to-end run, and the two full transaction
> builders are not driven from one injected tip (T39.11.c).
>
> **T39.11.g** is `z_coin/z_lightwalletd/htlc_index/tests.rs`. Decisions, from
> hand-assembled version-4 transactions: a spend under the pubkey-hash form, and under
> the script-hash form (an older build), is found and its matching input (not input 0)
> is reported; a transaction spending another output or vout is not the spend; the
> unspent listing under either form is the negative answer; a row with the right key
> and the wrong script is discarded; **a healthy empty index for a mined or unknown
> funding is an index-coverage failure and not a negative**; an unmined funding with a
> clean empty index is not spent; a failed call, a skipped height, a stream failure
> and an undecodable delivered transaction each leave the decision undetermined and
> differ from the empty success, including for an unmined funding; a spend is still
> found when the stream that delivered it failed afterwards or another form failed; a
> history showing the funding and no spender is **not** "not spent"; a healthy endpoint
> answers for a failing one; already-sent detection separates found (history and
> listing), not sent, and could-not-tell; the P2SH script shape check; and the two
> forms of the 2026-10-12 live fixture encode to the observed `R...` and `b...`
> strings. Orchestration, over a scripted transport: both forms and the unspent listing
> are requested with no bound below 1, the answering form is remembered per endpoint, an
> endpoint that passes admission and then fails the index is skipped, total failure is an
> error, an unservable height is the gap condition, and the search window floors at the
> Sapling height and takes in the funding height. **Not covered offline:** the live
> chain's index with a spend in it. The opt-in `live_spend_detection_of_a_redeemed_htlc`
> in `z_coin/z_htlc_live_probe.rs` (inert unless `ARRR_TESTCHAIN_LIVE_DIR` is set) funds
> a fresh HTLC, redeems it in the same run, and asserts the routed detection and
> `extract_secret`; it has to be run by someone who may broadcast.
>
> **T39.11.e** is `z_coin/z_tx_read.rs` (`tests`): inline recordings of one version-4
> and one version-6 transaction (fixtures in `z_coin/z_tx_read_fixtures.rs`), plus
> hand-assembled version-4 and version-5 transparent transactions for the HTLC
> decisions. It pins that the shared parser refuses the version-6 recording while the
> routed reader returns the chain's identifier and not a double-SHA256 digest; that
> a version-4 recording reads identically (identifier, expiry, outputs, round trip)
> under both parsers; that the payment-output and secret-extraction decisions agree
> across both parsers on version 4 and the routed reader also reads version 5; the
> three identifier byte-order boundaries on a non-symmetric identifier; and, by
> source inspection of the swap-operations file, the HTLC builder and the coin's
> market operations, that no swap step names a shared parser-bound helper. An opt-in
> run over the offline corpus (`ARRR_IRONWOOD_FIXTURES` pointing at the untracked
> `local/arrr-ironwood-fixtures`) checks all 19 recorded transactions. Not covered:
> the asynchronous wrappers are not driven end to end (no scripted `ZCoin`
> exists); the pure decisions they make are.
>
> **Retired:** `light_mode_still_requires_both_server_lists`
> (`z_coin_activation.rs`) asserted that an absent `electrum_servers` fails Light
> activation; it is replaced by `light_mode_requires_only_the_lightwalletd_list`.

T39.11.a A single-backend test shall drive a Light-mode shielded coin against a
lightwalletd fixture only, with no Electrum server reachable or configured, and
shall prove: that activation succeeds; that every function of R39.11.1's table is
answered from the fixture; that no Electrum connection is attempted; and that a
shielded receipt mined after activation becomes visible in balance and
`z_coin_tx_history` without re-activation.

T39.11.b An activation-wire test shall prove that `light_wallet_d_servers` absent
or empty fails Light-mode activation as a configuration error before any shielded
work begins (R39.11.5); that `electrum_servers` absent, empty, and populated with
unreachable servers all activate identically and successfully (R39.11.5,
R39.11.7); and that a configured endpoint set which is entirely unreachable, and
one which is entirely excluded by admission, each fail activation with
distinguishable typed errors and never a placeholder tip (R39.11.6).

T39.11.c A builder-height test shall drive the ordinary shielded send path and
the HTLC spend/refund construction path from a single injected tip and assert
that both produce the same expiry height; with an injected tip more than the
builder's expiry delta behind the fixture chain tip, assert that both paths
refuse to build rather than producing a transaction whose expiry height is
already past; and with the tip read failing, assert that both return a typed
error rather than falling back to the wallet database's target height
(R39.11.11).

T39.11.d A confirmation-wait test shall prove that an unfetchable transaction
combined with a tip past its expiry height is reported distinguishably from a
genuine expiry (R39.11.12); that a tip-read failure leaves the coin in the
history-sync error state carrying the height scanned so far; and that
confirmation depth is computed from the mined height and the tip, with an unmined
transaction and a transaction above the tip both yielding zero (R39.11.4e).

T39.11.e A parser-coverage test shall pin, for recorded transactions of each
version the coin's network accepts: which parser each swap step of R39.11.13
uses; that each step's decision is identical under the old and the new parser for
every version the old parser supported, version 4 included (R39.11.16a(a)); that
the identifier computed for a version-5-or-above transaction is the ZIP-244
identifier and not a double-SHA256 digest; and that no swap step of R39.11.13
still reaches the shared UTXO parser for a ZHTLC coin (R39.11.16a(b)). A fixture
whose identifier is not byte-order symmetric shall be used, per the reasoning of
R39.8.7a and R39.11.16a(c).

T39.11.f An admission test shall prove, against server-information fixtures
(R39.11.7a): that an endpoint reporting a different network name or a different
Sapling activation height is excluded; that with the coin's configuration unable
to name the upgrade active at the reported height, a wallet carrying scanned
state admits only the partition matching the identifier recorded with that state
and excludes a higher-tipped endpoint from another partition **without
rewinding**; that a fresh wallet admits the strict-majority partition and fails
with a typed error reporting every observed identifier when no partition holds a
majority, including the single-endpoint case; and -- the inversion case this
requirement exists for -- that a configuration which does *not* declare the
chain's newest upgrade height does **not** exclude the endpoints on the chain's
current branch in favour of the pre-upgrade ones.

T39.11.k A staleness test shall prove (R39.11.7a(j)), against fixtures in which the
node height and the cache height are separate inputs: that an endpoint reporting the
current node height with a cache far behind it is excluded and never asked for
anything; that an empty or failing cache read is excluded; that an endpoint behind the
consensus height of the others is excluded; that lags of a few blocks are admitted and
the tolerance is exact on both sides; that a stale endpoint does not vote; and that a
failure to establish a majority names the excluded and the unreachable endpoints.

T39.11.g A P2SH-coverage test shall prove (R39.11.4d): that both address
encodings of the same redeem-script hash are queried, with neither hard-coded;
that a fixture answering only under the pubkey-hash re-encoding is handled, and
one answering only under the script-hash encoding is handled too; that a returned
row whose output script is not the expected P2SH script is discarded rather than
accepted on the key alone; that a query over an HTLC address whose funding
transaction is known and mined inside a gap-free queried window, which completes
*without* returning that funding transaction under either encoding, is reported as
an index-coverage failure through the sync-error channel and does **not** yield
"not spent"; and that the same query returning the funding transaction but no
spending transaction does yield "not spent".

T39.11.h A stream-robustness test shall prove (R39.11.4b): that a history stream
which delivers entries and then fails is not reported as "no history" and its
delivered entries are retained; that the failing window is bisected and the
offending height skipped rather than failing the pass, and that the skipped height
is recorded as a gap; that a swap-path negative answer is refused over a window
containing a gap while history/sync still advances; that resumption starts at the
last received height, not one past it, with duplicate transactions suppressed by
identifier; that no open-ended range request is issued; and that no request is
issued with a range bound below 1.

T39.11.i A monotonicity test shall prove (R39.11.10) that a tip read lower than
the highest previously observed tip neither lowers the coin's reported
`current_block` nor triggers a rewind, re-anchor, or discontinuity recovery, and
that a genuine hash-link discontinuity at a height both sides claim still does.

T39.11.j A balance-request test shall prove (R39.11.4a) that the
transparent-balance request is emitted with the address list as a flat repeated
field, asserted against the **encoded request bytes** and not only the decoded
structure; that a multi-address request is issued as one call; and that an
invalid-address-list rejection is reported as a request-or-index condition rather
than as "these addresses are invalid".

T39.11.l An already-sent-before-payment test shall prove, for both roles over a
scripted lightwalletd transport (R39.11.18, R39.11.19): that the payment step
issues the already-sent query before any construction; that a "sent" answer
carrying a transaction with an output of exactly the HTLC script is adopted and
nothing is broadcast; that a candidate transaction without such an output is not
adopted as "sent"; that a "not sent" answer past the step's deadline fails the step
without broadcasting; and that an error answer ends the step with the role's
payment-failed event with nothing broadcast and no second query.

T39.11.m An index-unavailable test shall prove (R39.11.20): that a transparent
address method failing with the node's address-index-disabled rejection is
classified as index-unavailable and distinguished from unreachable, from an
incomplete stream and from the index-coverage failure; that the next admitted
endpoint is tried; that the condition is remembered per endpoint for the
re-evaluation interval and does not affect the wallet half's use of the endpoint;
that with every admitted endpoint index-unavailable the answer is undetermined with
the named condition and is written to the sync-error channel; and that no
combination of index-unavailable and unreachable endpoints yields "not sent".

T39.11.n A spend-watch tolerance test shall prove (R39.11.21): that transient poll
failures do not end the wait; that a spender delivered after failed polls is
returned; that reaching the deadline yields the wait-failed outcome; that an
all-endpoints index-unavailable condition is reported once per change and the wait
still runs to its deadline; and that a spender found after such a condition is
still returned.

T39.11.o A recovery test shall prove (R39.11.22): that with no own-payment record
the already-sent query runs exactly once; that an error is returned as the recovery
error with nothing broadcast; that "not sent" is a not-recoverable error; and that
over lightwalletd the error names the condition.


T39.11.p An Electrum-fallback test shall prove (R39.11.25), against scripted
lightwalletd and Electrum transports: that with a lightwalletd endpoint that
answers, Electrum is never asked; that with every endpoint marked "no address index",
already-sent, the spend search and a spend-watch poll are answered from Electrum;
that a server whose header hash differs, or whose tip lags by more than 10 blocks, is
not used and the reason is named; that a spend found through Electrum yields the
secret; that "not spent" needs the unspent listing and "not sent" a complete empty
history; that every failure is undetermined, never negative; that a version-6
transaction is read with the Zcash reader; and that activation with only
unreachable Electrum servers succeeds.

T39.11.q A local-record test shall prove (R39.11.26): that a recorded own send
paying exactly the HTLC script answers "sent" with no server request; that a mined
record counts; that an unmined record past its expiry is disregarded and the servers
are asked; that a recorded send without the HTLC output is disregarded; and that the
record never answers "not sent".

T39.11.r A fresh-swap test shall prove (R39.11.27): that a swap started in this
process with no payment event sends a shielded coin's payment without the
already-sent question; that a restored swap asks it; and that a transparent coin
asks it in both cases.

T39.11.s A taker-refusal test shall prove (R39.11.28): that a taker whose own
payment is a Light-mode shielded coin, with every lightwalletd endpoint marked and
no usable Electrum server, fails at start with `StartFailed`, the named error and no
taker fee sent; that one capable lightwalletd endpoint, or one identity-checked
Electrum server, lets the swap proceed; and that a maker, and a taker paying another
coin, are not affected.

T39.11.t A naming test shall prove (R39.11.29), against scripted endpoints
configured with userinfo, a path and a query string, one of them failing every
call and one of them excluded by admission: that every captured log line, the
admission failure, the all-failed error, a detection error and the debug forms
of the backend and the parsed request contain each endpoint's host-only form
and none of the userinfo, path or query parts; that an entry without a scheme
is named with the scheme the client uses; that the duplicate warning of
R39.11.7b names both spellings in host-only form; and that an entry that cannot
be parsed is named by the placeholder and its list position.

T39.11.u A rejection test shall prove (R39.11.30): that a broadcast answered
with a non-zero error code by the first endpoint returns an error naming that
endpoint in host-only form with the code and message, logs one `warn` line
with the endpoint, the transaction identifier, the code and the message, is not
sent to the second endpoint, and leaves the first endpoint unsuspended; and that
the error's `error_type`, `error_data` shape and HTTP status are those of
R39.11.4.

T39.11.v A suspension test shall prove (R39.11.31), with an injected clock,
against scripted endpoints A, B and C in that order:

- A failing at transport level is suspended for 20 s, then 40 s after a second
  failure, up to the 12-hour cap, and a successful answer resets it to 20 s;
- while A is suspended, a tip read, a block download and a transparent read go
  to B first and send A nothing;
- with A and B suspended and C failing, the operation then tries A and B before
  it fails, and the all-failed error names each endpoint once;
- an admission evaluation while A is suspended still probes A, and A's branch
  still counts in the vote (a configuration where skipping A would change the
  majority shows the same outcome as with A unsuspended);
- with every endpoint unreachable, activation still ends with
  `LightwalletdUnreachable` after attempting each endpoint;
- a broadcast rejection, a "not found", the no-address-index answer and an
  undecodable answer each leave the endpoint unsuspended;
- the reported height stays monotonic and the fresh tip of R39.11.10a comes
  from the endpoint that answered;
- a new backend starts with no endpoint suspended.

#### Deferred Work

D39.11.a **Resolved 2026-10-11.** Shielded-backend-only Light operation, recorded
in the first form of this subsection as unsupported, is now the specified and
only supported design (R39.11.7). The three capabilities named as missing are
specified in R39.11.1, R39.11.4 and R39.11.4d respectively. The motivating live
condition -- a healthy, current shielded backend alongside stalled transparent
servers -- is the condition the reversal exists to make workable.

D39.11.b **Withdrawn 2026-10-11.** The cross-backend divergence tolerance this
item deferred has no subject: there is no second backend to diverge from. What it
was for -- deciding when two independent chain views disagreed enough to warrant
a report -- is replaced by two narrower rules that need no tolerance constant:
chain *identity* is settled by endpoint admission (R39.11.7a), and intra-list
height *lag* is settled by monotonicity plus the existing hash-link rule
(R39.11.10). The only residual need for a lag threshold would be endpoint
*selection* -- declining to use an endpoint that has fallen far behind -- which
R39.11.10 permits as an explicit named constant and explicitly forbids from
influencing chain selection.

D39.11.c **Resolved 2026-10-11** in favour of routing a ZHTLC coin's swap-path
reads to the Zcash library parser rather than extending the shared UTXO parser;
the decision and its three constraints are now normative as R39.11.16a. The
implementation of R39.11.16/R39.11.16a is done (plan Step D; see the status note
after R39.11.16a for its carve-outs); R39.11.17's remaining dictated preconditions are
outside this project's control and are tracked separately.

D39.11.d **The P2SH index workaround is proved for an unspent HTLC; detection of a
spend is not yet observed live.** *(This item replaces the earlier form that recorded the
workaround as source-derived and unverified.)* On 2026-10-12 a funded HTLC existed on the
test chain and was probed on both test endpoints: the `b...` form answered empty on all
three transparent methods, the `R...` form answered with the row, the UTXO and the
balance (R39.11.4d's status note has the figures). What stays open:

- a spend of an HTLC has not yet been seen through the index; the shape of that
  transaction (transparent input, no transparent output) is the one a node without its
  transaction index cannot serve (D39.11.g), and the opt-in live test is what settles
  it for the test endpoints;
- which key type an endpoint's rows carry still depends on the binary that indexed
  them, which is why both forms are probed on every detection rather than the
  workaround being hard-coded;
- the risk falls only on the swap half; the wallet half is unaffected.

The durable fix is a node-side correction, after which the workaround should be removed,
not kept.

*Observed live (2026-10-29).* The first bullet is settled for the main chain: the detection
functions found a mined HTLC spend through the index of two lightwalletd endpoints that
keep the transaction index, in about 230 ms, and `extract_secret` recovered the secret
from it (R39.11.4d's status note has the detail). A node without its transaction index
remains D39.11.g.

D39.11.e **Transaction submission is declared but not yet observed.** The
service's submission method is specified as the broadcast route (R39.11.4) and is
part of the dictated contract, but it has not been exercised against the deployed
node, because doing so requires a validly signed transaction of the chain's
current version, which depends on the crate bump and the version-6 construction
work. Until it is observed, the submission path's failure reporting
(service-level rejection versus unreachable endpoint) is specified but unverified.

*Resolved for the success path (2026-10-29).* Submission has been observed: the deployed
service accepted version-6 transactions this project submitted on 2026-10-26 and
2026-10-27, and each was mined (R39.11.4's status note). A service-level rejection has not
been seen live and stays covered by the scripted tests only.

D39.11.f **The `requires_notarization` policy for a ZHTLC coin needs a maintainer
decision.** R39.11.4e establishes the fact -- no notarized-depth field exists on
this service, so a notarized depth would be fabricated from plain confirmation
depth -- and requires that it not be silently satisfied. Which observable form
the refusal takes is a policy choice: refusing the activation request that
asserts it, refusing only at the point a notarized depth would be required, or
reporting the coin as not notarization-capable. The published ARRR entry at the
head dated 2026-10-26 does not set it, but earlier published revisions did
(D39.11.i), so a deployment that ships or caches such a revision is affected; this
chapter specifies the prohibition without choosing the form.

*Decided 2026-10-29 (maintainer Decision AT): kept as a known issue.* Asserting
`requires_notarization` for a ZHTLC coin stays accepted, at activation and by the
`set_requires_notarization` request; nothing refuses it there. Plain confirmation depth is
still never substituted (R39.11.4e), so a swap that requires notarization for such a coin
fails at the confirmation wait, after its payment is on chain, and ends by the swap's
ordinary failure path. Older published coins-file revisions set the flag for ARRR
(D39.11.i); an operator running ARRR must set `requires_notarization` to `false`. The
item stays open in `docs/plans/arrr-ironwood-compatibility.md` §11.6.

*Amended 2026-10-30 (maintainer Decision AT, amendment): ignored with a warning;
supersedes the known-issue form above.* The flag selects a notarized confirmation count,
and a ZHTLC coin has none; the plain depth of `required_confirmations` is the only measure
there is, so the flag is accepted and has no effect (R39.11.4e as amended). The known-issue
form failed such a swap once a payment was on chain, and the funds came back only by
refund after the swap's lock times had passed. A counterparty's settings can never raise
the flag a node waits with, so with the coin reporting `false` no counterparty setting
reintroduces the wait. The swap lock time no longer takes the notarization multiplier from
this node's setting for the coin. Both peers compute the lock time from the confirmation
settings both sides sent when the order matched, so they agree: a peer that still sends
`true` for the coin (a coins file from an older revision) makes it four times as long on
both sides, as before. The published coins file sets `false` for ARRR (D39.11.i), so
peers that use it get the ordinary lock time. A peer that computed a different lock time
would be stopped by the taker's check of the maker's lock time at negotiation, before
any payment. An operator no longer needs to edit the coins file. The item is closed in
`docs/plans/arrr-ironwood-compatibility.md` §11.6.

*Known residual (2026-10-30).* An order whose `base_nota` / `rel_nota` explicitly sets
`true` for the coin still advertises it, and that still makes the swap lock time four
times as long on both sides. It no longer changes any wait. Clearing an explicit order
value would take a coin-type check in each place that builds an order's settings, so it
is left as it is.

D39.11.g **The serving node's transaction index is an unverifiable operational
precondition for swap-path spend detection.** Without it the node cannot serve the
raw bytes of a transaction that is fully spent or has no transparent output, and
an HTLC spend -- transparent input, shielded output -- is exactly that shape
(R39.11.4b, R39.11.4c, R39.11.17). The implementation cannot require the setting,
cannot read it from the server-information method, and can only detect it from the
failure. Two things are therefore open and need a maintainer decision rather than
an implementation choice: whether an endpoint observed to fail this way should be
excluded from the admitted set for swap-path use while remaining admitted for
wallet use, and whether a coin with no endpoint that can serve this shape should
refuse to enter swaps -- a refusal of the shape R39.6.4b already provides for a
different cause. At least one live endpoint was observed in this state on
2026-10-11, so this is not a theoretical configuration.

*Deferred to after the beta (2026-10-29).* Both questions are decided with the
lightwalletd-only transaction walk, which keeps the transaction index as a precondition
of the spend questions and carries them as its open question 4
(`docs/plans/arrr-htlc-lightwalletd-txwalk.md`). For the beta the rules already in force
narrow the exposure: an endpoint without an address index is marked and skipped
(R39.11.4i), the three swap questions go to the Electrum fallback when no admitted
endpoint answers them (R39.11.25), and a taker that would pay with a shielded coin is
refused at start when no source can watch its payment (R39.11.28).

D39.11.h **Decided 2026-10-28 (maintainer Decision AK): adopted as R39.11.26.** The
proposal is kept below as the record. *(Reloaded-original proposal.)* This project records every
shielded send it makes (the in-flight record of §39.8.0.7 persists each sent
transaction's identifier and expiry across restart). A locally recorded send whose
transaction carries an output of exactly the HTLC script, and which is mined or not
yet past its expiry height against the single height authority, is proof of "sent"
that needs no address index; an expired, unmined record is disregarded. Adopting it
would let a restarted wallet answer R39.11.18 against an index-less endpoint set,
with the index query of R39.11.4d as corroboration rather than sole source; a
locally unknown payment would still need the index and would fall under
R39.11.20(c). The reference has no local source (R39.11.23), so this is a
divergence to record if adopted. The risk if **not** adopted: against the
address-index-less endpoints observed in the field, every shielded swap fails at the
payment step by R39.11.19, which is the safe but unusable outcome.

D39.11.i **D39.11.f's premise depends on the coins-file revision.** The published
ARRR `coins` entry at the head dated 2026-03-05 (fetched 2026-03-07) sets
`requires_notarization: true` and `required_confirmations: 2`. The head dated
2026-10-26 sets `requires_notarization: false` and `required_confirmations: 5`,
as it has since 2026-09-21; between those dates the published value changed more
than once. D39.11.f's "no published configuration sets it" holds for the current
head only, and a wallet that ships or caches an older coins file asserts it. The reference honours the flag through the node's notarized confirmation count
relayed in the verbose transaction answer of its transparent backend; this project
has no notarized-depth source (R39.11.4e). The maintainer decision D39.11.f asks for
therefore affects every ARRR activation configured from such a revision, not a
hypothetical one; D39.11.f is amended accordingly (2026-10-28).

D39.11.j **Native-mode already-sent on a node wallet that did not make the import.**
See the second code-quality finding above. Decide between proving the import
before answering and answering undetermined for an unknown address whose swap start
block predates the import.

D39.11.k **Admission probes suspended endpoints.** *(Decided 2026-10-29,
maintainer: as implemented.)* The decision of 2026-10-28 names admission among
the places that skip a suspended endpoint. R39.11.31(e) does not: the probe asks
every configured endpoint. Skipping a suspended endpoint there would leave it
out of the branch vote and the consensus height, so a suspension could change
which branch a fresh wallet settles on, and R39.11.7a was written to keep that
choice independent of which endpoints happen to answer. The cost is one probe
per suspended endpoint per evaluation, every 300 s. The maintainer confirmed
this reading.

D39.11.l **Which failures suspend, and suspension per endpoint.** *(Decided
2026-10-29, maintainer: as implemented.)* R39.11.31(a) counts as
transport-level the failures this repository's lightwalletd client already
treats as transport failures, the five gRPC codes included. Two readings are
open:

- `RESOURCE_EXHAUSTED` usually means a rate limit, and `ABORTED` a conflict;
  both suspend under (a);
- suspension is per endpoint, not per method. An endpoint whose proxy answers
  one method with a gateway error, which reaches the client as `UNAVAILABLE`,
  is suspended for every method, though it may serve the others.

The maintainer confirmed this reading.

D39.11.m **Naming an unparsable entry by its position.** *(Decided 2026-10-29,
maintainer: as implemented, placeholder and position; chapter 35 R35.1.14 now
says the same.)*
R39.11.29 and chapter 38 R38.6.9a name an entry that cannot be parsed by a
placeholder and its position in the configured list. Chapter 35 R35.1.14 now
says the same for EVM nodes, where the position comes from R35.1.16.

D39.11.n **The "invalid address list" rejection keeps the ordinary failure class -- after
the beta.** *(Recorded 2026-10-29.)* As R39.11.4a's status note says, the node's rejection
of a balance request as an invalid address list surfaces as an ordinary endpoint failure
that carries the node's text. Reclassifying it as a request-or-index condition would change
only the class of the error; the request itself is refused either way, and a P2SH address
never reaches the method (R39.11.4d).

## 39.7 Acceptance criteria (chapter)

- Baseline: a light-mode shielded coin activates via `task::enable_z_coin::init`
  (or its flat `init_z_coin` alias), advances through the documented progress
  states, accepts multiple lightwalletd endpoints, and returns `current_block` +
  `wallet_balance` (§39.2--§39.3).
- Activation wire contract (§39.10): the four `task::enable_z_coin::*` methods
  and the flat aliases are routed on native and WASM; the request schema of
  R39.10.4 (optional `mode`, `"Electrum"`/`servers` synonyms, connection bounds,
  non-zero `scan_blocks_per_iteration`) parses as bound; in-progress values
  include `{"TemporaryError": string}`; task errors map exactly as R39.10.11 --
  R39.10.14 (height/balance failure -> `Transport` string; unresolvable date
  and start-of-task lightwalletd outage -> `CoinCreationError {ticker, error}`;
  mid-sync backend failure -> `TemporaryError` with retry, not a terminal
  error; missing parameters -> `error_data.error` `ZCashParamsNotFound`), and
  none of the discriminants of R39.10.13 is observable (T39.10.1--T39.10.7).
- A shielded HTLC swap completes maker and taker legs and a refund path (§39.5).
- R39.6.3 is implemented with integrity-check behaviour enforced before prover
  initialization.
- Config-loading (R39.1.2–R39.1.4): a `{"type":"ZHTLC","protocol_data":{...}}`
  config with a well-formed `consensus_params` (plus optional `check_point_block`
  and `z_derivation_path`) deserializes successfully; a bare
  `{"type":"ZHTLC"}` (no `protocol_data`) is rejected at coin-config parse time.
- Parameter sourcing (R39.6.4, implemented): a ZHTLC coin whose `protocol_data`
  declares non-mainnet HRP/b58 prefixes, `coin_type`, or activation heights
  produces addresses/keys under those declared values, not from hardcoded
  mainnet constants. Its shielded sync begins, in **Native** mode, at the
  declared `check_point_block` (or `sapling_activation_height` when absent);
  in **Light** mode, at the resolved sync start -- a caller-supplied
  `sync_params`, or the default recent window of R39.8.0av when none is
  supplied -- whose anchor is fetched from the backend rather than read from
  the configured checkpoint (R39.6.4 §3, R39.8.0g).
- R39.6.4 is implemented, including HD-derived shielded key-policy support and
  its "Transparent key under HD" bullet (Decision D39.5.a, 2026-10-28).
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
- Post-activation shielded sync (§39.8.0.5): after a Light-mode activation
  completes, a subsequently mined shielded receipt becomes visible in balance and
  `z_coin_tx_history` without re-activation, within one block interval plus at
  most one poll period; the background task keeps the wallet's sync anchor
  unchanged, survives a single-endpoint failure, stops when the coin is disabled,
  and cannot run concurrently with an activation that rebuilds or renames the
  same wallet database.
- Pending shielded receipts (§39.8.0.6, implemented -- D39.8.0c resolved
  2026-08-09): an unconfirmed wallet-owned shielded receipt observed through
  `GetMempoolTx` appears only in the non-spendable balance, never in
  spendable or tradable amounts, is not double-counted when the transaction
  is mined and scanned, and disappears if the transaction is dropped or
  expires.
- In-flight note exclusion (§39.8.0.7): after a Light-mode shielded send is
  accepted by the backend, the notes it spends are no longer selectable and stay
  unselectable across a daemon restart, until either the containing block is
  scanned or the wallet's scanned tip passes the transaction's expiry height; a
  rejected broadcast leaves every selected note immediately reusable and the
  balance unchanged; two concurrent sends never select the same note
  (R39.8.0at -- implemented, D39.8.0d resolved 2026-09-22). A withdrawal
  submitted outside this daemon remains uncovered (D39.8.0e).
- Generic history separation: for an activated ZCoin, generic `my_tx_history`
  v2 rejects the coin as unsupported for that method; Desktop uses
  `z_coin_tx_history` for shielded history (§39.8.0c).
- `z_coin_tx_history` returns a paginated page of shielded transaction detail
  entries for an activated shielded coin, honours `limit` and both
  `PageNumber`/`FromId` paging modes, echoes paging metadata, reports
  `sync_status: Finished` while the shielded sync is healthy, and rejects
  non-shielded coins (`NotSupportedFor`) and inactive coins (`CoinIsNotActive`)
  (§39.8).
- Swap recovery (§39.5, R39.5.4): recovery of a shielded swap rebuilds the
  HTLC script with the swap's own per-swap key, so a refunded payment is
  reported as refunded, a spent one yields its secret, and an expired one is
  refunded (T39.5.4).
- Bounded recovery (§39.5, R39.5.5; R39.11.4h): `recover_funds_of_swap`
  returns within 10 minutes and names the stage it stopped in. Shielded HTLC
  detection stops an endpoint that cannot serve the history, after a few
  requests or 120 seconds, tries the next endpoint, and logs each endpoint and
  address form it read. An endpoint whose node keeps no address index is
  marked and skipped for the rest of the activation (T39.5.5, T39.11.4h,
  T39.11.4i).
- Swap-path transparent questions of a Light-mode shielded coin (§39.11.5):
  already-sent consults the wallet's own send record first. Already-sent, spend
  search and spend watch fall back to identity-checked Electrum servers when no
  lightwalletd endpoint can answer. A fresh swap skips already-sent for a shielded
  coin's own payment, and a taker paying such a coin is refused at start when no
  source can watch its payment (T39.11.l--T39.11.s).
- History order and unmined sends (§39.8.5.1): by default an own shielded send
  is served from broadcast on, ahead of every mined row, with its final
  amounts, and moves to its block's position once mined (R39.8.9i, decided in
  D39.8.0h); with `shielded_history_unmined_sends` set to `false` it is absent
  until mined and scanned, as in the reference. Mined rows are served
  newest-first with the mined block's height and time; rows in one block are
  served in ascending `internal_id`; the order is identical across repeated
  requests and restarts (T39.8.9a--T39.8.9d).
- Sync-failure reporting (§39.8.0.5, R39.8.0au): when a background pass fails —
  unreachable chain tip, every shielded backend failed, or a scan error — the
  reported history-sync state is the error state carrying a message and the
  height scanned so far, not the in-progress state; the next successful pass
  clears it.
- Single backend (§39.11.1, required change): every function in R39.11.1's table
  is obtained from `lightwalletd`; a Light-mode ZHTLC coin activates and operates
  with no Electrum server configured or reachable; no Electrum connection is
  opened on its behalf; and the transparent-balance request encodes its address
  list as the dictated flat repeated field, asserted on the encoded bytes.
- Activation requirements (§39.11.2, required change): `light_wallet_d_servers`
  absent or empty fails Light-mode activation as a configuration error;
  `electrum_servers` absent, empty or unreachable all activate identically and
  successfully; an entirely unreachable endpoint set and an entirely
  admission-excluded endpoint set each fail with distinguishable typed errors and
  never a placeholder tip.
- Endpoint admission (R39.11.7a, required change): an endpoint on another fork of
  the chain is excluded from use rather than merely reported, a wallet with
  scanned state stays bound to the branch recorded with that state without
  rewinding, and a configuration that does not declare the chain's newest upgrade
  height does not invert the check by excluding the current-branch endpoints.
- One builder height authority (R39.11.11, required change): the ordinary
  shielded send path and the HTLC spend/refund path derive the same expiry
  height from one injected tip, both refuse to build rather than emit a
  transaction whose expiry height is already past, and neither falls back to a
  second height source when the tip read fails.
- Negative answers are earned, not defaulted (R39.11.4b, R39.11.4d, required
  change): a transparent history window that fails part-way, or completes with a
  skipped height, is never reported as "no history" and never yields a negative
  swap-path answer; windows are bounded with both bounds at least 1; resumption
  starts at the last received height with duplicates suppressed; and a P2SH query
  that returns the known funding transaction under neither address encoding is
  reported as an index-coverage failure rather than "not spent".
- Swap-path parser coverage (§39.11.4, required change): every swap step of
  R39.11.13 reads the coin's bytes through the Zcash library parser (R39.11.16a),
  which accepts every transaction version the coin's network accepts and computes
  the identifier by the rule that version dictates, with identical validation
  decisions to the previous parser on every version it supported, version 4
  included; no swap step of a ZHTLC coin still reaches the shared UTXO parser.

## 39.9 Provenance Footer

- *Inputs:* the project's own revision history and current tree, for the
  T-DOC parts of this chapter (§39.1-§39.5's as-built native ZCoin type,
  dual activation modes, `init_z_coin` task-RPC trio, shielded HTLC swap
  operations, and §39.8's `z_coin_tx_history` method -- by public
  behaviour and wire-contract shape only, no code transcribed); the
  published Komodo DeFi Framework API documentation (the source of truth
  for §39.6's WASM/sync-tuning/parameter-integrity ports and §39.8's
  `z_coin_tx_history` wire contract, per this chapter's own "Source of
  truth"/"Source-of-truth note" callouts); the published Zcash Sapling
  protocol (shielded note/commitment-tree semantics, spend/output proving
  system) and the published lightwalletd gRPC service contract (both
  explicitly named as this chapter's binding-scope source of truth); the
  present reloaded workspace's own shared transaction-history HTTP
  status/error-discriminant mapping that §39.8's `z_coin_tx_history`
  reuses from the generic `my_tx_history` (v2) method; the published lightwalletd
  compact-transaction-streamer service definition, which this repository carries
  as its generated service contract (the source of truth for every method,
  message field and selector constraint tabled in §39.11.1 and R39.11.4a
  --R39.11.4f); the published source of the selected `zcash_primitives` release
  (the source of the expiry-delta constant and the transaction-version coverage
  facts in R39.11.8(e), R39.11.15 and R39.11.16a); this repository's own shared
  UTXO RPC operations trait and shielded-coin call sites (the subject of
  R39.11.4g and of the code-quality findings in §39.11.3); this repository's own
  current tree read a second time, on 2026-10-11, purely to establish §39.11's
  implementation status -- which gRPC methods the shielded coin actually reaches,
  where each height consumer takes its height, what the activation wire currently
  requires, and that no `T39.11.*` test exists (the source of every
  `Status update (reloaded)` note in §39.11 and of those added at R39.2.2,
  R39.8.0ae and R39.8.0au); public-network
  observations of the ARRR Electrum and lightwalletd endpoints on 2026-10-08 and
  of all ten published Pirate hosts plus the live transparent-RPC probe on
  2026-10-11 (the §39.11 preamble, R39.11.4a--R39.11.4e, R39.11.7a and
  R39.11.17); the **published source of the Pirate node and of Pirate
  `lightwalletd`** -- a third-party chain's own public source, not this
  project's lineage -- which is the authority for the transparent address-index
  key-type defect and its workaround (R39.11.4d), for the stream-abort and
  dropped-final-height-group behaviour and the range-bound rule (R39.11.4b), and
  for the consensus rule that pins the on-chain redeem-script reveal and its
  fixed shape (the informative note under R39.11.4d); and the maintainer's
  recorded design decision of 2026-10-11
  (`docs/plans/arrr-ironwood-compatibility.md` §10); and, for R39.11.24, D39.11.i
  and D39.11.f, the public coins repository's `light_wallet_d/ARRR` file and ARRR
  `coins` entry (master at its head dated 2026-03-05, fetched 2026-03-07, and at
  its head dated 2026-10-26, read 2026-10-28), together with public-network probes
  of the published ARRR endpoints on 2026-10-28.
- *Permitted-input classes used:* baseline/as-built source (the shipped
  ZCoin type, activation flow, and history method, for the T-DOC parts);
  external public specification (the published KDF API documentation, the
  Zcash Sapling protocol, the lightwalletd gRPC contract, the ZIP-225/ZIP-230/
  ZIP-244 transaction-format specifications); published dependency-crate sources
  (the selected Zcash crate release); published third-party chain sources (the
  Pirate node and Pirate `lightwalletd`, read for dictated chain behaviour only);
  public-network observation; independent work (the maintainer's design
  decision); Interop /
  wire-and-API-bound reuse (R29/R31) for the dictated method strings, JSON
  field names, and error discriminants this chapter distills.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* (addition of 2026-10-25, R39.5.2, cleared by the
  Dirty Gate with [Chapter 60](60-trading-floors-and-refusal-contract.md)
  on 2026-10-25, attestation in its §60.12; the shielded spend
  construction was checked by token inspection only:
  also consulted, at `v2.6.0-beta` and `v3.0.0-beta`, only to establish
  that a shielded coin's floors follow the UTXO rule and that a shielded
  HTLC spend deducts a fixed 1,000 base units; no discretionary
  expression reproduced.) Also consulted on 2026-10-26, at `v2.6.0-beta` and
  `v3.0.0-beta`, for §39.8.5.1 (R39.8.9f--R39.8.9h, T39.8.9a--T39.8.9d,
  D39.8.0h and the notes under them -- the absence of unmined rows, the
  same-block tie-break and the amounts of an own send), as that
  subsection's derivation record states. The KDF Dirty Gate passed
  §39.8.5.1 on 2026-10-26: verdict PASS, discretionary-expression measure
  0.02 (FAIL threshold 0.10), no leaks to scrub, and the chapter 02 §2.15A
  reference-state check passed. Scope attestation, in corpus-free terms:
  *extent* -- the reference's shielded history query and both of its
  storage backends, the shielded send and broadcast path, and the paging
  of the shielded history method, at both states, as part of a
  transaction-history pass of about 18 source files shared with chapter 38
  §38.5A, plus a mechanical diff between the two states; *method* -- the
  candidate hunks and the corresponding reference regions read in full, a
  targeted search of the reference for every identifier, number, phrase
  and ordering claim of the candidate, and the inter-state diff; the
  mechanical gate tool was not run because the candidate has no code
  blocks; *measure* -- 0.02. Otherwise consulted, under the chapter-01 two-team clean-room
  workflow, for §39.8.0.4, §39.8.0.7, §39.10, §39.11 and R39.8.0av (with the
  parity-position note under R39.8.0an and the sharpened start-point
  divergence note in §39.10.6 that follow from it) -- and, for §39.10,
  the matching wording updates of §39.0, R39.2.1, R39.3.1 and R39.3.2 --
  each as that subsection's own derivation record states; §39.10 and
  R39.8.0av at the GLEEC KDF release designations `v3.0.0-beta` (reference)
  and `v2.6.0-beta` (legacy cross-check). Only externally observable behavior
  and dictated interfaces were distilled; no implementation expression was
  carried across. Each consulted subsection is subject to KDF Dirty Gate
  clearance, as its derivation record states; §39.10, §39.11, and R39.8.0av
  together with the material that follows from it have passed it -- the last
  of those as the clearance item below records, which also records the one
  element of that pass's attestation that is a gap. Every other
  section of this chapter rests on the permitted inputs listed above with no
  corpus read. R39.11.7a(j) and T39.11.k, and the logging sentence added to
  R39.11.7a(i), are original to this design (like R39.11.7a(h)'s branch admission
  they have no counterpart in the reference): they derive from no forbidden-class
  input, only from the lightwalletd gRPC contract, public-network observation of
  the endpoints (2026-10-23) and this repository's code. Also consulted, under
  the reference versions `v2.6.0-beta` and `v3.0.0-beta` (identical for that
  subsection), for §39.11.5: for the reference's swap-path transparent-query
  contract, its lightwalletd method surface and its endpoint-list semantics only;
  the Light-mode failure policy of R39.11.20--R39.11.22's lightwalletd clauses and
  D39.11.h are independent work.
- *Dirty Gate clearance (chapter 01 T1, R40, R43) for R39.8.0av:* the
  forbidden-class-sourced material this item covers is exactly what the
  derivation record above R39.8.0av names -- that rule, the two informative
  notes under it, and T39.8.0av -- together with what follows from it
  elsewhere in the chapter: the parity-position note under R39.8.0an, the
  sharpened start-point divergence note in §39.10.6, and the cross-references
  to R39.8.0av in §39.6.4's parameter-sourcing entry, R39.8.0g, R39.8.0al and
  the §39.11 status list. All of it was authored by the KDF Spec Reader under
  the two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty Gate**
  on 2026-10-24: PASS, discretionary-expression measure 0.02, finding list
  empty, and accordingly no remedy class for any line or for the candidate as a
  whole. That was the first gate pass on this material; no earlier verdict for
  it exists or was relayed. The clearance is therefore **complete and not
  pending**, and R40 withholds nothing in it. This item is written on the clean
  side from the verdict, which reached the clean side as relayed text rather
  than as a gate-written file; that is record-keeping and adds no chapter
  content.

  **Only the named material was gated.** The rest of this chapter was not
  re-examined in this pass, so the earlier clearances of §39.10 and §39.11 are
  neither reopened nor widened by it and keep their own scopes, and the pending
  records of §39.8.0.4 and §39.8.0.7 are untouched.

  *Which parts carry an inline pointer to this item, and why those.* R39.8.0av
  carries the clearance statement at the point the material appears (R40), and
  T39.8.0av carries a one-line pointer back to it. Those two are the numbered,
  rule-bearing and test-bearing text an implementer codes from, and both sit
  among R- and T-numbers that have stood in this chapter through earlier
  revisions -- exactly the case in which long-settled requirement and
  newly-cleared requirement are easy to confuse. Nothing else carries a
  pointer, and that is a scope judgment rather than an oversight. The two
  informative notes under R39.8.0av are labelled informative in their own first
  line, bind nothing, and sit between the clearance statement and the test, so
  a reader reaching either has already passed the notice. The parity-position
  note under R39.8.0an and the sharpened divergence note in §39.10.6 are
  likewise informative and each hands back to R39.8.0av by name, where the
  notice is; a notice at the second would also sit beneath §39.10's own earlier
  clearance statement, where it could be read as widening that pass, which this
  one does not. The remaining touches -- the cross-references in §39.6.4,
  R39.8.0g, R39.8.0al and the §39.11 status list -- carry no requirement of
  their own and only name the rule that does. §39.9 is where the record lives
  and carries none.

  The three elements of the R39 scope attestation, carried forward here in
  corpus-free terms as chapter 01 T1 requires. One of the three is incomplete,
  and it is recorded as a gap rather than completed from elsewhere:

  - *Extent.* The forbidden-class material corresponding to the subject matter
    this chapter names in its own terms: the shielded-coin activation path's
    resolution of a sync start height -- the defaulted start when the caller
    supplies none, the block-time-based conversion of a start instant to a
    height that the same path shares with its non-shielded sibling builder, the
    Light-mode and Native-mode anchor acquisition, and the initialization of
    the shielded wallet store and of the compact-block fetch buffer, including
    the condition that decides whether a prior local state is taken to exist.
    Quantity examined: **four files** read in substance, together with
    token-level searches across that subsystem's file set, of the order of a
    dozen files. Reference designation: the publicly published release
    designation `v3.0.0-beta`. **The record is no more precise than that
    designation** -- no post-anchor commit identifier, no branch state and no
    location within the compared material is given (chapter 01 R11 and the
    reference-version limit of §1.7).

    **The gap in this element, left visible rather than completed.** The
    derivation record above R39.8.0av establishes its facts at two designations
    and asserts that they agree. This clearance reached material at one of them
    only: no material at the `v2.6.0-beta` designation was available to the
    pass, so **the asserted agreement between the two designations is not
    attested here**. The element is incomplete rather than approximated, and
    chapter 01 T1 is satisfied by saying so -- a completed clearance statement
    carries the three elements **or** states that an element is not recorded.
    Nothing in this clearance supports the legacy-cross-check half of the
    derivation record: that half rests on the Spec Reader's record alone, and
    this item neither confirms nor contradicts it. Whether either designation
    is a publicly published release designation of the kind R11's carve-out
    admits is a separate question, checkable on the clean side against public
    release records, and is not answered here.

  - *Method.* **Combined.** Reading, together with a mechanical token
    comparison. The candidate's extent was fixed by a line-level comparison of
    this chapter against the branch tip, so that only added and modified lines
    were judged and the surrounding chapter was excluded; every backtick-quoted
    token on an added line was then extracted mechanically and searched for on
    the reference side, and each match was put through the forced-versus-free
    test individually. No aggregate match-density judgment was used or
    admitted. **This repository's own clean-room gate tooling under `tools/`
    was not applicable and was not run:** the added material embeds no code
    block, and that tool's test is defined over source-file bodies and does not
    map onto prose, so the instrument had no operand. The absence is stated so
    that it is visible rather than implied.

  - *Measure.* **0.02** on the discretionary-expression basis -- the same
    figure the leak report carries, not a second one. Excluded from the
    measured set as dictated: every token on an added line that matches the
    reference side is a coin-configuration key, an activation request or status
    field, or a public error discriminant. **Judgment standard read against:**
    a measure at or near zero passes only if, in addition, no added line falls
    into any leak category; one item of genuine discretionary expression fails
    a candidate however much legitimately dictated content surrounds it. The
    figure stands above zero only because two of the added statements sit
    nearest the behavior-versus-expression boundary -- the arithmetic rule for
    the size of the window, and the single condition identified as the cause of
    the code-quality finding. Both were judged behavior: each is externally
    observable in the resolved start height, in the reported start field, or in
    a reported balance, and neither carries a body, an ordering transcription,
    a private name, or a message literal. With the finding list empty, no added
    line was classified into any leak category at all -- no private identifier
    outside the dictated set, no body or control-flow transcription, no error or
    log literal, no internal module tree, and no internal test identifier.

  *A note on the informative finding under R39.8.0av.* The verdict's separately
  labelled non-leak section observes, about this document set's own text, that
  the marking the finding carries -- that it is a finding against the reference,
  **not** a convergence target, and that a Coder shall not adopt the behavior it
  describes -- is load-bearing for the clean-room wall rather than editorial: a
  finding against the reference that omitted it would read as a specification to
  implement. That marking is accordingly preserved as written, and any future
  rewording of the finding must carry it forward. The parity-position note under
  R39.8.0an depends on the same finding and inherits the same marking, so it is
  re-checked whenever the finding is touched.

- *Dirty Gate clearance for §39.11.5 (2026-10-28):* PASS, measure 0.02 (threshold
  0.10), no leak items. *Extent* -- the shielded-coin subsystem (swap operations,
  HTLC send, spend and refund, the lightwalletd client, the sync loop, activation
  and the Light-mode payload schema), the shared UTXO swap helpers and the
  Electrum and native RPC-client layer, and the legacy maker and taker state
  machines (payment step, spend wait, recovery) with the swap timing constants, at
  both clean-room rows: ten files per row, the inter-row diff, and a mechanical scan
  of the whole shielded module for every gRPC call. *Method* -- reading plus that
  mechanical scan; the gate script was not run, because the candidate is prose.
  *Public data* -- the coins repository's ARRR and ZOMBIE lightwalletd lists and its
  ARRR entry. The gate's fact check contradicted one statement of the candidate,
  the median-time-past and block-headers items of R39.11.23, which were removed
  before the text entered this chapter. A clean editor applied the gate's editor
  list; its public-data item on the ARRR notarization setting was found to depend
  on the coins-file revision and is recorded so in D39.11.i rather than as written.
- *Clean-context addition (2026-10-28), §39.11.6:* R39.11.29 -- R39.11.31 with
  their provenance, compatibility and status notes, T39.11.t -- T39.11.v,
  D39.11.k -- D39.11.m, and the pointers added after R39.11.3, R39.11.4,
  R39.11.7a and R39.11.7b and in the §39.11 status list are Reloaded-original.
  They rest on the maintainer decisions of 2026-10-26 (host-only node naming
  everywhere, except the RPC wire dump) and 2026-10-28 (naming the endpoint
  that rejected a broadcast, and the suspension backoff shared with Electrum),
  on chapter 38 R38.6.4d and on this repository's own code. They take no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required. No rule text cleared by the Dirty Gate is edited: the additions
  are new rules, notes and one sentence after R39.11.7b, itself
  Reloaded-original.

- *Dirty Gate clearance for R39.5.6--R39.5.12 (2026-10-28):* PASS, measure 0.01 (threshold
  0.10), no leak items.
  - *Extent:* the shielded coin's v1 swap operations at both clean-room rows, namely HTLC key
    derivation, payment, spend, refund and DEX-fee construction, the already-sent check and
    the spend searches. Also the maker and taker refund stages and funds-recovery paths, the
    saved-swap recoverability predicate, the shielded coin's key policy and activation, the
    shared UTXO swap helpers, and the key, hash and signature primitives. 17 files at the
    first row, 10 diffed to the second, plus one file each of the pinned public Zcash library
    and the pinned secp256k1 crate.
  - *Method:* reading plus a mechanical diff between the two rows. The gate script was not
    run, because the candidate is prose.
  - *Corpus consulted:* the forbidden corpus, through the two-team procedure (chapter 01
    §1.4A), at the designations `v2.6.0-beta` and `v3.0.0-beta`, for R39.5.6--R39.5.12 and
    the code-quality finding.
  - *Other inputs:* the public Zcash library fork (tag k-1.4.2), for the expiry delta and
    the builder defaults; BIP 113 and zcashd's finality rule, as public specification.
  - *Original to this design:* R39.5.13, D39.5.a and the informative pre-check under
    R39.5.11.
