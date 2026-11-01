# Plan: configuration-selected UTXO header families

**Status:** in progress (beta blocker H4a and the fallback H4b landed in PRs #62, #67 and #74; H4 in part in PR #82, its remaining family rules open; H5 open) — **Last reviewed:** 2026-10-29

> **Context:** this plan closes the open work left by the fix for standard headers whose
> version collides with a header family (commit `47ede628c`, CRD ch.37
> §37.5.8). That fix made header reads consume the served bytes exactly and fall
> back to the standard 80-byte layout, which removed the observed failure. It
> did not give the reader what ch.37 §37.5 actually requires: a header family
> chosen per coin from its configuration. Of the four items below, three are
> open and depend on each other; item 3 is now largely answered:
>
> 1. ch.37 §37.9 **D1**: accept `protocol.chain_variant` and carry the family
>    into every header read path.
> 2. ch.37 §37.9 **D2**: identity hashes and stored bytes from the served bytes
>    (including the 80-byte AuxPoW hash scope), and the linkage check.
> 3. What is left of the former open `SPEC` note in ch.37 §37.5.7: only the
>    three chain-defined identity hashes, which R37.5.7's identity-hash
>    divergence alone needs. The served header shapes were observed and the
>    R37.5.5 chain detail pinned by the chain-variant pass of 2026-10-24,
>    cleared in ch.37 §37.10 the same day; nothing left here blocks parity
>    (H0).
> 4. The behaviour change of the fix: a header list with leftover bytes is now
>    rejected, so coins whose headers we cannot parse now get an error from
>    median-time-past where they used to get a wrong value. The AuxPoW coins
>    were in this set while the reader recognised AuxPoW only by two exact
>    version values; recognition now uses the AuxPoW flag bit 8 per R37.5.2 and
>    R37.5.4, so they have left it (2026-10-24, see *Resolved* below). LBRY,
>    PIVX and RUNES remain until D1 parses their families. RIC, XPM and
>    MEWC do **not** leave the set with D1: no value of the reference's
>    closed set lays out their headers, so they wait on ch.37 §37.9 D3
>    (cleared 2026-10-24; revised and re-cleared 2026-10-25). For them
>    the error is not the safer failure this item once took it for: the
>    median-time-past error reaches the **HTLC spend**, which queries the
>    median before it signs (ch.37 §37.6 note), so on an Electrum
>    connection RIC and MEWC swaps fail at the spend where the
>    reference's complete — a **swap-breaking** gap, not a safer failure.
>    XPM is wallet-only (registry `wallet_only: true`) and outside swap
>    scope. **D3 is resolved (2026-10-25, ch.37 §37.5.9):** RIC and MEWC
>    leave the set with steps H4a and H4b below — the Reloaded-only key
>    `header_layout` as a local override, and the switch-governed
>    structural fallback of R37.5.9e that reads the registry entries as
>    they stand; XPM stays in it (wallet-only, no value, no fallback step).

## How the four items connect

The header family is the root. It decides the byte layout (item 1), and the
layout decides where one header ends and the next begins, and so which bytes
make up a header's identity (item 2). The layouts and the identity hashes no
longer wait on unconfirmed chain rules: §37.5.7b establishes that the
reference defines none of them (item 3). Item 4 is mostly a symptom of item 1:
LBRY, PIVX and RUNES headers fail only because the reader lacks their
families, and they parse once those families exist. RIC, XPM and MEWC are a
different case: no value of the reference's set lays out their headers
(Riecoin, Primecoin and Meowcoin layouts, ch.37 §37.5.2), so D1 leaves them
failing and they waited on ch.37 §37.9 D3, which needed a family the
reference does not have and a decision on how a coin selects it. That
decision is made (2026-10-25, ch.37 §37.5.9) and has two legs: a
Reloaded-only key, `protocol.header_layout`, that the reference ignores and
that takes precedence over `chain_variant`, with per-header layout
recognition as its mechanism — step H4a below — as a **local override**
(the key is not expected in the shared registry); and a **structural
fallback** under the standard family, governed by the node-level switch
`header_layout_fallback_families` (default `["BTC"]`), that reads the
registry's `RIC` and `MEWC` entries as they stand, with no key and no
registry change — step H4b below (ch.37 R37.5.9e). Because the failure
reaches the HTLC spend (ch.37 §37.6 note), H4a and H4b are the steps that
restore RIC and MEWC swaps — H4b for the shared registry, H4a for an
operator's pinned local entry; together they are the beta blocker, and they
rank above item 2 (identity hashes), which blocks SPV readiness but no
swap, and above the rest of H4, which blocks no swap either.

So the order is: add the family and thread it (item 1) → the Reloaded-only
layout key and its two layouts (H4a) and the switch-governed structural
fallback (H4b), which together restore RIC and MEWC swaps →
parse the missing families (H4, which closes item 4 for LBRY, PIVX and
RUNES) → switch identity and storage to served bytes (item 2). Item 3 no
longer comes first: what is left
of it is a dependency of item 2's linkage divergence only, not a prerequisite
for any step.

## Current state (verified 2026-09-23)

- **Reader hint.** `serialization::CoinVariant` (kdf_codec) has two values,
  `Standard` and `Qtum`. It derives only `Debug`, so it cannot be copied into
  several reads. *(Superseded by PR #62, 2026-10-25: the type now derives
  `Clone, Copy, PartialEq, Eq` and carries two more values, `Riecoin` and
  `KawpowAuxpowMixed`, the layouts of step H4a. The `chain_variant` families
  of H1 are still absent.)*
- **Where Qtum comes from.** The code picks the Qtum variant by coin type, not
  by configuration. The four `get_current_mtp` implementations pass a constant
  (`qtum.rs`, `qrc20_helpers.rs`: Qtum; `utxo_standard.rs`, `bch.rs`,
  `z_coin.rs`: Standard). Nothing else passes a variant.
- **Header read paths.** All go through
  `BlockHeader::from_served_bytes` / `list_from_served_bytes` (kdf_chain) with
  `CoinVariant::Standard`, except median-time-past. The paths are:
  - `ElectrumClient::retrieve_last_headers`, `get_median_time_past` and
    `get_block_timestamp` (`electrum_rpc_client.rs`);
  - the SPV header fetch and anchor check (`utxo_common_spv.rs`);
  - both stores' decode paths (`utxo_sql_block_header_storage.rs`,
    `utxo_indexedb_block_header_storage.rs`);
  - the WalletConnect genesis lookup (`wc_integration.rs`).
- **Configuration.** `CoinProtocol::UTXO` is a unit variant under
  `#[serde(tag = "type", content = "protocol_data")]`. A `chain_variant` key
  beside `type` is ignored silently. `UtxoCoinConf` has no family field.
- **Public coin registry.** In the public registry (checked against the
  `coins` file, 2026-07 snapshot), `chain_variant` sits beside
  `protocol.type`, with these values:

  | Value | Coins |
  |---|---|
  | `BTC` | 8, including BTC, BCH, NAV and RIC |
  | `LBC` | LBC, LBC-segwit |
  | `PIVX` | PIVX, DOGEC |
  | `PPC` | PPC |
  | `QTUM` | 20 QRC20 tokens |
  | `RVN` | RVN, AIPG, AVN, XNA, EVR |

  Every KawPoW-family coin in the registry already carries `"RVN"`.

  **Superseded counts.** The figures above are the 2026-07 snapshot as
  verified on the date in this section's heading, and stay as recorded.
  Ch.37 §37.5.1 carries a newer snapshot (2026-10-18) whose counts differ;
  read the registry counts there rather than from the table above. The one
  difference that changes this plan's own reasoning: the newer snapshot's
  Ravencoin-family set is `RVN`, `AVN`, `EVR` and `XNA` — it does not
  include the `AIPG` listed above — so the registry coverage H4 relies on
  should be read from §37.5.1.
- **Stores.** Both stores persist `header.raw()`, which is the re-encoded
  header. `get_block_height_by_hash` compares `header.hash()`, the
  double-SHA256 of the re-encoding, so for AuxPoW headers the hash also covers
  the proof.
- **Existing helpers.** `RawBlockHeader` (kdf_chain) already wraps served bytes
  and hashes them. The SPV proof path already carries a `RawBlockHeader`
  alongside the parsed header.

## Resolved

- **AuxPoW recognition (2026-10-24).** The reader gated the whole AuxPoW branch
  on equality with two hardcoded block-version values, so every current
  Syscoin-family header missed the test, was read as a plain 80-byte header,
  and the list was rejected at median-time-past — which both HTLC spend
  directions and both refund directions need. Recognition now tests the AuxPoW
  flag bit 8 (`version & 0x100`) per header, per CRD ch.37 **R37.5.2** and
  **R37.5.4**, and both stale constants are gone. Fixed on branch
  `fix/auxpow-bit8-recognition` (**PR #55**); it is a parity fix, so it needed
  no compatibility switch. Consequences for this plan: the AuxPoW coins have
  left item 4's set (above), and the std-80 fallback is now load-bearing for
  correctness — a BIP9 chain may legitimately signal deployment bit 8 in an
  ordinary 80-byte header, and the fallback is what reads it — so H4 must not
  weaken it. The AuxPoW *identity-hash scope* question is untouched and stays
  open as item 2 / §37.9 D2: AuxPoW coins are still not SPV-ready.

## Goal (what done looks like)

Every header read of a UTXO coin parses with the family its configuration
names (ch.37 R37.5.1, R37.5.1a). Explicit families are exclusive (R37.5.3).
KawPoW is read only for `"RVN"` coins (R37.5.4). LBRY, PIVX, Peercoin and
RICK/MORTY headers parse (R37.5.2). RIC and MEWC headers parse — as the
shared registry serves them, with no key — by the structural fallback of
R37.5.9e under the default switch, and, when an operator pins them locally
with the Reloaded-only key `header_layout`, by that key, which takes
precedence over their `"BTC"` value; either way their median-time-past is
the true one (R37.5.9–R37.5.9e). With `header_layout_fallback_families: []`
the fallback is off and such lists are rejected as today. Identity and storage come from the served
bytes (R37.5.7), and header lists are checked for linkage where the family's
hash allows it (R37.5.6). None of this may change behaviour for a coin whose
configuration carries no `chain_variant` and whose headers already parse today,
apart from the KawPoW withdrawal, which is gated in step H4.

## Design

### H0 — Confirm the chain rules (item 3; dirty side)

Largely discharged. The dispatch happened and its result is gated: ch.37
§37.5.7b answers four of the five items, and every one of the four comes back
negative — no Firo MTP start rule, no KawPoW activation time on any network,
no chain-defined identity hash for the KawPoW, Firo or Verus families, and no
PIVX header-version threshold. The reference keys these families on exact
version values — with one timestamp for Firo's ProgPoW switch — instead of
the structural rules this plan expected. Read §37.5.7b for the detail rather
than restating it here. The consequence for this plan is that none of those
rules blocks parity, so H4 and H5 no longer wait on them.

What remained after that was a two-item residual, and the chain-variant pass
of 2026-10-24 (cleared in ch.37 §37.10) closed most of it:

- residual (1), the served header shapes, is **closed by observation**: the
  registry's Ravencoin, Evrmore and Neurai servers serve 120 bytes per
  post-switch header and the Firo servers 180 bytes per MTP-era header and
  120 per ProgPoW-era header, on both header methods and without a
  checkpoint height (ch.37 R37.5.2a, §37.5.7b);
- of residual (2), the PIVX version boundaries, the Ravencoin switch time
  and the Firo MTP lower bound are closed in §37.5.7b's table (and R37.5.5
  now states the PIVX thresholds). Only the **three chain-defined identity
  hashes** remain — the KawPoW-family, Firo and Verus header hashes — and
  they are a dependency of H5's linkage divergence (§37.9 D2) alone, not of
  parity or of any other step.

The narrowed `SPEC` note of §37.5.7 now carries that single item.

### H1 — A copyable family type

Extend the reader hint rather than add a parallel type, because the
`Transaction` deserializer already consumes it for Qtum. The values are:

| Value | Meaning |
|---|---|
| `Default` | Today's `Standard`: version-signalled families, per R37.5.4 |
| `Btc` | Standard only, exclusive |
| `Ppc` | Standard; version 4 is not Equihash |
| `Qtum` | Qtum proof-of-stake header |
| `Lbc` | LBRY claim-trie header |
| `Rvn` | Standard; the KawPoW trailer at the exact version value `0x30000000`, with no switch time — the chapter's own rule now (R37.5.5), no longer an interim reading |
| `Pivx` | 32-byte field after the nonce for header versions 4–6 and ≥8; none for 7 and ≤3 (R37.5.5) |
| `KomodoAssetChain` | Equihash for every block, genesis included (RICK/MORTY) |

The type becomes `Clone + Copy + PartialEq`. The fallback in
`from_served_bytes` keys on "family is `Default`" instead of "not Qtum".

### H2 — Read the key at activation

**Done (2026-10-25, PR #74).** `UtxoConfBuilder` reads `protocol.chain_variant`
beside `header_layout` (exact, case-sensitive, eight values; unknown or non-string
fails the coin build naming ticker and value, even when `header_layout` is
present) into `UtxoCoinConf::chain_variant`. One layout per coin is resolved by
`HeaderReadPolicy::for_conf`: key, else `chain_variant`'s reader, else Qtum for a
`QTUM`/`QRC20` protocol type, else standard. Not as the text below planned: there
is no `header_family` field and no new `CoinVariant` values; the family name (used
for the `header_layout_fallback_families` membership, an absent key counting as
`"BTC"`) is derived, and "key present" stays observable through the existing
`header_layout` field. `"QTUM"` selects the Qtum reader; `"BTC"`, `"RVN"`,
`"PPC"`, `"RICK"` and `"MORTY"` select the default reader (their exclusivity and
thresholded rules are H4); `"LBC"` and `"PIVX"` had no reader here and refused
every header read as "not implemented"; the readers followed (H4 note below). Open decisions 2, 3 and 5 are
settled by this (reject, store holds the policy, Qtum fallback kept). Consequence of keeping the
fallback: a `QTUM`/`QRC20` coin with no `chain_variant` key is family `"BTC"` for the switch, so a
failed Qtum read of its headers is retried as Riecoin and as the mixed layout under the default
switch. That is harmless: 246-byte Qtum headers never consume exactly as 112, 120 or 80 bytes, and
the ambiguity rule guards the remaining cases.

`UtxoConfBuilder` reads `conf["protocol"]["chain_variant"]` from the raw coin
JSON. This leaves the serde shape of `CoinProtocol` unchanged, so nothing that
serializes it changes — and it matches the reference, which reads the key as a
plain string beside `type` without touching the typed protocol descriptor,
compares it exactly and case-sensitively against the eight spellings, and
fails the coin-configuration step on an unrecognized value (ch.37 §37.5.1,
the contract details under R37.5.1). Mapping:

- absent → `Default`. The reference maps an absent key to the default for
  **every** protocol type; a QTUM- or QRC20-protocol coin whose file omits
  the key is the default family there, not the Qtum family (ch.37 §37.5.1).
  Keeping today's protocol-type `Qtum` fallback for the QTUM/QRC20 protocol
  types would be a Reloaded leniency that no registry entry needs — every
  QRC20 entry carries `"QTUM"` — so whether to keep it is open decision 5;
- a known string → its family;
- an unknown string → activation error.

Store the result as `UtxoCoinConf::header_family`. ZHTLC and BCH build their
conf through the same builder, so check both.

- **Explicit work left by H4a (PR #62):** the builder already reads
  `conf["protocol"]["header_layout"]` (built in PR #62 under the earlier
  name `reloaded_header_layout`; the field `UtxoCoinConf::header_layout:
  Option<CoinVariant>` is renamed with the key on the follow-up) and fails
  the coin build on an unknown value, naming the ticker. H2 reads
  `chain_variant` beside it and resolves the two into the single per-coin
  value H4a requires — the key's layout when present, else the
  `chain_variant` family, else `Default` (R37.5.9a) — so that H3 threads one
  field, not two. The fallback of H4b is **not** a value of that field: it
  is read-time behavior keyed on whether the resolved family is in the
  switch's set and on whether the key was present, so H2 must also keep
  "key present" observable to the reader (for instance as the resolved
  value carrying its origin), or H4b cannot tell a pinned coin from an
  unpinned one of the same family.

### H3 — Carry the family everywhere

**Done (2026-10-25, PR #74), for every path the Electrum backend has.** A cloneable
`HeaderReadPolicy` (layout, optional fallback) is built once per coin and reaches:
`retrieve_last_headers`, the sync loop and reorg walk, `get_block_timestamp`
(policy-taking variant; the old method is the standard-layout wrapper), the SPV
single-header read, the anchor check, the direct fetch of a confirmed transaction's
header, both header stores (each holds the policy; rows are re-read through it,
fallback included), the WalletConnect genesis read and the median
(`get_current_mtp` lost its constant argument). Not threaded, deliberately: the Lightning
platform's `get_block_header` (`ln_platform.rs`), the one unthreaded path. It has no coin
configuration in reach, the Lightning platform is Bitcoin (standard 80-byte headers) and the
Lightning plans are parked, so it stays on the standard read; revisit it only if a Lightning
coin ever needs another layout. The native-daemon and lightwalletd backends parse no headers.

Replace every `CoinVariant::Standard` literal from H1's list with the coin's
`header_family`:

- **Electrum client methods:** take the family as an argument, as
  `get_median_time_past` already does.
- **`get_current_mtp`:** loses its constant argument and reads the conf.
- **Header stores:** need the family to decode what they stored. Either
  `BlockHeaderStorage` holds it (built in `new_from_ctx` from the coin conf),
  or the decode helpers take it per call. Prefer holding it: every store is
  per coin, and per-call passing would put the argument on every trait method.
- **WalletConnect genesis lookup:** reads the coin conf.
- **Explicit work left by H4a (PR #62):** that PR routes the resolved layout
  into `get_median_time_past` only (`header_variant` in
  `utxo_common_helpers.rs`). For a coin carrying `header_layout`,
  the header-sync fetch and anchor check (SPV, `utxo_common_spv.rs`),
  `retrieve_last_headers`, `get_block_timestamp` and both header stores'
  decode paths still read standard headers. H3 threads the resolved value to
  every one of them, so that the key governs each read path as R37.5.9a
  requires and not the median alone.

This step changes no parse result for existing configurations, because every
coin still resolves to `Default` or `Qtum` until H4 — except a coin carrying
`header_layout`, whose sync, SPV, store and single-header reads
switch from a rejected (or std-80) read to the key's layout. Its test is
plumbing coverage: each path is exercised with a non-default family.

### H4 — Family rules (closes item 4 for LBRY, PIVX and RUNES)

**Partly done (2026-10-26, PR #82): the LBRY and PIVX readers.** Triggered by a live
PIVX swap that failed at the HTLC spend, after the taker had paid, with the "not implemented"
error of H2. `kdf_chain` gains dedicated `Lbc` (112 bytes in every header) and `Pivx` (80 bytes,
plus 32 after the nonce for versions 4 to 6 and 8 and above) readers behind the existing
`CoinVariant` and `HeaderReadPolicy` plumbing; the refusal is gone; all seven header read paths
and the WalletConnect genesis read serve both. The identity hash is a double-SHA256 of the
served bytes for every family and every PIVX version, matching the reference (ch.37 R37.5.7);
the chain's own pre-version-4 Quark hash is not used for identity. Verified on real headers
fetched from public Electrum servers for every PIVX era including the three version boundaries,
and on real LBC and Runebase lists (Runebase, a `"QTUM"` coin, already parsed with the Qtum
reader). RUNES needed no new reader. **Still open in H4:** `Btc`/`Ppc` exclusivity, the
RICK/MORTY rule for every version, the Peercoin family's rule, and withdrawing version-only
KawPoW detection under the default family (4a).

Implement R37.5.2–R37.5.5 in the header deserializer:

- `Btc`/`Ppc` exclusivity;
- the `Lbc`, `Pivx` and `KomodoAssetChain` layouts; under `Pivx` the trailing
  32 bytes are read after the nonce **only** for header versions 4–6 and 8
  and above, and not for version 7 or for 3 and below — the thresholded read
  of R37.5.5, which ch.37 §37.9 D1 requires in place of the reference's
  unconditional read (wrong for every version-7 header, observed on mainnet
  from height 2153200 through 2700499);
- KawPoW only under `Rvn`, and there by the exact version value `0x30000000`
  (R37.5.5): the value is shared by chains whose switch rules differ
  (Ravencoin by time, Evrmore from its first block, Avian no longer at all),
  so no time rule is derivable from it. A pre-switch Ravencoin header that
  carries the KawPoW version at 80 bytes falls to the R37.5.6 guard: alone it
  is re-read as std-80, in a list the list is rejected.

Last, withdraw version-only KawPoW detection under `Default`. The registry
survey shows no registry coin depends on it. A hand-written configuration for
a KawPoW chain without the key would stop parsing, so call this out in the
changelog and in `docs/GLEEC_COMPATIBILITY.md` if upstream differs.

Once LBRY, PIVX and RUNES parse, item 4's regression is gone for them. The
strict list check itself stays; it is required by R37.5.6. RIC, XPM and MEWC
stay in the failing set after this step: no family of the set lays out their
headers, so their lists are rejected whole and median-time-past errors until
H4a and H4b land (ch.37 §37.9 D3, decided 2026-10-25; open decision 6). That error is not the better
outcome it was once called here: it reaches our HTLC spend, which queries the
median before it signs, so our spend fails too (ch.37 §37.6 note, §37.9 D3).
The reference's random median is tolerated by *its* spend — the spending
transaction's final sequence makes consensus ignore the lock-time — and not
by its refund, which is mis-timed and delayed by up to an hour per round.
Reloaded is stricter and, until D3, less usable on these two coins. D3 is
decided (2026-10-25, ch.37 §37.5.9): H4a and H4b below implement it, and
because they are the beta blocker they land ahead of this step. H4b also
fixes the shape of this step's `Btc` exclusivity: *standard first;
structural fallback only after a failed exact-consumption read and only for
a family in the switch's set* (ch.37 R37.5.9e, §37.9 D1 step 4b).

R37.5.5 no longer waits on anything (ch.37 §37.9 D1, closing bullet): the
PIVX thresholds are pinned, the KawPoW trailer keeps the version key and the
Firo rule is confirmed, so it is implemented in this step as the bullets
above state. Its PIVX reading is a Reloaded improvement on top of the port;
parity does not need it, and the guard of R37.5.6 applies to every
within-family choice exactly as it does to a default-family one.

### H4a — Reloaded-only header-layout key (ch.37 §37.5.9; item 4 for RIC and MEWC; **beta blocker**)

**Partly done (2026-10-25, PR #62).** Landed: the key, read in
`UtxoConfBuilder` beside `type` into `UtxoCoinConf::header_layout` (as
built, still under the earlier name `reloaded_header_layout`)
(an unknown value fails the coin build naming the ticker; the absent key has
no effect); the Riecoin reader (112 bytes, 64-bit time, a time above the
32-bit range is a parse error) and the mixed KawPoW/merged-mined list reader
(per-header choice by the AuxPoW flag bit 8); both re-encode to the served
bytes; the median-time-past path takes the key's layout; pinned
public-server fixtures for `RIC` and `MEWC`; `CHANGELOG.md` and the
`docs/GLEEC_COMPATIBILITY.md` row (added after all, as an informational
"none -- no switch exists" row). Still owed: threading the layout to header
sync, SPV, both header stores and `get_block_timestamp` — moved into H2 and
H3 above as explicit work, because it is the same threading those steps do
for `chain_variant`; and H5's linkage skip for the Riecoin and native
KawPoW forms (below); and the rename of the key and its field to
`header_layout`, settled 2026-10-25 (decision 6).

Decided 2026-10-25 (open decision 6): ch.37 §37.9 D3 is resolved by a
Reloaded-only coin-configuration key, `protocol.header_layout`,
that the reference ignores (ch.45 R45.3.7) and that takes precedence over
`chain_variant` for the coin it is set on (R37.5.9a) — the local-override
leg; the registry-free leg is H4b. Its two initial values
select the Riecoin layout (`"riecoin"`: 112 bytes, fixed) and the mixed
KawPoW/merged-mined layout (`"kawpow_auxpow_mixed"`: per header, AuxPoW flag
bit 8 clear → 120-byte KawPoW, set → std-80). The rules are
clean-room-original (ch.37 §37.5.9 provenance note) and need no gate. The
key name is settled as `header_layout` (unbranded: the key is a local
override, not expected in the shared registry) and the value spellings
stand; the Coder keeps each in exactly one place. What the Coder builds:

- **Type (H1):** two more values of the family type, one per layout. The
  resolved value is one per-coin field: the key's layout when the key is
  present, else the `chain_variant` family, else `Default` (R37.5.9a). No
  second field, so every H3 call site stays as H3 left it.
- **Config (H2):** read `conf["protocol"]["header_layout"]` beside
  `chain_variant`, as a plain string, exact and case-sensitive; an unknown
  value is the same activation error class as an unknown `chain_variant`;
  absent means no effect. `chain_variant` is still read and validated when
  the key is present (R37.5.9a). The serde shape of `CoinProtocol` is
  unchanged.
- **Reader (kdf_chain):** a Riecoin header read — version, previous hash,
  Merkle root, 64-bit time, bits, 32-byte nonce, 112 bytes; a time above the
  32-bit range is a typed parse error, not a truncation (R37.5.9c). A mixed
  list read that reads each header's version field before consuming the
  header and dispatches to the existing KawPoW (120) or std-80 reader
  (R37.5.9b); the existing exact-consumption guard covers the whole list and
  the `count × 80` std-80 re-read applies as under `Rvn`. In a list a
  flag-set header is exactly 80 bytes; a single flag-set header longer than
  80 bytes takes the existing AuxPoW-proof reader.
- **Re-encoding:** both layouts round-trip to the served bytes exactly, so
  the stores keep full headers (112, 120 or 80 bytes) with or without H5
  (R37.5.9c). Nothing trims a header to 80 bytes.
- **Median-time-past:** the code path is unchanged — the parsed time of each
  header; no byte slicing (R37.5.9c).
- **Linkage (H5):** skip the Riecoin and native KawPoW forms under
  R37.5.7's escape until their chain-defined hashes are specified; the
  80-byte merged-mined form hashes as std-80.
- **Fixtures:** fetch once from the registry's public servers and pin with
  source and height — an eleven-header `RIC` tip list (11 × 112); a `MEWC`
  eleven-header tip window of nine 120-byte and two 80-byte headers in the
  chain's own order, and the 80 + 80 + 120 tip shape (ch.37 §37.5.1); a
  uniform 80-byte `MEWC` window if one is obtainable. Tests: the ch.37
  §37.8 R37.5.9 criteria one to one — parse and median for both coins with
  `"chain_variant": "BTC"` present; key wins over `"BTC"`; unknown value →
  activation error; absent key → the existing rejection; stored bytes equal
  served bytes for a 120-byte and a 112-byte header; a per-header mismatch
  list is rejected.
- **Docs in the same change:** ch.37 §37.5.8's as-built list (the key is
  read) and the closing block of §37.9 D3; `CHANGELOG.md` *Fixed* (the
  entry sits under Fixed: it restores swaps that failed). No
  `docs/COMPAT_SWITCHES.md` entry (R37.5.9d: nothing a counterparty observes
  changes, and no value reproduces the reference's random median); PR #62
  did add an informational `docs/GLEEC_COMPATIBILITY.md` row of the
  "none -- no switch exists" kind; with H4b that row becomes the
  switch's row. The registry maintainer is **not** asked to carry the
  key: the shared entries are served by H4b, and the key is documented
  as a local override for an operator's own coins file.

Dependencies: H1–H3 (the type must be copyable and threaded so that the
key's layout reaches every read path). Not H4: the KawPoW and AuxPoW
parsers H4a reuses are already in the tree, and the LBRY, PIVX and RICK
rules are independent. Being the beta blocker, H4a lands ahead of H4; the
two may share one PR if H4 is ready at the same time.

### H4b — Structural fallback under the standard family, governed by a switch (ch.37 R37.5.9e; item 4 for RIC and MEWC as the registry serves them; **beta blocker**)

**Pending (decided 2026-10-25; the follow-up on PR #62).** This is the leg
that makes the registry's `RIC` and `MEWC` entries swap **with no change to
the shared registry** and no local coins file. Clean-room-original (ch.37
§37.5.9 provenance note), no gate. It is a deliberate divergence from the
reference, so it ships with its switch and both documentation entries under
`docs/COMPAT_SWITCHES.md`. What the Coder builds, from ch.37 R37.5.9e:

- **Switch:** the top-level `MM2.json` key `header_layout_fallback_families`,
  an array of `chain_variant` value strings; absent → `["BTC"]`; `[]` → the
  fallback is off for every coin. Read once at startup beside the other
  node-level switches (`max_eth_tx_type_absent_unrestricted`,
  `coin_conf_unrecognised_enum_falls_back`). A value that is not an array,
  an array with a non-string element, a duplicate, or a string outside the
  eight accepted spellings **refuses the launch** with a clean
  configuration error naming the key and the value, never a panic (ch.45
  R45.7.1 / T45.4) — stricter, deliberately, than the boolean switches'
  "any other value leaves the default".
- **Reader (kdf_chain) and call sites:** on every read path of H3, for a
  coin that carries no `header_layout` key and whose resolved family is in
  the set, when the standard read (including R37.5.6's `count × 80` and
  lone-80-byte std-80 re-reads) fails exact consumption, try the Riecoin
  layout, then the mixed KawPoW/merged-mined layout, in that order, each
  accepted only on exact consumption. If more than one alternative consumes
  a **list** exactly, reject it as ambiguous, naming both layouts. If none
  does, reject as today. Never try an alternative when the standard read
  succeeded; never for a coin with the key (the key's layout governs and
  misses are rejected); never for a family outside the set. Until H2 lands,
  every coin without the key is the `"BTC"` family for this test.
- **Logging:** one `info` line per coin per activation on the first
  fallback parse, naming the coin and the layout chosen, so the operator can
  pin the coin with `header_layout`; no repeat and no higher level.
- **Median, storage, identity:** unchanged from H4a (R37.5.9c): parsed
  timestamps, served bytes stored, linkage skip for the two forms whose
  identity hash is unspecified.
- **Fixtures and tests** (ch.37 §37.8, the R37.5.9e criteria one to one):
  the pinned `RIC` 11 × 112 list and the pinned `MEWC` interleaved window
  and 80 + 80 + 120 tip shape parse **with no key** under the default switch
  and yield the true median; with `[]` they are rejected as before and no
  line is logged; the std-80 eleven-header list at `0x30000000`, a std-80
  list with bit 8 set and a genuine Bitcoin list are never fallback-parsed
  (identical result under `["BTC"]` and `[]`, no line); the `info` line
  fires once per coin per activation; with `header_layout` present a list
  the key's layout does not read is rejected without consulting the
  fallback; a 560-byte five-header list of four native and one merged-mined
  header is rejected as ambiguous under no key and parses under the key;
  each malformed switch value refuses the launch, and `[]`, `["BTC"]` and
  `["BTC", "PPC"]` start the daemon.
- **Why the lengths cannot collide for the median window** (keep as a test
  comment): 11 × 80 = 880, 11 × 112 = 1232, mixed = 880 + 40k for k = 0..11;
  1232 − 880 = 352 is not a multiple of 40. In general 112n = 80n + 40k only
  when 4n = 5k, hence the ambiguity rule for other counts.
- **Docs in the same change:** ch.37 §37.5.8 as-built list and the §37.9 D3
  implementation note (fallback landed); `docs/COMPAT_SWITCHES.md`'s two
  locations — the per-setting Compatibility note (ch.37 R37.5.9e carries
  it) and the `docs/GLEEC_COMPATIBILITY.md` row for
  `header_layout_fallback_families` (GLEEC-compatible selection: `[]`;
  default `["BTC"]`; not acknowledgement-gated), replacing PR #62's
  informational row; `RELOADED_VS_GLEEC.md`; `CHANGELOG.md` *Fixed* (RIC and
  MEWC swap from the shared registry) and *Added* (the switch). The rename
  of the key and its field to `header_layout` rides on the same change.

Dependencies: H4a (the two layout readers and the key). Not H2/H3: before
`chain_variant` is read every coin is the `"BTC"` family, so the fallback is
reachable on the median-time-past path as soon as it exists, and H3's
threading extends it to the other read paths exactly as it extends the key.

### H5 — Served-byte identity and linkage (item 2)

- **Parsing:** returns the served byte range with each header. Keep
  `RawBlockHeader` beside the parsed header, as the SPV proof path already
  does.
- **Stores:** persist the served bytes. Identity is the served bytes'
  double-SHA256, and for AuxPoW it covers only the 80-byte base.
  `get_block_height_by_hash` uses that identity.
- **Linkage check:** in list reads, check each header's previous-block hash
  against the identity of the header before it, for families whose identity
  is a double-SHA256. Skip it for KawPoW, Firo and Verus: no chain-defined
  identity hash is available for them, so R37.5.7's own escape applies
  (ch.37 §37.5.7a). Still owed from H4a (PR #62): skip it likewise for the
  Riecoin and native KawPoW forms the `header_layout` key — or the fallback
  of H4b — selects; the 80-byte merged-mined form hashes as std-80
  (R37.5.9c).
- **Migration:** rows written so far hold re-encoded bytes. Since
  `47ede628c`, those equal the served bytes for every family except AuxPoW,
  where the difference is the hash scope, not the bytes. Recommendation: no
  data migration. On upgrade, clear the store for coins whose family is
  AuxPoW-signalled and let sync rebuild it. Decide this before implementing
  (see open decisions).

## Out of scope

- The R37.6.1a code-quality finding (`enable_spv_proof` without `spv_conf`).
  That is a config-validation decision in activation, and separate.
- The active reorg detector (ch.37 §37.7). H5's linkage check shares its
  predecessor-hash comparison, so reuse it; H5 does not change the
  detector's contract.
- Proof-of-work validation for the non-SHA256d families. Validation stays as
  scoped by ch.37 §37.4.

## Sequencing

| Step | Depends on | Size | PR |
|---|---|---|---|
| H0 spec confirmation | — | done 2026-10-24 | chapter only |
| H1 + H2 type and config | — | small | PR #74 |
| H3 threading | H1, H2 | medium (≈10 call sites, both stores) | PR #74 |
| H4a Reloaded-only layout key (D3) | H3 — not H4: the KawPoW and AuxPoW parsers it reuses are in the tree | medium (key read, two layouts, per-header list read, public-server fixtures) | **PR #62** (key, both readers, median-time-past path) — the remaining threading moved into H2/H3, the linkage skip into H5 |
| H4b structural fallback and its switch (D3, registry-free leg) | H4a — the layout readers; not H2/H3 (every coin is the `"BTC"` family until H2) | small–medium (switch parse at startup, fallback in the list and single-header entry points, info line, COMPAT docs, key rename) | **pending**: the follow-up on PR #62 |
| H4 family rules | H3 | medium | PR #82 (LBRY and PIVX readers); the rest open |
| H5 identity, storage, linkage | H4, H4a, H4b | medium, touches persisted data | open |

H1+H2 can start immediately; H0 gates nothing any more. Each PR updates ch.37
§37.5.8 and §37.9 in the same commit as its code, and closes the
corresponding D paragraph when it lands.

## Risks

- **H4 KawPoW withdrawal:** breaks a hand-written KawPoW configuration that
  lacks the key. Mitigation: registry coverage (verified above), a changelog
  note, and an activation-time warning, rather than a silent misread, when a
  `Default` coin serves a header that parses only as KawPoW.
- **`MEWC` names a family that does not describe its chain (observed
  2026-10-24, ch.37 §37.5.1 and §37.9 D3):** it is configured with
  `"chain_variant": "BTC"`, and its servers serve **interleaved 80- and
  120-byte headers** — merged-mined blocks (AuxPoW flag bit 8 set) stripped
  to 80 bytes, native blocks with the 120-byte KawPoW trailer; a three-header
  list at the tip measured 80 + 80 + 120. Under `"BTC"` the reference reads 80
  bytes per header and returns a **random** median-time-past, not a slightly
  skewed one — measured 2026-10-25 against the registry's own servers over
  eleven-header tip windows (ch.37 §37.5.1): RIC 2 of 11 records read
  correctly, MEWC 4 of 11 with the median about eleven years early, XPM 1 of
  11 with the median about thirty-four years late. The reference's default
  family fails too: about half of the misaligned records carry a random
  bit 8 and start an AuxPoW read that errors, so `"BTC"` is a deliberate,
  still-load-bearing workaround — the one value that turns a failing list
  into a silently wrong median — which the registry maintainer confirmed on
  2026-10-25 (open decision 7). The reference's HTLC spend does not depend
  on the value (final sequence, ch.37 §37.6 note); only its refund
  mis-times. Under `"RVN"` (the rework's own guidance for the ticker)
  neither kind parses, so that value **fails** too. No value of the
  reference's set describes the chain, and a corrected coin file cannot fix
  it. Under this plan's rules the coin fails loudly instead: mixed lists are
  rejected whole, median-time-past errors, and the error reaches the HTLC
  spend (item 4 above). Resolution is ch.37 §37.9 D3 (open decision 6);
  `RIC` and `XPM` are in the same position, and their layouts are now
  confirmed against the chains' public definitions (ch.37 §37.5.2,
  2026-10-25): Riecoin `version (4) + prev (32) + merkle (32) + time (8,
  64-bit) + bits (4) + nonce (32, 256-bit)` = 112 bytes, under `"BTC"`;
  Primecoin std-80 plus a length-prefixed big-number multiplier, 88–91
  bytes, no key. **Resolution (2026-10-25):** H4a and H4b, from ch.37
  §37.5.9 — as the registry serves them, `MEWC` and `RIC` are read by the
  structural fallback of R37.5.9e under the default switch (H4b); pinned
  locally, `MEWC` under `header_layout: "kawpow_auxpow_mixed"` and `RIC`
  under `"riecoin"` (H4a); both keep their registry `"BTC"`; `XPM` gets no
  value and no fallback step (wallet-only).
- **Refund-wait floor vs cap (ch.37 §37.6 code-quality finding, cleared
  2026-10-25):** our UTXO refund-eligibility routine *floors* the wait it
  returns at one hour where the reference *caps* it, so a refund is delayed
  by up to an hour per round beyond the moment it became spendable; no funds
  at risk, nothing signed changes. Own-repo fix in both branches (cap instead
  of floor), no gate needed; owner is ch.51's refund timing, cross-referenced
  from ch.52 (maker refund polling). Separate from this plan's steps —
  recorded here because it fell out of reading the median's consumers for D3.
- **H5 hash-scope change:** changes lookup results for AuxPoW stores. It needs
  the migration decision and a test on both backends.
- **WASM:** H3 and H5 touch the IndexedDB store. Run
  `cargo check --target wasm32-unknown-unknown -p coins` at every step.
- **Swap paths:** median-time-past feeds swap lock-times. H3 must not change
  any existing coin's result. Keep the network MTP tests as a canary (`rvn_mtp`,
  `qtum_mtp` and similar).

## Testing

- **Fixtures:** one real header list per family (standard, AuxPoW, KawPoW,
  Equihash, Verus, Firo MTP and ProgPoW, Qtum, LBRY, PIVX, Peercoin,
  RICK/MORTY genesis). Fetch each once from public servers and pin it with
  its source and height. Parse each through both entry points, round-trip
  it, and check its identity.
- **Collisions:** under `Btc`, headers with versions `0x30000000`,
  `0x20001000`, `0x00010004` and an AuxPoW-flagged version all parse as
  std-80. Under `Default`, a KawPoW header list parses as std-80 only when it
  is 80 bytes per header, and fails otherwise.
- **Config:** unknown `chain_variant` is rejected. Absent key maps to
  `Default` (or, if open decision 5 keeps the fallback, to `Qtum` for the
  QTUM/QRC20 protocol types). Each registry value maps correctly.
- **D3 chains:** a 112-byte Riecoin list under `Btc`, and an interleaved
  80/120-byte Meowcoin list under `Btc` or `Default`, are each rejected whole
  and surface as an error, never as a median (ch.37 §37.8) — **without the
  key and with `header_layout_fallback_families: []`**. With the key (H4a)
  the same lists, `"BTC"` still present, parse at their true boundaries and
  yield the true median; an unknown value fails activation; the absent key
  under `[]` leaves the rejection in place; a 120-byte and a 112-byte
  header's stored bytes equal their served bytes; a per-header mismatch
  list is rejected (ch.37 §37.8, the R37.5.9 criteria). With no key and the
  default switch (H4b) the same lists parse by fallback with one `info`
  line per coin per activation; genuine 80-byte lists are never
  fallback-parsed; a key present means no fallback; the 560-byte ambiguous
  list is rejected; malformed switch values refuse the launch (ch.37 §37.8,
  the R37.5.9e criteria).
- **Stores:** a served-bytes round-trip on SQLite, and on IndexedDB through
  the existing wasm test harness. `get_block_height_by_hash` works for an
  AuxPoW header.
- **Linkage:** a list with a broken previous-block hash is rejected, and a
  correctly linked one passes.

## Open decisions

1. **AuxPoW stores on upgrade:** clear them and re-sync (recommended), or
   migrate the stored rows.
2. **(Decided 2026-10-25: reject.)** **Unknown `chain_variant` values:** reject activation, as ch.37 requires,
   or only warn for one release to protect hand-written configurations.
3. **(Decided 2026-10-25: in the store.)** **Where the store's family lives:** in `BlockHeaderStorage` (recommended)
   or per call.
4. **KawPoW without the key:** keep version-only detection behind an explicit
   compatibility switch for a release, or remove it outright. Removing it is
   simpler and the registry does not need it.
5. **(Decided 2026-10-25: kept.)** **Protocol-type `Qtum` fallback for an absent key:** the reference maps an
   absent key to the default for every protocol type (ch.37 §37.5.1). Keeping
   today's QTUM/QRC20 fallback is a Reloaded leniency that no registry entry
   needs; dropping it is exact parity. If kept, it is a divergence and falls
   under `docs/COMPAT_SWITCHES.md`.
6. **How a D3 chain selects its family** (ch.37 §37.9 D3 — `RIC`, `XPM`,
   `MEWC`) — **decided 2026-10-25 (maintainer): option (b), with (c) as
   its mechanism**, bound in ch.37 §37.5.9 (R37.5.9–R37.5.9e) and
   implemented by H4a and H4b. **Updated 2026-10-25 (maintainer):** the
   key is renamed `protocol.header_layout` — it is a local-override
   feature, not expected in the shared registry, so its name is unbranded —
   and (c) is additionally bound as a **switch-governed structural
   fallback** under the standard family (R37.5.9e, step H4b): the node
   switch `header_layout_fallback_families`, default `["BTC"]`, `[]` for the
   reference's selection. The values `"riecoin"` / `"kawpow_auxpow_mixed"`
   stand. The fallback is what serves the shared registry's `RIC` and `MEWC`
   entries with no registry change; the key serves an operator's pinned
   local entry and a layout the fallback does not know. Option (d) is
   rejected as a wire-visible stop-gap.
   The record below stands as history: the four options the chapter
   records, none chosen there at the time.
   **Precedence constraint** (ch.37 §37.9 D3, 2026-10-25): the registry
   keeps `"BTC"` on `RIC` and `MEWC`, so whatever selects a Reloaded-only
   family must take precedence over `chain_variant` for `"BTC"`-pinned
   entries — otherwise R37.5.3 (exclusive std-80) rejects their lists after
   D1 exactly as the default does today. The options: (a) Reloaded-only
   `chain_variant` values beyond the eight, which the reference rejects at
   activation, so such a coin file is a Reloaded coin file — this forks the
   coins file for three coins; (b) a separate Reloaded-only key beside
   `chain_variant` that the reference ignores, so one registry entry serves
   both, with the precedence rule as a one-line contract; (c) no new
   configuration — structural recognition of the layouts under the default
   family by exact byte consumption, which reintroduces version-free
   inference, cannot read the Meowcoin mixed list without a per-header
   decision, and does nothing for a `"BTC"`-pinned entry unless it may
   override that value, which R37.5.3 forbids — viable only as a fallback
   for `RIC` and `XPM` (fixed or self-delimiting lengths); (d) *(added
   2026-10-25)* spend without a median: the HTLC spend needs no median for
   its validity, so a spend that cannot obtain one falls back to the HTLC
   lock-time alone. That restores the happy path for all three coins
   without a new family, but it changes the lock-time field of a signed,
   broadcast transaction relative to the reference (wire-visible),
   substitutes a value in a funds-moving path for data the call site could
   not obtain, and leaves the refund path as broken as today — a stop-gap
   that would need a `docs/COMPAT_SWITCHES.md` record; not favored.
   **Evidence favors (b)**, with (c) as the fallback for `RIC` and `XPM`
   should the registry decline a Reloaded key. The Riecoin and Primecoin
   layouts in ch.37 §37.5.2 are confirmed against the chains' published
   definitions (2026-10-25), so nothing blocks implementing either once the
   selector is chosen.
7. **Raise `RIC`'s and `MEWC`'s `"BTC"` with the registry maintainer** —
   **answered 2026-10-25.** The maintainer confirmed that the value is
   intentional, that swaps with both coins completed on the reference under
   it, and that `XPM` is wallet-only; ch.37 §37.5.1's re-examination shows
   why the value is still load-bearing there. No registry change to request;
   Reloaded selects its families by other means (decision 6). The chapter no
   longer calls the entries a mis-description.

## Upstream reference pointers for a KDF Spec Reader dispatch

> **Dispatched 2026-10-24.** The Spec Reader took these pointers in the
> chain-variant pass of that date; its results are in ch.37 (the contract
> details and sharpened vintage note of §37.5.1, the re-confirmation under
> R37.5.1a, the §37.5.2 rows and R37.5.2a observation, the rewritten R37.5.5,
> the §37.5.7b table and §37.9 D3) and were cleared by the Dirty Gate the same
> day (ch.37 §37.10, PASS 0.02). The table below stays as the record of what
> was dispatched; the ch.37 footer deliberately does not carry these numbers.

> **Dispatched 2026-10-25.** The `"BTC"`-family re-examination pass took,
> beside the corpus at the release designations `v2.6.0-beta` and
> `v3.0.0-beta` (and `v2.0.0-beta` and the baseline for the refund-wait
> bound): the public issue threads **#2048**, **#2620** and **#2584** (the
> reference's header-parsing failure reports, read for failure mode and
> coins affected only); the pull requests **#2563**, **#2583** and **#2692**
> (the three change-sets ch.37 §37.5.3 describes by kind — the last two are
> also in the table below); and the release tag **`v3.0.0-beta`**. Its
> results are in ch.37 (the §37.5.1 re-examination sub-note, the §37.5.2
> confirmation cells, the §37.5.3 vintage paragraph, the §37.6 *Where the
> median goes* note and code-quality finding, the §37.8 D3 sentence and the
> revised §37.9 D3) and were cleared by the Dirty Gate the same day (ch.37
> §37.10, PASS 0.02). As before, the numbers live here and not in the
> chapter.

> **A clean context must not open any of these.** They are recorded here solely
> to direct a KDF Spec Reader dispatch — the only role permitted to read
> upstream/GLEEC KDF material. The numbers and descriptions below were supplied
> by a community maintainer and transcribed without being fetched, opened,
> previewed or verified.

| Upstream reference | As described to us |
|---|---|
| pull request **#2692** | the large rework of the coin-variant / chain-variant logic. Before it the variants were hardcoded in the code, keyed on tickers; after it they are read from the coins configuration file. |
| pull request **#2572** | PIVX block headers, made while the variants were still hardcoded. |
| pull request **#2563** | KawPoW headers, also from the hardcoded era. |
| pull request **#2583** | further header work. |

**Take #2692 first.** That is the maintainer's recommendation: it affects
multiple coins at once, and his stated minimum goal for this plan is that our
build also reads the variants from the coins file instead of using hardcoded
values — which is exactly H2 and H3 here. The three header-specific pull
requests predate that rework, so anything read from them has to be re-read
against the configuration-driven shape #2692 introduced; order the dispatch
accordingly.

## References

- CRD ch.37 §37.5 (R37.5.1–R37.5.7), R37.5.2a, §37.5.7a, §37.5.7b, §37.5.8,
  §37.9 D1–D3, and the chain-variant pass and `"BTC"`-family
  re-examination pass clearance items of §37.10.
- CRD ch.37 §37.5.9 (R37.5.9–R37.5.9e): the Reloaded-only header-layout
  key `header_layout` and the switch-governed structural fallback
  (`header_layout_fallback_families`) that resolve D3 — clean-room-original,
  no gate (its provenance note and the matching §37.10 item).
- `docs/COMPAT_SWITCHES.md` (the default-side rule) and
  `docs/GLEEC_COMPATIBILITY.md`, for the H4b switch's documentation.
- Commit `47ede628c` (the collision fix) and its tests in
  `mm2src/kdf_chain/src/header.rs`.
- Branch `fix/auxpow-bit8-recognition` (PR #55) and its tests in the same file
  — the AuxPoW recognition fix recorded under *Resolved*.
- Public coin registry `coins` file: `protocol.chain_variant` usage (survey
  above).
- Electrum protocol: `blockchain.block.header`, `blockchain.block.headers`.
