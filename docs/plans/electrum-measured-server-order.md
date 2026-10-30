# Plan: measured Electrum server order

**Status:** in progress (shipped in PR #144; the RIN field run and the `mixed`-default decision left) — **Last reviewed:** 2026-10-29

**CRD:** ch.38 R38.6.4l (Reloaded-original), with T38.6.4l.1–T38.6.4l.15 and
the decision items D38.6.4l.1–D38.6.4l.9, all resolved by 2026-10-29.

> **Context.** The Electrum connection pool (CRD ch.38 §38.6.4) ranks a coin's
> servers by an order drawn once at activation: R38.6.4b-R, with
> `electrum_server_order` set to `random`, the default. This plan adds two
> further values of that switch:
> - `mixed`: measured quality and chance, 50:50. It is intended to become the
>   default.
> - `measured`: measured quality only, opt-in.
>
> Everything here is a Reloaded-original design with no wire effect: it
> changes only which of the operator's own configured servers a node uses.

## Goals

1. Spread users across the listed servers. Every wallet ships the same coins
   file, so a fixed order sends everyone to the same first server.
2. Prefer servers that are responsive and up to date.
3. Do this without adding measurable load to the public Electrum servers, and
   without revealing the user's addresses to servers the node does not use.

## Values of `electrum_server_order`

| Value | Order | Status |
|---|---|---|
| `listed` | The order of the activation request, as GLEEC KDF. | in the pool PR |
| `random` | A uniform random draw once at activation (R38.6.4b-R). | in the pool PR, current default |
| `mixed` | A weighted draw among eligible servers: 50 % by measured quality, 50 % by chance (below). | this plan; intended default |
| `measured` | Eligible servers ordered by measured quality alone; the random draw only breaks ties. | this plan; opt-in |

An absent switch, or an unrecognized value, means the default and logs one
warning naming the value.

`measured` is opt-in on purpose. If every node in one region ranks purely by
speed, all of them pick the same fastest server, overload it, and then move
together to the next one. That herding is the problem the shuffle solves.
`mixed` keeps a bias toward good servers while still spreading users.

## 1. What is measured, and when

| When | Servers | Measurement | Extra cost |
|---|---|---|---|
| Activation probe (R38.6.4k-R) | every listed server, already connected in parallel by the probe | the round trip of one `blockchain.headers.subscribe` sent right after version negotiation; its answer, the tip header, also gives the server's height | one small request per server, over a connection the probe holds anyway |
| While running | maintained connections only | the round trip of every ordinary request and of every 30 s `server.ping`, smoothed as an exponentially weighted moving average (α = 0.2); the tip height from header notifications | none |
| Re-measure (after a "poor" trigger, §3) | non-maintained candidates that are not suspended | the same as at activation: connect, negotiate, one header request, release | one connection per candidate, at most once per 5 minutes per coin |

**Why a header request and not a network ping.**
- It goes through the server's own request handling and index, so it measures
  the server, not only the route.
- It returns the tip, which is the staleness signal.
- It is cheap, and it carries no user data.

**What is never used for measuring.**
- **Address-history or balance queries.** They would reveal the user's
  addresses to every listed server and load their indexers.
- **A fee estimate,** which reaches the coin daemon behind the server. It may
  later be offered as an activation-only option, but it is off by default and
  not part of this plan.

## 2. Eligibility, quality and the draw

- **Best tip:** the highest height any server reported in the current
  measurement round.
- **Best latency:** the lowest measured round trip in that round.
- **Eligible:** a server is eligible when all of these hold:
  - it is not suspended or removed (R38.6.4d);
  - its height is at most 1 block behind the best tip;
  - its latency is at most max(4 × the best latency, 300 ms). The 300 ms
    floor stops differences between fast servers from excluding any of them.
- **Quality:** q = best latency / latency, so the fastest eligible server has
  q = 1.
- **`mixed` draw:** a weighted random order over the eligible servers, without
  replacement, with weight w = 0.5 · q / Σq + 0.5 · 1 / n, where n is the
  number of eligible servers.
- **`measured` order:** eligible servers by descending q. Equal q is ordered
  by the random draw.
- **Non-eligible servers** follow in random order. They stay reachable for the
  retry over every server (R38.6.4f item 2) and as floor fillers.

The result replaces the drawn order of R38.6.4b-R as the rank. Maintenance,
displacement, routing and the activation probe keep working by rank.

**Example.** Three eligible servers answering in 50, 100 and 150 ms:
- quality q = 1, 0.5 and 0.33;
- `mixed` weights: about 0.44, 0.30 and 0.26.

So the fastest server comes first for about 44 % of nodes, not for all of
them. Latency mostly reflects distance, so nodes in different regions favor
different servers, which spreads load further.

## 3. A "poor" connection and how it is replaced

A maintained connection is **poor** when one of these holds:

- **P1 stale, two or more maintained servers:** on two consecutive header
  notifications, its height is two or more blocks behind the best maintained
  height.
- **P1 stale, one maintained server:** no new header for longer than
  max(3 × the coin's block interval, 10 minutes). There is no peer to compare
  against, so this triggers a re-measure, which reveals the other servers'
  tips. Without a configured block interval, 10 minutes applies.
- **P2 slow:** after at least 5 samples, the smoothed latency exceeds all
  three of:
  - 4 × the server's own activation latency;
  - 2 × the best activation latency in the pool;
  - 1 s.
- **P3 failing:** already handled by the pool. A failure closes the connection
  and suspends the server (R38.6.4d, R38.6.4f item 1).

**On P1 or P2:**
1. The server is marked **poor-demoted** for 15 minutes. It still works, so it
   is not suspended, but it is not eligible during that time.
2. The candidates are re-measured (§1). This happens at most once per
   5 minutes per coin.
3. A replacement for that one slot is drawn from the eligible candidates: with
   the §2 weights under `mixed`, by quality under `measured`. It joins by the
   join-and-demote rule of R38.6.4c. The rest of the order keeps its place.
4. If no candidate is eligible, the poor server stays: a degraded server is
   better than none. The trigger stays armed.

## 4. Deliberately not done

- **No periodic probe of every server.** Across every node, every enabled coin
  and every listed server, that adds constant load to volunteer-run public
  servers.
- **No measuring through address queries** (privacy, §1).
- **No full reshuffle after one bad sample.** Stability comes from:
  - the 4× / 2× / 1 s thresholds;
  - the 5-sample minimum;
  - the 15-minute cooldown;
  - the 5-minute re-measure limit.

## Work packages

1. **CRD before code.** Write ch.38 R38.6.4l (Reloaded-original). It covers
   §1–§3 above, the two new values of `electrum_server_order`, and an
   amendment of R38.6.4b-R item 3 (the values stop being reserved). Add test
   items T38.6.4l.1 onward, and add the divergence note: no wire effect,
   arm 2 of the default-side rule, GLEEC value `listed`.
2. **Stub server harness.** Extend `electrum_stub_server.rs` with:
   - a per-method reply delay;
   - a settable tip height;
   - pushing header notifications with chosen heights.
3. **Implementation** in the pool:
   - the scores, eligibility and the two orders;
   - the smoothed latency per maintained connection;
   - the P1 and P2 triggers with the cooldown and the re-measure limit;
   - the single-slot replacement;
   - the switch values.
4. **Tests**, deterministic, with millisecond timings and injected RNG:
   1. the eligibility filters: stale, slow, suspended;
   2. the `mixed` weights for the example above;
   3. the distribution of first places over 2,000 draws, within bounds;
   4. P1 with two servers, and P1 with one server through the block-interval
      timer;
   5. P2, with the smoothing and the 5-sample minimum;
   6. the cooldown;
   7. the re-measure rate limit;
   8. single-slot replacement;
   9. the poor server stays when no candidate is eligible;
   10. no address or balance query is ever sent while measuring;
   11. `measured` orders by quality, with ties broken by the draw;
   12. switch parsing for all four values.
5. **Docs.**
   - CHANGELOG in plain words;
   - the `GLEEC_COMPATIBILITY.md` row for `electrum_server_order` updated;
   - the default changed to `mixed` only once the tests are green, as a
     separate, visible line.

## Open points

- **Block interval per coin.** Check what the UTXO coin configurations carry;
  a coin without one uses the 10-minute fallback for P1.
- **When `mixed` becomes the default.** Decided 2026-10-28: after a field run
  on RIN with `mixed`, as a separate change (CRD D38.6.4l.1).
- **The CRD's reading of the plan.** Decided 2026-10-29: the rules behind
  D38.6.4l.2, .3 and .5 to .9 are kept as written.
