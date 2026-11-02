# Plan: peer-to-peer dialing, discovery and send-queue follow-ups (0.4.0)

**Status:** in progress — **Last reviewed:** 2026-11-02

> **Context.** These are the peer-to-peer items deferred from the 0.3.0 beta and the field
> findings made around it. The maintainer approved batch 1 (P1 to P3) on 2026-11-02; the other
> items wait on the decisions listed under open points. All of the work is Reloaded-original,
> except where an item says it aligns with GLEEC KDF. In those places the reference facts are
> already in CRD ch.28, cleared by the KDF Dirty Gate, and no new reading of the reference is
> needed.

## Work packages

| Id | What | CRD | State |
|---|---|---|---|
| P1 | **Redial guards.** No address is dialed again within 5 minutes, as GLEEC KDF does. No configured seed is dialed while its peer is connected, which ends a dial-and-close loop every 150 s. The short-of-relays warning appears at most once per 5 minutes. | ch.28 R27a, T12 | Spec PR #199; code PR #206; lab check L-SEED pending |
| P2 | **Peer-exchange polish.** Skipped addresses are logged at DEBUG, with an hourly INFO summary. The idle close of a relay outside the relay mesh logs at INFO. The routability ranges are aligned with GLEEC KDF. Two wording fixes (R8c, R23 item 3). | ch.28 R8c items 4 and 7, R23 item 3, R24, R27b, T6a, T13, D9; ch.32 note under R-F3b | Spec PR #203; code PR #211; public probe pending |
| P3a | **Send-queue windows visible.** Each connection's dropped share per 60 s window, as a DEBUG line and a gauge. | ch.28 RQ1 items 6 and 8, T-RQ1.13; ch.32 R-MP5, T-MP10 | Spec PR #204; code PR #214 |
| P3b | **Queue-signal clear rule.** The signal clears when nothing has waited 2 s or more, not only after a 5 s empty queue. It affects only makers with `maker_update_coalescing` on (see `maker-gossip-rate-control.md`). | ch.32 R-MP2 item 2, T-MP5, D9 | Spec PR #204; code PR #214 |
| P4 | **Review of the repeated-drops threshold** (25 % in each of 3 minutes), from field data and a lab link that loses messages without breaking. | ch.28 RQ1 item 5 | Waits for P3a and field logs |
| P5 | **Resolver for seed hostnames:** the fixed public resolver or the system's resolver. | ch.28 D9 | Design only; waits for a maintainer decision |
| P6 | **Bounded connection close:** a 30 s deadline on every close, in the vendored yamux wrapper. | ch.28 RQ1 item 5, D8 | Design only; waits for a maintainer decision |
| P8 | **Lab capture** confirming that GLEEC KDF relays send nothing on the peers topic. | ch.28, note after R8c | Lab only, any time |

Item P7 (asynchronous hostname lookups) was dropped from 0.4.0 on 2026-11-02. The bounded
blocking lookup of R8c item 7 stays.

## Decided

- **Skip logging (P2).** DEBUG for each skipped address, plus one INFO summary per hour with
  counts per form. Confirmed on 2026-11-02.
- **No exception to the 5-minute dial guard (P1).** It matches GLEEC KDF. A connection that
  lasted less than 5 minutes delays the redial of its address to the 5-minute mark, so a client
  whose only seed restarts soon after connecting waits up to about 5 minutes instead of 30 s.
- **Bounded close design (P6).** If it is implemented, it uses the yamux deadline with no switch.

## Open points

- **P5:** which resolver, and the default. The evaluation is held with the maintainer.
- **P6:** implement in 0.4.0, or keep as a design.
- **P4:** whether the threshold changes at all.

## Verification

| Item | Unit tests | Lab |
|---|---|---|
| P1 | T12 | Ghost topology again, compared before and after with the same counting script. Expected: no maintenance dial to a seed whose own connection is open, and at most one dial per 5 minutes to an unreachable address. |
| P2 | R8c and R24 cases | Public netid-8762 probe at TRACE for at least 25 minutes. |
| P3 | Trickle test for the clear rule; window-share cases | Optional: a maker with coalescing on whose relay carries steady traffic. |
| P4 | — | A link that loses at least 25 % of messages per minute without breaking for 180 s or more. |
| P6 | Close over a writer that never takes data | A peer process stopped while its queue is at the cap. |
