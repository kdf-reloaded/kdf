# Plan: Radiant signature-hash support

**Status:** parked (signing shipped in PRs #54, #65; step 6 compatibility bookkeeping and step 8 open) — **Last reviewed:** 2026-10-29

> **Context:** Radiant (`RXD`) is the one coin in the shipped registry whose
> `signature_version` names a signing dialect this build did not implement.
> The unsafe half of that was closed first — the coin no longer signs
> something invalid (PR #54) — and the feature itself is now **implemented and
> merged** (PR #65, 2026-10-25): the dialect CRD ch.58 binds, with two
> mainnet vectors behind it. What is left is the compatibility bookkeeping
> of step 6 and one follow-up the implementation surfaced (ch.58 D5, not a beta item).

## Where it stands (2026-10-29)

| Item | State |
|---|---|
| PR #54 (guard + switch) | merged 2026-10-25 |
| Steps 2–4 (vectors, implementation, tests T1–T8) | **done** — merged in PR #65, 2026-10-25 |
| Step 5 (close the records) | ch.58: R58.5.1 closed, R58.5.2 implemented, D1 and D4 closed, D3 partly corroborated — done 2026-10-25 |
| Step 6 (compatibility bookkeeping) | **open** — to be done in the implementation PR (see step 6) |
| R58.3.3 correction against Radiant-Core | **done** in ch.58 on 2026-10-25 (see "The R58.3.3 correction" below) |
| Fail-early follow-up (ch.58 D5) | **open**, owner ch.58 §58.3, not a beta item |

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

**The chapter was ready for the Coder, and the Coder has delivered: implemented and
merged in PR #65.** Every forbidden-class-sourced
part of ch.58 is cleared: §58.2 (the configuration contract), §58.4 (the
reference-against-Radiant comparison) and §58.5 (the implementation status)
were authored by the KDF Spec Reader under the chapter-01 §1.4A two-team
procedure and **passed the KDF Dirty Gate on 2026-10-24** with an empty
finding list; §58.4A (the reference change's extent, coverage and
designations, added the same day) passed in a **second pass** after a
first-pass FAIL was remediated, also with an empty finding list. Nothing in
the chapter is a candidate awaiting a gate, and §58.10 carries both
clearance records. §58.3 — the Radiant dialect itself, a BIP-143 preimage with
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
signature version's behavior. **Confirmed from the cleared §58.4A:** R58.4A.1
finds the reference change itself confined to exactly those three items (plus
one unrelated documentation-comment edit that binds nothing), and the
informative note beneath it records that no Radiant `chain_variant` value is
recognized at any released designation — a coin entry naming one is rejected
at activation under ch.37 R37.5.1, whatever it says under `signature_version`.

**Designations.** Per R58.4A.4, the reference change is first carried by
`v3.0.0-beta`, is absent from `v2.6.0-beta` (which carries only the first
three values), and no newer release existed on 2026-10-24. A `v2.6.0-beta`
node cannot produce a valid Radiant signature and so cannot be a Radiant swap
counterparty at all (R58.4A.5).

**What was built (2026-10-25).** All three changes of R58.5.2, in
`mm2src/kdf_script/src/sign.rs`: the fourth enumeration value
(`SignatureVersion::ForkIdRxd`, serde `fork_id_rxd`; the conf builder's
accepted-values list now names four values); the dialect routine
`sighash_fork_id_rxd`, routed only when the fork-id bit `0x40` is set and
falling through to the legacy algorithm otherwise, with the R58.3.4 sentinel,
the summary helpers `hash_output_summaries` / `append_output_summary`, and the
tolerant, overflow-checked reference walk `extract_push_refs` (collects
`0xd0`/`0xd8`, skips `0xd1`–`0xd3` as 36-byte operands, ordered set, 32 zero
bytes for a reference-free output); and the strict-encoding extension
(`is_defined` masks `0x40|0x80` for the new value, `from_u32` recognizes the
fork-id bit for it). Ch.38 R38.5.4 holds — every signing site,
`sign_raw_transaction` included, gets the resolved version. No address, fee,
Electrum or swap change. The full statement is ch.58 §58.5 "as-built
statement".

## The R58.3.3 correction (2026-10-25)

Verifying the implementation against Radiant-Core's public source turned up a
defect in the chapter, not in the dialect: R58.3.3 as written bound a
**tolerant** reference walk (a malformed or truncated output script
contributes what it contributed so far and never fails the signing), and that
is what was implemented. Radiant-Core's walk is **fatal** — it rejects an
undecodable or truncated output script, a reference named by
`OP_DISALLOWPUSHINPUTREF` that is also pushed, more than one
`OP_STATESEPARATOR`, and an `OP_RETURN` preceding a state separator; such a
transaction has no digest and fails verification on chain. §58.3 takes its
authority from Radiant-Core, so the chapter was the defect of record (ch.01
R24) and R58.3.3 has been rewritten to state Radiant-Core's rule, with a
verification record naming the Radiant-Core file classes read.

The impact of the as-built tolerance is nil for anything this project
constructs (P2PKH, P2SH and OP_RETURN outputs only, R58.3.5) and bounded for a
caller-supplied script via `sign_raw_transaction`: the implementation signs a
transaction the network rejects anyway — never a wrong-but-valid signature.
Making the hash fallible is a fourth change outside R58.5.2 and is deferred as
**ch.58 D5** (fail early on the four cases; owner §58.3; not a beta item). The
as-built divergence note under R58.3.3 and the T6 note record which test
clause inverts when D5 is done.

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
chapter. **D3** noted that no on-chain vector corroborated the
reference-bearing-output form; one was found (below).

**Vectors sourced (2026-10-25).** Two Radiant mainnet transactions, fetched
from a Radiant Electrum server listed in the shipped coins registry (ch.01 R6)
and embedded as bytes in deterministic unit tests:

| Vector | Transaction | Covers |
|---|---|---|
| 1 | `10fa6331635fadede304b279cc3f823a65f54a2f592e9c157d7fea6229232521`, block 475700 | two P2PKH inputs, no reference-bearing output — the zero-reference form |
| 2 | `e9da468db4109e5bc3d99edeb82b73d6547642906f37612675095e1b22b9a0fc`, block 475790 | output 0 pushes two distinct references in non-ascending script order plus reference-opcode bytes embedded inside `OP_PUSHDATA1` payloads; output 1 pushes one; input 1 is the P2PKH input tested — the reference-bearing form D3 wanted |

Each input's own DER signature verifies **only** with the summary digest
inserted before the outputs digest; appended after the lock time it fails
(T4); omitted — the plain Bitcoin-Cash preimage — it fails. For vector 2,
**sorted** references verify and script-order references fail, so D3's
ascending-order rule and the singleton/plain collection into one set are now
corroborated on chain. Still synthetic only (T6): de-duplication of a
reference pushed twice, and the skipped opcodes `0xd1`–`0xd3`; D3 stays open
for those two. T8 is confirmed on both vectors (identifier = double-SHA256).
T1's unrecognized-string cases run under PR #54's fall-back switch, because
under the default an unknown value fails activation. T2 accepts `0x41`,
`0x43`, `0x81`, `0xC2` and rejects `0x00`, `0x04`, `0x5F`, `0x141`, `0x1C2`,
`0xFFFFFFC2`, `0x40`, `0x80`, `0xC0`.

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

*State as of 2026-10-29: steps 1–4 done and merged (PRs #54, #65, 2026-10-25);
step 5 done in ch.58; step 6 open; steps 7–8 added 2026-10-25.*

1. **Land PR #54** (or confirm it landed). Everything below assumes the guard
   and the switch are in place; without them the coin signs invalid
   transactions instead of refusing. *Open — #54 waits on #57.*
2. **Source the T3 vector.** *Done 2026-10-25 — two vectors, see above.* A Radiant mainnet transaction with a
   pay-to-public-key-hash input, plus that input's previous script code and
   previous output value, recorded with its source. Verify by hand, once, that
   the transaction's own signature verifies against the digest computed under
   R58.3.1 with sighash type `0x41`, before any production code exists.
   Per the cleared **R58.4A.2**, Radiant-Core publishes no fixed sighash
   vector for its dialect (its data-driven vector test is disabled and its
   remaining sighash test uses random scripts with no reference opcode), so
   *any* vector — the reference's included — can only come from a transaction
   the Radiant network accepted. Derive ours from the public chain under
   ch.01 R6, and prefer an independently chosen transaction over whichever one
   the reference used: that makes T3 an independent corroboration of R58.4.1
   rather than a re-run of the reference's check. A vector for the
   reference-bearing form (**D3**) can likewise come only from a public-chain
   artifact, and none is known; D3 stays open on that basis alone.
3. **Implementation via the Coder**, from ch.58 — the three changes of R58.5.2
   and nothing else. *Done 2026-10-25, merged in PR #65.* The Coder implements the algorithm against Radiant-Core
   per R58.4.3. Scope is confirmed, not merely asserted: the cleared R58.4A.1
   finds nothing in the reference change beyond R58.5.2's three items, and no
   Radiant `chain_variant` value is recognized at any released designation,
   so there is no fourth item to discover.
4. **Tests T1–T8** of §58.6, with T3 and T4 built on the step-2 vector. *Done
   2026-10-25, merged in PR #65; as-built notes under each test in ch.58 §58.6.* T4 in
   particular must assert the digest *changes* when field 8 is appended after
   the lock time instead of inserted before the outputs digest — a
   constant-equality test would not catch a correctly computed, wrongly placed
   summary digest.
5. **Close the records.** *Done 2026-10-25 in ch.58: R58.5.1 closed, R58.5.2
   implemented (PR #65), D1 closed (answered by PR #54), D4 closed (matrix
   carried into §58.6), D3 partly corroborated and kept open for the two
   synthetic-only details.* Mark R58.5.1 closed in ch.58 §58.5 and answer **D1**
   (the unrecognized-value warning, which PR #54 settled in practice by failing
   activation behind a switch). **D4's question is answered** in the cleared
   §58.4A: R58.4A.5 finds exactly one inter-designation difference on this
   contract — the fourth value is present at `v3.0.0-beta` and absent at
   `v2.6.0-beta` — and no netid dimension at all (nothing in the two keys'
   resolution or in the signing substrate reads the network identifier). The
   `AGENTS.md` §3 matrix is therefore satisfied by the stated difference on the
   reference row and the stated equivalence on the network row; what remains
   is to carry that finding into §58.6 alongside the implementation and close
   D4 together with D1 in this step.
6. **Compatibility bookkeeping.** Implementing a value the reference already
   implements is parity, not divergence, so per ch.58 R58.5.1 it needs no
   compatibility switch and no `docs/COMPAT_SWITCHES.md` entry of its own.
   PR #54's switch keeps its entry in `docs/GLEEC_COMPATIBILITY.md` and
   `RELOADED_VS_GLEEC.md`; check whether its wording needs amending once RXD
   is no longer the coin it refuses. *Open — belongs in the implementation
   PR.* Also stale once it lands: `docs/reloaded-rewrite/UPSTREAM-PARITY-GAPS.md`
   #9 (marked OPEN) and ch.38 R38.5.4's clause "the one coin whose dialect
   reloaded does not yet support".
7. **R58.3.3 correction.** *Done 2026-10-25 in ch.58* — see "The R58.3.3
   correction" above. Nothing in the implementation changes for it; the
   chapter now records the as-built tolerance as a divergence.
8. **Fail-early follow-up (ch.58 D5).** *Open; owner ch.58 §58.3; not a beta
   item.* Make the Radiant dialect's signature-hash computation fallible on the
   four fatal cases of R58.3.3, returning a typed error instead of a digest;
   invert T6's truncation clause and add the other three cases. Separate PR
   after the implementation lands; it changes the signing substrate's error
   surface and so is deliberately not folded into R58.5.2's three changes.

## Reference pointer for a KDF Spec Reader dispatch (dispatched 2026-10-24)

> **A clean context must not open this.** It is recorded here solely to direct a
> KDF Spec Reader dispatch — the only role permitted to read upstream/GLEEC KDF
> material. The number and description were supplied by a community maintainer
> and transcribed without being fetched, opened, previewed or verified.

| Upstream reference | As described to us |
|---|---|
| pull request **#2713** | what Radiant "did to get it working" |

**Dispatched on 2026-10-24.** The Spec Reader's findings are in ch.58 §58.4A,
which passed the KDF Dirty Gate in a second pass the same day (a first pass
returned FAIL and was remediated); see ch.58 §58.10 for the clearance record.
The findings confirm the route of R58.4.3 rather than change it: the change is
confined to the three items of R58.5.2 (R58.4A.1); its only on-chain vector is
a public-chain artifact and Radiant-Core publishes no fixed vector of its own
(R58.4A.2); no Radiant-specific behavior exists outside the signing substrate
(R58.4A.3); the designations are `v3.0.0-beta` (first) and `v2.6.0-beta`
(absent) (R58.4A.4); and D4 is answered (R58.4A.5). No further dispatch is
needed for this work package. A second dispatch would be warranted only if an
implementation that satisfies T3 still fails against a reference-lineage
counterparty, or to verify rather than report what the lineage says about
**D2** (below).

## What done looks like

- A coins file carrying `"signature_version": "fork_id_rxd"` activates with no
  switch set, and resolves to the Radiant dialect — asserted on the resolved
  version, not merely on activation succeeding (T1). *Done in PR #65.*
- An RXD withdrawal built and signed by this node is accepted by the Radiant
  network. *Not yet exercised against the live network; the two mainnet
  vectors are the evidence so far.*
- T3 passes as a deterministic unit test with embedded mainnet bytes, and T4
  fails if field 8 moves. *Done — two vectors.*
- Ch.58 R58.5.1 is closed and D1 is answered in the chapter. *Done
  2026-10-25.*
- The beta release notes explain the activation refusal, the switch, and that
  it is now gone for RXD. *Open — with step 6.*

## Out of scope

- **D2 — Radiant beyond signing.** HTLC script acceptance, the relay-fee floor
  against the configured transaction fee, its block-header family, its Electrum
  server behavior, and whether its roughly 300-second block time interacts
  badly with the swap lock-time rules are all unestablished. The cleared
  R58.4A.3 records that the reference lineage *reports* a completed legacy
  (version-one) RXD/KMD swap under the dialect; that is reported, not verified
  by this project, bears on the legacy swap path only, and says nothing about
  version-two swaps, watchers or the trade preimage. It does not close D2.
  Closing R58.5.2 enables correct **signing**; enabling RXD for *trading* is a
  separate decision with its own verification, and should not be announced as
  a side effect of this one.
- The other coin-configuration enumerations PR #54's switch is named for
  (`estimate_fee_mode`, `trezor_coin`, `mtp_block_count`,
  `estimate_fee_blocks`, the address-format keys). None is wired to it; adopting
  them is its own decision.

## Risks

- **Two vectors prove two shapes.** T3's vectors are P2PKH inputs with
  sighash `0x41`, one spending from a transaction without and one with
  reference-bearing outputs. They do not exercise `SIGHASH_ANYONECANPAY`,
  `SIGHASH_NONE` or `SIGHASH_SINGLE`; the digest collapse rules, R58.3.4's
  sentinel, reference de-duplication and the skipped opcodes `0xd1`–`0xd3`
  are tested against the chapter and Radiant-Core only (D3). Treat an
  unverified shape as unverified in the release notes.
- **The walk is tolerant where Radiant-Core's is fatal** (ch.58 R58.3.3
  as-built note, D5). Bounded: no project-constructed output can trip it, and
  for a caller-supplied script it yields a signature on a transaction the
  network rejects anyway. Not a funds-safety risk; a diagnostics one.
- **Enabling activation re-exposes the trade-off in reverse.** If the dialect
  is implemented but subtly wrong, the coin activates, shows a balance and
  fails at broadcast — exactly the state PR #54 removed. T3 is what keeps that
  from happening, which is why it gates rather than follows.
- **The switch's documented wording goes stale.** `GLEEC_COMPATIBILITY.md` and
  `RELOADED_VS_GLEEC.md` currently describe a switch whose only practical
  effect is on RXD. Amend them in the same commit as the implementation, not
  after.
