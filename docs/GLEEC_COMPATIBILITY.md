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
| Shielded withdrawal short because of an unconfirmed transaction | **none -- no switch exists** | Cannot be reproduced, and none is wanted. A Light-mode ZHTLC (ARRR) `withdraw` that is short only because its notes are committed to an earlier, unconfirmed transaction of the same node fails with the additive `error_type` `BalanceLockedByUnconfirmedTransaction` (HTTP 400, naming the coin, available, required and committed amounts); the reference reports the bare `NotSufficientBalance`. No wire format and no other node is affected, so by the second arm of [the default-side rule](COMPAT_SWITCHES.md#the-default-side-rule) Reloaded's variant is the default. A client that matches only on `NotSufficientBalance` sees a different token for this one case; after a node restart the bare `NotSufficientBalance` is reported again. | No | [CRD ch.39 D39.8.0e](reloaded-rewrite/39-zcash---z_coin-shielded-coin.md) |
| Key export | `allow_insecure_key_export` | `true` — enables GLEEC-parity offline / no-activation / HD-range / shielded key export. Default `false`. | Yes | [CRD ch.07 §7.3A](reloaded-rewrite/07-wallet-lifecycle-and-key-export.md) |
| Sia activation result under an HD wallet | `sia_activation_single_address_balance` | `true` -- `wallet_balance` is always the single-address `"Iguana"` form, as GLEEC KDF reports it. Default `false`: under an HD wallet Reloaded reports the HD per-account report (`"wallet_type": "HD"`), like the other coins of an HD wallet. Both shapes carry the same single address and balance. | No | [CRD ch.46 R46.2.4](reloaded-rewrite/46-sia-v2-activation-rpcs.md) |
| Legacy `enable` after a failed balance read | `legacy_enable_keeps_coin_on_balance_failure` | `true` -- `enable` leaves the coin registered when its first balance read fails, as GLEEC KDF does (a retry then fails as already active). Default `false`: both `enable` and `electrum` deregister the coin so a retry can succeed. The switch only reproduces GLEEC's inconsistency between the two methods; it is not recommended. | No | [CRD ch.55 R55.4.6](reloaded-rewrite/55-coin-activation-wire-contract.md) |
| EVM transaction type for a coin with no `max_eth_tx_type` | `max_eth_tx_type_absent_unrestricted` *(top-level `MM2.json` boolean)* | `false` *(also the default)* -- a coin whose configuration has no `max_eth_tx_type` key may send untyped (legacy) transactions only, as GLEEC KDF does; an EIP-1559 swap transaction is then refused at signing (not downgraded). `true` opts into Reloaded's divergence: an absent key means *unrestricted* and the swap gas-fee policy alone decides the form. A key that is present is honoured identically either way. Only the boolean `true` turns it on. | No | [CRD ch.35 D35.7](reloaded-rewrite/35-evm-v2-activation-rpcs.md) |
| EVM token activation on an HD wallet | `erc20_hd_activation` | `false` -- `enable_erc20` refuses an HD-derived platform (HD mode; a hardware-backed platform is refused either way) with `UnexpectedDerivationMethod` (`"ExpectedSingleAddress"`), as GLEEC KDF does. Default `true`: on a software HD wallet the token is activated on the platform's enabled address and `balances` lists every known address. Single-address wallets and `enable_nft` are unaffected. | No | [CRD ch.35 R35.2.3, R35.2.5](reloaded-rewrite/35-evm-v2-activation-rpcs.md) |

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
