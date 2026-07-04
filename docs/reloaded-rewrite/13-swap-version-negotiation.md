# Chapter 13 -- Atomic-Swap Version Negotiation

**Status:** driving-spec

> **One-sentence claim:** the codebase shall carry a single-
> byte typed version tag on the two order-protocol messages
> that initiate an atomic swap, with three bound numeric
> values (`1` legacy, `2` trading-protocol-upgrade,
> `3` non-fungible-token-extended), with element-wise-
> minimum pair negotiation, and with the legacy value
> omitted on the wire so that nodes that pre-date the tag
> deserialise messages from version-aware nodes unchanged.

## 13.0 Executive Summary

The atomic-swap protocol carried on the wire is a five-stage
hash-time-locked-contract dance: taker fee, then maker
payment, then taker payment, then maker spend, then taker
spend. Without an explicit version tag on the messages that
initiate the dance, any future change to the protocol would
be a hard fork of the gossip overlay: nodes that pre-date
the change would simply fail to deserialise messages from
nodes that have adopted it.

This chapter binds the codebase's resolution: a typed
single-byte version tag on the two order-initiation messages
(the taker's order-request and the maker's reservation
reply), carried under three bound numeric values, omitted
from the wire when its value is the legacy default, with
element-wise-minimum pair negotiation so peers that
advertise different versions still complete a swap by
falling back to the lower of the two.

The single-byte choice over a richer capability-vector
shape is bound by the constrained consumer set at the time
of writing: a single legacy path, a single trading-
protocol-upgrade path, and a single non-fungible-token
extension of the upgrade path. The wrapper-struct shape
over a bare integer leaves room for additive capability
fields to land in the same on-the-wire envelope without
breaking peers that already accept the present
`{"version": N}` shape.

## 13.1 Subsystem Shape

The version negotiation layer has four behavioural surfaces:

| Surface                          | Effect                                                        |
|----------------------------------|---------------------------------------------------------------|
| Typed version-tag value          | Single-byte wrapper carrying the version number               |
| Bound numeric values             | Three fixed wire-stable values + default rule                 |
| Wire-shape on order messages     | Field on order-request + order-reservation; legacy omitted    |
| Negotiation function             | Pair-min, fixed at the order-request/reservation exchange     |

The version-tag value type, the bound numeric values, the
predicate set used at dispatch time, and the negotiation
function form one substrate. The wire-shape integration on
the two order-protocol messages is the other.

## 13.2 Typed Version-Tag Value

R1. **Single-byte wrapper.** The version tag shall be a
    typed wrapper around a single unsigned byte. The wire
    shape of the wrapper shall be a JSON object with a
    single `version` key whose value is the integer byte.

R2. **Additive-extension safety.** The wrapper-struct shape
    (rather than a bare integer on the wire) shall be
    preserved so that future additive fields (capability
    flags, etc.) can land inside the wrapper without
    breaking peers that already accept the present shape.

R3. **Predicate surface.** The wrapper shall expose
    predicate operations sufficient for the dispatcher to
    decide which swap path to invoke without exposing the
    underlying integer at the call site: at minimum
    `is_legacy`, `is_v2_or_higher`, and `is_nft_v2` (or
    equivalents in name).

## 13.3 Bound Numeric Values

R4. **Three bound values.** The chapter binds the
    following wire-stable values:

    | Value | Meaning                                          |
    |-------|--------------------------------------------------|
    | `1`   | Legacy: the pre-upgrade five-stage HTLC swap     |
    | `2`   | Trading-protocol-upgrade: the V2 state-machine   |
    |       | swap path bound by [Chapter 14](14-state-machine-runtime.md) |
    | `3`   | Non-fungible-token extension of value `2`        |

R5. **Legacy is the default.** The wrapper's default
    constructor shall return value `1`. Every code path
    that constructs a version tag without a specific value
    in mind shall rely on the default so that an
    unspecified-version code path cannot accidentally
    upgrade the wire format.

R6. **Numeric stability.** The three bound values shall not
    change meaning. A future protocol revision adds a new
    value above the highest in the table; it does not
    reassign an existing one.

## 13.4 Wire-Shape Integration

R7. **Two carrier messages.** The version tag is carried on
    exactly two messages of the order-protocol exchange:
    the taker's order-request message and the maker's
    order-reservation reply. The handshake-completion
    messages (taker-connect, maker-connected) do not carry
    the tag; by the time those are exchanged the
    negotiated version is already fixed by the prior
    request-and-reservation exchange.

R8. **Legacy is omitted on the wire.** The tag field on
    both carrier messages shall be serialised with
    omit-if-legacy semantics: a serialiser shall not emit
    the field when its value is the legacy default of R5.
    A deserialiser that does not find the field shall
    resolve it to the legacy default. This is the
    backward-compatibility hinge that lets a version-aware
    node and a pre-tag node interoperate without code
    changes on the pre-tag side.

R9. **Persisted-order parity.** The same field with the
    same omit-if-legacy serde behaviour shall be carried
    on the persisted-order types so that orders authored
    before the tag landed deserialise to legacy without
    operator intervention. The persistence layer round-
    trips the field unchanged alongside the order JSON.

R10. **Fixed at single point.** The negotiated value shall
     be fixed at the order-request / order-reservation
     exchange and shall not be re-negotiated later in the
     swap. A code path that mutates the tag after the
     order-match envelope is built shall be rejected as
     an attempted protocol downgrade.

## 13.5 Negotiation Function

R11. **Element-wise minimum.** The pair-negotiation
     function shall be the element-wise minimum of the
     two peers' advertised version values. A pair of
     legacy peers settles on legacy; a V2 peer matched
     with a legacy peer settles on legacy (the V2 peer
     falls back to the old protocol rather than failing
     the match, preserving the gossip overlay across the
     upgrade); a V2 peer matched with another V2 peer
     settles on V2; a non-fungible-token-extended peer
     matched with a V2 peer settles on V2.

R12. **Downgradability invariant.** R11's correctness
     depends on every future version being a clean
     downgrade target for every later version. Any
     introduction of a value that is not a strict
     downgrade of its successor invalidates R11 and
     requires the richer capability-vector approach
     deferred under D1.

R13. **Single dispatch predicate.** The dispatch decision
     "should the V2 state-machine path run?" shall reduce
     to a single predicate call on the negotiated version
     value (the `is_v2_or_higher` predicate of R3). The
     dispatcher shall not switch on the underlying byte.

> **Reloaded implementation status (informative).** This
> negotiation substrate ships in the reloaded baseline
> (placement: the swap-versioning module within the main swap
> crate). The negotiated value is the exact numeric minimum of
> the two advertised single-byte versions per R11 — a true
> pairwise minimum, not an approximation over independent
> sub-fields — and the legacy fall-back described in R11 is the
> direct consequence of taking that minimum, requiring no
> separate code path. The dispatch predicates `is_legacy`,
> `is_v2_or_higher`, and `is_nft_v2` (R3) and the
> legacy-returning default constructor (R5) are present as
> specified. The single-byte values bound by R4 are 1 (legacy),
> 2 (V2), and 3 (non-fungible-token-extended).

## 13.6 Tests

The version-negotiation substrate shall be covered by
unit tests including:

T1. **Default is legacy.** Constructing the wrapper via
    its default constructor produces the legacy value.

T2. **Round-trip through JSON.** A wrapper containing each
    of the three bound values serialises to the
    `{"version": N}` shape and deserialises back to an
    equal wrapper.

T3. **Missing-field defaults to legacy.** Deserialising an
    object that does not contain the `version` field into
    a struct whose tag field carries omit-if-legacy semantics
    resolves the tag to the legacy default.

T4. **Predicates classify correctly.** The legacy value is
    classified as legacy; the V2 and the non-fungible-
    token-extended values are both classified as V2-or-
    higher; only the non-fungible-token-extended value is
    classified as non-fungible-token-V2.

T5. **Pair-negotiation corners.** The pair-negotiation
    function shall return the minimum for each of the
    pairs `(1,1)`, `(2,1)`, `(2,2)`, `(3,2)`, `(3,1)`,
    and `(3,3)`.

T6. **Cancel-swap is not an mmrpc 2.0 method.** An authenticated
    mmrpc 2.0 request whose method is `cancel_swap` and whose
    `params` member is absent, an empty object, an object carrying a
    UUID-shaped member, or any other syntactically valid JSON value
    returns the dispatcher method-not-found error with HTTP status
    400.

T7. **Cancel-swap has no success response.** No syntactically valid
    mmrpc 2.0 `cancel_swap` request returns a `result` member. The
    response is always the dispatcher error path after ordinary
    authentication and request-envelope parsing have succeeded.

T8. **Cancel-swap does not mutate an active swap.** Given a running
    swap UUID that appears in `active_swaps`, invoking `cancel_swap`
    with that UUID in `params` returns the same method-not-found
    error; a subsequent `active_swaps` call still includes the UUID,
    and `my_swap_status` reports no cancellation-derived terminal
    event.

T9. **Cancel-swap scope is role- and version-uniform.** The same
    method-not-found result and no-mutation invariant hold for maker
    and taker roles, for legacy and version-two swap records, and for
    unknown, inactive, or already-finished UUIDs.

T10. **Cancel-swap is not order cancellation.** Invoking
     `cancel_swap` shall not remove maker orders, taker orders, or
     matched-order state. Existing order-cancellation RPCs remain the
     only order-control surface.

T11. **Cancel-swap has no legacy alias.** A legacy-envelope request
     named `cancel_swap` is not handled as a legacy method. If the
     request reaches the compatibility fallback into mmrpc 2.0, it
     still resolves to the same method-not-found result.

These eleven tests are the negotiation and active-swap control
contract; any implementation shall keep them passing.

## 13.7 Cross-Subsystem Integration

The negotiated version is consumed by the following
substrates:

- The state-machine runtime
  ([Chapter 14](14-state-machine-runtime.md)) is dispatched
  when the negotiated version is V2-or-higher per R13.
- The V2 UTXO swap path
  ([Chapter 15](15-swap-v2-utxo-path.md)), the V2 pre-burn
  output engine ([Chapter 16](16-swap-v2-pre-burn-output.md))
  and the V2 EVM swap path
  ([Chapter 17](17-swap-v2-evm-path.md)) are the
  coin-family implementations on top of the state-machine
  runtime.
- The legacy fee-routing engine
  ([Chapter 8](08-fee-routing-engine.md)) runs unchanged on
  the legacy version.
- The recent-swaps and stream-status surfaces expose the
  negotiated version as a top-level field so that
  consumers (GUIs, integrations) can branch on protocol
  level without inspecting the swap's internal payload.
- The active-swap status surfaces do not expose a swap-cancellation
  RPC. The non-method behaviour of the reserved-looking
  `cancel_swap` name is bound in §13.7A so clients do not confuse it
  with order cancellation or task cancellation.

R14. **Non-fungible-token outcome enum.** Where the
     non-fungible-token-extended path is dispatched, the
     dispatcher shall return a typed outcome enum with at
     minimum the variants "use NFT V2 path",
     "version mismatch (advertise maker+taker values)",
     and "no NFT contract configured", so callers can
     distinguish a downgrade decision from a
     mis-configuration.

## 13.7A Non-Method `cancel_swap` Behaviour

R15. **No mmrpc 2.0 route.** The mmrpc 2.0 dispatcher shall not
     register `cancel_swap` as a callable method. A request with the
     standard mmrpc 2.0 envelope and method `cancel_swap` reaches only
     the generic dispatcher miss path after ordinary envelope parsing
     and authentication have succeeded.

R16. **Params have no schema or effect.** Because there is no
     `cancel_swap` handler, `params` is not decoded as a
     method-specific request. An absent `params` member, an empty
     object, an object carrying a UUID-shaped member, or any other
     syntactically valid JSON value in `params` shall not change the
     result.

R17. **Method-not-found error.** An authenticated, well-formed mmrpc
     2.0 `cancel_swap` request shall return the standard mmrpc 2.0
     error envelope with `error_type` equal to `NoSuchMethod` and
     HTTP status 400. There is no successful `cancel_swap` response
     shape.

R18. **Active and inactive UUIDs are equivalent.** A UUID supplied in
     `params` is not interpreted. Unknown UUIDs, inactive UUIDs,
     already-finished swap UUIDs, and UUIDs present in `active_swaps`
     all produce the same method-not-found result.

R19. **No running-swap cancellation.** A `cancel_swap` request shall
     not stop, abort, refund, remove, or otherwise advance a running
     swap state machine. The active-swap index, per-swap runtime
     tasks, and peer-to-peer swap-message channels shall remain as
     they were before the request, except for ordinary time-driven
     progress that would have happened without the request.

R20. **No persistence or status effect.** A `cancel_swap` request
     shall not append a swap event, mark a swap finished, alter the
     persisted swap record, or alter the response of
     `my_swap_status`, `my_recent_swaps`, or `active_swaps` except
     for ordinary time-driven progress that would have happened
     without the request.

R21. **Not order cancellation.** `cancel_swap` shall not cancel,
     remove, or amend maker orders, taker orders, matched-order
     reservations, or orderbook entries. Order cancellation remains
     limited to the order-control RPCs specified outside this
     subsection.

R22. **Legacy and swap-version scope.** The legacy flat dispatcher
     shall not expose `cancel_swap` as a method alias. If a
     legacy-style request falls through to mmrpc 2.0 compatibility
     handling, it still resolves to `NoSuchMethod`. The absence of
     `cancel_swap` applies uniformly to legacy swap records,
     version-two swap records, maker roles, and taker roles.

> **Upstream status (informative).** The analysed upstream corpus
> contains active-swap listing and status surfaces, but no shipped
> active-swap cancellation RPC. This chapter therefore binds the
> non-method behaviour above until a future chapter explicitly
> specifies cancellable swap states and a public cancellation method.

## 13.8 Invariants

| Invariant                                                | Bound by  |
|----------------------------------------------------------|-----------|
| Single-byte typed wrapper with `{"version": N}` shape    | R1        |
| Three bound numeric values: 1, 2, 3                      | R4        |
| Default constructor returns legacy (= 1)                 | R5        |
| Tag carried on order-request + order-reservation only    | R7        |
| Legacy omitted on the wire; missing field = legacy       | R8        |
| Field round-tripped on persisted orders                  | R9        |
| Negotiation fixed at the request/reservation exchange    | R10       |
| Element-wise-minimum pair negotiation                    | R11       |
| Dispatch reduces to single predicate                     | R13       |
| `cancel_swap` is not an mmrpc 2.0 method                 | R15, R17  |
| `cancel_swap` has no parameter schema or success response | R16, R17  |
| `cancel_swap` treats active and inactive UUIDs equally   | R18       |
| `cancel_swap` does not mutate runtime or persisted status | R19, R20  |
| `cancel_swap` does not cancel orders                     | R21       |
| `cancel_swap` has no legacy alias                        | R22       |

## 13.9 Deferred Work

D1. **Capability-vector negotiation.** R11's element-wise-
    minimum is a single-axis ordering. If a future
    protocol revision introduces a capability that is not
    strictly subsumed by later versions, the negotiation
    substrate shall be extended to a capability-vector
    form (each peer advertises a set; the substrate
    negotiates the intersection). Not in scope at the time
    of writing.

D2. **Wire-tag in the order-match envelope itself.** The
    tag presently rides on order-protocol messages, not
    on the persisted match envelope's wire-shape footer.
    Adding a tag to the envelope footer would let an
    inspector reconstruct the negotiated version from a
    persisted envelope alone without traversing back to
    the original request/reservation. Not in scope at the
    time of writing.

D3. **Anti-downgrade challenge.** R10 prohibits mutation
    after the order-match envelope is built but does not
    cryptographically prevent a manipulated re-broadcast.
    A signed-tag mechanism would close this gap; it is
    not in scope at the time of writing.

## 13.10 External References

- The publish-subscribe overlay over which the order-
  protocol messages travel; the per-network-id scoping is
  bound by [Chapter 6](06-network-id-seed-node.md) and
  the substrate by [Chapter 28](28-libp2p-modernization.md).
- The published JSON serialisation framework's
  omit-if-default and field-default attribute semantics,
  on which R8's backward-compatibility hinge depends.
- The hash-time-locked-contract atomic-swap protocol that
  the version tag governs the dispatch of.

## 13.11 Baseline Verifications

The following are verifiable from the baseline state defined
in [Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree carries no version-negotiation
    substrate. Verifiable by tree-wide
    `git grep -E 'SwapVersion|swap_version|swap_versioning'`
    against the baseline; matches are zero.

V2. The baseline order-protocol message shapes carry no
    version-tag field. Verifiable by inspection of the
    baseline order-protocol message types.

V3. The five-stage HTLC swap dance is present at the
    baseline and is exactly the protocol the legacy value
    `1` of R4 names; this chapter does not introduce a new
    swap protocol, only the version-tag substrate that
    allows additional swap protocols to coexist with the
    legacy one in the same gossip overlay.

## 13.12 Provenance Footer

- *Status:* driving-spec.
- *Version:* v2.
- *Verified against:* baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`; absence of
  the version-negotiation substrate at baseline verified
  via tree-wide `git grep`; absence of a version-tag field
  on the baseline order-protocol messages verified by
  inspection of the baseline message types; the
  publicly-documented JSON serialisation attribute
  semantics (omit-if-default, field-default) that R8
  relies on; the publicly-documented hash-time-locked-
  contract atomic-swap protocol that the legacy value of
  R4 names; current upstream mmrpc 2.0 and legacy dispatcher
  surfaces for the absence of a `cancel_swap` route; the
  upstream analysis corpus for the absence of a shipped
  active-swap cancellation contract.
- *Forbidden corpus:* consulted for upstream parity of the
  `cancel_swap` surface only; no source text, private helper
  structure, log strings, or internal decomposition copied.
