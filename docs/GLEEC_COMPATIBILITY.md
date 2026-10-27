# Running KDF Reloaded in full GLEEC-KDF compatibility

This chapter is the single place that lists every configuration value an operator must set to make a KDF Reloaded node behave equivalently to a GLEEC KDF node, for the benefit of operators migrating an existing GLEEC KDF deployment or third-party integrations that target the GLEEC behaviour.

The convention behind this chapter — including the developer rule that requires every divergent change to land an entry here — is documented in [`COMPAT_SWITCHES.md`](COMPAT_SWITCHES.md).

## How to use this chapter

1. Start from your existing `MM2.json` (or whichever configuration surface you use).
2. Walk down the table below. For each row, set the listed value if you want the GLEEC-equivalent behaviour for that area.
3. Skip any row whose KDF Reloaded default already matches what you want; rows are listed exhaustively, not by impact.
4. For acknowledgement-gated settings (marked **⚠ gated**), additionally set the listed acknowledgement key — and read the per-setting documentation linked from the row, because these settings select behaviour the project does not endorse but still supports for compatibility.

There is no global "GLEEC mode" switch and no shared JSON object — every setting is its own thing in its own place. This chapter is the unifying reference, not a code construct.

## Settings

| Area | Setting | GLEEC-compatible value | Acknowledgement-gated? | Per-setting docs |
|------|---------|------------------------|------------------------|------------------|
| Network selection | `netid` | `8762` or `6133` — GLEEC's default netid `0` is **not supported** | No | [see below](#netid) |
| WalletConnect session storage | `wc_session_persistence` | `open` *(also the default)* | No | [CRD ch.22 §22.5](reloaded-rewrite/22-walletconnect-v2.md) |
| Shielded network upgrade | `ironwood_gate_disabled` *(coin config, `protocol.protocol_data.consensus_params`)* | **No GLEEC-compatible value exists any more.** KDF Reloaded builds the version-6 transactions Pirate Chain's Ironwood upgrade requires (below the activation height, version 4); GLEEC KDF cannot, and every transaction it builds for a post-Ironwood chain is rejected. The activation height is configured (`ironwood_activation_height`) or discovered at runtime (CRD ch.39 R39.6.4e). With version 6 supported the swap freeze and build refusal this switch used to suppress never engage, so the switch is accepted and does nothing; it does not disable version-6 construction. | No | [CRD ch.39 R39.6.4a–e](reloaded-rewrite/39-zcash---z_coin-shielded-coin.md) |
| Shielded (ZHTLC) coin backend | **none -- no switch exists** | Cannot be reproduced. GLEEC KDF serves a Light-mode ZHTLC coin's whole transparent side from the activation payload's `electrum_servers`; KDF Reloaded serves the coin from `light_wallet_d_servers` alone and ignores `electrum_servers`. `light_wallet_d_servers` must therefore be present and non-empty. | No | [see below](#zhtlc-backend) |
| EVM foreign-transaction decoding | **none -- no switch exists** | Cannot be reproduced. Reloaded refuses trailing bytes after a complete transaction and a typed envelope with an all-zero signature; the reference accepts both, folding trailing bytes into the identifier it reports and reporting a placeholder sender for the latter. No opt-back is offered: a switch would be an opt-in to weaken validation (`AGENTS.md` §5), and either acceptance lets a counterparty choose a value we report as fact. Accepting EIP-2930 / EIP-1559, the identifier and sender derivation, and refusing EIP-4844 / EIP-7702 are all **parity**. | No | [CRD ch.35 §35.6A](reloaded-rewrite/35-evm-v2-activation-rpcs.md), D35.1 |
| Shielded (ZHTLC) balance reads zero after migrating a wallet | **none -- no switch exists** | Not a divergence to configure: activating a shielded coin with no `sync_params` scans only a recent window, in both implementations. Pass `sync_params` to reach older funds. | No | [see below](#zhtlc-zero-balance-after-migrating-a-wallet) |
| Shielded withdrawal short because of an unconfirmed transaction | **none -- no switch exists** | Cannot be reproduced, and none is wanted. A Light-mode ZHTLC (ARRR) `withdraw` that is short only because its notes are committed to an earlier, unconfirmed transaction of the same node fails with the additive `error_type` `BalanceLockedByUnconfirmedTransaction` (HTTP 400, naming the coin, available, required and committed amounts); the reference reports the bare `NotSufficientBalance`. No wire format and no other node is affected, so by the second arm of [the default-side rule](COMPAT_SWITCHES.md#the-default-side-rule) Reloaded's variant is the default. A client that matches only on `NotSufficientBalance` sees a different token for this one case; after a node restart the bare `NotSufficientBalance` is reported again. | No | [CRD ch.39 D39.8.0e](reloaded-rewrite/39-zcash---z_coin-shielded-coin.md) |
| RIC and MEWC header layout (Electrum) | `header_layout_fallback_families` *(top-level `MM2.json` array of distinct `chain_variant` names)* | `[]` -- no structural fallback, so a header list the standard 80-byte layout cannot read is rejected as GLEEC KDF rejects it (its median-time-past for `RIC` and `MEWC` is then a wrong value; Reloaded's HTLC spend errors instead). Default `["BTC"]`: for a coin with no `protocol.header_layout` key whose family is listed (every such coin is `"BTC"` today), a list or lone header the standard read does not consume exactly is retried as Riecoin and as the mixed KawPoW/AuxPoW layout, the one exact fit wins (a list both consume is rejected as ambiguous until the coin is pinned), so `RIC` and `MEWC` swaps complete with the shared registry unchanged. A read the standard layout accepts is never retried, and a coin that carries `protocol.header_layout` (`"riecoin"` or `"kawpow_auxpow_mixed"`, which GLEEC KDF ignores) never falls back. A value that is not an array of distinct names from `BTC`, `QTUM`, `LBC`, `PPC`, `RICK`, `MORTY`, `RVN`, `PIVX` refuses the launch. Nothing a counterparty observes changes (no wire message, RPC shape, signed preimage or persisted schema). | No | [CRD ch.37 R37.5.9e](reloaded-rewrite/37-utxo-spv-and-block-header-validation.md) |
| Key export | `allow_insecure_key_export` | `true` — enables GLEEC-parity offline / no-activation / HD-range / shielded key export. Default `false`. | Yes | [CRD ch.07 §7.3A](reloaded-rewrite/07-wallet-lifecycle-and-key-export.md) |
| Sia activation result under an HD wallet | `sia_activation_single_address_balance` | `true` -- `wallet_balance` is always the single-address `"Iguana"` form, as GLEEC KDF reports it. Default `false`: under an HD wallet Reloaded reports the HD per-account report (`"wallet_type": "HD"`), like the other coins of an HD wallet. Both shapes carry the same single address and balance. | No | [CRD ch.46 R46.2.4](reloaded-rewrite/46-sia-v2-activation-rpcs.md) |
| Custom withdraw fee for Siacoin | **none -- no switch exists** | Cannot be reproduced, and none is wanted. KDF Reloaded accepts `fee: {"type":"UtxoPerKbyte","amount":"<n>"}` on an SC `withdraw` (SC per kilobyte, replacing walletd's estimate) and rejects any other `fee` type for SC with an invalid-fee-policy error; GLEEC KDF uses a fixed fee for SC and silently ignores `fee`. A client that sends a non-`UtxoPerKbyte` `fee` for SC and relied on it being ignored must stop sending it. A fee one tenth of the estimate was mined within about ten minutes on an uncongested transaction pool (checked live 2026-10-26; a full pool is untested). | No | [CRD ch.20 §20.9](reloaded-rewrite/20-siacoin-integration.md), [plan](plans/sia-withdraw-fee-control.md) |
| PIVX header version 7 | **none -- no switch exists** | Cannot be reproduced, and none is wanted. GLEEC KDF reads a 32-byte field after the nonce in every PIVX header, which misreads version-7 headers (80 bytes, observed on mainnet from height 2153200 to 2700499); KDF Reloaded reads that field only for versions 4 to 6 and 8 and above. No peer or wire value observes the difference; it only decides whether a header list is read. | No | [CRD ch.37 §37.5.8](reloaded-rewrite/37-utxo-spv-and-block-header-validation.md), [plan](plans/utxo-header-family-selection.md) |
| Legacy `orderbook` / `best_orders` with a shielded-coin order | **none -- no switch exists** | Cannot be reproduced, and none is wanted. GLEEC KDF's legacy `orderbook` fails the whole response for a pair that carries a shielded-coin (ARRR) order, and its legacy `best_orders` leaves such orders out. KDF Reloaded lists the order and renders its `address` as the fixed string `Shielded`, as the mmrpc-2.0 methods do with `{"address_type":"Shielded"}`; an order whose coin has no address derivation is left out on its own. A client of the legacy methods that parsed `address` as a real address must tolerate the word `Shielded`. | No | [CRD ch.32 §32.8](reloaded-rewrite/32-orderbook-p2p-and-trie.md), [gap 17](reloaded-rewrite/UPSTREAM-PARITY-GAPS.md) |
| Legacy `enable` after a failed balance read | `legacy_enable_keeps_coin_on_balance_failure` | `true` -- `enable` leaves the coin registered when its first balance read fails, as GLEEC KDF does (a retry then fails as already active). Default `false`: both `enable` and `electrum` deregister the coin so a retry can succeed. The switch only reproduces GLEEC's inconsistency between the two methods; it is not recommended. | No | [CRD ch.55 R55.4.6](reloaded-rewrite/55-coin-activation-wire-contract.md) |
| EVM transaction type for a coin with no `max_eth_tx_type` | `max_eth_tx_type_absent_unrestricted` *(top-level `MM2.json` boolean)* | `false` *(also the default)* -- a coin whose configuration has no `max_eth_tx_type` key may send untyped (legacy) transactions only, as GLEEC KDF does; an EIP-1559 swap transaction is then refused at signing (not downgraded). `true` opts into Reloaded's divergence: an absent key means *unrestricted* and the swap gas-fee policy alone decides the form. A key that is present is honoured identically either way. Only the boolean `true` turns it on. | No | [CRD ch.35 D35.7](reloaded-rewrite/35-evm-v2-activation-rpcs.md) |
| Unrecognised value of a coin-configuration enumeration key | `coin_conf_unrecognised_enum_falls_back` *(top-level `MM2.json` boolean)* | `true` -- a coin-configuration key that holds an unrecognised enumeration member (today only a UTXO coin's `signature_version`, accepted values `base`, `witness_v0`, `fork_id`, `fork_id_rxd`; RXD's shipped `fork_id_rxd` is recognised and is not affected) takes the key's default and the coin activates, as under GLEEC KDF; Reloaded additionally logs one warning per activation naming the key, the value and the default. Default `false`: the activation fails with an error naming the key and the value. An absent key or JSON `null` takes the default either way. Only the boolean `true` turns it on. A malformed or non-string `fork_id` is not an enumeration member and fails under either setting. No wire format and no other node is affected, so by the second arm of [the default-side rule](COMPAT_SWITCHES.md#the-default-side-rule) Reloaded's variant is the default. | No | [`RELOADED_VS_GLEEC.md`](../RELOADED_VS_GLEEC.md) |
| Qtum header family for a `QTUM`/`QRC20` entry without `chain_variant` | **none -- no switch exists** | Cannot be reproduced. When a coin entry's `protocol.type` is `QTUM` or `QRC20` and it carries no `protocol.chain_variant` key, KDF Reloaded reads the coin's headers (on every header read path) with the Qtum header family, selected from the protocol type; GLEEC KDF maps an absent key to the default 80-byte family for every protocol type. The shipped coin registry sets `"chain_variant": "QTUM"` on every such entry, so no deployed coin depends on the fallback; it only affects a hand-written entry missing the key. Recorded as a divergence; kept now that `chain_variant` is read (decided 2026-10-25). | No | [CRD ch.37 §37.5.1](reloaded-rewrite/37-utxo-spv-and-block-header-validation.md), [plan](plans/utxo-header-family-selection.md) |
| EVM token activation on an HD wallet | `erc20_hd_activation` | `false` -- `enable_erc20` refuses an HD-derived platform (HD mode; a hardware-backed platform is refused either way) with `UnexpectedDerivationMethod` (`"ExpectedSingleAddress"`), as GLEEC KDF does. Default `true`: on a software HD wallet the token is activated on the platform's enabled address and `balances` lists every known address. Single-address wallets and `enable_nft` are unaffected. | No | [CRD ch.35 R35.2.3, R35.2.5](reloaded-rewrite/35-evm-v2-activation-rpcs.md) |
| EVM node URLs in logs and errors | **none -- no switch exists** | Cannot be reproduced, and none is wanted. GLEEC KDF prints a node's full URL, including any path, query string or userinfo that carries an API key, in its EVM transport errors; KDF Reloaded names an EVM or Tron node by scheme, host and port only, and an error a node answered with ends with `(node <host>)` (with a warn log line `EVM RPC <host> answered <method> with error <code>: <message>`). KDF Reloaded names the URLs of the fee-estimation services it contacts (the `gas_station_url` and the gas-API provider URL) the same way, because they carry provider API keys too. Node access is not peer-visible and the change only protects credentials, so by the second arm of [the default-side rule](COMPAT_SWITCHES.md#the-default-side-rule) Reloaded's variant is the default, and an opt-in to print credentials would be an opt-in to weaken security. A script that parsed a node URL out of an error text no longer finds it. | No | [CRD ch.35 R35.1.14, R35.1.15](reloaded-rewrite/35-evm-v2-activation-rpcs.md), D35.1 |
| Pre-trade spend-safety check | `pre_trade_spend_check` *(top-level `MM2.json` boolean)* | `false` -- no check: an order whose smallest trade would leave a UTXO-family or Sia payment unspendable at the current fee is accepted, as under GLEEC KDF, and its swap can later fail in the coin layer. Default `true`: `setprice`, `update_maker_order`, `buy`, `sell` and `trade_preimage` refuse such an order with the volume-too-low error, giving the spend-fee estimate plus dust (Sia: the miner fee plus one hasting) as the threshold. Only a JSON `false` turns it off. Received orders, reservations and started swaps are never affected, and no advertised value changes, so by the second arm of [the default-side rule](COMPAT_SWITCHES.md#the-default-side-rule) Reloaded's variant is the default. | No | [CRD ch.60 R60.6.7](reloaded-rewrite/60-trading-floors-and-refusal-contract.md) |
| Own shielded (ARRR) send in the transaction history before it is mined | `shielded_history_unmined_sends` *(top-level `MM2.json` boolean)* | `false` -- `z_coin_tx_history` lists an own shielded send only once it is mined, as GLEEC KDF does. Default `true`: the send is listed from broadcast on, at the top of the history with `block_height`, `timestamp` and `confirmations` 0 and already with its final amounts (the change the wallet pays itself counted as received), then moves to its block's position once mined; a send that expires unmined disappears. Only a JSON `false` turns it off. Balances, note selection and other methods are the same either way, and no other node is affected, so by the second arm of [the default-side rule](COMPAT_SWITCHES.md#the-default-side-rule) Reloaded's variant is the default. | No | [CRD ch.39 R39.8.9i, D39.8.0h](reloaded-rewrite/39-zcash---z_coin-shielded-coin.md) |
| TRON (TRX, TRC20) and Solana (SOL, SPL) activation in this beta | **none -- no switch exists** | Cannot be reproduced in this beta. Every activation route of KDF Reloaded refuses these coins with "TRON support is not available in this build" or "Solana support is not available in this build". GLEEC KDF `v3.0.0-beta` activates TRON through mmrpc 2.0 methods and offers experimental Solana methods, neither of which KDF Reloaded has routed. The support is not finished; a switch would only let it be turned on unfinished. Both families are planned to return in the next beta (compile-time constants `TRON_ACTIVATION_AVAILABLE`, `SOLANA_ACTIVATION_AVAILABLE`). | No | [CRD ch.21](reloaded-rewrite/21-tron-integration.md), [ch.40](reloaded-rewrite/40-solana-coin.md), [ch.55 D55.5](reloaded-rewrite/55-coin-activation-wire-contract.md) |
| Tendermint (Cosmos) swaps in this beta | **none -- no switch exists** | Cannot be reproduced in this beta. Tendermint coins and tokens are wallet-only in KDF Reloaded: activation, balances and withdrawals work, but `setprice`, `buy`, `sell` and `start_swap` refuse them, they decline to match, and an order with a Tendermint coin saved by an earlier version is cancelled at startup. A swap already in progress still completes. GLEEC KDF lets Tendermint coins with an HTLC module (IRIS, Nucleus) take part in swaps, so a KDF Reloaded node does not trade them with GLEEC KDF peers during this beta; the order book still shows their orders. Swaps are planned to return in a later release (compile-time constant `TENDERMINT_SWAPS_AVAILABLE`). | No | [CRD ch.18](reloaded-rewrite/18-tendermint-ibc-htlc.md), [ch.36](reloaded-rewrite/36-tendermint-v2-activation-rpcs.md) |

### ZHTLC backend

There is **no setting** for this divergence, and this chapter says so rather than
naming one that does not exist.

KDF Reloaded serves a Light-mode shielded (ZHTLC) coin from `lightwalletd` alone
(CRD ch.39 §39.11). The Electrum support GLEEC KDF uses for the coin's transparent
side -- chain tip, transactions, balance, history, UTXOs and broadcast -- is
**removed**, not deprecated and not kept behind a switch. An opt-back would restore a
path that cannot be used: as of 2026-10-11 no host published for Pirate Chain (ARRR)
runs an ElectrumX service at all, and the two that did were serving a fork that
stopped at the first block of the Ironwood upgrade containing transactions. It would
also re-arm the two defects the new design closes: the wallet following an off-branch
endpoint's chain, and HTLC spends and refunds built from a different chain height than
ordinary sends, which are then born expired.

What an integrator observes:

- `light_wallet_d_servers` is **mandatory and must be non-empty** in the Light-mode
  `rpc_data`. An empty or absent list fails activation as a configuration error naming
  the coin (it no longer fails later as a scan error).
- `electrum_servers`, `min_connected` and `max_connected` are still **accepted and
  ignored**, so a payload written for GLEEC KDF activates unchanged. Absent, empty, or
  populated with unreachable servers all behave identically. A caller supplying a
  working Electrum list today is, in fact, supplying a list that no longer works.
- Every configured `light_wallet_d_servers` entry is **admitted before use**: its
  `GetLightdInfo` network name and Sapling activation height must match the coin, and
  its consensus branch must be the one the wallet already follows (or, for a fresh
  wallet whose configuration cannot name the active upgrade, the one a strict majority
  of at least two reachable endpoints reports). An endpoint on another fork is
  excluded, not merely warned about. **A fresh wallet therefore needs two agreeing
  reachable endpoints** to activate; a wallet that has scanned blocks needs only one.
  Activation fails with a typed error that distinguishes "every endpoint unreachable"
  from "every reachable endpoint excluded".
- `txfee: 0` (fee estimation) is refused for a ZHTLC coin: `lightwalletd` has no fee
  estimation. A coin with no `txfee` key, like the published ARRR entry, keeps the shared
  fixed default and activates as before.
- `requires_notarization` cannot be satisfied for a ZHTLC coin: `lightwalletd` has no
  notarized-depth field, and plain confirmation depth is not substituted for it.
- Swap-path HTLC spend detection and "payment already sent" detection run over
  `lightwalletd`'s transparent address index (CRD ch.39 R39.11.4d). The serving node files
  every output under the pubkey-hash key type, so a `b...` (P2SH) query is empty forever;
  Reloaded queries **both** the P2SH form and the `R...` re-encoding of the same twenty bytes
  and accepts a row only when its script is the expected P2SH script. These two decisions
  answer "not spent" / "not sent" **only** on positive or well-founded evidence (the index
  lists the output as unspent, or the funding transaction is not mined and a healthy endpoint
  returned nothing) and otherwise fail with a named error: an index-coverage failure, a
  skipped history height, a transport failure, or a spend the index did not deliver. GLEEC KDF
  answered these from Electrum. A swap that GLEEC would have continued on an empty answer
  therefore waits, or stops with an explicit error, instead. No opt-out exists, because the
  alternative is a "not spent" that can be wrong. Wallet functions -- balance, history,
  receiving, sending, withdrawing -- are unaffected.

**Compatibility:** GLEEC KDF has no equivalent of the endpoint admission check or of
the single builder-height authority; both are additive for a deployment whose endpoints
are all on the chain they claim, and the only observable change there is that HTLC
spends and refunds stop being built from a different height than ordinary sends.

### ZHTLC zero balance after migrating a wallet

**Symptom:** a shielded (ZHTLC) coin such as ARRR activates, the sync completes and keeps
pace with the chain, the logs look healthy, and the balance is zero or lower than expected.

**Warning.** Activating a shielded coin **without `sync_params`** scans only a recent
window below the chain tip. A wallet whose funds are older than that window shows a zero or
partial balance even though nothing failed. Ship this knowledge to your users: a wallet
that migrates an existing wallet must pass `sync_params`.

This is expected behavior, not a bug to report. GLEEC KDF behaves the same way with a
shorter window: it uses one day of blocks, worked out from the coin's `avg_blocktime`,
which is 1,440 blocks for ARRR. KDF Reloaded uses a fixed window of **2,880 blocks** for
every shielded coin. That is two days **at a 60-second block time**; on a chain with a
different block time the same block count spans a different interval. The window is
floored at Sapling activation.

**Why a migrated wallet hits it.** KDF Reloaded keeps its shielded databases under
distinct file names, `<TICKER>_RELOADED_WALLET.db` and `<TICKER>_RELOADED_COMPACT_BLOCKS.db`
(for example `ARRR_RELOADED_WALLET.db`). The files a GLEEC KDF node wrote
(`<TICKER>_WALLET.db`, `<TICKER>_COMPACT_BLOCKS.db`) are a separate namespace that Reloaded
does not open, rename, migrate, truncate or delete where they lie. So the first run on a
migrated node starts from a fresh wallet database, and the default window applies.
**Do not rename, copy or reuse the old database files under the new names.** Renaming them
does not fail; it simply gains nothing, because Reloaded fingerprints every database before
it opens one, and a recognized GLEEC-era wallet is preserved recoverably, replaced with a
new current-generation wallet and fully rescanned, reusing no note, nullifier, witness,
commitment tree or scan state. You land back on the same default window that caused the
zero balance, having risked the old file for no gain. Only a file Reloaded cannot recognize,
or a corrupt one, is preserved and refused with a typed activation failure.

**Remedy.** Activate with `sync_params` inside the Light-mode `rpc_data`
(`activation_params.mode.rpc_data.sync_params`; it is not a top-level field), chosen to
precede the wallet's first received funds. Exactly three forms are accepted:

| Form | JSON | Start |
|------|------|-------|
| height | `"sync_params": {"height": 2000000}` | that block height |
| date | `"sync_params": {"date": 1700000000}` | the block matching that Unix timestamp, in seconds |
| earliest | `"sync_params": "earliest"` | Sapling activation; slowest, always sufficient |

Each is floored at Sapling activation. When unsure of the first-receive date, use
`"earliest"`. The activation result's `first_sync_block` reports the start that was
actually used, which is the quickest way to confirm which window applied. A start that
differs from the wallet's current scan state rewinds and rescans, so correcting a wallet
that already activated with the default window is done by activating again with
`sync_params`.

### `netid`

KDF Reloaded compiles its per-network parameters at build time and recognises
only the netids in its registry — currently `8762` and `6133`. Any other netid
is **rejected at startup**: `lp_init` returns `MmInitError::UnsupportedNetId`
listing the supported networks, and the node does not start.

This includes the *inherited default*. Both KDF Reloaded and GLEEC KDF derive
`netid` the same way — an unset `netid` resolves to `0` — but GLEEC KDF treats
netid `0` as a live operating network, so a GLEEC node started without an
explicit `netid` simply joins netid `0`. KDF Reloaded has no compiled
configuration for netid `0` and therefore has **no equivalent to GLEEC's
default network**: it operates only on its compiled netids.

Consequences for operators and integrators:

- A GLEEC deployment that ran on the default netid `0` has no direct reloaded
  equivalent. Choose a supported network and set `netid` to `8762` or `6133`.
- Always set `netid` explicitly. Relying on the default is a startup error on
  KDF Reloaded, not a fallback to a public network.
- Test fixtures and tooling that spin up a node (anything that initiates the
  P2P network) must likewise set a supported `netid`; the inherited default of
  `0` will fail the same startup check.

**`wc_session_persistence`.** Controls whether and how WalletConnect v2
sessions are written to durable storage. It governs *saving* only — existing
stored sessions are always read and used regardless of the value. Values:

- `open` *(default)* — sessions are stored in the GLEEC-compatible plaintext
  format, including the session symmetric key. This keeps the on-disk format
  byte-interchangeable with GLEEC KDF in both directions. The key is stored
  unencrypted at rest; the bounded exposure, and why it is accepted for
  compatibility, is described in CRD chapter 22. This is **not** the
  fund-controlling wallet secret, which is encrypted independently.
- `none` — sessions are never written to storage. Existing rows are still
  read and used, but never rewritten. Choose this if you do not want the
  session key persisted at all.
- `encrypted` — reserved for a future encrypted-at-rest format; not yet
  available (selecting it stops startup with an explanatory error).

**`allow_insecure_key_export`.** A top-level `MM2.json` boolean, default
`false`. It governs how much private-key material the node will export.

- `false` *(default)* — the secure posture. The single-coin private-key reveal
  for one *activated* coin, and the reduced per-activated-coin key export,
  remain available; own-mnemonic self-export and wallet-password re-encryption
  remain available. The GLEEC export *superset* — offline export with no coin
  activation, hierarchical-deterministic per-derivation-path ranges, and
  shielded (ZHTLC) viewing-key export — is **refused**.
- `true` — full GLEEC-parity key export: offline / no-activation bulk export,
  HD per-derivation-path ranges, and protocol-specific formatting for UTXO
  (WIF), EVM (hex), Tendermint, and shielded ZHTLC viewing keys.

In both states a hardware-wallet session is always rejected (keys are never
exported off the device), secret material is never logged or persisted in
plaintext, and the surface is intended for localhost / trusted-channel use.
This is a **fund-controlling secret**: enabling the switch is a deliberate,
acknowledged operator act and the node emits a prominent warning when it is
active. The bounded exposure and rationale are described in CRD chapter 07
§7.3A. See [`COMPAT_SWITCHES.md`](COMPAT_SWITCHES.md) and
[`CODING_STANDARDS.md`](CODING_STANDARDS.md) §5.1 for why this is the sole
fund-controlling-secret carve-out.

## Future fork chapters

The same chapter pattern (one document, one exhaustive table) can be reused if other significant forks or upstream divergences ever need an analogous compatibility surface — for example, `UPSTREAM_COMPATIBILITY.md` for the upstream Komodo DeFi Framework, should that ever diverge meaningfully from KDF Reloaded. Each such chapter is independent of the others and lists only the settings relevant to its named target.

## See also

- [`COMPAT_SWITCHES.md`](COMPAT_SWITCHES.md) — the developer-facing rule and convention.
- [`../RELOADED_VS_GLEEC.md`](../RELOADED_VS_GLEEC.md) — the catalogue of divergences (what changed, not how to undo it).
- [`../CHANGELOG.md`](../CHANGELOG.md) — every divergent change is logged here.
