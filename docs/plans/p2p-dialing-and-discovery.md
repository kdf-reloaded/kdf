# Plan: peer-to-peer dialing, discovery and send-queue follow-ups (0.4.0)

**Status:** in progress — **Last reviewed:** 2026-11-12

> **Context.** These are the peer-to-peer items deferred from the 0.3.0 beta and the field
> findings made around it. The maintainer approved batch 1 (P1 to P3, then P5) on 2026-11-02;
> the other items wait on the decisions listed under open points. All of the work is
> Reloaded-original, except where an item says it aligns with GLEEC KDF. In those places the
> reference facts are already in CRD ch.28, cleared by the KDF Dirty Gate, and no new reading of
> the reference is needed.

## Work packages

| Id | What | CRD | State |
|---|---|---|---|
| P1 | **Redial guards.** No address is dialed again within 5 minutes, as GLEEC KDF does. No configured seed is dialed while its peer is connected, which ends a dial-and-close loop every 150 s. The short-of-relays warning appears at most once per 5 minutes. | ch.28 R27a, T12 | Spec PR #199; code PR #206; lab row L-SEED passed 2026-11-05 (release tests 0.4.0-beta.1, row 14) |
| P1a | **Early redial after a cut.** A connection the node dialed that lived at least 30 s and ended in a cut is dialed again at the next maintenance tick, at most twice in 5 minutes; a peer that flaps keeps the 5-minute guard. Added 2026-11-08 (batch 4 item B4-13). | ch.28 R27a item 1a, T15 | Code PR #303; PR #328 stopped a stuck-peer close ending with a transport error from counting as a cut; lab row L-RELINK passed 2026-11-08 (release tests 0.4.0-beta.1, fourth batch) |
| P2 | **Peer-exchange polish.** Skipped addresses are logged at DEBUG, with an hourly INFO summary. The idle close of a relay outside the relay mesh logs at INFO. The routability ranges are aligned with GLEEC KDF. Two wording fixes (R8c, R23 item 3). | ch.28 R8c items 4 and 7, R23 item 3, R24, R27b, T6a, T13, D9; ch.32 note under R-F3b | Spec PR #203; code PR #211; peer-exchange probe passed 2026-11-05 (release tests 0.4.0-beta.1, row 15; the hourly summary was not exercised) |
| P3a | **Send-queue windows visible.** Each connection's dropped share per 60 s window, as a DEBUG line and a gauge. | ch.28 RQ1 items 6 and 8, T-RQ1.13; ch.32 R-MP5, T-MP10 | Spec PR #204; code PR #214 |
| P3b | **Queue-signal clear rule.** The signal clears when nothing has waited 2 s or more, not only after a 5 s empty queue. It affects only makers with `maker_update_coalescing` on (see `maker-gossip-rate-control.md`). | ch.32 R-MP2 item 2, T-MP5, D9 | Spec PR #204; code PR #214 |
| P4 | **Review of the repeated-drops threshold** (25 % in each of 3 minutes), from field data and a lab link that loses messages without breaking. | ch.28 RQ1 item 5 | Waits for P3a and field logs |
| P5 | **One resolver for every hostname.** A `dns_resolver` setting chooses Google Public DNS (the default), the system resolver or fixed servers, for seeds, peer names, coin servers and HTTP(S) services alike. In-memory cache, last known answer for up to 24 hours when the resolver is unreachable, no fallback. The transport library's DNS layer and the old resolver library leave the build. Sia, WalletConnect, Lightning and the statistics method follow in a later release (P5b). | ch.45 §45.6B, T45.16–T45.22, D45.6–D45.8; ch.28 R0b, R18, R8c item 7, D9, OQ2; pointers in ch.35, ch.38, ch.39 | Spec PR #216; code PR #219; lab row L-DNS (T45.22) passed 2026-11-05, cases 1 to 7 and 9 (release tests 0.4.0-beta.1, row 3; case 8 not exercised) |
| P5b | **The remaining lookups** under the same resolver: Sia, the WalletConnect relay, Lightning peer addresses, the statistics method. | ch.45 D45.6 | Not in 0.4.0; follows in a later release (CHANGELOG, Known limitations) |
| P5c | **Encrypted DNS** for the Google and fixed modes. | ch.45 D45.7 | Plan row only, no estimate |
| P6 | **Bounded connection close:** a 30 s deadline on every close, in the vendored yamux wrapper. | ch.28 R27c, RQ1 item 5, D8 | Implemented in batch 3: PR #280; lab row L-CLOSE passed (2026-11-07); its memory criterion was restated after the run, from "back to baseline" to "flat after the close, where the control keeps growing" |
| P8 | **Lab capture** confirming that GLEEC KDF relays send nothing on the peers topic. | ch.28, note after R8c | Lab only, any time |
| P9 | **Unreachable peer addresses.** After repeated refused or timed-out dials, an address is skipped as a peer-exchange target and as a maintenance candidate for a period that grows with each failure; a connected relay is preferred as the peer-exchange target; the dial address list carries each address once; the warning names the TCP cause instead of the transport library's negotiation wording. Reloaded-original. | a new ch.28 rule, to be written | Planning, after 0.4.0-beta.1 (CHANGELOG, Known limitations) |

Item P7 (asynchronous hostname lookups) was dropped from 0.4.0 on 2026-11-02. P5 brings the
lookups of R8c item 7 onto the shared resolver, and they become asynchronous with it.

## Decided

- **Skip logging (P2).** DEBUG for each skipped address, plus one INFO summary per hour with
  counts per form. Confirmed on 2026-11-02.
- **No exception to the 5-minute dial guard (P1).** It matches GLEEC KDF. A connection that
  lasted less than 5 minutes delays the redial of its address to the 5-minute mark, so a client
  whose only seed restarts soon after connecting waits up to about 5 minutes instead of 30 s.
- **Bounded close design (P6).** It uses the yamux deadline with no switch. On 2026-11-02 the
  maintainer chose the yamux deadline over a swarm-level one. It is implemented in batch 3 (CRD
  ch.28 R27c). The lab row L-CLOSE passed on 2026-11-07: toward a frozen peer the close ended after
  30 seconds, where a build without the deadline kept it open 13 minutes. Its memory criterion was
  restated after the run, from "back to baseline" to "flat after the close, where the control keeps
  growing" (T14). In the 10-second link cuts of the same row the relay started no close.
- **An outage without a reset (batch 4 item B4-8, 2026-11-08).** The drop-only arm of L-CLOSE
  dropped every packet between a relay and a peer without resetting a socket. In three 10-second
  drops both connections survived, and traffic resumed 2.5 to 5.5 seconds after the restore. After a
  30-second drop the connections were closed and the peer connected again; after a 60-second drop
  both connections were closed and came back. Traffic resumed 23 and 40 seconds after the restore.
  This is input for P4. The earlier redial of a connection that a cut ended was decided on
  2026-11-08 (ch.28 R27a item 1a) and implemented in PR #303 (batch 4 item B4-13; lab row L-RELINK
  passed 2026-11-08, recorded in that PR); PR #328 closed its remaining gap, so that a stuck-peer
  close ending with a transport error is no longer taken for a cut.
- **Resolver (P5), 2026-11-02.**
  - One setting, `dns_resolver`, for every hostname. Google Public DNS is the default for all
    names, `"google"` names it explicitly, and `"system"` is the way back to the operating
    system's resolver. There is no value for GLEEC KDF's mix of the two.
  - A mistyped value refuses the start. A failed lookup never stops the node, and no other
    resolver is ever asked.
  - The last known answer may be served for up to 24 hours when the resolver is unreachable.
  - The library is the maintained successor of the old one. The old one leaves the build in
    batch 1.
  - On 2026-11-03 the maintainer chose that successor's message crate (0.26), with our own
    queries, cache and hosts-file reading on top. Its resolver crate needs a cache crate with
    Apache-2.0-only files, and its older 0.24 line has an unfixed advisory (CRD ch.45 D45.8).
    P5 in batch 1 is about 7–8 days of code.
  - Batch 1 covers the peer-to-peer layer, HTTP(S), Electrum and lightwalletd; the rest was
    planned for batch 2 and follows in a later release (P5b).

## Open points

- **P4:** whether the threshold changes at all. Open after this beta; it waits for field data.
- **P9:** the hold-off periods and the failure count that starts them; open after this beta, with
  its ch.28 rule. In a 29-hour run of the desktop wallet on Windows, 557 dial warnings named 32
  addresses that other nodes hand out in the peer exchange and that accepted no connection.
- **P5b, P5c, P8:** open after this beta. P5b follows in a later release, P5c has no estimate,
  P8 is lab-only, as their rows say. No code of theirs is in 0.4.0.

## Verification

| Item | Unit tests | Lab |
|---|---|---|
| P1 | T12 | Ghost topology again, compared before and after with the same counting script. Expected: no maintenance dial to a seed whose own connection is open, and at most one dial per 5 minutes to an unreachable address. |
| P2 | R8c and R24 cases | Public netid-8762 probe at TRACE for at least 25 minutes. |
| P3 | Trickle test for the clear rule; window-share cases | Optional: a maker with coalescing on whose relay carries steady traffic. |
| P4 | — | A link that loses at least 25 % of messages per minute without breaking for 180 s or more. |
| P5 | T45.16–T45.21 | T45.22: a lab DNS server with seven cases, part of the batch-1 lab run. |
| P6 | Close over a writer that never takes data | A peer process stopped while its queue is at the cap and the TCP path to it is full; 10-second link cuts. Passed 2026-11-07; the memory criterion was restated after the run (T14). |
