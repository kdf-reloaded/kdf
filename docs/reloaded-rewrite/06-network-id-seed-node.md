# Chapter 06 -- Network-ID and Network-Configuration Registry

**Status:** driving-spec

> **One-sentence claim:** the project's notion of a "network" --
> a peer-to-peer subnetwork identified by a 16-bit numeric
> netid -- shall be expressed as a compile-time registry
> implementing a single public trait that covers every
> per-network constant (optional seed nodes, DEX-fee addresses in
> three flavours, fee rates, burn-share parameters), and the
> daemon shall refuse to start on any netid the binary cannot
> describe.

## 6.0 Executive Summary

Every peer-to-peer instance of the codebase belongs to exactly
one **network**, identified by a 16-bit numeric tag (`netid`)
supplied in the daemon's JSON configuration. Two daemons with
different netids cannot exchange swap traffic with each other:
their seed-node sets do not intersect, their DEX-fee addresses
differ, their fee-rate constants differ, and their burn
behaviour differs. The netid is the single coarse-grained
namespace that separates one operational network from another.

This chapter defines a binding shape for the source-of-truth
that backs every per-netid constant in the codebase:

1. A dedicated workspace crate provides a **network-config
   trait** and a **compile-time registry** that maps each
   supported netid to a trait object. The trait covers every
   per-network constant the codebase needs.
2. Each supported network is registered by adding a small Rust
   module that implements the trait against a zero-sized unit
   struct. Production netids are unconditionally registered;
   test-only netids are gated behind a single Cargo feature so
   release builds cannot accidentally accept them.
3. The peer-to-peer subsystem is **netid-blind**: it accepts a
   resolved list of relay addresses to dial on startup and does
   not know which netid produced that list. Production operation
   expects operators to provide reachable `seednodes` in
   `MM2.json`; compiled-in seed-node lists are optional bootstrap
   conveniences, not a precondition for a working binary.
4. The daemon **refuses to start** on any netid the registry
   does not describe. This is enforced at initialisation, before
   the peer-to-peer subsystem is brought up.
5. Section 6.6A records the **netid lifecycle of the historical
   reference implementation** — which netids it admits or refuses at
   which published release designation, and which fee schedule each
   designation carries — because §6.8 R4 and this project's
   concurrent support for two production netids are only
   interpretable against it. That section and the two rules it
   produces (§6.8 R16-R17) have passed chapter 01 R39 gate
   clearance, which §6.13 records; §6.10 D5 governs any
   restatement of them outside this document set.

The wire protocol does not encode the netid; the JSON
configuration shape is unchanged from any prior arrangement
(`"netid": u16`, `"seednodes": [string]`, `"i_am_seed": bool`).
What this chapter binds is the *source of truth* for
per-network constants: a typed, compile-time registry behind a
single trait, not scattered constants across the codebase.

## 6.1 The Network-Config Trait

A single workspace crate exposes a public trait,
**`NetConfig`** (this is the in-tree symbol name and is bound by
this chapter), with the operations enumerated below. The trait
is `Send + Sync + 'static` so trait-object handles can be held
across threads.

| Operation                          | Returns                    | Purpose                                          |
|------------------------------------|----------------------------|--------------------------------------------------|
| Netid identity                     | `u16`                      | The netid this configuration belongs to          |
| Human-readable network name        | `&'static str`             | Display / logging                                |
| **DEX-fee address (secp256k1)**    | `&'static str`             | Hex-encoded compressed-secp256k1 pubkey          |
| DEX-fee raw pubkey                 | `&'static [u8]`            | The same pubkey as raw bytes                     |
| **DEX-fee Z-address**              | `&'static str`             | The Zcash-family shielded-address variant        |
| **DEX-fee ed25519 pubkey**         | `&'static str`             | The ed25519 variant for Sia-style chains         |
| DEX-fee rate                       | `BigRational`              | Precise fee rate (no floating-point)             |
| Fee-discount ticker set            | `&'static [&'static str]`  | Tickers that receive the discounted fee rate     |
| DEX-fee discounted rate            | `BigRational`              | The discounted rate applied to the ticker set    |
| DEX-fee minimum threshold          | `BigRational`              | Floor below which the fee does not drop          |
| Burn-share enabled                 | `bool` (default `false`)   | Whether a fraction of the fee is burned          |
| DEX-fee share                      | `BigRational` (default 1)  | Fraction retained as fee (vs burned)             |
| Burn address (secp256k1)           | `&'static str` (default "")| Hex-encoded compressed-secp256k1 pubkey          |
| Burn address raw pubkey            | `&'static [u8]` (default &[])| Burn pubkey as raw bytes; also the no-fee waiver key of chapter 08 R15C / chapter 16 R7, non-empty on both production netids |
| Version-two no-fee ticker set      | `&'static [&'static str]` (default empty) | Version-two swaps with either coin in this set carry no dex fee (chapter 16 R12B); `["KMD"]` on netid 8762, empty on 6133 |
| Seed-node list                     | `&'static [&'static str]`  | Optional DNS names or address strings            |

Return-type discipline:

- Numeric identity returns are plain values.
- String and byte-slice returns are `&'static`, with the data
  interned into the binary at compile time. Hex-decoded bytes
  are produced at compile time via a `const`-decoded helper so
  there is no runtime parse cost and no possibility of a parse
  failure at runtime.
- Rate returns use a big-rational type so DEX-fee math never
  loses precision.
- Burn-related methods carry default implementations that
  produce the "burn-disabled" answer; networks that do not burn
  do not need to spell those methods out.

The DEX-fee address is exposed in **three flavours** because the
codebase serves chains with three distinct address-encoding
families (Bitcoin-family compressed secp256k1, Zcash-family
shielded, Sia-style ed25519). A single network has a single
DEX-fee identity expressed three ways; consumers select the
flavour appropriate to the chain being charged.

## 6.2 The Registry

The same crate exposes two registry entry points:

| Function                           | Return                          | Purpose                                              |
|------------------------------------|---------------------------------|------------------------------------------------------|
| **`net_config_for(netid: u16)`**   | `Option<&'static dyn NetConfig>`| Lookup; returns `None` for unknown netids            |
| **`net_config_or_panic(netid)`**   | `&'static dyn NetConfig`        | Lookup; panics with a descriptive message on failure |

Both names are the in-tree symbol names and are bound by this
chapter.

The lookup is implemented as a single `match` over the netid
value, with one arm per registered network returning a static
reference to a unit-struct trait object. The returned trait
object is a fat pointer to a zero-sized unit struct living in
`'static` storage; there is no heap allocation, no
synchronisation cost, no shutdown ordering issue. The cost of
looking up a network's parameters at runtime is one match arm
and one indirect call.

The descriptive panic emitted by the second entry point shall
list every netid the binary was compiled to support, so that an
operator who supplies an unsupported netid is informed which
netids would be accepted.

## 6.3 Compile-Time Registration of a Network

Each supported network is registered by a small Rust module
inside the crate. The module convention is:

| Element                       | Shape                                                  |
|-------------------------------|--------------------------------------------------------|
| File name                     | `netid_NNNN.rs` where `NNNN` is the netid              |
| Public type                   | One unit struct, `pub struct NetidNNNN;`               |
| Implementation                | One `impl NetConfig for NetidNNNN { ... }` block       |
| Constants                     | All per-network values appear in this one impl block   |
| Crate registration            | One match arm in `net_config_for` returning `&NetidNNNN`|

The unit-struct + per-module convention is binding: it keeps
each network's constants in exactly one file, ensures there is
exactly one statically-known instance per network, and lets the
registry's match arm return a `&'static dyn NetConfig` without
any allocation.

Adding a new network is therefore three changes in one crate:
the new file, the new use, the new match arm. No file outside
this crate needs to change.

## 6.4 Production and Test Netid Separation

The codebase distinguishes **production** netids from
**test-only** netids:

- Production netids are registered unconditionally; they are
  always present in any build of the daemon.
- Test-only netids are registered behind a single Cargo
  feature, **`regtest-netid`** (this is the in-tree feature
  name and is bound by this chapter). The release build of the
  daemon does not enable this feature.

This separation is binding: it is not acceptable to register a
test netid unconditionally, and it is not acceptable to gate a
production netid behind a feature flag. The intent is that the
release binary cannot be tricked into accepting a test-only
netid through a configuration alone; the cost of doing so is a
rebuild with a non-release feature enabled.

The test netids are collected in a single Rust module that
exists only when the feature is enabled; the production netids
each have their own module. A single macro inside the test-
netid module produces one unit struct, one impl block, and one
constructor call per test netid, keeping the test-netid set
short and uniform.

## 6.5 Peer-to-Peer Netid-Blindness

The peer-to-peer subsystem (covered in detail in
[Chapter 28](28-libp2p-modernization.md)) is **netid-blind**:

R1. The peer-to-peer subsystem shall not accept a netid
    parameter on any of its public entry points.
R2. The peer-to-peer subsystem shall not store a netid in any
    of its state structures.
R3. The peer-to-peer subsystem shall not contain any per-netid
    branch on a numeric netid value.
R4. The peer-to-peer subsystem shall not carry an in-source
    list of seed-node addresses for any specific netid.
R5. The peer-to-peer subsystem shall accept its bootstrap
    seed-node list as a parameter at startup; the caller is
    responsible for having resolved that list from operator
    configuration and, only when appropriate, the network-config
    registry fallback.

R1-R5 together mean the peer-to-peer subsystem can be compiled
and tested without knowing any netid at all. The "what network
am I on" question is answered exclusively by the caller's
selection of which network-config trait object to consult.

The pub/sub fan-out behaviour at the peer-to-peer layer is
**not netid-conditional**: the codebase shall use the
unconditional more-permissive variant of the fan-out behaviour
across all networks. There is no per-network branch on whether
the well-known fan-out variant applies.

## 6.6 Daemon Startup Guard

The daemon initialisation path shall validate the network before
it resolves bootstrap relays or starts the peer-to-peer subsystem:

1. Read `netid` from the daemon's JSON configuration, treating a
   missing value as `0`.
2. Resolve that value through the network-config registry. If the
   binary does not describe the selected netid, startup is
   refused before seed-node resolution and before peer-to-peer
   startup.
3. Use the returned network configuration as the source of truth
   for every per-network constant the daemon needs at startup.
   Seed nodes are an exception in priority only: an explicit
   operator `seednodes` field takes precedence over the registry
   seed-node list.

The daemon call-site contract is that bootstrap relay resolution
is complete before peer-to-peer startup is invoked. The
peer-to-peer subsystem receives a concrete list of relay
addresses to dial, not a netid, not the daemon JSON
configuration, and not a registry handle. The caller therefore
owns the precedence rules in §6.8 R9-R15, and the peer-to-peer
subsystem remains netid-blind under §6.5.

> **Upstream divergence (informative).** The historical lineage treated
> operator-provided `seednodes` as the normal bootstrap source. The active
> RELOADED branch may additionally use a network-registry fallback. This
> chapter keeps the fallback optional and requires production deployments to
> work without hard-coded production relay addresses in the binary.

## 6.6A Reference Netid Lifecycle (dictated-interop record)

> **Cleared (chapter 01 R39-R40, T1).** Section 6.6A, together with
> rules R16-R17 of §6.8, test T6 of §6.9, item D5 of §6.10 and the
> external-reference entry added to §6.11, rests on forbidden-class
> input taken through the two-team procedure of chapter 01 §1.4A and
> **has been cleared** by the chapter 01 R39 gate. The clearance, with
> the three elements of the R39 scope attestation and the one element
> that is a gap, is recorded in this chapter's provenance footer
> (§6.13), which also carries the scope this section adds to the
> *Forbidden corpus* item. R40 accordingly withholds nothing here.
>
> The clearance covers this section's text **inside this document set**.
> It authors and clears no public-facing or operator-facing text:
> restating any of this outside the document set is bounded by §6.10 D5,
> and the internal prohibition of §6.6A.6 L5 travels with every such
> draft.

### 6.6A.1 Why this chapter records the lifecycle

KDF Reloaded supports netid 8762 and netid 6133 concurrently, and §6.8
R4 refuses any netid the binary cannot describe. Neither statement is
interpretable by an operator without the external fact it answers:
which netids the historical reference implementation itself admits, at
which **published release designation** it admits them, and on what fee
terms. A netid value, an admission or refusal behaviour at startup, and
a fee ratio are network-identity and fee-arithmetic interop facts of
the kind chapter 01 R4 and R6 admit. They are recorded in this chapter,
rather than only in an operator document, so that they carry a
derivation record and a clearance state.

This section is a factual record of *external* behaviour. It attributes
nothing to the intent, motive, or decision of any party, and it states
nothing finer than designation granularity, because chapter 01 R20
withholds anything finer.

### 6.6A.2 In the reference, netid admission is a denylist

The reference implementation keeps no set of *supported* netids. At
every designation read (§6.6A.7), a configured netid passes exactly two
admission gates:

1. a **numeric bound**, which is narrower than the full unsigned 16-bit
   range because the netid is the input to the derivation of the
   daemon's listening port numbers, and a netid large enough to push
   those derived ports past the 16-bit port space is refused;
2. a **compiled-in set of retired netid values**, enumerated one value
   at a time; a netid in that set is refused.

Every other netid value is admitted, including values no operator has
ever used. The consequence that matters here is structural: in the
reference a netid becomes unusable by being *named* as retired, not by
ceasing to appear in a supported set. Two values have been named there
across the designations read: 7777 and 8762.

KDF Reloaded's registry is the inverse of that shape — a closed
allowlist (§6.2, §6.8 R2, §6.8 R4). Netid 7777 is refused in this
project because it is absent from the registry, and netids 8762 and
6133 are admitted because they are present. This project therefore has
no retired-netid concept at all, and the reference's narrower numeric
bound needs no separate implementation here, because the allowlist is
strictly narrower than it. [Chapter 45](45-startup-configuration-and-environment-tolerance.md)
R45.5.3 remains the binding rule for malformed and out-of-range
`netid` configuration values; the allowlist of §6.8 R4 is what decides
an in-range value.

### 6.6A.3 Observable behaviour when a retired netid is selected

For a netid in the reference's retired set, at the designations read:

- **Startup is refused outright.** Daemon initialisation aborts with a
  typed initialisation error that identifies the configured netid as
  retired. It is not a warning, not a degraded or read-only mode, and
  not a deferred failure that first appears when a swap is attempted.
- **The refusal is unconditional within that designation.** It does not
  depend on whether seed nodes are supplied, on whether the node is
  configured as a seed or bootstrap node, or on whether peer-to-peer
  networking is disabled by configuration. No configuration admits the
  netid.
- **No service is ever reachable.** The refusal occurs during
  initialisation and before the JSON-RPC listener is bound, so the
  daemon never enters its run loop; there is no running process to
  query, no orderbook, and no later point at which the condition
  presents itself differently.
- **The mechanism is not seed-node or configuration removal.**
  Peer-bootstrap provisioning is one of the five subject areas the
  clearance of §6.13 records as its extent, and what that extent
  supports is narrower than a sweep of the whole surface: where
  peer-bootstrap provisioning was compared, the reference provisions
  bootstrap peers from the operator's configuration rather than from a
  compiled-in list for the netid at issue. A retired netid's refusal
  therefore does not operate by withdrawing compiled-in bootstrap
  material, and it does not depend on peer availability. This record
  claims no more than that. It does not assert the absence of
  compiled-in bootstrap material at a designation at which that surface
  was not compared, nor for any netid other than the one whose
  retirement is at issue. A netid's seed-node hosts, if an operator
  still runs them, remain reachable in the network sense; what refuses
  is the binary.

What an operator observes, therefore, is that a binary of the first
designation naming a netid as retired will not start on that netid
under any configuration.

### 6.6A.4 In the reference, the fee schedule is global, not per-netid

At every designation read, the reference resolves the taker DEX-fee
rate from one compile-time pair — a base rate and a discounted rate,
the discounted rate selected by whether either side of the traded pair
is one named ticker — with **no netid input**. One build has one fee
schedule, on whichever netid it runs. The same holds for the
fee-collection identity key: it is one global value per build, not a
per-netid value. This shape is not a later development: it is already
the shape of the chapter-02 baseline tree, which is permitted input
(see the note under §6.6A.5).

This is the single most important fact for reading the designation
table in §6.6A.5, and it has two consequences:

- **No designation of the reference charges one rate on netid 8762 and
  a different rate on netid 6133.** The question "what does the
  reference charge on netid 8762" is only answerable as "what did the
  build in which netid 8762 was admitted charge".
- The netid-8762 and netid-6133 fee parameters this document set
  attributes to each network (§6.1, [Chapter 08](08-fee-routing-engine.md),
  [Chapter 16](16-swap-v2-pre-burn-output.md) §16.1.1) are each the
  **global** schedule of the designation at which that netid was the
  reference's own default. Selecting them per netid in one binary is a
  KDF Reloaded divergence (§6.1, §6.8 R17), not an inherited shape.

### 6.6A.5 Designation table

Each row states what was read at the designations named in that row.
Designations are published release designations of the historical
lineage, admitted by chapter 01 R20; no commit identifier, branch
state, or undesignated tree state is named, relied on, or implied.

| Published release designation | Reference's default netid | Netids refused as retired | Base taker-fee rate | Discounted rate, and the ticker that selects it |
| --- | --- | --- | --- | --- |
| the v1.0.x-beta line and v1.1.0 | 7777 | none | 1/777 | 9/7770, selected by `"KMD"` |
| v2.0.0-beta through v2.6.0-beta (every published designation of the v2 line) | 8762 | 7777 | 1/777 | 9/7770, selected by `"KMD"` |
| v3.0.0-beta | 6133 | 7777 **and 8762** | 2/100 | 1/100, selected by `"GLEEC"` |

**Row precision.** A row states what was read, not that every cell of
it was read at every designation it names, and this enumeration is
reconciled against the extent element of the clearance recorded in
§6.13: it accounts for every cell of the table, and for the two subject
areas the table does not carry — peer-bootstrap provisioning (§6.6A.3)
and the fee-collection identity (the note below this table) — the
enumeration is the extent element itself, which covers both in their
own right. In row 1 the default netid was read at the chapter-02
baseline and at the v1.0.7-beta designation; the two fee ratios and the
`"KMD"` selector were read at the baseline and at every v1.0.x-beta
designation and v1.1.0. In row 2 every cell was read at every published
designation of the v2 line. In row 3 every cell was read at
v3.0.0-beta. Rows 2 and 3 are therefore complete sweeps; row 1 is not,
and no fact in §6.6A.6 depends on a row-1 cell that was not read. With
that and the naming note immediately below, the read provenance of this
section is settled: no fact of §6.6A rests on a reference point the
extent element does not carry.

**One designation of the v2 line cannot be named in this record.** The
phrase *every published designation of the v2 line*, wherever this
section uses it, reaches one designation beyond the nineteen the
clearance of §6.13 names. That designation's published version string
embeds an abbreviated commit identifier, and chapter 01 R11, as §1.7
applies it to a record of this kind, bars this section and that
clearance from naming it. It was examined, and the v2-line sweep holds
at it, so the sweep is **attested in substance**; what cannot be done
within R11's naming limits is to express that one reference point here.
The gap is therefore left visible, here and in the extent element of
§6.13, rather than closed by approximating the designation — and the
sweep must not be read as unattested on account of it. Attested in
substance and unnameable in form are different things, and it is the
second that applies. The lead-in above is undisturbed: that designation
is a published release designation and not a tree state, and nothing
here names or relies on a commit identifier as a locator, a branch
state, or an undesignated tree state — which is exactly why its
designation string cannot be reproduced in this record.

The fee-collection identity key also differs between the v2 line and
v3.0.0-beta; the two values are the ones this project already records
per netid (§6.1, chapter 16 §16.1.1), and no further key material is
restated here.

**The first row rests on permitted input, not on forbidden-class
input.** The baseline tree itself — the chapter-02 anchor, commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f` of 2022-06-03 — already
carries the whole of that row: its user-facing documentation names 7777
as the main network identifier of its era
([Chapter 02](02-baseline-state.md)), it names netid 7777 as a
compile-time constant, and its fee-rate computation is already the
single global pair of §6.6A.4, returning 1/777 generally and 9/7770
when either side of the pair is the exact ticker `"KMD"`. Three facts
this section needs are therefore pre-anchor, permitted-input facts and
need no clearance: the global-rather-than-per-netid shape of the fee
schedule, the 1/777 and 9/7770 ratios with their `"KMD"` selector, and
the 7777-era default. What §6.6A adds beyond the baseline is only the
designation at which each later change becomes observable.

### 6.6A.6 Established lifecycle facts

L1. **Netid 8762 is refused by the reference as of v3.0.0-beta.** The
    refusal is a hard refusal of daemon startup with the observable
    behaviour of §6.6A.3. It is not a warning, not a late rejection,
    and not an effect of removing seed nodes or configuration.

L2. **v2.6.0-beta is the last published designation that does not
    refuse netid 8762, and v3.0.0-beta is the first that does.** Both
    designations were read directly for this fact. At v2.6.0-beta netid
    8762 is additionally the reference's own default netid.

L3. **Netid 6133 first appears at v3.0.0-beta**, as the reference's new
    default netid. It appears at no earlier published designation of the
    v1 or v2 lines. The fee schedule in force at that designation —
    base 2/100, discounted 1/100 selected by `"GLEEC"` — is the schedule
    that designation launched with; it is the only published designation
    at which netid 6133 exists, so no later change to it can be
    established at designation granularity, and none is asserted.

L4. **The netid-8762-era fee parameters never changed.** Base 1/777 and
    discounted 9/7770 selected by `"KMD"` were read at every published
    designation of the v2 line, at which netid 8762 was the default, and
    also at the v1.0.x-beta line and v1.1.0, which predate netid 8762
    and used netid 7777. They are absent from v3.0.0-beta. The span
    reaches back further than any designation read: the same two ratios
    and the same `"KMD"` selector are in the chapter-02 baseline tree of
    2022-06-03, which is permitted input. The unbroken run therefore
    covers the baseline and every published designation up to and
    including v2.6.0-beta, and it ends not with a rate change on netid
    8762 but with L1.

L5. **Netid 8762's admissibility, not its price, is what changed.** The
    reference's *global* fee schedule did change at v3.0.0-beta, from
    1/777 to 2/100. Because §6.6A.4 holds — the schedule is global and
    netid 8762 is refused at that same designation — there is **no
    published designation of the reference in which a node is able to
    run on netid 8762 while charging the 2/100 schedule**. The weaker
    statement "the fee on netid 8762 was never raised" is supported; the
    stronger statement "the fee on netid 8762 was raised to 2%" is not,
    and must not be published.

    **This prohibition is internal to the document set and travels with
    every draft taken from this section.** The clearance recorded in
    §6.13 does not discharge it and does not weaken it: that clearance
    covers this chapter's text inside the document set and clears no
    public-facing or operator-facing wording at all. Anyone drafting
    such a page from §6.6A — `README.md`, `docs/NETWORK_CONFIG.md`,
    release material, or anything else outside this document set —
    carries this prohibition into the draft, and must not state the fee
    history in the stronger form, in that wording or in any equivalent
    of it. §6.10 D5 states the rest of what such a draft owes.

L6. **The three v3.0.0-beta changes are simultaneous at designation
    granularity, not sequential.** The introduction of netid 6133 as
    the default, the naming of netid 8762 as retired, and the change of
    the global fee schedule are all first observable at the same
    published designation, v3.0.0-beta. No published designation
    sits between v2.6.0-beta and v3.0.0-beta. An account that places the
    introduction of netid 6133 in one release and the retirement of
    netid 8762 in a later release is therefore **not** supported by the
    published designations and must be corrected before publication.

L7. **Netid 8762 remains a structurally valid netid value in the
    reference.** It is within the reference's numeric bound of §6.6A.2,
    the derivation of its port numbers is unaffected, and it was not
    removed from an accepted set — there is no accepted set. It is
    refused because it is enumerated as retired, which is a
    single-value exclusion: a neighbouring never-used netid value still
    passes the netid gate of the same build that refuses netid 8762
    (whether such a node then starts depends on the unrelated launch
    preconditions, not on its netid). In that precise sense netid 8762
    is still reachable in principle: the value is structurally intact,
    its seed hosts are operator-supplied rather than compiled in, and a
    binary of any designation that does not name it as retired still
    runs on it. A binary of v3.0.0-beta does not.

L1-L7 are facts about an external implementation. They state no
requirement on this project; the requirements this project derives from
them are R16 and R17 of §6.8.

### 6.6A.7 Derivation record (cleared; §6.13 carries the clearance)

- **What needed forbidden-class input and what did not.** Three of the
  section's facts are pre-anchor, permitted-input facts drawn from the
  chapter-02 baseline tree and require no clearance: the
  global-rather-than-per-netid shape of the fee schedule (§6.6A.4), the
  1/777 and 9/7770 ratios with their `"KMD"` selector, and the
  netid-7777-era default (both in the note under §6.6A.5). Everything
  else — the designation at which each later change becomes observable,
  the netid-admission shape, the retired-netid refusal behaviour, and
  the netid-6133 parameters — was established from forbidden-class
  input taken under chapter 01 §1.4A by the dirty-side specification
  author role of chapter 01 R37. The whole section was held under R40
  until the R39 gate cleared it, including the permitted-input parts,
  because they are stated here as part of one record; the clearance is
  recorded in §6.13 and R40 now withholds nothing in the section.
- **Designations read:** the v1.0.x-beta line and v1.1.0; every
  published designation of the v2 line, from v2.0.0-beta through
  v2.6.0-beta (one of which this record cannot name — see the note
  under §6.6A.5 and the extent element of §6.13); and v3.0.0-beta.
  v2.6.0-beta and v3.0.0-beta were each read directly for every fact
  attributed to them. No fact in §6.6A is inferred from assumed
  agreement between two designations where only one of them was read.
- **No undesignated state is relied on.** Where a fact could only be
  established at a single designation, §6.6A attributes it to that
  designation alone and says so (L3).
- **Release dates**, where §6.6A or a public document needs them, are
  those the lineage's own published release notes carry: v2.6.0-beta is
  published as 2025-11-28 and v3.0.0-beta as 2026-03-06. These are
  public-release facts under chapter 01 R6.
- **Scope carried into §6.13, and where it comes from.** The record in
  §6.13 takes its scope from the **extent element of the R39 scope
  attestation**, not from this bullet, because an earlier form of this
  bullet named four subject areas and would have under-recorded the
  clearance. The two now agree, and the scope is five subject areas:
  netid admission shape, including default-netid history and the
  startup refusal a retired netid produces; the
  startup-initialisation ordering up to the point at which the public
  JSON-RPC listener is bound, which is what makes that refusal
  observable; **peer-bootstrap provisioning**, asserted at §6.6A.3's
  mechanism bullet, which the earlier form omitted; the taker DEX-fee
  rate schedule and its history; and the **fee-collection identity**,
  asserted at §6.6A.4 and at the note below the §6.6A.5 table, which
  the extent carries in its own right rather than as part of its
  fee-schedule wording. The reference points are those of §6.13's
  extent element: the v1.0.x-beta line and v1.1.0, the v2 line from
  v2.0.0-beta through v2.6.0-beta, and v3.0.0-beta, together with the
  chapter-02 baseline anchor as permitted input — with the one v2-line
  designation the note under §6.6A.5 records as examined but
  unnameable.

## 6.7 Wire and Configuration Invariants

The following must remain true regardless of how the registry
is extended:

I1. The daemon's JSON configuration shape is unchanged: the
    `netid`, `seednodes`, and `i_am_seed` fields retain their
    types and meanings.
I2. The peer-to-peer wire protocol does not encode the netid.
    Two daemons with different netids end up on disjoint
    gossipsub meshes because their seed-node sets do not
    intersect, not because the protocol carries a netid byte.
I3. The DEX-fee identity for a given netid is a single value
    expressed three ways (one per address-encoding family); the
    three representations correspond to the same key material.
I4. The DEX-fee rate is exact (`BigRational`), not
    floating-point. No part of the codebase shall convert it to
    `f64` for fee math.
I5. The burn-share parameters are optional: a network that does
    not burn omits the corresponding overrides and inherits the
    defaults of §6.1.

## 6.8 Binding Requirements

R1. **Single trait.** All per-network constants flow through
    the one trait of §6.1. No per-network constant shall live
    outside that trait.

R2. **Closed registry.** The lookup function of §6.2 is the
    single point at which a netid is mapped to its
    configuration. No code outside the network-config crate
    shall match on a numeric netid value.

R3. **Compile-time storage.** Each network's data is stored as
    `&'static`-backed constants on a unit struct. No allocation
    and no synchronisation participates in the lookup path.

R4. **Refuse-unknown.** The daemon shall resolve the selected
    netid through the registry during initialisation, before
    seed-node resolution and before bringing up the peer-to-peer
    subsystem. Operating on an unrecognised netid is a fatal
    startup error; a launch path that uses the panic-on-missing
    registry entry point shall do so only after the selected
    netid has already been accepted.

R5. **Production / test separation.** Production netids are
    unconditional; test netids are gated behind the
    `regtest-netid` Cargo feature. The release build does not
    enable that feature.

R6. **Netid-blindness of the peer-to-peer subsystem.** R1-R5
    of §6.5 are binding; the peer-to-peer subsystem does not
    receive, store, or branch on a netid.

R7. **Wire/config invariants.** I1-I5 of §6.7 are binding.

R8. **Three-flavour DEX-fee identity.** Every registered
    network exposes the DEX-fee identity in all three flavours
    of §6.1, even if some flavour is currently unused by any
    consumer; this keeps the trait object total and avoids
    later widening that would force every existing
    registration to be edited.

R9. **Bootstrap resolution call-site.** Trigger: daemon
    initialisation has resolved a known netid and is about to
    start the peer-to-peer subsystem. Required behaviour: the
    daemon shall resolve a single bootstrap relay list before
    peer-to-peer startup and pass that concrete list to the
    peer-to-peer startup interface. The peer-to-peer subsystem
    shall not perform netid lookup, JSON configuration lookup,
    or registry fallback on its own.

R10. **Operator-supplied seednodes win.** Trigger: the daemon
     JSON configuration contains a `seednodes` field that
     decodes as a list. Required behaviour: the resolved
     bootstrap relay list is exactly the operator-supplied list
     after applying the Chapter 28 relay-address grammar. The
     registry seed-node list shall not be appended, prepended,
     or used as a fallback for this startup.

R11. **Explicit empty seednodes suppress fallback.** Trigger:
     the daemon JSON configuration contains `seednodes: []`.
     Required behaviour: the resolved bootstrap relay list is
     empty. Startup and local peer-to-peer listening may still
     proceed subject to the other launch rules, but the daemon
     shall not dial any bootstrap relay and shall not consult
     the registry seed-node list for this startup.

R12. **Absent seednodes may use registry fallback.** Trigger:
     the daemon JSON configuration omits `seednodes` or supplies
     it as null, and the selected netid has already been
     accepted by the registry. Required behaviour: an
     implementation MAY support fallback to the registry
     seed-node list for the selected netid in this branch only.
     When that fallback is supported and the registry list is
     non-empty, the daemon resolves bootstrap relays from that
     list. If the fallback is not supported, or if the registry
     list is empty, the resolved bootstrap relay list is empty;
     absence of bootstrap relays alone shall not refuse startup.

R13. **Registry seed-node host mapping.** Trigger: R12 uses one
     or more registry seed-node strings on a native target.
     Required behaviour: each registry seed-node string is a
     bare IPv4 literal or DNS host string, not a full multiaddr.
     Before peer-to-peer startup, the daemon maps each such host
     to the TCP P2P port derived from the active netid. DNS may
     remain represented as DNS until dial time, or may be
     resolved earlier, but the port selected for native registry
     hosts is the netid-derived TCP P2P port.

R14. **WSS and memory seed boundaries.** Trigger: bootstrap
     resolution or address normalisation runs on a target that
     supports WSS or in-process memory transport. Required
     behaviour: WSS seed connectivity is optional transport
     support and does not replace the native TCP mapping in R13.
     When WSS is configured, WSS dials use the configured WSS
     port and TLS bundle. In-process memory addresses are
     reserved for tests and shall not appear in production
     registry seed-node lists.

R15. **Unknown netid rejected before fallback.** Trigger: the
     daemon JSON configuration selects a netid that the compiled
     registry does not describe. Required behaviour: startup is
     refused before seed-node resolution and before peer-to-peer
     startup. The registry fallback of R12 shall never define
     behaviour for an unknown netid, and an unknown netid shall
     not be converted into an empty bootstrap list.

R16. **Both production netids stay admitted; this project has no
     retired-netid concept.** Trigger: any production build of the
     daemon. Required behaviour: the registry of §6.2 shall describe
     both netid 8762 and netid 6133, and either shall pass network
     validation and reach bootstrap resolution under R9-R15. No code
     path shall refuse a netid on the ground that an external lineage
     treats it as retired, and no denylist of netid values shall be
     introduced: the closed allowlist of R2 and
     R4 is this project's only netid-admission mechanism, and a netid
     absent from it (netid 7777, for example) is refused by that
     absence alone. Concurrent support for the two production netids
     is the deliberate project-wide divergence the contributor
     instructions record; §6.6A.2 records the shape it diverges from.
     *Cleared with §6.6A; the clearance and its scope attestation are
     recorded in §6.13, and R40 withholds nothing in this rule.*

R17. **A per-netid parameter set is frozen to the designation it came
     from.** Trigger: an external designation changes a fee or
     fee-identity constant that is global in that lineage rather than
     per-netid (§6.6A.4). Required behaviour: such a change applies
     only to the netid whose registry entry this document set
     attributes to that designation. Netid 8762 retains the
     `v2.6.0-beta` schedule (base rate 1/777; discounted rate 9/7770
     selected by the exact ticker `"KMD"`) and netid 6133 retains the
     `v3.0.0-beta` schedule (base rate 2/100; discounted rate 1/100
     selected by the exact ticker `"GLEEC"`). A later designation's
     global change shall not be propagated onto netid 8762, the two
     schedules shall not be blended into a hybrid, and a rate shall
     never be selected by anything other than the active `NetConfig`.
     [Chapter 08](08-fee-routing-engine.md) owns the arithmetic that
     consumes these values and [Chapter 16](16-swap-v2-pre-burn-output.md)
     §16.1.1 records the resulting per-netid policy state. R17 is the
     network-parameter counterpart of
     [Chapter 51](51-legacy-v1-swap-state-machine.md) R70, which bars the
     same kind of silent carry-over inside the legacy swap machines.
     *Cleared with §6.6A; the clearance and its scope attestation are
     recorded in §6.13, and R40 withholds nothing in this rule.*

## 6.9 Acceptance Tests

T1. **Operator-supplied list wins.** With a supported netid and
    a non-empty `seednodes` list in the daemon JSON
    configuration, bootstrap resolution MUST return exactly the
    operator-supplied relay entries after Chapter 28 parsing. A
    test fixture with a non-empty registry seed-node list for the
    same netid MUST confirm that registry entries are not merged
    into the result.

T2. **Explicit empty list.** With a supported netid and
    `seednodes: []`, bootstrap resolution MUST return an empty
    list, MUST NOT consult the registry fallback, and MUST allow
    peer-to-peer startup to proceed without bootstrap dials when
    the remaining launch inputs are valid.

T3. **Absent list uses registry fallback.** With a supported
    netid whose registry seed-node list contains at least one
    test host, and with `seednodes` omitted from the daemon JSON
    configuration, an implementation that supports R12 fallback
    MUST return the registry hosts for that netid. A companion
    case with an empty registry list MUST resolve to an empty
    bootstrap list and no seed-related startup refusal.

T4. **Native registry host port mapping.** On a native target,
    a registry fallback host entry for a supported netid MUST
    normalise to a dial address using the TCP P2P port derived
    from that same netid. The test MUST cover at least one IPv4
    host and one DNS host, and MUST confirm that the WSS port is
    not substituted for the native TCP registry-host mapping.

T5. **Unknown netid rejected before fallback.** With an
    unsupported netid and no operator `seednodes` field, daemon
    startup MUST refuse before seed-node resolution and before
    peer-to-peer startup. The observed result MUST NOT be an
    empty bootstrap list on a running daemon.

T6. **Both production netids admitted, no retired-netid refusal.**
    (Cleared with §6.6A; see §6.13.) A production-feature build MUST
    resolve netid 8762 and netid 6133 through the registry of §6.2 and
    MUST return the no-configuration answer for netid 7777. A startup
    case configured with netid 8762 MUST pass network validation and
    proceed to bootstrap resolution; the test MUST fail if any code
    path refuses a netid for being netid 8762 specifically, rather
    than for being absent from the registry. A companion case MUST
    confirm that the fee parameters resolved for netid 8762 and for
    netid 6133 are the two distinct schedules of R17 in the same
    binary, since a single global schedule would satisfy every other
    test in this chapter.

## 6.10 Deferred and Out-of-Scope Items

D1. **DEX-fee semantics** beyond identity (when the fee is
    charged, who charges it, who receives it, how the burn
    share is computed and emitted) are covered in
    [Chapter 08](08-fee-routing-engine.md). This chapter binds only the
    *source of truth* for the fee identity and rate constants,
    not their interpretation.

D2. **Per-coin activation** is handled by the per-coin activation
    layer, which consults the network-config registry to resolve the
    DEX-fee identity for the chain it is activating.

D3. **Burn-emission integration** is covered in the
    daemon-wide central-context substrate (how that context
    exposes the network-config handle) and
    [Chapter 08](08-fee-routing-engine.md) (how the burn share is split
    out and emitted on-chain).

D4. **Macro consolidation** for the test-netid module is an
    implementation choice; the binding rule is that all test
    netids live in a single feature-gated module of §6.4, not
    that they are produced by any specific macro shape.

D5. **Restating §6.6A outside the document set.** Public-facing and
    operator-facing documents are out of scope for this chapter, and
    **the clearance recorded in §6.13 does not reach them.** That
    clearance covers §6.6A's text *inside this document set*; it
    authors and clears no public-facing or operator-facing text
    whatever. Section 6.6A supplies the netid-lifecycle facts those
    documents need, and R40 no longer bars restating them, but a
    restatement is bounded by the three items below and is deferred to
    the owners of those files. None of the three is done in this
    revision, and this chapter's own file is the only one this revision
    touches.

    - **A restatement must not go beyond what §6.6A states as
      cleared.** A passage in `README.md`, `docs/NETWORK_CONFIG.md`, or
      any other document outside this document set that asserts more
      than §6.6A does — a granularity finer than a published release
      designation, a fact about a designation this chapter does not
      record, or a motive, intent, or decision attributed to any party,
      which §6.6A.1 bars — is **new candidate material**. It needs its
      own pass through the chapter 01 §1.4A procedure before it is
      published. Clearing §6.6A cleared no such passage in advance, and
      no part of §6.13's record may be read as covering one.
    - **The internal prohibition of §6.6A.6 L5 travels with any such
      draft.** The stronger formulation of the fee history that L5 bars
      must not appear in a public-facing or operator-facing page, in
      that wording or in any equivalent of it. A drafter who takes the
      facts of §6.6A without taking that prohibition has produced an
      unsupported public claim out of cleared material, which is the
      one failure mode this item exists to prevent.
    - The netid-7777 section of `docs/NETWORK_CONFIG.md` should point
      at §6.6A.2, so a reader can see that this project's refusal of
      netid 7777 and an external lineage's refusal of netid 8762 are
      different mechanisms.

    The fourth follow-up this item previously carried — that the
    *Forbidden corpus* item of §6.13 take the scope §6.6A establishes —
    is **done**: §6.13 now records that scope, taken from the extent
    element of the R39 scope attestation (§6.6A.7).

## 6.11 External References

- The libp2p gossipsub specification (the fan-out semantics
  the codebase relies on across all networks).
- The libp2p floodsub specification (the more-permissive
  fan-out variant of §6.5 used unconditionally).
- The `num-rational` crate (the big-rational type backing the
  precise fee-rate returns of §6.1).
- The Cargo features model (the mechanism backing the
  production / test netid separation of §6.4).
- The Rust conditional-compilation reference (the `cfg`
  attribute used to feature-gate the test-netid module).
- The historical lineage's own published release notes for the
  designations named in §6.6A.5 — the v1.0.x-beta line and v1.1.0, the
  v2 line from `v2.0.0-beta` through `v2.6.0-beta`, and `v3.0.0-beta`
  — cited as public-release facts under chapter 01 R6 for the release
  designations and their published dates. (Added with §6.6A and
  narrowed with it: the entry no longer cites these notes for any
  characterisation of a change, §6.6A.6 L6 having dropped the one
  clause that rested on one.)

## 6.12 Baseline Verifications

The following are verifiable from the baseline state defined in
[Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree contains no `mm2_net_config` workspace
    member. A directory listing of the baseline tree
    (`git ls-tree c1d46c0c1592faa0860f704008b2b2381bc3840f`)
    contains no `mm2_net_config` entry; a tree-wide
    `git grep -l 'NetConfig\|net_config_for'` against the
    baseline returns no matches.

V2. At baseline, the per-network constant set the trait of
    §6.1 collects is scattered across multiple files:
    seed-node lists, fee-address constants, and the magic
    fan-out gate live in different places. The driving rule
    R1 of §6.8 (single trait) is therefore a strengthening of
    the baseline shape, not a restatement of it.

V3. At baseline, the peer-to-peer subsystem accepts a `netid`
    parameter on its public entry points and stores it in its
    behaviour struct. R1-R5 of §6.5 (netid-blindness) are
    therefore a strengthening of the baseline shape and
    require coordinated change in both the peer-to-peer
    subsystem and its callers.

V4. At baseline, the daemon does not refuse to start on an
    unrecognised netid: a daemon configured with an unknown
    netid will boot with empty seed-node lists. R4 of §6.8
    (refuse-unknown) is therefore a strengthening of the
    baseline shape.

V5. The 16-bit width of `netid` is preserved from the
    baseline. The `netid` field in the JSON configuration is
    a `u16` at baseline and remains a `u16` under the rules
    of this chapter.

## 6.13 Provenance Footer

- *Status:* driving-spec.
- *Version:* v4 (adds §6.6A, §6.8 R16-R17, §6.9 T6, §6.10 D5, the
  §6.11 release-notes entry, and the clearance record below).
- *Verified against:* baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`; absence of the
  network-config crate at baseline verified via
  `git ls-tree c1d46c0c1592faa0860f704008b2b2381bc3840f`
  and tree-wide `git grep` for the trait and registry symbol
  names against the baseline; the libp2p gossipsub
  specification; the libp2p floodsub specification; the
  `num-rational` crate (rate-arithmetic substrate); the
  Cargo features model; the Rust conditional-compilation
  reference; the active reloaded source tree for registry-backed
  startup validation and bootstrap relay resolution; and, for §6.6A,
  the chapter-02 baseline tree (the three pre-anchor facts §6.6A.7
  identifies) together with the historical lineage's own published
  release notes for the designations named in §6.6A.5, read for those
  designations and their published dates as public-release facts under
  chapter 01 R6.
- *Forbidden corpus:* consulted only to verify the historical
  daemon startup seed-node priority, empty-list behaviour, and
  native relay-address port mapping; and, for §6.6A and the material it
  produces (§6.8 R16-R17, §6.9 T6, §6.10 D5 and the §6.11
  external-reference entry), for exactly the five subject areas the
  extent element of the clearance item below records — netid admission
  shape with its default-netid history and the startup refusal it
  produces; the startup-initialisation ordering up to the binding of
  the public JSON-RPC listener; peer-bootstrap provisioning; the taker
  DEX-fee rate schedule and its history; and the fee-collection
  identity — under the reference designations that element names. That
  material was authored by the KDF Spec Reader under the two-team
  procedure of chapter 01 §1.4A and cleared by the KDF Dirty Gate. The
  first clause of this item keeps the position it already had: the pass
  recorded below neither extended to it nor reopened it. No
  discretionary expression — no private type, field, or function name,
  no body, no control-flow transcription, no local name, and no error
  or log literal — was reproduced.
- *Dirty Gate clearance (chapter 01 T1, R40, R43) for §6.6A and the
  material it produces:* the forbidden-class-sourced material this item
  covers is exactly what §6.6A.7's derivation record names — §6.6A in
  full, §6.8 R16 and R17, §6.9 T6, §6.10 D5, and the §6.11
  external-reference entry. It was authored by the KDF Spec Reader
  under the two-team procedure of chapter 01 §1.4A and **passed the KDF
  Dirty Gate** on 2026-10-24: PASS, discretionary-expression measure
  0.03, **leak-finding list empty**, and accordingly no remedy class
  for any line or for the candidate as a whole. That was the first gate
  pass on this material; no earlier verdict for it exists or was
  relayed. The clearance is therefore **complete and not pending**, and
  R40 withholds nothing in the covered material. This item is written
  on the clean side from the verdict, which reached the clean side as
  relayed and transcribed text rather than as a gate-written file; that
  is record-keeping and adds no chapter content.

  **Only the named material was gated.** The rest of this chapter was
  not re-examined, and the first clause of the *Forbidden corpus* item
  above keeps the position it already had.

  *The advisory findings, and what became of them.* The verdict's
  separately labelled non-leak section carried six advisory items on
  this chapter's own text, each stated as a location, a category, and
  at most one remedy class of R39's closed vocabulary and nothing
  further:

  - §6.6A.3's mechanism bullet — derivation-scope over-breadth —
    `re-establish`;
  - §6.6A.7's scope bullet — derivation-scope under-statement —
    `hold`;
  - §6.6A.5's row-precision paragraph — read-provenance enumeration
    incomplete — `hold`;
  - one clause of §6.6A.6 L6 — public-document characterisation —
    `remove`;
  - two structural-characterisation items inside §6.6A — thin margin —
    `none`.

  None is a leak finding. The first three were acted on in this
  revision: the derivation scope was re-established at the extent this
  item attests, and the scope bullet and the row-precision paragraph
  were settled against that extent. The fourth was acted on by
  deletion, not by rewording; the clause was cleared rather than
  failed, so its removal is a form judgment and is recorded as one. It
  was the only passage in the section whose basis was a
  characterisation of an external published document rather than
  observable behaviour, §6.6A.1 holds this section to the second, and
  §6.10 D5 sends the section's facts towards a public restatement in
  which such a passage is the likeliest to be read as an assertion
  about a third party; nothing in the section depended on it. The two
  `none` items are recorded so that the margin is on the record, and
  were left exactly as they stand: a class of `none` offers no remedy
  and none is owed.

  *Which parts carry an inline pointer to this item, and why those.*
  Four do. The head of §6.6A carries the clearance statement at the
  point the material appears (R40), because the whole section is gated
  material and a reader entering there must meet the notice before the
  facts. §6.8 R16, §6.8 R17 and §6.9 T6 each carry a one-line pointer
  back here, because those three are the numbered, rule-bearing and
  test-bearing text an implementer codes from and they sit among R- and
  T-numbers that have stood in this chapter through earlier revisions —
  the case in which a long-settled requirement and a newly cleared one
  are easiest to confuse. Nothing else carries a pointer, and that is a
  scope judgment rather than an oversight: §6.0's summary item and
  §6.10 D5 state the clearance in their own prose because each is about
  the clearance, the §6.11 entry is an external-reference citation that
  binds nothing and names §6.6A.5 as the section it serves, and every
  other section of this chapter rests on the position the
  *Forbidden corpus* item's first clause already records. §6.13 is
  where the record lives and carries none.

  The three elements of the R39 scope attestation, carried forward here
  in corpus-free terms as chapter 01 T1 requires. One element carries a
  gap, and it is recorded as a gap rather than completed from
  elsewhere:

  - *Extent.* The forbidden-class material corresponding to the subject
    matter this chapter names in its own terms, in five subject areas:
    the netid admission and startup-refusal surface (§6.6A.2, §6.6A.3);
    the daemon startup-initialisation ordering surface up to the point
    at which the public JSON-RPC listener is bound, which is what makes
    that refusal observable (§6.6A.3); the peer-bootstrap provisioning
    surface (§6.6A.3's mechanism bullet); the taker trading-fee rate
    surface (§6.6A.4, §6.6A.5); and the fee-collection-identity surface
    (§6.6A.4 and the note below the §6.6A.5 table), which this element
    carries in its own right and not as part of its fee-schedule
    wording. **This record takes its scope from this element and not
    from §6.6A.7's scope bullet**, an earlier form of which named four
    subject areas and omitted peer-bootstrap provisioning; a record
    copied from that form would have under-recorded the clearance, and
    §6.6A.7 has been corrected to agree with this element. Quantity
    examined: **ten distinct source or document units**, at twenty
    reference points, for approximately eighty unit-at-designation
    comparisons. Reference points: **nineteen published release
    designations** — the eight of the v1.0.x-beta line together with
    `v1.1.0`; the nine published designations of the v2 line from
    `v2.0.0-beta` through `v2.6.0-beta`; and `v3.0.0-beta` — together
    with this project's own chapter-02 baseline anchor, which is
    pre-anchor permitted input and nameable as such. **The record is no
    more precise than those designations:** no post-anchor commit
    identifier, no branch state, and no location within the compared
    material is given (chapter 01 R11 and the reference-version limit
    of §1.7).

    **The gap in this element, left visible rather than completed.** A
    twentieth designation exists, of the v2 line, whose published
    version string embeds an abbreviated commit identifier. R11, as
    §1.7 applies it to this element, bars naming it in a record of this
    kind, so the nameable extent stops at the nineteen designations
    above and that one reference point is not expressible here. It was
    examined, and this chapter's claim of a complete sweep of the v2
    line **holds at it in substance**. The element is therefore
    incomplete rather than approximated, and chapter 01 T1 is satisfied
    by saying so — a completed clearance statement carries the three
    elements **or** states that an element is not recorded. What must
    not be inferred is that the sweep is unattested: it is attested in
    substance and unnameable in form, which is a different thing, and
    the only thing missing is the naming. The note under §6.6A.5
    carries the same gap at the point the sweep is claimed.

  - *Method.* **Combined.** Reading, together with mechanical retrieval
    used solely to address each comparison to a published release
    designation rather than to any materialised undesignated state.
    **This repository's own clean-room gate tooling was not applicable
    and was not run:** it compares source bodies, the gated material
    embeds no source block, and its output would therefore have been an
    upper bound over an empty measured set. The absence is stated so
    that it is visible rather than implied.

  - *Measure.* **0.03** on the discretionary-expression basis — the
    same figure the leak report carries, not a second one — with
    dictated-interop and pinned-interface content excluded from the
    measured set before the judgment was made. **Judgment standard read
    against:** a presence standard rather than a proportion. Any
    genuine discretionary expression fails a candidate irrespective of
    volume, and a high match density on dictated or public-contract
    tokens is excluded from the measured set entirely, so the figure is
    not a margin to be spent. For orientation only, this repository's
    gate tooling carries an absolute discretionary cap of 0.50, which
    was not reached and was not the operative test here.
