# Plan: Electrum resilience against stale and bad servers

**Status:** in progress (decisions D1–D7 taken 2026-11-09; one PR for all items) — **Last reviewed:** 2026-11-09

**CRD:** ch.38 R38.6.4a item 6 and R38.6.4l items 4, 6 and 7 (amended), new rules for the
confirmation wait, the broadcast route, header agreement and the server-state report
(Reloaded-original). Builds on [the measured server order](electrum-measured-server-order.md).

> **Context.** A coin's Electrum servers can fall behind the chain, stop following it, or follow
> another chain. Today a lagging server keeps its share of reads, a stuck server among two or
> more connections is never judged, the tolerance is a fixed block count whatever the block
> time, and the reference tip is the highest height any single server reports. Under the default
> order `random` no height is requested at all.
>
> This plan ranks its items by importance: security first, then stability, then speed. Every
> item is a Reloaded-original design with no wire effect: it changes only which of the
> operator's own configured servers a node uses for what.

## Examples the plan is measured against

**E1 — field report, 2026-11: PIVX, 2 listed servers, one more than 1000 blocks behind
(about 17 hours at 60 s blocks), no `min_connected`/`max_connected` given.**
Under the defaults (`electrum_server_order` `random`, `electrum_spread_reads` true,
`max_connected` = the 2 listed) both servers are maintained and both are near (floor of
two), so about half of every balance, unspent-list, history and transaction read comes from
the stale server, and a broadcast may be offered to it first. Under `random` no height is
ever requested, so nothing notices. Under `mixed` the stale server is ranked last, but it
still answers half of the reads, and a server that is *stuck* (sends no header
notification) is never judged by P1 while two or more connections are maintained: P1 counts
strikes only on the connection that notified, and the silence rule applies to a lone
connection only. With 2 listed servers there is also nothing to replace the stale one with.
What helps here: rank the good server first, keep every read and every swap confirmation
poll on it, warn, and show which server is stale.

**E2 — field report: DGB (15 s blocks) servers are routinely a few blocks apart.**
Blocks take seconds to propagate and an indexer under load lags tens of seconds, so two
healthy DGB servers often differ by 1–3 blocks. A fixed tolerance of 1 block marks healthy
servers non-eligible on fast chains, while on BTC (600 s) the same 1 block is ten minutes.

**E3 — RIN (60 s blocks), 3 listed servers, one stuck for over 3 hours (more than 200 blocks
behind).** A RIN payment received two hours ago is missing from every third balance and
history read; an order placed from a balance read on the stuck server can be refused or
oversold; a swap's search for the counterparty's payment fails on every third poll. Healthy
RIN servers differ by 1 block for some seconds after each block.

**E4 — a mainnet BTC list that by mistake contains a testnet server.**
The testnet tip height is far above mainnet's. Under the present "highest reported height
wins" rule, every mainnet server is more than a million blocks "behind" that one server:
all honest servers are non-eligible and the wrong server is ranked first. No malice is
needed for a single server to steer the node; a lying server gets the same effect on
purpose.

**E5 — a BCH server in a BTC list (or any fork of a well-known chain).**
Heights are close, so no height rule notices; at the same height the two servers report
different header bytes. Reads from the fork server show a different chain's balances.

## Steps

Work order inside the one PR: (1 + 3 + 4) first, as the fix for E1; then 5; 2; 7; 8; 9;
6 last. The CRD amendment comes before the code of each step.

### 1. Agreed tip, stuck-server rule, no single-server veto (security)

Today the reference tip is the plain maximum of every reported height (E4).

- *Agreed tip*: with 3 or more reported heights, the highest height that at least two
  distinct servers have reached; with exactly 2, the higher of the two (a stale honest
  server is far more common than a lying one, and with 2 servers the choice is binary
  either way); with 1, no height comparison.
- *Lag* = agreed tip − own height. Eligible iff lag ≤ T (step 3). P1 "behind" iff lag > T
  on two consecutive own notifications.
- *Stuck*: a maintained connection that received no header notification for longer than
  P = max(3 × `avg_blocktime`, 10 min) while another maintained connection did receive one
  is poor ("stuck"). It trusts no claimed height and parses nothing, and it generalizes the
  lone-connection silence timer. When no connection received one within P, nothing is stuck
  (chain halt). E1: the stuck PIVX server is marked within 10 minutes; E3 likewise.
- A height whose header does not parse under the coin's header family (ch.37) counts as no
  measurement.

Considered and rejected: judging staleness by the tip header's timestamp. It needs
per-family header parsing, says nothing on proof-of-stake chains (PIVX), and trusts the
local clock; the stuck rule gives the same verdict without any of that, at most P later.

Tests: E4 with 3 servers (honest servers stay eligible); E1 (the higher of two wins); a
stuck server among 3 (marked within P); chain halt (none marked); a single height (no
exclusion).

### 2. Swap confirmation polls stick to one server (security)

`wait_for_confirmations` trusts the `confirmations` field of whichever server answers;
under spreading each poll may reach another server, and the first "confirmed" answer ends
the wait, so the weakest of N servers decides. The wait picks one session at its start (the
highest-ranked near maintained connection) and polls it for the whole wait; on failure it
moves to the next and sticks again. Priority routing (`electrum_spread_reads` false) is
unchanged. No extra load. E1: every confirmation poll goes to the up-to-date PIVX server.

Tests: stub A answers 3 confirmations, stub B answers 0; the wait does not end on A's
answer while B is the chosen session; failover sticks.

### 3. Tolerance T from `avg_blocktime` (stability)

Observed failures are hundreds of blocks (E1); natural spread between healthy servers is a
few blocks on fast chains (E2). A tolerance generous in time therefore catches every real
case without false marks.

W = 180 s; T = clamp(ceil(W / `avg_blocktime`), 1, 20). Without `avg_blocktime`, or with
one that is not a positive number, T = 1. The block cap guards against nonsense in a coins
file; it is not a safety feature, because what harms a swap is time behind, not blocks
behind. A cap of 5–10 would let UFO (1.5 s) and CHIPS (10 s) tolerate only 15–100 s of
spread, below their natural spread, while every coin with a block time of 18 s or more is
unaffected by any cap between 10 and 20.

| coin | `avg_blocktime` | T |
|---|---|---|
| BTC | 600 | 1 |
| LTC | 150 | 2 |
| KMD, RIN, PIVX | 60 | 3 |
| DGB | 15 | 12 |
| CHIPS | 10 | 18 |
| UFO | 1.5 | 20 |

T is the eligibility limit of a measurement round and the "behind" limit of P1 on two
consecutive notifications. E3: a RIN server 3 blocks behind is still eligible; 4 or more
behind on two notifications is "behind". One derived figure replaces the two fixed
constants (1 block eligible, 2 blocks behind). Of 172 UTXO entries in the coins file, 170
carry `avg_blocktime` (1.5 s UFO … 600 s BTC).

Tests: the table; absent, zero, negative and string values → 1; the cap.

### 4. Stale connections leave the read rotation at once (stability)

"Near" is latency-only today; a fast but stale server keeps its share of reads until it is
replaced, and with 2 listed servers it is never replaced (E1).

A maintained connection with lag > T, or stuck, is *not near*: no read-only query goes to
it while another near connection exists. It stays maintained and a fallback. This takes
precedence over the floor of two: the floor applies among up-to-date connections, so in E1
every read goes to the one good server and in E3 to the two good ones. Heights are needed
under every order, so the header request on join is sent under `random` and `listed` too
(one request per joined connection; no triggers or replacements under those orders).

Degrading early for reads is cheap and reversible: the connection is near again as soon as
it is within T. Demotion in the rank still needs the two-notification rule or the stuck
rule.

Tests: E3 with stubs (reads rotate over the two good servers; the third returns when it
catches up); E1 with stubs (all reads on the good server).

### 5. Observability (stability; prerequisite for the field run)

Poor marks are `debug!` only today, and nothing shows a server's height.
- WARN once per server per state change, host-only label: `<ticker>: Electrum <host> is
  <N> blocks behind (tip <h>), not used for reads` / `... up to date again` /
  `... replaced by <host>` / `... stuck: no new block for <P>`.
- Read-only RPC `electrum_servers {coin}` → per server: host-only label, listed rank,
  maintained, near, height, lag, smoothed latency in ms, suspended/poor until, removed.

Deterministic with stubs. A GUI can show "server X is 1000 blocks behind" instead of a
wrong balance; an operator sees which server to fix or to drop from the coins file.

### 6. `mixed` becomes the default (stability)

Lab rows N4/N5 (2026-11-06) were informative only. Pass rule: 72 h with steps 1, 3, 4 and 5
in, on RIN, on DGB (natural spread, 4 or more servers) and on one 600 s chain, plus an E1
regression row (2 servers, one stale): (a) every replacement has a logged reason; (b) no
false stale: a demoted or not-near server was never within T within 60 s of the mark;
(c) loop and swap times within 10 % of `random`; (d) E1 row: zero reads reach the stale
server after the activation probe. The default changes in the last commit of the PR, after
the run passes (CHANGELOG line, `GLEEC_COMPATIBILITY.md` row).

### 7. Re-measure backoff (stability, server load)

A chain halt, or a lone connection on a halted chain, starts a round every re-measure
limit (5 min) on every node. After a round with no eligible replacement, the next round
waits twice as long (5 → 10 → 20 → … cap 60 min); the wait resets when the agreed tip
advances. Rounds connect at most 4 servers at a time.

### 8. Header agreement at equal height (security and stability, no extra requests)

Two servers at the same height with different header bytes are on different chains (E5:
fork, wrong network, misconfigured server). At the agreed tip height, a server whose header
differs from the one at least two others report is non-eligible that round. Not a
suspension; a natural tip reorg resolves itself in the next round. Fewer than two agreeing
servers → nothing happens.

### 9. Broadcast avoids stale connections (speed and stability)

Broadcasts follow the priority order. A stale server rejects a transaction ("missing
inputs"), which costs a retry, or accepts it and never relays it, which is silent. A
broadcast skips maintained connections that are not near (step 4) when another is
available; the retry over every server stays.

## Deliberately not in the plan

- No scheduled probing of unused servers (load on volunteer-run public servers). With the
  default `max_connected` every listed server is maintained and notifies, so every server's
  height is known continuously anyway; the gap exists only when `max_connected` is set
  below the list length, and then a trigger (steps 1 and 7) re-measures the unused servers.
- No persistence of measurements; no hard refusal of every server (a degraded server beats
  none); no new per-coin settings beyond `avg_blocktime`; no SPV coupling in this series;
  `measured` stays opt-in (herding).
- No tip-timestamp check (step 1).
- No corroboration of a "confirmed" answer by a second server (D3); a candidate for a later
  change for coins without `spv_conf`.

## Decisions (taken 2026-11-09, maintainer)

- D1a window W: 180 s.
- D1b block cap: 20.
- D2 agreed tip: the highest height reached by at least two servers when 3 or more report;
  the higher of the two when 2 report.
- D3 confirmation polls: sticky only; no corroboration now.
- D4 header request on join under every order: yes.
- D5 observability: WARN lines and the `electrum_servers` RPC.
- D6 field-run pass rule for the `mixed` default: as step 6, including the DGB and E1 rows.
- D7 stuck period P: max(3 × `avg_blocktime`, 10 min).
- Delivery: one PR for every step, in the work order above.
