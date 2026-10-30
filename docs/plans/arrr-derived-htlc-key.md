# Plan: ARRR's per-swap HTLC key derived as the reference derives it

**Status:** done (PRs #140, #142, #146, #152; field recovery passed 2026-10-29) — **Last reviewed:** 2026-10-30

> **Context.** Reloaded gives a shielded coin's swap leg a random HTLC key and saves its private
> key with the swap. The reference instead derives that key from the coin's transparent key and
> a per-swap input, and never stores it (CRD ch.39 R39.5.6–R39.5.9, gated 2026-10-28).
>
> Swaps made by the reference therefore cannot be recovered by Reloaded, and the reverse also
> holds. A field case: a September ARRR maker swap's refund key is derived, and Reloaded's
> recovery cannot sign it.

## Steps

1. **CRD.** Ch.39 R39.5.6–R39.5.13, T39.5.a–f and D39.5.a; the candidate order added to
   R39.5.4; pointers from ch.51.
2. **Key-pair consistency fix** (R39.5.4). It is the safety net: the named "no private key held"
   stop. A separate pull request — done, PR #142.
3. **Code — done.** The ZCoin HTLC key is derived per R39.5.8 (the maker uses the secret hash, the taker
   the uuid). It is used for payment, spend and refund, and re-derived on restart and in
   recovery, with the candidate order (1) derived, (2) a saved key that belongs to the saved
   pubkey, (3) persistent.
   - The private key is no longer written to the swap record.
   - Swaps already in flight keep their saved key and keep working.
   - Under an HD wallet the base key is the transparent key at `derivation_path` (D39.5.a).
4. **Bit-exact check (T39.5.a) — done.** A vector from a reference-made ARRR swap on a throwaway
   non-HD test wallet. The code from step 3 is not merged before this passes. The vector
   came from two swept throwaway wallets on 2026-10-28, and the derivation matches it bit for
   bit for both roles, plus two low-S cases computed from the rule.
   - The `Negotiated` events' `taker_pubkey` / `maker_pubkey` are 33 zero bytes in both
     implementations. Reloaded parses that value and uses the HTLC public keys instead, so a
     vector needs only the `Started` and `Negotiated` HTLC public keys.
5. **Field recovery — done 2026-10-29.** The operator retried the stuck ARRR refunds, the
   September swap among them, on dev `8b5acc41a`. One refund was broadcast
   (`9c94bb484f67ae4e6a8a23bdd7e986c8fcb5e2af4f206ca1f94edf658dd6e6cf`), and the others answered
   that they were already refunded, by a transaction that matched the derived key. On a build
   without PR #152 the same swaps stopped with the named error of step 2, as expected.
6. **Refund timing — done** (R39.5.13, maintainer Decision AP, 2026-10-28). The shielded refund waits
   until the chain's median time past has passed the lock time, and a refund rejected as
   non-final is retried until the refund deadline. A separate pull request; it does not wait
   for step 4.

## Decisions

- **D39.5.a, decided 2026-10-28: follow the reference.** Under an HD wallet the coin's
  transparent key, and so the HTLC base key, is the key at the coins-file `derivation_path`
  (`m/<derivation_path>/0'/0/0`, R39.6.4 §2). It is implemented in the step 3 pull request,
  with a test that the HD and seed policies give different base keys and that recovery
  re-derives from the policy in force.
