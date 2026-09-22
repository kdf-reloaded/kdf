# Chapter 52 — Version-Two Atomic-Swap State Machine

**Status:** driving-spec.

The two role-specific event-sourced state machines that execute the
version-two (trading-protocol-upgrade) atomic swap — their complete
state and event sets, every success, failure, refusal, abort and
timeout transition with its exact triggering condition, the ordering of
each transition relative to wire output and persistence, the
resume mapping after a restart, the peer-to-peer message contract and
timeout budget that drives them, the per-state reserved-funds semantics
including release on unsuccessful termination, and the field and type
expectations the version-two messages impose on their counterparties.

This chapter is the version-two counterpart of
[chapter 51](51-legacy-v1-swap-state-machine.md) and is deliberately
structured to be read side by side with it.

## 52.1 Executive Summary

[Chapter 13](13-swap-version-negotiation.md) binds the version tag that
selects between the legacy protocol (value `1`) and the version-two
protocol (values `2` and `3`), and chapter 13 R13 reduces the question
"should the version-two path run?" to a single predicate on that tag.
What that predicate dispatches *into* has, until now, been specified
only in fragments. [Chapter 14](14-state-machine-runtime.md) binds the
generic persistent state-machine runtime the version-two machines are
built on, but not the graphs they drive.
[Chapters 15](15-swap-v2-utxo-path.md),
[16](16-swap-v2-pre-burn-output.md) and
[17](17-swap-v2-evm-path.md) bind the per-coin-family implementations of
the coin-trait surface the machines call, and chapter 17 §17.10 sketches
the happy-path state sequence without binding its conditions.
[Chapter 33](33-swap-v2-wire-schema-embedding.md) binds the wire schema
the messages are encoded in, but not which message is sent in which
state, by whom, how often, or for how long it is awaited.
[Chapter 44](44-database-persistence-and-migrations.md) R44.8.3 and
R44.8.4 bind the persisted row fields and the fact that an event log is
appended by UUID, but not the event vocabulary or its meaning. This
chapter closes the remaining gap: the two role graphs themselves.

The version-two swap replaces the legacy five-stage single-payment
exchange with a two-stage taker flow (a funding output that a
cooperatively co-signed spend converts into the taker payment) and a
dual-secret maker payment, as bound by chapter 15 R8–R10. Both sides run
the chapter-14 storable runtime rather than the hand-rolled loop of
chapter 51: every non-initial state produces exactly one event on entry,
the event is applied and persisted *before* the state's own work begins,
and recovery re-enters the state named by the last persisted event.

Five properties of the version-two machines are specified here for the
first time and are the practical reason this chapter exists:

1. **Both refusal signals are defined and neither is transmitted.** The
   version-two schema carries two distinct refusal encodings: the
   taker's abort action inside its negotiation message, and the maker's
   negative acknowledgement with an optional reason. Each side decodes
   and correctly acts on the refusal it may *receive*. Neither side ever
   *sends* one: on every negotiation rejection each role transitions
   straight to its terminal abort state with no wire output at all. The
   result is symmetric to — and worse than — the legacy defect bound by
   chapter 51 R34: a rejected counterparty always waits out its full
   receive budget. R37–R41 bind the transmission obligation on both
   sides.
2. **Reservations are tracked by a different mechanism than V1, and the
   swap-start balance check cannot see them.** Chapter 51 R51 binds the
   legacy reservation to membership of the running-swap registry.
   Version-two swaps do not join that registry; they record a separate
   per-coin ledger entry keyed by swap identifier. The aggregate
   locked-amount query reads both sources, but the *self-excluding*
   variant used by every swap-start balance check reads only the legacy
   registry. A node with live version-two swaps therefore admits new
   swaps against a balance it has already committed. R60 and R61 bind
   the correction.
3. **The terminal abort state does not mean "nothing was committed".**
   Both machines can reach their terminal abort state *after* their own
   payment is on chain — the maker when its refund broadcast fails, the
   taker when secret extraction or the maker-payment spend fails. A
   consumer that reads the terminal abort event as "no funds moved" is
   wrong in exactly the cases that matter. R21, R34 and R44 bind this.
4. **One receive budget is not a budget.** Every version-two receive is
   bounded by a duration plus a fixed grace allowance. One call site —
   the maker awaiting the taker's payment-spend preimage — supplies an
   absolute wall-clock deadline where a duration is required, making
   that wait effectively unbounded and leaving the maker's committed
   payment exposed with no timeout into its refund path. R51 binds the
   value as a duration.
5. **Key widths are coin-family-determined, not fixed.** Chapter 51 R62
   fixes every legacy public-key field at 33 bytes for every chain, with
   a dictated padding convention (R64) for chains whose native key is
   not a 33-byte secp256k1 point. Version two has no such rule: the
   fields are length-unconstrained byte sequences and each coin family
   transmits its own natural encoding — 33-byte compressed for the UTXO
   family, 64-byte uncompressed coordinates for the EVM family. One of
   the two deployed parse paths performs no length check and aborts the
   process on a wrong-width field supplied by a counterparty. R66–R71
   bind the width contract and the mandatory length check.

Bound rules R1–R9 cover the runtime binding and lifecycle; R10–R22 the
maker state graph; R23–R36 the taker state graph; R37–R46 the refusal,
abort and timeout contract; R47–R56 the message contract; R57–R65 the
reserved-funds semantics; R66–R73 the wire field and type expectations;
R74–R78 the reference-version split.

## 52.2 Subsystem Shape

The substrate is two parallel role-specific machines plus four shared
services they both consume:

| Surface                        | Responsibility                                                                     |
| ------------------------------ | ---------------------------------------------------------------------------------- |
| Maker state machine            | Twelve states, eleven event types (R10–R22).                                        |
| Taker state machine            | Sixteen states, fifteen event types (R23–R36).                                      |
| Per-swap message inbox         | Single-slot-per-message-kind store, sender-pinned (R47–R50).                        |
| Repeating broadcast service    | Re-transmits one message on an interval until cancelled or its state exits (R52).   |
| Per-coin reserved-amount ledger| Per-swap entries keyed by coin ticker; the version-two reservation record (R57).    |
| Per-swap exclusion lock        | Time-to-live lock preventing two runners for one swap identifier (R6).              |

Both machines are anchored to the same per-swap publish-subscribe topic,
which is the version-two swap topic prefix joined to the swap's
identifier. This topic is distinct from the legacy swap topic of
chapter 51; the two protocols never share a topic, so a peer that
understands only one of them never sees traffic it cannot decode.
Both machines subscribe to the topic when the run begins and unsubscribe
when it ends.

The substrate has five external dependencies it does not own: the
chapter-14 storable runtime; the version-two coin-trait surface bound by
chapters 15 and 17; the dex-fee descriptor and its arithmetic bound by
[chapter 08](08-fee-routing-engine.md) and
[chapter 16](16-swap-v2-pre-burn-output.md); the wire schema bound by
chapter 33; and the persistence layer bound by chapter 44. For a UTXO
coin with SPV configured, the chapter-15 payment-validation methods
this substrate calls (chapter 15 R12) additionally perform the
proof-of-inclusion check bound by
[chapter 37](37-utxo-spv-and-block-header-validation.md) §37.6; this is
a property of what that coin-trait method can conclude, not of this
chapter's own receive budgets or state transitions, which are the same
whether or not SPV is configured for either coin.

**Chapter-bound identifiers.** Every state name used in this chapter
except one is *also* the discriminant of the event that state emits on
entry, and those discriminants are externally dictated: they are
persisted verbatim in the swap's event log (chapter 44 R44.8.4) and
returned verbatim by the version-two swap-status RPC. Naming the state
by its event is therefore not a description of upstream internals but a
statement of the persisted contract. The single exception is each
machine's initial state, which emits no event; this chapter binds the
label `STATE-INITIALIZE` for it in both roles. The abort- and
refund-reason discriminants tabulated in R22, R36, R42 and R43 are
likewise persisted inside the corresponding event's payload and exposed
through the same RPC, and are bound here for that reason.

## 52.3 Bound Runtime Binding and Lifecycle Contract

**R1.** *The version-two machines are storable machines.* Both roles
MUST be implemented as chapter-14 storable state machines
(chapter 14 R20), not as a hand-rolled loop. This is the deliberate
architectural difference from the legacy machines, which chapter 51
R1–R7 binds to a hand-rolled loop and which chapter 51 D1 explicitly
declines to migrate. Consequently every rule chapter 14 binds about the
storable layer — the six-method storage contract (R19), the
transition helpers (R23, R24), the per-transition persistence auto-impl
(R25), the negative implementation that forbids the non-storable
transition helper (R26), and the recovery surface (R27–R30) — applies to
this substrate unchanged and is not restated here.

**R2.** *One event per state entry.* Every state other than
`STATE-INITIALIZE` MUST produce exactly one event when it is entered.
There is no state that produces two events, and no event that is
produced by two states. The event carries the full data the state needs
to be re-entered from storage alone (R8). This is a second deliberate
difference from the legacy machines, whose handlers return ordered
*lists* of events (chapter 51 R1) and several of whose events are
emitted from a state they do not name.

**R3.** *Transition ordering.* On every transition the runner MUST, in
this order: obtain the destination state's event; apply the event to
in-memory reservation state (R57) and emit it on the swap-status
notification stream; append it to the persistent per-swap event log and
await completion of that append; and only then invoke the destination
state's entry logic. A persistence failure MUST propagate as the
machine's error and MUST NOT allow the destination state to run.

**R4.** *Effects precede the event that records them, and the event
precedes the next state's effects.* Because of R2 and R3, a state's
wire output and chain broadcasts happen inside that state's own entry
logic, while the event describing their *results* is the entry event of
the state that follows. An implementation MUST NOT move an effect into
the event-producing accessor, and MUST NOT persist a destination event
before the source state's effect has been attempted. A crash between an
effect and its event is recovered by R8 re-entering the source state, so
every state's entry logic MUST tolerate being re-executed after a
partial effect.

**R5.** *Two entry modes.* The runner MUST support starting a fresh swap
at `STATE-INITIALIZE` and resuming a persisted swap from its event log.
The fresh-start path MUST, before the initial state runs: acquire the
exclusion lock of R6; store the initial persisted representation if no
record for the identifier already exists; start the lock-renewal
activity; subscribe to the swap topic; create the per-swap message inbox
pinned to the counterparty's peer-to-peer public key; and register the
swap in the active-swap index. On termination it MUST mark the swap
finished in storage and then perform the exact inverse: unsubscribe,
destroy the inbox, deregister from the active-swap index, and remove
every reserved-amount entry the swap holds (R59).

**R6.** *Single runner per swap.* Before any state runs, the runner MUST
acquire a per-swap-identifier exclusion lock with a forty-second
time-to-live. If the lock is held, the runner MUST wait one full
time-to-live period and retry exactly once; if it is still held the
runner MUST fail the run with a typed lock-contention error and MUST NOT
emit any event. While the run proceeds the lock MUST be refreshed on a
thirty-second interval, i.e. strictly more often than its time-to-live.
This contract is identical to chapter 51 R5 and the two protocols MUST
use the same lock namespace, so that a legacy and a version-two runner
can never both claim one swap identifier.

**R7.** *Resume requires both coins.* Before a persisted version-two
swap can be recreated, both its coins MUST be activated. The recovery
path MUST wait, polling at one-second resolution, until each coin is
found, and MUST abandon the attempt only on a hard lookup error. The
wait MUST NOT be bounded by a timeout: a swap whose coin the user has
not yet re-enabled must resume when they do, not be lost.

**R8.** *Resumption re-enters the state named by the last event.* The
resume map MUST be the identity: for every non-terminal event, the
machine resumes the state that emitted it, and that state's entry logic
runs again from the beginning. This is a total function and MUST NOT
depend on in-memory state. It is a deliberate simplification relative to
chapter 51 R7, R19 and R32, where the legacy resume map is a
many-to-one function that sends several events to a state other than the
one that produced them. Because the map is the identity, R4's
replay-tolerance requirement is the whole of the recovery contract.

**R9.** *Four refusals to resume.* Recreation MUST fail, emitting no
event and taking no reservation, in exactly four cases: the persisted
event list is empty; the last persisted event is `Aborted`; the last
persisted event is `Completed`; and the last persisted event is one of
the role's refund-terminal events (R22, R36). A fifth case — stored
transaction, preimage, signature, public-key or address bytes that no
longer parse — MUST also fail recreation, and MUST do so with a distinct
parse-failure error so that an operator can tell a corrupt record from a
finished one. A swap whose coin pair is not a supported version-two
combination MUST be left unfinished in storage and retried on a later
start rather than marked finished.

## 52.4 Bound Maker State Graph

**R10.** *State set.* The maker machine MUST have exactly twelve states:
`STATE-INITIALIZE`, `Initialized`, `WaitingForTakerFunding`,
`TakerFundingReceived`, `MakerPaymentSentFundingSpendGenerated`,
`TakerPaymentReceived`,
`TakerPaymentReceivedAndPreimageValidationSkipped`, `TakerPaymentSpent`,
`MakerPaymentRefundRequired`, `MakerPaymentRefunded`, `Aborted`, and
`Completed`. The last three are terminal. `STATE-INITIALIZE` emits no
event; the other eleven emit the identically-named event (R2, R22).

**R11.** *Derived time values.* The maker MUST fix, from its own start
time and the negotiated lock duration, exactly these values, and MUST
NOT re-derive them later in the swap:

| Derived value                          | Definition                                  |
| -------------------------------------- | ------------------------------------------- |
| maker payment locktime                 | start time + 2 × lock duration              |
| taker-payment confirmation deadline    | start time + ⅔ × lock duration              |
| taker-funding confirmation deadline    | maker payment locktime                      |
| taker-payment-spend confirmation deadline | maker payment locktime                   |

The maker payment locktime is what the taker independently recomputes
and compares in R25; the ⅔ deadline bounds both the maker's search for
the funding spend (R18) and its wait for the taker payment's
confirmations (R19).

**R12.** *Initialize.* `STATE-INITIALIZE` MUST, in order: read the
maker coin's current block height; read the taker coin's current block
height; obtain the sender-side trade-fee estimate for the maker coin at
the swap-start approximation stage for exactly the maker volume; obtain
the receiver-side trade-fee estimate for the taker coin at the same
stage; and verify the node's balance covers the maker volume plus both
estimates, excluding amounts already reserved by *other* swaps (R61).
Failure of any of these five steps MUST transition to `Aborted` with the
corresponding reason of R22. On success it MUST transition to
`Initialized`, whose event carries both start-block heights and both
trade-fee estimates.

**R13.** *Negotiate.* `Initialized` MUST begin repeatedly broadcasting
the maker's negotiation message (R52) and then wait for the taker's
negotiation message within the negotiation budget (R51). It MUST then
apply, in this order:

| # | Check                                                                                   | On failure                          |
|---|------------------------------------------------------------------------------------------|-------------------------------------|
| 1 | A taker negotiation arrived within the receive budget.                                    | refuse (R38), `Aborted`             |
| 2 | The negotiation carries a data-bearing action rather than the abort action.                | `Aborted`, MUST NOT refuse (R40)    |
| 3 | The action is present at all (a message carrying neither branch is a protocol violation). | refuse, `Aborted`                   |
| 4 | The absolute difference between the two sides' declared start times is at most 60 seconds. | refuse, `Aborted`                  |
| 5 | The taker's declared funding locktime equals the taker's declared start time plus three lock durations, exactly. | refuse, `Aborted` |
| 6 | The taker's declared payment locktime equals the taker's declared start time plus one lock duration, exactly.    | refuse, `Aborted` |
| 7 | The taker coin accepts the taker's declared taker-coin public key.                        | refuse, `Aborted`                   |
| 8 | The maker coin accepts the taker's declared maker-coin public key.                        | refuse, `Aborted`                   |

On success it MUST cancel the negotiation repeat and transition to
`WaitingForTakerFunding`, whose event carries the two locktimes the
maker *recomputed* (not the values as received), the taker's two
per-coin public keys, the taker's optional per-coin swap-contract
addresses, and the taker's secret hash.

**R14.** *Clock-skew bound.* The 60-second bound of R13 check 4 MUST be
exactly 60 seconds and MUST be the same value the legacy machines use
(chapter 51 R12). It is three times the peer-to-peer layer's per-peer
clock-gap tolerance. Implementations MUST NOT widen it and MUST NOT let
the two protocols drift apart on this value, because both sides derive
locktimes from their own clocks and checks 5 and 6 would otherwise admit
a pair whose locktimes disagree.

**R15.** *Locktime expectation asymmetry.* The maker's expectation of
the taker's *funding* locktime is three lock durations and of the
taker's *payment* locktime is one, while its own payment locktime is two
(R11). This three-tier ladder — taker payment first, maker payment
second, taker funding last — is the protocol's safety ordering and MUST
be preserved exactly. It is the version-two replacement for the
two-tier legacy ladder of chapter 51 R10 and R22, and the two MUST NOT
be conflated.

**R16.** *Await taker funding.* `WaitingForTakerFunding` MUST begin
repeatedly broadcasting the *positive* negotiation acknowledgement (R52)
and wait for the taker's funding-information message within the
negotiation budget (R51). A receive timeout, or funding transaction
bytes that do not parse, MUST transition to `Aborted` with the
corresponding reason. On success it MUST cancel the acknowledgement
repeat and transition to `TakerFundingReceived`, whose event carries the
funding transaction identifier.

**R17.** *Validate funding, gate on it, and pay.* `TakerFundingReceived`
MUST, in order:

1. validate the funding transaction semantically against the negotiated
   payment and funding locktimes, both secret hashes, the taker's
   taker-coin public key, the premium, the trading amount, and the
   dex-fee descriptor **recomputed with the taker's public key** rather
   than the pre-negotiation estimate (chapter 16 R14);
2. apply the propagation gate of R20;
3. generate the funding-spend preimage and the maker's signature over
   it;
4. broadcast the maker payment.

Any failure MUST transition to `Aborted` with the corresponding reason
of R22. Step 4 is the last maker failure that terminates with nothing
committed; every failure after it routes into the refund state (R21).
On success it MUST transition to
`MakerPaymentSentFundingSpendGenerated`, whose event carries the maker
payment identifier, the funding identifier, and the funding-spend
preimage together with its signature.

**R18.** *Announce the payment and watch for the funding spend.*
`MakerPaymentSentFundingSpendGenerated` MUST begin repeatedly
broadcasting the maker-payment message — carrying the maker payment
bytes, the funding-spend preimage and the maker's signature over it —
and MUST keep that repeat running for the whole life of the state (R52).
It MUST then poll the taker coin for a spend of the funding output,
starting from the taker-coin start block, until the ⅔ deadline of R11,
sleeping thirty seconds between polls and after any transient
node error. The poll result determines the transition:

| Poll result                                   | Transition                                                                     |
|-----------------------------------------------|--------------------------------------------------------------------------------|
| Cooperative spend (funding became the taker payment) | `TakerPaymentReceived`, or `TakerPaymentReceivedAndPreimageValidationSkipped` when the taker coin declares the preimage exchange unnecessary (chapter 15 R25) |
| Timelock refund of the funding                | `MakerPaymentRefundRequired`                                                   |
| Secret-reveal refund of the funding           | `MakerPaymentRefundRequired`, carrying the taker secret recovered from the refund |
| Nothing found, or a transient node error      | sleep thirty seconds and poll again                                             |
| Any non-transient search error                | `MakerPaymentRefundRequired`                                                   |
| Deadline reached                              | `MakerPaymentRefundRequired`                                                   |

The maker MUST NOT expect a message announcing the taker payment. It
learns of the taker payment only by observing the chain. This is a
deliberate difference from the legacy protocol, where the taker payment
arrives as a message (chapter 51 R42).

**R19.** *Spend the taker payment.* `TakerPaymentReceived` MUST wait for
the taker payment to reach the order's configured taker-coin
confirmation and notarisation settings, with the ⅔ deadline of R11 as
its cut-off and a ten-second poll interval; then await the taker's
payment-spend preimage message within the budget bound by R51; then
parse the preimage and the signature; then validate the preimage against
the spend the maker expects to make; then sign and broadcast the spend,
revealing the maker's secret. Every one of these five failures MUST
transition to `MakerPaymentRefundRequired` with the corresponding reason
of R22. On success it MUST transition to `TakerPaymentSpent`.

`TakerPaymentReceivedAndPreimageValidationSkipped` MUST behave
identically except that it neither awaits, parses nor validates a
preimage, and instead signs and broadcasts the spend directly. Its two
failure paths are the confirmation wait and the broadcast, both routing
to `MakerPaymentRefundRequired`.

**R20.** *The propagation gate.* Before committing its own payment, the
maker MUST establish that the taker's funding transaction is actually on
the network. The gate MUST be: poll the taker coin for the transaction
by its hash for a thirty-second window at one-second resolution, and on
the first miss make exactly one best-effort re-broadcast of the
transaction the maker already holds. The default policy MUST then accept
mempool visibility alone. An operator-selectable stricter policy MUST
instead wait for `min(configured taker-coin confirmations, 1)`
confirmations with the maker payment locktime as its deadline and a
ten-second poll interval, consistent with chapter 15 R43. Under either
policy, failure MUST transition to `Aborted`. The default MUST remain
the visibility-only policy: on chains whose first confirmation routinely
exceeds the counterparty's receive budget, requiring a confirmation here
converts a healthy swap into a timeout.

**R21.** *Refund state.* `MakerPaymentRefundRequired` MUST branch on the
reason it carries:

- when the reason is that the taker reclaimed its funding by revealing
  its own secret, the maker MUST immediately broadcast the
  secret-reveal refund of its own payment using that recovered secret,
  with no timelock wait (chapter 15 R14);
- in every other case the maker MUST poll the coin until it reports the
  maker payment refundable — sleeping the duration the coin reports, and
  retrying after thirty seconds on a transient error — and then
  broadcast the timelock refund (chapter 15 R13).

On success it MUST transition to `MakerPaymentRefunded`, whose event
carries the payment identifier, the refund identifier and the reason,
and which is terminal. On failure of the refund broadcast it MUST
transition to `Aborted`. Implementations and consumers MUST NOT read the
maker's terminal `Aborted` event as "no maker funds were committed":
this path reaches it with the maker payment on chain and unrefunded.

**R22.** *Maker event set, resume map, and reason vocabularies.* The
maker machine MUST use exactly the eleven event types below with exactly
these resume states.

| Event type                                          | Resumes at                                        |
|-----------------------------------------------------|---------------------------------------------------|
| `Initialized`                                       | `Initialized`                                     |
| `WaitingForTakerFunding`                            | `WaitingForTakerFunding`                          |
| `TakerFundingReceived`                              | `TakerFundingReceived`                            |
| `MakerPaymentSentFundingSpendGenerated`             | `MakerPaymentSentFundingSpendGenerated`           |
| `TakerPaymentReceived`                              | `TakerPaymentReceived`                            |
| `TakerPaymentReceivedAndPreimageValidationSkipped`  | `TakerPaymentReceivedAndPreimageValidationSkipped`|
| `TakerPaymentSpent`                                 | `TakerPaymentSpent`                               |
| `MakerPaymentRefundRequired`                        | `MakerPaymentRefundRequired`                      |
| `MakerPaymentRefunded`                              | (terminal, refuses resume)                        |
| `Aborted`                                           | (terminal, refuses resume)                        |
| `Completed`                                         | (terminal, refuses resume)                        |

The persisted abort-reason vocabulary MUST be exactly these eighteen
discriminants: failure to read the maker-coin block height; failure to
read the taker-coin block height; failure to estimate the maker-payment
fee; failure to estimate the taker-payment-spend fee; balance-check
failure; no taker negotiation received; taker aborted the negotiation;
invalid taker negotiation; start-time difference too large; invalid
taker funding locktime; invalid taker payment locktime; public-key parse
failure; no taker funding information received; taker funding parse
failure; taker funding validation failure; funding-spend generation
failure; maker-payment send failure; maker-payment refund failure.

The persisted refund-reason vocabulary carried by
`MakerPaymentRefundRequired` and `MakerPaymentRefunded` MUST be exactly
these thirteen discriminants: taker payment not received; taker payment
parse failure; taker payment not confirmed in time; taker
payment-spend preimage not received; taker payment-spend preimage
invalid; taker payment-spend not confirmed in time; taker preimage parse
failure; taker signature parse failure; taker payment-spend broadcast
failure; taker funding not spent in time; taker funding reclaimed by
timelock; taker funding reclaimed by secret (carrying the recovered
secret); error while searching for the funding spend.

Unlike the legacy record (chapter 51 R19, R32A), the version-two
persisted record MUST NOT carry `success_events` / `error_events`
arrays. There is no success/error partition of the version-two event
vocabulary; outcome is read from which terminal event the log ends with.

## 52.5 Bound Taker State Graph

**R23.** *State set.* The taker machine MUST have exactly sixteen
states: `STATE-INITIALIZE`, `Initialized`, `Negotiated`,
`TakerFundingSent`, `MakerPaymentAndFundingSpendPreimgReceived`,
`MakerPaymentConfirmed`, `TakerPaymentSent`,
`TakerPaymentSentAndPreimageSendingSkipped`, `TakerPaymentSpent`,
`MakerPaymentSpent`, `TakerFundingRefundRequired`,
`TakerPaymentRefundRequired`, `TakerFundingRefunded`,
`TakerPaymentRefunded`, `Aborted`, and `Completed`. The last four are
terminal.

**R24.** *Derived time values.*

| Derived value                        | Definition                     |
|--------------------------------------|--------------------------------|
| taker funding locktime               | start time + 3 × lock duration |
| taker payment locktime               | start time + 1 × lock duration |
| maker-payment confirmation deadline  | start time + ⅓ × lock duration |
| maker-payment-spend confirmation deadline | taker payment locktime    |

The taker's payment locktime is the shortest of the three swap
locktimes and its funding locktime the longest; the maker's payment
locktime sits between them (R11, R15).

**R25.** *Negotiate — receive first.* Unlike the maker, `Initialized`
MUST *first* wait for the maker's negotiation message within the
negotiation budget (R51), and only then reply. It MUST apply, in this
order:

| # | Check                                                                            | On failure                |
|---|-----------------------------------------------------------------------------------|---------------------------|
| 1 | A maker negotiation arrived within the receive budget.                            | refuse (R39), `Aborted`   |
| 2 | The start-time difference is at most 60 seconds.                                  | refuse, `Aborted`         |
| 3 | The maker's secret hash is exactly 20 or exactly 32 bytes long.                   | refuse, `Aborted`         |
| 4 | The maker's declared payment locktime equals the maker's declared start time plus two lock durations, exactly. | refuse, `Aborted` |
| 5 | The maker coin accepts the maker's declared maker-coin public key.                | refuse, `Aborted`         |
| 6 | The taker coin accepts the maker's declared taker-coin public key.                | refuse, `Aborted`         |
| 7 | The taker coin accepts the maker's declared taker-coin receiving address.         | refuse, `Aborted`         |

Check 3 is the only length check anywhere in the version-two negotiation
and MUST be preserved; R71 extends the symmetric obligation to the
maker.

**R26.** *Reply then await acknowledgement.* After its checks pass the
taker MUST assemble its own negotiation payload — its start time, its
funding locktime, its payment locktime, its own secret hash, its two
per-coin public keys and its optional per-coin swap-contract addresses —
begin repeatedly broadcasting it (R52), and wait for the maker's
acknowledgement within the negotiation budget. It MUST then apply two
further checks:

| #  | Check                                             | On failure          |
|----|---------------------------------------------------|---------------------|
| 8  | An acknowledgement arrived within the budget.     | `Aborted`           |
| 9  | The acknowledgement's boolean value is positive.  | `Aborted`           |

Check 9 is the taker's handling of the maker's refusal signal and MUST
be present; the optional reason accompanying a negative value MUST be
preserved into the persisted abort reason, and MUST tolerate being
absent (chapter 33 R9). Neither check 8 nor check 9 causes the taker to
transmit its own refusal: by that point the taker has already replied
and the maker has already stopped listening for that slot. On success it
MUST cancel the negotiation repeat and transition to `Negotiated`, whose
event carries the maker's payment locktime, the maker's secret hash, the
maker's taker-coin receiving address, the maker's two per-coin public
keys and its optional swap-contract addresses.

**R27.** *Send funding.* `Negotiated` MUST construct and broadcast the
taker funding transaction, whose value is the trading amount plus the
premium plus the dex-fee component (chapter 15 R16, chapter 16). Failure
MUST transition to `Aborted`; this is the last taker failure that
terminates with nothing committed. On success it MUST transition to
`TakerFundingSent`, whose event carries the funding identifier.

Unlike the legacy taker (chapter 51 R27), the version-two taker MUST NOT
broadcast a separate dex-fee transaction, and there is therefore no
version-two counterpart of the legacy fee-send deadline or of the legacy
empty-transaction-identifier convention for a no-fee trade.

**R28.** *Await the maker payment.* `TakerFundingSent` MUST begin
repeatedly broadcasting the taker's funding-information message (R52)
and wait for the maker's payment message within the negotiation budget.
A receive timeout, or maker-payment bytes, funding-spend preimage bytes
or signature bytes that do not parse, MUST each transition to
`TakerFundingRefundRequired` with the corresponding reason of R36 — the
taker's funding is already on chain, so there is no abort path here. On
success it MUST cancel the repeat and transition to
`MakerPaymentAndFundingSpendPreimgReceived`.

**R29.** *Validate, gate, and convert funding into payment.*
`MakerPaymentAndFundingSpendPreimgReceived` MUST, in order:

1. validate the maker payment semantically against the negotiated maker
   payment locktime, both secret hashes, the maker volume and the
   maker's maker-coin public key;
2. validate the maker's funding-spend preimage and signature against the
   spend the taker expects (chapter 15 R22);
3. apply the propagation gate of R20 to the *maker payment*, with the
   same thirty-second window, one-second poll and one-shot re-broadcast;
4. branch on the confirmation policy of R30.

Failures at steps 1, 2 and 3 MUST each transition to
`TakerFundingRefundRequired`.

**R30.** *The taker's confirmation policy is the opposite of the
maker's.* The taker MUST by default require the maker payment to reach
the order's configured maker-coin confirmation and notarisation settings
*before* it broadcasts the funding spend, with the ⅓ deadline of R24 as
its cut-off and a ten-second poll interval; on timeout it MUST
transition to `TakerFundingRefundRequired`, and on success to
`MakerPaymentConfirmed`. Under the non-default policy it MUST instead
proceed directly from visibility to the funding spend, and MUST then
perform the confirmation wait afterwards in `TakerPaymentSent` (R31).
Exactly one of the two waits MUST happen: an implementation that
performs both, or neither, is non-conformant. The asymmetry with the
maker's default (R20) is deliberate and MUST be preserved — the maker
risks only a delayed refund by proceeding on visibility, whereas the
taker would be irreversibly converting its funding against a payment
that may still be replaced.

**R31.** *Send the taker payment.* Both `MakerPaymentConfirmed` and the
direct branch of R29 MUST co-sign and broadcast the funding spend that
converts the funding output into the taker payment (chapter 15 R23).
Failure MUST transition to `TakerFundingRefundRequired`. On success the
machine MUST transition to `TakerPaymentSent`, or to
`TakerPaymentSentAndPreimageSendingSkipped` when the taker coin declares
the preimage exchange unnecessary (chapter 15 R25).

`TakerPaymentSent` MUST then, in order: perform the deferred
confirmation wait of R30 if and only if it was not performed earlier;
generate the taker payment-spend preimage and signature; begin
repeatedly broadcasting the preimage message and keep that repeat
running for the whole life of the state (R52); and poll the taker coin
for a spend of the taker payment, from the taker-coin start block, at a
ten-second interval, until the taker payment locktime. A failed
confirmation wait, a failed preimage generation, or a spend that is not
observed before the locktime MUST each transition to
`TakerPaymentRefundRequired` with the corresponding reason.
`TakerPaymentSentAndPreimageSendingSkipped` MUST behave identically
except that it neither generates nor broadcasts a preimage. On success
both transition to `TakerPaymentSpent`.

**R32.** *Claim the maker payment.* `TakerPaymentSpent` MUST extract the
maker's secret from the observed spend of the taker payment
(chapter 15 R30) and then broadcast the spend of the maker payment using
that secret (chapter 15 R15). Either failure MUST transition to
`Aborted`. On success it MUST transition to `MakerPaymentSpent`.

Both of these abort paths are reached with the taker's payment already
spent by the maker, i.e. with the taker having paid and not yet been
paid. R44 binds the consequence.

**R33.** *Complete.* `MakerPaymentSpent` MUST, when the post-spend
confirmation gate is enabled (the default), wait for the maker-payment
spend to reach the order's configured maker-coin confirmation and
notarisation settings, with the taker payment locktime as its cut-off
and a ten-second poll interval. Failure MUST transition to
`TakerPaymentRefundRequired`; success MUST transition to `Completed`.

**R34.** *Two independent refund states.*
`TakerFundingRefundRequired` MUST immediately broadcast the
secret-reveal refund of the funding output, revealing the taker's *own*
secret, with no timelock wait (chapter 15 R19), and transition to
`TakerFundingRefunded`. `TakerPaymentRefundRequired` MUST poll the coin
until the taker payment is refundable against the taker payment
locktime — sleeping the duration the coin reports, retrying after thirty
seconds on a transient error — and then broadcast the timelock refund
(chapter 15 R24), transitioning to `TakerPaymentRefunded`. Failure of
either broadcast MUST transition to `Aborted`.

The two refund states MUST remain distinct and MUST NOT be merged: they
refund different outputs by different script branches, one immediately
and one only after a timelock. The immediate path is what makes the
taker's exposure bounded when the maker walks away after funding, which
is the whole purpose of the two-stage taker flow.

**R35.** *No refund state is reachable from a state whose funds are not
yet committed.* `TakerFundingRefundRequired` MUST be reachable only from
`TakerFundingSent`, `MakerPaymentAndFundingSpendPreimgReceived` and
`MakerPaymentConfirmed`. `TakerPaymentRefundRequired` MUST be reachable
only from `TakerPaymentSent`,
`TakerPaymentSentAndPreimageSendingSkipped`, `MakerPaymentConfirmed` and
`MakerPaymentSpent`. Every earlier failure MUST go to `Aborted`.

**R36.** *Taker event set, resume map, and reason vocabularies.* The
taker machine MUST use exactly the fifteen event types below with
exactly these resume states.

| Event type                                    | Resumes at                                    |
|-----------------------------------------------|-----------------------------------------------|
| `Initialized`                                 | `Initialized`                                 |
| `Negotiated`                                  | `Negotiated`                                  |
| `TakerFundingSent`                            | `TakerFundingSent`                            |
| `MakerPaymentAndFundingSpendPreimgReceived`   | `MakerPaymentAndFundingSpendPreimgReceived`   |
| `MakerPaymentConfirmed`                       | `MakerPaymentConfirmed`                       |
| `TakerPaymentSent`                            | `TakerPaymentSent`                            |
| `TakerPaymentSentAndPreimageSendingSkipped`   | `TakerPaymentSentAndPreimageSendingSkipped`   |
| `TakerPaymentSpent`                           | `TakerPaymentSpent`                           |
| `MakerPaymentSpent`                           | `MakerPaymentSpent`                           |
| `TakerFundingRefundRequired`                  | `TakerFundingRefundRequired`                  |
| `TakerPaymentRefundRequired`                  | `TakerPaymentRefundRequired`                  |
| `TakerFundingRefunded`                        | (terminal, refuses resume)                    |
| `TakerPaymentRefunded`                        | (terminal, refuses resume)                    |
| `Aborted`                                     | (terminal, refuses resume)                    |
| `Completed`                                   | (terminal, refuses resume)                    |

The persisted abort-reason vocabulary MUST be exactly these eighteen
discriminants: failure to read the maker-coin block height; failure to
read the taker-coin block height; failure to estimate the taker-payment
fee; failure to estimate the maker-payment-spend fee; balance-check
failure; no maker negotiation received; start-time difference too large;
public-key parse failure; address parse failure; invalid maker locktime;
unexpected secret-hash length; no maker acknowledgement received; maker
declined to negotiate; taker-funding send failure; secret extraction
failure; maker-payment spend failure; taker-funding refund failure;
taker-payment refund failure.

The persisted funding-refund-reason vocabulary MUST be exactly these
eight discriminants: maker payment not received; funding-spend preimage
parse failure; funding-spend signature parse failure; taker-payment send
failure; maker-payment validation failure; funding-spend preimage
validation failure; maker-payment parse failure; maker payment not
confirmed in time. The persisted payment-refund-reason vocabulary MUST
be exactly these four: maker payment not confirmed in time;
maker-payment spend not confirmed in time; spend-preimage generation
failure; maker did not spend in time.

## 52.6 Bound Refusal, Abort and Timeout Contract

This section is normative for the behaviour that motivated the chapter.
Chapter 33 already binds the two refusal *encodings*; what it does not
bind, and what deployed peers therefore do not receive, is the
obligation to send them.

**R37.** *Two refusal signals exist and both are bidirectionally
meaningful.* The version-two protocol carries a refusal in each
direction: the taker's negotiation message can select an abort action
carrying a reason, and the maker's acknowledgement can carry a negative
value with an optional reason. Each role MUST be able to both transmit
and receive its own refusal, and to receive the counterparty's. This is
a structural improvement over the legacy protocol, whose single refusal
signal is maker-directional only (chapter 51 R33), and the version-two
implementation MUST realise it rather than leaving the sending half
unimplemented on both sides.

**R38.** *The maker's seven refusal conditions.* The maker MUST transmit
the negative acknowledgement, exactly once, immediately before
transitioning to `Aborted`, in each of the following seven conditions
and in no others:

| # | Condition                                                                                   |
|---|----------------------------------------------------------------------------------------------|
| 1 | The taker's negotiation did not arrive within the receive budget.                            |
| 2 | The taker's negotiation carried neither the data branch nor the abort branch.                |
| 3 | The two sides' declared start times differ by more than 60 seconds.                          |
| 4 | The taker's declared funding locktime is not exactly its start time plus three lock durations.|
| 5 | The taker's declared payment locktime is not exactly its start time plus one lock duration.  |
| 6 | The taker coin rejects the taker's declared taker-coin public key.                           |
| 7 | The maker coin rejects the taker's declared maker-coin public key.                           |

**R39.** *The taker's seven refusal conditions.* The taker MUST transmit
the abort action, exactly once, immediately before transitioning to
`Aborted`, in each of the following seven conditions and in no others:

| # | Condition                                                                          |
|---|-------------------------------------------------------------------------------------|
| 1 | The maker's negotiation did not arrive within the receive budget.                   |
| 2 | The two sides' declared start times differ by more than 60 seconds.                 |
| 3 | The maker's secret hash is neither 20 nor 32 bytes long.                            |
| 4 | The maker's declared payment locktime is not exactly its start time plus two lock durations. |
| 5 | The maker coin rejects the maker's declared maker-coin public key.                  |
| 6 | The taker coin rejects the maker's declared taker-coin public key.                  |
| 7 | The taker coin rejects the maker's declared taker-coin receiving address.           |

**R40.** *A refusal is never answered with a refusal.* A maker that
receives the taker's abort action MUST NOT transmit a negative
acknowledgement in response; it MUST simply terminate. A taker that
receives a negative acknowledgement MUST NOT transmit an abort action in
response. Without this rule the two roles would exchange refusals until
their repeat loops expired.

**R41.** *Refusal is one-shot; acceptance is repeated.* Every refusal
MUST be transmitted as a single broadcast, not on a repeating schedule.
The positive acknowledgement MUST be transmitted on a repeating schedule
for the duration of the subsequent state (R16, R52). The asymmetry is
the same one chapter 51 R35 binds for the legacy protocol and for the
same reason: the refusing party is about to stop running and has no
state in which to maintain a repeat loop.

**R42.** *Refusal precedes persistence.* The refusal broadcast MUST be
issued inside the refusing state's own entry logic, and therefore before
the terminal abort event is applied, streamed or persisted (R3, R4). A
role that persists first and transmits second can lose the refusal
entirely on a crash, leaving the counterparty to time out.

**R43.** *No refusal signal exists after the negotiation phase.* There
is no message by which either role can signal a later failure. Every
failure after the negotiation phase is communicated only by the absence
of the expected next message and, for on-chain stages, by the refund or
spend the counterparty observes on the chain. Implementations MUST NOT
invent additional refusal messages: the wire schema is closed by
chapter 33 R1, and a peer that receives an unknown inner variant treats
the envelope as carrying no variant at all, which chapter 33 R8 defines
as a protocol violation.

**R44.** *The terminal abort event does not imply that nothing was
committed.* Both roles reach `Aborted` from states in which their own
funds are already on chain: the maker when its refund broadcast fails
(R21), the taker when secret extraction or the maker-payment spend fails
(R32) and when either refund broadcast fails (R34). Any consumer of the
persisted log or the swap-status RPC — including recovery tooling, the
graphical client and the counterparty-statistics view — MUST determine
whether funds moved from the *reason* the abort event carries and from
the events preceding it, and MUST NOT infer it from the terminal
discriminant alone. Implementations MUST NOT "fix" this by re-routing
those transitions to a refund-terminal event, because the refund did not
happen.

**R45.** *No counterparty penalty.* The version-two machines MUST NOT
record a counterparty penalty on any transition. The legacy penalty
contract of chapter 51 R40 is legacy-only, and its interaction with
refusal — where a conforming refusal causes the refused party to
penalise the refuser — MUST NOT be carried into version two. A refusing
version-two peer is therefore penalty-free, which is what makes R38 and
R39 unambiguously safe to implement.

**R46.** *No terminal status broadcast.* The version-two machines MUST
NOT broadcast a terminal swap-status object on the swap topic. The
legacy two-attempt decode fallback and its redaction requirement
(chapter 51 R48–R50) are legacy-only and MUST NOT be replicated on the
version-two topic, where every payload is a signed envelope (R47) and an
unsigned JSON object would be discarded. The consequence — that a
version-two node never learns its counterparty's terminal outcome — is
recorded as deferred work (D4).

## 52.7 Bound Message Contract

Chapter 33 binds the message schema: the message names, field names,
field numbers, types and oneof tags, and the generated-binding location.
This section binds only what chapter 33 does not: who sends which
message in which state, how often, for how long it is awaited, and how
the envelope is authenticated and routed.

**R47.** *Signed envelope, pinned sender, matched identifier.* Every
version-two message MUST be encoded, wrapped in the signed envelope, and
broadcast on the swap's version-two topic. The signature MUST be
produced over the digest of the encoded inner message by the per-swap
signing key where the swap has one and by the node's persistent
peer-to-peer key otherwise, and the envelope MUST carry the
corresponding public key. On receipt the runner MUST, in this order:
reject the payload unless the envelope's sender key equals the
counterparty key pinned when the swap's inbox was created; verify the
signature; decode the inner message; and reject it unless the swap
identifier it carries equals the identifier the topic denotes. A payload
failing any of these MUST be discarded with no state change and MUST NOT
be logged at a level that reproduces its contents.

**R48.** *The inbox exists only for the run's lifetime.* The per-swap
inbox MUST be created before the initial state runs and destroyed when
the run terminates (R5). A message arriving for a swap with no inbox
MUST be discarded silently. This is what bounds the memory a hostile
peer can cause a node to allocate on the version-two topic.

**R49.** *Single-slot inbox with take-on-read.* The inbox MUST hold at
most one payload per message kind, and reading a slot MUST clear it. A
later message of the same kind overwrites an unread earlier one. The
consequences, which implementations MUST preserve, are that the repeated
broadcasts of R52 are idempotent at the receiver and that a message
arriving before its awaiting state begins is not lost. This matches the
legacy inbox contract of chapter 51 R44.

**R50.** *Direction and state.* Each message kind MUST be sent only in
the state and direction below.

| Kind                          | Direction        | Sent during                                                   | Awaited during                                          |
|-------------------------------|------------------|---------------------------------------------------------------|---------------------------------------------------------|
| Maker negotiation             | maker → taker    | maker `Initialized`                                           | taker `Initialized` (first half)                        |
| Taker negotiation (data)      | taker → maker    | taker `Initialized` (second half)                             | maker `Initialized`                                     |
| Taker negotiation (abort)     | taker → maker    | once, at taker refusal (R39)                                  | maker `Initialized`                                     |
| Maker negotiated (positive)   | maker → taker    | maker `WaitingForTakerFunding`                                | taker `Initialized` (second half)                       |
| Maker negotiated (negative)   | maker → taker    | once, at maker refusal (R38)                                  | taker `Initialized` (second half)                       |
| Taker funding information     | taker → maker    | taker `TakerFundingSent`                                      | maker `WaitingForTakerFunding`                          |
| Maker payment information     | maker → taker    | maker `MakerPaymentSentFundingSpendGenerated`                 | taker `TakerFundingSent`                                |
| Taker payment-spend preimage  | taker → maker    | taker `TakerPaymentSent`                                      | maker `TakerPaymentReceived`                            |
| Taker payment information     | —                | never sent                                                    | never awaited                                           |

**R51.** *Receive budgets.* Every receive MUST be bounded by a
per-receive budget expressed as a **duration**, plus a fixed
ninety-second grace allowance added to every budget, and MUST poll at
one-second resolution. Every version-two receive MUST use a
ninety-second budget, giving a total of one hundred and eighty seconds.
This explicitly includes the maker's wait for the taker's payment-spend
preimage in `TakerPaymentReceived`: an implementation MUST NOT supply an
absolute wall-clock timestamp where the duration is required, because
doing so makes the wait effectively unbounded and leaves the maker's
committed payment with no timeout into its refund path. Where a stage
also has an absolute deadline — the ⅔ deadline of R11 for the maker, the
⅓ deadline of R24 for the taker — that deadline bounds the *chain*
waits of that stage and is separate from the message budget.

**R52.** *Repeat-until-cancelled broadcast, and repeat-until-state-exit
broadcast.* Outgoing messages other than the two refusals MUST be
broadcast on a repeating interval. Two cancellation disciplines exist
and MUST be applied as follows:

| Message                       | Interval | Repeat ends                                             |
|-------------------------------|----------|---------------------------------------------------------|
| Maker negotiation             | 30 s     | when the taker negotiation is received                  |
| Taker negotiation (data)      | 30 s     | when the maker acknowledgement is received              |
| Maker negotiated (positive)   | 30 s     | when the taker funding information is received          |
| Taker funding information     | 600 s    | when the maker payment information is received          |
| Maker payment information     | 600 s    | when the emitting state exits (up to the ⅔ deadline)    |
| Taker payment-spend preimage  | 600 s    | when the emitting state exits (up to the taker payment locktime) |

The last two have no reply to cancel on: the maker learns of the taker
payment from the chain (R18) and the taker learns of its payment's spend
from the chain (R31). Their repeats therefore MUST be bound to the
lifetime of the emitting state and MUST be stopped when it exits, so a
resumed or abandoned swap does not leave a broadcaster running.

**R53.** *Non-message waits.* Waits that are not message receives MUST
use their own deadlines and poll intervals: every confirmation wait polls
at ten seconds against its stage-specific deadline (R11, R24); the
maker's funding-spend search polls at thirty seconds until the ⅔
deadline (R18); the taker's payment-spend search polls at ten seconds
until the taker payment locktime (R31); refundability polling sleeps the
duration the coin reports and retries after thirty seconds on a
transient error (R21, R34); the propagation gate polls at one second for
thirty seconds (R20).

**R54.** *One dead message kind.* The taker-payment-information kind of
chapter 33 §33.3.8 is neither sent nor awaited by either role (R50), but
its slot MUST remain present in the inbox and its decode path MUST
remain functional, because chapter 33 R1 forbids removing it from the
schema and a peer running a future revision may begin sending it. An
implementation MUST NOT treat its arrival as a protocol violation.

**R55.** *Instruction fields are unused but must round-trip.* The
forward-compatibility instruction fields chapter 33 §33.3.7–§33.3.9 bind
on three message kinds MUST be transmitted absent by both roles and MUST
be tolerated when present. There is no version-two counterpart of the
legacy payment-instruction validation of chapter 51 R13 and R16, and one
MUST NOT be introduced without a chapter revision.

**R56.** *No watcher participation.* The version-two machines MUST NOT
emit, await, or act on third-party watcher messages, and MUST NOT
broadcast on the watcher topic bound by
[chapter 09](09-watcher-reward-infrastructure.md). Watcher support is
legacy-only (chapter 51 R30, chapter 17 §17.11); a version-two swap has
no watcher-outcome events and its persisted log MUST NOT contain any.

## 52.8 Bound Reserved-Funds Semantics

**R57.** *Reservation lives in a per-coin ledger, not the legacy
registry.* A version-two swap MUST record its reservation as an entry in
a per-coin-ticker ledger keyed by swap identifier, and MUST NOT join the
legacy running-swap registry that chapter 51 R51 binds. The entry MUST
carry the coin ticker, the reserved amount, and the associated trade fee
descriptor. Keying by swap identifier is what makes removal exact and
unconditional.

**R58.** *Entry creation and removal are driven by events.* Two entries
MUST be created when the role's initialisation event is applied, and
both removed together when the role's own funds leave the wallet:

| Role  | Entry created when applying | Reserved                                                                                  | Entry removed when applying                     |
|-------|-----------------------------|-------------------------------------------------------------------------------------------|-------------------------------------------------|
| Maker | `Initialized`               | (1) the maker volume, in the maker coin; (2) the maker-payment trade fee, in whichever coin the maker coin's sender-fee descriptor names | `MakerPaymentSentFundingSpendGenerated` |
| Taker | `Initialized`               | (1) the taker volume plus the total dex-fee spend plus the premium, in the taker coin; (2) the taker-payment trade fee, in whichever coin the taker coin's sender-fee descriptor names | `TakerFundingSent` |

The fee entry's coin MUST NOT be assumed to be the trading coin. It is
the fee descriptor's own coin, which chapter 08's `get_sender_trade_fee`
contract makes the trading coin's platform ticker — the same coin for
every coin family whose native asset pays its own fees, and the
platform coin rather than the token for a coin family whose asset does
not (an ERC20 token, for example, where the fee is gas billed to the
platform coin). The reserved amount is only the fee's reservable part
per R62 — zero if the fee's descriptor marks it payable out of the
trading volume, the full amount otherwise — evaluated once, when the
descriptor is still in hand at initialisation (R64 states the identical
evaluation for the receiving-side fee, and both are resolved at the same
point for the same reason).

The two entries MAY land in the same ledger bucket, when the fee's coin
happens to equal the trading coin, or in two different buckets, when it
does not; either way both share the same creation and removal points and
MUST be removed together. Filing the fee as a second same-coin-bucketed
entry rather than nesting it inside the volume entry is a requirement,
not an implementation choice: a per-coin-ticker ledger can only apply a
nested fee's coin-and-marker predicate against the bucket already being
queried, so a nested fee naming a *different* coin than the bucket it
was filed under is invisible to every bucket's total — the token-fee
failure mode V5 records.

The removal points are correct: at each of them the reserved funds have
just been committed to an on-chain output and are no longer spendable
balance. That last clause is an assumption about the coin layer, not
something this rule can enforce: the coin MUST stop offering the inputs
of a broadcast-but-unconfirmed payment as spendable, or removing the
reservation at broadcast hands the same funds back to the next trade. For
a Zcash-Sapling shielded coin in light mode that guarantee is bound by
[chapter 39](39-zcash---z_coin-shielded-coin.md) §39.8.0.7. Note the
consequence that the reservation window opens *after*
the swap's own start balance check has already passed, unlike the legacy
contract where the registry entry precedes the first stage
(chapter 51 R51).

This is not the role's only reservation: R64 binds a further one, on the
coin that pays for spending the *incoming* payment, with a longer
window. The removal above MUST remove only the two entries of this rule,
because all three can share a ledger bucket.

**R59.** *Unconditional release on every exit path.* When the run
terminates for any reason — completion, abort, either refund terminal,
or a failure inside the runtime — the runner MUST remove **every** entry
the swap holds, in whatever coin it was filed under, unconditionally and
without consulting how the swap ended. The swap's own two coins are not
a sufficient search: the fee-headroom entry of R64 is filed in the coin
that pays the fee, which for a token leg is that token's platform coin
and need be neither of them. This MUST happen in the same process,
immediately, as part of the termination sequence of R5, and MUST NOT be
deferred to a restart, a garbage-collection pass, or the dropping of any
object. A swap that terminated during `STATE-INITIALIZE` never created
an entry; a swap that terminated at any later point MUST leave none
behind.

**R60.** *A terminated swap reserves nothing.* Once a version-two swap
has terminated, its contribution to every reservation total MUST be
zero, regardless of which transactions it did or did not broadcast. This
is the version-two statement of the rule chapter 51 R52 binds for the
legacy protocol, and it holds for the same reason: the reservation total
feeds the maximum-tradable-volume answer a user or a market-making
client may query in the next second.

**R61.** *The self-excluding total MUST read the version-two ledger.*
Two reservation totals exist: the aggregate total, and the variant that
excludes one named swap and is the form used by every swap-start balance
check so that a swap never blocks itself. Both MUST sum the legacy
registry **and** the version-two ledger. A self-excluding total that
reads only the legacy registry makes every live version-two reservation
invisible to the balance check of R12 and R23, so a node with
version-two swaps in flight admits further swaps against balance it has
already committed and discovers the shortfall only when a payment fails
to construct. This is the single most consequential rule in this
section.

**R62.** *Trade-fee inclusion rule.* A ledger entry's trade fee MUST
contribute to a coin's total only when the fee's coin matches that coin
and the fee is not marked as payable out of the trading volume itself.
This MUST be the same predicate the legacy total applies
(chapter 51 R53), so that a mixed node cannot compute two different
answers for one coin.

**R63.** *Reconstruction on resume.* When a swap is resumed, both R58
entries — the volume entry and its accompanying send-fee entry — MUST be
re-created together if and only if the resumed entry event is one at
which the role's funds had not yet been committed: for the maker,
`Initialized`, `WaitingForTakerFunding` or `TakerFundingReceived`; for
the taker, `Initialized` or `Negotiated`. For every other resumed event
neither MUST be created. Only the `Initialized` event carries the
send-fee entry's reservable amount (R58); recovering it for a resume at
a later event in the window means reading it back out of the
`Initialized` event earlier in the same persisted log, not out of the
event actually being resumed at.

The fee-headroom entry of R64 has a longer window and therefore a
different rule: it MUST be re-created at every resumed event except the
one at which the incoming payment was spent — `TakerPaymentSpent` for
the maker, `MakerPaymentSpent` for the taker — which is exactly the span
over which the legacy protocol reserves it. Re-creation MUST be
idempotent, so that applying the initialisation event on a resume does
not reserve a second time.

Re-creation on resume MUST NOT go through the live-transition
notification path, consistent with chapter 14 R29's requirement that
resume entry be distinguishable from first-time entry.

**R64.** *Fee-headroom reservation.* Each role MUST additionally reserve
the balance it will need to pay for spending the payment it is owed, so
that a concurrent trade cannot consume it and leave the swap unable to
collect. This is the version-two statement of what chapter 51 R54 and
R55 bind for the legacy protocol, and the two MUST produce the same
number, because one node runs both and a single maximum-tradable-volume
answer is computed over the union.

The amount is **not** simply the counterparty coin's spend fee. A coin
that reports its spend fee as payable out of the trading volume takes
that fee from the payment being claimed — a UTXO spend pays the miner
out of the hash-time-locked output itself — and therefore needs no
balance kept free at all. A coin that does not takes it from the account
balance. The reserved amount is therefore the fee amount when the fee is
not payable out of the trading volume and zero when it is; the coin it
is reserved *in* is the coin the fee descriptor names, which for a token
payment is the token's platform coin rather than the token. This is the
same predicate R62 and chapter 51 R53 apply to every other reserved
amount, applied here at the point the descriptor is still available.

| Role  | Entry created when applying | Reserved                                                        | Entry removed when applying |
|-------|-----------------------------|-----------------------------------------------------------------|-----------------------------|
| Maker | `Initialized`               | the reservable part of the taker-payment-spend fee, in the coin that pays it | `TakerPaymentSpent`        |
| Taker | `Initialized`               | the reservable part of the maker-payment-spend fee, in the coin that pays it | `MakerPaymentSpent`        |

Two consequences follow from the coin that pays it being neither role's
trading coin in general. First, the entry may share a ledger bucket with
that role's own volume entry — a token traded against its own platform
coin puts both there — so removing the volume entry at the point R58
binds MUST NOT remove the headroom entry with it. Second, the entry may
sit in a bucket that is neither of the swap's two coins, which is why
R59's release is stated over every bucket rather than over two.

Only the initialisation event records the amount, but it is owed for as
long as the incoming payment is unspent; a resumed swap MUST therefore
recover it from the persisted log rather than from the event it happens
to be resuming at (R63).

This rule is a deliberate KDF Reloaded divergence: the upstream
version-two implementation this chapter is contrasted against reserves
nothing here, which is why the requirement was first recorded as the gap
D2 rather than as a rule. It is stated as a requirement rather than
offered as an option because the alternative behaviour is a node that
can be left unable to collect a payment it has already been sent, and
because a node running both protocols must not answer the same balance
question two different ways depending on which protocol asked.

**R65.** *Reservation is process-local.* The ledger is in-memory only
and is cleared on restart; reservations are reconstructed by resuming
unfinished swaps from persistent storage (R5, R63). A reservation leak
is therefore unbounded within a process lifetime and cleared only by
restarting, which is why R59, R60 and R61 are stated as hard
requirements rather than as optimisations.

## 52.9 Bound Wire Field and Type Expectations

Chapter 33 binds the declared type of every field. This section binds
what the declared types do not say: the widths and encodings the two
roles actually put in them, and the validation each role owes the other.

**R66.** *There is no fixed public-key width in version two.* Every
public-key field on the version-two wire is a length-unconstrained byte
sequence, and the width is determined by the coin family that owns the
key, not by the protocol. This is a deliberate and total departure from
the legacy contract of chapter 51 R62 and R63, which fixes every legacy
key field at exactly 33 bytes for every chain. Implementations MUST NOT
apply the legacy 33-byte rule to version-two fields, and MUST NOT
introduce a protocol-level width constraint, because the two deployed
coin families already disagree on the width.

**R67.** *The two deployed encodings.* The value a role places in a
per-coin public-key field MUST be exactly what that coin's public
version-two key-derivation operation produces for the local node
(chapter 15 R36):

| Coin family | Encoding placed in the field                                                     |
|-------------|-----------------------------------------------------------------------------------|
| UTXO        | the 33-byte compressed secp256k1 point                                            |
| EVM         | the 64-byte uncompressed secp256k1 coordinate pair, with the uncompressed-form tag byte removed |

Both are secp256k1 keys; they differ only in encoding. The receiving
role MUST interpret the field with the parse operation of the coin that
field belongs to, and MUST NOT assume the two per-coin fields of one
message have the same width — in a cross-family swap they do not.

**R68.** *Parsing counterparty key bytes MUST be length-checked and MUST
NOT be able to abort the process.* Each coin family's parse operation
MUST reject a byte sequence whose length is not one this coin accepts,
and MUST report the rejection as a typed parse failure that the
negotiation states of R13 and R25 turn into a refusal and a terminal
abort. An implementation MUST NOT reach a fixed-width conversion that
terminates the process on a length mismatch, because the bytes are
supplied by an unauthenticated-at-that-point counterparty and a
wrong-width field is the cheapest possible remote denial of service.
This requirement is absolute and applies to every coin family that
implements the version-two surface, present and future.

**R69.** *Chains whose keys are not secp256k1 have no version-two
representation.* No chain whose native key is an Edwards-curve
(ed25519) value implements the version-two coin-trait surface, so the
dictated trailing-zero padding convention chapter 51 R64 binds for the
legacy wire has **no version-two counterpart**. Implementations MUST NOT
speculatively apply that convention to version-two fields. When such a
chain is added to the version-two surface, its field representation is a
new decision requiring a revision of this chapter and, if the chosen
representation is not self-describing, coordination with deployed peers;
it MUST NOT be inherited from chapter 51 by analogy.

**R70.** *Secret-hash widths.* The maker's secret hash MUST be exactly
20 or exactly 32 bytes, matching the two secret-hash algorithms the
protocol supports, and the taker MUST enforce this on receipt (R25 check
3). The version-two scripts of chapter 15 R6 consume the 32-byte form;
the 20-byte form remains acceptable on the wire for algorithm
compatibility. There is no shape-dependent width rule of the kind
chapter 51 R65 binds for the legacy negotiation payload, because the
version-two negotiation has exactly one shape.

**R71.** *The maker owes the taker the symmetric secret-hash check.*
The maker MUST apply the same 20-or-32-byte length check to the taker's
secret hash that R25 check 3 requires of the taker, and MUST refuse and
abort on failure. Without it the maker carries an arbitrary-length value
into its own payment script construction and into its persisted record,
where it is a stored-data parse hazard on every subsequent resume.
This check is a required addition, not a restatement of existing
behaviour.

**R72.** *Contract-address fields and the address string.* The two
per-coin swap-contract fields MUST be present when either side of the
pair is a contract-based chain and absent otherwise (chapter 33 R6);
their contents are opaque to the message layer and MUST NOT be validated
by it. The maker's taker-coin receiving address is the one identity on
the version-two wire carried as a chain-specific *string* rather than
bytes; the taker MUST parse it with the taker coin's public address
parser and MUST refuse and abort if it does not parse (R25 check 7).
Neither role MUST attempt to interpret an address for a chain other than
the one the field names.

**R73.** *Swap identifier.* The swap identifier carried by every message
MUST be the swap's 16-byte identifier and MUST equal the identifier the
topic denotes; a mismatch or a wrong length MUST cause the payload to be
discarded (R47, chapter 33 R10). Because the topic is per-swap, the
identifier is redundant on a well-formed message and is a cross-check,
not a routing key.

## 52.10 Bound Reference-Version Split

**R74.** *The version-two state machines are identical across both
reference lineages.* For the substrate this chapter binds — the state
sets, the event sets, the resume maps, the acceptance checks, the
refusal conditions, the message contract, the timeout budgets, the
propagation and confirmation gates, the reservation predicates and the
wire field expectations — the `v2.6.0-beta` contract and the current
v3-lineage contract are the same. This chapter therefore binds a single
contract for both, and an implementation does not need a
reference-version switch anywhere in the version-two swap machines.
This mirrors the legacy finding of chapter 51 R67.

**R75.** *The wire schema is identical across both lineages.* The
protobuf descriptor chapter 33 embeds is unchanged between
`v2.6.0-beta` and the v3 lineage: no message, field, field number, type
or oneof tag differs. An implementation MUST NOT gate any version-two
encoding or decoding decision on the reference version.

**R76.** *Differences that exist are outside this substrate.* Where the
v3 lineage differs in the version-two role modules it does so only in
the dex-fee exemption and discount policy applied to a named ticker set,
which is bound by [chapter 08](08-fee-routing-engine.md) and by the
network configuration and is therefore netid-selected, not
swap-machine-selected. That policy is consumed by this substrate at
exactly three points — the taker's start-time balance reservation, the
maker's funding validation, and both roles' spend construction — and at
each of them it MUST be reached through the fee descriptor, never
re-derived locally. No rule in this chapter changes with it.

**R77.** *Netid applicability.* Because R74 and R75 hold, the
version-two swap machines are netid-independent. Netid `8762` and netid
`6133` MUST run the identical version-two state machines. Everything
netid-specific a version-two swap consumes — the dex-fee policy, the
dex-fee recipient, the discount ticker set, the burn policy of
chapter 16 — MUST be reached through the active network configuration,
consistent with the repository-wide rule that network-specific behaviour
is selected by configuration rather than by divergent code paths.

**R78.** *No silent upgrade.* An implementation MUST NOT allow a v3-era
change to alter any rule in this chapter for netid `8762` without an
explicit, documented compatibility boundary. In particular: the state
and event discriminants of R22 and R36 MUST NOT be renamed, because they
are the persisted log's vocabulary and the swap-status RPC's contract;
the three-tier locktime ladder of R15 MUST NOT be re-proportioned; and
the negotiation acknowledgement MUST NOT be widened beyond the boolean
plus optional reason that chapter 33 binds.

## 52.11 Invariants

| Invariant                                                                     | Bound by      |
| ------------------------------------------------------------------------------ | ------------- |
| The version-two machines run the chapter-14 storable runtime; the legacy ones do not | R1, chapter 51 R1 |
| Exactly one event per state entry; no state emits two, no event has two sources | R2           |
| A state's effects run before the event that records them is persisted          | R3, R4        |
| The resume map is the identity; every state tolerates full replay              | R4, R8        |
| Resume refuses on an empty log, a terminal event, or unparseable stored data   | R9            |
| One runner per swap identifier, in a namespace shared with the legacy protocol | R6            |
| Clock-agreement tolerance is exactly 60 seconds and equals the legacy value    | R14, chapter 51 R12 |
| Three-tier locktime ladder: taker payment < maker payment < taker funding      | R11, R15, R24 |
| The maker learns of the taker payment from the chain, not from a message       | R18, R50      |
| Maker defaults to visibility; taker defaults to confirmation; exactly one taker confirmation wait happens | R20, R30 |
| Both refusal signals are transmitted, once, on seven conditions each           | R37–R39, R41  |
| A refusal is never answered with a refusal                                     | R40           |
| Refusal is broadcast before the terminal event is persisted                    | R42           |
| No refusal message exists after the negotiation phase                          | R43           |
| The terminal abort event does not imply that no funds were committed           | R21, R32, R44 |
| No counterparty penalty and no terminal status broadcast exist in version two  | R45, R46      |
| Signed envelope, pinned sender, identifier cross-check, inbox scoped to the run | R47, R48     |
| Single-slot take-on-read inbox makes repeated broadcast idempotent             | R49           |
| Every receive budget is a duration plus a fixed 90-second grace                | R51           |
| Chain-observed stages repeat their broadcast until the state exits, not until a reply | R52     |
| Reservation lives in the version-two ledger, released unconditionally on every exit | R57–R60   |
| The self-excluding reservation total reads both the legacy registry and the version-two ledger | R61 |
| Both roles reserve the fee headroom to claim the incoming payment, in the same amount the legacy protocol does | R64, chapter 51 R54, R55 |
| A role's send-fee reservation is filed by the fee's own coin, never assumed to be the trading coin | R58, R62 |
| Public-key width is coin-family-determined, never protocol-fixed               | R66, R67      |
| Counterparty key bytes are length-checked and can never abort the process      | R68           |
| Chapter 51's ed25519 padding convention has no version-two counterpart         | R69           |
| Secret hashes are 20 or 32 bytes and both roles check                          | R70, R71      |
| The machines and the wire schema are identical across lineages and netids      | R74, R75, R77 |

## 52.12 Tests

**T1.** *Resume map identity and totality.* For each role, every
non-terminal event type in R22 / R36 maps to the identically-named state
and every terminal event refuses resume; the set of event types the
persisted-log parser accepts equals the set in the resume map; and an
empty log and an unparseable stored transaction each produce their own
distinct recreation failure.

**T2.** *Replay safety of every state.* For each role, re-entering each
non-terminal state after its effect has already taken place — the
payment already broadcast, the funding already spent, the spend already
on chain — completes without producing a second transaction and without
failing on an elapsed deadline.

**T3.** *Refusal on each of the seven maker conditions.* Seven
maker-side tests, one per row of R38, drive the maker into that
condition and assert that a negative acknowledgement is broadcast
exactly once, that the terminal abort event is persisted after the
broadcast with the matching reason discriminant, and that no repeat loop
was created for the refusal.

**T4.** *Refusal on each of the seven taker conditions.* Seven
taker-side tests, one per row of R39, with the same assertions for the
abort action.

**T5.** *A refusal is not answered with a refusal.* A maker given a
taker abort action, and a taker given a negative acknowledgement, each
terminate without transmitting anything.

**T6.** *Counterparty honours each refusal.* A taker driven to the
acknowledgement wait and given a negative value terminates immediately
with the maker-declined reason, preserving the maker's supplied reason
string and also tolerating its absence; the same taker given no
acknowledgement at all terminates only after the full one-hundred-and-
eighty-second budget of R51 has elapsed — distinguishing the timeout
path from the refusal path by elapsed time. The symmetric pair of tests
covers the maker receiving the taker's abort action.

**T7.** *Clock-skew boundary.* Negotiation succeeds at a 60-second
start-time difference and both refuses and aborts at 61 seconds, on both
roles, and the boundary value is the same one the legacy tests assert.

**T8.** *Locktime ladder.* A maker refuses a taker reply whose funding
locktime is anything other than three lock durations and whose payment
locktime is anything other than one; a taker refuses a maker negotiation
whose payment locktime is anything other than two; and a legacy-shaped
locktime pair is refused by both roles.

**T9.** *Preimage receive budget is a duration.* The maker's wait for
the taker payment-spend preimage times out within one hundred and eighty
seconds of entering the state, for a swap whose lock duration and start
time place the ⅔ deadline far in the future, and the timeout routes to
the refund state with the preimage-not-received reason.

**T10.** *Exactly one taker confirmation wait.* Under the default
policy the taker waits for the maker payment's confirmations before the
funding spend and not after; under the non-default policy it waits after
and not before; in both cases the swap reaches the same next state and
the wait happens exactly once.

**T11.** *Propagation gate.* For each role, a counterparty transaction
that is invisible for the whole thirty-second window causes exactly one
re-broadcast attempt and then the bound failure transition; a
transaction that becomes visible within the window proceeds; and under
the maker's stricter policy a configured confirmation count of zero
remains zero while any higher value is capped to one.

**T12.** *Reservation released on pre-commitment failure.* A swap driven
to the terminal abort state from any pre-commitment state, for both
roles, contributes zero to the reserved total for both coins immediately
after termination, in the same process and without a restart.

**T13.** *Reservation held and then released at commitment.* The maker
contributes exactly the amounts of R58 between its initialisation event
and its payment event and zero afterwards; the taker likewise between
its initialisation event and its funding event.

**T13a.** *Fee headroom, both families and both protocols.* For a spend
fee reported as payable out of the trading volume the reserved headroom
is zero, and for one that is not it is the whole fee amount — and for
each of the two, the version-two reservation and the legacy reservation
of chapter 51 R54 / R55 contribute the identical amount to the total.
The headroom is visible to the self-excluding total of R61 for another
swap's identifier and invisible for its own; reserving twice for one
swap reserves once; committing the volume entry from a shared bucket
leaves the headroom standing; and termination clears the bucket even
when it is neither of the swap's coins.

**T13b.** *Send-fee entry is filed by its own coin, not the trading
coin.* For each role, a sender-fee descriptor whose coin differs from
the trading coin — the platform-ticker case a token trading coin
produces — contributes its reservable amount to the fee's own coin's
total and not to the trading coin's total beyond the volume itself; the
two entries release together at the same event; re-applying the
initialisation event does not double-reserve either; and a descriptor
whose coin equals the trading coin, nested in the same bucket as the
volume entry under the shape R58 forbids, is shown to be invisible to
every total.

**T14.** *The self-excluding total sees version-two reservations.* With
one live version-two swap holding a reservation, the self-excluding
total computed for a *different* swap identifier includes that
reservation, and a second swap start that would exceed the remaining
balance is rejected. The same test computed for the holding swap's own
identifier excludes it, so a swap never blocks itself.

**T15.** *Reconstruction on resume.* Resuming at each of the events
listed in R63 re-creates the volume entry exactly once; resuming at any
other event creates none; and resuming twice does not duplicate an
entry. The fee-headroom entry is re-created at every resumed event but
the incoming-payment spend, including events whose own payload does not
carry the amount, which the resume path recovers from the persisted
initialisation event.

**T16.** *Key-width acceptance and rejection.* A UTXO-side field of 33
bytes and an EVM-side field of 64 bytes are both accepted in the same
cross-family negotiation; a field of any other length for either family
is rejected as a typed parse failure that produces a refusal and a
terminal abort; and the rejection path completes without terminating the
process. The EVM case MUST be asserted explicitly, including at least
one length shorter and one longer than the accepted width.

**T17.** *Secret-hash width, both directions.* A maker secret hash of 20
and of 32 bytes is accepted by the taker and any other length refused; a
taker secret hash of 20 and of 32 bytes is accepted by the maker and any
other length refused.

**T18.** *Sender pinning and identifier cross-check.* A correctly signed
message from a key other than the pinned counterparty key leaves every
inbox slot unchanged; so does a correctly signed message from the right
key whose carried swap identifier does not match the topic; so does any
message for a swap with no inbox.

**T19.** *Inbox overwrite and take-on-read.* Two successive messages of
one kind leave only the later payload readable, and reading a slot twice
yields the payload then nothing.

**T20.** *Dead message kind is tolerated.* A well-formed taker-payment-
information message delivered to either role is stored, is never
awaited, and causes no state change and no protocol-violation error.

**T21.** *Broadcast lifetimes.* The three negotiation-phase repeats stop
when their reply arrives; the funding-information repeat stops when the
maker payment arrives; and the maker-payment-information and
payment-spend-preimage repeats stop when their emitting state exits,
including when it exits into a refund state.

**T22.** *Abort after commitment is distinguishable.* A maker whose
refund broadcast fails, and a taker whose maker-payment spend fails,
each terminate with the abort event carrying the reason discriminant
that identifies the post-commitment cause, and the preceding events in
the log show the payment on chain.

**T23.** *Reference-version and netid equivalence.* The state sets,
event sets, resume maps, acceptance checks, refusal conditions, budgets
and gates asserted by T1–T22 hold identically when the swap is
configured for netid `8762` and for netid `6133`, and the encoded bytes
of every message kind are identical to those produced against the
`v2.6.0-beta` schema (R74, R75, R77).

## 52.13 Deferred Work

**D1.** *A structured refusal taxonomy.* R38 and R39 bind seven
conditions per role, but the wire carries only a free-text reason
alongside the refusal. A refused peer therefore cannot programmatically
distinguish a clock-skew refusal (retryable after a clock fix) from a
malformed-key refusal (not retryable). A machine-readable reason code
would need a new field on an existing message and is deferred; the
free-text reason MUST NOT be parsed as if it were one.

**D2.** *(Resolved by §52.8, R64.)* *Fee-headroom reservation.* R64 now
binds the reservation on both roles, deriving the amount so that it
equals what chapter 51 R54 / R55 already reserve for the legacy
protocol; the legacy side needed no change. The maximum-tradable-volume
answer shrinks by the incoming-payment spend fee while a version-two
swap is live against a coin that pays that fee from the account balance,
and is unchanged for coins that pay it out of the payment being claimed.

**D3.** *Per-swap key isolation for the taker's spend path.* The
maker's spend of the taker payment on the preimage-skipping branch
supplies empty per-swap derivation data where the ordinary branch
supplies the swap's own. This is harmless for the coin families
currently on the version-two surface, whose key derivation ignores it,
but it blocks the per-swap key isolation chapter 15 D2 defers and MUST
be resolved before that work lands.

**D4.** *Terminal outcome visibility.* R46 records that version two has
no counterpart of the legacy terminal status broadcast, so a node never
learns how its counterparty's side of a swap ended and the
counterparty-statistics store receives nothing for version-two swaps. A
signed terminal-outcome message on the version-two topic is the natural
migration path and is deferred.

**D5.** *Counterparty reputation.* R45 records that version two carries
no penalty mechanism. Whether the legacy time-bounded penalty
(chapter 51 R40) should be extended to version two, or replaced for both
protocols by something proportional, is deferred; the two decisions
should be taken together.

**D6.** *Bounded resume wait on coin activation.* R7 requires an
unbounded wait for both coins to be activated before a swap can resume.
A swap whose coin the user never re-enables therefore never resumes and
never times out. Surfacing such swaps to the operator, rather than
bounding the wait, is the likely resolution and is deferred.

**D7.** *WebAssembly persistence is a no-op.* `swap_v2_common.rs`'s
`wasm` module implements `StateMachineStorage` for both
`MakerSwapStorage` and `TakerSwapStorage` entirely as stubs:
`store_repr`/`store_event`/`mark_finished` return `Ok(())` without
writing anything, `has_record_for` always returns `false`, and
`get_unfinished` always returns an empty list (`get_repr` is the one
honest member, returning an explicit "not yet implemented" error). A
version-two swap run from a WebAssembly build therefore persists
nothing: a page reload or tab close during a live swap is
indistinguishable, on resume, from a swap that never started — no
event log to replay, no record for kickstart to find. This is a gap
against [Chapter 26](26-cross-platform-and-wasm.md) R8, which binds
every persistence consumer in the workspace — the version-two swap
state stores named explicitly among R8's chapter-bound consumers — to
a real native-SQLite-plus-WebAssembly-IndexedDB pair, not one working
implementation and one silent stub. Native (non-WASM) resume/kickstart
is unaffected; this is WebAssembly-only. Closing it means a real
IndexedDB-backed implementation following R9's substrate, which is its
own project (data shape, migration from nothing since no prior WASM
V2 swap was ever actually recorded, and coordination with R8's other
consumers' established shape) — deferred rather than attempted inline.
Until closed, a WebAssembly-based GUI or SDK integration that offers
version-two swaps should treat an in-progress swap as unsafe to leave
unattended (no crash/reload recovery exists), and this limitation
should be stated to users, not left implicit.

**D8.** *Closed.* `MakerPaymentSpent`'s confirmation-timeout abort no
longer persists an unusable, empty payment record. `taker_payment:
BytesJson` is now threaded through `TakerPaymentSpent` and
`MakerPaymentSpent` (both the state structs and the persisted
`TakerSwapEvent::TakerPaymentSpent`/`::MakerPaymentSpent` variants) as
an additive, `#[serde(default)]` field, following the same
backward-compatible-add convention already established for R64's
headroom field — a log written before this field existed resumes with
empty bytes, which is safe only because no V2 swap logged that far
back can still be waiting at `MakerPaymentSpent` today, not because
empty bytes are fine going forward. `MakerPaymentSpent::on_changed`'s
confirmation-timeout branch now passes the real, carried-forward
`self.taker_payment` to `TakerPaymentRefundRequired` instead of
`BytesJson::default()`. Two regression tests cover it: one proves
`taker_payment` round-trips through `get_event()` (would not have
compiled against the pre-fix code, the strongest available form of
"fails before, passes after"); one is a source-text meta-test, mirroring
the existing `dex_fee.rs` `include_str!` idiom, proving the fixed call
site no longer contains the empty-bytes fallback. Full
`on_changed()`-level end-to-end coverage was not attempted: this
codebase has no mock/test-double infrastructure for
`MakerCoinSwapOpsV2`/`TakerCoinSwapOpsV2` anywhere, and building one is
its own scoped effort, not part of this fix. Code:
`mm2src/mm2_main/src/lp_swap/taker_swap_v2.rs`.

**D9.** *R9's "fifth case" is not implemented for persisted P2P pubkey
bytes, and only incidentally covered for some other byte types.*
`recreate_machine` (both maker and taker) does not itself validate any
stored transaction, preimage, signature, public-key, or address bytes
before accepting a persisted record and constructing the state
machine — it checks only event-list emptiness and terminal-event
exclusion (R9's first four cases). For transaction and HTLC-pubkey
bytes this gap is largely masked in practice: the states that actually
consume them re-parse independently in their own `on_changed`, so a
malformed value still produces a clean abort, just later and via a
different code path than R9 describes. It is not masked for the
per-swap P2P identity pubkey (`maker_p2p_pubkey`/`taker_p2p_pubkey`):
this value is consumed exactly once, inside
`StorableStateMachine::init_additional_context` — a hook that is
infallible by trait signature (returns `()`, not a `Result`) — via
`secp256k1::PublicKey::from_slice(...).expect(...)`. A malformed
persisted value here reaches that `.expect()` and panics, which R9
explicitly says should not happen: recreation should fail up front,
distinctly, not panic deep inside a later, unrelated-looking hook.
Found while investigating whether that `.expect()` could safely become
a logged degrade instead of a panic (it was judged it should not,
absent this fix — skipping `init_v2_msg_store` would silently break
the swap's P2P routing instead of failing loudly); the real fix
belongs in `recreate_machine` validating the fifth case up front, as
R9 already requires, not in patching the symptom at the consuming
hook. Deferred rather than fixed inline, since it touches both roles'
recreation paths and deserves its own scoped pass. Code:
`mm2src/mm2_main/src/lp_swap/{taker_swap_v2,maker_swap_v2}.rs`
(`recreate_machine`, `init_additional_context`).

## 52.14 Baseline and Repository Verifications

The version-two substrate does not exist in the baseline state defined
by [chapter 02](02-baseline-state.md), so unlike chapter 51 this chapter
cannot be verified against the baseline tree. Chapter 14 V1 and V2
already record that the baseline contains no event-sourced or
kickstart-capable state-machine infrastructure and that the baseline
swap subsystem drives swaps by hand-rolled control flow with no generic
resume contract; it follows that neither version-two role machine, the
version-two message inbox, the version-two topic, nor the version-two
reserved-amount ledger exists at baseline. The verifications below are
therefore stated against the present reloaded tree, which already
carries an independently authored version-two implementation.

**V1.** The present tree contains both version-two role machines, the
storable-runtime binding of R1, the per-swap inbox, the version-two
topic and the reserved-amount ledger. This chapter governs an existing
substrate; it does not introduce one.

**V2.** The present tree implements the receiving half of both refusal
signals — the maker acts on a received abort action, the taker acts on a
received negative acknowledgement — and implements neither sending half:
on every negotiation rejection each role transitions straight to its
terminal abort state with no wire output. R38, R39 and R41 are therefore
the rules whose absence must be corrected on both roles; the receiving
halves are already present and correct.

**V3.** The present tree's self-excluding reservation total reads only
the legacy running-swap registry, while the aggregate total reads both
that registry and the version-two ledger. R61 is therefore a required
correction, and it is the one rule in this chapter whose absence
silently over-admits swaps rather than failing visibly.

**V4.** The present tree's version-two event enumerations carry no
serialisation tag and content attributes, so the persisted event objects
are encoded in a different JSON shape from the one deployed peers and
graphical clients read. Two event discriminants also differ from the
bound vocabulary — the maker's preimage-skipping received event and the
taker's preimage-skipping sent event are each spelled without the
conjunction the deployed vocabulary uses. R22, R36 and R78 are therefore
tightening rules relative to the present tree, and correcting them
changes only the persisted encoding, not the state graph.

**V5.** *(Resolved by §52.8, R58.)* The present tree stored the two
trade-fee estimates in the initialisation events as bare numbers rather
than as full fee descriptors carrying the fee's coin and its
payable-out-of-trading-volume marker. R58 and R62 require the
descriptor, because the inclusion predicate of R62 cannot be evaluated
without it, and the ledger's per-coin-ticker bucketing made the gap
concrete rather than cosmetic: a volume entry that nested the send fee
as a `TradeFee` naming a coin other than the bucket it was filed under
was invisible to `get_locked_amount` in *every* bucket, because the
fold logic checks a nested fee's coin only against the bucket it is
already iterating, never against the bucket the fee's own coin would
name. This was live for a token trading coin, whose sender-side fee is
billed to its platform coin (R58's second table row) rather than to the
token; a token maker or taker reserved nothing for its own payment's
gas.

Both estimates are now resolved into full descriptors where they are
computed, at initialisation, rather than persisted as bare numbers: each
initialisation event carries the send fee's reservable amount as an
additional defaulted field, alongside the existing estimate. The paying
coin is recovered from the live coin rather than persisted, since it is
that coin's platform ticker and does not vary over a swap. The ledger
entry for the two is likewise no longer one entry with a nested fee, but
two entries under `Volume` sharing the coin's own bucket when the fee's
coin agrees with the trading coin and separate buckets when it does not
— eliminating the nested-fee-versus-bucket mismatch rather than working
around it. A log written before this field existed falls back to
reserving the full persisted estimate, in the newly correct bucket,
which changes only which bucket a token's send-fee reservation lands in
on resume, never how much is reserved.

**V6.** The present tree's maker machine reaches its terminal abort
state from the refund state on a failed refund, and its taker machine
reaches it from the post-spend states, exactly as R21, R32 and R44
describe. No change is required; R44 exists to stop a future
implementation from "tidying" these transitions into a refund-terminal
event.

**V7.** The present tree's embedded wire descriptor declares a package
name that differs from the deployed one. Because a proto3 package name
does not appear in the encoded bytes, this has no wire consequence and
no swap can fail because of it; it does change the generated-binding
path and would change any descriptor-set or reflection consumer.
Chapter 33 R3 binds the present name; this chapter records the
divergence rather than resolving it.

## 52.15 External References

- The two-stage taker flow and dual-secret maker payment this machine
  drives, and the coin-trait surface it calls —
  [chapter 15](15-swap-v2-utxo-path.md) and
  [chapter 17](17-swap-v2-evm-path.md).
- The generic persistent state-machine runtime, its transition
  helpers, its persistence auto-implementation and its recovery surface
  — [chapter 14](14-state-machine-runtime.md).
- The version tag that dispatches into this substrate —
  [chapter 13](13-swap-version-negotiation.md) R4, R13.
- The peer-to-peer wire schema every message in R50 is encoded in —
  [chapter 33](33-swap-v2-wire-schema-embedding.md).
- The dex-fee descriptor, its rational arithmetic, and its discount and
  burn forms — [chapter 08](08-fee-routing-engine.md) and
  [chapter 16](16-swap-v2-pre-burn-output.md).
- The persisted version-two row fields and the event-log append and
  unfinished-swap selection paths —
  [chapter 44](44-database-persistence-and-migrations.md) R44.8.3,
  R44.8.4.
- The legacy protocol this chapter is the counterpart of, including the
  legacy refusal contract, reservation registry and fixed-width key
  fields — [chapter 51](51-legacy-v1-swap-state-machine.md).
- The publish-subscribe overlay carrying the version-two swap topic;
  per-netid scoping is bound by
  [chapter 06](06-network-id-seed-node.md) and the substrate by
  [chapter 28](28-libp2p-modernization.md).
- The Protocol Buffers proto3 language specification, for the
  encoded-bytes independence of the package name recorded in V7.
- secp256k1 point encodings — the 33-byte compressed form and the
  65-byte uncompressed form whose 64-byte tagless remainder R67 binds
  for the EVM family.

## 52.16 Provenance Footer

- *Inputs:* chapter 01 (clean-room rules and the canonical chapter
  shape); chapter 02 (the baseline state, which contains none of this
  substrate); chapter 08 and chapter 16 (the fee descriptor consumed by
  R12, R17, R27 and R58); chapter 13 (the version tag that dispatches
  into this substrate); chapter 14 (the storable runtime this substrate
  is built on, whose rules R1 incorporates by reference); chapters 15
  and 17 (the coin-trait surface every state calls, and the happy-path
  sequence sketched in chapter 17 §17.10); chapter 33 (the wire schema
  this chapter's message contract sits above); chapter 44 (the persisted
  row and event-log contract this chapter gives semantics to);
  chapter 51 (the legacy machine this chapter is contrasted against
  throughout); the present reloaded workspace (the independently
  authored version-two implementation the verifications of §52.14 are
  stated against); published Protocol Buffers proto3 documentation;
  published secp256k1 point-encoding documentation.
- *Permitted-input classes used:* R1 (baseline source, for the absence
  findings of §52.14); R3 (external public specifications); R4 (dictated
  wire formats, persisted vocabularies and interfaces the project must
  inter-operate with); R7 (independent work).
- *Sibling-allowlist consultations:* none beyond the cross-chapter
  references listed in *Inputs*.
- *Forbidden corpus:* consulted, under the chapter-01 two-team
  clean-room workflow, for upstream parity of the version-two swap state
  machines only — specifically the state and event vocabularies and
  resume maps of R22 and R36, the transition conditions of R12–R21 and
  R25–R34, the untransmitted refusal signals of R37–R39, the receive and
  repeat budgets of R51 and R52, the reservation mechanism and its
  ledger-versus-registry split of R57–R61, the coin-family key
  encodings of R67, and the reference-lineage equivalence of R74 and
  R75. No source text, private helper structure, internal decomposition,
  log or error string, or per-method internal table was copied; every
  rule above is stated as an externally observable behavioural, wire, or
  persisted-contract requirement.
