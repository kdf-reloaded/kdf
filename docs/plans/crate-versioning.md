# Plan: crate versioning

**Status:** planning (topic list for the next round; nothing is decided or implemented) — **Last reviewed:** 2026-11-02

> **Context.** The daemon's start-up log used to show only the short commit hash. The 0.4.0 cycle adds the release version to
> the banner (CRD ch.45 R45.6.7). Doing so showed that the version we call "the release version" does not live where one
> would expect. This plan only lists the questions for the next round; it is not a design.

## 1. The release version and the crate that holds it

The release commit bumps the version of the package `mm2_bin_lib` (`mm2src/mm2_bin_lib/Cargo.toml`, and `Cargo.lock`). The
library crate `mm2_main`, which prints the banner, has its own, unrelated version (`0.1.0`) and does not depend on
`mm2_bin_lib`, so its own version cannot give the release version. For now the build script of `mm2_main` reads the version
from `mm2_bin_lib`'s `Cargo.toml` and a test guards that the two stay in step. Open: is that bridge the right long-term
shape, and should the other workspace crates carry any version at all?

## 2. Where the version should live

Open: one `workspace.package` version inherited by the crates that need one, or a version per crate as today. How the
version-bump commit of a release should touch it (one place or two, the lock file, the README status line, the CHANGELOG
heading), and what a check in CI could verify (for example that the banner version equals the tag).

## 3. Whether the `version` RPC and swap records should carry it

The `version` method and the version recorded in swap records carry the bare short commit hash. That is unchanged by the
banner work. Open (maintainer decision D4 (b), deferred): should either ever carry the release version, and what would
other nodes and clients do with it?

## Not in scope

No version change of any crate, no new policy, no change to the string that swap records and the `version` method carry.
