# Plan: make TRON work again

**Status:** parked at the 0.4.0 freeze (added to the 0.4.0 scope on 2026-11-08; the work stops after PR #333, the TRON wallet path, with TRON activation still off; swaps not built) — **Last reviewed:** 2026-11-10

> **Context:** created 2026-10-28 as the umbrella plan for bringing TRON (TRX,
> TRC20) back. By maintainer decision TRON activation is switched off on `dev`
> for the coming beta: while `TRON_ACTIVATION_AVAILABLE`
> (`coins/lp_coins_ops.rs`) is `false`, every TRON activation route answers
> "TRON support is not available in this build" (CRD ch. 21 availability note,
> ch. 55 D55.5). TRON is planned to return in the next beta. It is postponed,
> not dropped. Two narrower plans already exist for parked branches; this plan
> ties them together and holds the work that belongs to neither.

## Goal

TRX and TRC20 activate, show balances and history, and withdraw reliably in
the next beta. Swap participation follows once its own blocker is cleared
(work package 5).

## Work packages

1. **Settle the activation route before anything else.** The CRD
   contradicts itself on how TRON is activated:
   - ch. 35 accepts the TRON platform protocol in `enable_eth_with_tokens`
     (the `ticker` and `nodes[].url` rows of its request table), and ch. 21
     §21.4 lists `enable_eth_with_tokens` and `task::enable_eth::*` as
     TRON's activation route;
   - ch. 55 D55.5 and the code say the opposite: Reloaded has no mmrpc 2.0
     TRON activation, the EVM activator rejects TRX, and TRON activates only
     through legacy `enable` / `electrum`.

   The contradiction predates the beta switch-off. It needs a maintainer
   decision on the route, or a Spec Reader pass if the decision depends on how
   the reference activates TRON. Then ch. 21, ch. 35 and ch. 55 must be made
   to agree. Until then no activation code is written.

   *Decided 2026-11-08:* TRON gets the mmrpc 2.0 route (`enable_eth_with_tokens`,
   `task::enable_eth::*`) and keeps the legacy route. *Decided 2026-11-10:* TRON
   stays switched off in the 0.4.0 beta; the node-client, activation-route and
   switch-on changes move to a later version.
2. **Switch activation back on** (ch. 21 D6): set the constant to `true` and
   retire the refusal tests that go with it.
3. **Node access.** Check Reloaded's TRON node client against ch. 21 §21.9
   and fix what does not match. Items to verify first, from a survey of the
   Reloaded code on 2026-10-26:
   - no per-request timeout is applied, which R-N2 requires;
   - an HTTP error status does not move the request to the next node;
   - one error text still carries the full node URL, against the host-only
     naming rule of ch. 35 R35.1.14 that ch. 21 §21.9 applies to TRON. The
     host-only naming change of 2026-10-28 (ch. 21 R-N1) covers this text;
     confirm it once that change is merged.
4. **Wallet path end to end.** Activation, balance, history and withdraw for
   TRX and one TRC20 token, on a testnet first, then on mainnet with small
   amounts.

   *2026-11-10:* the balance (TRX from the account query, TRC20 through
   `balanceOf`), the coin's own address in the base58check form, `validateaddress`
   and raw-transaction broadcast are wired behind the availability constant (CRD
   ch. 21 §21.10A); the testnet rows follow on a lab build with the constant
   flipped.
5. **Parked branches.** Each has its own plan with its blocker; revive them
   only after work packages 1 and 2:
   - [`tron-swap-support.md`](tron-swap-support.md): `feat/tron-swap-support`,
     TRON V1 atomic swaps. Blocker: its ch. 21 rewrite must pass the KDF
     Dirty Gate before any line of it is reused. That rewrite failed the gate
     on 2026-11-08 and is not reused; the decisions a TRON swap needs were put
     to the maintainer on 2026-11-08 and are recorded in the new ch. 21 section
     of Spec Reader round 0.4.0-6, which lands with the first TRON code PR.
   - [`tron-gasfree.md`](tron-gasfree.md): `feat/tron-gasfree`, the GasFree
     fee-delegated TRC20 withdraw. Blocker: the proxy creation bytecode must be
     pinned and checked against published address vectors.

   Both branches also carry smaller fixes. Collect those that do not depend on
   the blockers and land them separately.
6. **Documentation.** When TRON returns, update the beta-availability notes in
   ch. 21 and ch. 55, `docs/GLEEC_COMPATIBILITY.md`,
   `docs/API_REFERENCE_DEVIATIONS.md` and the two branch plans, and add a
   `CHANGELOG.md` entry.
