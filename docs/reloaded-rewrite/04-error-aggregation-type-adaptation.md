# Chapter 04 -- Error-Aggregation Type Adaptation to the Modern Trait Solver

**Status:** driving-spec

> **One-sentence claim:** the codebase shall carry an
> error-aggregation envelope `MmError<E>` (inner error plus
> ordered trace of source-code locations) built on a
> negative auto-trait (`NotMmError`) that bars
> `MmError`-of-`MmError` nesting, plus an explicit
> trace-preserving lift extension (`MmResultExt::map_mm_err`)
> for cross-error-type propagation; the historical
> `From<MmError<E1>> for MmError<E2>` blanket impl gated on
> a `(E1, E2): NotEqual` disjointness auto-trait shall not be
> present, because modern Rust trait-solver behaviour
> rejects the disjointness argument it relied on, and the
> explicit lift extension is the bound replacement.

## 4.0 Executive Summary

The codebase pairs every domain error with an ordered list
of source-code locations recording the path the error took
through the call stack. The envelope around this pairing,
the result alias, and the lift combinators together form
the codebase's standard error surface: every public RPC
handler in the codebase returns either this envelope or a
shape that is structurally indistinguishable from it on the
wire ([Chapter 27](27-infrastructure-crate-carve-outs.md)
R1).

The wire shape of a serialised error envelope is part of
the codebase's external contract and is bound verbatim
here. The Rust-language-level shape, however, has been
forced to evolve. A historical design used a negative auto-
trait to express a disjointness constraint that let the
question-mark operator transparently lift `MmError<E1>` to
`MmError<E2>` whenever `E2: From<E1>`. Modern Rust trait-
solver behaviour rejects that disjointness argument; the
two `From` implementations the historical design depended
on are no longer coherent.

This chapter binds the adapted design: a single non-
overlapping `From<E1> for MmError<E2>` impl, the
`NotMmError` negative auto-trait kept unchanged, the
disjointness auto-trait removed, and a single explicit
extension method that performs the trace-preserving lift
the historical design used to do automatically.

The wire shape (R7 below) is unchanged through the
adaptation. The call-site impact is mechanical: a small
suffix added to call sites that previously relied on the
removed implicit lift.

## 4.1 Subsystem Shape

The error substrate has the following surfaces, each bound
by this chapter:

| Surface                                  | Effect                                                       |
|------------------------------------------|--------------------------------------------------------------|
| Generic envelope `MmError<E>`            | Pairs domain error with ordered trace of locations           |
| Result alias                             | Idiomatic result type parameterised over the inner error     |
| Negative auto-trait `NotMmError`         | Bars `MmError`-of-`MmError` nesting at the type-checker level|
| Single `From` blanket impl               | Lifts bare inner error into envelope of another inner type   |
| Trace-preserving lift extension          | Explicit method that converts `MmError<E1>` → `MmError<E2>`  |
| Caller-location capture                  | Records the source location of every conversion site         |
| Wire-format serialisation envelope       | Five-field JSON shape consumed by integration clients        |

The five-field wire envelope (R7) is the external contract;
every other surface is internal to the Rust API.

## 4.2 The Envelope and the Trace

R1. **Envelope shape.** `MmError<E>` shall pair an inner
    error of type `E` with an ordered list of source-code
    locations. The list shall be appended to on every
    conversion that crosses the envelope, in stack order,
    so that the first entry is the innermost call and the
    last entry is the outermost.

R2. **Caller-location capture.** Every constructor and
    every conversion site within the substrate shall use
    the standard caller-location capture mechanism so that
    the appended trace entry records the source location of
    the call site, not of the conversion site inside the
    substrate.

R3. **Result alias.** The substrate shall expose a result
    alias parameterised over the inner error type, with
    the envelope as the error variant. Every call site that
    returns from the substrate's domain shall use this
    alias rather than the bare result type.

## 4.3 The Negative Auto-Trait Constraint

R4. **`NotMmError` is the substrate-defining negative
    auto-trait.** The substrate shall expose an auto-trait
    `NotMmError` with a negative implementation on
    `MmError<E>` for every `E`. The auto-trait shall be the
    sole declared constraint on the inner-error parameter
    of every conversion impl on the envelope.

R5. **`NotMmError` bars envelope nesting at the type
    checker.** Because the auto-trait has a negative
    implementation on the envelope itself, no `MmError<E>`
    is ever a valid inner-error type; a typo or refactor
    that would otherwise produce an `MmError<MmError<E>>`
    is rejected at compile time. R5 is the invariant that
    makes the substrate's flat-trace assumption (R1)
    sound.

R6. **Auto-trait propagation compensations.** The
    auto-trait shall carry the small fixed set of
    additional positive implementations on standard-
    library wrappers that are required because auto-traits
    do not propagate through unsized wrappers. The bound
    set at the time of writing is three: the standard
    library's boxed-trait-object wrapper (`Box<T>`), its
    interior-mutability cell wrapper (`UnsafeCell<T>`), and
    its non-null raw-pointer wrapper (`NonNull<T>`). The
    third compensation exists because at least one standard-
    library error type can, on some standard-library
    implementations, internally reach a boxed trait object
    through the raw-pointer wrapper rather than through the
    boxed-trait-object wrapper directly; the boxed-trait-
    object compensation alone does not cover that reachable
    case, so the auto-trait needs the raw-pointer
    compensation as well for the negative-implementation
    constraint of R4/R5 to hold uniformly across every
    standard-library error shape the substrate wraps.

## 4.4 The Single `From` Impl and the Explicit Lift

R7. **Single blanket `From` impl.** The substrate shall
    expose exactly one blanket `From` impl on the envelope:
    `From<E1> for MmError<E2>` where `E1: NotMmError`,
    `E2: From<E1>`, `E2: NotMmError`. This impl wraps a
    bare inner error of one type as an envelope of another
    type, starting a fresh trace, and is what the
    question-mark operator uses to lift any non-envelope
    inner error into the substrate's envelope.

R8. **No `From<MmError<E1>> for MmError<E2>` impl.** The
    substrate shall **not** carry a `From<MmError<E1>> for
    MmError<E2>` blanket impl. The historical
    implementation of that impl required a disjointness
    auto-trait (a negative implementation on the structural
    pair `(X, X)`) to avoid overlapping with R7; modern
    Rust trait-solver behaviour rejects that disjointness
    argument. The absence of the impl is the wire-stable
    consequence of that rejection.

R9. **Explicit trace-preserving lift extension.** The
    substrate shall expose an extension trait on
    `Result<T, MmError<E1>>` exposing a single method
    (named `map_mm_err` at the time of writing) that, for
    any `E2: From<E1>` and `E2: NotMmError`, returns
    `Result<T, MmError<E2>>` by converting the inner error
    with `E2::from` and preserving the existing trace
    (plus appending the call site under R2).

R10. **Non-`From` conversions use the pre-existing
     mapping helper.** Where a caller's conversion is not
     a `From` conversion (the target inner error is not
     mechanically derivable from the source inner error),
     the substrate's pre-existing per-call mapping helper
     (named `mm_err` at the time of writing, taking a
     closure `Fn(E1) -> E2`) shall be used. This helper
     does not depend on the trait solver's disjointness
     reasoning and was present before the adaptation.

## 4.5 Call-Site Impact

R11. **Mechanical, additive impact.** The adaptation's
     effect on call sites is mechanical: every call site
     that previously relied on the implicit envelope-to-
     envelope lift through the question-mark operator
     shall now write `.map_mm_err()?` (where the
     conversion is a `From` conversion) or
     `.mm_err(|e| ...)?` (where it is not). Call sites
     that lift a non-envelope inner error into the
     envelope continue to work unchanged through R7.

R12. **No swap, order-match, or RPC behaviour change.**
     The adaptation shall not alter swap, order-match, or
     RPC behaviour. The trace contents, the inner-error
     types, and the wire envelope (§4.6) are identical
     before and after. The only externally observable
     change at the source level is the extra method call
     at affected call sites.

## 4.6 Wire Envelope

R13. **Five-field JSON shape.** The substrate's standard
     serialisation of `MmError<E>` shall produce a JSON
     object with the following five fields (bound by
     name):

     | Field        | Source                                              |
     |--------------|-----------------------------------------------------|
     | `error`      | Display form of the inner error                     |
     | `error_path` | Dot-separated, de-duplicated file chain of the trace|
     | `error_trace`| Full `file:line` chain of the trace                 |
     | `error_type` | Discriminator tag of the inner error's wire shape   |
     | `error_data` | Payload of the inner error's wire shape             |

R14. **Tag/content shape for the inner error.** The
     `error_type` / `error_data` field pair shall be
     produced by the substrate's adjacent-tagged wire
     shape gating mechanism (R15); the discriminator name
     and the payload shape are owned by each domain error
     enum, not by the envelope.

R15. **Sealed marker for the wire-shape derive.** The
     substrate shall gate the derive of the
     adjacent-tagged wire shape behind a sealed marker
     trait emitted by a companion procedural-macro
     substrate so that arbitrary types cannot accidentally
     opt into the envelope's wire format without an
     explicit derive on their enum.

## 4.7 Preserved API Surface

R16. **Construction surface.** The envelope's public
     construction surface (`MmError::new` and equivalents
     used to start a fresh trace; `MmError::err` returning
     a result; `MmError::map` for inner-type substitution;
     `MmError::split` for decomposing the envelope back to
     its inner error and trace; and the constructor that
     accepts an externally-provided trace prefix) shall
     all be preserved across the adaptation.

R17. **Lift combinator surface.** The substrate's
     pre-existing per-call lift combinators (`map_to_mm`,
     `mm_err`, `or_mm_err`, and the futures-equivalents
     `map_to_mm_fut` and equivalents) shall all be
     preserved across the adaptation. The extension method
     of R9 augments this surface; it does not replace it.

R18. **HTTP-status-mapping integration.** Every domain
     error type that flows through the substrate to a
     public RPC handler shall declare its HTTP-status
     mapping at the handler boundary, per
     [Chapter 27](27-infrastructure-crate-carve-outs.md) R2.

## 4.8 Tests

T1. **Trace appends on lift.** Every conversion path
    through R7 and R9 shall be covered by a test that
    asserts the resulting envelope's trace has one more
    entry than the input, and that the new entry's source
    location matches the call site.

T2. **Wire-shape round-trip.** Every field listed in R13
    shall be exercised by a serde round-trip test against
    at least one representative domain error.

T3. **Negative-impl coverage.** A compile-fail test (or
    equivalent doc-test) shall demonstrate that
    constructing an `MmError<MmError<E>>` is rejected at
    the type-checker level by R5.

## 4.9 Deferred Work

D1. **Eventual return to a transparent lift impl.** Should
    a future trait-solver mode (a recognised stable
    alternative or its successor) admit the disjointness
    proof that R8's rejected impl relied on, the substrate
    may re-introduce the implicit lift impl, deprecate
    the explicit extension (R9) for `From`-derived
    conversions, and adjust call sites accordingly. The
    wire shape (R13) would remain unchanged. Not in
    scope at the time of writing.

D2. **Doc-comment lift the contracted invariants.** The
    bound invariants of §4.3 and §4.4 are presently
    expressed at the type-checker level only. Adding
    surface doc-comments that name them explicitly would
    let a downstream reader confirm the design intent
    without reading the trait-solver rules. Not in
    scope at the time of writing.

D3. **Per-error-class HTTP-status helper.** R18 delegates
    HTTP-status mapping to each handler. Common error
    classes recur across handlers; a per-class helper
    (e.g. "this error class always maps to 400") would
    reduce per-handler boilerplate. Not in scope at the
    time of writing.

## 4.10 External References

- The Rust language reference's auto-trait declaration
  and negative-implementation syntax (the language
  features on which R4 and R5 depend).
- The Rust language reference's caller-location-capture
  attribute (the mechanism R2 depends on).
- The Rust language reference's coherence and overlap
  rules (the basis for R8's coherence rejection).
- The Rust language's standard adjacent-tagged
  serialisation shape (the wire format R14 commits to).
- The codebase's toolchain-modernisation chapter
  ([Chapter 3](03-toolchain-modernization.md)), which
  records the bootstrap mechanism that lets the
  substrate retain the language features of R4 and R5
  while the rest of the codebase compiles on the stable
  toolchain.

## 4.11 Baseline Verifications

The following are verifiable from the baseline state defined
in [Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree carries the envelope, the negative
    auto-trait `NotMmError`, and the historical
    `From<MmError<E1>> for MmError<E2>` blanket impl gated
    on a `NotEqual` disjointness auto-trait. Verifiable by
    inspection of the baseline error substrate.

V2. The baseline tree compiles against the historical
    nightly toolchain pinned in
    [Chapter 3](03-toolchain-modernization.md) but not
    against the stable toolchain that
    [Chapter 3](03-toolchain-modernization.md) binds the
    codebase to move to. The R8 rejection is therefore
    forced by a language-level change, not by a
    substrate-level redesign.

V3. The substrate's wire-format envelope (R13) is present
    at the baseline with the same field names; the
    adaptation preserves it byte-for-byte at the API
    boundary. Verifiable by inspection of the baseline
    serialisation implementation.

## 4.12 Provenance Footer

- *Status:* driving-spec.
- *Version:* v2.
- *Verified against:* baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`; presence at
  baseline of the envelope, `NotMmError`, and the
  historical impl gated on a disjointness auto-trait
  verified by inspection of the baseline error substrate;
  the publicly-documented Rust language features of
  auto-traits and negative implementations on which R4-R5
  depend; the publicly-documented Rust caller-location-
  capture attribute on which R2 depends; the publicly-
  documented Rust coherence and overlap rules on which
  R8's rejection depends; the publicly-documented Rust
  adjacent-tagged serialisation shape on which R14
  depends; the toolchain modernisation contract of
  Chapter 3 (the `RUSTC_BOOTSTRAP` allowlist mechanism
  that keeps R4-R5's language features available).
- *Forbidden corpus:* not consulted.
