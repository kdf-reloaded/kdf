# Plan: ARRR swap questions from `lightwalletd` blocks, without an address index

**Status:** planning (target: after the beta) — **Last reviewed:** 2026-10-28

> **Context.** The maintainer wants KDF Reloaded to answer every ARRR swap question
> without Electrum. An ARRR swap asks three questions about its HTLC, a transparent
> P2SH output: *was my payment already sent*, *was this payment spent, and by which
> transaction* (the one-shot search of recovery), and *has the counterparty spent it
> yet* (the taker's polling wait, its only way to the secret). Today they are asked of
> the address index of the node behind each `lightwalletd` server (CRD ch.39 R39.11.4d),
> and of the activation's `electrum_servers` when no `lightwalletd` endpoint can answer
> (R39.11.25, plan [`arrr-htlc-electrum-fallback.md`](arrr-htlc-electrum-fallback.md)).
> Nodes without that index are common in the published server list (R39.11.4h, R39.11.4i).
>
> This plan describes a path that needs no address index: walk the swap's block range
> with the compact blocks `lightwalletd` already serves, fetch each listed transaction
> and match it against the HTLC. That plan's "detecting spends by reading blocks" was
> rejected for the beta; this plan takes it up again for after the beta, with
> measurements.
>
> **Decided (maintainer, 2026-10-28):** the Electrum fallback stays for the beta, and
> this plan's work comes after the beta. Its last step retires the fallback. Nothing
> else here is decided.

## Summary

- **Already sent** needs no block walk. The payment spends this wallet's own notes, so
  the wallet's own scan already lists it. Asking a server costs zero to three
  transaction lookups.
- **Spent / spend watch** needs the walk. It costs about **0.19 transaction lookups
  per block** on ARRR mainnet: a whole taker watch is 25 lookups on average and 67 at
  most. The block lists for a live swap are already in the wallet's compact-block
  cache, so they cost no extra requests.
- **The walk removes the address-index dependency, not the transaction-index one.** A
  mined ARRR transaction without a transparent output can only be fetched from a node
  that keeps its transaction index (R39.11.4b, R39.11.4c, D39.11.g). The nodes that
  lacked the address index in October did keep a transaction index (R39.11.4i,
  amendment), so the walk works on exactly the servers that fail today.
- **A spend is seen only once it is mined**, about one block later than through
  Electrum on average: about 65 s at the nominal 60 s block time. This does not affect
  swap safety. An optional mempool step removes the delay.
- **Effort:** about 8–13 working days of implementation, plus live-chain waiting,
  after the beta. The last step retires the Electrum fallback.

## How the walk works

1. **Block list.** For each height in the window, the list of transactions comes from
   the compact block: `CompactTx.hash` (internal byte order) and `CompactTx.index`, its
   position in the block. Inside the wallet's sync range, the blocks are already in the
   wallet's compact-block cache. That cache is hash-chain validated (R39.8.0n), and the
   wallet's rewind clears it above a reorg (R39.8.0al), so it costs no request. Outside
   that range (recovery of a swap older than the wallet), the blocks are fetched with
   `GetBlockRange`, one streamed request per 1,000 blocks, and validated the same way.
2. **Transaction.** Each listed transaction is fetched once with `GetTransaction`, hash
   selector, internal byte order (R39.11.4c), and read with the Zcash reader
   (R39.11.16a). The returned bytes must hash to the requested identifier.
3. **Match.** A transaction that **spends** the HTLC has an input whose previous output
   is the HTLC output (funding txid, output 0). A transaction that **funds** it has an
   output whose script is exactly the HTLC's P2SH script. These are the rules
   R39.11.4d(c) already applies to rows the index returns.
4. **Result.** The first matching spend is the answer, and the secret is read from its
   input script and checked against the secret hash. "Not spent" needs every listed
   transaction in the window to have been fetched and read, up to a tip read at the
   start of the question. A failed lookup leaves the question undetermined, as a
   failed read does today (R39.11.4b(f)).

**Why the compact block lists every possible spend.** Compact blocks list only
transactions with Sapling parts. Measured on mainnet, the coinbase never appears, and
127 of the 1,059 blocks that carry transactions have gaps in `CompactTx.index`. Those
gaps are transparent-only transactions such as notarizations. A spend cannot be one of
them. By the chain's consensus rule recorded under R39.11.4d, a non-coinbase
transaction may have a transparent output only when it has no transparent input. A
spend has a transparent input (the HTLC), so it pays only shielded outputs. Until the
Ironwood pool is in use those are Sapling outputs, so the spend is listed. Whether a
transaction paying **only** Ironwood outputs is listed is not yet measured
(step 0(d)). Until it is, a negative answer from the walk is sound only for spends
that pay Sapling outputs.

## The three questions

| Question (rule) | Source in this plan | Window | Lookups |
|---|---|---|---|
| Already sent (R39.11.18) | own-send record (R39.11.26), then the wallet's own scan, then mempool nullifiers | swap start block (taker: its fee block) to tip | 0 with a record; else one `GetTransaction` per own send in the window (typically 1–3) and one `GetMempoolTx` stream |
| Spent, one-shot (R39.11.22) | block walk | funding's mined height (R39.11.4h(a)) to tip | 0.19 per block; stops at the first spend |
| Spend watch (R39.11.21) | block walk, incremental | funding's mined height to the taker payment lock | per poll: one tip read plus 0.19 per new block; most 10-second polls see no new block |

**Already sent, in detail.** The payment is funded from this wallet's notes, so the
wallet's scan marks it as the wallet's own transaction through its Sapling nullifiers.
Only those transactions inside the window need fetching to look for the HTLC script.
The funding transaction keeps its unspent P2SH output until the swap moves on, so a
node can serve it even without its transaction index. "Not sent" needs all three:

- the wallet is scanned through the tip read at the start of the question;
- no own transaction in the window pays the HTLC script;
- no mempool transaction spends one of the wallet's notes. The nullifiers in
  `GetMempoolTx` show this, and it covers a payment broadcast just before a crash,
  before its record was written.

Otherwise the answer is undetermined. R39.11.27 already skips this question for a
fresh swap's shielded payment, so it reaches this path only for restored swaps and
recovery.

## Measured block fill

ARRR mainnet, heights 4,154,530–4,161,184 (6,655 blocks, 111 hours up to the Ironwood
activation, average block time 60.2 s), counted from a Reloaded compact-block cache:

| Measure | Value |
|---|---|
| Compact transactions per block | mean 0.19, median 0, p95 1, p99 2, max 11 |
| Blocks with no compact transaction | 84.1 % |
| 130 blocks (taker lock, 7,800 s) | mean 25, max 67 transactions |
| 260 blocks (maker lock, 15,600 s) | mean 51, max 103 |
| 2,600 blocks (slow-coin maker lock, ×10) | mean 502, max 680 |
| Recovery of a swap 33,500 blocks old | about 6,400 lookups |

These figures predate Ironwood and the chain's halt in mid-October 2026, after which
blocks arrived far more slowly than 60 s. Step 0(e) measures them again. Slower blocks
mean fewer blocks per lock window, so fewer lookups per swap.

## The transaction index stays a precondition

Nearly every ARRR transaction is shielded-to-shielded. It has no transparent output, so
a node serves it by identifier only with its transaction index enabled (R39.11.4b,
R39.11.4c). The walk fetches every listed transaction in the window, so for the
spend questions it needs that index. It is the second remaining precondition of
R39.11.17, not the address-index one.

- **Detection.** For each endpoint, probe once whether it serves a mined
  shielded-to-shielded transaction from the cache. A failure marks the endpoint "no
  transaction index" for the spend questions, in the way R39.11.4i marks "no address
  index", with its own named condition. This is the decision D39.11.g asks for, so it
  needs the maintainer's answer (open question 4).
- **Already sent is unaffected** (see the previous section).
- **Unmined transactions.** A node serves those from its mempool without the index; that
  is to be verified in step 0(c). The mempool step below therefore also gives a node
  without the index a way to see a spend, but only while the spend is unmined.

## Latency, and the mempool step

A walk over blocks sees a spend once it is mined. An Electrum server lists unmined
history too, so through the Electrum fallback the secret can arrive before the spend is
mined. Today's address-index path reads mined history only, so against it the walk adds
no delay.

- **Added delay against Electrum.** Block arrivals are close to memoryless, so the
  expected time from broadcast to the next block is about one block interval *T*.
  Adding half the 10-second poll gives about **T + 5 s**: 65 s at the nominal 60 s,
  with a 95th percentile of about 3*T* (180 s). At ten times slower blocks it is about
  10 minutes on average.
- **No safety effect.** After the maker spends the taker's payment, the taker has until
  the maker payment lock to spend the maker's payment. That leaves at least one lock
  duration (7,800 s by default), against a delay of about one block interval. Only the
  time a swap takes to complete changes.
- **Optional mempool step.** `GetMempoolTx` is already polled for pending receipts
  (R39.8.0ai, R39.8.0aj), and it names the unmined transactions. Fetching each new one
  once lets the spend watch see a spend before it is mined, at about 0.2 lookups per
  minute. Whatever the mempool shows is confirmed by the block walk afterwards.

## Caching

- **What is cached.** The walk's per-block result: the block hash, and for each listed
  transaction either "nothing transparent" or the transparent outputs it spends and
  its P2SH output scripts. Almost every row is the short form. At 0.19 transactions
  per block, a year of ARRR blocks is about 100,000 rows.
- **Where.** Next to the compact blocks, with the same life. Rows above a rewind are
  deleted with the blocks (R39.8.0al), and a discarded cache takes its rows with it
  (R39.8.0n). All swaps of the coin share the cache, so a poll or a second recovery
  never fetches a transaction twice, and an interrupted recovery resumes where it
  stopped. Whether the rows survive a restart is open question 2.

## Request rate and bounds

- **Pacing.** One endpoint answers a question at a time, and requests to it are
  sequential, as today. Endpoints are tried in order (R39.11.3).
- **Bounds.** A question carries a time budget and a request cap, as R39.11.4h(b) and
  (c) do. When a recovery of an old swap reaches the cap, it ends undetermined, with
  its progress kept in the cache; the next call continues from there.
- **Measured so far.** Address-history windows of 1,000 blocks answered in under 0.2 s
  (R39.11.4h, provenance). `GetTransaction` latency and any rate limit of the published
  endpoints are not yet measured (step 0(a)).
- **Background cost.** None while no swap is running. The walk runs only when a swap
  question is asked.

## Reorgs

- **Block lists from the wallet cache.** These inherit the wallet's rewind
  (R39.8.0al): when blocks above a fork are deleted, their cached walk results go with
  them, and the next poll walks the new blocks.
- **Block lists fetched outside the cache.** These are checked hash to hash within
  the fetched range and against the last cached block. On a break, results above the
  fork are dropped, going back block by block up to the wallet's rewind depth (99). A
  deeper break drops the window's results and walks the window again.
- **A secret, once read and checked against the secret hash, is kept.** A secret
  stays valid even if the block holding the spend is orphaned. Whether the output is
  spent is derived again on the next question.
- **An orphaned own payment** is handled by the own-send record (R39.11.26), whose
  mined height the wallet's rewind clears.

## How it fits the decided rules

- **R39.11.26** (own-send record) stays first for already-sent. This plan adds the
  wallet-scan and mempool-nullifier steps behind it, before any address question.
- **R39.11.27** (fresh swap skips already-sent) is unchanged. It limits the
  already-sent path to restored swaps and recovery.
- **R39.11.28** (taker refused when nothing can watch) gains a third way to pass: an
  admitted endpoint that passed the transaction-index probe.
- **Order of sources for the spend questions**, until the last step: walk, then the
  address-index path, then Electrum. The walk goes first because each poll then costs
  less (usually no request, against at least four index reads), and because it does
  not rest on the node defect R39.11.4d works around. Open question 1.

## What the walk cannot do

- **See an unmined spend** without the mempool step (see Latency).
- **Serve the spend questions from a node without its transaction index**, except
  through the mempool step while the spend is unmined.
- **Prove that an Ironwood-only spend is listed in compact blocks**, until step 0(d)
  has measured it.
- **Supply notarized depth**, which remains unobtainable (R39.11.4e).
- **Make old recoveries cheap.** A recovery of a swap many weeks old pays 0.19
  lookups per block once; afterwards the cache holds the results.

## Hardening

Before a negative answer from the walk is acted on in a swap, three areas need to be
elaborated and specified:

- corroboration of negative answers by more than one independent endpoint;
- checks that a block's transaction list is complete;
- bounds on what a single endpoint's answer can decide.

## Steps

Each step is its own pull request and stays green on its own.

0. **Measurements** (an opt-in live probe, no behavior change):
   - (a) `GetTransaction` latency and rate behavior per published endpoint;
   - (b) which endpoints serve a mined shielded-to-shielded transaction (transaction
     index);
   - (c) whether an unmined transaction is served from the mempool by an endpoint
     without its transaction index;
   - (d) whether a transaction paying only Ironwood outputs appears in compact blocks;
   - (e) block time and block fill after Ironwood and the October halt.
1. **CRD.** New Reloaded-original rules in ch.39 §39.11.5: sources, match rules, the
   completeness conditions for a negative answer, the transaction-index probe and its
   named condition, reorg handling, and the hardening items once elaborated. Tests are
   specified as for T39.11.4h. No reference question is involved: the reference
   answers these questions from Electrum (R39.11.23).
2. **Already sent from the wallet** (wallet scan plus mempool nullifiers). Small.
3. **Block walk with cache and reorg handling** for the spend questions, with
   scripted-endpoint tests. The largest step.
4. **Source order and the R39.11.28 update.** Small.
5. **Mempool step** (optional, see open question 3).
6. **Field check.** A swap between two Reloaded nodes whose lists hold only endpoints
   without an address index and no `electrum_servers`, and a recovery of an old swap.
7. **Retire the Electrum fallback (last step).** Remove R39.11.25, the fallback code and the
   Electrum part of R39.11.28. Update `GLEEC_COMPATIBILITY.md`, the coin-configuration
   documentation and the changelog. `electrum_servers` in a shielded activation
   request is then ignored with one log line, so existing wallet applications that
   send it keep activating (open question 5). The address-index path can be reviewed
   for removal after this.

## Effort

| Step | Estimate |
|---|---|
| 0 Measurements | 0.5–1 day |
| 1 CRD | 1 day |
| 2 Already sent from the wallet | 0.5–1 day |
| 3 Walk, cache, reorg, tests | 3–5 days |
| 4 Order and R39.11.28 | 0.5 day |
| 5 Mempool step | 1–2 days |
| 6 Field check | 0.5–1 day, plus chain time |
| 7 Retire Electrum | 1–2 days |
| **Total** | **about 8–13 days** |

## Open questions

All five are decisions for after the beta; none needs an answer before it.

1. **Order of sources for the spend questions.** Walk first, then the index, then
   Electrum (recommended)? Or keep the index first until the field check?
2. **Cached walk results.** Persist them next to the compact blocks, so they survive
   a restart (recommended)? Or keep them in memory only?
3. **Mempool step.** Before Electrum is retired, for latency parity? Or after?
4. **D39.11.g.** Should an endpoint without a transaction index be excluded from the
   spend questions? Should a coin with no endpoint that passes the probe refuse to
   start swaps?
5. **`electrum_servers` after retirement.** Ignore it with a log line (recommended,
   because wallet applications send it today)? Or reject it?
