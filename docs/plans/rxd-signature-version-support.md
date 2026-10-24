# Plan: Radiant signature-hash support

**Status:** proposed — **Last reviewed:** 2026-10-24

> **Context:** Radiant (`RXD`) is the one coin in the shipped registry whose
> `signature_version` names a signing dialect this build does not implement.
> The unsafe half of that is already closed — the coin no longer signs
> something invalid — so what is left is the feature itself: implement the
> dialect CRD ch.58 binds in full. The chapter is written, gated and cleared,
> so this plan is about test vectors and scope, not about discovering a
> contract.

## Current state

**The configuration.** The shipped registry (`komodo-coins-rin`, root `coins`
at `ea86c441`) gives Radiant `"coin": "RXD"` with
`"signature_version": "fork_id_rxd"` and `"fork_id": "0x40"`. It is the only
entry with that value; the other eight entries carrying a `signature_version`
use `base`, and one (`XEP-segwit`) uses `witness_v0`.

**What we do with it today.** Reloaded's signature-version enumeration carries
`base`, `witness_v0` and `fork_id` only. Per ch.58 **R58.5.1**, the
unrecognised string once resolved to the fork-id default — its `fork_id` is
non-zero — so the coin activated and signed with the Bitcoin-Cash BIP-143
preimage: field 8 of R58.3.1 missing, the digest wrong for every input, and
every Radiant withdrawal and swap rejected at broadcast. Silently.

**What changed.** Branch `fix/signature-version-guard` (**PR #54**, open)
replaced that silent fallback with an explicit activation failure. A present
but unrecognised `signature_version` now fails the conf build with
`UtxoConfError::InvalidSignatureVersion`, naming the value, the accepted set
and the switch. The divergence is switchable: the top-level `MM2.json` boolean
`coin_conf_unrecognised_enum_falls_back` (default `false`) restores the
reference's silent-fallback contract, with one `warn!` per activation. An absent
key or JSON `null` takes the default under both settings. The same branch also
made a malformed or non-string `fork_id` fail with
`UtxoConfError::InvalidForkId` under every setting, where it used to panic on
`"0xZZ"` and silently default on a number. **Under the default, RXD is the only
registry coin that refuses to activate.** The two address-only helpers
(`address_by_conf_and_pubkey_str`, `get_private_keys`) never read the signing
algorithm, so an RXD public key still maps to an address.

So the current state is: safe, documented, and unsupported. Read PR #54's own
description for the reasoning and the eleven conf-builder tests behind it, and
read `docs/reloaded-rewrite/58-utxo-signature-version-and-radiant-sighash.md`
for the contract rather than this plan's summary of it.

**The chapter is ready to implement from.** Ch.58 §58.2 (the configuration
contract) and §58.5 (the implementation status) were authored by the KDF Spec
Reader under the chapter-01 §1.4A two-team procedure and **passed the KDF Dirty
Gate on 2026-10-24** with an empty finding list, so neither is a candidate
awaiting a gate. §58.3 — the Radiant dialect itself, a BIP-143 preimage with
one inserted per-output summary digest as field 8, before the outputs digest
and not appended — takes its authority from Radiant-Core, which is a permitted
clean input (ch.01 R4, with BIP-143 under R3). **R58.4.3** settles the route:
implement the algorithm from Radiant-Core, and take from the chapter only the
configuration contract of §58.2, the absence-of-accompanying-changes findings
of §58.5, and the test obligation of T3. Nothing in this work package needs a
Spec Reader dispatch to begin.

**R58.5.2** fixes the scope at exactly three changes: a fourth accepted
enumeration value deserializing from `fork_id_rxd`; the §58.3 dialect in the
signing substrate, selected by that value under the R58.2.3 fork-id gate and
falling back to the `base` algorithm when the gate is not satisfied; and
extending the strict-encoding sighash-validity check so the new value masks
`0x40` as well as `0x80`. No new transaction-hash algorithm, no `chain_variant`
work, no address-format work, no fee-policy work, and no change to any other
signature version's behaviour.

## The gating practical requirement: test vectors

Ch.58 **T3** is the only test in the chapter that proves agreement with both
Radiant-Core and the reference at once, and the chapter deliberately supplies
no vector for it: the implementer obtains a Radiant **mainnet** transaction,
together with the spent output's script code and value, from the public chain
(a public-network artefact under ch.01 R6) and embeds the bytes in a
deterministic unit test. **T4** depends on the same vector. These two are the
only tests in §58.6 that cannot be written from the chapter alone, so sourcing
the vector is the first real task and the gate on the whole package — an
implementation without T3 is an implementation nobody can show to be correct.

T5–T8 (zero-reference summary, reference extraction and de-duplication,
degenerate indices, transaction-hash algorithm unchanged) are writable from the
chapter. **D3** notes that no on-chain vector corroborates the
reference-bearing-output form; if a Radiant mainnet transaction spending one
becomes available, add it as a second vector under T3.

## The visible-balance trade-off (already recorded against PR #54)

PR #54 weighed showing a balance against refusing to activate and chose
refusal: **a wallet that shows a balance it cannot spend, with no explanation,
is worse than one that says which value it does not support.** An operator who
prefers the view sets `coin_conf_unrecognised_enum_falls_back = true` and gets
the reference's behaviour — RXD activates, shows its balance, and signs in a
way Radiant rejects. That trade-off stands as recorded; this plan does not
reopen it, it removes the need for it.

**This belongs in the beta release notes**: an operator upgrading with RXD in
their coins file sees a coin stop activating, and needs to be told why, that
the switch exists, and that support is planned rather than withdrawn.

## Steps

1. **Land PR #54** (or confirm it landed). Everything below assumes the guard
   and the switch are in place; without them the coin signs invalid
   transactions instead of refusing.
2. **Source the T3 vector.** A Radiant mainnet transaction with a
   pay-to-public-key-hash input, plus that input's previous script code and
   previous output value, recorded with its source. Verify by hand, once, that
   the transaction's own signature verifies against the digest computed under
   R58.3.1 with sighash type `0x41`, before any production code exists.
3. **Implementation via the Coder**, from ch.58 — the three changes of R58.5.2
   and nothing else. The Coder implements the algorithm against Radiant-Core
   per R58.4.3.
4. **Tests T1–T8** of §58.6, with T3 and T4 built on the step-2 vector. T4 in
   particular must assert the digest *changes* when field 8 is appended after
   the lock time instead of inserted before the outputs digest — a
   constant-equality test would not catch a correctly computed, wrongly placed
   summary digest.
5. **Close the records.** Mark R58.5.1 closed in ch.58 §58.5 and answer **D1**
   (the unrecognised-value warning, which PR #54 settled in practice by failing
   activation behind a switch). Note that ch.58's §58.6 preamble and **D4**
   record the missing Reference × Network test matrix as deferred — this change
   affects signature-hash modes and signed preimages, so `AGENTS.md` §3 asks
   for that matrix, and it is owed once D4's question is answered.
6. **Compatibility bookkeeping.** Implementing a value the reference already
   implements is parity, not divergence, so per ch.58 R58.5.1 it needs no
   compatibility switch and no `docs/COMPAT_SWITCHES.md` entry of its own.
   PR #54's switch keeps its entry in `docs/GLEEC_COMPATIBILITY.md` and
   `RELOADED_VS_GLEEC.md`; check whether its wording needs amending once RXD
   is no longer the coin it refuses.

## Reference pointer for a KDF Spec Reader dispatch

> **A clean context must not open this.** It is recorded here solely to direct a
> future KDF Spec Reader dispatch — the only role permitted to read
> upstream/GLEEC KDF material. The number and description were supplied by a
> community maintainer and transcribed without being fetched, opened, previewed
> or verified.

| Upstream reference | As described to us |
|---|---|
| pull request **#2713** | what Radiant "did to get it working" |

Note that this pointer is **not** on the critical path: ch.58 §58.4 already
established, on public-network evidence, that the reference's dialect and
Radiant-Core's own sighash **match** for every transaction shape this project
can produce, and R58.4.3 therefore routes implementation through Radiant-Core.
Dispatch a Spec Reader on #2713 only if an implementation that satisfies T3
still fails against a reference-lineage counterparty, or to answer **D2**
(whether Radiant is fit for swaps at all beyond signing).

## What done looks like

- A coins file carrying `"signature_version": "fork_id_rxd"` activates with no
  switch set, and resolves to the Radiant dialect — asserted on the resolved
  version, not merely on activation succeeding (T1).
- An RXD withdrawal built and signed by this node is accepted by the Radiant
  network.
- T3 passes as a deterministic unit test with embedded mainnet bytes, and T4
  fails if field 8 moves.
- Ch.58 R58.5.1 is closed and D1 is answered in the chapter.
- The beta release notes explain the activation refusal, the switch, and that
  it is now gone for RXD.

## Out of scope

- **D2 — Radiant beyond signing.** HTLC script acceptance, the relay-fee floor
  against the configured transaction fee, its block-header family, its Electrum
  server behaviour, and whether its roughly 300-second block time interacts
  badly with the swap lock-time rules are all unestablished. Closing R58.5.2
  enables correct **signing**; enabling RXD for *trading* is a separate decision
  with its own verification, and should not be announced as a side effect of
  this one.
- The other coin-configuration enumerations PR #54's switch is named for
  (`estimate_fee_mode`, `trezor_coin`, `mtp_block_count`,
  `estimate_fee_blocks`, the address-format keys). None is wired to it; adopting
  them is its own decision.

## Risks

- **A single vector proves one shape.** T3 on a pay-to-public-key-hash input
  with sighash `0x41` does not exercise `SIGHASH_ANYONECANPAY`,
  `SIGHASH_NONE`, `SIGHASH_SINGLE` or a reference-bearing output. The digest
  collapse rules and R58.3.4's sentinel are tested against the chapter only
  (D3). Treat an unverified shape as unverified in the release notes.
- **Enabling activation re-exposes the trade-off in reverse.** If the dialect
  is implemented but subtly wrong, the coin activates, shows a balance and
  fails at broadcast — exactly the state PR #54 removed. T3 is what keeps that
  from happening, which is why it gates rather than follows.
- **The switch's documented wording goes stale.** `GLEEC_COMPATIBILITY.md` and
  `RELOADED_VS_GLEEC.md` currently describe a switch whose only practical
  effect is on RXD. Amend them in the same commit as the implementation, not
  after.
