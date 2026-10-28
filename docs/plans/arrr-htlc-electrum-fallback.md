# Plan: ARRR swap questions without an address index (Electrum fallback, own-send record)

**Status:** in progress — **Last reviewed:** 2026-10-28

> **Context:** decided by the maintainer on 2026-10-28 (Decision AK).
>
> An ARRR swap asks three transparent questions about its HTLC: was my payment already
> sent, was this payment spent, and (a polling wait) has the counterparty spent it. Since
> 2026-10-12, Reloaded asks them of the address index of the node behind each
> `lightwalletd` server. Two servers of the published ARRR list run nodes without that
> index, so a swap that reaches only those servers fails at its payment step.
>
> The reference answers the same questions from Electrum (CRD ch.39 R39.11.23). On
> 2026-10-28 one published ARRR ElectrumX served the chain's tip, on the same chain as the
> `lightwalletd` endpoints.

## Decided

- **Electrum as fallback for the three questions** (ch.39 R39.11.25). It is asked only when
  no `lightwalletd` endpoint can answer, and only after a chain-identity check (block hash
  at a recent height) and a tip-lag bound. It is never used for heights, broadcast or the
  wallet.
- **The wallet's own send record answers "already sent" first** (R39.11.26; D39.11.h
  adopted).
- **A fresh swap skips "already sent" for a shielded coin's own payment** (R39.11.27).
  Every other coin keeps the check.
- **A taker paying with a Light-mode shielded coin is refused at start**, before its fee,
  when no source can watch its payment (R39.11.28).
- **Rejected:** keeping the fail-fast as the only behavior; treating "cannot tell" as "not
  sent"; detecting spends by reading blocks.

## Steps

1. **CRD and documents.** Ch.39 §39.11.5 (the reference contract, R39.11.18–R39.11.24,
   gated) plus the decided rules R39.11.25–R39.11.28 and tests T39.11.l–T39.11.s. Ch.38
   §38.6.4 carve-out, ch.55 rows, `GLEEC_COMPATIBILITY.md`. This plan.
2. **Own-send record** (R39.11.26). Look up the wallet database's record of own sends
   before any server is asked. Short.
3. **Electrum fallback** (R39.11.25). Wire the Electrum client into the shielded coin
   for the three questions only: identity check, lag bound, script-history and unspent
   reads decoded with the Zcash reader, decision rules and logging.
4. **Fresh-swap skip and taker refusal** (R39.11.27, R39.11.28) in the legacy swap state
   machine.
5. **Field check.**
   - An ARRR swap between two Reloaded nodes whose `lightwalletd` list holds only the
     no-index servers, with the ElectrumX configured.
   - The recovery black box against the same servers.
   - A swap with the published list, unchanged.

Each step is its own pull request and stays green and reviewable on its own.

## Open questions

- The tip-lag bound and the identity height (10 blocks each) are first values. Revisit them
  after the field check.
- Only one published ARRR ElectrumX followed the chain on 2026-10-28. Should more hosts be
  asked to run one, or should the published list be reordered so index-capable
  `lightwalletd` servers come first? That is for the coins-file maintainers, not this
  repository.
