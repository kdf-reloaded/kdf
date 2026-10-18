# Plan: Pirate Chain (ARRR) v6.0 / Ironwood compatibility for KDF Reloaded

**Status:** in progress (steps A–E landed; field testing green-lit 2026-10-18) — **Last reviewed:** 2026-10-18

> **Context:** accepted 2026-09-17; implementation ran through 2026-10-12 (steps A–E) and
> the reorg-recovery fix merged 2026-10-16. The §7 consultation with the Pirate Chain team
> is answered (2026-09-28); §5A is resolved by §10.
> Written to be readable without knowledge of KDF Reloaded internals: the **Overview** is for
> everyone; §1 lists verified facts with sources; §2–§5 are implementation detail for our
> engineers; §6 is the timeline; §7 the open items for the Pirate team; §9 the decisions log.
>
> **Start at §10.** It carries the current plan as of 2026‑10‑11 — one backend
> (`lightwalletd`), Electrum removed, and a bump to a stable v6 release — and says which of
> the earlier sections it supersedes.

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

### What we asked the Pirate team — **answered 2026‑09‑28**

Most of the original questions were **answered from published code** on 2026‑09‑21 (Treasure
Chest, `piratenetwork/librustzcash`, Stashi, Pirate `lightwalletd`) and are recorded in §9
rather than asked: wire/txid/sighash identity, the v6 Sapling digest personalizations, the
`ac_private` P2SH exemption, the activation‑height derivation rule, and whether Sapling
survives v6. The rest went to the Pirate maintainer and came back on **2026‑09‑28**. Short
form; full record with quotations in §7.

| Asked | Answer |
|---|---|
| Grace period for v4 after activation? | **No — not possible.** His advice: *disable ARRR swaps from 2 Oct.* |
| Publish the activation height / expose via `lightwalletd`? | **No.** Timestamp rule only: 3 Oct 19:00 UTC + 60 blocks. Deriving it ourselves is now mandatory. |
| Fix `GetTreeState` returning a bare `finalRoot`? | **No — intentional legacy.** Client must not let it overwrite local `finalState`. Our validation is the only protection. |
| Current endpoints? `piratedBuild`? | **List supplied** (§1.1); the shared coin list is stale. `piratedBuild` not addressed. |
| Public Ironwood testnet? | **Live now** — `testlightwalletd1/2.cryptoforge.cc`, Ironwood already active. Perishable. |
| cipig ElectrumX v6 ready? | **Unknown to Pirate** — ask cipig directly. Still open. |
| Compact format: field 6 or `ironwoodActions = 9`? | **Field 6.** Upstream's Ironwood scanner cannot be used against Pirate unmodified. |
| Sapling deprecation? | **Not soon;** ≥1 yr notice, first step would be a one‑way pool. Currently open both ways. |
| 19 Sep `requiredSigs` relevant? | **No.** |

**What this changes:** the swap freeze is vindicated by the chain's own maintainer and our
cut‑off is the conservative one; runtime height derivation is promoted from fallback to
required; the tree‑state validation is permanent rather than a stopgap; and there is a live
Ironwood network to test against, today, that will not necessarily be there next week.

---

## 1. Verified facts and where they come from

All external facts were read from source on 2026‑09‑16; nothing is inferred from
announcements alone.

### 1.1 Infrastructure, probed 2026‑09‑28

Supplied by the Pirate maintainer (§7 item 4/5) and verified by direct connection the same
day. **The shared coin list is stale**: `light_wallet_d/ARRR` in `GLEECBTC/coins` still lists
four `piratelightd*.cryptoforge.cc` hosts that no longer resolve, plus
`electrum3.cipig.net:9447` which refuses connections.

| Endpoint | Probe result (2026‑09‑28) |
|---|---|
| `testlightwalletd1.cryptoforge.cc:443` | **`branchId=37a5165b` — Ironwood ACTIVE**, tip 53 041, `v1.0.0.0` |
| `testlightwalletd2.cryptoforge.cc:443` | **`branchId=37a5165b` — Ironwood ACTIVE**, tip 53 041, `v1.0.0.0` |
| `lightd1.pirate.black:443` | up, `branchId=76b809bb` (pre‑Ironwood), tip 4 153 707, `v1.0.0.0` |
| `lightwalletd1.cryptoforge.cc:443`, `lightwalletd2.cryptoforge.cc:443` | TCP open; gRPC probe timed out at 40 s (not concluded dead) |
| `pirate.mathnodes.com:443`, `arrr{,2,3}.qortal.link:443` | TCP open; gRPC probe timed out at 40 s |
| `electrum1.cipig.net:9447`, `electrum2.cipig.net:9447` | TCP open |
| `electrum3.cipig.net:9447` | **connection refused** |
| `piratelightd1‑4.cryptoforge.cc:443` | **no DNS** — renamed to `lightwalletd1‑2`, and four hosts became two |

The testnet result is the important one: **Ironwood is already active on Pirate testnet**, so
the v4‑rejected / v6‑accepted behaviour can be exercised there before mainnet. Per the
maintainer those servers are perishable ("no idea how long they will stay up").

### 1.2 Protocol facts

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
   the canonical list (§7 item 4).
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

> **Superseded in part by §10.4 step B (2026‑10‑11).** The target versions and the ordering
> here are out of date: v6 reading requires `zcash_primitives 0.30.1`, and the bump is now a
> hard prerequisite rather than a parallel track. The factual content below stands.

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

> **Superseded in part by §10.4 steps C–E (2026‑10‑11).** This section assumes Electrum
> remains a backend; it is being removed for ZHTLC coins. §5A's open design decision is
> resolved in favour of the Zcash parser (§10.4 step D).

Definition: *every ARRR transaction KDF Reloaded builds for a height at or after Ironwood
activation is a version‑6 transaction committing to branch ID `0x37a5165b`; before activation
it stays exactly what we build today; and every swap step that reads ARRR transaction bytes
understands v6 and its ZIP‑244 txid.*

**Scope, settled 2026‑09‑21 (§9).** This step needs the v6 **wire format**, the ZIP‑244 v6
digests and the `Nu6_3` branch ID — and nothing else. Sapling is a first‑class field of
Pirate's v6 transaction, so we keep building Sapling bundles with the key, note, witness and
commitment‑tree machinery we already have. The Ironwood **pool** is out of scope: no halo2
proving, no Orchard circuit, no `pirate1…` addresses, no ZIP‑32 Ironwood derivation, no
second commitment tree, no second sync pool. Every piece we do need is already implemented in
upstream `librustzcash`, and is compiled out of our build today only because Ironwood sits
behind the `orchard` feature we do not enable.

1. **Consensus parameters.** Map `NetworkUpgrade::Nu6_3` to `ironwood_activation_height` in
   `ZcoinConsensusParams::activation_height`. With that single arm the upstream builder
   selects `TxVersion::V6`, the Ironwood branch ID, ZIP‑244 txids and ZIP‑229 Sapling digests
   by itself; `Transaction::read` parses v6 natively. Plumbing note: the height is learned at
   runtime but `ZcoinConsensusParams` is a plain `Clone + Serialize` value copied into every
   builder and into `WalletDb<_, ZcoinConsensusParams, _, _>` — hold it in an
   `Arc<AtomicU32>` marked `#[serde(skip)]` (0 = unknown), or rebuild the params per build.
2. **Activation‑height discovery at runtime** — **mandatory as of 2026‑09‑28**: the Pirate
   team will not publish the height and offered no `LightdInfo` field (§7 item 1), so this is
   the only route by which a light client can learn it. (Layered; ordered so that
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
   `ironwood_activation_height` as soon as the network derives it (§7 item 1); `txversion` stays 4.
7. **Testing.** ZOMBIE runs Komodo `komodod` and will **not** get Ironwood. Build a docker
   harness with `pirated -regtest` (Ironwood fixed at height 200) + Pirate `lightwalletd`:
   mine past 200, activate the KDF light wallet, receive, then send/refund an HTLC and a
   withdrawal. **Regtest has `fRequireStandard = false`, so it proves consensus validity
   only** — the "v4 rejected, v6 accepted" case needs a standardness‑enforcing network.
   **As of 2026‑09‑28 that exists and is reachable:** `testlightwalletd1/2.cryptoforge.cc`
   report `branchId=37a5165b` (Ironwood active) at tip 53 041 (§1.1). Use it in preference to
   the regtest harness, and use it soon — the maintainer does not guarantee it stays up.
   Then Step 0 on mainnet after 3 Oct.

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
ZIP‑244 txids (§7 item 6) — otherwise spend detection is impossible on Electrum regardless of our
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
exists and matches Pirate by construction — confirmed 2026‑09‑21, see §9; no consensus‑critical hashing written by
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

**That exceeds the window, and the window has now closed** — this entry is written on
2026‑09‑28 with activation 5.2 days away and step 2 not started. ARRR will lose send/swap
capability on 3 Oct and regain it when steps 2–3 land. That was the anticipated outcome, and
the plan is built so that this slip is safe rather than fast:

- The **guard and swap freeze ship in step 1**, before any dependency change. Whatever
  happens to steps 2–3, a step‑1 build stops opening ARRR swaps ahead of activation and stops
  building ARRR transactions at activation, with clear messages; receiving and balance display
  keep working (Sapling notes are unaffected by Ironwood). Users lose nothing; they wait for
  the next release to trade ARRR again.
- There is **no useful shortcut around step 2**, and as of 2026‑09‑28 the one hypothetical
  escape is closed. Patching `Nu6_3` into the old `zcash_protocol 0.9.0` would give us the
  branch ID but the old `zcash_primitives` would still emit v4 (or, if `Nu6_2` were reused, a
  v5 header Pirate does not accept), which is non‑standard after activation. That was only
  survivable if Pirate relaxed `IRONWOOD_MIN_CURRENT_VERSION` to 4; **they have refused a
  grace period outright** (§7 item 2), so the ~40‑line vendored‑patch interim is dead and
  step 2 is unavoidable.
- Any KDF build (ours or Komodo's) that is not upgraded will be unable to send or swap ARRR
  after activation.
- **The freeze only protects deployments whose coin configuration declares
  `ironwood_activation_time`.** No published configuration does, so as things stand the safety
  net is inert everywhere but our own development setup — see the reopened decision in §9,
  which must be settled by **1 Oct 22:28 UTC**.

---

## 7. Open items for the Pirate Chain team — **answered 2026‑09‑28**

The list below was put to the Pirate Chain maintainer (Øswald) and answered on
**2026‑09‑28**. Answers are recorded verbatim in substance, each with what it means for us.
Everything that the published code already settled is in §9 and was not asked again.

**Headline outcomes:** there will be **no grace period** and **no published activation
height**; the maintainer's own recommendation is to **disable ARRR swaps from 2 Oct** until a
v6‑capable build ships. Sapling is safe for the long term. An **Ironwood‑activated testnet
with `lightwalletd` is live now**. The `GetTreeState` behaviour is intentional and will not
change. The compact‑format divergence resolves in favour of field 6.

### 7.1 Requests

1. **Publish the activation height / expose it via `lightwalletd`** — *not offered.* The
   answer restated the rule only: activation is **timestamp‑based**, 3 Oct 2026 19:00 UTC
   **+ 60 blocks** ("the 60th block that shows up after 19:00 UTC"). No height will be
   published ahead of time and no `LightdInfo` field was offered.
   **Consequence: the runtime derivation in §5 step 3.2 is mandatory, not a fallback.** It is
   now the only way a light client can learn the height. `ironwood_activation_height` in the
   coin config remains useful only as an operator override after the fact.
2. **A standardness grace period** — **refused, and not possible.** Verbatim: *"Not possible
   unfortunately, would recommend disabling ARRR swaps on OCT 2 till the update is ready."*
   This is an independent confirmation of the R39.6.4b swap freeze from the chain's own
   maintainer, and his date (2 Oct) is *later* than our cut‑off (1 Oct 22:28 UTC), so our
   margin is the conservative one. It also means the post‑activation failure is hard: every
   v4 transaction becomes non‑standard at the activation block with no taper.
3. **`GetTreeState` returning a bare `finalRoot`** — **intentional legacy behaviour; it will
   not be changed.** The maintainer hit the same problem building Stashi and gave the
   client‑side rule: *"you should prevent finalroot from overwriting local finalstate when
   finalstate is not available as that would cause anchor mismatches. It is intentional legacy
   behavior, but it is confusing because it is serialized as a tree state."*
   This is exactly what R39.8.0ak does. **No server‑side fix is coming, so client‑side
   validation is the only protection** — ours stays load‑bearing, and every other light client
   remains exposed.
4. **Endpoint list** — **supplied** (see §1 for the reachability probe). Pirate team
   `lightwalletd`: `lightwalletd1.cryptoforge.cc:443`, `lightwalletd2.cryptoforge.cc:443`,
   `pirate.mathnodes.com:443`, `lightd1.pirate.black:443`, plus two I2P and two Onion
   addresses. Third‑party: `arrr.qortal.link:443`, `arrr2.qortal.link:443`,
   `arrr3.qortal.link:443`. Explorers: `explorer1/2.cryptoforge.cc`,
   `explorer.piratechain.com`. On cipig's servers: *"I think Cipi also runs a few servers but
   idk if they are updated, better to ask him directly."* **`LightdInfo.piratedBuild` was not
   addressed**, so it stays empty and unusable as an upgrade signal.
   Note the rename: the four `piratelightd1‑4.cryptoforge.cc` entries in the shared coin list
   are gone (no DNS) and are replaced by two `lightwalletd1‑2.cryptoforge.cc`.
5. **A public Ironwood testnet** — **live now.** `testlightwalletd1.cryptoforge.cc` and
   `testlightwalletd2.cryptoforge.cc`, with explorers `testexplorer1/2.cryptoforge.cc`. Both
   verified reachable on 2026‑09‑28 reporting **`branchId=37a5165b` (Nu6_3/Ironwood active)**
   at tip 53 041 — i.e. Ironwood is already activated there, which is precisely the
   environment needed to prove the v4‑rejected / v6‑accepted behaviour before mainnet.
   **Caveat, in his words:** *"No idea how long they will stay up or if Forge has the test
   node still mining."* Treat it as perishable and use it immediately.

### 7.2 Facts we could not determine ourselves

6. **cipig's ElectrumX v6 readiness** — **to be asked directly of cipig**; the Pirate team
   does not know. Still open, and still on the critical path: those servers are our chain
   tip, broadcast and spend‑detection path. (`electrum3.cipig.net:9447` is confirmed refusing
   connections; `electrum1`/`electrum2` are up.)
7. **The compact‑format divergence** — **resolved in favour of field 6.** Verbatim: *"We
   don't use the librustzcash scanner in neither the node nor in stashi, for both we use
   field 6, I suggest just using Stashi's scanner for KDF."*
   So Pirate's wire truth is `actions = 6` carrying Ironwood actions, and upstream
   librustzcash's `ironwoodActions = 9` is **not** what Pirate emits. Harmless for us today —
   we are Sapling‑only and read `spends`/`outputs`, never `actions`. But it is a hard
   constraint on any future Ironwood support: **upstream's Ironwood scanner cannot be used
   against Pirate unmodified**, and the field number would have to be patched. His suggestion
   to adopt Stashi's scanner was assessed and declined for the reasons in §9 (it comes
   attached to a whole alternative storage and sync stack).
8. **Sapling longevity** — **confirmed safe.** Verbatim: *"Sapling won't be depreciated
   anytime soon, and even if we decide that, we would probably have to give over 1 yr notice
   and it would start by making the pool one way only i.e. no ironwood to sapling, but as is
   right now its open both ways."* Our Sapling‑only swap protocol is therefore sound for the
   foreseeable future, we would get ≥1 year of notice, and the first signal would be the pool
   becoming one‑way — something we can watch for rather than be surprised by.
9. **The 19 Sep `requiredSigs` dPoW change** — **confirmed irrelevant** to light clients.

### 7.3 Left with them / left with us

- **Unanswered:** populating `LightdInfo.piratedBuild` (item 4), and any means of learning the
  activation height other than deriving it (item 1).
- **Ours to do:** ask cipig about v6 (item 6); exercise the Ironwood testnet while it is up
  (item 5); arm the swap freeze before 1–2 Oct (§9).
- **A question back to us:** the maintainer asked what distinguishes this project from the
  KMDCL KDF and why we do not work on that instead. That is the maintainer's to answer, not
  a technical item; noted here so it is not lost.

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
- **A5 dropped (2026‑09‑21):** updating ARRR's entry in `GLEECBTC/coins` is the Pirate
  team's call, not ours; we do not edit another project's coin definition.
- **The beta ships with both gates dormant (2026‑09‑21).** `ironwood_activation_time` and
  `ironwood_activation_height` are **our own invention**: no coin
  in `GLEECBTC/coins` mentions Ironwood — ARRR is the only coin there carrying
  `consensus_params` at all. Since both A3 and A4 read `ironwood_activation_time` and both
  return `false` when it is absent, neither gate fires on a default installation. Accepted
  knowingly, for three reasons: the project is in alpha/beta under a standing warning and can
  afford ARRR being unusable for a period; asking GLEEC to carry a schema extension only this
  fork reads would be presumptuous while we cannot yet transact v6 at all; and — decisively —
  a compiled‑in activation timestamp is *worse than none* if Pirate slips the date, because
  it would refuse every ARRR transaction and freeze the market until a new binary shipped,
  whereas configuration keeps a moved date a data change. The mechanism is built, tested and
  arms itself the moment any configuration supplies the timestamp. Residual exposure is
  narrow: withdrawals merely fail with a less clear error, but a swap funded shortly before
  activation could be left unable to spend *or* refund its HTLC. Recorded in `CHANGELOG.md`,
  `RELOADED_VS_GLEEC.md` and CRD R39.6.4a, which now also forbids a per‑ticker compiled‑in
  default.

- **Stashi Wallet assessed; its crates are not adoptable, and are not needed (2026‑09‑21).**
  Investigated `PirateNetwork/Stashi-Wallet` at the Pirate team's suggestion. Three findings,
  all from published code:
  - *Stashi's own crates are the wrong layer.* It is a 16‑crate, ~120 k‑line application
    workspace (MIT) with its **own** storage and sync engines — `pirate-storage-sqlite`
    (27 897 lines) replaces `zcash_client_sqlite`, which Stashi does not depend on at all,
    and `pirate-sync-lightd` (35 661 lines) replaces the whole lightwalletd sync path.
    Adopting it means deleting `z_coin` and rewriting against a foreign storage model with no
    swap/HTLC layer in it — not a wrapper crate, a different product.
  - *Ironwood is an upstream Zcash upgrade (NU6.3), not a Pirate invention.* Upstream
    `zcash/librustzcash` implements it in full: `BranchId::Nu6_3 => 0x37a5_165b` (Pirate's
    branch ID verbatim), `BranchId::Nu6_3 => TxVersion::V6`, and the Ironwood pool across 116
    files. **`piratenetwork/librustzcash` is 2 commits ahead of upstream** (3 files, +16
    lines), one of which is only a Cargo patch table. The entire Pirate‑specific delta in the
    whole stack is `sapling-crypto` (3 commits / 2 files / +18) and `orchard` (1 commit /
    1 file / +33).
  - *The one substantive Pirate patch is the ZIP‑212 lead‑byte rule, which we already solved.*
    Their `plaintext_version_is_valid` ignores enforcement and accepts `0x01 || 0x02`
    unconditionally — the same semantics as our `ZcoinDecryptionParams` (R39.8.0am), reached
    independently from mainnet evidence. Ours is the safer of the two: theirs is global, so
    their *builder* also accepts both, whereas ours is confined to decryption with a test
    pinning that real params still resolve to Sapling. Note that Stashi's own
    `PirateNetwork::activation_height` returns `None` for Canopy exactly as ours does — they
    needed the library patch for precisely the reason we needed the parameter split.
  Conclusion: **the target is upstream `librustzcash`, not Pirate's fork and not Stashi.**
  No wrapper crate. `[patch.crates-io]` cannot bridge a version gap anyway (patching
  `=0.23.0` with `0.24.0-rc.1` is refused), so step 2 must bump the pins regardless.
- **Sapling survives inside v6; the Ironwood pool is out of scope (2026‑09‑21).** This was
  §7's largest open question and it is answered by Pirate's own serializer
  (`src/primitives/transaction.h:722‑760`): a v6 transaction is `branchId, lockTime,
  expiryHeight, vin, vout, saplingBundle, <empty Orchard slot>, ironwoodBundle`. Sapling is a
  first‑class field, no turnstile touches it, and no consensus rule requires an Ironwood
  bundle. Corroborated four ways: `TxVersion::V6.has_sapling() == true` upstream; release
  notes 6.0.4 default coincontrol `"type"` to `"sapling"` "for backward compatibility";
  Stashi's builder dispatches purely on address prefix (`pirate1…` → Ironwood, else Sapling);
  and the public record has the turnstile as Orchard→Ironwood, a pool Pirate never activated
  (ZIP 2005 is quantum *recoverability*, opt‑in). **Step 3 therefore needs only the v6 wire
  format, ZIP‑244 v6 digests and the `Nu6_3` branch ID — all already upstream — while we keep
  building Sapling bundles.** Dropped entirely: halo2 proving, the Orchard circuit,
  `pirate1…` addresses, ZIP‑32 Ironwood derivation, a second commitment tree, a second sync
  pool, and the `PirateNetwork/halo2` and `PirateNetwork/orchard` forks.
- **Wire and digest identity confirmed by construction (2026‑09‑21).** Treasure Chest computes
  its own txids and sighashes with the *same* `piratenetwork/librustzcash` rev `cc3c11cf`
  that we would build against, and neither of that fork's two commits touches txid or
  sighash. So the v6 serialization, ZIP‑244 txid and Sapling v6 digest personalizations
  (`ZTxIdSSpendNH_v6`, `ZTxAuthSapliH_v6`) are identical because they are *the same code*.
  The empty‑slot encoding is likewise settled: `write_bundle(None, …)` emits
  `CompactSize::write(0)` — a single `0x00` — for each of the Orchard and Ironwood slots,
  exactly what Pirate's `READWRITE(COMPACTSIZE(nOrchardSlotActions))` expects. And the
  `ac_private` exemption our swap layer depends on (Sapling → P2SH with the redeem script
  revealed, `src/main.cpp:1740‑1752`) lives in `CheckTransaction` with **no version gating**,
  so it applies to v6 unchanged.
- **The newer backend does not drag Ironwood into our build (2026‑09‑21).** In
  `zcash_client_backend`, every Ironwood scanning path — keys, nullifiers, domains — is behind
  `#[cfg(feature = "orchard")]`, and the `sync` module that probes Ironwood subtree roots is
  behind `sync`/`sync-decryptor`. We build with `default-features = false` and neither
  feature, so Ironwood is compiled out entirely.

- **The Pirate maintainer answered §7 on 2026‑09‑28; four answers change the plan.**
  Full record in §7. The load‑bearing ones:
  - *No grace period, and the maintainer's own advice is to stop ARRR swaps from 2 Oct.*
    "Not possible unfortunately, would recommend disabling ARRR swaps on OCT 2 till the update
    is ready." This independently confirms R39.6.4b from the chain's own maintainer, and our
    cut‑off (1 Oct 22:28 UTC) is the more conservative of the two. **It also makes the dormant
    gates a live problem — see the reopened decision below.**
  - *No activation height will be published, and no `LightdInfo` field was offered.* The
    runtime derivation in §5 step 3.2 is therefore **mandatory infrastructure, not a
    fallback**; it is the only route by which a light client can learn the height.
    `ironwood_activation_height` in the coin config survives only as an after‑the‑fact
    operator override.
  - *The `GetTreeState` bare‑root behaviour is intentional and will not be fixed server‑side.*
    The maintainer independently arrived at our rule — do not let `finalRoot` overwrite a local
    `finalState`, because it causes anchor mismatches — having hit it while building Stashi.
    R39.8.0ak is therefore permanent load‑bearing validation, not a workaround awaiting a fix.
  - *Sapling is safe long term*: no deprecation soon, ≥1 year of notice if ever, and the first
    step would be making the pool one‑way (no Ironwood→Sapling) — currently it is open both
    ways. Our Sapling‑only swap protocol is sound, and we have a specific signal to watch.
  Also settled: the compact‑format divergence resolves **in favour of field 6** ("we don't use
  the librustzcash scanner in neither the node nor in stashi, for both we use field 6"), which
  means upstream's Ironwood scanner cannot be used against Pirate unmodified should we ever
  add Ironwood support; and the 19 Sep `requiredSigs` change is confirmed irrelevant to light
  clients.
- **An Ironwood‑activated testnet exists and is perishable (2026‑09‑28).** `testlightwalletd1/
  2.cryptoforge.cc` both report `branchId=37a5165b` at tip 53 041 — Ironwood already active.
  This is the only environment in which the v4‑rejected / v6‑accepted mempool behaviour can be
  proven before mainnet, and the maintainer does not guarantee it stays up. It supersedes the
  regtest‑only acceptance path in §5 step 3.7, which could never show standardness rejection
  (`fRequireStandard = false`).

**Open (to be decided with the wider team):**

- **REOPENED — arming the swap freeze before 2 Oct.** On 2026‑09‑21 we shipped R39.6.4b/c
  dormant, because `ironwood_activation_time` is absent from every published coin
  configuration and because a compiled‑in date would be *worse* than none if Pirate slipped
  the upgrade. **The second premise has now weakened:** the maintainer restated 3 Oct
  19:00 UTC + 60 blocks as fixed, five days out, and explicitly recommends disabling ARRR
  swaps from 2 Oct. Meanwhile the freeze cannot fire on any deployment whose coin
  configuration omits the field, and `GLEECBTC/coins` still omits it. The local
  `~/.kdf/coins` used for development now carries `1791054000`, which covers our own testing
  and nothing else. Decision needed on how third‑party deployments get gated — a coins‑repo
  entry (not ours to make), a built‑in default with an explicit opt‑out (reverses a documented
  compat switch), or an operator instruction to disable ARRR manually. **Deadline is
  1 Oct 22:28 UTC**, ~3 days from this entry.

- **§5A — which parser reads v6 ARRR transactions in the swap layer:** Option 1 (ZCoin
  overrides on the Zcash crate parser) vs Option 2 (extend `kdf_chain` to v5/v6 with
  ZIP‑244). Affects step 3's size (2–4 vs 4–7 days) and review surface.

Open items depending on Pirate's answers:

- q1 (grace period) ⇒ smaller step 3 and a viable interim; q2 (wire incompatibility) ⇒ a
  Pirate‑specific vendored patch on `zcash_primitives`; q4 (no testnet) ⇒ regtest harness
  only, standardness unproven before mainnet.
- q6 decides whether tip/broadcast/spend‑detection move from Electrum to `lightwalletd`.

## 10. Revised plan (2026‑10‑11): one backend, stable v6

**Sections 4 and 5 are superseded in part by this section.** Their step ordering and their
assumption that Electrum remains a backend no longer hold; their factual content stands.

### 10.1 Why the plan changed

Three things arrived after activation, in this order:

1. **The Electrum provider disappeared.** cipig's Pirate node never upgraded past Ironwood: on
   2026‑10‑08 his `arrr.electrum1/2.cipig.net` ElectrumX *and* his
   `electrum1/2.cipig.net:9447` lightwalletd all reported tip 4 161 184 with
   `branchId=76b809bb`, against a real chain at ~4 167 700 and `37a5165b`. Both froze at the
   first post‑activation block containing transactions. A probe of all ten Pirate‑official
   hosts found **no ElectrumX anywhere**: they serve lightwalletd or HTTP only. So the
   Electrum dependency has no healthy provider and nothing to wait for.
2. **The chain's maintainer said the dependency is wrong in principle:** *"Pirate needs
   lightwalletd not electrum, because of zk-snarks/zcash requires it."*
3. **`lightwalletd` turns out to serve the transparent side too** — verified in §10.2, not
   merely declared.

### 10.2 Step A — the transparent surface, verified against the live network (done)

Probed 2026‑10‑11. The method that distinguishes "works" from "always returns empty" is
comparing an address with real history against a synthetic one: `RM6uyifBAuowR7LKyzkdSmsjzAUBvGTT1p`
(the test chain's miner address, recovered from 300 input `scriptSig` pubkeys in the local
fixture corpus) versus an unused address of the same form.

| Function | RPC | Verdict |
|---|---|---|
| chain tip | `GetLatestBlock` | **works** |
| compact blocks, block time | `GetBlock` / `GetBlockRange` | **works** |
| raw transaction, mined height | `GetTransaction` (by `hash`) | **works** |
| commitment‑tree anchor | `GetTreeState` | **works** |
| mempool | `GetMempoolTx` | **works** |
| consensus branch id | `GetLightdInfo` | **works** |
| transparent history (spend detection) | `GetTaddressTxids` | **works** — streamed 8 661 transactions, ~1 MB, for the real address; empty for the control |
| transparent UTXOs and balance | `GetAddressUtxos` | **works** — one UTXO returned for the real address; empty for the control |
| broadcast | `SendTransaction` | **declared, untested** — needs a valid signed transaction |
| transparent balance (direct) | `GetTaddressBalance` | **works** — see the correction below; several addresses per call, reply carries only `valueZat` |
| fee rate | — | not needed; ARRR declares a fixed per‑kB fee |

**Corrected 2026‑10‑11 after reading Pirate's published source.** Two of the three findings
first recorded here were wrong, in opposite directions — one blamed Pirate for our own defect,
the other understated a real one. Both corrections are kept visible rather than silently
rewritten, because the first is the kind of mistake worth not repeating.

- **`GetTaddressBalance` works; the failure was ours.** It was reported here as returning
  `-5: Invalid addresses` for every address. The probe encoded `AddressList` as field 1
  *containing* a field‑1 submessage, where `repeated string addresses = 1` requires a flat
  `0a <len> <addr>` per address. With the correct encoding the same address on the same server
  returns `grpc-status 0` and a balance of 17 935 616.0014 ARRR, verified directly. Nothing to
  report to Pirate, and no workaround is needed. The lesson generalises: a gRPC error carrying a
  *daemon* error code (`-5`) is evidence the request reached the node, which should have
  prompted suspicion of the request rather than of the server.
- **P2SH lookups are broken by a Pirate daemon defect**, not merely unproven. The transparent
  index does store P2SH rows, but under the wrong key type, so a `b…` query can never match
  them. `GetAddressType` (`src/main.cpp:3378‑3403`) guards on
  `vDest.index() != std::variant_npos`, which for a `std::variant` is always true, so the
  `TX_SCRIPTHASH → keyType = 2` branch is dead code and every indexed output is written as
  type 1; `CBitcoinAddress::GetIndexKey` (`src/base58.cpp:298‑312`) resolves a
  `SCRIPT_ADDRESS` to type 2, and the type is the first byte of the LevelDB key, so the two are
  disjoint ranges. It reads as a `boost::variant` → `std::variant` port regression — the
  correct form survives for the input path in `src/txmempool.cpp:190‑192`. **So
  `getaddresstxids` / `getaddressutxos` / `getaddressbalance` are empty forever for the HTLC's
  address, however much activity it has**, and the earlier "accepted, returned empty" probe was
  not evidence of coverage.

  *Workaround, source‑derived and unverified at runtime:* query the redeem script's `hash160`
  re‑encoded as an **R‑address** (version byte 60), which hashes to the same twenty bytes under
  type 1, where the rows actually are. Two constraints follow: the implementation must prove
  this on first use rather than trust it, and it must probe **both** encodings, because the
  stored type depends on which binary indexed the block — a datadir indexed by an older
  boost‑variant build may hold P2SH under type 2. It must fail loudly rather than report "not
  spent" if neither resolves.
- **`GetTaddressTxids`' mid‑stream failure is fully explained**, and is neither a cap nor a
  range limit — there are none. The daemon cannot serve a transaction that is fully spent or
  has no transparent output when the tx index is off (`src/main.cpp:2631‑2696`), and
  `lightwalletd` returns on the first such failure (`frontend/service.go:208`), ending the
  stream. It also buffers a height group and flushes only on seeing a higher height
  (`service.go:212‑224`), so the last group is always lost — which is exactly why 8 662
  messages became 8 661 delivered. Only seven 1 000‑block windows of the whole test chain are
  affected. Four rules follow: request in bounded windows; on that error bisect and skip the
  offending height rather than failing the sync; resume from `last_received_height` and **not**
  `+ 1`; and always send `start >= 1` and `end >= 1`, because the daemon applies the range only
  when both exceed zero (`src/rpc/misc.cpp:1466‑1474`) and `start = 0` silently means the whole
  chain.
- **`TxFilter{block, index}` is not served**; only `TxFilter{hash}`, and the txid must be in
  **internal** (compact‑block) byte order — display order errors.

The transparent index is also **opt‑in**: `-addressindex=1` is required and switching it on an
existing datadir forces a reindex. Both probed servers have it enabled, and it can be detected
from outside — with the index off, `GetTaddressBalance` returns
`-5: No information available for address` rather than a balance.

One consensus fact found while reading, and it is in our favour: on a private chain a
non‑coinbase transparent output is legal in exactly one shape —
`vin` empty, `vout.size() == 2`, `vout[0]` pay‑to‑script‑hash and `vout[0].IsRedeemScriptReveal(vout[1])`
(`src/main.cpp:1697‑1711`, `1740‑1752`) — and `IsRedeemScriptReveal`
(`src/script/script.cpp:259‑284`) is hard‑coded to the 110‑byte atomic‑swap redeem‑script
shape. The HTLC is therefore a consensus‑blessed special case whose shape cannot drift, which
is also why a second implementation cannot diverge from ours on it.

### 10.3 Maintainer decisions (2026‑10‑11)

- **Electrum support is removed for ZHTLC coins, not retained behind a switch.** There is no
  need for it and no working provider. Recorded as a deliberate divergence from GLEEC KDF with
  the documentation the compatibility rule requires (§10.5).
- **The crate bump targets stable v6**, not a prerelease and not a git dependency.

### 10.4 Steps

**B — bump the Zcash stack to published stable releases.** A hard prerequisite for everything
else, because v6 *reading* does not work today: in the pinned `zcash_primitives 0.28.0` the v6
arm of `TxVersion::read` sits behind `#[cfg(zcash_unstable = "nu7")]`, a rustc cfg we do not
set, so a v6 transaction is rejected as "Unknown transaction format". It is **unconditional in
0.30.1**, which is published and stable.

| Crate | Pinned now | Target | Note |
|---|---|---|---|
| `zcash_primitives` | 0.28.0 | **0.30.1** | v6 unconditional; **vendored and patched** |
| `zcash_transparent` | 0.8.0 | **0.10.0** | **vendored and patched**; what 0.30.1 requires |
| `zcash_client_backend` | 0.23.0 | 0.24.0 | keep `orchard` off; vendor copy droppable |
| `zcash_client_sqlite` | 0.21.1 | 0.22.0 | wallet‑DB migration |
| `zcash_protocol` | 0.9.0 | 0.10.6 | carries `BranchId::Nu6_3` |
| `sapling-crypto` | 0.7.0 | **0.7.0 — unchanged** | the whole 0.30.1 stack needs `sapling-crypto 0.7` |

> **This table was wrong in three ways, corrected 2026‑10‑12 from the implementation.**
> Three of these crates are not version pins at all but **vendored source copies we patch**
> under `vendor-patches/`, so this step is a re‑vendor onto pristine upstream sources with each
> KDF extension re‑applied, not a version bump. `zcash_transparent` was **missing from the
> table** although we vendor and patch it. And `sapling-crypto 0.9.0` **would not resolve** —
> primitives 0.30.1, `zcash_client_backend` 0.24.0 and `zcash_client_sqlite` 0.22.0 all require
> `sapling-crypto 0.7`.

These are **newer than the Pirate node's own stack**, which pins `zcash_primitives 0.29.0` plus
two `-rc.1` prereleases via git. We take published releases and keep our own ZIP‑212 handling,
which is narrower than the fork's: theirs patches `plaintext_version_is_valid` globally, so
their builder also accepts both lead bytes, whereas R39.8.0am confines the behaviour to
decryption with a test pinning that transaction construction still resolves to Sapling. Re‑check
that the fix is still required after the bump; the upstream Canopy rule is unchanged, so expect
it to be.

**C — `lightwalletd` becomes the only backend.** Move every function in §10.2 off Electrum;
make the Electrum server list unused for ZHTLC; give all shielded construction paths **one**
height authority, which is what fixes the refund defect; add a `GetLightdInfo`
consensus‑branch‑id check that **excludes** off‑branch servers rather than reporting them. With
a single backend the two‑backend divergence tolerance largely dissolves and the branch‑id check
replaces it — three of the eight published `light_wallet_d_servers` entries are cipig's and
serve a different fork, and an off‑branch server above the wallet's anchor can otherwise make
the wallet rewind and follow that server's chain.

> **Step C's branch‑id check inverts if written naively, and this section originally ordered it
> wrongly.** "Expected branch id" has to be relative to something. Derived from the coin's
> configured network‑upgrade heights — the obvious reading — the expectation at today's ARRR
> heights is the **pre‑upgrade** value, because `ironwood_activation_height` is optional
> (R39.6.4a), no published configuration carries it, and a test pins that the branch lookup
> still returns Sapling at any height. A naive check would therefore have **admitted the stale
> fork and excluded every healthy endpoint** — the exact inverse of the intent, shipped as a
> safety feature. CRD `R39.11.7a` is written to be correct in both configuration states and
> `T39.11.f` pins the inversion case, so the ordering hazard is closed by the requirement rather
> than by sequencing C after E. **Carry this into the Coder prompt**; it is the single most
> likely way to implement this step backwards.

> **Step C's one hard dependency is the P2SH defect in §10.2.** Spend detection for the swap
> HTLC rests on a source‑derived, runtime‑unverified workaround (query the redeem script's
> `hash160` as an R‑address). If that workaround does not hold, the swap half of this step has
> no indexed way to find an HTLC and would need either a fix released by Pirate or a different
> detection strategy — scanning blocks for the known 110‑byte redeem‑script shape is the
> obvious fallback, since consensus guarantees it is revealed in `vout[1]`. **Prove the
> workaround on a funded HTLC before building on it.** The wallet half of this step — tip,
> broadcast, raw transactions, balance, history, mempool, anchor — has no such dependency and
> can proceed regardless.

> **A related risk, measured rather than left open.** An HTLC spend is exactly the transaction
> shape a node without a transaction index cannot serve — a transparent P2SH input in, a
> shielded output out, so no transparent output — which would hide the `scriptSig` carrying the
> swap secret, and a compact block cannot substitute because it carries no `scriptSig`. Probing
> the provider set on 2026‑10‑11 with a fully shielded mainnet transaction (which only the
> transaction index can serve) resolved it on **six of seven** endpoints:
> `lightwalletd1`/`lightwalletd2.cryptoforge.cc`, `pirate.mathnodes.com` and
> `arrr`/`arrr2`/`arrr3.qortal.link`. So this is a reportable condition, not a blocker. The
> seventh, `lightd1.pirate.black`, returned an nginx **502** for that method while answering
> `GetLightdInfo` and `GetLatestBlock` normally — which is its own constraint: **endpoint
> admission cannot rest on one probe**, because an endpoint can pass admission and then fail
> the methods actually needed. Recorded as CRD `D39.11.g`.

**D — route ARRR's transaction parsing to the Zcash parser.** The shared UTXO parser applies
the v4 layout with double‑SHA256 txids. It cannot simply be deleted: `UtxoTx` is referenced in
48 files and is the transaction type for every UTXO coin. `zcash_primitives::Transaction::read`
handles Sprout, v3, v4, v5 and v6 in one parser, so routing ARRR to it removes the duplication
*and* keeps the v4 history an old wallet still needs when restoring from a pre‑fork birthday.

> **Corrected 2026‑10‑12 from the implementation.** This step was written on the belief that the
> shared parser “mis‑parses v6 and computes the wrong txid by construction”. It does neither: it
> **refuses** v6 outright. `deserialize::<UtxoTx>` returns `MalformedData` or `UnexpectedEnd` for
> all 18 v6 transactions of the offline corpus and for a hand‑built v5, so no identifier is
> produced at all. Separately its identifier *rule* is the wrong rule — on a v4 transaction it
> yields the double‑SHA256 of the bytes, which is not a v6 transaction's ZIP‑244 digest. The
> routing is therefore still required, but because the swap step **fails** rather than proceeding
> on a wrong identifier. CRD R39.11.15 and R39.11.16a carried the same error and are corrected.
> The file count was 42 here and is 48 as measured.

**E — v6 construction, then lift the gates.** Map `NetworkUpgrade::Nu6_3` to the Ironwood
activation height so the builder selects v6; implement runtime activation‑height discovery,
which is mandatory because the Pirate team will publish no height; correct the built‑in
activation‑time guard, whose test/regtest withholding keys on address HRPs and therefore does
not detect Pirate's test chain, which presents as mainnet; then set `IRONWOOD_V6_SUPPORTED`
true, which makes R39.6.4b and R39.6.4c inert by their own terms.

**F — verification.** The offline corpus first (`local/arrr-ironwood-fixtures`, 19 transactions
covering v4, Sapling‑only v6, Ironwood‑only v6 and mixed), then the test chain while it lasts,
then mainnet.

### 10.5 Obligations this creates

- **A documented divergence.** Removing Electrum changes behaviour relative to GLEEC KDF, so
  `RELOADED_VS_GLEEC.md`, `docs/GLEEC_COMPATIBILITY.md` and the governing CRD chapter must be
  updated in the same commit as the behaviour. Unlike the Ironwood gates there is no opt‑in to
  offer, because the removed path has no working provider; the entry must say so plainly rather
  than naming a switch that does not exist.
- **CRD chapter 39 §39.11 reverses.** R39.11.5–R39.11.7 move from "a non‑empty Electrum server
  list is mandatory and shielded‑backend‑only operation is unsupported" to the opposite;
  R39.11.9–R39.11.12 simplify to a single authority; D39.11.a resolves; D39.11.b largely
  dissolves in favour of the branch‑id check; D39.11.c resolves to the Zcash‑parser option.
  This is a KDF Spec Reader pass plus a Dirty Gate round.
- **Sequencing against other sessions.** `fix/activation-api-parity` holds chapter 39 Part D /
  §39.10 and has deliberately held the "all Electrum servers unusable" behaviour pending this
  decision; it should finish without implementing it, since the path is being removed.
  `fix/zcoin-reorg-stale-anchor` does not conflict. §39.11 is amended after Part D merges.

---

## 11. Field testing (green‑lit 2026‑10‑18)

**This is the active work. Everything else in the project is parked until field testing and
the beta are through.** Every statement in this section was checked against the tree on
2026‑10‑18; the one correction it produced is in §11.6.

### 11.1 Why now, and what is actually missing

Steps A–E are merged (§10.4, and the progress log below), and so is the last known pre‑beta
blocker: the **reorg‑wedge fix landed as PR #31**, merge commit `da4c95c90`, 2026‑10‑16. A
Light‑mode wallet that met a reorg of its own scanned tip used to fail every 30‑second pass
with `Compact block hash discontinuity …` forever; it now asks lightwalletd on every pass,
rewinds inside the retained checkpoint window, and falls back to a rebuild when the divergence
is deeper than that window.

**It has never been exercised against a live chain.** Its evidence is 204 unit tests driving
the observed shape against the real backend, plus a negative proof (restoring the old
in‑memory short‑circuit fails five of them, and restoring the 100‑block depth fails the
later‑note variant). That is good evidence that the logic is right and no evidence at all that
it survives contact with the network. Field testing is what closes that gap, and it is the
only thing standing between the current tree and the beta.

### 11.2 The window the ARRR team handed us

The ARRR team green‑lit field testing on **2026‑10‑18**, with a **known chain rollback to
2026‑10‑14 in progress**. Consequences, accepted deliberately:

- **Transactions sent during the tests vanish** once nodes finish reindexing. The maintainer
  accepts this: the tests move small amounts between his own wallets, so what is actually lost
  is the swap and transaction fees, not the funds.
- A rollback of this size is exactly the event the reorg fix exists for, and it is not
  reproducible on demand. Testing *now*, while it is happening, is worth more than testing on
  a quiet chain later.

### 11.3 The rollback does not endanger the Ironwood constants

Checked rather than assumed, because a height‑based gate would have made an Oct 14 rollback a
real hazard:

- `ARRR_IRONWOOD_ACTIVATION_TIME` is a **timestamp**, not a height:
  `1_791_054_000` = **2026‑10‑03 19:00 UTC**, in `mm2src/coins/z_coin.rs`. Rolling the chain
  back does not move it, and nothing in the gate compares it to a stored height.
- An Oct 14 rollback target is **~10 days after activation**, so every node stays on the
  Ironwood branch (`0x37A5165B`). The branch‑majority endpoint‑admission rules from step C
  are therefore not stressed by the rollback: there is no partition between pre‑ and
  post‑Ironwood endpoints to resolve.
- Escape hatches exist if a test ever needs them, and neither is needed by default:
  `ironwood_activation_time` (per‑coin configuration override) and `ironwood_gate_disabled`
  (suppresses the built‑in time for test and regtest networks). Both are in
  `ZcoinConsensusParams`; see §10.4 step E and CRD ch.39 R39.6.4a/R39.6.4d.

### 11.4 Expected behaviour: the fallback path, not the rewind

Arithmetic first. ARRR targets **60‑second blocks**, so Oct 14 → Oct 18 is roughly
**5 760 blocks** — two orders of magnitude beyond the fix's rewind window
(`ZCOIN_REORG_REWIND_DEPTH = 99`, and at most `ZCOIN_MAX_REORG_REWINDS = 3` further attempts,
so under ~400 blocks of rewinding in total). **The rewind cannot cover this rollback, and it is
not supposed to:** the fallback path fires, which is a wallet rebuild at the account birthday
(the recorded sync start) with **exactly one backup file**, followed by a rescan from there.

So the expected outcome is **a long resync, not a wedge, and not a lost wallet**. Every
shielded note in range is recoverable from the chain with the wallet's own viewing key, which
is why the rebuild is lossless within the range the wallet was asked to cover.

The exact log sequence to expect, in order (`mm2src/coins/z_coin/z_coin_wallet_db.rs`):

1. `ZCoin shielded wallet diverges from the chain at height … : wallet has block …,
   lightwalletd reports …` (WARN) — the divergence is **detected**, which is the part that
   used to be skipped entirely.
2. `ZCoin shielded wallet rewound from height … to … after a chain divergence` (INFO), up to
   four times. **This line is the success signal for an ordinary reorg**; here it is only the
   first move, because 99‑block steps cannot reach back four days.
3. `ZCoin shielded wallet still diverges from the chain after rewinding; rebuilding at its
   sync start` (WARN), then `ZCoin lightwalletd requesting rebuild anchor tree state at
   height …` and `ZCoin shielded wallet rebuilt at its sync start: anchored at height …,
   rescanning from …` (INFO).
4. Normal scanning, and the balance and history coming back.

**Failure signal:** `Compact block hash discontinuity at height …: prev_hash does not match
the scan anchor`, **repeated** pass after pass with no `requesting wallet checkpoint tree
state` line in between. That is the original wedge, and it means the fix did not engage.

Two other things to watch, both known and both cheap to confirm in the field:

- **No `.invalid-chain.<ms>.bak` flood.** A compact cache that fails validation is now deleted
  rather than renamed; the rename is what produced multi‑gigabyte backup files after the
  2026‑09‑29 reorg. Exactly one wallet‑database backup from the rebuild is expected, and
  nothing else.
- **The rebuild anchors at the sync start, never at 0.** If it ever reports an anchor near
  zero, or fails with "has no recorded sync start", that is a defect, not a long resync.

### 11.5 What else these tests validate, incidentally

They are the first live exercise of the **whole new lightwalletd transport**: `tonic` 0.14 on
**hyper 1** with rustls 0.23, from PRs #37 and #42. That transport has only ever been
compiled and unit‑tested; PR #37's description says so outright ("none of it has been
exercised against a real lightwalletd server") and names what to expect to differ: a
different default user‑agent, transport error text that reads differently, and
`GetBlockRange` streaming through the hyper 1 body path. A sync against a real `https` lightwalletd through a
multi‑thousand‑block rescan is close to the best test of it available, so transport symptoms
(connection drops mid‑`GetBlockRange`, idle‑pool behaviour, certificate failures, error text
that reads differently from before) are worth capturing even when the shielded logic behaves.

### 11.6 Known gaps that could surface mid‑test

- **A withdrawal is recorded when its transaction is submitted.** In‑flight shielded note
  locking covers the swap sends (selection, construction, broadcast and recording are one
  critical section in `ZCoin::send_outputs`) and the **withdraw** path: `withdraw` builds and
  returns, and when the client submits the result through `send_raw_transaction`,
  `ZCoin::send_raw_tx_bytes` records it once the backend accepts it (CRD ch.39 R39.8.0ap,
  D39.8.0e). A built withdrawal that is never submitted holds nothing. Two daemons on one seed
  with separate wallet databases are still not protected. See
  [`shielded-in-flight-note-locking.md`](shielded-in-flight-note-locking.md).
- **The P2SH / HTLC spend path is deliberately refused**, not silently wrong: the R‑address
  workaround for Pirate's transparent‑index defect is still unproved on a funded HTLC, so
  `find_output_spend` / `check_if_my_payment_sent` fail with an explicit error and a P2SH
  address is refused by balance, UTXO and history (§10.4 step C). Proving that workaround on a
  funded HTLC is the next piece of real work after the wallet‑level tests pass.
- **v6 broadcast acceptance is unproven.** Construction is verified offline against a mined
  v6 transaction, but **network acceptance has never been tested**, and a v6 shielded
  signature over a transaction with transparent inputs is not covered by the corpus (§10.4
  step E). A rejected broadcast during the tests is therefore a plausible and informative
  outcome, not necessarily a regression.
- **Most of T39.8.0h is uncovered** (broadcast/restart/expiry/concurrency), because no harness
  can drive a shielded wallet database through those transitions yet (CRD ch.39 D39.8.0f).

---

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
- **2026‑09‑20 — the ARRR light wallet works again; Plan A round 1 + A4 merged to `dev`.**
  Testing against real wallets exposed three further defects, all found by running the
  wallet rather than reading code, and all fixed and verified live on mainnet. A wallet
  that had scanned an orphaned tip could **never sync again**: there was no rewind path, so
  every retry repeated the same comparison. It surfaced as "all lightwalletd servers
  failed", which reads like an outage but was the opposite — the servers agreed and the
  wallet was stale. **Incoming payments from current Pirate wallets were invisible**:
  Pirate accepts both note plaintext versions at every height, but librustzcash derives
  ZIP‑212 enforcement solely from Canopy, which Pirate does not have, so enforcement
  resolved to `Off`, only `0x01` was accepted, and every `0x02` note was discarded without
  an error. And a **caller-requested rescan was silently discarded** by the next
  activation, which matters most when restoring a seed whose funds predate the recent scan
  window. The ZIP‑212 defect is very likely the original GleecDEX report, though that
  remains inference — we never saw that wallet. Proven end to end: a 0.001 ARRR payment
  from Treasure Chest, previously invisible, was received. Merged to `dev` (fast-forward,
  CI green before and after) and built on all six platforms.
- **2026‑09‑20 — A3 landed (R39.6.4c), completing the 3 Oct safety net.** The swap freeze
  alone left a hole: `withdraw` passes none of the swap gates, so after activation it would
  still have built a v4 transaction and failed at broadcast as an opaque network rejection.
  Every ARRR transaction is now refused from activation onwards, at the single point they
  are all built, before that path's blocking wait. The two cut-offs are deliberately
  staggered and must not be merged — between them a swap begun before the freeze must still
  spend or refund, so a build refusal starting at the freeze would strand exactly the swaps
  the freeze protects; a test pins that ordering.
  Compatibility recorded per `docs/COMPAT_SWITCHES.md`: the opt-out is omitting
  `ironwood_activation_time` from the coin's `consensus_params`, which restores
  GLEEC-equivalent behaviour exactly. Documented at the setting (CRD §39.6.4a and the field
  itself) and as a row in `docs/GLEEC_COMPATIBILITY.md`; `RELOADED_VS_GLEEC.md` carries the
  ZIP‑212 divergence, which has no switch by design.
  **Still open for Plan A:** A5, the dead `cryptoforge` endpoints upstream in
  `GLEECBTC/coins`.
- **2026‑10‑12 — Step B landed: the Zcash stack is re‑vendored on `zcash_primitives` 0.30.1,
  and v6 now parses.** The reason for the step is confirmed rather than assumed: in the pinned
  0.28.0 the v6 arm of `TxVersion::read` sits behind `cfg(zcash_unstable = "nu7")`, a rustc cfg
  we do not set, so a v6 transaction was rejected as "Unknown transaction format"; it is
  unconditional in 0.30.1. Proven by two new tests — one inlining the test‑chain transaction at
  height 68373 (asserts `V6`, `BranchId::Nu6_3`, the indexed txid, exact byte round‑trip), and
  one, opt‑in via `ARRR_IRONWOOD_FIXTURES`, parsing the whole offline corpus: all 19
  transactions parse, every txid matches `tx_index.json`, and every one round‑trips byte for
  byte. `zcash_transparent` went 0.8.0 → 0.10.0 and its patch applied cleanly; the
  `zcash_primitives` builder had drifted ~450 lines because 0.30.1 adds Ironwood and padding
  support, so nine of ten hunks applied with fuzz and `add_transparent_output_raw` /
  `set_lock_time` were placed by hand — **the focused test's pinned wire bytes still match
  exactly, which is the evidence that the deployed v4 swap transaction shape is unchanged**.
  The `zcash_client_backend` vendor copy was **dropped**: its only delta was an exact
  `time-core` pin that upstream has since removed, the condition its own patch note named.
  `orchard` became a default feature of both `zcash_client_backend` 0.24.0 and
  `zcash_client_sqlite` 0.22.0, so both now carry `default-features = false`. The ZIP‑212 fix is
  still required and still confined to decryption — upstream `zip212_enforcement` is unchanged
  and still returns `Off` without Canopy. `NetworkUpgrade::Nu6_3` returns `None` for now, which
  keeps the builder on Sapling‑era versions; the real mapping is Step E.
  **Open decision before this can ship to anyone with an existing ARRR wallet:**
  `zcash_client_sqlite` 0.22.0 adds 23 unconditional migrations (71 total, up from 48; schema
  fingerprint 467 → 727 records), creating Ironwood and Orchard tables even with the feature
  off. `classify_wallet_db` accepts a wallet only if its fingerprint equals a freshly
  initialised current schema, so an existing wallet now classifies as `Unknown` and activation
  fails with "unrecognized shielded wallet schema; file was left unchanged". The chapter's
  existing policy for a recognised older schema is **preserve‑and‑rebuild**
  (`WalletDbGeneration::ReferenceLegacy` → back up, rebuild fresh, rescan), not in‑place
  migration. Upstream `init_wallet_db` should apply the 23 missing migrations to such a DB, but
  that is untested. Recommended shape: back up unconditionally, attempt the upstream migration,
  verify the resulting fingerprint equals the current schema, and fall back to the existing
  preserve‑and‑rebuild path on any failure — so the outcome degrades to today's behaviour rather
  than to a half‑migrated wallet.
- **2026‑10‑12 — Step C, wallet half (branch `feat/arrr-lightwalletd-only`).** `lightwalletd` is now the
  sole backend of a Light‑mode ZHTLC coin: the coin carries an explicit `ZcoinMode`, the RPC client is a
  new `UtxoRpcClientEnum::LightWalletD` arm (a first‑class `UtxoRpcClientOps`), `electrum_servers` is
  accepted and ignored, and `light_wallet_d_servers` must be non‑empty. Two defects are closed. **One
  height authority:** the Light ordinary send path (wallet‑DB target height) and the HTLC spend/refund
  path (Electrum block count) both read `ZCoin::builder_target_height`, the admitted lightwalletd tip + 1,
  and refuse to build — with a typed error, no fallback — when the read fails or is more than the
  builder's 40‑block expiry delta behind what the wallet has seen. **Endpoint admission:**
  `GetLightdInfo` network, Sapling height and consensus branch exclude off‑branch endpoints; the local
  branch expectation is applied only below the *horizon* (the last declared upgrade's height, while a
  later known upgrade is undeclared), so at ARRR's real tip the partition rule decides — a wallet that has
  scanned blocks stays on its recorded branch, a fresh one needs a strict majority of at least two
  endpoints — and the inversion in the Step C warning cannot occur. **Deliberately not done:** swap‑path
  spend and already‑sent detection (`find_output_spend`, `check_if_my_payment_sent`) fail with an explicit
  error and a P2SH address is refused by balance/UTXO/history, because the R‑address workaround is still
  unproved on a funded HTLC; parser routing (Step D); `Nu6_3` mapping and the gates (Step E). The
  divergence from GLEEC KDF is recorded in `RELOADED_VS_GLEEC.md` and `docs/GLEEC_COMPATIBILITY.md`
  (no opt‑in exists, and the entries say so). CRD ch.39 §39.11 status notes updated in the same commit.
- **2026‑10‑12 — Step E (branch `feat/arrr-v6-construction`).** v6 construction exists and was moved
  **ahead** of the swap half of Step C: a live `z_send_htlc` attempt was rejected with
  `64: ironwood-version`, so nothing — including the P2SH index probe — can be transacted on a
  post‑Ironwood chain until the builder emits v6. `Nu6_3` reports the configured
  `ironwood_activation_height`, else a height discovered at runtime and held in a cell shared by every
  copy of `ZcoinConsensusParams`; the builder then selects branch `0x37A5165B`/v6 by itself (Sapling
  bundle, one `0x00` Orchard slot, empty Ironwood bundle). Discovery runs inside
  `ZCoin::builder_target_height` (the single height authority): server branch at its tip +
  first‑block‑over‑the‑activation‑time search + the 60‑block margin and 30‑block trust depth; a
  disagreement or an unreadable answer **refuses the build** instead of choosing a version. The
  built‑in activation time is now keyed on the ticker **and** the Sapling activation height (152 855),
  because Pirate's test chain presents as mainnet. `IRONWOOD_V6_SUPPORTED` is true, so the swap freeze
  and build refusal are inert. Verified offline: wrapper bytes, round trip, ZIP‑244 txid, v4 below the
  height and without one, a Sapling spend + P2SH + change funding verifying under the v6 sighash, and a
  real mined v6 transaction (1 spend, 2 outputs, 2 Ironwood actions) whose Sapling proofs and
  signatures verify under the library's digest. **Still needs a broadcast:** network acceptance, and a
  v6 shielded signature over a transaction with transparent inputs (digest commits to prevout data not
  in the corpus). Not done: the pre‑activation quiet window, persistence of the discovered height,
  native‑mode discovery. CRD ch.39 R39.6.4e added; `RELOADED_VS_GLEEC.md` and
  `docs/GLEEC_COMPATIBILITY.md` updated (no opt‑back exists).
- **2026‑10‑12 — Step D landed: ARRR transaction parsing routes to the Zcash reader**
  (`c70542f57`, in `dev` via PR #19). Recorded here late, for completeness: the swap‑path
  parsing no longer goes through the shared UTXO parser, which **refused** v6 outright rather
  than mis‑identifying it, and whose identifier rule (double‑SHA256 of the bytes) is not a v6
  transaction's ZIP‑244 digest in any case. Steps B, C (wallet half), D and E are all in `dev`.
- **2026‑10‑13 — a future `sync_start` is rejected and a pre‑Ironwood endpoint is never
  admitted** (PR #22). Closes the two ways a wallet could anchor itself somewhere the chain
  cannot support: a sync date in the future, and an endpoint still serving the pre‑fork branch.
- **2026‑10‑16 — the reorg wedge is fixed and merged** (PR #31, merge commit `da4c95c90`).
  `ensure_lightwalletd_chain_state` now asks lightwalletd on every pass instead of trusting the
  chain state the scan left in memory; the rewind depth is **99**, which lands inside
  `zcash_client_sqlite` 0.22's retained one‑checkpoint‑per‑block window (the old `anchor - 100`
  sat one block below it, so the backend either refused — treated as fatal — or truncated to
  before the wallet's earliest note); a refused rewind is redirected to the oldest retained
  checkpoint; compact blocks above the rewound height are deleted so the orphaned block cannot
  be fed back; a divergence deeper than the window rebuilds at the **account birthday** with
  exactly one backup rather than at height 0; and a cache that fails validation is deleted
  instead of renamed, which is what produced the backup flood after the 2026‑09‑29 reorg. 204
  `z_coin::` tests, with a negative proof for each half. **Not exercised against a live chain
  — that is §11.**
- **2026‑10‑18 — field testing green‑lit by the ARRR team; the project refocuses.** ARRR field
  testing, then beta, then everything else; all other implementation is parked. A chain
  rollback to **2026‑10‑14** is in progress, so test transactions will vanish once nodes finish
  reindexing — accepted, because the amounts move between the maintainer's own wallets and only
  the swap and transaction fees are lost. Re‑checked against the tree before starting: the
  Ironwood constant is a **timestamp** (`1_791_054_000` = 2026‑10‑03 19:00 UTC) and not a
  height, so the rollback cannot disturb it, and an Oct 14 target is ~10 days post‑activation,
  so no node leaves the Ironwood branch and the branch‑majority admission rules are not
  stressed. The rollback is ~5 760 blocks against a 99‑block rewind window, so the **rebuild‑at‑
  birthday fallback** is the expected path, not the rewind — a long resync rather than a wedge.
  Expected and failing log sequences, the gaps that could surface mid‑test, and what these tests
  incidentally prove about the new hyper 1 lightwalletd transport are in **§11**. One claim
  checked and **found wrong** while writing that section: in‑flight shielded note locking is
  *not* missing — it covers both v1 swap sends; only the `withdraw` path remains unrecorded
  (§11.6).
