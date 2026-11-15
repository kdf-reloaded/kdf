# Plan: gaps of the browser build

**Status:** planning (written 2026-11-15 from the WASM probe of November 2026; not scheduled) — **Last reviewed:** 2026-11-15

> **Context.** A survey and probe of the browser (WASM) build in November 2026 found two kinds of gap. Some are
> limits of a browser by nature (no raw sockets, no inbound connections, no UDP, the browser owns TLS); the rest
> is missing work, and none of it is blocked. Today's browser build runs end to end in Firefox: RIN over WSS
> (activation, balance, legacy history) and Siacoin (activation, balance; its first browser run). This plan lists
> the missing work outside the shielded coins, records the limits by nature, and names the packages in order of
> cost. Shielded coins (ARRR) in the browser have their own plan,
> [`shielded-coins-in-browser`](shielded-coins-in-browser.md). Nothing here is decided or scheduled.

## Scope

The daemon compiled for `wasm32-unknown-unknown` and run in a page: its RPC methods, coin activations and daemon
features, compared with the native build. Governing CRD: chapter 26 (cross-platform and WASM substrate), with
the chapters named per item.

## Work packages

Sizes are rough estimates for planning ("small" ≈ days, "medium" ≈ 1–2 weeks), not measurements.

### G1 — Versioned transaction history in the browser

On wasm32 the versioned `my_tx_history` (mmrpc 2.0) answers `NotSupportedInWasm`, HTTP 400, for **every** coin
family; the whole RPC is a stub there. The native RPC has two routes, and neither exists on wasm:

- **G1a, the cheap partial route (small).** Compile the runtime-history route for wasm. It reads the same
  per-coin history list that the legacy path already keeps in IndexedDB, so UTXO, QTUM and Siacoin gain
  versioned history in the browser at once.
- **G1b, the SQL-indexed families (medium; smaller on the SQLite route).** BCH and SLP tokens, and the platform
  activations with tokens (BCH, ETH and others), use a SQL-indexed history store whose only implementation needs
  a SQLite connection, which a browser build does not have; their `tx_history: true` is silently ignored there.
  Two routes: an IndexedDB implementation of the store's interface (paging and token filtering need indexes, the
  raw-transaction cache a second table), or the SQLite-in-the-browser route of the shielded plan, which would let
  the existing SQL implementation run unchanged and share the persistence decision with the shielded wallet
  database. Depends on that plan's W1 if the SQLite route is chosen.

### G2 — Wallet management and the encrypted-passphrase start (small–medium)

A browser user has no wallet-management RPCs: `create_wallet`, `delete_wallet`, `get_wallet_names`,
`get_mnemonic` and `change_mnemonic_password` answer `NoSuchMethod`; a start with an encrypted passphrase is
refused, and the `wallet_name` and `wallet_password` settings are ignored. The work is an IndexedDB-backed wallet store and the start path that reads it.

### G3 — Event delivery to the page (small)

`stream::*::enable` is a silent no-op in the browser: the streamer starts, but no client can attach, because the
browser library exports no event callback. The work is a JavaScript callback or message channel through which the
page receives the streamed events. (`stream::shutdown_signal::enable` stays absent: a page has no process signals.)

### G4 — Version-two swap persistence and kickstart

The browser build's version-two swap storage is a no-op, and version-two swaps are not resumed after a reload.
This is recorded in plan [`v2-swap-engine-hardening`](v2-swap-engine-hardening.md) (finding F8; CRD chapter 52 D7
and chapter 26 §26.14 D6) and is not duplicated here. It is latent today: no code path starts a fresh
version-two swap.

### G5 — Smaller items (small each unless noted)

| Item | Browser behavior today | Notes |
| --- | --- | --- |
| `gui_storage::*` (11 methods) | every call errors (its IndexedDB backend is not ported) | small–medium |
| NFT methods (7) and NFT activation | `NoSuchMethod`; activation succeeds with an empty snapshot | needs browser history storage |
| EVM Trezor policy and withdraw | request fails to parse | WebUSB already works for Trezor |
| ETH/ERC20 legacy history | logs "not supported" and ends | needs browser history storage; four "not implemented in WASM" panics sit behind the same early return |
| `tx_history: true` on token-platform activations | silently ignored | follows G1b |
| Version statistics (5 methods), `stats_swap_status` | "only supported in native mode" | other nodes' swaps are not saved |
| `orders_history_by_filter` | "not supported in WASM" | — |
| `help` (legacy) | HTTP 500 | trivial |
| `metrics` (legacy) | always `[]` | the JSON form is missing work; an exporter is a browser limit |
| `trezor_connection_status` | `NoSuchMethod` | — |
| Solana and SPL in the browser | coins-file entry fails to parse | no blocker found: three name-resolution errors in today's code; tracked in plan [`solana-support`](solana-support.md) |
| UTXO verbose transaction cache | no cache, more Electrum traffic | — |
| Clock check | `ntp_*` settings ignored (no UDP) | an HTTP `Date` header check would be possible; see plan [`startup-clock-check`](startup-clock-check.md) |
| `block_on` in shared code | panics in WASM | call sites not traced; a hazard for shared code |
| Two source comments | say the shielded coin is active on every target | activation is refused in WASM |

## Limits by nature

These cannot be done in a page, or only with a service someone else runs. They stay refused or ignored in the
browser build; [`docs/WASM_BUILD.md`](../WASM_BUILD.md) lists them for users.

- **Lightning.** Its peers need raw TCP; a browser node is only possible behind a WebSocket-to-TCP proxy. The
  protocol crates themselves check clean on wasm32; the peer transport fails, and the persister is missing work
  on top. Whether the reference routes Lightning in its browser build is an open question for the Spec Reader.
- **Electrum over TCP or SSL, key pinning (`spki_sha256`) and `disable_cert_verification`.** A page reaches
  Electrum servers over WS/WSS only, and the browser owns TLS, so a pin cannot be checked and certificate
  verification cannot be switched off.
- **Native-daemon (coin daemon) mode.** Needs the daemon's configuration file on disk, and an RPC that answers
  cross-origin requests from the page, which coin daemons do not do.
- **Node roles.** No inbound connections: a browser node is a light node; seed and relay roles are refused
  unless the P2P layer runs in memory. P2P uses WSS relays only.
- **DNS resolver settings.** Ignored; the browser resolves names.
- **NTP clock check.** No UDP (see G5 for an HTTP-based alternative).
- **RPC server, configuration and log files, crash reports, allocator settings.** Absent; configuration comes
  from JavaScript and logs go to a JavaScript callback.

## Running separately

Three small fixes found by the probe are handled outside this plan and the shielded plan, as their own changes:

- the browser-store layer's event handlers are not withdrawn when a pending request is discarded (chapter 39
  F39.6.6.1);
- the vendored `zcash_primitives` breaks the wallet library's `orchard` feature on every target;
- the demo page in `wasm_build/` imports a bundle name that the build no longer produces.

## Notes on the dependency tree

- **The second `secp256k1-sys`.** In today's tree the second copy (0.4.0, native-only) comes only through
  `secp256k1 0.20.3`, which `bitcoin 0.27.1` and the `lightning` crates depend on. Every shielded crate uses
  `secp256k1 0.29.1` → `secp256k1-sys 0.10.1`, the copy already in the wasm tree. A sentence in the released
  0.2.0-beta.1 section of the CHANGELOG attributes the duplicate to the Zcash/ARRR stack; that does not match
  today's tree (whether it matched the tree of its day was not checked). The released section is not edited;
  this note records the source. Moving Lightning to the browser would bring the second copy back; moving the
  shielded stack would not.
- **`wasm-opt`.** The `wasm-opt` bundled with `wasm-pack` rejected the probe's modules (bulk-memory
  instructions). The daemon's bundle does not meet this, because its release profile turns `wasm-opt` off;
  turning it on for size needs a newer `wasm-opt` or `--enable-bulk-memory`.
- **Random source.** `getrandom` 0.2 and 0.3 are wired for the browser through the workspace; a crate that pulls
  `getrandom` 0.4 would need its own wiring.

## Order

G3 and G1a first (small, no dependencies), then G2, then G5 by demand, then G1b with or after the shielded plan's
W1. G4 follows the version-two swap work.
