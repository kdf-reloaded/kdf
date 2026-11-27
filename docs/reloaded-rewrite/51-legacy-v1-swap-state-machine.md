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

> **Pointer (2026-11-22; passed the KDF Dirty Gate with its target).**
> The on-disk form of the persistence that R2 through R5 rely on, where a
> data directory is shared with a reference build, is bound by
> [Chapter 44](44-database-persistence-and-migrations.md) §44.9A: the lock
> file of R5 (its name, place, content form, and what the reference does with
> content it does not recognize) in R44.9A.6 through R44.9A.9, the saved-swap
> files behind the event log of R2 and R3 in R44.9A.10 through R44.9A.12, and
> the rules for RELOADED in R44.9A.13. This pointer changes no rule of this
> chapter.

> **Reloaded status note (2026-11-22, Reloaded-original; informative).** R5 is
> silent on a runner whose lock is no longer its own while the run proceeds.
> With PR #480 a RELOADED runner checks before each refresh that the lock
> still holds the value it wrote last (chapter 44 §44.9A, Reloaded status
> update). When it does not, the run is stopped where it stands, like an
> abrupt stop, with no event, and the swap is handed to the kick-start of a
> saved swap: that start takes the lock by this rule (wait one time-to-live,
> retry once, abandon without an event if still held) and, when it gets the
> lock, resumes the swap from its event log by R6 and R7. Detail: PR #480.

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
transaction identifier. For a shielded coin, when the refund becomes
permitted and what the refund transaction is are bound by
[chapter 39](39-zcash---z_coin-shielded-coin.md) R39.5.12, and for this
project R39.5.13 (Decision AP): the refund also waits for the chain's
median time past, and a refund rejected as non-final is retried instead
of ending in `MakerPaymentRefundFailed`. The taker chain of R31 follows
the same rules.

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

> **Status note (2026-11-23, Reloaded-original, clean side; maintainer decisions
> of 2026-11-22 and 2026-11-23).** Under §51.6G R100, when the validation of the
> taker payment in `STAGE-VALIDATE-TAKER-PAYMENT` answers that its read is
> undetermined, the maker repeats the validation every 15 seconds until the
> deadline of that stage (the swap start time plus four fifths of the lock
> duration). At the deadline, or on any other error, it emits
> `TakerPaymentValidateFailed` and goes into the refund chain as stated above.

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

> **Status note (2026-11-22, KDF Spec Reader, round 0.5.0-27; passed the KDF
> Dirty Gate with §51.6F).** How an EVM coin judges a declared swap-contract
> address, and the case in which the taker selects its own fallback contract
> instead of ending the swap, are recorded in §51.6F R98–R99. R24 is not
> edited.

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

> **Status note (2026-11-23, Reloaded-original, clean side; maintainer decisions
> of 2026-11-22 and 2026-11-23).** Under §51.6G R100, when the validation of the
> maker payment answers that its read is undetermined, the taker repeats the
> validation every 15 seconds until the maker-payment wait deadline of R22. At
> the deadline, or on any other error, it emits `MakerPaymentValidateFailed` as
> this rule states.

**R29.** *Send taker payment.* `STAGE-SEND-TAKER-PAYMENT` MUST first
search the chain for a taker payment it may already have broadcast. Only
if none is found MUST it check that the maker-payment wait deadline has
not passed: the swap's stored `maker_payment_wait`, the swap start time
plus two fifths of the lock duration, the same deadline the taker's wait
for the maker payment's confirmations uses. Failure of the search or a passed deadline MUST emit
`TakerPaymentTransactionFailed` and transition to `STAGE-FINISH`. On
success it MUST emit `TakerPaymentSent`. This search-before-deadline
ordering is required by R7: a replayed stage must not refuse to continue
a swap whose payment is already on chain merely because the send window
has since closed.

> **Reloaded status note (2026-11-22, Reloaded-original; informative).** R17
> and R29 let a failed search end the swap. For a swap that the kick-start
> resumes with the payment stage as its next stage, this project asks the
> search again within a bound and then waits without sending, as the status
> note of 2026-11-22 after T39.11.5A.1 in
> [chapter 39](39-zcash---z_coin-shielded-coin.md) records. Every other swap
> keeps R17 and R29 as written. The event sets and resume maps of R19 and R32
> are unchanged. Detail: PR #489.

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

> **Status note (2026-11-22, KDF Spec Reader, round 0.5.0-23).** How each coin
> family waits for and searches a spend, where it reads the secret and
> whether it compares it with the secret hash, and on which clock a payment
> counts as refundable, is recorded for the reference rows in §51.6E
> (R85–R92, T36–T38). R30, R31 and R18 are unchanged. Gate status as §51.6E
> states.

> **Status note (2026-11-22, Reloaded-original, clean side; maintainer decision
> of 2026-11-22).** The taker, and a watcher acting for it, uses a value as
> the swap's secret only when it hashes to the swap's secret hash, for every
> coin family: RIPEMD-160 of SHA-256 for a 20-byte hash, SHA-256 for a 32-byte
> hash (R71). A value that does not match is an extraction error; R30's exit
> applies in the wait for the spend, and the manual recovery, the refund
> chain's look and the watcher keep the handling they already give an
> extraction error. An ETH or ERC20 coin reads
> the secret from the call input when that is a direct call of the swap
> contract's claim method, and otherwise from the `secret` word of a
> `ReceiverSpent` log in the transaction's receipt. A Tendermint coin examines
> every message of the transaction for a claim. This is stricter than the
> reference record of §51.6E (R85, R86, R91), which §51.6E permits; no message
> and no transaction a peer validates changes.

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

**R31a.** *The taker's refund chain claims a payment the maker has spent
(Reloaded addition, 2026-10-29, Decision AV).* The taker enters its
refund chain from failures that do not prove the maker has not spent the
taker payment: its own wait for the payment's confirmations failed
(`TakerPaymentWaitConfirmFailed`), or its wait for the spend ended at the
locktime (`TakerPaymentWaitForSpendFailed`) while the maker may spend just
after it. The maker can spend the taker payment until the taker's refund
is on chain, and such a spend makes the refund invalid. A refund chain
that only builds the refund ends in `TakerPaymentRefundFailed` with the
maker holding both payments until the maker payment locktime (R10, later
than the taker payment locktime of R22), after which the maker can refund
its own payment too. The taker execute stage therefore MUST look on chain
for a spend or a refund of the taker payment, with the swap's own HTLC
public key, at three points:

1. once on entry, before the refundability wait;
2. during the wait for the payment to become refundable, which is cut
   into steps of at most 300 seconds and at least 5 seconds: after a step,
   when at least 60 seconds have passed since the previous look, so that
   a spend made during the wait is claimed before the taker payment
   locktime rather than after it, while a refundability check that keeps
   answering "a few seconds more" (a chain whose median time has not yet
   reached the locktime) does not search the chain on every step;
3. once after a refund attempt is rejected for a reason that ends the
   chain, before `TakerPaymentRefundFailed` is emitted. A rejection that
   R18 retries (R39.5.13) is retried as before.

A spend found at any point MUST be handled as R30 handles the spend it
observes: the secret is extracted from the spending transaction,
`TakerPaymentSpent` is emitted carrying that transaction and the secret,
and the swap continues at `STAGE-SPEND-MAKER-PAYMENT` (R31). The
rejected refund is not published. A refund of the taker payment found on
chain (a restart after the refund was broadcast) MUST emit
`TakerPaymentRefunded` with that transaction and end the chain, without
building another refund. One look is bounded to 120 seconds. When the
search fails, does not finish within that bound, finds nothing, or finds
a spend whose secret cannot be extracted, the chain continues exactly as
R31 states. A swap whose record already holds a spend of the taker
payment (resumed after `MakerPaymentSpendFailed`, whose resume stage is
the refund chain, R32) MUST NOT look again, so that a failed spend of the
maker payment cannot lead back to the same spend; its refund chain is
the one R31 states.

A swap completed this way keeps the error events that sent it into the
refund chain (`TakerPaymentWaitConfirmFailed` or
`TakerPaymentWaitForSpendFailed`, and `TakerPaymentWaitRefundStarted`),
followed by `MakerPaymentSpent`, and `recover_funds_of_swap` reports it
as not recoverable because the maker payment is spent.

The event set and the resume map of R32 are unchanged: the addition
emits only existing event types, and the only new event order it
produces (`TakerPaymentSpent` after `TakerPaymentWaitRefundStarted`)
resumes where R32 already sends `TakerPaymentSpent`. The persisted log
shape of chapter 44 is unchanged. This rule records no claim about the
reference implementation; it closes a path on which a taker otherwise
depends on a manual `recover_funds_of_swap` within the maker payment's
locktime.

> **Status note (2026-11-21, Reloaded-original, clean side; maintainer
> decision of 2026-11-21).** For a taker coin that reads its chain through
> Electrum servers (a UTXO coin with an Electrum client), a look that ends
> without a spend or a refund (the search failed, did not finish within its
> bound, or found nothing) MUST repeat the search once on every usable
> server of the coin's rank, with directed requests (ch.38 R38.6.4g), before
> the chain continues. The servers are asked at the same time and share one
> bound of 120 seconds, which begins when the first search has ended, so a
> look can take up to two such bounds. The first spend or refund a server
> finds is handled as this rule states for a found spend or refund; when
> none is found, the chain continues exactly as R31 states. This applies at
> all three points above. Other coins keep their single search. The reason
> is a field report: a maker spent the taker payment a few seconds after its
> fourth confirmation, and the taker's looks did not find that spend until
> hours after the maker payment's locktime.

> **Status note (2026-11-22, Reloaded-original, clean side; maintainer decision
> of 2026-11-22).** For a coin whose swap payment is a script output (the UTXO
> family: standard UTXO coins, BCH, Qtum, SLP tokens and the transparent side of
> the shielded coins), a found spend of the payment is classified as a spend or
> a refund by the script of the input that spends the payment's output. This
> holds for every search this chapter names (R31a with the search on every
> server, R82, R83 and R84) and for the swap's other searches of the same
> payment. A spend whose spending input carries neither form is an error that
> names the input.

> **Pointer (2026-11-22, clean side).** The reference record of the same
> classification, and its difference from the baseline anchor, is §51.6E R90
> items 2 and 5.

> **Status note (2026-11-23, Reloaded-original, clean side; maintainer decisions
> of 2026-11-22 and 2026-11-23).** Under §51.6G R100 a look lets the chain
> continue only when it is a determined "not spent". A search that fails, or
> does not finish within its bound, leaves the look undetermined. A spend that
> the search on every server (status note of 2026-11-21) finds on one server is
> handled on that one answer when its secret matches the swap's secret hash
> (R100 item 3); a refund, or a spend whose secret does not match, found there
> on one server is not acted on, and the look is undetermined. A spend found by
> the coin's own search whose secret cannot be extracted continues as R31
> states, unless the read behind it was undetermined. While the look is
> undetermined the chain waits: it builds no refund, it does not emit
> `TakerPaymentRefundFailed` after a rejection, and it looks again after the
> chain's retry wait (30 seconds in the node), writing one warning, which names
> the fix when the read is undetermined under R100. The look also runs at a
> fourth point: each time the refundability check answers that the refund can be
> sent, before the refund is built, which happens only after a determined "not
> spent". An undetermined answer of the refundability check is retried the same
> way. A look or a refundability check that is still undetermined at the
> deadline of R100 item 9 ends the chain as chapter 51 R100 item 9 and its
> status notes of 2026-11-23 state: the later of the taker payment's lock time
> plus 3700 seconds and 3700 seconds after the refund step, in its current run,
> first could act (the refundability check first answered that the refund can be
> sent, or the check or a look first gave an undetermined answer at or after the
> lock time). The event set, the resume map of R32 and the persisted log shape
> are unchanged.

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

## 51.6A Bound Refusal to Refund After the Maker's Own Spend of the Taker Payment

> **Status (Reloaded-original; decisions DK and DV, 2026-11-07).** These rules
> record no claim about the reference implementation. R75 item 1 states, as
> the intended behavior, a refusal that the code already applies; R75 item 2
> extends it to a spend that is sent but not yet confirmed.

Every terminated maker swap is marked finished (R4) and its log ends in
`Finished` (R19), a failed swap included. This section concerns the swaps
whose record holds the maker's own spend of the taker payment (the event
`TakerPaymentSpent`). Two cases are told apart:

- a *completed swap*: the record also holds the confirmation of that spend
  (the event `TakerPaymentSpendConfirmed`);
- a *swap with a sent spend*: the record holds the spend but not its
  confirmation, because the confirmation did not arrive in time or the node
  stopped before it did.

Every other terminated swap is a *swap without a spend*.

The spend of the taker payment is recorded only after it was broadcast, and the
broadcast published the secret. The claim branch of the maker payment's own
contract has no time lock (for a UTXO coin this is its script), so the taker can
claim the maker payment with that secret at any time until a refund is mined.
After the locktime the maker's refund is valid as well, and whichever of the two
transactions is mined first decides. A maker that refunds then holds both
payments, which is the outcome R31a closes for the taker. The maker's spend may
still be mined, or may have been dropped; in either case the secret is public,
and the maker payment is the taker's side of the trade.

**R75.** *The manual recovery never refunds the maker payment of a swap whose
record holds the spend of the taker payment.* `recover_funds_of_swap` for a
maker swap that has the spend in its record MUST NOT build a refund of the
maker payment, whatever the state of the maker payment on the chain and whether
or not its refund time has passed:

1. for a completed swap the call MUST refuse before any look at the chain and
   before any transaction is built, and its text MUST say that the taker payment
   spend was sent and confirmed;
2. for a swap with a sent spend the call looks at the chain as for a swap
   without a spend, and answers:
   - if the maker payment is *spent* (by the taker, or by a watcher): as for a
     swap without a spend, including the spend of the taker payment when it
     is still unspent (the maker then completes the trade it began);
   - if the maker payment is *refunded*: as for a swap without a spend;
   - if the maker payment is *unspent*: the call MUST refuse, whether or not its
     refund time has passed, before the refund time is looked at and before any
     transaction is built, and its text MUST say that the taker payment spend
     was sent and that the maker payment belongs to the taker.

The reason is honesty toward the taker: once the maker has spent the taker
payment, the maker payment belongs to the taker, whatever the technical chance to
refund it.

> **Status note (2026-11-12, Reloaded-original).** R83 narrows item 2: when the
> taker payment is shown on its own chain to be refunded by the taker, with a
> confirmation, the maker payment may be refunded by the manual recovery.

> **Status note (2026-11-13, Reloaded-original).** R84 narrows item 2 further: a
> record that holds no sent spend because the claim call failed is not covered by
> item 2, and the manual recovery looks at the taker payment first.

**R76.** *The refusal changes nothing.* A refusal of R75 MUST NOT append an
event to the log of the swap, change its status or its recoverable flag, or
write the swap record. The recoverable flag is derived from the events of the
swap and is not a statement about the chain; it is not consulted by the call.

**R77.** *Nothing refunds a completed swap automatically.* No part of the node
refunds the maker payment of a completed swap on its own, at start-up or later.
The guards that precede R75 stay as they are: the swap MUST be finished, and a
swap whose record already holds the refund of the maker payment is refused as
not recoverable. The refund that the run loop makes after the confirmation of
the spend failed is not changed by this section (D8).

> **Status note (2026-11-12, Reloaded-original).** The refund that the run loop
> makes after the claim step is now bound by R82 (2026-11-12).

**R78.** *Other swaps are unchanged.* A swap without a spend keeps every answer
of the manual recovery as it is, including the refund of an unspent maker
payment after its refund time and the spend of the taker payment when the maker
payment is spent and the taker payment is not.

> **As built.** R75 item 2 is one check at the start of the
> "maker payment unspent" answer of the maker's `recover_funds`, ahead of the
> refund-time check, the refund gate and any transaction building: when the
> record holds the spend of the taker payment, the call returns the refusal of
> item 2. The guard of item 1, the "spent" and "refunded" answers and the run
> loop are unchanged; no state is written on the refusal. Tests T32 items 7 to
> 11: `test_recover_funds_sent_spend_unspent_maker_payment_after_the_lock_is_not_refunded`,
> `test_recover_funds_sent_spend_unspent_maker_payment_before_the_lock_gets_the_same_refusal`,
> `test_recover_funds_sent_spend_spent_maker_payment_still_spends_the_unspent_taker_payment`,
> `test_recover_funds_sent_spend_spent_maker_payment_and_spent_taker_payment_builds_nothing`,
> `test_recover_funds_sent_spend_refunded_maker_payment_is_refused_as_already_refunded`
> in `mm2src/mm2_main/src/lp_swap/maker_swap.rs`. Run on 2026-11-07:
> `cargo test -p mm2_main --lib test_recover_funds` passed, 42 tests of which these five
> are new, and the formatting check passed.

## 51.6B Bound Warning for a Refund Wait on a Stalled Node

> **Status (Reloaded-original; batch 4, 2026-11-08).** These rules record no
> claim about the reference implementation. They add a log line only; the
> events, the resume map (R19, R32) and the persisted log shape of chapter 44
> are unchanged.

While a maker or taker swap waits to refund its own payment, the wait
consults the coin only through the refundability check, which answers "wait
this many seconds" (at most an hour at a time) and never says why the answer
does not change. When the coin's node stops advancing, the answer stays
"wait", the swap keeps waiting and the log says nothing that points to the
node. A wait that outlives a stalled node therefore looks like an ordinary
wait.

**R79.** *A refund wait observes the coin's tip height, beside the wait.* While a
swap of this chapter waits for the refund of its own payment, the swap MUST read
the tip height of the paying coin once every 5 minutes, and MUST NOT read it more
often (a wait that is cut into shorter steps reads once per 5-minute period, not
once per step; one second of clock rounding is tolerated). The read runs
alongside the sleep of the step and is abandoned when that sleep ends, so it is
never on the path of the wait; a read that fails or does not finish by then is not
an observation and is skipped. The wait itself (its length, the refund decision,
the events and the state) MUST NOT change because of the observation.

**R80.** *A tip that does not advance is warned about, at most hourly.* When
the observed height has not changed for 60 minutes, the swap MUST write one
warning to the log that names the coin by its ticker, the height and the number
of whole minutes the height has not changed. While the height stays the same
the warning MUST NOT be repeated more often than once every 60 minutes. A
change of height ends the stall, and a later stall counts from the change. The
warning MUST NOT contain a server address, URL, host or response text; the
ticker is the only name of the coin's node source. The clock of the stall
starts at the first observation of the wait; a restart of the node starts it
again.

**R81.** *What the rule does not cover.* The warning exists for the refund waits
of the maker and the taker of this chapter. It is not written for the wait for
the confirmation of a payment, for the swaps of the second protocol version
or for the watcher. It is not written while the refundability check itself fails (that
case keeps its existing error line and 30-second retry). It does not name the
cause (a stopped node, a node that is behind, a chain that has stopped).

> **As built (batch 4, 2026-11-08).** The rule lives in
> `mm2src/mm2_main/src/lp_swap/stale_tip_watch.rs`: a pure `StaleTipWatch`
> (`observe(now_secs, height)`, no clock inside) and `sleep_observing_tip`, which
> cuts the wait into slices of at most 300 s. At the start of a slice, when the
> height is due, `current_block()` of the paying coin is read alongside the
> slice's sleep (`sleep_beside_read`: the read raced against the sleep). The slice
> ends exactly when the sleep ends. A read that finishes first is observed and the
> sleep is awaited on; a read that fails, or has not finished when the sleep
> ends, is dropped and skipped, so no read is ever awaited after its sleep and
> the wait is not lengthened. A read is due only when 300 s (less 1 s of clock
> rounding) have passed since the previous one, so the short steps of the taker's
> refund wait read the coin once per 5 minutes, not at every step. The maker's
> refund wait (`refund_maker_payment_retrying`) uses it in place of its
> `HaveToWait` sleep; the taker's (`refund_taker_payment_paced`) in place of its
> step sleep, one watch per refund wait, created before the inner loop. The `Err`
> arm (30 s retry) is unchanged. Tests: the `t33_*` tests of that module (fake
> clock, sleeper and reads); two existing taker tests that answer "wait" now mock
> `current_block`; only the taker's call site runs in a test (through those two
> tests), the maker's call site is covered by construction. A read is counted as
> made when it starts, so a maker wait that ends in very short sleeps (a wait of
> 0 to 2 seconds close to the lock time) whose read takes longer than the sleep
> has that read dropped and its 5-minute period used up; such a wait may observe
> rarely, while the taker's steps (5 to 300 seconds by default) and the
> 300-second slices of the maker's observe as the rule says.
> Run on 2026-11-08: `cargo test -p mm2_main --lib -- stale_tip_watch` passed (15
> tests), `-- r31a` passed (10 tests), and the formatting check passed. The warning
> is verified by these unit tests only: the timing is driven by a fake sleeper
> and a fake height source, and no real-time observation of it on a stalled
> node exists. A lab row that observes it on a frozen tip is an optional
> follow-up, not a condition of this rule.

## 51.6C Bound Automatic Refund After the Claim Step

> **Status (Reloaded-original; 2026-11-12).** This rule records no claim about
> the reference implementation. It binds the refund that the maker's run loop
> makes, which R77 and D8 left unchanged. It adds no event; the persisted log
> shape of chapter 44 and the resume map (R19, R32) are unchanged.

The maker's run loop goes to the refund of the maker payment when the claim of
the taker payment fails with a built transaction, when the wait for the
confirmation of the claim fails, and after a restart that finds either failure
in the log. None of these proves that the claim is not on the chain: the claim
may be mined while the answer that reached the node was an error or a lagging
backend. A refund after a mined claim leaves the maker holding both payments.

**R82.** *The refund after the claim step looks at the taker payment first.* When
the run loop reaches the refund of the maker payment of a swap that reached the
claim step (the claim was sent, the claim failed with a built transaction, or the
wait for its confirmation failed; the taker payment is known), the swap MUST
search the taker coin's chain for the spend of the taker payment, with the
swap's own keys, before it builds the refund, and decides as follows:

1. A *confirmed* spend of the taker payment by the maker's claim is found: the
   swap MUST NOT refund. The record completes: `TakerPaymentSpent` (only when the
   record does not hold the spend yet) and `TakerPaymentSpendConfirmed` are
   recorded, and the swap finishes. One info line names the swap and the ticker
   of the taker coin. A spend that is found but has no confirmation (it may be
   only in a mempool and may drop, after which the taker could refund) records
   nothing and is handled as item 3: the refund is held and the search is
   repeated, and at the deadline the failed-refund text says that the claim is
   found but not confirmed.
2. The taker's refund of the taker payment is found, or nothing is found: the
   refund goes on as before (a fair race of the two refund times is what the
   protocol intends).
3. The search fails or cannot decide, or a spend is found but not confirmed: the
   swap MUST NOT refund. It writes one warning naming the swap and the ticker of
   the taker coin (no server address, URL, host or response text) the first
   time each of the two situations occurs, logs the repeats at debug level,
   waits and searches again, until the refund deadline of the swap. At the
   deadline the swap records `MakerPaymentRefundFailed` with a text that says
   the state of the taker payment could not be determined on that coin (or that
   the claim of the taker payment on that coin is found but not confirmed),
   that the maker payment is not refunded, and that manual recovery applies; the
   swap then finishes.

No new event is added. A swap that did not reach the claim step (the refund
follows a failure before it, or the taker payment is not known) refunds as
before and searches nothing. The restart path is the same code: the claim step
is derived from the events of the saved log. The reverse case of D8 (the claim
dropped and the taker refunded) is not part of this rule.

> **As built (2026-11-12).** `MakerSwap::refund_maker_payment_retrying` in
> `mm2src/mm2_main/src/lp_swap/maker_swap.rs`: after the refund gate answers
> "refund now" and before the refund is built, a swap whose
> `taker_payment_spend_attempted` flag is set (set by `apply_event` for
> `TakerPaymentSpent`, `TakerPaymentSpendFailed` and
> `TakerPaymentSpendConfirmFailed`, so a restored swap has it) and that holds the
> taker payment calls `look_at_taker_payment`, which runs
> `search_for_swap_tx_spend_other` of the taker coin and hands the result to the
> pure `classify_taker_payment_search`
> (`TakerPaymentOutcome::{SpentByUs, RefundedByTaker, Unspent, Undetermined}`).
> A found spend is then checked with `wait_for_confirmations` of the taker coin
> for one confirmation, for at most the retry wait; a failed check turns it into
> `TakerPaymentOutcome::SpentUnconfirmed`, which is held like `Undetermined`.
> The wait between searches is the retry wait the refund stage already takes
> (30 s in the node). The warning is written once for each of the two held
> situations (undetermined, found but not confirmed) and the repeats go to the
> debug log; the search or confirmation error text is logged at debug level only.
> The deadline check comes before the sleep, so the last search may run up to one
> retry wait after the refund deadline; if that search answers "unspent" the
> refund is built just after the deadline, which is harmless on the chain.
> Tests (`maker_swap_tests` in the same file, 12 new):
> `a_taker_payment_spend_found_on_the_chain_stops_the_refund_and_completes_the_record`,
> `a_taker_payment_spend_found_after_the_confirmation_failed_adds_only_the_confirmation`,
> `a_taker_refund_found_on_the_chain_lets_the_maker_refund`,
> `an_unspent_taker_payment_lets_the_maker_refund`,
> `an_unreadable_taker_chain_holds_the_refund_until_the_deadline_and_reports_it`,
> `an_unreadable_taker_chain_after_the_deadline_reports_at_once`,
> `a_taker_payment_spend_that_is_found_but_not_confirmed_holds_the_refund_until_the_deadline`,
> `an_unconfirmed_taker_payment_spend_after_the_deadline_reports_at_once_and_records_nothing`,
> `a_swap_that_never_reached_the_claim_step_refunds_without_a_search`,
> `a_swap_without_a_taker_payment_refunds_without_a_search`,
> `a_restored_swap_after_a_failed_claim_confirmation_does_not_refund_a_spent_taker_payment`
> (also checks the lock time and key the restored swap passes to the search),
> `the_taker_payment_search_result_is_classified_into_the_four_outcomes`.
> Run on 2026-11-12: `cargo test -p mm2_main --lib lp_swap::maker_swap` passed, 81 tests of which these twelve
> are new, and the formatting check passed.

> **Status note (2026-11-21, Reloaded-original, clean side).** A restart
> before the outcome of the claim is saved is covered by this rule as it
> stands: the saved log then ends at `TakerPaymentValidatedAndConfirmed`, which
> resumes at `STAGE-SPEND-TAKER-PAYMENT` (R19). That stage runs again, and every
> way out of it records `TakerPaymentSpent` or `TakerPaymentSpendFailed` before
> the refund stage is reached, including the exit taken when four fifths of the
> lock duration have passed, which sends nothing. The swap has therefore
> reached the claim step as this rule defines it, and its refund stage makes
> the look of this rule, whether or not the claim sent before the restart
> reached the chain. A second send that is accepted records the claim, and the
> wait for its confirmation decides as before. T35 pins this path. No code is
> changed.

> **Status note (2026-11-23, Reloaded-original, clean side; maintainer decisions
> of 2026-11-22 and 2026-11-23).** Under §51.6G R100, a look of item 3 that
> fails or cannot decide, and an undetermined answer of the refundability check,
> are held as item 3 states, with one warning that names the fix when the read
> is undetermined under R100. Its bound is the one of R100 item 9 and its status
> notes of 2026-11-23: the later of the maker payment's lock time plus 3700
> seconds and 3700 seconds after the refund step, in its current run, first
> could act (the refundability check first answered that the refund can be sent,
> or the check or a look first gave an undetermined answer at or after the lock
> time). At that bound `MakerPaymentRefundFailed` names the kind of failure and,
> for a read undetermined under R100, the cause and the fix, and asks for
> `recover_funds_of_swap`; as R80 requires, neither the warning nor this text
> carries a server address, URL, host or response text, and the full reason goes
> to the debug log. A look held for another reason (a spend found but not
> confirmed) keeps the text and the deadline of item 3. The manual recovery of
> R83 and R84, and the taker's, answer as before; an error whose read is
> undetermined under R100 also names the fix.

## 51.6D Bound Manual Refund After a Lost Claim

> **Status (Reloaded-original; 2026-11-12).** This rule records no claim about
> the reference implementation. It narrows item 2 of R75 for the manual
> recovery only. It adds no event and no RPC field; the persisted log shape of
> chapter 44 and the resume map (R19, R32) are unchanged.

A maker swap can end with the log holding the sent claim of the taker payment
(`TakerPaymentSpent`) although the claim never reached the chain: the
transaction was built and logged as broadcast, the confirmation wait failed at
its deadline, and the search of R82 could not read the backend. The taker then
refunds the taker payment on its own chain. The record is not confirmed, the
maker payment is unspent, and R75 item 2 refuses the manual recovery: the maker
holds neither payment, and the taker, who knows the secret from the broadcast,
can still claim the maker payment. D8 described this case.

**R83.** *The manual recovery refunds after a lost claim only when the taker's
refund is shown.* For a swap whose record holds the sent spend of the taker
payment without its confirmation, and whose maker payment is unspent on its
chain, `recover_funds_of_swap` MUST look at the taker payment on the taker
coin's chain before it answers, with the key that rebuilds the taker payment's
script (the key the recovery's own spend of the taker payment uses), and a spend
or refund that is found counts only with at least one confirmation. It
answers:

1. the taker's refund of the taker payment is found with a confirmation: the
   maker payment MAY be refunded. One info line names the swap and both tickers.
   The refund time, the refund gate and the refund build are exactly as for a swap
   without a spend (R78);
2. the maker's own spend of the taker payment is found with a confirmation: the
   call MUST refuse with the text of R75 item 2, unchanged;
3. the maker's spend is found without a confirmation, or the refund is found
   without a confirmation, or the search fails or cannot decide: the call MUST
   refuse with a text that says the state of the taker payment on that coin could
   not be determined (or that the spend is found but not confirmed) and that the
   maker payment is not refunded, and MUST build no transaction;
4. nothing spent the taker payment: the call MUST refuse with the text of R75
   item 2 and MUST add that the taker payment is still unspent on that coin and
   that the call is to be repeated once the taker has refunded (the claim may
   still be in a mempool).

R75 item 1 (a completed swap, whose spend is confirmed) is unchanged and refuses
before any look at a chain. A refusal of this rule changes nothing (R76): no
event, no status, no recoverable flag and no record write. The run loop is not
changed by this rule.

> **As built (2026-11-12).** `MakerSwap::recover_funds_tracked` in
> `mm2src/mm2_main/src/lp_swap/maker_swap.rs`: in the arm of the maker payment
> search that finds the maker payment unspent, a record that holds the sent spend
> calls `look_at_taker_payment_for_recovery`, which resolves the recovery key of
> the taker payment with `resolve_recovery_htlc_key`, records the `SearchSpend`
> stage of the taker coin, runs `search_for_swap_tx_spend_other` and hands the
> result to `classify_taker_payment_search`. A found spend or refund is checked
> with `confirm_found_spend` (`wait_for_confirmations` of the taker coin, one
> confirmation, for at most the retry wait of the refund stage, which the
> run loop's look at the taker payment uses too); a found refund that cannot be
> confirmed is reported as undetermined. The answer to a confirmed refund goes on
> into the existing refund code (refund time, refund gate, build) unchanged.
> `TakerPaymentOutcome::RefundedByTaker` now carries the refund transaction.
> Tests (`maker_swap_tests` in the same file, 8 new, 2 adapted to mock the taker
> search):
> `test_recover_funds_lost_claim_taker_refund_confirmed_refunds_the_maker_payment`,
> `test_recover_funds_lost_claim_taker_refund_unconfirmed_is_refused`,
> `test_recover_funds_lost_claim_own_confirmed_spend_keeps_the_sent_spend_refusal`,
> `test_recover_funds_lost_claim_own_unconfirmed_spend_is_refused_naming_the_reason`,
> `test_recover_funds_lost_claim_unspent_taker_payment_is_refused_with_a_repeat_note`,
> `test_recover_funds_lost_claim_failed_taker_search_is_refused_as_undetermined`,
> `test_recover_funds_lost_claim_completed_swap_refuses_without_looking_at_the_chain`,
> `test_recover_funds_lost_claim_taker_refund_confirmed_before_the_lock_is_too_early`.
> Run on 2026-11-12: `cargo test -p mm2_main --lib lp_swap::maker_swap` passed, 89 tests of which these eight
> are new, and the formatting check passed.

> **Status note (2026-11-21, Reloaded-original, clean side; maintainer
> decision of 2026-11-21).** The run loop and the manual recovery answer
> differently in one case, and the difference is deliberate: the record holds
> the sent claim of the taker payment, no spend of the taker payment is found
> and the taker payment is unspent. The run loop refunds the maker payment
> (R82 item 2: at the maker payment's refund time the taker has had a full lock
> duration to claim it, and the two refund times race as the protocol intends).
> The manual recovery refuses (item 4 above), because a claim that is still
> waiting in a pool may not be visible to every coin's search, and refusing is
> the cautious answer until the taker's refund is shown. The manual recovery
> reaches this case only after the run loop ended without a refund: the look
> could not decide by the refund deadline (R82 item 3), or the look answered
> that nothing spent the taker payment and the refund that followed was
> rejected. In every other case the two answer alike.

> **Status note (2026-11-13, Reloaded-original).** R84 narrows R83: it covers the
> record without a sent spend, after a failed claim call.

> **Status (Reloaded-original; 2026-11-13).** This rule records no claim about
> the reference implementation. It adds no event and no RPC field; the persisted
> log shape of chapter 44 and the resume map (R19, R32) are unchanged.

**R84.** *The manual recovery looks at the taker payment after a failed claim
call.* For a swap whose claim step was reached (the log holds
`TakerPaymentSpent`, `TakerPaymentSpendFailed` or `TakerPaymentSpendConfirmFailed`,
the same condition R82 uses), whose record holds no sent spend (the claim call
returned an error, so the claim may still have been mined), whose record holds
the taker payment, and whose maker payment is unspent on its chain,
`recover_funds_of_swap` MUST look at the taker payment exactly as R83 does
(same key, same one-confirmation rule) before it answers:

1. the maker's own spend of the taker payment is found with a confirmation: the
   call MUST refuse with a text that says the claim of the taker payment on the
   taker coin is on the chain and that the maker payment on the maker coin belongs
   to the taker and is not refunded;
2. the spend is found without a confirmation, or the search fails or cannot
   decide: the call MUST refuse with the texts of R83 item 3, and MUST build no
   transaction;
3. the taker's refund of the taker payment is found with a confirmation, or
   nothing spent the taker payment: the maker payment MAY be refunded. One info
   line names the swap and both tickers and says which of the two applies. The
   refund time, the refund gate and the refund build are exactly as for a swap
   without a spend (R78). Unlike R83 item 4, an unspent taker payment does not
   refuse here, because no spend was recorded as sent (the same answer as R82
   item 2 gives in the run loop).

A swap that did not reach the claim step makes no search and is unchanged. R83
(a record that holds the sent spend) is unchanged. A refusal of this rule changes
nothing (R76). The automatic refund of the run loop (R82) is not changed by this
rule.

> **As built (2026-11-13).** `MakerSwap::recover_funds_tracked` in
> `mm2src/mm2_main/src/lp_swap/maker_swap.rs`: in the arm of the maker payment
> search that finds the maker payment unspent, a second branch, taken when the
> record holds no sent spend, `taker_payment_spend_attempted` is set (the replay of
> the saved events sets it) and the taker payment is known, calls
> `look_at_taker_payment_for_recovery` unchanged and answers as above; a refund or
> an unspent payment goes on into the existing refund code. Tests
> (`maker_swap_tests` in the same file, 7 new):
> `test_recover_funds_failed_claim_call_confirmed_own_claim_is_refused`,
> `test_recover_funds_failed_claim_call_unconfirmed_own_claim_is_refused`,
> `test_recover_funds_failed_claim_call_failed_taker_search_is_refused_as_undetermined`,
> `test_recover_funds_failed_claim_call_taker_refund_confirmed_refunds_the_maker_payment`,
> `test_recover_funds_failed_claim_call_unspent_taker_payment_refunds_the_maker_payment`,
> `test_recover_funds_failed_claim_call_unspent_taker_payment_before_the_lock_is_too_early`,
> `test_recover_funds_claim_step_not_reached_makes_no_taker_payment_search`.
> The six fixtures that reach the claim step are restored from a saved log in which
> `TakerPaymentSpendFailed` takes the place of `TakerPaymentSpent`; the seventh,
> `test_recover_funds_claim_step_not_reached_makes_no_taker_payment_search`, from the same log with
> `TakerPaymentSpent` replaced by `TakerPaymentValidatedAndConfirmed`, so that neither the spend nor
> its failure is in the log. One existing test (`test_recover_funds_maker_payment_refund_errored`, a
> restored swap that ended in the failed refund) now mocks the taker search, because its log holds
> the failed claim call.
> Run on 2026-11-13: `cargo test -p mm2_main --lib lp_swap::maker_swap` passed, 96 tests of which these
> seven are new.

> **Status note (2026-11-26, Reloaded-original, clean side; maintainer
> decisions of 2026-11-23, 2026-11-24 and 2026-11-26).** `recover_funds_of_swap`
> also accepts a swap of version two (chapter 52). It recovers a version-two
> swap only when the swap ended at the bound of a refund step, judged by the
> structure of its saved events: the last event is `Aborted`, the event before
> it is the refund-required event of that step, and the abort carries the
> discriminant that step ends with at its bound. The recoverable endings are:
>
> 1. the maker: `MakerPaymentRefundRequired`, then `Aborted` with
>    `InternalError`;
> 2. the taker's refund of its payment: `TakerPaymentRefundRequired`, then
>    `Aborted` with `TakerPaymentRefundFailed`;
> 3. the taker's refund of its funding: `TakerFundingRefundRequired`, then
>    `Aborted` with `TakerFundingRefundFailed`.
>
> No text marker is read. An abort of the same step with the same
> discriminant for another reason (a refund broadcast that failed) is
> recovered as well; that is safe because the recovery repeats every read
> and refunds only on determined answers. The recovery makes one attempt of
> that refund step, with fresh reads under §51.6G R100 (two agreeing nodes,
> or the node the operator trusts) and without any wait: the refund gate, the
> look at the taker payment (the maker after the claim step, the taker always)
> or the search for the spend of the funding, and then the refund. A read
> that is undetermined under R100, or that fails, builds no refund. Inputs
> the refund-required event does not hold are read from the swap's saved
> events, as chapter 52 states. It answers in the existing shapes:
>
> 1. the refund was broadcast: `RefundedMyPayment` with the refund;
> 2. the taker's look found the maker's spend of the taker payment, whose
>    secret matches the hash: the maker payment is claimed, and the answer
>    is `SpentOtherPayment` with the claim;
> 3. the taker's funding was spent into the taker payment: the attempt goes
>    on as the refund of that payment (items 1 and 2);
> 4. an error, building no transaction, when the maker's own claim of the
>    taker payment is found, when the funding is already refunded, when the
>    refund gate asks to wait (with the seconds), when a read gives no answer
>    the refund may rest on (naming the read, the coin and, for a read
>    undetermined under R100, the fix), when the saved events lack what a
>    read needs, or when the look finds the taker payment already spent
>    and no maker secret in the spend (the answer says so);
> 5. for every other ending of a version-two swap, the error that the swap is
>    not recoverable.
>
> The recovery of one version-two swap runs once at a time: a second call
> for the same swap while one is running gets an error answer at once. The
> maker's recovery does not look for its own earlier refund; a repeated call
> after a refund is refused by the chain. A recovery records no event with
> the swap. The answers for a legacy swap are
> unchanged.

## 51.6E Reference Behavior of the Coin Families: Spend Search, Secret Extraction, Refund Eligibility

> **Status (2026-11-22; KDF Spec Reader, round 0.5.0-23).** Reference record,
> read at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A. The
> two rows behave alike except where a rule names a difference. **Gate status:
> passed the KDF Dirty Gate on 2026-11-22, measure 0.03 (chapter 01 R40;
> attestation in §51.16).** R85–R92 state what the reference does. They do not
> require Reloaded to copy a missing check: none of the behavior below is
> carried on the wire, so a stricter check on Reloaded's side changes no
> message and no transaction a peer validates. The findings are informative
> (chapter 01). The reading was of source only; nothing here was observed on a
> running reference node.

The legacy machines of this chapter rely on four operations that every coin
family supplies in its own way:

- the *wait for a spend* of a payment, which the taker uses in R30;
- the *search for a spend or a refund* of a payment, which the manual
  recovery `recover_funds_of_swap` uses on both sides (in Reloaded also
  R31a and R82–R84);
- the *extraction of the secret* from the transaction that spent the taker
  payment (R30 and the taker's manual recovery);
- the answer to *"can this payment be refunded now"*, which the refund
  stages (R18, R31) and the manual recovery ask before they build a refund.

**R85.** *The machine does not check the secret itself.* The swap machine
hands the swap's secret hash and the spending transaction to the taker
coin's extraction and uses the 32 bytes that come back as the secret: it
records them in `TakerPaymentSpent` and spends the maker payment with them.
The machine compares nothing. Whether the value was compared with the secret
hash therefore depends on the taker coin's family alone: it is compared for
the UTXO families (R90) and for QRC20 (R89), and it is not compared for
ETH/ERC20 (R86) and for Tendermint (R91). When the extraction answers an
error, the run loop takes the exit R30 states (`TakerPaymentWaitForSpendFailed`,
then `STAGE-FINISH`) and the manual recovery fails with that error. When the
extraction answers a value that is not the secret, the spend of the maker
payment is built with it; when that spend fails, the run loop records
`MakerPaymentSpendFailed`.

> **Observation (informative, 2026-11-22).** At both rows the run that
> records `MakerPaymentSpendFailed` ends at `STAGE-FINISH`. The refund chain
> that R31 names for this event is reached through the resume map (R32) when
> the swap is started again, not within the same run.

**R86.** *ETH and ERC20: the secret comes from the call input only.* The
extraction decodes the call input of the spending transaction as a call of
the swap contract's claim method and returns its third argument, the
`secret` of `receiverSpend(bytes32 id, uint256 amount, bytes32 secret,
address tokenAddress, address sender)`. Further:

1. The receipt and the logs of the transaction are not read. The
   `ReceiverSpent(bytes32 id, bytes32 secret)` event carries the secret too,
   and the reference found the transaction through that event (R87), but the
   secret in the event is never used.
2. The value is not compared with the swap's secret hash. The hash is handed
   to the extraction and is not used by it.
3. The recipient of the transaction is not compared with the swap contract,
   and the `id` argument is not compared with the swap's id.
4. A call input that is not a call of the claim method is an error. Only
   the transaction's own call input is read; a claim that reaches the swap
   contract through a call of another contract is not seen there, and R85's
   error exit applies although the event with the secret is on the chain.
5. *Row difference.* At `v2.6.0-beta` the expected method is chosen by the
   swap: `receiverSpendReward` when the swap uses the watcher reward,
   otherwise `receiverSpend`, and only that one is accepted. At `v3.0.0-beta`
   either method is accepted, recognized by its selector, whatever the swap
   uses; the secret is the third argument in both. At `v3.0.0-beta` a call
   input shorter than a selector is an error.

**R87.** *ETH and ERC20: a spend or a refund is found through the swap
contract's event logs.* Neither the wait nor the search scans transactions
or their call input. Both ask the node for logs (`eth_getLogs`) that the
swap contract's address emitted with the event's signature as the first
topic, over a block range, and accept a log whose data begins with the
32-byte swap id. The id is the first argument of the payment transaction's
own call input. The contract address is the one the swap negotiated; in the
wait, when none is given, it is the recipient of the payment transaction.

1. *Wait for a spend.* Only `ReceiverSpent` is asked for. The range runs
   from the swap's start block of that coin to the node's tip, in windows of
   the coin's `logs_block_range`, and the whole pass is repeated at the
   caller's interval until the deadline, which is measured on the local
   clock. A refund is not looked for. When a log matches, the transaction it
   names is fetched by its hash, repeatedly until the node returns it or the
   deadline passes, and that transaction is the result.
2. *Search.* The same range is walked in ascending windows. In each window
   `ReceiverSpent` is asked for first and `SenderRefunded` second; the first
   matching log decides between *spent* and *refunded*, and the transaction
   it names is the result. A matching log whose transaction the node does not
   return is an error. No match up to the tip is the answer *nothing found*.
3. *Not checked, in either.* The receipt of the transaction is not read, so
   its execution status is not checked. No number of confirmations is
   required. A marker that the node sets on a log of a block that left the
   chain is not read.
4. *Row difference.* At `v2.6.0-beta` the payment's call input is decoded as
   the method the swap's watcher-reward setting selects (`ethPayment` or
   `erc20Payment`, or their `...Reward` forms). At `v3.0.0-beta` the method is
   recognized by its selector.

> **Note (informative).** A log is produced only by a call that completed, so
> a transaction found through the event did execute the claim or the refund.
> That is a property of the chain, not a check the reference makes; item 3
> stands for everything the log does not prove, for example how deep the
> block is.

**R88.** *Refund eligibility: the local clock, except for the UTXO coins
that read the chain's median time past.* The answer to "can this payment be
refunded now" has one general rule and a few family rules.

1. *General rule.* The payment is refundable when the local wall clock is
   strictly later than the lock time. Otherwise the answer is to wait for
   the difference plus one second. No chain time is read (no tip timestamp,
   no median time past, no height) and there is no margin beyond that one
   second.
2. *Families under the general rule:* ETH and ERC20, QRC20, SLP and
   Tendermint. None of them has a rule of its own, at either row.
3. *Families with their own rule:* a standard UTXO coin, BCH and the QTUM
   base coin. While the local clock is before the lock time the answer is to
   wait, as in item 1 but at most 3600 seconds at a time. After it, the
   chain's median time past is read, and the payment is refundable when the
   lock time is strictly less than the median time past; otherwise the
   answer is to wait for the difference plus one second, at most 3600
   seconds. KMD has a further condition that this section does not restate.
   Sia has its own rule (chapters 20 and 54); the shielded coin follows the
   general rule (chapter 39 R39.5.12 item (a)).
4. *What the general rule means per family.* For ETH, ERC20 and QRC20 the
   swap contract applies its own lock-time condition when the refund call
   executes; the reference does not read the chain's time before it sends
   the call. For QRC20 the rule is the general one although the QTUM base
   coin reads the median time past. For SLP the rule is the general one
   although the refund is a transaction of the platform coin, whose own rule
   reads the median time past. For Tendermint the general rule is all the
   reference has, and the refund call it then makes answers an error
   (chapter 18 R18.11.15); the chain refunds by height on its own.
5. *Where the answer is used.* The refund stages of R18 and R31 sleep for
   the time the answer names and ask again; the manual recovery refuses with
   an error that names the time.

> **Code-quality finding (informative).** An SLP refund is asked for by the
> wall clock alone, while the platform chain's lock-time rule (BIP 113)
> counts a time-locked transaction as final only once the median time past
> has passed its lock time, and the median time past trails the wall clock.
> A refund sent in between is not final under that rule, which is the case
> the platform coin's own answer waits out. *Proposed fix:* answer an SLP token's refund
> eligibility with the platform coin's rule (item 3). The same reasoning
> suggests reading the chain's time for a contract-based family before the
> refund call is sent, for example the tip block's timestamp against the lock
> time, instead of relying on the local clock.

**R89.** *QRC20: candidates by token history and event presence, the match
by call data.* The search and the wait work on the token's `Transfer`
history (chapter 38 R38.6.4Q.8 for the Electrum methods).

1. *Search, the payment first.* The payment transaction must have at least
   one confirmation; without it the search answers an error. Its receipts
   are read, the `erc20Payment` call among its outputs is located, and the
   receipt of *that* output is checked: an `excepted` value other than none
   is an error. The swap id in the call must equal the id derived from the
   swap's lock time and secret hash, otherwise the search answers an error.
2. *Search, the spend.* The candidates are the receipts in the token's
   `Transfer` history of the payment's receiver address, from the swap's
   start block. A receipt qualifies when it holds a `Transfer` log and a log
   whose only topic is the `ReceiverSpent` signature. The transaction of a
   qualifying receipt is fetched, and all of its outputs are examined for a
   `receiverSpend` call whose `id` is the swap id and whose `secret` hashes
   to the swap's secret hash (item 6). The first such transaction is the
   answer *spent*.
3. *Search, the refund.* Only when no spend is found: the same procedure
   with the `SenderRefunded` signature, over the `Transfer` history of the
   node's own address, and a `senderRefund` call with the swap id in any
   output. The first such transaction is the answer *refunded*.
4. *Not checked for the spend and the refund.* The `excepted` value of the
   receipt is not read; the presence of the event log stands in for the
   outcome. The address that emitted the event log is not compared with the
   swap contract, and the log's data (the id) is not read. The event is not
   tied to the call that was matched: the event is looked for in a receipt,
   the call in the whole transaction, and the receipt's output index is not
   compared with the index of the output that holds the matched call. The
   contract address of the matched call's output is not compared with the
   swap contract. No confirmation is required of the spend or the refund.
5. *Wait for a spend.* Item 2 is repeated at the caller's interval until the
   deadline, with the id and the secret hash taken from the payment's own
   call; the payment's details are read first as in item 1, retried until
   they can be read. A refund is not looked for.
6. *Extraction.* Every output of the spending transaction is examined for a
   `receiverSpend` call. Its `secret` is accepted when
   RIPEMD-160(SHA-256(secret)) equals the swap's 20-byte secret hash; a
   32-byte secret hash is first reduced with RIPEMD-160. An output that does
   not match is skipped, and a transaction without a matching output is an
   error. The `id` is not compared here.
7. The two rows are identical for QRC20.

> **Code-quality finding (informative).** The search has one form for both
> directions (a payment of the node and a payment of the counterparty), and
> its refund step always reads the `Transfer` history of the node's own
> address. A refund of the counterparty's payment moves the tokens to the
> counterparty and does not appear there, so for a counterparty's payment the
> search can answer *spent* or *nothing found*, never *refunded*. *Proposed
> fix:* read the refund candidates from the history of the payment's sender.
> A second point follows from item 4: tie the event to the matched call by
> the receipt's output index, compare the emitting address with the swap
> contract, and read the receipt's `excepted` value.

**R90.** *UTXO families, including SLP: the input that spends the swap
output decides.* The search rebuilds the swap script from the lock time, the
secret hash and the two keys, and requires the payment transaction's swap
output to pay to that script's hash; otherwise it answers an error. The swap
output is output 0 for a standard UTXO coin and output 1 for an SLP token
(output 0 of an SLP transaction is its metadata output).

1. *Finding the spending transaction.* Over Electrum the history of the
   output's script is read (a history with fewer than two entries means
   *nothing found*), and each transaction in it is examined for an input
   that refers to the swap output. With a native node the wallet's
   transactions since the swap's start block are examined in the same way,
   leaving out conflicting ones. No confirmation is required. For an SLP
   token the lookup is the platform coin's; nothing token-specific is
   checked on the spending transaction.
2. *Classification.* The spending transaction is classified by the signature
   script of the input that refers to the swap output, wherever that input
   stands in the transaction; the first input has no special role. When the
   third element of that script is the empty push, the answer is *spent*;
   otherwise, when its second element is the push of the number one, the
   answer is *refunded*; otherwise the search answers an error. These are
   the two forms a spend of the swap script takes: signature, secret, empty
   push, script for a claim; signature, one, script for a refund. The secret
   is not examined at this step.
3. *Wait for a spend.* The lookup of item 1 is repeated at the caller's
   interval until the deadline, and the spending transaction is returned
   without classification. A refund found this way fails at the extraction.
4. *Extraction.* Every input of the spending transaction is examined, and in
   each signature script every 32-byte data push. The first push whose
   RIPEMD-160(SHA-256(push)) equals the swap's 20-byte secret hash is the
   secret; a 32-byte secret hash is first reduced with RIPEMD-160. The
   extraction is not limited to the input that spends the swap output or to
   a position in the script. Witness data is not examined. A transaction
   without such a push is an error.
5. The two rows are identical in all of this. The baseline anchor differs
   in two points: it classifies by the first input of the spending
   transaction, and it reads the secret from the second element of each
   input's signature script (with the same hash comparison).

**R91.** *Tendermint: the first message only, no comparison.* The extraction
decodes the first message of the spending transaction as the dialect's
claim message and returns its `secret` field, hex-decoded to exactly 32
bytes (chapter 18 R18.11.14). Other messages are not examined, so a
transaction that carries the claim in a later message is an error. The value
is not compared with the swap's secret hash. The search (chapter 18
R18.11.16) and the wait (chapter 18 R18.11.19) take the first result of the
transaction search `claim_htlc.id='<id>'`; neither examines the result code
of the transaction found, and neither establishes which of its messages is
the claim. The payment's own HTLC is read from the first message of the
payment transaction. The two rows are identical.

> **Code-quality finding (informative).** For ETH/ERC20 and Tendermint the
> value that becomes the swap's secret is taken from one fixed place of the
> spending transaction and is not compared with the secret hash (R86, R91),
> while the UTXO families and QRC20 compare it. *Proposed fix:* compare the
> extracted value with the swap's secret hash in the machine, for every
> family, using the hash algorithm the swap selected (R71), and treat a
> mismatch as "not found here" rather than as the secret. For ETH/ERC20 the
> `secret` word of the matching `ReceiverSpent` log is a second source when
> the call input does not decode; for Tendermint every message of the
> transaction can be examined for a claim with the swap's HTLC id.

**R92.** *Not established by this reading.*

1. Nothing was run. How a node answers the log request across a
   reorganization, and how real QTUM and Tendermint servers index failed
   calls, was not examined.
2. The KMD condition named in R88 item 3 and the Sia and shielded rules are
   not restated here.
3. The watcher's own search and extraction, the swaps of the second protocol
   version, and the Tron-VM form of the EVM family (chapter 21) are outside
   this section.
4. The QRC20 history of R89 was read for both clients; with a native node
   the candidates come from the node's log search over the token contract
   in steps of 100 blocks, filtered to receipts with a `Transfer` log. The
   server-side behavior of either source was not examined.
5. Whether this repository's code already differs from R85–R91 was not
   surveyed in this reading, apart from the baseline comparison in R90
   item 5.

> **Status note (2026-11-22, Reloaded-original, clean side; maintainer decision
> of 2026-11-22).** Reloaded answers "can this payment be refunded now" from the
> chain's time for three families that R88 item 2 records under the general
> rule. An SLP token answers with its platform coin's rule (R88 item 3). A QRC20
> token answers with the QTUM base coin's rule: the payment is refundable once
> the QTUM chain's median time past is past the lock time. An ETH or ERC20 coin
> waits on the local clock until the lock time, at most 3600 seconds at a time,
> then reads the latest block's timestamp: the payment is refundable when that
> timestamp is past the lock time; otherwise the answer is to wait for the
> difference plus one second, at most 60 seconds, so that a node that lags
> behind is asked again soon. A failed read is an error, which the refund stages
> of R18 and R31 retry. The Tron-VM form of the EVM family, which R92 item 3
> leaves outside §51.6E, keeps its current rule; Tendermint, Sia and the
> shielded coin keep the rules R88 records. Reloaded's manual recovery does not
> ask this answer: it has its own gate (the local clock past the lock time plus
> 3700 seconds, and for the shielded coin the chain's median time past), which
> is unchanged, as is the clock check before a refund. No message and no
> transaction a peer validates changes, which §51.6E permits.

> **Status note (2026-11-22, Reloaded-original, clean side; maintainer decision
> of 2026-11-22).** Reloaded's QRC20 search and wait take a candidate
> transaction as the spend (the refund) of the payment only when one of its
> outputs is the `receiverSpend` (`senderRefund`) call of the swap to the swap
> contract the payment used, and the receipt of that same output reports no
> exception and holds the `ReceiverSpent` (`SenderRefunded`) event, with the
> event signature as its only topic, emitted by that swap contract, its data
> beginning with the swap id. A candidate that fails is skipped and the search
> goes on. The refund candidates come from the history of the payment's
> sender, whichever side the node is. The payment checks of R89 item 1, the
> secret-hash check of the spend and the extraction of R89 item 6 are
> unchanged. This is stricter than the reference record of R89, which §51.6E
> permits; no message and no transaction a peer validates changes.

## 51.6F Reference Behavior of an EVM Token Leg: Fee Descriptors, Volume Limits, Swap-Contract Negotiation

> **Status (2026-11-22; KDF Spec Reader, round 0.5.0-27).** Reference record,
> read at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A. The
> two rows behave alike in every rule below. **Gate status: passed the KDF
> Dirty Gate on 2026-11-22, measure 0.03, no findings (chapter 01 R40;
> attestation in §51.16).** R93–R99 state what the reference does in a legacy
> swap when one coin is an ERC20 token of the EVM family. They complete R24 and
> R53–R55 and edit no sentence of this chapter. The reading was of source only;
> nothing here was observed on a running reference node. The gas figures
> themselves are bound in [chapter 57](57-evm-fee-inputs.md) and are not
> restated.

In this section the *token* is an ERC20 token and the *platform coin* is the
coin that pays its gas. "Available" means a coin's spendable balance minus
the amount running swaps reserve in that coin (R53).

**R93.** *Fee descriptor of a token.* Every fee figure of a swap is a
descriptor with the members `coin`, `amount` and `paid_from_trading_vol`.
For a token:

| Figure | `coin` | `paid_from_trading_vol` |
|---|---|---|
| fee to send the swap payment | platform coin's ticker | `false` |
| fee to spend the counterparty's payment | platform coin's ticker | `false` |
| fee to send the taker fee (DEX fee) | platform coin's ticker | `false` |
| the legacy `get_trade_fee` figure | platform coin's ticker | `false` |
| the taker fee (DEX fee) itself | the token's ticker | `false` |

`amount` of the first four is a quantity of the platform coin. The EVM
family never sets `paid_from_trading_vol` to `true`, for a token or for a
platform coin: no gas is taken out of the traded volume.

**R94.** *`trade_preimage` with a token.* The answer keeps its ordinary
members: `base_coin_fee`, `rel_coin_fee` and `total_fees`, for `buy` and
`sell` also `taker_fee` and `fee_to_send_taker_fee`, and for `setprice` with
`max` the volume members.

- The fee member of the side that pays (`base` of `setprice` and `sell`,
  `rel` of `buy`) is the fee to send the payment. The fee member of the
  other side is the fee to spend the counterparty's payment. When that side
  is a token, the member names the platform coin (R93).
- `taker_fee` names the paying coin, so a paying token is named there, and
  `fee_to_send_taker_fee` names its platform coin.
- `total_fees` has one entry per ticker named by the members above. An
  entry's `amount` is the sum of the figures naming that ticker, and its
  `required_balance` is the sum of those not paid from the trading volume,
  which for the EVM family is the same sum. An entry whose amount is zero is
  left out. A taker paying with a token therefore gets one entry for the
  token (the taker fee) and one for the platform coin (all gas figures in
  that coin, from both sides when both coins share the platform).

**R95.** *`trade_preimage` when the platform coin is short.* The answer is an
error and carries no fee figure.

1. *The typed refusal.* `NotSufficientBaseCoinBalance`, HTTP 400. Its data
   are `coin` (the platform coin's ticker), `available` (the platform coin's
   available balance), `required` and `locked_by_swaps` (the amount reserved
   in the platform coin). `required` is the platform-coin gas of one side
   only:
   - a maker paying with the token: the fee to send the payment;
   - a taker paying with the token: the fee to send the payment plus the fee
     to send the taker fee;
   - a token on the receiving side: the fee to spend the counterparty's
     payment.
2. *Order.* The paying side is checked first, and within it the platform
   coin before the token. The token's own check follows and compares the
   token's available balance with the volume, plus the taker fee for a
   taker; it answers `NotSufficientBalance` naming the token. No gas enters
   the token's `required`. The receiving side is checked last.
3. *No sum across sides.* Each side's figure is compared with the available
   balance on its own. Two sides that both need the platform coin are not
   added together.
4. *Token against its own platform coin.* When the other coin of the pair is
   the platform coin itself, that side's shortage is reported as
   `NotSufficientBalance` naming the platform coin, because the fee coin and
   the checked coin are the same.
5. *Earlier failure at an estimate.* Two figures ask the node for a gas
   estimate before any balance is compared: the fee to send the taker fee
   (chapter 57 §57.4), and the approve part of the payment fee when the
   token allowance is below the volume (chapter 57 R57.5.3b). A node that
   refuses the estimate makes the method answer `Transport`, HTTP 500,
   instead of item 1.
6. *`setprice` with `max`.* The volume is that of R97 item 1, with its
   platform-coin check, before the figures above are computed.

**R96.** *Swap start and the fee members of the swap status.* At
`STAGE-START` each side computes its descriptors anew and runs the checks of
R95 items 1–4, leaving out its own reservation (R53).

- *Failure.* A shortage of the platform coin, or a refused estimate, ends
  the swap with `StartFailed`. Nothing is sent to the peer. The event's
  error text is not bound.
- *Success.* The data of the `Started` event carry the descriptors, each
  with `coin`, `amount` (a decimal) and `paid_from_trading_vol`:
  - maker: `maker_payment_trade_fee`, `taker_payment_spend_trade_fee`;
  - taker: `fee_to_send_taker_fee`, `taker_payment_trade_fee`,
    `maker_payment_spend_trade_fee`.

  `my_swap_status` and `my_recent_swaps` show them as part of the event. For
  a token leg they name the platform coin with `false`, as in R93.
- *Quotes, not receipts.* The figures are the estimates made at the start.
  They are not replaced by the gas the transactions paid, and the payment
  events carry a transaction and its hash, no fee.
- *Reservation.* A descriptor naming the platform coin counts toward the
  platform coin's reserved total (R53–R55), not toward the token's.
- *Fee-exempt taker.* When the trade carries no DEX fee (chapter 08), the
  fee to send the taker fee is a zero descriptor that names the taker coin
  itself, for a token as well.

*Code-quality finding (informative; apparent from the reading, no audit was
made).* The taker's check requires the fee to send the taker fee and the fee
to send the payment to name the same coin. For a fee-exempt taker paying
with a token they do not: the first names the token (R96), the second the
platform coin (R93). As read, such a swap ends with `StartFailed` on an
internal error. *Possible remedy:* let the zero descriptor name the fee coin
of the payment, or skip the comparison when its amount is zero.

**R97.** *`max_maker_vol` and `max_taker_vol` for a token.*

1. *`max_maker_vol`.* The volume is the token's whole available balance.
   Nothing is taken off it for gas. The fee to send the payment is computed
   at the maker's upper-bound stage (chapter 57 R57.3.4) and compared with
   the platform coin's available balance. When that is short the method
   answers `NotSufficientBaseCoinBalance`, HTTP 400, with the four data
   members of R95 item 1; it does not answer a zero volume. The fee to
   spend the counterparty's payment is not reserved; no counterparty coin
   is known to the method.
2. *`max_taker_vol`.* The result is the largest volume that, with its DEX
   fee, fits into the token's whole available balance. **No platform-coin
   reserve is applied, the platform coin's balance is not read, and there is
   no error for a short platform coin**: the answer is the same with an
   empty platform-coin balance. The fee to send the taker fee is not
   computed for a token. The fee to send the payment is still computed, only
   to learn its coin, so the allowance read and a possible approve estimate
   happen and their failure fails the method with the legacy error body,
   HTTP 500.
3. *Token balance too small.* The floor is the coin's minimum transaction
   amount, which is zero for the EVM family (chapter 57 R57.7A.3). Below it,
   `max_maker_vol` answers `NotSufficientBalance` naming the token. The
   legacy `max_taker_vol` answers success with a zero `result` when the
   volume is not above the floor; it turns every insufficient-balance
   outcome into zero.
4. *Contrast.* For a platform coin both methods take the gas figures off
   the volume, because the fee coin is the coin itself.

*Code-quality finding (informative; apparent from the reading).* The two
methods disagree for a token. `max_maker_vol` refuses when the platform coin
cannot pay the gas; `max_taker_vol` returns a volume that `buy`, `sell` and
`trade_preimage` then refuse with `NotSufficientBaseCoinBalance`. *Possible
remedy:* run the platform-coin check of item 1 in `max_taker_vol` too, with
the payment fee plus the fee to send the taker fee as the required amount.

**R98.** *Which swap-contract addresses travel.* (Completes R26, R59.)

- *Maker's `Negotiation`.* `maker_coin_swap_contract` and
  `taker_coin_swap_contract` are, for each coin, the primary swap contract
  the maker's own coin holds from its activation: 20 bytes for an EVM coin
  or token, an empty byte string for a coin that has none. The maker never
  declares its fallback contract.
- *Taker's `NegotiationReply`.* The two members are the addresses the taker
  selected by R99. They are not always the taker's own primary contract.
- *Nowhere else.* No other legacy swap message carries a contract address;
  the payment messages carry transactions. The order-match messages carry
  none for an EVM coin ([chapter 59](59-legacy-ordermatch-negotiation.md)
  R59.4.7), and the watcher message carries none.
- *Status.* The `Started` data show the node's own primary addresses as
  `maker_coin_swap_contract_address` and `taker_coin_swap_contract_address`.
  The `Negotiated` data show the selected ones as
  `maker_coin_swap_contract_addr` and `taker_coin_swap_contract_addr`. From
  `Negotiated` on, the selected address is the one every payment, validation,
  spend and refund of that coin uses.

**R99.** *What an EVM coin does with the peer's address.* (Completes R11
check 4, R24 and R34 conditions 4 and 5.) The coin holds one primary swap
contract and optionally one fallback contract (chapter 35). For the value
the peer declared for that coin:

| Declared value | Outcome |
|---|---|
| 20 bytes equal to the primary contract | accepted; the primary is selected |
| 20 bytes equal to the fallback contract | accepted; the fallback is selected |
| any other 20 bytes | not accepted |
| a length other than 20 bytes, the empty string included | not accepted |
| no member at all (shape 1 of R59) | the fallback is selected when one is configured; otherwise not accepted |

- *Maker.* "Not accepted" is the refusal of R34 condition 4 or 5: one
  negative acknowledgement, `NegotiateFailed`, end. The maker never
  substitutes an address.
- *Taker with a fallback configured.* "Not accepted" does not end the swap.
  The taker selects its own fallback contract, whatever the maker declared,
  and sends it in its reply. The maker then applies the table: it goes on
  only when the taker's fallback equals the maker's primary or fallback
  contract. Otherwise the maker refuses and the taker ends with
  `NegotiateFailed` at R26 check 7.
- *Taker without a fallback.* "Not accepted" ends the swap at once with
  `NegotiateFailed`. The taker sends no reply and no refusal, and the maker
  ends by R34 condition 1 when its wait runs out.
- *When.* A difference is settled inside `STAGE-NEGOTIATE` on both sides,
  before the taker fee. Nothing compares contract addresses at order match,
  and no later stage refuses for this reason.
- *Other families.* A UTXO coin accepts every declared value and selects no
  address.

*Not established (R93–R99).*

1. No outcome was run against a reference binary.
2. Whether a given node refuses a gas estimate for an account that cannot
   pay the gas (R95 item 5, R97 item 2).
3. The error texts of `StartFailed` and `NegotiateFailed`, which are
   deliberately not recorded.
4. The contract-address rules of the QRC20, Tendermint, Sia and shielded
   families were not read for this section.
5. The second protocol version and the watcher's own checks are outside
   this section.
6. Whether this repository's code already differs from R93–R99 was not
   surveyed.

**T39.** *Reference characterization: fee descriptors (R93, R94, R96).* With
a token and its platform coin active and funded, `trade_preimage` for `sell`
with the token as `base`: `base_coin_fee.coin` and
`fee_to_send_taker_fee.coin` are the platform ticker, `taker_fee.coin` is
the token ticker, every `paid_from_trading_vol` is `false`, and `total_fees`
has one entry for each of the two tickers. A swap started from the same pair
shows the platform ticker and `false` in `taker_payment_trade_fee` and
`fee_to_send_taker_fee` of its `Started` event.

**T40.** *Reference characterization: short platform coin (R95, R97).* With
the token funded and the platform coin's balance below the gas figure, and a
node that answers the gas estimates: `trade_preimage` for `setprice` with the
token as `base` answers `NotSufficientBaseCoinBalance`, HTTP 400, whose
`coin` is the platform ticker and which carries `available`, `required` and
`locked_by_swaps`; `max_maker_vol` for the token answers the same type;
`max_taker_vol` for the token answers a non-zero `result`.

**T41.** *Reference characterization: contract addresses (R98, R99).* A
maker and a taker whose EVM coin has different primary contracts and no
fallback: the taker ends with `NegotiateFailed` without sending a reply, and
the maker ends with `NegotiateFailed` after its wait. With the taker's
fallback equal to the maker's primary contract the swap passes negotiation,
and both `Negotiated` events show the maker's primary address for that coin.
With a taker fallback the maker does not know, the taker ends with
`NegotiateFailed` after the maker's negative acknowledgement.

## 51.6G Bound Agreement of the Chain Reads a Swap Step Rests On

> **Status (Reloaded-original; maintainer decisions of 2026-11-22).** This
> section records no claim about the reference implementation. It is an
> operator's choice of trust: it governs only which chain answers a node acts
> on, and it has no compatibility effect, since no message and no transaction a
> peer validates changes. It edits no sentence of this chapter.

**R100.** *The chain reads behind a refund or a claim must agree.* Before a
swap takes a refund step, or a step that uses the swap's secret, the chain
reads that step rests on and that the node cannot prove for itself MUST agree
between two nodes of the coin's node pool. These are the counterparty's payment
state, whether a payment is claimed, refunded or not spent before a refund, and
the chain time of the refund gate. A secret read from a claim proves itself
when its hash equals the swap's secret hash (R71); it is used on one node's
answer.

1. *Agreement.* Two answers agree when each of them on its own satisfies the
   step's condition: both show the counterparty's payment in the same state;
   both show the payment as claimed, both as refunded, or both as not spent;
   both chain times are at or past the lock of the refund gate.
2. *A node the operator trusts.* An operator MAY instead name one node to trust
   through the optional activation field `swap_evidence_node`. The field holds
   one node address, which MUST also be in the coin's node list. With the field
   set, that node's answer alone is taken.
3. *Undetermined.* When the nodes disagree, or fewer than two answer (with the
   field set: when the named node does not answer; no other node and no single
   other answer is taken in its place), the read is undetermined. The step
   waits, and it MUST NOT refund or claim on an undetermined read. A claim that
   rests only on a secret proven by its hash (the last sentence of R100's head)
   needs no agreeing read, and an undetermined read does not hold it back.
4. *Admission.* A coin whose node list has fewer than two nodes and which has
   no `swap_evidence_node` does not start a swap: an order with it is refused
   at placement and at the swap start, with a reason that names
   `swap_evidence_node`.
5. *Scope.* R100 applies to every coin family.
6. *The operator's own daemon.* A coin activated in native mode reads its chain
   through the operator's own full-node daemon (native RPC); that daemon counts
   as the node the operator trusts (item 2), no `swap_evidence_node` is needed,
   and item 4 does not refuse the coin.
7. *Two nodes are two hosts.* The two nodes of R100 are two distinct hosts.
   Entries of the coin's node list that name the same host (for example its TCP
   and its SSL port) are one node. Whether two entries name the same host is
   judged by the host part of each address as written, compared without regard
   to letter case and without name resolution, so two names of one machine
   count as two nodes. Hosts run by the same operator count as two nodes, and
   no warning is given for them.
8. *No outvoting.* Each attempt of a read takes its own pair of answers. When
   the two disagree, the attempt is undetermined (item 3), and a later attempt
   needs a new pair that agrees; answers of different attempts are not
   combined, and a disagreement is not outvoted by further answers.
9. *Swaps already running.* A swap that is running when its coin stops meeting
   item 4 follows R100 as well: its reads are undetermined and wait (item 3),
   and the node logs how to fix it, by adding a second node to the coin's node
   list or by setting `swap_evidence_node`.

> *Status note (2026-11-23, Reloaded-original; maintainer decision of
> 2026-11-23).* Item 9 has a bound. A running swap whose refund reads stay
> undetermined waits at most until its refund deadline, the lock time of its
> own payment plus 3700 seconds, for the maker (R18) and for the taker (the
> deadline table of §51.5). Then it ends with its refund-failed event
> (`MakerPaymentRefundFailed` or `TakerPaymentRefundFailed`), whose text names
> the fix: add a node on a second host to the coin's node list, or set
> `swap_evidence_node`, and then run `recover_funds_of_swap`. No sentence of
> R100 is changed.

> *Status note (2026-11-23, a second one, Reloaded-original; maintainer
> decision of 2026-11-23).* The bound of the note above is refined; that note
> stays as written. A refund step of the maker or the taker whose reads stay
> undetermined waits until the later of two moments: the lock time of its own
> payment plus 3700 seconds, and 3700 seconds after the moment at which the
> refund step, in its current run, first could act. The refund step first could
> act when its refundability check first answered that the refund can be sent,
> or when a look or a refundability check of the step first gave an
> undetermined answer at or after the lock time; while that moment has not
> come, the step does not end. That moment is not stored, so a restart starts a
> new wait of 3700 seconds. A chain whose refundability check follows the
> median time past can open the refund up to about an hour after the lock time,
> and the later of the two moments keeps a full wait for it. Then the step ends
> with its refund-failed event, whose text names the kind of failure; only when
> the read is undetermined under R100 does it name the fix of the note above (a
> node on a second host, or `swap_evidence_node`, then
> `recover_funds_of_swap`). No sentence of R100 is changed.

**T42.** *Agreement of the reads (R100).* Over two scripted nodes: answers that
agree by item 1 let the refund step and the step that uses the secret go on
(including two chain times that differ but are both past the lock); answers
that differ, and a single answering node, leave the step waiting with no refund
and no claim; a secret whose hash matches is used on one node's answer, and one
whose hash does not is not used; with `swap_evidence_node` naming one of the
nodes, that node's answer alone is taken, and when it does not answer the step
waits; a `swap_evidence_node` outside the coin's node list is not accepted; a
coin with one node and no `swap_evidence_node` is refused at order placement
and at the swap start.

**T43.** *Refinements of R100 (items 6 to 9).* A coin read through the
operator's own daemon starts swaps and takes that daemon's answer alone; a node
list whose two entries name one host (TCP and SSL) counts as one node and is
refused by item 4; two hosts of one operator count as two nodes, with no
warning; after a disagreement, one further agreeing answer does not decide, and
a new agreeing pair does; a running swap whose coin loses its second node waits
and logs the two ways to fix it.

*Status note (2026-11-23, clean side; the maintainer's decisions of 2026-11-22
and 2026-11-23 and decisions within the lane). How the code applies R100.* It
changes no sentence above.

1. *Nodes.* Nodes are counted as item 7 defines them; two schemes of one host
   (for example `https` and `wss`) also name one node.
2. *The field.* `swap_evidence_node` matches a list entry with the same host;
   ports are compared only when both have one, and the default port of a scheme
   counts as written. The activation error `SwapEvidenceNodeNotInList` names the
   ticker and the node as host and port only, never a scheme, path, query or
   credential.
3. *Per family.* UTXO, BCH and QTUM over Electrum count the configured servers.
   In native mode the daemon is trusted as item 6 says; a field equal to the
   daemon's address is accepted, any other value refused. A QRC20 token takes
   its platform's policy, and the field is part of the shared QTUM client's key.
   An SLP token takes its platform's policy, but has too few nodes while its
   `bchd_urls` hold fewer than two hosts. EVM counts the nodes that passed the
   activation probe; the field may name a configured node that failed it, which
   is then a trusted node that does not answer. ZHTLC counts
   `light_wallet_d_servers`, and the field must be one of them. Sia has one node
   and starts swaps only with the field naming it. Tendermint counts its RPC
   nodes. A token of a platform takes the platform's policy.
4. *Adoption.* Each family switches to R100 through one table in the code once
   its reads ask two nodes; until then its coins keep today's reads and are not
   refused at admission. The table marks a family whose swaps are switched off
   in a release; R100 binds the families that swap. A release-gate test fails
   while a family that swaps has not switched, and a test in every build fails
   when a family's swaps are switched on while the table still marks them off. A
   release is not tagged before the gate passes.
5. *Asking two nodes.* Reads are asked as item 8 says: an attempt asks nodes of
   distinct hosts, in the order of preference the family's node list gives,
   until two have answered. Each node is read at its own tip, never at a height
   both share. Two different answers are a disagreement, also when the tips lie
   far apart. Two equal answers whose tips lie further apart than the read
   allows (a height or a time) leave the read undetermined. An answer that
   proves itself, as R100's head says of a secret, is taken alone from any node,
   also when too few nodes answered or the tips lie too far apart.
6. *Reporting.* An undetermined read is reported with its reason and what each
   node answered, each node named by host and port only. The swap step
   recognizes it by one marker in that text, wherever the text is wrapped.

*Status note (2026-11-24, clean side; Reloaded-original; maintainer decision of
2026-11-24). A coin listed with one host.* It changes no sentence above.

1. *The switch.* The top-level setting `swap_evidence_allow_single_host`, a
   boolean that is `true` when absent, relaxes item 4 for a coin with no
   `swap_evidence_node` whose node list, as configured, names exactly one host
   by item 7. That host is the node the operator trusts, exactly as if
   `swap_evidence_node` named it (item 2), and item 4 does not refuse the coin.
2. *The list as configured.* Whether a list names one host is judged by the list
   the coin was activated with, never by the hosts that answer: a coin listed
   with two or more hosts stays under items 1 to 4 while one of them does not
   answer, and a coin whose one host does not answer has a trusted node that
   does not answer (item 3).
3. *Unchanged.* A coin with `swap_evidence_node`, a coin read through the
   operator's own daemon (item 6) and a coin with an empty list are unchanged.
   With the setting `false`, item 4 applies as written.
4. *Visibility.* The activation answer of a relaxed coin carries
   `swap_evidence_single_host` with the value `true`, and the node logs one
   warning that names the coin and no node. Reads log nothing more for it.
5. *SLP tokens.* The setting covers the bchd servers of an SLP token's platform
   too: with it `true`, a platform whose `bchd_urls` name exactly one host
   trusts that host for the token's reads, and the token takes its platform's
   policy instead of having too few nodes; two or more bchd hosts stay under
   items 1 to 4. The platform's activation answer carries
   `swap_evidence_single_bchd_host` with the value `true`, and the node logs one
   warning that names the platform and no host.

*Status note (2026-11-26, clean side; Reloaded-original; maintainer decision of
2026-11-25). A readiness check before a swap starts.* It changes no sentence
above and adds no condition to R100: what R100 requires of a step is unchanged.
The check only keeps a swap from starting whose reads could not be agreed at
that moment.

1. *Ready.* A coin is ready when its reads can be agreed now: under items 1 and
   7, at least two distinct hosts of its list have answered a request within the
   last 60 seconds; with a trusted node (item 2, or a coin listed with one host
   by the status note of 2026-11-24), that node has; a coin read through the
   operator's own daemon (item 6) is ready while the daemon answers. A host not
   heard from within that time is asked once, with a 3-second limit and at most
   once a minute, and only when the hosts heard from are not enough. A family
   that has not adopted R100 is always ready.
2. *Maker.* A maker answers a taker's request only when both coins of the pair
   are ready; otherwise it sends nothing, so no message changes. While a coin is
   not ready, the maker's orders with it stay saved but are not announced, and
   they are announced again when it is ready. A new order on such a coin is
   accepted and starts paused. One log line names each change of a coin, with
   the ticker and the kind, and no host.
3. *Taker.* A taker refuses `buy` and `sell` when a coin of the pair is not
   ready, and a taker swap checks both coins again before it pays the taker fee;
   when either is not ready, the swap ends before the fee is sent.
4. *After the start.* A host that stops answering after the start is handled by
   items 3 and 9 as before.

> **Status note (2026-11-27, Reloaded-original, clean side; maintainer
> decision of 2026-11-27).** The confirmation wait of an EVM coin under
> the agreed-reads policy is agreed by two nodes for every caller. The
> confirmation waits of the legacy swap path (the maker and taker payment
> confirmations and the confirmation of a found spend) are therefore
> agreed as well, as the confirmation wait of a UTXO coin already is. The
> confirmation read is shared by the legacy and the v2 swap path. No
> sentence of R100 is changed.

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

**R50B.** *The transaction-helper topic (2026-10-30, Decision BB; the
publisher side is parity, the receiver side a Reloaded correction of a
reference defect -- see the notes below and D6).* Each side publishes the spend and refund transactions it
builds on the coin's transaction-helper topic (`txhlp/<ticker>`), so that
other nodes can broadcast them too: after its own broadcast succeeded, and
also when its own broadcast failed with the transaction at hand. The
message is the transaction's bytes in the signed envelope that R43 uses,
signed by the key that signs the swap's messages. A node that receives such
a message for a coin it has enabled MUST decode and verify the envelope and
broadcast the transaction inside it through that coin, never the message
itself. A message that does not decode or verify is discarded with a
warning. A broadcast that the network refuses is the usual outcome, since
the parties have normally broadcast the transaction already (it is in the
mempool or in a block), and is logged at debug level only. Which nodes act
on the topic is open (D6). What a reference receiver does with the message
was open too; the note below records it (passed the KDF Dirty Gate on
2026-11-04, PR #232, merged 2026-11-05). *(Pointer added 2026-11-07.)* The reference's publishing side --
which steps of the maker, the taker, their recovery paths and the watcher
publish, in which outcomes, for which coin kinds, and with which key -- is
recorded in [Chapter 09](09-watcher-reward-infrastructure.md) R24, which
passed the KDF Dirty Gate on 2026-11-07.

> **Reference behavior of the transaction-helper receiver (passed the KDF
> Dirty Gate on 2026-11-04, measure 0.04; Spec Reader 2026-11-04, rows `v2.6.0-beta` and `v3.0.0-beta` of
> chapter 02 §2.15A, identical at both).** The reference subscribes a node to
> a coin's transaction-helper topic only when that coin is activated through
> the legacy `enable` method in **native** (local daemon) mode; an Electrum
> or task-route activation subscribes nothing, so such a node never receives
> the topic's messages, and a relay that is not subscribed does not forward
> them. A subscribed receiver, relay or not, looks the coin up by the topic's
> ticker, requires the message to parse as a raw transaction of that coin,
> and -- only when the coin is in native mode -- broadcasts it through the
> coin's daemon; a message that fails to parse is logged and dropped
> **without propagation**, and a message for a coin the receiver has not
> enabled is propagated without action. The sender, as R50B says, publishes
> the transaction **inside the signed envelope of R43**.
>
> **Code-quality finding (informative; trigger: a contradiction between two
> things read side by side, the sender and the receiver).** The reference
> receiver applies the coin's raw-transaction parser to the **whole received
> message**, not to the transaction inside the envelope: it neither unwraps
> nor verifies the envelope. An envelope is not a raw transaction of any
> coin, so at both reference rows every helper message a reference node
> receives for a coin it has enabled fails to parse, is dropped, and is
> neither submitted to the chain nor propagated; the topic is inert on the reference side. This is a finding
> against the reference, **not** a convergence target: R50B's rule -- decode
> and verify the envelope, broadcast the transaction inside it -- is the
> correct behavior, and a Coder shall not adopt the reference's. For D6 it
> settles the open question: a reference receiver does nothing useful with a
> helper message in native or Electrum mode, so the topic helps only between
> Reloaded nodes. Chapter 09 R23 records a relay's handling of the watcher
> topic, which does propagate.

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
deployed wire format, not of secp256k1. For a shielded coin, the HTLC key
behind that public key is derived per swap and never stored
([chapter 39](39-zcash---z_coin-shielded-coin.md) R39.5.6--R39.5.9).

> **Reloaded status under an HD login (informative, 2026-11-04).** For a
> secp256k1 coin the machines resolve the coin's own HTLC key where the coin
> supplies one, and the node's persistent key otherwise. Under an iguana login
> these are one key. Under a global-HD login
> ([chapter 45](45-startup-configuration-and-environment-tolerance.md)
> R45.4.8) they differ. A UTXO coin activated through the HD task route
> supplies its activated HD key, and its spend and refund outputs go to that
> key's address; it funds the swap from its first address (account `0`, chain
> `External`, address `0`), so the two agree only when it was activated there.
> A coin that supplies no key of its own gets the node's persistent key, which
> under HD is the hash of the passphrase text. For EVM coins and for coins
> enabled through legacy `enable` or `electrum` that is not the key the coin
> pays from. A swap of such a coin under an HD login does not complete as
> intended. Plan
> [`hd-key-unification`](../plans/hd-key-unification.md) first refuses those
> swaps (item H1, chapter 05 §5.12B R36), then gives every coin its own
> key (item H3, chapter 05 §5.12C R41–R43). R74 states the target: the internal key as
> the persistent key and the coin's own key as each HTLC key; chapter 05
> §5.9B tabulates the keys.

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

> *Status note added 2026-11-17 (Reloaded-original, clean side; the cleared text
> above is unchanged; no forbidden-class input).* The Sia key this rule pads is
> the ed25519 key of the single Sia address. Under a global-HD login that key is
> derived at the fixed single-address path by default; when the Sia activation
> request carries `priv_key_policy: IguanaPrivKey`, it is built from the
> internal key's secret bytes instead, as under an iguana login, and the HTLC
> key follows it (padded as above). See chapter 05 R46, status note of
> 2026-11-17.

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

> **Status note (2026-11-22, Reloaded-original, clean side; maintainer
> decision of 2026-11-08).** TRON (a TRX coin and a TRC20 token) joins
> the forcing families of this rule, as
> [Chapter 21](21-tron-integration.md) R21.B6 requires: the Tron-VM V1
> swap contract chosen there (chapter 21 D21.B2) locks with SHA-256 over
> 32 bytes. `select_secret_hash_algo` has the arm: a TRX or TRC20 coin on
> either side of the pair selects the 32-byte alternate. An ETH or ERC20
> coin is not in the family and keeps the 20-byte default, unless the
> other side of its pair forces the alternate. No TRON swap runs until
> the TRON swap switch is on (chapter 21, the status note at R21.B3), so
> the arm changes no swap that can run today. A saved swap keeps the
> secret hash it was started with: the selection is evaluated once, by
> the maker at the start of a new swap, and the result is saved with the
> swap. The TRON payment path that carries the 32-byte value unnarrowed
> (R72A) is not built yet; it comes with the TRON swap operations. The
> remark that "the TRON V1 path has no secret hash", at the end of the
> note of 2026-11-08 on the EVM leg further below, describes the tree
> before this note; the TRON payment path will carry the 32-byte value
> as R72A requires. The cleared text above is unchanged.

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
>
> **EVM leg: closed in code and verified in the lab as of 2026-11-08.** The
> "plausible in other coin families" remark above held for the EVM legacy
> swap contract. That contract takes the hash as a `bytes20` argument and
> verifies a spend with `ripemd160(sha256(secret))`, which is exactly the
> second case of R72A. Before this change the EVM coin layer put the shared
> `secret_hash` into that slot unchanged: with the 32-byte alternate the
> ABI encoder accepted the value, the counterparty decoded only its leading
> 20 bytes and refused the payment ("secret_hash arg ... is invalid"), and
> such a payment could not have been spent. Now the EVM layer derives the
> slot value with one helper, `contract_secret_hash`
> (`mm2src/coins/eth/secret_hash_value.rs`): 20 bytes unchanged, 32 bytes
> `RIPEMD160` applied once, any other width an error with the text of the
> UTXO comparator. The payment build (`send_hash_time_locked_payment`, ETH
> and ERC20 arms, `eth_impl.rs`) embeds the derived value and fails before
> building when the width is wrong; the payment validation
> (`validate_payment`, both arms) compares the decoded slot with the derived
> value through `check_payment_secret_hash_arg`, and its error names the
> derived value as the expected one. The swap id (`etomic_swap_id`) is still
> computed over the shared value on both sides, unchanged. Spend and refund
> read the payment's own arguments from the payment transaction, which hold
> the derived value already, and secret extraction reads the revealed secret
> from the spend call, so they needed no change (R72B holds: the maker and
> taker stage graphs pass the shared value unchanged). The 20-byte path is
> byte-for-byte as before. Tests: `eth_secret_hash_tests.rs`, all prefixed
> `r72a_` (helper, payment build for ETH and ERC20 against literal vectors,
> swap id, validation). Run on 2026-11-08: `cargo test -p coins --lib -- eth::`
> passed (425 tests, the new ones included) and the formatting check passed.
> Lab (2026-11-08, build of PR #290, Siacoin against USDC-MON on Monad, 32-byte
> secret hash H32, contract value D20 = RIPEMD-160(H32)): cell A (the maker sells
> SC) and cell A2 (the maker sells USDC-MON) passed, payment and spend, and cell
> B, the refund case (maker USDC-MON, taker Siacoin), passed. In cell B the
> maker's real payment carried D20 left-aligned in the hash slot (payment word 4)
> and the leading 20 bytes of H32 (the value before the fix) appeared in no
> word; the maker's refund transaction carried D20 in its payment-hash slot
> (refund word 2); the maker ended
> MakerPaymentRefunded and Finished with no "secret_hash arg ... is invalid" line
> in either log; the taker never sent its Siacoin payment and ended cleanly. The
> maker's USDC-MON balance was exactly back to its starting amount after the refund (the
> token is not reduced by gas; gas is paid in MON). A run of the build before
> the fix, for comparison, was not done.
>
> The QRC20 legacy swap uses the same contract and the same `bytes20` slot,
> and has the same defect and the same fix, with the same helpers
> (`mm2src/coins/qrc20/swap.rs`). `erc20_payment_call_bytes` derives the slot
> value (a wrong width is an `InvalidParams` error, so no payment is built);
> both the payment build and the validation's expected call bytes come from
> it. `validate_payment` also compares the decoded slot with the derived
> value through `check_payment_secret_hash_arg`, before the call-bytes
> comparison, so the error names the derived value as expected.
> `extract_secret_impl` and `search_for_swap_tx_spend` compare the
> `dhash160` of a revealed secret with the derived value instead of the
> shared one, which for a 32-byte shared value could never match.
> Unchanged, with reasons: the swap id (`qrc20_swap_id`, over the shared
> value on both sides); the refund (`refund_hash_time_locked_payment`,
> `sender_refund_output`) and `wait_for_tx_spend_impl`, which read the
> `secret_hash` of the payment transaction, already the derived value; the
> trade-fee estimate, which uses a 20-byte dummy. The QRC20 path is covered
> by unit tests only (`r72a_qrc20_*` in `qrc20_tests.rs`; run on 2026-11-08:
> 9 passed); the live lab row is Siacoin against an EVM token. The v2 EVM
> swap contracts take 32-byte hashes (`bytes32`) and the TRON V1 path has no
> secret hash; neither is affected.

**R74.** *Which key each side puts on the wire under a global-HD login.*
*(Passed the KDF Dirty Gate on 2026-11-04, measure 0.04. Reference fact, Spec Reader 2026-11-04, rows
`v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at both.)*
Under a global-HD login (chapter 05 R20) the `persistent_pubkey` of R59, the
own-key field of the start event and the own-key column of the swap
statistics record (R50A) are the node's **internal key** of chapter 05 R16;
each per-coin HTLC public key of R63 is the **coin's own key** -- for a
UTXO-family, QTUM, BCH, SLP, QRC20, EVM or Tendermint coin the key activated
at the coin's `path_to_address`, for a shielded coin the per-swap derivation
of chapter 39 R39.5.6, for Sia the padded account key of R64. No key derived
by hashing the mnemonic string appears anywhere on the swap wire under a
global-HD login. Because the two HTLC keys then differ from the persistent
key, the sender emits shape 3 under R60. The reference performs no
key-consistency check before a swap (chapter 05 R34): the key on the wire is
the key that signs. Under the legacy single-key login all of these are the
one key, and shape 2 results.

> **Compatibility (this repository; not corpus-sourced).** Reloaded's v1
> machines put a hash of the mnemonic string on the wire as the persistent
> key and, for every coin that supplies no swap key (EVM; UTXO-family coins
> activated through the legacy route), as the HTLC key too, so the payments
> that follow are signed by a different key than the one announced. This is
> recorded in plan [`hd-key-unification`](../plans/hd-key-unification.md)
> (items H3 and H4) and is a divergence from R74, not a retained parity
> behavior. Chapter 05 §5.12C (item H3) binds Reloaded to R74's HTLC keys
> and to the UTXO funding address, and §5.12D (item H4) binds it to R74's
> persistent key. A swap that recorded the passphrase hash keeps it
> (chapter 05 R47). Correcting it changes the key Reloaded
> announces for its own swaps and is therefore visible to peers and to swap
> statistics at upgrade; a swap in flight must keep the key its start event
> recorded.

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
| A taker in its refund chain claims a taker payment the maker has spent, instead of failing the refund | R31a |
| A completed maker swap (its record holds a confirmed spend of the taker payment) is never refunded, by the manual recovery or on its own | R75, R76, R77, R78 |
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

**T29.** *The refund chain claims a spent taker payment (R31a).* With a
taker swap in its refund chain and the chain answers mocked:

1. a spend found on entry emits `TakerPaymentSpent` with the extracted
   secret and continues at `STAGE-SPEND-MAKER-PAYMENT`; the
   refundability wait is not entered and no refund is built;
2. with the wait answering "not yet" and the spend appearing after its
   second step, the same outcome follows after exactly two steps, and no
   refund is built;
3. a refund rejected for a terminal reason, with the spend appearing
   then, gives the same outcome instead of `TakerPaymentRefundFailed`,
   and the rejected refund is not published;
4. with nothing on chain, the rejection ends in `TakerPaymentRefundFailed`
   as R31 states, after the entry look and the post-rejection look;
5. a swap whose record already holds a spend of the taker payment does
   not look and refunds as R31 states;
6. a refund already on chain emits `TakerPaymentRefunded` with that
   transaction, builds nothing and publishes nothing;
7. a refundability check that answers "wait 0 s" five times before the
   payment becomes refundable leads, within the 60-second look interval,
   to no look besides the one on entry, and the refund goes through;
8. a search that fails, a spend whose secret cannot be extracted, and a
   search that does not finish within its bound each leave the chain as
   R31 states (a terminal rejection still ends in
   `TakerPaymentRefundFailed`; an accepted refund still ends in
   `TakerPaymentRefunded`).

*(Added 2026-10-29 with R31a; items 7 and 8 added 2026-10-30 after
review.)*

**T30.** *The transaction-helper receiver unwraps the envelope (R50B).* A
transaction's bytes signed as a transaction-helper message yield exactly
those bytes, which are what is broadcast; a bare transaction and an envelope
with a changed signed byte are both refused.

**T31.** *HD login keys on the v1 wire (R74).* Under a global-HD login, a v1
swap of a UTXO coin activated at a non-default `path_to_address` and a v1
swap of an EVM coin shall each emit a shape-3 negotiation payload whose
`persistent_pubkey` is the internal key and whose per-coin HTLC key is the
coin's activated key; the payment, spend and refund transactions of each
shall verify against that HTLC key; and no field of the payload shall equal
a key derived by hashing the mnemonic string.

**T32.** *Refusal to refund after the maker's own spend (R75-R78).* A maker
swap whose log ends in `Finished`:

1. with the record holding `TakerPaymentSpendConfirmed`, the maker payment
   unspent and the refund time passed, a call refuses, the text says that the
   taker payment spend was sent and confirmed, and no transaction is built or
   broadcast;
2. with the same record and the refund time not passed, a call refuses the same
   way;
3. with the same record and the maker payment spent, a call refuses the same way
   and builds nothing;
4. after any of these calls the log, the status and the recoverable flag of the
   swap are unchanged;
5. a swap that has not terminated is refused by the "must be finished" guard,
   and a swap whose record holds the refund of the maker payment is refused as
   not recoverable;
6. a swap without a spend is answered as before (R78);
7. with the record holding `TakerPaymentSpent` but not its confirmation, the
   maker payment unspent and the refund time passed, a call refuses, the text
   says that the taker payment spend was sent and that the maker payment belongs
   to the taker, and no refund is built or broadcast;
8. with the same record and the refund time not passed, a call refuses the same
   way, with the same text;
9. with the same record, the maker payment spent and the taker payment unspent,
   a call spends the taker payment, as for a swap without a spend;
10. with the same record, the maker payment spent and the taker payment already
    spent, a call refuses and builds nothing;
11. with the same record and the maker payment refunded on the chain, a call
    refuses as for a swap without a spend.

Existing unit tests that cover items 1 to 3 and 5:
`test_recover_funds_should_not_refund_on_the_successful_swap` (items 1 to 3),
`test_recover_funds_maker_swap_not_finished` and
`test_recover_funds_maker_swap_maker_payment_refunded` (item 5).

**T33.** *The stale-tip warning (R79-R81).* With a height source and a clock
that the test controls:

1. a height that has not changed for 59 minutes of observation gives no
   warning; at 60 minutes it gives exactly one, naming the ticker, the
   height and 60 minutes;
2. while the height stays the same, no second warning before 120 minutes; at
   120 minutes a second one, naming 120 minutes;
3. a change of height gives no warning and starts a new stall clock; a stall of
   59 minutes after the change gives none;
4. a read that fails, or does not finish before the sleep of its step ends, is skipped: it neither warns nor resets the clock, and the
   observation after it counts from the last successful read;
5. the first observation of a wait only records the height;
6. the text of the warning contains the ticker and no address, URL or host;
7. in the maker's and the taker's refund wait the sleep is cut into slices of
   at most 5 minutes, the height is read once per 5-minute period (not once per
   shorter step), the total sleep and the refund decision are those of the
   unchanged wait, and the warning is written once the stall reaches 60 minutes.

**T34.** *The refund chain's look asks every Electrum server (R31a, status
note of 2026-11-21).* With a taker swap in its refund chain and an
Electrum-backed taker coin whose spread search fails, or finds nothing,
while another server of the coin's rank holds the maker's spend of the taker
payment:

1. the look on entry emits `TakerPaymentSpent` with the extracted secret
   and continues at `STAGE-SPEND-MAKER-PAYMENT`; no refund is built;
2. the same outcome follows from the look after a refund rejected for a
   terminal reason, and the rejected refund is not published;
3. a refund of the taker payment found on another server emits
   `TakerPaymentRefunded` with that transaction and ends the chain;
4. when every server fails, does not answer within the shared bound, or
   finds nothing, the chain continues as R31 states;
5. a taker coin that is not Electrum-backed makes its single search only.

*(Added 2026-11-21 with the status note under R31a.)*

**T35.** *A restart before the outcome of the claim is saved (R82, status
note of 2026-11-21).* With a maker swap restored from a saved log that ends at
`TakerPaymentValidatedAndConfirmed`, the taker payment known:

1. the restored swap resumes at `STAGE-SPEND-TAKER-PAYMENT`;
2. the claim is sent again and the send fails (the claim sent before the
   restart is already on the chain): `TakerPaymentSpendFailed` is recorded, the
   refund stage searches the taker payment, finds the maker's confirmed claim,
   records `TakerPaymentSpent` and `TakerPaymentSpendConfirmed` and finishes
   without building a refund;
3. the same log after four fifths of the lock duration have passed:
   `TakerPaymentSpendFailed` is recorded without a send, and the outcome is as
   in item 2;
4. the same log with nothing found by the search: the refund is built once the
   maker payment's refund time has passed.

A second send that is accepted records `TakerPaymentSpent`, and the wait for
the claim's confirmation decides as before; T35 has no item for it.

*(Added 2026-11-21 with the status note under R82.)*

**T36.** *Reference characterization: where the secret is read and whether
it is compared (R85, R86, R89 item 6, R90 item 4, R91).* These items
describe the reference; they are not acceptance tests for a Reloaded check
that is stricter. Given a spending transaction and the swap's secret hash:

1. ETH/ERC20: a transaction whose call input is a `receiverSpend` call
   yields the third argument, whether or not it hashes to the secret hash; a
   transaction with any other call input yields an error, whatever its logs
   hold;
2. QRC20: a transaction with two `receiverSpend` outputs, of which only the
   second hashes to the secret hash, yields the second secret; with none
   that hashes, an error;
3. UTXO and SLP: a transaction whose matching 32-byte push stands in an
   input other than the first, or at a position other than the second
   element, yields that push; a transaction whose 32-byte pushes do not hash
   to the secret hash yields an error;
4. Tendermint: a transaction whose first message is a claim yields its
   secret, whether or not it hashes to the secret hash; a transaction whose
   claim is the second message yields an error.

**T37.** *Reference characterization: how a spend or a refund is found and
classified (R87, R89 items 1–5, R90 items 1–3).*

1. ETH/ERC20: a `ReceiverSpent` log of the swap contract whose data begins
   with the swap id gives *spent* with the transaction the log names; a
   `SenderRefunded` log likewise gives *refunded*; with both in one window,
   *spent*; the receipt of the named transaction is not requested;
2. ETH/ERC20: the wait returns for a `ReceiverSpent` log only and keeps
   waiting through a `SenderRefunded` log until its deadline;
3. QRC20: a payment transaction without a confirmation, or whose
   `erc20Payment` receipt has an `excepted` value other than none, gives an
   error; a spend candidate is taken from a receipt that holds the event log
   and is matched by a `receiverSpend` call in any output of the same
   transaction; the candidate's `excepted` value is not read;
4. UTXO and SLP: a spending transaction whose second input spends the swap
   output is classified by that input's signature script (empty push as
   third element: *spent*; the number one as second element: *refunded*;
   neither: an error), whatever the first input holds; the swap output is
   output 0, and output 1 for an SLP token.

**T38.** *Reference characterization: refund eligibility (R88).* With a lock
time `L`:

1. ETH/ERC20, QRC20, SLP and Tendermint: at local time `L` the answer is to
   wait one second; at `L + 1` the answer is *refundable*; no request to the
   chain is made for the answer;
2. a standard UTXO coin, BCH and the QTUM base coin (not KMD): at local time
   `L − 5000` the answer is to wait 3600 seconds without a request to the
   chain; at local time `L + 10` with a median time past of `L` the answer is
   to wait one second; with a median time past of `L + 1` the answer is
   *refundable*.

*(T36–T38 added 2026-11-22 with §51.6E; gate status as §51.6E states.)*

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

**D6.** *Who acts on the transaction-helper topic.* A node subscribes to a
coin's transaction-helper topic only for a UTXO coin in native mode
([Chapter 55](55-coin-activation-wire-contract.md) R55.4.6), where its own
daemon is an independent path to the network; a relay receives every topic
and today acts on R50B for any enabled coin, so a relay with Electrum-mode
coins broadcasts through the same kind of servers the parties use. Limiting
the receiver to native-mode coins, and confirming what a reference node does
with the message (whether it unwraps the envelope, and whether a relay acts
on a topic it has not subscribed to), are deferred to a Spec Reader pass.

> *Reference answer (2026-11-04; passed the KDF Dirty Gate on 2026-11-04,
> measure 0.04; last clause corrected after micro-run 0.4.0-2c, PASS 0.03).*
> The two questions D6 deferred are answered in the note under R50B: a
> reference node subscribes only for a native-mode legacy activation, acts
> only for a native-mode coin, does not unwrap the envelope and in
> consequence never gets a received helper transaction onto the chain; it
> forwards a received helper message (when it is a relay) only for a coin it
> has not enabled, and it does publish helper messages itself after its own
> spend and refund attempts, exactly as R50B describes for Reloaded. What
> remains of D6 is this project's own choice of receiver scope.

**D7.** *No automatic refund of a completed maker swap, by decision.*
Nothing looks at the maker payment of a completed swap again after
`Finished`, and nothing refunds it (R77). An automatic refund watch was
considered and rejected on 2026-11-07: the maker payment of a completed swap
belongs to the taker. A log line at the end of a completed swap about the state
of the maker payment is not specified.

**D8.** *A refund after a sent spend, under consideration.* R75 item 2 refuses
the refund of an unspent maker payment whenever the record holds a sent spend.
A case it gives up on purpose: the spend was dropped, replaced or reorganized
out of the chain, and the taker refunded the taker payment. The maker then holds
neither payment, and the taker, who knows the secret from the broadcast, can
still claim the maker payment and end up holding both. That is the reverse of
the honesty argument of R75, and it is what makes this item worth deciding. A
later version may allow the manual recovery to refund in that case only, when the
taker payment is shown on its own chain to be refunded and no spend of ours is
waiting in the pool of that chain. That needs a detection per coin family (the
families differ in how a refund is found), must fail closed on any search error,
and needs its own rule and tests; it is under consideration and not specified.
The refund that the run loop makes after the confirmation of the spend failed
(the confirmation wait ends, the log records the failure, and the refund chain
follows) makes no look at the taker payment either; it is a separate decision and
is not changed here.

> **Status note (2026-11-12, Reloaded-original).** The automatic half is
> decided: see R82. The reverse case (claim dropped, taker refunded) stays under
> consideration.

> **Status note (2026-11-12, Reloaded-original).** The reverse case is decided
> for this version: see R83 (manual recovery only; the run loop is unchanged).

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
- *Addition of 2026-11-07 (Reloaded-original, no Spec Reader input):*
  §51.6A (R75-R78), T32 and D7 rest on the project's own reading of the
  manual-recovery call and on the project's decision DK; they record no claim
  about the reference implementation.
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
  (Addition of 2026-10-29: R31a, its invariant row and T29 are a
  Reloaded addition written in a clean session from this workspace's own
  taker refund chain and recovery code and from R30, R31 and R32 of this
  chapter; no reference input was consulted, the rule makes no parity
  claim, and no Dirty Gate pass is needed.)
  (Addition of 2026-11-21: the status note under R31a and T34 are a
  Reloaded addition written in a clean session from the maintainer's
  decision of 2026-11-21, a field report, this project's own reads of public
  LCC and RVN Electrum servers and this workspace's own refund chain and
  Electrum client code; no reference input was consulted, the note makes no
  parity claim, and no Dirty Gate pass is needed.)
  (Addition of 2026-11-22: the status note after the 2026-11-21 note under
  R31a is a Reloaded addition written in a clean session from the
  maintainer's decision of 2026-11-22 and this workspace's own UTXO spend
  search; no reference input was consulted, the note makes no parity claim,
  and no Dirty Gate pass is needed.)
  (Addition of 2026-11-22, a second one: the status note before R31 that
  follows the round-0.5.0-23 note, and the pointer after the 2026-11-22 note
  under R31a, are a Reloaded addition written in a clean session from the
  maintainer's decision of 2026-11-22, the cleared §51.6E and this
  workspace's own secret extraction code; no other reference input was
  consulted, the note makes no parity claim, and no Dirty Gate pass is
  needed.)
  (Addition of 2026-11-22, a third one: the status note after §51.6E R92 is
  a Reloaded addition written in a clean session from the maintainer's
  decision of 2026-11-22, the cleared §51.6E and this workspace's own refund
  gate code; no other reference input was consulted, the note makes no
  parity claim, and no Dirty Gate pass is needed.)
  (Addition of 2026-11-22, a fourth one: the status note after §51.6E R92
  on the QRC20 search is a Reloaded addition written in a clean session from
  the maintainer's decision of 2026-11-22, the cleared §51.6E and this
  workspace's own QRC20 search code; no other reference input was consulted,
  the note makes no parity claim, and no Dirty Gate pass is needed.)
  (Addition of 2026-11-23: the status note after the 2026-11-22 pointer
  under R31a is a Reloaded addition written in a clean session from the
  maintainer's decisions of 2026-11-22 and 2026-11-23, §51.6G and this
  workspace's own refund chain code; no reference input was consulted, the
  note makes no parity claim, and no Dirty Gate pass is needed.)
  (Addition of 2026-11-23, a second one: the status notes after the
  description of `STAGE-VALIDATE-TAKER-PAYMENT`, after R28 and at the end
  of §51.6C are a Reloaded addition written in a clean session from the
  maintainer's decisions of 2026-11-22 and 2026-11-23, §51.6G and this
  workspace's own swap engine code; no reference input was consulted, the
  notes make no parity claim, and no Dirty Gate pass is needed.)
  (Addition of 2026-11-21, a second one: the status notes under R82 and
  R83 and T35 are a Reloaded addition written in a clean session from the
  maintainer's decision of 2026-11-21, an issue report, and this workspace's
  own maker run loop, resume map and recovery code; no reference input was
  consulted, the notes make no parity claim, and no Dirty Gate pass is
  needed.)
  (Addition of 2026-10-30: R50B, T30 and D6 are a Reloaded addition
  written in a clean session from this workspace's own transaction-helper
  sender and receiver and from R43 of this chapter, prompted by a Reloaded
  seed's own log from a field run on the public network, where every
  received transaction-helper message failed to broadcast; no reference
  input was consulted, the rule makes no parity claim, and no Dirty Gate
  pass is needed.)
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
  (Addition of 2026-11-04, KDF Spec Reader, **KDF Dirty Gate: PASS on 2026-11-04, measure 0.04**:
  R74, T31, the compatibility note under R74, the reference-behavior note
  and code-quality finding under R50B, and the answer note under D6 rest on
  the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at
  both. The facts taken are: which key is the v1 persistent key and which key
  each coin family's HTLC public key is under a global-HD login, the
  consequent negotiation shape, and the absence of any key-consistency check;
  and, for the transaction-helper topic, who subscribes, what a receiver
  requires of a message, when it broadcasts, when it propagates, and that the
  receiver parses the whole message rather than the envelope's content. No
  private identifier, function body, helper decomposition, log or error
  literal was reproduced. The compatibility note rests on this repository's
  own code, as inventoried in plan `hd-key-unification`; T31 is Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-04, measure
  0.04, no findings. *Extent:* the legacy swap's negotiation keys under a
  global-HD login and the transaction-helper topic, at `v3.0.0-beta` with
  `v2.6.0-beta` as cross-check; for the whole batch, 30 source files and one
  dependency manifest were read at `v3.0.0-beta`, 29 of them checked by hash
  against the tagged states of both rows. *Method:* side-by-side reading of
  the compared regions against every added or changed line, plus that hash
  check; the gate script was not run because the candidate has no code blocks.
  *Measure:* 0.04, read against the standard that any discretionary finding
  fails a file; dictated-interop and public-surface tokens were excluded from
  the measured set.)
  (Correction of 2026-11-04, KDF Spec Reader micro-run 0.4.0-2c, gated
  2026-11-04, **KDF Dirty Gate: PASS on 2026-11-04, measure 0.03**: the last
  clause of the answer note under D6, the last sentence of the code-quality
  finding under R50B, and the label of R50B. The micro-run re-read the rows
  `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at both,
  only to establish whether the reference publishes transaction-helper
  messages itself and when a relay forwards a received one: it publishes after
  its own spend and refund attempts as R50B states, and it forwards only for a
  coin it has not enabled or for bytes that parse as a raw transaction of an
  enabled coin. The corrected wording is Reloaded-original. No private
  identifier, function body, helper decomposition, log or error literal was
  reproduced. *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-04,
  measure 0.03, no finding in the passages used here. *Extent:* the
  transaction-helper topic publisher, including its call sites in the legacy
  maker and taker swaps and the watcher, and the receiver and relay-forwarding
  handler for that topic, at `v2.6.0-beta` and `v3.0.0-beta`, within a
  micro-run that read seven source files at each row. *Method:* reading, plus
  a mechanical diff between the two rows; the gate script was not run because
  the candidate has no code blocks. *Measure:* 0.03, read against the judgment
  standard of chapter 01 R35.)
- *Reloaded-original note (2026-11-04) -- "Reloaded status under an HD
  login" under R63.* It records which key the machines resolve per coin
  under a global-HD login and points to plan `hd-key-unification`. Written
  by a context that never read the forbidden corpus, from this repository's
  own swap, coin-key and activation code. No reference implementation was
  consulted, and it needs no Dirty Gate pass. *Pointer added 2026-11-05:*
  its last sentence now names R74 as the target; it carries no reference
  content of its own.
- *Pointer added 2026-11-05 -- the HD status note under R63 and the
  compatibility note under R74.* Both now name chapter 05 §5.12C (plan item
  H3). Written by a context that never read the forbidden corpus. It carries
  no reference content of its own and needs no Dirty Gate pass.
- *Pointer added 2026-11-05 -- the compatibility note under R74.* It names
  chapter 05 §5.12D and R47 (plan item H4). Written by a context that never
  read the forbidden corpus. It carries no reference content of its own and
  needs no Dirty Gate pass.
- *Pointer added 2026-11-07 -- the sentence under R50B naming chapter 09 R24.*
  It carries no reference content of its own; its target passed the KDF Dirty
  Gate on 2026-11-07, as chapter 09 records. **KDF Dirty Gate: PASS on 2026-11-07, measure 0.00.**
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-07, measure
  0.00, no findings; the pointer was judged with its target, whose clearance
  is recorded in chapter 09's provenance footer of the same date.
- *Pointer added 2026-11-22 -- the note under R5 naming chapter 44 §44.9A.*
  Written by the KDF Spec Reader in round 0.5.0-24. It carries no reference
  content of its own beyond naming what its target binds; the reference
  facts are in chapter 44 §44.9A, whose provenance footer records the rows
  read. **KDF Dirty Gate: PASS on 2026-11-22, measure 0.00, no findings**, as
  recorded for §44.9A in chapter 44's provenance footer.
- *Scoped addition (2026-11-22), KDF Spec Reader, round 0.5.0-23 -- §51.6E
  (R85–R92, the observation under R85, the note under R87, the three
  code-quality findings) and T36–T38.* Forbidden-class input was taken
  through the two-team procedure of chapter 01 §1.4A, from the rows
  `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A; the comparison in R90
  item 5 rests on the row *baseline anchor (R1)*. The facts taken are, for
  the ETH/ERC20, QRC20, UTXO, SLP and Tendermint families of the legacy
  swap: where the secret is read in a spending transaction and whether it is
  compared with the secret hash; by which chain data a spend or a refund of
  a payment is found and classified, and what is not checked while doing so;
  and on which clock, with which margin, a payment counts as refundable. The
  method and event names used are those of the public swap contract's ABI,
  the public JSON-RPC and Electrum method names, and the public transaction
  search key of the HTLC module. No private identifier, function body,
  helper decomposition, log or error literal was reproduced. The proposed
  fixes in the findings are the author's own. **KDF Dirty Gate: PASS on
  2026-11-22, measure 0.03, no findings** (chapter 01 R40; the attestation
  closes this item).
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-22, measure 0.03,
  no findings, in a single pass; the chapter 02 §2.15A rows were verified
  (`v2.6.0-beta`, `v3.0.0-beta` and the baseline anchor (R1)). *Extent:* the
  legacy-swap operations of the coin families -- the spend wait, the spend and
  refund search, the secret extraction and the refund-eligibility answer --
  for ETH/ERC20, QRC20, the standard UTXO family, BCH, the QTUM base coin, SLP
  and Tendermint, and the taker's use of the extraction; the row `v3.0.0-beta`
  read in full (nine source files), the row `v2.6.0-beta` in part (the
  differences of the EVM family) and the baseline anchor in part (the UTXO
  extraction and classification), with a mechanical pass over 275, 263 and 105
  files of these three rows respectively; not read: KMD, Sia, the shielded
  coins, the watcher, the version-two swap and the TRON virtual machine; on
  the candidate side, the 413 added lines. *Method:* reading plus a per-token
  forced-versus-free test and a five-word sequence comparison; the
  repository's clean-room gate tool was not applicable because the additions
  embed no code blocks. *Measure:* 0.03, on the discretionary-expression
  basis.
- *Clean-side addition of 2026-11-22 (PR #480):* the Reloaded status note
  under R5 on a runner that lost its lock. It is Reloaded-original, rests only
  on this repository's own lock and kick-start code and the cleared text of
  R5 to R7 and chapter 44 §44.9A, and changes no sentence of the chapter.
- *Clean-side addition of 2026-11-22 (PR #489):* the Reloaded status note
  after R29 on a swap resumed at its payment stage. It is Reloaded-original,
  rests only on this repository's own swap code and the cleared text of R7,
  R17 and R29, and changes no sentence of the chapter.
- *Scoped addition (2026-11-22), KDF Spec Reader, round 0.5.0-27 -- §51.6F
  (R93–R99, the two code-quality findings, T39–T41) and the status note
  after R24.* Forbidden-class input was taken through the two-team procedure
  of chapter 01 §1.4A, from the rows `v2.6.0-beta` and `v3.0.0-beta` of
  chapter 02 §2.15A; no untabled state was read. The facts taken are, for a
  legacy swap with an ERC20 token of the EVM family: which coin each fee
  figure names and the value of its trading-volume flag; the answer of
  `trade_preimage`, of the swap start and of the two maximum-volume methods
  when the platform coin cannot pay the gas; which swap-contract addresses
  the negotiation messages and the swap status carry; and how a declared
  address is accepted, replaced by the fallback contract or refused. The
  names used are those of the public RPC methods, their answer and error
  members, the swap status members and the negotiation message members. No
  private identifier, function body, helper decomposition, log or error
  literal was reproduced. The proposed remedies in the findings are the
  author's own. No cleared sentence was edited. **KDF Dirty Gate: PASS on
  2026-11-22, measure 0.03, no findings** (chapter 01 R40; the attestation
  closes this item).
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-22, measure 0.03,
  no findings; the chapter 02 §2.15A rows `v2.6.0-beta` and `v3.0.0-beta` are
  tabled. *Extent:* the legacy swap subsystem as §51.6F scopes it for an ERC20
  token leg of the EVM family: the `trade_preimage` and swap-start fee
  descriptors, the swap balance checks including the platform coin, both
  maximum-volume calculations, the EVM swap-contract address acceptance rule,
  and the maker and taker start and negotiation stages with their event data,
  compared at both release rows; four source files read, the address rule and
  the taker side at one row each and the rest at `v3.0.0-beta`, with the
  differences between the rows checked by their size only; QRC20, Tendermint,
  Sia, the shielded coins, the watcher and the second protocol version are out
  of scope. *Method:* reading all 304 added lines and the reference material,
  a forced-versus-free test of every backticked token (53, all public or
  serialized surface), and a check of the shape of the difference against the
  base; the repository's clean-room gate tool was not applicable because the
  additions embed no code blocks. *Measure:* 0.03, on the
  discretionary-expression basis.
- *Clean-side addition of 2026-11-22:* §51.6G (R100, T42), the agreement of
  the chain reads a refund or a claim rests on, with the optional activation
  field `swap_evidence_node`. It is Reloaded-original, records only the
  maintainer's decisions of 2026-11-22, takes no reference input and no
  forbidden-class input, and changes no sentence of the chapter.
- *Clean-side addition of 2026-11-22:* the status note after R72 on TRON
  as a forcing family. It is Reloaded-original, rests only on the cleared
  text of R71, R72 and R72A, of chapter 21 §21.11B.2, R21.B3 and R21.B6, the
  maintainer's decision of 2026-11-08 on chapter 21 D21.B2 and D21.B4, and
  this repository's own swap code, and changes no sentence of the chapter.
- *Clean-side addition of 2026-11-22, a third one:* R100 items 6 to 9 and
  T43, the maintainer's refinements of 2026-11-22 (the operator's own daemon,
  two hosts, no outvoting, swaps already running). Reloaded-original; no
  reference input and no forbidden-class input; no sentence of the chapter is
  changed.
- *Clean-side addition of 2026-11-23:* the status note after R100 item 9 on
  the bound of a waiting running swap (its refund deadline, then the
  refund-failed event naming the fix). Reloaded-original; it records the
  maintainer's decision of 2026-11-23, takes no reference input and no
  forbidden-class input, and changes no sentence of the chapter.
- *Clean-side addition of 2026-11-23 (PR #521):* the status note after T43,
  how the code of PRs #508 and #519 applies R100: nodes, the field, the policy
  per family, adoption and the release gate, asking two nodes, and reporting.
  It records the maintainer's decisions of 2026-11-22 and 2026-11-23 and
  decisions within the lane, takes no reference input, and changes no sentence
  of the chapter.
- *Clean-side addition of 2026-11-23, a second one:* the second status note
  after R100 item 9, which refines the bound of the first (the later of the
  payment's lock time plus 3700 seconds and 3700 seconds after the refund step
  first could act in its current run). Reloaded-original; it records the
  maintainer's decision of 2026-11-23, takes no reference input and no
  forbidden-class input, and changes no sentence of the chapter.
- *Clean-side addition of 2026-11-24 (PR #536):* the status note before §51.7
  on a coin listed with one host (`swap_evidence_allow_single_host`).
  Reloaded-original; it records the maintainer's decision of 2026-11-24,
  takes no reference input and no forbidden-class input, and changes no
  sentence of the chapter.
- *Clean-side addition of 2026-11-26 (PR #547):* the status note before §51.7
  on a readiness check before a swap starts. Reloaded-original; it records the
  maintainer's decision of 2026-11-25, takes no reference input and no
  forbidden-class input, and changes no sentence of the chapter.
- *Clean-side addition of 2026-11-27 (PR #549):* the status note before §51.7
  on the confirmation wait of an EVM coin under the agreed-reads policy, which
  is agreed by two nodes for the legacy swap path as well. Reloaded-original;
  it records the maintainer's decision of 2026-11-27, takes no reference input
  and no forbidden-class input, and changes no sentence of the chapter.
