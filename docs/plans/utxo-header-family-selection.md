# Plan: configuration-selected UTXO header families

> **Status:** proposed 2026-09-23. Nothing below is implemented yet.
>
> This plan closes the open work left by the fix for standard headers whose
> version collides with a header family (commit `47ede628c`, CRD ch.37
> §37.5.8). That fix made header reads consume the served bytes exactly and fall
> back to the standard 80-byte layout, which removed the observed failure. It
> did not give the reader what ch.37 §37.5 actually requires: a header family
> chosen per coin from its configuration. Four open items remain, and they
> depend on each other:
>
> 1. ch.37 §37.9 **D1**: accept `protocol.chain_variant` and carry the family
>    into every header read path.
> 2. ch.37 §37.9 **D2**: identity hashes and stored bytes from the served bytes
>    (including the 80-byte AuxPoW hash scope), and the linkage check.
> 3. The open `SPEC` note in ch.37 §37.5.7: chain rules that are not yet
>    confirmed.
> 4. The behaviour change of the fix: a header list with leftover bytes is now
>    rejected, so coins whose headers we cannot parse (LBRY, PIVX) now get an
>    error from median-time-past where they used to get a wrong value.

## How the four items connect

The header family is the root. It decides the byte layout (item 1), and the
layout decides where one header ends and the next begins, and so which bytes
make up a header's identity (item 2). For some families the layout, and the
identity hash, depend on chain rules that are not confirmed yet (item 3). Item 4
is a symptom of item 1: LBRY and PIVX headers fail only because no family
exists for them. It needs no fix of its own; it goes away when those families
are parsed.

So the order is: confirm the rules (item 3) → add the family and thread it
(item 1) → parse the missing families (which closes item 4) → switch identity
and storage to served bytes (item 2).

## Current state (verified 2026-09-23)

- **Reader hint.** `serialization::CoinVariant` (kdf_codec) has two values,
  `Standard` and `Qtum`. It derives only `Debug`, so it cannot be copied into
  several reads.
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
- **Stores.** Both stores persist `header.raw()`, which is the re-encoded
  header. `get_block_height_by_hash` compares `header.hash()`, the
  double-SHA256 of the re-encoding, so for AuxPoW headers the hash also covers
  the proof.
- **Existing helpers.** `RawBlockHeader` (kdf_chain) already wraps served bytes
  and hashes them. The SPV proof path already carries a `RawBlockHeader`
  alongside the parsed header.

## Goal

Every header read of a UTXO coin parses with the family its configuration
names (ch.37 R37.5.1, R37.5.1a). Explicit families are exclusive (R37.5.3).
KawPoW is read only for `"RVN"` coins (R37.5.4). LBRY, PIVX, Peercoin and
RICK/MORTY headers parse (R37.5.2). Identity and storage come from the served
bytes (R37.5.7), and header lists are checked for linkage where the family's
hash allows it (R37.5.6). None of this may change behaviour for a coin whose
configuration carries no `chain_variant` and whose headers already parse today,
apart from the KawPoW withdrawal, which is gated in step H4.

## Design

### H0 — Confirm the chain rules (item 3; dirty side)

Dispatch KDF Spec Reader to resolve the `SPEC` note of ch.37 §37.5.7, using
public chain definitions:

- PIVX header-version thresholds;
- Firo MTP start and ProgPoW switch rules;
- KawPoW activation time per network for each `"RVN"` coin (RVN, AIPG, AVN,
  XNA, EVR may differ);
- the KawPoW, Firo and Verus identity hashes;
- the header shapes Electrum servers serve for those families.

Gate the result with KDF Dirty Gate. Where a rule cannot be pinned, the chapter
keeps the upstream version key as the interim reading (R37.5.5), and H4 uses
that.

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
| `Rvn` | Standard before the KawPoW switch, KawPoW after |
| `Pivx` | Sapling root after the nonce |
| `KomodoAssetChain` | Equihash for every block, genesis included (RICK/MORTY) |

The type becomes `Clone + Copy + PartialEq`. The fallback in
`from_served_bytes` keys on "family is `Default`" instead of "not Qtum".

### H2 — Read the key at activation

`UtxoConfBuilder` reads `conf["protocol"]["chain_variant"]` from the raw coin
JSON. This leaves the serde shape of `CoinProtocol` unchanged, so nothing that
serializes it changes. Mapping:

- absent → `Default`, except for the QTUM/QRC20 protocol types, which map to
  `Qtum` as today;
- a known string → its family;
- an unknown string → activation error.

Store the result as `UtxoCoinConf::header_family`. ZHTLC and BCH build their
conf through the same builder, so check both.

### H3 — Carry the family everywhere

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

This step changes no parse result for existing configurations, because every
coin still resolves to `Default` or `Qtum` until H4. Its test is plumbing
coverage: each path is exercised with a non-default family.

### H4 — Family rules (closes item 4)

Implement R37.5.2–R37.5.5 in the header deserializer:

- `Btc`/`Ppc` exclusivity;
- the `Lbc`, `Pivx` and `KomodoAssetChain` layouts;
- KawPoW only under `Rvn`, by the H0 switch rule, or by the version key if H0
  could not pin it.

Last, withdraw version-only KawPoW detection under `Default`. The registry
survey shows no registry coin depends on it. A hand-written configuration for
a KawPoW chain without the key would stop parsing, so call this out in the
changelog and in `docs/GLEEC_COMPATIBILITY.md` if upstream differs.

Once LBRY and PIVX parse, item 4's regression is gone. The strict list check
itself stays; it is required by R37.5.6.

### H5 — Served-byte identity and linkage (item 2)

- **Parsing:** returns the served byte range with each header. Keep
  `RawBlockHeader` beside the parsed header, as the SPV proof path already
  does.
- **Stores:** persist the served bytes. Identity is the served bytes'
  double-SHA256, and for AuxPoW it covers only the 80-byte base.
  `get_block_height_by_hash` uses that identity.
- **Linkage check:** in list reads, check each header's previous-block hash
  against the identity of the header before it, for families whose identity
  is a double-SHA256. Skip the check for KawPoW, Firo and Verus unless H0
  supplies their identity hashes.
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
| H0 spec confirmation | — | small, dirty side | chapter only |
| H1 + H2 type and config | — | small | 1 |
| H3 threading | H1, H2 | medium (≈10 call sites, both stores) | 2 |
| H4 family rules | H0, H3 | medium | 3 |
| H5 identity, storage, linkage | H0, H4 | medium, touches persisted data | 4 |

H0 and H1+H2 can run in parallel. Each PR updates ch.37 §37.5.8 and §37.9 in
the same commit as its code, and closes the corresponding D paragraph when it
lands.

## Risks

- **H4 KawPoW withdrawal:** breaks a hand-written KawPoW configuration that
  lacks the key. Mitigation: registry coverage (verified above), a changelog
  note, and an activation-time warning, rather than a silent misread, when a
  `Default` coin serves a header that parses only as KawPoW.
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
  `Default` or `Qtum` by protocol type. Each registry value maps correctly.
- **Stores:** a served-bytes round-trip on SQLite, and on IndexedDB through
  the existing wasm test harness. `get_block_height_by_hash` works for an
  AuxPoW header.
- **Linkage:** a list with a broken previous-block hash is rejected, and a
  correctly linked one passes.

## Open decisions

1. **AuxPoW stores on upgrade:** clear them and re-sync (recommended), or
   migrate the stored rows.
2. **Unknown `chain_variant` values:** reject activation, as ch.37 requires,
   or only warn for one release to protect hand-written configurations.
3. **Where the store's family lives:** in `BlockHeaderStorage` (recommended)
   or per call.
4. **KawPoW without the key:** keep version-only detection behind an explicit
   compatibility switch for a release, or remove it outright. Removing it is
   simpler and the registry does not need it.

## References

- CRD ch.37 §37.5 (R37.5.1–R37.5.7), §37.5.8, §37.9 D1–D2.
- Commit `47ede628c` (the collision fix) and its tests in
  `mm2src/kdf_chain/src/header.rs`.
- Public coin registry `coins` file: `protocol.chain_variant` usage (survey
  above).
- Electrum protocol: `blockchain.block.header`, `blockchain.block.headers`.
