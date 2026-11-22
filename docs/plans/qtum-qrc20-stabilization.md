# Plan: QTUM/QRC20 activation, transport, history, and swap stabilization

**Status:** in progress (P1 #354, P0 #356, P2 #377, P3 #385 and #406, P6 #417 merged; P4 recorded in CRD ch.38 after D38.6.4Q.1; P5 #427 and P7 #436 merged; Phase C item 3 #462 and item 5 #476 merged; P8 in PR #473; P9 next) — **Last reviewed:** 2026-11-22

> **Context:** this plan was created after the 2026-08-05 wallet-log maintenance pass.
> That run showed long-lived QRC20 history failures and separate QTUM and QRC20
> Electrum activity against the same platform servers. The retry loop has been
> made less noisy, but this plan deliberately does not treat reduced logging as
> a functional fix and does not yet prescribe a shared-client implementation.

## Goal

Make QTUM and its QRC20 tokens behave as one coherent platform family for
activation lifetime, RPC transport, QTUM gas-UTXO ownership, transaction
history, and swaps, while retaining all existing public RPC, configuration,
transaction, and swap-wire contracts.

The first implementation step must prove which resources are safe to share.
QTUM and a QRC20 token use the same chain and wallet-owned QTUM inputs, but they
remain separately addressable tickers with distinct balances and history. A
single connection pool may be desirable; assuming that every per-coin object
can be merged is not.

## Governing CRD and existing evidence

- [Chapter 38](../reloaded-rewrite/38-utxo-coin-maintenance-and-features.md)
  §38.1 requires Qtum-specific behavior to remain separated from generic UTXO
  behavior. Section 38.6.4 binds ordered and bounded Electrum connection
  management, sequential block discovery, race-free protocol negotiation, and
  bounded confirmation waits.
- [Chapter 10](../reloaded-rewrite/10-sse-streaming.md) §10.18 R41-R43 includes
  Qtum-family transaction history in the reactive `TX_HISTORY:<ticker>`
  contract. A successfully enabled stream must have a real history producer;
  it cannot be a dormant subscription.
- [Chapter 48](../reloaded-rewrite/48-platform-coin-task-activation.md) records
  `task::enable_qtum` as an existing **standalone-coin** task family. Its generic
  platform-with-tokens requirements apply to EVM and Tendermint, not to QRC20.
  This plan must not silently convert QTUM/QRC20 to that wire surface.
- [Chapter 2](../reloaded-rewrite/02-baseline-state.md) records QRC20 as part of
  the allowed baseline coin surface. Public QRC20 activation and transaction
  shapes therefore remain compatibility constraints even where later CRD
  chapters do not yet describe their internals in detail.
- [Disabled tests](../DISABLED_TESTS.md) Group 4 records missing deterministic
  coverage for both QRC20-to-UTXO and UTXO-to-QRC20 swaps because the QTUM
  Docker fixture is not stable.
- The current QRC20 transaction constructor already notes that QRC20 tokens
  and their base QTUM coin need coordinated recently-spent state. The current
  native verbose-transaction cache is named by platform ticker, but each coin
  object still constructs its own UTXO fields and RPC client. These facts are
  audit inputs, not approval for a particular refactor.

### CRD gap checkpoint

The CRD binds Qtum specialization, Electrum behavior, history streaming, and
the existing activation task surface, but it does **not** currently bind:

- whether QTUM and QRC20 tickers share one live Electrum client;
- ownership and lifetime rules for platform-scoped recently-spent state;
- activation/deactivation ordering between QTUM and its tokens;
- server capability selection for QRC20-specific Electrum methods; or
- coordinated activation of QTUM with multiple QRC20 tokens.

Before implementation, update Chapter 38 with the independently designed
observable requirements and acceptance tests. If compatibility research is
needed for an externally observable behavior not already covered by the CRD or
public API, obtain operator approval for the Spec Reader and Dirty Gate
workflow. Internal resource sharing that leaves all observable behavior
unchanged does not by itself require forbidden-corpus research.

## State check (2026-11-15)

A check of this plan against the code at the 0.4.0-beta.1 release found phases A–F not started. Two pieces came from
other work: the QRC20 history retry backs off and resets after a success, and activations of one ticker are serialized
(CRD ch.55 R55.1.12). The five gaps of the checkpoint above are still gaps. What the code does today:

- **One Electrum pool per ticker.** A QRC20 token builds Electrum client fields of its own. QTUM with *n* tokens is
  *n* + 1 pools against the same servers, each with its own probe, version negotiation, maintenance, ping and
  measure loops, ranking and disagreement state.
- **No failover after an error answer.** A server that answers a QRC20 contract method with an error is not replaced
  for that request (CRD ch.38 R38.6.4f item 1). The contract methods are not on the read-only list (R38.6.4a).
- **Spent inputs.** QRC20 sends record no spent QTUM inputs, and QTUM's record is per instance.
- **Delegation and the build lock.** The delegation builds did not hold the build lock; fixed in PR #354.
- **Disable lifetime.** A history loop checks only that its ticker exists, and the balance streamer outlives a
  disable (the streamer for every coin).
- **Tests.** The offline QRC20 unit tests run in no CI cell; the QTUM docker image is not pinned.
- **CRD.** Two ch.38 passages said that a QRC20 token answers its disagreement state and reads its fee through the
  platform coin's client; it uses its own. They carry dated corrections since 2026-11-15.

Work packages, in order, each one PR with its docs:
1. P1, the delegation build lock (PR #354);
2. P0, these CRD corrections;
3. P2, CI coverage and a pinned docker image;
4. P3, the Phase A harness and its measurements (the gate of Phase A);
5. P4, the sharing design (Phase B);
6. P5, the shared pool (Phase C);
7. P6, history reliability (Phase D);
8. P7, spent inputs across QTUM and its tokens;
9. P8, the balance streamer after a disable;
10. P9, the lab rows and rollout (Phase F).

Decided 2026-11-15: the ch.38 passages are corrected now, by dated notes; P7 corrects a transaction path and does not
change what a swap pays; P8 stays in this plan. Waiting for the measurements of P3: whether tokens share their
platform's pool, how a server's error answer to a contract method is handled, and whether the contract-event
subscription is placed again after a reconnect. P3 to P6 use regtest; funds for P9 are decided when P9 is near.

*P8 (2026-11-21).* PR #439 ends the background work and the event streams of a removed coin, the balance streamer
included, for every coin. P8 keeps what it does not cover for QTUM and its tokens: a removed instance's streamer must
not add its owner to the shared client's subscriptions again, and a contract-event subscription of the same owner
must not be skipped after a failed placement. Tests come first, on dev with PR #439; code changes only if a test
fails. P8 starts after PR #439 is merged.

*P8 (2026-11-22).* On dev with PR #439, the tests found two defects in the subscriptions of the shared client, and
PR #473 fixes them: a subscription request that waited for its answer while its instance was disabled, or one sent
later by a handle of the disabled instance, remembered that instance again; and a subscription whose placement over
a new session failed still counted as placed, so the next request of the same instance sent nothing. The rules are
in the status note of 2026-11-22 after D38.6.4Q.1 in CRD ch.38. A disabled instance's balance streamer and a
notification after the removal add nothing back (tests). Phase C item 5 is done (PR #476).

## Non-goals and compatibility guardrails

- Do not add a new activation RPC or change request/result/error JSON.
- Do not make QRC20 activation implicitly enable QTUM, or require QTUM to have
  been activated first, unless an approved CRD requirement explicitly binds
  that behavior.
- Do not merge QTUM and token balances, histories, ticker identities, or event
  streams.
- Do not broaden HD, hardware-wallet, or address-policy support as a side
  effect. Any such work belongs under Chapters 5, 7, and 38 with its own tests.
- Do not weaken server validation or suppress the first actionable transport or
  history failure merely to keep a token nominally enabled.
- Preserve swap transactions, QTUM gas accounting, QRC20 contract calls,
  confirmation rules, and both production-netid behavior exactly.

## Phase A -- reproduce and measure

1. Build a deterministic activation harness with a mock Electrum server that
   implements both standard QTUM methods and the QRC20 contract extensions.
2. Record connection, ping, version-negotiation, block-height, and history-call
   counts for these scenarios:
   - QTUM only;
   - one QRC20 token only;
   - QTUM followed by one token;
   - one token followed by QTUM;
   - QTUM plus two tokens;
   - repeated and concurrent activation requests.
3. Determine whether the observed duplicate connections are merely redundant
   resource use or create correctness risks during reconnect, disable, and
   concurrent transaction construction.
4. Reproduce QRC20 `blockchain.contract.event.get_history` failures with one
   incapable/failing server and one capable server. Verify whether the current
   client tries another connected server and whether a history failure changes
   activation or balance availability.

**Gate:** no ownership refactor starts until tests capture current connection
counts, task lifetimes, and failure behavior.

*Progress (2026-11-17).* The harness and the measurements are in PRs #385 and
#406: a stub Electrum server with the QRC20 methods, and offline tests for
every acceptance row of CRD ch.38 §38.6.4Q. Their results are in that
section's two "Measured" notes. The gate is met. Not covered:
- *Repeated and concurrent activation* (item 2, last bullet). Activations of
  one ticker are serialized since PR #156 (R55.1.12); a test for it is still
  owed.
- *The balance stream's own subscription inside the node.* It was skipped on
  purpose. The tests already pin what the token's client does with the
  subscription: it is never placed again after a reconnect, and an error or no
  answer breaks nothing. What remains for D38.6.4Q.3 is whether real QTUM
  servers answer the subscription at all, which a mock server cannot show. A
  read-only probe of the public servers decides that instead.

## Phase B -- platform-resource ownership design

Define the smallest platform-scoped resource boundary that can safely be
shared. Audit at least:

| Resource | Required decision |
| --- | --- |
| Electrum transport | Whether one pool can serve QTUM and all QRC20 methods while retaining per-ticker metrics and diagnostics. |
| Protocol negotiation and ping loops | One owner per physical pool; no duplicate background loops after token activation. |
| Server capabilities | How QRC20-specific method support is detected, selected, failed over, and re-evaluated after reconnect. |
| Recently-spent QTUM outpoints | One wallet-and-platform-scoped view so QTUM and token sends cannot independently select the same gas input. |
| Verbose transaction cache | Preserve the existing platform namespace while preventing concurrent file/store races. |
| History tasks | Keep one producer per ticker, with shared transport but independent cursors, storage, retry state, and SSE identity. |
| Metrics/log attribution | Preserve both the physical endpoint identity and the logical requesting ticker without double-counting connections. |
| Disable/re-enable lifetime | A token must not tear down resources still used by QTUM or another token; the last user must release them. |

Prefer an explicitly owned platform runtime registered in the existing coin
context over a new global singleton. Keying must include wallet identity,
platform ticker, RPC mode, and any server/config dimensions needed to prevent
cross-wallet or incompatible-client reuse.

**Gate:** document lifetime and lock ordering before code changes. Prove that
the design cannot cross wallet identities and cannot deadlock coin disable,
history, or transaction construction.

*Gate met (2026-11-18):* the status note after D38.6.4Q.1 in CRD ch.38 records
when instances share a client, its lifetime, reuse, lock order, naming and the
subscriptions of a disabled instance. The registry lives in the node's context,
which serves one wallet, so no client crosses wallet identities.

## Phase C -- Electrum and activation stabilization

1. Reuse only the resources approved in Phase B; retain separate logical QTUM
   and QRC20 coin objects.
2. Apply Chapter 38 §38.6.4 consistently to the shared or independent pool:
   configured order, bounded `min_connected`/`max_connected`, deterministic
   failover, sequential block discovery, and race-free version negotiation.
3. Deduplicate concurrent activation at the correct identity boundary. A
   repeated activation must return the existing compatibility error/result,
   not start another connection/ping/history set.
4. Make partial activation rollback explicit. A failed token activation must
   not unregister or poison a pre-existing QTUM platform, and a failed QTUM
   activation must not leave an undiscoverable resource owner behind.
5. Add bounded INFO diagnostics for ownership creation/reuse/release and TRACE
   attribution for individual calls. Avoid periodic success logs.

*P5 (2026-11-19).* PR #427 implements the shared client of D38.6.4Q.1, all six
items of its status note in CRD ch.38. Of the Phase C items above, it covers
items 1 and 4, and item 3 for QTUM and a token activating at the same time.
Item 2 holds for the shared client as for any other, because it is built by the
same code under §38.6.4. Still open in Phase C: a test of a repeated activation
of one instance (item 3), and the INFO lines for creating, reusing and releasing
a shared client (item 5).

*Item 3 (2026-11-21).* PR #462 adds offline tests of a repeated activation of
one instance. A second legacy activation of QTUM or of a token, one after the
other or started together, answers "already initialized" (CRD ch.55 R55.1.12)
and starts no session, tip subscription, client holder, contract-event
subscription or history collector. No code changed. Still open in Phase C: item
5.

*Item 5 (2026-11-22).* PR #476 adds the INFO lines decided on 2026-11-21:
one when a QTUM-family client is created, one per reuse, and one when it is
released, with the platform, the instance and the servers as `host:port` (CRD
ch.38, status note of 2026-11-22 after D38.6.4Q.1). The TRACE attribution of
individual calls named in item 5 was not part of that decision and is not
added.

## Phase D -- QRC20 history reliability

1. Separate transport failure, unsupported QRC20 method, malformed response,
   and chain/server internal error into typed internal outcomes while
   preserving the public error contract.
2. Route QRC20 history calls only to suitable servers and fail over before
   entering the bounded exponential retry cycle.
3. Keep token-history cursors and storage independent per ticker even when the
   RPC pool is shared.
4. Reset retry delay after a successful page and avoid tight retries when every
   suitable server reports the same failure.
5. Prove Chapter 10 behavior: newly observed QRC20 transfers produce
   `TX_HISTORY:<token>` events with the public `TransactionDetails` shape;
   QTUM history remains under `TX_HISTORY:QTUM`.

## Phase E -- spend and swap correctness

1. Add a deterministic concurrency test in which QTUM and a QRC20 token prepare
   transactions simultaneously from the same wallet. They must not reserve or
   spend the same QTUM input independently.
2. Extend this to two QRC20 tokens sharing the same gas wallet.
3. Restore the disabled Docker coverage with a pinned, health-checked QTUM
   image and deterministic funding/deployment setup:
   - QRC20 maker to UTXO taker;
   - UTXO maker to QRC20 taker;
   - both production netids where network policy affects the shared swap path;
   - refund/restart recovery for at least one direction.
4. Assert exact contract-call outputs, gas accounting, fee values, transaction
   serialization, and swap terminal state. Balance-only assertions are not
   sufficient.

*P7 (2026-11-20).* PR #436 implements D38.6.4Q.4 (b), as its status note in CRD
ch.38 records it: one recently-spent record per platform and address script for
QTUM and its tokens, whose lock orders the builds of that address. Items 1 and
2 are covered by offline tests with the stub server. Items 3 and 4 stay open;
the swap rows of this package run on a QTUM regtest in the lab.

*P7 (2026-11-21).* PR #436 is merged. Its lab rows passed: both swap directions
and back-to-back sends from one address on a QTUM regtest, and back-to-back
withdraws on RIN mainnet with the maturity check off and on. On the regtest the
build before P7 gave the same results, so the offline tests with the stub
Electrum server are the evidence of the fix.

## Phase F -- verification and rollout

- Focused unit tests for ownership keys, reference counts, server capability
  failover, retry reset, and shared outpoint reservation.
- `cargo test -p coins qrc20` and the relevant `coins_activation` tests.
- QTUM/QRC20 Docker activation, history, withdraw, and both swap directions.
- Native and `wasm32-unknown-unknown` checks for every affected supported path.
- Pinned formatting, scoped strict clippy, `cargo deny check advisories`, and
  native plus Windows GNU release builds.
- Compare INFO and TRACE logs: INFO must show a bounded lifecycle summary;
  TRACE may show per-server/per-token details without secrets or raw auth.
- Update Chapter 38 in the implementation commit, plus `CHANGELOG.md`,
  `docs/DISABLED_TESTS.md`, and operator documentation affected by activation or
  configuration behavior.

## Definition of done

- Connection/background-task counts are bounded and deterministic for QTUM
  plus multiple QRC20 tokens.
- Platform gas inputs and recently-spent state are coordinated without merging
  logical coin state.
- An incapable or failing server does not cause an endless high-frequency
  QRC20 history loop when another suitable server exists.
- Repeated/concurrent activation and disable/re-enable have deterministic,
  compatibility-preserving outcomes with no leaked task or half-registration.
- QTUM and each token retain independent balances, history, SSE identities,
  and public RPC shapes.
- Deterministic swap tests cover both QRC20/UTXO directions and exact
  transaction structure.
- Governing CRD and implementation agree, and no forbidden corpus was consulted
  by the implementation context.
