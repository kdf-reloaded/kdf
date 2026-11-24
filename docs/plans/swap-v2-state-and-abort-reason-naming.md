# Plan: names of the swap protocol v2 states and abort reasons

**Status:** planning — **Last reviewed:** 2026-11-23

**CRD:** ch.52 (R22, R36, R44 and their status notes). Reloaded-original, clean side. After 0.5.0.

The states and the abort reasons of a version 2 swap are visible to wallets: they appear in the swap status
answers and in the stored swap rows. In 0.5.0 their set does not change. Where a v2 step needs to tell two
endings apart, it uses an existing reason with a fixed opening phrase in its text. This plan decides whether
later versions rename or add states and reasons, and how wallets are brought along.

## Steps

0. A decision on renaming or adding v2 states and abort reasons, together with the questions for wallet
   maintainers: which names they read, which they show to users, and how much notice they need.
1. Compatibility: the effect on interoperation and on the documents that record it (`RELOADED_VS_GLEEC.md`,
   `docs/GLEEC_COMPATIBILITY.md`).
2. The migration of stored v2 swap rows, in both directions of an upgrade, and what an older build answers for a
   row it cannot read.
3. Documentation: the CRD notes, the RPC reference and the CHANGELOG's upgrade notes.

## Decisions

- 2026-11-23, maintainer: no new v2 states or abort reasons in 0.5.0; a later change is coordinated with wallet
  maintainers.
