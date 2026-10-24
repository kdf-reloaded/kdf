# Chapter 37 -- UTXO SPV & Block-Header Validation

**Status:** driving-spec (target parity; documents the shipped reloaded SPV
subsystem plus the active chain-reorganization detector now being ported to
upstream/corpus parity -- see §37.7 -- and the configuration-selected
header-family contract of §37.5, which is only partly implemented -- see
§37.5.8 and §37.9).

> **One-sentence claim:** for UTXO-family coins the project shall, when SPV
> proof is configured, maintain a persistent local store of block headers that
> it syncs forward in bounded chunks, validate those headers against the chain's
> proof-of-work and difficulty-retarget rules, parse each chain's header bytes
> according to a configuration-selected chain variant, and use the verified
> header chain to prove that swap-relevant transactions are confirmed -- on the
> native target backed by SQLite and on the WASM target backed by IndexedDB.

> **Treatment:** **T-PORT (mostly T-DOC).** Reloaded already ships most of the
> SPV / block-header subsystem (an `kdf_spv_validation` crate plus per-coin
> block-header storage backends in the UTXO coin module, a multi-family header
> reader, and proof-of-work / retarget validation); that part is documented
> as-built. Two sub-behaviours are reduced relative to upstream/corpus and are
> **target** requirements: the **active** chain-reorganization
> detect-and-resolve routine (§37.7), and the **configuration-selected** header
> family of §37.5 -- the shipped reader picks most families from the block's
> version value and knows only a Qtum variant besides the default (§37.5.8).
> Both are specified behaviourally so they can be implemented and unit-tested.

## 37.0 Executive Summary

SPV (Simplified Payment Verification) lets a UTXO coin confirm transactions
against block-header proof-of-work without trusting a single remote server. The
subsystem has four observable parts:

1. **Activation configuration** -- an optional SPV configuration object supplied
   at coin-activation time (not a core coin field). When absent, the coin runs
   without header verification. On the WASM target the configuration is honoured
   the same way as native.
2. **Header sync loop** -- a per-coin background task that fetches headers from
   the coin's RPC/Electrum backend in bounded chunks, persists them, retries on
   transient failure, and advances a stored tip.
3. **Header storage** -- a persistent store with a total-count query, bounded
   retention (oldest headers beyond a configured limit are pruned), and bulk
   removal up to a height; native uses SQLite, WASM uses IndexedDB.
4. **Header validation** -- proof-of-work target checks, difficulty-retarget
   height computation at the adjustment boundary, and per-chain header byte
   parsing keyed to a configured chain variant.

> **Binding scope (R36).** Requirements bind observable behaviour, the public
> cross-crate/activation interface, and externally *dictated* interop: the
> Bitcoin block-header wire layout and the chain-specific header families of
> §37.5.2 (with their `chain_variant` config values), the 2016-block
> difficulty-retarget interval, and the double-SHA256 proof-of-work rule. Private
> type names, helper decomposition, control flow, and diagnostic wording are
> informative.

## 37.1 Activation configuration (R29 dictated by config schema)

R37.1.1 A UTXO coin's activation request MAY carry an SPV configuration object.
When present, the coin enables header verification; when absent, it does not.
The configuration is consumed at activation and is **not** a persistent field of
the running coin's core state.

R37.1.2 **Dictated config schema (config-compat).** The SPV configuration is
supplied as a single optional object under the coin's `conf` at the key
**`spv_conf`**. Third-party coin-config files populate it, so the exact JSON key
names below are dictated interop and shall be accepted verbatim. The object's
fields are:

| Key | Type | Required | Meaning |
|-----|------|----------|---------|
| `starting_block_header` | object (see R37.1.2a) | required | Trusted anchor: the height/header from which header sync and validation begin. |
| `max_stored_block_headers` | positive integer (non-zero) | optional | Maximum number of headers retained in the store. When the stored set would exceed this, the **oldest** headers are pruned (see §37.3). When omitted, retention is unbounded. |
| `validation_params` | object (see R37.1.2b) | optional | How fetched headers are validated. When omitted, headers are stored **without** proof-of-work / difficulty validation (trusted-RPC mode). |

R37.1.2a The **`starting_block_header`** object (the trusted anchor) has the
dictated fields:

| Key | Type | Meaning |
|-----|------|---------|
| `height` | unsigned integer | Block height of the anchor. |
| `hash` | string (hex) | Block hash of the anchor, given in the usual displayed (big-endian) hex form. |
| `time` | unsigned 32-bit integer | Anchor block timestamp, epoch seconds. |
| `bits` | unsigned 32-bit integer | Anchor block's compact difficulty bits. |

R37.1.2b The optional **`validation_params`** object selects difficulty
validation behaviour with the dictated fields:

| Key | Type | Required | Meaning |
|-----|------|----------|---------|
| `difficulty_check` | bool | required within the object | Whether to validate each header's proof-of-work / difficulty against the chain rule. |
| `constant_difficulty` | bool | required within the object | Whether the chain uses a fixed (non-retargeting) difficulty, so retarget computation is skipped. |
| `difficulty_algorithm` | string enum | optional | Chain difficulty-algorithm / chain-variant selector. Dictated string values: `"Bitcoin Mainnet"` and `"Bitcoin Testnet"`. When omitted, no algorithm-specific retarget rule is applied. |

R37.1.3 The configuration shall be validated at activation:
- When `difficulty_algorithm` is `"Bitcoin Mainnet"`: the
  `starting_block_header.height` shall be an exact multiple of the
  difficulty-retarget interval (2016), i.e. a retarget-boundary height;
  otherwise the configuration is rejected. If `max_stored_block_headers` is set,
  it shall be strictly greater than the retarget interval (2016); otherwise the
  configuration is rejected.
- `"Bitcoin Testnet"` is not currently supported and shall be rejected.
- At sync start the anchor fetched from the coin's RPC shall match the configured
  `starting_block_header` (its `bits`, `hash`, and `time`); a mismatch rejects
  activation.

R37.1.4 SPV verification shall be available on both native and WASM targets.

## 37.2 Header sync loop

R37.2.1 For an SPV-enabled coin the project shall run a background loop that
fetches block headers forward from the stored tip toward the chain's current
height and persists them.

R37.2.2 Each fetch shall be bounded to at most the chain's difficulty-retarget
interval of **2016** headers per request, so that a sync never spans more than
one retarget window in a single fetch.

R37.2.3 The loop shall compute its starting block from stored state and the
configured anchor, retry on transient backend failure, and surface its status
(e.g. progress / temporary error) without aborting the coin. During a task
activation this status is reported through the in-progress values
`SyncingBlockHeaders` and `TemporaryError` bound in
[Chapter 55](55-coin-activation-wire-contract.md) R55.5.12; a permanent
synchronisation failure ends the activation as bound in R55.5.14.

## 37.3 Header storage (native SQLite + WASM IndexedDB)

R37.3.1 The header store presents a single backend-agnostic contract (a public
cross-module storage trait) with these operations and semantics:

- **Initialize / check-initialized** -- create the coin's header collection and
  report whether it already exists.
- **Insert-or-overwrite a batch** -- add a set of headers keyed by height;
  writing a height that already exists **overwrites** it (last-writer-wins per
  height). This is the only write path and is how a divergent height is replaced.
- **Retrieve by height** -- return the stored header for a given height (in both
  decoded and raw-hex forms), or nothing if absent.
- **Total-count / emptiness query** -- report whether the store holds any
  headers (a `COUNT`-based check), used to decide first-time initialization and
  by tests.
- **Highest stored height (tip)** -- return the greatest stored height, or
  nothing when empty.
- **Height-by-hash lookup** -- return the height of a stored header matching a
  given hash, or nothing.
- **Most-recent non-limit-bits header** -- return the most recent stored header
  whose compact difficulty bits differ from a supplied maximum/limit value; used
  by the retarget computation.
- **Bulk removal of an inclusive height range** -- delete every stored header
  whose height lies in the closed interval `[from, to]` (**both endpoints
  inclusive**). This single primitive serves two directions:
  - **Oldest-pruning (retention):** remove the range `[0, bound]` where
    `bound = (tip_after_this_batch − max_stored_block_headers)`, deleting the
    OLDEST headers (heights `<= bound`) so the retained count stays within the
    configured limit. Pruning runs only when `tip_after_this_batch` exceeds the
    limit; otherwise nothing is removed.
  - **Divergent-suffix removal (reorg):** remove the range
    `[fork_height, current_tip]`, deleting every header at and above the fork
    height (heights `>= fork_height`). See §37.7.

R37.3.2 The native backend shall persist headers in SQLite; the WASM backend
shall persist them in IndexedDB. Both backends shall present the same storage
contract above. The IndexedDB backend shall store **real header records** in an
object store indexed by height and shall iterate with a **height-bounded cursor**
(reversed when locating the tip) that remains valid across asynchronous
continuation; it shall not be a no-op stub.

## 37.4 Header validation (R29/R31 dictated by Bitcoin consensus)

R37.4.1 The project shall validate a header's proof-of-work by checking that its
double-SHA256 hash meets the difficulty target encoded in its compact bits.

R37.4.2 At each difficulty-adjustment boundary the project shall compute the
retarget height and validate the next-block difficulty against the chain's
retarget rule.

R37.4.3 A header that fails proof-of-work or difficulty validation shall be
rejected and shall not be used to prove transaction confirmation.

## 37.5 Chain-variant header parsing (R29/R31 externally dictated)

This section binds how the project decides which byte layout ("header
family") a UTXO chain's block header uses, the dictated layout of each
family, and the consistency guards that keep one chain's headers from being
read under another family's layout. It covers every path that turns raw
header bytes into a header: a single header (`blockchain.block.header`, the
SPV starting header of R37.1.3, a re-read from the header store of §37.3) and
a list of consecutive headers (`blockchain.block.headers`, used by the sync
loop of §37.2 and by the Electrum-side median-time-past computation of
R37.6.2).

> **Clearance (chapter 01 T1, R40).** The material the later
> re-confirmation pass added here was authored by the KDF Spec Reader
> under the chapter-01 §1.4A two-team procedure and **passed the KDF
> Dirty Gate** on 2026-10-24 before being relied on (PASS, finding list
> empty), so nothing it added is a candidate awaiting a gate. Its extent,
> method and measure are recorded once, in the *Dirty Gate clearance*
> item of §37.10, and are not restated here; the chapter's earlier
> forbidden-class-sourced material stands on the separate pre-adoption
> record of the same section and was not part of that candidate.

### 37.5.1 Selection authority

R37.5.1 The header family of a UTXO coin shall be selected **per coin, from
the coin's configuration**, not hardcoded per ticker and not inferred from a
block's version value alone. The dictated coin-config key is
**`chain_variant`** inside the coin's `protocol` object (a sibling of the
protocol `type`, i.e. `conf.protocol.chain_variant`). Third-party coin-config
files populate it, so the key and its string values are config-compat interop
and shall be accepted verbatim:

| `chain_variant` value | Family it pins (see §37.5.2) |
|---|---|
| *(key absent)* | Default: standard 80-byte header, plus the version-signalled families of R37.5.4 subject to the guard of R37.5.6. |
| `"BTC"` | Standard 80-byte header only, whatever the version value (R37.5.3). **†** |
| `"PPC"` | Standard 80-byte header (Peercoin-style); a version value of 4 does not select the Equihash-style layout. |
| `"QTUM"` | Qtum proof-of-stake header. |
| `"LBC"` | LBRY claim-trie header. |
| `"RVN"` | Standard header before the chain's KawPoW switch, KawPoW header after it. **†** |
| `"PIVX"` | PIVX header with the trailing Sapling root. |
| `"RICK"`, `"MORTY"` | Komodo asset-chain Equihash-style header for every block, including a genesis block whose version value is 1. |

**† This cell states a requirement of this chapter and not a ported
behaviour.** The distinction matters here and not only two subsections
away: the whole column reads as one list, and a reader who takes every
cell for as-built reference behaviour will under-count what has to be
built. Unmarked cells fall within the scope of the port note in §37.5.8 —
the key, the closed value set, the rejection of an unrecognised value,
and the family each of those values pins. The two marked cells do not:
`"BTC"`'s exclusivity is bound by R37.5.3, and the finding beneath that
rule records that the value is a partial guard rather than the
exclusivity the rule requires; `"RVN"`'s time-switched reading is bound
by R37.5.5, and §37.5.7b records that this family is keyed on a version
value with no time comparison. §37.5.8's closing list is the full
inventory of what remains this project's own, and §37.9 D1 is what
closing the ported part takes.

An unrecognised `chain_variant` string shall reject coin activation with a
configuration error; it shall not silently fall back to the default.

> **Reference vintage of R37.5.1 (confirmed).** The configuration-selected
> family, the accepted-value set in the table above, and the
> reject-on-unrecognised-value rule are all **as-built in the reference**
> from release designation `v2.6.0-beta` onward, unchanged at
> `v3.0.0-beta`. The accepted set is closed at exactly the eight strings
> named above (`BTC`, `QTUM`, `LBC`, `PPC`, `RICK`, `MORTY`, `RVN`,
> `PIVX`); the key-absent default is not expressible as a string. R37.5.1
> is therefore a **port**, not an invention, and the value spellings are
> dictated by the deployed coin files rather than chosen here.
>
> *Deployed values.* The public third-party coins configuration (snapshot
> of 2026-10-18) uses six of the eight: `BTC` on fourteen entries, `QTUM`
> on eleven, `RVN` on four, `PIVX` on two, `LBC` on two and `PPC` on one.
> `RICK` and `MORTY` no longer appear in it — they are the two
> development asset chains the value set was introduced for — so the
> project must still accept them but no shipped coin exercises them.
>
> *Operator check worth making before R37.5.3 is enforced.* `MEWC` is
> configured with `"chain_variant": "BTC"`. If that chain's headers
> actually carry the KawPoW shape of §37.5.2, then under the exclusivity
> of R37.5.3 its header-list reads consume 80 bytes per 120-byte header:
> the reference accepts the result silently as a list of wrongly shaped
> headers (see the finding in §37.5.6) and the project rejects the list
> under R37.5.6. Either outcome breaks that coin's median-time-past, and
> therefore the refund path of R37.6.2, so the coin's own published header
> definition should be checked against its configured value and the coin
> file corrected if they disagree. The same check applies to any further
> KawPoW-family entry added later.

R37.5.1a The configured family shall apply identically to **every** header
read path of the coin: single-header reads, header-list reads, the starting
header check, re-reads of stored headers, and header-list reads made to
compute median-time-past. No path may parse with the default family while
another path of the same coin parses with the configured one.

> **Code-quality finding (informative).** Upstream applies the configured
> family to header-list reads, to the starting-header check and to re-reads
> from the header store, but two single-header paths ignore it and always
> parse with the default family: the fallback that fetches one header from the
> Electrum server when the header store has no entry for a height (used for a
> block's timestamp and for the confirmation proof of R37.6.1a when no
> `spv_conf` is set), and the path that fetches the header for a confirmed
> transaction directly from the server. For any coin that sets
> `chain_variant` (for example a Qtum-, LBRY-, PIVX- or KawPoW-family coin)
> those reads either fail on leftover bytes or produce a wrongly shaped header.
> R37.5.1a closes this: the coin's configured family travels with every read.
>
> *Confirmed at the current vintage.* The finding was re-checked and still
> holds at release designation `v3.0.0-beta`. The split is now precise: the
> configured family **does** travel with header-list reads, with the
> starting-header check, with the median-time-past header list, and with both
> header stores' re-reads; it does **not** travel with the two single-header
> fetches named above, which still parse through the family-agnostic generic
> entry point. That makes the asymmetry sharper rather than broader — those
> two paths are exactly the ones that supply a block's timestamp and a
> confirmation proof, so a configured-family coin can sync and store its
> headers correctly and still mis-read the one header a swap confirmation
> decision rests on. One compensation applies only to those two paths: the
> generic entry point requires the parse to consume the served bytes exactly
> (see §37.5.6), so for a fixed-size family longer than 80 bytes the read
> fails loudly instead of returning a wrongly shaped header.

### 37.5.2 Header families and their dictated layouts

R37.5.2 The project shall parse at least the families below, each with the
wire layout the chain itself dictates. All integers are little-endian; a
"compact size" is Bitcoin's variable-length integer; "std-80" denotes the
standard layout `version (4) + previous block hash (32) + Merkle root (32) +
time (4) + compact difficulty bits (4) + nonce (4)`, fields in that order.

| Family | Wire layout (field: bytes) | Chain-dictated signal that the layout applies | Upstream recognition |
|---|---|---|---|
| Standard (Bitcoin) | std-80 = 80 bytes | Default for Bitcoin-derived chains. | Default; the only layout under `"BTC"`/`"PPC"` except as noted in R37.5.3. |
| AuxPoW / merged mining | std-80, then the AuxPoW proof: parent-chain coinbase transaction (full transaction encoding); parent block hash (32); coinbase Merkle branch (compact-size count, count×32 hashes, 4-byte side mask); chain Merkle branch (same shape); parent block header (std-80) | The block's version value has the AuxPoW flag, bit 8 (`0x100`), set; the chain ID sits in the version's upper 16 bits. | Bit 8 set and `chain_variant` is not `"BTC"`. |
| KawPoW (Ravencoin-style) | `version (4) + previous hash (32) + Merkle root (32) + time (4) + bits (4) + height (4) + 64-bit nonce (8) + mix hash (32)` = 120 bytes; there is no 32-bit nonce field | The chain's own KawPoW activation time (a block time at or after it uses this layout). | Version value exactly `0x30000000` **and** `chain_variant` = `"RVN"`. |
| Equihash-style (Zcash / Komodo; Sapling-root before time) | `version (4) + previous hash (32) + Merkle root (32) + final Sapling root / reserved (32) + time (4) + bits as plain 4-byte integer (4) + nonce (32) + solution (compact-size length + bytes; 1344 bytes for Equihash 200,9)` | Chain family (Zcash-derived headers are always this shape; version value is 4). | Version value 4 and `chain_variant` neither `"BTC"` nor `"PPC"`; or `chain_variant` `"RICK"`/`"MORTY"` for every version value. |
| Verus | Same shape as the Equihash-style row; the solution is the chain's own proof-of-work solution vector | The version field carries the value 4 with the solution-version marker bit 16 (`0x00010000`) set, i.e. `0x00010004`. | Version value `0x00010004`, whatever `chain_variant` says; the marker bit is cleared for the in-memory version and restored on re-encoding. |
| Firo MTP | std-80, then `MTP version (4) + MTP hash value (32) + reserved (32) + reserved (32)` = 180 bytes | Block time from the chain's MTP start until its ProgPoW switch. | Version value exactly `0x20001000` and time before `1635228000` (the chain's ProgPoW switch time), whatever `chain_variant` says. |
| Firo ProgPoW | `version (4) + previous hash (32) + Merkle root (32) + time (4) + bits (4) + height (4) + 64-bit nonce (8) + mix hash (32)` = 120 bytes; no 32-bit nonce | Block time at or after the chain's ProgPoW switch time (`1635228000`). | Version value exactly `0x20001000` and time at or after `1635228000`, whatever `chain_variant` says. |
| Qtum proof-of-stake | std-80, then `state root (32) + UTXO root (32) + stake prevout (32-byte txid + 4-byte index) + block signature (compact-size length + bytes)` | Every Qtum header carries these fields, whatever its version value. | Version value exactly `0x20000000` **and** `chain_variant` = `"QTUM"`. |
| LBRY claim trie | `version (4) + previous hash (32) + Merkle root (32) + claim-trie root (32) + time (4) + bits (4) + nonce (4)` = 112 bytes | Every LBRY header carries the claim-trie root. | Version value exactly `0x20000000` **and** `chain_variant` = `"LBC"`. |
| PIVX (Sapling root after nonce) | std-80, then final Sapling root (32) = 112 bytes | Chain's own header rule: the Sapling root follows the nonce from header version 8 onward; zerocoin-era versions carry a different 32-byte field instead, and the earliest versions carry none. | Always read after the nonce when `chain_variant` = `"PIVX"`, whatever the version value. |
| Peercoin | std-80 (the block signature is outside the header) | Chain family. | `chain_variant` = `"PPC"` only suppresses the Equihash-style reading of version-4 headers. |

R37.5.2a **What an Electrum-protocol server returns.** `blockchain.block.header`
returns one header as hex (or an object with a `header` field when a
checkpoint height `cp_height` is requested); `blockchain.block.headers`
returns an object with `count` (number of headers actually returned, which
may be fewer than requested), `hex` (the headers concatenated in height
order) and `max` (the server's per-request cap), plus `root`/`branch` when
`cp_height` is given. For fixed-size families the served bytes per header are
exactly the family's size above. For AuxPoW chains the Electrum protocol lets
the server strip the AuxPoW proof: from protocol version 1.4.1 onward a
server truncates each header to its 80-byte base **when `cp_height` is
non-zero**, and returns the full header including the AuxPoW proof when
`cp_height` is zero or the negotiated protocol is older (ElectrumX 1.18.0
behaves this way). The project requests headers without a checkpoint, so it
shall expect full AuxPoW headers; but because the served shape is
server-controlled, a reader shall accept an 80-byte header for an
AuxPoW-flagged block when the bytes served are exactly 80 (see R37.5.6).

> **Upstream divergence (informative).** Upstream keys four families to an
> exact version value inside their own configured family, whereas the chain
> uses a structural rule: Qtum and LBRY headers always carry their extra
> fields, KawPoW and Firo switch layout by block time, and PIVX by its own
> version thresholds. So a Qtum or LBRY block that signals any version bit,
> a Ravencoin-style block after the switch whose version is not
> `0x30000000`, and a PIVX header from before its Sapling version would be
> read with the wrong layout even with the correct `chain_variant`. This
> chapter requires the configured family's own rule where it is known
> (R37.5.5). Where a chain's rule is not yet pinned, the upstream version
> keys are an acceptable interim reading.
>
> *Confirmed at the current vintage.* Every entry in the "Upstream
> recognition" column above was re-verified at release designations
> `v2.6.0-beta` and `v3.0.0-beta` and is accurate as written, including
> the PIVX row's unconditional trailing read and the three rows whose
> recognition ignores `chain_variant` entirely (Verus and both Firo
> layouts). No family was added or removed between the two designations.

R37.5.2b **The AuxPoW parent header is std-80.** Inside the AuxPoW proof
of the merged-mining row above, the trailing parent block header shall be
read as a plain 80-byte header, whatever its version value and whatever
family the coin is configured as. The merged-mining specification places
the parent chain's own header there, and a parent header never carries a
nested proof of its own, so no family inference applies to it.

> **Code-quality finding (informative).** Upstream reads the parent header
> through the same family-sensitive path as the outer header, carrying the
> coin's configured family and the whole version-inference set into it. A
> parent header whose version value happens to set the AuxPoW flag bit
> would therefore be read as carrying a nested proof of its own, and a
> parent header whose version value is 4 on a coin configured as neither
> `"BTC"` nor `"PPC"` would be read as an Equihash-style header — in both
> cases consuming far past the end of the real parent header. Neither is
> reachable on the merged-mined chains deployed today, because their
> parent chains' version values set neither pattern; but the version
> values in question belong to the *parent* chain and not to this project
> or to the coin being activated, so neither is a safe implicit
> assumption. R37.5.2b removes the inference. *(Confirmed at release
> designation `v3.0.0-beta`.)*

### 37.5.3 Explicit families are exclusive

R37.5.3 When `chain_variant` pins a family, the version value shall be read
only as that family's own chain rule reads it. It shall never move the
header into another family. In particular, under `"BTC"` (and `"PPC"`) every
header is std-80 **whatever** its version value: the AuxPoW flag bit, a
version of 4, `0x30000000`, `0x20001000` and `0x00010004` all leave the
layout at 80 bytes.

> **Code-quality finding (informative).** Upstream's `"BTC"` value only
> guards against part of this. It suppresses the AuxPoW, Equihash-style and
> KawPoW readings, but the Firo MTP/ProgPoW reading (`0x20001000`, keyed on
> time only) and the Verus reading (`0x00010004`) apply to every coin,
> `"BTC"` included. A Bitcoin-derived chain whose blocks signal only version
> bit 12 (version `0x20001000`) after late October 2021 would have each
> header read as a 120-byte ProgPoW header.
>
> *Confirmed at the current vintage.* Re-verified at release designation
> `v3.0.0-beta`: the AuxPoW, Equihash-style and KawPoW readings are gated
> on the configured family, and the two Firo readings and the Verus
> reading are not gated on it at all. The `"BTC"` value is therefore still
> a partial guard rather than the exclusivity R37.5.3 requires.

### 37.5.4 Default family (key absent) and version-signalled families

R37.5.4 For config compatibility with third-party coin files, the default
family (no `chain_variant`) shall keep recognising the version-signalled
families that upstream recognises for coins without the key: AuxPoW (bit 8),
Equihash-style (version 4), Verus (`0x00010004`) and Firo MTP/ProgPoW
(`0x20001000` with the time switch). Public coin files rely on this for
merged-mined, Zcash-derived, Verus and Firo coins, which carry no
`chain_variant`. The KawPoW family shall **not** be recognised under the
default: it requires `"RVN"`. A standard header whose version value is
`0x30000000` shall therefore parse as std-80 under both the default and
`"BTC"`.

R37.5.4a The version value `0x30000000` is a known collision: it is the
KawPoW family's version value, and it is also exactly what any BIP9 chain
produces when its blocks signal only version bit 28 (`0x20000000` with bit 28, `1<<28`, added).
Bitcoin Core's regtest test deployment uses bit 28, so a regtest chain
signals it while that deployment is started or locked in. Some mainnet
Bitcoin-derived chains also emit this value. Such headers are std-80. No
reader of a non-`"RVN"` coin may read the 40 bytes following the 76-byte
prefix as KawPoW fields. Doing so shifts every following header of a list
by 40 bytes, and the list ends early (an unexpected-end failure).

### 37.5.5 Within-family rule

R37.5.5 Inside a pinned family, the reader shall use that chain's structural
rule where §37.5.2 names one: Qtum and LBRY extra fields always; the
KawPoW/ProgPoW layout by block time against the chain's switch time; PIVX's
trailing field by its version thresholds. A family whose chain rule is keyed
to activation time needs that chain's switch time for the configured network.
When no switch time is known, the upstream version key is the interim
reading (see the divergence note in §37.5.2).

### 37.5.6 Structural-consistency guard against ambiguous version values

R37.5.6 Whenever the layout of a header was chosen from its version value
(not pinned by `chain_variant`), the parse result shall be checked for
structural consistency before it is accepted:

- **Single header:** the parse shall consume the served bytes exactly. If a
  version-signalled layout does not consume them exactly, and the served
  bytes are exactly 80, the header shall be read as std-80. That covers both
  a standard chain whose version value collides with a special family and an
  AuxPoW header whose proof the server stripped. Otherwise the header is
  rejected as malformed.
- **Header list:** exactly `count` headers shall be read from `hex`, and the
  read shall end exactly at the end of `hex`. Running out early and leftover
  trailing bytes both make the whole list malformed; a partial list shall
  never be accepted silently. If the version-signalled parse fails this test
  and `hex` is exactly `count × 80` bytes, the list shall be re-read with
  every header as std-80, and that reading used if it passes. Otherwise the
  list is rejected and no header from it is stored or used.
- **Linkage (where computable):** when the family's block-identity hash is a
  double-SHA256 (R37.5.7), each header in a list after the first shall carry
  as its previous-block hash the identity hash of the header before it. A
  version-signalled parse that breaks this linkage while the std-80 reading
  keeps it shall be discarded in favour of the std-80 reading.

A rejected list shall be surfaced as a transient backend/format error of the
sync loop (R37.2.3) or of the median-time-past query (R37.6.2). It shall not
be stored, and it shall not be turned into a median from a truncated set.

> **Code-quality finding (informative) — corrected and re-confirmed.** An
> earlier revision of this note claimed upstream checks no lengths at all.
> That was too strong, and the corrected statement is more useful because
> it localises exactly where the protection is missing. As re-verified at
> release designations `v2.6.0-beta` and `v3.0.0-beta`:
>
> - **Single header through the generic entry point** — the two paths
>   named in §37.5.1a: the parse **must** consume the served bytes
>   exactly, and leftover bytes are a hard error. So a length check does
>   exist here. It is the one place it exists, and it is also the one
>   place that ignores the configured family, so the two properties never
>   help each other.
> - **Single header through a family-aware entry point** — the
>   starting-header check and both header stores' re-reads: the configured
>   family is honoured and **no** exact-consumption check is applied.
> - **Header list** — the sync loop and the median-time-past query: the
>   served count is prefixed as a compact-size integer and that many
>   headers are read one after another, with **no** check that the read
>   ends exactly at the end of the served bytes. A misparse that consumes
>   too few bytes per header therefore **succeeds silently** and yields
>   that many wrongly shaped headers; only one that consumes too many
>   surfaces, and then as an unexpected end.
> - **Nowhere** is there a retry under the standard layout, and nowhere is
>   there a linkage check between consecutive headers.
>
> The silent-success case is the dangerous one and it is reachable from
> configuration alone: a coin whose headers are longer than 80 bytes but
> whose configured family does not select that length reads 80 bytes per
> header and returns a full list of headers whose time fields were taken
> from the wrong offsets. On an Electrum-connected coin the
> median-time-past query consumes that list; the value feeds the
> refund-eligibility check of an HTLC and the lock-time of the refund
> transaction, so a wrong median can delay or mis-time a refund **without
> any error being reported**. A header-list parse failure, by contrast,
> stops the query loudly. The proposed correct behaviour is unchanged:
> R37.5.3, R37.5.4 and R37.5.6 — explicit families are exclusive, KawPoW
> needs `"RVN"`, and any version-derived layout choice is confirmed by
> exact byte consumption and, where computable, by linkage, with std-80 as
> the fallback. Reloaded already applies exact consumption on every path
> (§37.5.8), which is what turns the silent-success case into a rejected
> list.

R37.5.6a Every length-prefixed list read from header bytes shall be bounded
before anything is allocated for it, and a length above the bound shall be a
parse error, never a panic or an allocation failure. The bounds are: the
Equihash `solution`, 1 MiB; Qtum's `vch_block_sig_dlgt`, 4096 bytes; each
AuxPoW merkle branch, 32 hashes (the width of its side mask). A bound is a
sanity ceiling well above any real value, not a validity rule. A rejection
under this rule takes the same path as any other parse failure, including the
std-80 fallback of R37.5.6. Independently of those bounds, the shared list
reader shall never trust a wire length for preallocation: it reserves at most
4096 items up front and grows as items are actually decoded, so an unbounded
list elsewhere (block transactions, for instance) costs at most the bytes
really present and a hostile length ends at the first missing byte.

> **Provenance (informative).** R37.5.6a is clean-room-original hardening
> written without reference to the upstream implementation, in the manner of
> §22.9C. It follows from the finding above: on a header list, a layout chosen
> from the version can read past its own 80 bytes, so a length field may come
> from bytes the server chose.

### 37.5.7 Identity hash and stored bytes

> **Clearance (chapter 01 T1, R40).** The material the later
> re-confirmation pass added here was authored by the KDF Spec Reader
> under the chapter-01 §1.4A two-team procedure and **passed the KDF
> Dirty Gate** on 2026-10-24 before being relied on (PASS, finding list
> empty), so nothing it added is a candidate awaiting a gate. Its extent,
> method and measure are recorded once, in the *Dirty Gate clearance*
> item of §37.10, and are not restated here; the chapter's earlier
> forbidden-class-sourced material stands on the separate pre-adoption
> record of the same section and was not part of that candidate.

R37.5.7 A header's block-identity hash, and the bytes the header store keeps
(§37.3), shall be derived from the bytes as served. They shall not come from a
re-encoding whose layout is itself chosen from the version value or from a
field's value. Re-encoding a parsed header shall reproduce the served bytes
exactly for every family and for every nonce value. The identity hash is:
std-80, Qtum, LBRY, PIVX and Equihash-style: double-SHA256 of the full
header as the family defines it; AuxPoW: double-SHA256 of the **80-byte base
only**, never including the AuxPoW proof (per the merged-mining
specification); KawPoW, Firo (MTP and ProgPoW) and Verus: the chain's own
identity hash as that chain defines it, not a double-SHA256 of the served bytes, so linkage checks
(R37.5.6, R37.7.2) for those families shall use the chain's identity hash or
be skipped.

> **Code-quality finding (informative).** Upstream re-encodes a header to
> compute its hash and to store it. The re-encoder drops the 32-bit nonce of
> any header whose version value is `0x30000000` and whose nonce is zero. The
> rule was written for KawPoW headers, which have no such field, but it is
> keyed on the version and nonce values, not on the family. A standard
> header with a genuine nonce of zero and a bit-28 version, which is common
> on regtest, where roughly every other nonce meets the minimum difficulty, is
> therefore stored 4 bytes short and hashed wrongly. Its stored copy then
> fails to re-read, and its successor's previous-block hash no longer
> matches. The same re-encoding also hashes an AuxPoW header over its proof
> as well as its 80-byte base, so hash-keyed lookups and parent-hash checks
> on merged-mined chains cannot match the chain's real block hashes. R37.5.7
> removes both problems.
>
> *Confirmed at the current vintage.* Both halves were re-verified at
> release designation `v3.0.0-beta` and both still hold: the nonce is
> omitted from the re-encoding when the header actually carries a
> KawPoW/ProgPoW nonce trailer **or** when its version value equals the
> KawPoW value and its 32-bit nonce is zero — the second condition is keyed
> on values rather than on the family, which is the defect — and the
> identity hash is a double-SHA256 over the whole re-encoding, AuxPoW proof
> included.

#### 37.5.7a Chain-identity hashes: what the reference does, and the divergence R37.5.7 creates

R37.5.7a The reference computes **every** family's block-identity hash the
same way: a double-SHA256 over the re-encoded header. It implements no
chain-defined identity hash for any family, including the three whose
chains define one (KawPoW, both Firo layouts, and Verus). R37.5.7's
requirement that those three use their chains' own identity hashes is
therefore a deliberate **Reloaded divergence** from the reference, and
this rule bounds it:

- The divergence is **local, not on the wire.** A header's identity hash
  is the header store's hash-keyed lookup key and an input to the linkage
  checks of R37.5.6 and R37.7.2. It is never sent to a peer, never part of
  a signed preimage, and never part of an RPC response shape, so two nodes
  computing it differently cannot disagree over the wire.
- It therefore needs **no** `docs/COMPAT_SWITCHES.md` entry, but it does
  need a storage decision: changing the hash changes an existing store's
  keys, so adopting it carries the migration-or-resync decision already
  recorded in §37.9 D2.
- Until a chain's own identity hash is implemented for one of those three
  families, R37.5.7's own escape applies — the linkage checks skip that
  family rather than compare a hash that cannot match.

#### 37.5.7b Resolution of the former open `SPEC` note

The `SPEC` note that stood here listed five chain-rule details to confirm
before R37.5.5 could be implemented for the time-switched and
version-thresholded families. Four of the five are now answered, and the
answers collapse most of the blocker — because what they establish is
that **the reference implements none of these chain rules**, so reaching
parity on R37.5.1–R37.5.4 needs none of them:

| Former open item | Resolution |
| --- | --- |
| Firo MTP start and ProgPoW switch rules | The reference has **no MTP start rule at all**. It keys both Firo layouts on one exact version value together with one switch timestamp, `1635228000`: the MTP layout when the block time is **before** it, the ProgPoW layout when the time is **at or after** it. There is no lower bound on the MTP layout. Confirmed at `v2.6.0-beta` and `v3.0.0-beta`. |
| Ravencoin KawPoW activation time per network | The reference has **no activation time** for this family on any network. The KawPoW layout is keyed on the exact version value plus the configured `"RVN"` family, with no time comparison anywhere. There is therefore no per-network constant to obtain for parity. Confirmed at both designations. |
| KawPoW, Firo and Verus identity hashes | The reference uses **no** chain-defined identity hash for these families (§37.5.7a). None needs to be sourced for parity; they are needed only for R37.5.7's divergence, whose adoption is gated on §37.9 D2 regardless. |
| PIVX header-version thresholds | The reference applies **no version threshold**: for a coin configured as `"PIVX"` it reads one 32-byte field after the nonce for every header, whatever the version value. That is consistent with being byte-correct over the chain's recent history, because the chain's zerocoin-era field and its Sapling-root field occupy the same 32-byte position and differ only in meaning — a single unconditional read is correct for every version carrying **either**, and wrong only for the chain's earliest versions, which carry neither. The exact version boundaries remain unverified and are still needed for R37.5.5. |
| Whether Electrum servers for the KawPoW and Firo families serve the full header shapes | **Still open.** The reference's reader expects the 120-byte KawPoW/ProgPoW shape and the 180-byte Firo MTP shape from server-served bytes, which is indirect evidence that the servers deployed for those coins do serve them. It is not a direct observation and the project has not made one. |

> **Superseded `SPEC` note — retained for the record, not an open item.**
> The note below carried the `SPEC` markers until §37.5.7b answered four
> of its five items. The markers have been removed and the text kept in
> full, so that a reader, and equally a count of open `SPEC` items in
> this chapter, finds exactly one open note — the one immediately after
> this block — rather than two of which one is closed. The superseded
> text, unchanged:
>
> *Chain-rule details cited from public chain definitions and not
> re-verified in this pass: the PIVX header-version thresholds for its
> zerocoin-era field and its Sapling-root field; the Firo MTP start and
> ProgPoW switch rules; the Ravencoin KawPoW activation time per network;
> the KawPoW, Firo and Verus identity hashes; and whether Electrum
> servers for the KawPoW and Firo families serve the full header shapes
> of §37.5.2. Confirm them against each chain's published header
> definition before R37.5.5 is implemented for those families. Superseded
> by §37.5.7b: four of the five are answered there, and the two residual
> items are restated in the note immediately below.*

<<<SPEC Two residual chain-rule items, both narrower than the note above they replace, and neither of them a blocker for parity with the reference (§37.5.7b). (1) Not directly observed: whether the Electrum servers configured for the KawPoW-family and Firo coins serve the full 120-byte and 180-byte header shapes of §37.5.2 rather than an 80-byte truncation. Confirm by querying a configured server for a known post-switch height and measuring the served byte length per header before relying on either family's header sync; the exact-consumption guard of §37.5.8 makes a truncation safe to discover at runtime rather than fatal. (2) Not re-verified against the chains' published definitions, and needed only for the structural rules of R37.5.5 and for the divergence of R37.5.7a: the exact PIVX header-version boundaries at which its trailing 32-byte field appears and changes meaning; the per-network Ravencoin KawPoW activation times; the Firo MTP lower bound; and the three chain-defined identity hashes. SPEC>>>

> **Interop note (R29/R31).** Each family's byte layout is dictated by its
> chain, and the `chain_variant` key and values are dictated by third-party
> coin-config files; neither is this project's choice. §37.5 binds the
> selection contract, the layouts, and the consistency guards. It does not
> bind any particular parser structure.

### 37.5.8 Implementation status

> **Clearance (chapter 01 T1, R40).** The material the later
> re-confirmation pass added here was authored by the KDF Spec Reader
> under the chapter-01 §1.4A two-team procedure and **passed the KDF
> Dirty Gate** on 2026-10-24 before being relied on (PASS, finding list
> empty), so nothing it added is a candidate awaiting a gate. Its extent,
> method and measure are recorded once, in the *Dirty Gate clearance*
> item of §37.10, and are not restated here; the chapter's earlier
> forbidden-class-sourced material stands on the separate pre-adoption
> record of the same section and was not part of that candidate.

The requirement of R37.5.1 is correct and the shipped reader falls short of
it. Selection by configuration is the contract; the version-value selection
described below is a gap to be closed (§37.9 D1), not an alternative
contract. The chain-variant set the reader knows is much smaller than the
set of families it branches on, and that mismatch is the source of the
collision of R37.5.4a.

> **Closing D1 is a port, not a design.** Confirmed at release
> designations `v2.6.0-beta` and `v3.0.0-beta`: the reference implements
> exactly the configuration-selected family of R37.5.1 — the same
> `protocol.chain_variant` key, the same closed set of eight accepted
> strings, the same reject-on-unrecognised-value rule, and the same
> propagation of the coin's family into the header-list reads, the
> starting-header check, the median-time-past read and both header
> stores. Everything §37.9 D1 asks for therefore has a reference
> implementation to be compatible with, and the only parts of §37.5 that
> remain Reloaded's own are the exclusivity of R37.5.3 for the
> non-family-gated layouts, the structural rules of R37.5.5, the linkage
> rule of R37.5.6, the served-bytes and chain-identity rules of R37.5.7
> and §37.5.7a, and the parent-header rule of R37.5.2b.

As built:

- The reader knows two variants, the default and Qtum. The Qtum variant is
  chosen by the coin's protocol type in code, and only for the Qtum and
  QRC20 median-time-past read. The `chain_variant` key of R37.5.1 is not
  read: a coin file that sets it is accepted and the value has no effect.
- Every other header read uses the default variant, for Qtum coins as well:
  single-header Electrum reads, header lists for the sync loop, the anchor
  check of R37.1.3, re-reads from both header stores, and the genesis-hash
  lookup. This falls short of R37.5.1a.
- Under the default variant, the AuxPoW layout (recognised by two specific
  version values only, not by the bit-8 rule), the Equihash-style layout
  (version 4), Verus, Firo MTP and ProgPoW, and KawPoW are all chosen from the
  version value alone. The LBRY, PIVX and Peercoin families and the
  RICK/MORTY genesis rule are not implemented. So R37.5.3 and R37.5.4 are not
  met: KawPoW is still recognised by its version value for every non-Qtum
  coin. Withdrawing that before the configuration key exists would leave
  KawPoW coins unable to read their headers.

Implemented (part of R37.5.6 and part of R37.5.7):

- Every read path listed above parses through one of two entry points of
  the chain-primitives crate, one for a single served header and one for a
  served header list. Both require the parse to consume the served bytes
  exactly. A list must yield exactly `count` headers ending exactly at the
  end of the served bytes; otherwise it is rejected whole. When the layout
  was inferred from the version value (every variant except Qtum) and the
  served bytes are exactly 80 per header, an inferred parse that fails this
  test is replaced by the std-80 reading. This is R37.5.6's single-header and
  header-list rule. Its linkage rule is not implemented (§37.9 D2).
- The re-encoder omits the 32-bit nonce only for a header that actually
  carries a KawPoW or ProgPoW nonce trailer. A std-80 header whose version
  value collides with one of those families therefore re-encodes to its
  served 80 bytes, and hashes correctly, whatever its nonce. This is the
  nonce part of R37.5.7. The header stores still keep re-encoded rather than
  served bytes, and an AuxPoW header's hash still covers its proof (§37.9 D2).
- Effect on R37.5.4a: std-80 headers whose version value is `0x30000000`, on
  their own or mixed with headers of other versions, read as std-80 on every
  read path above. That includes the header list behind median-time-past,
  and therefore the lock-time of a swap spend or refund. The guard cannot
  help a chain whose own headers are longer than 80 bytes and whose version
  collides with a special family. That case needs the configured family of
  §37.9 D1.
- Regression tests in the chain-primitives crate's header module cover:
  - an eleven-header std-80 list at version `0x30000000` with zero nonces;
  - a list that crosses the start and end of bit-28 signalling;
  - a single header with version `0x30000000` and nonce zero, round-tripped
    through its re-encoded form;
  - a 120-byte KawPoW list and single header, to keep the KawPoW path
    covered;
  - rejection of a list with trailing or missing bytes.

## 37.6 Use in swap confirmation & current MTP

R37.6.1 The verified header chain shall back the SPV confirmation path used when
deciding whether a swap-relevant transaction is sufficiently confirmed.
Concretely: a UTXO coin's configuration carries an independent boolean flag,
**`enable_spv_proof`**, under the coin's `conf` (dictated config-compat, like
`spv_conf` of R37.1.2 but a separate switch -- a coin may set either without
the other). When `enable_spv_proof` is true, the coin-layer payment-validation
operation that [chapter 15](15-swap-v2-utxo-path.md) R12, and the legacy and
version-two swap state machines
([chapter 51](51-legacy-v1-swap-state-machine.md),
[chapter 52](52-swap-v2-state-machine.md)) invoke as an external dependency to
validate a maker or taker payment shall, in addition to the ordinary
confirmation-count wait, fetch a Merkle inclusion proof for the swap-relevant
transaction and validate that proof against the header of the block the
transaction is included in, retrying until a deadline; a transaction whose
proof does not validate shall not be accepted as a validated payment. This
proof-of-inclusion check applies only when the coin is connected through the
project's Electrum-family RPC backend; a coin connected through the chain's
native-daemon RPC backend does not perform it. When the payment's own
required-confirmations value is zero, the check is skipped along with the
confirmation wait.

R37.6.1a The block header the proof-of-inclusion check of R37.6.1 validates
against shall be drawn from the persistent header store of §37.3 -- and
therefore already carries the proof-of-work / difficulty-retarget validation
of §37.4 -- whenever the coin's `spv_conf` (R37.1.2) is configured. When
`spv_conf` is not configured for that coin, `enable_spv_proof` may still be
set independently, and the check falls back to a header fetched directly from
the RPC backend for that request, with no proof-of-work/difficulty validation
applied to it and nothing about it persisted.

> **Code-quality finding (informative).** The fallback of R37.6.1a is a real
> reduction in what the proof-of-inclusion check proves, not merely a
> documented option. When `enable_spv_proof` is set without a configured
> `spv_conf`, the header the Merkle proof is checked against is trusted from
> the same RPC backend the check exists to avoid fully trusting, with no
> proof-of-work or difficulty validation and no persisted cross-request
> record of it. A Merkle inclusion proof against an unauthenticated header
> only shows that the transaction is included in *some* block the server
> handed back for that height, not that the block belongs to the coin's
> actual heaviest valid chain -- the exact property §37.0 states this
> subsystem exists to avoid trusting a single remote server for. Requiring
> `spv_conf` whenever `enable_spv_proof` is set (or deriving one flag's
> effective value from the other's presence, rather than treating them as
> fully independent switches) would close the gap. This chapter does not
> resolve the choice; it is a config-validation decision belonging to the
> owning coin-activation path.

R37.6.1b The proof-of-inclusion check of R37.6.1 changes what a payment-
validation call can conclude, not when it is called or how long it waits: the
confirmation-wait deadlines, stage/state transitions, and event vocabularies
bound by chapters 51 and 52 are identical whether or not SPV is configured for
a coin, because both state machines treat payment validation as a single
opaque external step regardless of its internal implementation (chapter 51
§51.2, chapter 52 §52.2). SPV is therefore a trust-minimization layer nested
inside an existing validation step, not a parallel or alternative
confirmation mechanism with its own timing.

R37.6.2 The public `get_current_mtp` RPC shall report a coin's current
median-time-past, computed from the relevant recent headers.

## 37.7 Chain-reorganization handling

R37.7.1 The header store shall tolerate a reorganization by overwriting a stored
height with a newly-fetched header value for that height (last-writer-wins on a
per-height basis), so that a shorter divergent suffix is replaced as the loop
re-fetches.

R37.7.2 **(Target -- active reorg detect-and-resolve.)** In addition to the
passive overwrite of R37.7.1, the sync loop shall run an **active** reorg
detector with the following behaviour:

**(a) Trigger.** During validation of a freshly-fetched batch, the loop compares
each header's recorded parent/previous-block hash against the hash of its stored
(or just-validated) predecessor. A discontinuity -- a header whose parent hash
does not match the predecessor at the height immediately below it -- signals a
fork and yields the height at which the mismatch was observed (the candidate fork
height). Detection of this parent-hash discontinuity is what arms the resolver;
absent it, normal forward sync proceeds.

**(b) Resolve.** On detection the resolver re-fetches a bounded chunk of headers
starting at the candidate fork height from the backend and re-validates it
against the stored header one below the chunk's start. If that chunk now
validates cleanly (parent hashes line up and proof-of-work/difficulty pass), the
divergent suffix already in the store -- the inclusive range from the fork height
up to the current stored tip -- is removed (per the suffix-removal direction of
§37.3.1), and the loop resumes forward sync, which re-fetches and re-stores the
now-correct suffix from the heavier valid chain. If, instead, re-validation
surfaces a parent-hash discontinuity at a still-lower height, the search window
moves strictly further back (by up to one chunk) and the attempt repeats from
there.

**(c) Walk-back bound.** The backward search is bounded **below by the configured
trusted anchor** (`starting_block_header`): each step clamps the next fetch range
so it never goes below the anchor height, and the window moves back by at least
one position per unresolved step. It does **not** rely on stopping at the
retarget interval. If the search reaches the anchor without finding a consistent
join -- i.e. the divergence extends down to the preconfigured starting header --
the resolver reports a **bad starting-header chain** condition (the trusted
anchor itself must be reconfigured) rather than looping further.

**(d) Convergence.** Because every unresolved step moves the search window
strictly backward toward a fixed lower bound (the anchor) and any header failing
proof-of-work/difficulty is rejected, the resolver terminates in a bounded number
of steps. Each successful resolution deletes the divergent suffix and re-syncs
the valid one, so the store converges to the heaviest valid chain consistent with
the trusted anchor.

> **Upstream divergence (informative).** Reloaded historically shipped only the
> passive per-height overwrite of R37.7.1; the active detector of R37.7.2 is the
> behaviour being ported here to reach upstream/corpus parity. Both converge to
> the same verified chain once sync catches up, but the active detector converges
> faster and rejects an invalid fork earlier. This is a behavioural (not
> wire-format) port. The implementation shall be expressed independently; this
> section binds the contract, not any particular branch structure.

**Acceptance (binds the §37.7 unit test).** A unit test shall seed a stored
header chain, then feed a **divergent, heavier valid suffix** whose first header's
parent hash does not match the stored predecessor at the fork height, and assert
that the detector (i) identifies the fork height, (ii) removes the stale suffix
from the fork height through the old tip, and (iii) leaves the store converged on
the heavier valid chain. A companion test feeding a divergence that extends to
the trusted anchor shall assert the bad-starting-header-chain condition is
reported rather than an unbounded walk-back.

## 37.8 Acceptance criteria

- An SPV-enabled UTXO coin activates only with a valid SPV configuration
  (R37.1.3) and rejects an invalid starting header or an under-sized
  max-stored-headers limit.
- The sync loop persists headers in <= 2016-header chunks and advances the tip
  (R37.2.2).
- Header storage round-trips on both SQLite (native) and IndexedDB (WASM),
  including count, inclusive-range removal, and oldest-pruning (R37.3).
- Proof-of-work and retarget validation reject a header with invalid bits
  (R37.4).
- The active reorg detector converges to the heavier valid chain on a divergent
  suffix and reports a bad-starting-header-chain condition when the divergence
  reaches the trusted anchor (R37.7.2).
- Headers for at least one coin from each family of §37.5.2 (standard,
  AuxPoW, KawPoW, Equihash-style, Verus, Firo MTP and ProgPoW, Qtum, LBRY,
  PIVX, Peercoin) parse and re-encode byte-identically under their configured
  `chain_variant`, on the single-header and the header-list paths alike
  (R37.5.1a, R37.5.2, R37.5.7). The following ambiguity cases hold (R37.5.3,
  R37.5.4a, R37.5.6, R37.5.7):
  - A `blockchain.block.headers` response of several standard 80-byte headers
    whose version value is `0x30000000`, as produced by a chain signalling
    only version bit 28 (for example a Bitcoin Core regtest chain while its
    test deployment is started or locked in), parses as `count` std-80 headers
    under both the default and `"BTC"`. The median-time-past computed from it
    equals the median of their timestamps.
  - A standard header with version `0x30000000` and nonce zero hashes to its
    true block hash and round-trips through the header store unchanged.
  - Under `"BTC"`, a standard header with version `0x20001000` and a
    timestamp after the Firo switch time parses as std-80.
  - A header list whose bytes do not end exactly at the last of `count`
    headers is rejected whole: neither stored nor used for median-time-past.
  - An 80-byte header served for an AuxPoW-flagged block is accepted as
    std-80.
- `get_current_mtp` returns a plausible median-time-past for an SPV-enabled coin
  (R37.6.2).
- A swap-relevant payment on an Electrum-connected coin with `enable_spv_proof`
  set is validated only when its Merkle inclusion proof checks against the
  applicable block header, and that header carries the §37.4 proof-of-work
  validation whenever `spv_conf` is also configured for that coin (R37.6.1,
  R37.6.1a). A coin's confirmation-wait deadlines and swap-stage transitions
  are unchanged by whether SPV is configured (R37.6.1b).

## 37.9 Deferred Work

> **Clearance (chapter 01 T1, R40).** The material the later
> re-confirmation pass added here was authored by the KDF Spec Reader
> under the chapter-01 §1.4A two-team procedure and **passed the KDF
> Dirty Gate** on 2026-10-24 before being relied on (PASS, finding list
> empty), so nothing it added is a candidate awaiting a gate. Its extent,
> method and measure are recorded once, in the *Dirty Gate clearance*
> item of §37.10, and are not restated here; the chapter's earlier
> forbidden-class-sourced material stands on the separate pre-adoption
> record of the same section and was not part of that candidate.

**D1.** *Configuration-selected header family.* R37.5.1, R37.5.1a, R37.5.3,
R37.5.4 and R37.5.5 are not implemented (§37.5.8). Closing this takes four
steps:

1. Accept `protocol.chain_variant` at coin activation with the values of
   R37.5.1, and reject unknown values.
2. Extend the reader's variant set to those families, adding the LBRY,
   PIVX, Peercoin and RICK/MORTY rules it lacks.
3. Carry each coin's configured family into every header read path of
   §37.5.8, on native and WASM alike. That includes both header stores,
   which need the family to re-read what they stored.
4. Two halves, which the rest of §37.5 keeps apart: **(4a)** withdraw
   version-only KawPoW recognition under the default family (R37.5.4);
   **(4b)** make `"BTC"` exclusive (R37.5.3).

It is deferred because it changes the coin-activation configuration surface
and every header read path at once, and it touches both storage backends.
The length guard of §37.5.8 removes the observed failure without any of
that.

> **The step-4a blocker is smaller than it looks.** This note is about
> step **4a** and not step 4b: the coordination worry it answers was
> about KawPoW coins configured without the key, which is what 4a
> withdraws, and 4a is also the half within the scope of §37.5.8's port
> note. Step 4b is the exclusivity of R37.5.3, which §37.5.8's closing
> list records as remaining this project's own and which concerns coins
> configured **with** `"BTC"`, so neither the coin-file census below nor
> the release-note point reaches it. This item previously
> said the last step needs coordinated coin-file updates because it
> changes behaviour for KawPoW coins configured without the key. In the
> public third-party coins configuration (snapshot of 2026-10-18) **every**
> Ravencoin-family entry already carries `"chain_variant": "RVN"`
> (`RVN`, `AVN`, `EVR`, `XNA`), so withdrawing version-only KawPoW
> recognition breaks none of them as shipped. What remains is (a) the
> operator check of §37.5.1 for an entry that may be configured with the
> wrong family, and (b) the possibility of a *locally* supplied coin file
> that sets no `chain_variant` for a KawPoW chain — which is a release-note
> matter, not a coordination matter. Step 4a should therefore be done in
> the same change as steps 1–3 rather than held back.

Until this is closed:

- A standard chain whose blocks carry the colliding version value works
  through that guard.
- A Qtum coin's single-header and stored-header reads use the default family.
- The R37.5.5 families wait on residual item (2) of the `SPEC` note in
  §37.5.7, which is now narrower: §37.5.7b establishes that reaching parity
  with the reference needs **none** of those chain rules, because the
  reference implements none of them. R37.5.5 remains a Reloaded improvement
  on top of the port, and only it waits.

**D2.** *Linkage check and served-byte identity.* Two rules are not
implemented:

- the linkage rule of R37.5.6;
- the served-bytes rule of R37.5.7. Under it, the header stores keep the
  bytes as served, an AuxPoW header's identity hash covers only its 80-byte
  base, and the KawPoW, Firo and Verus families use their chains' own
  identity hashes for linkage.

Storing served bytes changes what both header stores persist, so existing
stores need a migration or re-sync decision. Narrowing the AuxPoW hash
changes the store's height-by-hash lookup, and the chain-defined hashes wait
on residual item (2) of the `SPEC` note in §37.5.7. The length guard does
not depend on any of these.

All three parts of this item are **Reloaded divergences** from the
reference rather than ports, and §37.5.7a bounds why that is acceptable:
the reference keeps re-encoded bytes, hashes an AuxPoW header over its
proof, and uses a double-SHA256 of the re-encoding for every family. None
of those values crosses the wire, so the divergence is confined to the
local header store and the local linkage checks and needs no
compatibility switch — only the storage decision named above.

## 37.10 Provenance Footer

- *Inputs:* the project's own revision history and current tree, for the
  T-DOC majority of this chapter (§37.0-§37.6: the already-shipped
  `kdf_spv_validation` crate, per-coin block-header storage backends,
  configuration-selected chain-variant reader, and proof-of-work/retarget
  validation -- by public behaviour and storage-contract shape only, no
  code transcribed); published Bitcoin consensus documentation (proof-of-work
  and difficulty-retarget rules, R31 externally dictated, §37.4); published
  Merkle-proof / SPV construction documentation (§37.6); the present reloaded
  workspace's own SQLite/IndexedDB storage contracts (§37.3). For the single
  **target** requirement (R37.7.2, the active chain-reorganization detector):
  a forensic comparison of reloaded's shipped behaviour against
  upstream/corpus, conducted under the chapter-01 two-team clean-room
  workflow -- the source of the "Upstream divergence" finding that
  reloaded historically shipped only the passive per-height overwrite of
  R37.7.1. For §37.5 (header families, selection, consistency guards): the
  baseline's own header reader, which selected layouts by version value
  alone plus a Qtum-only variant; the public header definitions of each
  chain family named in §37.5.2 (Bitcoin Core, including BIP-9 version
  signalling and its regtest test deployment on bit 28; the Zcash protocol
  specification; the merged-mining specification used by Namecoin-style
  chains; and the Ravencoin, Firo, Qtum, LBRY, PIVX, Peercoin and Verus
  node sources); the Electrum protocol method documentation for
  `blockchain.block.header` / `blockchain.block.headers` and the observed
  AuxPoW-truncation behaviour of ElectrumX 1.18.0; the public third-party
  coin-config file for the dictated `chain_variant` key and values; and an
  upstream/corpus correctness analysis of header-family selection, requested
  explicitly for this bug chase and conducted under the chapter-01 two-team
  workflow.
- *Permitted-input classes used:* baseline/as-built source (the shipped SPV
  subsystem, for the T-DOC majority); external public specification (Bitcoin
  consensus proof-of-work/retarget rules, Merkle-proof construction); R6
  (behavioural observation, for the storage/validation contract as currently
  implemented). For §37.5: R1 (baseline header reader), R3 (BIP-9,
  merged-mining and chain header specifications), R4 (Electrum protocol
  responses, chain-dictated header layouts, third-party coin-config key and
  values), R6 (observed Electrum-server header serving). For R37.7.2 and
  §37.5's upstream-behaviour notes: Forbidden corpus, under the chapter-01
  two-team clean-room workflow -- see below.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* consulted, under the chapter-01 two-team clean-room
  workflow, for upstream parity of the chain-reorganization detector only
  -- specifically the behavioural gap identified in R37.7.2's "Upstream
  divergence" note (that reloaded's shipped passive per-height overwrite
  omits the active detect-and-resolve routine upstream/corpus has). The
  active detector's own contract (trigger condition, resolve procedure,
  walk-back bound, convergence argument, and the acceptance test) is
  independently authored behavioural specification, not corpus expression:
  it binds observable outcomes and explicitly directs that "the
  implementation shall be expressed independently; this section binds the
  contract, not any particular branch structure." The corpus was also
  consulted for §37.5. The consultation used the stable reference tag
  v2.6.0-beta. The upstream development line at its March 2026 tip was
  compared with that tag and parses headers the same way, so nothing in
  §37.5 depends on which of the two is taken. From that consultation §37.5
  takes the following behavioural facts, all restated in this chapter's own
  words:
  - the per-family "Upstream recognition" column of §37.5.2;
  - the read paths that do not carry the configured family (R37.5.1a note);
  - the families that `"BTC"` does not exclude (R37.5.3 note);
  - the absence of any consistency guard on header lists (R37.5.6 note);
  - the value-keyed re-encoding and the AuxPoW hash scope (R37.5.7 note).

  The consistency guards, exclusivity rule and identity-hash rule in §37.5
  are this chapter's own proposed behaviour, not corpus expression. No other
  section of this chapter draws on the forbidden corpus.

  A **later re-confirmation pass** extended that consultation, under the
  same two-team workflow and under the public release designations
  `v2.6.0-beta` and `v3.0.0-beta` (the record is no more precise than those
  designations). It added the following behavioural facts, all restated in
  this chapter's own words:
  - that the configuration-selected family of R37.5.1 is as-built in the
    reference from `v2.6.0-beta` onward, that its accepted-value set is
    closed at exactly the eight strings of R37.5.1's table, and that an
    unrecognised value rejects activation (the vintage note in §37.5.1 and
    the port note in §37.5.8);
  - which read paths now carry the configured family and which two still do
    not, and that the two that do not are the ones that enforce exact byte
    consumption (the confirmation added to the R37.5.1a note);
  - that every entry of §37.5.2's recognition column is unchanged between
    the two designations, including the PIVX row's unconditional trailing
    read;
  - that the AuxPoW parent header is read through the same family-sensitive
    path as the outer header (the R37.5.2b note);
  - the correction of the §37.5.6 note: exact byte consumption is enforced
    on the generic single-header entry point and nowhere else, header lists
    are read without an end-of-buffer check so a short-per-header misparse
    succeeds silently, and there is no standard-layout retry and no linkage
    check anywhere;
  - that the re-encoding and identity-hash defects of the §37.5.7 note are
    unchanged, and that no chain-defined identity hash is implemented for
    any family (§37.5.7a);
  - the four resolutions tabulated in §37.5.7b: the single Firo switch
    timestamp with no lower bound, the absence of any per-network KawPoW
    activation time, the absence of chain-defined identity hashes, and the
    absence of any PIVX version threshold.

  §37.5.7a's divergence bound, §37.5.7b's residual `SPEC` items, and the
  step-4 and divergence notes added to §37.9 D1 and D2 are this chapter's
  own reasoning over those facts and over the public coins configuration.
- *Clean-side reconciliation:* §37.5.8 and §37.9 D1–D2 were written on the
  clean side of the wall, from this repository's own tree and the gated
  text of §37.5, without access to the forbidden corpus. They record how
  the shipped reader compares with §37.5 and what this change implements.
  The additions made to them by the later re-confirmation pass (the port
  note in §37.5.8 and the step-4a and divergence notes in §37.9 D1–D2) were
  not: they were written in the same pass as the §37.5 additions and rest
  on the facts that pass established.
- *Dirty Gate clearance (chapter 01 T1, R40, R43) for the later
  re-confirmation pass.* This item records one clearance and deliberately
  not two. The chapter's **original** forbidden-class-consulting material
  was cleared before the adoption of chapter 01 R37–R44 and those verdicts
  were recorded only in the commit messages of the revisions concerned, so
  no extent/method/measure attestation for them survives in the document
  set; under R43, T1 does not reach those revisions, and the derivation
  record above continues to carry them on their own pre-adoption footing.
  They were **not re-gated**, and nothing below is to be read as clearing
  them or as a record of their clearance.

  The **later re-confirmation pass** described above is a post-adoption
  revision and T1 does reach it. The material it added — the vintage note
  and the deployed-values and operator-check notes in §37.5.1, the
  confirmation appended to the R37.5.1a note, the confirmation appended to
  the §37.5.2 divergence note, R37.5.2b with its finding, the confirmation
  appended to the §37.5.3 finding, the corrected and re-confirmed §37.5.6
  finding, the confirmation appended to the §37.5.7 finding, §37.5.7a,
  §37.5.7b with its table and its residual `SPEC` note, the port note in
  §37.5.8, the step-4a note and the closing bullet of §37.9 D1, the
  divergence note of §37.9 D2, and the re-confirmation paragraph of the
  derivation record above together with this item — was authored by the
  KDF Spec Reader under the two-team procedure of chapter 01 §1.4A and
  **passed the KDF Dirty Gate** on 2026-10-24: PASS,
  discretionary-expression measure 0.00, finding list empty, and
  accordingly no remedy class for any line or for the candidate as a
  whole. That was the first gate pass on this material; no earlier verdict
  for it exists or was relayed. **Only that added material was gated.**
  The clearance for the later pass is therefore **complete and not
  pending**, and R40 withholds nothing here; the re-confirmation additions
  are no longer candidate text.

  *One consequence of the two footings sitting side by side.* The
  pre-adoption sentence of the derivation record above identifies its
  reference partly by a development-line tip, which is a branch state and
  therefore of the class §1.7's reference-version constraint withholds.
  That constraint, by its own wording and by R43, binds a scoped record
  written on or after its adoption, so the older sentence is grandfathered
  and this chapter is not defective for carrying it; the record below is
  the one the constraint binds, and it names nothing beyond the two public
  release designations. Chapter 01's R25 re-check paragraph nevertheless
  states that one pre-adoption record in the document set names an
  identifier of that withheld class and that no other does. A reader
  comparing that statement with this footer may read the two as
  inconsistent. Reconciling them is chapter 01's business and the subject
  of that chapter's own deferred item (D5), not this chapter's, and
  nothing is changed here on that account.

  *Which sections carry a pointer to this item, and how that set was
  fixed.* §37.5, §37.5.7, §37.5.8 and §37.9 each carry a short clearance
  pointer, so that a reader or an implementer entering at the material
  passes the notice where the material appears (R40) and not only on
  reaching this footer. The set was fixed from where readers demonstrably
  enter — inbound references within this repository, ranked, rather than
  taste — crossed with where the pass left numbered or rule-bearing text.
  §37.5 is the highest-traffic entry by a wide margin: chapters 38, 58 and
  39 and the implementation plan under `docs/plans/` all cite it, and its
  pointer sits immediately above §37.5.1, so it is also what an entrant
  arriving at §37.5.1 or at R37.5.1a by name from chapter 39 passes first.
  Reading forward from it reaches the three sections that carry new
  material but have no inbound reference of their own — §37.5.2 with
  R37.5.2b, §37.5.3 and §37.5.6 — so none of them needs a pointer of its
  own. §37.5.7 is a separate entry and not reached by reading forward from
  the head: the implementation plan dispatches a Spec Reader to the `SPEC`
  note that stood there, and an entrant arriving that way lands on
  §37.5.7a, §37.5.7b and the residual note, which is where three of the
  four numbered-or-rule-bearing additions sit. §37.5.8 and §37.9 D1–D2 are
  what an implementer actually works from — the plan names them in its
  scope list and requires each change to update them — and both carry new
  material. The remaining sections carry none, and that is a scope
  judgement rather than an oversight: §37.0–§37.4 and §37.6–§37.8 took
  nothing from this pass, §37.6 is well referenced but entirely unchanged
  by it, §37.8's acceptance criteria are derivative of rules the pointed
  sections already bind, and §37.10 is the record itself.

  The three elements of the R39 scope attestation for that pass, carried
  forward here in corpus-free terms as chapter 01 T1 requires:

  - *Extent.* The forbidden-class material corresponding to the subject
    matter this chapter's §37.5 names in its own terms: the reference's
    UTXO block-header codec, the header-read surface of its
    Electrum-backed UTXO coin layer, and the configuration surface that
    selects a header family — that is, family selection, the per-family
    byte layouts, and the single-header, header-list,
    trusted-anchor-check, header-store-re-read and median-time-past read
    occasions. Quantity examined: **seven files** of that surface at the
    newer designation, five of them read in the relevant part or in full
    and all seven covered by the mechanical checks named below, two of
    them additionally in full at the older designation, together with a
    designation-to-designation comparison across that surface. Reference
    designations: the publicly published release designations
    `v2.6.0-beta` and `v3.0.0-beta`, which the lineage's own releases
    carry and which chapter 01 §1.7 admits as public-release facts.
    **The record is no more precise than those two designations** — no
    post-anchor commit identifier, no branch state and no location within
    the compared material is given (chapter 01 R11 and the
    reference-version limit of §1.7), and that limit is left visible
    rather than papered over.

  - *Method.* A stated **combination**: by reading, and by two mechanical
    comparisons run for this pass — an identifier-shape extraction over
    the candidate's added lines, checked against the identifier set of the
    compared material, and a word-level n-gram and string-literal overlap
    measurement between the added lines and that material. **This
    repository's own residual-similarity gate tooling was not run:** it
    compares embedded code blocks and the added material embeds none, so
    the instrument had no operand and its figure would not have been
    informative. The absence is stated so that it is visible rather than
    implied.

  - *Measure.* **0.00** on the discretionary-expression basis — the same
    figure the leak report carries, not a second one — with
    dictated-interop and pinned-interface content excluded from the
    measured set before the judgement was made, per token under the
    forced-versus-free test rather than by the group a token sits in.
    Excluded on that basis, so that the figure's coverage is visible: the
    chain-dictated header byte layouts, field orders and byte lengths; the
    chain and BIP-9 version-signalling values, bit positions and masks;
    the merged-mining flag bit; the chain-dictated switch timestamp; the
    double-SHA256 proof-of-work and identity rule; the third-party
    coin-configuration key and its accepted string values; and the public
    coin tickers. **Judgement standard read against:** zero tolerance —
    one identified item of genuine discretionary expression fails a
    candidate however much of the surrounding content is legitimately
    dictated, and match volume alone is not evidence either way, so the
    pass is not a function of the low figure and each candidate item was
    judged individually. What the figure reflects: the only multi-token
    coincidence found anywhere in the added lines lies wholly inside the
    dictated configuration-value set this chapter already frames as
    config-compat interop that must be accepted verbatim, and is excluded
    on that ground; no other coincidence of five or more consecutive words
    was found; no string literal of the compared material is reproduced;
    and the added lines carry exactly two identifier-shaped tokens, the
    dictated configuration key and the filename of a document of this
    repository. Absent throughout, as the verdict's classification
    records: private non-wire identifiers; verbatim and near-verbatim
    bodies; step-by-step control-flow, branch or loop transcription;
    diagnostic, error and log **message wording**; per-method tables keyed
    to internal names; internal module trees; internal test identifiers;
    any location within the compared material; and any post-anchor commit
    identifier or branch designation.
