# Plan: a swap's confirmation wait and its spend look move past a bad Electrum server

**Status:** planning — **Last reviewed:** 2026-11-21

**CRD:** ch.38 R38.6.4o item 6 (dated status note), the known-limitations bullet of its as-built note
(status note), D38.6.4o.3 (status note), T38.6.4o.23 and T38.6.4o.24 (new); ch.51 R31a (dated status
note) and T34 (new). Reloaded-original, clean side. Follows
[Electrum resilience against stale and bad servers](electrum-server-staleness.md), step 2, whose known
limitation this closes.

> **Context.** A field report of 2026-11-21: a taker sold LCC for RVN on a
> development build. The taker payment had its four confirmations six minutes
> after it was mined, and the maker spent it, revealing the secret, a few
> seconds later. The taker's wait for those confirmations still ran until its
> deadline (four fifths of the lock duration, 88 minutes later) and failed with
> "Waited too long … to be confirmed 4 times". The swap went into its refund
> chain, whose looks for the maker's spend (ch.51 R31a) did not find it either.
> The maker payment was claimed about ten hours later, six hours after the maker
> could have refunded it. The chain was not slow: 55 LCC blocks in the 104
> minutes of the wait, 95 s apart on average against a target of 150 s, with no
> gap above 16 minutes.
>
> The wait of R38.6.4o item 6 chooses one server when it starts and chooses
> again only when a request to it fails in transport. A server that stays
> connected but falls behind, stops following the chain, or answers every
> request for the transaction with an error keeps the wait until its deadline.
> The R31a look reads through the coin's spread reads, and an error answer from
> the server a read reaches fails the whole look. Which of the reporter's LCC
> servers failed, and how, is not known from the report; one of the three listed
> LCC servers refuses connections since.

## Steps

### 1. The confirmation wait leaves an out-of-date or erroring server (ch.38 R38.6.4o item 6)

While `electrum_tip_checks` is `true` (the default):

- After every poll that does not end the wait, the wait checks the state of item 4 of the server it polls.
  When that server is now out of date, the wait continues on the first up-to-date maintained server that
  has not failed during the wait, or, when there is none, on the first up-to-date one that has, which then
  no longer counts as failed and is polled only after the usual pause. The out-of-date server is not
  marked as failed: when it is up to date again, a later choice may return to it.
- An error answer of the polled server to the transaction request (an answer, not a transport failure)
  counts. Three in a row (about 45 seconds at the swap's 15-second poll) treat the server as failed for
  this wait, as a transport failure does: it is left out while another choice exists, and the wait
  chooses again. Any answer that is not an error resets the count.
- An up-to-date server that answers too few confirmations keeps the wait, as before (T38.6.4o.7): a
  behind or stuck server reports too few confirmations, never too many, so moving on is safe, but an
  up-to-date server's answer is not second-guessed.
- Each change of server, whatever its reason, logs one INFO line naming the old and new server by host
  and port (§38.6.9) and the reason.

With `electrum_tip_checks` `false` nothing changes: no server is ever out of date, error answers keep the
wait on its server, and the wait polls the first connection as T38.6.4o.11 states.

Tests (T38.6.4o.23, T38.6.4o.24): two maintained stubs, *A* chosen first. *A* turns out of date during the
wait while *B* has the confirmations: the wait ends on *B*. *A* up to date with 0 confirmations and *B*
with 3: the wait stays on *A* (T38.6.4o.7 unchanged). *A* fails once, the wait moves to *B*, and *B* turns
out of date while *A* is up to date again: the wait returns to *A*. *A* answers an error three times in a
row: the wait moves to *B* and ends on its answer. Two errors and then an answer: the wait stays on *A*.
With the switch `false`, error answers keep the wait on *A* until its deadline.

### 2. The refund chain's look for the maker's spend asks every server (ch.51 R31a)

When an R31a look of an Electrum-backed UTXO taker coin ends without a spend or a refund (the search
failed, did not finish within its 120-second bound, or found nothing), the look repeats the search once on
every usable server of the coin's rank, with directed requests (R38.6.4g: a server whose connection is
closed is opened for the request and released afterwards). The servers are asked at the same time and
share one bound of 120 seconds, which begins when the first search has ended, so a look can take up to two
such bounds; that delays only the taker's own refund. The first spend or refund one of them finds is
handled as R31a states; when none finds one, the chain continues exactly as R31 states. This applies at
all three points of R31a. Coins that are not Electrum-backed UTXO coins are unchanged.

Load: one history request and the transaction requests it names per server and look, only for a swap in
its refund chain, at most once a minute (R31a item 2) until the refund lock time.

Tests (T34): with a stub Electrum coin whose spread search fails, or finds nothing, while another
server holds the maker's spend, the look on entry gives `TakerPaymentSpent` with the secret and the
swap continues at the maker-payment spend, without building a refund. The same after a rejected refund.
A refund found on another server ends the chain as refunded. When every server fails or finds nothing,
the chain continues as R31 states. A coin without Electrum keeps its single search.

### 3. Lab row (cheap, if available)

One Electrum server of the taker coin made to stop following the chain, or to refuse the transaction,
during a taker's confirmation wait, with a healthy second server: the wait ends on the healthy server
once it has the confirmations, and an INFO line names the change.

## Deliberately not in this plan

- The maker's waits and the other searches for a swap spend (`recover_funds_of_swap`, the taker's wait
  for the spend after its confirmations) keep their reads. They are candidates for the same treatment if
  a field case shows the need.
- Removing the LCC server that refuses connections from the coins file is a coins-file change of its
  own.
- No corroboration of a "confirmed" answer by a second server (D38.6.4o.3 stays as decided for that).

## Decisions

- 2026-11-21, maintainer: fix both the confirmation wait (step 1) and the R31a look (step 2), in one PR
  with the CRD notes and tests.
