# Chapter 29 -- Treatment of License Conditions (e) and (f)

**Status:** legal-position.

The chapter records this project's legal position on the additional
copyright-holder conditions the upstream project appended to its
license after the chapter-02-anchored baseline date, and how the
post-baseline portion of this project relates to them.

## 29.0 Executive Summary

A short time after the baseline (see
[Chapter 02](02-baseline-state.md)), the upstream project relicensed
its codebase. As part of that relicensing it published two
restrictions, identified in the upstream license document as
"Condition (e)" and "Condition (f)". They name a small set of code
regions that consumers of the relicensed tree are told not to modify:
the DEX-fee receiver public key, the DEX-fee constants surrounding it,
the Z-coin shielded-fee outgoing viewing key, and the fee-emission
path that historically lived at `lp_swap.rs` line 504.

This chapter records how the post-baseline portion of this project
relates to those two conditions. The position is summarised by three
statements and developed below:

1. **The two conditions do not bind this project as a matter of law.**
   Conditions (e) and (f) are clauses of the upstream license that
   covers material produced upstream *after* the baseline. This
   project inherits only the baseline tree, which was distributed
   under GPL version 2; it has incorporated no post-baseline upstream
   material (the prohibition in
   [01-clean-room-rules.md §1.4](01-clean-room-rules.md#14-bound-forbidden-input-classes)).
   The post-baseline conditions therefore have no contractual reach
   into this tree.

2. **The values the two conditions point at are preserved
   byte-identically in this tree, for three independent reasons.**
   First, both constants existed in the GPLv2 baseline tree (see
   [§29.1.2](#2912-the-values-exist-in-the-baseline)) and are
   inherited under that license. Second, the same values are
   wire-format interoperability constants of the swap protocol --
   the DEX-fee receiver public key and the Z-coin shielded-fee
   outgoing viewing key are values that every participant in the
   live atomic-swap network must agree on. Third, this same
   byte-identical preservation happens to satisfy Condition (f),
   but that satisfaction is a consequence of the first two reasons,
   not the reason itself.

3. **The fee-routing logic those values feed has been independently
   re-expressed.** The post-baseline DEX-fee path in this tree is
   written from scratch in a small dedicated module
   ([`mm2src/mm2_main/src/lp_swap/dex_fee.rs`](../../mm2src/mm2_main/src/lp_swap/dex_fee.rs))
   plus a per-network configuration crate
   ([`mm2src/mm2_net_config/`](../../mm2src/mm2_net_config/)) that
   resolves the fee parameters by network identifier. Production code
   no longer reaches the baseline-era `common::DEX_FEE_*` globals; it
   resolves the same facts through the pluggable configuration
   surface.

These three statements together are this project's "treatment of
Conditions (e) and (f)": the constants are preserved because the
protocol requires them to be; the logic that consumes them is the
project's own work; the post-baseline upstream regions named by the
two conditions are not the source from which any of that work
derives.

The chapter is filed under the document set's clean-room rules
(see [Chapter 01 §1.8](01-clean-room-rules.md#18-bound-missing-derivation-discipline)
and [§1.7](01-clean-room-rules.md#17-bound-per-chapter-shape): the methodology
treats high-risk regions as requiring an explicit chapter even when
no legal constraint forces one) even though, as just noted, the two
conditions are not a legal constraint on this tree. The reason is that the regions they name are exactly
the regions where casual modification or casual copying would cause
the largest interoperability and legal-risk surface. Documenting how
the tree behaves around those regions, and why, is part of the
project's audit trail.

## 29.1 Reproduction Detail

### 29.1.1 What the two conditions say

The two clauses are reproduced here in summary. The authoritative
text is the upstream license document itself; it is not reproduced
verbatim in this chapter because verbatim reproduction is not
necessary for the chapter's purpose.

**Condition (e)** names a class of code regions defined by a comment
marker or by an identifier prefix (`Dex_Fee`, `DEX_FEE`, and case
variants thereof) together with "software logic related to" them. It
restricts modification of any region that meets the definition.

**Condition (f)** enumerates four specific code locations by historical
upstream URL and line number. The four loci are:

- the DEX-fee receiver public key constant in `common/common.rs`
  (originally line 164);
- the supporting initializer that decodes that constant (originally
  line 166);
- the `DEX_FEE_OVK` constant in the Z-coin module
  (`coins/z_coin.rs`, originally line 97);
- the fee-emission call path in the swap module (`lp_swap.rs`,
  originally line 504).

Both clauses are clauses of the post-baseline license. They first
appear in the upstream tree at the relicensing commit; they were not
present in the baseline tree at `c1d46c0`.

### 29.1.2 The values exist in the baseline

The two constants are not novel to the post-baseline upstream tree.
`git show c1d46c0:mm2src/common/common.rs` confirms that
`DEX_FEE_ADDR_PUBKEY` was already present in the baseline (line 157
in the baseline tree); `git show c1d46c0:mm2src/coins/z_coin.rs`
confirms the same for `DEX_FEE_OVK` (baseline line 120). Both come
into this tree as part of the inherited GPLv2 corpus, are covered by
the inherited license, and are not novel post-baseline material.

The fee-emission call path is similarly inherited. The baseline
filename was `mm2src/mm2_main/src/lp_swap.rs` (verified with
`git ls-tree -r c1d46c0 -- mm2src/`); the post-baseline upstream
URL in Condition (f) points to an older upstream path
(`mm2src/lp_swap.rs`), reflecting an earlier upstream layout. The
line-504 reference is therefore a historical pointer rather than a
present-day file address.

### 29.1.3 Where the values live in the current tree

The two value constants are reachable today through the same module
paths they used at the baseline:

- [`mm2src/common/common.rs`](../../mm2src/common/common.rs) defines
  `DEX_FEE_ADDR_PUBKEY` (current line 221) and a supporting
  initializer that decodes it to bytes
  (`DEX_FEE_ADDR_RAW_PUBKEY`, current line 229). Both carry doc
  comments marked `DEPRECATED` that redirect callers to
  `mm2_net_config::NetConfig::dex_fee_addr_pubkey()` /
  `dex_fee_addr_raw_pubkey()` for production use.

- [`mm2src/coins/z_coin.rs`](../../mm2src/coins/z_coin.rs) defines
  `DEX_FEE_OVK` (current line 282, exact bytes `[7; 32]`, exact type
  `OutgoingViewingKey`, exact module path) and is preceded by a
  multi-paragraph doc comment (lines 263-281) that records the two
  independent reasons the value is frozen: the cross-implementation
  audit convention on the ARRR shielded-fee path, and continuity of
  the value, type, and source location inherited from the shared
  GPLv2 baseline.

The values are byte-identical to the baseline. They are not modified
by any post-baseline commit; `git log c1d46c0..HEAD --
mm2src/common/common.rs mm2src/coins/z_coin.rs` shows surrounding
restructuring (deprecation notes, doc comments, module
reorganization) but no edit that changes the constant bytes
themselves.

### 29.1.4 The fee-routing logic is independently re-expressed

Production fee resolution does not run through the deprecated
globals. It runs through two post-baseline crates:

- [`mm2src/mm2_net_config/`](../../mm2src/mm2_net_config/) -- a small
  crate whose job is to bind each supported network identifier to
  its fee parameters. It exposes a `NetConfig` trait, one
  implementation per supported network
  (`netid_6133.rs`, `netid_8762.rs`), a global lookup
  (`net_config_for(netid)` returning `Option<&NetConfig>`), a
  panic-on-missing variant (`net_config_or_panic(netid)`), and a
  compile-time constant `SUPPORTED_NETIDS` enumerating the network
  identifiers the build accepts. Boot-time activation in
  [`lp_native_dex.rs`](../../mm2src/mm2_main/src/lp_native_dex.rs)
  rejects any configured `netid` that is not in
  `SUPPORTED_NETIDS`, so production binaries cannot accidentally
  operate on a network whose fee parameters have not been
  registered.

- [`mm2src/mm2_main/src/lp_swap/dex_fee.rs`](../../mm2src/mm2_main/src/lp_swap/dex_fee.rs)
  -- a small dedicated module (around 250 lines) holding the
  post-baseline fee-emission logic. It exposes a `DexFee` value type
  and the helper functions the V1 and V2 swap state machines call to
  compute fee amounts. The module was authored in this tree as a
  carve-out; the equivalent logic at baseline lived inside the
  monolithic `lp_swap.rs` file. [Chapter 8](08-fee-routing-engine.md)
  binds `dex_fee.rs` and the `DexFee` descriptor as substrate in
  full; this chapter discusses the module only insofar as it bears
  on the two preserved constants.

The dependency direction is:
`maker_swap`, `taker_swap`, `maker_swap_v2`, `taker_swap_v2`,
`swap_watcher` (all in
[`mm2src/mm2_main/src/lp_swap/`](../../mm2src/mm2_main/src/lp_swap/))
call `dex_fee.rs`; `dex_fee.rs` and these five swap-state-machine
modules read the fee address and related parameters through
`mm2_net_config::NetConfig`. None of these five modules reads the
deprecated `common::DEX_FEE_*` constants in production. Those
constants survive in `common.rs` so that the public Rust surface
promised by the baseline crate continues to resolve, and so that
protocol-level interoperability with peers running baseline-era
binaries remains exact.

**Code-quality finding (informative).** One coin-layer consumer
below `dex_fee.rs` still reads the deprecated global directly rather
than resolving it through `NetConfig`: the unspent-transaction-output
dex-fee output builder for the `DexFee::Standard` case, part of the
version-two unspent-transaction-output swap path
([Chapter 15](15-swap-v2-utxo-path.md)) under
`mm2src/coins/utxo/`, resolves the fee-recipient public key from the
deprecated `common::DEX_FEE_ADDR_RAW_PUBKEY` global instead of from
the active network's `NetConfig`. Because that global carries the
netid-`8762` value, an unspent-transaction-output version-two swap
running on netid `6133` would build its standard dex-fee output
against the netid-`8762` fee address rather than netid `6133`'s
distinct fee address -- the same class of defect the `NetConfig`
carve-out described above exists to prevent, and a wire-compatibility
break for any netid-`6133` counterparty validating that output. The
fix is to thread the active `NetConfig` (or the resolved fee-recipient
bytes) into that coin-layer function the same way the EVM and
Tendermint fee-output builders and the five `lp_swap` modules above
already do, rather than reading the deprecated global.

### 29.1.5 Why the constants are preserved

The reasons the two constants are preserved byte-identically in this
tree are not legal reasons. They are protocol-engineering reasons:

- The DEX-fee receiver public key identifies the address all
  participants in the swap protocol agree to route fees to. A
  participant who used a different value would never have their fees
  recognized by counterparties; their swaps would be rejected.

- The Z-coin shielded-fee outgoing viewing key
  (`OutgoingViewingKey([7; 32])`) is the value by which any party --
  the maker, the taker, the auditor, an external observer -- can
  decrypt the memo on a shielded fee output and confirm that the fee
  was paid to the agreed address. A participant who used a different
  OVK would emit fee outputs that nobody else could verify; their
  taker-side swaps would be rejected by makers who could not audit
  the fee.

Both values are therefore wire-format constants in the same sense as
a magic byte in a serialization format or a fixed key derivation
constant in a hardware-wallet protocol. They are inherited under the
baseline license and are not redrafted by any post-baseline commit.

### 29.1.6 Why the logic is independently re-expressed

The reasons the fee-routing *logic* in this tree is independently
authored are likewise not primarily legal reasons. They are
clean-room-methodology reasons (see
[Chapter 01](01-clean-room-rules.md)):

- The baseline-era `lp_swap.rs` file was the single largest module
  in the inherited tree. Carrying it forward unchanged would have
  prevented the swap-version negotiation and the V2 state-machine
  redesign described in
  [Chapter 13](13-swap-version-negotiation.md) and
  [Chapter 14](14-state-machine-runtime.md). The split into
  per-side, per-version modules under
  [`mm2src/mm2_main/src/lp_swap/`](../../mm2src/mm2_main/src/lp_swap/),
  and the carve-out of fee computation into `dex_fee.rs`, were
  driven by those redesigns.

- Concentrating fee-resolution behind a single `NetConfig` trait was
  driven by the multi-network requirement described in
  [Chapter 06](06-network-id-seed-node.md). Each supported network
  has its own fee receiver address; that fact does not fit a
  single-value global constant.

The legal-risk picture follows from the engineering: post-baseline
fee logic is written from the public protocol description, the
baseline GPLv2 source, the per-network configuration values that
each network's operators publish, and nothing else. The
clean-room-rules forbidden-input rule
([01-clean-room-rules.md §1.4](01-clean-room-rules.md#14-bound-forbidden-input-classes))
applies here as it applies everywhere in the tree; the post-baseline
upstream `lp_swap.rs` is not consulted.

### 29.1.7 The audit trail

A standing internal engineering audit (maintained outside the
published document set) exists
independently of this chapter and applies the four legal lenses
(functional vs expressive, constant vs logic, Condition (e)/(f),
toxicity check) to every post-baseline file in the tree. Its job is
to answer a forensic question this chapter raises but does not
itself prove: *is the byte-identical preservation of the inherited
constants a carryforward of the post-baseline upstream
implementation, or is it independent preservation of the same
protocol facts*? The audit's present record on the loci this
chapter discusses is:

- the Condition (e) loci (regions reachable from `DEX_FEE`-prefixed
  identifiers) are routed through `mm2_net_config` in production,
  with the deprecated globals retained only as a public Rust surface
  marker;
- the Condition (f) named loci (the two value constants and the
  fee-emission path) are preserved byte-identically where they are
  values, and independently re-expressed where they are logic;
- the toxicity check found and removed one stray comment that
  attributed behaviour to a post-baseline upstream party, replacing
  it with a factual design-intent note. The specific commit is
  tracked in that internal audit's own record, not reproduced here as
  a hash: this document's project history was checked directly (full
  and unreachable-object log search) and does not contain a commit
  matching the hash this chapter previously cited, so citing it here
  would assert a false sense of independent verifiability this
  document cannot back up.

The audit conclusions, the package-by-package classifications, and
the standing methodology are recorded in that internal engineering
audit. Those records are operational, not normative
documentation; they are maintained outside the published document
set. This chapter records
their existence and what they observe about the loci it covers.

### 29.1.8 What this chapter does not claim

To avoid overstating the legal posture:

- This chapter does not claim that Condition (e) or Condition (f)
  bind this project as a matter of contract. They do not; the
  project never accepted the post-baseline upstream license.

- This chapter does not claim that preserving the inherited
  constants byte-identically is a compliance act with respect to
  Conditions (e) and (f). It is a protocol-interoperability act with
  respect to the live atomic-swap network. The preservation
  satisfies Condition (f) as a side effect of doing the right
  engineering thing.

- This chapter does not claim that the project has independently
  obtained legal review of the position it describes. The internal
  engineering audit is an engineering-side record; counsel
  review is recorded there as an open item.

- This chapter does not enumerate every `DEX_FEE`-prefixed
  identifier in the current tree. The set of such identifiers
  changes as the source evolves; the binding statement is the
  three-part position in [§29.0](#290-executive-summary), not a
  symbol-table snapshot.

## 29.2 External References

- The upstream license document published with the post-baseline
  upstream tree. Reachable through the upstream repository at the
  relicensing commit; this chapter does not pin a specific
  upstream-tree path because the project's clean-room rules
  ([§1.4](01-clean-room-rules.md#14-bound-forbidden-input-classes)) treat
  post-baseline upstream material as a forbidden input. The license
  document is referenced through its observable existence on a
  public repository.

- BIP-32 / SLIP-21 background for hierarchical key derivation; not
  directly used by this chapter but cited in adjacent chapters
  ([05](05-hd-wallet-support.md)) because the internal engineering
  audit reasons about whether key-derivation constants are
  authorial choices or protocol facts.

- The Zcash shielded-transaction protocol description (the Zcash
  Protocol Specification, current and historical versions) for the
  meaning of an OutgoingViewingKey. This chapter does not reproduce
  the protocol text; it relies on the protocol's existence to make
  the interoperability argument in
  [§29.1.5](#2915-why-the-constants-are-preserved).

## 29.3 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit `c1d46c0c1592faa0860f704008b2b2381bc3840f` of chapter 02
  (the two value constants verified present at baseline via
  `git show c1d46c0:` of `mm2src/common/common.rs` and
  `mm2src/coins/z_coin.rs`; the baseline filename for the historical
  `lp_swap.rs` verified via `git ls-tree -r c1d46c0 -- mm2src/`); the
  present-day working tree, re-verified for this revision by `grep`
  for `DEX_FEE_ADDR_PUBKEY`, `DEX_FEE_OVK`, `SUPPORTED_NETIDS`,
  `net_config_or_panic`, and `dex_fee_standard_output`, and by
  directory listing of `mm2src/mm2_net_config/src/`; chapter 02 (the
  pinned baseline anchor); chapter 08 (the `dex_fee.rs` / `DexFee`
  substrate this chapter discusses); chapter 06 (the network-identity
  and `NetConfig` registry the preserved constants are resolved
  against in production).
- *Permitted-input classes used:* the baseline itself (chapter 01
  R1); the present-day working tree, quoted as evidence under the
  legal-position carve-out of chapter 01 R11.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* not consulted. Citations to the internal
  engineering audit refer to an operational record maintained outside
  the published document set. The chapter does not quote from the
  post-baseline upstream license document; the two clauses are
  summarised, not reproduced verbatim.
