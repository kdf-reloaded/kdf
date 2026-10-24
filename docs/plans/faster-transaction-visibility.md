# Plan: faster transaction visibility (push notifications and pending receipts)

**Status:** done (both workstreams shipped 2026-08-09/11, with tests) — **Last reviewed:** 2026-10-18

> **Before deleting this at a release** (the lifecycle in
> [`docs/GIT_FLOW_AND_WORKING_PROCESS.md`](../GIT_FLOW_AND_WORKING_PROCESS.md#plans-and-the-changelog)):
> the behaviour is bound in the CRD — ch.38 R38.6.5 for workstream A, ch.39 R39.8.0af–aj for
> workstream B — but this work predates the project's pull-request flow, so the three resolved
> decisions in the progress note below (no subscription cap, per-token QRC20 subscription,
> history refreshing on the same signal) exist **only here**. Move them into ch.38/ch.39 first,
> or keep this file.

> **The "Current state" section below describes the tree as it was before this plan ran, and
> is kept for that reason.** Both workstreams are implemented — see the progress note under
> *Sequencing* — so statements like "no `blockchain.scripthash.subscribe` call exists in the
> tree" are history, not the present: the subscription, the watcher registry, the
> re-subscription on reconnect, the history/balance wake and the shielded pending receipts
> are all in `mm2src/coins/utxo/rpc_clients/electrum_rpc_client.rs` and
> `mm2src/coins/z_coin.rs` today. All three open decisions are resolved there too.
>
> **Context:** created after the 2026-08-08/09 wallet-log maintenance pass, which showed a
> user waiting minutes for a deposit to appear and eventually restarting the
> wallet to force it. Two independent causes were confirmed: balances are
> refreshed only on a timer, and unconfirmed shielded receipts are not
> represented at all.

## Goal

Reduce the time between a transaction existing on-chain (or in a mempool) and
the user seeing it, without changing any swap protocol, wire format, or
netid-`8762`/`6133` observable behaviour. This is an enhancement, not a
divergence: it changes *when* the wallet notices things, never *what* it does
about them.

Two workstreams share that goal and this document, because they solve the same
user-visible problem from opposite ends:

- **A — Push notifications** (Electrum/UTXO family): stop waiting for the next
  poll tick when the server can tell us immediately.
- **B — Pending shielded receipts** (ZHTLC/ARRR): represent a payment that
  exists but is not yet mined.

## Current state

Verified against this repository, not assumed.

**Balance updates are pure polling.** `stream::balance::enable` spawns a per-coin
loop with a default 30 s period and a 10 s floor
(`mm2src/mm2_main/src/rpc/streaming_activations/balance.rs`). Nothing shortens
that interval when a transaction actually arrives; the coin's own RPC client may
already know, but nothing connects the two.

**The Electrum client already receives server-initiated notifications, for
exactly one method.** `mm2src/coins/utxo/rpc_clients/electrum_rpc_client.rs`
parses a subscription-notification response variant and routes it, but the
dispatcher matches a single hard-coded method id — the headers subscription —
and every other notification falls through a catch-all that logs and drops it.
The transport half of push already works; only the routing and the subscription
lifecycle are missing.

**A per-connection setup hook already exists.** The client publishes each newly
established connection on an `mpsc` channel (`on_connect_tx` / `on_connect_rx`
in `mm2src/coins/utxo/utxo_builder/utxo_coin_builder.rs`), already consumed by a
loop that negotiates protocol version per connection. Re-subscription after a
reconnect or a server swap has a natural home there and does not need inventing.

**Shielded light mode has no pending concept.** `my_balance` reports
`unspendable: 0` unconditionally (`mm2src/coins/z_coin.rs`); its balance comes
from the scanned wallet database, which by construction holds only mined,
scanned notes. Native mode does count 0-confirmation notes as unspendable.

## Push availability across coin families

The stated goal was consistency from the user's perspective, so this was checked
per family rather than assumed.

| Family | Push mechanism | Reachable today? |
|---|---|---|
| UTXO + QTUM (Electrum) | `blockchain.scripthash.subscribe` — server-initiated notification on scripthash status change | **Yes.** Standard ElectrumX method; our client already has the notification path |
| EVM (ETH/BEP20/PLG20/…) | `eth_subscribe` (`newHeads`, `logs`) | **No.** Requires a persistent WebSocket or IPC connection; our transport is HTTP-only (`Vec<http::Uri>` in `mm2src/coins/eth/alloy_compat/transport.rs`) |
| ZHTLC (ARRR) | — | **No.** The vendored lightwalletd contract (`mm2src/coins/z_coin/service.proto`) offers only a *request-scoped* mempool stream; there is no persistent server-push method. Its own comment notes results may be seconds stale |

So genuine push exists for one family. Two consequences shape the design:

1. **EVM push is a separate, larger project.** Adding a WebSocket transport is
   real work, and many public EVM RPC providers gate WebSocket behind paid
   tiers, so it cannot be assumed available for a user-supplied URL. Out of
   scope here; see *Out of scope* below.
2. **ZHTLC cannot be push at all** under the protocol we speak. Workstream B
   therefore improves *what* is represented (pending receipts) rather than *how
   fast* it is noticed.

**Consistency is achieved at the architecture layer, not the transport layer.**
Every family funnels into one refresh-trigger abstraction (below). Push fires it
where push exists; a timer fires it everywhere else. Consumers cannot tell the
difference, latency differs, and a family can be upgraded later without touching
any consumer. Pretending all three can be equally fast would be the wrong
promise; making them equally *structured* is achievable now.

## Design: one refresh trigger, several sources

Introduce a per-coin refresh signal that any source can fire and the balance
streamer (and any future consumer) awaits alongside its timer.

- The existing poll **remains** and is the correctness guarantee. Push is an
  accelerator layered on top, never a replacement.
- A missed, dropped, or silently-dead subscription therefore degrades to
  today's behaviour rather than to a wallet that never updates. This is the
  single most important property in this plan and it is what makes push safe to
  ship incrementally.
- The signal is coalescing: several notifications arriving before a refresh
  completes result in one refresh, not a queue.

This inverts the usual framing: the feature is not "replace polling with push",
it is "let push shorten the wait, and keep polling as the floor".

## Workstream A — Electrum scripthash subscriptions

**This is an unimplemented existing requirement, not new scope.** CRD ch.38
R38.6.5 already binds it: *"For Electrum-backed UTXO coins the project shall emit
balance-change events over the streaming infrastructure, registering the
addresses to be watched at enable time (and re-registering as needed)."*

What exists today satisfies only the weaker half. The balance streamer tracks a
"watch address" and re-reads it when the active address rotates
(`mm2src/mm2_main/src/rpc/streaming_activations/balance.rs`), but that is local
bookkeeping deciding *which address to poll* — nothing is registered with any
server. No `blockchain.scripthash.subscribe` call exists in the tree. So
balance-change events are emitted (by polling), while the registration and
re-registration the requirement names are absent.

Workstream A implements that missing half. No CRD amendment is needed for the
mechanism itself; the open decisions below (subscription cap, QRC20 trigger) may
warrant one.

### A1. Notification routing

Replace the single hard-coded method match in the notification dispatcher with a
registry keyed by subscription method, so more than one subscription kind can be
routed. Unknown methods must remain non-fatal.

### A2. Subscription registry and address resolution

A scripthash notification carries a scripthash and a status hash, not an
address. A reverse map from scripthash to (coin, address) is required, populated
when a subscription is created.

Bound it explicitly: HD wallets have many addresses, and subscription count
scales with watched addresses. Decide and record a cap and what happens when a
wallet exceeds it (degrade that coin to polling only — never fail activation).

### A3. Lifecycle across reconnect and failover

The hardest part, and where most of the effort and tests belong. The client
already rotates and replaces servers, so subscriptions must be re-established on
each new connection via the existing `on_connect` stream. Requirements:

- Re-subscribe every watched scripthash on a newly connected server.
- Never assume a subscription survived a reconnect.
- Treat a re-subscription failure as "this coin is polling-only for now", not as
  an error surfaced to the user.

### A4. Wire the trigger

On a status-hash change, fire the coin's refresh signal. Deliberately do **not**
attempt to derive the balance delta from the notification: the status hash says
"something changed", and the authoritative read is the existing balance/history
path.

### A5. Coverage

UTXO family plus QTUM. QRC20 tokens ride the QTUM platform connection; confirm
whether a token's balance refresh can be triggered by the platform coin's
notification, or whether tokens need their own trigger.

## Workstream B — Pending shielded receipts (ZHTLC)

Specification is already approved: CRD ch.39 §39.8.0.6 (R39.8.0af–aj), tests
T39.8.0f/T39.8.0g, tracked as unimplemented under D39.8.0c. This plan does not
restate the requirements; it records the implementation shape and the risks.

### B1. Source

Call the vendored request-scoped mempool method on the same coin-lifetime task
that already performs the post-activation sync
(ch.39 §39.8.0.5), reusing its poll period rather than adding a second timer.

### B2. Detection

Trial-decrypt mempool compact outputs with the wallet's incoming viewing key.
The compact ciphertext carries enough to recover the note value, so no full
transaction fetch is needed. `try_sapling_compact_note_decryption` is available
in the `sapling-crypto` crate already in our dependency graph.

### B3. Representation and safety

Pending amounts appear **only** in the non-spendable balance field. This was
verified as safe: every swap path reads `my_spendable_balance()`
(`mm2src/mm2_main/src/lp_swap/check_balance.rs`,
`taker_swap.rs`, and both v2 state machines), and the unspendable field appears
only in tests and the display/streaming path. A pending receipt therefore cannot
size a trade or enable a spend.

### B4. Invalidation

Recomputing the pending set from scratch on each poll handles mined, dropped,
and expired transactions without bespoke invalidation logic. The one residual
risk is a transient double-count if a transaction is mined between the mempool
read and the wallet read; poll the mempool **after** the wallet scan within a
pass and exclude transaction ids already present in the wallet database.

### B5. Verification risk — read before implementing

Trial decryption cannot be verified against a live lightwalletd in the current
development sandbox (no outbound network). Its characteristic failure is
**silent**: incorrect key or type plumbing yields "no notes found", which is
indistinguishable from an empty mempool and identical to today's behaviour.

Mitigation, required rather than optional: build a fixture-based test that
constructs a known encrypted output for a known viewing key and asserts positive
detection *and* correct value. A test that only asserts "no crash" or "empty
result" would pass against a completely broken implementation. This is the
acceptance gate for B.

## Out of scope

- **EVM WebSocket transport / `eth_subscribe`.** Larger project; provider
  support cannot be assumed for caller-supplied URLs. If pursued later, it slots
  into the same refresh-trigger abstraction with no consumer changes — that is
  the main reason to build the abstraction now rather than wiring Electrum
  notifications straight into the balance streamer.
- **Replacing polling anywhere.** The poll is the correctness floor.
- **Lowering the existing 10 s poll floor.** Push makes it unnecessary where it
  works; changing it is a separate decision with load implications for public
  Electrum servers.

## Sequencing

Each stage is independently shippable and independently revertible.

> **Progress (2026-08-09).** Both workstreams are implemented. Workstream A
> landed as notification routing, the script-hash watcher registry,
> `subscribe_scripthash` / `resubscribe_watched_scripthashes`, re-arming on every
> newly connected server, and the balance streamer subscribing its coin's
> address and racing notifications against the poll deadline. Workstream B
> landed as mempool-derived pending receipts.
>
> All three open decisions are resolved. History refresh (3): the history loop
> wakes on the same signal. Subscription cap (1): **no cap** — an Iguana wallet
> contributes one subscription per activated Electrum coin, an HD wallet one per
> known address, and a 10,000-watcher stress test registers and notifies in
> milliseconds, so the ceiling is orders of magnitude above any real wallet.
> QRC20 trigger (2): per-token contract-event subscription, because a token
> balance is contract storage and need not disturb the holder's UTXOs.

1. **Refresh-trigger abstraction** + balance streamer awaits it. No behaviour
   change on its own; everything else builds on it.
2. **A1 + A2** — routing and the scripthash registry, subscriptions created but
   used only for logging. Proves notifications arrive without changing user-
   visible behaviour.
3. **A3** — reconnect/failover lifecycle. The riskiest stage; ship it before
   anything depends on subscriptions being reliable.
4. **A4 + A5** — fire the trigger; UTXO/QTUM coverage. First user-visible win.
5. **B1–B4** — pending shielded receipts, gated on B5's fixture test.

Stages 1–4 and stage 5 are independent and may proceed in either order or in
parallel.

## Risks

| Risk | Mitigation |
|---|---|
| Silently dead subscription | Poll retained as floor; a stale subscription costs latency, never correctness |
| Subscription storm on a large HD wallet | Explicit cap; degrade to polling rather than failing activation |
| Extra load on public Electrum servers | Subscriptions are cheaper than the polling they displace; do not lower the poll floor at the same time |
| Shielded trial decryption silently no-ops | Fixture test asserting positive detection and value (B5) is the acceptance gate |
| Transient pending/confirmed double-count | Order mempool read after wallet scan; exclude already-scanned transaction ids |

## Testing

- Notification routing: unknown methods are ignored without disturbing the
  connection.
- Re-subscription: simulated disconnect and server swap re-establishes every
  watched scripthash; failure leaves the coin polling-only.
- Trigger coalescing: N rapid notifications produce one refresh.
- Fallback: with subscriptions disabled entirely, behaviour is byte-identical to
  today.
- Shielded: fixture-based positive detection with correct value (B5); pending
  never appears in spendable; mined transaction transitions without a
  double-count.

## Open decisions

1. **Subscription cap per coin**, and HD-wallet behaviour above it.
2. **QRC20 trigger**: platform-coin notification versus per-token subscription
   (A5).
3. Whether the refresh trigger should also drive **transaction history**
   refresh, or balance only in the first iteration.

## References

- CRD ch.39 §39.8.0.5, §39.8.0.6, R39.8.0af–aj, D39.8.0c — shielded background
  sync and pending receipts.
- CRD ch.38 R38.6.5 — the governing requirement for workstream A: balance-change
  events with addresses registered at enable time and re-registered as needed.
- CRD ch.38 §38.6.4 — bounded and ordered Electrum connection management; A3
  must not weaken it.
- [`qtum-qrc20-stabilization.md`](qtum-qrc20-stabilization.md) — overlaps A5;
  shared-client decisions there affect where QRC20 subscriptions live.
