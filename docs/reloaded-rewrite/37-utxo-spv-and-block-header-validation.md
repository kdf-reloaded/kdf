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

> **Clearance (chapter 01 T1, R40) — the chain-variant pass of
> 2026-10-24.** The material the chain-variant pass added or rewrote in
> this chapter was authored by the KDF Spec Reader on 2026-10-24 under the
> chapter-01 §1.4A two-team procedure: from the forbidden corpus under the
> release designations named in the *chain-variant pass* clearance item of
> §37.10, from public-network observation of the Electrum servers the
> public coin registry configures (R6), and from the published header
> definitions of the chains concerned (R3). It **passed the KDF Dirty
> Gate** on 2026-10-24 before being relied on (PASS,
> discretionary-expression measure 0.02, finding list empty), so nothing
> it added is a candidate awaiting a gate; the point-of-appearance markers
> R40 required while the clearance was pending have been removed. Where a
> passage of that pass replaces earlier text, the earlier text is
> superseded on its substance. Its extent, method and measure are
> recorded once, in that item of §37.10, and are not restated here; the
> chapter's earlier forbidden-class-sourced material stands on the two
> records above it in the same section and was not part of this
> candidate.

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
| `"RVN"` | Standard header, except that a header whose version value is exactly `0x30000000` carries the KawPoW trailer (R37.5.5); shared by several KawPoW-descended chains. |
| `"PIVX"` | PIVX header with the trailing Sapling root. |
| `"RICK"`, `"MORTY"` | Komodo asset-chain Equihash-style header for every block, including a genesis block whose version value is 1. |

**† This cell states a requirement of this chapter and not a ported
behaviour.** The distinction matters here and not only two subsections
away: the whole column reads as one list, and a reader who takes every
cell for as-built reference behaviour will under-count what has to be
built. Unmarked cells fall within the scope of the port note in §37.5.8 —
the key, the closed value set, the rejection of an unrecognised value,
and the family each of those values pins. The one marked cell does not:
`"BTC"`'s exclusivity is bound by R37.5.3, and the finding beneath that
rule records that the value is a partial guard rather than the
exclusivity the rule requires. The `"RVN"`
cell carried the same mark until the chain-variant pass of 2026-10-24,
when it read "standard before the chain's KawPoW switch, KawPoW after
it"; R37.5.5 as rewritten keeps the reference's version key for that
family, so the cell now states a ported behaviour and the mark is
removed. §37.5.8's closing list is the full
inventory of what remains this project's own, and §37.9 D1 is what
closing the ported part takes.

An unrecognised `chain_variant` string shall reject coin activation with a
configuration error; it shall not silently fall back to the default.

Three details of that contract, as the reference implements it at the
designations named in §37.10 (established by the chain-variant pass of
2026-10-24 and cleared with it), which an implementer of §37.9 D1 needs:

- **Where the key is read and what it may not disturb.** The value is read
  from the coin's raw `protocol` object as a plain string, beside `type`.
  It is not part of the typed protocol descriptor
  [chapter 55](55-coin-activation-wire-contract.md) binds (`{type,
  protocol_data}`) and does not change how that descriptor is parsed;
  a protocol object that carries the key still parses as the same protocol
  type with or without it. The comparison is exact and case-sensitive on
  the eight spellings; no ticker-derived or protocol-type-derived default
  is applied — a QTUM- or QRC20-protocol coin whose file omits the key is
  the **default** family in the reference, not the Qtum family.
- **How the rejection surfaces.** An unrecognised value fails the
  coin-configuration step of the UTXO coin build, so the activation ends in
  the error class chapter 55 binds for a coin that cannot be built
  (`CoinCreationError`, carrying the ticker and a textual reason), not in
  the protocol-parse error class; the textual reason is diagnostic wording
  and not bound.
- **Where the value lives at run time.** The resolved family is a field of
  the coin's configuration, handed to the coin's RPC client object (the
  Electrum backend and the native-daemon backend alike) and to its header
  store at construction. Of these only the Electrum backend and the header
  stores parse header bytes; the native-daemon backend takes a block's time
  and the median-time-past from the node's own block data and never parses
  a header, so R37.5.1a's read paths are the Electrum backend's and the two
  header stores'.

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
> *Vintage, sharpened (chain-variant pass of 2026-10-24).* At the release
> designation immediately preceding `v2.6.0-beta` — `v2.5.2-beta` — the
> `chain_variant` key is **not read**: the family is chosen in code from
> the coin ticker (suffixed forms such as `BTC-segwit` included). Between
> the two designations the rework
> replaced that ticker table with the configuration key, and its published
> coin-file migration guidance mapped the then-hardcoded tickers onto the
> values: `BTC`, `BCH`, `NAV`, `RIC` → `"BTC"`; `QTUM`, `RUNES` →
> `"QTUM"`; `LBC` → `"LBC"`; `PPC` → `"PPC"`; `RICK`, `DOC` → `"RICK"`;
> `MORTY`, `MARTY` → `"MORTY"`; `RVN`, `AIPG`, `XNA`, `EVR`, `MEWC`, `AVN`
> → `"RVN"`; `PIVX`, `DOGEC` → `"PIVX"`; every other coin stays on the
> key-absent default. Two consequences: a coin file that carries the key
> is accepted by both designations (the older one ignores it), so adding
> the key to a shared file is backward-compatible; and the public registry
> departed from that guidance for one coin — `MEWC`, placed under `"BTC"`
> the day before the rework landed — which the next note takes up with
> observed facts. No value of the set selects a *time*- or
> *height*-switched layout by itself: the only time comparison in the
> reference's header reader is the Firo switch of §37.5.2, keyed on a
> version value and applied to every family, and there is no height
> comparison anywhere.
>
> *Deployed values.* The public third-party coins configuration (snapshot
> of 2026-10-18) uses six of the eight: `BTC` on fourteen entries, `QTUM`
> on eleven, `RVN` on four, `PIVX` on two, `LBC` on two and `PPC` on one.
> `RICK` and `MORTY` no longer appear in it — they are the two
> development asset chains the value set was introduced for — so the
> project must still accept them but no shipped coin exercises them.
>
> *The `MEWC` check, made (chain-variant pass of 2026-10-24).* The earlier
> revision of this note asked for an operator check of `MEWC`'s configured
> value against its real header shape. The check was made on 2026-10-24
> against the registry's own Electrum servers for the coin (R6) and the
> chain's published header definition (R3), and the answer is that **no
> value of the reference's set describes the chain**:
>
> - the chain's header rule is a time switch with a merged-mining
>   exception — a block carrying the AuxPoW flag bit (bit 8) has the
>   standard nonce followed by the AuxPoW proof whatever its time, and a
>   block without it carries the KawPoW-shaped trailer (height, 64-bit
>   nonce, mix hash) once past the chain's switch time;
> - the chain's **current** blocks interleave both kinds. The servers
>   serve the merged-mined ones as **80 bytes** (the AuxPoW proof is
>   stripped; the version value observed was `0x30090100`) and the native
>   ones as **120 bytes** (version `0x30090000`); a three-header list at
>   the tip measured 80 + 80 + 120 bytes. Earlier history is uniform:
>   120-byte headers with version `0x30000000` up to roughly height 1.5
>   million, then 120-byte headers with version `0x30090000`, then the
>   mixed régime from roughly height 1.7 million on;
> - under `"BTC"` (the registry's value) the reference reads 80 bytes per
>   header: the list read is the silent misparse of the §37.5.6 finding,
>   so the coin's median-time-past is **wrong without an error**; under
>   `"RVN"` (the rework's own guidance) neither kind parses — version
>   `0x30090000` is not the exact KawPoW value, so the native headers read
>   as 80 bytes, and the flag bit on the merged-mined ones starts an
>   AuxPoW read on bytes the server did not send;
> - under this chapter's rules the coin fails **loudly** instead: with the
>   AuxPoW recognition of R37.5.2 and the fallback of R37.5.6 the 80-byte
>   merged-mined headers parse, the 120-byte native ones do not under
>   `"BTC"` or the default, and a mixed list is rejected whole.
>
> The coin therefore needs a family of its own, which the reference does
> not have and which the shared registry cannot name without the reference
> rejecting the entry; §37.9 D3 records the gap and the options. The same
> check was made for every other registry entry whose family this chapter
> models, and two more entries turned out to be mis-described by their
> value or by its absence — `RIC` under `"BTC"` and `XPM` with no key — for
> the same structural reason (their chains' own layouts, §37.5.2); they
> are in D3 as well. Everything else checked matches its value: the
> `"RVN"` entries `RVN`, `EVR`, `XNA` serve 120-byte KawPoW headers, and
> `AVN` serves 80-byte headers whose version value is not the KawPoW one,
> so it reads correctly as std-80 under `"RVN"` by the version key of
> R37.5.5; `RUNES` under `"QTUM"` serves Qtum-shaped headers; `LBC` 112
> bytes; `PIVX` 112 bytes at the tip. The check is repeatable: fetch one
> header and one short header list from a configured server at a recent
> height and compare the byte count with §37.5.2.

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
>
> Re-confirmed by the chain-variant pass of 2026-10-24 at both
> designations named in §37.10, reading the rework that introduced
> the key: the rework threaded the configured family into exactly the
> paths listed as carrying it — the header-list read of the sync loop, the
> trusted-anchor check, the median-time-past list and both header stores'
> re-reads — and left the two single-header fetches on the family-agnostic
> entry point. Nothing merged after it and before `v3.0.0-beta` touches
> those two paths. No read path of the reference is keyed on a block
> *height* or on a *time* taken from configuration.

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
| KawPoW (Ravencoin-style) | `version (4) + previous hash (32) + Merkle root (32) + time (4) + bits (4) + height (4) + 64-bit nonce (8) + mix hash (32)` = 120 bytes; there is no 32-bit nonce field | Ravencoin: block time at or after the chain's published mainnet switch time `1588788000` (first such block observed at height 1219736; the block before it carries the same version value with the 80-byte layout). Each other chain sharing the `"RVN"` value has its own rule — Evrmore from its first block, Neurai likewise at the tip, Avian **no longer** (80-byte headers at the tip, version not the KawPoW value) — so no single time rule serves the value (R37.5.5). | Version value exactly `0x30000000` **and** `chain_variant` = `"RVN"`. |
| Equihash-style (Zcash / Komodo; Sapling-root before time) | `version (4) + previous hash (32) + Merkle root (32) + final Sapling root / reserved (32) + time (4) + bits as plain 4-byte integer (4) + nonce (32) + solution (compact-size length + bytes; 1344 bytes for Equihash 200,9)` | Chain family (Zcash-derived headers are always this shape; version value is 4). | Version value 4 and `chain_variant` neither `"BTC"` nor `"PPC"`; or `chain_variant` `"RICK"`/`"MORTY"` for every version value. |
| Verus | Same shape as the Equihash-style row; the solution is the chain's own proof-of-work solution vector | The version field carries the value 4 with the solution-version marker bit 16 (`0x00010000`) set, i.e. `0x00010004`. | Version value `0x00010004`, whatever `chain_variant` says; the marker bit is cleared for the in-memory version and restored on re-encoding. |
| Firo MTP | std-80, then `MTP version (4) + MTP hash value (32) + reserved (32) + reserved (32)` = 180 bytes | Block time from the chain's MTP start until its ProgPoW switch. On mainnet the MTP start is the chain's published block 117564 (observed: 80-byte headers with version `0x20000000` through 117563, 180-byte headers from 117564), and the ProgPoW switch is block 419264 (observed: 180 bytes at 419263, 120 at 419264), matching the switch time. Because pre-MTP headers never carry the version value the reference keys on, the missing lower bound is harmless on mainnet. | Version value exactly `0x20001000` and time before `1635228000` (the chain's ProgPoW switch time), whatever `chain_variant` says. |
| Firo ProgPoW | `version (4) + previous hash (32) + Merkle root (32) + time (4) + bits (4) + height (4) + 64-bit nonce (8) + mix hash (32)` = 120 bytes; no 32-bit nonce | Block time at or after the chain's ProgPoW switch time (`1635228000`). | Version value exactly `0x20001000` and time at or after `1635228000`, whatever `chain_variant` says. |
| Qtum proof-of-stake | std-80, then `state root (32) + UTXO root (32) + stake prevout (32-byte txid + 4-byte index) + block signature (compact-size length + bytes)` | Every Qtum header carries these fields, whatever its version value. | Version value exactly `0x20000000` **and** `chain_variant` = `"QTUM"`. |
| LBRY claim trie | `version (4) + previous hash (32) + Merkle root (32) + claim-trie root (32) + time (4) + bits (4) + nonce (4)` = 112 bytes | Every LBRY header carries the claim-trie root. | Version value exactly `0x20000000` **and** `chain_variant` = `"LBC"`. |
| PIVX (32-byte field after nonce) | std-80, then one 32-byte field = 112 bytes **or** std-80 alone, by header version | The chain's published header definition: header versions **4, 5 and 6** carry a 32-byte zerocoin accumulator checkpoint after the nonce; version **7 carries nothing** (std-80); versions **8 and above** carry the final Sapling root in the same position; versions 3 and below carry nothing. Observed boundaries on mainnet: version 4 from height 863787 (80 bytes at 863786, 112 from 863787), version 7 from 2153200 (112 bytes at 2153199, 80 from 2153200 through 2700499), version 8 from 2700500 (112 bytes), version 11 and 112 bytes at the tip. | Always read after the nonce when `chain_variant` = `"PIVX"`, whatever the version value — correct for versions 4–6 and 8+, **wrong for version 7 and for 3 and below**. |
| Peercoin | std-80 (the block signature is outside the header) | Chain family. | `chain_variant` = `"PPC"` only suppresses the Equihash-style reading of version-4 headers. |
| Riecoin *(added by the chain-variant pass; cleared, §37.10)* | `version (4) + previous hash (32) + Merkle root (32) + time (8, a 64-bit integer) + bits (4) + prime offset (32)` = 112 bytes; there is no 32-bit nonce | Every Riecoin header (observed from height 1 through the tip on 2026-10-24, with version values 2 and `0x20000000`; the field layout was decoded from the served bytes and shall be confirmed against the chain's published header definition before it is implemented). | **None.** The reference has no value for this layout; the registry's `"BTC"` entry for `RIC` reads 80 bytes per header (§37.9 D3). |
| Primecoin *(added by the chain-variant pass; cleared, §37.10)* | std-80, then the prime-chain multiplier as a compact-size-prefixed byte string (observed 8–9 bytes, so 89–90 bytes per header); variable length | Every Primecoin header (observed at height 1000000 and at the tip; confirm against the chain's published definition before implementing). | **None.** The registry's `XPM` entry carries no key; the default reads 80 bytes per header (§37.9 D3). |
| Meowcoin *(added by the chain-variant pass; cleared, §37.10)* | A native block past the chain's switch time: the KawPoW layout, 120 bytes. A merged-mined block (version flag bit 8 set): std-80 followed by the AuxPoW proof — which the registry's servers serve **stripped to 80 bytes** | The chain's published rule, in this order: the AuxPoW flag selects the nonce-plus-proof form whatever the time; otherwise the time switch selects the KawPoW trailer. The two kinds interleave in the current chain. | **None** that parses the current chain (§37.5.1 note, §37.9 D3). |

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

*Served shapes, observed (chain-variant pass of 2026-10-24; cleared — see
the clearance item of §37.10).* On 2026-10-24 the
servers the public coin registry configures were queried directly
(`blockchain.block.header` at several heights and a short
`blockchain.block.headers` list at the tip, without a checkpoint height),
and the served byte counts per header were: Ravencoin, Evrmore and Neurai
**120** (Evrmore from height 1; Ravencoin 120 from height 1219736 and 80
before it); Avian **80** at the tip; Firo **80** through height 117563,
**180** from 117564 through 419263, **120** from 419264 and at the tip;
PIVX **80** or **112** by version exactly as the row above states; LBRY
**112** from height 1; Verus **1487** (Equihash-style with a 1344-byte
solution); Runebase (`"QTUM"`) Qtum-shaped and variable, 251–311 bytes;
Riecoin **112**; Primecoin **89–90**; Meowcoin **80** for merged-mined and
**120** for native blocks, interleaved. Two conclusions bind: (a) the
servers deployed for the KawPoW-family and Firo coins **do** serve the
full 120- and 180-byte shapes this section expects, which closes the
former residual item on served shapes (§37.5.7b); (b) the AuxPoW-stripping
behaviour above is **not** confined to the checkpoint case — a second
server implementation (the Meowcoin servers identify as an electrs/esplora
derivative) stripped the proof with no checkpoint height requested — so the
sentence "the project shall expect full AuxPoW headers" is a default
expectation and not an assumption a reader may rely on; the 80-byte
acceptance is the binding rule.

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
>
> *What the observations of the chain-variant pass add to this note.*
> Three of the mismatches above are now measured rather than argued, and
> one of them changes the conclusion. **PIVX:** the unconditional read is
> wrong for a whole régime, not only the earliest blocks — every version-7
> header (observed heights 2153200–2700499, about a year and a half of the
> chain) is 80 bytes and reads 32 bytes into the next header. **Ravencoin:**
> blocks shortly before the switch carry the KawPoW version value with the
> 80-byte layout (observed at height 1219735), so the version key misreads
> them; this only matters for an anchor or a median window placed at the
> switch. **Avian:** its current 80-byte headers carry a version value
> other than the KawPoW one, so the version key reads them correctly under
> `"RVN"`, whereas a Ravencoin time rule applied to the `"RVN"` value would
> read them wrongly. That last point is why R37.5.5 now keeps the version
> key for the KawPoW trailer within `"RVN"` rather than requiring a time
> rule: the value is shared by chains whose activation rules differ, so a
> time rule is not derivable from the value.

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

> **Clearance (chapter 01 T1, R40).** This rule was rewritten by the
> chain-variant pass of 2026-10-24 — the earlier wording required a
> block-time rule for the KawPoW/ProgPoW layout and is superseded on its
> substance — and the rewritten rule **passed the KDF Dirty Gate** on
> 2026-10-24 (PASS, discretionary-expression measure 0.02, finding list
> empty), so it is a cleared requirement and not a candidate. Its extent,
> method and measure are recorded once, in the *chain-variant pass*
> clearance item of §37.10, and are not restated here.

R37.5.5 Inside a pinned family, the reader shall use the rule below for the
layout choices that remain after the family is fixed. Every such choice is a
*within-family* choice and is subject to the structural guard of R37.5.6 in
the same way as a version-signalled choice under the default family.

- **Qtum and LBRY** (`"QTUM"`, `"LBC"`): the extra fields are present in
  every header of the chain, whatever the version value (observed for LBRY
  from height 1; Qtum-shaped from the first block for Runebase). The
  reference reads them only when the version value is exactly
  `0x20000000`; the project reads them unconditionally within the family.
- **PIVX** (`"PIVX"`): the trailing 32-byte field is read when the header
  version is **4, 5, 6, or 8 and above**, and not read for version 7 or
  for 3 and below (the chain's published header definition, §37.5.2). The
  reference's unconditional read is wrong for version 7 and is not
  followed.
- **KawPoW trailer** (`"RVN"`): read when the version value is exactly
  `0x30000000`, as the reference does, and not by a time rule. The value is
  shared by several chains whose switch rules differ (Ravencoin by time,
  Evrmore from its first block, Avian no longer at all — §37.5.2), so a
  time rule is not derivable from the value, while every post-switch
  header of every chain observed under the value carries that exact
  version and every current non-KawPoW header under it carries another.
  The one known exception — a pre-switch Ravencoin header carrying the
  KawPoW version at 80 bytes — is a within-family version choice and so
  falls to R37.5.6: served alone it is re-read as std-80; in a list it is
  rejected with the list, which is the correct outcome for a window that
  straddles the chain's switch.
- **Firo** (default family): the version value `0x20001000` together with
  the chain's ProgPoW switch time `1635228000`, as the reference does. On
  mainnet this coincides exactly with the chain's published time rule
  (§37.5.2), and no lower bound is needed because pre-MTP headers carry a
  different version value.
- A chain with no family in the set (§37.9 D3) has no within-family rule
  here until a family exists for it.

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

*Extension by the chain-variant pass of 2026-10-24 (cleared — see the
clearance item of §37.10).* The guard applies equally to a layout chosen
**within** a pinned family from the version value (the KawPoW trailer under
`"RVN"`, the PIVX trailing field by its version, the Firo switch), because
such a choice is as exposed to a colliding or pre-switch version value as a
default-family choice is. It does not apply to a layout the family fixes
unconditionally (Qtum, LBRY, Riecoin, Primecoin): there a header of the
wrong length is simply malformed under that family.

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
| Firo MTP start and ProgPoW switch rules | The reference has **no MTP start rule at all**. It keys both Firo layouts on one exact version value together with one switch timestamp, `1635228000`: the MTP layout when the block time is **before** it, the ProgPoW layout when the time is **at or after** it. There is no lower bound on the MTP layout. Confirmed at `v2.6.0-beta` and `v3.0.0-beta`. *Closed by observation and the chain's published parameters:* the mainnet MTP start is block 117564 and the ProgPoW switch is block 419264 (the block whose time first reaches `1635228000`); headers before the MTP start carry version `0x20000000` and 80 bytes, so the absent lower bound cannot misfire on mainnet. |
| Ravencoin KawPoW activation time per network | The reference has **no activation time** for this family on any network. The KawPoW layout is keyed on the exact version value plus the configured `"RVN"` family, with no time comparison anywhere. There is therefore no per-network constant to obtain for parity. Confirmed at both designations. *Closed, with a changed conclusion:* Ravencoin's published mainnet switch time is `1588788000` (first 120-byte header observed at height 1219736; its testnet value differs), but R37.5.5 no longer asks for it — the `"RVN"` value is shared by chains with other rules (Evrmore from genesis, Avian none at the tip), so the version key is kept and the time is informative only. |
| KawPoW, Firo and Verus identity hashes | The reference uses **no** chain-defined identity hash for these families (§37.5.7a). None needs to be sourced for parity; they are needed only for R37.5.7's divergence, whose adoption is gated on §37.9 D2 regardless. |
| PIVX header-version thresholds | The reference applies **no version threshold**: for a coin configured as `"PIVX"` it reads one 32-byte field after the nonce for every header, whatever the version value. That is consistent with being byte-correct over the chain's recent history, because the chain's zerocoin-era field and its Sapling-root field occupy the same 32-byte position and differ only in meaning — a single unconditional read is correct for every version carrying **either**. *Corrected and closed:* the earlier claim that it is wrong "only for the chain's earliest versions" was too narrow. The chain's published definition carries the field for versions 4–6 and 8+, and **not** for version 7 or for 3 and below; observed on mainnet, version-7 headers are 80 bytes from height 2153200 through 2700499. The thresholds R37.5.5 needs are therefore known and R37.5.5 now states them. |
| Whether Electrum servers for the KawPoW and Firo families serve the full header shapes | **Closed by direct observation (2026-10-24, R37.5.2a):** the registry's Ravencoin, Evrmore and Neurai servers serve 120 bytes per post-switch header, and the Firo servers 180 bytes per MTP-era header and 120 per ProgPoW-era header, on both the single-header and the header-list methods, without a checkpoint height. The earlier text — that the reference's expectation was only indirect evidence — is superseded. |

> **Superseded `SPEC` note — retained for the record, not an open item.**
> The note below carried the `SPEC` markers until §37.5.7b answered four
> of its five items. The markers have been removed and the text kept in
> full, so that a reader, and equally a count of open `SPEC` items in
> this chapter, finds exactly one open note — the narrowed one that
> follows this block — rather than two of which one is closed. The superseded
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

> *The residual note, narrowed again (chain-variant pass of 2026-10-24).* The
> two-item `SPEC` note that stood here is superseded: item (1), the served
> header shapes, is closed by the observation recorded in R37.5.2a; of
> item (2), the PIVX version boundaries, the Ravencoin switch time and the
> Firo MTP lower bound are closed in the table above. What remains is the
> single item below, which is a dependency of §37.9 D2 only and not of
> parity. For the record, the superseded text read: *"Two residual
> chain-rule items … (1) Not directly observed: whether the Electrum
> servers configured for the KawPoW-family and Firo coins serve the full
> 120-byte and 180-byte header shapes … (2) Not re-verified against the
> chains' published definitions … the exact PIVX header-version
> boundaries …; the per-network Ravencoin KawPoW activation times; the
> Firo MTP lower bound; and the three chain-defined identity hashes."*

<<<SPEC One residual item, needed only for the identity-hash divergence of R37.5.7 and R37.5.7a (§37.9 D2) and not for parity with the reference: the three chain-defined block-identity hashes — the KawPoW-family header hash, the Firo MTP and ProgPoW header hashes, and the Verus header hash — have not been specified from the chains' published definitions. Until they are, R37.5.7's own escape applies and the linkage checks skip those families. SPEC>>>

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
  - Under `"PIVX"`, a version-4 and a
    version-8 header of 112 bytes each parse with the trailing field, and a
    version-7 header of 80 bytes parses as std-80; a list mixing the two
    lengths in the chain's own order parses whole (R37.5.5).
  - Under `"RVN"`, a list of 120-byte headers
    with version `0x30000000` parses as KawPoW; a single 80-byte header
    with that version is re-read as std-80; an 80-byte header with another
    version value (the Avian case) parses as std-80 directly (R37.5.5,
    R37.5.6).
  - A header list in which the configured
    family's layout does not account for every served byte — for example
    112-byte Riecoin headers read under `"BTC"`, or an interleaved 80/120
    Meowcoin list — is rejected whole and surfaces as an error; it never
    yields a median (R37.5.6, §37.9 D3).
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
- The R37.5.5 families no longer wait on
  anything: the chain-variant pass of 2026-10-24 pinned the PIVX version thresholds, kept
  the version key for the KawPoW trailer and confirmed the Firo rule, so
  R37.5.5 can be implemented together with step 2. It remains a Reloaded
  improvement on top of the port — the reference's PIVX read is wrong for
  version-7 headers and its Qtum/LBRY reads are keyed on one version value
  — but it is no longer blocked. Step 2 shall implement the PIVX rule in
  R37.5.5's thresholded form, not the reference's unconditional read.

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
on the narrowed `SPEC` note of §37.5.7 (now the only item left open
there: the three chain-defined identity hashes). The length guard does not
depend on any of these.

All three parts of this item are **Reloaded divergences** from the
reference rather than ports, and §37.5.7a bounds why that is acceptable:
the reference keeps re-encoded bytes, hashes an AuxPoW header over its
proof, and uses a double-SHA256 of the re-encoding for every family. None
of those values crosses the wire, so the divergence is confined to the
local header store and the local linkage checks and needs no
compatibility switch — only the storage decision named above.

**D3.** *Chains the reference's value set cannot describe.* *(Added by the
chain-variant pass of 2026-10-24 and cleared with it — see the clearance
item of §37.10.)* Three registry entries were found, by direct observation
of their configured servers (§37.5.1, §37.5.2a), to carry headers that no
value of the closed set of R37.5.1 lays out correctly:

| Entry | Registry value | Served shape | Under the reference | Under this chapter after D1 |
|---|---|---|---|---|
| `RIC` | `"BTC"` | 112 bytes, every header (Riecoin layout, §37.5.2) | silent misparse of every list (§37.5.6 finding) | every list rejected; median-time-past errors |
| `XPM` | *(absent)* | 89–90 bytes, variable (Primecoin layout) | silent misparse | every list rejected |
| `MEWC` | `"BTC"` | 80 (merged-mined, proof stripped) and 120 (native) interleaved (Meowcoin rule) | silent misparse under `"BTC"`; failure under `"RVN"` | mixed lists rejected; uniform 80-byte lists parse |

Rejection is the better of the two outcomes — a wrong median-time-past
mis-times a refund without any report, §37.5.6 — but it still leaves the
three coins without a working median-time-past, and therefore without
working HTLC spend and refund lock-times, on an Electrum connection. Closing
this needs three families the reference does not have (the Riecoin,
Primecoin and Meowcoin rows of §37.5.2, the first two to be confirmed
against the chains' published definitions) **and** a decision on how a coin
selects them, because the shared public registry cannot carry a value the
reference rejects at activation. The options, none chosen here:

1. accept Reloaded-only `chain_variant` values in addition to the eight (a
   coin file carrying one is rejected by the reference, so such a file is a
   Reloaded coin file and `docs/GLEEC_COMPATIBILITY.md` records it);
2. a separate Reloaded-only configuration key beside `chain_variant` that
   the reference ignores, so one registry entry serves both — the
   backward-compatible shape the rework itself relied on when it added
   `chain_variant` to files an older release ignored (§37.5.1 vintage
   note);
3. no new configuration: recognise the three layouts structurally under the
   default family, each confirmed by exact byte consumption (R37.5.6) —
   simplest for operators, but it reintroduces version-free inference for
   exactly the families whose lengths collide least, and cannot read the
   Meowcoin mixed list without a per-header AuxPoW-or-trailer decision.

Whichever is chosen, the registry's `"BTC"` on `RIC` and `MEWC` should be
raised with the registry's maintainer as a mis-description, since it makes
the reference return a wrong median silently for both coins.

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
- *Scoped addition, and its Dirty Gate clearance (chapter 01 T1, R40,
  R43) — the chain-variant pass of 2026-10-24.* This item is the scoped
  record for the material the chain-variant pass added or rewrote
  throughout the chapter: the `"RVN"` cell of R37.5.1's table and the
  sentence beneath the table that records its mark's removal; the three
  contract details under R37.5.1; the sharpened vintage note and the
  `MEWC` note in §37.5.1; the re-confirmation appended to the R37.5.1a
  note; the amended KawPoW, Firo MTP and PIVX rows and the new Riecoin,
  Primecoin and Meowcoin rows of §37.5.2; the served-shapes observation
  beneath R37.5.2a and the addition to the §37.5.2 divergence note; the
  rewritten R37.5.5; the within-family extension of R37.5.6; the four
  amended cells of §37.5.7b's table and the narrowed residual note there;
  three acceptance criteria in §37.8; the closing bullet of §37.9 D1, the
  parenthetical in D2 and the whole of D3; and this item. That material
  was authored by the KDF Spec Reader under the two-team procedure of
  chapter 01 §1.4A and **passed the KDF Dirty Gate** on 2026-10-24: PASS,
  discretionary-expression measure 0.02, finding list empty, and
  accordingly no remedy class for any line or for the candidate as a
  whole. That was the first gate pass on this material; no earlier
  verdict for it exists or was relayed. **Only that added and rewritten
  material was gated.** Its clearance is therefore **complete and not
  pending**, and R40 withholds nothing here: the point-of-appearance
  markers that R40 required while the clearance was pending have been
  removed, and the two clearance records above are unchanged by this pass
  and keep their own dates and scopes — they neither cover this material
  nor are reopened by it. This item is written on the clean side from the
  verdict, which reached the clean side as relayed text rather than as a
  gate-written file; that is record-keeping and adds no chapter content.

  *Which parts carry an inline pointer to this item.* The block at the
  head of §37.5, where the pending notice stood, now carries the
  clearance statement. The rule-bearing items the pass added or rewrote —
  R37.5.5 (rewritten), the within-family extension of R37.5.6, §37.9 D3,
  and the three new rows and the served-shapes observation of §37.5.2 —
  each carry a short pointer back here, converted where the pending
  markers stood. Those are numbered, rule- or layout-bearing text an
  implementer codes from, inserted among rules that have stood in this
  chapter since earlier revisions, which is the case in which
  long-settled requirement and newly-cleared requirement are easiest to
  confuse. The informative notes of §37.5.1 and §37.5.2, the §37.5.7b
  table cells and residual note, the §37.8 criteria and the D1–D2
  amendments carry a short label naming the pass but no pointer: each is
  reached by reading forward from a pointed section, or is derivative of
  a rule a pointed item binds; §37.9 is in addition headed by the
  clearance block of the earlier pass, and D3 carries its own pointer.

  *One classification the gate recorded, carried here because it bears
  on this record's own wording.* The ticker-to-value mapping in §37.5.1's
  vintage note was classified by the gate as coin-file configuration data
  under R11's externally-visible inter-operability carve-out — public
  tickers mapped onto the closed value set of R37.5.1, whose correctness
  is fixed by the chains' header layouts and whose deployed form is the
  public registry (R4) — and was excluded from the measure on that
  ground; the scoped record below lists it as R4 on the same footing, and
  the chapter's R4 footer item already names the registry as the
  authoritative source. The gate also recorded that this item's
  reference to "the four pull requests the plan under `docs/plans/`
  names" keeps change-request numbers out of the chapter: paired with a
  section scope such a number is a locator into forbidden-class material
  under §1.7, so the numbers stay confined to the process document and
  the wording here is kept as it is.

  *Forbidden-class input taken, and under which designations.* The
  reference's UTXO header codec, the configuration-reading step of its UTXO
  coin builder, its Electrum-backed header read surface and both header
  stores, read at the release designations `v2.6.0-beta` and
  `v3.0.0-beta`, together with the designation-to-designation changes that
  introduced the configuration key between `v2.5.2-beta` and
  `v2.6.0-beta` and the published coin-file migration guidance that
  accompanied them. **The record is no more precise than those
  designations** (chapter 01 §1.7). Behavioural facts taken, all restated
  in the chapter's own words: that at `v2.5.2-beta` the key is not read
  and the family is ticker-keyed; the migration guidance's ticker-to-value
  mapping (coin-file data, R4); that the key is read from the raw
  protocol object beside `type` with no protocol-type default and an
  exact string match; that an unrecognised value fails the configuration
  step and surfaces through the coin-build failure class; that the family
  is carried into both RPC backends and both stores, that the native
  backend parses no header bytes, and that nothing merged between the
  rework and `v3.0.0-beta` touched the two family-agnostic single-header
  paths; that no read path is keyed on configured time or height; and that
  the four pull requests the plan under `docs/plans/` names changed no
  identity-hash or stored-bytes behaviour (§37.5.7, §37.9 D2 stand).

  *Permitted-input classes added by this pass.* R6, public-network
  observation on 2026-10-24 of the Electrum servers the public coin
  registry (snapshot of 2026-10-18) configures for `MEWC`, `RVN`, `EVR`,
  `AVN`, `XNA`, `FIRO`, `PIVX`, `LBC`, `RIC`, `XPM`, `RUNES` and `VRSC`:
  per-header byte counts and version values at the heights §37.5.2 and
  R37.5.2a name, by `blockchain.block.header` and
  `blockchain.block.headers` without a checkpoint height. R3, the
  published node sources of PIVX (header version thresholds), Ravencoin
  (mainnet switch time), Firo (MTP start block, ProgPoW switch time and
  block) and Meowcoin (AuxPoW-first, then time-switched header rule); the
  Riecoin and Primecoin layouts in §37.5.2 were decoded from served bytes
  only and are marked for confirmation. R4, the public coin registry's
  `chain_variant` values and the registry's own revision history for the
  `MEWC` entry. §37.9 D3 and the rewritten R37.5.5 are this chapter's own
  reasoning over those facts.

  The three elements of the R39 scope attestation for that pass, carried
  forward here in corpus-free terms as chapter 01 T1 requires. Two gaps
  in the first element are recorded as gaps rather than completed from
  elsewhere:

  - *Extent.* The forbidden-class material corresponding to the subject
    matter this chapter's §37.5 names in its own terms: the reference's
    UTXO block-header codec and its chain-variant configuration surface —
    the header reader, the chain-variant value parser, the
    coin-configuration step of the UTXO coin build, the Electrum-backed
    header read paths, the native-daemon median-time-past path and both
    header stores. Quantity examined: **nine files** of that surface,
    compared at the release designations `v2.6.0-beta` and `v3.0.0-beta`
    and taken at the lineage's current state, which was confirmed
    unchanged on this surface since `v3.0.0-beta`; **one file** carrying
    the pre-rework ticker-keyed selection, at `v2.5.2-beta`; the merged
    rework change that introduced the configuration key, by its file
    inventory (**22 files**) and its published description; and the
    published release change lists at `v2.6.0-beta` and `v3.0.0-beta`.
    Reference designations: the publicly published release designations
    `v2.5.2-beta`, `v2.6.0-beta` and `v3.0.0-beta`, which the lineage's
    own releases carry and which chapter 01 §1.7 admits as public-release
    facts. **The record is no more precise than those designations** — no
    post-anchor commit identifier, no branch state and no location within
    the compared material is given (chapter 01 R11 and the
    reference-version limit of §1.7). The public coin registry and the
    Electrum-server observations are permitted-class input (R4, R6) and
    were not part of the corpus comparison.

    **Two gaps in this element, left visible rather than completed.**
    First, the comparison also reached the rework's merge state and the
    lineage's current unreleased tip; both are branch states, which
    chapter 01 R11 and §1.7 withhold, so the record can say only that the
    tip is unchanged on this surface since `v3.0.0-beta` and that the
    rework landed between `v2.5.2-beta` and `v2.6.0-beta` — nothing
    finer. Second, `v2.5.1-beta` was not compared: nothing in this
    clearance attests to any designation earlier than `v2.5.2-beta`, and
    the vintage note's statement about the pre-rework reading rests on
    that one designation. Chapter 01 T1 is satisfied by saying so — a
    completed clearance statement carries the three elements **or**
    states that an element is not recorded.

  - *Method.* A stated **combination**: by reading, together with
    mechanical history queries over the lineage's history — file-set,
    tag-containment and content searches — and the public hosting
    interface for the rework's published description. **This
    repository's own residual-similarity gate tooling was not run:** it
    compares embedded code blocks and the candidate embeds none, so the
    instrument had no operand. The absence is stated so that it is
    visible rather than implied.

  - *Measure.* **0.02** on the discretionary-expression basis — the same
    figure the leak report carries, not a second one — read against the
    standard that any single line carrying discretionary expression fails
    the candidate however much legitimately dictated content surrounds
    it; no such line was found, and no aggregate match-density judgment
    was used or admitted. Dictated-interop and public-surface tokens —
    request-method and field names, coin-file key names and values, wire
    error tokens, public chain constants and published release
    designations — were excluded from the measured set before the
    judgment was made, per token under the forced-versus-free test.
