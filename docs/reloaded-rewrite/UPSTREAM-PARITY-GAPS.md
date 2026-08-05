# Upstream Parity Gaps — TODO Tracker

**Status:** active backlog plus recent parity completions awaiting wallet/CI
soak testing.

This document tracks confirmed functional gaps between the reloaded tree and
the upstream Komodo DeFi Framework wire surface, as established by the changelog
parity audit (Jan 2022 → present) cross-referenced against
[`rpc-method-census.md`](./rpc-method-census.md) (the upstream wire-method
parity reference) and the reloaded dispatcher routing.

Each unresolved gap below has been confirmed at **both** the
CRD-specification level and the reloaded-code level (the audit method and
evidence are recorded inline). Items are numbered to match the parity report
delivered to the maintainer.

Resolved chapters are normally removed from this active tracker. The resolved
chapters retained below are intentionally still present because their
implementation or documentation changed in the current two-day proving window
and still needs ordinary wallet/CI soak testing before archival.

---

## #2 — Event streaming: missing `stream::` streamer implementation

**Resolved by C8.**

CRD reference: [`10-sse-streaming.md`](./10-sse-streaming.md). R6/R20/R24 and
§10.19 now bind `stream::shutdown_signal::enable` as the ninth streamer. The
remaining streaming parity gap in this tracker was the reloaded code
implementation for that method; C8 adds it on native non-Windows targets.

Reloaded implementation evidence:
- The public streamer-origin set includes `ShutdownSignal`.
- The `stream::` dispatcher routes `stream::shutdown_signal::enable` on native
  non-Windows targets and leaves it unavailable through the normal
  missing-method path elsewhere.
- A native non-Windows shutdown-notification producer publishes supported
  process signal names to the streaming manager before runtime stop begins.

Upstream `stream::` surface (census) vs reloaded:

| upstream method | reloaded | gap |
| --- | --- | --- |
| `stream::balance::enable` | yes | — |
| `stream::heartbeat::enable` | yes | — |
| `stream::order_status::enable` | yes | — |
| `stream::orderbook::enable` | yes | — |
| `stream::swap_status::enable` | yes | — |
| `stream::network::enable` | yes | implemented (D2 completion) |
| `stream::fee_estimator::enable` | yes | implemented (#3 completion) |
| `stream::tx_history::enable` | yes | C7 — `TX_HISTORY:<ticker>` reactive streamer |
| `stream::shutdown_signal::enable` | yes | C8 — native non-Windows `SHUTDOWN_SIGNAL` reactive streamer |
| `stream::disable` | yes | C6 — generic per-client unsubscribe |

- [x] **`stream::network::enable`** — implemented (D2 completion). The `Network`
  streamer, its activation request/payload, and the `stream::network::enable`
  route are bound by [`10-sse-streaming.md`](./10-sse-streaming.md) §10.16
  (R28–R31) and merged on `dev`.
- [x] `stream::tx_history::enable` — implemented by C7. The `TxHistory`
  streamer, activation request/response, supported-family contract, and producer
  obligations are bound by [`10-sse-streaming.md`](./10-sse-streaming.md) §10.18
  (R39-R45).
- [x] `stream::shutdown_signal::enable` — implemented by C8. The native
  non-Windows `ShutdownSignal` streamer, activation route, and
  shutdown-notification producer are bound by
  [`10-sse-streaming.md`](./10-sse-streaming.md) §10.19 (R46-R51).
- [x] `stream::disable` — implemented by C6 as the generic per-client
  unsubscribe RPC bound by [`10-sse-streaming.md`](./10-sse-streaming.md) R21.

## #3 — Fee estimator stream (`stream::fee_estimator::enable`) — **RESOLVED**

**Confirmed real.** The EIP-1559 fee estimator is exposed upstream as a streamer
(`stream::fee_estimator::enable`), not a legacy method. The on-demand
single-shot estimator `get_eth_estimated_fee_per_gas` **is** present in reloaded
(arms=1), and the gas-policy pair `get_swap_gas_fee_policy` /
`set_swap_gas_fee_policy` are present (arms=2 each). Only the **continuous
streaming** variant is missing.

CRD reference: [`08-fee-routing-engine.md`](./08-fee-routing-engine.md) covers
the swap fee-routing engine but does not bind a fee-estimator streamer;
[`10-sse-streaming.md`](./10-sse-streaming.md) now binds it as the seventh
concrete streamer (§10.17, R32–R37).

- [x] **`stream::fee_estimator::enable`** — implemented: a `FeeEstimation`
  `StreamerId` variant + activation module that periodically broadcasts EIP-1559
  fee estimates for a given EVM coin. Bound by
  [`10-sse-streaming.md`](./10-sse-streaming.md) §10.17 (R32–R37) and merged on
  `dev`.

## #1 — NFT activation (`enable_nft`) — **RESOLVED**

**Resolved.** Upstream activates the NFT subsystem via `enable_nft`; reloaded now
exposes that method directly.

CRD reference: [`19-nft-module-layout.md`](./19-nft-module-layout.md) binds the
NFT subsystem activation entry point in §19.6.2 and records the transition from
the earlier `update_nft` initialization role to the explicit `enable_nft`
activation contract.

Code evidence:
- `mm2src/mm2_main/src/rpc/dispatcher/dispatcher.rs` routes `enable_nft`.
- `mm2src/coins/nft/activation.rs` implements the mmrpc-2.0 `enable_nft`
  handler, request, response, and activation error surface.

- [x] **`enable_nft`** — implemented and bound by
  [`19-nft-module-layout.md`](./19-nft-module-layout.md) §19.6.2.

## #4 — Experimental liquidity routing (DOCUMENTED ONLY — NOT to be implemented)

**Confirmed real. Out of scope by maintainer decision.** Upstream exposes an
experimental multi-quote liquidity-routing namespace; reloaded has 0 routes for
it.

Upstream methods (census, all `experimental::liquidity_routing::`):
- `experimental::liquidity_routing::execute_routed_trade`
- `experimental::liquidity_routing::find_best_quote`
- `experimental::liquidity_routing::get_quotes_for_tokens`

No CRD chapter binds this namespace. Per maintainer instruction, this experimental
feature is **deliberately not implemented**; it is recorded here for completeness
only.

- [x] Documented as a known, intentional omission. No implementation planned.

## #5 — Streaming activation response carries an extra `active` field — **RESOLVED**

**Resolved. Reloaded wire contract now matches upstream.**

Upstream's `stream::*::enable` success response is the mmrpc-2.0 envelope whose
`result` is an object with a single string field `streamer_id` (the wire-stable
`StreamerId` display token, e.g. `HEARTBEAT`, `BALANCE:<ticker>`) — with **no
boolean field**. CRD reference: [`10-sse-streaming.md`](./10-sse-streaming.md)
R21 (corrected this round to bind the `streamer_id` response and drop the
previously-specified boolean `active`).

Code evidence:
- `mm2src/mm2_main/src/rpc/streaming_activations/mod.rs` — `EnableStreamingResponse`
  now serialises **only** `streamer_id: String`; the reloaded-only `active: bool`
  field was removed and the constructor takes just the `streamer_id`.
- No reloaded code reads `active`; the Komodo DeFi SDK's `BalanceManager` reads
  `streamer_id` (a captured run log shows `Key "streamer_id" not found in Map`
  failures from an older build that returned only `active`, confirming the SDK
  requires `streamer_id` and does not depend on `active`).

Three-way alignment: corpus `{streamer_id}` = reloaded code `{streamer_id}` = CRD
R21. The superset has been removed; all three shapes now agree.

- [x] **Drop `active`** from `EnableStreamingResponse` (and its constructor) so
  the success payload is exactly `{ "streamer_id": "<token>" }`, matching upstream
  and the corrected R21. Done on `dev` (commit `fd075e66e`); the shared struct and
  all activation handlers compile and the streaming_activations tests pass.

## Runtime-stub backlog — unfinished / TODO

The following items were found during the runtime-stub and wallet-log hardening
pass. They are tracked here so they are not lost, but they are not marked as
resolved until their owning CRD chapter and implementation are both updated.

### UTXO Standard Swap V2 HD/Trezor stubs — **PARTIALLY RESOLVED**

CRD reference: [`15-swap-v2-utxo-path.md`](./15-swap-v2-utxo-path.md).

The clean-room chapter-15 pass confirmed that software-HD and Trezor-backed UTXO
Standard Swap V2 address and HTLC public-key identity must use the enabled HD
address record. That record now carries the display address, compressed public
key, and full derivation path. Wallet-funded maker-payment and taker-funding
transactions use the enabled address path for Trezor P2PKH input signing, with
P2SH HTLC and OP_RETURN outputs marked external and change marked by path when
the signer can represent it.

- [x] Promote/confirm the chapter-15 HD/Trezor requirements with a clean-room
  Spec Reader pass and Dirty Gate pass.
- [x] Replace the HD local-address runtime stub with enabled-HD-address
  behavior.
- [x] Replace the Trezor address/public-key deferral with enabled hardware-HD
  address metadata selection.
- [x] Add structured Trezor wallet-funded signing failures for unsupported coin
  mapping/script mode, missing derivation metadata, user rejection/cancel,
  transport/disconnect, unexpected device, and invalid device responses.
- [x] Add unit coverage for HD trade-preimage sender derivation, HD V2 local
  address selection, missing enabled HD address errors, Trezor enabled address
  pubkey selection, and unsupported Trezor HTLC script signing.
- [ ] Future work: extend the Trezor UTXO signer to support the V2 arbitrary
  P2SH HTLC input scripts, then add emulator-backed wallet-funded and HTLC spend
  coverage. The current signer only supports standard P2PKH inputs, so V2 HTLC
  spend/finalization fails at the first local HTLC-signing step with a
  structured `hardware_wallet:unsupported_script_signing_mode` error and never
  falls back to host private keys.

### Solana / SPL swap and history surface — **TODO**

CRD reference: [`40-solana-coin.md`](./40-solana-coin.md).

The Solana/SPL modules still contain broad unimplemented areas around market
operations, swap operations, history, raw transaction handling, and fee
preimage/conversion flows.

- [ ] Decide whether Solana/SPL is in scope for the current release.
- [ ] If in scope, run a clean-room CRD pass before implementation.
- [ ] If out of scope, mark the unsupported RPC/swap paths explicitly and return
  structured errors instead of panics.

### Lightning Network market/swap/history surface — **TODO**

CRD reference: [`41-lightning-network.md`](./41-lightning-network.md).

Lightning support still contains large feature stubs in market operations, swap
operations, and transaction-history-style surfaces.

- [ ] Decide whether Lightning is in scope for the current release.
- [ ] If in scope, split implementation into activation, payment/channel, swap,
  and history work packages.
- [ ] If out of scope, bind the unsupported behavior in the CRD and make runtime
  paths return structured errors.

### Ledger APDU transport — **TODO**

CRD reference: [`50-evm-trezor-signing.md`](./50-evm-trezor-signing.md) for the
current hardware-wallet policy surface. A separate Ledger chapter may be needed
if Ledger support is brought into scope.

- [ ] Decide whether Ledger transport support is in scope.
- [ ] If in scope, define the public hardware-wallet transport contract and add
  simulator or mock-device tests.
- [ ] If out of scope, ensure any runtime entry point reports unsupported
  hardware transport instead of panicking.

### Low-S signature verification helper — **TODO**

The `kdf_keys` low-S helper is crypto-sensitive and currently not on an active
KDF call path.

- [ ] Confirm whether any enabled signing or verification path requires this
  helper.
- [ ] If required, implement against the public secp256k1 rule and add boundary
  tests.
- [ ] If not required, document it as intentionally unavailable until the owning
  feature is implemented.

### Wallet app sequencing warnings — **TODO / needs reproduction**

Wallet logs still show transient-looking conditions such as duplicate activation
requests, balance polling before activation completion, inactive stream polling,
and bad external provider endpoints.

- [ ] Re-test with a current KDF build.
- [ ] If still reproducible, classify each symptom as KDF compatibility behavior,
  app call-ordering behavior, or external-provider failure.
- [ ] Fix only the KDF-owned compatibility cases in this repository.

---

## Audit method (for reproducibility)

1. Extracted the 223 unique upstream wire-method strings from
   `rpc-method-census.md`.
2. Extracted reloaded's routed method strings from
   `mm2src/mm2_main/src/rpc/dispatcher/dispatcher.rs` and
   `dispatcher_legacy.rs`.
3. Normalised namespace prefixes on both sides and diffed.
4. Verified every candidate gap by direct source grep across `mm2src/`
   (eliminating routing-style false positives such as generic `::cancel` arms
   and task-lifecycle leaves).
