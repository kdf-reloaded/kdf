# Plan: ARRR native mode and the Ironwood pool

**Status:** planning (the v0.4.0 items shipped in PRs #227 to #288; native mode and the rest of the Ironwood pool are not designed yet) — **Last reviewed:** 2026-11-08

> **Context:** decided by the maintainer on 2026-10-28 (Decision X). Release 0.3.0-beta stays
> Sapling-only. It builds version-6 transactions that carry a Sapling bundle and an empty Ironwood
> bundle (CRD ch.39 R39.6.4), and it refuses native-mode activation of a shielded coin
> (ch.39 R39.11.1a). This plan collects what has to exist before native mode and the Ironwood pool
> can ship. The v0.4.0 part (the next section, and the capability word of §4) is merged; the rest
> targets a later release and is not settled design yet. Each item needs its own analysis pass, and
> the CRD rules come first.
>
> **Scope decided on 2026-11-04.** The ARRR wallet and ARRR swaps stay Sapling-only. v0.4.0 takes
> two items from the Ironwood pool: receiving payments that come from Ironwood wallets, and paying
> Pirate1 addresses (next section). Native mode, Ironwood scanning and spending, and Pirate1
> receiving addresses move to a later release. Ironwood swaps are reconsidered only if the reference
> gains that capability.

## In v0.4.0

Pirate Chain's own wallets hand out Pirate1 (Ironwood) addresses by default, and pay from
Ironwood notes. A Sapling-only wallet therefore needs these two items to keep working next to them.

| Work package | What it does | CRD | Code | Evidence | State |
|---|---|---|---|---|---|
| Ironwood deposits | A Sapling output inside a transaction that also carries Ironwood actions is found, counted (pending, then spendable) and spent like any other. | ch.39 R39.8.0bb, T39.8.0i | tests only: the compact conversion already drops the Ironwood actions | mainnet deposit from an Ironwood wallet to a lab wallet, then spent | tests in PR #242; live deposit passed 2026-11-06 (release tests, rows ARRR-L3 and ARRR-L4); the missing pending display before mining is fixed in PR #263 |
| Pirate1 withdrawals | `validateaddress` accepts a Pirate1 address; `withdraw` pays it from Sapling notes with one Ironwood output in an output-only bundle anchored at the empty tree, a fixed 0.0001 ARRR fee and Sapling change; the submitted transaction is recorded like any withdrawal. | ch.39 §39.6.5 (R39.6.5a–R39.6.5i, T39.6.5a–T39.6.5e) | address type and network prefix; a withdraw-only Ironwood output; a spends-disabled option in the vendored builder; fee; broadcast record. No new dependency: the pinned builder already carries the Ironwood output and the prover | mainnet withdrawal from a lab wallet to an Ironwood wallet, seen by that wallet; first proving-key build timed | code in PR #246; live round trip passed 2026-11-06 (release tests, rows ARRR-L2 to ARRR-L6); the history row names the Pirate1 recipient since PR #268 (ch.39 R39.8.9d2) |

- **First step:** T39.6.5a. Parse a mainnet transaction with Ironwood actions, and verify its proof
  with this project's own verifying key. It shows that the pinned libraries match the chain before
  any building code is written.
  *Done 2026-11-05 (PR #242):* the Ironwood bundles of two mainnet transactions verify with
  this project's own verifying key.
- **Reference answer (gated 2026-11-04):** the reference refuses a Pirate1 address (ch.39 §39.5A).
  Reloaded's acceptance is a local, additive divergence without a switch back (maintainer
  decision, 2026-11-04), recorded in the compatibility row.

## Scope (a later release, except the items above)

### 1. Native mode (a shielded coin served by its own full node)

What exists: the activation route, a native RPC client, and the older commitment-tree state cache.

What is missing:

- **Compact-block cache from full-node blocks.** Native activation does not convert the node's
  blocks into the compact-block cache the wallet database scans. A fresh native activation
  therefore fails the complete-through-tip scan gate (ch.39 R39.8.0f, its status update).
- **One height authority for native mode**, matching what R39.11.9 to R39.11.11 bind for light
  mode: tip, confirmations, expiry and the builder's target height. For confirmations and expiry,
  use the fresh tip, as R39.11.10a requires in light mode.
- **Ironwood activation discovery from the node** (design drafted on 2026-10-26, never implemented):
  1. A configured `ironwood_activation_height` is used as is.
  2. Otherwise, before a transaction is built, read the node's `getblockchaininfo`. An Ironwood
     entry among its network upgrades that carries an activation height is recorded as the
     discovered height.
  3. Otherwise, the node's consensus branch for the next block decides as the light-mode server
     branch does (ch.39 R39.6.4(c)). Ironwood's branch means active from the tip; another known
     branch means not yet active.
  4. Anything unreadable or not understood refuses the build with
     `BuilderHeightError::IronwoodActivation`. Native mode never guesses, and never builds
     version 4 once the node reports Ironwood's branch.
- **Lifting the refusal of R39.11.1a**, with a live activation, a send, and a swap on mainnet as
  evidence.

### 2. The Ironwood pool

Today the project builds no Ironwood bundle and does not scan for Ironwood-pool notes. v0.4.0
adds only the withdrawal output and the deposits of the section above. The rest is to be specified
from public Pirate Chain documentation and the dictated transaction format:

- **Scanning:** detecting and decrypting notes this wallet receives in the Ironwood pool, and
  keeping their witnesses. Balance and history then include them.
- **Spending:** building Ironwood spends and outputs, including change, fees, and moving funds
  between the Sapling and Ironwood pools.
- **Key derivation:** the Ironwood spending and viewing keys from the wallet seed, for both
  iguana and HD wallets.
- **Swaps:** whether HTLC payments, fees and refunds stay transparent P2SH funded from Sapling
  (as today), or may be funded from the Ironwood pool.

### 3. Pirate1 addresses

The address form that goes with the new pool. Parsing, validation and use as a withdraw
destination are in v0.4.0 (ch.39 R39.6.5a–R39.6.5c). Use as the wallet's own receiving address,
which needs Ironwood keys and scanning, stays for a later release.

*Facts established 2026-11-04 (KDF Spec Reader; passed the KDF Dirty Gate on
2026-11-04, measure 0.00; recorded in chapter 39 §39.5A and R39.5.14, whose
provenance footer carries the clearance):* the reference answers an
Ironwood (`pirate1…`) address with `is_valid: false` on `validateaddress` and
with the `InvalidAddress` error on the task withdraw route, decoding under the
Zcash main-network Sapling prefix, and has no legacy `withdraw` for a shielded
coin at all; a shielded coin's per-coin protocol information at the reference
is the transparent address-format descriptor, and a reference counterparty
accepts a value that extends that descriptor with trailing bytes and matches as
before, so a version-6 capability word must be enforced on the Reloaded side in
both roles.

### 4. Counterparty version-6 capability (decided 2026-11-04)

An ARRR swap must never start with a node that cannot build version-6 transactions. GLEEC KDF
cannot build for the post-Ironwood chain at all (CRD ch.39), and users cannot tell such a node
apart from a Reloaded one. The maintainer decided:

- **Decision BM:** ARRR's protocol information carries a capability word saying the node builds
  version-6 transactions. Reloaded requires the word from the counterparty, with a switch that
  is on by default.
- **Decision BN:** Reloaded 0.3.0-beta.1 nodes, which build version 6 but send no word, are
  declined for ARRR swaps too.
- **Decision BO:** the order-book answer shows no capability flag.

| Part | What | CRD | State |
|---|---|---|---|
| Capability word | A generic word after a coin's first value in its protocol information, sent only by coins that opt in. The reader fails closed. The order-match layer carries the bytes unchanged and logs declines. | ch.59 §59.4A (R59.4A.1–R59.4A.7, T14) | CRD PR #227, code PR #235 (generic part), both merged 2026-11-05 |
| ARRR requirement | ARRR sends `tx_v6` always. Inside the Ironwood window it declines a counterparty without it, on the taker and the maker path, with the per-coin switch `require_v6_counterparty` (default on). One debug line per decline, and at most one warning per coin per hour with the count. | ch.39 R39.6.4f, T39.6.4f | CRD PR #230, merged 2026-11-05 |
| ARRR predicate code | The shielded coin's protocol information and acceptance check, the switch, and the logs. Stacked on the capability-word code. | — | code PR #241, merged 2026-11-05 |

### 5. Transparent addresses in the shielded history (done for the node's own swap transactions)

`z_coin_tx_history` lists no transparent address. That covers the transparent inputs of
CRD ch.39 R39.8.9c and the transparent outputs of R39.8.9d: an ARRR swap's payment to its
script address, the spend or refund from it, and fee outputs. The shielded fee address that
a swap's fee payment pays is not listed either. Those rows show an empty or wallet-only `from`
and `to`, so a user reconciling a swap sees funds leave or arrive with no counterpart address.
The status note under ch.39 R39.8.9e records the gap, which was found by reading the code. For the
node's own swap transactions, the lab row "ARRR swap history", Part B, passed on 2026-11-07 with the
change of PR #277 (release tests 0.4.0-beta.1).

**Decided** on 2026-11-07 as a separate change, after the fix that lists the wallet's own address
in one form and names a withdrawal's recipient (ch.39 R39.8.9d1, R39.8.9d2; PR #268). Done in
PR #277 (see below). Size: S–M.

What it needs:

- the addresses, recorded at broadcast from the transaction this node built: the outputs of a
  swap payment and a fee payment, the script address a spend or refund spends (it follows
  from the redeem script the input carries), and the shielded fee address. They go into the
  store of ch.39 R39.8.9d2, with several addresses per transaction, each on its `from` or `to`
  side. Nothing is fetched from the light endpoint;
- an as-built note and tests in ch.39 (the rules exist);
- one live swap in the lab, to see the payment, spend or refund rows: done 2026-11-07 (release
  tests 0.4.0-beta.1, row "ARRR swap history", Part B).

Rows from before the change keep their empty sets.

**Done** (2026-11-07) for the transactions the node builds for a swap: ch.39 R39.8.9d3 and
T39.8.9d3, PR #277. The transparent addresses of a transaction another wallet built (a payment
from a transparent address, for example) stay unlisted, because nothing is fetched.

Later, if it matters: each scan pass checks every kept entry of a mined transaction against the
wallet database, and each history page reads the whole record. Both grow by one entry per
transaction the node builds, which is cheap at today's sizes. Marking an entry once its
transaction is mined, or joining the record into the wallet-database query, would make both
cost only the new entries.

### 6. Fee of a shielded history row (done for the node's own sends)

`z_coin_tx_history` reported `transaction_fee` 0 on every row, although a withdrawal's answer
names its fee. A live withdrawal in the release tests showed it on 2026-11-07. CRD ch.39 R39.8.9e
computes the fee from the full transaction, which a Light-mode wallet does not have.

**Done** (decided 2026-11-07) in PR #288, ch.39 R39.8.9e1: an own send (a withdrawal, a swap
payment, a swap fee payment) reports the fee the node recorded at broadcast. A transaction
another wallet built, a swap spend or refund, and an own send found again after a rebuild of the
wallet database report 0.

Later, if it matters: a swap spend or refund could record its fee when it is built, as item 5
records its address. The fee of a transaction another wallet built would need the full
transaction from the light endpoint.

## Open questions

- Which reference behavior, if any, applies? The reference builds no version-6 transactions.
  Native-mode parity questions go to the Spec Reader before any CRD rule is written.
- Do the light-mode backend (lightwalletd) and its compact format already carry Ironwood-pool
  outputs, or does scanning that pool need a backend change? *Answered 2026-11-04 from Pirate's
  published lightwalletd source:* they do. Ironwood actions travel in `CompactTx` field 6, and the
  Ironwood tree in `TreeState.ironwoodTree`. Scanning the pool needs an adapter on this side, not a
  backend change.
- Can native mode and light mode share one wallet database for the same seed, or must they stay
  separate?
- Is the capability word of section 4 temporary, as the maintainer expects (Decision BO)? If
  capabilities keep being needed, a far more universal flag, for every coin and visible to users,
  needs its own analysis (CRD ch.59 D5).

## Not in this plan

- Detecting a second instance on the same seed, and showing that instance's change while it is
  unmined. That stays with the two-daemon item of
  [`shielded-in-flight-note-locking`](shielded-in-flight-note-locking.md).
