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

## Removing this patch

Delete the directory, drop the `libp2p-yamux` line from the
`[patch."https://github.com/KomodoPlatform/rust-libp2p.git"]` stanza and the
`exclude` entry, once a `KomodoPlatform/rust-libp2p` tag ships a `libp2p-yamux`
that no longer depends on `yamux012` (upstream dropped it after 0.44), or the
ch28 modernization moves the pin to upstream libp2p.
