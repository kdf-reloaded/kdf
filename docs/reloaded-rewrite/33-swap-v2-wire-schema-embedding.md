# Chapter 33 -- Bound Swap V2 P2P Wire Schema (B-gleec Embedding)

**Status:** driving-spec (B-gleec embedding appendix; rule R31 of
[Chapter 1 §1.11](01-clean-room-rules.md)).

> **One-sentence claim:** the Swap V2 protocol's
> peer-to-peer wire-message schema is fixed by a single
> Protocol Buffers descriptor file that is embedded verbatim
> in this chapter; the chapter's substrate carries the
> descriptor (preserved without alteration to any field
> number, message name, or oneof tag) plus fresh CRD
> commentary on every message and every field.

## 33.0 Why This Chapter is a B-gleec Embedding

The Swap V2 protocol of [Chapter 15](15-swap-v2-utxo-path.md)
runs between two peers — a maker and a taker — that may be
running independently-developed implementations of the
protocol. The wire format MUST be byte-identical across
every implementation, in every field of every message,
or the swap fails. The format is fixed by a Protocol
Buffers descriptor; changing any message name, field name,
field number, field type, or oneof tag is a breaking change
that requires a coordinated upgrade across every protocol
participant.

Re-expressing the descriptor in prose alone would lose the
fixed-encoding guarantee. The clean-room substrate of
[§1.11 R31](01-clean-room-rules.md) therefore permits a
**B-gleec embedding**: the descriptor is reproduced
verbatim in this chapter (no upstream comments preserved,
no field-renames, no field-number changes, no message-shape
changes) and the chapter wraps the descriptor in
fresh-authored CRD commentary that binds the substrate's
behaviour above the bytes.

The chapter-bound substrate is the file:
`mm2src/mm2_main/src/lp_swap/swap_v2.proto`.

## 33.1 Bound Descriptor (Verbatim)

The following Protocol Buffers descriptor is the chapter-
bound substrate, embedded verbatim. No field number, no
message name, no oneof tag in this section is open to
interpretation; the file is the contract.

```proto
syntax = "proto3";

package mm2_swap_v2.pb;

message SignedMessage {
  bytes from = 1;
  bytes signature = 2;
  bytes payload = 3;
}

message MakerNegotiation {
  uint64 started_at = 1;
  uint64 payment_locktime = 2;
  bytes secret_hash = 3;
  bytes maker_coin_htlc_pub = 4;
  bytes taker_coin_htlc_pub = 5;
  optional bytes maker_coin_swap_contract = 6;
  optional bytes taker_coin_swap_contract = 7;
  string taker_coin_address = 8;
}

message Abort {
  string reason = 1;
}

message TakerNegotiationData {
  uint64 started_at = 1;
  uint64 funding_locktime = 2;
  uint64 payment_locktime = 3;
  bytes taker_secret_hash = 4;
  bytes maker_coin_htlc_pub = 5;
  bytes taker_coin_htlc_pub = 6;
  optional bytes maker_coin_swap_contract = 7;
  optional bytes taker_coin_swap_contract = 8;
}

message TakerNegotiation {
  oneof action {
      TakerNegotiationData continue = 1;
      Abort abort = 2;
  }
}

message MakerNegotiated {
  bool negotiated = 1;
  optional string reason = 2;
}

message TakerFundingInfo {
  bytes tx_bytes = 1;
  optional bytes next_step_instructions = 2;
}

message TakerPaymentInfo {
  bytes tx_bytes = 1;
  optional bytes next_step_instructions = 2;
}

message MakerPaymentInfo {
  bytes tx_bytes = 1;
  optional bytes next_step_instructions = 2;
  bytes funding_preimage_sig = 3;
  bytes funding_preimage_tx = 4;
}

message TakerPaymentSpendPreimage {
  bytes signature = 1;
  bytes tx_preimage = 2;
}

message SwapMessage {
  oneof inner {
    MakerNegotiation maker_negotiation = 1;
    TakerNegotiation taker_negotiation = 2;
    MakerNegotiated maker_negotiated = 3;
    TakerFundingInfo taker_funding_info = 4;
    MakerPaymentInfo maker_payment_info = 5;
    TakerPaymentInfo taker_payment_info = 6;
    TakerPaymentSpendPreimage taker_payment_spend_preimage = 7;
  }
  bytes swap_uuid = 10;
}
```

## 33.2 Chapter-Bound Substrate Rules

**R1.** **Verbatim embedding.** The descriptor of §33.1 is
the chapter-bound substrate. The implementer MUST place
the file at the path `mm2src/mm2_main/src/lp_swap/swap_v2.proto`
with content matching the §33.1 block exactly in every element
that carries function or external identity: message order,
message names, field names, field numbers, field types,
`optional` markers, oneof grouping and tags, and the
`syntax`/`package` declarations. Any divergence in one of
those elements — re-ordering, field renames, field-number
changes, type changes, oneof-tag changes, package-name
changes, syntax-level changes — is a chapter violation. Per
[Chapter 1 §1.11 R36](01-clean-room-rules.md), comments are
informative, not binding: the implementer's copy of the file
MAY carry comments the §33.1 block does not show (and the
§33.1 block itself carries none, per the R31 sanitization
discipline of dropping upstream commentary), and such
comments are conformant, not a chapter violation.

**R2.** **`proto3` syntax.** The descriptor MUST declare
`syntax = "proto3";` as its first non-empty line. The
`proto3` choice fixes the wire-encoding semantics
(no default-value emission, `optional` is opt-in, scalar
default is zero).

**R3.** **Package name.** The descriptor MUST declare
package `mm2_swap_v2.pb`. The package name is part of the
Rust-binding generator's output path and changing it would
ripple through every `use` statement in the swap V2
modules.

**R4.** **Generated-Rust home.** The `prost`-generated Rust
binding MUST land at the module path
`crate::lp_swap::swap_v2_pb` (i.e., a sibling module of
the swap V2 path of [Chapter 15](15-swap-v2-utxo-path.md)).
The chapter-bound substrate's build script MUST invoke
`prost-build` to materialise this binding at compile time.
The file is **not** committed; it is regenerated on every
build.

## 33.3 Bound Message Semantics

This section binds the substrate's read of each message of
§33.1. Field numbers and types are taken from the
descriptor; field semantics are bound here.

### 33.3.1 `SignedMessage`

The outer envelope every Swap V2 message travels in.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `from` | 1 | `bytes` | The sender's libp2p-derived public key (compressed secp256k1, 33 bytes). |
| `signature` | 2 | `bytes` | ECDSA signature over `payload` produced by the keypair whose public key is `from`. |
| `payload` | 3 | `bytes` | The serialised inner message — a `SwapMessage` of §33.3.10. |

**R5.** Receivers MUST verify `signature` against `from`
over `payload` before deserialising the payload. A
verification failure MUST drop the message and MUST NOT
log the failing payload at INFO or higher.

### 33.3.2 `MakerNegotiation`

The maker's opening message of the negotiation phase.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `started_at` | 1 | `uint64` | Maker's wall-clock seconds-since-epoch at the moment the maker started this swap. Used by the taker to bound the maker's clock skew. |
| `payment_locktime` | 2 | `uint64` | Maker's chosen payment-script absolute lock time (seconds since epoch). |
| `secret_hash` | 3 | `bytes` | The HTLC secret hash. Length is fixed by the HTLC family (32 bytes for the SHA-256 family). |
| `maker_coin_htlc_pub` | 4 | `bytes` | Maker's HTLC pubkey on the maker-coin chain. |
| `taker_coin_htlc_pub` | 5 | `bytes` | Maker's HTLC pubkey on the taker-coin chain. |
| `maker_coin_swap_contract` | 6 | `optional bytes` | EVM-only: maker-coin chain's swap-contract address (20 bytes). Absent on UTXO/UTXO swaps. |
| `taker_coin_swap_contract` | 7 | `optional bytes` | EVM-only: taker-coin chain's swap-contract address. |
| `taker_coin_address` | 8 | `string` | The maker's expected taker-coin receiving address (chain-specific encoding). |

**R6.** The two `optional bytes` swap-contract fields MUST
be absent for UTXO-only swap pairs and present for any
swap where at least one side is an EVM chain.

### 33.3.3 `Abort`

A negotiation-abort message with a free-text reason.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `reason` | 1 | `string` | Short human-readable abort reason. |

**R7.** `reason` MUST NOT carry secrets, internal paths,
or sensitive identifiers (per §1.11 / `AGENTS.md` security
rules). Receivers MUST treat the field as untrusted input
and MUST NOT log it at WARN or higher.

### 33.3.4 `TakerNegotiationData`

The taker's negotiation-data payload.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `started_at` | 1 | `uint64` | Taker's started-at timestamp (parallel to §33.3.2). |
| `funding_locktime` | 2 | `uint64` | Taker's chosen funding-script absolute lock time. |
| `payment_locktime` | 3 | `uint64` | Taker's chosen payment-script absolute lock time. |
| `taker_secret_hash` | 4 | `bytes` | The taker's secret hash for the funding HTLC. |
| `maker_coin_htlc_pub` | 5 | `bytes` | Taker's HTLC pubkey on the maker-coin chain. |
| `taker_coin_htlc_pub` | 6 | `bytes` | Taker's HTLC pubkey on the taker-coin chain. |
| `maker_coin_swap_contract` | 7 | `optional bytes` | EVM-only (parallel to §33.3.2). |
| `taker_coin_swap_contract` | 8 | `optional bytes` | EVM-only (parallel to §33.3.2). |

### 33.3.5 `TakerNegotiation`

A oneof envelope letting the taker either continue the
negotiation with data or abort it with a reason.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `action.continue` | 1 | `TakerNegotiationData` | The data-bearing path. |
| `action.abort` | 2 | `Abort` | The abort path. |

**R8.** The `oneof` MUST be exhaustive in the implementer's
match block: receiving neither branch (an empty oneof) is a
protocol violation and the connection MUST be dropped.

### 33.3.6 `MakerNegotiated`

The maker's accept/reject reply to the taker's negotiation.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `negotiated` | 1 | `bool` | Acceptance flag. |
| `reason` | 2 | `optional string` | Free-text rejection reason; present iff `negotiated == false`. |

**R9.** When `negotiated == true`, `reason` MUST be absent.
When `negotiated == false`, `reason` SHOULD be present.
Receivers MUST tolerate a missing `reason` even when
`negotiated == false` (the field is `optional`).

### 33.3.7 `TakerFundingInfo`

The taker's announcement of its funding transaction.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `tx_bytes` | 1 | `bytes` | The serialised funding transaction. Format is the coin's native transaction format. |
| `next_step_instructions` | 2 | `optional bytes` | Forward-compatibility envelope for coin-specific next-step hints (e.g., spv-proof seed). |

### 33.3.8 `TakerPaymentInfo`

The taker's announcement of its payment transaction.
Structurally identical to `TakerFundingInfo`.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `tx_bytes` | 1 | `bytes` | Serialised payment transaction. |
| `next_step_instructions` | 2 | `optional bytes` | Forward-compatibility envelope. |

### 33.3.9 `MakerPaymentInfo`

The maker's announcement of its payment transaction plus
its preimage signature against the taker's funding.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `tx_bytes` | 1 | `bytes` | The maker's payment transaction. |
| `next_step_instructions` | 2 | `optional bytes` | Forward-compatibility envelope. |
| `funding_preimage_sig` | 3 | `bytes` | The maker's signature over the funding-spend preimage; the taker uses this to claim funding refund. |
| `funding_preimage_tx` | 4 | `bytes` | The funding-spend transaction in unsigned form (or partially signed; coin-specific). |

### 33.3.10 `TakerPaymentSpendPreimage`

The taker's payment-spend preimage and signature.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `signature` | 1 | `bytes` | The taker's signature over the preimage. |
| `tx_preimage` | 2 | `bytes` | The unsigned payment-spend transaction. |

### 33.3.11 `SwapMessage`

The top-level dispatch envelope. Carries a single inner
message variant plus the swap's UUID for correlation.

| Field | # | Type | Semantic |
| --- | --- | --- | --- |
| `inner.maker_negotiation` | 1 | `MakerNegotiation` | §33.3.2 |
| `inner.taker_negotiation` | 2 | `TakerNegotiation` | §33.3.5 |
| `inner.maker_negotiated` | 3 | `MakerNegotiated` | §33.3.6 |
| `inner.taker_funding_info` | 4 | `TakerFundingInfo` | §33.3.7 |
| `inner.maker_payment_info` | 5 | `MakerPaymentInfo` | §33.3.9 |
| `inner.taker_payment_info` | 6 | `TakerPaymentInfo` | §33.3.8 |
| `inner.taker_payment_spend_preimage` | 7 | `TakerPaymentSpendPreimage` | §33.3.10 |
| `swap_uuid` | 10 | `bytes` | The 16-byte UUID of the swap. Field number 10 (not 8) is bound; the gap between 7 and 10 is reserved for future inner variants and MUST NOT be reused for `swap_uuid`. |

**R10.** Receivers MUST drop a `SwapMessage` whose
`swap_uuid` is not 16 bytes or does not match a known swap
they are participating in.

**R11.** The field-number gap (8, 9) is part of the wire
contract. Future inner variants MUST take numbers 8 and 9
(in oneof position) before any new top-level field is
considered.

**R12.** Receivers MUST treat a `SwapMessage` whose `inner`
oneof carries none of the seven variants of §33.3.11 as a
protocol violation and MUST drop the connection, mirroring
the empty-oneof handling R8 binds for `TakerNegotiation`'s
`action` oneof.

## 33.4 Outgoing Cross-References

The wire envelope's signature semantics of §33.3.1 R5 are
satisfied by the swap-uuid signing helpers of
[Chapter 15 §15.x](15-swap-v2-utxo-path.md). The wire
envelope's runtime decoder is the `prost`-generated module
of R4, consumed from the swap-V2 maker/taker state-machines
of [Chapters 15-17](15-swap-v2-utxo-path.md).

## 33.5 Tests

T1. **Round-trip test.** A test constructs every message
    type with representative payloads, encodes it,
    decodes it, asserts pointwise equality.

T2. **Field-number stability test.** A test loads the
    descriptor at build time (via `prost_build`'s
    introspection) and asserts the field-number map
    matches a hard-coded reference table. The reference
    table catches a silent rename of any field that left
    the field-number unchanged.

T3. **Unknown-field tolerance.** A test injects a payload
    with an unknown field number into a known message and
    asserts that the `prost` decoder silently drops the
    unknown field rather than failing. This is the
    forward-compatibility guarantee of the chapter-bound
    substrate.

## 33.6 Deferred Work

D1. **Protocol-version negotiation.** The chapter-bound
    substrate does not carry a protocol-version field. A
    version handshake (perhaps as a new top-level field
    `protocol_version` on `SwapMessage` taking field
    number 8) is deferred work. The deferral is
    conservative: peers running the same KDF version are
    the only currently-supported deployment.

D2. **Forward-secret signing keys.** The signing in
    §33.3.1 R5 uses the long-lived libp2p keypair. A
    per-swap ephemeral signing key would limit the blast
    radius of a key compromise. This is deferred work
    independent of this chapter.

## 33.7 External References

- The Protocol Buffers language specification:
  [`protobuf.dev/programming-guides/proto3`](https://protobuf.dev/programming-guides/proto3/).
- The `prost` Rust binding generator:
  [`docs.rs/prost`](https://docs.rs/prost/).
- The build-script generator `prost-build`:
  [`docs.rs/prost-build`](https://docs.rs/prost-build/).

## 33.8 Provenance Footer

- *Inputs:* the chapter-bound wire descriptor embedded verbatim in
  §33.1 (a B-gleec embedding per chapter 01 §1.11 R31, sourced from
  the `swap_v2.proto` substrate at
  `mm2src/mm2_main/src/lp_swap/swap_v2.proto`; upstream prose is not
  preserved); chapter 01 (the R31 embedding discipline and the R36
  binding-scope rule §33.2's R1 applies); chapter 15 and chapter 17
  (the descriptor's runtime behaviour as consumed by the version-two
  maker/taker state machines); chapter 52 (the state-machine-level
  message contract that sits above this chapter's wire schema).
- *Permitted-input classes used:* wire formats and external
  interfaces the project must inter-operate with (chapter 01 R4);
  the Interop / wire-format reuse category of chapter 01 R29–R31.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* not consulted for the §33.2–§33.6 commentary,
  which is clean-room CRD authored from the descriptor's structural
  shape and the substrate's runtime behaviour as bound in
  [Chapters 15-17](15-swap-v2-utxo-path.md). The descriptor in §33.1
  itself is the R31 verbatim embedding named above, not clean-room
  derived; see chapter 34 §34.3 (Interop reuse — wire-format-only
  source) for its ledger entry.
