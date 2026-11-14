# Plan: check the computer's clock against NTP servers at start and while running

**Status:** planning (post-beta; ordered 2026-11-14; nothing is implemented) — **Last reviewed:** 2026-11-14

> **Context.** Swaps depend on the computer's clock: lock times, the deadlines of each swap step and the
> started-at comparison of the version-2 swaps are all wall-clock times. Peers already check each other's clocks
> (CRD ch.28 RP2: a peer more than 20 s away is disconnected), but that check needs peers and does not tell the
> operator that the fault is on this computer. This plan adds a check of the node's own clock against public time
> servers: at start, a clock more than 10 s off stops the node with an error that names the measured difference;
> while running, it stops new swaps until the clock is right again. It is a KDF Reloaded addition, written from
> the maintainer's order of 2026-11-14 and our own design. It changes nothing on the wire and never touches a
> running swap. The binding rules are CRD ch.45 §45.6C (R45.6C.1–R45.6C.11, T45.23–T45.31). Work starts after
> the 0.4.0 beta; the PR stays a draft until the beta is released.

## Rules (summary; the chapter binds)

1. **Servers.** The key `ntp_servers` lists NTP servers as `"host"` or `"host:port"` (port 123 by default; an
   IPv6 address in brackets). Absent or `null` selects the default list, `0.pool.ntp.org` to `3.pool.ntp.org`,
   kept in one constant (decision D2 below). `[]` turns the check off, and then `ntp_required` has no effect.
   An invalid value stops the node at start with a configuration error, like `dns_resolver` (R45.6B.2).
2. **One check.** The node asks every listed server at once over SNTP (RFC 4330), waits up to 5 s, computes the
   offset of the local clock from each valid answer, and takes the median of the offsets (with an even number
   of answers, the mean of the two middle ones). Host names are looked up through the node's resolver
   (R45.6B).
3. **The limit.** 10 s, a named constant, not a setting. A median offset above it fails the check.
4. **At start.** The check runs after the resolver is installed and before the wallet, the peer-to-peer layer
   and the swap resumption. What a failed check does is decision D1 below: either the start stops, or the node
   starts with new orders and swap starts refused and running swaps resumed. Either way the error names the
   measured offset, the limit and the number of servers that answered. The code keeps the two outcomes behind
   one decision point and selects the first until D1 is decided.
5. **No answer.** With `ntp_required` `false` (the default), the node logs one WARN line and goes on. With
   `true`, the outcome of rule 4 applies.
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
9. **Logs.** Servers are named by host only.

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

## Decisions pending (the maintainer's)

| # | Decision | Options | Proposal |
|---|---|---|---|
| D1 | A clock more than 10 s off at start (and no answer with `ntp_required` `true`) | (a) the start stops, so running swaps are not resumed until the clock is right; (b) a restricted start: running swaps are resumed, new orders and swap starts are refused until a check passes | Put to the maintainer with both sides: (a) is the order of 2026-11-14 as given; (b) keeps a running swap's spend or refund going, which the chain judges, not the local clock. The code selects (a) until decided. |
| D2 | The default list | the four `pool.ntp.org` hosts, or empty (the check off unless configured) | Put to the maintainer; one constant either way. |

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
