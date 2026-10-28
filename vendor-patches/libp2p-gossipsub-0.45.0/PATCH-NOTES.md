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

Three call sites, all in the backoff path. Apart from the send queue change described
below, everything else is byte-identical to the fork, including the relay-mesh extension
(`i_am_relay` / `IAmRelay`) that is the reason the fork exists.

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

## Bounded per-connection send queue (not security-related)

A second, unrelated change, so the "byte-identical" statement above now reads: the rest of the
crate is byte-identical to the fork except for the backoff patch and this change. It implements
CRD ch.28 §28.9B RQ1 (Reloaded-original; the reference leaves this queue unbounded). The wire is
unchanged.

**Why.** The connection handler's `send_queue` was an unbounded `SmallVec` popped from the end.
A peer that stops reading, or reads more slowly than messages arrive, made the sending node hold
every message queued for it (the payload plus about 0.4 KB each, separately per peer), and a backlog
was delivered newest first. The ping safeguard closes only a peer that stops answering pings, not
one that answers pings and reads no gossip.

**What changed.**

1. **`src/send_queue.rs` (new)** -- `SendQueue`, a FIFO of queued RPCs with accounting. An RPC that
   carries at least one published message is a *data RPC*; everything else (subscriptions, GRAFT,
   PRUNE, IHAVE, IWANT, the relay-mesh notices) is *control* and is never counted or evicted. The data
   RPCs are capped at 2,048 and 4 MiB of encoded size per connection. A data RPC that does not fit
   evicts the oldest queued data RPCs first (a lone RPC larger than the byte cap evicts the rest and is
   queued anyway). The first eviction starts a period at the cap; it ends when the data queue is at or
   below half of both limits. After 60 s in the period the queue reports the connection as stuck, once.
   The logic takes the current time as an argument, so its tests need no sleeping. The tests are in the
   module.
2. **`src/handler.rs`** -- `send_queue` is now a `SendQueue` (written in queue order, no more
   `shrink_to_fit` per pop); the handler gets the peer ID and the limits at construction, logs the
   drop warnings (peer ID, counts and sizes only, never content) and arms a timer
   (`futures_ticker::Ticker`, the same time source the behaviour uses, native and wasm) for the
   stuck deadline. A stuck connection is reported to the behaviour with the new
   `HandlerEvent::SendQueueStuck`.
3. **`src/behaviour.rs`** -- on `SendQueueStuck` it emits `ToSwarm::CloseConnection` for that one
   connection. The handler cannot close the connection itself (`ConnectionHandler::Error` is
   `Void`) and `KeepAlive::No` would not close it while other protocols keep the connection alive.
   The `Handler::new` call sites pass the peer ID and the limits.
4. **`src/config.rs`** -- `ConfigBuilder::send_queue_max_messages`, `send_queue_max_bytes` and
   `send_queue_stuck_timeout` (and getters on `Config`). The defaults are the values above; the
   60 s is also the minimum interval between drop summary log lines. KDF's own gossipsub
   configuration keeps the defaults; there is no `MM2.json` setting.

`src/behaviour/tests.rs` changes only for the new `Handler::new` arguments.

**Tests.** `cargo test --manifest-path vendor-patches/libp2p-gossipsub-0.45.0/Cargo.toml --lib -- send_queue:: handler::tests`
(29 tests), and `cargo test -p mm2_p2p --test gossip_send_queue` (in-process, fast tests).
The six upstream failures listed under "Test status" are unchanged.

## Pressure signal and priority lane (not security-related)

An amendment to the send queue above, for the maker publication rate control: CRD ch.28 §28.9B RQ1
items 8 and 9 and CRD ch.32 §32.9B (Reloaded-original). The wire is unchanged.

**Why.** A node that publishes faster than a relay reads cannot move a keep-alive past the messages
it has already queued. The application needs to know when a connection is falling behind, and
cancellations and swap negotiation messages must not wait behind queued order updates.

**What changed.**

1. **`src/send_queue.rs`** -- `SendQueue` gains a *priority lane*, a separate FIFO that `pop`
   serves before everything else. A priority RPC does not count toward the data cap and is never
   dropped. A lane that already holds 256 RPCs (`priority_max`) refuses the next one and reports an
   overflow once. It also derives a *pressure* level from the data queue: raised when the queue
   reaches 256 data RPCs (`pressure_raise`) or a period at the cap starts, cleared when it is at 64 or
   fewer (`pressure_clear`) with no period running. `take_pressure_change` hands out each change once.
   `SendQueueStats` (queued data RPCs and dropped count, atomics shared with the behaviour) and
   `SendQueueSnapshot` serve the metrics.
2. **`src/handler.rs`** -- `HandlerIn::PriorityMessage` queues into the lane. `HandlerEvent`
   gains `SendQueuePressure { raised }`, emitted from `poll`; a lane overflow is reported as
   `SendQueueStuck` (the behaviour closes the connection) with a log line naming the peer.
3. **`src/behaviour.rs`** -- `publish_with_priority`, `publish_from_with_priority` and
   `propagate_message_with_priority` mark every copy of the message they write as priority
   (`forward_msg`, `send_message` and `notify_primary` take the mark; all other callers pass
   `false`). Gossipsub does not tell message kinds apart; the application chooses. The behaviour
   turns `SendQueuePressure` into `Event::SendQueuePressure { peer_id, connection_id, raised }`,
   reports a closed connection as cleared, and keeps the counters of closed connections in the
   dropped total. `send_queue_snapshot()` returns the sums, `relay_send_queue_pressure()` tells
   whether any connection to a connected relay is raised. `Event::RelayMeshJoined { peer_id }` is
   emitted when a connected relay is added to the relay mesh (used for the keep-alive round after a
   reconnect).
4. **`src/config.rs`** -- `ConfigBuilder::send_queue_pressure_raise`, `send_queue_pressure_clear`
   and `send_queue_priority_max` (and getters); defaults 256, 64 and 256. KDF's own configuration
   keeps the defaults.

**Queue signal and relay close** (CRD ch.32 §32.9B R-MP2 item 2, ch.28 §28.9B RQ1 item 8, amended
2026-10-28). The maker publication budget needs two signals that a relay is not keeping up.

5. **`src/send_queue.rs`** -- the *queue signal*: raised when the data queue has held at least one
   data RPC for 2 seconds without becoming empty, or the pressure level of item 8 is raised; cleared
   when the data queue has been empty for 5 seconds and the pressure level is cleared. Timing takes
   the current time as an argument (`update_signal`, `signal_deadline`, `pop_at`).
6. **`src/handler.rs`** -- `HandlerEvent::SendQueueSignal { raised }`, reported once per change. The
   handler's timer now serves the earliest of the stuck deadline and the signal deadline.
7. **`src/behaviour.rs`** -- `Event::SendQueueSignal`, `relay_queue_signal()` (the signal is raised
   toward a connected relay), `queue_signals_raised()` (how often it was raised toward a relay) and
   `Event::RelayConnectionClosed { peer_id }`, emitted when the last connection to a relay closes.
8. **`src/config.rs`** -- `send_queue_signal_raise_after` and `send_queue_signal_clear_after`, for tests.

**Tests.** Unit tests in `send_queue.rs` and `handler.rs` (included in the filter above) and four
in `src/behaviour/tests.rs`, run with the filter `test_priority_ test_send_queue_ test_relay_mesh_join`;
in-process tests in `mm2_p2p/tests/gossip_send_queue.rs`.

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
→ **158 passed, 6 failed** (124 passed before the send queue change added 15 tests, and 13 more with
the pressure signal and the priority lane, and 6 more with the queue signal).

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
