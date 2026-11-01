# Plan: swap and transaction-path hardening

**Status:** planning (stub; must be elaborated before any work is scheduled) — **Last reviewed:** 2026-10-26

> This plan is a stub. It names the areas a hardening review of the swap and
> transaction paths must cover. It does not yet contain findings, designs or
> test scenarios, and it must be elaborated before any work is scheduled.

## Scope

Every coin family that takes part in swaps or sends transactions: UTXO
(Electrum and native), EVM, the shielded coins, Sia, and any family added later.

## Areas to cover

1. **Reliance on server answers.** Which decisions in the swap and send paths
   rest on the answer of a single server, and where a second source or a local
   verification is warranted.
2. **Transaction broadcast and sequence handling.** Robustness when the
   configured servers disagree, lag behind, or answer incorrectly.
3. **Bounded waits.** Every step that waits on server state has an upper bound
   and a defined outcome the swap state machine can act on (see CRD ch.35 D35.9).
4. **Server selection and failover.** How the choice of server interacts with
   the areas above.
5. **Shipped defaults.** The default server lists and URL schemes that clients
   ship, and what Reloaded should warn about.
6. **Test strategy.** Deterministic tests with stub servers that misbehave.

## Related

- CRD ch.35 §35.1.F (EVM multi-node access), ch.38 R38.6.4 (Electrum),
  ch.39 §39.11 (lightwalletd).
- [`v2-swap-engine-hardening.md`](v2-swap-engine-hardening.md): a different
  scope (engine dedup and error paths).
