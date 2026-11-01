# Plan: the EVM minimum trading volume

**Status:** in progress (PR #66 merged; an end-to-end preview on a real 8-decimal coin left) — **Last reviewed:** 2026-10-29

> **Context:** a field report of a refused WBTC-PLG20 buy traced to
> `min_trading_vol` for EVM coins, which is computed from the coin's decimals by
> a formula with no stated basis. For an 8-decimal token the formula yields a
> 0.01 floor, which on a Bitcoin-pegged token is roughly 0.01 BTC as a *minimum
> trade*. The reporter states the same defect existed in the reference and was
> fixed there long ago, so the shape of the fix is a Spec Reader question before
> it is an implementation question. When this plan was written nothing had been
> implemented and no branch existed; see the State line for the current
> position.

> **2026-10-25:** PR #66 is being widened to cover the minimum trading volume
> of every coin family under plan `trading-floors-parity`, which now carries
> the remaining steps. This plan's EVM/QRC20 scope is unchanged.

**State (2026-10-25): implemented, PR #66, merge after #57.** Steps 2 and 3
were done on 2026-10-24 — the Spec Reader dispatch produced CRD ch.57 §57.7A
(Part D: R57.7A.1–R57.7A.6, T10–T12, D7–D8, V4), and it passed the Dirty Gate
the same day (PASS, measure 0.01, recorded in ch.57 §57.12's second
scoped-addition item). Step 4 was implemented from the cleared chapter alone on
branch `fix/evm-min-trading-volume` (PR #66), to be merged after PR #57. The
one item of "What done looks like" not met is the end-to-end preview of the
field report's volume on a real eight-decimal coin, recorded below as a
coverage gap.

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

### Answers (ch.57 §57.7A, cleared 2026-10-24)

1. **One smallest unit.** The reference reports exactly `10^-decimals` as an
   EVM platform coin's and an ERC-20 token's minimum trading volume, computed
   exactly as a rational with no rounding, clamp or other floor (R57.7A.1):
   18 decimals → 10^-18, 8 → 10^-8, 6 → 10^-6. It is **not** configurable —
   the reference reads no coins-file key for it and the public coins file
   carries none (R57.7A.2). It was not removed.
2. **Changed to `10^-decimals`**, at `v2.0.0-beta` as the first public release
   designation carrying it; `v2.6.0-beta` and `v3.0.0-beta` agree (§57.1).
   The QRC20 token layer changed at the same designation to the same value
   (R57.7A.6).
3. **Local policy, visible in one place.** A node applies its own coins' floors
   only to orders it creates and never to a peer's order; a received order
   record is checked only for a non-negative minimum volume and a positive
   price and maximum volume (R57.7A.5, ch.32 §32.5). The value reaches the
   wire only as the default `min_volume` a maker advertises when it sets
   none (ch.32 R-W1), shown in orderbook and best-orders responses.
4. **Yes, and they stay distinct.** `min_tx_amount` is **zero** for EVM coins
   at every designation, before and after the change, and stays zero
   (R57.7A.3). The DEX fee and the `max_taker_vol`/`max_maker_vol` paths read
   that accessor, not the trading-volume floor, so they are untouched (T12).
   The same single value is applied on every path that does consume it —
   maker and taker order creation, both trade previews and both
   `min_trading_vol` methods (R57.7A.4).

## Steps

1. **Confirm the failing path, and its breadth.** Answered by the chapter
   rather than by observation: ch.57 R57.7A.4 states that maker order
   creation and update, taker order creation, both trade previews and both
   `min_trading_vol` methods consume the floor, while `max_taker_vol` and
   `max_maker_vol` do not. The end-to-end observation this step asked for is
   now T11 (the field report's volume against all four request kinds) and is
   the Coder's to write, not a prerequisite.
2. **Spec Reader dispatch** — **done 2026-10-24.** Output: ch.57 §57.7A.
3. **Dirty Gate** — **done 2026-10-24.** PASS, measure 0.01, finding list
   empty; recorded in ch.57 §57.12 (second scoped-addition item).
4. **Implementation via the Coder** — **done 2026-10-25, PR #66**
   (`fix/evm-min-trading-volume`, merge after #57), from the gated chapter
   only. As built:
   - a new helper `one_smallest_unit(decimals)` in
     `mm2src/coins/lp_coins_types.rs`, computing `BigDecimal::new(1, decimals)`
     exactly; the pre-existing satoshi-to-decimal helper was not reused
     because it overflows above 19 decimals;
   - `min_trading_vol` in `mm2src/coins/eth/eth_market_ops.rs` and in the
     QRC20 market-operations file now return that helper's value (R57.7A.1,
     R57.7A.6); the `decimals / 3` expression is gone from both;
   - `min_tx_amount` is unchanged at zero (R57.7A.3); no compatibility switch
     (R57.7A.5).
   - Tests: **T10** pins the exact value for 18, 8, 6 and 24 decimals, for an
     EVM coin and a QRC20 token. **T11** covers the taker order builder's
     `BaseAmountTooLow` and the maker order builder's `MaxBaseVolTooLow`
     refusals and their mapping to `TradePreimageRpcError::VolumeTooLow`
     with threshold 10^-8, the maker preview for a volume of zero, and both
     `min_trading_vol` RPCs. **T12** covers the DEX-fee path not reading the
     floor.
   - **Coverage gap, recorded honestly:** the end-to-end maker and taker
     trade preview for `0.00072437` on a real eight-decimal coin is not
     driven — the test coin lacks the balance and fee mocks it would need.
     The maker order-update path uses the same validator as creation but is
     not separately tested. The `max_taker_vol` half of T12 cannot detect a
     future read of the floor; only the DEX-fee half is guarded.
   - D7's one-line price-floor remedy was **left out by maintainer decision**;
     it is confirmed fully separable — see Risks.
5. **Compatibility question — answered; no switch.** The premise of this
   step was that question 3 would decide the arm. It does not arise: the
   change is **toward parity** — it replaces a pre-anchor baseline formula
   (ch.57 V4) with the reference's value at every designation on both
   production netids — so the
   [default-side rule](../COMPAT_SWITCHES.md#the-default-side-rule) of
   `docs/COMPAT_SWITCHES.md` does not call for a switch to recover the
   baseline formula (ch.57 R57.7A.5). The visible change in the cached
   `min_trading_vol` responses is a changelog line. `docs/GLEEC_COMPATIBILITY.md`
   and `RELOADED_VS_GLEEC.md` need an entry only if either currently lists
   the baseline formula as a divergence.

## Which CRD chapter owns this

**Settled: ch.57 §57.7A (Part D).** `docs/reloaded-rewrite/57-evm-fee-inputs.md`
owns the EVM coin's *value* — the minimum trading volume is a per-coin property
the coin layer reports, and the UTXO analogue is bound where the UTXO coin
layer is bound (ch.38 R38.6.6). The order-match layer's *consumption* of the
value (the default-and-refusal arithmetic, and the maker price floor of D7)
is coin-agnostic validation that no chapter binds yet; ch.57 restates it only
so far as needed to say where the value is observable, and ch.57 D7 records
that it belongs to whichever order-match chapter comes to own order
validation. Chapters 32, 38 and 59 carry one-line pointers to §57.7A.

## What done looks like

Checked against PR #66 on 2026-10-25; every item is met except the first,
which is recorded as a coverage gap.

- A buy, a maker order and a trade preview of `0.00072437 WBTC-PLG20` all
  succeed, or fail for a reason that is not this floor. — **Gap:** not
  driven end to end in PR #66's tests (the test coin lacks balance and fee
  mocks); the floor the four requests consult is pinned by T10 and the
  refusal arithmetic by T11, so the item rests on those plus field use.
- `min_trading_vol` for an EVM coin and a QRC20 token is exactly
  `10^-decimals` (ch.57 R57.7A.1, R57.7A.6) and the `decimals / 3` expression
  is gone from both market-operations files; `min_tx_amount` stays zero
  (R57.7A.3). — **Met** (PR #66).
- The governing CRD chapter binds the rule and answers the four questions
  — done, ch.57 §57.7A, cleared 2026-10-24.
- No compatibility switch (ch.57 R57.7A.5); a changelog line for the changed
  `min_trading_vol` responses. — **Met** (PR #66).
- A regression test pins the floor for an 8-decimal EVM token and fails on
  10^-2, so the baseline value cannot come back unnoticed (ch.57 T10); the
  field report's volume passes end to end (T11); the fee and maximum-volume
  paths are shown untouched (T12). — **Met** for T10 and for the refusal
  half of T11; the end-to-end half of T11 is the gap above; T12 guards the
  DEX-fee path, while its maximum-volume half cannot detect a future read of
  the floor.

## Risks

- **Lowering a floor can expose a dust problem elsewhere.** The floor may be
  carrying weight it was never designed to carry (fee-versus-value sanity, a
  dex-fee minimum). Check `mm2src/mm2_main/src/lp_swap/dex_fee.rs`, which reads
  `min_tx_amount` and not `min_trading_vol`, before assuming nothing depends on
  the current value.
- **`min_trading_vol` is reported over RPC.** A GUI caches it; changing it
  changes what wallets display as the minimum. Harmless, but it is a visible
  change worth a changelog line.
- **Per-coin configuration would touch a third-party file** — **moot.** The
  reference reads no coins-file key for the value and the public coins file
  carries none on any entry (ch.57 R57.7A.2); nothing in `komodo-coins-rin`
  needs to change. Introducing such a key would be an extension of the public
  configuration surface and a maintainer's decision (ch.57 D8), not part of
  closing this gap.
- **The maker price floor is a second, adjacent gap.** Reloaded refuses a
  maker price below 10^-8; the reference accepts any strictly positive price
  since the same designation and refuses only zero and negative prices (ch.57
  D7, the code-quality finding under R57.7A.6). Same shape — an order a
  reference node would place is refused locally, no effect on any peer — and
  a one-line remedy (`price <= 0` instead of `price < 10^-8`) with no switch.
  It is coin-agnostic order validation that no chapter binds yet, so it is
  not in the Coder's scope from ch.57 alone. **Decided 2026-10-25: left out
  of PR #66 by maintainer decision.** It is confirmed fully separable from
  the volume floor — a single comparison in `validate_price` (`price <
  10^-8` → `price <= 0`) — so it can be carried later, when an order-match
  chapter owns order validation, without reopening this change.
- **The QRC20 companion.** The QRC20 token layer carries the same formula and
  the same zero `min_tx_amount`, and ch.57 R57.7A.6 binds it alike; the fix is
  incomplete if only the EVM file changes. Ch.57 D8 records that the rule
  should move to ch.38 when its Qtum material is next revised.

## Open decisions

1. **Whether to wait for the Spec Reader at all** — **resolved** by waiting:
   the dispatch and the gate completed on 2026-10-24, and the reference's
   answer is the coin's smallest representable unit.
2. **Whether `min_tx_amount` stays zero for EVM** — **answered: it stays
   zero** (ch.57 R57.7A.3). The two accessors are distinct in the reference
   too, and only `min_trading_vol` changes.
3. **Chapter ownership** — **settled**, ch.57 §57.7A (above).
4. **Whether to carry the D7 price-floor remedy in the same change** (Risks
   above) — **resolved 2026-10-25: not carried.** Maintainer decision; the
   remedy is one comparison in `validate_price` and fully separable, and
   waits for an order-match chapter to own order validation.
