# Chapter 03 — Toolchain Modernisation

**Status:** driving-spec.

The chapter binds the substrate by which the project migrates off
the chapter-02-bound unstable Rust toolchain pin onto the stable
Rust toolchain: the new toolchain pin, the four-crate compiler-
bootstrap allowlist with the bound feature-attribute set, the
language-edition migration across the workspace, the removed
patched-dependency block, the renamed release-profile override
block, the workspace-wide dependency-inheritance block, the
explicit non-changes, and the format-check continuous-integration
carve-out.

## 3.1 Executive Summary

The chapter-02-anchored baseline tree was pinned to a specific
Rust nightly toolchain (chapter-02 R3, the nightly channel of
2022-02-01) and consumed a chapter-bound set of nightly-only
language feature attributes scattered across several crates. It
also carried the chapter-02 R5 patched dependencies for a chapter-
02-bound Android backtrace issue and spelled its release-profile
dependency overrides in a syntax the Cargo tool subsequently
renamed.

The chapter-bound substrate moves the project to a chapter-bound
pinned stable Rust toolchain release (a specific version number,
not the floating `stable` channel alias, so the pin cannot drift
to a newer release without a deliberate bump). The four chapter-bound crates that still
depend on a chapter-bound nightly-only language feature
(chapter-bound auto-traits with negative implementations, chapter-
bound custom-test-frameworks) compile on the stable toolchain
through the chapter-bound Cargo compiler-bootstrap allowlist
mechanism, scoped to exactly those crate names, documented in-tree
as a chapter-bound temporary bridge, and earmarked for removal
once the relevant chapter-bound features stabilise. The chapter-02
R5 patched-dependency block is removed. The chapter-bound release-
profile override block uses the renamed-by-Cargo current spelling.
Workspace members migrate from the chapter-bound 2018 language
edition to the chapter-bound 2021 language edition. Cross-
compilation configuration is preserved unchanged.

A separate concern — code formatting — still relies on chapter-
bound nightly-only formatting-tool options. The substrate handles
this at the continuous-integration layer by installing a chapter-
bound date-pinned nightly toolchain *only* for the format-check
job; the build-and-test toolchain remains the chapter-bound
pinned stable release of R1.

Bound rules R1–R4 cover the toolchain pin and the bootstrap
allowlist; R5–R6 cover the edition migration; R7–R9 cover the
removed-patch, renamed-profile-block, and workspace-dependency
changes; R10–R12 cover the explicit non-changes and the format-
check carve-out.

## 3.2 Subsystem Shape

The substrate occupies a structural seam between three artefact
classes:

- the chapter-bound root manifest (`Cargo.toml`) and the chapter-
  bound toolchain manifest (`rust-toolchain.toml`);
- the per-crate manifest of every workspace member of chapter-02
  R4;
- the chapter-bound continuous-integration configuration consumed
  by the format-check job (R12) and by the build-and-test jobs
  (R11).

The substrate does *not* modify the chapter-02 R8 build-target
surface, the chapter-02 R9 license posture, the chapter-02 R6
configuration surface, or the chapter-02 R7 request-and-response
surface.

## 3.3 Bound Toolchain Pin

**R1.** The chapter-bound toolchain manifest at the substrate
landing point MUST pin the chapter-bound `[toolchain].channel`
key to a specific stable-release version number and MUST pin
the chapter-bound `[toolchain].components` key to the chapter-
bound two-component list `["rustfmt", "clippy"]`, unchanged
from the chapter-02 R3 baseline.

The chapter-bound floating `stable` channel alias MUST NOT be
used as the pinned value: that alias resolves to whatever
release the installing tool considers current at install time,
which is not a pin at all and lets two otherwise-identical
builds silently compile against two different compiler
releases. Every routine build (developer, continuous-
integration, release) runs against the chapter-bound pinned
stable-release version, not against whichever release an
unpinned floating alias would have resolved to at build time.
The chapter-bound nightly channel of the chapter-02 R3 anchor is
not consumed except via R12's format-check carve-out.

The chapter-bound pinned stable-release version number MUST be
advanced only deliberately, when a substrate-level reason
requires the newer release, and MUST NOT be left to drift
silently. A later stable-release compiler can change a
standard-library type's internal representation in a way that
alters which types satisfy an auto-trait constraint the
substrate depends on elsewhere in the codebase (the chapter-04-
bound `NotMmError` auto-trait's propagation-compensation set of
[Chapter 4](04-error-aggregation-type-adaptation.md) R6 is the
bound example at the time of writing); an unplanned toolchain-
version change is therefore capable of silently breaking the
build even when no first-party source changed, which is exactly
why the pin must be an explicit version number rather than a
floating alias.

## 3.4 Bound Bootstrap-Allowlist Substrate

**R2.** The substrate MUST scan the chapter-02-anchored workspace
for chapter-bound `#![feature(...)]` attribute consumers and
classify each consumer into one of three chapter-bound disposition
classes:

| Bound disposition class | Bound substrate action |
| ----------------------- | --------------------- |
| The chapter-bound feature has been stabilised in the language. | Rewrite the call site against the stable accessor (the chapter-bound stable equivalents include, non-exhaustively: the chapter-bound iterator extract-if accessor in place of the chapter-bound drain-filter feature; the chapter-bound first-and-last iterator-method group in place of the chapter-bound map-first-last feature; the chapter-bound hash-map-entry accessor group in place of the chapter-bound hash-raw-entry feature; the chapter-bound input/output-error-kind concrete variants in place of the chapter-bound input/output-error-more feature; the chapter-bound panic-information message accessor in place of the chapter-bound panic-information-message feature). |
| The chapter-bound feature has a stable-API rewrite of acceptable scope. | Apply the rewrite (the chapter-bound asynchronous-closure feature, the chapter-bound integer-atomics feature, the chapter-bound internet-protocol feature, and the chapter-bound statement-expression-attributes feature fall in this class). |
| The chapter-bound feature has no stable equivalent and the substrate would otherwise touch substantively more code than the feature attribute itself. | Retain the feature attribute on the consuming crate and route the consuming crate through the bootstrap allowlist of R3. |

**R3.** The substrate MUST route a chapter-bound four-crate
bootstrap allowlist through the chapter-bound Cargo compiler-
bootstrap environment-variable mechanism in the chapter-bound
workspace-local Cargo configuration directory file. The allowlist
MUST be:

| Bound crate                    | Bound feature attributes retained                       | Bound consuming substrate                                                                                                                                                                                                                                                                                                                            |
| ------------------------------ | ------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| The shared-utility crate `common`         | Auto-traits + negative-implementations.                  | The chapter-bound chapter-14-consumed marker-trait pattern that expresses *if T is not already X then treat T as Y*; the chapter-bound chapter-14 R8–R10 storable-state-machine substrate consumes this pattern through the chapter-bound NotSame auto-trait marker.                                                                                                                                                                                                                                                                                                |
| The error-handling-framework crate `mm2_err_handle` | Auto-traits + negative-implementations.                  | The chapter-bound chapter-04-consumed error-aggregation framework consumes the same pattern to make the chapter-04-bound error-envelope conversions compose without conflicting blanket implementations.                                                                                                                                                                                                                                                                                                |
| The generic state-machine-runtime crate `mm2_state_machine`         | Auto-traits + negative-implementations.                  | The chapter-bound chapter-14 R8–R10 storable-state-machine substrate consumes the pattern directly to enforce the chapter-14 R10 storable-versus-non-storable runtime separation through the chapter-14-bound NotSame auto-trait.                                                                                                                                                                                                                                                                          |
| The chapter-bound integration-test harness crate consumed by the application-entry crate's container-test module | Custom-test-frameworks + the chapter-bound test feature.   | The substrate's container-integration-test harness enumerates and dispatches tests in a chapter-bound non-standard way that the chapter-bound stable test harness cannot express.                                                                                                                                                                                                                                                                                                |

In addition to the four chapter-bound first-party crates, two
chapter-bound sibling-allowlist test-mocking dependencies (the
chapter-bound `mocktopus` crate and the chapter-bound
`mocktopus_macros` crate) also require chapter-bound nightly-only
features and MUST be included on the allowlist when the substrate
consumes them.

**R4.** The bootstrap-allowlist mechanism is an *opt-in*: it
enables the chapter-bound nightly-only feature attributes only in
the listed crates and only during builds run with the chapter-
bound workspace-local Cargo configuration in scope. Crates outside
the allowlist MUST NOT carry chapter-bound `#![feature(...)]`
attributes and the chapter-bound stable compiler MUST reject any
such attribute outside the allowlist as it would on any chapter-
bound stable build. The allowlist MUST be documented in-tree as a
chapter-bound temporary bridge; when the chapter-bound language
team stabilises the auto-traits-equivalent behaviour, the chapter-
bound storable-state-machine substrate of chapter 14 R8–R10
migrates off the pattern, and the chapter-bound container-test
harness migrates onto a chapter-bound stable harness, the
corresponding crate MUST be removed from the allowlist.

## 3.5 Bound Language-Edition Migration

**R5.** The chapter-02-anchored baseline distribution of language
editions across the workspace was bound by chapter 02 R4 (the
workspace-member registry); the substrate's target distribution at
the substrate landing point MUST migrate every first-party
workspace crate onto the chapter-bound 2021 language edition with
exactly three chapter-bound carve-outs:

| Bound carve-out | Bound language edition | Bound rationale |
| --------------- | ---------------------- | --------------- |
| The chapter-bound sibling-allowlist in-tree copy of the chapter-bound container-test sibling-repository | The chapter-bound 2018 edition. | Preserved at the chapter-bound edition of the sibling-allowlist origin. |
| The chapter-bound sibling-allowlist in-tree copy of the chapter-bound Ethereum-virtual-machine application-binary-interface sibling-repository | The chapter-bound 2015 edition. | Preserved at the chapter-bound edition of the sibling-allowlist origin. |
| The chapter-bound sibling-allowlist patched-clone of the chapter-bound Siacoin sibling-repository | The chapter-bound 2018 edition. | Preserved at the chapter-bound edition of the sibling-allowlist origin. |

One first-party crate (the chapter-23-bound external-trading-
application-programming-interface client crate `trading_api`) MAY
remain on the chapter-bound 2018 edition because it was added by a
later chapter against an external service whose generated bindings
were authored against that edition; migrating it is tracked under
D2 of this chapter.

**R6.** The chapter-bound migration discipline is a per-crate
operation: change the chapter-bound `edition` key in the crate's
manifest from `"2018"` to `"2021"`, then run the chapter-bound
Cargo edition-fix command against that crate to mechanically
rewrite any source patterns the new edition treats differently.
For the chapter-bound 2018-to-2021 hop, the principal chapter-
bound difference the substrate MUST handle is that array-into-
iterator conversions follow the chapter-bound `IntoIterator for
[T; N]` implementation, which can require small turn-of-phrase
changes at call sites that were relying on the previous
behaviour.

## 3.6 Bound Removed Patched-Dependency Block

**R7.** The chapter-02 R5 patched-dependency block (pinning the
chapter-bound `backtrace` and `backtrace-sys` crates to the
chapter-bound sibling-allowlist clone for the chapter-bound
Android backtrace issue) MUST be removed from the chapter-bound
root manifest at the substrate landing point. The chapter-bound
rationale: with the move to stable Rust of R1 and current published
versions of the chapter-bound backtrace crate, the chapter-bound
workaround the sibling-allowlist clone applied is no longer needed
on any chapter-02 R8-bound supported target.

## 3.7 Bound Renamed Release-Profile Override Block

**R8.** The chapter-02-anchored release-profile override block
(spelled with the chapter-bound `overrides` table key, which is
Cargo's then-name for per-package profile adjustments and which
the Cargo tool subsequently renamed) MUST be renamed at the
substrate landing point to the chapter-bound current Cargo
spelling using the chapter-bound `package` table key. The bound
debug-symbol setting (`debug = false`) and the bound package
wildcard (`"*"`) MUST be preserved. The bound semantic is
unchanged: dependencies outside the workspace build without debug
symbols even when the release profile carries `debug = true` for
the workspace's own crates.

## 3.8 Bound Workspace-Wide Dependency-Inheritance Block

**R9.** The substrate MUST introduce a chapter-bound `[workspace.
dependencies]` block at the chapter-bound root manifest. The
chapter-bound Cargo feature MUST be the chapter-bound
workspace-wide dependency-inheritance feature stabilised in the
chapter-bound Cargo release 1.64; per-crate manifests inherit the
shared version via the chapter-bound `workspace = true`
inheritance flag.

The chapter-02-anchored baseline manifest did not consume this
feature — each crate declared its own dependency versions
independently. Centralising the chapter-bound shared versions
removes a chapter-bound long-running risk of two workspace crates
accidentally compiling against two different minor versions of the
same library and pulling both into the resulting binary.

## 3.9 Bound Explicit Non-Changes

**R10.** The chapter-bound Cargo feature-resolver MUST remain at
the chapter-bound `resolver = "2"` selection of chapter 02 R4
(the chapter-02-anchored manifest already selected this resolver;
the substrate preserves the selection).

**R11.** The chapter-02-anchored cross-compilation configuration
file `Cross.toml` MUST be preserved byte-identical to the chapter-
bound chapter-02 R8 anchor state. The chapter-bound cross-
compilation configuration for the chapter-bound ARM-v7 Linux
target (chapter-bound custom image name + chapter-bound dynamic-
linker rust-flags) continues to be the only entry, with the same
image name and the same flag.

## 3.10 Bound Format-Check Continuous-Integration Carve-Out

**R12.** The chapter-02-anchored formatting-tool configuration
file `rustfmt.toml` consumes chapter-bound nightly-only formatting
options (the chapter-bound `unstable_features = true` enabling
switch and the several options it gates including the chapter-
bound function-single-line option, the chapter-bound imports-
indent visual option, the chapter-bound inline-attribute-width
option, and the chapter-bound overflow-delimited-expression
option). The chapter-bound stable formatting tool rejects these
options with an error rather than ignoring them.

The substrate MUST install a chapter-bound nightly toolchain *only*
for the format-check step of the continuous-integration workflow
and run the chapter-bound nightly formatting-tool invocation from
there. The chapter-bound installed nightly toolchain MUST be
pinned to a specific date rather than the chapter-bound floating
`nightly` channel alias, for the same reason R1 pins the build-
and-test toolchain to a specific stable-release version rather
than the floating `stable` alias: the formatting tool's rendered
output can drift between nightly releases, so an unpinned
floating nightly would make the format-check step fail
spontaneously when an unrelated upstream formatting-tool change
lands, independent of any change to the workspace's own source.
The chapter-bound pinned nightly date MUST be advanced only
deliberately (bumping the pinned date and re-running the chapter-
bound nightly formatting-tool invocation across the workspace in
the same change) and MUST NOT be left to drift silently. The
chapter-bound build-and-test continuous-integration jobs MUST
continue to use the pinned stable toolchain of R1. The chapter-
bound nightly toolchain installation is the *only* nightly
toolchain installation routine continuous-integration performs
and it MUST NOT produce any compiled artefact.

## 3.11 Tests

**T1.** *Stable-toolchain build.* The chapter-bound continuous-
integration substrate MUST run the workspace's build and unit-test
suite on the chapter-bound stable toolchain of R1 across the
chapter-02 R8 build-target set. The build and unit-test suite
MUST pass.

**T2.** *Bootstrap-allowlist scope.* A chapter-bound regression
test MUST `grep` the workspace for `#![feature(` attributes and
assert that every consuming crate is on the chapter-bound R3
allowlist. A new consumer outside the allowlist is a chapter-bound
regression and MUST fail the test.

**T3.** *Format-check.* The chapter-bound R12 format-check
continuous-integration job MUST install a chapter-bound pinned-
date nightly toolchain (not the chapter-bound floating `nightly`
channel alias), run the chapter-bound nightly formatting-tool
with the chapter-bound check flag, and pass.

**T4.** *Patched-dependency removal verification.* A chapter-bound
regression test MUST `grep` the chapter-bound root manifest for
the chapter-bound patched-dependency block of R7 and assert it is
absent.

**T5.** *Toolchain-pin drift verification.* A chapter-bound
regression test MUST inspect the chapter-bound toolchain manifest
of R1 and assert that the `[toolchain].channel` value is a
specific stable-release version number and not the chapter-bound
floating `stable` channel alias.

**T6.** *Format-check nightly-pin drift verification.* A chapter-
bound regression test MUST inspect the chapter-bound continuous-
integration configuration of R12 and assert that the installed
format-check nightly toolchain is pinned to a specific date and
is not the chapter-bound floating `nightly` channel alias.

## 3.12 Deferred Work

**D1.** Removal of crates from the chapter-bound bootstrap
allowlist of R3 once the chapter-bound auto-traits and negative-
implementations features stabilise in the chapter-bound language
or once the chapter-14 R8–R10 substrate migrates off the pattern.

**D2.** Migration of the chapter-23-bound external-trading-
application-programming-interface client crate onto the chapter-
bound 2021 edition.

**D3.** Migration of the chapter-bound container-integration-test
harness onto a chapter-bound stable test harness so the
corresponding allowlist entry of R3 can be removed.

## 3.13 Baseline Verifications

**V1.** The chapter-02-anchored baseline toolchain manifest of
chapter 02 R3 MUST be confirmed to pin the chapter-bound nightly
channel of 2022-02-01 with the chapter-bound two-component list.

**V2.** The chapter-02-anchored baseline root manifest MUST be
confirmed to carry the chapter-02 R5 patched-dependency block and
the chapter-bound `[profile.release.overrides."*"]` block (with
the chapter-bound `overrides` table key, not the chapter-bound
`package` table key R8 renames it to).

**V3.** A chapter-bound scan of the chapter-02-anchored baseline
tree for `#![feature(` attributes MUST be confirmed to surface
the chapter-bound set the substrate disposes of under R2: the
chapter-bound async-closure feature, the chapter-bound auto-
traits feature, the chapter-bound custom-test-frameworks feature,
the chapter-bound drain-filter feature, the chapter-bound
hash-raw-entry feature, the chapter-bound integer-atomics feature,
the chapter-bound input/output-error-more feature, the chapter-
bound internet-protocol feature, the chapter-bound map-first-last
feature, the chapter-bound negative-impls feature, the chapter-
bound panic-info-message feature, the chapter-bound statement-
expression-attributes feature, the chapter-bound test feature.

## 3.14 External References

- *The Rust Edition Guide* — describes the chapter-bound
  2018-to-2021 edition migration and the chapter-bound Cargo
  edition-fix workflow.
- *The Rust Reference* — describes the chapter-bound `#![feature(
  …)]` attribute mechanism and its chapter-bound restriction to
  the nightly channel.
- *The Cargo Reference* — describes the chapter-bound compiler-
  bootstrap environment-variable mechanism the language team
  provides as a chapter-bound escape hatch for using chapter-bound
  unstable features on the stable toolchain during chapter-bound
  build bootstraps.
- *The Cargo Reference* — describes the chapter-bound profile-
  override syntax (the chapter-bound `[profile.<name>.package.
  <spec>]` syntax of R8, the chapter-bound successor to the
  chapter-bound earlier-spelling `overrides` table key R8
  renames).
- *The Cargo Reference* — describes the chapter-bound `[workspace.
  dependencies]` block of R9 stabilised in Cargo 1.64.
- The chapter-bound public tracking record for the chapter-bound
  Android backtrace situation that the chapter-02 R5 patched-
  dependency block R7 removes worked around.

## 3.15 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit of chapter 02 R1; chapter 01 (the methodology this
  chapter is shaped by; the chapter-01 R5 sibling-allowlist class
  consumed by the R5 carve-outs and by the R3 mocking-dependency
  routing); chapter 02 (the baseline anchor, the chapter-02 R3
  toolchain pin this chapter migrates off, the chapter-02 R4
  workspace-member registry every per-crate edition rewrite of R6
  consumes, the chapter-02 R5 patched-dependency block R7
  removes, and the chapter-02 R8 build-target surface T1 covers);
  chapter 04 (the error-aggregation framework consuming R3's
  second allowlist entry, and the R6 auto-trait propagation-
  compensation example the deliberate-bump discipline of R1
  cites); chapter 14 (the storable-state-machine
  substrate consuming R3's first and third allowlist entries);
  chapter 23 (the external-trading-application-programming-
  interface client crate D2 defers); the chapter-bound public
  documentation for the Cargo tool, the language reference, the
  edition guide, and the Android backtrace tracking record.
- *Permitted-input classes used:* the baseline itself (chapter 01
  R1); external public specifications (chapter 01 R3, for the
  Cargo and language-reference documentation citations);
  sibling open-source repositories under compatible licenses
  (chapter 01 R5, for the in-tree sibling-allowlist edition-
  carve-outs of R5 and the mocking-dependency routing of R3).
- *Sibling-allowlist consultations:* the chapter-bound in-tree
  sibling-allowlist clones of the container-test, Ethereum-
  virtual-machine application-binary-interface, and Siacoin
  sibling-repositories cited by R5; the chapter-bound mocktopus
  and mocktopus-macros sibling-allowlist test-mocking
  dependencies cited by R3.
- *Forbidden corpus:* not consulted.
