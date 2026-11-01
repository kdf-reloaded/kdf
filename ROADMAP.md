# Roadmap

This document captures the rough direction of KDF Reloaded beyond the v0.1.0-alpha.1 release. Specific dates are not committed; items may be reordered, deferred, or dropped as we learn from operating the alpha.

## Alpha (v0.1.0-alpha.x)

Goal: a publicly reviewable, buildable codebase exercising the same atomic-swap surface as upstream on netids 8762 and 6133, with maintainer-controlled CI and a clean licensing posture.

- [x] GPLv2-only continuation from upstream anchor `c1d46c0c` (2022-06-03).
- [x] Stable Rust toolchain, no nightly pin.
- [x] Self-hosted CI: format → unit tests (matrix) → docker tests, on-demand release builds.
- [x] Test-only netids gated behind `regtest-netid` Cargo feature.
- [x] Compatibility convention documented (developer rule + central admin chapter `docs/GLEEC_COMPATIBILITY.md`).
- [ ] Public alpha tag with signed Linux binary.
- [ ] Vulnerability disclosure mailbox and signing key fingerprints published.
- [ ] **Pirate Chain (ARRR) "Ironwood" hard fork, 3 Oct 2026 19:00 UTC.** After
  activation only version-6 transactions are standard, so every ARRR swap step breaks on
  an un-upgraded build. A wall-clock guard and swap freeze ship ahead of the date so a
  slip is safe rather than costly; receiving and balance display are unaffected. Plan,
  live-network evidence and staging: `docs/plans/arrr-ironwood-compatibility.md`.

## Beta (v0.2.0-beta.x)

Goal: stabilise APIs and on-disk formats; broaden platform coverage.

- Hardware wallet flows: complete Trezor coverage; promote Ledger out of `experimental-`.
- WalletConnect v2 stabilisation across native and WASM.
- Resolve outstanding HD-wallet dispatch TODOs in swap and ordermatch paths.
- Track GLEEC KDF evolution; populate `docs/GLEEC_COMPATIBILITY.md` as divergent behaviours land.
- Reproducible builds for Linux x86-64 and ARM64.
- WASM build kept in CI; documented integration story for downstream GUIs.
- Bump the vendored `librustzcash` (anchor-era 2022) to a modern release with
  batched note decryption and `shardtree` witnesses, to speed up shielded
  (ARRR/ZHTLC) sync. Workload analysis: `docs/plans/librustzcash-upgrade.md`.
- V2 swap engine: the `MakerPaymentSpent` timeout-abort refund gap (CRD
  ch.52 D8) is fixed and most of the copy-paste dedup across `lp_swap/`
  is done; still open — the V1 legacy event-deserializer mirrors (needs
  its own careful, wire-compat-focused pass, not mechanical dedup), V1
  driver consolidation, and real WebAssembly persistence for V2 swaps
  (currently a silent no-op, CRD ch.52 D7 / ch.26 D6 — a V2 swap in a
  browser build does not survive a reload). Workload analysis:
  `docs/plans/v2-swap-engine-hardening.md`.
- **Siacoin V2 swap protocol (CRD ch.54): drafted, awaiting review, not
  implemented.** ch.54 is an original-design chapter (no corpus
  precedent exists for Sia's V2 path) binding the coin-generic V2 swap
  traits to Sia's native spend-policy primitives. Per the plan that
  produced it, no implementation pass may start until it's been read
  and signed off. Until then, Sia swaps run V1 only — unaffected either
  way.

## Stable (v1.0.0)

Goal: long-term maintenance baseline.

- Public security disclosure process battle-tested through at least one full cycle.
- Reproducible signed builds for all release platforms.
- Deprecation policy for RPC methods documented and applied.
- Documented LTS branch policy.

## Out of scope (currently)

- iOS targets are not in the alpha release matrix.
- New atomic-swap protocol designs (e.g. submarine swaps, taproot-based variants) are tracked as research, not committed work.
- A first-party GUI is not in scope for this repository; existing third-party GUIs continue to consume the JSON-RPC surface.

## How to influence the roadmap

Open a discussion or an issue describing the use case. Concrete pull requests against documented gaps move things faster than open-ended feature requests.
