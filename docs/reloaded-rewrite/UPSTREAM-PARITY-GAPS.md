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

## #6 — KMD swaps (issue #11): reward rule, fee split, no-fee waiver, V2 layout — **OPEN**

CRD references:
- [`38-utxo-coin-maintenance-and-features.md`](./38-utxo-coin-maintenance-and-features.md)
  §38.4 (R38.4.1–R38.4.3);
- [`08-fee-routing-engine.md`](./08-fee-routing-engine.md) R8, R15A, R15B,
  R15C, T5A, T5B, T7;
- [`16-swap-v2-pre-burn-output.md`](./16-swap-v2-pre-burn-output.md) R7,
  R12A, R12B, R13–R20, T4A–T9, D5;
- [`15-swap-v2-utxo-path.md`](./15-swap-v2-utxo-path.md) R16, R17, R21,
  R22, R26–R28, R40;
- [`52-swap-v2-state-machine.md`](./52-swap-v2-state-machine.md) R17, R27,
  R76, R77;
- [`06-network-id-seed-node.md`](./06-network-id-seed-node.md), accessor
  table.

The chapters were corrected on 2026-09-27. The code has not been changed yet.
Each chapter carries an "Implementation obligations" note listing the files
and tests to change.

- [ ] **KMD reward (R38.4.1).** Apply the KIP-0001 ÷500 reduction for UTXOs
  confirmed at or after height 3,484,958. Without it, any KMD spend of an
  eligible post-fork UTXO over-claims and is rejected with
  `bad-txns-in-belowout`. This covers taker fees, swap payments and
  withdraws on both netids.
- [ ] **KMD direct-burn split (ch.08 R8).** Implement the three ranges:
  `Standard(dust)` at or below dust; a fee leg clamped to dust when 75% of
  the total is below dust; 75/25 above that. Withdraw the builder's
  under-dust exemption (ch.08 R15A). Without this fix, reloaded and
  `v2.6.0-beta` peers on netid 8762 reject each other's KMD taker fees for
  trades up to about 0.0115 KMD.
- [ ] **No-fee waiver (ch.08 R15C, ch.16 R7; maintainer decision
  2026-09-27: adopt).** A non-privacy taker whose key equals the network
  burn key pays no fee, on both protocols. The keys are `0369aa…3153` on
  8762 and `03a778…bf5c` on 6133. Netid 8762 needs its burn key configured.
- [ ] **Version-two KMD-pair exemption (ch.16 R12B).** On netid 8762, a
  version-two swap with KMD on either side carries no dex fee. There is no
  such exemption on 6133, and none on the legacy protocol. This needs a new
  network-config ticker set (ch.06).
- [ ] **Version-two UTXO taker-payment-spend contract (ch.16 R13–R20,
  ch.15 R26–R28).** The project's current implementation differs on every
  point, so version-two UTXO swaps with reference nodes fail on both
  netids for any ticker. The required contract:
  - the fee-first `Standard` preimage, signed `SINGLE` by the taker, with
    the maker appending its payout and signing `ALL`;
  - fee, then burn, then maker order for `WithBurn`;
  - a bare `OP_RETURN` carrying the burn value;
  - exact preimage equality in validation;
  - the funding amount including the burn (ch.15 R16);
  - a 496-byte, proportional spend-fee estimate (ch.16 R14A).
- [ ] **Deferred, non-blocking: funding-spend preimage tolerance (ch.16
  D5, ch.15 R22).** The references check the fee ratio within [0.9, 1.1]
  and then require exact equality. The project uses a value-relative 10%
  tolerance, which only makes its own side more permissive.
- [ ] **Recorded only, maintainer decision 2026-09-27: dormant
  burn-account split.** When the fee share is below dust, the references
  clamp the fee leg to dust and burn the rest, as long as the rest is not
  dust. The project's ch.16 R9 falls back to `Standard` instead. The path
  is inactive on both production netids and is left unchanged.

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

### Legacy `enable` for EVM coins under an HD wallet — **TODO**

CRD reference: [`35-evm-v2-activation-rpcs.md`](./35-evm-v2-activation-rpcs.md),
[`55-coin-activation-wire-contract.md`](./55-coin-activation-wire-contract.md) §55.4.

Since `fix/activation-api-parity`, the mmrpc 2.0 EVM activation under an HD
wallet uses the address at the coin's `derivation_path` plus `path_to_address`.
Legacy `enable` for EVM coins under an HD wallet still binds the daemon's
internal-path key, so the two activation paths report different addresses for
the same seed phrase. Left unchanged by maintainer decision (2026-10-11).

- [ ] Establish which key the reference binds on legacy EVM `enable` under an HD
  wallet (Spec Reader question) and bind it in ch. 55 §55.4.
- [ ] Align the legacy path, with an upgrade note like the one for the mmrpc 2.0
  path in `CHANGELOG.md`.

### Solana / SPL swap and history surface — **TODO**

CRD reference: [`40-solana-coin.md`](./40-solana-coin.md).

The Solana/SPL modules still contain broad unimplemented areas around market
operations, swap operations, history, raw transaction handling, and fee
preimage/conversion flows.

- [x] Decide whether Solana/SPL is in scope for the current release. Decided
  2026-10-26 (maintainer): switched off for this beta and planned for the next
  one. While `SOLANA_ACTIVATION_AVAILABLE` (`coins/lp_coins_ops.rs`) is
  `false`, every Solana activation route (`enable_solana_with_tokens`,
  `enable_spl`, legacy `enable` / `electrum`) refuses with "Solana support is
  not available in this build" and registers nothing (ch. 40 availability
  note, D40.4). The items below apply to the next beta.
- [ ] **Activation compatibility surface (decided 2026-10-11, deferred).**
  Reloaded's `enable_solana_with_tokens` / `enable_spl` stay the primary
  activation surface. The reference's `experimental::enable_solana_with_assets`
  / `experimental::enable_solana_token` methods and its `SOLANA` /
  `SOLANATOKEN` coin-configuration forms (ch. 40 §40.1-§40.3) are not routed or
  parsed yet, Solana is native-only, and the primary surface's wire shapes are
  not yet bound in the CRD. Work package: `docs/plans/solana-support.md`.
- [ ] For the next beta, run a clean-room CRD pass before implementation.
- [ ] Before Solana is switched back on, mark the unsupported RPC/swap paths
  explicitly and return structured errors instead of panics.

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

## #7 — EVM fee inputs: per-coin overrides never read, and an absolute gas-price floor — **OPEN**

CRD reference: [`57-evm-fee-inputs.md`](./57-evm-fee-inputs.md) (new chapter).
Reference designations: `v2.6.0-beta` and `v3.0.0-beta`, which agree.

Four independent gaps on the same surface. They compound, so a sub-gigawei
chain's fee quote needs at least the first two closed before it matches.

- [x] **The one-gigawei increment floor (ch. 57 R57.3.2).** Fixed in PR #59.
  Reloaded's
  proportional gas-price increase substituted a fixed one-gigawei increment
  whenever the proportional one was smaller. The reference removed that floor;
  no release designation at or after `v2.6.0-beta` carries it. It is a
  pre-anchor baseline defect (ch. 57 V1), so removing it is a parity **fix**
  with no compatibility switch. Effect: inactive at and above ≈7.70 gwei,
  roughly 1.8× at 1 gwei, ≈89× at 0.01 gwei, ≈443× at 0.002 gwei, and applied
  twice on the trade-preimage path. The floor is what the signed price carries
  on any coin that resolves to the legacy fee form — notably the
  Binance-Smart-Chain platform coin, whose configuration entry caps the
  envelope type at `0` — so this is an overpayment and not only a display
  error.
- [ ] **Swap figures ignore the swap gas-fee policy (ch. 57 R57.4.1).** All
  four swap-side fee figures are computed from the legacy gas price
  unconditionally. The reference computes them from the policy-resolved fee
  form, and the public coins configuration selects the medium EIP-1559 tier
  for the Arbitrum, Base, Ethereum, Avalanche, Polygon, Gnosis and several
  other platform coins. This is the larger half of the reported fee error on
  those chains.
- [x] **`gas_limit` and `gas_limit_v2` coin-configuration blocks are never
  read (ch. 57 R57.5.1–R57.5.4, R57.6.1–R57.6.4) -- implemented in PR #105 (2026-10-28).** This is the gap the
  tracker previously did not record at all. Reloaded hard-codes 21000 for a
  native send, 210000 for a token send, 150000 for every V1 HTLC send, spend
  and refund, 300000 for the platform-coin sender preimage, and builds its V2
  gas-limit set from its own defaults. The deployed coins configuration sets
  per-coin overrides on dozens of entries (the Arbitrum platform coin's
  native-send limit, the Moonriver entry's payment, receiver-spend and
  sender-refund limits, and the payment/spend/refund sets on the
  Binance-Smart-Chain, Polygon and Avalanche token entries), all silently
  ignored. Closing it also adopts the reference's differing defaults, which
  ch. 57 R57.5.4 requires be done in the same change. The two refund keys are
  called out separately in ch. 57 R57.5.2 because a failed refund is the worst
  failure mode in the system.
- [ ] **`gas_price_adjust` is never read (ch. 57 R57.2.4).** The reference's
  only chain-relative gas-price mechanism. No deployed coin sets it today, so
  adopting it changes nothing immediately; it is the sanctioned remedy for a
  chain the shared defaults misprice, and the alternative a Coder would
  otherwise be tempted to hard-code.

Two smaller items on the same surface are recorded as deferred rather than
tracked here: the three additional fee-approximation stages (ch. 57 D1) and
the removal of reloaded's inert third (external gas-oracle) price source
(ch. 57 D4). The per-coin gas-estimate multiplier the configuration sets to
1.3 for two layer-two platform coins, formerly ch. 57 D3, is bound since
2026-10-27 as ch. 57 R57.5.3b (it scales only the legacy ERC-20 approve's
estimated limit, never a configured or default limit) and is part of the
`gas_limit` item above: reloaded reads neither.

---

## #8 — EVM withdraw: no EIP-1559 fee form, no envelope-type derivation — **OPEN**

CRD reference: [`57-evm-fee-inputs.md`](./57-evm-fee-inputs.md) §57.7, and
[`19-nft-module-layout.md`](./19-nft-module-layout.md) §19.7 / D8 for the
non-fungible flavours.

- [ ] Reloaded's withdraw fee enumeration accepts only the legacy `EthGas`
  form; the reference also accepts an `EthGasEip1559` form and derives the
  signed envelope type from whichever was resolved, checking it against the
  per-coin envelope-type cap before building. Reloaded always builds the
  untyped envelope and performs no cap check on this path. A caller sending
  the EIP-1559 fee form today receives an expected-`EthGas` error. Applies to
  the platform-coin, token and both non-fungible withdrawals; ch. 57 R57.7.5
  requires them to land together.
- [ ] Note the hazard in ch. 57 R57.7.1a before implementing: the nested
  explicit-gas-limit form has **no** valid JSON encoding in the reference, so
  only the estimating form is reachable from an RPC caller and inventing an
  encoding for the other is a deliberate divergence (ch. 57 D5).
- [ ] The enumeration also lacks the Cosmos-family gas form the reference
  accepts, so the Tendermint withdraw path cannot be driven by an explicit
  fee (ch. 57 §57.7 finding).

---

## #9 — UTXO: `signature_version` value `fork_id_rxd` unsupported (Radiant) — **OPEN (implemented in PR #65, pending merge)**

CRD reference:
[`58-utxo-signature-version-and-radiant-sighash.md`](./58-utxo-signature-version-and-radiant-sighash.md).
Reference designation: `v3.0.0-beta` (the dialect is not in `v2.6.0-beta`).

*Status (2026-10-25).* The first two items below are **implemented in
PR #65** (`feat/rxd-signature-version`, stacked on PR #54; merge order
#57 → #54 → #65) from the cleared chapter and public Radiant-Core alone;
ch. 58 §58.5 carries the as-built statement. The checkboxes stay open
until the PR is merged. The third item — trading fitness (ch. 58 D2) — is
**separately still open** and is not touched by PR #65: Radiant is not to
be called tradable on the strength of the signing work.

- [ ] The shipped coins configuration gives the Radiant entry
  `"signature_version": "fork_id_rxd"` with `"fork_id": "0x40"`. Reloaded's
  value set has three entries and not that one, so the value fails to
  deserialize and the coin silently falls back to the Bitcoin-Cash BIP-143
  preimage — producing signatures Radiant rejects on every transaction.
  Parity **gap**; no compatibility switch. Scope is exactly three things
  (ch. 58 R58.5.2): the fourth accepted value, the dialect itself, and the
  strict-encoding mask. **No** new transaction-hash algorithm is needed
  (ch. 58 R58.2.4) and no `chain_variant` work is needed
  (ch. 58 R58.2.6).
- [ ] Implement the algorithm **from Radiant-Core**, which is public and
  permitted: ch. 58 R58.4.1 establishes on public-network evidence that the
  reference's dialect matches Radiant's own, so the public source is both the
  correct and the authoritative route (ch. 58 R58.4.3). Add the mainnet
  signature vector of ch. 58 T3 as the parity guard.
- [ ] Separately: enabling Radiant for **trading** is not covered by the above
  and is unestablished (ch. 58 D2).

---

## #10 — Order matching: no penalty on a failed negotiation — **OPEN**

CRD reference:
[`59-legacy-ordermatch-negotiation.md`](./59-legacy-ordermatch-negotiation.md).
Reference designation: `v2.6.0-beta`.

- [ ] Reloaded penalises a counterparty's public key for an hour on two
  taker-swap outcomes; the reference also penalises on a **failed
  negotiation** (ch. 59 R59.5.2, R59.5.4). This single missing trigger is the
  whole delta for the field-reported "a dead maker keeps absorbing my trade
  attempts" problem: with it, the maker that just failed is out of the next
  attempt's candidate set for an hour. One-line widening plus the regression
  test of ch. 59 T4.
- [ ] Expectations to set, because the fix is narrower than the symptom: the
  taker still does **not** retry a different maker inside one order (ch. 59
  R59.3.4 — that is parity, and a retry would be a divergence), and dead rows
  do **not** leave the orderbook faster (ch. 59 §59.6 owns that).
- [ ] Related, same chapter, separate items: the own-pubkey exemption from
  liveness eviction covers only the persistent key and not the per-order keys,
  so a node can evict its own live orders from its own read index (ch. 59
  R59.6.5); and the coin protocol-support check cannot carry the amount, lock
  duration or maker-side flag the reference's carries, so neither side can
  refuse a match the reference would refuse (ch. 59 D1).

---

## #11 — Watcher payload carries transaction bodies where hashes are required — **OPEN**

CRD reference:
[`09-watcher-reward-infrastructure.md`](./09-watcher-reward-infrastructure.md)
R6a / R10a.

- [ ] Reloaded's taker publishes the **serialized transactions** in the
  watcher payload's taker-fee and taker-payment hash fields. A watcher
  length-checks the fee field for 32 bytes before spawning, so no watcher
  assists any reloaded taker's swap, for any coin, in either direction. The
  defect is invisible in operation because a declining watcher is
  indistinguishable from an absent one. Parity **fix**; the hash is already
  computed on both paths. Add a test asserting the field length is 32 — a
  round-trip through the project's own code cannot see the difference.
- [ ] A no-fee (network burn/waiver key) swap publishes an **empty** fee field
  and gets no watcher. That is **parity**, not a gap: the reference reaches
  the same outcome by the same route (ch. 09 R10a). Changing it is a
  divergence needing a compatibility switch, and it is deferred (ch. 09 D5).
  Nothing about it should be attempted before the item above is fixed.
- [ ] *Watcher side (2026-10-30, Decision BA):* the UTXO-standard, BCH and QTUM
  families opted in to watching without any watcher method, so a Reloaded node
  started watchers for them that could only stop. They no longer opt in
  (ch. 09 R17); restoring them needs their watcher methods (ch. 09 D3). Whether a
  reference node watches swaps of these families is open (Spec Reader queue).

---

## #12 — EVM/QRC20 minimum trading volume: baseline formula, and the maker price floor — **OPEN (chapter cleared 2026-10-24; first item implemented in PR #66, pending merge)**

CRD reference: [`57-evm-fee-inputs.md`](./57-evm-fee-inputs.md) §57.7A
(Part D), R57.7A.1–R57.7A.6, T10–T12, D7–D8, V4. Reference designations:
`v2.0.0-beta` (first), `v2.6.0-beta` and `v3.0.0-beta`, which agree. The
section **passed the KDF Dirty Gate** on 2026-10-24 (PASS,
discretionary-expression measure 0.01, finding list empty), recorded in
ch. 57 §57.12's second scoped-addition item; both items below may be
implemented from the cleared chapter.

*Status (2026-10-25).* The first item is **implemented in PR #66**
(`fix/evm-min-trading-volume`, to be merged after PR #57): a shared
`one_smallest_unit(decimals)` helper feeds both the EVM and the QRC20
`min_trading_vol`; `min_tx_amount` stays zero; no switch. Tests follow
T10–T12, with one recorded coverage gap (the end-to-end preview of the
field report's volume on a real eight-decimal coin is not driven — see
`docs/plans/evm-min-trading-volume.md`). The checkbox stays open until the
PR is merged. The second item (D7, the maker price floor) was deliberately
**left out** of PR #66 by maintainer decision; it is one comparison in the
price validator and fully separable, and remains open.

- [ ] **EVM and QRC20 minimum trading volume (ch. 57 R57.7A.1, R57.7A.6).**
  Reloaded derives it as ten to the power of minus `decimals / 3` with integer
  division — a pre-anchor baseline formula (ch. 57 V4) — so every
  eight-decimal ERC-20 token has a 0.01 floor (36 entries in the deployed
  coins file, the four WBTC variants being the serious cases). The reference
  reports one smallest unit of the coin (10^-decimals) at every designation
  since `v2.0.0-beta`. The floor is applied to the node's own maker and
  taker orders and trade previews and is advertised as the default
  `min_volume` of its maker orders; it is never applied to a peer's order, so
  the defect is one-sided against reloaded's users and closing it is a parity
  **fix** with no compatibility switch (ch. 57 R57.7A.5). *(2026-10-25,
  cleared by the Dirty Gate with ch. 60: the maker also leaves unanswered any request below that
  advertised minimum. The cross-family form of the rule is in ch. 60
  R60.5.2 and #14. This does not change the conclusion.)* The EVM minimum
  *transaction* amount is zero in both trees and stays zero (R57.7A.3).
- [ ] **Maker price floor (ch. 57 D7, finding under R57.7A.6).** Reloaded
  refuses a maker price below 10^-8; the reference accepts any positive
  price since the same designation. Coin-agnostic, one-line, no switch; no
  order-match chapter binds order validation yet, so ch. 57 D7 is the record.

---

## #13 — UTXO header families: `RIC` and `MEWC` swaps fail at the HTLC spend where the reference's complete — **OPEN (chapter cleared 2026-10-25; resolution bound 2026-10-25 in two legs; the key landed in PR #62 for the median-time-past path; the registry-free structural fallback R37.5.9e, the key rename and the header-sync/store threading pending on the PR #62 follow-up)**

CRD reference:
[`37-utxo-spv-and-block-header-validation.md`](./37-utxo-spv-and-block-header-validation.md)
§37.5.1 (the `"BTC"`-value re-examination), §37.6 (*Where the median
goes*), §37.9 D3 (revised). Reference designations: `v2.6.0-beta` and
`v3.0.0-beta`, which agree. The revised material **passed the KDF Dirty
Gate** on 2026-10-25 (PASS, discretionary-expression measure 0.02,
finding list empty), recorded in ch. 37 §37.10's `"BTC"`-family
re-examination item; this entry was gated with it as a dependent
restatement, and both items below may be implemented from the cleared
chapter. The gap the first item names is in addition observable from
this repository's own code and from public-network measurement alone.

- [ ] **Three registry coins whose headers no header family of the set lays
  out (ch. 37 §37.9 D3).** `RIC` (112-byte Riecoin headers) and `MEWC`
  (interleaved 80-byte merged-mined and 120-byte native headers) are
  configured `"chain_variant": "BTC"` in the public registry; `XPM`
  (88–91-byte Primecoin headers) carries no key. Reloaded's header reader
  requires exact byte consumption, so on an Electrum connection every
  `RIC` median-time-past list and every `MEWC` list holding a native block
  is rejected and the **HTLC spend fails** — the spend queries the median
  before it signs. The reference reads the same lists as consecutive
  80-byte records to a *random* median, which its spend tolerates (the
  lock-time is consensus-irrelevant under a final sequence) and only its
  refund mis-times. Net: the reference completes `RIC`/`MEWC` swaps and
  delays their refunds; Reloaded fails the swaps. `XPM` is wallet-only
  and has no swap path. The registry maintainer confirmed on 2026-10-25
  that `"BTC"` on both entries is deliberate and will stay, so closing
  this needs Reloaded-only families **and** a selector that takes
  precedence over `chain_variant` for `"BTC"`-pinned entries; the four
  options and the one the evidence favours are in ch. 37 §37.9 D3. Parity
  **gap**; the family work needs no compatibility switch, while the
  stop-gap option 4 (spend without a median) would. *(The structural
  fallback later bound as the registry-free leg, below, is switch-governed
  — a deliberate divergence with its `[]` opt-back — so the sentence before
  this one holds for the key leg only.)*

  **Resolution path (2026-10-25, maintainer decisions; clean-room-original,
  no gate).** Bound in ch. 37 §37.5.9 (R37.5.9–R37.5.9e) in **two legs**.
  *Leg 1, the key as local override:* a Reloaded-only coin-configuration
  key, `protocol.header_layout` (renamed 2026-10-25 from the proposed
  `reloaded_header_layout`: the key is **not expected in the shared public
  registry** — its maintainer will not add a Reloaded-specific key to a
  shared file — so it is a local-override feature for an operator's own
  coins file and its name is unbranded), which the reference ignores
  (ch. 45 R45.3.7) and which takes precedence over `chain_variant` for the
  coin it is set on — D3 option 2 with option 3's per-header structural
  recognition as its mechanism; option 4 rejected. Values: `"riecoin"`
  (112 bytes, fixed) for `RIC`; `"kawpow_auxpow_mixed"` (per header: AuxPoW
  flag clear → 120-byte KawPoW, set → std-80) for `MEWC`. *Leg 2, the
  registry-free path (R37.5.9e):* a **structural fallback** under the
  standard family — for a coin without the key whose family is in the
  node-level `MM2.json` array `header_layout_fallback_families` (default
  `["BTC"]`, which until ch. 37 §37.9 D1 lands is every coin), a header
  list or lone header the standard read does not consume exactly is re-read
  under the Riecoin layout, then the mixed layout, and accepted only on
  exact consumption; ambiguous lists are rejected; one `info` line per coin
  per activation names the layout chosen so the operator can pin the coin
  with the key; a malformed switch value refuses the launch. This is what
  makes the registry's `RIC` and `MEWC` entries swap **with no change to
  the shared registry**. Both entries keep their registry `"BTC"`, so the
  shared coins file serves both implementations. Median-time-past is
  computed from each parsed header's timestamp; stored bytes and identity
  are the full served bytes (R37.5.9c). Compatibility: the key needs no
  switch (R37.5.9d: nothing a counterparty observes changes); the fallback
  is a deliberate divergence from the reference (which has no fallback and
  misparses to a random median silently), Reloaded side by default under
  `docs/COMPAT_SWITCHES.md`'s default-side rule, arm 2, with the
  reference's selection reachable by `[]`, and takes a
  `docs/GLEEC_COMPATIBILITY.md` row and a `RELOADED_VS_GLEEC.md` entry on
  the code branch. Implementation is steps H4a (key) and H4b (fallback and
  switch) of `docs/plans/utxo-header-family-selection.md`, placed ahead of
  the remaining D1 family work as the **beta blocker**. **Implementation
  landed in PR #62 (median-time-past path, key leg):** the key is read into
  the coin configuration (under its earlier name), the Riecoin and mixed
  readers are in the tree, and the median-time-past the HTLC spend and
  refund query is computed from the key's layout, so the spend no longer
  fails for an entry carrying the key. **Pending on the PR #62 follow-up:**
  the structural fallback and its switch (which is what fixes the two
  coins for a node running the shared registry as it stands), the rename
  to `header_layout`, and the threading of the layout to the header-sync
  (SPV), single-header timestamp and header-store reads (ch. 37 §37.5.8).
  This item **stays open until the fallback lands and header sync and the
  stores follow**, and the ch. 37 §37.8 R37.5.9 and R37.5.9e criteria pass
  on every read path. `XPM` is out of scope (wallet-only; no fallback
  step). The ch. 37 material is clean-room-original and carries its own
  provenance note and §37.10 item; nothing here is gated or awaiting a
  gate.
- [ ] **Refund-wait bound floored instead of capped (ch. 37 §37.6,
  code-quality finding).** This repository's UTXO refund-eligibility
  routine returns a wait of *at least* one hour where the reference, since
  `v2.0.0-beta`, returns *at most* one hour (the baseline anchor carries
  the floor). Refunds are delayed by up to an hour per round; no funds at
  risk, nothing signed changes. One-line fix in both branches; owner is
  ch. 51's refund timing. The floor in this repository is observable from
  the tree; the comparison to the reference's cap — and the designation
  named for it — is corpus-sourced and recorded as such in ch. 37 §37.10
  (cleared 2026-10-25).

---

## #14 — Trading floors and fixed-fee arithmetic outside the EVM family — **OPEN (chapters gated in five passes 2026-10-25, PASS)**

CRD reference:
[`60-trading-floors-and-refusal-contract.md`](./60-trading-floors-and-refusal-contract.md)
(new; §60.3 per-family table, §60.5 refusal and failure map, §60.6
requirements, findings F1–F6) and
[`38-utxo-coin-maintenance-and-features.md`](./38-utxo-coin-maintenance-and-features.md)
§38.6.6 (rewritten: R38.6.6a–i), with pointers in ch. 18 §18.5.2, ch. 20
R-S11, ch. 21 §21.11A, ch. 39 R39.5.2, ch. 40 §40.6A, ch. 41 §41.11A, and
an amendment to ch. 57 R57.7A.5. Reference designations: `v2.6.0-beta` and
`v3.0.0-beta`, which agree on every item. The material was gated by the
KDF Dirty Gate in five passes on 2026-10-25 (each PASS,
discretionary-expression measure 0.02, finding list empty; pass 5 also
found every state name tabled in ch. 02 §2.15A with matching
identifiers), recorded
with its scope attestation and stated gaps in ch. 60 §60.12, ch. 38
§38.9's fee-policy item and ch. 57 §57.12's third scoped-addition item.
The items below may be implemented from the cleared chapters, subject
to the stated gaps of those records: (a) no later unreleased v3 state is
covered; (b) the four areas named there were checked by token
inspection, with pass 3's mechanical sweep, not side by side; (c) the
`v2.0.0-beta` designation rests on ch. 57's earlier clearance. The one
sentence of ch. 38 R38.6.6e reworded after pass 3 at the gate's request
was cleared by pass 4.

*Is the floor a wire parameter?* Partly. No node refuses a swap at
negotiation for its size, in either protocol. The minimum trading volume
reaches a peer only as the maker's advertised `min_volume`, and a maker
sends nothing in reply to a request below it (ch. 60 R60.5.1–R60.5.3).
The minimum *transaction* amount is a true wire parameter. Each peer
floors the DEX fee with its own figure for the taker coin. The maker then
checks the fee against its own figure. In the legacy protocol a UTXO fee
must be at least that figure, while SLP, Tendermint and Sia fees must
match it exactly. In version two a UTXO taker's funding must match
exactly (ch. 60 R60.5.4). Reloaded matches the reference on
this value for every family that can swap.

- [x] **UTXO floor (ch. 38 R38.6.6i items 1–2)** — implemented in PR #66. Reloaded still fixes the
  exact ticker `BTC` at 0.00777 (BTC-segwit is already 0.0001, the
  reference value for both), and rounds the fee term up where the
  reference truncates. The rounding difference affects no entry of the
  public coins file today. Local policy; no switch. Finding F1 (BTC at
  0.0001 fails a floor-sized legacy spend above roughly 19 sat/vB) is
  answered by the local pre-trade check of ch. 60 R60.6.7 (maintainer
  decision 2026-10-25), which ships with this fix.
- [x] **Whole-kilobyte fixed-fee option and proportional plain fixed fees
  (ch. 38 R38.6.6a–c, R38.6.6i item 3).** Implemented in PR #72. Reloaded ignores `dingo_fee`.
  It rounds every fixed-fee coin's transaction size up to a whole
  kilobyte, so it overpays: a 226-byte KMD transaction costs 1,000 base
  units instead of 226. For the six entries that carry the option, its
  version-two spend fee is proportional where the reference charges one
  kilobyte, so a version-two swap whose taker coin is one of them fails
  the exact preimage check against a reference peer (ch. 16 R17).
- [x] **Legacy HTLC spend estimate (ch. 38 R38.6.6c, R38.6.6i item 4;
  ch. 60 finding F4).** Implemented in PR #72. Reloaded estimates over 305 bytes with whole-kB
  rounding; the reference uses 496 bytes under the R38.6.6b rule. The
  peer never validates this figure; the cost is overpayment.
- [x] **Unchecked spend arithmetic (ch. 60 R60.6.4, finding F3).** Implemented in PR #73.
  Reloaded's legacy spend and refund builders, and its version-two
  maker-payment spend and refund builder, subtract the fee without
  checking it. A fee that reaches the HTLC value then wraps and the
  daemon rejects the broadcast. The version-two funding-spend builder
  checks the fee but not the dust. Required: refuse cleanly.
- [x] **Lightning floors (ch. 41 R41.11A.2, finding F5)** — implemented in PR #66. Both accessors
  are placeholders that end the request abnormally. Required: the
  reference value, or an orderly refusal while Lightning trading is
  unsupported.
- [x] **Sia spend below the fixed miner fee (ch. 20 R-S11, finding F2).** Implemented in PR #73, with the pre-trade check of ch. 60 R60.6.7.
  In both trees and in reloaded, the Sia floors are one hasting while every
  HTLC spend or refund deducts 0.00002 SC. Required in reloaded: an orderly
  refusal. The Sia floor stays at parity; the pre-trade check of ch. 60
  R60.6.7 keeps such a payment from being created (maintainer decision
  2026-10-25, ch. 60 D60.2).
- Informative, no action: TRON shares the EVM accessor, so PR #66 (#12)
  fixes its value too (ch. 21 §21.11A). Solana has no parity target,
  because neither tree can trade it (ch. 40 §40.6A, finding F6).

## #15 — EVM legacy transaction history: equal-height order not deterministic — **OPEN (follow-up, not verified against the reference)**

The EVM history writers (`mm2src/coins/eth/eth_impl.rs`, the two sorts of the
collected history) use a comparator that is not a total order: it ranks
unconfirmed rows first but leaves rows of equal height, and several unconfirmed
rows, in whatever order the collection produced. The UTXO and QRC20 writers
had the same comparator and now follow ch. 38 R38.5A.1 (unconfirmed first,
height descending, `internal_id` ascending). §38.5A does not cover EVM, so the
reference order for EVM is not established yet: a Spec Reader question comes
before any change. Low priority: the desktop wallet does not read EVM history
from KDF (it uses an explorer proxy), so it is not affected.

## #16 — UTXO: P2PK spending exists in Electrum mode only — **OPEN (not verified live, low priority)**

P2PK inputs can be spent when the coin runs in Electrum mode. In Native mode
(a coin daemon behind the node) Reloaded has no P2PK path, whereas the reference
carries the previous output script on every unspent from both backends. A P2PK
output held by a Native-mode wallet is therefore probably not spendable. This
has not been tried on a live node. More in
[`38-utxo-coin-maintenance-and-features.md`](./38-utxo-coin-maintenance-and-features.md)
R38.6.3.

---

## #17 — Orderbook: legacy `orderbook` fails, legacy `best_orders` drops, on a shielded-coin order — **IMPLEMENTED in PR #88 (one legacy-versus-v2 difference remains, last item)**

CRD reference: [`32-orderbook-p2p-and-trie.md`](./32-orderbook-p2p-and-trie.md)
§32.8 R-F6b, R-F6c, R-F6d, R-F6e, T-F6.1–T-F6.4;
[`39-zcash---z_coin-shielded-coin.md`](./39-zcash---z_coin-shielded-coin.md)
R39.5.3. Reference designations: `v2.6.0-beta` and `v3.0.0-beta` (agree).

- [x] With one maker order for a shielded-coin/UTXO pair on the book, the
  legacy `orderbook` for that pair fails on every call with an internal
  error (address derivation unsupported for the shielded protocol), so a
  taker on the legacy generation cannot read the pair at all; the legacy
  `best_orders` silently omits the same order. The mmrpc-2.0 generation of
  both methods is correct (order listed with `{"address_type":"Shielded"}`).
  Reproduced in the swap lab on 2026-10-26, and again on 2026-10-28 with the
  3.0.0-beta release binary, where a taker had to take an ARRR order by uuid.
- [x] This is parity with a **reference defect**: at both designations the
  legacy generation routes through a coin-generic resolver with no shielded
  outcome and aborts (orderbook) or omits (best_orders). Reloaded deliberately
  diverges: ch. 32 R-F6d routes both legacy methods through the single
  R-F6 derivation, R-F6e renders the shielded outcome as the fixed string
  `Shielded`, and R-F6c (drop only the unresolvable order) applies to all four
  methods.
- [x] Same root cause, wider than shielded coins: Siacoin and Lightning orders
  (the earlier R-F6b fix reached mmrpc 2.0 only) and Tendermint orders
  (Reloaded's coin-generic resolver refuses the family the reference derives)
  abort the legacy `orderbook` too. Covered by the same routing change.
- [x] Node-local response shape only; nothing on the P2P wire changes. The
  placeholder string is a Reloaded decision with no reference counterpart; a
  `docs/GLEEC_COMPATIBILITY.md` note is appropriate once implemented.
- [ ] One legacy-versus-v2 difference remains outside R-F6c: when the orderbook's
  unordered index names an order uuid that is missing from the order set, the
  legacy `orderbook` still fails the whole response, where the mmrpc-2.0 handler
  skips that uuid with a warning. Not covered by R-F6c, which is about address
  derivation; a Reloaded-internal inconsistency, not a reference-parity item.

## #19 — Statistics store: an existing row is not refreshed from a later terminal report — **OPEN (small, post-beta follow-up)**

CRD reference: [`51-legacy-v1-swap-state-machine.md`](./51-legacy-v1-swap-state-machine.md)
R50A item 5. Reference designations: `v2.6.0-beta` and `v3.0.0-beta` (agree).

The publication, redaction and own-role key fill of R50A are implemented in
PR #108. Two small items remain:

- [ ] For a swap identifier that is already in `stats_swaps`, R50A item 5 also
  refreshes the fiat price, GUI and version columns from the later report.
  Reloaded sets the own-role key column only and leaves the other columns as
  the first report wrote them (the previous code did not refresh them either).
  Needs a decision on which columns each role owns.
- [ ] No state-16 migration corrects the key columns of databases already at
  state 15; they keep the old both-parties values. Deliberately not done for
  the beta, since almost no Reloaded node has a populated statistics store yet.

---

## #18 — Orderbook keep-alive: one aggregated message to every pair topic, where the reference sends one single-pair message per topic — **OPEN (hotfix; chapter material authored 2026-10-28, passed the KDF Dirty Gate 2026-10-28)**

CRD reference: [`32-orderbook-p2p-and-trie.md`](./32-orderbook-p2p-and-trie.md)
§32.9 R-K2a and its divergence note, T-K1–T-K4; §32.7.3 R-Q4–R-Q6;
§32.8 R-F3a; §32.6 R-T3b open-decision note and §32.10 D5, D6;
[`59-legacy-ordermatch-negotiation.md`](./59-legacy-ordermatch-negotiation.md)
R59.6.2. Reference designations: `v2.6.0-beta` and `v3.0.0-beta` (agree).

- [x] *Fixed 2026-10-28: Reloaded conforms to R-K2a.* A Reloaded maker with K advertisable pairs under one pubkey publishes,
  every 30 s, K gossip messages that each carry all K roots, one per pair
  topic. The reference publishes K messages that each carry one root, each
  on its own pair's topic. Against reference relays the aggregated form
  costs up to K K-pair synchronization requests and K² full snapshots per
  maker per tick, and one unanswerable pair fails all K. *(Corrected
  2026-10-28, second pass, passed the KDF Dirty Gate on 2026-10-28: a relay has already
  forwarded the K copies at the gossip layer — ch. 32 R-F3a — so the
  failure does not un-forward them; it leaves every downstream peer that
  asks the relay at the placeholder failure of R-Q5a.)*
  Present on the network on 2026-10-28 while public seed nodes were
  overloaded; the swaplab control showed no growth attributable to the
  aggregated shape alone. Fix: conform to R-K2a (sender side only; the receiver needs no
  change).
- [x] *Fixed 2026-10-28 (ch. 32 D5):* Reloaded's synchronization responder no
  longer refuses while its trie worker lags (amended R-T3b item 2); it answers
  from the store as it stands, as the reference does.
- [x] *Done 2026-10-28 (ch. 32 R-Q4a):* Reloaded names its own held root in
  the synchronization request by default; wire-compatible, yields usable
  deltas; a deliberate divergence with the opt-back switch
  `orderbook_sync_names_held_root`.
- [x] *Done 2026-10-30 (ch. 32 R-Q5c):* after a quick failure of its own
  keep-alive-driven synchronization request, Reloaded asks the same peer once
  more after a pause, for the pairs still at the placeholder, while the answer
  can still land within the R-Q5b bound; the reference never retries.
  Wire-compatible; a deliberate divergence with the opt-back switch
  `sync_request_retry_ms`.
- [x] *Done 2026-10-30 (ch. 32 R-F3b, D11):* a Reloaded relay sends the
  application-level second copy only for priority messages and for
  messages outside the orderbook topics; the reference sends every message
  twice. Receivers drop the copy either way (R-F3c). Wire-compatible; a
  deliberate divergence with the opt-back switch
  `relay_second_copy_priority_only`.
- [ ] Hardening candidates (ch. 32 D6): bounded or coalescing mutation queue,
  in-flight synchronization de-duplication, orphan-node release, inbound
  task cap. Deferred, none bound.

---

## #19 — Relay forwards before it handles; unbounded per-peer outbound queue; orphaned trie nodes — **PARITY (reference facts authored 2026-10-28, second pass; passed the KDF Dirty Gate 2026-10-28)**

CRD reference: [`32-orderbook-p2p-and-trie.md`](./32-orderbook-p2p-and-trie.md)
§32.8 R-F3a/R-F3b (rewritten), §32.7.3 R-Q5a, §32.9A R-B1–R-B3, V5;
[`28-libp2p-modernization.md`](./28-libp2p-modernization.md) R11a, R13a,
D7, T11, V5. Reference designations: `v2.6.0-beta` and `v3.0.0-beta`
(agree); the baseline anchor differs (V5 in both chapters).

- [ ] Not a gap but a corrected description: a relay forwards every gossip
  message at the gossip layer immediately on receipt and again on the
  application's success; the first-pass claim that a relay withholds a
  keep-alive until its own synchronization succeeds was wrong for the
  reference and for Reloaded alike. Downstream requests therefore race the
  relay's own handling, and the three failure classes of R-Q5a are the
  expected result at a rate set by keep-alive messages per second times
  downstream requesters. A non-relay forwards nothing it received, so only
  relays and the publishing maker are ever asked (ch. 32 R-F3b). Nothing to
  implement; chapters corrected.
- [ ] Reference bound inventory (R-B1): unbounded are the trie node store
  (nodes of a cleared slice are never released), the mutation queue, the
  per-message task count and the substrate's per-peer outbound queue; a
  pubkey that keeps sending keep-alives never expires. Reloaded shares the
  profile (R-B2 parity). Hardening candidates: ch. 32 D6 and ch. 28 D7
  (per-peer outbound queue bound; forward once instead of twice). Deferred,
  none bound; each is a divergence to record when adopted.
- [ ] The message-id encoding mismatch (ch. 28 R13, closed on dev) never
  involved seeds: a relay that subscribes to no orderbook topic neither
  announces nor requests message ids for them (R13a). No action.

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
