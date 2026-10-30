# Plan: trading-floor parity for every coin family

**Status:** in progress (beta blocker; all four PRs merged: #66, #69, #72, #73; the swaplab run of #72 left) — **Last reviewed:** 2026-10-29

> **Context:** a field report of a refused WBTC-PLG20 buy led to the EVM fix of
> PR #66 (plan `evm-min-trading-volume`). The maintainer then asked how the UTXO
> floor is computed and commissioned a Spec Reader pass over every coin family.
> Its output, cleared by the KDF Dirty Gate on 2026-10-25, is CRD chapter 60 and
> chapter 38 §38.6.6. On 2026-10-25 the maintainer made the whole set a beta
> blocker, to be implemented fully at parity, and chose the options recorded
> below. The governing text is chapter 60 (R60.6.1–R60.6.7) and chapter 38
> R38.6.6a–i; this plan records only the decisions and the packaging.

## Decisions (maintainer, 2026-10-25)

1. **Packaging.** Widen PR #66 into the minimum trading volume for all
   coins. Put UTXO fee policy into one new PR and clean spend refusal into
   another. Each later PR is stacked on the one before, because they touch the
   same UTXO fee and spend code.
2. **UTXO fee policy: full parity.** Implement `dingo_fee`, proportional
   plain fixed fees and the 496-byte HTLC spend estimate (ch. 38 R38.6.6a–c,
   ch. 60 R60.6.2), then test on live coins in swaplab: KMD, LTC, DOGE and one
   `dingo_fee` coin. *Rejected:* `dingo_fee` only while keeping whole-kB
   rounding for plain fixed coins (a divergence that keeps users overpaying),
   and deferral (a version-two swap with a `dingo_fee` taker coin would
   compute a different spend fee than a reference peer).
   *Accepted cost:* fixed-fee transactions get cheaper; an old daemon that
   still rounds its relay fee per started kilobyte would reject them. The
   live test is there to catch that.
3. **Dynamic-fee coins such as BTC: parity floor plus a local pre-trade
   check** (ch. 60 R60.6.7, switch `pre_trade_spend_check`, default on).
   *Rejected:* keeping BTC at 0.00777 (contradicts R60.6.1 and does not
   protect BTC-segwit), and operator `dust` guidance alone.
   *Open for later (default-side arm 3, not chosen):* raising the advertised
   default minimum volume instead of refusing. That would change a value
   peers see (W1), so it needs a project-team decision.
4. **Sia: parity floor, orderly spend and refund refusal (R60.6.4), plus the
   same pre-trade check.** *Rejected:* a raised local Sia floor (peers would
   see a different advertised minimum).
5. **Lightning: the reference value now**, in the widened PR #66, with the
   order builders refusing Lightning pairs while Lightning trading is
   unsupported (R60.6.5). The parked plan `lightning-activation-features`
   carries a pointer.
6. **Solana: plan note only.** The reference has no value, so ours is chosen
   when Solana trading is designed (plan `solana-support`). The request does
   not end abnormally today.
7. **TRON:** its value is fixed by PR #66; the parked plan
   `tron-swap-support` carries a note.
8. **QRC20 rule placement:** it stays in ch. 57 R57.7A.6, with a pointer from
   ch. 38 R38.6.6g (ch. 57 D8).

> **Beta availability (2026-10-26).** By maintainer decision three families
> take part in no trade in the coming beta: Solana
> (`SOLANA_ACTIVATION_AVAILABLE`) and TRON (`TRON_ACTIVATION_AVAILABLE`) cannot
> be activated, and Tendermint coins and tokens are wallet-only
> (`TENDERMINT_SWAPS_AVAILABLE`), all three constants in
> `coins/lp_coins_ops.rs` set to `false`. Items 6 and 7 and the Tendermint row
> of ch. 60 (C1) stand unchanged for when the families return (Solana and TRON
> planned for the next beta, Tendermint swaps for a later release); turning the
> families back on belongs to plans `solana-support`, `tron-swap-support` and
> `tendermint-activation-features`, not to this one.

## Work packages and merge order

| # | Branch / PR | Content | Depends on |
|---|---|---|---|
| 0 | `crd/trading-floors-all-coins` (PR #69) | ch. 60, ch. 38 §38.6.6, R60.6.7, this plan | — |
| 1 | `fix/evm-min-trading-volume` (PR #66, widened) | EVM/QRC20/TRON (done); UTXO floor: no `BTC` exception, truncated fee term (R60.6.1, R38.6.6i items 1–2); Lightning floors (R60.6.5); Solana requests answer without abnormal end | 0 |
| 2 | new: UTXO fee policy | `dingo_fee` classification and arithmetic, proportional plain fixed fees, the whole-kB fee term in the floor, the 496-byte legacy spend estimate (R38.6.6a–c, R38.6.6i items 3–4, R60.6.2) | 1 |
| 3 | new: spend safety | orderly spend and refund refusal for UTXO and Sia, version-two funding fee plus dust (R60.6.4); pre-trade check and its switch (R60.6.7) | 2; PR #60 merged first (same refund code) |

## State

- 0: PR #69 merged 2026-10-25.
- 1: PR #66 widened and merged 2026-10-25: UTXO floor (R60.6.1) and Lightning floors
  with the untradable-pair refusal (R60.6.5) are implemented. Solana and SPL
  are refused the same way, because their fee functions are unimplemented.
  The vendored LDK fork gained one local patch (plan `lightning-ldk-upgrade`).
- 2: UTXO fee policy merged in PR #72 (2026-10-25). An
  explicit `UtxoFixed` withdraw fee keeps its per-started-kilobyte meaning,
  since no chapter covers it. Swaplab live tests are pending a dedicated
  swaplab session.
- 3: spend safety merged in PR #73 (2026-10-25): R60.6.4 and R60.6.7 with
  the switch `pre_trade_spend_check`. The Sia miner fee turned out to be
  0.00002 SC, not 0.02 SC as the chapter first said; the chapter is
  corrected.
