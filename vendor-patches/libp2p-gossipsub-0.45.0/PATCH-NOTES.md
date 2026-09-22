# libp2p-gossipsub 0.45.0 — vendor patch

## Provenance

Copied verbatim from `KomodoPlatform/rust-libp2p`, tag `k-0.52.12`
(commit `8bcc1fda79d56a2f398df3d45a29729b8ce0148d`), path `protocols/gossipsub`.
That is the same tag the root `Cargo.toml` pins `libp2p` to, and the copy is
redirected back into the graph with:

```toml
[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]
libp2p-gossipsub = { path = "vendor-patches/libp2p-gossipsub-0.45.0" }
```

`libp2p 0.52.1` from the fork binds to this copy; `cargo tree -p libp2p-gossipsub -i`
shows a single gossipsub and a single `libp2p-core` in the graph.

## Why it exists

`libp2p-gossipsub` 0.45.0 has a **remotely reachable, unauthenticated panic**. A
peer's PRUNE control message carries a `backoff: Option<u64>` that the behaviour
converts straight to a `Duration` and hands to `BackoffStorage`, which then does
unchecked `Instant` arithmetic. A single crafted PRUNE with a near-`u64::MAX`
backoff crashes the swarm state machine — on every KDF node and seed node that
will peer with the sender.

- GHSA-gc42-3jg7-rxr2 — `Instant::now() + time` overflow on ingest.
- GHSA-xqmp-fxgv-xvq5 — `backoff_time + slack` overflow on a later heartbeat
  (`overflow when adding duration to instant`).

Upstream fixed these in gossipsub 0.49.3 and 0.49.4, which belong to libp2p 0.56.
`k-0.52.12` is the newest tag the KomodoPlatform fork has (checked 2026-09-23),
so there is no fork bump to take, and rebasing onto libp2p 0.56 would mean
re-doing the ch28 migration and porting the fork's relay-mesh extension. Patching
the one crate is the proportionate fix.

## The patch

Three call sites, all in the backoff path. Everything else is byte-identical to
the fork, including the relay-mesh extension (`i_am_relay` / `IAmRelay`) that is
the reason the fork exists.

1. **`src/behaviour.rs`** — `remove_peer_from_mesh` clamps the peer-supplied value
   to `MAX_PRUNE_BACKOFF_SECS` (24h) before it becomes a `Duration`. The protocol
   default `prune_backoff` is 60s, so this is far above any legitimate request and
   keeps all downstream arithmetic trivially in range.
2. **`src/backoff.rs`, `update_backoff`** — `Instant::now() + time` becomes
   `checked_add` with a saturating fallback (`saturating_backoff`, which bisects
   for the largest addable `Duration` so it is correct on both the native and the
   wasm `instant::Instant` representations). An unrepresentable backoff is treated
   as "effectively forever", which is what the peer asked for.
3. **`src/backoff.rs`, `update_backoff` index computation** — the
   `heartbeat_index + heartbeats(time) + backoff_slack` sum becomes
   `saturating_add`. This one is **not** in the upstream advisories: it is a plain
   `usize` overflow, found by the regression test added here, and it panics in
   debug builds and on 32-bit targets — both of which this workspace ships
   (armv7, wasm32).
4. **`src/backoff.rs`, `heartbeat`** — `backoff_time + slack > now` becomes
   `checked_add(slack).map_or(true, |d| d > now)`. An unrepresentable deadline is
   by definition still in the future, so the entry is kept.

Regression tests live in `src/backoff.rs` under `mod tests`.

## Manifest changes (not security-related)

The fork declares deps through `[workspace.dependencies]`, which does not resolve
outside its own workspace. They are rewritten as explicit git deps **on the same
fork tag** so Cargo unifies them with the copies the root pin already resolves —
a tag mismatch here would produce two incompatible `libp2p-core`s. `[lints.rust]`
allows five pre-existing upstream warnings that were previously invisible because
the crate was a git dependency. `libp2p-identity` gains `ed25519`/`rand` in
dev-dependencies so the suite builds standalone.

## Lockfile

This directory's `Cargo.lock` is **gitignored on purpose**, unlike the three zcash
vendor patches. It would only govern `cargo test --manifest-path ...` runs; the
shipped build resolves this crate through the root lockfile. Committing it would
add a fourth Dependabot-scanned manifest reporting alerts about nothing that
ships -- exactly the noise the 2026-09-23 triage removed. Standalone test runs
resolve fresh.

## Test status

`cargo test --manifest-path vendor-patches/libp2p-gossipsub-0.45.0/Cargo.toml --lib`
→ **124 passed, 6 failed**.

Those 6 (`test_flood_publish`, `do_forward_messages_to_explicit_peers`,
`test_do_not_use_floodsub_in_fanout`, `test_publish_to_floodsub_peers_without_flood_publish`,
`test_do_not_publish_to_peer_below_publish_threshold`,
`test_do_not_flood_publish_to_peer_below_publish_threshold`) **fail identically on
the unmodified fork source** — verified by running the same suite against a
pristine copy of `protocols/gossipsub` with only this manifest (121 passed, the
same 6 failed). They are upstream publish/forward-routing tests that the fork's
relay-mesh extension invalidates, not a regression from this patch.

## Removing this patch

Delete the directory, drop the `[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]`
stanza and the `exclude` entry from the root `Cargo.toml`, once a
`KomodoPlatform/rust-libp2p` tag carries gossipsub ≥ 0.49.4 (or the ch28
modernization moves the pin to upstream libp2p ≥ 0.56).
