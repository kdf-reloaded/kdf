# Plan: operator-selectable Siacoin withdraw fee

> **Status:** deferred, not started. Parked deliberately on 2026-09-20 during the
> beta triage of the Siacoin work. Nothing here is a defect fix: the gap is a
> missing feature that the wallet's UI currently implies exists, and the
> behaviour is at parity with the deployed interoperability reference, so it
> carries no instability and does not gate a beta.

## The gap

`WithdrawRequest` carries a `fee` object whose variants are all UTXO-, EVM-,
QRC20- or Cosmos-shaped (`UtxoFixed`, `UtxoPerKbyte`, `EthGas`,
`EthGasEip1559`, `Qrc20Gas`, `CosmosGas`). None of them can express a Sia fee.
The field deserialises for an SC request and is then ignored: CRD ch.20 §20.9
R-W3 states outright that "the request does not carry a fee-override value for
this path". There is no way to set an SC withdraw fee over RPC.

The fee itself is not the problem. Reloaded estimates it from the transaction's
real serialized weight times walletd's live `/api/txpool/fee` rate, recomputed
as each input is added (ch.20 R-W3, §20.10 D6 closed). That is a better default
than the deployed reference's hard-coded constant. What is missing is the
override.

## Why it is user-visible, and why it is still not urgent

The shipped Komodo DeFi wallet shows its "Enable Custom Fees" switch for every
non-ZHTLC coin, Siacoin included (`SendModal.qml`, `visible:
!General.isZhtlc(...)`). A user can therefore enable it for SC, enter a fee,
and send. The wallet puts a `UtxoPerKbyte` object in the request, KDF ignores
it, and the withdrawal goes out at the estimated fee. No error is raised on
either side.

That is a silent no-op, which is the part worth fixing eventually: the user
believes they set something and did not. It is *not* a funds-safety problem --
the transaction is well-formed, the fee is sound, and the amount delivered is
the one requested -- and the deployed reference behaves the same way, so no
integration regresses by waiting.

## Sketch of the work

1. **A Sia-shaped fee variant.** Add an arm to the withdraw-fee enum expressing
   a Sia fee. Two forms are plausible and the choice is the first real decision:
   a fixed total (the simplest, matching `UtxoFixed`), or a hastings-per-byte
   rate that the existing size-aware estimator would multiply by the real weight
   (matching `UtxoPerKbyte` and this coin's own fee model more honestly). The
   per-byte form is the better fit; a fixed total is the one a user is more
   likely to understand.
2. **Plumb it through the withdraw path**, replacing the estimated rate rather
   than the final amount, so the weight-aware recomputation per input still
   applies.
3. **Validate it.** Reject a fee below what the transaction plausibly needs, and
   reject one that exceeds the spendable balance, with typed errors rather than
   letting walletd refuse the broadcast later.
4. **Bind it in ch.20**, amending R-W3 (which currently states the opposite) and
   adding verification items.
5. **Divergence bookkeeping.** Accepting a `fee` that the reference ignores is
   additive, so it follows the R-W7/R-W12 reasoning already recorded in
   `RELOADED_VS_GLEEC.md` rather than needing a compat switch. Confirm that
   before implementing, not after.
6. **Wallet side, separately.** Either hide the custom-fee switch for SC until
   the KDF side lands, or teach it the new variant. Hiding it is the smaller
   change and removes the silent no-op immediately, independent of this plan.

## Ordering note

Step 6's "hide the switch" half is worth doing on its own schedule: it costs one
QML condition, removes the misleading control, and does not depend on any of the
KDF work above. It is listed here only so the two halves are not forgotten
separately.

## Governing CRD

- [Chapter 20 §20.9](../reloaded-rewrite/20-siacoin-integration.md) -- R-W3
  (fee estimation, no override), §20.10 D6 (size-aware estimation, closed).
- [Chapter 49](../reloaded-rewrite/49-withdrawal-task-path.md) -- the
  coin-generic withdraw request/response contract this would extend.
