# Plan: where the EVM gas multiplier and the gas limits reach the swap transactions

**Status:** planning (option (c)+(d) decided 2026-10-28; the ch.57 paragraph and the coins-file proposal text are written, a docs-only PR is pending) — **Last reviewed:** 2026-10-28

> **Context:** an operator running a maker on USDC-MON (Monad) and USDC-BASE set
> `estimate_gas_mult` and still saw the fixed 210,000 / 150,000 limits on the
> taker fee and the payment; on Monad the payment ran out of gas and reverted.
> Since PR #107 the multiplier reaches exactly one transaction, the legacy
> ERC-20 `approve` (CRD ch.57 R57.5.3b). That scope is parity with the
> reference. This plan records which swap transactions use a fixed limit and
> which an estimate, weighs four ways to change that, and recommends one. No
> code is written until the maintainer picks an option (Decision AF).

> **Decision (2026-10-28, Tomáš):** option (c) with (d). The `gas_limit` block is
> an existing coins-file parameter, so a Monad platform block is ordinary
> coins-file content, proposed to the coins-file maintainer; no Reloaded-only key
> is added. The informative paragraph is written under ch.57 R57.5.3b and the
> proposal text is prepared (`local/notes/coins-file-monad-gas-proposal.md`).
> The (a) tidy-up and any idea of letting the multiplier scale the fixed limits
> wait for the outcome of the discussion the reference's maintainers are having
> about their own multiplier (the operator reports it is inactive for swap
> transactions in their builds as well).

## 1. What each swap transaction does today

"Fixed" means the gas limit comes from the coin's `gas_limit` / `gas_limit_v2`
block (defaults of ch.57 R57.5.2 / R57.6) and is used verbatim. "Estimated"
means `eth_estimateGas`. Tokens inherit the platform coin's blocks and
multiplier unless their own entry sets them.

| Transaction | Limit source | `estimate_gas_mult` applied? |
|---|---|---|
| V1 taker fee, platform coin | fixed, `eth_send_coins` (21,000) | no |
| V1 taker fee, token | fixed, `eth_send_erc20` (210,000) | no |
| V1 payment, platform coin | fixed, `eth_payment` (65,000) | no |
| V1 payment, token | fixed, `erc20_payment` (150,000) | no |
| V1 token `approve` before the payment | estimated | **yes** (the only one) |
| V1 receiver spend | fixed, `eth_receiver_spend` / `erc20_receiver_spend` (65,000 / 150,000) | no |
| V1 sender refund | fixed, `eth_sender_refund` / `erc20_sender_refund` (100,000 / 150,000) | no |
| V2 maker and taker payment, spends, both refunds (secret and timelock), taker `approve_payment` step | fixed, `gas_limit_v2.maker` / `.taker` (65,000 to 150,000; `approve_payment` 50,000) | no |
| V2 NFT maker payment, spend, refunds | fixed, `gas_limit_v2.nft_maker` | no |
| V2 token allowance (`handle_allowance`) | estimated | no (unscaled; ch.57 as-built note under R57.5.4) |

Quotes, which are not transactions: the flat trade fee uses `eth_max_trade_gas`
(150,000); the sender preimage adds payment and refund limits, plus the raw
approve estimate when the allowance is short; the receiver preimage uses the
spend limit. The **taker-fee preimage** (`get_fee_to_send_taker_fee`) is the one
quote that estimates, so it quotes an estimated figure for a transaction that
is later broadcast with the fixed 210,000.

Two facts that bound every option:

- **Nothing the counterparty checks depends on the limit.** No swap validator
  reads the gas field of the other side's transaction. A change to our own
  limits is visible only on chain (the `gas` field and the fee actually
  charged), never on the wire.
- **The V1 token payment cannot be estimated right after its approve.** The V1
  path sends the approve and the payment back to back and does not wait for the
  allowance (the V2 path does, `wait_for_required_allowance`). An
  `eth_estimateGas` for the payment issued in between reverts on the missing
  allowance.

## 2. Measured gas, Base and Monad (public RPC, 2026-10-28)

Same USDC code on both chains, so the difference is the chain's gas schedule.
Method: `eth_estimateGas` from a recent token holder, and `eth_getLogs` plus
receipts for the swap contract named in the public coins file; no API key.

| Operation | Base | Monad | Monad / Base |
|---|---|---|---|
| USDC `transfer` to a fresh recipient | 62,772 | 100,299 | 1.60 |
| USDC `transfer` to an existing holder | 62,964 | 83,364 to 83,424 | 1.32 |
| USDC `approve(max)` | 56,589 | 87,992 | 1.55 |
| V1 `erc20Payment`, gas used (6 txs) | 84,702 to 101,814 | no token payment in history | |
| V1 `receiverSpend`, gas used (6 txs, both kinds) | 43,632 to 72,460 | platform coin: succeeded at 65,000 (receipt shows the limit) | |
| V1 `ethPayment` / `senderRefund` (1 tx each) | 51,027 / 56,224 | `ethPayment` succeeded at 65,000; no refund in history | |

Direct Monad data is thin: the swap contract in the public coins file saw two
transactions in the last two million blocks (about nine days), a platform-coin
`ethPayment` and a platform-coin `receiverSpend`, both sent with a 65,000 limit
and both successful. Their receipts report `gasUsed` 65,000, equal to the limit,
because Monad charges the limit: **on Monad the receipt cannot show real use**,
so the Base figures cannot be re-measured from Monad receipts, and a token
payment's real use there has to come from `eth_estimateGas` or a trace.
The two successes say that the platform-coin payment and spend fit the default
65,000 on Monad (Base used 51,027 for the payment, a ratio of at most 1.27).
The token operations have no Monad swap history to read.

Why Monad is dearer: it reprices cold access (cold account 10,100 against
2,600; first read of a 128-slot storage page 8,100 against 2,100; a
state-growing `SSTORE` 17,000 against 20,000 but with extra page costs), per
its published opcode-pricing page. And it **charges the gas limit, not the gas
used** (`gas_paid = gas_limit x price`, from its gas-pricing page). On Base and
other Ethereum-style chains the unused part of the limit is refunded, so a
generous limit costs nothing but the up-front balance. On Monad every unit of
limit above the real use is paid.

Read with the defaults: the 150,000 token-payment limit has about 1.5x headroom
over Base's worst measured use (101,814). The Monad figure for the same
operation is the Base figure times roughly 1.3 to 1.6, which is 132,000 to
163,000 for 101,814, so the default sits inside that range. That matches the
operator's out-of-gas revert. The platform-coin payment is not affected. The 210,000 taker-fee limit is about twice the
measured Monad transfer (100,299), and is paid in full there.

The spread inside Base's own payment figures (84,702 to 101,814) is about
17,000. That is the size of a first-write against an overwrite of one token
balance slot, and is consistent with the swap contract's token balance being
zero or not at that moment. So a payment's gas is bounded per chain and token
but not a constant: it depends on contract state at inclusion time.

Existing coins-file practice already treats the limits as a per-chain knob. The
public coins file carries a `gas_limit` block on many platform and token
entries (for example the BEP20 family at `eth_send_erc20` 55,000 to 60,000,
`erc20_payment` 110,000, spend and refund 85,000; the PLG20 family at 70,000 /
130,000 / 100,000 / 100,000; MOVR with `eth_receiver_spend` 500,000). Its Base
platform entry carries `estimate_gas_mult` 1.3 and no `gas_limit`; its MON and
USDC-MON, USDC-BASE entries carry neither.

## 3. Options

**(a) Apply the multiplier to every transaction that is estimated.** Today
that is the V1 approve (done) and the V2 allowance approve. Wire: none. Extra
RPC: none. Failure modes: none new. It does **not** touch the payment, the
taker fee, the spends or the refunds, which are all fixed, so the operator's
complaint would stay. Fee bookkeeping: unchanged. Switch: none needed. Worth
doing only as a tidy-up (the V2 approve is the one remaining unscaled estimate).

**(b) Estimate every swap transaction and multiply, falling back to the
configured limit when estimation fails or exceeds a ceiling.** Wire: none; chain
sees different `gas` values. Extra RPC: one `eth_estimateGas` per transaction,
up to four or five per swap per side (taker fee, payment, spend or refund, and
a token approve), each beside the gas-price call the code already makes. Failure modes, and why they cut hard:
- *Estimate lower than real use.* The estimate is taken against the state at
  estimate time. A payment's cost depends on contract state at inclusion (the
  17,000 spread above). A spend or refund that reverts out of gas does not just
  waste a fee: a reverted receiver spend near the timelock can lose the swap, a
  reverted refund delays recovery. A multiplier of 1.3 narrows this, it does not
  remove it.
- *Cannot be estimated at the right moment.* A refund estimated before the
  timelock expires reverts; the V1 token payment reverts right after its
  approve; both need the fallback, which then is the fixed limit anyway.
- *More round trips on the time-critical steps,* and a new dependency on the
  node answering `eth_estimateGas` correctly (some providers return
  inconsistent values for state-dependent calls).
Fee bookkeeping: the quotes (trade fee, preimages, taker-fee preimage) must use
the same rule as the broadcast or they drift apart; today they already differ
for the taker fee. A "never lower than the configured limit" variant (limit =
max(configured, estimate x mult)) removes the under-estimate risk but never
saves gas, and on Monad it pays the higher number. Switch: yes. This departs
from the reference's fixed limits on the swap path, so it would ship per coin and
default off.

**(c) Keep fixed limits and ship per-chain defaults in the coins file.** Add
`gas_limit` (and, if V2 is used on the chain, `gas_limit_v2`) to the platform
entry of each chain; tokens inherit it (PR #105). Wire: none. Extra RPC: none.
Failure modes: a limit set too low reverts, so values carry a margin over the
measured use; a limit set too high is free on Ethereum-style chains and costs
the excess on Monad, so Monad values are the tightest. Fee bookkeeping: the
quotes already read the same fields, so quote and broadcast stay consistent by
construction. Switch: none; it is configuration. The reference reads the same
keys, so a coins file carrying them behaves the same on both. It is a data
change in the coins repository, not code in this one.

**(d) Status quo plus documentation that `gas_limit` is the operator knob.**
Nothing changes for anyone who has not read the documentation; the operator in
the report would have been served by a note saying that `estimate_gas_mult`
reaches only the V1 approve, and that `gas_limit` is what to set.

## 4. Recommendation (adopted)

**(c), together with (d).** The small tidy-up of (a) is deferred (see the decision above). Reasons:

1. The payment's gas is bounded per chain and token (Base: 84,702 to 101,814 on
   one contract; Monad: that times about 1.3 to 1.6), so a per-chain limit with a
   margin is both safe and, on Monad, cheaper than today's 150,000 and 210,000.
   An estimate adds a round trip and a new way to revert on exactly the steps
   (spend, refund) where a revert is expensive.
2. It needs no change to the swap code, so no compatibility switch and no risk
   to the swaps in flight; a platform block reaches every token that has no block of its own.
3. It fixes the report: with a Monad block the payment no longer runs out of
   gas, and the taker fee stops paying for twice the gas it uses.

Proposed coins-file values (2026-10-28), kept as the reasoning. They rest on the 1.3 to
1.6 ratio of the `transfer` and `approve` measurements, not on a Monad token payment:

| Entry | Member | Proposed |
|---|---|---|
| MON (platform; tokens inherit) | `eth_payment`, `eth_receiver_spend` | keep the default 65,000 (two live successes at that limit) |
| MON | `erc20_payment` | 200,000 (Base worst 101,814 x 1.6 = 163,000, plus margin) |
| MON | `eth_send_erc20` | 130,000 (measured 100,299 fresh recipient, plus margin; default 210,000) |
| MON | `erc20_receiver_spend`, `erc20_sender_refund` | 150,000 (unchanged default; Base worst 72,460 x 1.6 = 116,000) |
| ETH-BASE | none | defaults already have about 1.5x headroom; no change |

**Outcome (2026-10-29).** The coins-file maintainer committed a block measured on
Monad itself, on the MON tokens only (USDC-MON, USDT-MON, WBTC-MON and the other listed
MON tokens), and left the MON platform coin on the code defaults, which work for it:

| Member | Committed on each MON token |
|---|---|
| `eth_send_erc20` | 150,000 |
| `erc20_payment` | 250,000 |
| `erc20_receiver_spend` | 200,000 |
| `erc20_sender_refund` | 200,000 |

These are above the proposal in the payment and spend rows because they were measured on
real Monad token calls, which this plan could not do. They replace the table above as the
values in use; the Base-based reasoning stays as the explanation of why a Monad token needs
more than the Ethereum-tuned defaults. The `gas_limit` block of a token is honored by
Reloaded for all four members (the token's own entry is read first, R57.5.3). It replaces
the platform coin's block as a whole: a member the token's block omits takes the default of
R57.5.2, not the platform coin's value. An operator copying this block to another chain's
token must therefore carry every member that token needs. The maintainer also set
`estimate_gas_mult` on MON; it reaches the legacy `approve` only and is inert for the swap
transactions.

If `estimate_gas_mult` is later wanted on payments, (b) stays available as an
opt-in per coin, with the two limits above (the refund timing and the V1 approve
timing) designed in from the start, and written as a Reloaded-original divergence
note because the reference does not do it.

## 5. What changes in the chapters

- **ch.57:** no normative change. Done: an informative paragraph after the
  as-built note of R57.5.3b says that the multiplier applies to the V1 approve
  only, that the `gas_limit` blocks are the operator's means for every other swap
  transaction, and that on a chain which charges the gas limit (Monad) the limits
  should be set close to measured use. Deferred: (a)'s tidy-up (R57.5.3b item 5
  naming the V2 allowance approve) and (b), which would be a new rule and a
  Reloaded-original divergence note with its own tests.
- **Coins file (separate repository):** the Monad block above, after the direct
  measurement; nothing else.
- **Documentation:** a short section in the operator-facing docs describing which
  transactions `estimate_gas_mult` reaches and which `gas_limit` members to
  set, with the Monad note.

## 6. Open points for the maintainer

1. ~~Pick an option~~ decided: (c)+(d); (a) and (b) wait for the reference maintainers' outcome.
2. ~~Who carries the Monad block?~~ the coins-file maintainer, by the proposal text.
3. The taker-fee quote (estimated) against the broadcast (fixed 210,000) is a
   small inconsistency of its own; leave it, or make the quote follow the limit.
