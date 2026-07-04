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
| Burn address raw pubkey            | `&'static [u8]` (default &[])| Burn pubkey as raw bytes                       |
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
- *Version:* v3.
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
  startup validation and bootstrap relay resolution.
- *Forbidden corpus:* consulted only to verify the historical
  daemon startup seed-node priority, empty-list behaviour, and
  native relay-address port mapping.
