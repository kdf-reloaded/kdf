# Chapter 58 — UTXO `signature_version` Contract and the Radiant Sighash

**Status:** driving-spec.

> **One-sentence claim:** the project shall select a UTXO coin's
> signature-hash algorithm from the coin configuration's
> `signature_version` key over a closed set of four accepted values, and
> shall implement the fourth of them — Radiant's fork-id dialect — as a
> BIP-143 preimage with one additional per-output summary digest inserted
> before the outputs digest, so that a Radiant transaction this project
> signs is accepted by the Radiant network and by a reference-lineage
> swap counterparty alike.

## 58.0 Executive Summary

A UTXO coin's signature-hash algorithm is not inferable from its other
configuration: Bitcoin-derived chains have forked the algorithm several
times, and two chains with identical address versions, fee policy and
transaction version can require different preimages. The project
therefore reads the algorithm from a dedicated configuration key.

The shipped third-party coins configuration gives Radiant
(`"coin": "RXD"`) the value `"fork_id_rxd"` together with
`"fork_id": "0x40"`. Reloaded's signature-version enumeration carries
only three values, so that string fails to deserialize and the coin
silently falls back to the Bitcoin-Cash BIP-143 preimage — producing
signatures the Radiant network rejects. This chapter binds the full
value set, the algorithm each value selects, and the Radiant preimage in
full.

**The route question, answered.** Radiant-Core is public, and the sighash
it defines is a shape the project MUST produce to inter-operate with the
Radiant network, so it is a permitted clean input (chapter 01 R4, with
BIP-143 under R3). The Radiant dialect could therefore in principle be
implemented from it without any forbidden-class input. The question
that decides the route is whether the reference's dialect *matches*
Radiant-Core's own sighash or diverges from it, because wire
compatibility with a swap counterparty is what matters. §58.4 answers it:
**they match**, on direct public-network evidence, for every transaction
shape this project can produce. The practical consequence is in R58.4.3 —
implement the algorithm from Radiant-Core, and take from this chapter
only the configuration contract of §58.2, the absence-of-accompanying-
changes findings of §58.5, and the test obligation of T3.

## 58.1 Subsystem Shape

Two surfaces are involved and they are deliberately separate:

1. **Configuration.** The coin-activation configuration builder resolves
   the coin's signature version and its fork-identifier value once, at
   activation, and stores them on the coin's resolved configuration.
   Nothing re-reads the configuration later.
2. **Signing.** The transaction-signing substrate takes the signature
   version and a sighash-type integer per input and produces the digest
   to sign. The substrate is coin-agnostic: it knows signature versions,
   not tickers.

The signature version chosen at (1) therefore reaches every signing site
of the coin — withdrawals, HTLC payments, HTLC spends, HTLC refunds, the
DEX-fee transaction, the raw-transaction signing RPC, and message signing
— with no per-site override. That is the property that makes a wrong
value at (1) uniformly fatal rather than intermittently so.

## 58.2 Bound configuration contract (dictated by third-party coin files)

> **Clearance (chapter 01 T1, R40).** This section was authored by the
> KDF Spec Reader under the chapter-01 §1.4A two-team procedure and
> **passed the KDF Dirty Gate** on 2026-10-24 before being relied on
> (PASS, finding list empty). Its clearance is therefore *complete* and
> not pending: nothing in this section is a candidate awaiting a gate.
> The extent, method and measure of that pass are recorded once, in the
> *Dirty Gate clearance* item of §58.10, and are not restated here.

**R58.2.1** *Accepted values.* The coin configuration key
`signature_version` is a top-level string member of a coin's entry (a
sibling of `coin` and `fork_id`, **not** inside the `protocol` object).
Exactly four values are accepted, and the project shall accept these
spellings verbatim:

| Value | Selects |
| --- | --- |
| `"base"` | The original pre-segwit Bitcoin signature hash: serialize the transaction with every input's script replaced per the sighash type, append the 4-byte little-endian sighash type, double-SHA256 (but see R58.2.4 on the final hashing step). |
| `"witness_v0"` | The BIP-143 segwit version-0 preimage. |
| `"fork_id"` | The BIP-143 preimage reached through the fork-id gate of R58.2.3 — the Bitcoin-Cash dialect, also used by every other chain that signals replay protection with a sighash fork-id bit. |
| `"fork_id_rxd"` | The Radiant dialect of §58.3, reached through the same fork-id gate. |

**R58.2.2** *Default when the key is absent, and the fallback when it is
unrecognised.* When the key is absent, the signature version shall
default to the fork-id value (`"fork_id"`) if either the coin's ticker is
`BCH` or its resolved fork-identifier value (R58.2.5) is non-zero, and to
`"base"` otherwise. When the key is **present but not one of the four
values of R58.2.1**, the same default is taken silently: activation does
**not** fail.

This silent fallback is itself the parity contract and shall be
preserved. It is how the reference behaves, and it is why a coins file
that names a value the running binary does not know produces wrong
signatures rather than a refusal to activate. Reloaded already behaves
this way; the defect is the missing fourth value, not the fallback.

> **Code-quality finding (informative).** The silent fallback is a real
> hazard with a funds consequence, and it is worth a Reloaded
> **improvement** on top of parity rather than a change to it: emit a
> warning at activation naming the unrecognised value and the version
> actually used. A warning changes no wire behaviour, breaks no
> configuration, and would have turned the Radiant failure from "signs
> rejected transactions" into one visible log line at startup. Failing
> activation outright would be the safer behaviour but is a divergence on
> a configuration surface and would need a compatibility switch; the
> warning is not. *(Trigger: a rule the surrounding code plainly
> violates — a funds-path value resolved by a silent fallback — apparent
> while establishing R58.2.2.)*

**R58.2.3** *The fork-id gate.* For the two fork-id values, the dialect
is reached only when the sighash type presented for the input has the
fork-id bit `0x40` set. When that bit is clear, both fork-id values fall
back to the `"base"` algorithm for that input. The bit is therefore part
of the per-input selection and not only of the signed bytes.

Correspondingly, when the project validates sighash-type values under
strict encoding, the two fork-id values shall mask off **both** `0x40`
and `0x80` before checking that the remainder is one of the three defined
base types (all / none / single); the other two values mask off `0x80`
only.

**R58.2.4** *Per-coin final hashing step.* Orthogonally to the signature
version, a coin's signing substrate selects the final hashing step
applied to the assembled preimage of the `"base"` algorithm: double
SHA-256 for every coin except Groestlcoin, which uses a single SHA-256.
The selection is by ticker and is **unchanged** by this chapter:

- Radiant does **not** need a third algorithm. Its transaction
  identifiers and its signature digests are double-SHA256 like
  Bitcoin's, and every digest inside the Radiant preimage of §58.3 is
  likewise a double-SHA256.
- No value of `signature_version` changes this selection, and no
  configuration key exposes it.

**R58.2.5** *The fork-identifier value and the signed sighash byte.* The
coin configuration key `fork_id` is a top-level string member holding a
hexadecimal integer with a `0x` prefix. It defaults to `0x40` for the
ticker `BCH` and to `0x0` for every other coin. The resolved value is
combined with the base sighash type by bitwise OR to form the sighash
type actually signed and appended to the script signature:

| Signing site | Sighash type signed |
| --- | --- |
| Every ordinary spend, withdrawal, DEX-fee transaction, HTLC payment, HTLC spend and HTLC refund | `SIGHASH_ALL \| fork_id` |
| The one documented exception — the taker's pre-signature over a version-two funding output when the DEX fee takes its single-output standard form, where the taker commits to only one output | `SIGHASH_SINGLE \| fork_id`; the burn-bearing and no-fee forms of the same signature use `SIGHASH_ALL \| fork_id` ([Chapter 15](15-swap-v2-utxo-path.md), [Chapter 16](16-swap-v2-pre-burn-output.md)) |

For Radiant, `fork_id` is `0x40` in the shipped configuration, so the
signed sighash type is `0x41` at every site except that one exception,
where it is `0x43`. The 4-byte little-endian encoding of that integer is
what the preimage's trailing sighash-type field carries; the upper 24 bits
are zero, exactly as for Bitcoin Cash. Both values are in the set T2
requires the strict-encoding check to accept.

**R58.2.6** *No accompanying configuration change.* Radiant support
requires **no** other configuration difference: no `chain_variant` entry
([Chapter 37](37-utxo-spv-and-block-header-validation.md) §37.5), no
transaction-version override, no Zcash-style overwintered flags, no
segwit, and no fee-policy special case. The shipped Radiant entry is an
ordinary UTXO entry plus the two keys of R58.2.1 and R58.2.5.

## 58.3 Bound Radiant sighash algorithm (dictated by Radiant)

> **Binding scope.** Everything in this section is the Radiant chain's
> own consensus rule. Radiant-Core is the source of truth and the
> authority a Coder should implement against (R58.4.3); the section is
> written out here so the contract survives in the document set and so
> T3's obligation is unambiguous.

> **Where the authority sits, stated plainly (chapter 01 R36).** The
> fields of R58.3.1 that BIP-143 already defines — 1–7 and 9–11, and the
> digest-collapse rules — are bound by this chapter on BIP-143's own
> authority (§58.9). Everything that is Radiant's addition to them —
> field 8, the per-output summary layout of R58.3.2, and the reference
> extraction, de-duplication and ordering of R58.3.3 — is **bound as
> requirement here while taking its authority from Radiant-Core**: where
> this section and Radiant-Core disagree, Radiant-Core governs and this
> section is the defect of record (chapter 01 R24, and R58.4.2 for the
> details no on-chain vector corroborates). R36's binding-force taxonomy
> has no category for that status — bound but not authoritative — so it
> is stated here rather than left to be inferred from R58.4.3. The status
> is compatible with R17 only because Radiant-Core is one of this
> chapter's cited inputs (§58.9): an implementer reading the chapter
> together with the inputs it cites can still reach the authority.

**R58.3.1** *The preimage.* For the input being signed, with the
fork-id gate of R58.2.3 satisfied, the digest is the double-SHA256 of the
concatenation, in this order:

| # | Field | Width / encoding |
| --: | --- | --- |
| 1 | transaction version | 4 bytes, little-endian |
| 2 | prevouts digest | 32 bytes — as BIP-143 |
| 3 | sequences digest | 32 bytes — as BIP-143 |
| 4 | this input's outpoint | 36 bytes (32-byte transaction id, then 4-byte little-endian index) |
| 5 | script code of the output being spent | compact-size length, then the script bytes |
| 6 | value of the output being spent | 8 bytes, little-endian |
| 7 | this input's sequence | 4 bytes, little-endian |
| 8 | **output-summaries digest** | 32 bytes — R58.3.2, **the only field BIP-143 does not have** |
| 9 | outputs digest | 32 bytes — as BIP-143 |
| 10 | transaction lock time | 4 bytes, little-endian |
| 11 | sighash type | 4 bytes, little-endian (R58.2.5) |

Fields 1–7 and 9–11 are BIP-143 unchanged, including BIP-143's rules for
which of fields 2, 3, 8 and 9 collapse to 32 zero bytes under
`SIGHASH_ANYONECANPAY`, `SIGHASH_NONE` and `SIGHASH_SINGLE`. Field 8 is
inserted between the input part and the outputs digest; it is **not**
appended at the end, and getting its position wrong produces a digest
that verifies against nothing.

**R58.3.2** *The output-summaries digest.* Field 8 is the double-SHA256
of the concatenation of a fixed 76-byte summary per output in scope:

| # | Summary field | Width / encoding |
| --: | --- | --- |
| 1 | output value | 8 bytes, little-endian |
| 2 | double-SHA256 of the output's script-public-key bytes | 32 bytes |
| 3 | number of **distinct** references in that script — the cardinality of the de-duplicated set of R58.3.3, not the number of push operations | 4 bytes, little-endian, unsigned |
| 4 | references digest | 32 bytes — R58.3.3 |

The outputs in scope follow BIP-143's output rules for the sighash base
type:

- base type **all**: every output of the transaction, in transaction
  order;
- base type **single**: the single output at the same index as the input
  being signed, when that index is within range; when it is not, the
  digest is 32 zero bytes;
- base type **none**: 32 zero bytes.

**R58.3.3** *The references digest and reference extraction.* Field 4 of
each summary is the double-SHA256 of the concatenated 36-byte reference
values pushed by that output's script-public-key, **de-duplicated and
ordered ascending by their 36 bytes compared as unsigned bytes**. When
the script pushes no reference, the digest field is 32 **zero** bytes
(not the double-SHA256 of the empty string), and the count field of
R58.3.2 is zero.

A reference is extracted by walking the script-public-key from its first
byte and advancing past each opcode's operand:

| Opcode byte | Operand | Contributes a reference |
| --- | --- | --- |
| `0x01`–`0x4b` | that many data bytes | no |
| `0x4c` (`OP_PUSHDATA1`) | 1 length byte, then that many data bytes | no |
| `0x4d` (`OP_PUSHDATA2`) | 2 little-endian length bytes, then that many data bytes | no |
| `0x4e` (`OP_PUSHDATA4`) | 4 little-endian length bytes, then that many data bytes | no |
| `0xd0` (`OP_PUSHINPUTREF`) | exactly 36 bytes | **yes** |
| `0xd8` (`OP_PUSHINPUTREFSINGLETON`) | exactly 36 bytes | **yes** |
| `0xd1` (`OP_REQUIREINPUTREF`) | exactly 36 bytes | no — skipped |
| `0xd2` (`OP_DISALLOWPUSHINPUTREF`) | exactly 36 bytes | no — skipped |
| `0xd3` (`OP_DISALLOWPUSHINPUTREFSIBLING`) | exactly 36 bytes | no — skipped |
| any other byte | none | no |

Walking stops — leaving the references collected so far in place, without
error — as soon as an operand would run past the end of the script. The
walk is deliberately tolerant: a malformed or truncated script
contributes whatever it contributed before the truncation and does not
fail the signing. Length arithmetic shall be overflow-checked, because
the 4-byte push length is attacker-controlled and a 32-bit target would
otherwise wrap.

**R58.3.4** *Degenerate input indices.* When the index of the input being
signed is not a valid input index, or when the base type is **single**
and that index is not a valid output index, the digest shall be the
32-byte value whose first byte is `1` and whose remaining bytes are zero
— the same sentinel the Bitcoin-Cash dialect uses for those cases, not an
error. This rule exists so that the two fork-id dialects agree on the
degenerate cases.

**R58.3.5** *What the swap paths actually exercise.* Every output this
project constructs on a UTXO chain is a pay-to-public-key-hash output, a
pay-to-script-hash output, or a data-carrier output; none of them pushes
a Radiant reference. For every transaction this project signs,
R58.3.2's reference count is therefore `0` and R58.3.3's digest is 32
zero bytes. The reference-bearing path is still bound in full because the
project must also sign a spend of a *counterparty-chosen* previous
output, and because the raw-transaction signing RPC accepts
caller-supplied scripts.

## 58.4 Reference-against-Radiant comparison, and the route it implies

> **Clearance (chapter 01 T1, R40).** This section was authored by the
> KDF Spec Reader under the chapter-01 §1.4A two-team procedure and
> **passed the KDF Dirty Gate** on 2026-10-24 before being relied on
> (PASS, finding list empty). Its clearance is therefore *complete* and
> not pending: nothing in this section is a candidate awaiting a gate.
> The extent, method and measure of that pass are recorded once, in the
> *Dirty Gate clearance* item of §58.10, and are not restated here.

**R58.4.1** *The reference's Radiant dialect matches Radiant's own.* The
reference lineage carries the algorithm of §58.3 and it agrees with
Radiant-Core. The evidence is public-network evidence and not a reading
of either implementation: the reference's own coverage of the dialect
takes a **Radiant mainnet transaction** that the Radiant network accepted,
recomputes the digest for one of its pay-to-public-key-hash inputs under
the algorithm of §58.3 with sighash type `0x41`, and verifies that
transaction's **real signature** against that digest with that input's
real public key. A signature over a digest produced by a different
algorithm cannot verify; the network's acceptance of the transaction
means Radiant-Core computed the same digest. The agreement is therefore
established for the complete preimage of R58.3.1 and for the zero-
reference form of R58.3.2–R58.3.3.

**R58.4.2** *Where the agreement is not independently established.* The
mainnet evidence of R58.4.1 covers only outputs that push no Radiant
reference (the transaction's outputs are a pay-to-script-hash output, a
data carrier and a pay-to-public-key-hash output). The non-zero-reference
form of R58.3.2–R58.3.3 — in particular the de-duplication and the
ascending byte order of R58.3.3, and the choice of which three
reference opcodes are skipped rather than collected — is **not**
corroborated by any on-chain vector in the reference's coverage. A Coder
shall take those details from Radiant-Core, and shall not treat this
chapter as their authority. By R58.3.5 the uncorroborated form is
unreachable for every transaction the project constructs itself, so the
exposure is limited to spending a counterparty-supplied
reference-bearing output.

**R58.4.3** *Implied route.* Because R58.4.1 holds, implementing the
Radiant dialect from Radiant-Core is both permitted and correct: it
reaches wire compatibility with the Radiant network and with a
reference-lineage swap counterparty in one step, and it is the only route
that settles R58.4.2's details on authority rather than on inference.
What this chapter contributes beyond that is the configuration contract
of §58.2 (which is not in Radiant-Core at all), the
no-accompanying-change findings of R58.2.4 and R58.2.6, and the
cross-check obligation of T3.

## 58.5 Implementation status in reloaded

> **Clearance (chapter 01 T1, R40).** This section was authored by the
> KDF Spec Reader under the chapter-01 §1.4A two-team procedure and
> **passed the KDF Dirty Gate** on 2026-10-24 before being relied on
> (PASS, finding list empty). Its clearance is therefore *complete* and
> not pending: nothing in this section is a candidate awaiting a gate.
> The extent, method and measure of that pass are recorded once, in the
> *Dirty Gate clearance* item of §58.10, and are not restated here.

**R58.5.1** *The gap.* Reloaded's signature-version enumeration carries
`"base"`, `"witness_v0"` and `"fork_id"` and not `"fork_id_rxd"`. Under
R58.2.2 the Radiant entry's unrecognised value therefore resolves to the
fork-id default (its `fork_id` is non-zero), and the coin signs with the
Bitcoin-Cash BIP-143 preimage: field 8 of R58.3.1 is missing and the
digest is wrong for every input. Radiant withdrawals and Radiant swaps
both fail at broadcast.

This is a parity **gap**, not a divergence: no compatibility switch and
no `docs/COMPAT_SWITCHES.md` entry is needed, because the target
behaviour is both the reference's and the chain's.

**R58.5.2** *Scope of the change.* Closing R58.5.1 requires exactly
three things, and nothing else:

1. a fourth accepted value in the signature-version enumeration,
   deserializing from the string `fork_id_rxd` (R58.2.1);
2. the dialect of §58.3 in the signing substrate, selected by that value
   under the fork-id gate of R58.2.3 and falling back to the `"base"`
   algorithm when the gate is not satisfied;
3. extending the strict-encoding sighash-validity check so the new value
   masks `0x40` as well as `0x80` (R58.2.3).

It requires **no** new transaction-hash algorithm (R58.2.4), no
`chain_variant` work, no address-format work, no fee-policy work, and no
change to any other signature version's behaviour.

## 58.6 Tests

> *No reference or network dimension (informative).* No test below
> carries a reference-designation or a netid case. That is an omission
> recorded as deferred work in D4, not a finding that the dimension is
> irrelevant: `AGENTS.md` §3 requires a Reference × Network matrix for a
> change affecting signature-hash modes and signed preimages, and that
> matrix is owed once the question D4 names has been answered.

**T1.** *Configuration resolution.* A coin entry with
`"signature_version": "fork_id_rxd"` resolves to the Radiant dialect; one
with each of the other three strings resolves to that value's algorithm;
one with an unrecognised string and a non-zero `fork_id` resolves to the
fork-id dialect **without failing activation** (R58.2.2); one with an
unrecognised string and a zero or absent `fork_id`, and a ticker other
than `BCH`, resolves to the base algorithm. Each case MUST assert the
resolved version, not merely that activation succeeded.

**T2.** *Fork-id gate and strict encoding.* Under the Radiant value, a
sighash type with bit `0x40` set routes to the Radiant dialect and one
without it routes to the base algorithm (R58.2.3). The strict-encoding
check accepts `0x41`, `0x43`, `0x81` and `0xC2` under the Radiant value
and rejects a value whose low five bits are not one of the three defined
base types.

**T3.** *Radiant mainnet vector (the parity guard).* Take a Radiant
**mainnet** transaction, recompute the digest for one of its
pay-to-public-key-hash inputs under R58.3.1 with that input's previous
script code, that input's previous output value and sighash type `0x41`,
and assert that the transaction's own signature verifies against that
digest under that input's public key. This is the test that proves
agreement with both Radiant-Core and the reference at once, and it is the
only test in this chapter that does. It MUST be a deterministic unit test
with the transaction bytes embedded, not a network test.

**The chapter supplies no such transaction, deliberately.** The
implementer shall obtain the transaction bytes together with the spent
output's script code and value from the public Radiant chain — a
public-network artefact under chapter 01 R6, reachable from any Radiant
block explorer or node — and embed them in the test. Keeping the artefact
out of the document set is the correct choice and not an omission:
nothing obliges a chapter to carry a vector for which it can cite a
public source, and embedding one here would add bytes the document set
would then have to maintain. The consequence is nonetheless worth
stating rather than leaving implicit — this test, alone among the tests
of §58.6, cannot be written from the chapter alone. T4 depends on the
same vector.

**T4.** *Field-8 position.* Assert that the digest of T3 changes if field
8 of R58.3.1 is appended after the lock time instead of inserted before
the outputs digest. A test that only checks "the digest equals this
constant" does not catch an implementation that computes the right
summary digest and places it wrongly, because the constant would simply
have been recorded from the wrong implementation.

**T5.** *Zero-reference summary.* For an output whose script pushes no
Radiant reference, the summary of R58.3.2 carries a reference count of
`0` and a references digest of 32 zero bytes — specifically **not** the
double-SHA256 of the empty byte string (R58.3.3).

**T6.** *Reference extraction.* Given a synthesized script-public-key
containing, in a deliberately non-ascending order, two
`OP_PUSHINPUTREF` pushes, one `OP_PUSHINPUTREFSINGLETON` push, one
`OP_REQUIREINPUTREF` push, a duplicate of one of the first two, and an
ordinary data push whose payload contains bytes that would look like a
reference opcode: the extracted set contains exactly the three distinct
collected references, in ascending byte order, and excludes the skipped
and the embedded ones (R58.3.3). A truncated script whose last operand
runs past the end yields the references collected before the truncation
and no error.

**T7.** *Degenerate indices.* An out-of-range input index, and a
base-type-single signature whose input index exceeds the output count,
both yield the one-then-zeros sentinel of R58.3.4 under both fork-id
values.

**T8.** *Transaction-hash algorithm unchanged.* A Radiant transaction's
identifier is its double-SHA256; the single-SHA256 selection remains
reachable only for Groestlcoin (R58.2.4).

## 58.7 Deferred Work

**D1.** *Unrecognised-value warning.* The activation warning proposed in
the finding under R58.2.2 is not bound as a requirement, because it is an
improvement over parity rather than parity itself and the maintainer may
prefer to fail activation instead (which would need a compatibility
switch). Either decision should be taken once for all coin-configuration
enumerations resolved by silent fallback, not for this key alone.

**D2.** *Radiant beyond signing.* This chapter binds only the
signature-hash contract. Whether Radiant is additionally fit for swaps —
its HTLC script acceptance, its relay-fee floor against the configured
transaction fee, its block-header family, its Electrum server behaviour,
and whether its 300-second average block time interacts badly with the
swap lock-time rules — is out of scope here and unestablished. A Coder
closing R58.5.2 enables correct **signing**; enabling the coin for
trading is a separate decision with its own verification.

**D3.** *Reference-bearing outputs.* R58.4.2 leaves the non-zero-
reference form corroborated only by Radiant-Core and not by an on-chain
vector. If a Radiant mainnet transaction spending a reference-bearing
output becomes available, add it as a second vector under T3; until then
the project's own transactions never reach the path (R58.3.5).

**D4.** *The two-reference question, recorded as open.* This chapter
binds its configuration contract (§58.2), its change scope (R58.5.2) and
its tests (§58.6) with no reference-designation and no netid dimension
anywhere. R58.2.1's accepted value set and R58.2.2's silent fallback are
stated unconditionally; R58.5.2 enumerates three changes and states that
no compatibility switch is needed; no T-numbered test of §58.6 carries a
reference or a network case. The chapter's own derivation record,
meanwhile, is reference-scoped: it states that the Radiant value is
present at the publicly published designation `v3.0.0-beta` and absent
at `v2.6.0-beta`. That asymmetry is exactly the condition under which
`AGENTS.md` §3 bites — §3 lists signature-hash modes and signed
preimages among the dimensions “wire compatible” covers, treats
`v2.6.0-beta` as the primary behavioural reference for netid `8762` and
the v3 lineage as the reference for v3 and netid `6133` behaviour, and
requires a compatibility matrix with a *Reference* row and a *Network*
row for an affected change. This chapter carries neither row, and until
this item existed the omission was not recorded even as deferred.

What is deferred here is the **question**, not an answer to it. Whether
the two designations differ on anything this chapter binds — the closed
value set, the silent fallback, the fork-id gate, the strict-encoding
masking — and whether either netid carries a contract of its own, cannot
be established from the permitted-input classes of chapter 01 §1.3
alone. It is a question for the KDF Spec Reader under the §1.4A
procedure, and this item deliberately does not guess at its outcome:
binding a reference or network dimension that no permitted input
establishes would be a worse defect than recording the gap. Until the
question is answered, this chapter's rules are to be read as stating the
behaviour the Radiant chain requires and that a counterparty at the
newer designation exhibits, and §58.6's tests as carrying no Reference or
Network dimension by omission rather than by finding. Should the answer
introduce a per-designation or per-netid difference, R58.2.1, R58.2.2,
R58.5.2 and §58.6 are the places that must carry it and the §3 matrix is
owed at that point; should it establish that the two designations agree
on this contract, that finding is recorded here and the matrix is
satisfied by a stated equivalence.

## 58.8 Baseline Verifications

**V1.** *Three signature versions at baseline.*

- *Claim.* The baseline tree's signature-version enumeration carries
  three values and no Radiant dialect, so the fourth value is
  post-baseline work and R58.5.1 records an absence rather than a
  regression.
- *Verification.*
  `git show c1d46c0c1592faa0860f704008b2b2381bc3840f:mm2src/mm2_bitcoin/script/src/sign.rs`
  — the signature-version enumeration has three variants, deserializing
  from `base`, `witness_v0` and `fork_id`, and the signature-hash
  dispatcher has the corresponding three arms. (The module is the
  pre-rename ancestor of reloaded's present script crate.)
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md).

**V2.** *Two transaction-hash algorithms at baseline.*

- *Claim.* The per-coin final-hashing selection of R58.2.4 has carried
  exactly two algorithms since the baseline, with a ticker-keyed
  selection.
- *Verification.*
  `git show c1d46c0c1592faa0860f704008b2b2381bc3840f:mm2src/mm2_bitcoin/chain/src/transaction.rs`
  — the transaction module defines a two-valued hashing selector
  (double-SHA256 as the default, single SHA-256) and the coin builder of
  the same commit selects the single-SHA256 value for one ticker only.
- *Cross-reference.* [Chapter 2 — Baseline State](02-baseline-state.md).

## 58.9 External References

- BIP-143 (transaction signature verification for version-0 witness
  programs): fields 1–7 and 9–11 of R58.3.1, the four digest-collapse
  rules, and the compact-size-prefixed script code.
- BIP-141 / BIP-144 for the segwit version-0 context BIP-143 sits in.
- The Bitcoin-Cash replay-protection sighash fork-id bit `0x40` and its
  24-bit fork-identifier field, for R58.2.5.
- Radiant-Core (`https://github.com/Radiant-Core/Radiant-Core`) and the
  Radiant project documentation (`https://radiantblockchain.org`): the
  authority for §58.3 — the inserted output-summaries field, the summary
  layout, the reference opcodes and their 36-byte operands, and the
  reference-set de-duplication and ordering.
- The public third-party coins-configuration file: the Radiant entry's
  `signature_version` and `fork_id` values, the absence of any other
  Radiant-specific key, and the deployed value sets of
  `signature_version` across all coins (R58.2.1).
- [Chapter 37 — UTXO SPV & Block-Header Validation](37-utxo-spv-and-block-header-validation.md)
  §37.5 for the `chain_variant` key R58.2.6 states Radiant does not need.
- [Chapter 38 — UTXO Coin Maintenance & Features](38-utxo-coin-maintenance-and-features.md)
  §38.3 for the address types a Radiant output can take and §38.5 for the
  raw-transaction signing RPC that reaches this contract with
  caller-supplied scripts.
- [Chapter 51 — Legacy (V1) Atomic-Swap State Machine](51-legacy-v1-swap-state-machine.md)
  §51.9.1 for the per-coin secret-hash and signing selections the swap
  paths make alongside the signature version.

## 58.10 Provenance Footer

- *Inputs:* BIP-143 and the public Bitcoin-Cash fork-id sighash
  definition (the preimage fields and the fork-id bit); Radiant-Core and
  the Radiant project documentation (the inserted output-summaries field,
  the per-output summary layout, the reference opcodes and the
  reference-set rule of §58.3); the public third-party
  coins-configuration file (every configuration key name, accepted value
  and deployed value in §58.2); a public-network observation — a Radiant
  mainnet transaction and its own signature — for R58.4.1 and T3; the
  pinned baseline tree (V1, V2); the present reloaded workspace (§58.5).
- *Permitted-input classes used:* R3 (external public specifications —
  BIP-143, BIP-141/144, and the public Bitcoin-Cash fork-id sighash
  definition), R4 (wire formats and external interfaces the project must
  inter-operate with — the Radiant sighash the chain dictates, published
  by its own node implementation, and the coins-configuration key
  schema), R6 (public-network observation — the Radiant mainnet
  transaction behind R58.4.1), R1 (the baseline tree, for V1 and V2).
- *Sibling-allowlist consultations:* none. Radiant-Core is cited under R4
  as the third party that dictates the signed shape, not under R5: it is
  not a repository of this project's organisation and the citation is not
  a pre-baseline-expression citation. No code was transcribed from it
  into this chapter.
- *Forbidden corpus:* consulted **only** to establish, under reference
  designation `v3.0.0-beta` (the designation at which the Radiant dialect
  first appears; `v2.6.0-beta` carries only the first three values of
  R58.2.1): that the accepted value set is closed at the four values of
  R58.2.1 with the dictated spellings; that an unrecognised value falls
  back silently per R58.2.2 rather than failing activation; that the
  fork-id gate and the strict-encoding masking of R58.2.3 apply to both
  fork-id values; that no third transaction-hash algorithm and no other
  per-coin accompaniment arrived with Radiant support (R58.2.4, R58.2.6);
  that the dialect implemented is the one §58.3 describes; and that its
  agreement with Radiant-Core rests on the mainnet-signature check of
  R58.4.1 while the reference-bearing form of R58.4.2 has no on-chain
  corroboration there. The algorithm of §58.3 is Radiant's own consensus
  rule and is sourced from Radiant-Core and the public Radiant
  documentation; the preimage fields it shares with BIP-143 are sourced
  from BIP-143. No discretionary expression — no private type, field or
  function name, no function body, control-flow transcription, helper
  decomposition, local name, error or log literal, and no test
  identifier — was reproduced, and the mainnet transaction behind R58.4.1
  and T3 is a public-network artefact rather than corpus content.
- *Dirty Gate clearance (chapter 01 T1, R40, R43).* The whole of this
  chapter's forbidden-class-sourced material — the scoped item above,
  which is the configuration contract of §58.2 (R58.2.1 through
  R58.2.6), the reference-against-Radiant comparison and the route it
  implies in §58.4, and the gap and change scope of §58.5 — was authored
  by the KDF Spec Reader under the two-team procedure of chapter 01
  §1.4A and **passed the KDF Dirty Gate** on 2026-10-24: PASS,
  discretionary-expression measure 0.03, finding list empty, and
  accordingly no remedy class for any line or for the candidate as a
  whole. This was the chapter's first gate pass; no earlier verdict was
  re-gated. The clearance is therefore **complete and not pending**, so
  R40 withholds nothing here; §58.2, §58.4 and §58.5 each carry a short
  pointer to this item so that a reader entering at the material does not
  have to come here to learn as much. The algorithm of §58.3 is sourced
  from Radiant-Core and the public Radiant documentation rather than from
  forbidden-class material, as the item above records, and §58.3 is
  accordingly not a consulted section. The three elements of the R39
  scope attestation, carried forward here in corpus-free terms as
  chapter 01 T1 requires:

  - *Extent.* The forbidden-class material corresponding to the subject
    matter this chapter's own scope names in §58.1: the coin-activation
    configuration resolution of the two coin-configuration keys this
    chapter binds, and the transaction-signing substrate's
    signature-version selection, its signature-hash computation for the
    two fork-id dialects, and its strict-encoding sighash-validity check.
    Quantity examined: **three files compared** within that scope.
    Reference designation: the publicly published release designation
    `v3.0.0-beta`, which the lineage's own release artefacts carry and
    which §1.7 admits as a public-release fact. **The record is no more
    precise than that designation** — no post-anchor commit identifier,
    no branch state and no location within the compared material is
    given, and that limit is left visible rather than papered over. Not
    compared: this chapter's V-numbered baseline verifications, which
    cite R1 baseline material and fall outside a
    candidate-against-corpus comparison.

  - *Method.* By **reading**, with the per-token forced-versus-free test
    applied individually to every identifier, string literal, constant
    and field-layout statement the candidate carries. No aggregate
    match-density judgement was used or admitted. **No mechanical
    comparison was run:** this repository's R35 gate tooling operates on
    files of source and this candidate embeds no code block, so that
    instrument was not applicable; the absence is stated so that it is
    visible rather than implied.

  - *Measure.* **0.03** on the discretionary-expression basis — the same
    figure the leak report carries, not a second one. Excluded from the
    measured set as dictated or public-surface under the
    forced-versus-free test: the two coin-configuration key names and
    their accepted string values (public third-party configuration
    schema); the sighash flag bits, the three defined base types and
    their combined byte values (the public Bitcoin / Bitcoin-Cash sighash
    definition); the BIP-143 preimage field set, order, widths and
    digest-collapse rules (BIP-143); the Radiant consensus digest layout,
    the per-output summary layout and the script opcode bytes, names and
    operand widths (Radiant-Core and the public Radiant documentation,
    which are public and permissively licensed, relied on **explicitly**
    for this exclusion rather than admitted by proximity to surrounding
    dictated content); and the per-coin final-hashing selection together
    with the degenerate-index sentinel (baseline-existing, R1 / R11).
    **Judgement standard read against:** a pass requires zero findings in
    R39's closed leak-category set, with residual similarity confined to
    the excluded classes above and each residual match individually
    attributable to one of them. The figure sits well below the level at
    which an interop-dense prose candidate would warrant a finding; a
    high volume of dictated matches is the expected shape of such a
    candidate and was not read as evidence.
