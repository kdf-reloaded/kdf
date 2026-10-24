# Plan: the EVM minimum trading volume

**Status:** planning — **Last reviewed:** 2026-10-24

> **Context:** a field report of a refused WBTC-PLG20 buy traced to
> `min_trading_vol` for EVM coins, which is computed from the coin's decimals by
> a formula with no stated basis. For an 8-decimal token the formula yields a
> 0.01 floor, which on a Bitcoin-pegged token is roughly 0.01 BTC as a *minimum
> trade*. The reporter states the same defect existed in the reference and was
> fixed there long ago, so the shape of the fix is a Spec Reader question before
> it is an implementation question. Nothing here has been implemented; no branch
> exists.

## The defect (verified in this tree, 2026-10-24)

`mm2src/coins/eth/eth_market_ops.rs` implements the floor as:

```rust
fn min_tx_amount(&self) -> BigDecimal { BigDecimal::from(0) }

fn min_trading_vol(&self) -> MmNumber {
    let pow = self.decimals / 3;
    MmNumber::from(1) / MmNumber::from(10u64.pow(pow as u32))
}
```

That is `10^-(decimals/3)` with **integer** division, and the division by three
has no recorded justification anywhere in the tree. The resulting floors:

| `decimals` | `decimals / 3` | floor |
|---|---|---|
| 18 | 6 | 0.000001 |
| 8 | 2 | **0.01** |
| 6 | 2 | 0.01 (one cent; tolerable) |

`min_tx_amount` for the same coins is `0`, so `min_trading_vol` is the only
floor an EVM coin has.

**Field report.** A buy of `0.00072437 WBTC-PLG20` was refused with
`The volume 0.00072437 of the WBTC-PLG20 coin less than minimum transaction
amount 0.01`. At present prices the floor is roughly three orders of magnitude
above the volume the user asked for.

## Blast radius (verified against the registry, 2026-10-24)

Counts measured against the `coins` file of `/home/tomas_admin/komodo-coins-rin`
at `ea86c441` (770 entries):

- **44** entries declare `decimals: 8`.
- **36** of those are EVM entries (all with `protocol.type == "ERC20"`); every
  one takes the 0.01 floor.
- The remaining **8** are not EVM and are unaffected by this defect: `JKC`,
  `CAT`, `CRNC`, `FIVETRAT`, `LCN`, `LCN-segwit`, `MAZA` (all `UTXO`) and
  `HPY-QRC20` (`QRC20`).
- The serious cases are the four **WBTC** variants, each `decimals: 8` and
  `ERC20`: `WBTC-ERC20`, `WBTC-PLG20`, `WBTC-ARB20`, `WBTC-MON`. The other 32
  are low-value tokens for which a 0.01 floor is not currently a practical
  obstacle.

> *Correction to the brief this plan was written from:* that brief gave "36
> entries with 8 decimals, of which roughly 33 are EVM". The measured figures
> are 44 at 8 decimals of which 36 are EVM. The four WBTC variants are
> confirmed as stated.

## Where the floor is enforced (verified)

`min_trading_vol()` is read in more places than the taker-buy path the report
came from:

- `mm2src/mm2_main/src/lp_ordermatch/ordermatch_types.rs` — the **maker** order
  builder (`min_base_vol` defaults to it, and `validate_and_get_min_vol` uses
  it as a threshold) and the **taker** order builder (`MinVolumeTooLow`,
  `MaxBaseVolBelowMinBaseVol`);
- `mm2src/mm2_main/src/lp_ordermatch/ordermatch_trading.rs` — the preimage /
  trading-volume path;
- `mm2src/mm2_main/src/lp_swap/maker_swap.rs` and `.../taker_swap.rs`;
- `mm2src/mm2_main/src/rpc/lp_commands/lp_commands_legacy.rs` and the
  `min_trading_vol_v2` RPC, which report the value to a GUI.

The `VolumeTooLow` message the reporter saw is declared in four places that
share its wording: `mm2src/coins/lp_coins_errors.rs`,
`mm2src/mm2_main/src/lp_swap/check_balance.rs`,
`mm2src/mm2_main/src/lp_swap/trade_preimage.rs` and
`mm2src/mm2_main/src/lp_swap/max_maker_vol_rpc.rs`. So the floor plausibly
blocks maker orders and trade previews as well as taker buys — step 1 is to
establish that by observation rather than by reading, because it decides
severity.

## Reference pointers for a KDF Spec Reader dispatch

> **A clean context must not open any of these.** They are recorded here solely
> to direct a future KDF Spec Reader dispatch — the only role permitted to read
> upstream/GLEEC KDF material. The numbers and descriptions below were supplied
> by a community maintainer and transcribed without being fetched, opened,
> previewed or verified.

| Upstream reference | As described to us |
|---|---|
| issue **#1635** | describes the 0.01 minimum-volume problem |
| pull request **#1971** | reportedly the fix for it |
| pull request **#2564** | described as related to minimum trade volumes |

### Questions for that dispatch

1. What does the reference use for an EVM coin's minimum trading volume today —
   still derived from `decimals`, a fixed value, per-coin configurable in the
   coins file, or removed for EVM entirely?
2. What was it changed *to*, concretely (the expression or the value)?
3. Is the change observable on the wire — does a counterparty or the orderbook
   see it — or is it purely local node policy?
4. Does the reference distinguish `min_tx_amount` from `min_trading_vol` for EVM
   coins, and if so how? Ours sets the former to zero, so the two are not
   interchangeable here.

## Steps

1. **Confirm the failing path, and its breadth.** From the four error sites that
   share the `VolumeTooLow` wording (`check_balance.rs`, `trade_preimage.rs`,
   `max_maker_vol_rpc.rs`, and the taker swap path), establish by test which
   requests the floor actually refuses: taker buy, maker `setprice`, and
   `trade_preimage` / `max_maker_vol`. If maker orders and previews are blocked
   too, the defect is a severity higher than the report suggests.
2. **Spec Reader dispatch** on the four questions above, carrying the three
   reference pointers. The output is a CRD chapter section, not code.
3. **Dirty Gate** on that candidate section. Nothing from the dispatch may be
   used before it passes.
4. **Implementation via the Coder**, from the gated chapter only.
5. **Compatibility question.** Consult `docs/COMPAT_SWITCHES.md` and apply its
   [default-side rule](../COMPAT_SWITCHES.md#the-default-side-rule) to decide
   whether a changed floor needs a switch. Question 3 of the dispatch is what
   decides the arm: a purely local policy change that only improves user
   experience falls in arm 2 (Reloaded default, no switch); anything a
   counterparty can observe falls in arm 1 (parity default plus a switch);
   anything in between is a project-team decision and not the implementer's.
   Record the outcome in `docs/GLEEC_COMPATIBILITY.md` and
   `RELOADED_VS_GLEEC.md` either way.

## Which CRD chapter owns this

**To be checked, not assumed.** `docs/reloaded-rewrite/57-evm-fee-inputs.md`
owns EVM fee and amount inputs and is the first candidate, but a *volume floor*
is an order-match constraint rather than a fee input, so it may belong with the
order-match chapters instead (ch.11 order-match cancellation, ch.12 order-match
state store, ch.59 legacy ordermatch negotiation). Settle the ownership before
the Spec Reader dispatch, so the candidate section is authored into the right
chapter.

## What done looks like

- A buy, a maker order and a trade preview of `0.00072437 WBTC-PLG20` all
  succeed, or fail for a reason that is not this floor.
- `min_trading_vol` for an EVM coin is derived from something with a written
  basis — a per-coin configuration value, the coin's own smallest unit, or a
  documented constant — and the `decimals / 3` expression is gone.
- The governing CRD chapter binds the new rule, and the Spec Reader's four
  questions are answered in it rather than left open.
- The compatibility decision is recorded in `docs/GLEEC_COMPATIBILITY.md` and
  `RELOADED_VS_GLEEC.md`, with a switch if the default-side rule asks for one.
- A regression test pins the floor for an 8-decimal EVM token, so the 0.01
  value cannot come back unnoticed.

## Risks

- **Lowering a floor can expose a dust problem elsewhere.** The floor may be
  carrying weight it was never designed to carry (fee-versus-value sanity, a
  dex-fee minimum). Check `mm2src/mm2_main/src/lp_swap/dex_fee.rs`, which reads
  `min_tx_amount` and not `min_trading_vol`, before assuming nothing depends on
  the current value.
- **`min_trading_vol` is reported over RPC.** A GUI caches it; changing it
  changes what wallets display as the minimum. Harmless, but it is a visible
  change worth a changelog line.
- **Per-coin configuration would touch a third-party file.** If the reference
  moved the value into the coins file, we depend on `komodo-coins-rin` carrying
  the key for the affected entries. Verify before relying on it.

## Open decisions

1. **Whether to wait for the Spec Reader at all.** A defensible floor could be
   derived from first principles (for example the coin's smallest representable
   unit, matching the SLP and Siacoin implementations). Parity with the
   reference is the argument for waiting; a one-line fix for a user-visible
   refusal is the argument against.
2. **Whether `min_tx_amount` stays zero for EVM.** If the reference distinguishes
   the two, ours may need both changed rather than one.
3. **Chapter ownership** (above).
