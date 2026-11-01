# Plan: expire a withdraw's recorded spend by the chain's real block time

**Status:** planning (post-beta; nothing is implemented) — **Last reviewed:** 2026-10-30

> **Context.** Field test of 2026-10-30: two `withdraw` + `send_raw_transaction`
> pairs for DOGE, three seconds apart, from one address. The second withdraw
> spent the same input as the first and the Electrum server refused its broadcast
> as a mempool conflict. A withdraw 46 seconds later succeeded. The cause is that
> a UTXO coin remembers the outputs it spent only for its own swap sends, not for
> withdraws, so the next withdraw relies on the Electrum server having indexed the
> broadcast (CRD ch.38 §38.5B R38.5B.2, R38.5B.3). Maintainer Decision BE
> (2026-10-30) fixes this before the beta: a withdraw's spend is recorded when its
> broadcast is accepted (R38.5B.9), with no expiry of recorded entries, as in the
> reference (D38.5B.2). This plan holds the two ideas left for after the beta.

## 1. Expire a recorded withdraw entry after a multiple of the real block time

**The problem it solves.** An entry in the record never leaves it (R38.5B.8). A
withdraw whose transaction later drops out of the mempool (replaced, evicted, too
low a fee) keeps its inputs hidden and its change offered until the coin is
deactivated or the node restarts. A user withdraw is more likely to hit this than a
swap send, because the user can choose the fee.

**The idea (maintainer, 2026-10-30).** Drop a withdraw's recorded entry once a
period has passed that scales with how fast the chain really produces blocks:
take the timestamps of the last ten blocks or so, average the gaps, and expire the
entry after three times that average. A DOGE entry then lives a few minutes, a
Bitcoin entry about half an hour, and neither needs a number picked by hand per
coin.

**Is it feasible for KDF to get the timestamps cheaply? Yes.** The code already
reads recent block times for the median time past (`get_median_time_past`,
`mm2src/coins/utxo/rpc_clients.rs:420`). With an Electrum server this is one
`blockchain.block.headers` request for the last ten heights (the tip height is
already known from the header subscription), parsed with the coin's header layout
policy of ch.37 R37.5.1a, so AuxPoW and other non-standard headers are handled the
same way as there. With a native daemon the existing path needs one block-hash
call and one block call per height, about twenty calls, so it should be cached
per tip height and not asked per withdraw; the median time past field the daemon
returns does not help, because only the gaps matter. In both cases the result
changes only when a new block arrives, so one refresh per block is enough and no
extra call is made on the withdraw path itself. Points to design: block
timestamps are set by miners and are not strictly increasing, so the average
should be (newest − oldest) / (n − 1) over the sorted times, not a sum of signed
gaps; a chain with very bursty block times (after a hash-rate drop) can make three
times the average too short, so a floor (and a ceiling) is needed; and the chain
rule of D38.5B.2 applies: a linked chain of entries (a withdraw spending the
change of an earlier one) must be dropped as a unit, so that no input a later link
still spends comes back.

**Status.** Not started. The maintainer decides whether and when; if yes, it needs
a CRD addendum under ch.38 §38.5B (a Reloaded-original rule) and a switch-free
implementation like R38.5B.9.

## 2. Key the record by address script (HD coins)

The record is keyed to the coin's single address (R38.5B.8). For a hierarchical
wallet with several addresses, the spent inputs of any address are excluded from
the listing, but a change output to another address of the coin is never offered
as a replacement until the Electrum server shows it. The code-quality finding of
§38.5B proposes one record per address script. Decision BE deferred it past the
beta (D38.5B.4): the field defect was a single address, and the change touches
both coin builders, the hardware-wallet address re-establishment hook and the
tests. Same decision path as item 1.

## 3. What this plan does not cover

The pending entries of R38.5B.9 (ten minutes, 64 entries) only bound memory and
hide nothing; they stay as decided. The legacy error body for a refused broadcast
(HTTP 500 with the server's text) stays as it is: R38.5B.6 is parity with the
reference, and a typed error would be a new legacy wire shape.
