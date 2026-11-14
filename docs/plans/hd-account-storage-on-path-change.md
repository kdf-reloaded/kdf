# Plan: stored HD accounts after a change of a coin's derivation path

**Status:** planning (post-beta; nothing is implemented) — **Last reviewed:** 2026-11-11

> **Context.** Under an HD login (`enable_hd: true`) with the seed as the key source, KDF Reloaded
> loads a coin's stored HD accounts only after a check: each stored account's extended public key
> must equal the key the seed derives for that account index below the coin's currently configured
> `derivation_path`. A stored account that differs is removed from the database with one WARN and is
> not loaded. The check came with batch 4 item B4-2 (PR #293) of plan
> [`hd-key-unification`](hd-key-unification.md); its purpose is to drop rows that an earlier build
> stored for a `Trezor`-policy coin under the software digest (CRD ch.05 §5.10B, status note of
> 2026-11-08). Code: `mm2src/coins/hd_wallet_storage/mod.rs` (`load_accounts_derived_from_seed`,
> `is_account_of_the_seed`), called from `mm2src/coins/utxo/utxo_common/utxo_common_hd.rs` and
> `mm2src/coins/eth/eth_activation.rs`. A `Trezor`-policy coin skips the check and loads its rows as
> stored.
>
> After the coins file changes a coin's `derivation_path` between two sessions, every stored account
> of that coin differs from what the seed derives under the new path, so all of them are removed and
> the coin starts as a fresh HD wallet at the new path. This differs from GLEEC KDF, as CRD ch.05
> §5.10B records in its status note of 2026-11-11. The maintainer decided on 2026-11-11 that
> 0.4.0-beta.1 documents the behavior (CHANGELOG, Known limitations; `RELOADED_VS_GLEEC.md`;
> `docs/GLEEC_COMPATIBILITY.md`) and that a friendlier handling is analyzed after the beta.

## What a user sees today

- The coin looks like a new, empty HD wallet: the first account (or the one `path_to_address` names)
  is created at activation, and, with the default `scan_policy`, its addresses are scanned up to
  the gap limit.
- One WARN per coin: "removed N stored HD account(s) that the wallet's seed did not derive". For this
  case the wording is inaccurate: the seed did derive those accounts, under the previous path.
- No key material is lost: every address can be derived from the seed again at its path. What is
  lost are the stored address counters of the removed accounts. A scan rebuilds them only up to the
  gap limit, and accounts other than the one the activation creates have to be added again.
- Restoring the previous path brings the earlier addresses back the same way: the accounts stored
  under the new path are removed in turn, and the coin starts afresh at the previous path.
- Normal use does not trigger it. It needs an edit of a coin's `derivation_path` in the coins file,
  or a switch between two coins files that give a ticker different paths.

## Options to analyze

- **(A) A compatibility switch** that reproduces the reference behavior: stored rows are loaded as
  stored, whatever the configured path.
- **(B) Non-destructive handling.** A stored account that belongs to another path is skipped, not
  deleted, so that a restored path finds its accounts and counters intact. A stored row is
  identified by the coin, the two wallet digests and the account index (`coin`, `mm2_rmd160`,
  `hd_wallet_rmd160`, `account_id`); the path is not part of it. Two paths for one ticker would
  therefore collide on the same account index, so (B) needs a row key or a marker that tolerates two
  paths for one ticker. The rows that the check was made for (a `Trezor`-policy coin's rows under the
  software digest) must still not be loaded.
- **(C) Path-aware storage** that keeps the accounts of each path and can report both account sets
  of a coin.

**Recommendation:** analyze (B) first, (C) if it comes at little extra cost, and (A) only if an
integrator asks for it.

## Work packages

| Id | What | State |
|---|---|---|
| P1 | **WARN wording.** Name the configured derivation path, for example "…that the wallet's seed did not derive at the configured derivation path". | Not started |
| P2 | **The chosen handling** (one of the options above), with unit tests in `mm2src/coins/hd_wallet_storage`. | Not started; waits for the analysis |
| P3 | **Lab row.** Activate a coin under path X, change the path to Y and activate, restore X and activate; check that the earlier balance and the address counters are visible again. | Not started |

## Open points

- Which option, after the beta.
- Whether P1 ships on its own before P2.
