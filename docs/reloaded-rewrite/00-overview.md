# Chapter 00 — Overview and Purpose

**Status:** driving-spec (meta-chapter).

This chapter is the entry point to the document set. It binds what
the document set *is*, who it is *for*, what it *is not*, the
chapter-set composition discipline, and the reading order a first
reader should follow.

## 0.1 Executive Summary

The document set is the project's *derivation record*: a
chapter-by-chapter, publicly-reproducible record of how the project
codebase arrives at its present shape starting from the pinned
baseline tree of chapter 02, using only the permitted-input classes
of chapter 01, and citing per substrate the external specifications
the substrate's behaviour is dictated by.

Each substantive chapter is a *driving specification* of one
substrate: it binds the substrate's contract surface, names its
permitted inputs, lists its tests, deferred items, and baseline
verifications, and closes with a provenance footer. The document
set composes those substrate-by-substrate driving specifications
into a single end-to-end record of the project's delta from the
baseline tree.

This chapter does not bind any substrate of its own. It is a meta-
chapter that binds the document set as a whole: how chapters are
shaped, how the set composes, and the reading order. R1–R4 govern
the document set; R5–R6 govern the per-chapter shape.

## 0.2 Subsystem Shape

The document set is a sequence of standalone chapters under a
single repository directory, maintained on the same branches as
the source it describes. The set is composed of:

| Class                        | Chapters                                                            |
| ---------------------------- | ------------------------------------------------------------------- |
| Meta-chapters                | Chapter 00 (this), chapter 01 (rules and methodology), chapter 30 (cross-cutting index), chapter 34 (provenance ledger). |
| Anchor chapters              | Chapter 02 (baseline state), chapter 29 (license-condition treatment).               |
| Substantive driving-spec chapters | Chapters 03 through 28, chapters 31 through 33, and chapter 50.    |

The substantive chapters are independent: each can be read on its
own without prior chapters, provided the reader has read chapters 01
and 02 first. Cross-chapter references between substantive chapters
are by chapter number, not by source-tree path. Chapter 30 is the
reverse index over the substantive chapters, and chapter 34 is the
ledger companion artifact referenced by those chapters.

## 0.3 Bound Document-Set Discipline

**R1.** The document set MUST be a derivation record. For every
area in which the project differs from the baseline tree of
chapter 02, the set MUST contain a chapter that binds the
substrate's contract surface, names the publicly-available input
classes that informed it, and documents the discipline by which a
clean-room implementer could arrive at the same outcome.

**R2.** The document set MUST be *spec-first*. When a substrate's
behaviour is dictated by an external specification (a Bitcoin
Improvement Proposal, a Satoshi Labs Improvement Proposal, an
Ethereum Improvement Proposal, an Inter-Blockchain Communication
standard, a gossipsub protocol document, a contract Application
Binary Interface, a request-and-response payload shape, etc.), the
substrate's chapter MUST cite that specification by name and (where
applicable) by version.

**R3.** The document set MUST be maintained on the same branches as
the source it describes. When a chapter is added or revised, the
revision is recorded in the same commit history as any source
change it accompanies. Where a chapter becomes inconsistent with
the source it describes, the chapter is the defect and is to be
reported as an issue against the document directory.

**R4.** The document set MUST NOT make legal claims. It is a
technical-and-procedural record only, not legal advice or legal
defence; chapter 29 separately binds the project's legal position
on the additional copyright-holder conditions chapter 02 anchors
the baseline tree relative to.

## 0.4 Bound Per-Chapter Shape

**R5.** Every substantive chapter (chapters 03 through 28, chapters
31 through 33, and chapter 50) MUST
follow the canonical chapter shape: a chapter-bound title line, a
single `Status:` header on the third line carrying one of the two
chapter-bound status values (`driving-spec` for substrate-binding
chapters and the chapter-30-bound `legal-position` for chapter 29),
a one-paragraph one-sentence chapter claim, sections covering the
substrate's contract surface organised by sub-sub-claims and
binding rules of the form `R<n>`, a `Tests` section of the form
`T<n>`, a `Deferred Work` section of the form `D<n>`, a `Baseline
Verifications` section of the form `V<n>`, an `External References`
section, and a bulleted *Provenance Footer* closing with the
chapter-bound *Forbidden corpus: not consulted.* trailer.

**R6.** Each chapter's contract surface MUST be carried by the
rules-tests-deferred-verifications shape of R5 (the R/T/D/V
discipline). Per-substrate prose between sections is permitted but
MUST NOT carry binding contract that is not also carried in an
`R<n>` rule. Where prose and an R-rule conflict, the R-rule is
authoritative.

## 0.5 Reading Order

A first reader SHOULD read the document set in the following order:

1. Chapter 01 — *Clean-Room Rules and Methodology.* The normative
   rules the document set claims to follow (permitted-input classes,
   forbidden inputs, identifier hygiene, citation discipline, the
   canonical chapter shape).
2. Chapter 02 — *Baseline State.* The exact pinned baseline commit
   and the inherited tree shape at that commit.
3. Chapters 03 through 28 in numerical order, then chapters 31 through
  33, then chapter 50. Each chapter is self-contained and can be read
  independently if the reader is interested only in one substrate.
4. Chapter 29 — *Treatment of License Conditions (e) and (f).* The
   project's legal position on the additional copyright-holder
   conditions appended after the baseline date.
5. Chapter 30 — *Provenance and Attribution Index.* The cross-cutting
   per-chapter capsule index, the aggregated input register, and the
   per-substrate reverse map.
6. Chapter 34 — *Provenance Ledger.* The companion per-file
  classification ledger used by the chapter set.

## 0.6 What the Document Set Is Not

The document set is *not*:

- a *legal opinion or defence* — nothing in it is intended as legal
  advice; chapter 29 binds the legal-position substrate separately;
- a *substitute for the source* — the chapters describe behaviour
  and design intent in plain language and do not reproduce the
  source code itself; to understand the code, read the code;
- an *exhaustive feature catalog* — areas the project did not modify
  relative to the baseline tree of chapter 02 are out of scope; the
  set covers only the delta relative to the baseline tree;
- a *changelog* — a changelog answers *what* changed and *when*; a
  derivation record answers *how* the change could be produced from
  the permitted-input classes of chapter 01.

## 0.7 Tests

This is a meta-chapter; it has no test surface of its own. The
testing discipline the document set binds is carried on the per-
chapter `Tests` sections of the substantive chapters (R5).

## 0.8 Deferred Work

**D1.** Automated lint enforcement of R5 and R6 against the
document set is deferred to chapter 30 D1; see chapter 30 for the
audit-tooling-gap binding.

## 0.9 Baseline Verifications

**V1.** The document set MUST be confirmed to have no
substantive-chapter gap relative to the chapter 30 capsule-index
table: every chapter listed in chapter 30 §30.3 MUST exist under
the document directory, and every file under the document
directory MUST be listed in chapter 30 §30.3.

## 0.10 External References

This chapter has no external-specification citations of its own.
External-specification citations are carried per substrate on the
substantive chapters per R2, and aggregated by chapter 30 §30.4.

## 0.11 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit of chapter 02; chapter 01 (the canonical chapter shape and
  the permitted-input classes the rest of the document set follows);
  chapter 30 (the cross-cutting index over the substantive chapters
  and the audit-tooling-gap binding D1 of §0.8 refers to).
- *Permitted-input classes used:* the document set as it stands.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* not consulted.
