# Plan: QTUM/QRC20 activation, transport, history, and swap stabilization

> **Status:** analysis and implementation plan only; not started.
>
> This plan was created after the 2026-08-05 wallet-log maintenance pass.
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
