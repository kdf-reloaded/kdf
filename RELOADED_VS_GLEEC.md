# KDF Reloaded 0.3.0-beta.1 — Comparison to GLEEC KDF 3.0.0-beta

> The lists below are the public catalogue of differences as of KDF Reloaded 0.3.0-beta.1.

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

- Top-level `MM2.json` boolean `swap_evidence_allow_single_host` (default `true`): a coin whose configured node list names exactly one host trusts that host for its swap reads, as if `swap_evidence_node` named it; `false` requires two hosts or `swap_evidence_node` (CRD ch.51 §51.6G). The activation answer of such a coin, including the legacy `enable` and `electrum` answers, carries the added member `"swap_evidence_single_host": true`, and the activation answer of a BCH platform whose `bchd_urls` name exactly one host, which its SLP tokens then trust, carries `"swap_evidence_single_bchd_host": true`; every other answer keeps its members.
- Optional activation field `swap_evidence_node` for every coin family that can swap, with the activation error `SwapEvidenceNodeNotInList` for a value outside the coin's node list (CRD ch.51 §51.6G R100).
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
  out. Komodo DeFi Framework 2.6.0-beta behaves the same. On live nodes
  (2026-11-05) of Komodo DeFi Framework 2.6.0-beta and of GLEEC KDF (its
  3.0.0-beta release and its development build
  [kdf_a12695e](https://devbuilds.gleec.com/dev/kdf_a12695e-linux-x86-64.zip), a prebuilt binary
  of its 2.6.0-beta line), a client
  of the legacy `orderbook` got an error for an ARRR pair while a shielded order
  was in the book (or an empty book, where the client treats the error as no
  data), and their mmrpc-2.0 `orderbook` listed the order. KDF Reloaded's legacy
  `orderbook` lists such orders, the node's own included, as its mmrpc-2.0 call
  does. Reloaded routes both legacy methods through the same derivation as the
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

- **Kerrigan (KRGN) headers through `"chain_variant": "KRGN"`.** Released GLEEC
  KDF versions refuse the value `"KRGN"` and read Kerrigan headers as standard
  80-byte headers, so claims and refunds of a KRGN swap fail over Electrum in
  nearly every case (CRD ch.37 R37.5.11.5 -- R37.5.11.7). An open proposal for
  GLEEC KDF adds the same value. Reloaded accepts it and reads each header by
  its algorithm, so the KRGN entry Reloaded needs is one that such a GLEEC KDF
  build would also read; released GLEEC KDF versions refuse to activate it. No
  compat switch: no wire value, signed preimage or RPC shape depends on it.
  Code: `kdf_chain/src/header.rs`.

- Reloaded-specific Z-coin shielded database names isolate the upgraded stable
  `librustzcash` schema from GLEEC KDF's legacy schema. See
  [Shielded database isolation](#shielded-database-isolation) below.

- **Additional activation method names.** Reloaded routes, besides the
  reference method names, the flat `init_utxo*`, `init_qtum*`, `init_z_coin*`
  and `init_trezor*` task aliases and the one-shot `enable_lightning` (all
  inherited from the GPLv2 baseline), the flat `init_lightning*` /
  `cancel_init_lightning` aliases (uniform with the other `init_<coin>*` names),
  and its own Solana surface `enable_solana_with_tokens` / `enable_spl`
  (Solana is wallet-only; see
  [Removed / disabled](#removed--disabled-in-kdf-reloaded)). GLEEC
  KDF (`v2.6.0-beta` and `v3.0.0-beta`) routes none of them; clients written for
  GLEEC use the `task::` equivalents, which Reloaded also routes. The extra
  names are additive and need no switch. See
  `docs/reloaded-rewrite/rpc-method-census.md`.

- **Additional Solana coin-configuration forms.** Besides the forms GLEEC KDF
  uses (`SOLANA` with `protocol_data: {}`, and `SOLANATOKEN` with `platform`,
  `decimals`, `mint_address` and `program_id`), Reloaded reads its own older
  forms: `{"type": "SOLANA"}` without `protocol_data`, and `SPLTOKEN` with
  `platform`, `decimals` and `token_contract_address`, which means the SPL
  Token program. The extra forms are additive and need no switch. Native and
  browser (WASM) builds read all of these (browser builds since PR #430).
  CRD ch.40 R40.1.2, R40.1.3; PR #361.

- **Solana compatibility activation methods.** `experimental::enable_solana_with_assets`
  and `experimental::enable_solana_token` take GLEEC KDF's requests and answer its results
  and errors. Where Reloaded differs, no switch exists: a request with `tx_history: true`
  gets its normal answer (GLEEC KDF ends it without one); a wallet without a token account
  activates the token with balance `"0"` (GLEEC KDF fails the activation); the platform
  method also takes `hd_path_layout`, `path_to_address` and `priv_key_policy`; an inline
  token `protocol` may also use Reloaded's `SPLTOKEN` form. Native and browser (WASM)
  builds (browser builds since PR #430). CRD ch.40 §40.2, §40.3, D40.6B.2; PR #426.

- **Solana node selection.** GLEEC KDF asks the nodes `getHealth` in list order before every
  request and uses the first healthy one. Reloaded draws a random node order once when the coin is
  enabled (`solana_node_order: "listed"` keeps the listed order), asks a node `getHealth` at most
  once per 30 seconds, sets a failing node aside for 20 seconds doubling up to 12 hours, and keeps
  the reads of one withdraw on one node. A sent transaction is never rebuilt on another node. CRD
  ch.40 R40.1.5; PR #426.

- **Solana wallet methods answer.** On an active Solana coin or SPL token
  GLEEC KDF ends many requests without a JSON answer (`sign_message`,
  `verify_message`, `validateaddress`, `convertaddress`,
  `get_raw_transaction`, the legacy `my_tx_history`,
  `set_required_confirmations`, `show_priv_key` and others). Reloaded answers
  each: with a result where the code exists, with an error naming the coin
  otherwise. Message signing uses the message's bytes with no prefix and a
  base58 signature; `show_priv_key` gives the 64-byte keypair in base58. The
  minimum transaction amount of an SPL token is one smallest unit of its own
  decimals, where GLEEC KDF states one lamport for every token; no peer sees
  it while Solana cannot trade. No switch: an abnormal end cannot be a
  compatibility target. CRD ch.40 D40.6B.1, R40.6B.14; PR #401.

- **Solana withdraws and token balances.** The withdraw answers have GLEEC
  KDF's shape, `fee_details` with `fee_amount`, `rent_amount` and
  `total_amount` included, and a short SOL balance answers
  `NotSufficientPlatformBalanceForFee` as there. Reloaded differs where GLEEC
  KDF moves the wrong amount, fails or builds a wrong transaction:
  - an SPL `max` sends the whole token balance;
  - a wallet that holds no account for a token reads balance zero instead of
    failing the activation or `my_balance`;
  - a recipient that is a token account is refused with `InvalidAddress`;
  - a Token-2022 token is refused at activation with a text that says so;
  - `AmountTooLow` for an SPL token names one smallest unit of the token's
    decimals as its threshold, not one lamport.
  No switch: none of these is a compatibility target. CRD ch.40 D40.6B.2 to
  D40.6B.6; PR #425.
- **Sums in Solana withdraw answers.** Members that add or subtract two
  amounts carry the coin's full decimals: `spent_by_me` and
  `my_balance_change` of a SOL withdraw, `my_balance_change` of an SPL
  withdraw, an SPL withdraw's `fee_details.total_amount`, and `required` of
  `NotSufficientPlatformBalanceForFee` and of a SOL `NotSufficientBalance`.
  Reloaded writes `"1.000000000"` where GLEEC KDF writes `"1.000000"` or
  `"1"`; the values are equal. No switch: a client that reads them as
  decimal numbers sees no difference. CRD ch.40 R40.2A.4, D40.2A.1;
  PR #456.

- **Solana under an HD login.** GLEEC KDF refuses Solana activation under a
  global-HD login and derives no Solana key from a seed. Reloaded derives the
  key from the seed phrase at `m/44'/501'/N'/0'` by default, or at
  `m/44'/501'/N'` with the request member `"hd_path_layout": "bip44"`; nothing
  is stored per wallet, so a client that used `"bip44"` must send it on every
  activation. One seed phrase gives three Solana addresses (single-key, default
  layout, `"bip44"`). `hd_path_layout`, `path_to_address` and `priv_key_policy`
  are additional request members; a hardware-wallet policy is refused, and
  `IguanaPrivKey` under an HD login gives the wallet's single internal key
  and its address. No
  switch: the reference behavior is a refusal, which cannot be a useful
  compatibility target, and no peer sees which key a wallet uses. CRD ch.40
  §40.1A; PR #392.

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
- **Opt-in: a re-pricing replaces the previous orders only after the new order passes
  its checks.** By default `setprice` with `cancel_previous` behaves as in GLEEC KDF: it
  cancels the pair's previous orders first, so a failed balance check or a refused order
  leaves the pair empty until the next successful `setprice`. Setting the top-level
  `MM2.json` boolean `setprice_replace_after_checks` to `true` (only the boolean `true`
  turns it on) runs every check and builds the new order first, and cancels the previous
  orders only when all of them pass; a failed check returns its error and cancels
  nothing. The cost is a stale price: after a failed re-pricing the previous orders stay
  live and matchable at their old price until the next successful `setprice` or a cancel,
  and while an empty pair only misses trades, a stale price can lose funds. That is why the
  switch is off by default. With it on, a `timeout_in_minutes` on each `setprice` bounds
  that window: the order is cancelled at the latest that many minutes after it was placed
  (for an order restored at startup, after the restart). Nothing on the wire changes.
  See CRD ch.32 R-GH8a and [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
- **EVM coins and tokens log the gas limits they will use.** At activation, one
  INFO line names where each gas-limit block comes from (the coin's own entry,
  its platform coin's entry or the defaults) and lists the members that differ
  from the defaults, with the gas-estimate multiplier when one is set; a DEBUG
  line lists every member. It is logging only: no request, response or
  transaction changes, so no switch exists. See CRD ch.57 R57.5.5.
- **Electrum servers can be pinned by their key, and a server that skips its
  certificate checks is flagged at activation.** A server entry with `protocol`
  `SSL` may carry `spki_sha256`: the SHA-256 digest of the server's public key
  (`SubjectPublicKeyInfo`), or an array of 1 to 4 of them. With a pin the node
  accepts the server only if it proves that it holds a pinned key; the chain,
  the dates and the name of its certificate are not checked, so a self-signed
  certificate can be used and still be authenticated. A mismatch is a handshake
  failure that suspends the server and is logged at WARN; a malformed pin or a
  pin on a `TCP` server removes that server. GLEEC KDF and Komodo DeFi
  Framework ignore the key (measured 2026-11-07), so an entry that carries it
  works there, unpinned. The key is opt-in and changes nothing unless set, so no
  switch exists. Separately, at activation Reloaded logs one WARN line for each
  `SSL` server that has `disable_cert_verification` set and no pin: the
  connection is encrypted, but the server is not authenticated. The reference
  logs no such line. What `disable_cert_verification` accepts and refuses is
  the same on both. See CRD ch.38 R38.6.4m and R38.6.4n and
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
- **A coin's Electrum servers that are behind the chain or stuck are asked last, and a swap's
  confirmation wait polls one server.** Reloaded asks each connected Electrum server for its newest
  block when it joins and compares the heights: a server more than the coin's tolerance (180 seconds
  of blocks, at least 1 and at most 20) behind the height two or more servers have reached, or one
  that sends no new block for three block times or ten minutes while another does, is tried after
  the others for reads and broadcasts, and logged once per change of state; a confirmation wait
  polls one up-to-date server and moves on only when a request to it fails, when it falls out of
  date, or when it answers the transaction request with an error three times in a row. The read-only
  request `electrum_servers` shows the servers' state. GLEEC KDF ranks by the list position and has
  none of this, so a lagging server keeps answering there. No peer observes it. To get GLEEC KDF's
  routing, set `electrum_tip_checks` to `false` (see the settings list below). Servers that disagree
  about the chain (one far ahead of the rest, or two with different headers at a common height) fail
  the activation with an error naming them, and after activation, once the disagreement has lasted
  two block times and at least 60 s, the coin refuses new orders and withdrawals with that error,
  and starts no new swap, until they agree again; GLEEC KDF has no such check. Row in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.38 R38.6.4o.
- **Bitcoin II (BC2) transactions are signed the way the BC2 network requires.**
  Since block 57,750 the BC2 network checks every signature against a digest
  that carries an extra 4-byte field after the signature-hash type, and refuses
  a transaction signed the ordinary way ("Signature must be zero for failed
  CHECK(MULTI)SIG operation"). A coin entry may now carry the top-level key
  `sighash_suffix`, a `0x`-prefixed hexadecimal value of at most 32 bits; when
  it is present, every input the node signs for that coin (withdrawals, the
  DEX-fee transaction, HTLC payments, spends and refunds, the taker's
  pre-signature of a version-two swap, and `sign_raw_transaction`) is signed
  over the digest with those four bytes appended. BC2's value is `0x01324342`.
  The key `sighash_suffix_height` is read and checked but not used yet: the
  suffix applies at every height. A malformed value fails the activation of
  the coin with an error naming the key, and so does the combination with
  Radiant's signature version or with the Zcash-family overwintered signature
  hash. Hardware wallets cannot add the field, so a coin
  with a suffix refuses a hardware signing request before the device is
  contacted. A BC2 entry without the key still activates and logs one warning
  that the network will refuse its transactions. A coin without the key signs
  exactly as before. The shared coins repository carries the key on the `BC2` and
  `BC2-segwit` entries since 2026-11-14. **No switch returns the ordinary signature:** the
  Komodo DeFi Framework 2.6.0-beta and the GLEEC KDF development build
  `a12695e` (the two builds measured) cannot sign BC2 transactions after block
  57,750 at all, so there is no reference behavior to return to. **BC2 swaps
  require a counterparty that signs with the suffix.** A coin with a suffix
  adds a capability word to its order and match messages and declines a
  counterparty without it before any swap starts (CRD ch.61 R61.5A, ch.59
  §59.4A); nodes that predate the word, and the reference builds, are declined.
  Set `require_suffix_counterparty` to `false` in the coin entry to admit them;
  a swap with them still cannot complete when they have to spend BC2. From
  v0.4.1 the order book does not list the BC2 orders of such a node (see
  "From v0.4.1, orders that cannot be matched are not listed" below; this
  setting lists them again). This
  check is temporary and will be removed as soon as the other implementations
  support suffix signing. A coins file from before 2026-11-14 may lack the
  `sighash_suffix` key on the `BC2` and `BC2-segwit` entries: add it, or use
  the KDF Reloaded coins file, because without it the BC2 network rejects BC2
  withdrawals and swaps. See
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md) and
  [`docs/plans/bc2-replay-protection.md`](docs/plans/bc2-replay-protection.md).
- **A clock check against NTP time servers.** At start, every hour and after the computer slept
  or its clock was set, the node takes the median clock offset from NTP time servers
  (`0.pool.ntp.org` to `3.pool.ntp.org` by default, or the servers in `ntp_servers`). A clock more
  than 10 seconds off stops the node at start, so swaps in progress wait until it starts with a
  correct clock; while it runs, new orders and swap starts are refused until the clock is right
  again, and running swaps and withdrawals go on. When no server
  answers the node warns and goes on, unless `ntp_required` is `true`. `"ntp_servers": []` turns
  the check off. Nothing changes on the wire. Contract:
  [CRD ch.45 §45.6C](docs/reloaded-rewrite/45-startup-configuration-and-environment-tolerance.md).
  Plan: [`docs/plans/startup-clock-check.md`](docs/plans/startup-clock-check.md).

### Behavioural divergences without a compatibility switch

These are divergences from GLEEC KDF that no compatibility switch reverses in this release. They are logged here and in [`CHANGELOG.md`](CHANGELOG.md) per the convention; a per-feature switch may be added in a later release. Where an entry names a setting, that setting adjusts the KDF Reloaded behavior it describes and does not restore GLEEC KDF's.

- **Swap chain reads must agree between two nodes.** Before a refund, or a step that uses the swap's secret, KDF Reloaded acts only on chain reads that two distinct hosts of the coin agree on, or on the answer of the one node named in `swap_evidence_node`, or on the operator's own daemon in native mode. A coin whose configured list names exactly one host trusts it by default (`swap_evidence_allow_single_host`); with that switch `false`, a coin with fewer than two distinct hosts and no `swap_evidence_node` starts no swap. This is a Reloaded addition (CRD ch.51 §51.6G). The rule is switched on per coin family; the families it covers so far are listed in the CHANGELOG breaking change "A new swap needs two nodes of the coin that agree". With `swap_evidence_node` set, that node's answer alone is taken, and when it does not answer the step waits.
- **A swap refund waits for chain reads it can rely on, up to a bound.** KDF Reloaded's legacy swap engine applies the rule above to its refund steps. Before the taker refunds, it looks for a claim or a refund of its payment and refunds only after an answer that the payment is not spent: on every coin a look that fails or does not finish, and on a coin under the two-node rule also a look whose nodes do not agree, makes the refund wait and look again. A claim found on one server whose secret matches the swap's secret hash is used at once; a refund, or another spend, found on one server only is not acted on. The maker's refund step waits in the same way on its look at the taker payment and on an undetermined answer of the check whether the refund can be sent. Such a wait (a look that fails, does not finish or does not agree, or an undetermined answer of that check) lasts until an hour after the refund first became possible in the step's current run, and at least until its payment's lock time plus 3700 seconds; then the step ends with `MakerPaymentRefundFailed` or `TakerPaymentRefundFailed`, whose text names the kind of failure and, for reads that did not agree, the fix, and the payment waits for `recover_funds_of_swap`. Other waits of a refund step keep their own rules (a check that fails for another reason is retried without a bound; a claim found but not confirmed keeps its own deadline). A watcher refunds the taker payment only after a search that answered that it is not spent, and keeps searching otherwise. A check of the other side's payment that cannot be determined is repeated until the step's existing deadline. A refund that would otherwise be sent can therefore wait, or end as refund failed and need `recover_funds_of_swap`. These are Reloaded additions and make no parity claim; no message and no transaction a peer validates changes, so no switch exists; row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.51 R31a, §51.6G R100 item 9 and the other status notes of 2026-11-23 in ch.51; ch.09, status note after §9.6.
- **Order-management methods under `mmrpc: "2.0"`.** `setprice`, `update_maker_order`, `cancel_order`, `cancel_all_orders`, `my_orders` and `order_status` are legacy-only in GLEEC KDF: a versioned request answers `NoSuchMethod`. KDF Reloaded also routes them on the versioned surface, with the legacy members inside `params`, the legacy result under `result`, typed errors with HTTP statuses, and unknown `params` members ignored. The legacy forms and their success bodies are unchanged; a legacy error text no longer carries the inner `file:line]` trace tokens. Additive; no switch. Plan: [`docs/plans/mmrpc-v2-completion.md`](docs/plans/mmrpc-v2-completion.md); CRD ch.43 D43.1A.1 status note.
- **Trading methods under `mmrpc: "2.0"`.** `buy`, `sell`, `orderbook_depth` and `orders_history_by_filter` are legacy-only in GLEEC KDF. KDF Reloaded also routes them on the versioned surface in the same way as the order-management methods above (legacy members inside `params`, legacy result under `result`, typed errors with HTTP statuses, unknown `params` members ignored; legacy forms and success bodies unchanged). `get_trade_fee` stays legacy-only, as the reference documentation marks it deprecated in favor of the versioned `trade_preimage`. Additive; no switch. CRD ch.43 §43.1R.
- **Wallet and coin methods under `mmrpc: "2.0"`.** `my_balance`, `disable_coin`, `send_raw_transaction`, `validateaddress`, `convertaddress`, `convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`, `set_required_confirmations` and `set_requires_notarization` are legacy-only in GLEEC KDF. KDF Reloaded also routes them on the versioned surface as above; their typed errors follow the versioned `withdraw` as the reference answers it (an unknown coin 404, a transport failure 502), and `disable_coin` reports the blocking swaps or orders inside `error_data`. Legacy forms and success bodies unchanged. Additive; no switch. CRD ch.43 §43.1R.
- **A refused legacy `disable_coin` names the blocking swaps twice.** When running swaps or orders being matched block the deactivation, or the coin is not active, the legacy error body carries the members the reference answers (`orders` with `matching` and `cancelled`, `active_swaps`) and in addition `swaps` with the same list as `active_swaps`, which the reference documentation names. Orders of the coin that can be cancelled are cancelled first, and `orders.cancelled` lists them, where GLEEC KDF cancels none before refusing. Additive members; no switch. CRD ch.43 D43.1E.1 status note.
- **`disable_coin` and the passive platform: what still differs.** The legacy `disable_coin` of KDF Reloaded makes a platform coin with active tokens passive, answers `passivized`, and takes `force_disable`, as GLEEC KDF does. Six differences remain, and the sixth has a switch. (1) The `mmrpc: "2.0"` form of KDF Reloaded neither makes a platform passive nor reads `force_disable`: it removes the platform, and its tokens stay registered and active, but the platform no longer collects their history; it does so again once the platform is activated again with `tx_history` and the token named in the request; GLEEC KDF has no versioned form. (2) A forced removal checks the platform and every one of its tokens for running swaps and orders being matched, and refuses the whole call if any is in use; GLEEC KDF checks the platform's ticker only. (3) An EVM token whose platform is passive keeps working, including its orders and fee estimates; in GLEEC KDF its transactions and fee estimates fail until the platform is activated again. (4) A standalone activation task for the ticker of a passive platform is refused at `init` with `CoinIsAlreadyActivated`; GLEEC KDF accepts the task and ends it with that error after building the coin. (5) Removing a coin ends its background work, except that of a Lightning coin, which goes on until the node stops. (6) A platform-with-tokens activation that names a token registered for another platform keeps that token silently in GLEEC KDF; KDF Reloaded refuses it with `TokenIsAlreadyActivated` before building anything by default (a registered token of the same platform is kept in both). Set the `MM2.json` member `reactivation_registered_token` to `"keep"` to match GLEEC KDF; see [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). A passive platform is activated again by the platform-with-tokens methods, as in GLEEC KDF. CRD ch.43 §43.1E and §43.1F.
- **Swap, daemon and network methods under `mmrpc: "2.0"`.** `all_swaps_uuids_by_filter`, `import_swaps`, `version`, `stop`, `metrics`, `get_my_peer_id`, `get_directly_connected_peers`, `get_gossip_mesh`, `get_gossip_peer_topics`, `get_gossip_topic_peers`, `get_relay_mesh`, `ban_pubkey`, `list_banned_pubkeys` and `unban_pubkeys` are legacy-only in GLEEC KDF. KDF Reloaded also routes them on the versioned surface as above; the ban methods' typed errors (`InvalidParam` 400, `Internal` 500) are KDF Reloaded's own choice under the general convention, since the reference has no versioned sibling for them. Legacy forms and success bodies unchanged. `recover_funds_of_swap`, `stats_swap_status` and `get_trade_fee` stay legacy-only. Additive; no switch. CRD ch.43 §43.1R.
- **ZEC version-5 transactions.** With a ZEC entry of `txversion` 5, KDF Reloaded builds and reads version-5 transactions in the ZIP 225 format with the ZIP 244 transaction id and signature, as the Zcash network requires since NU7. GLEEC KDF has no version-5 support: with that entry it builds a version-4 body that the network refuses. KDF Reloaded also refuses at activation a Zcash-family entry whose `txversion` it cannot build (not 4 or 5) and a version-5 entry without a branch id; GLEEC KDF accepts any `txversion`. No switch. CRD ch.39 §39.11.4V status notes.
- **ARRR pays one fixed fee of 0.0001 ARRR, and its minimum trading volume is 0.001 ARRR.** With no `txfee` in its coin entry (the published ARRR entry has none), KDF Reloaded's shielded coin pays a fixed 10,000 base units for every transaction it builds (withdrawal, swap payment, DEX fee, and the spend and refund of a swap payment) and reports that fee in `trade_preimage` and the swap figures; its minimum trading volume, and the default minimum volume of a new order, is ten times that fee. GLEEC KDF takes the shared default of 1,000 base units per kilobyte, deducts a fixed 1,000 base units in a shielded HTLC spend and has a minimum trading volume of 0.0001 ARRR. A `txfee` in the coin entry sets the fee in KDF Reloaded, the HTLC spend and refund and the minimum included, so `txfee: 1000` gives the earlier fee and minimum back (the receiver fee it reports is then 1,000, not the size estimate); an entry with `txfee: 0` or the ZIP 317 fee class is refused at activation. No switch. CRD ch.39 R39.5.2, R39.6.5f (status note of 2026-11-26).
- **Withdraw with an omitted HD `from`.** For an HD-mode UTXO wallet, `withdraw` and `task::withdraw` with no `from` sender spend from the coin's activated address, the address at `path_to_address` (by default account 0, external chain, index 0), instead of failing. GLEEC KDF rejects an omitted `from` with `FromAddressNotFound`. This makes the shipped Komodo DeFi wallet's withdraw-preview flow (which omits `from`) work against a reloaded node; the maintainer kept it deliberately on 2026-11-04, set the address on 2026-11-06, and no switch is planned. An explicit but invalid `from` is still rejected (`UnknownAccount` / `UnexpectedFromAddress`) on both methods. Until plan [`hd-key-unification`](docs/plans/hd-key-unification.md) item H5 (PR #270), the fallback was always account 0, external chain, index 0, and the direct `withdraw` method refused an HD UTXO wallet. Code: `coins/utxo/utxo_common/utxo_common_hd.rs` (`get_withdraw_hd_sender`), `utxo_withdraw.rs` (`resolve_withdraw_sender`), `utxo_common_helpers.rs` (`withdraw_with_hd_sender`, `validate_task_withdraw_sender`).
- **An HD account number already in use.** `task::create_new_account` with an `account_id` that the wallet already holds ends the task with `AccountAlreadyExists` (with the `account_id`) before any key is derived; `task::create_new_account::status` reports it. GLEEC KDF derives the key first and then reports an internal error. Only a client that asks for an existing account sees the difference. The same task also serves Siacoin, which GLEEC KDF does not offer HD accounts for; that is a Reloaded extension (the maintainer's decision of 2026-11-04). Plan [`hd-key-unification`](docs/plans/hd-key-unification.md) item H5.4 (PR #279); CRD ch.05 R35c. Code: `coins/hd_wallet.rs` (`choose_new_account_id`), `coins/rpc_command/init_create_account.rs`.
- **`get_my_address` while a Trezor is initialized.** For an EVM coin that is not activated, KDF Reloaded derives the reported address from the login's key, also when a Trezor context exists. GLEEC KDF derives it from the device whenever a Trezor context exists. As on GLEEC KDF, each coin takes its keys and stores its accounts by its own key policy, and only a coin activated for the Trezor is refused key export. One point still differs: the BCH activation task refuses the Trezor policy (CRD ch.05 §5.10B). A new address of a coin activated for the Trezor is confirmed on the device by `task::get_new_address` and not by the direct `get_new_address`, in both implementations (PR #491; CRD ch.49 R49.42). Batch 4 item B4-2 (PR #293); CRD ch.05 §5.10B R57 and D7.e.
- **A Trezor replaced while its coins are active.** A coin activated under the Trezor policy keeps the identity of the device it was activated with. After `task::init_trezor` puts a different device in the slot, the coin answers `FoundUnexpectedDevice` to `task::create_new_account`, `get_new_address` and a withdrawal before it asks the device or writes anything; it works again with its own device, or after it is activated anew with the other one. A swap, also one that was already running, keeps signing with the coin's own device and refuses any other. GLEEC KDF uses the new device's keys against the old device's accounts. The maintainer decided on 2026-11-08 that no switch restores it. Batch 4 item B4-14 (PR #302); CRD ch.05 §5.10B R58a and R58a.1. Code: `crypto/src/hw_ctx.rs` (`ensure_same_device`), `coins/hd_pubkey.rs`.
- **`task::get_new_address` follows the coin's key policy.** `task::get_new_address::init`, `::status`, `::user_action` and `::cancel` derive, record and query without any device state when the coin uses software keys, also while a hardware wallet is connected, and complete with the answer of `get_new_address` (states `Preparing` and `RequestingAccountBalance`); a TRX or TRC20 ticker and a Siacoin complete this way. With a Trezor the task asks the device to show the address, reports `{"ConfirmAddress": {"expected_address": ...}}` while it waits for the button, and records the address only after the press. GLEEC KDF runs this task on the device path for every coin, so with software keys it ends with `HwContextNotInitialized` and a TRX or TRC20 ticker can never complete. Further differences: a device that shows another address than the one derived ends the task with `DeviceAddressMismatch` and both addresses (GLEEC KDF: `Internal`); the task runs the gap check and answers `EmptyAddressesLimitReached` with the request's `gap_limit` (the direct `get_new_address` of KDF Reloaded still runs none); the `Internal` chain is refused for EVM and TRON coins with `InvalidBip44Chain` in the task and the direct form (the GLEEC KDF task accepts both chains); there is no address-subscription step and no `FailedScripthashSubscription`. The comparison the user makes on the device's screen is the check that matters. CRD ch.49 §49.2.3, ch.05 R54.
- **A coin's `derivation_path` changed between sessions under an HD login.** Under an HD login with the seed as the key source, KDF Reloaded loads a coin's stored HD accounts only when each stored account key equals the key the seed derives for that account index below the coin's currently configured `derivation_path`; a stored account that differs is removed with one WARN and not loaded. After the coins file changes a coin's path, the coin therefore starts as a fresh HD wallet at the new path (the first account created and, by default, its addresses scanned up to the gap limit), and restoring the previous path brings the earlier addresses back the same way (the first account, or the one `path_to_address` names, scanned up to the gap limit; other accounts with `task::create_new_account`, addresses beyond the gap limit with `get_new_address`). No key material is lost in either implementation, since every address can be derived from the seed again at its path. GLEEC KDF loads stored accounts as stored: after a path change the earlier accounts and their addresses stay in use while the reported path is the new one. A coin activated under the Trezor policy loads its rows as stored. The WARN reads "…that the wallet's seed did not derive", which is inaccurate for this case. The maintainer decided on 2026-11-11 that this release documents it; a friendlier behavior is planned, see plan [`hd-account-storage-on-path-change`](docs/plans/hd-account-storage-on-path-change.md). The check came with batch 4 item B4-2 (PR #293); CRD ch.05 §5.10B, status note of 2026-11-11. Code: `coins/hd_wallet_storage/mod.rs` (`load_accounts_derived_from_seed`, `is_account_of_the_seed`), `coins/utxo/utxo_common/utxo_common_hd.rs` (`load_hd_accounts_from_storage`), `coins/eth/eth_activation.rs`.
- **`page_balance` of a page that lists no address, and an HD account that knows no address.** `account_balance` of every HD coin answers `page_balance` as an object keyed by ticker, summed over the addresses of the returned page, as GLEEC KDF does. For a page that lists no address (past the known range), and for the `total_balance` of an account without a known address, KDF Reloaded answers one entry for the requested coin's ticker with a zero balance, where GLEEC KDF answers `{}`. Under the default `scan_policy`, KDF Reloaded also scans a stored account that has no known address at the next activation, for UTXO coins, QTUM, Sia and EVM coins including TRON; GLEEC KDF scans only a wallet that has no stored account, so an account left empty by a first activation that failed while scanning stays unscanned there. The maintainer decided on 2026-11-22 that no switch restores either behavior. CRD ch.49 R49.35 and R49.36 item 7, ch.38 R38.8.5, ch.05 R35a. Code: `coins/rpc_command/account_balance.rs`, `coins/coin_balance.rs` (`enable_hd_wallet`).
- **Legacy `my_balance` and `get_enabled_coins` answer for an EVM coin under an HD login.** They report the activated address. GLEEC KDF fails both under an HD login, for EVM coins as for UTXO coins; for a UTXO coin KDF Reloaded fails them too. Code: `coins/eth/eth_market_ops.rs` (`my_address`), `coins/lp_coins_ops.rs` (`get_enabled_coins`). See CRD ch.05 §5.9B R35 and plan [`hd-key-unification`](docs/plans/hd-key-unification.md).
- **A Light-mode ZHTLC coin is served by `lightwalletd`; Electrum is only a
  fallback for three swap questions, and there is no switch to restore the
  Electrum backend.** GLEEC KDF requires a non-empty `electrum_servers` list in
  the Light-mode activation payload and serves the coin's whole transparent side
  (chain tip, raw transactions and mined heights, balance, history, UTXOs,
  broadcast, block times) from it. Reloaded takes every one of those from the
  configured `light_wallet_d_servers` (`GetLatestBlock`, `GetTransaction`,
  `GetTaddressBalance`, `GetAddressUtxos`, `GetTaddressTxids`, `SendTransaction`,
  `GetBlock`). It asks the `electrum_servers` only when no `lightwalletd`
  endpoint can answer three swap questions about an HTLC (was my payment already
  sent, was this payment spent, the wait for the counterparty's spend), and only
  a server whose block hash at a recent height matches the `lightwalletd`
  endpoints'; never for heights, broadcast or the wallet, and activation never
  connects to it (CRD ch.39 R39.11.25, decided 2026-10-28; PR #130). The
  activation payload still accepts `electrum_servers`, `min_connected` and
  `max_connected`; the two counts are ignored. `light_wallet_d_servers` becomes
  mandatory and non-empty, and an empty list is a configuration error naming the
  coin at activation. **No compat switch restores the Electrum backend, and none
  could work:** as of 2026-10-11 no host published for Pirate
  Chain runs an ElectrumX service at all (a port scan of all ten found none),
  and the two that did stalled at the last block before the chain's first
  post-Ironwood transaction. An opt-back would restore a path with no working
  provider and would re-arm the defects the new design closes. Related, and
  also without a switch: `txfee: 0` (fee estimation) is refused for the coin
  (lightwalletd has no fee estimation; an absent `txfee` keeps the fixed default); `requires_notarization` has no effect on
  the coin (there is no notarized confirmation count; it is accepted with a warning and
  plain confirmations are counted); and the swap-path HTLC spend
  and already-sent detection run over the transparent address index with both
  the P2SH form and the `R...` re-encoding of the redeem-script hash probed (the
  serving node's index files every output under the pubkey-hash key type, proved
  live on 2026-10-12), a returned row accepted only if its script is the expected
  one, and a named error -- never "not spent" or "not sent" -- when the index
  returns nothing for a funding transaction known to be mined, when a history
  window skipped a height, or when a call failed (CRD ch.39 R39.11.4d). GLEEC
  answered these from Electrum; Reloaded asks Electrum only as the fallback above,
  and **no opt-out exists** for the index path, because the alternative is an
  empty answer read as "not spent". Code: `coins/z_coin/z_lightwalletd.rs`,
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
- **An ARRR withdrawal takes no memo.** Reloaded's `withdraw` request has no
  `memo` member, so a memo sent with it is ignored, the shielded output carries
  none, and the answer has no `memo` member. GLEEC KDF echoes the request's memo
  in the answer (CRD ch.39 R39.5A.3). Memo support is a separate, later item,
  noted under R39.5A.3. No compat switch: there is nothing to switch on yet.
  Code: `coins/lp_coins_types.rs` (`WithdrawRequest`), `coins/z_coin.rs`
  (`withdraw`).
- **The ARRR history reports a fee only where the node recorded it.** GLEEC KDF
  computes a row's `transaction_fee` from the full transaction (CRD ch.39
  R39.8.9e). Reloaded's Light-mode wallet reads compact blocks, which carry no
  fee data. A row reports the fee the node recorded when it broadcast its own
  send, and `0` for a transaction another wallet built, for a swap spend or
  refund, and for an own send found again after a rebuild of the wallet
  database (R39.8.9e1). No compat switch: the data the reference computes from
  is not there. Code: `coins/z_coin/z_coin_wallet_db.rs`.
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
  unaffected. Listed in `docs/GLEEC_COMPATIBILITY.md` as a divergence no setting
  reverses.
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
- **QRC20 token history: an error answer moves on, and the collector can
  end.** No switch. Three differences, all for QRC20 tokens:
  - *Error answers.* An error answer to `blockchain.contract.call`,
    `blockchain.contract.event.get_history` or
    `blochchain.transaction.get_receipt` moves the request on to the coin's
    next Electrum server; the session stays open and the server is not
    demoted. The exact error `{"code": 1, "message": "history too large"}` and
    unreadable answers stay final. The three reads are also spread over the
    connected servers like other read-only queries. GLEEC KDF takes any error
    answer as the final answer.
  - *End state.* When three history passes in a row fail with an error answer
    from every server tried, the collector stops and `my_tx_history` reports
    `sync_status` with code `-2` and the message "No listed Electrum server
    answers the contract-event history. History is not available". Activating
    the token again starts it anew. A transport failure does not count, so a
    node that is offline keeps retrying. GLEEC KDF repeats a failed history
    request every 10 s without limit; its only end state is "history too
    large", code `-1`, which is unchanged.
  - *Incomplete pass.* A pass that left a listed transaction out, because its
    details could not be read, asks for the history again after a retry delay
    (10 s, doubling up to 300 s). GLEEC KDF asks again only when the balance
    changes or the token is activated anew.

  The contract-event subscription that KDF Reloaded's balance stream places for
  a token is placed again after a reconnect, at most once per new session; GLEEC
  KDF has no balance stream for tokens and places no such subscription.
  Contract: [CRD ch.38 §38.6.4Q](docs/reloaded-rewrite/38-utxo-coin-maintenance-and-features.md)
  (status notes after D38.6.4Q.2 and D38.6.4Q.3, and after the
  code-quality finding of R38.6.4Q.6). Code: `coins/utxo/rpc_clients/electrum_pool.rs`,
  `coins/utxo/rpc_clients/electrum_rpc_client.rs`, `coins/qrc20/history.rs`.
- **QTUM and its QRC20 tokens share one Electrum client when their client
  inputs match.** No switch. GLEEC KDF gives every activated instance its
  own client, so each server holds one session for QTUM and one for each
  token. KDF Reloaded gives instances of the QTUM family one client when the
  platform, the server list (in order and per entry), `min_connected`,
  `max_connected`, `listed_order_for_spends`, `block_interval` and the
  builder arguments are all equal; any difference gives the instance its own
  client, as in GLEEC KDF. What stays the same: a token activates without
  QTUM and keeps working after QTUM is disabled, and disabling one instance
  or a failed activation closes no session another instance uses. What
  differs on a shared client:
  - *Sessions.* Each server holds one session for all sharing instances.
  - *Naming.* Activation errors, transport error texts, log lines and
    `CoinTransportMetrics` name the platform ticker, also for a token's
    requests, and `electrum_servers` for a token reports the shared client.
  - *Disagreeing servers.* A token that would reuse a client whose servers
    disagree is refused with `ElectrumServersDisagree`, as a fresh build is.
  - *Subscriptions.* A disabled instance's remembered subscriptions are
    not placed again after a reconnect unless another live instance
    remembers them.
  Contract: [CRD ch.38 §38.6.4Q](docs/reloaded-rewrite/38-utxo-coin-maintenance-and-features.md)
  (status note after D38.6.4Q.1). Code:
  `coins/utxo/rpc_clients/electrum_client_registry.rs`,
  `coins/utxo/utxo_builder/utxo_coin_builder.rs`, `coins/lp_coins_ops.rs`.
- **QTUM and its QRC20 tokens share one record of spent coins per address,
  and their builds no longer wait for other UTXO coins.** No switch. GLEEC
  KDF keeps a record of recently spent coins per coin instance, enters
  nothing for a token's broadcast, and serializes token broadcasts and the
  withdraw builds of every UTXO coin with one process-wide lock; a QTUM swap
  send does not take it and a delegation does not hold it. So a QTUM send
  right after a token send from the same address, or two token sends, can
  select the same coin, and the second broadcast is rejected. KDF Reloaded
  keeps one record per platform and address script for QTUM and its tokens.
  Every transaction it
  built from that address and the backend accepted is entered: QTUM's swap
  sends and merges, every token broadcast, and the withdraws and delegations
  promoted when they are broadcast. The record's own lock orders the builds
  of that address, from the answered listing through the broadcast and the
  entry, in place of the process-wide lock, which the other UTXO coins keep.
  Under an HD login, QTUM's enabled address and the tokens' address have
  separate records. Contract:
  [CRD ch.38 §38.6.4Q](docs/reloaded-rewrite/38-utxo-coin-maintenance-and-features.md)
  (status note after D38.6.4Q.4). Code:
  `coins/utxo/spent_record_registry.rs`, `coins/qrc20/qrc20_helpers.rs`,
  `coins/utxo/utxo_withdraw.rs`, `coins/utxo/qtum_delegation.rs`.
- **A seed that is already connected is not dialed again.** A node short of relays dials each
  address at most once per 5 minutes, as GLEEC KDF does. GLEEC KDF also dials a seed that has
  connected to it by itself, because it compares addresses only, and the extra connection closes
  unused after two minutes. KDF Reloaded remembers which peer each seed address reached and does
  not dial it while that peer is connected. Nothing changes on the wire; no switch. Contract:
  [CRD ch.28 R27a](docs/reloaded-rewrite/28-libp2p-modernization.md). Code:
  `mm2_p2p/src/redial_guard.rs`.
- **One DNS resolver for every name, Google Public DNS by default.** GLEEC KDF looks seeds up
  through Google Public DNS and every other name through the computer's resolver. KDF Reloaded
  looks every name up through the resolver `dns_resolver` chooses: Google Public DNS by default,
  `"system"` for the computer's resolver, or DNS servers of the operator's choice. The default
  matches GLEEC KDF for seeds; `"system"` matches it for coin servers and services; no value
  matches both. Sia, the WalletConnect relay, Lightning peer addresses and the statistics method
  still use the computer's resolver in this release. Nothing changes on the wire. Contract:
  [CRD ch.45 §45.6B](docs/reloaded-rewrite/45-startup-configuration-and-environment-tolerance.md).
  Code: `common/dns_resolver.rs` (with `dns_resolver/wire.rs` and `hosts.rs`),
  `mm2_p2p/src/resolving_transport.rs`.

- **Reloaded never writes a cancelled order's stored record back, with no switch.**
  On the reference, an order update or the node's own match-pruning step that runs
  into a concurrent cancel can write the cancelled order's stored record back. The
  order then returns as a live order at the next start and is announced to other
  nodes again. Reloaded never writes the record of an order once it has been removed.
  On the reference, an order whose record could not be deleted at cancel time comes
  back after a restart, because the failed deletion is logged and not retried.
  Reloaded retries the deletion and, at start, skips and deletes a record whose
  removal the order history already records (switch `maker_restore_skips_cancelled`,
  CRD ch.12 R7c); only an order without a history entry, such as one created with
  `save_in_history: false`, can still come back. Contract:
  [CRD ch.12 R7b, R7c](docs/reloaded-rewrite/12-order-match-state-store.md),
  [ch.32 R-GH11](docs/reloaded-rewrite/32-orderbook-p2p-and-trie.md).

- **Median-time-past of coins whose headers carry a fixed-length trailer.**
  A coin whose Electrum servers serve the standard 80-byte header plus a
  fixed-length trailer (Bitcoin Atom, 84 bytes from height 505888) has a
  median-time-past list that no header layout reads, so its HTLC spend and
  refund fail to build. GLEEC KDF reads such a list as consecutive 80-byte
  records, so its median is a wrong value (black-box observation, SwapLab
  2026-11-02, release 3.0.0-beta binary; for BCA 42 minutes early at the tip
  observed). Reloaded, when the layout-aware reads fail, splits the list at the
  even stride (odd count, byte count divisible by it, header size 80 to 4096) and
  reads the time at byte 68 of each header, or else fetches the headers one by one,
  and checks the times for plausibility; a read that cannot get plausible times
  fails with an error naming the check, never a guess. No switch value reproduces
  GLEEC KDF here, and `header_layout_fallback_families: []` now only turns off the
  full-layout fallback of RIC and MEWC, not this read. Nothing a counterparty
  observes changes. See CRD ch.37 R37.5.10 and
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
- **EVM V1 payment-spend wait.** Both scan the swap contract's logs in windows of
  `logs_block_range` blocks (default 1,000). GLEEC KDF rescans from the swap's
  start block on every poll and retries a refused window query. KDF Reloaded
  checks the deadline on every iteration, including after a failed query, keeps a
  cursor past the windows already scanned, and shrinks the window when a node
  reports a range limit. KDF Reloaded's search for an existing spend, refund or
  own payment uses exact windows where GLEEC KDF's are one block wider; there is
  no observable difference beyond one duplicate block per window.
- **Reference prices: two sources that check each other, a warn-once log and a 10 s cap on native builds.** GLEEC KDF takes
  the first of a fixed list of three price addresses that answers, rotates the bot's list after a success, logs
  every failed attempt at error level on every attempt and applies no timeout of its own (CRD ch.62 §62.2 to
  §62.7). KDF Reloaded asks two sources at the same time (`prices.gleec.com` with its cache
  `defistats.gleec.com`, and `prices.cipig.net:1717`, with `?expire_at=259200`) and combines them: a coin
  priced by one source is kept, a coin both price within 5 % takes the newer record, and a coin they
  disagree on by more than that is, by default, priced from the first source with a warning (on 2026-11-07 the
  two services priced KMD at 0.00313 and 0.00680 USD, and a wrong figure from the first source can be used this
  way). The `MM2.json` key `price_conflict` set to `"refuse"` leaves such a coin unpriced instead: the bot quotes
  no pair with it and a swap with it is stored without USD prices. There is
  no rotation, since the first address of a source is asked first every time. A failing price address is
  reported by one warning when it starts failing and one info line when it answers again (the bot finding
  no source at all, by one error line), each attempt on a native build is cut off after 10 s (a browser build
  also sets a timer, which no test covers yet), and the USD prices of an own swap are fetched once, only for a successful swap (as in GLEEC
  KDF; earlier KDF Reloaded builds fetched them for every swap, from an address that no longer resolves). The bot's
  per-pair refusal and immediate cancellation of orders are the reference's; they are under observation in
  v0.4.0 and will be revisited. The `MM2.json` keys `price_urls`, `price_providers` and `price_conflict` are new; GLEEC KDF
  has none of them and no configurable provider set. The start request now accepts the reference's `price_urls`
  too. See CRD ch.62,
  [`docs/NETWORK_CONFIG.md`](docs/NETWORK_CONFIG.md#price-service) and
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).

- **A shielded withdrawal short only because of an earlier unconfirmed send names the cause.** A
  Light-mode ARRR `withdraw` whose notes are committed to an earlier, unconfirmed transaction of the
  same node fails with `BalanceLockedByUnconfirmedTransaction` (HTTP 400, with the available, required
  and committed amounts); GLEEC KDF reports `NotSufficientBalance`. Row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.39 D39.8.0e.
- **Shielded coins cannot be activated in a browser (WASM) build.** Activation ends with
  `CoinCreationError`; GLEEC KDF's browser build has no such refusal. Use a native build. Row in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.39 R39.10.1.
- **A node far behind on order updates answers a delta request with the full set.** It keeps only
  the changes between the ends of a long run of updates, at most 2,048 per maker and pair, and answers
  a request from a state no longer stored with the maker's full order set for the pair. Every peer can
  apply it; only the size differs. Row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.32 R-K5, R-K6.
- **Nodes and servers are named host-only in logs, errors and debug output.** KDF Reloaded names
  every node or server by scheme, host and port and prints no password, API key, header value or a
  URL's userinfo, path or query; GLEEC KDF prints full node URLs in its EVM transport errors. Rows in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.35 R35.1.17.
- **The gossip queue toward a slow peer is bounded.** KDF Reloaded caps each peer's queue at 2,048
  messages or 4 MiB, drops the oldest waiting messages toward that peer when a new one does not fit,
  and disconnects a peer that stays at the cap for 60 seconds or keeps losing messages; GLEEC KDF's
  queue is unbounded. The wire is unchanged. Row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.28 RQ1a, D8.
- **Seeds and relays release order-book memory on a timer.** KDF Reloaded expires the change history
  every 30 seconds, shrinks the node store after each ten-minute purge and releases the nodes of a
  cleared pair or pubkey; GLEEC KDF releases them only when traffic touches the same pair again. A peer
  out of sync for more than five minutes gets a full snapshot instead of a delta. Row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.32
  R-K4, R-B2.
- **ARRR native mode is refused in this release.** An activation with `mode.rpc` `Native` is refused
  with a configuration error; GLEEC KDF accepts native mode. Light mode is unaffected. Row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md);
  CRD ch.39 R39.11.1a.
- **Electrum calls share a time budget.** A call that may try several Electrum servers has a budget
  (60 s by default) from which each attempt's bound is derived, and enabling a UTXO coin probes every
  listed server at once and succeeds as soon as one connects; GLEEC KDF's per-attempt bounds do not
  depend on how many servers remain, and it does not wait at activation. Row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.38
  R38.6.4j.
- **A shielded coin's `lightwalletd` endpoints are named and suspended on failure.** The endpoint that
  rejected a broadcast is named (host-only) in the error and the log; an endpoint that fails at
  transport level is suspended for 20 s, doubling up to 12 hours, and endpoints that are not
  suspended are tried first. Only the order in which the operator's own endpoints are asked, and
  error texts, change. Row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.39 R39.11.30, R39.11.31.
- **A Tendermint coin can carry priority gas prices, and a request or a swap fee policy can pick one.**
  Reloaded reads the optional `priority_gas_prices` member (three numbers, the chain's low, average and high gas
  prices) of a `TENDERMINT` entry's `protocol_data`, accepts the `CosmosGasPriority` fee form beside the
  `CosmosGas` one on withdraw and staking requests, and lets `get_swap_gas_fee_policy` and
  `set_swap_gas_fee_policy` take a Tendermint platform coin; GLEEC KDF `v3.0.0-beta` has none of the three beyond
  `CosmosGas`, and answers `NotSupported` for a Tendermint coin on the two methods. Two points are Reloaded's own:
  an array with a figure of zero or less fails the coin's activation, where a zero price would make every fee zero;
  and `set_swap_gas_fee_policy` on a Tendermint coin without `priority_gas_prices` accepts and stores any value
  with one warning in the log that the coin cannot honor the level, and prices at the default. A Tendermint request
  that carries a fee form of another family ignores it, as GLEEC KDF does. No switch is needed: a coins file
  without the member and a request without the new forms are priced as before; only the staking default gas
  limits change (CHANGELOG, Changed). Rows in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); CRD ch.36 R36.3.5, ch.18 R18.6B, ch.49 R49.28 to
  R49.30, ch.35 R35.6.8, R35.6.9. PR #323.

- **The TRON node client gives each node a time limit, fails over on bad answers and
  accepts request headers and an API key.** A TRON node that does not answer within 10 seconds is skipped, and so is a
  node that answers with an error status, with something that is not the expected JSON, or with the node's own
  error form; a rate-limit answer is never read as an empty account. The node that answers is asked first next time,
  and several requests of one coin run at the same time. A signed transaction that a node rejects with a code ends the
  request with that code. An entry of the TRON `urls` list may also be an object `{"url": "...", "headers": {...}}`
  whose headers go to that node only, and the top-level `MM2.json` key `tron_api_key` is sent as `TRON-PRO-API-KEY`
  to TronGrid hosts over HTTPS only. Both are Reloaded request extensions that a reference client never sends; no switch is
  needed because a request without them is served as before. The values are secrets and are never logged or shown.
  Row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md); PR #394; CRD ch.21 §21.9 R-N3 to R-N9.

- **TRON gets the standard platform-with-tokens activation route, and keeps the legacy one.**
  `enable_eth_with_tokens`, `task::enable_eth::*`, `enable_erc20` and `task::enable_erc20::init` accept a TRX
  platform and TRC20 tokens, as GLEEC KDF `v3.0.0-beta` serves them; Reloaded also keeps TRX and TRC20 on the legacy
  `enable` and `electrum` methods, which GLEEC KDF refuses for them, as an extension a GLEEC client never reaches.
  Where Reloaded differs from that reference, none of it needs a switch, and each is a row of
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md): swap-contract members may also be written as
  base58check or `41...` hex on a TRON platform (a wider input; an EVM platform keeps `0x` only); the MetaMask
  policy and `rpc_mode` are refused for TRON at activation (the reference activates an address its withdraw then
  refuses); `IguanaPrivKey` under a global-HD login is honored with the node's single internal key; and a key in the
  query string of a node URL works. Entries of `nodes` may also carry a `headers` object and get `tron_api_key`.
  CRD ch.21 §21.4, R21.10B.2, R21.10B.4, D21.B1, D21.B7, D21.10B.3.

- **TRON has an HD wallet on the standard methods; the legacy route keeps the internal key.**
  Under a global-HD login `enable_eth_with_tokens` and `task::enable_eth::init` derive the TRX address under the
  `derivation_path` of the coin config (coin type 195), as GLEEC KDF does; the HD methods (`get_new_address`,
  `account_balance` and the task forms of `scan_for_new_addresses` and `account_balance`; this build has no task form
  of `get_new_address`) answer
  base58check addresses and a balance map (over TRX and the enabled TRC20 tokens for the TRX ticker, that token only for
  a TRC20 ticker, as GLEEC KDF does), and `task::create_new_account::init` fails with `Internal`. An address counts as
  used when the chain has created its account or an enabled TRC20 token has a balance there. For a scan asked for a
  TRC20 ticker this differs from GLEEC KDF, which looks at the account only: receiving a TRC20 token does not create the
  account, so GLEEC KDF's scan misses an address that holds only a token, and Reloaded's finds it (a superset, never
  fewer addresses; not visible to peers). The legacy `enable` method keeps the node's internal key under a global-HD login, as Reloaded's
  EVM and UTXO legacy routes do, so its TRON address differs from the HD route's address for the same seed; GLEEC KDF
  refuses TRON on the legacy method, so a GLEEC client never reaches it. `withdraw` of a TRON coin takes an HD `from`
  and signs with that key, and refuses `from` on a single-key coin (it was ignored before). A TRC20 withdraw under a global-HD login takes an HD `from` resolved in the platform's HD wallet (any activated address of the platform); this is a Reloaded extension for TRON. An EVM ERC-20 withdraw under a global-HD login resolves `from` in the platform's HD wallet in the same way, as GLEEC KDF does (CRD ch.49 R49.34). No switch; row in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.21 R-A2, R21.10B.4.

- **`convertaddress` for a TRON coin converts between the base58check and the `41...` hex form.**
  GLEEC KDF answers an EVM `0x` form for a TRX or TRC20 address. Reloaded takes `{"format": "base58"}` or
  `{"format": "hex"}`, accepts `from` as base58check, `41...` or `0x41...`, and refuses an EVM `0x` address and the EVM
  format values with the legacy error body; an EVM coin is unchanged. No switch; row in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.21 R21.10B.8, D21.10B.2.
- **TRON has a transaction history when a TronGrid API key is set.** GLEEC KDF has no TRON
  history: its versioned `my_tx_history` answers `NotSupportedFor`, its legacy one an empty list, and it ignores
  `tx_history` for TRON. Reloaded answers the same without a key. With `tx_history: true` and a keyed TronGrid node
  (the node's own `TRON-PRO-API-KEY` header or `tron_api_key`), both methods list the transactions of TRX and TRC20
  coins from TronGrid's account listings, over every known address of an HD wallet, with base58check addresses and the
  fee details of a withdraw result. Only confirmed transactions; history requests go to keyed TronGrid hosts only. An
  addition a GLEEC client does not meet unless it sets the key; no switch; row in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.21 §21.10C.

- **The fee of a TRON withdrawal follows the node's bandwidth rule, and a TRC20 fee limit
  has a margin and a maximum.** The withdraw estimate, its amounts, its errors and its fee details follow GLEEC KDF
  `v3.0.0-beta` as the CRD records it, with three differences. (1) *Bandwidth.* GLEEC KDF charges only what the
  sender's free and staked bandwidth, added together, do not cover. The TRON node takes a transaction's bandwidth from
  the staked bandwidth as a whole, or from the free bandwidth as a whole, and otherwise charges TRX for all of it.
  Reloaded estimates by the node's rule, so its estimate is never below what the node charges; for an account whose
  two quotas only together reach the size it is higher than GLEEC KDF's, and `max` then sends less. (2) *Fee limit
  margin.* GLEEC KDF sets a TRC20 transfer's `fee_limit` to the cost of the dry-run energy exactly. Reloaded adds 20%,
  rounded up to whole SUN, so that a small change of the contract's state between the dry run and the block does not
  fail the transfer; `total_fee` in the fee details stays the expected charge. (3) *Maximum.* Reloaded never sets a
  `fee_limit` above 100 TRX and refuses, before signing, a transfer whose energy alone would cost more, with
  `UnsupportedError` naming both figures. None of the three is visible to a peer and none changes the shape of an
  answer, so no switch exists; rows in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
  CRD ch.21 R21.10B.7, D21.10B.1.
- **A TRC20 withdrawal whose dry run reverts is refused.** GLEEC KDF `v3.0.0-beta`, as
  the CRD records it, judges the energy dry run of a TRC20 transfer only by its `energy_used`: when the token contract
  would refuse the transfer, it still returns a signed transaction and a fee estimate, and broadcasting it burns the
  fee. Reloaded reads the dry run's revert indication and refuses the withdrawal before anything is signed, with
  `UnsupportedError` and the text "the token contract refused this transfer (its dry run reverted)" followed by the
  node's reason; no other node is asked. The energy a contract's deployer may pay is not modeled in either, so the
  estimate of a TRC20 fee can be above the chain's charge in both. No peer sees the check and a successful dry run
  answers the same, so no switch exists; row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
  CRD ch.21 R21.10B.10 to R21.10B.13.

- **A TRON GasFree custody address is checked against the GasFree controller before it is shown or used.** No
  released GLEEC KDF version that the CRD records carries GasFree; the reference's GasFree work is an open, unmerged
  draft (as of 2026-05-04, CRD ch.63 §63.0), and this check is a KDF Reloaded addition.
  Reloaded compares it with the answer of the controller contract's `getGasFreeAddress` and keeps a confirmation for
  10 minutes. In the answers that carry `gasfree_address`, an address the controller contradicts is left out, with a
  warning in the log; when the controller cannot be read, the derived address is shown. Before a gasless transfer is
  signed, a contradicted address refuses the gasless rail and leaves the native one. A safety divergence: no switch;
  row in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.63 R29, §63.3.3.

- **The result of a gasless TRON withdraw carries each member name once.** No released GLEEC KDF version that the
  CRD records carries GasFree; the reference's GasFree work is an open, unmerged draft (as of 2026-05-04, CRD ch.63
  §63.0). In that draft the result of a gasless TRC20 withdraw holds the names `coin` and
  `from` twice in one JSON object: `from` once as the request's HD selector and once as the array of sending
  addresses, so a client whose JSON reader keeps the last of two equal names never sees the selector. Reloaded
  writes each name once: `coin` once, `from` always as the array with the sending address, and no selector. The
  other members of the signed authorization's envelope (`relay_type`, `chain_id`, `from_address`,
  `gasfree_address`, `verifying_contract`, `signed_authorization`, `created_at`) are the draft's, at the top level
  of the result, and the result has no `tx_hex` and no `tx_hash`. It also has no `memo` member, where the draft
  writes `null`: Reloaded omits an absent memo in every withdraw result outside the Tendermint family. A client that
  needs the selector has it in its own request. No peer sees the result, so no switch exists. CRD ch.63 §63.8A,
  D63.8.

- **The task form of a gasless TRON withdraw submits the transfer and follows it until it settles.** No released
  GLEEC KDF version that the CRD records carries GasFree; in the reference's open, unmerged draft (as of 2026-05-04,
  CRD ch.63 §63.8B) no RPC submits a signed authorization to the provider and none asks the provider about a
  transfer: the direct `withdraw` and `task::withdraw::*` both end at the signed authorization, and the three task
  phases for a submission are declared and never reported. In Reloaded the direct `withdraw` answers as the draft
  does (sign-only). `task::withdraw::init` goes further: it submits the authorization once, reports
  `SubmittingGaslessAuthorization`, `WaitingForGaslessProvider` and `WaitingForGaslessSettlement`, and asks the
  provider every `status_poll_interval_ms` until the transfer is settled or 120 seconds have passed since the
  signed deadline. Its final answer is the sign-only result with the provider's `trace_id` in the fee details and a
  top-level `tx_hash` (the provider's transaction); the draft's task answer has `trace_id` `null` and no `tx_hash`.
  The errors `QuoteExpired` and `TraceNotFound`, which the draft declares and never raises, are raised: the first
  when the signed deadline has passed before the submission is sent, the second for a 404 of the provider's trace
  request. A client that wants the draft's behavior uses the direct `withdraw`. No peer sees any of this, so no
  switch exists; row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.63 §63.8B (R30 to
  R35, D63.9 and its status note).
- **A swap database migrated beyond state 15 is refused at start-up.** When the highest migration state in `MM2.db` is
  above the highest state the build knows (15 in both), KDF Reloaded stops at start-up with an error that names both
  numbers. GLEEC KDF starts, changes nothing and uses the database as a state-15 database without a warning. No
  released version of either records a state above 15; two unreleased reference development branches each record a
  different state 16, so that value identifies no layout. The refusal is compatible with GLEEC KDF: a database KDF
  Reloaded leaves at state 15 is fully migrated for both. No switch exists; row in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.44 R44.9A.1 to R44.9A.5.
- **The swap's secret is checked against its hash on every coin family.** A taker, and a watcher acting for one,
  uses a value read from the transaction that claimed the taker payment as the swap's secret only when it matches
  the swap's secret hash (RIPEMD-160 of SHA-256 for a 20-byte hash, SHA-256 for a 32-byte one); otherwise the read
  fails as an extraction error. For ETH and ERC20 coins KDF Reloaded also reads the secret from the swap
  contract's `ReceiverSpent` event in the transaction's receipt when the call input is not a direct claim call,
  and for Tendermint coins it examines every message of the transaction. GLEEC KDF's reading for each coin family
  is recorded in CRD ch.51 §51.6E. No message and no transaction a peer validates changes, so no switch exists;
  row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). Plan
  [`swap-chain-reading-correctness`](docs/plans/swap-chain-reading-correctness.md) step 2 (PR #488); CRD ch.51,
  status note of 2026-11-22 before R31.
- **An already-sent read that cannot decide stops the payment step.** Before a legacy swap sends its payment it
  asks the paying coin whether that payment already exists. For EVM coins and tokens, QRC20 tokens and Sia, KDF
  Reloaded answers an error that names the condition when the answer cannot be decided, and the payment step fails
  without a broadcast; the manual recovery gets the same error. GLEEC KDF answers these cases differently (CRD ch.39
  §39.11.5A). No switch exists. Row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.39
  D39.11.5A.1; PR #489.
- **A swap resumed at its payment step repeats an already-sent check that cannot decide.** In KDF Reloaded, a
  legacy swap that a restart resumes right before its payment repeats the check within a bound, and then waits
  without sending until the payment is found or the refund deadline passes. A swap that was not resumed at this
  step asks once. GLEEC KDF asks once (CRD ch.39 §39.11.5A). No switch exists. Row in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). PR #489.

- **Saved swap and order records are synced to disk when they are replaced.** Both write a saved swap through a
  temporary file and a rename. KDF Reloaded also forces the temporary file to disk before the rename and, on Unix,
  the folder after it, uses a temporary name unique per write (`<file name>.<process id>.<counter>.tmp`) instead of
  the fixed `<uuid>.json.tmp`, and writes saved orders the same way. Both ignore the other's leftover temporary
  files. No peer sees it, so no switch exists; row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
  CRD ch.44 R44.9A.10 to R44.9A.13.

- **A swap run that lost its swap lock stops.** Both keep a swap's lock in `<uuid>.lock` as a Unix time in seconds.
  GLEEC KDF rewrites it in place and keeps running, refreshing and finally removing it even after another runner
  took it over. KDF Reloaded replaces it in one step and checks before each refresh that it still holds its own
  value; when it does not, the run is stopped where it stands and leaves the file, and the swap is handed to the
  ordinary start of a saved swap, which resumes it if no other runner holds the lock after 40 seconds. The file's
  content is the same in both, so each respects the other's lock by its timestamp. No peer sees it, so no switch
  exists; row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.51 R5, ch.44 §44.9A.

- **A Tendermint swap takes only a successful claim of its payment as the claim.** Both find the claim of a
  Tendermint swap payment by a transaction search for the payment's HTLC id. KDF Reloaded pages through the results
  and takes the oldest one that succeeded and holds a claim message for that HTLC in any of its messages; it skips
  the others. GLEEC KDF reads the search result differently (CRD ch.51 R91). No peer sees it, so no switch exists;
  row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). CRD ch.18 R18.11.16 and R18.11.19.

- **The refund gate of SLP, QRC20, ETH and ERC20 swap payments reads the chain's time.** KDF Reloaded judges whether
  such a payment can be refunded now by the chain: an SLP token by the median time past of BCH, a QRC20 token by that
  of QTUM, an ETH or ERC20 coin by the latest block's timestamp once the local clock is past the lock time (then a
  wait is at most a minute at a time). GLEEC KDF's rule for each coin family is recorded in CRD ch.51 §51.6E R88. A
  refund can therefore start later than on GLEEC KDF (for an SLP token up to about an hour); no message and no
  transaction a peer validates changes, so no switch exists; row in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). Plan
  [`swap-chain-reading-correctness`](docs/plans/swap-chain-reading-correctness.md) step 3 (PR #490); CRD ch.51,
  status note of 2026-11-22 after §51.6E R92.

- **A QRC20 swap's spend or refund is recognized only from its own successful contract call.** KDF Reloaded takes a
  transaction as the spend (refund) of a QRC20 swap payment only when the `receiverSpend` (`senderRefund`) call of
  the swap goes to the swap contract the payment used and the receipt of that same output reports no exception and
  holds the swap contract's `ReceiverSpent` (`SenderRefunded`) event for the swap id; it looks for a refund in the
  history of the payment's sender, so a refund of the other side's payment is found as well. GLEEC KDF's search is
  recorded in CRD ch.51 §51.6E R89. No message and no transaction a peer validates changes, so no switch exists; row
  in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md). Plan
  [`swap-chain-reading-correctness`](docs/plans/swap-chain-reading-correctness.md) step 4 (PR #496); CRD ch.51,
  status note of 2026-11-22 after §51.6E R92.

### Removed / disabled in KDF Reloaded

- iOS build target removed from the alpha release matrix.
- **Solana (SOL, SPL tokens) coins are wallet-only.** In 0.5.0 Solana can be
  activated again (earlier releases refused it on every route): on Reloaded's
  own `enable_solana_with_tokens` and `enable_spl`, and on
  `experimental::enable_solana_with_assets` and
  `experimental::enable_solana_token`, the methods GLEEC KDF `v3.0.0-beta`
  serves, in native and browser (WASM) builds. The legacy `enable` /
  `electrum` reject a Solana coin as one that needs an mmrpc 2.0 activation,
  as GLEEC KDF does (CRD ch.55 R55.4.7). Activation, balances, `withdraw`,
  message signing and `disable_coin` work. Orders and swaps do not: no GLEEC
  KDF release has a Solana swap, and placing an order with a Solana coin
  there ends abnormally (CRD ch.40 R40.6B.6, F40.6B.3). Reloaded
  refuses `setprice`, `buy`, `sell` and `trade_preimage` for a pair with a
  Solana coin or SPL token with "Trading is not supported for coin …", and
  every swap operation of such a coin answers an error that names it (CRD
  ch.60 R60.6.5). No switch exists: an abnormal end is no compatibility
  target. Reloaded has a Solana and SPL transaction history of its own,
  started by `tx_history: true` at the platform's activation; GLEEC KDF has
  none (CRD ch.40 §40.6C, D40.4).
- **TRON (TRX, TRC20) coins are wallet-only.** Since 2026-11-20 TRON can be
  activated again (0.4.0 refused it on every route): on the
  platform-with-tokens activation, `enable_eth_with_tokens` and the
  `task::enable_eth::*` family, where GLEEC KDF `v3.0.0-beta` serves it, and on
  the legacy `enable` / `electrum` methods as a Reloaded extension, which GLEEC
  KDF refuses for TRON, so a client written for GLEEC never sends such a
  request (CRD ch.21 §21.4, ch.55 D55.5). Activation, balances, addresses,
  `withdraw` and the transaction history work. Orders and swaps do not:
  Reloaded has no TRON swap, and neither has a GLEEC KDF release. `setprice`,
  `buy`, `sell` and `start_swap` refuse a TRON coin with the usual "… is wallet
  only" answer, a TRON coin declines to match, an order with a TRON coin
  saved earlier is kept at startup but never announced or matched, and an
  unfinished saved swap that names
  a TRON coin is not resumed at startup (its record is kept, with a line in the
  log). This holds whatever the coins-file entry says. For an entry with
  `"wallet_only": true`, which every published TRON entry has, the answer is
  GLEEC KDF's. For an entry without it GLEEC KDF passes the wallet-only check
  and refuses the order at its balance-and-fee check, with an answer of the same
  form and no order; Reloaded refuses one check earlier, and its text names the
  wallet-only ticker. No switch exists: the build constant
  `TRON_SWAPS_AVAILABLE` (`coins/lp_coins_ops.rs`) is `false` (CRD ch.21 R-D1,
  R21.10B.9, R21.B3). Gasless TRC20 transfers through GasFree are available:
  the provider setting of the activation, the `gasfree_address` of the
  reports and the gasless withdraw. The direct `withdraw` returns a signed
  authorization the node does not hand to the provider; the task form
  submits it and follows the transfer (CRD ch.63 §63.3.1 to §63.3.6, §63.8A,
  §63.8B).
- **A TRON coin answers "not supported" for the EVM-only methods.**
  `get_raw_transaction`, `sign_raw_transaction`,
  `get_eth_estimated_fee_per_gas`, `stream::fee_estimator::enable`,
  `get_token_info` for an `ERC20` protocol on a TRON platform, the legacy
  `get_trade_fee` and `trade_preimage` with a TRON coin on the receiving side
  have no TRON implementation. Reloaded refuses a TRX or TRC20 coin in each of
  them with a typed "not supported" error before any request to a node; an
  EVM coin is unchanged. GLEEC KDF fails five of these methods for a TRON
  coin as well, without a node request: `trade_preimage` with the same
  error, the legacy `get_trade_fee` with the same legacy error body and
  another text, the other three with other error types (for example
  `Transport`, HTTP 502, from `get_raw_transaction`). With a gas-fee
  provider it answers
  `get_eth_estimated_fee_per_gas` with Ethereum's fees under the TRON ticker;
  it also accepts `stream::fee_estimator::enable`, and the stream then
  reports errors or Ethereum's fees. Not answering another chain's fees for a
  TRON coin, and not starting a stream that can only report errors, are
  deliberate. The answers are local and no peer sees them, so no switch
  exists (CRD ch.21 §21.11 D7, R21.10B.14 to R21.10B.21).
- **A Tendermint activation refuses a hardware key policy.** A request that names the `Trezor` `priv_key_policy`
  fails before any node is contacted, with a `PlatformCoinCreationError` (HTTP 500) that says the hardware key policy
  is not supported for Tendermint coins and to activate with the local key; GLEEC KDF ignores the member and activates
  with the local key. An absent value, `ContextPrivKey` and `IguanaPrivKey` activate as before. Any other value, such
  as the WalletConnect form sent as this member (the WalletConnect forms of `activation_params` are built, see below),
  fails request parsing with `InvalidRequest` (HTTP 400). No switch exists.
  CRD ch.36 §36.7. PR #358.
- **A Tendermint activation under an HD login honors `priv_key_policy: IguanaPrivKey`.** GLEEC KDF accepts and ignores
  `priv_key_policy` for Tendermint coins; Reloaded activates the coin with the single internal key of the node, as the
  UTXO family does, and `path_to_address` then plays no part. An absent value and `ContextPrivKey` still use
  the key at `path_to_address`. No switch exists. CRD ch.36 §36.7. PR #396.
- **`priv_key_policy: IguanaPrivKey` under an HD login runs EVM coins, BCH with its tokens, Sia and Lightning on the node's
  internal key.** This is a Reloaded decision; the reference behavior is not recorded in the CRD, and no parity is
  claimed. Under a global-HD login `IguanaPrivKey` activates a single-address coin on the internal key (the HD context's
  key at the node-key path), as Reloaded's UTXO family does; an absent value and `ContextPrivKey` keep today's behavior,
  and an iguana login is unchanged. Lightning under this value is a separate identity with its own data directory.
  No switch exists. CRD ch.05 R46, ch.35 R35.1.4, ch.51 R64, status notes of 2026-11-17.
- **A Tendermint coin activated from a public key is watch-only, and its orders are refused.** The `WithPubkey` form
  of `activation_params` builds a coin that holds no private key: it answers `withdraw` and the staking requests with
  an unsigned document for the caller's own wallet (a `sign_doc`, or with `is_ledger_connection: true` the Amino form
  with three times the fee for the coin, not for a token), as GLEEC KDF does, and refuses every signing path with `PrivKeyNotAllowed::WatchOnlyCoin`.
  Where it differs: GLEEC KDF accepts orders for such a coin when the Ledger flag is off; KDF Reloaded refuses them for
  both forms (CRD ch.18 R18.11.23a), because it would announce a key the coin cannot sign with. An `ed25519` key gets
  the account of the public Cosmos SDK rule for that key type, where GLEEC KDF applies the secp256k1 rule to it. A
  malformed key is `InvalidRequest`, where GLEEC KDF aborts the request. No switch exists. CRD ch.36 §36.14.3, D36.17,
  D36.18; ch.18 D18.11.10. PR #382.
- **A Tendermint coin activated with a WalletConnect session is signed by the wallet, with a typed signing error
  and a checked account.** Both forms (`{"type": "WalletConnect", "params": {"session_topic": ...}}` and the public
  docs' `{"priv_key_policy": {"type": "WalletConnect", "data": ...}}`, which GLEEC KDF rejects) activate the coin of
  the paired session; `withdraw` and the staking requests are signed in the wallet (Amino for a Ledger-backed account,
  where the Ledger fee factor and the bank-send-only rule are as for the public-key form), and `withdraw` answers the
  signed transaction without broadcasting it, as GLEEC KDF does. Where it differs: (1) a wallet that refuses, does not
  answer, or a session that is gone or no longer grants the chain fails with the new `SigningError` (HTTP 500), where
  GLEEC KDF answers `InternalError`; (2) the key record that supplies the public key and the Ledger flag is the one of
  the coin's chain, where GLEEC KDF takes the first record whatever its chain, and when there is none the wallet is
  asked for its accounts with no parameter, where GLEEC KDF sends the session's account address as the parameter;
  (3) the address derived from the key must be one of the session's accounts for the chain, otherwise the activation
  fails with `PlatformCoinCreationError`, where GLEEC KDF does not compare; (4) in the direct sign mode the
  transaction body the wallet echoes must equal the one that was requested, where GLEEC KDF assembles the transaction
  from the echo without comparing; (5) orders of such a coin and of its tokens are refused as wallet-only, where GLEEC
  KDF accepts them (swaps are not signed through the wallet); (6) the fee and gas limit of a request the wallet signs
  come from the node's gas simulation (gas limit = simulated gas x 1.5, fee = limit x gas price; an explicit gas limit
  is kept), where GLEEC KDF uses a fixed assumed gas of 125,000, which ran out of gas for a real send and lost the
  fee, and a failed simulation fails the request; `fee_details` reports the fee of the transaction the wallet signed.
  A test with a real wallet is still to be run. No switch exists (a fixed limit that runs out of gas loses the fee).
  CRD ch.36 §36.14.2, D36.15, D36.16; ch.18 D18.11.10.
- **A Tendermint coin's transaction history differs from GLEEC KDF in three ways.** (1) While the first read of the
  chain runs, the history state is `InProgress` (with `{"transactions_left": n}`); GLEEC KDF reports `NotStarted`
  until the first read ends (§36.14.1 R36.14.11), and a failed first balance read ends its fetcher silently where KDF
  Reloaded retries it (§36.14.1 finding 4).
  (2) A fee-only transaction counts its fee once (GLEEC KDF counts it twice), a reward claim counts the fee it cost
  (GLEEC KDF leaves it out), a transaction that touches several validators gets one row per validator (GLEEC KDF one
  row) (all three: §36.14.1 finding 5), and `tx_hex` holds the signed transaction (GLEEC KDF the first message, which
  cannot be broadcast; D36.12). (3) A denomination resolves as at GLEEC KDF (§36.14.1 finding 7): the platform's `denom`
  exactly, otherwise the first coins-file entry with that denomination whatever its platform, so a coins-file token
  of another platform is counted and shown under the entry's ticker (the activated token's ticker when one is
  activated); a custom token absent from the coins file is not counted and has no history in 0.5.0. When what the
  coins file resolves changes, the history store of the platform coin is read again from the chain. A fee that is
  not one coin of the platform denom enters no amount (the platform entry is read wherever it stands in the fee
  list), and a transaction with an empty fee list, and a staking event without a `delegator` attribute when the
  transaction has exactly one staking message of that kind, get rows, where GLEEC KDF writes none; for these
  transactions amounts and rows can differ from GLEEC KDF's (§36.14.6 D36.19, D36.20). The row identifier and the stored form
  are meant to be the same as GLEEC KDF's (the position counting follows the cleared §36.14.6; checked in the lab on 2026-11-15 and 2026-11-16 with the Komodo DeFi Framework 2.6.0-beta reference build as the other writer, in both directions: the same rows, identifiers and address-index entries from a fresh store written by either program, from each program continuing the other's store, and from a store written by an earlier head and backfilled; GLEEC KDF 3.0.0-beta (it refuses the lab network) was not run), so a data directory can move between
  the two (§36.14.1 findings 1 and 2). No switch exists. Native builds only. CRD ch.36
  §36.14.1 (D36.10, D36.11, D36.12, D36.13, D36.19, D36.20, D36.21). A history database is per coin and shared
  by every account activated on it, so rows of an earlier account stay and are listed under their own addresses,
  as at GLEEC KDF (D36.23). A transaction whose block time no node gives is read again by the next pass
  (D36.24; GLEEC KDF leaves it out of that pass). PR #363.
  The table of addresses that GLEEC KDF needs in order to list a history row is written for every coin the SQLite history
  store serves (BCH and SLP tokens too), not for Tendermint coins only, and the BCH/SLP fetcher prepares it at start
  (rows stored earlier get their entries, entries without a row are removed); the listing of BCH and SLP tokens is
  unchanged. CRD ch.36 R36.14.23 -- R36.14.26. PR #380. A comparison of a BCH store (one transaction) with the one the Komodo DeFi Framework 2.6.0-beta build writes was run on
  2026-11-16 with equal results; an SLP token store is still to be compared.
- **Tendermint (Cosmos) coins and tokens are wallet-only in this beta.**
  Activation, balances and `withdraw` work; `setprice`, `buy`, `sell` and
  `start_swap` refuse the coin with the usual "… is wallet only" answer, and an
  order with a Tendermint coin saved by an earlier version is kept at
  startup but never announced or matched. A swap already in progress still
  completes or is recovered. Orders and swaps are planned to return in a later
  release. No switch exists: the build constant `TENDERMINT_SWAPS_AVAILABLE`
  (`coins/lp_coins_ops.rs`) is `false`. When they return, three points will
  differ from GLEEC KDF (CRD ch.18 §18.11): a token's sender-side trade fee and
  its fee to send the DEX fee are labeled with the platform coin, which pays
  them, where GLEEC KDF labels them with the token; a chain without an HTLC
  module is refused when an order is placed, where GLEEC KDF fails at the first
  payment unless the coins file marks the coin wallet-only; and a counterparty
  whose Tendermint order carries protocol information, where an IBC route would
  travel, is declined, where GLEEC KDF accepts any. PR #299.
  Two more cases are wallet-only whatever the switch says, and are refused when
  an order is placed. A custom Tendermint token, activated from an inline
  `protocol` in `tokens_params` or in `enable_tendermint_token` and absent from
  the coins file, is wallet-only, as a custom EVM token is. A Tendermint coin
  or token whose activation policy holds no local private key is wallet-only,
  so this node's own key can never stand in as the coin's HTLC public key on
  the legacy swap path; a token takes the policy of its platform coin. No
  activation can produce such a policy yet, so the second refusal only guards
  the policies that come later. No switch exists. CRD ch.36 R36.1.2, ch.35
  R35.2.7. PR #319.
- **A node does not watch swaps of UTXO, BCH and QTUM coins.** The checks a
  watcher runs are not implemented for these coins yet, so a node with
  `is_watcher` set does not subscribe to their watcher topics. GLEEC KDF
  watches swaps of these coins, and does not watch Siacoin swaps, which KDF
  Reloaded does. No switch exists. Watching for these coins is planned to
  return with the taker-side watcher hand-off. See
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md) and
  [CRD ch.09 R17](docs/reloaded-rewrite/09-watcher-reward-infrastructure.md).

### Work in progress

- Hardware wallet — Ledger transport (scaffolding only; crate not built into any artifact).
- Siacoin V1 atomic-swap operations. Supported (lab-verified 2026-11-04: seven
  swaps completed on the live chain, and a refund between two KDF Reloaded
  nodes). SC negotiates its own ed25519 HTLC key (ch.51 R-S9), gets the
  CRD-dictated 32-byte secret hash whenever it is on either side of the pair
  (ch.51 R71/R72), and has completed real send/spend/refund swaps against UTXO
  counterparties (confirmed live, both directions). Limits with Komodo KDF and
  GLEEC KDF nodes: see the changelog's Known limitations. The swap-spend search walks the HTLC
  address's event log and tells a spend from a refund (ch.20 D3, closed).
  Verified further on 2026-11-10 between two KDF Reloaded nodes: the maker-side
  refund of an SC payment (automatic, gated by the Sia node's median block
  time), the automatic refund of a swap resumed at start, and both directions
  of an SC/RIN swap on netid 6133 with that network's fee schedule. Manual
  recovery was tested only on swaps whose record already held the final event
  (it refused and broadcast nothing); the chain-reading recovery of a Siacoin
  payment has unit tests only.
  Still open: the V2 swap protocol is not implemented for SC (ch.20 D5,
  V1 only); a watching node can refund but not spend a Siacoin payment for
  the taker (ch.20 D7); the fixed 0.00002 SC swap fee is a watch item (ch.20
  R-S11); Siacoin in a browser (WASM) build is untested.
- The hash of the passphrase text under an HD login. Each coin swaps with its own key, and the node trades under the internal key, the key `get_public_key` reports, as in GLEEC KDF (CRD ch.05 §5.12C and §5.12D; PRs #250, #254). KDF Reloaded still keeps the passphrase hash for three things: the swaps an earlier build started with it, a Lightning node's seed, and the activation of BCH with its tokens, until plan item H7 moves that to the coin key. GLEEC KDF never uses that hash under an HD login. Tendermint is no longer on the list: under an HD login `enable_tendermint_with_assets` and `task::enable_tendermint` derive the coin's key from the seed at the coin's `derivation_path` extended by `path_to_address` (default account 0, `External`, address 0), as GLEEC KDF does and as other wallets derive a Cosmos address (`m/44'/118'/0'/0/0` for ATOM), and the coin's public key is derived from that key. A coin configuration without `derivation_path`, or a `path_to_address` that cannot be derived, fails the activation with `PlatformCoinCreationError`. An iguana login does not change. There is no switch. The address of a Tendermint coin under an HD login changes at the upgrade, so funds on the old address are moved first (see the changelog's upgrade note). Plan [`hd-key-unification`](docs/plans/hd-key-unification.md) and [`tendermint-activation-features`](docs/plans/tendermint-activation-features.md); CRD ch.05 R46, ch.36 R36.1.2 and R36.1.4 step 3. PR #319.
- **Legacy `enable` and `electrum` under an HD login.** Today KDF Reloaded binds such a coin to the internal key and answers with that key's address. GLEEC KDF builds the coin with the coin's own HD key at `path_to_address` under its `derivation_path`, registers it, and then fails the response; the coin stays enabled. Planned: KDF Reloaded binds the coin's own HD key in the same way and answers successfully with its address. A compatibility switch, on by default, keeps that answer; switched off, the node fails the response as GLEEC KDF does. The switch is named when the work lands. In both positions the address such a coin shows changes at that upgrade, so funds on the old address are moved to an HD address first. A coin configuration without `derivation_path` fails this activation at GLEEC KDF; KDF Reloaded's answer for it is settled with the work. Plan [`hd-key-unification`](docs/plans/hd-key-unification.md), item H7; CRD ch.55 R55.4.10.

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

**From v0.4.0 Reloaded also pays Pirate1 addresses.** Pirate Chain's own wallets now
hand out Pirate1 (Ironwood pool) addresses by default and pay from Ironwood notes.
Reloaded's ARRR wallet stays Sapling-only, but it accepts a Pirate1 address in
`validateaddress` and as a `withdraw` destination. It pays from its Sapling notes, at
Pirate Chain's fixed fee of 0.0001 ARRR, with the change kept in Sapling (CRD ch.39
§39.6.5). Payments that come from Ironwood wallets arrive like any other (R39.8.0bb).
The coin's `consensus_params` key `hrp_ironwood_payment_address` can name another prefix
for its Pirate1 addresses; absent, the built-in Pirate1 prefix applies. GLEEC KDF builds
nothing the post-Ironwood chain accepts, so no setting reproduces its side.

Because of that, the swap freeze and the build refusal below no longer engage in this
build, and `ironwood_gate_disabled` has nothing left to switch off. It is still
accepted, and still describes the older behaviour: for a build that cannot construct
version 6, the default `false` stopped entering new swaps for a coin with a known
Ironwood activation 160 300 s before it and refused to build any of that coin's
transactions from it onwards (CRD ch.39 R39.6.4b/R39.6.4c). The gates protected a counterparty whose HTLC would otherwise be
left unspendable and unrefundable across Pirate Chain's Ironwood upgrade
(3 Oct 2026 19:00 UTC). The settings below are the ones to set for GLEEC-compatible
behaviour; [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md) lists every setting.

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

**From v0.4.0 an ARRR swap starts only with a node that declares version-6 capability.**
An ARRR node adds a capability word to its order and match messages, and declines a
counterparty without it before any swap starts (CRD ch.39 R39.6.4f, ch.59 §59.4A). A swap
with a node that cannot build version 6 used to start, and failed at that node's first
ARRR transaction, after the DEX fee was paid. GLEEC KDF nodes and KDF Reloaded
0.3.0-beta.1 nodes send no word and are declined. Set `require_v6_counterparty` to
`false` in the coin's `consensus_params` to admit them again; an ARRR swap with GLEEC KDF
still cannot complete. The capability check is temporary: it will be removed once
version 6 is generally supported.

**From v0.4.1, orders that cannot be matched are not listed.** GLEEC KDF lists every order
of a pair. KDF Reloaded leaves out an order whose maker does not declare a capability this
node requires for one of the order's coins: version-6 capability for ARRR, suffix signing
for BC2 (CRD ch.59 §59.4C). This applies to `orderbook` and `best_orders` (legacy and
mmrpc 2.0), to the order-book event stream and to the `orderbook_depth` counts the node
takes from its own book; counts and volume totals cover the listed orders. `best_orders`
reads the pair's full book when the best-priced orders are left out, and `buy` or `sell`
that names only such orders in `match_by` fails at once instead of timing out. Such an
order used to be listed, often at the best price, and a taker that picked it got no swap.
`require_v6_counterparty: false` and `require_suffix_counterparty: false` list the orders
again, together with admitting those counterparties; while a requirement is on there is no
request that lists them. Other nodes are not affected: no message changes, and a KDF
Reloaded relay still passes every order on. Counts that `orderbook_depth` gets from a relay
still include such orders.

Three coin-activation behaviours differ from GLEEC KDF by default (CRD ch.46
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

More behaviors, in the order book and its relaying, peer exchange, fee readings and fee
quotes, Electrum, the shielded history and trading under an HD login, differ from GLEEC KDF by
default, each with one top-level `MM2.json` switch. The value below gives the GLEEC-compatible
behavior, except for `swap_key_check`; the row of each switch in
[`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md) gives the full detail:

- `pre_trade_spend_check: false` — no pre-trade spend check.
- `spend_safe_min_volume: false` — a maker order without `min_volume` advertises the default of the
  coin floors, as GLEEC KDF does, instead of at least ten times the HTLC spend fee.
- `erc20_hd_activation: false` — `enable_erc20` refuses a token whose platform coin is activated
  on an HD wallet, as GLEEC KDF does. By default KDF Reloaded activates the token on the platform's
  enabled address.
- `swap_key_check: false` — restores the behavior of earlier KDF Reloaded builds, not GLEEC KDF's:
  under an HD login (`enable_hd`), a swap whose coin announces another key than the one it pays
  from (every EVM coin, a coin enabled with a single key, a UTXO coin activated away from its first
  address) is negotiated and fails in the coin layer, instead of being refused up front. GLEEC KDF
  has no such check and needs none: its swaps announce the key the coin signs with (CRD ch.05 §5.9B
  R34), so these pairs trade normally there. Since plan item H3 (PR #250) Reloaded's swaps announce
  that key too, so the check refuses nothing; `false` only turns off the guard.
- `fee_estimate_cache_secs: 0`, with no per-coin value — no fee reading is reused between
  requests.
- `electrum_spread_reads: false` — every read-only Electrum query goes to the highest-ranked
  connected server first. The near factor `electrum_spread_near_factor` and the near offset
  `electrum_spread_near_offset_ms`, which limit spreading to the servers with the shortest round
  trip (with spreading on, reads always take turns among at least the two fastest connected
  servers when two or more are connected), then have no effect.
- `electrum_server_order: "listed"` — a coin's Electrum servers are ranked in the order they are
  listed instead of by measurement (`mixed`, the default since 2026-11-14; `random` gives the
  earlier default, a random draw).
- `solana_node_order: "listed"` — a Solana coin's nodes are used in the order they are listed
  instead of in a random order drawn once when the coin is enabled.
- `electrum_tip_checks: false` — the node does not compare the block heights of a coin's Electrum
  servers, does not fail an activation or refuse trades when they disagree about the chain: a server
  that is behind the chain or stopped following it keeps its place in reads, broadcasts and a swap's
  confirmation wait, which polls the first connected server in priority order. Together with the two
  switches above this gives GLEEC KDF's routing. The `electrum_servers` request has no GLEEC KDF
  equivalent.
- `orderbook_sync_names_held_root: false` — a sync request names the root the maker advertised
  instead of the one this node holds.
- `order_messages_refresh_liveness: false` — only keep-alives keep a maker's order book alive.
- `sync_answer_deferral_ms: 0` — a sync request for a pair this node is still synchronizing is
  answered at once.
- `sync_request_retry_ms: 0` — a failed sync request of this node is not asked again before the
  maker's next keep-alive.
- `sync_request_alternate_relay: false` — a failed or outdated sync answer is not followed by a
  request to another relay before the maker's next keep-alive.
- `sync_answer_deferral_before_placeholder: false` — a relay answers at once a sync request for a
  maker's pair it has not started synchronizing yet, even while it is handling that maker's
  keep-alive.
- `orderbook_answer_waits_for_worker: false` — an order-book request fails at once when a listed
  maker has no record in the node's order-book store yet, instead of waiting up to 250 ms.
- `cancel_final_window_secs: 120` — a received cancel keeps the order out for 120 seconds only, so
  a relay that missed the cancel can bring the order back after that.
- `maker_restore_skips_cancelled: false` — at start every stored active order is restored,
  including one whose record outlived its cancel.
- `pair_liveness: false` — a pair the maker stops advertising is kept as long as the maker stays
  online, so a cancelled order this node missed stays visible until the maker's whole record expires.
- `relay_second_copy_priority_only: false` — a relay sends every message it forwards a second
  time, not only cancellations, swap negotiation and messages outside the order book.
- `p2p_resolve_peer_hostnames: false` — a hostname a peer gives in its list of peer addresses is
  skipped without a lookup, and this node's answers give a peer it dialed under the dialed address,
  hostnames included.
- `shielded_history_unmined_sends: false` — an own ARRR send is listed in the history only once
  mined.
- `suppress_unchanged_order_updates: false` — every `update_maker_order` is broadcast, as the GPLv2
  baseline does; what GLEEC KDF does here is not established.
- `evm_taker_fee_quote_at_limit: false` — the fee to send an EVM taker fee is quoted at the gas
  estimate, as GLEEC KDF does and as KDF Reloaded did before this switch, instead of at the gas
  limit the fee is sent with; both send it with that limit.

One more switch, `maker_update_coalescing`, already defaults to the GLEEC-compatible `false`:
every maker-order message is published at once, and `true` turns on the maker's rate control.

`setprice_replace_after_checks` also already defaults to the GLEEC-compatible `false`: a
re-pricing cancels the previous orders before its checks, and `true` keeps them when a check
fails (at the price of a stale price on the book).

`wc_session_persistence` defaults to the GLEEC-compatible `open` (plaintext WalletConnect
sessions, byte-interchangeable with GLEEC KDF); `none` turns session persistence off.

`netid` has no GLEEC-compatible default: GLEEC KDF's default netid `0` is not supported, so a
node with `netid` unset or `0` refuses to start; set `8762` or `6133`.

`dns_resolver` has no GLEEC-compatible value: the default matches GLEEC KDF for seeds and
`"system"` matches it for coin servers; see its row in
[`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).

`ntp_servers` set to `[]` turns off the clock check against NTP time servers, a KDF Reloaded
addition; see its row in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).

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
| Addresses of the transactions the node builds (a withdrawal's recipient, a swap's script and fee addresses), shown in the history | `<TICKER>_RELOADED_TX_ADDRESSES.db` | none |
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

#### Upgrade of a shielded wallet database and its backup

When a new KDF Reloaded build needs a newer wallet-database schema than the one a
`<TICKER>_RELOADED_WALLET.db` was created with, the first activation of that coin upgrades the
file. Before it changes anything, the node:

- checks that the disk holding the database has room for the backup and for the working copy:
  twice the size of the wallet database plus 16 MiB. If not, the activation fails with an error that
  gives the bytes needed and the bytes available, and no backup is written. Free some space and
  activate again;
- writes a consistent copy of the database next to it, named
  `<TICKER>_RELOADED_WALLET.db.pre-upgrade.<milliseconds>.bak`, in the same data directory. It is
  about as large as the wallet database;
- checks the copy (SQLite integrity check, same table layout as the original). If the check fails,
  the copy is deleted, the wallet database's content stays as it was, and the activation fails with an error
  naming the check.

The upgrade then runs on a second copy made from the verified backup, and replaces the wallet
database only when the result checks out. If the upgrade itself fails, the original is set aside as
`.upgrade-failed.<milliseconds>.bak` and a fresh wallet is scanned from the chain, as before.

*Going back to an older KDF Reloaded build:* stop the node, copy the `.pre-upgrade.<milliseconds>.bak`
file over `<TICKER>_RELOADED_WALLET.db`, delete `<TICKER>_RELOADED_WALLET.db-wal` and
`<TICKER>_RELOADED_WALLET.db-shm` if they exist, and start the older build. The older build then
sees the wallet as it was before the upgrade.

The upgrade of the Zcash libraries (wallet database `zcash_client_sqlite` 0.22 to 0.23) is such an upgrade: a wallet database
of the earlier generation is backed up and migrated on the first activation, and the older build cannot open the migrated
file. Put the backup back as described above to return to it.

*How many backups stay:* after a successful upgrade the node keeps only the newest
`.pre-upgrade` backup of that wallet and deletes older ones. The top-level `MM2.json` key
`shielded_db_upgrade_backups` (a whole number, default `1`, lowest effective value `1`) changes the
number; the backup just made is always kept. Other `.bak` files, such as `.upgrade-failed` and
`.legacy-v2`, are never deleted by the node.

#### Shielded wallet files per key, and `priv_key_policy`

The shielded activation request accepts `priv_key_policy` in the shape the other coin families use. This is a
KDF Reloaded addition: whether GLEEC KDF reads it for a shielded coin has not been established here, and the
legacy/GLEEC database files are not involved either way. Under a global-HD login `IguanaPrivKey` selects the
node's single internal key (shielded and transparent); leaving it out keeps the HD key.

Each key keeps its own wallet. `<TICKER>_RELOADED_WALLET.db` (with `<TICKER>_RELOADED_COMPACT_BLOCKS.db` and
`<TICKER>_RELOADED_TX_ADDRESSES.db`) stays with the key whose account it holds, so a wallet created by an
earlier build is found unchanged. The default key (the single key, or the HD key of account 0) uses these
names when no wallet exists yet; any other key uses the same three names with a 16-character fingerprint of
its viewing key before `.db`, for example `ARRR_RELOADED_WALLET.0123456789abcdef.db`. The fingerprint is a hash;
the viewing key is not in the name. Balance and history show the active key's wallet only. The backup before a
schema upgrade (see above) covers every one of these files as it is opened.

An activation is refused, with a message that names the swaps and says what to do, while swaps of the coin that
are not finished were started with another key, and when a wallet file of the coin cannot be read while a swap
still has to send its payment or fee. Changing the HD `account` alone is allowed.

## Forward-compatibility commitment

KDF Reloaded commits to following GLEEC KDF's evolution:

- We monitor GLEEC KDF releases and incorporate compatible changes when they are legally and technically feasible.
- For changes we *cannot* incorporate verbatim (licensing, project-direction, or quality reasons), we still expose a per-setting opt-in so the GLEEC-equivalent behaviour remains reachable, and we list that setting in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md).
- The only exception is a behaviour that would either violate GPLv2 or directly contradict the project's principle of free, open trading at reasonable cost. In that case the divergent setting defaults to the KDF Reloaded behaviour and the original-compatible value is gated behind an explicit acknowledgement with a runtime warning. See the developer rule in [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md).
