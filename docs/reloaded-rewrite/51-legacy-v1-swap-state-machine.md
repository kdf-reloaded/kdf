# Chapter 51 — Legacy (V1) Atomic-Swap State Machine

**Status:** driving-spec.

The two role-specific command-driven, event-sourced state machines that
execute the legacy (version-`1`) five-stage hash-time-locked-contract
atomic swap — their complete stage and event sets, every success,
failure, refusal and abort transition with its exact triggering
condition, the peer-to-peer message contract and timeout budget that
drives them, the per-stage reserved-funds semantics including release on
unsuccessful termination, and the fixed-width field expectations the
negotiation and swap-status messages impose on the wire.

## 51.1 Executive Summary

[Chapter 13](13-swap-version-negotiation.md) binds the version tag that
selects between the legacy swap protocol (value `1`) and the
version-two protocol (values `2` and `3`). Chapter 13 R4 names the
legacy value but deliberately does not specify the protocol it selects;
[chapter 14](14-state-machine-runtime.md) binds the generic persistent
state-machine runtime that the *version-two* paths of chapters 15, 16
and 17 are built on, and the legacy paths explicitly do **not** use that
runtime. [Chapter 44](44-database-persistence-and-migrations.md) binds
the persisted encoding of the legacy event vocabulary but not its
semantics. The legacy protocol itself — by far the most widely deployed
swap protocol on netid `8762` — has therefore had no governing chapter.
This chapter closes that gap.

The legacy swap is executed independently by both counterparties. Each
side runs a single-threaded loop over a *stage* value: the loop invokes
the handler for the current stage, the handler performs input/output and
returns a (possibly empty) ordered list of *events* plus the next stage,
each event is applied to in-memory state and appended to a persistent
per-swap event log, and the loop repeats until a handler returns no next
stage. The event log is the sole recovery record: on restart the last
persisted event alone determines which stage to resume at. There is no
compile-time transition validator, no reentrancy trait, and no storage
abstraction of the kind chapter 14 binds — the legacy machines predate
all of it and must not be retrofitted onto it, because doing so would
change the persisted log shape that deployed peers and graphical clients
already parse.

Three properties of the legacy machine are specified here for the first
time and are the practical reason this chapter exists:

1. **The refusal signal.** The negotiation acknowledgement carried on
   the wire is a boolean. Both a positive and a *negative* value are
   transmitted by conforming peers. A maker that rejects a taker's
   negotiation reply, for any of seven distinct reasons, transmits the
   negative value before terminating. A taker that receives the negative
   value terminates immediately and penalises the maker. An
   implementation that never emits the negative value leaves its
   counterparty waiting out the full receive budget on every rejected
   negotiation, and is therefore not wire conformant.
2. **Reservation release.** Every unfinished swap reserves trade volume
   and fee headroom against the node's tradable balance. The reservation
   is released by removing the swap from the in-memory running-swap
   registry when the run loop exits — including when it exits before any
   transaction has been broadcast. A registry that only releases the
   entry when the swap object is dropped, or that keys the reservation
   on whether a transaction was sent, permanently shrinks the node's
   tradable balance on every failed negotiation.
3. **Fixed-width key fields.** The negotiation message's public-key
   fields are fixed at 33 bytes on the wire and are rejected at any
   other length. Chains whose keys are not 33-byte secp256k1 values
   occupy the field by a dictated padding convention rather than by
   varying the length.

Bound rules R1–R7 cover the run loop and lifecycle; R8–R19 the maker
stage graph; R20–R32 the taker stage graph; R33–R40 the refusal and
abort contract; R41–R50A the message contract, timeout budget and
terminal status report;
R51–R58B the reserved-funds semantics and the registry and message-store
lifecycle; R59–R66 the wire field and type
expectations; R71–R73 the secret-hash algorithm selection; R67–R70 the
reference-version split.

## 51.2 Subsystem Shape

The substrate is two parallel role-specific machines plus four shared
services they both consume:

| Surface                          | Responsibility                                                                 |
| -------------------------------- | ------------------------------------------------------------------------------ |
| Maker stage machine              | Twelve stages, twenty-seven event types (R8–R19).                              |
| Taker stage machine              | Thirteen stages, thirty-one event types (R20–R32).                             |
| Per-swap message inbox           | Single-slot-per-message-kind store, sender-pinned (R41–R44).                    |
| Repeating broadcast service      | Re-transmits one message on an interval until cancelled (R45–R47).              |
| Running-swap registry            | Keyed set of live swaps; the sole source of trade-volume reservations (R51).    |
| Per-swap exclusion lock          | Time-to-live lock preventing two runners for one swap identifier (R5).          |

Both machines are anchored to the same per-swap publish-subscribe topic,
which is the swap topic prefix joined to the swap's identifier. All
legacy swap traffic for one swap, in both directions, travels on that
one topic.

The substrate has three external dependencies it does not own: the
coin-layer swap operations (payment construction, validation, spend,
refund, confirmation waiting), the fee descriptor and its arithmetic
bound by [chapter 08](08-fee-routing-engine.md), and the persistence
layer bound by [chapter 44](44-database-persistence-and-migrations.md).
For a UTXO coin with SPV configured, the coin-layer payment-validation
step this substrate calls (R17, R28) additionally performs the
proof-of-inclusion check bound by
[chapter 37](37-utxo-spv-and-block-header-validation.md) §37.6; this is
a property of what that opaque validation step can conclude, not of
this chapter's own timeout budgets or stage transitions, which are the
same whether or not SPV is configured for either coin.

**Chapter-bound identifiers.** The stage names used throughout this
chapter (`STAGE-START`, `STAGE-NEGOTIATE`, and the rest) are bound by
this chapter as its own contract surface. They are descriptive names for
positions in the state graph; they are not required to appear in any
implementation, and the graph is fully recoverable from the persisted
event vocabulary alone (R7). By contrast, every event-type name and
every message-kind name quoted in this chapter is externally dictated:
event-type names are the `type` discriminants of the persisted and
RPC-exposed event objects bound by chapter 44, and message-kind names
are the variant discriminants of the swap message envelope that a
counterparty must decode.

## 51.3 Bound Run-Loop and Lifecycle Contract

**R1.** *Two-value handler result.* Each stage handler MUST return a
pair: an optional next stage, and an ordered list of zero or more
events. Returning no next stage terminates the run. Every handler MUST
return successfully in normal operation; a stage's failure is expressed
as a failure *event* plus a next stage, never as a run-loop error. This
is what makes every failure path recoverable and inspectable rather than
losing the swap on an unwind.

**R2.** *Per-event ordering.* For each event a handler returns, in list
order, the runner MUST perform the following steps in exactly this
order:

1. evaluate the counterparty-penalty predicate for the event and, if it
   holds, record a time-bounded penalty against the counterparty's
   persistent public key (R40);
2. apply the event to the machine's in-memory state;
3. emit the event on the swap-status notification stream;
4. append the event, together with a millisecond timestamp, to the
   persistent per-swap event log and await completion of that append.

Only after every event of a handler's result has been appended may the
runner invoke the next stage's handler. A stage's wire output is
therefore always transmitted *before* the event describing it is
persisted; a crash between the two is recovered by the resumption rule
of R7 replaying the stage.

**R3.** *Persistence is blocking and fatal.* The event-log append of
R2 step 4 MUST be awaited. A failure to persist MUST NOT be swallowed
and MUST NOT allow the loop to advance, because the log is the only
recovery record and a gap in it silently changes the resume point.

**R4.** *Termination sequence.* When a handler returns no next stage,
the runner MUST, in order: mark the swap finished in persistent
storage; broadcast the node's own swap status to the swap topic (R48);
exit the loop; and then release the swap's entry in the running-swap
registry (R51). The registry release MUST occur on every exit path,
including exits caused by failure or refusal.

**R5.** *Single runner per swap.* Before any stage runs, the runner MUST
acquire a per-swap-identifier exclusion lock with a forty-second
time-to-live. If the lock is held, the runner MUST wait one full
time-to-live period and retry exactly once; if the lock is still held it
MUST abandon the run without emitting any event. While the run
proceeds, the lock MUST be refreshed on a thirty-second interval, i.e.
strictly more often than its time-to-live, so that a live runner never
appears expired.

**R6.** *Two entry modes.* The runner MUST support starting a fresh swap
at `STAGE-START` and resuming a persisted swap from its event log. On
resume, if the log's last event yields no next stage, the swap is
already finished and the runner MUST abandon the run without emitting
any event.

**R7.** *Resumption is a total function of the last event.* Every event
type MUST map to exactly one resume stage, or to termination. The map is
bound per role by R19 and R32. Two consequences are load-bearing: the
resume point never depends on in-memory state, and a stage is replayed
in full when the crash occurred between its wire output and its event
append, so every stage handler MUST tolerate being re-executed after a
partial effect (for example, by first searching the chain for a payment
it may already have broadcast).

## 51.4 Bound Maker Stage Graph

**R8.** *Stage set.* The maker machine MUST have exactly twelve stages:
`STAGE-START`, `STAGE-NEGOTIATE`, `STAGE-AWAIT-TAKER-FEE`,
`STAGE-SEND-MAKER-PAYMENT`, `STAGE-AWAIT-TAKER-PAYMENT`,
`STAGE-VALIDATE-TAKER-PAYMENT`, `STAGE-SPEND-TAKER-PAYMENT`,
`STAGE-CONFIRM-SPEND`, `STAGE-REFUND-PREPARE`, `STAGE-REFUND-EXECUTE`,
`STAGE-REFUND-FINALIZE`, and `STAGE-FINISH`. `STAGE-FINISH` MUST emit
the terminal event and return no next stage.

**R9.** *Start.* `STAGE-START` MUST, in order: obtain the sender-side
trade-fee estimate for the maker coin at the swap-start approximation
stage; obtain the receiver-side trade-fee estimate for the taker coin;
verify the node's balance covers the maker volume plus both estimates,
excluding amounts already reserved by other swaps (R53); record the
current wall-clock second as the swap start time; and read the current
block height of both coins. Failure of any of these five steps MUST
emit `StartFailed` and transition to `STAGE-FINISH`. On success it MUST
emit `Started` carrying the swap's frozen parameter set and transition
to `STAGE-NEGOTIATE`.

**R10.** *Locktime derivation.* The `Started` parameter set MUST fix the
maker payment locktime as the swap start time plus the lock duration
scaled by the taker coin's maker-locktime multiplier, rounded up. The
lock duration itself is the negotiated payment locktime for the coin
pair. This value is what the counterparty independently recomputes and
compares in R11 and R25; it MUST NOT be re-derived later in the swap.

**R11.** *Negotiate.* `STAGE-NEGOTIATE` MUST begin repeatedly
broadcasting the maker's negotiation message (R41, R45) and then wait
for the taker's negotiation reply within the negotiation budget (R46).
It MUST then apply five acceptance checks, in this order:

| # | Check                                                                                      | On failure                       |
|---|---------------------------------------------------------------------------------------------|----------------------------------|
| 1 | The reply arrived within the receive budget.                                                | `NegotiateFailed`, refuse (R34)  |
| 2 | The absolute difference between the two sides' declared start times is at most 60 seconds.  | `NegotiateFailed`, refuse        |
| 3 | The taker's declared payment locktime equals the taker's declared start time plus the lock duration exactly. | `NegotiateFailed`, refuse |
| 4 | Each coin accepts the counterparty's declared swap-contract address for that coin.          | `NegotiateFailed`, refuse        |
| 5 | Each coin accepts the counterparty's declared per-coin hash-time-locked-contract public key as a well-formed key for that coin. | `NegotiateFailed`, refuse |

Every failure MUST transition to `STAGE-FINISH`. On success it MUST emit
`Negotiated`, carrying the taker's payment locktime, the two negotiated
swap-contract addresses, and the taker's two per-coin public keys, and
transition to `STAGE-AWAIT-TAKER-FEE`.

**R12.** *Clock-skew bound.* The 60-second bound of R11 check 2 is the
protocol's clock-agreement tolerance and MUST be exactly 60 seconds. It
is three times the peer-to-peer layer's per-peer clock-gap tolerance;
implementations MUST NOT widen it, because both sides derive locktimes
from their own clocks and check 3 would otherwise admit a pair whose
locktimes disagree.

**R13.** *Await taker fee.* `STAGE-AWAIT-TAKER-FEE` MUST begin
repeatedly broadcasting the *positive* negotiation acknowledgement
(R35) and wait for the taker's fee message within the fee budget (R46).
It MUST then: validate any payment instructions carried alongside the
fee against the maker's own secret hash, amount and maker-side lock
duration; compute the expected fee descriptor for the taker coin, the
maker coin ticker, the taker volume and the taker's taker-coin public
key; and — unless the descriptor is the no-fee form — decode and
validate the fee transaction against the expected sender, the expected
descriptor and the taker-coin start block. The output structure and
the one-sided value tolerance of that validation are bound by
[chapter 08](08-fee-routing-engine.md) R15B. Any failure MUST emit
`TakerFeeValidateFailed` and transition to `STAGE-FINISH`.

**R14.** *No-fee short circuit.* When the fee descriptor for the trade
is the no-fee form (on the legacy protocol this happens only through the
waiver of [chapter 08](08-fee-routing-engine.md) R15C / chapter 16 R7;
the version-two ticker exemption of chapter 16 R12B never applies here),
`STAGE-AWAIT-TAKER-FEE` MUST NOT require or decode a
fee transaction. It MUST emit `TakerFeeValidated` carrying an
empty transaction identifier — empty transaction bytes and empty hash
bytes — and proceed. The empty identifier is the dictated
representation of "no fee transaction exists" in the persisted log and
in the counterparty-visible status; it MUST NOT be replaced by an absent
field.

**R15.** *Fee-validation retries.* On-chain validation of the fee
transaction MUST be retried a bounded number of times with a fixed
delay between attempts before being treated as a failure, so that a
transaction not yet visible to the maker's node does not fail an
otherwise valid swap. Both the attempt bound and the delay are fixed
values shared with other fee-validating call sites and are not
per-swap-configurable.

**R16.** *Instructions event always emitted.* `STAGE-AWAIT-TAKER-FEE`
MUST emit `MakerPaymentInstructionsReceived` on the success path
regardless of whether instructions were present, carrying the validated
instructions or their absence. It MUST be emitted *before*
`TakerFeeValidated`. The event is a success event and resumes at the
same stage that produced it (R19), which is what makes the
re-entrant replay of R7 safe here.

**R17.** *Send maker payment and await taker payment.*
`STAGE-SEND-MAKER-PAYMENT` MUST first search the chain for a maker payment
it may already have broadcast, and adopt one it finds without a second
broadcast. Only if none is found MUST it check that the swap start time
plus one third of the lock duration has not passed, then construct and
broadcast the maker payment; either way it emits `MakerPaymentSent`,
transitioning to `STAGE-AWAIT-TAKER-PAYMENT`. Failure of the search, a
passed deadline, or failure to construct or broadcast MUST emit
`MakerPaymentTransactionFailed` and transition to `STAGE-FINISH` — this
is the last maker failure that terminates without a refund, because no
maker funds are yet committed. `STAGE-AWAIT-TAKER-PAYMENT` MUST then, in
order: assemble and repeatedly broadcast the maker payment message
(R45); wait for the maker payment to reach its required confirmation
count, with a deadline of the swap start time plus two fifths of the
lock duration; and wait for the taker's payment message for three
fifths of the lock duration. Failure to assemble the payment message
MUST emit `MakerPaymentDataSendFailed`; failure to confirm MUST emit
`MakerPaymentWaitConfirmFailed`; a missing, late or undecodable taker
payment MUST emit `TakerPaymentValidateFailed`. Each of these three MUST
be immediately followed by `MakerPaymentWaitRefundStarted` carrying the
refund deadline (R18) and MUST transition to `STAGE-REFUND-PREPARE`.

**R18.** *Refund deadline and refund chain.* The maker's refund deadline
MUST be the maker payment locktime plus 3700 seconds. Every maker
failure occurring after `MakerPaymentSent` MUST route into the
three-stage refund chain: `STAGE-REFUND-PREPARE` notifies the taker coin
that a maker refund is beginning and emits `MakerPaymentRefundStarted`;
`STAGE-REFUND-EXECUTE` waits until the coin reports the
hash-time-locked contract refundable (polling, with a fixed retry delay
on transient errors), broadcasts the refund and emits
`MakerPaymentRefunded`, or emits `MakerPaymentRefundFailed` and goes to
`STAGE-FINISH`; `STAGE-REFUND-FINALIZE` notifies the taker coin of
refund success and emits `MakerPaymentRefundFinished`. For a coin that
refunds automatically the execute stage MUST instead wait for the coin's
own refund to complete and emit `MakerPaymentRefunded` with no
transaction identifier.

**R19.** *Maker event set and resume map.* The maker machine MUST use
exactly the twenty-seven event types below, with exactly these resume
stages. Twelve are success events and fifteen are error events; the
split MUST be exposed verbatim in the persisted record's
`success_events` and `error_events` arrays (R49).

| Event type                          | Class   | Resumes at                  |
|-------------------------------------|---------|-----------------------------|
| `Started`                           | success | `STAGE-NEGOTIATE`           |
| `StartFailed`                       | error   | `STAGE-FINISH`              |
| `Negotiated`                        | success | `STAGE-AWAIT-TAKER-FEE`     |
| `NegotiateFailed`                   | error   | `STAGE-FINISH`              |
| `MakerPaymentInstructionsReceived`  | success | `STAGE-AWAIT-TAKER-FEE`     |
| `TakerFeeValidated`                 | success | `STAGE-SEND-MAKER-PAYMENT`  |
| `TakerFeeValidateFailed`            | error   | `STAGE-FINISH`              |
| `MakerPaymentSent`                  | success | `STAGE-AWAIT-TAKER-PAYMENT` |
| `MakerPaymentTransactionFailed`     | error   | `STAGE-FINISH`              |
| `MakerPaymentDataSendFailed`        | error   | `STAGE-REFUND-PREPARE`      |
| `MakerPaymentWaitConfirmFailed`     | error   | `STAGE-REFUND-PREPARE`      |
| `TakerPaymentReceived`              | success | `STAGE-VALIDATE-TAKER-PAYMENT` |
| `TakerPaymentWaitConfirmStarted`    | success | `STAGE-VALIDATE-TAKER-PAYMENT` |
| `TakerPaymentValidatedAndConfirmed` | success | `STAGE-SPEND-TAKER-PAYMENT` |
| `TakerPaymentValidateFailed`        | error   | `STAGE-REFUND-PREPARE`      |
| `TakerPaymentWaitConfirmFailed`     | error   | `STAGE-REFUND-PREPARE`      |
| `TakerPaymentSpent`                 | success | `STAGE-CONFIRM-SPEND`       |
| `TakerPaymentSpendFailed`           | error   | `STAGE-REFUND-PREPARE`      |
| `TakerPaymentSpendConfirmStarted`   | success | `STAGE-CONFIRM-SPEND`       |
| `TakerPaymentSpendConfirmed`        | success | `STAGE-FINISH`              |
| `TakerPaymentSpendConfirmFailed`    | error   | `STAGE-REFUND-PREPARE`      |
| `MakerPaymentWaitRefundStarted`     | error   | `STAGE-REFUND-PREPARE`      |
| `MakerPaymentRefundStarted`         | error   | `STAGE-REFUND-EXECUTE`      |
| `MakerPaymentRefunded`              | error   | `STAGE-REFUND-FINALIZE`     |
| `MakerPaymentRefundFailed`          | error   | `STAGE-FINISH`              |
| `MakerPaymentRefundFinished`        | error   | `STAGE-FINISH`              |
| `Finished`                          | success | (terminates)                |

`MakerPaymentWaitRefundStarted` carries a refund deadline field; every
other error event carries a swap-error payload; the remaining events
carry the payloads bound by chapter 44 R44.8A.2.

The two intermediate stages not otherwise described are
`STAGE-VALIDATE-TAKER-PAYMENT`, which waits for the taker payment's
confirmations against a deadline of the swap start time plus four fifths
of the lock duration and then validates the payment's script, amount,
locktime, counterparty key and secret hash — emitting
`TakerPaymentWaitConfirmFailed` or `TakerPaymentValidateFailed` plus
`MakerPaymentWaitRefundStarted` into the refund chain on failure, and
`TakerPaymentValidatedAndConfirmed` on success — and
`STAGE-SPEND-TAKER-PAYMENT`, which first checks that the same four-fifths
deadline has not passed, then broadcasts the spend revealing the secret
and emits `TakerPaymentSpent` plus `TakerPaymentSpendConfirmStarted`.
`STAGE-CONFIRM-SPEND` waits for the spend's confirmations with the
refund deadline as its cut-off and emits `TakerPaymentSpendConfirmed`.

## 51.5 Bound Taker Stage Graph

**R20.** *Stage set.* The taker machine MUST have exactly thirteen
stages: `STAGE-START`, `STAGE-NEGOTIATE`, `STAGE-SEND-TAKER-FEE`,
`STAGE-AWAIT-MAKER-PAYMENT`, `STAGE-VALIDATE-MAKER-PAYMENT`,
`STAGE-SEND-TAKER-PAYMENT`, `STAGE-AWAIT-TAKER-PAYMENT-SPEND`,
`STAGE-SPEND-MAKER-PAYMENT`, `STAGE-CONFIRM-MAKER-PAYMENT-SPEND`,
`STAGE-REFUND-PREPARE`, `STAGE-REFUND-EXECUTE`, `STAGE-REFUND-FINALIZE`,
and `STAGE-FINISH`.

**R21.** *Start.* `STAGE-START` MUST obtain three trade-fee estimates —
the fee to send the taker fee transaction, the taker payment's own trade
fee, and the fee to spend the maker payment — verify the balance covers
the taker volume plus the dex fee plus those estimates excluding other
swaps' reservations, and read both coins' current block heights. Any
failure MUST emit `StartFailed` and transition to `STAGE-FINISH`.

**R22.** *Taker deadlines.* The `Started` parameter set MUST fix:

| Derived value                | Definition                                                        |
|------------------------------|-------------------------------------------------------------------|
| taker payment locktime       | swap start time + lock duration                                   |
| maker-payment wait deadline  | swap start time + two fifths of the lock duration                 |
| taker-fee send deadline      | swap start time + one third of the lock duration                  |
| taker refund deadline        | taker payment locktime + 3700 seconds                             |

The taker's locktime is one lock duration where the maker's is the lock
duration scaled by the maker-locktime multiplier (R10); this asymmetry
is the protocol's safety margin and MUST be preserved.

**R23.** *Negotiate — receive first.* Unlike the maker, the taker's
`STAGE-NEGOTIATE` MUST *first* wait for the maker's negotiation message
within the negotiation budget, and only then reply. This ordering is
what makes the maker the party that adjudicates the negotiation and the
taker the party that can be refused.

**R24.** *Taker acceptance checks.* On receiving the maker's negotiation
message the taker MUST apply, in order: the same 60-second start-time
agreement check as R11 check 2; a check that the maker's declared
payment locktime equals the maker's declared start time plus *twice* the
lock duration; and per-coin acceptance of both declared swap-contract
addresses. Any failure MUST emit `NegotiateFailed` and transition to
`STAGE-FINISH`. The taker MUST NOT transmit a refusal signal — the
refusal signal is maker-directional only (R33).

**R25.** *Taker locktime expectation asymmetry.* The taker's expectation
in R24 is start time plus twice the lock duration, whereas the maker's
expectation in R11 check 3 is start time plus one lock duration. This
asymmetry is not an error and MUST be preserved exactly; it mirrors R22.

**R26.** *Reply then await acknowledgement.* After its checks pass, the
taker MUST derive its own negotiation reply — echoing the maker's secret
hash and the two negotiated swap-contract addresses, and carrying its
own two per-coin public keys and its own start time and payment
locktime — begin repeatedly broadcasting it (R45), and wait for the
negotiation acknowledgement within the negotiation budget. It MUST then
apply two further checks:

| # | Check                                                | On failure                      |
|---|------------------------------------------------------|---------------------------------|
| 6 | An acknowledgement arrived within the budget.        | `NegotiateFailed`, `STAGE-FINISH` |
| 7 | The acknowledgement's boolean value is positive.     | `NegotiateFailed`, `STAGE-FINISH` |

Check 7 is the taker's handling of the refusal signal and MUST be
present. On success the taker MUST emit `Negotiated` carrying the
maker's payment locktime, the maker's secret hash, the two negotiated
swap-contract addresses and the maker's two per-coin public keys, and
transition to `STAGE-SEND-TAKER-FEE`.

**R27.** *Send taker fee.* `STAGE-SEND-TAKER-FEE` MUST first check that
the taker-fee send deadline of R22 has not passed, emitting
`TakerFeeSendFailed` and transitioning to `STAGE-FINISH` if it has. It
MUST then compute the fee descriptor from the taker coin, the maker coin
ticker, the taker volume and the taker's own taker-coin public key. If
the descriptor is the no-fee form it MUST NOT broadcast anything and
MUST emit `TakerFeeSent` with the empty transaction identifier of R14.
Otherwise it MUST broadcast the fee transaction, emitting
`TakerFeeSendFailed` and transitioning to `STAGE-FINISH` on failure and
`TakerFeeSent` on success.

**R28.** *Await and validate maker payment.* `STAGE-AWAIT-MAKER-PAYMENT`
MUST repeatedly broadcast the taker fee message (R45) and wait for the
maker payment message within the maker-payment budget (R46). Failure to
assemble the fee message, a receive timeout, invalid payment
instructions, or an undecodable maker payment MUST each emit
`MakerPaymentValidateFailed` and transition to `STAGE-FINISH`. On
success it MUST emit, in order, `TakerPaymentInstructionsReceived`,
`MakerPaymentReceived` and `MakerPaymentWaitConfirmStarted`.
`STAGE-VALIDATE-MAKER-PAYMENT` MUST then wait for the maker payment's
confirmations with the maker-payment wait deadline of R22 as its cut-off
— emitting `MakerPaymentWaitConfirmFailed` on timeout — and validate the
payment, emitting `MakerPaymentValidateFailed` on rejection. Both
transition to `STAGE-FINISH`: up to and including this point the taker
has committed only the dex fee, and there is nothing to refund.

**R29.** *Send taker payment.* `STAGE-SEND-TAKER-PAYMENT` MUST first
search the chain for a taker payment it may already have broadcast. Only
if none is found MUST it check that the maker-payment wait deadline has
not passed. Failure of the search or a passed deadline MUST emit
`TakerPaymentTransactionFailed` and transition to `STAGE-FINISH`. On
success it MUST emit `TakerPaymentSent`. This search-before-deadline
ordering is required by R7: a replayed stage must not refuse to continue
a swap whose payment is already on chain merely because the send window
has since closed.

**R30.** *Await spend.* `STAGE-AWAIT-TAKER-PAYMENT-SPEND` MUST
repeatedly broadcast the taker payment message on a fifteen-second
interval and, when the coin pair supports third-party watchers and both
watcher preimages exist, repeatedly broadcast the watcher message on a
six-hundred-second interval on the watcher topic bound by
[chapter 09](09-watcher-reward-infrastructure.md), emitting
`WatcherMessageSent`. It MUST then poll for a spend of the taker payment
on a ten-second interval until the taker payment locktime. On timeout it
MUST emit `TakerPaymentWaitForSpendFailed` followed by
`TakerPaymentWaitRefundStarted` carrying the refund deadline, and
transition to `STAGE-REFUND-PREPARE`. On observing the spend it MUST
extract the secret from the spending transaction — emitting
`TakerPaymentWaitForSpendFailed` and transitioning to `STAGE-FINISH` if
extraction fails, which is the one post-payment taker failure that does
*not* enter the refund chain — and otherwise emit `TakerPaymentSpent`
carrying both the spending transaction and the recovered secret.

**R31.** *Spend maker payment and refund chain.*
`STAGE-SPEND-MAKER-PAYMENT` MUST broadcast the spend of the maker
payment using the recovered secret, emitting `MakerPaymentSpent` on
success or `MakerPaymentSpendFailed` into the refund chain on failure.
`STAGE-CONFIRM-MAKER-PAYMENT-SPEND` MUST wait for that spend's
confirmations, emitting `MakerPaymentSpendConfirmed` or
`MakerPaymentSpendConfirmFailed`. The taker refund chain mirrors R18
with taker-side event names: prepare emits `TakerPaymentRefundStarted`,
execute emits `TakerPaymentRefunded` or `TakerPaymentRefundFailed`, and
finalize emits `TakerPaymentRefundFinished`.

**R32.** *Taker event set and resume map.* The taker machine MUST use
exactly the thirty-one event types below with exactly these resume
stages.

| Event type                            | Class   | Resumes at                          |
|---------------------------------------|---------|-------------------------------------|
| `Started`                             | success | `STAGE-NEGOTIATE`                   |
| `StartFailed`                         | error   | `STAGE-FINISH`                      |
| `Negotiated`                          | success | `STAGE-SEND-TAKER-FEE`              |
| `NegotiateFailed`                     | error   | `STAGE-FINISH`                      |
| `TakerFeeSent`                        | success | `STAGE-AWAIT-MAKER-PAYMENT`         |
| `TakerFeeSendFailed`                  | error   | `STAGE-FINISH`                      |
| `TakerPaymentInstructionsReceived`    | success | `STAGE-VALIDATE-MAKER-PAYMENT`      |
| `MakerPaymentReceived`                | success | `STAGE-VALIDATE-MAKER-PAYMENT`      |
| `MakerPaymentWaitConfirmStarted`      | success | `STAGE-VALIDATE-MAKER-PAYMENT`      |
| `MakerPaymentValidatedAndConfirmed`   | success | `STAGE-SEND-TAKER-PAYMENT`          |
| `MakerPaymentValidateFailed`          | error   | `STAGE-FINISH`                      |
| `MakerPaymentWaitConfirmFailed`       | error   | `STAGE-FINISH`                      |
| `TakerPaymentSent`                    | success | `STAGE-AWAIT-TAKER-PAYMENT-SPEND`   |
| `WatcherMessageSent`                  | success | `STAGE-AWAIT-TAKER-PAYMENT-SPEND`   |
| `TakerPaymentTransactionFailed`       | error   | `STAGE-FINISH`                      |
| `TakerPaymentDataSendFailed`          | error   | `STAGE-REFUND-PREPARE`              |
| `TakerPaymentWaitConfirmFailed`       | error   | `STAGE-REFUND-PREPARE`              |
| `TakerPaymentSpent`                   | success | `STAGE-SPEND-MAKER-PAYMENT`         |
| `TakerPaymentWaitForSpendFailed`      | error   | `STAGE-REFUND-PREPARE`              |
| `MakerPaymentSpent`                   | success | `STAGE-CONFIRM-MAKER-PAYMENT-SPEND` |
| `MakerPaymentSpendConfirmed`          | success | `STAGE-FINISH`                      |
| `MakerPaymentSpendConfirmFailed`      | error   | `STAGE-REFUND-PREPARE`              |
| `MakerPaymentSpentByWatcher`          | success | `STAGE-CONFIRM-MAKER-PAYMENT-SPEND` |
| `MakerPaymentSpendFailed`             | error   | `STAGE-REFUND-PREPARE`              |
| `TakerPaymentWaitRefundStarted`       | error   | `STAGE-REFUND-PREPARE`              |
| `TakerPaymentRefundStarted`           | error   | `STAGE-REFUND-EXECUTE`              |
| `TakerPaymentRefunded`                | error   | `STAGE-REFUND-FINALIZE`             |
| `TakerPaymentRefundedByWatcher`       | error   | `STAGE-FINISH`                      |
| `TakerPaymentRefundFailed`            | error   | `STAGE-FINISH`                      |
| `TakerPaymentRefundFinished`          | error   | `STAGE-FINISH`                      |
| `Finished`                            | success | (terminates)                        |

Note the deliberate classification anomalies that MUST be preserved:
`TakerPaymentWaitForSpendFailed` is an error event that nevertheless
occurs on a swap the taker may still complete, and the entire refund
family including successful refunds is classified as error, because the
classification answers "did the swap complete as a trade", not "did the
node act correctly". `MakerPaymentSpentByWatcher` and
`TakerPaymentRefundedByWatcher` are watcher-outcome events that a node
may never emit itself but MUST accept in a persisted log
(chapter 44 R44.8A.5).

**R32A.** *Two success-event vocabularies.* The persisted
`success_events` array (R49) MUST carry the twelve-entry watcher-free
vocabulary when the swap does not use watchers and the fourteen-entry
vocabulary that additionally contains `WatcherMessageSent` and
`MakerPaymentSpentByWatcher` when it does. The `error_events` array is
the same seventeen entries in both cases.

## 51.6 Bound Refusal and Abort Contract

This section is normative for the behaviour that motivated the chapter:
a conforming peer transmits an explicit negative negotiation
acknowledgement, and an implementation that cannot emit one is not wire
conformant.

**R33.** *The refusal signal exists and is maker-directional.* The
negotiation acknowledgement message carries a boolean. The positive
value means "negotiation accepted, proceed"; the negative value means
"negotiation refused, do not proceed". Only the maker transmits this
message, because only the maker adjudicates the counterparty's
negotiation reply (R23). The taker MUST be able to receive and act on
both values (R26 check 7); the maker MUST be able to transmit both.

**R34.** *The seven refusal conditions.* The maker MUST transmit the
negative acknowledgement, exactly once, immediately before emitting
`NegotiateFailed` and terminating, in each of the following seven
conditions and in no others:

| # | Condition                                                                                   |
|---|----------------------------------------------------------------------------------------------|
| 1 | The taker's negotiation reply did not arrive within the negotiation receive budget.          |
| 2 | The two sides' declared start times differ by more than 60 seconds.                          |
| 3 | The taker's declared payment locktime is not exactly the taker's start time plus the lock duration. |
| 4 | The maker coin rejects the counterparty's declared maker-coin swap-contract address.          |
| 5 | The taker coin rejects the counterparty's declared taker-coin swap-contract address.          |
| 6 | The maker coin rejects the counterparty's declared maker-coin public key as malformed.        |
| 7 | The taker coin rejects the counterparty's declared taker-coin public key as malformed.        |

Conditions 1 through 5 correspond to checks 1 through 4 of R11 with the
two contract-address checks separated; conditions 6 and 7 are R11
check 5 separated per coin.

**R35.** *Refusal is one-shot, acceptance is repeated.* The negative
acknowledgement MUST be transmitted as a single broadcast, not on a
repeating schedule. The positive acknowledgement MUST be transmitted on
a repeating schedule for the duration of the subsequent stage (R13,
R45). The asymmetry is deliberate: the refusing maker is about to stop
running and has no stage in which to maintain a repeat loop, whereas the
accepting maker must keep re-announcing acceptance until the taker's fee
arrives.

**R36.** *Refusal precedes persistence.* The refusal broadcast MUST be
issued before the `NegotiateFailed` event is returned by the stage
handler, and therefore before that event is applied, streamed or
persisted (R2). A maker that persists first and transmits second can
lose the refusal entirely on a crash, leaving the taker to time out.

**R37.** *Refusal on receive-timeout is still transmitted.* Condition 1
of R34 requires transmitting a refusal even though the reason for
refusing is that nothing was received. This is not vacuous: the taker
may have been broadcasting its reply on a channel the maker's node could
not read while the taker can read the maker's, and the refusal converts
the taker's remaining wait into an immediate termination.

**R38.** *No other refusal signal exists.* There is no refusal message
for any later stage. Every failure after negotiation is communicated
only by the absence of the expected next message and by the terminal
swap-status broadcast of R48. Implementations MUST NOT invent additional
refusal messages on the legacy topic, because a peer that does not
recognise the message kind will fail to decode the envelope.

> **Pointer (informative; Spec Reader 2026-10-25, cleared by the Dirty
> Gate with chapter 60 on 2026-10-25).** None of R34's conditions concerns volume: a swap is never
> refused at negotiation for being below a minimum trading volume, a dust
> limit, or the fees it will deduct. A volume too small to complete fails
> later, in the coin layer — for example as a spend failure of R31 when the
> spend fee reaches the payment value. Where each such failure occurs and
> what each party then observes is bound in
> [Chapter 60](60-trading-floors-and-refusal-contract.md) R60.5.5–R60.5.6.

**R39.** *Abort without any event.* Three conditions MUST abort a run
without emitting or persisting any event at all: failure to acquire the
per-swap exclusion lock after one retry (R5); resuming a swap whose
persisted log is already terminal (R6); and a failure to load the
persisted swap record on resume. An aborted run MUST still release its
running-swap registry entry if one was taken.

**R40.** *Counterparty penalty predicate.* The penalty step of R2 step 1
MUST fire on exactly these events: for the taker, `NegotiateFailed`,
`MakerPaymentValidateFailed`, and `TakerPaymentWaitForSpendFailed`; for
the maker, `TakerFeeValidateFailed` and `TakerPaymentValidateFailed`.
The penalty MUST be recorded against the counterparty's persistent
public key, MUST carry the swap identifier and the causing event as its
reason, and MUST expire after one hour. Note the direct consequence of
R33 and R34 together: a maker that transmits a refusal causes the
refused taker to penalise that maker's public key for an hour, because
the taker's refusal handling produces `NegotiateFailed`, which is in the
taker's penalty set. Refusal is therefore correct behaviour but not
cost-free, and MUST NOT be emitted outside R34's seven conditions.

## 51.7 Bound Message Contract

**R41.** *Six message kinds, one envelope.* The legacy swap message
envelope MUST carry exactly six kinds, in this discriminant order:
`Negotiation` (negotiation data), `NegotiationReply` (negotiation data),
`Negotiated` (boolean), `TakerFee` (opaque payload), `MakerPayment`
(opaque payload), `TakerPayment` (opaque payload). The order is
wire-significant; a new kind is appended, never inserted, and no kind is
removed.

**R42.** *Direction and stage.* Each kind MUST be sent only in the stage
and direction below.

| Kind               | Direction        | Sent during                                     | Awaited during                                  |
|--------------------|------------------|-------------------------------------------------|-------------------------------------------------|
| `Negotiation`      | maker → taker    | maker `STAGE-NEGOTIATE`                         | taker `STAGE-NEGOTIATE` (first half)            |
| `NegotiationReply` | taker → maker    | taker `STAGE-NEGOTIATE` (second half)           | maker `STAGE-NEGOTIATE`                         |
| `Negotiated`       | maker → taker    | maker `STAGE-AWAIT-TAKER-FEE` (positive) or once at maker `STAGE-NEGOTIATE` refusal (negative) | taker `STAGE-NEGOTIATE` (second half) |
| `TakerFee`         | taker → maker    | taker `STAGE-AWAIT-MAKER-PAYMENT`               | maker `STAGE-AWAIT-TAKER-FEE`                   |
| `MakerPayment`     | maker → taker    | maker `STAGE-AWAIT-TAKER-PAYMENT`               | taker `STAGE-AWAIT-MAKER-PAYMENT`               |
| `TakerPayment`     | taker → maker    | taker `STAGE-AWAIT-TAKER-PAYMENT-SPEND`         | maker `STAGE-AWAIT-TAKER-PAYMENT`               |

Note that each side broadcasts its outgoing message during the stage in
which it awaits the *reply*, not during the stage that produced it. This
is what makes the repeat loop of R45 terminate naturally when the reply
arrives.

**R43.** *Signed envelope, pinned sender.* Every message MUST be
serialised, signed, and broadcast on the swap topic. On receipt the
runner MUST decode and verify the signature, then MUST accept the
message only if the recovered sender key equals the counterparty key
pinned when the swap's inbox was created. A message from any other
sender MUST be discarded without effect. Where the swap uses a
per-swap ephemeral signing key, that key signs; otherwise the node's
persistent key signs.

**R44.** *Single-slot inbox with take-on-read.* The per-swap inbox MUST
hold at most one payload per message kind, and reading a slot MUST clear
it. A later message of the same kind overwrites an unread earlier one.
The consequence, which implementations MUST preserve, is that the
repeated broadcasts of R45 are idempotent at the receiver and that a
message arriving before its awaiting stage begins is not lost.

**R45.** *Repeat-until-cancelled broadcast.* Outgoing messages other
than the negative acknowledgement MUST be broadcast on a repeating
interval, and the repeat MUST be cancelled as soon as the awaited reply
is received. The intervals are:

| Message                                       | Interval                        |
|-----------------------------------------------|---------------------------------|
| `Negotiation`, `NegotiationReply`             | negotiation budget ÷ 6 = 15 s   |
| `Negotiated` (positive)                       | fee budget ÷ 6 = 100 s          |
| `TakerFee`                                    | maker-payment budget ÷ 6 = 100 s |
| `MakerPayment`, `TakerPayment`                | 15 s (fixed)                    |
| Watcher message (watcher topic, chapter 09)   | 600 s (fixed)                   |

The fixed fifteen-second interval on the two payment messages is
deliberately shorter than a sixth of its budget so that a
mobile client that is foregrounded only briefly still transmits or
receives it.

**R46.** *Receive budgets.* Every receive MUST be bounded by a
per-stage budget plus a fixed ninety-second grace allowance added to
every budget, and MUST poll at one-second resolution. The stage budgets
are:

| Await                                            | Stage budget            | Total with grace |
|--------------------------------------------------|-------------------------|------------------|
| Negotiation message / reply / acknowledgement    | 90 s                    | 180 s            |
| Taker fee message (maker side)                   | 600 s                   | 690 s            |
| Maker payment message (taker side)               | 600 s                   | 690 s            |
| Taker payment message (maker side)               | ⅗ × lock duration       | + 90 s           |

**R47.** *Non-message waits.* Waits that are not message receives MUST
use their own deadlines and poll intervals: confirmation waits poll at
fifteen seconds against a stage-specific deadline (R17, R19, R28); the
taker's spend search polls at ten seconds until the taker payment
locktime (R30); refundability polling retries after thirty seconds on a
transient coin error (R18).

**R48.** *Terminal swap-status broadcast.* On termination (R4) each side
MUST load its own completed swap record, redact secret material from
it (the complete redaction set is R50A item 3), and broadcast it on the
swap topic as an unsigned JSON object with exactly two members: a
`method` member whose value is the fixed string `swapstatus`, and a
`data` member carrying the redacted record. This message is deliberately
*not* wrapped in the signed envelope of R43. R50A binds when the report
is made, for which swaps, what it contains, and what a receiver does
with it.

**R49.** *Receiver handling of the status broadcast.* A receiver that
fails to decode an incoming payload as the signed envelope of R43 MUST
attempt to decode it as the R48 status object, and on success MUST store
the record in its counterparty-statistics store. Only if both decodes
fail is the payload discarded. Implementations MUST retain this
two-attempt fallback: it is the only mechanism by which a node learns
its counterparty's terminal outcome.

**R50.** *Secret redaction is mandatory.* The redaction of R48 MUST
remove the maker's secret from the broadcast record. The record is
broadcast on a public topic; a maker that broadcasts an unredacted
record before its own spend confirms discloses the secret to any
observer. R50A item 3 binds the full set of fields the redaction
touches; the secret is the one whose omission is a loss of funds.

**R50A.** *The terminal status report: scope, content, redaction and
receiver storage.* (Authored by the KDF Spec Reader 2026-10-27 from the
rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at
both; **passed the KDF Dirty Gate on 2026-10-28**, see §51.16.) The broadcast of R48
is the legacy protocol's only report of a swap's outcome to the network,
and the counterparty-statistics store of R49 is populated from nothing
else. Its complete contract:

1. *When, and for which swaps.* The report MUST be made on **every**
   termination of a legacy maker or taker run loop (R4) — after the swap
   is marked finished and before the registry entry is released —
   whatever the terminal outcome: success, failure at any stage, refusal
   and abort alike. The report MUST be made for a swap in which either
   coin, or both, is a privacy (shielded) coin: there is **no**
   privacy-coin suppression at either reference row, so a node of either
   row publishes and expects to receive privacy-coin reports. Version-two swaps
   ([chapter 52](52-swap-v2-state-machine.md)) make no such report at
   either row. The report carries no netid logic and is identical on
   netid `8762` and `6133` (R69).
2. *What is published.* The `data` member of R48 is the node's complete
   persisted saved-swap record for the swap — the role-tagged record of
   [chapter 44](44-database-persistence-and-migrations.md) §44.8A with its
   identifiers, coins, amounts, GUI and daemon-version metadata, success
   and error vocabularies and full ordered event array — after the
   redaction of item 3 and nothing else is removed, added or renamed. A
   successful swap's record already carries the two fiat reference prices
   the node set on it at completion (both roles, both rows; chapter 42
   §42.6); a failed swap's record carries none; a receiver replaces
   whatever it finds with its own fetch (item 4). On a native build the
   node MUST first store the same redacted record in its own aggregate
   statistics store (item 5), then publish it; a browser build publishes
   without storing. A failure to load, store or publish
   the report MUST be logged and MUST NOT alter the termination sequence
   of R4.
3. *Redaction.* Exactly the following fields of the **start event's**
   payload (the first event of the record, type `Started` in both role
   vocabularies of chapter 44 R44.8A.3 / R44.8A.4) are altered; no other
   event and no other field is touched:
   - maker record: the maker's **secret** is replaced by the all-zero
     value of its 32-byte field (R50); the **taker's public key** field is
     replaced by the all-zero value of its fixed-width field;
   - taker record: the **maker's public key** field is replaced by the
     all-zero value of its fixed-width field;
   - both records: when the swap signed its peer-to-peer traffic with a
     per-swap ephemeral key (R43) — which is the case whenever the node's
     own coin in the swap is a privacy coin — the node's **own persistent
     public key** field is replaced by the all-zero value of its
     fixed-width 33-byte field; and, in every case, the **ephemeral key**
     field is emptied so that the published record carries no key
     material for it.
   Two consequences are the point of the design and MUST be preserved:
   each party's report identifies, by persistent key, **at most itself**,
   never its counterparty; and a privacy-coin party's report identifies
   neither party. The per-coin HTLC public keys and swap-contract
   addresses of the start event are not redacted (but see the
   code-quality finding below).
4. *What a receiver does.* A native-build node subscribed to the swap
   topic that receives a payload which is not the signed envelope of R43
   and decodes as the R48 object (R49) MUST fetch the current fiat
   reference prices of the two coins ([chapter 42](42-price-service-and-makerbot.md)
   §42.6), set them on the record, and store the record in the aggregate
   statistics store of item 5; a decode or storage failure is logged and
   the payload dropped. A browser build does not attempt the second
   decode and stores nothing. The report is unauthenticated (R48): the
   store is statistics, and nothing in the swap machines or the
   reservation rules of §51.8 MUST ever read it as a trust input.
5. *What the store keeps.* The aggregate statistics store is the
   `stats_swaps` table whose schema, index and migrations
   [chapter 44](44-database-persistence-and-migrations.md) §44.6 binds;
   this item binds only how a received or self-published record fills
   it. A record whose swap identifier is not yet present creates the row
   with the base and split-coin columns (ch. 44 §44.6.1, §44.6.3); a
   record whose identifier is present creates nothing. In both cases the
   optional columns are then updated from the record: the fiat price
   columns (§44.6.4) from the prices of item 4, the GUI and version
   columns (§44.6.6) from the record's metadata, and the two public-key
   columns (§44.6.5) **from the record's own role only** — a maker record
   supplies `maker_pubkey` from the maker's persistent key in its start
   event and never `taker_pubkey`; a taker record supplies `taker_pubkey`
   and never `maker_pubkey`. A swap's two key columns are therefore both
   filled only once both parties' reports have been received, and a
   privacy-coin party's column stays empty by item 3. The state-15
   backfill of ch. 44 R44.6.3 applies the same own-role derivation to
   historical records.

> **Reloaded divergence (2026-10-27).** Reloaded diverges on three points
> of R50A, the first by a decision the maintainer has since reversed in
> favor of parity. (i) *Suppression.* Reloaded still skips the report
> when either coin is a privacy coin
> (`mm2src/mm2_main/src/lp_swap/maker_swap.rs:2056` computes the
> condition and `:2117` gates the broadcast on it;
> `mm2src/mm2_main/src/lp_swap/taker_swap.rs:369` and `:419` likewise;
> the shielded coin answers the predicate at `mm2src/coins/z_coin.rs:2661`).
> The maintainer decided parity: **publish**, as item 1 binds. (ii)
> *Redaction set.* Reloaded's redaction
> (`mm2src/mm2_main/src/lp_swap/saved_swap.rs`, the redaction method)
> zeroes the maker's secret and empties the ephemeral key, and
> additionally empties two per-swap HTLC private-key fields that
> Reloaded's start-event payload carries and the reference's does not —
> those two removals are correct and MUST stay — but it leaves the
> counterparty's public key and the node's own persistent key in place,
> so a Reloaded report identifies both parties even for a privacy-coin
> swap. Item 3 binds the reference set on top of Reloaded's own two
> fields. (iii) *Key columns.* Reloaded's statistics fill
> (`mm2src/mm2_main/src/database/stats_swaps.rs`, the per-role key
> derivation) takes **both** parties' keys from each record — its own
> from the start event and the counterparty's from the negotiation
> event — and writes both columns from either role's report, which item
> 5 forbids; ch. 44 R44.6.3's backfill in Reloaded does the same. A Coder
> closing R50A changes the three together: remove the suppression, widen
> the redaction, and narrow the column fill to own-role, so that a
> published Reloaded record and a received reference record are stored
> by the same rule.

> **As built (2026-10-28, PR #108).** The three points above are closed:
> the report is made on every legacy termination with no privacy-coin
> condition; the redaction zeroes exactly the start-event fields of item
> 3 on top of Reloaded's two HTLC private-key fields (the negotiation
> event is untouched); the statistics fill and the state-15 backfill set
> only the record's own-role key column, and leave it unset (not an
> all-zero value) when the own key is zero or absent. Open: for a swap
> identifier that is already present, Reloaded sets the own-role key
> column only; the fiat price, GUI and version columns of item 5 are
> still not refreshed from the later report. State 15 is already applied
> on existing databases, so their stored key columns keep the old
> both-parties values until a later migration corrects them. Tests:
> T26–T28 in `mm2src/mm2_main/src/lp_swap/terminal_report_tests.rs`.

> **Code-quality finding (informative; trigger: a contradiction between
> two things read side by side).** At both reference rows the redaction
> of item 3 removes the counterparty's persistent public key from the
> start event, with the stated intent that a statistics node learn each
> party's key only from that party's own report. The same record's
> negotiation event (`Negotiated` in both role vocabularies of chapter 44
> R44.8A.3 / R44.8A.4) still carries the counterparty's negotiation
> payload, whose fields include the counterparty's **persistent public
> key itself** (the `persistent_pubkey` of R59) and its two per-coin HTLC
> public keys, so a receiver recovers the redacted key from the second
> event in the array. The redaction therefore achieves its attribution
> intent only when the swap failed before negotiation completed. R50A
> binds the reference set as-is for wire and storage parity. Proposed
> resolution for the maintainer, not a Coder: if attribution hygiene is
> wanted, extend item 3 to zero the counterparty's key fields of the
> negotiation event as well; because the record is unsigned and consumed
> only by statistics stores, the extension breaks no peer, but it should
> be adopted as a documented Reloaded-original divergence, not silently.

## 51.8 Bound Reserved-Funds Semantics

**R51.** *Reservation follows registry membership.* A swap's funds are
reserved for exactly as long as the swap has an entry in the in-memory
running-swap registry. The entry MUST be created before the first stage
runs and MUST be removed when the run loop exits (R4, R39). The registry
MUST be keyed by swap identifier so that removal is exact and
unconditional; a design in which entries expire only when the swap
object is dropped, or in which removal is conditional on how the swap
ended, does not satisfy this rule.

**R52.** *A terminated swap reserves nothing.* Once a swap has
terminated, its contribution to every reservation total MUST be zero,
regardless of which transactions it did or did not broadcast. This
explicitly includes swaps that terminated during `STAGE-START` or
`STAGE-NEGOTIATE`, before any transaction existed. Any implementation
whose per-stage reservation predicates are phrased purely as "has
transaction X been sent yet?" MUST additionally short-circuit on
termination, because such predicates answer "not sent" forever for a
swap that ended before sending anything.

**R53.** *Reservation totals.* The reserved total for a coin MUST be the
sum, over all registry entries, of each entry's declared reserved
amounts for that coin, plus each declared trade fee whose coin matches
and which is not marked as payable out of the trading volume itself. A
variant of the total that excludes one named swap MUST exist and MUST be
the form used when checking the balance for that same swap, so a swap
never blocks itself.

**R54.** *Maker per-stage reservations.* The maker MUST declare:

| Condition                                    | Reserved                                                        |
|----------------------------------------------|-----------------------------------------------------------------|
| Maker payment not yet broadcast              | the full maker volume in the maker coin, plus the maker payment's trade fee |
| Taker payment not yet spent                  | zero volume in the taker coin, plus the taker-payment-spend trade fee |
| Swap terminated                              | nothing (R52)                                                   |

The zero-volume-with-fee entry is not redundant: it reserves the taker
coin fee headroom needed to claim the incoming payment, which would
otherwise be spendable by another trade and leave the swap unable to
collect. What it is worth is decided by R53 and can be nothing: a coin
that pays its spend fee out of the payment being claimed marks the fee
accordingly and contributes zero, while a coin that pays it from the
account balance contributes the fee in whichever coin the descriptor
names. Chapter 52 R64 binds the same reservation for the version-two
protocol and MUST arrive at the same amount; a node runs both and
computes one maximum-tradable-volume answer over the union.

**R55.** *Taker per-stage reservations.* The taker MUST declare:

| Condition                                    | Reserved                                                        |
|----------------------------------------------|-----------------------------------------------------------------|
| Taker fee not yet broadcast                  | the dex fee amount in the taker coin, plus the fee-sending trade fee |
| Taker payment not yet broadcast              | the full taker volume in the taker coin, plus the taker payment's trade fee |
| Maker payment not yet spent                  | zero volume in the maker coin, plus the maker-payment-spend trade fee |
| Swap terminated                              | nothing (R52)                                                   |

**R56.** *Dex fee reservation uses the taker's own key.* The dex fee
amount reserved by R55 MUST be computed with the same inputs the taker
will later use to construct it — taker coin, maker coin ticker, taker
volume, and the taker's own taker-coin public key — so that a discounted
or no-fee trade (chapter 08) reserves the amount it will actually spend
rather than the undiscounted amount.

**R57.** *Release is not deferred to persistence.* The release of R51
MUST be observable immediately on termination in the same process. It
MUST NOT depend on the swap being marked finished in persistent storage,
on a subsequent restart, or on any garbage-collection pass, because the
reservation total feeds the maximum-tradable-volume computation that a
user or a market-making client may query in the next second.

**R58.** *Reservation is process-local.* The running-swap registry is
in-memory only and is cleared on restart; reservations are reconstructed
by resuming unfinished swaps from persistent storage (R6). It follows
that a reservation leak is unbounded within a process lifetime and is
cleared only by restarting, which is why R52 and R57 are stated as hard
requirements rather than as optimisations.

**R58A.** *Registry lifecycle.* (Authored by the KDF Spec Reader
2026-10-27 from the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02
§2.15A, identical at both; passed the KDF Dirty Gate on 2026-10-27, see §51.16.) The
running-swap registry of R51 is a map from swap identifier to a strong
(owning) handle on the running swap, with this lifecycle:

1. *Insertion.* An entry is inserted when the swap's run loop begins —
   for a maker swap and a taker swap alike, and whether the swap was
   freshly negotiated or resumed from storage at node start (R6). A
   resumed swap and a fresh one enter the same run loop and therefore
   the same insertion point; there is no separate kick-start path that
   could miss it.
2. *Removal.* The entry is removed by key when the run loop exits, on
   the maker path and on the taker path (R4), on every exit — normal
   completion, failure, refusal and abort. Removal is an explicit
   operation and never depends on the last handle being dropped.
3. *Shutdown.* When the node is stopping, the registry is cleared in one
   step once the stop signal has been observed. The node does **not**
   wait for the number of running swaps to reach zero. Clearing stops no
   swap — swaps are stopped by canceling the task scopes they run
   under — it only drops the strong handles so that the shared
   application context can be released.
4. *Reads.* Every question about running swaps — the count, the list of
   active swaps with their protocol versions, the swaps that use a given
   coin set, and the reservation totals of R53 — is answered from the
   map's entries directly. There is no liveness probe: an entry is a
   running swap, by construction of items 1–3.

> **Reloaded divergence (2026-10-27).** Reloaded today keeps the
> baseline's shape (V3): a vector of weak handles, appended at swap start
> and never pruned. A finished swap leaves a dead entry behind, the
> count is the number of entries whose weak handle still upgrades, every
> count and reservation query walks all entries, and shutdown spins until
> that count reaches zero. For a market maker the vector grows by one
> dead entry per completed swap for the process lifetime. R51 and R57
> already forbid the reservation side of this shape; R58A binds the rest.

**R58B.** *Per-swap message store (shared weakness; Reloaded-original
improvement).* The legacy protocol keeps, per running swap, a store for
the counterparty messages received for that swap: keyed by swap
identifier, created when the swap starts, and accepting a message only
from the negotiated counterparty's key (a message for an identifier with
no store, or from any other key, is ignored). At both rows the reference
never removes a legacy store entry — the map grows by one entry per swap
for the process lifetime — whereas the version-two engine (the subsystem
of [Chapter 52](52-swap-v2-state-machine.md)) removes its store entry
when its swap finishes; that is a reference fact at both rows, not a rule
chapter 52 states. Reloaded is in the same state today (V7). Reloaded
SHALL remove the store entry when the run loop exits, at the point of
R58A item 2. The two observable effects are intended: a late message for
a finished swap is ignored exactly as a message for an unknown swap is;
and the watcher-side check that declines to watch a swap this node is
itself a party to, which consults the store, no longer recognizes a
finished swap — a difference only for a reused identifier, which
version-4 random identifiers make negligible. This is a
Reloaded-original divergence from both rows, documented here as such.

## 51.9 Bound Wire Field and Type Expectations

**R59.** *Three negotiation-data shapes, untagged.* The negotiation
payload of the `Negotiation` and `NegotiationReply` kinds MUST be one of
three shapes, discriminated by *structure* rather than by a tag, and
attempted in declaration order:

| Shape | Fields                                                                                                                   |
|-------|--------------------------------------------------------------------------------------------------------------------------|
| 1     | `started_at`, `payment_locktime`, `secret_hash`, `persistent_pubkey`                                                     |
| 2     | shape 1 plus `maker_coin_swap_contract`, `taker_coin_swap_contract`                                                       |
| 3     | `started_at`, `payment_locktime`, `secret_hash`, `maker_coin_swap_contract`, `taker_coin_swap_contract`, `maker_coin_htlc_pub`, `taker_coin_htlc_pub` |

Field names and their order are dictated interop and MUST NOT change.

**R60.** *Shape selection on send.* A sender MUST emit shape 3 whenever
its two per-coin hash-time-locked-contract public keys differ from each
other or either differs from its persistent public key; otherwise it MUST
emit shape 2. Shape 1 MUST be accepted on receipt but MUST NOT be
emitted. This keeps a single-key node's messages decodable by peers that
predate the two-key split.

**R61.** *Accessor collapse.* A receiver MUST expose the two per-coin
public keys uniformly across all three shapes: for shapes 1 and 2 both
per-coin keys MUST resolve to `persistent_pubkey`; for shape 3 they
resolve to their own fields. Likewise, the two swap-contract fields MUST
resolve to absent for shape 1 and to their own values otherwise. A
receiver MUST NOT branch on shape anywhere else.

**R62.** *Public-key fields are exactly 33 bytes.* Every public-key
field in R59 — `persistent_pubkey`, `maker_coin_htlc_pub`,
`taker_coin_htlc_pub` — MUST be a fixed-width 33-byte value. It MUST be
serialised as a byte sequence of exactly 33 elements. A deserialiser
MUST accept either a byte string or a sequence and MUST reject, as a
length error, any input whose length is not exactly 33. There is no
variable-length public-key form on this wire.

**R63.** *The coin-layer key contract is 33 bytes for every chain.* The
coin-layer operation that derives a node's per-coin
hash-time-locked-contract public key MUST return exactly 33 bytes for
every supported chain, and the coin-layer operation that validates a
counterparty's key MUST accept exactly 33 bytes. This chapter binds
these two operations as `derive_htlc_pubkey` and `validate_other_pubkey`
respectively. Neither the swap machines nor the negotiation message may
be made key-length-polymorphic; the 33-byte width is a property of the
deployed wire format, not of secp256k1.

**R64.** *Representation of non-secp256k1 keys — the dictated padding
convention.* A chain whose native key is not a 33-byte secp256k1 point
MUST occupy the 33-byte field by a fixed, chain-specific convention
rather than by shortening the field. For a chain whose native key is a
32-byte Edwards-curve (ed25519) value, the dictated convention is:

- **on send:** place the 32 native key bytes in the field's *first* 32
  byte positions and set the final byte to zero;
- **on validate:** require the received field to be exactly 33 bytes,
  then interpret the *first* 32 bytes as the native key and ignore the
  final byte;
- **on use:** every downstream consumer that needs the native key MUST
  take the leading 32 bytes.

Both the padding position (trailing) and the pad value (zero) are
dictated by the deployed format and MUST be reproduced exactly; a
leading pad or a non-zero pad is not interoperable. A chain whose native
key is genuinely a 33-byte secp256k1 value (which includes chains built
on Cosmos-family software configured for secp256k1) uses the field
directly with no padding, and its validator MUST reject any input that
is not a well-formed secp256k1 point.

**R65.** *Secret-hash field width is shape-dependent.* The `secret_hash`
field MUST be a fixed-width 20-byte value in shape 1 and a
variable-length byte sequence in shapes 2 and 3. This asymmetry is
deliberate and MUST be preserved: shape 1 predates the introduction of
32-byte secret-hash algorithms, and widening it would make shape 1
ambiguous with shape 2 under the structural discrimination of R59.
A receiver MUST NOT assume 20 bytes when reading shapes 2 or 3.

**R66.** *Contract-address fields are opaque byte sequences.* The two
swap-contract fields MUST be variable-length byte sequences with no
length constraint, and an empty sequence MUST be the encoding of
"this coin has no swap contract". The per-coin acceptance check of R11
check 4 and R24 is the coin layer's, not the message layer's; the
message layer MUST NOT validate these fields.

### 51.9.1 Secret-Hash Algorithm Selection (dictated interop)

**R71.** *The secret-hash algorithm is coin-pair-selected, not
universal.* The `secret_hash` value carried by R59 MUST be produced by
a hash algorithm chosen once per swap from the negotiated maker/taker
coin pair, not by applying one hash algorithm unconditionally to every
swap. Exactly two algorithms are in scope: a 20-byte default (the
`RIPEMD160(SHA-256(secret))` composition already assumed by R65's
shape-1 width) and a 32-byte alternate (`SHA-256(secret)`). The
selection MUST be a pure function of the two coins' identities, MUST
be evaluated independently and identically by both counterparties (it
is not itself carried on the wire — only its 20- or 32-byte result
is), and MUST agree with the algorithm every other conforming peer
implementation selects for the same coin pair; disagreement is a
protocol-level incompatibility, not a display concern.

**R72.** *A native-32-byte-hash-lock coin on either side forces the
32-byte algorithm.* A coin family whose own atomic-swap payment
construction commits to a native 32-byte hash value — at minimum, the
Cosmos/Tendermint IBC-HTLC family ([Chapter 18](18-tendermint-ibc-htlc.md)),
Bitcoin Lightning ([Chapter 41](41-lightning-network.md)), and Siacoin's
native spend-policy HTLC ([Chapter 20](20-siacoin-integration.md) R-H1,
R-H2) — MUST cause the 32-byte alternate of R71 to be selected whenever
that coin family appears on **either** the maker or the taker side of
the pair, regardless of what the other side would otherwise select.
Every coin pairing not covered by this rule MUST select the 20-byte
default. This "widest-commitment wins" selection is dictated (R29/R33):
it is the only way a swap paired against one of these coin families can
produce a `secret_hash` value the native hash-lock on that side is
actually capable of verifying — the native construction has no
provision for accepting or being satisfied by a differently-sized
external value, and this chapter's own wire contract (R65) already
anticipates a 32-byte secret hash existing on shapes 2 and 3. No coin
pairing in scope of this rule is refused at negotiation time, or
restricted to a different swap-protocol version, merely because it
requires the 32-byte alternate: the legacy V1 path selects the wider
algorithm per R71/R72 and proceeds like any other pairing, including
pairings where the *other* side is a coin family that has no native
32-byte hash-lock of its own.

**R72A.** *Every leg consumes the one shared value; each coin family's
own construction decides how.* R71's `secret_hash` is computed once per
swap and used identically by both legs (R71); it is never renegotiated,
recomputed, or replaced per leg. What differs per leg is entirely
downstream of that shared value, inside each coin family's own
payment-construction, payment-validation, and secret-extraction
operations (the external coin-layer dependency named in §51.2). Every
such operation MUST treat the shared `secret_hash` as its sole source of
truth for the hash commitment and MUST derive whatever value it embeds,
compares, or reconstructs from it in a manner appropriate to what that
coin family's own native hash-lock construction is actually capable of
verifying, rather than assuming the shared value's raw bytes are always
the value to use:

- A coin family whose native construction accepts the hash commitment as
  an opaque value of whatever width R71/R72 produced — verifying a
  revealed secret against it by the coin's own logic rather than by a
  fixed-width on-chain primitive applied to the secret — MUST use
  `secret_hash` unchanged, at whichever width R71/R72 selected.
- A coin family whose native construction instead verifies a revealed
  secret by applying its own fixed-width, non-configurable hash
  primitive to the secret at spend time — for example, a hash-time-lock
  construction whose satisfaction opcode has no form capable of
  consuming a hash wider than that primitive's output — MUST derive the
  value it embeds and compares as follows: when `secret_hash` already
  carries that native output width, use it unchanged; when
  `secret_hash` carries the 32-byte alternate of R72, derive the
  embedded/compared value by applying that same fixed-width primitive
  once, directly to the 32-byte `secret_hash` value itself, with no
  further hashing of the secret. For every coin family in scope of R72
  today, the 32-byte alternate is defined as the wide half of that
  coin's narrower composite primitive's own construction (R71), so this
  single-application derivation is mathematically guaranteed to equal
  what the coin's on-chain primitive independently computes from the
  real secret at spend time.

A coin-layer implementation that instead uses the shared `secret_hash`'s
raw bytes unconditionally in a fixed-width embed/compare, or that
rejects a `secret_hash` wider than its own native output instead of
deriving from it per the second case above, cannot construct, validate,
or spend a payment for any pairing where R72 selects the wide algorithm
on that coin's side — even though negotiation (R71/R72) and any
validation step that only reconstructs and structurally compares the
construction (rather than executing it) both complete without error,
because neither observes the mismatch that only on-chain execution of
the native primitive against the real secret would reveal.

**R72B.** *The derivation of R72A is a coin-layer duty, not a
swap-machine duty.* The maker and taker stage graphs of §51.4 and §51.5
MUST pass R71's single `secret_hash` value to the external coin-layer
operations of §51.2 unchanged, at every stage that constructs,
validates, spends, refunds, or extracts a secret from a payment (R17,
R28–R31 and their maker-side counterparts). Neither stage graph may
itself branch on `secret_hash`'s width, substitute a derived value
before calling into the coin layer, or assume that the value one
coin-layer operation is given is the literal byte value another chain's
construction exposes on-chain. Any derivation R72A requires MUST happen
entirely inside the coin-layer operation for that specific coin family.

**R73.** *Algorithm selection is orthogonal to shape selection.* The
negotiation-shape choice of R60 (shape 2 vs. shape 3) turns solely on
per-coin HTLC-public-key equality; it MUST NOT be influenced by, and
MUST NOT influence, which secret-hash algorithm R71/R72 selects. A
32-byte `secret_hash` therefore always lands inside the
already-variable-length field that shape 2 or shape 3 carries (R65); no
additional shape and no widening of shape 1 is needed, or permitted, to
carry it.

> **Code-quality finding (informative).** This repository's own legacy
> maker- and taker-swap state machines compute `secret_hash` with the
> 20-byte default algorithm unconditionally, for every coin pair, with
> no coin-pair-based selection step anywhere in that computation. A
> coin-pair hash-algorithm-selection primitive matching R71/R72 already
> exists elsewhere in this repository's cryptography surface, but the
> legacy swap state machines never call it. The practical consequence is
> that a swap pairing any coin governed by R72 (Sia, Lightning, or a
> Tendermint-family coin) still receives the 20-byte value, which that
> coin's own native hash-lock construction cannot consume — for Sia
> concretely, this is the field-width mismatch a live swap in this
> defect area was observed to fail on. Wiring the selection into both
> state machines' secret-hash computation is a genuine correctness gap
> to close, not a discretionary style choice.
>
> A second, compounding gap sits downstream of the first: this
> repository's own taker-side negotiation-result storage and its
> maker-negotiation-data carrier both type `secret_hash` as a
> fixed-20-byte value rather than the variable-length byte sequence R65
> already requires for shapes 2 and 3. Even after the selection gap
> above is closed, a received 32-byte `secret_hash` would still fail or
> be truncated when the taker stores the negotiated value, because the
> carrying type cannot hold more than 20 bytes. Both gaps must be closed
> together for a swap against an R72 coin to complete; closing only the
> selection gap would move the failure from "wrong hash sent" to "wrong
> hash received," not remove it.
>
> A third gap, independent of the first two and confirmed against a
> reproduced live-network failure, sits inside this repository's own
> UTXO-family coin-layer payment construction, payment-validation
> reconstruction, and spend-secret-extraction functions: none of them
> perform the width-dependent derivation R72A now requires. Each of them
> instead uses the shared `secret_hash` value's raw bytes unconditionally
> — embedding it directly into the fixed-width hash-comparison slot of
> the constructed payment, reconstructing the identical fixed-width
> comparison on the validating side, and comparing an observed spend's
> revealed secret's fixed-width digest directly against `secret_hash`'s
> raw bytes rather than against a derived comparator. UTXO's native
> hash-lock satisfaction opcode is fixed-width and non-configurable and
> is exactly the second case R72A describes, so once the first two gaps
> are closed and a UTXO-paired swap actually receives and stores the
> 32-byte alternate, this third gap makes the constructed payment one the
> native opcode can never be satisfied against — for anyone, including
> the legitimate secret holder — because a fixed-width on-chain primitive
> applied to the real secret can never equal a differently-sized embedded
> value. This is invisible at both construction and validation time
> because validation only reconstructs and structurally compares the
> same (uniformly wrong) expected construction that construction itself
> produced; nothing on either side executes the on-chain primitive until
> an actual spend is attempted, which is the point at which a live swap
> in this defect area was observed to fail, after which the payment
> remains spendable only via the refund timelock. All three gaps — the
> selection gap, the storage-width gap, and this construction/validation/
> extraction-derivation gap — must be closed together for a legacy V1
> swap pairing an R72-governed coin against a UTXO-family coin to
> complete on the honest-spend path; closing any subset moves the failure
> to a different one of the three without removing it. The same class of
> gap is plausible in this repository's other coin families whose native
> hash-lock construction is likewise built on a fixed-width,
> non-configurable primitive rather than an opaque-width comparison, but
> confirming that is outside what this chapter's UTXO-focused reading
> covers.
>
> **Resolved.** All three stacked gaps this finding described are closed.
> The selection gap: the maker machine now calls `select_secret_hash_algo`
> once per swap and persists its result as `secret_hash`
> (`maker_swap.rs`, commit `bfa0e41b3`); the taker machine never computed
> its own value in the first place (it adopts whatever the maker sent in
> the `Negotiated` message, R59), so closing the maker side closes both
> roles. The storage-width gap: both `MakerSwapData::secret_hash` and
> `TakerSwapData::secret_hash` are `BytesJson` (variable-length), not a
> fixed-20-byte type, so a 32-byte value round-trips through persistence
> and the negotiation wire intact. The UTXO construction/validation/
> extraction-derivation gap: `htlc_hash160_comparator` now performs the
> exact R72A derivation -- a 20-byte `secret_hash` is used unchanged, a
> 32-byte one has `RIPEMD160` applied to it once -- and every UTXO
> payment-construction, payment-validation-reconstruction, and
> spend-secret-extraction call site routes through it instead of
> embedding `secret_hash`'s raw bytes (`utxo_common_swap.rs`, commit
> `8ae48f460`). A Sia-paired (or Lightning- or Tendermint-family-paired)
> legacy swap negotiated by this repository today gets the 32-byte
> algorithm and a UTXO counterparty that can actually spend against it.
> This does **not** retroactively fix a swap already persisted with the
> pre-fix 20-byte value -- recovering that swap's payment still fails
> exactly as this finding described, now surfaced as a named error
> instead of a raw parse failure (`SiaCheckIfMyPaymentSentArgsError::
> WrongSecretHashLength`, `SiaValidatePaymentInputError::
> WrongSecretHashLength`) rather than something recovery can act on.

## 51.10 Bound Reference-Version Split

**R67.** *The legacy state machine is identical across both reference
lineages.* For the substrate this chapter binds — the stage sets, the
event sets, the resume maps, the acceptance checks, the refusal
conditions, the message contract, the timeout budgets, the reservation
predicates, and the wire field widths — the `v2.6.0-beta` legacy
contract and the current v3-lineage legacy contract are the same. This
chapter therefore binds a single contract for both, and an
implementation does not need a reference-version switch anywhere in the
legacy swap machines.

**R68.** *Differences that exist are outside this substrate.* Where the
v3 lineage differs in the legacy files it does so only in: the dex-fee
rate and discount policy and the KMD direct-burn policy (on netid 8762
a KMD taker fee has three ranges, dust-only, clamped split and 75/25
split; on netid 6133 it is always a single output), which are bound
by [chapter 08](08-fee-routing-engine.md) R8/R15B and by the network
configuration and are therefore netid-selected, not
swap-machine-selected; the
plumbing by which a watcher-reward amount reaches the coin layer, which
is an internal detail carrying no wire or persisted-log consequence; and
the argument lists of two coin-layer operations, likewise with no wire
consequence. None of these change any rule in this chapter.

**R69.** *Netid applicability.* Because R67 holds, the legacy swap
machines are netid-independent. Netid `8762` and netid `6133` MUST run
the identical legacy state machine. Everything netid-specific that a
legacy swap consumes — the dex fee policy, the dex fee recipient, the
discount ticker set — MUST be reached through the active network
configuration, consistent with the repository-wide rule that
network-specific behaviour is selected by configuration rather than by
divergent code paths.

**R70.** *No silent upgrade.* An implementation MUST NOT allow a v3-era
change to alter any rule in this chapter for netid `8762` without an
explicit, documented compatibility boundary. In particular the boolean
acknowledgement of R33 MUST NOT be widened into a richer refusal type,
and the 33-byte key width of R62 MUST NOT be relaxed, on either netid.

## 51.11 Invariants

| Invariant                                                              | Bound by      |
| ---------------------------------------------------------------------- | ------------- |
| Failure is an event plus a next stage, never a run-loop error          | R1            |
| Wire output precedes event persistence; persistence precedes next stage | R2, R3       |
| Resume point is a total function of the last persisted event           | R7, R19, R32  |
| Stage handlers tolerate full replay                                    | R7, R17, R29  |
| One runner per swap identifier, lock refreshed faster than its TTL     | R5            |
| Clock-agreement tolerance is exactly 60 seconds on both sides          | R12, R24      |
| Maker locktime uses the multiplier; taker locktime does not            | R10, R22, R25 |
| A negative negotiation acknowledgement is transmitted, once, on seven conditions | R33, R34, R35 |
| Refusal is broadcast before the failure event is persisted             | R36           |
| No refusal message exists after the negotiation stage                  | R38           |
| Refusal causes the refused taker to penalise the maker for one hour    | R40           |
| Six message kinds in fixed discriminant order, signed, sender-pinned   | R41, R43      |
| Single-slot take-on-read inbox makes repeated broadcast idempotent     | R44           |
| Every receive budget carries a fixed 90-second grace allowance         | R46           |
| Terminal status broadcast is unsigned JSON with a fixed method string  | R48, R49      |
| The maker's secret is redacted from the terminal broadcast             | R50           |
| The terminal report is made on every legacy terminal, privacy coins included, V2 never | R50A item 1 |
| A report identifies by persistent key at most its own author; a privacy-coin party identifies no one | R50A items 3, 5 |
| Reservation exists exactly while the registry entry exists             | R51           |
| A terminated swap reserves nothing, even if it never broadcast anything | R52, R57     |
| Public-key fields are exactly 33 bytes on every chain                  | R62, R63      |
| Ed25519 keys occupy the field by trailing zero pad, read from the front | R64          |
| Secret-hash width is 20 bytes in shape 1, variable in shapes 2 and 3   | R65           |
| Secret-hash algorithm is coin-pair-selected; a native-32-byte-hash-lock coin on either side forces the 32-byte algorithm | R71, R72 |
| The shared secret_hash is never refused, renegotiated, or replaced per leg; each coin family's own construction derives what it needs from it | R72A, R72B |
| Algorithm selection never changes which negotiation shape is emitted   | R73           |
| The legacy machine is identical across reference lineages and netids   | R67, R69      |

## 51.12 Tests

**T1.** *Resume map totality.* For each role, every event type in R19 /
R32 maps to exactly one resume stage or to termination, and the set of
event types accepted by the persisted-log parser (chapter 44 R44.8A.3 /
R44.8A.4) equals the set in the resume map.

**T2.** *Success/error partition.* For each role, the success and error
classifications of R19 / R32 partition the event set with no overlap and
no omission, and the emitted `success_events` / `error_events` arrays
equal the bound vocabularies of R32A.

**T3.** *Refusal on each of the seven conditions.* Seven maker-side
tests, one per row of R34, drive the maker into that condition and
assert that a negative acknowledgement is broadcast exactly once, that a
`NegotiateFailed` event is persisted after the broadcast, and that the
next stage is terminal.

**T4.** *Acceptance is repeated, refusal is not.* Asserts the positive
acknowledgement is re-broadcast on the R45 interval for the duration of
the fee-await stage, and that the negative acknowledgement is broadcast
exactly once with no repeat loop created.

**T5.** *Taker honours the refusal.* A taker driven to the
acknowledgement wait, given a negative value, terminates with
`NegotiateFailed` without sending a fee transaction, and records the
one-hour penalty against the maker's key (R40).

**T6.** *Taker honours the acknowledgement timeout.* The same taker,
given no acknowledgement at all, terminates with `NegotiateFailed` only
after the full 180-second budget of R46 has elapsed — distinguishing the
timeout path from the refusal path by elapsed time.

**T7.** *Clock-skew boundary.* Negotiation succeeds at a 60-second
start-time difference and both refuses and fails at 61 seconds, on both
roles.

**T8.** *Locktime expectation asymmetry.* A maker refuses a reply whose
payment locktime is start plus twice the lock duration, and a taker
fails a negotiation whose payment locktime is start plus one lock
duration — i.e. each role rejects the other role's formula.

**T9.** *Reservation released on pre-transaction failure.* A swap driven
to `NegotiateFailed` (both roles) contributes zero to the reserved total
for both coins immediately after termination, and the maximum tradable
volume returns to its pre-swap value in the same process without a
restart.

**T10.** *Reservation held while running.* The same swap, paused before
its first broadcast, contributes exactly the amounts of R54 / R55 to the
reserved total.

**T11.** *Self-exclusion.* The balance check performed for a swap
excludes that swap's own reservation, so a swap that is already
registered does not fail its own start check.

**T12.** *Trade-fee inclusion rule.* A declared trade fee marked as
payable out of the trading volume contributes zero to the reserved
total, while an unmarked one of the same amount contributes its amount.

**T13.** *Key-field length rejection.* Deserialising a negotiation
payload whose public-key field is 32 or 34 bytes fails with a length
error; exactly 33 bytes succeeds. Both the byte-string and the sequence
input forms are covered.

**T14.** *Ed25519 padding round trip.* For an ed25519-keyed chain, the
derived key field is 33 bytes whose final byte is zero and whose leading
32 bytes equal the native key; validation of that field succeeds;
validation of a 33-byte field whose leading 32 bytes are not a valid
curve point fails; and validation of a field of any other length fails.

**T15.** *Negotiation shape selection.* A node whose two per-coin keys
equal each other and its persistent key emits shape 2; a node whose keys
differ emits shape 3; a receiver resolves both per-coin keys to the
persistent key when given shape 1 or 2.

**T16.** *Secret-hash width.* A shape-1 payload with a 32-byte secret
hash is rejected; a shape-2 or shape-3 payload with a 32-byte secret
hash is accepted.

**T16a.** *Secret-hash algorithm selection.* For a coin pair where
neither side is governed by R72, the emitted `secret_hash` is the
20-byte default algorithm's output on the swap secret. For a coin pair
where an R72-governed coin is the maker, the taker, or both, the
emitted `secret_hash` is the 32-byte algorithm's output, and this holds
independently of which negotiation shape (R60) the same swap emits.

**T16b.** *Per-coin-family construction derivation (R72A).* For a coin
family whose native hash-lock construction verifies via a fixed-width
primitive applied to the raw secret, driving a swap through the 32-byte
branch of T16a produces, on that coin's leg: a constructed payment whose
on-chain construction the real secret actually satisfies; a
validation-by-reconstruction step on the counterparty's side that
accepts the identical construction; and, from an observed honest spend
of it, correct secret extraction. The same three checks pass for the
20-byte branch of T16a on a coin family that requires the opaque-width
form. Both a fixed-width-primitive coin family and an opaque-width coin
family are covered.

**T17.** *Sender pinning.* A correctly signed message from a key other
than the pinned counterparty key leaves every inbox slot unchanged.

**T18.** *Inbox overwrite and take-on-read.* Two successive messages of
one kind leave only the later payload readable, and reading a slot twice
yields the payload then nothing.

**T19.** *Status-broadcast fallback.* A payload that is not a valid
signed envelope but is a valid status object is stored in the
counterparty-statistics store; a payload that is neither is discarded
with no state change.

**T20.** *Secret redaction.* The terminal broadcast of a maker swap that
has revealed its secret on chain contains no secret material.

**T21.** *Replay safety of the payment stages.* Re-entering the maker
payment stage and the taker payment stage after the payment is already
on chain does not broadcast a second payment, and the taker payment
stage does not fail on an expired send deadline when its payment is
already on chain (R29).

**T22.** *Terminal-log resume aborts silently.* Resuming a swap whose
last persisted event is terminal emits no event and takes no registry
entry.

**T23.** *Reference-version equivalence.* The stage sets, event sets,
resume maps, acceptance checks, refusal conditions and budgets asserted
by T1–T22 hold identically when the swap is configured for netid `8762`
and for netid `6133` (R67, R69).

**T24.** *Registry returns to empty (R58A).* Run N legacy swaps to
completion on one node — any mix of maker and taker roles, successful
and failed. After the last run loop exits, the registry has no entries,
the running-swap count is zero, and the reservation total of every coin
is zero. Run N more: the registry's size never exceeds the number of
swaps concurrently running at any instant. A resumed swap (R6) counts
exactly once while it runs and zero after it exits.

**T25.** *Shutdown with a running swap, and store removal (R58A item 3,
R58B).* Stop a node while a legacy swap is in a waiting stage. The stop
completes without waiting for the swap to finish, the registry is empty
afterwards, and the swap's persisted event log is intact and resumes
under R6 at the next start. Separately, after a swap finishes normally
the node holds no message store for its identifier, and a message
addressed to that identifier is ignored.

**T26.** *A privacy-coin swap end publishes a redacted report (R50A
items 1–3).* Run a legacy swap between a shielded coin and a transparent
UTXO coin (for example ARRR against RIN) to each of: success, a
negotiation refusal, and a payment-stage failure, in both roles. On every
termination exactly one R48 object is published on the swap topic by
each side, after the swap is marked finished and before the registry
entry is released (R4); the test MUST fail if the publication is skipped
because either coin is a privacy coin. Repeat for a swap between two
transparent coins and assert the same. A version-two swap on the same
node publishes nothing of the kind. *(Added 2026-10-27 with R50A; passed the KDF Dirty Gate on 2026-10-28.)*

**T27.** *Redaction fields, exactly (R50A item 3).* For a published maker
record: the start event's secret is the all-zero 32-byte value, the
taker's public-key field is all-zero, and when the swap used a per-swap
signing key the maker's own persistent-key field is all-zero and the
ephemeral-key field carries no key; when the swap used the persistent key,
the own persistent-key field is intact. For a published taker record: the
maker's public-key field is all-zero and the same two conditional
redactions apply. In both: every other event, including the negotiation
event's key fields, and every other start-event field equals the stored
record byte for byte; and the node's own stored record (its `my_swaps`
copy) is **not** redacted. The test MUST fail if the two Reloaded-specific
HTLC private-key fields are present with a value. *(Added 2026-10-27 with R50A; passed the KDF Dirty Gate on 2026-10-28.)*

**T28.** *A receiver stores it (R50A items 4–5).* Deliver a published
maker report for an unknown swap identifier to a native-build node: a
`stats_swaps` row is created, `maker_pubkey` equals the maker's persistent
key from the record, `taker_pubkey` is unset, and the two fiat price
columns are set from the price source. Deliver the matching taker report:
no second row; `taker_pubkey` is now set and `maker_pubkey` is unchanged.
Deliver a maker report whose own persistent key is all-zero (privacy-coin
author): `maker_pubkey` stays unset. Deliver the same payloads to a
browser-build node: nothing is stored. Deliver a payload that is neither
the signed envelope nor the R48 object: it is dropped with no store
change. *(Added 2026-10-27 with R50A; passed the KDF Dirty Gate on 2026-10-28.)*

## 51.13 Deferred Work

**D1.** *Migration of the legacy machines onto the chapter-14 runtime.*
The legacy machines predate the persistent state-machine runtime and use
a hand-rolled loop. Migration is possible in principle but would have to
preserve the persisted log shape byte for byte (chapter 44) and the
resume maps of R19 / R32 exactly; it is not in scope.

**D2.** *A structured refusal reason.* R33 binds a boolean. A refusing
maker cannot tell the taker *why* it refused, so a taker cannot
distinguish a clock-skew refusal (retryable after a clock fix) from a
malformed-key refusal (not retryable). Widening the type is a wire break
and is prohibited by R70 on the current netids; carrying a reason in a
new, additively-numbered message kind (R41) is the migration path if it
is ever taken.

**D3.** *Penalty proportionality.* R40 applies the same one-hour penalty
to a counterparty that refused a negotiation for a clock-skew reason as
to one that sent an invalid payment. Differentiating the penalty by
cause is deferred.

**D4.** *Persistent reservation ledger.* R58 records that reservations
are process-local and rebuilt by resume. A persisted reservation ledger
would make the maximum-tradable-volume answer correct across a restart
before resumption completes; not in scope.

**D5.** *Refusal on the receive-timeout condition is unacknowledged.*
R37 requires transmitting a refusal to a peer the maker could not hear
from. Whether the peer hears the refusal is unverifiable, and no
retransmission is specified. A bounded retransmission of the negative
acknowledgement is deferred.

## 51.14 Baseline Verifications

The following are verifiable against the baseline state defined in
[chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`.

**V1.** The baseline tree contains both legacy role machines, their
stage enumerations, their event enumerations, and the six-kind message
envelope of R41. This chapter documents an existing baseline substrate;
it does not introduce one.

**V2.** The baseline tree contains the boolean negotiation
acknowledgement of R33 and the taker-side handling of its negative value
(R26 check 7). The baseline tree does **not** contain any maker-side
path that transmits the negative value. R34 is therefore the rule whose
absence from the present tree must be corrected; the receiving half of
the contract is already present and correct.

**V3.** The baseline tree's reservation predicates are phrased as
transaction-sent questions without a termination short-circuit, and the
baseline running-swap registry is a collection of weak references with
no keyed removal at termination. R51, R52 and R57 are therefore the
rules whose satisfaction requires a keyed registry with unconditional
removal on every exit path.

**V4.** The baseline tree's negotiation public-key fields are
variable-length byte sequences rather than the fixed 33-byte fields of
R62. Verifiable by inspection of the baseline negotiation payload
types. R62 through R64 are therefore tightening rules relative to the
baseline, and are required for interoperability with deployed peers that
reject any other length.

**V5.** The baseline maker refund path is a single stage, whereas R18
binds a three-stage prepare/execute/finalize chain with distinct
persisted events for each. The taker refund path is likewise a single
stage against R31's three. The additional events are already bound as
acceptable persisted values by chapter 44 R44.8A.3 and R44.8A.4, so no
persisted-log migration is implied.

**V6.** The baseline `WatcherMessageSent` event carries no payload,
whereas the reference lineages carry payload data on it. Chapter 44
R44.8A.5 already requires tolerant parsing of this event; this chapter
does not bind its payload shape.

**V7.** The baseline tree's legacy per-swap message store is created at
swap start and never removed, and its running-swap registry is the weak
vector of V3. R58A restores the keyed, owning registry the reference
rows carry; R58B is a tightening relative to the baseline and to both
reference rows alike.

## 51.15 External References

- The hash-time-locked-contract atomic-swap construction that the
  five-stage legacy dance implements; the version tag that selects it is
  bound by [chapter 13](13-swap-version-negotiation.md) R4.
- The publish-subscribe overlay carrying the swap topic; per-netid
  scoping is bound by [chapter 06](06-network-id-seed-node.md) and the
  substrate by [chapter 28](28-libp2p-modernization.md).
- The dex-fee descriptor, its rational arithmetic and its discount and
  burn forms — [chapter 08](08-fee-routing-engine.md).
- The third-party-watcher protocol and its topic naming —
  [chapter 09](09-watcher-reward-infrastructure.md).
- The persisted legacy swap record and its event `type` vocabulary —
  [chapter 44](44-database-persistence-and-migrations.md) §44.8A; the
  aggregate statistics table the terminal report of R48/R50A fills —
  chapter 44 §44.6; and the fiat price snapshot a receiver sets on the
  record — [chapter 42](42-price-service-and-makerbot.md) §42.6.
- The generic persistent state-machine runtime that the legacy machines
  deliberately do not use — [chapter 14](14-state-machine-runtime.md).
- The published JSON serialisation framework's byte-sequence
  serialisation and length-checked deserialisation semantics, on which
  R62 depends.
- Ed25519 public-key encoding (32-byte compressed Edwards point), on
  which R64's padding convention operates.
- The negotiation exchange that precedes this chapter's stage graphs —
  the four-message order-match loop, its selection and timeout
  semantics, and the one-hour counterparty penalty a taker applies when
  negotiation fails — [chapter 59](59-legacy-ordermatch-negotiation.md).
  That penalty is triggered by the `NegotiateFailed` taker event of
  §51.6, so §51.6's refusal conditions are what arm it: ch. 59 R59.5.2
  and R59.5.4 bind the penalty side, and ch. 59 R59.5.4 records that
  reloaded does not yet arm it from this event.
- The per-coin signature-hash selection every signing site of a UTXO
  coin in this chapter's stage graphs goes through —
  [chapter 58](58-utxo-signature-version-and-radiant-sighash.md) §58.2,
  including the sighash byte that the coin's fork-identifier value forms
  (ch. 58 R58.2.5), which this chapter's DEX-fee, payment, spend and
  refund transactions all carry.

## 51.16 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit of chapter 02 (the legacy role machines, the message envelope,
  the negotiation payload shapes and the reservation predicates all
  exist at baseline); chapter 01 (clean-room rules and the canonical
  chapter shape); chapter 08 (the fee descriptor consumed by R13, R27
  and R56); chapter 09 (the watcher topic referenced by R30 and R45);
  chapter 13 (the version tag that dispatches into this substrate);
  chapter 14 (the runtime this substrate is explicitly distinguished
  from); chapter 44 (the persisted event vocabulary this chapter gives
  semantics to); the publicly documented hash-time-locked-contract
  atomic-swap construction; published ed25519 key-encoding
  documentation; published JSON-serialisation attribute semantics;
  behavioural observation of deployed peers on the public mesh,
  including observation of a peer transmitting a negative negotiation
  acknowledgement.
- *Permitted-input classes used:* R1 (baseline source), R3 (external
  public specifications), R4 (dictated wire formats and interfaces the
  project must inter-operate with), R6 (behavioural observation of
  public networks), R7 (independent work).
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* (addition of 2026-10-25: the informative pointer
  after R38 restates chapter 60's corpus-sourced finding that no volume
  check precedes or accompanies negotiation; cleared by the Dirty Gate
  with [Chapter 60](60-trading-floors-and-refusal-contract.md) on
  2026-10-25, attestation in its §60.12.) (Addition of 2026-10-27, KDF
  Spec Reader, passed the KDF Dirty Gate on 2026-10-27: R58A, R58B, T24, T25 and V7 rest on
  the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A,
  identical at both, compared with the baseline anchor. The facts taken
  are the keyed strong-handle registry, its insertion and removal
  points, the one-step clearing at shutdown without waiting, the
  map-based reads, and the never-removed legacy message store beside a
  removed version-two one. No private identifier, function body, helper
  decomposition, log or error literal was reproduced. Dirty Gate
  clearance (chapter 01 T1, R40): PASS, measure 0.02, no leaks; every
  reference fact confirmed at both rows. *Extent:* the registry lifecycle,
  the shutdown path and the per-swap message stores, five files per row and two
  baseline files, with the row-to-row difference. *Method:* reading of the
  compared regions plus a mechanical regex scan of every added line against
  private identifiers, literals and distinctive phrases; the mechanical gate
  tool was not run (prose). *Measure:* 0.02 against the 0.10 threshold.)
  (Addition of 2026-10-27, KDF Spec Reader, **passed the KDF Dirty Gate on 2026-10-28**: R50A,
  T26–T28, the pointer sentences added to R48 and R50, the two invariant
  rows, the §51.1 range text and the §51.15 item rest on the rows
  `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, read at both and
  identical at both for every fact stated; The facts taken are: that the report is made on every
  legacy terminal with no privacy-coin suppression and never for a
  version-two swap; that the published object is the complete saved
  record after redaction; the exact redaction set — maker secret,
  counterparty persistent key, own persistent key when a per-swap key
  signed, and the per-swap key itself — confined to the start event; the
  native-only self-store before publication; the native-only receive path
  with fiat prices fetched at receive time; and the own-role-only fill of
  the two statistics key columns on insert-or-update. The Reloaded
  divergence note and the file:line citations in it are from this
  workspace. No private identifier, function body, control-flow
  transcription, helper decomposition, log or error literal was
  reproduced. Dirty Gate clearance (chapter 01 T1, R40): PASS on 2026-10-28, measure
  0.02, no leaks; every reference fact was confirmed at both rows. *Extent:* the
  legacy swap run-loop termination, the terminal swap-status report, its
  redaction, the swap-topic receive path and the swap-statistics store fill and
  backfill; the order-matching site that chooses a per-swap signing key; and the
  version-two swap machines for the absence claim, at `v2.6.0-beta` and
  `v3.0.0-beta` (eleven files per row). *Method:* reading of the candidate
  hunks against the reference regions at both rows, and a mechanical regex
  comparison of every added line against identifier, literal, comment-phrase,
  path and version-control-identifier lists; the mechanical gate tool was not
  run (prose). *Measure:* 0.02 against the 0.10 threshold.)
  Otherwise
  consulted, under the chapter-01 two-team
  clean-room workflow, for upstream parity of the legacy swap state
  machine only — specifically the refusal-transmission conditions of
  R34, the reservation-release mechanism of R51, the fixed-width
  key-field contract of R62 through R64, the refund-chain stage split of
  R18 and R31, the reference-lineage equivalence of R67, the coin-pair
  secret-hash-algorithm-selection contract of R71 through R73 (added
  prompted by a reproduced Siacoin V1-swap failure), and — added in this
  revision, prompted by a reproduced UTXO-family V1-swap spend failure —
  the per-coin-family construction/validation/extraction derivation
  contract of R72A and R72B and the no-refusal clarification appended to
  R72. For R71–R73 and for R72A/R72B alike, both the `v2.6.0-beta`
  legacy reference and the current v3-lineage reference were consulted
  and agree, byte-for-byte identical at the two consulted call sites:
  the selection and per-family derivation are dictated interop
  (R29/R33), reproduced here only as the two-algorithm choice, the
  widest-commitment-wins selection rule, the coin families it currently
  covers, the opaque-width-vs-fixed-width-primitive distinction between
  coin families, and the single-application derivation's cryptographic
  identity with the coin's own on-chain primitive — not as any internal
  function name, argument list, helper decomposition, or per-branch
  internal structure. The same fixed-width-primitive derivation pattern
  was also observed, at both reference points, in this workspace's
  EVM-legacy and QRC20-family coin-layer swap code, confirming R72A's
  two-case shape is the general one and not UTXO-specific; those other
  families' own chapters are not amended here and this observation is
  not a claim about this repository's EVM or QRC20 code, which this
  reading did not inspect. No source text, private helper structure,
  internal decomposition, log or error string, or per-method internal
  table was copied; every rule above is stated as an externally
  observable behavioural or cryptographic requirement.
