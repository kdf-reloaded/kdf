# Plan: check the computer's clock against NTP servers at start and while running

**Status:** planning (post-beta; ordered 2026-11-14; implemented in draft PR #348, not merged; D1 and D2 decided) — **Last reviewed:** 2026-11-14

> **Context.** Swaps depend on the computer's clock: lock times, the deadlines of each swap step and the
> started-at comparison of the version-2 swaps are all wall-clock times. Peers already check each other's clocks
> (CRD ch.28 RP2: a peer more than 20 s away is disconnected), but that check needs peers and does not tell the
> operator that the fault is on this computer. This plan adds a check of the node's own clock against public time
> servers: at start, a clock more than 10 s off stops the node with an error that names the measured difference
> and says that swaps in progress wait until the clock is right; while running, it stops new orders and swaps
> until the clock is right again. It is a KDF Reloaded addition, written from
> the maintainer's order of 2026-11-14 and our own design. It changes nothing on the wire; at run time it
> never touches a running swap. The binding rules are CRD ch.45 §45.6C (R45.6C.1–R45.6C.11, T45.23–T45.31). Work starts after
> the 0.4.0 beta; the PR stays a draft until the beta is released.

## Rules (summary; the chapter binds)

1. **Servers.** The key `ntp_servers` lists NTP servers as `"host"` or `"host:port"` (port 123 by default; an
   IPv6 address in brackets). Absent or `null` selects the default list, `0.pool.ntp.org` to `3.pool.ntp.org`,
   kept in one constant (decision D2 below: on by default). `[]` turns the check off, and then `ntp_required` has no effect.
   An invalid value stops the node at start with a configuration error, like `dns_resolver` (R45.6B.2).
2. **One check.** The node asks every listed server at once over SNTP (RFC 4330), waits up to 5 s, computes the
   offset of the local clock from each valid answer, and takes the median of the offsets (with an even number
   of answers, the mean of the two middle ones). Host names are looked up through the node's resolver
   (R45.6B).
3. **The limit.** 10 s, a named constant, not a setting. A median offset above it fails the check.
4. **At start.** The check runs after the resolver is installed and before the wallet, the peer-to-peer layer
   and the swap resumption. A failed check stops the start (decision D1 below), so running swaps are not
   resumed until the clock is right. The error names the measured offset, the limit and the number of servers
   that answered, and says plainly that swaps in progress wait and that setting the clock right is urgent,
   because their deadlines keep running.
5. **No answer.** With `ntp_required` `false` (the default), the node logs one WARN line and goes on. With
   `true`, the start stops as in rule 4.
6. **While running.** The node checks again every hour, and at once after a pause of the process or a jump of
   the clock: a 60 s tick compares the wall clock with the monotonic clock and its own lateness. A failed check
   (an offset above the limit, or no answer with `ntp_required` `true`) logs an ERROR line and refuses every new
   order and every swap start until a check passes; while refused, the node checks every 5 minutes. A passing
   check logs one INFO line that new swaps are allowed again.
7. **What a refusal covers.** The same places as the Electrum disagreement state (CRD ch.38 R38.6.4o item 12):
   `buy`, `sell`, `setprice` and the order creation of the market-maker bot, and the start of a matched swap on
   both sides. Withdrawals and running swaps are not affected.
8. **Browser builds.** A browser cannot send UDP, so the check is off there, as if the list were empty. The
   HTTP `Date` header is no clean substitute: a browser lets a page read it only from a server that allows it
   by a CORS header, and its resolution is one second.
9. **Logs.** Servers are named by host and port only.

## Settled points (2026-11-14)

| # | Point | Settled |
|---|---|---|
| O1 | Key names | `ntp_servers` (list) and `ntp_required` (boolean). |
| O2 | Does a refusal also refuse new orders, or only swap starts? | Both, at the same places as the Electrum disagreement state; withdrawals untouched. |
| O3 | How many answers does a check need? | One: the median of whatever answered. |
| O5 | Browser builds | Off, as if the list were empty; see rule 8. |
| O6 | An invalid list entry | Stops the start with a configuration error, as for `dns_resolver`. |
| O7 | Check cadence | Hourly; at once after a pause or a clock jump; every 5 minutes while refused. |
| O8 | What the default list discloses | The configuration documentation says that the NTP pool learns the node's address about once an hour. |

## Decisions (the maintainer's, 2026-11-14)

| # | Decision | Options | Decided |
|---|---|---|---|
| D1 | A clock more than 10 s off at start (and no answer with `ntp_required` `true`) | (a) the start stops, so running swaps are not resumed until the clock is right; (b) a restricted start: running swaps are resumed, new orders and swap starts are refused until a check passes | **(a).** With a clock known to be wrong, the node takes no swap decision at all. The error says that swaps in progress wait and that fixing the clock is urgent. |
| D2 | The default list | the four `pool.ntp.org` hosts, or empty (the check off unless configured) | **The four pool hosts:** the check is on by default. |

## Open points

| # | Point | Options | Proposal |
|---|---|---|---|
| O9 | Should the reasoning of D1 (no swap decision with a clock known to be wrong) also apply while the node runs? Today a run-time failure stops only new orders and swap starts; running swaps go on with the wrong clock. | (i) Stop the node when a run-time check fails, with the error of the start: running swaps then wait for a start with a correct clock, exactly as at start. Small to build: the run-time loop asks the node to stop. (ii) Hold running swaps in place: every swap engine (the legacy and the version-2 state machines) waits before each step that sends a transaction or acts on a deadline until a check passes. Faithful to D1 without a stop, but it touches every swap state machine and its timers, and needs its own CRD rules and tests. (iii) Keep today's run-time rule. In (i) and (ii) a swap held past a deadline can end in a refund rather than a completed trade, as at start. | Keep (iii) for this PR, as ordered. Decide between (i) and (ii) once field runs show whether run-time failures happen at all; a drift of 10 s within an hour is not expected on a computer that synchronizes its clock, and a computer that slept is caught by the jump detection. |
| O10 | A server given by a name with both IPv4 and IPv6 addresses is asked at the first address the lookup returns (R45.6C.4 item 1). On a computer without IPv6, such a server never answers when the IPv6 address comes first; with `ntp_required` `true` that refuses the start. | (i) Try the next address within the same 5 s; (ii) prefer IPv4 addresses. (iii) Keep the rule. | (i), after this PR: it costs a loop over the addresses and no new setting. In the Google and fixed resolver modes, a name with no family given resolves to its IPv4 addresses when it has any (R45.6B.3 item 1); with `"system"` the order is the operating system's. |

## Work packages

| # | Package | Where |
|---|---|---|
| 1 | Chapter rules and tests (§45.6C), this plan, the CHANGELOG entry, `docs/NETWORK_CONFIG.md`, `RELOADED_VS_GLEEC.md`, `docs/GLEEC_COMPATIBILITY.md` | docs |
| 2 | Configuration parsing, SNTP client behind a small time-source interface, median and limit | `mm2_main` |
| 3 | Start-up check in `lp_init`; run-time loop; the refusal state and its gate at the order and swap-start places | `mm2_main` |
| 4 | Unit tests with a stub time source; no real network | `mm2_main` |

## Test list

T45.23–T45.31 in the chapter: configuration parsing; SNTP packet handling; within the limit; above the limit
stops the start; no answer with `ntp_required` `false` warns; with `true` stops the start; an empty list sends
no query; the median with one outlier; a run-time failure refuses new orders and swap starts but not
withdrawals, and a later pass lifts the refusal; pause and clock-jump detection triggers a check.
