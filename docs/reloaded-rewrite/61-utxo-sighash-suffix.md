# Chapter 61 — The Configurable Sighash Suffix in the UTXO Signature Hash (Bitcoin II)

**Status:** driving-spec.

> **One-sentence claim:** the project shall let a UTXO coin's configuration
> name a *sighash suffix* (`sighash_suffix`) — a 32-bit value that is appended, in
> little-endian order, to the end of the signature-hash preimage of every
> input the project signs for that coin, directly after the 4-byte sighash
> type — and shall configure it for Bitcoin II (BC2) with the value
> `0x01324342`, so that a BC2 transaction this project signs is accepted
> by the BC2 network.

## 61.0 Executive Summary

Since block 57,750 the BC2 network no longer accepts a transaction whose
signatures were made over the ordinary Bitcoin digest. Its nodes compute
the digest with one extra field, a 4-byte suffix after the sighash type, and a
signature made without that field fails script verification with the
message "Signature must be zero for failed CHECK(MULTI)SIG operation".

**What was observed (2026-11-05, SwapLab, public-network data).** A
KDF Reloaded node and a Komodo KDF 2.6.0-beta node each built a plain
withdrawal from the same native-segwit BC2 output and signed it. Both
signatures are correct ordinary BIP-143 signatures (recomputed
independently), and the BC2 network rejected both. A transaction the
network *did* accept after the activation block — the funding transaction
of that output — verifies only when the suffix is appended. Sampling the
public chain, every segwit spend before block 57,750 verifies under the
ordinary digest and every sampled spend from block 57,756 on verifies only
with the suffix; the boundary is the activation height of R61.2.2.

**What it is not.** It is not the Bitcoin-Cash fork-id of
[Chapter 58](58-utxo-signature-version-and-radiant-sighash.md) R58.2.3. That
mechanism sets the `0x40` bit in the sighash type and shifts a fork
identifier into its upper bits, so the sighash *type* changes. BC2 leaves
the sighash type at `SIGHASH_ALL` (`0x01`, and the byte that ends the script
signature stays `0x01`) and adds a separate 4-byte field after it. The two
mechanisms do not substitute for each other, so this chapter adds a
separate, mechanism-named configuration key (R61.3.1) and leaves
`signature_version`, `fork_id` and the Radiant dialect untouched.

**Why a separate key and not `fork_id` (decision CV, measured 2026-11-06).**
A coin entry that carries a key an older build does not know is accepted by
it; a `fork_id` of BC2's size is not harmless. On the GLEEC reference
development build `a12695e` (prebuilt binary
`https://devbuilds.gleec.com/dev/kdf_a12695e-linux-x86-64.zip`, sha256
`4793ee719eb050dd25f7e64c22497fc3ba43e0a17dc375ce9acee7ccab696b2f`), on the
Komodo KDF `2.6.0-beta` and on this project's `dev`, all three of these
activated both BC2 entries and answered the balance call: an entry with
unknown extra keys, an entry with `fork_id "0x01324342"`, and the unchanged
entry. But a withdrawal built only (never broadcast) on the two reference
builds with that `fork_id` ended its signature with the byte `0x43` instead of
`0x01` (the configured value is folded into the sighash-type byte), and an
entry with the 64-bit value `"0x0132434200000000"` made both reference builds
panic in the coin-configuration parse (the call aborts without a reply and the
coin does not activate). A new key is therefore the only form that leaves older
builds exactly where they are today. The mechanism is not specific to one coin:
the key names what happens to the digest, not the network.

**Standards.** No BIP defines an appended sighash word. The ordinary digests
are BIP-143 (segwit version 0) and the legacy algorithm; the existing
fork-identifier rule is the Bitcoin-Cash / Bitcoin-Gold `fork_id` of
[Chapter 58](58-utxo-signature-version-and-radiant-sighash.md); the appended
word is dictated by BC2's own consensus rules (§61.10). This chapter does not
claim conformance to a standard for the suffix itself.

**Extension, not parity.** No reference build of the Komodo DeFi Framework
signs this field, so none of them can spend BC2 on the live network. The
change is therefore a Reloaded extension driven by the BC2 network, not a
reference-parity item. Its source of truth is the BC2 network itself (R61.2,
§61.10); no forbidden-class input was used (§61.11).

## 61.1 Subsystem Shape

The surfaces are the two of [Chapter 58](58-utxo-signature-version-and-radiant-sighash.md)
§58.1, with one addition:

1. **Configuration.** The coin-activation configuration builder resolves the
   coin's signature version and fork identifier once, at activation, and
   stores them on the coin's resolved configuration. This chapter adds the
   resolved sighash suffix (and its optional activation height) to
   the same place. Nothing re-reads the configuration later.
2. **Signing.** The transaction-signing substrate takes the signature
   version and a sighash-type integer per input and produces the digest to
   sign. This chapter adds the resolved suffix as a further input of that
   one routine, so that every site that signs a UTXO input obtains the same
   digest rule.

A wrong or missing suffix is therefore uniformly fatal for the coin rather
than intermittently so: every withdrawal, HTLC payment, HTLC spend, HTLC
refund, DEX-fee transaction and raw-transaction signing call of the coin
produces a transaction the network rejects at broadcast. That uniformity is
what the observation of §61.0 showed.

## 61.2 Bound network rule (dictated by BC2)

**R61.2.1** *The digest.* For a coin whose resolved sighash suffix
`D` is non-zero, the digest signed for an input is the ordinary digest of the
input's signature version with one change: the 4 little-endian bytes of `D`
are appended to the preimage immediately after the 4-byte little-endian
sighash type, and before the final hashing step. The final hashing step is
unchanged (double SHA-256). For the segwit version-0 (BIP-143) preimage this
makes the preimage

> `nVersion ‖ hashPrevouts ‖ hashSequence ‖ outpoint ‖ scriptCode ‖ amount ‖
> nSequence ‖ hashOutputs ‖ nLockTime ‖ sighashType(4, LE) ‖ D(4, LE)`

and for the legacy (`"base"`) preimage the serialized trimmed transaction
followed by `sighashType(4, LE) ‖ D(4, LE)`. When `D` is zero nothing is
appended and every digest is the ordinary one.

**R61.2.2** *Activation.* The rule applies to a transaction when the height
of the block that validates it is at least the network's activation height.
For the BC2 main network the activation height is **57,750** and `D` is
**`0x01324342`** (the bytes `42 43 32 01` in preimage order: the ASCII
letters "BC2" followed by the suffix version `1`). A transaction waiting in
the memory pool is checked as if it were in the next block, so the height of
the *next* block governs a transaction a node is about to broadcast. Before
the activation height `D` is treated as zero.

**R61.2.3** *The out-of-range single-output case.* The legacy digest for a
`SIGHASH_SINGLE` input whose index has no output is, in the ordinary rule,
the constant 1 (the historical Bitcoin quirk). When `D` is non-zero the BC2
network does not return that constant: it returns the double SHA-256 of the
32-byte value 1 (as the 32-byte little-endian encoding of the integer 1)
followed by the 4-byte little-endian sighash type and the 4-byte
little-endian `D`. The project signs `SIGHASH_SINGLE` in one place, the
taker's pre-signature of [Chapter 58](58-utxo-signature-version-and-radiant-sighash.md)
R58.2.5's exception row. Whatever that site's inputs and outputs are, a
conforming digest routine MUST NOT answer the out-of-range case with the bare
constant when `D` is non-zero.

**R61.2.4** *The signed sighash byte.* The sighash type that is signed and
the byte that ends the script signature are unchanged: `0x01` for
`SIGHASH_ALL`, `0x03` for the single-output exception. The suffix is only
part of the digest; it appears nowhere in the signature or the script.

**R61.2.5** *Taproot.* The BC2 digest for Taproot (Schnorr) spends also
carries the suffix. The project signs no Taproot input, so this chapter binds
nothing for it (D3).

**R61.2.6** *Other networks.* The BC2 test network carries a `D` of the same
value with its own activation height. This chapter does not pin a test-network
height; a test-network coin entry sets its own `sighash_suffix_height`
(R61.3.2).

## 61.3 Configuration contract

**R61.3.1** *`sighash_suffix`.* A coin entry MAY carry the
top-level string member `sighash_suffix`, a hexadecimal integer
with a `0x` prefix that fits in 32 bits, the same shape as `fork_id`
(R58.2.5). When it is absent, or JSON `null` (as for `fork_id`), the resolved
suffix is zero. A present value
that is not of that shape fails activation of the coin with an error that
names the key and the offending value; there is no silent fallback for this
key, because a wrong suffix signs transactions the network rejects.

**R61.3.2** *`sighash_suffix_height` (optional).* A coin entry MAY carry
the top-level non-negative integer `sighash_suffix_height`. When it is
absent the suffix applies at every height (the BC2 main-network entries omit
it: every output a user can spend today lies above the activation height, and
a signing site that does not know the chain height must not guess). When it
is present, a signing site that knows the height of the block that will
validate the transaction (the node's current tip height plus one) applies the
suffix only when that height is at least the value; a signing site that does
not know the height applies the suffix. A present value that is not a
non-negative integer fails activation like R61.3.1. Whether the first
implementation honors this key or only parses and validates it is decision D1.

**R61.3.3** *Independence.* The two keys are orthogonal to
`signature_version` (R58.2.1), `fork_id` (R58.2.5) and the per-coin final
hashing step (R58.2.4). The suffix is appended in each of `"base"`,
`"witness_v0"` and `"fork_id"`, in the same place and the same way. The
combination of a non-zero suffix with `"fork_id_rxd"` is not defined by any
network and fails activation with an error naming both keys. The same holds
for a coin whose configuration selects the Zcash-family overwintered digest,
which this chapter does not bind: a non-zero suffix on such a coin fails
activation with an error naming the key, instead of being ignored.

**R61.3.4** *The BC2 entries.* The shared coins file carries the two BC2
entries with the one added member:

- `BC2` (P2PKH addresses): add `"sighash_suffix": "0x01324342"`.
- `BC2-segwit` (native segwit addresses): add `"sighash_suffix":
  "0x01324342"`.

Nothing else in the two entries changes (R61.5.3 lists what was checked).
A node whose coins file lacks the member signs BC2 without the suffix and the
network rejects the transactions (§61.0); R61.4.5 asks for an activation-time
warning for exactly this case.

## 61.4 Signing sites

**R61.4.1** *All input digests.* The resolved suffix applies to every digest
the project computes in order to sign a UTXO input of the coin: ordinary
spends and withdrawals (P2PKH, P2WPKH, and P2PK inputs spent through the
legacy path), the DEX-fee transaction, the HTLC payment, the HTLC spend and
the HTLC refund (including inputs that spend a P2SH output through its
redeem script), the single-output pre-signature of R61.2.3, and the
raw-transaction signing RPC. There is no per-site override.

**R61.4.2** *One routine.* The suffix shall enter the digest through the one
routine that computes the digest of an input (the substrate of §61.1), so
that a site added later cannot omit it by accident. A site that computes a
digest by another route is a defect.

**R61.4.3** *Message signing.* Signing of arbitrary messages (the "signed
message" prefix, R58.1) is not a transaction digest and is unchanged.

**R61.4.4** *Hardware wallets.* A hardware wallet signs the digest its own
firmware defines and cannot be given a suffix. For a coin whose resolved
suffix is non-zero, a signing request routed to a hardware wallet MUST fail
before anything is sent to the device, with an error that names the
unsupported suffix, instead of producing a transaction the network rejects.

**R61.4.5** *Activation warning.* When a coin's ticker or configuration
identifies BC2 (the ticker `BC2` or `BC2-segwit`) and the resolved suffix is
zero, activation SHOULD log one warning that transactions of the coin will be
rejected by the network from block 57,750 on. Detection by ticker is
acceptable here because the point is a visible message, not a rule.

**R61.4.6** *Verification.* Any code that recomputes an input digest in order
to *verify* a signature MUST apply the same suffix. The implementer states in
the as-built record whether any such path exists; this chapter does not
require one.

## 61.5 Interoperability, reference behavior and compatibility

**R61.5.1** *The reference cannot sign BC2.* Neither `v2.6.0-beta` nor
`v3.0.0-beta` of the reference appends the field, so a reference node that
must sign a BC2 transaction — a withdrawal, or the BC2-side spend of a swap —
produces a transaction the network rejects. The consequence for a swap
between a Reloaded node and a reference node is: if the BC2 side must be
spent by the reference node, the swap cannot complete on the live network;
if the BC2 side must be spent by the Reloaded node, only the Reloaded node's
signing matters. The refusal at order matching for a counterparty that
cannot sign is bound in R61.5A (D2).

**R61.5.2** *Documentation.* Because the change makes Reloaded able to do
what the reference cannot, the user-facing documents record it as a Reloaded
extension: `RELOADED_VS_GLEEC.md`, `docs/GLEEC_COMPATIBILITY.md` (a row that
says no switch exists and the reference builds cannot spend BC2 after the
activation block), and the Known limitations of `CHANGELOG.md`. The
implementing change carries them.

**R61.5.3** *What else the BC2 entries do not need.* The BC2 chain keeps the
ordinary Bitcoin address versions (`0`, `5`, WIF `128`), bech32 prefix `bc`,
the ordinary transaction version and the ordinary segwit rules; no
`signature_version` change, no `fork_id` and no transaction-version override
is needed for signing. The shipped entries therefore stay as they are apart
from R61.3.4. (The network's other consensus rules that began at the same
height are in §61.6; none of them changes a signature.)

## 61.5A The counterparty requirement (Reloaded-original)

*Reloaded-original; no forbidden-class input. It relies only on the reference
facts of Chapter 59 R59.4.7 item 2 (passed the KDF Dirty Gate), as
[Chapter 59](59-legacy-ordermatch-negotiation.md) §59.4A does. Decided by the
maintainer on 2026-11-07 (the requirement ships together with the signing).*

A swap of a coin with a sighash suffix needs both sides to sign with it. The
order-match layer therefore declines a counterparty that does not say it does,
before any swap starts. The one-line rule for users is: **BC2 swaps require a
counterparty that signs with the suffix.**

**R61.5A.1** *The word.* A UTXO-family coin whose resolved `sighash_suffix`
(R61.3.1) is non-zero shall send, as its protocol information, its first value
unchanged followed by the capability word `{"sighash_suffix": true}`
(Chapter 59 R59.4A.1), in every message that carries protocol information. It
sends the word at all times; the height of R61.3.2 plays no part. A coin whose
resolved suffix is zero sends the bytes it sends without this chapter, byte for
byte (R59.4A.4). The criterion is the resolved suffix, not the ticker, so it
holds for `BC2` and `BC2-segwit` alike and for any future coin that sets the
key. The word is a boolean: it declares that the node signs every spend of the
coin with the coin's configured suffix; it does not carry the value, and a
counterparty configured with another value is not detected (both sides are
expected to read the same coins file).

**R61.5A.2** *The requirement.* A coin whose resolved suffix is non-zero, with
the switch of R61.5A.4 on, shall decline through its acceptance check
(Chapter 59 R59.4A.5) a counterparty's protocol information for that coin
unless the information declares `sighash_suffix` (R59.4A.2). The test is in
addition to the coin's ordinary first-value test, which reads only the first
value (R59.4A.3). It fails closed: absent or empty information, a first value
that cannot be skipped, nothing after the first value, a malformed word, and a
well-formed word without the key (or with the key set to `false`) all decline.
A coin whose resolved suffix is zero judges the information exactly as before.
Only the coin's own leg is judged by the coin: in a BC2/DOGE swap the BC2 check
looks at the BC2 information and the DOGE check at the DOGE information.

**R61.5A.3** *Where it applies.* At the two points of Chapter 59 R59.3.2 and
R59.4.3 and nowhere else: the maker judges the taker's two values in
`TakerRequest` before it reserves, the taker judges the maker's two values in
`MakerReserved` before it connects. A declined request is not reserved and a
declined reservation is not connected; no swap record exists, nothing is paid
and the penalty of R59.5.2 does not apply. A reference taker sees a Reloaded
maker's decline only as a request that times out (R59.4.7 item 4), and a
reference node accepts a value that carries the word (R59.4.7 item 2).

**R61.5A.4** *The switch.* The coin configuration key
`require_suffix_counterparty` (boolean, default `true`) switches R61.5A.2 off
when `false`: the coin then judges the information as before. A value that is
not a boolean fails activation with an error that names the key. The switch is
the only way to admit a counterparty without the word; it does not make a swap
with that counterparty possible (R61.5.1). It is the third arm of the
default-side rule of `docs/COMPAT_SWITCHES.md`, in the form of the shielded
coin's `require_v6_counterparty`.

**R61.5A.5** *Logs.* Besides the order-match layer's lines (R59.4A.6), the coin
shall log one debug line per decline that names the coin and the cause (no
word, malformed word, or a word without the key), and one warning per coin at
most once an hour that gives the number of declines since the previous warning
and names the switch.

**R61.5A.6** *Older nodes.* A node that predates this rule sends no word and is
declined by a node that requires it, in both roles. Such a node cannot sign
with the suffix either (R61.5.1), so the decline is intended; the
compatibility note says so.

**R61.5A.7** *The requirement is temporary.* R61.5A.1 to R61.5A.5 exist because
the other Komodo DeFi Framework implementations cannot sign the suffix
(R61.5.1). They are removed, and the capability word with them, as soon as the
suffix signing is generally supported by those implementations; until then the
switch of R61.5A.4 is the only opt-out. The user documents say so in plain
words.

## 61.6 Other BC2 consensus rules that began at the same height (informative)

Three BC2 rules share the activation height 57,750 and are listed so that an
implementer does not mistake them for part of this change:

- **Difficulty adjustment.** The network's published release describes a new
  difficulty-adjustment algorithm with the same activation height. It concerns
  block headers only. The project validates BC2 headers through the
  chain-variant machinery of
  [Chapter 37](37-utxo-spv-and-block-header-validation.md); whether that
  machinery is affected is open item D4, and this chapter binds nothing.
- **Restrictions on arbitrary data in transactions.** From that height a BC2
  block is invalid if a transaction has more than one `OP_RETURN` output, an
  `OP_RETURN` output script longer than 83 bytes, the opcode `OP_13` inside an
  `OP_RETURN` output (the network parses script operations, so the byte value
  inside a data push does not count), or a bare multisig output; and Taproot
  spends are restricted (no annex, a revealed script of at most 3,600 bytes, no
  inscription envelope). The project's UTXO transaction builders add no bare
  multisig output. They do add an `OP_RETURN` output in some transactions: the
  HTLC payment and the taker funding carry exactly one, with the secret hash
  as a data push, and an SLP token transaction on Bitcoin Cash carries one.
  The BC2 transactions that matter (withdrawal, DEX fee, HTLC payment, spend
  and refund) therefore stay within the limits: at most one such output, short,
  data in a push, no `OP_13` operation. T6 pins that.
- **Nothing for signing.** Neither rule changes a signature digest.

## 61.7 Tests

> *Reference and network dimension.* The `AGENTS.md` §3 matrix for this
> contract: **reference** — `v2.6.0-beta` and `v3.0.0-beta` both lack the
> field, so both are unable to spend BC2 after the activation block, and this
> project does not follow them here (R61.5.1); **network** — netid `8762` =
> netid `6133`: nothing in the resolution or the digest reads the network
> identifier, satisfied by that equivalence.

**T1.** *Mined mainnet vector (the pin).* Take the BC2 mainnet transaction
`51f4c719e24491d951e944bb93d3d9b3dc0755e85b2dc47e856419b218869e2d`
(mined at height 63,161; public chain data). Its input 0 spends the native
segwit output of transaction
`298537c9ffaf1051ec4b768d5ee2ff5dfc4ec3cd493c91c1bd005418781b9adf`, index 0,
whose value is 1,831,443,160 satoshi and whose script is
`0014b453f05463845d0b65b37fdabfeb690dd84f13f7`. The complete signed
transaction is

`02000000000101df9a1b78185400bdc1913c49cdc34efc5dffe25e8d764bec5110afffc93785290000000000fdffffff02d8d08e3100000000160014e304ff42ba9589959243c887be147c5bc47cd74372c99a3b0000000016001499fbb767396212563f9e77128db1b631cb8b0e940247304402201b7dc4092c3ad3eee587954b3c075c1ffe0cc68196b090e892d1b686c60d724902207c5f0aed3a963afd2f354e74b5ad17a78cb927e581534f2d8ad6a20ab1f527c90121037a546b1f10a5f957c25aad9126a0b12126c8fb76e9290e8aaff371e6ae580ed3b7f60000`

Recomputed independently, the segwit digest for input 0 with the script code
`76a914b453f05463845d0b65b37fdabfeb690dd84f13f788ac` is:

| Suffix | Digest (hex, as hashed) |
| --- | --- |
| zero (ordinary BIP-143) | `442acbd928298e2fe85992269a1f13128f8a511004b91014925bb37795ca9259` |
| `0x01324342` | `5184d4521d634bb457f7c03b86a9ecb2bd215195d344c128f7085a39dc4dbe50` |

The signature in the transaction verifies against the **second** digest and
not the first. The test MUST assert both digests and that the signature
verifies under the second only. (The intermediate values: `hashPrevouts`
`e5238151c8879680fc76d63caadd4785eabe53da366965086a4c5d48f38763fa`,
`hashSequence`
`caf35e5224de16efa3ccaf41070f6e7b9432b6f79551e629fca9d1c03b43bc52`,
`hashOutputs`
`126176f9bc9870f0dc0b178f8dfe93137db823ffd22c083ea3547061cd5869a0`.)

**T2.** *Zero suffix is the ordinary digest.* With the resolved suffix zero,
the digest of the same input equals the first row of T1 for every signature
version that shares the BIP-143 preimage, and the digest of a legacy input is
byte-for-byte what the routine produced before this change. A regression test
over the existing fixtures of the signing substrate suffices.

**T3.** *Legacy path vector.* A BC2 mainnet P2PKH spend mined after block
57,750 whose script signature verifies only with the suffix appended to the
legacy preimage. Transaction
`873a39fd0021a63948e669edf8f60dcae1f4f8d6d189d9cfca2eb89790e43a5f`, mined in
block 59,500 (one input, one output, signature hash type `0x01`), input 0
spending output 1 of
`fffe12821632260920155662f75e956ec63cf9207dfa724fbd168d35939e2ab8` (value 50
BC2, script `76a914d7b5db7d8660300f994e346e293bf02db5f1441288ac`). The
complete signed transaction is

`0200000001b82a9e93358d16bd4f72fa7d20f93cc66e955ef762561520092632168212feff010000006a47304402206c0ff4447390e781fb6c7dc672e613c40ba5022b7eb8da879482037cbdb90bc102204cca745ac02220a00cf3155931632016cd83458d1f7a4de73d2b1d5dd717480f0121026322de337226f79b389060c4144eb88df269b578316919202bffbcf88eda2e3afdffffff0144f1052a01000000160014565aefe7f8efe6d0a28b7478804046cfe9b1bfcd6be80000`

The legacy digest, with the previous output script substituted for the signed
input:

| Suffix | Digest (hex, as hashed) |
| --- | --- |
| zero (ordinary legacy) | `3f028ee0539f39083a83e40e497dc58aae31ade92184ceb4dc12ea0ac0d9692c` |
| `0x01324342` | `ebbbb47f628fd0d218a931747d43d8e6725c39a25891ab81841629111595868a` |

The signature verifies against the **second** digest and not the first. The
test MUST assert both digests and that the signature verifies under the second
only. The segwit vector of T1 does not cover the legacy preimage.

**T4.** *Out-of-range single-output digest.* With a non-zero suffix and an
index with no output, the legacy digest equals the double SHA-256 of the
32-byte encoding of 1 followed by the sighash type and the suffix (R61.2.3)
and not the constant 1; with a zero suffix it equals the constant 1.

**T5.** *Configuration resolution.* `sighash_suffix` absent
resolves to zero; `"0x01324342"` resolves to that value; a malformed string,
a value above 32 bits, a non-string (JSON `null` counts as absent) and a
negative height each fail
activation with an error naming the key (R61.3.1, R61.3.2); the combination
with `"fork_id_rxd"` or with an overwintered coin fails activation (R61.3.3); each case asserts the
resolved value or the exact error, not merely success or failure.

**T6.** *Output limits.* The withdrawal, the DEX-fee transaction and the HTLC
payment, spend and refund transactions built for a BC2 entry have at most one
`OP_RETURN` output, whose script is at most 83 bytes and contains no `OP_13`
operation, and no bare multisig output (R61.6).

**T7.** *Hardware wallet.* A coin with a non-zero suffix routed to a
hardware wallet fails before any device message is built (R61.4.4).

**T8.** *Every signing site.* For one coin entry with the suffix set, the
transaction produced by each site of R61.4.1 carries signatures that verify
under the suffix digest and not under the ordinary one. The check can
recompute the digest through the substrate's public routine.

**T9.** *Live acceptance (SwapLab).* On the BC2 main network: a withdrawal from
a native-segwit output and a withdrawal from a P2PKH output are accepted into
the network's memory pool and mined; a swap with BC2 on the Reloaded side
completes. The 2026-11-05 withdrawal (0.05 BC2 to a legacy address) is the
first acceptance case.

**T10.** *The word as written.* For a coin with a non-zero suffix, in each
address-format form (standard, segwit, cashaddress), the protocol information is
the first value followed by the one-entry map `"sighash_suffix"` → `true`, as
fixed bytes; the first value still decodes with the ordinary reader and the
reported address format is unchanged (R59.4A.3). With a zero or absent suffix the
bytes equal those of the same coin without this chapter. The `BC2` and
`BC2-segwit` entries both send the word.

**T11.** *The requirement.* With a non-zero own suffix and the switch on, the
acceptance check accepts information that carries the word and declines absent
information, empty information, a malformed first value, a first value alone, a
malformed word, a word without the key and the key with `false`; with the switch
off it accepts all of them as before. With a zero own suffix it judges every
input as before.

**T12.** *Both roles.* A maker declines a `TakerRequest` whose BC2 information
lacks the word (no reservation) and accepts one that carries it; a taker declines
a `MakerReserved` whose BC2 information lacks the word (no connect) and accepts
one that carries it. The other coin of the pair does not trigger the decline.
The order-match layer's debug and info lines of R59.4A.6 appear.

**T13.** *Switch and logs.* `require_suffix_counterparty` is `true` when absent,
accepts both booleans and fails activation on any other value with an error that
names the key. The coin's debug line is written per decline, and the warning at
most once an hour with the count since the previous one (the clock is injected).

## 61.8 Deferred Work

- **CV** *(decided 2026-11-06)* The key form is a new mechanism-named key,
  `sighash_suffix`, not a widened or reused `fork_id` and not a coin-named key
  (§61.0, R61.3.1).

- **D1** *Honor `sighash_suffix_height`.* Whether the first implementation
  applies the height gate or only parses and validates the key. Both supported
  networks are far above the activation height, so the gate has no effect on
  any transaction signed today; the recommendation is to parse and validate
  now and implement the gate with the first coin that needs it.
- **D2** *Refusal for reference counterparties.* *(Decided 2026-11-07 in favour
  of the refusal.)* An order or swap with a counterparty that does not declare
  suffix signing is declined before it starts: R61.5A.
- **D3** *Taproot digest.* Binding the Schnorr digest (R61.2.5) if the project
  ever signs a Taproot input.
- **D4** *BC2 header validation.* Whether the changed difficulty rule of §61.6
  needs a chain-variant entry in Chapter 37.
- **D5** *Test-network height.* The activation height of the BC2 test network
  (R61.2.6) once a test-network entry is wanted.

## 61.9 Baseline Verifications

- **V1** *Where the digest is computed.* In the working tree of this chapter's
  date, the digest of an input is computed by the signature-hash routine of the
  script-and-signing crate (the routine that takes the input index, the input
  amount, the script, the signature version and the sighash type and selects
  among the four versions), reached from the key-pair signer through its
  digest helper. The signature version and the fork identifier are resolved at
  activation and passed to the key-pair signer at every site. R61.4.2 asks that
  the suffix enter through this same routine.
- **V2** *No suffix support exists.* Neither the routine nor the signers carry
  any sighash-suffix parameter at this date; the sighash type passed in is
  `1 | fork_id`.

## 61.9A As-built Note

*Written with the implementation (2026-11-06). The code was compiled and its tests ran in the lab
windows of 2026-11-07, and the signing was accepted by the BC2 network the same day (T9, first case).*

- **Where the suffix lives.** The resolved coin configuration carries `sighash_suffix` (`u32`, zero
  when absent) and `sighash_suffix_height` (`Option<u64>`, validated and carried, never read by a
  signing site: decision D1). The signing substrate's `TransactionInputSigner` carries
  `sighash_suffix`, and `signature_hash` is the one place that reads it. The signer has no default
  and no longer converts from a bare transaction: `TransactionInputSigner::from_transaction(tx,
  suffix)` replaces that conversion, so every constructor states the suffix and a site added later
  cannot omit it. Every signer built for a coin takes the coin's own suffix: the transaction
  builder's preimage, the HTLC spend, the two version-two swap preimage builders, the raw-signing
  call, and the parse of a counterparty's preimage (which never takes a value from the peer's
  bytes).
- **Digest rules as built.** The four suffix bytes follow the 4-byte sighash type in the legacy
  and the BIP-143 preimage, and in the fork-id form of the latter; nothing is appended when the
  suffix is zero. The Radiant dialect and the overwintered (ZIP-243) digest take none: activation
  refuses a non-zero suffix on a coin that selects either, so the routine's zero for them is
  unreachable from a resolved configuration. The out-of-range `SIGHASH_SINGLE` case follows
  R61.2.3 in the legacy and in the fork-id form.
- **R61.4.4 as built.** The refusal is `refuse_sighash_suffix_for_hardware_wallet` in the signer
  crate. It is called first in the Trezor signer, before the device is contacted in the wallet-funded
  version-two signing path, and before the device connection of the task withdraw, whose error is
  `UnsupportedError`. A unit test calls the function; the Trezor signer itself needs a device and
  has no test.
- **R61.4.6 as built.** Two paths recompute an input digest to verify a signature: the
  validators of the version-two funding-spend and taker-payment-spend preimages. Both call
  `signature_hash` on the signer obtained from the parsed preimage, which carries the receiving
  coin's suffix, so the suffix applies to them. No other path recomputes a digest.
- **Tests as built.** T1, T2, T3 and T4 (including a case that builds the out-of-range preimage from a
  literal byte array) are in `kdf_script` (`suffix_tests`); T5 and the activation warning are in
  the configuration builder's tests; T7 is in the signer crate; T6 and T8 are in `utxo_tests.rs`
  (`sighash_suffix_sites`). The Trezor signer itself needs a device and has no test; the shared
  refusal function is tested.
- **Gate as built (§61.5A).** *Compiled and tested in the lab window of 2026-11-07: format, check,
  clippy, the WebAssembly checks and all tests of this section pass.* The key `sighash_suffix` is the constant `CAPABILITY_SIGHASH_SUFFIX` in
  `coin_protocol_capability.rs`, next to `tx_v6`. The configuration builder reads
  `require_suffix_counterparty` (boolean, default `true`; a non-boolean fails activation with an error
  that names the key) and the resolved configuration carries it. `coin_protocol_info` and
  `is_coin_protocol_supported` in the UTXO common helpers write and judge the word: the word is
  appended when the resolved suffix is non-zero, and the check runs after the first-value test only
  when the suffix is non-zero and the switch is on. These two helpers serve the standard UTXO coin,
  Bitcoin Cash, Qtum and QRC-20 coins and the shielded coin's transparent part; the SLP token coin and
  the non-UTXO families have their own methods and are untouched. The decline log is a new generic
  type in `coin_protocol_capability.rs` (debug line per decline, one warning per hour with the count,
  clock injected); its per-coin state lives in a process-wide map keyed by ticker behind a mutex in the
  helpers, the clock is the existing millisecond helper of the `common` crate, and the shielded coin's
  own decline log is unchanged. Tests: T10, T11 and T13 in the module `sighash_suffix_gate_tests` of
  `utxo_tests.rs`, T12 and the reader half of T10 in `t14_capability_word_tests` of
  `ordermatch_tests.rs` (the order-match tests mock the per-coin check, so they prove the two call sites,
  not the coin check, which T11 proves). The warning of the static map itself is not captured by a test:
  the rate limit is proved on the log type with an injected clock.

## 61.10 External References

- The BC2 node implementation published as the "BitcoinII Core" repository of
  the `Bitcoin-II` organization, release `v31.1.0` (tagged 2026-08-28; MIT
  licensed): the main-network consensus parameters (`nReplayProtectionHeight =
  57750`, `nReplayProtectionForkId = 0x01324342`) and the signature-hash
  routine that appends the suffix after the sighash type for the legacy and the
  segwit version-0 preimage, for the out-of-range single-output case and for
  the Taproot digest. The earlier public repository `BitcoinII-Dev/BitcoinII`
  (release `v0.27.1`, 2025-12-01) predates the rule and does not contain it.
- BIP-143 (segwit version-0 signature digest).
- Public BC2 chain data: the transactions of T1, and the boundary between
  block 57,728 (a segwit spend that verifies under the ordinary digest) and
  block 57,756 (a segwit spend that verifies only with the suffix).

## 61.11 Provenance Footer

- *Inputs:* the BC2 node implementation named in §61.10 (the consensus
  parameters and the digest rule only); BIP-143; public BC2 chain data
  (transactions, block heights); two raw withdrawal transactions captured in
  the project's SwapLab on 2026-11-05 (one signed by KDF Reloaded, one by a
  reference build); the present Reloaded workspace (§61.9).
- *Permitted-input classes used:* R3 (external public specification —
  BIP-143), R4 (wire formats and external interfaces the project must
  inter-operate with — the digest the BC2 network dictates, published by its
  own node implementation), R6 (public-network observation — the mined
  transaction of T1 and the activation boundary), R1/R2 (the project's own
  tree for §61.9).
- *Forbidden corpus:* **not consulted.** This is a Reloaded-original extension
  that the reference cannot perform; no Spec Reader pass and no Dirty Gate
  pass was needed, and no upstream implementation expression, name, comment or
  test was used.
- *Third-party licence note.* The BC2 node repository also carries a
  proprietary source-review licence for its difficulty-adjustment
  implementation. That material was not read, is not used here, and must not be
  read by the implementer; this chapter takes from the repository only the
  MIT-licensed consensus parameters and signature-hash rule that the network
  dictates.
- *Authorship:* written on the clean side of the wall from the inputs above.
