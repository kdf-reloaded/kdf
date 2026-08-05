# Plan: secp256k1 0.20 → 0.29.1 migration

> **Status:** planned / not started.
> This plan scopes a workspace-wide migration from `secp256k1 0.20` to
> `secp256k1 0.29.1`. It is separate from the current wasm CI workaround and
> should only be executed when the project is ready to accept the API churn and
> the wider dependency ripple.

## Goal

Move the repository off the legacy `secp256k1 0.20` family without changing
signing semantics, HD derivation results, public key encodings, or swap wire
behavior.

## Why this is a separate migration

The current tree uses two incompatible `secp256k1-sys` families:

- `secp256k1 0.20` through the core wallet, HD, signing, swap, and Lightning
  code;
- `secp256k1 0.29.1` through the Zcash stack.

That split is what breaks the wasm packaging/link step. A partial bump leaves
both families alive and does not solve the collision.

## Scope

Track and update every direct `secp256k1 0.20` user in the workspace, with
special attention to:

- `mm2src/coins/`
- `mm2src/crypto/`
- `mm2src/kdf_keys/`
- `mm2src/hw_common/`
- `mm2src/mm2_p2p/`
- `mm2src/mm2_main/`
- `mm2src/mm2_eth/`
- `mm2src/trezor/`
- Lightning-related crates that import the same key/signature API

## Planned order

1. Inventory all `secp256k1` call sites and classify them:
   - mechanical API rename only;
   - type-shape change;
   - recovery/signature-sensitive;
   - HD/key-export/swap-sensitive.
2. Migrate leaf crates that only consume the API internally.
3. Migrate shared crypto/key crates.
4. Migrate coin, swap, and transport crates.
5. Re-run native, Windows GNU, and wasm32 verification after each phase.

## Risk areas

- HD derivation and key export
- signature recovery and verification
- swap key handling and order signing
- public or persisted key encodings
- wasm packaging and `secp256k1-sys` linkage

## Required verification

- native build and tests for affected crates
- wasm32 compile check
- Windows GNU release build
- focused regression tests for key derivation, address generation, and swap
  signing
- dependency tree check confirming only one `secp256k1-sys` family remains

## Release posture

Do not start this migration as part of a normal bug fix. Treat it as a staged
dependency project with its own branch, its own regression pass, and its own
documentation update.
