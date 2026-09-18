# Plan: Pirate Chain (ARRR) v6.0 / Ironwood compatibility for KDF Reloaded

> **Status:** accepted 2026-09-17; implementation started. Pending consultation with the
> Pirate Chain team (§7) and one internal design decision (§5A).
> Written to be readable without knowledge of KDF Reloaded internals: the **Overview** is for
> everyone; §1 lists verified facts with sources; §2–§5 are implementation detail for our
> engineers; §6 is the timeline; §7 the questions for the Pirate team; §9 the decisions log.

## Overview

### What KDF Reloaded is, as far as ARRR is concerned

KDF Reloaded is an atomic‑swap engine and light wallet (a fork of the Komodo DeFi Framework).
For Pirate Chain it is a **Sapling‑only light wallet**:

- Shielded notes are found by downloading **compact blocks** from Pirate `lightwalletd`
  servers (gRPC `GetTreeState`, `GetBlockRange`, `GetMempoolTx`) and trial‑decrypting them
  locally with the upstream Zcash Rust libraries (`librustzcash`: `zcash_client_backend`,
  `zcash_primitives`, `zcash_client_sqlite`, …).
- The chain tip and transaction broadcast go through **ARRR Electrum servers**
  (`arrr.electrumN.cipig.net`), not through `lightwalletd`.
- Transactions (swap payments, refunds, withdrawals) are **built locally** with the upstream
  `zcash_primitives` transaction builder, which picks the consensus branch ID from the
  network‑upgrade heights we give it.
- After a transaction is built, the **swap protocol logic** (validating the counter‑party's
  payment, detecting spends, extracting the swap secret, waiting for confirmations) re‑reads
  transaction bytes with KDF's *own* UTXO parser, shared by every Bitcoin‑family coin.

We recently moved off the old Komodo fork of the Zcash crates onto **unmodified upstream
crates from crates.io** (`zcash_protocol 0.9.0`, `zcash_primitives 0.28.0`,
`zcash_client_backend 0.23.0`, `zcash_client_sqlite 0.21.1`), plus ~280 lines of local
patches that add our atomic‑swap P2SH input type to the builder.

### What changed on the Pirate side (v6.0.0 – v6.0.4, Aug 2026)

Verified against the Pirate source and release notes (exact references in §1):

1. **A new shielded pool, "Ironwood", with a new transaction format — version 6.**
   Pirate's Ironwood is Zcash's NU6.3 "Ironwood" (the Orchard‑V3 pool) carried over into
   Pirate: same consensus branch ID **`0x37a5165b`**, same v6 version‑group ID
   **`0xD884B698`**, same `BundleVersion::ironwood_v3()`. Pirate never activated Orchard, so
   the Orchard slot of a Pirate v6 transaction is always empty.
2. **Activation is by wall‑clock time, not by a pre‑agreed height.** Mainnet activates at
   the height of the first block whose timestamp exceeds **1791054000 (Sat 3 Oct 2026
   19:00 UTC)** **plus 60 blocks** (`komodo_activate_ironwood`). Every node derives that
   height itself, once the transition block is 30 blocks deep; it becomes known roughly
   half an hour before it takes effect.
3. **After activation only version‑6 transactions are "standard".** `IsStandardTx` requires
   `IRONWOOD_MIN_CURRENT_VERSION = 6` and mainnet has `fRequireStandard = true`. Version‑4
   Sapling transactions stay *consensus‑valid* but are **rejected by every default
   mempool**, and v4 transactions still in mempools at activation are **evicted**. Before
   activation, a v6 transaction is rejected (with a DoS score for the sender). So the
   switch is sharp, at one block height, in both directions.
4. **Pirate `lightwalletd` v1.0.0.0** shipped with Ironwood support: new fields on `TreeState`
   (`saplingFrontier`, `ironwoodTree`), Ironwood actions on `CompactTx` (reusing field 6),
   new RPCs (`GetBridgeTreeState`, `GetSubtreeRoots`). The Sapling fields we rely on kept
   their wire numbers; our vendored protobuf definitions still decode the new servers.
5. The block‑4141650 / 19 Sep event is a **separate, smaller** change (dPoW notary
   `requiredSigs`), not Ironwood. We found no impact on light clients.

### The two problems, in one sentence each

- **Problem A — "the light wallet shows a zero balance."** The symptom reported for the
  older Komodo‑based ARRR light wallet (GleecDEX / Cheetahdex) after the v6.0.x server
  upgrades. For KDF Reloaded this is a *verification and hardening* task: make sure our
  light‑sync handshake with v6‑era `lightwalletd`/`pirated` servers works and, when it does
  not, report it in a way the wallets can show, instead of quietly staying at the old number.
- **Problem B — "after 3 October, no ARRR swap step works."** Our transactions are version 4
  with the Sapling branch ID; after Ironwood activates the network rejects them. We must
  build **version‑6 transactions committing to the Ironwood branch ID**, know the activation
  height at runtime, **and** make every swap step that reads ARRR transaction bytes
  understand the v6 format and its new txid (ZIP‑244). Sending is only the first casualty:
  validating the other side's payment, detecting spends and extracting the swap secret all
  fail on v6 bytes today.

### What today's live checks show (2026‑09‑16, tip ≈ 4 136 800)

We probed every ARRR `lightwalletd` and Electrum endpoint in the shared coin configuration:

| Endpoint | Status | Notes |
|---|---|---|
| `lightd1.pirate.black:443` | up, **lightwalletd v1.0.0.0** (new) | `GetTreeState` returns a valid serialized Sapling tree at tip‑10, tip‑2881, tip‑50000 and 1 900 000; `ironwoodTree` = bare root (pool empty); compact blocks byte‑identical to the old servers |
| `electrum1.cipig.net:9447`, `electrum2.cipig.net:9447` | up, lightwalletd `v0.0.0.0-dev` (old) | identical Sapling trees and compact blocks |
| `electrum3.cipig.net:9447` | **down** (connection refused) | |
| `piratelightd1‑4.cryptoforge.cc:443` | **down** (DNS does not resolve) | 4 of the 8 configured servers |
| `arrr.electrum1/2.cipig.net:20008` | up, ElectrumX 2.0.0, tip 4 136 801 | |
| `arrr.electrum3.cipig.net:20008` | **down** | |

**Pre‑activation, the wire data our light wallet consumes is unchanged and valid** on both
old and new servers, and the tree‑state fallback hazard described in §3 did not fire at any
probed height. The zero‑balance report is therefore *not reproduced by the protocol data
alone*; Step 0 verifies our own binary end‑to‑end before we change code. Half the configured
servers are dead, which costs one connection timeout per dead server on every activation.

### The plan in four steps

| Step | What | Depends on | Addresses |
|---|---|---|---|
| **0. Verify live** (no code) | Activate ARRR in KDF Reloaded against the live servers, receive a small test amount, confirm balance and history, send it back; per‑server runs; keep the logs. | — | tells us whether Problem A exists for us today |
| **1. Problem A + the Oct‑3 safety net** → small release this week | (i) validate the tree‑state answer (reject a bare root, height/hash mismatch); (ii) report sync failures through the existing `sync_status` error channel, and file wallet‑GUI tickets to display it; (iii) remove dead servers from the coin list; (iv) **Ironwood guard + swap freeze**: from a configured wall‑clock time, stop starting new ARRR swaps and refuse every ARRR transaction build from activation on, on builds without v6 support; (v) regression tests. | — (no dependency change) | A, and protects funds on 3 Oct even if steps 2–3 slip |
| **2. Upgrade the Zcash library stack** | `zcash_protocol 0.9 → 0.10.x`, `zcash_primitives 0.28 → 0.30.x`, `zcash_client_backend 0.23 → 0.24`, `zcash_client_sqlite 0.21 → 0.22`; re‑apply our local builder patches; keep Orchard off; verify the wallet‑DB migration path. | step 1 released | nothing by itself; **prerequisite for B** (v6 transactions and the Ironwood branch ID exist only in the new line) and a stable base so A/B results cannot be undone by a later upgrade |
| **3. Problem B** — Ironwood‑era transactions | (i) map `Nu6_3` to the Ironwood activation height in our consensus parameters; (ii) learn the height at runtime (derive it from block timestamps → config override → server branch‑ID sanity check); (iii) let the builder emit v6 for target heights ≥ activation, with a quiet window at the boundary; (iv) replace every hardcoded "Sapling" branch ID with a height lookup; (v) **make the swap layer read v6 ARRR transactions** (design open — §5A); (vi) test on Pirate regtest / testnet. | step 2 | B |

Ordering rationale (maintainer decision 2026‑09‑16): Problem A is small and independent of
the dependency change, so it ships first as a low‑risk release that also carries the Oct‑3
safety net; the crate bump — the longest and riskiest item — follows on a clean base; B
builds on the bump because only the new crates know the v6 format.

### What we would like the Pirate team to confirm

Short form — full list with context in §7:

1. Version‑4 Sapling transactions are intentionally **non‑standard** after Ironwood; would a
   grace period (v4 standard until a later height) be considered?
2. For a transaction with only transparent + Sapling parts (empty Orchard slot, empty
   Ironwood bundle), Pirate's v6 wire format, txid and sighash are **byte‑identical to Zcash
   NU6.3**, so unmodified upstream `librustzcash 0.30` output is valid on Pirate.
3. The mainnet activation height will be **published** once derived, and ideally exposed by
   `lightwalletd` (a `LightdInfo` field).
4. A public **Ironwood‑activated testnet** (`lightwalletd` + Electrum) or a regtest recipe
   *with standardness enforced* is available for third‑party wallet testing.
5. `lightwalletd`'s `GetTreeState`/`GetBridgeTreeState` should not put a bare 32‑byte
   `finalRoot` in the `saplingTree` field when `finalState` is missing.
6. Which `lightwalletd` endpoints are canonical (5 of 8 in the shared coin list are down);
   will the cipig ElectrumX servers index, deserialize and relay v6 transactions.
7. Sapling‑only v6 transactions (with our P2SH HTLC output and OP_RETURN redeem‑script
   reveal) stay standard indefinitely — no Sapling→Ironwood turnstile or sunset planned.

---

## 1. Verified facts and where they come from

All external facts were read from source on 2026‑09‑16; nothing is inferred from
announcements alone.

| Fact | Source |
|---|---|
| Ironwood branch ID `0x37a5165b`, chosen to match Zcash NU6.3 so upstream Rust resolves `BundleVersion::ironwood_v3()` | `PirateNetwork/pirate` `src/consensus/upgrades.cpp` |
| v6 tx: `IRONWOOD_TX_VERSION = 6`, `IRONWOOD_VERSION_GROUP_ID = 0xD884B698`; layout = header, branch id, locktime, expiry, vin, vout, Sapling bundle, **compact‑size 0 Orchard slot**, Ironwood bundle ("wire‑compatible with the upstream v6 parser") | `src/primitives/transaction.h` (`isIronwoodV6` block) |
| Upstream `zcash_protocol 0.10.6`: `BranchId::Nu6_3 = 0x37a5165b`; `V6_TX_VERSION = 6`, `V6_VERSION_GROUP_ID = 0xD884B698`; not cfg‑gated (only Nu7/Tachyon are) | `zcash_protocol-0.10.6/src/{consensus,constants}.rs` |
| Upstream `zcash_primitives 0.30.1`: `TxVersion::suggested_for_branch(Nu6_3) = V6`, `TxVersion::V4` still `valid_in_branch(Nu6_3)`; `Builder::new(params, target_height, BuildConfig)` derives branch and version from `BranchId::for_height(&params, target_height)`; `BuildConfig::Standard { sapling_anchor, orchard_anchor, ironwood_anchor, orchard_padding, ironwood_padding }`; `ironwood_anchor: None` ⇒ no Ironwood bundle; `Transaction::read` parses v6 natively; ZIP‑229 Sapling v6 digests `ZTxIdSSpendNH_v6`/`ZTxAuthSapliH_v6` | `zcash_primitives-0.30.1/src/transaction/{mod,builder,txid}.rs` |
| Pinned `zcash_protocol 0.9.0` has **no** `Nu6_3` (Sprout … Nu6_2; `Nu7` behind a cfg) | `~/.cargo/registry/src/*/zcash_protocol-0.9.0/src/consensus.rs` |
| `zcash_client_backend 0.24.0` **enables `orchard` by default** (`default = ["time/default", "orchard"]`); 0.23.0 did not; our native manifest line does not set `default-features = false` | `zcash_client_backend-0.24.0/Cargo.toml:50‑53`, `vendor-patches/zcash_client_backend-0.23.0/Cargo.toml:50`, `mm2src/coins/Cargo.toml:188` |
| After Ironwood only v6 is standard: `IRONWOOD_MIN/MAX_CURRENT_VERSION = 6`; `Params().RequireStandard() && !IsStandardTx(...)`; mainnet and testnet `fRequireStandard = true`, **regtest `false`**; before activation v6 is rejected (`bad-sapling-tx-version-group-id`, `bad-tx-pre-ironwood-consensus-branch-id`, DoS‑scored); v4 evicted from mempools at activation (`removeWithoutBranchId`) | `src/primitives/transaction.h:616‑619`, `src/main.cpp:935‑950, 1403‑1433, 2120, 5341`, `src/chainparams.cpp:225, 442, 527` |
| Activation rule: `KOMODO_IRONWOOD_ACTIVATION = 1791054000`; `activation = (height of first block with nTime > T) + 60`; evaluated once the chain is ≥30 blocks past the transition, 24 h look‑back; regtest fixed at 200, testnet at 280 500 | `src/komodo_defs.h:39`, `src/main.cpp:4922‑4990, 5103`, `src/chainparams.cpp:387, 491` |
| Fee policy unchanged (legacy `minRelayTxFee`); our fixed 1000‑zat HTLC‑spend fee | `src/main.cpp:134, 2024‑2036`; `z_htlc.rs:179` |
| Pirate's `ac_private` exemption — Sapling→P2SH output with redeem‑script reveal in OP_RETURN — is what our HTLC relies on | `src/main.cpp:1740‑1752`; `z_htlc.rs:64‑76` |
| lightwalletd `GetTreeState` prefers `z_gettreestate` (frontier anchors, legacy‑serialized via `SaplingMerkleFrontierLegacySer`) and falls back to `z_gettreestatelegacy`; **both** `GetTreeState` and `GetBridgeTreeState` fill `saplingTree` via `preferredTreeState()`, which returns **`finalRoot` when `finalState` is empty**; `LightdInfo.consensusBranchId` is the **chain‑tip** branch (`getblockchaininfo.consensus.chaintip`), not next‑block | `PirateNetwork/lightwalletd` `frontend/service.go:370‑470`, `common/common.go:91‑92, 294`; `pirate` `src/rpc/blockchain.cpp:2132‑2290` |
| lightwalletd protos: `TreeState.tree(5)` → `saplingTree(5)` (+`saplingFrontier(6)`, `ironwoodTree(7)`); `CompactTx.actions(6)` **reused for Ironwood** (upstream lightwallet‑protocol uses a separate `ironwood_actions`); `GetAddressUtxosArg.address` → `repeated addresses` — all wire‑compatible with our vendored copies while Orchard stays off | `lightwalletd/walletrpc/{service,compact_formats}.proto` vs `mm2src/coins/z_coin/*.proto` |
| Live server behaviour (versions, tips, tree sizes, compact‑block equality) | probes 2026‑09‑16, re‑run 2026‑09‑17 with identical results |

Our own code — key locations (paths relative to the repository root):

| Concern | Location |
|---|---|
| Network parameters given to the Zcash libs; `activation_height` returns `None` for every upgrade after Sapling ⇒ branch ID is Sapling at every height; the `match` is exhaustive, so `Nu6_3` must be added the moment the crate is bumped | `mm2src/coins/z_coin.rs` `ZcoinConsensusParams` (L145‑217) |
| **All funds‑moving ARRR builds go through the upstream builder** and therefore through `ZcoinConsensusParams`: HTLC send / dex fee (`z_htlc.rs:39,82` → `gen_tx`, `z_coin_ops.rs:261, 424`), HTLC spend & refund (`z_swap_ops.rs:56,94,132,167` → `z_p2sh_spend`, `z_htlc.rs:151`, `add_kdf_p2sh_input` at `:176`), withdraw (`z_coin.rs:1348`) | see cited lines |
| **Swap‑layer parsing with our own parser** (v3/v4 only, SHA256d txid): ZCoin delegates `validate_maker_payment`, `validate_taker_payment`, `check_if_my_payment_sent`, `search_for_swap_tx_spend_my/other`, `extract_secret` (`z_swap_ops.rs:290‑354`) and `wait_for_confirmations`, `wait_for_tx_spend` (`z_coin.rs:1237‑1262`) to `utxo_common`, which calls `deserialize::<UtxoTx>` at 11 sites (`utxo_common_swap.rs:243,315,388,457,736,758,799,824,885,1042,1494`), plus `utxo_common_helpers.rs:254,273` and the Electrum client's `find_output_spend` (`electrum_rpc_client.rs:891`); `z_p2sh_spend` re‑parses its own output before broadcast (`z_htlc.rs:207`) | `mm2src/kdf_chain/src/transaction.rs:397‑475` (`deserialize_tx`), `:258‑263` (`hash` = double‑SHA256) |
| Static transparent‑side branch ID `0x76b809bb` (from `txversion: 4`); live for ARRR only in `sign_raw_transaction` (`utxo_common_tx.rs:1312`) and a dead `p2sh_spending_tx` impl (`z_coin.rs:1627‑1649`) | `mm2src/coins/utxo/utxo_builder/utxo_conf_builder.rs:221‑230`, `mm2src/coins/utxo.rs:510` |
| Hardcoded `BranchId::Sapling` when parsing counter‑party transactions | `mm2src/coins/z_coin/z_swap_ops.rs:65,103,141,176`, `mm2src/coins/z_coin.rs:1266` |
| Builder call sites (`BuildConfig::Standard { sapling_anchor, orchard_anchor }`, `target_height`) | `z_coin_ops.rs:261‑266` (Electrum tip), `z_coin_ops.rs:424‑429` (wallet DB "max scanned + 1", may lag the tip by a 30 s poll), `z_htlc.rs:151‑158` (Electrum tip) |
| `TransactionEnum::ZTransaction` exists (native) and the swap layer accepts it; ZHTLC paths currently return `UtxoTx` into the enum | `mm2src/coins/lp_coins_types.rs:104‑115`, `z_swap_ops.rs:11‑197` |
| Light‑sync tree‑state handshake: `GetTreeState` at `start_height − 1` → `read_commitment_tree` | `z_coin_wallet_db.rs` `ensure_lightwalletd_chain_state` (L580‑639), `init_wallet_checkpoint_from_tree_state` (L640‑698) |
| Server failover loop and error strings (pattern to copy) | `z_coin_wallet_db.rs:284‑305` |
| Error surfacing today: activation failure is a hard error (`ShieldedWalletDbScanIncomplete`, `z_coin_activation.rs:271‑283, 474‑484`); periodic sync only WARNs every 30 s (`z_coin.rs:817‑866`); `my_balance` returns the stale DB number with a WARN when the scan is incomplete (`z_coin.rs:1166‑1191`); the unused error channel is `HistorySyncState::Error(Json)` returned as `sync_status` by the tx‑history RPC (`lp_coins_types.rs:1004‑1012`, `my_tx_history_v2.rs:554`) | see cited lines |
| Tip height for light sync = Electrum `get_block_count`; `GetLatestBlock`/`GetLightdInfo`/`GetBlock` are declared in our proto but never called | `mm2src/coins/z_coin.rs:691, 830`, `z_coin_activation.rs:456‑460`; `service.proto:136, 140, 171` |
| gRPC client generated from our vendored Pirate protos (`tonic_build`, client only) | `mm2src/coins/build.rs`, `mm2src/coins/z_coin/z_rpc.rs:141‑143` |
| Local patches to the upstream builder (swap P2SH input, locktime, raw outputs; ~126 + ~154 lines); the P2SH signing arm takes its digest from the builder‑supplied `calculate_sighash`, so ZIP‑244 sighash for v6 comes for free once re‑vendored | `vendor-patches/zcash_primitives-0.28.0/KDF-PATCH.md`, `vendor-patches/zcash_transparent-0.8.0/{KDF-PATCH.md, src/builder.rs:776‑782}` |
| Previous crate migration: policy, 7 acceptance gates, progress log — port landed 2026‑08‑05, stabilization ran to 08‑11; 3 of 4 runtime defects surfaced only in live Windows GUI wallets | `docs/plans/librustzcash-upgrade.md` (L38‑42, 44‑66, 620‑734) |
| Governing specification chapter (updated in the same commit as behaviour) | `docs/reloaded-rewrite/39-zcash---z_coin-shielded-coin.md` |
| Coin configuration consumed by wallets (`consensus_params`, server lists) | **`GLEECBTC/coins`** (`master`, actively maintained): `coins`, `light_wallet_d/ARRR`, `light_wallet_d/ARRR_WSS`, `electrums/ARRR`. `komodo-coins-rin` is a stale 6‑month‑old fork — do not use it as a source of truth. |

---

## 2. Step 0 — verify against the live network (no code changes)

Purpose: establish, with evidence, whether Problem A exists for KDF Reloaded today.

1. Build the current `dev` branch; activate ARRR in Light mode exactly as the desktop wallet
   does (`task::enable_z_coin::init`, `sync_params.height` ≈ tip − 3 000) against the full
   configured server list; then once per **individual** `lightwalletd` server.
2. Send a small amount (≥ 0.01 ARRR) from a Treasure Chest v6.0.4 wallet to the KDF
   z‑address; confirm it appears in `my_balance` and `my_tx_history` after 2 confirmations;
   send it back (exercises the builder pre‑activation).
3. Record activation duration, per‑server timeouts, `GetTreeState` heights requested, and
   every `WARN`/`ERROR` line. Attach to the progress log.
4. Keep the script; it is re‑run as a gate after every step and again after 3 Oct.

Exit criteria: balance visible and spendable — or a reproducible failure with logs that
redefines Problem A precisely.

---

## 3. Step 1 — Problem A hardening and the Oct‑3 safety net (release before any dependency change)

Definition for this plan: *KDF Reloaded must either show the correct shielded balance or
report, through a channel the wallets can display, why it cannot; it must never stay silently
at an old number because a server answered in a shape we did not expect or because the sync
stopped.*

1. **Validate the tree state before trusting it.** In `init_wallet_checkpoint_from_tree_state`
   accept a tree state only if `height` matches the request, `hash` decodes to 32 bytes, and
   field 5 parses with `read_commitment_tree` **and is not a bare 32‑byte root** (the
   `preferredTreeState` hazard — 64 hex chars), **and the parse consumes the whole buffer**
   (`read_commitment_tree` ignores trailing bytes, so a root beginning `00 00 00` otherwise
   parses silently as an *empty* tree). Add the `saplingFrontier` (6) and `ironwoodTree` (7)
   fields to our vendored `TreeState` for logging (wire numbers unchanged; the two exhaustive
   test literals at `z_coin_wallet_db.rs:2705, 2855` need `..Default::default()`). **Not**
   switching to `GetBridgeTreeState`: it fills the Sapling field through the same fallback and
   adds nothing. Error text names the server, the height and the reason (reuse
   `decode_hex_field`, `decode_display_block_hash`, `lightwalletd_error_with_sources`).
2. **Report sync failures where wallets can see them** (maintainer decision: `my_balance`
   stays numeric — a `my_balance` error is not rendered by either GUI and would break
   `withdraw max` and order placement). Record on the coin the light‑scan completion state
   and the last sync error; the tx‑history RPC returns `sync_status: Error { reason }`
   (existing `HistorySyncState::Error`, unused today) while the scan is incomplete or the last
   sync failed. Spending is already blocked in that state (`wallet_db_scan_complete` guard).
   File tickets on `komodo-wallet-desktop-rin` and `komodo-wallet-mobile-rin` to display
   `sync_status.Error` for ZHTLC coins. Document in `docs/GLEEC_COMPATIBILITY.md` and
   chapter 39 — note that ch.39 `:1193`/`:1347`/`:1368` currently assert `sync_status` is
   always `Finished` for a shielded coin, which the shipped code already contradicts; the
   chapter is corrected in the same commit.
3. **Server hygiene.** Remove the five dead lightwalletd endpoints and the one dead Electrum
   endpoint upstream in **`GLEECBTC/coins`** (`light_wallet_d/ARRR`, `electrums/ARRR`;
   `ARRR_WSS` inherits the dead `electrum3`), order by measured latency, and ask Pirate for
   the canonical list (§7 q6).
4. **Ironwood guard and swap freeze** (maintainer decision: wall‑clock, with freeze). This
   is B's safety net but needs nothing from the crate bump, so it ships here:
   - add `ironwood_activation_time: Option<u32>` and `ironwood_activation_height: Option<u32>`
     to `ZcoinConsensusParams` (additive; `serde` has no `deny_unknown_fields` on this path,
     so old and new coin files stay mutually compatible) and publish
     `ironwood_activation_time: 1791054000` in the ARRR coin entry of `GLEECBTC/coins`;
   - a build carries a capability flag for v6 (`false` until step 3);
   - **swap freeze:** on non‑capable builds, refuse to place or match ARRR orders and to
     start ARRR swaps from `T − max_htlc_locktime`, with the error
     `"ARRR trading paused: network upgrade (Ironwood) at <T>; a v4 payment made now could
     not be spent or refunded after the upgrade"`. Rationale: a v4 HTLC funded before
     activation needs a v6 spend/refund after it; an un‑upgraded build cannot produce one,
     so a swap that straddles activation is a loss path for one side. **See Correction 1
     below — `max_htlc_locktime` is ~1.8 days, not the ~5 h originally written here.**
   - **transaction guard:** from `T` on non‑capable builds, `gen_tx` refuses with
     `"ARRR network upgrade (Ironwood) is active; this build cannot create ARRR
     transactions — please upgrade"`. Receiving and balance keep working. **See Correction 2
     below — `z_p2sh_spend` must *not* be guarded.**
   - `GetLightdInfo.consensusBranchId` (chain‑tip branch) is fetched and **logged** as a
     secondary signal; it is not the switch (it lags the next‑block rule by one block).
5. **Tests.** Unit: `init_wallet_checkpoint_from_tree_state` with (a) a valid legacy tree,
   (b) a bare‑root field 5, (c) a leading‑zeros bare root (the silent‑empty‑tree case),
   (d) trailing bytes, (e) height/hash mismatch. `sync_status` = `Error` when
   `wallet_db_scan_complete` is false; freeze and guard trip at the configured times with an
   injected clock. Integration (loopback `lightwalletd` fixture as specified in
   `docs/plans/librustzcash-upgrade.md` §"Deterministic Light‑mode activation harness"):
   all‑servers‑fail surfaces the error; a bare‑root server is rejected and the next used.
6. Release: version bump, `CHANGELOG.md`, chapter 39 (new R‑IDs), coin‑config PR, GUI
   tickets. **Live‑GUI gate:** activation + one HTLC round‑trip from the desktop wallet on
   Windows before tagging (the previous migration's runtime defects were found only there).

### Correction 1 (2026‑09‑18) — the freeze window is ~1.8 days, not ~5 hours

From `mm2src/mm2_main/src/lp_swap.rs:209‑232` and `:734‑786`: `PAYMENT_LOCKTIME = 7 800 s`,
the **maker** payment locks for `lock_duration * 2`, `lp_atomic_locktime_v1` multiplies by
**10** when either side is BTC, and V1 is reachable whenever a legacy peer sends no
`conf_settings` (`ordermatch_trading.rs:446‑456`, `:530‑540`). The 4× V2 path additionally
triggers when either side requires notarization; ARRR upstream sets
`requires_notarization: false`, so for ARRR that depends on the *counterparty* coin. Either
way the BTC ×10 V1 branch dominates and sets the constant.

| Pairing | maker HTLC lock | + refund grace (`+3700`) |
|---|---|---|
| ARRR vs ordinary coin | 15 600 s (4 h 20 m) | 5 h 22 m |
| ARRR vs BCH/BTG/SBTC, or notarized (V2) | 62 400 s (17 h 20 m) | 18 h 22 m |
| **ARRR vs BTC, V1 legacy peer** | **156 000 s (43 h 20 m)** | **44 h 22 m** |

**Decision (2026‑09‑18): flat coin‑level freeze at `T − 156 000 s`**, computed at runtime as
`get_payment_locktime() * 20` — never hardcoded, because `--features custom-swap-locktime`
makes `PAYMENT_LOCKTIME` a settable `AtomicU64`. That puts the freeze at **1 Oct 2026
23:40 UTC**. ARRR trading stops about two days before the fork; chosen over a pair‑aware
freeze for provable safety and a far smaller change.

Implementation: two overrides on `ZCoin`, because neither alone suffices —
`MmCoin::wallet_only` (covers `buy`/`sell`/`setprice`; precedent and rationale at
`mm2src/coins/eth/eth_mm_coin.rs:57‑74`) **and** `is_coin_protocol_supported`
(`z_coin.rs:1526‑1528`; covers the incoming‑P2P match paths `process_taker_request:1027‑1028`
and `process_maker_reserved:922‑923`, which `wallet_only` never reaches). Accepted gap:
`is_wallet_only_ticker`/`is_wallet_only_conf` (`lp_coins_ops.rs:13‑17`) read the static coin
config and will not see a trait override, so `orderbook`, `best_orders` and `trade_preimage`
keep listing a frozen ARRR while `buy`/`sell` refuse cleanly.

### Correction 2 (2026‑09‑18) — do **not** gate `z_p2sh_spend`

The original text above said to guard both `gen_tx` and `z_p2sh_spend`. `z_p2sh_spend` is
reached by all four of `send_maker_spends_taker_payment`, `send_taker_spends_maker_payment`,
`send_taker_refunds_payment`, `send_maker_refunds_payment` (`z_swap_ops.rs:79, 117, 152, 187`)
— it is the **completion and refund** path of swaps that are *already running*. Gating it
would strand in‑flight refunds, the exact opposite of the intent. **Guard `gen_tx` only**, at
the very top, **before** the `z_unspent_mutex` lock at `z_coin_ops.rs:166` and therefore
before the sapling‑sync spin at `:167‑169` (a guard after that spin could hang forever, and a
guard inside only the native branch would miss the Electrum dispatch at `:171‑173`, which is
the ARRR light path).

---

## 4. Step 2 — upgrade the Zcash library stack

Target line (mutually compatible on crates.io; MSRV 1.88, our toolchain is pinned at 1.98.0):

| Crate | From | To |
|---|---|---|
| `zcash_protocol` | 0.9.0 | 0.10.6 |
| `zcash_primitives` | 0.28.0 (vendored) | 0.30.1 (re‑vendored) |
| `zcash_transparent` | 0.8.0 (vendored) | 0.10.0 (re‑vendored) |
| `zcash_client_backend` | 0.23.0 (vendored, manifest‑only patch) | 0.24.0 (**drop the vendored copy** — its manifest has plain `time = "0.3.22"`, the `time-core` workaround is gone) |
| `zcash_client_sqlite` | 0.21.1 | 0.22.0 (needs `rusqlite ^0.37`; we pin 0.37.0) |
| `zcash_keys` | 0.14.0 | 0.16.1 |
| `zcash_proofs` | 0.28.0 | 0.30.0 |
| `sapling-crypto`, `zcash_note_encryption`, `zcash_script`, `zip32`, `incrementalmerkletree`, `orchard`, `shardtree` | keep / follow what the above require (`orchard 0.15`, `shardtree 0.7`, `incrementalmerkletree 0.8.2`) |

Work items, in order:

1. **Fetch and diff first.** `zcash_client_sqlite 0.22.0`, `zcash_transparent 0.10.0` and
   `zcash_keys 0.16.1` were *not* inspected during planning. Before sizing, diff the APIs we
   use: `WalletDb::for_path`, `init_wallet_db`, `BlockDb`, the migration set;
   `zcash_transparent::builder` (target of the 154‑line patch);
   `UnifiedFullViewingKey::from_sapling_extended_full_viewing_key` (`unstable`).
2. Bump pins in `mm2src/coins/Cargo.toml` (wasm32 **and** native sections) and the
   `[patch.crates-io]` block in the root `Cargo.toml`. **Set `default-features = false` on
   the native `zcash_client_backend`/`zcash_client_sqlite`/`zcash_primitives` lines** (today
   only wasm32 has it) and list features explicitly, because 0.24 turns **Orchard on by
   default** — which would add Orchard/Ironwood tree‑size demands to the compact scanner
   (`TreeSizeUnknown { Ironwood }` at the first post‑activation block whenever
   `chain_metadata` is `None`, which is always on Pirate) and Orchard pool state to the
   wallet DB.
3. Re‑apply the two local patches onto the new upstream sources: `zcash_primitives`
   (~126 lines) and `zcash_transparent` (~154 lines). Re‑run their pinned‑hex unit tests;
   update `KDF-PATCH.md` and `rust-version` in each.
4. Fix compile fallout (all under `mm2src/coins/`), known so far: `NetworkUpgrade::Nu6_3` in
   the exhaustive match at `z_coin.rs:215` (return `None` until step 3);
   `BuildConfig::Standard { .. }` at the three call sites gains `ironwood_anchor: None,
   orchard_padding, ironwood_padding`; `select_spendable_notes` gains a `LockFilter` and takes
   `&[ShieldedPool]`; `get_target_and_anchor_heights` returns `(TargetHeight, BlockHeight)`;
   `ChainMetadata` gains `ironwood_commitment_tree_size` and `CompactTx` gains
   `ironwood_actions`, `CompactBlock::proto_version` is removed; `ZTxBuilderError` arity in
   `z_coin_errors.rs`.
5. **Wallet‑database policy** (maintainer decision): allow upstream in‑place migration for
   modern→modern. Verify that `zcash_client_sqlite`'s own migrations take a 0.21.1 database to
   0.22.0, add a fixture proving in‑place migration with no rescan, and update the
   fingerprints in `z_coin_wallet_db.rs`. Rebuild + rescan stays only for unknown/old‑Komodo
   schemas.
6. Verify with the gates in §8. Update `docs/plans/librustzcash-upgrade.md` status and
   chapter 39 in the same commit.

This step must be **behaviour‑neutral on the network**: pre‑activation ARRR transactions
byte‑identical before and after (gate 4).

---

## 5. Step 3 — Problem B: Ironwood‑era transactions

Definition: *every ARRR transaction KDF Reloaded builds for a height at or after Ironwood
activation is a version‑6 transaction committing to branch ID `0x37a5165b`; before activation
it stays exactly what we build today; and every swap step that reads ARRR transaction bytes
understands v6 and its ZIP‑244 txid.*

1. **Consensus parameters.** Map `NetworkUpgrade::Nu6_3` to `ironwood_activation_height` in
   `ZcoinConsensusParams::activation_height`. With that single arm the upstream builder
   selects `TxVersion::V6`, the Ironwood branch ID, ZIP‑244 txids and ZIP‑229 Sapling digests
   by itself; `Transaction::read` parses v6 natively. Plumbing note: the height is learned at
   runtime but `ZcoinConsensusParams` is a plain `Clone + Serialize` value copied into every
   builder and into `WalletDb<_, ZcoinConsensusParams, _, _>` — hold it in an
   `Arc<AtomicU32>` marked `#[serde(skip)]` (0 = unknown), or rebuild the params per build.
2. **Activation‑height discovery at runtime** (maintainer decision: layered; ordered so that
   fresh installs after 3 Oct still work): (a) **derive on demand** — estimate the transition
   height from the Electrum tip and `ironwood_activation_time`, fetch a ±200‑block window
   with `GetBlockRange` (compact blocks carry `time`), take the first block with `time > T`,
   activation = that height + 60; trust only once `tip ≥ transition + 30` (Pirate's own rule);
   persist it; (b) **config override** — `ironwood_activation_height` wins when present;
   (c) **sanity check** — `GetLightdInfo.consensusBranchId` logged; server says Ironwood but
   we have no height ⇒ the step‑1 guard refuses; server says Sapling but our height says
   active ⇒ refuse and log.
3. **Boundary rule.** The switch is sharp at height A in both directions. Therefore
   `target_height = max(electrum_tip + 1, wallet_db_target)`; version by `target_height ≥ A`;
   **quiet window:** refuse to build when `A − 3 ≤ target_height < A`. Expiry height is
   irrelevant to either failure mode.
4. **Height‑aware branch IDs everywhere.** Replace the five `BranchId::Sapling` literals with
   `BranchId::for_height(&consensus_params, height)`; delete the dead `p2sh_spending_tx` impl;
   make `sign_raw_transaction` for ZHTLC use the same lookup.
5. **Swap‑layer parsing of v6 transactions** — **design open, see §5A.** Whichever option is
   chosen, `z_p2sh_spend` must stop re‑parsing its own output as `UtxoTx` before broadcast
   (`z_htlc.rs:207`) and the ZHTLC swap paths must return `TransactionEnum::ZTransaction`.
6. **Coin configuration.** `ironwood_activation_time` published in step 1;
   `ironwood_activation_height` as soon as the network derives it (§7 q3); `txversion` stays 4.
7. **Testing.** ZOMBIE runs Komodo `komodod` and will **not** get Ironwood. Build a docker
   harness with `pirated -regtest` (Ironwood fixed at height 200) + Pirate `lightwalletd`:
   mine past 200, activate the KDF light wallet, receive, then send/refund an HTLC and a
   withdrawal. **Regtest has `fRequireStandard = false`, so it proves consensus validity
   only** — the "v4 rejected, v6 accepted" case needs Pirate testnet (Ironwood at 280 500) or
   a regtest with standardness switched on (§7 q4). Then Step 0 on mainnet after 3 Oct.

### 5A. Open decision — which parser reads v6 ARRR transactions in the swap layer

Neither option touches or vendors the Zcash crates; both leave the transaction *builder* as
described above. The question is which of *our* code paths parse ARRR transaction bytes after
the initial build. Today they all use `kdf_chain`, KDF's own UTXO parser shared by every
Bitcoin‑family coin, which knows Zcash v3/v4 only and computes txids as double‑SHA256. A v6
transaction has a different layout (branch‑id field, v5‑style split Sapling arrays, Orchard
slot, Ironwood bundle) and a **ZIP‑244 txid** (a BLAKE2b digest tree), so each of these steps
fails on v6:

| Swap step (ARRR) | Delegated at | What the shared code does |
|---|---|---|
| validate the counter‑party's payment | `z_swap_ops.rs:290‑299` → `utxo_common_swap.rs:731‑770, 937` | `deserialize::<UtxoTx>`, check vout[0] P2SH, amount, OP_RETURN, `tx.hash()` |
| check whether my payment was sent | `z_swap_ops.rs:301‑310` → `:775` | Electrum scripthash history → fetch raw tx → `deserialize` |
| find who spent the HTLC output | `z_swap_ops.rs:312‑351` → `:836, 859` → `:1032` → `electrum_rpc_client.rs:872‑891` | fetch candidates → `deserialize` → match `previous_output` (needs the **ZIP‑244 txid**) |
| extract the swap secret | `z_swap_ops.rs:352‑354` → `:884` | `deserialize` → read scriptSig pushes |
| wait for confirmations / spend | `z_coin.rs:1237‑1262` → `utxo_common_helpers.rs:246, 266` | `deserialize`, `tx.hash()` → Electrum `transaction.get` by txid |
| our own spend/refund pre‑broadcast | `z_htlc.rs:203‑212` | `deserialize` the builder's bytes into `UtxoTx` |

Common to both options: the ARRR ElectrumX servers must themselves parse v6 and return
ZIP‑244 txids (§7 q6) — otherwise spend detection is impossible on Electrum regardless of our
parser, and tip/broadcast/history must move to `lightwalletd`. And the HTLC *script* helpers
(`payment_script`, script hashing, OP_RETURN layout) do not parse transactions and are reused
unchanged.

**Option 1 — ARRR uses the upstream Zcash crate parser (ZCoin‑specific overrides).**
*Change:* in `mm2src/coins/z_coin/`, stop delegating the six functions above to `utxo_common`;
implement them for ZCoin on `zcash_primitives::Transaction::read` (v4/v5/v6 correct) and
`Transaction::txid()` (SHA256d for v4, ZIP‑244 for v5/v6 — already implemented and tested
upstream), reading `transparent_bundle().vout/vin`. Keep the Electrum client for history and
raw‑tx fetches but stop letting it deserialize candidates: add a `find_output_spend_raw`
variant returning raw bytes. Keep validation *semantics* identical and prove it with a
side‑by‑side test over recorded v4 swap transactions.
*Impact:* ARRR and ZOMBIE only; the shared UTXO core and every other coin's swap path
untouched. Native only (`ZTransaction` is `cfg(not(wasm32))`).
*Effort:* ~300–500 lines + tests; 2–4 days.
*Pros:* contained blast radius; uses the one implementation of v6 and ZIP‑244 that already
exists and matches Pirate by construction (§7 q2); no consensus‑critical hashing written by
us; easiest to review.
*Cons:* duplicates ~6 HTLC checks that exist in `utxo_common` (two places to maintain); a
subtle semantic drift between the copies would only show in mixed‑version swaps; benefits no
other coin.

**Option 2 — extend `kdf_chain` to v5/v6 with ZIP‑244 txids.**
*Change:* in `mm2src/kdf_chain/src/transaction.rs` (570 lines today): extend
`deserialize_tx`/`serialize` with the v5/v6 layout (header + version‑group + branch id +
lock/expiry, transparent, Sapling v5 split arrays, binding sig, Orchard slot, Ironwood
bundle, proofs and signatures) and add a `TxHashAlgo::Zip244` computing the BLAKE2b digest
tree. `sign_raw_transaction` on a v6 ARRR tx would also need ZIP‑244 in `kdf_script`.
*Impact:* shared core used by every UTXO coin on every target (incl. wasm); every coin's swap
path in the regression surface; consensus‑critical hashing re‑implemented and maintained by
us in parallel to the Zcash crate. `kdf_chain` stays free of Zcash dependencies (it has only
`hex`, `kdf_crypto`, `kdf_primitives`, `kdf_codec`).
*Effort:* ~400–700 lines + a ZIP‑244 test‑vector suite; 4–7 days + broader regression testing.
*Pros:* a single parser for all coins (no duplicated HTLC logic); any future v5+
Zcash‑family coin becomes supportable; works on wasm.
*Cons:* largest change to the most shared code right before a hard deadline; a second
implementation of consensus hashing whose mismatch would be silent and funds‑affecting;
review burden on people who do not otherwise touch Zcash.

**Option 3 — hybrid: `kdf_chain` delegates v5/v6 to the Zcash crate.** Rejected for the
record: it would make the lightweight, wasm‑friendly `kdf_chain` depend on the whole Zcash
stack for every coin and target.

**Recommendation for the discussion:** Option 1 for the 3 Oct deadline, with Option 2 recorded
as a possible later consolidation if a second v5+ coin is ever wanted.

---

## 6. Timeline and fallback

Activation is 3 Oct 19:00 UTC. Sizing, with the previous migration's history as the yardstick
(port landed in a day after a spike, stabilization took a further week, three of four runtime
defects were found only in live GUI wallets):

| Step | Working days |
|---|---|
| 0 verify live | 1 |
| 1 A hardening + guard/freeze + release + live‑GUI gate | 3–4 |
| 2 crate bump (fetch/diff, Orchard off, patches, fallout, DB migration, gates) | 6–8 |
| 3 B (params + discovery + boundary 2–3; v6 parsing 2–4 or 4–7 per §5A; regtest harness 2; verification 1–2) | 7–12 |
| **Total** | **17–25** |

**That exceeds the window.** The plan is therefore built so that a slip is safe rather than
fast:

- The **guard and swap freeze ship in step 1**, before any dependency change. Whatever
  happens to steps 2–3, a step‑1 build stops opening ARRR swaps ahead of activation and stops
  building ARRR transactions at activation, with clear messages; receiving and balance display
  keep working (Sapling notes are unaffected by Ironwood). Users lose nothing; they wait for
  the next release to trade ARRR again.
- There is **no useful shortcut around step 2**. Patching `Nu6_3` into the old
  `zcash_protocol 0.9.0` would give us the branch ID but the old `zcash_primitives` would
  still emit v4 (or, if `Nu6_2` were reused, a v5 header Pirate does not accept), which is
  non‑standard after activation — unless Pirate relaxes `IRONWOOD_MIN_CURRENT_VERSION` to 4
  (§7 q1). If they do, a ~40‑line vendored patch becomes a viable interim for B.
- Any KDF build (ours or Komodo's) that is not upgraded will be unable to send or swap ARRR
  after activation.

---

## 7. Questions for the Pirate Chain team

1. **Standardness.** `IsStandardTx` requires transaction version 6 after Ironwood
   (`IRONWOOD_MIN_CURRENT_VERSION = 6`). Zcash kept v4 standard through NU5/NU6. Is the
   intent to force every wallet to v6 on day one? Would a grace period (v4 standard until a
   later height) be considered? It would decouple light‑wallet upgrades from the fork date.
2. **Wire compatibility.** For a v6 transaction containing only transparent inputs/outputs
   and a Sapling bundle (empty Orchard slot, empty Ironwood bundle), are the serialization,
   txid (ZIP 244) and signature hash byte‑identical to Zcash NU6.3 — i.e. is a transaction
   produced by unmodified upstream `zcash_primitives 0.30` with `BranchId::Nu6_3` valid on
   Pirate? Are the Sapling v6 digest personalizations (`ZTxIdSSpendNH_v6`,
   `ZTxAuthSapliH_v6`) unchanged? Does the `ac_private` exemption (Sapling → P2SH output with
   the redeem script revealed in OP_RETURN, `main.cpp:1740‑1752`) apply unchanged to v6, and
   are there any v6‑specific fee or OP_RETURN standardness rules?
3. **Activation height.** Please confirm the rule (first block with `nTime > 1791054000`;
   activation = that height + 60; trusted once 30 blocks deep) and publish the derived
   mainnet height as soon as it is known. Could `lightwalletd` expose it (e.g.
   `LightdInfo.ironwoodActivationHeight`) so light clients need not re‑derive it? We note
   `LightdInfo.consensusBranchId` reports the chain‑tip branch, not the next block's.
4. **Test network.** Is there a public Ironwood‑activated testnet with `lightwalletd` (and
   ideally an Electrum server) that third‑party wallets can use? Regtest has
   `fRequireStandard = false`, so it cannot show the v4‑rejected / v6‑accepted behaviour — is
   there a flag to enforce standardness on regtest, or a recommended docker recipe?
5. **`GetTreeState` fallback.** `preferredTreeState()` returns `finalRoot` (a 32‑byte hash)
   in the `saplingTree` field of both `GetTreeState` and `GetBridgeTreeState` when
   `finalState` is unavailable. Legacy clients parse that field as a serialized commitment
   tree and fail — or worse, a root beginning `00 00 00` parses silently as an empty tree.
   Could the field be left empty (or the RPC return an error) in that case?
6. **Endpoints and Electrum.** `piratelightd1‑4.cryptoforge.cc` no longer resolve and
   `electrum3.cipig.net:9447` refuses connections — which `lightwalletd` endpoints are
   canonical for the shared coin list? `LightdInfo.piratedBuild` is empty on all servers —
   can it be populated? The `arrr.electrumN.cipig.net` ElectrumX 2.0.0 servers are our
   source of the chain tip, our broadcast path, and our spend‑detection path (scripthash
   history + raw tx by txid): will they deserialize v6 transactions, index them under their
   ZIP‑244 txids, and relay them after activation? If not, we will move those functions to
   `lightwalletd` (`GetLatestBlock`, `SendTransaction`, `GetTaddressTxids`) — please confirm
   those are considered stable.
7. **Sapling longevity.** Will Sapling‑only v6 transactions stay standard indefinitely, or is
   a Sapling→Ironwood turnstile / Sapling deprecation date planned? Our swap protocol is
   Sapling‑only.
8. **Compact format.** Pirate's `lightwalletd` reuses `CompactTx.actions` (field 6) for
   Ironwood actions, while the upstream lightwallet protocol defines a separate
   `ironwood_actions` field. Harmless for Sapling‑only clients, but worth aligning before
   third‑party Orchard‑aware scanners appear.
9. **Sanity.** The 19 Sep `requiredSigs` change has no effect on light clients — correct?

---

## 8. Acceptance gates and verification

Reused from `docs/plans/librustzcash-upgrade.md` (gates 1–7) plus Ironwood‑specific ones:

1. Native Linux, Windows GNU release and wasm32 (`-p coins`, `-p mm2_db`, `-p mm2_main`)
   compile; `cargo test --no-run --bin docker_tests --features regtest-netid` passes.
2. All shielded store/scan tests and `coins_activation` tests pass; the new tests from
   §3/§5 pass; Option‑1 side‑by‑side test (if chosen) or ZIP‑244 vector suite (if Option 2).
3. Wallet‑DB: a 0.21.1 fixture migrates in place with no rescan; unknown/old‑Komodo fixtures
   still rebuild + rescan, never migrate in place.
4. Pre‑activation ARRR transactions are **byte‑identical** before and after step 2
   (pinned‑hex fixtures in the vendored patches).
5. The Step 0 script passes against live servers before and after each step.
6. Regtest: a v6 transaction built for height ≥ 200 is accepted, mined and scanned back by
   KDF; a build request inside the quiet window is refused before broadcast. Testnet (or
   standardness‑enforcing regtest): v4 rejected as `ironwood-version`, v6 accepted.
7. All‑servers‑fail and bare‑root tree‑state cases surface as `sync_status: Error` (and as
   activation errors at activation time), never as a silently stale balance; the freeze and
   guard trip at the configured times.
8. **Live‑GUI gate** after step 1 and after step 3: activation + one HTLC round‑trip from
   the desktop wallet on Windows (where the previous migration's defects surfaced).
9. Chapter 39, `GLEEC_COMPATIBILITY.md`, `CHANGELOG.md`, coin config updated in the same
   commits; the diff is grepped for `unwrap_or(`, `::default()`, `TODO` in funds‑moving code.

---

## 9. Decisions and open items

Decisions taken with the maintainer:

- **Ordering (2026‑09‑16):** Step 0 (verify) → Step 1 (Problem A + guard/freeze, released) →
  Step 2 (crate bump) → Step 3 (Problem B).
- **Problem A reporting (2026‑09‑16):** `my_balance` stays numeric; sync failures go through
  `sync_status: Error { reason }`; GUI tickets filed. (Revised from an earlier "`my_balance`
  returns an error" after checking that neither GUI renders it.)
- **`GetBridgeTreeState` (2026‑09‑16):** not adopted — same fallback as `GetTreeState`;
  validation only.
- **3 Oct safety net (2026‑09‑16):** wall‑clock guard from `ironwood_activation_time` — swap
  freeze from `T − max_htlc_locktime`, transaction‑build refusal from `T` — on builds without
  v6 support; receiving keeps working; `lightwalletd` branch ID is a logged secondary signal.
- **Activation‑height source (2026‑09‑16):** derive on demand from block timestamps →
  config override → server branch‑ID sanity check → otherwise refuse.
- **Wallet‑DB policy for 0.21.1 → 0.22.0 (2026‑09‑16):** allow upstream in‑place migration;
  rebuild + rescan only for unknown/old‑Komodo schemas.
- **Freeze scope (2026‑09‑18):** flat coin‑level freeze at `T − 156 000 s` (Correction 1),
  computed at runtime as `get_payment_locktime() * 20`.
- **Step 0 timing (2026‑09‑18):** run it first, funded, before any code change.
- **Round 1 contents (2026‑09‑18):** A1 (tree‑state validation) + A2 (config fields) + a
  `shielded` CI matrix cell.

**Open (to be decided with the wider team):**

- **§5A — which parser reads v6 ARRR transactions in the swap layer:** Option 1 (ZCoin
  overrides on the Zcash crate parser) vs Option 2 (extend `kdf_chain` to v5/v6 with
  ZIP‑244). Affects step 3's size (2–4 vs 4–7 days) and review surface.

Open items depending on Pirate's answers:

- q1 (grace period) ⇒ smaller step 3 and a viable interim; q2 (wire incompatibility) ⇒ a
  Pirate‑specific vendored patch on `zcash_primitives`; q4 (no testnet) ⇒ regtest harness
  only, standardness unproven before mainnet.
- q6 decides whether tip/broadcast/spend‑detection move from Electrum to `lightwalletd`.

## Working rules and log

- Follow `AGENTS.md`: read chapter 39 first; no `unwrap()`/`expect()` in production paths;
  never substitute defaults in funds‑moving code; update the governing chapter in the same
  commit; stage only in‑scope files; do not format or lint vendored trees workspace‑wide.
- Every step ends with the Step 0 script against live servers and a progress‑log entry.

### Progress log

- **2026‑09‑16:** plan drafted; live probes of all configured ARRR endpoints recorded in the
  Overview; independent review applied (swap‑layer v6 parsing, Orchard default flip, GUI
  handling of balance errors, chain‑tip branch ID, boundary behaviour, regtest standardness,
  timeline); ordering and six design decisions fixed with the maintainer; §5A left open for
  team discussion.
- **2026‑09‑17:** plan accepted; committed to `docs/plans/`. Live endpoint probes re-run and
  reproduced unchanged (3 reachable lightwalletd, 2 reachable Electrum; the same 5 + 1 dead).
- **2026‑09‑18:** code survey of the Problem A and safety-net surfaces produced two
  corrections to §3, both recorded inline: the swap-freeze window is ~1.8 days rather than
  ~5 h (Correction 1, and the freeze is now a flat coin-level gate at `T − 156 000 s`), and
  `z_p2sh_spend` must not be guarded because it is the spend/refund path of live swaps
  (Correction 2). Three further findings that shaped the implementation plan: the shielded
  store/scan tests run in no CI job today; `read_commitment_tree` ignores trailing bytes, so
  a bare root beginning `00 00 00` parses silently as an empty tree; and ZCoin's
  `history_sync_status` has no `Error` arm at all, so a failed sync is currently
  indistinguishable from one still catching up. Implementation sequencing for Plan A is
  tracked in the session plan `arrr-ironwood-plan-a.md`.
- **2026‑09‑18 (later):** corrected the coin-config source of truth. Earlier entries cited
  `komodo-coins-rin`, a stale 6-month-old fork; the live upstream is **`GLEECBTC/coins`**
  (`master`, last pushed 2026‑09‑17). Re-checked against it: the ARRR entry there sets
  `requires_notarization: false` / `required_confirmations: 5`, not the `true`/`2` the fork
  carries — Correction 1's supporting claim is amended above, though its 156 000 s conclusion
  is unchanged because the BTC ×10 branch dominates regardless. The dead-endpoint finding is
  **confirmed against upstream**: `light_wallet_d/ARRR` and `electrums/ARRR` there are
  byte-identical to the fork, so the 5 dead lightwalletd and 1 dead Electrum entries are live
  in the list GUIs actually consume. Also noted: **ZOMBIE is absent from upstream `coins`, and
  ARRR is the only ZHTLC coin shipped** — the repo's `zhtlc-native-tests` build ZOMBIE from an
  inline test conf, so they are unaffected, but there is no shipped test ZHTLC coin.
- **2026‑09‑18 — Step 0 executed and passed.** Built `dev` at `d378b804e`
  (release, 24m22s) and ran a full Light-mode round trip against mainnet.
  **Problem A does not reproduce.** Activation via `lightd1.pirate.black` — the **new
  lightwalletd v1.0.0.0** — succeeded in 47 s: `GetTreeState` at height 4135638 returned a
  well-formed tree, 3 001 compact blocks fetched in 6.2 s, wallet scan in 0.7 s. A 0.1 ARRR
  receive (mined 4138653) was detected one block later and reported by
  `z_coin_tx_history` with `received_by_me: 0.1` and `sync_status: Finished`. The funds were
  then swept back out: `task::withdraw::init` + `send_raw_transaction` produced a **v4
  Sapling** transaction (header `04000080`, versionGroupId `0x892F2085`, 2 373 bytes) that
  mined at 4138661 — the pre-activation baseline the Oct-3 guard will later block. Each of
  the three reachable lightwalletd servers was then validated individually
  (`lightd1.pirate.black` 47 s, `electrum1.cipig.net:9447` 23 s, `electrum2.cipig.net:9447`
  23 s); every one rebuilt the wallet database from scratch and re-found the note, which
  also evidences the rebuild-and-rescan recovery path that Step 2's wallet-DB policy relies
  on. 17 scans, zero WARN or ERROR in the shielded path.
- **2026‑09‑18 — defect found by Step 0: `z_coin_tx_history` reports txids in the wrong byte
  order.** `z_coin_wallet_db.rs:1148` does `tx_hash: hex::encode(txid)` on the raw
  `zcash_client_sqlite` `transactions.txid` column, which holds **internal little-endian**
  bytes; every other KDF coin reverses to display order first
  (`utxo_common_history.rs:551`, `tx.hash().reversed()`). Confirmed against mainnet on both
  transactions, and the two RPCs **contradict each other**: `send_raw_transaction` returned
  `23bb6cda…` (found on the explorer) while `z_coin_tx_history` reports the same transaction
  as `0e377444…` (404 — it is the byte reversal). So an ARRR txid from history cannot be
  looked up, and cannot be correlated with the id the send returned. CRD §39.8.3 `:1205`
  says only "Transaction hash, hexadecimal" and does not pin the order, which is how this
  passed review — the same display-vs-little-endian confusion the previous migration hit
  with `TreeState` block IDs. Fix is one call site plus the CRD line; folded into round 1
  because it lives in the same file as A1.
- **2026‑09‑18 — Plan A round 1 landed on `feat/arrr-ironwood`.** Four commits:
  (1) a `shielded` cell in the `unit-tests` matrix — the `z_coin::` module ran in **no**
  CI job before, so its tests defended nothing; (2) the txid display-order fix with the two
  real mainnet IDs as fixtures; (3) tree-state checkpoint validation (R39.8.0ak) plus the
  `saplingFrontier`/`ironwoodTree` proto fields; (4) the optional Ironwood consensus
  parameters (R39.6.4a). 54 shielded tests (was 46), 69 `coins_activation`, native build,
  wasm32 `coins`/`mm2_db`/`mm2_main`, and the `docker_tests` binary all pass.
  Every new rejection test was **verified to fail without its fix** before being kept.
  Re-verified live against mainnet on the patched build: activation `Ok`, `sync_status`
  `Finished`, no shielded WARN/ERROR, and both history transaction IDs now **resolve on
  `explorer.pirate.black`** — `339740530aba…` matching exactly the ID the sending wallet
  reported. A2 deliberately changes no activation lookup: a test pins that
  `BranchId::for_height` still returns `Sapling` at any height even with an Ironwood height
  configured, because the shielded builder derives the transaction version it signs from
  that lookup.
- **2026‑09‑18 — A4, the swap freeze, landed (R39.6.4b), and the chosen margin was
  corrected upward.** The accepted decision was a flat cut-off at `T − 156 000 s`. Writing
  the cross-crate guard test exposed that 156 000 s covers only the *lock* — the swap
  machines then wait `lock + 3700` (`wait_refund_until`) before refunding, and the refund
  still has to be mined. The margin is now **160 300 s** (156 000 lock + 3 700 grace + 600
  ≈ 10 blocks), so a payment made in the last tradeable second is refundable *and confirmed*
  before activation. Cost of the correction: 72 minutes of extra freeze. **ARRR trading now
  pauses 1 Oct 2026 22:28 UTC.**
  Implemented as two overrides because neither alone suffices — `MmCoin::wallet_only`
  (`buy`/`sell`/`setprice`) and `is_coin_protocol_supported` (the incoming peer-match paths,
  which never consult `wallet_only`). The margin has to be a constant in `coins` because the
  locktime rules live in `mm2_main`, which depends on `coins` and not the reverse;
  `payment_locktime_covers_ironwood_freeze_margin` in `mm2_main` fails if they drift.
  Verified live on mainnet across three configured states: **no** activation time → trades
  (reaches the balance check at `ordermatch_trading:1802`); the **real** time 1791054000,
  13 days early → still trades, so shipping the value now breaks nothing; an activation
  inside the window → refused at `:1780` with "Base coin ARRR is wallet only", while
  `my_balance`, `z_coin_tx_history` (`sync_status: Finished`, 2 transactions) and activation
  itself all keep working. `mm2_main`'s 97 pre-existing test failures are missing-passphrase
  environment gates, unchanged from baseline (356→357 passing, the one addition being this
  guard).
