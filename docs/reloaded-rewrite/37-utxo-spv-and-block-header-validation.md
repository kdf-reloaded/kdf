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
(e.g. progress / temporary error) without aborting the coin.

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
| `"BTC"` | Standard 80-byte header only, whatever the version value (R37.5.3). |
| `"PPC"` | Standard 80-byte header (Peercoin-style); a version value of 4 does not select the Equihash-style layout. |
| `"QTUM"` | Qtum proof-of-stake header. |
| `"LBC"` | LBRY claim-trie header. |
| `"RVN"` | Standard header before the chain's KawPoW switch, KawPoW header after it. |
| `"PIVX"` | PIVX header with the trailing Sapling root. |
| `"RICK"`, `"MORTY"` | Komodo asset-chain Equihash-style header for every block, including a genesis block whose version value is 1. |

An unrecognised `chain_variant` string shall reject coin activation with a
configuration error; it shall not silently fall back to the default.

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

> **Code-quality finding (informative).** Upstream has no defence against a
> version value that belongs to two families. Its only protection is the
> per-coin `chain_variant` gate on some families. Header lists are read as
> "count headers, one after another" with no check that the bytes end exactly
> where the last header ends, so a misparse that reads too few bytes goes
> unnoticed and one that reads too many shows up only as an unexpected end.
> Nothing checks lengths or linkage and falls back to the standard layout.
> On an Electrum-connected coin, a header-list failure stops the
> median-time-past query. That value feeds the refund-eligibility check of an
> HTLC and the lock-time of the refund transaction, so a collision can stop
> such a coin from refunding until the parse is fixed. The proposed correct
> behaviour is R37.5.3, R37.5.4 and R37.5.6: explicit families are
> exclusive, KawPoW needs `"RVN"`, and any version-derived layout choice is
> confirmed by exact byte consumption and, where computable, by linkage, with
> std-80 as the fallback.

### 37.5.7 Identity hash and stored bytes

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

<<<SPEC Chain-rule details cited from public chain definitions and not re-verified in this pass: the PIVX header-version thresholds for its zerocoin-era field and its Sapling-root field; the Firo MTP start and ProgPoW switch rules; the Ravencoin KawPoW activation time per network; the KawPoW, Firo and Verus identity hashes; and whether Electrum servers for the KawPoW and Firo families serve the full header shapes of §37.5.2. Confirm them against each chain's published header definition before R37.5.5 is implemented for those families. SPEC>>>

> **Interop note (R29/R31).** Each family's byte layout is dictated by its
> chain, and the `chain_variant` key and values are dictated by third-party
> coin-config files; neither is this project's choice. §37.5 binds the
> selection contract, the layouts, and the consistency guards. It does not
> bind any particular parser structure.

### 37.5.8 Implementation status

The requirement of R37.5.1 is correct and the shipped reader falls short of
it. Selection by configuration is the contract; the version-value selection
described below is a gap to be closed (§37.9 D1), not an alternative
contract. The chain-variant set the reader knows is much smaller than the
set of families it branches on, and that mismatch is the source of the
collision of R37.5.4a.

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
4. Withdraw version-only KawPoW recognition under the default family
   (R37.5.4), and make `"BTC"` exclusive (R37.5.3).

It is deferred because it changes the coin-activation configuration surface
and every header read path at once, and it touches both storage backends.
Its last step also changes behaviour for KawPoW coins that are configured
today without the key, so it needs coordinated coin-file updates and its own
compatibility review. The length guard of §37.5.8 removes the observed
failure without any of that.

Until this is closed:

- A standard chain whose blocks carry the colliding version value works
  through that guard.
- A Qtum coin's single-header and stored-header reads use the default family.
- The R37.5.5 families also wait on the open `SPEC` note in §37.5.7.

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
on the `SPEC` note in §37.5.7. The length guard does not depend on any of
these.

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
- *Clean-side reconciliation:* §37.5.8 and §37.9 D1–D2 were written on the
  clean side of the wall, from this repository's own tree and the gated
  text of §37.5, without access to the forbidden corpus. They record how
  the shipped reader compares with §37.5 and what this change implements.
