# libp2p-yamux 0.44.0 — vendor patch

## Provenance

Copied verbatim from `KomodoPlatform/rust-libp2p`, tag `k-0.52.12`
(commit `8bcc1fda79d56a2f398df3d45a29729b8ce0148d`), path `muxers/yamux`, then
patched. Redirected into the graph with:

```toml
[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]
libp2p-yamux = { path = "vendor-patches/libp2p-yamux-0.44.0" }
```

## Why it exists

`yamux` 0.12.1 carries **GHSA-vxx9-2994-q338**: a crafted inbound Data frame
with SYN set and a body larger than `DEFAULT_CREDIT` panics the connection state
machine. On the first packet of a new inbound stream the stream state is created
and a receiver queued *before* oversized-body validation completes; when
validation then fails, cleanup calls `remove(...).expect("stream not found")`.
Remotely reachable over a normal Yamux session, no authentication. There is no
fix on the 0.12 line — 0.13.10 is the fix.

Upstream `libp2p-yamux` 0.44.0 depends on **both** yamux lines
(`yamux012` and `yamux013`) and wraps them in an `Either`. `Config::default()`
selects 0.13, but **any** call to a `Config` setter switches the connection back
to 0.12 — upstream asserted exactly that in its own test,
`config_set_switches_to_v012`, as intended behaviour. That includes
`set_max_num_streams`, which is *not* deprecated and looks entirely innocuous.

KDF calls only `libp2p::yamux::Config::default()`
(`mm2src/mm2_p2p/src/atomicdex_behaviour.rs:1213`), so we were on the patched
line — by accident, and one ordinary-looking config call away from not being.
This patch removes the accident.

## The patch

`yamux012` is removed from `Cargo.toml`, and with it every code path that used
it. The crate now speaks only yamux 0.13, so the 0.12 crate is not in the
dependency graph at all — `cargo tree -i yamux@0.12.1` returns nothing.

- `Muxer::connection`, `Stream` and `Error` drop their `Either` wrappers and
  hold the 0.13 types directly; `either` is no longer a dependency.
- `Config` is a newtype over `yamux013::Config` instead of
  `Either<Config012, Config013>`, so there is no fallback to select.
- **Removed public API** (all deprecated upstream, all unused here — verified
  across both this workspace and the rest of the fork): `Config::client`,
  `Config::server`, `WindowUpdateMode`, `set_window_update_mode`,
  `set_receive_window_size`, `set_max_buffer_size`. Deleted rather than made
  no-ops, deliberately: a future attempt to use one is now a compile error
  instead of a silent downgrade onto the vulnerable line.
- **Kept**: `set_max_num_streams`, which yamux 0.13 supports natively
  (`yamux::Config::set_max_num_streams`). Unlike upstream it no longer drags the
  connection back to 0.12.

Upstream's `config_set_switches_to_v012` test asserted the behaviour this patch
removes, so it is replaced by `configuring_does_not_leave_the_v013_line`, which
asserts the opposite.

## Behavioural note

yamux 0.13 is not a wire-protocol change — both speak `/yamux/1.0.0` and
interoperate. What differs is flow control: 0.13 always behaves as
`WindowUpdateMode::OnRead` (back-pressure from the reader) and manages the
connection receive window itself, whereas 0.12 defaulted to `OnRead` here but
allowed `OnReceive`. Since this workspace never configured either, the effective
behaviour before and after this patch is the same.

## Test status

`cargo test --manifest-path vendor-patches/libp2p-yamux-0.44.0/Cargo.toml` →
**3 passed, 0 failed**: upstream's own `tests/compliance.rs`
(`close_implies_flush`, `read_after_close`) plus the replacement unit test.

In the workspace, `cargo test -p mm2_p2p` → 19/19, including the mesh tests that
actually open multiplexed streams between two nodes
(`test_publish_reaches_subscriber_via_relay`,
`test_subscribe_propagates_to_remote_peer`, `test_request_response_ok_three_peers`).

## Patch 2: bounded close (CRD ch.28 R27c)

### Why it exists

A close that this node starts sends yamux's GoAway frame and flushes the socket, and the swarm counts the
connection as open until `StreamMuxer::poll_close` has finished. Toward a peer that reads nothing (a hung
process whose kernel keeps the TCP session open with a zero receive window) the flush never completes. The
connection, its gossip queue and the multiplexer's buffers stay held, the peer counts as connected, and the
relay maintenance never redials it. In a lab run the close of a stuck peer took 36 seconds and ended only
because the peer's kernel reset the connection. `libp2p-swarm` (not vendored, by decision) awaits the
muxer's close without a deadline, so the deadline lives here, in the layer every transport of this node
runs (R27c item 5).

R27c: every close this node starts ends within 30 seconds. After that, `poll_close` returns
`Ready(Err(..))` whose cause is an `std::io::Error` of kind `TimedOut` with the message "close timed out".
The swarm then reports the connection closed with an IO cause, and drops the muxer and its transport
stream. Note that `StreamMuxerBox` wraps the muxer's error in an `io::Error` of kind `Other`, so the
`TimedOut` is the innermost error; the Debug form of the swarm's cause contains both, and the text "close
timed out" appears in the log line (R27c item 2).

### The hunks

All in `src/lib.rs`. To re-apply after a bump of the fork, look for the comments starting `PATCHED (CRD
ch.28 R27c`.

1. Imports: `use std::time::Duration;` and `use futures_timer::Delay;`.
2. `Muxer<C>` gets three private fields, all initialized in `Muxer::new`:
   `close_timeout: Duration` (= `CLOSE_TIMEOUT`), `close_deadline: Option<Delay>` (= `None`) and
   `close_timed_out: bool` (= `false`).
3. New items next to `MAX_BUFFERED_INBOUND_STREAMS`: `const CLOSE_TIMEOUT: Duration =
   Duration::from_secs(30);` and `fn close_timed_out() -> Error`, which builds
   `Error(yamux013::ConnectionError::Io(io::Error::new(io::ErrorKind::TimedOut, "close timed out")))`.
4. `Muxer::poll_close` is replaced by:

   ```rust
   let this = &mut *self;
   if this.close_timed_out {
       return Poll::Ready(Err(close_timed_out()));
   }
   let timeout = this.close_timeout;
   let deadline = this.close_deadline.get_or_insert_with(|| Delay::new(timeout));

   if let Poll::Ready(result) = this.connection.poll_close(cx) {
       return Poll::Ready(result.map_err(Error));
   }
   if deadline.poll_unpin(cx).is_ready() {
       this.close_timed_out = true;
       this.close_deadline = None;
       return Poll::Ready(Err(close_timed_out()));
   }
   Poll::Pending
   ```

   The deadline starts at the first `poll_close`. The inner close is polled first, so a close that
   finishes in time behaves exactly as before. After the timeout the inner connection is never polled
   again.
5. `Cargo.toml`: `futures-timer = "3.0"`. No new crate enters the graph: `futures-timer` is already
   resolved for `libp2p-core` and `libp2p-swarm`.

The 30 seconds are a constant of the crate, not a setting. Only the `test` module of the crate can shorten
them (it assigns the private field `close_timeout`); no public API changed.

On `wasm32`, the deadline is kept on every target. `futures-timer` has a browser implementation behind its
`wasm-bindgen` feature, and `libp2p-swarm` already uses the same `Delay` type for its own connection
timers in the browser build. Checked with `cargo tree -i futures-timer --target wasm32-unknown-unknown -e
features` on 2026-11-07: the feature is on in the workspace's browser build (`gloo-timers`, `send_wrapper`
and `wasm-bindgen`), and `libp2p-yamux` is one of the direct dependents. The deadline adds no feature of
its own, so a change that turned the feature off would break the swarm's timers first.

### The tests

In `src/lib.rs`, module `test` (CRD ch.28 T14, crate level), over a transport stream whose writes either
stay pending forever or are taken at once:

- `close_toward_a_peer_that_reads_nothing_times_out_at_the_deadline`: the close is pending before the
  deadline and ends with the `TimedOut` "close timed out" error after it, and the inner stream is not
  touched again afterwards;
- `close_toward_a_peer_that_reads_ends_at_once`: the close ends at once with `Ok`;
- `close_deadline_is_thirty_seconds`: the constant is 30 s.

`cargo test --manifest-path vendor-patches/libp2p-yamux-0.44.0/Cargo.toml` runs them together with the
compliance tests and the GHSA test of the first patch; CI runs it in the `bounded-close` row of the
`unit-tests` matrix.

## Removing this patch

Delete the directory, drop the `libp2p-yamux` line from the
`[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]` stanza and the
`exclude` entry, once a `KomodoPlatform/rust-libp2p` tag ships a `libp2p-yamux`
that no longer depends on `yamux012` (upstream dropped it after 0.44), or the
ch28 modernization moves the pin to upstream libp2p.
