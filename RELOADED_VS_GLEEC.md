# KDF Reloaded v0.1.0-alpha.1 — Comparison to GLEEC KDF v\<X.Y.Z\>

> **Status: skeleton.** Detail sections will be filled in as v0.1.0-alpha.1 is finalised. The summary lists below are the authoritative public catalogue of differences once the alpha is tagged.

## What is KDF Reloaded?

KDF Reloaded is a GPLv2-only continuation of the Komodo DeFi Framework codebase, anchored to upstream commit `c1d46c0c1592faa0860f704008b2b2381bc3840f` (2022-06-03) — the last commit unambiguously distributed under GPLv2-only by upstream. Post-anchor history in this repository contains independently authored work plus selected imported/adapted components from publicly available, license-compatible sources; see [`docs/reloaded-rewrite/34-provenance-ledger.md`](docs/reloaded-rewrite/34-provenance-ledger.md).

**The project is openly hybrid.** It does not claim that every file is a clean-room rewrite. Source falls into four classes: (1) clean-room originals; (2) constrained-expression fragments (interop/wire-format, convergent-idiomatic, generated, third-party-API-bound — byte-identity expected and lawful under merger doctrine / de minimis); (3) code adapted in place from permissive upstreams (recorded per CRD §1 R27); and (4) **lineage-derived** content from the upstream/GLEEC KDF work product, carried under GPLv2 copyleft. The load-bearing license theory is **GPLv2 copyleft**, not clean-room: under GPLv2 §2(b) integrated derivatives of GPLv2 code are themselves GPLv2, and under GPLv2 §6 (equivalently GPLv3 §7/§10) a downstream fork cannot impose "further restrictions" on GPL-covered code. Clean-room rewriting is applied selectively as additional polish, not as the basis for the right to distribute. The basis for each class is documented in [§34.5 of the provenance ledger](docs/reloaded-rewrite/34-provenance-ledger.md).

Two post-divergence subsystems are named explicitly here to avoid any ambiguity:

- **WalletConnect** (`mm2src/kdf_walletconnect/`) is a post-anchor feature (same crate path as the upstream/GLEEC KDF WalletConnect work product). Its substantive files were **clean-room reimplemented** (2026-06-17) against the Chapter 22 driving-spec and are gated under the R35/R36 residual-similarity rules; they are classified as **clean-room originals** in the provenance ledger, with GPLv2 copyleft retained only as a backstop. A few dictated or thin files (third-party-API-bound or convergent-idiomatic) remain listed there.
- **Orderbook** (`lp_ordermatch`) descends *largely* from pre-GLEEC common ancestry — C-ported legacy code present at the anchor. It is **not** wholly common ancestry, however: it also carries post-anchor additions (e.g. `recently_cancelled` stale-cancellation handling) that are lineage-derived and carried under GPLv2 copyleft, not by direct descent.

Credit for the original design and the bulk of the pre-anchor code is due to Komodo Platform and the AtomicDEX-API contributors, and for ongoing development on the parallel branch to GLEEC and the GLEEC KDF contributors. KDF Reloaded aims to remain **operationally compatible where practical** with GLEEC KDF: where we diverge, we expose per-feature compatibility switches so existing GLEEC-KDF integrations can retain the original behaviour with explicit configuration. We follow GLEEC's evolution and incorporate compatible changes wherever it is legally and technically feasible and where the change does not contradict our own goals (a free, open trading platform under GPLv2).

The GLEEC fork of the same upstream codebase diverged from the joint history at upstream commit `d36369980a6c08f8689b64df56fbccf0097a0a6f` and continues under a different licensing posture. KDF Reloaded and GLEEC KDF share the pre-`d3636998` history; post-divergence histories are maintained independently, with any imported/adapted material expected to be documented in project provenance records.

For licensing details, see [`LEGAL/LICENSE`](LEGAL/LICENSE) and [`LEGAL/COPYING`](LEGAL/COPYING).

## Compatibility convention

KDF Reloaded does **not** define a single global compatibility mode, and does **not** introduce a common code construct (no `compatibility` JSON object, no `CompatMode` enum, no shared registry). Instead, every behavioural divergence from GLEEC KDF that could affect operators, users, or third-party API integrations is implemented in whatever shape fits the feature, and is then documented in two specific places so that any operator can reach the GLEEC-equivalent behaviour:

1. Next to the setting itself, a short "set this to `<value>` for GLEEC compatibility" note.
2. A row in the central admin chapter [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md), which lists every such setting end-to-end.

The developer-facing rule (mandatory for AI assistants, strong recommendation for human contributors) lives in [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md). The central chapter lists every such setting, including those whose default already matches GLEEC KDF and the divergences no setting can reverse; *Settings to set for GLEEC-compatible operation* below points to it and restates the main ones.

## Summary

### Added in KDF Reloaded

- Compatibility convention and central admin chapter for GLEEC-equivalent operation; see [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md) and [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
- `regtest-netid` Cargo feature exposing test-only netids 8100, 8999, 9000, 9998 (off by default in production builds).
- `allow_insecure_key_export` `MM2.json` switch (default `false`) gating GLEEC-parity key export (offline / no-activation, HD per-derivation-path ranges, shielded ZHTLC viewing keys); see [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md) and CRD chapter 07. The secure default refuses the export superset; the single-coin activated reveal and own-mnemonic self-export remain available regardless.
- On-demand `Build Linux` GitHub Actions workflow for release-profile binaries.
- Self-hosted CI runner support ([`docs/CI_RUNNERS.md`](docs/CI_RUNNERS.md)).
- Split CI: format → matrix unit-tests → docker-tests, with cancel-in-progress concurrency.
- Specific mismatch-kind reporting in UTXO maker-payment validation (better operator diagnostics).
- Read-only fallback for legacy `<db_root>/wallets/*.wallet` wallet files written by an earlier reloaded build (two-field envelope). New wallets are always written as the canonical `<db_root>/<name>.json` record; the legacy form is read (for login, listing and deletion) but never written. This fallback has no analogue in GLEEC KDF, which never produced the `.wallet` form. The on-disk format does not by itself select HD vs single-address signing (that is governed by `enable_hd` and per-coin activation); see CRD chapter 07 §7.5/§7.7 R9A.
- **Siacoin `get_raw_transaction` carries both encodings.** GLEEC KDF returns
  only `tx_hex` from this method for SC, hex-encoding the same Sia-native JSON.
  Reloaded returns `tx_json` alongside it (CRD ch.20 R-W12), on the same terms
  as the withdraw path, so one coin does not offer the JSON carrier from one
  method and withhold it from another. No compat switch is provided for the same
  reason as the withdraw carrier below: the change is strictly additive — it
  removes no field and alters no existing field's name, type or meaning — so an
  integration written against either shape keeps working unchanged. Code:
  `coins/lp_coins_types.rs`, `coins/siacoin/siacoin_mm_coin.rs`.

- **The legacy `orderbook` and `best_orders` show a shielded-coin order as
  `Shielded`.** GLEEC KDF's legacy `orderbook` fails the whole response when
  the pair carries one shielded-coin (ARRR) order, because its legacy address
  resolver has no shielded outcome; its legacy `best_orders` leaves such orders
  out. Reloaded routes both legacy methods through the same derivation as the
  mmrpc-2.0 methods (CRD ch.32 R-F6d), renders the shielded outcome as the fixed
  string `Shielded` (R-F6e), and leaves out only an order whose coin has no
  address derivation (R-F6c). Tendermint, Siacoin and Lightning orders are now
  served on the legacy methods as well. No compat switch: the response is
  node-local and nothing on the wire changes. Code:
  `mm2_main/src/lp_ordermatch/orderbook_rpc.rs`, `best_orders.rs`.

- **Siacoin withdraw carries the signed transaction in both encodings, and
  `send_raw_transaction` accepts either.** Sia is the coin family whose native
  serialisation of a signed transaction is JSON text rather than a binary
  encoding, and the two external contracts for SC disagree about how to carry
  it: the Komodo Platform published API reference documents `tx_hex` as the sole
  carrier for every coin and defines no `tx_json` field at all, while GLEEC KDF
  for this one coin emits only a top-level `tx_json` object and no `tx_hex`.
  Reloaded emits **both** — `tx_hex` stays mandatory and authoritative (CRD
  ch.20 R-W6, ch.49 R49.25), `tx_json` is added alongside it (R-W7) — and
  accepts either on `send_raw_transaction`, `tx_hex` winning when both are
  present (R-W8). No compat switch is provided because the result is a strict
  superset of both shapes: an integration written against either one keeps
  working unchanged, no field is removed, and no existing field's name, type or
  meaning changes. `tx_json` is absent for every other coin family and its
  absence is not an error. Code: `coins/lp_coins_types.rs`,
  `coins/lp_coins_ops.rs`, `coins/siacoin/sia_withdraw.rs`.

- **Siacoin withdraw accepts a custom fee; other fee types are rejected.**
  GLEEC KDF uses a fixed fee for SC and silently ignores the request's `fee`
  field. Reloaded accepts `fee: {"type":"UtxoPerKbyte","amount":"<n>"}` for SC,
  `amount` in SC per kilobyte, replacing walletd's estimated rate (CRD ch.20
  R-W3), which is what the Komodo wallet's custom-fee box already sends. Absent
  `fee` behaves exactly as before. Accepting the field is additive; the one
  stricter point is that any other `fee` type for SC (for example `UtxoFixed`
  or `EthGas`) is now rejected with the invalid-fee-policy error instead of
  being ignored. No compat switch: a client that sent such a value to GLEEC KDF
  for SC and relied on it being ignored must stop sending it. A fee below
  walletd's estimate is accepted; a fee one tenth of the estimate was mined
  within about ten minutes on an uncongested transaction pool (checked live
  2026-10-26; a full pool is untested). Code: `coins/siacoin/sia_withdraw.rs`.

- **PIVX header lists are read by the header version.** GLEEC KDF reads a
  32-byte field after the nonce in every header of a coin configured as PIVX,
  which is wrong for version-7 headers (80 bytes on the chain from height
  2153200 to 2700499). Reloaded reads that field only for header versions 4 to 6
  and 8 and above (CRD ch.37 R37.5.5), so a swap's time-lock lookup works across
  every version era. No compat switch: no wire value, signed preimage or RPC
  shape depends on it. Code: `kdf_chain/src/header.rs`.

- Reloaded-specific Z-coin shielded database names isolate the upgraded stable
  `librustzcash` schema from GLEEC KDF's legacy schema. See
  [Shielded database isolation](#shielded-database-isolation) below.

- **Additional activation method names.** Reloaded routes, besides the
  reference method names, the flat `init_utxo*`, `init_qtum*`, `init_z_coin*`
  and `init_trezor*` task aliases and the one-shot `enable_lightning` (all
  inherited from the GPLv2 baseline), the flat `init_lightning*` /
  `cancel_init_lightning` aliases (uniform with the other `init_<coin>*` names),
  and its own Solana surface `enable_solana_with_tokens` / `enable_spl` (routed,
  but refusing every request in this beta; see
  [Removed / disabled](#removed--disabled-in-kdf-reloaded)). GLEEC
  KDF (`v2.6.0-beta` and `v3.0.0-beta`) routes none of them; clients written for
  GLEEC use the `task::` equivalents, which Reloaded also routes. The extra
  names are additive and need no switch. See
  `docs/reloaded-rewrite/rpc-method-census.md`.

- **Opt-in: an absent `max_eth_tx_type` read as unrestricted.** By default
  Reloaded reads a coin configuration with no `max_eth_tx_type` key as GLEEC KDF
  does: the coin may send untyped (legacy) transactions only, and an EIP-1559
  swap transaction is refused at signing, not downgraded. Nothing needs setting
  for GLEEC-compatible operation. Setting the top-level `MM2.json` boolean
  `max_eth_tx_type_absent_unrestricted` to `true` opts into Reloaded's own
  reading, in which an absent key means unrestricted and the coin's swap gas-fee
  policy alone decides the form; only the boolean `true` turns it on. A key that
  is present is honoured the same way either way, and the switch changes only
  what the node sends, never which transactions it can read. See CRD ch.35
  D35.7 and [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).

- **Switch: header-layout fallback for RIC and MEWC.** The shared coin registry
  keeps `"chain_variant": "BTC"` on `RIC` (Riecoin) and `MEWC` (Meowcoin), whose
  Electrum servers serve 112-byte and mixed 120/80-byte headers. GLEEC KDF reads
  them as consecutive 80-byte records, so the median-time-past it takes before an
  HTLC spend is a wrong value. Reloaded, for a coin with no `protocol.header_layout`
  key, first reads the standard layout and only when that fails exact consumption
  retries the Riecoin layout and the mixed KawPoW/AuxPoW layout, accepting the
  one that consumes the bytes exactly (a list both consume is rejected as
  ambiguous until the coin is pinned); it logs once per coin which layout
  it chose and suggests pinning it with `header_layout`. A coin that carries the
  key never falls back, and a read the standard layout accepts never does either.
  The node-level `MM2.json` key `header_layout_fallback_families` (an array of
  distinct `chain_variant` names, default `["BTC"]`; anything else refuses the
  launch) names the families that may fall back; `[]` restores
  the reference behavior. Nothing a counterparty observes changes, so the
  divergence is the default; the shared registry needs no change. See CRD ch.37
  R37.5.9e and [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).

### Behavioural divergences without a compatibility switch

These are divergences from GLEEC KDF that are **not** operator-configurable in this release (no compat switch yet). They are logged here and in [`CHANGELOG.md`](CHANGELOG.md) per the convention; a per-feature switch may be added in a later release.

- **Withdraw with an omitted HD `from`.** For an HD-mode UTXO wallet, `task::withdraw` / `withdraw` with no `from` sender defaults to the wallet's enabled address (account 0, external chain, index 0) instead of failing. GLEEC KDF rejects an omitted `from` with `FromAddressNotFound`. This makes the shipped Komodo DeFi wallet's withdraw-preview flow (which omits `from`) work against a reloaded node. An explicit but invalid `from` is still rejected (`UnknownAccount` / `UnexpectedFromAddress`). Code: `coins/utxo/utxo_common/utxo_common_hd.rs` (`get_withdraw_hd_sender`), `utxo_common_helpers.rs` (`validate_task_withdraw_sender`).
- **A Light-mode ZHTLC coin is served by `lightwalletd` alone; Electrum is
  removed, and there is no switch to restore it.** GLEEC KDF requires a
  non-empty `electrum_servers` list in the Light-mode activation payload and
  serves the coin's whole transparent side (chain tip, raw transactions and
  mined heights, balance, history, UTXOs, broadcast, block times) from it.
  Reloaded takes every one of those from the configured `light_wallet_d_servers`
  (`GetLatestBlock`, `GetTransaction`, `GetTaddressBalance`, `GetAddressUtxos`,
  `GetTaddressTxids`, `SendTransaction`, `GetBlock`) and never opens an Electrum
  connection for the coin. The activation payload still accepts
  `electrum_servers`, `min_connected` and `max_connected`, and ignores them;
  `light_wallet_d_servers` becomes mandatory and non-empty, and an empty list is
  a configuration error naming the coin at activation. **No compat switch is
  provided, and none could work:** as of 2026-10-11 no host published for Pirate
  Chain runs an ElectrumX service at all (a port scan of all ten found none),
  and the two that did stalled at the last block before the chain's first
  post-Ironwood transaction. An opt-back would restore a path with no working
  provider and would re-arm the defects the new design closes. Related, and
  also without a switch: `txfee: 0` (fee estimation) is refused for the coin
  (lightwalletd has no fee estimation; an absent `txfee` keeps the fixed default); `requires_notarization` is refused for
  the coin (there is no notarized-depth field); and the swap-path HTLC spend
  and already-sent detection run over the transparent address index with both
  the P2SH form and the `R...` re-encoding of the redeem-script hash probed (the
  serving node's index files every output under the pubkey-hash key type, proved
  live on 2026-10-12), a returned row accepted only if its script is the expected
  one, and a named error -- never "not spent" or "not sent" -- when the index
  returns nothing for a funding transaction known to be mined, when a history
  window skipped a height, or when a call failed (CRD ch.39 R39.11.4d). GLEEC
  answered these from Electrum; **no opt-out exists**, because the alternative is
  an empty answer read as "not spent". Code: `coins/z_coin/z_lightwalletd.rs`,
  `coins/z_coin/z_lightwalletd/htlc_index.rs`, `coins/z_coin/z_tx_read.rs`,
  `coins/z_coin.rs`, `coins_activation/src/z_coin_activation.rs`
  (CRD ch.39 R39.11.1--R39.11.7).
- **Endpoint admission and one builder height for a ZHTLC coin.** GLEEC KDF
  performs no server-information check and uses every configured lightwalletd
  endpoint, so an endpoint serving another fork of the chain (three of the eight
  published ARRR endpoints did, after the Ironwood upgrade) above the wallet's
  anchor makes the wallet rewind about 99 blocks and follow that endpoint's
  chain. Reloaded reads `GetLightdInfo` first and excludes an endpoint whose
  network, Sapling activation height or consensus branch does not belong to the
  coin; a wallet that has scanned blocks stays on the branch recorded with them.
  A fresh wallet whose configuration cannot name the active upgrade needs two
  agreeing endpoints. Separately, GLEEC KDF's ordinary shielded send takes its
  transaction target height from the wallet database while its HTLC spend and
  refund take it from the transparent backend; with the transaction expiry delta
  at 40 blocks, a disagreement makes every refund expired when it is broadcast.
  Reloaded gives every shielded construction path one height authority -- the
  admitted lightwalletd tip -- and refuses to build, with a typed error, when
  that tip cannot be read or is more than 40 blocks behind what the wallet has
  already seen. No compat switch: the alternative is a wallet captured by a
  stale fork and refunds that cannot be broadcast. Code:
  `coins/z_coin/z_lightwalletd.rs`, `coins/z_coin/z_coin_ops.rs`,
  `coins/z_coin/z_htlc.rs` (CRD ch.39 R39.11.7a, R39.11.9--R39.11.12).
- **Z-coin shielded database schema and names.** Reloaded uses the modern stable
  Zcash wallet schema under Reloaded-only filenames. GLEEC KDF continues to use
  the legacy filenames and schema. This is intentionally not configurable:
  sharing either file between the two implementations would permit an older
  binary to open a schema it does not understand and could compromise shielded
  wallet state.
- **V2 atomic swaps reserve the fee needed to collect the incoming payment.**
  GLEEC KDF reserves this headroom for V1 swaps but not for V2 ones, so on a
  GLEEC node a concurrent trade can spend the balance a live V2 swap needs to
  claim what it has been sent. Reloaded reserves it on both protocols, in the
  same amount (CRD ch.52 R64, ch.51 R54/R55). The visible effect is that
  `max_taker_vol` and `get_locked_amount` shrink by that fee while a V2 swap is
  live against a coin that pays its spend fee from the account balance — EVM
  coins, where it is the spend gas, and where an ERC-20 leg bills it to the
  platform coin. Coins that pay the fee out of the payment being claimed, such
  as UTXO coins, are unaffected, and no V1 answer changes. No compat switch is
  provided: the alternative behaviour is a node that can be left unable to
  collect a payment it has already been sent, and a node running both protocols
  must not answer one balance question two ways. Code:
  `mm2_main/src/lp_swap.rs`, `mm2_main/src/lp_swap/maker_swap_v2.rs`,
  `mm2_main/src/lp_swap/taker_swap_v2.rs`.
- **Shielded in-flight note exclusion lapses at the transaction's expiry
  height.** In Light mode, a shielded note spent by a broadcast-but-unmined
  transaction must stop being offered to the next send, or the second
  transaction re-presents the same nullifier and the network rejects it. GLEEC
  KDF keeps that exclusion in a store of its own and clears it in exactly one
  place: when a scanned block yields the matching transaction. Nothing else
  removes it — no expiry, no startup reconciliation, no handling of a
  transaction that disappears from the backend. A transaction that is accepted
  and then dropped, evicted, or replaced therefore never arrives, and its inputs
  are excluded for the remaining life of the wallet, across restarts, with no
  operator-visible way to recover them. Reloaded instead records the broadcast in
  the shielded wallet database and lets that database's own spendable-note
  exclusion do the work, which lapses automatically once the scanned tip passes
  the transaction's expiry height (40 blocks, ~40 minutes on ARRR). The visible
  effect is that funds stranded by a never-mined shielded send come back by
  themselves instead of being lost. No compat switch is provided: the
  alternative behaviour is permanently unspendable balance, and a node must not
  answer one spendability question two ways (CRD ch.39 R39.8.0ao--R39.8.0at).
  Code: `coins/z_coin/z_coin_ops.rs`, `coins/z_coin/z_coin_wallet_db.rs`.
- **Shielded note decryption accepts both note plaintext versions.** Pirate
  accepts both the pre- and post-ZIP-212 note plaintext at every height, but has
  no Canopy upgrade, and librustzcash derives ZIP-212 enforcement solely from
  Canopy — so an unmodified stack silently discards every note sent by a current
  Pirate wallet, and the payment never appears. Reloaded derives its decryption
  parameters separately so that both versions are read (CRD ch.39 R39.8.0am).
  GLEEC KDF, on the legacy stack, is affected by the same underlying divergence.
  No compat switch: the alternative is invisible incoming payments. Code:
  `coins/z_coin.rs`, `coins/z_coin/z_coin_wallet_db.rs`.
- **Siacoin withdraw populates `internal_id`.** GLEEC KDF leaves `internal_id`
  empty on its SC withdraw response — only its history path fills the field in.
  Reloaded sets it to the raw bytes of the signed transaction's id, the same
  value whose lowercase hex the record already reports as `tx_hash` (CRD ch.20
  R-W10), so a withdrawal record and the history record that later appears for
  the same transaction share one primary key and a caller can join them without
  re-deriving one from the other. This is a deliberate improvement, not parity.
  No compat switch is provided: the field is mandatory in the ch.49 R49.25 field
  set either way and GLEEC KDF merely returns an empty value for it, so
  populating it supplies information where a caller previously had none rather
  than changing the meaning of a value anyone could already rely on — a consumer
  that ignored `internal_id` on SC withdrawals because it was always empty is
  unaffected. Not listed in `docs/GLEEC_COMPATIBILITY.md`, which catalogues
  configuration values an operator sets; this divergence has no setting.
  Code: `coins/siacoin/sia_withdraw.rs`.

- **Siacoin transaction history on the mmrpc-2.0 `my_tx_history`.** GLEEC KDF
  serves SC history through the legacy tier-1 `my_tx_history` only, and rejects
  an SC request on the mmrpc-2.0 method with the not-supported error. Reloaded
  accepts SC on both tiers, serving the mmrpc-2.0 request from the same
  coin-generic runtime history store the tier-1 method reads (CRD ch.53
  R53.2.7). No compat switch is provided because the change is strictly
  additive: it accepts a request GLEEC KDF refuses and alters no response GLEEC
  KDF produces, so no existing integration observes a behaviour change. SC is
  still deliberately *not* routed through the SQL-indexed history storage
  (R53.3.1, ch.53 D53.1). Code: `coins/my_tx_history_v2.rs`,
  `coins/siacoin/siacoin_history.rs`.

- **Removed divergence: EVM address under an HD wallet.** Earlier Reloaded
  builds activated EVM coins (`enable_eth_with_tokens`, `task::enable_eth`)
  under an HD wallet at the address of the daemon's internal identity key,
  whereas GLEEC KDF uses the address at the coin's `derivation_path` plus
  `path_to_address`. Reloaded now matches GLEEC. Users who received funds at
  the old address can reach them by starting in single-address mode with the
  same seed phrase; see the upgrade note in `CHANGELOG.md`. Legacy `enable`
  for EVM coins under an HD wallet still binds a different key and is not
  changed yet. Code: `coins/eth/eth_activation.rs`,
  `coins_activation/src/eth_with_tokens_activation.rs`; CRD ch.35.

- **Two stricter refusals when decoding a counterparty's EVM transaction, with no
  switch to relax them.** Reloaded refuses a payload carrying trailing bytes after a
  complete transaction, and refuses a typed (EIP-2718) envelope whose signature is
  all zeros. The reference accepts both: it folds trailing bytes into the identifier
  it reports, and it applies its pre-EIP-155 unsigned-transaction convention to typed
  forms, reporting a placeholder sender. Neither input can be a confirmed on-chain
  transaction, so no honest counterparty's bytes decode differently -- but accepting
  either would let a counterparty choose a value Reloaded then reports as fact: the
  transaction identifier in the first case, the sender in the second. **No opt-back is
  offered**, because an opt-back exists to recover a reference behaviour that is merely
  different, not one that is worse, and a switch here would be an opt-in to weaken
  validation, which `AGENTS.md` §5 bars. Everything else about typed-transaction
  decoding is *parity*, not divergence: Reloaded accepts EIP-2930 and EIP-1559 exactly
  as the reference does, derives the same identifier and recovers the same sender, and
  refuses EIP-4844 and EIP-7702 as the reference does. Contract:
  [CRD ch.35 §35.6A](docs/reloaded-rewrite/35-evm-v2-activation-rpcs.md); decision
  recorded as D35.1.

- **Qtum header family selected from the protocol type when `chain_variant`
  is absent, with no switch.** When a UTXO coin's `protocol.type` is `QTUM`
  or `QRC20` and the coin's entry carries no `protocol.chain_variant` key,
  Reloaded selects the Qtum header family (the proof-of-stake header with its
  state root, UTXO root, stake prevout and block signature) from the protocol
  type, on every header read path. The reference maps an absent key to
  the default 80-byte family for every protocol type and selects Qtum only
  from `"chain_variant": "QTUM"`. The shipped coin registry sets
  `"chain_variant": "QTUM"` on every QTUM and QRC20 entry, so no deployed
  coin depends on the fallback; it only helps a hand-written entry that omits
  the key, which on the reference would read Qtum headers as 80-byte records.
  No switch exists; recorded here as a divergence (maintainer decision,
  2026-10-25). The fallback is kept now that `chain_variant` is read (decided
  2026-10-25; open decision 5 of
  [`docs/plans/utxo-header-family-selection.md`](docs/plans/utxo-header-family-selection.md)).
  Contract: [CRD ch.37 §37.5.1](docs/reloaded-rewrite/37-utxo-spv-and-block-header-validation.md)
  (the key-absent row and the port note under §37.5.8). Code:
  `coins/utxo/header_layout_fallback.rs`, `coins/utxo/utxo_builder/utxo_conf_builder.rs`.

### Removed / disabled in KDF Reloaded

- iOS build target removed from the alpha release matrix.
- **TRON (TRX, TRC20) and Solana (SOL, SPL tokens) are switched off in this
  beta.** Their support is not finished, so Reloaded does not activate them at
  all: the legacy `enable` / `electrum` answer "TRON support is not available
  in this build" or "Solana support is not available in this build", and
  `enable_solana_with_tokens` / `enable_spl` answer the Solana text. Nothing is
  registered and the rest of the node is unaffected. Both families are planned
  to return in the next beta. No switch exists: the build constants
  `TRON_ACTIVATION_AVAILABLE` and `SOLANA_ACTIVATION_AVAILABLE`
  (`coins/lp_coins_ops.rs`) are `false`. When TRON returns, Reloaded activates
  TRX and TRC20 on the legacy methods, which GLEEC KDF `v3.0.0-beta` rejects
  (it requires an mmrpc 2.0 activation); a client written for GLEEC never sends
  such a request (CRD ch.21, ch.55 D55.5).
- **Tendermint (Cosmos) coins and tokens are wallet-only in this beta.**
  Activation, balances and `withdraw` work; `setprice`, `buy`, `sell` and
  `start_swap` refuse the coin with the usual "… is wallet only" answer, and an
  order with a Tendermint coin saved by an earlier version is cancelled at
  startup. A swap already in progress still
  completes or is recovered. Orders and swaps are planned to return in a later
  release. No switch exists: the build constant `TENDERMINT_SWAPS_AVAILABLE`
  (`coins/lp_coins_ops.rs`) is `false`.
- *(further entries to be enumerated as the alpha is finalised.)*

### Work in progress

- Hardware wallet — Ledger transport (scaffolding only; crate not built into any artifact).
- Siacoin V1 atomic-swap operations. No longer a stub: SC now negotiates its
  own ed25519 HTLC key (ch.51 R-S9), gets the CRD-dictated 32-byte secret
  hash whenever it is on either side of the pair (ch.51 R71/R72), and has
  completed real send/spend/refund swaps against UTXO counterparties
  (confirmed live, both directions). Still open: the swap-spend search
  event-walk remains unimplemented (ch.20 D3/R-S8 — the affected RPC now
  fails cleanly instead of silently guessing "not spent", but does not yet
  actually search), and the V2 swap protocol is not implemented for SC
  (ch.20 D5, V1 only).
- HD wallet dispatch in swap/ordermatch paths (legacy iguana-key fallback retained for parity with upstream and GLEEC; documented in code).
- *(further entries to be enumerated.)*

### Settings to set for GLEEC-compatible operation

See [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). To match GLEEC KDF's full key-export behaviour, set `allow_insecure_key_export` to `true` (default `false`).

**Reloaded constructs the transaction format Pirate Chain's Ironwood upgrade
requires; GLEEC KDF cannot.** From the coin's Ironwood activation height Reloaded builds
version-6 transactions (Sapling bundle, empty Orchard slot, empty Ironwood bundle), and
below it version 4, so a Pirate Chain (ARRR) coin keeps sending, swapping and refunding
after the upgrade, where GLEEC KDF's every transaction is rejected by the network. The
activation height is configured (`ironwood_activation_height`) or, because the chain
publishes none, discovered at runtime from the server's consensus branch and the
activation time (CRD ch.39 R39.6.4e). If it cannot be established the build is refused
with a typed error; it is never guessed. **No setting restores GLEEC's version-4-only
behaviour**, and none is offered, because that behaviour produces nothing the chain
accepts.

Because of that, the swap freeze and the build refusal below no longer engage in this
build, and `ironwood_gate_disabled` has nothing left to switch off. It is still
accepted, and still describes the older behaviour: for a build that cannot construct
version 6, the default `false` stopped entering new swaps for a coin with a known
Ironwood activation 160 300 s before it and refused to build any of that coin's
transactions from it onwards (CRD ch.39 R39.6.4b/R39.6.4c). The gates protected a counterparty whose HTLC would otherwise be
left unspendable and unrefundable across Pirate Chain's Ironwood upgrade
(3 Oct 2026 19:00 UTC). The two coin-activation switches below are the only settings
needed to match GLEEC KDF behaviour.

**ARRR carries a built-in activation time, so omitting `ironwood_activation_time` does
not remove it.** This reverses what an earlier release documented, and the reason is
worth stating. Both `ironwood_activation_time` and `ironwood_activation_height` are an
extension this project introduced: no published coin configuration declares either of
them — verified against `GLEECBTC/coins` master, where ARRR is the only coin carrying
`consensus_params` at all and no coin mentions Ironwood. Gating driven purely by
configuration therefore engaged on no deployment at all, which made the safety net
decorative. Pirate Chain's maintainer confirmed on 2026-09-28 that there will be **no
standardness grace period** and recommended disabling ARRR swaps from 2 Oct, so the
gates had to work without waiting on a third party's configuration change. The
activation time is a fact about the network rather than an operator preference, so
this build carries it for ARRR (CRD ch.39 R39.6.4d).

Configuration still wins where it is present: `ironwood_activation_time` overrides the
built-in value, so a moved activation time stays a data change rather than a release —
a compiled-in date that could not be overridden would refuse every ARRR transaction,
and freeze its market, until a new binary shipped. The built-in value is also withheld
unless the parameters describe the real ARRR chain (ticker and Sapling activation height
152 855): the test and regtest networks activate Ironwood at a height with no timestamp
at all, and Pirate's own Ironwood test chain carries mainnet address prefixes and reports
`chainName` `main`, so address prefixes alone cannot tell it from the real chain. Today
the built-in time only feeds the activation-height discovery above. A coin whose ticker this build does not know is unaffected either way.

Two coin-activation behaviours differ from GLEEC KDF by default (CRD ch.46
R46.2.4, ch.55 R55.4.6):

- Set `sia_activation_single_address_balance` to `true` to get GLEEC KDF's Sia
  activation result under an HD wallet: `wallet_balance` always in the
  single-address `"Iguana"` form. By default Reloaded reports the HD
  per-account report there, like the other coins of an HD wallet.
- Set `legacy_enable_keeps_coin_on_balance_failure` to `true` to get GLEEC
  KDF's legacy `enable` behaviour: the coin stays registered when its first
  balance read fails, so a retry fails as already active. By default Reloaded
  deregisters it, as GLEEC KDF's own `electrum` does. This switch only exists
  to reproduce that inconsistency and is not recommended.
- Set `coin_conf_unrecognised_enum_falls_back` to `true` to get GLEEC KDF's
  reading of a coin-configuration enumeration key that holds a value the node
  does not recognise: the key's default is used and activation proceeds. By
  default Reloaded fails the activation with an error naming the key and the
  value. With the switch on, Reloaded still logs one warning per activation
  naming the key, the value and the default it fell back to, which GLEEC KDF
  does not. Today only a UTXO coin's `signature_version` (accepted values
  `base`, `witness_v0`, `fork_id`, `fork_id_rxd`) reads it; a coin carrying
  any other value cannot activate under the default and would sign with a
  sighash algorithm the network rejects if the switch is on. RXD's shipped
  `fork_id_rxd` is recognised and activates under either setting.
  Only the boolean `true` turns it on. A malformed or non-string `fork_id` is
  not an enumeration member and fails under either setting. No wire format and
  no other node is affected. **Compatibility:** for behaviour matching GLEEC KDF,
  set this to `true`. See [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).

  **The switch is scoped to an unrecognized enumeration member, not to an
  unknown key.** A coin-configuration key no consumer reads is ignored
  silently — at the top level and inside a recognized nested object alike —
  and no setting changes that; it is parity, kept so that a shared coins file
  newer than the binary reading it cannot break activation (CRD ch.45
  R45.3.7). A recognized key whose value is of the wrong type remains a
  per-coin activation error naming the key, also outside the switch. Only an
  unrecognized *value* of the recognized `signature_version` key is this
  project's divergence, and that alone is what the switch hands back to GLEEC
  KDF; the malformed `fork_id` above stays outside it under every setting.

## Detail sections

Detail subsections will be added as features land. Each entry above expands here with: rationale, code locations, test coverage, migration notes, and links to relevant pull requests.

### Anchor and divergence

- **Joint history ends at:** `c1d46c0c1592faa0860f704008b2b2381bc3840f` (2022-06-03), GPLv2-only.
- **GLEEC divergence point:** `d36369980a6c08f8689b64df56fbccf0097a0a6f` (GLEEC-side first commit under modified terms).
- **KDF Reloaded baseline:** `c1d46c0c` verbatim, then post-anchor development in this repository.

### Build and toolchain

- Stable Rust per `rust-toolchain.toml`. (Upstream historical README pinned `nightly-2022-02-01`; this is no longer required.)
- CMake ≥ 3.12, system C/C++ toolchain.

### Networks

- Production netids supported: **8762** (AtomicDEX), **6133** (GLEEC).
- Test netids (8100/8999/9000/9998): only available with `--features regtest-netid` and never compiled into production `kdf` binaries.

### Shielded database isolation

Reloaded and GLEEC KDF deliberately use different native SQLite files for the
modern shielded wallet and compact-block stores. The older native Sapling-state
cache keeps its existing compatible name and format:

| Purpose | KDF Reloaded | GLEEC KDF / pre-upgrade name |
| --- | --- | --- |
| Shielded wallet | `<TICKER>_RELOADED_WALLET.db` | `<TICKER>_WALLET.db` |
| Compact-block cache | `<TICKER>_RELOADED_COMPACT_BLOCKS.db` | `<TICKER>_COMPACT_BLOCKS.db` |
| Legacy native Sapling-state cache | `<TICKER>_CACHE.db` (shared) | `<TICKER>_CACHE.db` (shared) |

On the first supported Light-mode activation after upgrading, Reloaded creates
its own modern wallet database and performs a full shielded rescan from the
resolved sync start. It does not open, migrate, rename, truncate, or delete the
GLEEC/pre-upgrade files. The compact cache is likewise isolated, so cached
blocks are validated and stored independently. Native full-node conversion into
the modern compact cache remains an explicit open item in CRD chapter 39.

`<TICKER>_CACHE.db` is not the upgraded wallet database or the modern
compact-block cache. It is the pre-existing native-mode commitment-tree cache;
Reloaded preserves its compatible table and serialization contract. A
Light-mode activation may create its empty schema when the file is absent, but
Light-mode balance, history, and scan state come from the two Reloaded-specific
files above. Existing compatible cache contents are not renamed merely because
the modern wallet schema changed.

Consequently, switching from Reloaded back to GLEEC KDF does **not** require
manual cache deletion. GLEEC reopens its original files, which Reloaded left
untouched, and resumes or refreshes them according to GLEEC's own sync logic.
The GLEEC database may naturally be behind the chain tip after time spent using
Reloaded, but it has not been converted to the incompatible modern schema.

Reloaded also protects its own filenames conservatively. If a
`<TICKER>_RELOADED_WALLET.db` contains the exact recognized legacy schema, it is
moved to a timestamped `.legacy-v2.*.bak` backup before a fresh modern wallet is
built and rescanned. An unknown or corrupt Reloaded wallet/cache file is left
unchanged and activation returns a typed schema error; it is never silently
deleted or treated as an empty wallet. Manual intervention is required only for
that explicit unknown/corrupt-file error, not for ordinary switching between
Reloaded and GLEEC KDF.

## Forward-compatibility commitment

KDF Reloaded commits to following GLEEC KDF's evolution:

- We monitor GLEEC KDF releases and incorporate compatible changes when they are legally and technically feasible.
- For changes we *cannot* incorporate verbatim (licensing, project-direction, or quality reasons), we still expose a per-setting opt-in so the GLEEC-equivalent behaviour remains reachable, and we list that setting in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
- The only exception is a behaviour that would either violate GPLv2 or directly contradict the project's principle of free, open trading at reasonable cost. In that case the divergent setting defaults to the KDF Reloaded behaviour and the original-compatible value is gated behind an explicit acknowledgement with a runtime warning. See the developer rule in [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md).
