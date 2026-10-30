# Chapter 38 -- UTXO Coin Maintenance & Features

**Status:** driving-spec (as-built baseline **plus** required-but-unimplemented
extensions). Mixed treatment -- see §38.0.

> **One-sentence claim:** the project shall provide a UTXO coin-support layer
> covering Qtum specialisation, config-driven coinbase maturity, multiple
> withdraw/spend address types, KMD interest/rewards handling, a raw-transaction
> signing RPC, a UTXO-consolidation RPC, and a configuration-selected
> chain-variant model -- and shall additionally grow, by required port, the
> post-2022 UTXO features reloaded does not yet carry (PoSV coins, Taproot
> output handling, P2PK balance, Electrum connection prioritisation, UTXO balance
> event streaming, the fixed-fee policy with its fee-derived minimum trading
> volume, and FIRO Spark verbose tx).

## 38.0 Treatment & scope split

This chapter is a brand-new chapter spanning a broad UTXO feature surface.
Reloaded ships some of it and lacks the rest, so the chapter is split:

- **§38.1--§38.5 (T-DOC, as-built):** capabilities verified present in reloaded
  -- Qtum split & staking-param naming, config-driven maturity, baseline address
  types (P2PKH / P2SH / segwit v0), KMD rewards/dust policy, `sign_raw_transaction`,
  `consolidate_utxos`, and the chain-variant model (shared with
  [chapter 37](37-utxo-spv-and-block-header-validation.md) §37.5).
  Exception: the KIP-0001 reward reduction of R38.4.1 is required but NOT
  as-built (issue #11; see §38.4).
- **§38.5A (T-DOC, reference-bound):** the transaction-history order,
  persistence and row-identity contract of the legacy `my_tx_history` method
  (UTXO and QRC20) and of the mmrpc-2.0 `my_tx_history` method, distilled from
  the reference states through the two-team workflow. Reloaded diverges today
  in the equal-height tie-break of the legacy UTXO history and in serving the
  mmrpc-2.0 method from the legacy file; the divergence notes under R38.5A.1
  and R38.5A.7 state what must change.
- **§38.6 (T-PORT, required, NOT yet in reloaded):** capabilities verified
  **absent** in reloaded that must be ported -- PoSV support, Taproot output
  parsing & withdraw guard, P2PK show/spend, Electrum connection prioritisation
  (min/max connected + server ordering), UTXO balance event streaming,
  the whole-kilobyte fixed-fee option (`dingo_fee`) with proportional plain
  fixed fees, the fee-derived minimum trading volume, and FIRO Spark
  verbose-tx support. Exceptions: the fee-derived floor of §38.6.6 is
  partly built (its as-built note lists what remains), the Electrum
  connection pool of §38.6.4 is partly built (its as-built note of 2026-10-27
  lists what remains), and P2PK (R38.6.3) and
  the Verus-family script label (R38.6.8) are built on dev (see their notes).
- **§38.8 (T-PORT, required, NOT yet in reloaded):** software (non-hardware)
  global-HD UTXO accounts -- storage availability, account-`0` bootstrap at
  activation, `get_new_address` advance, and gap/scan semantics for BIP-44 and
  BIP-84 coins -- consuming the Chapter 5 §5.9A software-HD crypto substrate.

> **Binding scope (R36).** Requirements bind observable behaviour, public RPC
> method strings / request-response field names, coins-config keys, and dictated
> script/format semantics (output script types, sighash variants). Private types,
> helper decomposition, and diagnostic wording are informative.

---

## Part A -- As-built baseline (T-DOC)

## 38.1 Qtum specialisation

R38.1.1 Qtum-specific behaviour shall be separated from the shared UTXO path so
that Qtum staking/delegation does not perturb generic UTXO coins.

R38.1.2 Qtum delegation RPCs shall use a parameter name consistent with the
Cosmos staking surface (a `validator_address` field), and Qtum staking RPCs are
exposed under the staking namespace (`get_staking_infos`, and the
`experimental::staking::` namespace).

## 38.2 Config-driven coinbase maturity

R38.2.1 A coin's coinbase-maturity / maturity-check behaviour shall be read from
the coins configuration (a `check_utxo_maturity`-style config key) rather than
hardcoded, so a coin can declare its own maturity policy.

## 38.3 Baseline address types

R38.3.1 The withdraw and spend paths shall support the legacy P2PKH, P2SH, and
segwit **v0** (P2WPKH) address/output types. Non-segwit coins shall be allowed to
withdraw to P2SH addresses.

R38.3.2 Output-script creation shall follow an address-builder model in which an
address carries its script type, and the signing path shall parse scriptSig
signatures tolerantly across real-world P2PKH variants.

R38.3.3 **HD derivation-path purpose generality.** When a UTXO coin's
coins-config supplies a `derivation_path` and the daemon is in HD mode, the coin
activation path and every UTXO HD consumer that parses that config field (the
account-derivation machinery, the new-address derivation RPC, the my-address RPC,
and the private-key-export RPC) shall deserialize it with the generic
purpose-level standard HD path of Chapter 5 R18 (`HDPathToCoin` /
`HDPathToAccount`), which accepts every standard BIP-43 purpose (32, 44, 49, 84).
These consumers shall NOT use the strict BIP-44 alias of Chapter 5 R19, whose
purpose level is pinned to 44 and which therefore rejects any other purpose.
Consequently a UTXO coin whose `derivation_path` declares a non-44 purpose — in
particular BIP-84 native segwit (`m/84'/<coin_type>'`) or BIP-49 nested segwit
(`m/49'/<coin_type>'`) — shall activate and derive HD addresses, instead of being
refused at activation with a purpose-mismatch error. The strict BIP-44 alias
remains reserved for fixed/internal paths only (e.g. the bound internal key path
of Chapter 5 R16); coins whose configured path is genuinely BIP-44 (such as the
EVM `m/44'/60'` family) are unaffected, since BIP-44 is a subset of the accepted
purposes. Acceptance (two-direction): a UTXO segwit coin configured with an
`m/84'` `derivation_path` activates in HD mode and a freshly derived external HD
address advances the address index under that same purpose; a UTXO coin
configured with an `m/44'` `derivation_path` continues to activate and derive.
Each derived address's reported `derivation_path` carries the configured purpose.

R38.3.4 **Withdraw-request derivation-path purpose generality (fifth
consumer).** The withdraw RPC's own `from` selector, when supplied as an
explicit derivation-path string (as opposed to being omitted, or supplied as
the structured account/chain/address-index selector), is a fifth UTXO HD
consumer of the purpose-generic parse rule R38.3.3 binds — not an exception to
it. The withdraw sender-resolution path shall deserialize that string with the
same generic purpose-level standard HD path of Chapter 5 R18, exactly as the
four consumers R38.3.3 names, and shall NOT use the strict BIP-44 alias of
Chapter 5 R19. Only the coin-type component of the parsed path is validated
against the activated coin's own configured SLIP-44 coin type; the account,
chain, and address-index components select which already-derived HD address
the withdrawal is sent from, and the resolved address itself is always the one
the coin's own configured account extended public key derives at that
account/chain/index — the purpose digit the caller supplies is not itself
trusted as a derivation input, only checked for being one of the recognised
BIP-43 purposes (32, 44, 49, 84) so a malformed or nonsensical path is still
rejected. Consequently a UTXO coin configured with a non-44 purpose (BIP-84
native segwit or BIP-49 nested segwit, per R38.3.3) shall accept a withdraw
`from.derivation_path` written at that same purpose, instead of being refused
with a purpose-mismatch error merely because the purpose is not 44. Acceptance
(two-direction): a UTXO segwit coin activated with an `m/84'` `derivation_path`
(R38.3.3) accepts a withdraw request whose `from.derivation_path` is a full
`purpose'/coin_type'/account'/chain/address_index` string at purpose `84'`
naming an already-derived address, and resolves it to the same sender address
the my-address / new-address consumers report for that path (two-direction
with R38.3.3's own acceptance); the same coin's withdraw continues to be
refused when the path's coin-type component does not match the activated
coin, independent of the path's purpose. This rule binds the same coin family
this chapter otherwise covers (UTXO); the identically-shaped question for
non-UTXO coin families that also accept an explicit withdraw derivation path
is outside this chapter's scope.

## 38.4 KMD interest / rewards & dust policy

> **Status of §38.4 (corrected 2026-09-27, issue #11).** §38.0 lists this
> section as as-built. R38.4.1 was not: the reloaded tree carries the
> pre-2023 reward rule without the KIP-0001 reduction. See the
> code-quality finding after R38.4.1B. R38.4.2 is unchanged.

R38.4.1 **Reward amount (consensus-dictated).** The KMD active-user reward
(AUR, also called "interest") that the project computes for one KMD UTXO
MUST equal the amount the Komodo consensus rule allows for that UTXO. The
rule comes from the public Komodo daemon and KIP-0001 (§38.10). It is
evaluated in unsigned 64-bit integer arithmetic, with no floating point,
from four inputs:

- `value` is the UTXO value in base units (1 KMD = 10^8);
- `utxo_height` is the block height at which the UTXO's transaction was
  confirmed;
- `utxo_lock_time` is the `nLockTime` of the transaction that created the
  UTXO;
- `ref_time` is the reference time given by R38.4.1A.

The reward is zero, and a not-accrued reason is reported where the surface
reports one, when any of these holds:

- `value` < 1,000,000,000 (10 KMD);
- `utxo_lock_time` is 0 or < 500,000,000;
- the UTXO is unconfirmed;
- `utxo_height` ≥ 7,777,777 (end of era);
- `ref_time` < `utxo_lock_time`;
- fewer than 60 whole minutes have passed.

Otherwise:

1. `minutes = floor((ref_time − utxo_lock_time) / 60)`;
2. cap `minutes` at 525,600 (one year);
3. if `utxo_height` ≥ 1,000,000, also cap `minutes` at 44,640 (31 days);
4. `minutes = minutes − 59`;
5. `reward = floor(value / 10,512,000) × minutes`;
6. **KIP-0001 reduction:** if `utxo_height` ≥ 3,484,958 (the dPoW Season-7
   hard-fork height), `reward = floor(reward / 500)`.

Each step truncates, and the steps run in the order given. Step 6 depends on
the height of the UTXO being spent. It does not depend on the current chain
height or on the height of the spending transaction. This matches the
consensus rule. A UTXO confirmed before 3,484,958 keeps the pre-reduction
rate, capped as in steps 2–3.

R38.4.1A **One rule, every surface.** R38.4.1 MUST be the only reward
computation. Every KMD surface that computes, claims, or reports a reward
MUST use it:

| Surface | `ref_time` |
| --- | --- |
| Building any KMD spend (withdraw, taker fee, swap payments, any send from the wallet), when the reward is added to the wallet's own output or change | the lock time the new transaction will carry: the current median-time-past used as its `nLockTime` |
| Transaction-history reward reporting (`kmd_rewards` on history entries) | the spending transaction's `nLockTime` |
| The `kmd_rewards_info` RPC (`accrued_rewards`) | the current median-time-past |
| Withdraw / transaction-details `kmd_rewards` | the same value as the build row |

A transaction built from R38.4.1 MUST NOT claim more reward than the
consensus rule allows at the transaction's own lock time. If it does, the
network rejects it (`bad-txns-in-belowout`, which is the daemon's
public-network error; see §38.10).

R38.4.1B **Lock time when nothing is claimed.** If the build row of
R38.4.1A computes zero total reward for a KMD transaction, the transaction's
`nLockTime` MUST be set to the current wall-clock time minus 2,046 seconds
(one hour minus two 777-second windows). This keeps the claimable window
open for later spends of its outputs. The netid-8762 and netid-6133
references both behave this way, and so does the baseline. It explains why
a no-reward KMD transaction's lock time trails the wall clock by about 34
minutes. It is not a defect.

> **Code-quality finding (informative; explicit look requested for issue
> #11).** The reloaded reward computation stops after step 5 of R38.4.1: it
> never applies the KIP-0001 reduction. Every reward-computing surface of
> R38.4.1A therefore overstates the reward 500× for any UTXO confirmed at or
> after height 3,484,958. On the build surface this is funds-affecting. Any
> KMD spend whose inputs include an eligible post-fork UTXO (value ≥ 10 KMD,
> at least an hour old) claims about 500× the allowed reward, and the network
> rejects it. Taker fees, swap payments, and withdraws are all affected.
> Issue #11's taker-fee rejection is this case: a 14.73232138 KMD UTXO at
> height 5,108,257 was spent with a lock-time difference of 12,518 minutes,
> and the transaction claimed 1,744,260 base units where the rule allows
> 3,488. History and `kmd_rewards_info` show the same inflated figures but
> do not move funds. *Proposed fix:* add step 6 to the single reward
> computation, keyed on the spent UTXO's confirmation height, and keep all
> other steps unchanged. Every surface then inherits the fix. Both reference
> lineages (`v2.6.0-beta` for netid 8762 and the v3 lineage for netid 6133)
> apply the reduction, so the fix needs no per-netid switch.

R38.4.2 When a KMD transaction's change plus accrued interest is at or below the
dust threshold, the accrued rewards shall be applied toward fees rather than
producing a dust output.

R38.4.3 **Tests (exact values, deterministic).** Every case uses the
integer rule of R38.4.1.

- *Issue #11 vector, post-fork.* `value` 1,473,232,138, `utxo_height`
  5,108,257, `utxo_lock_time` 1,789,721,423, `ref_time` 1,790,472,552. This
  gives 12,518 elapsed minutes and 12,459 after step 4. The reward MUST be
  exactly **3,488** base units. The pre-reduction figure 1,744,260 MUST NOT
  be produced.
- *Same vector, pre-fork height.* With `utxo_height` 3,484,957 and every
  other input unchanged, the reward MUST be exactly 1,744,260.
- *Fork boundary.* With `utxo_height` exactly 3,484,958, the reward MUST be
  3,488.
- *Legacy regression.* `value` 64,605,500,822, `utxo_height` 1,000,001,
  `utxo_lock_time` 1,556,623,906, `ref_time` = `utxo_lock_time` + 3,900: the
  reward MUST stay 36,870, because this UTXO is below the fork height. The
  existing not-accrued-reason cases (below 10 KMD, unconfirmed, lock time
  unset or below threshold, end of era, reference time before lock time,
  under one hour) MUST keep their current results.
- *Build-surface integration.* A KMD transaction built by the wallet's
  transaction builder, spending the issue #11 UTXO with lock time
  1,790,472,552, MUST add exactly 3,488 base units of reward to the wallet's
  own output. A UTXO of at least 10 KMD that is still younger than one hour
  MUST add zero and MUST get the lock time of R38.4.1B.
- *Surface agreement.* For one fixed UTXO and reference time, the history
  entry's `kmd_rewards`, the `kmd_rewards_info` `accrued_rewards`, and the
  built transaction's claimed reward MUST all equal the R38.4.1 value.

> **Implementation obligations (for the Coder; clean terms).**
> - *Reward computation:* the single KMD reward computation in
>   `mm2src/coins/utxo.rs` gains step 6 of R38.4.1. Its callers are the
>   spend builder in `mm2src/coins/utxo/utxo_common/utxo_common_tx.rs`,
>   history in `utxo_common_history.rs`, and the rewards-info RPC in
>   `utxo.rs`. They need no change beyond inheriting it.
> - *Tests:* extend `test_kmd_interest` in `mm2src/coins/utxo/utxo_tests.rs`
>   with the R38.4.3 vectors, keeping its existing cases, and add the
>   build-surface and surface-agreement tests.
> - *Changelog:* add a `CHANGELOG.md` entry stating that KMD spends of
>   post-hard-fork UTXOs no longer over-claim rewards.

## 38.5 Raw-tx signing, consolidation & chain variants

R38.5.1 The public `sign_raw_transaction` RPC shall sign a supplied raw
transaction for a UTXO coin (and is shared with the EVM path), returning the
signed transaction hex.

R38.5.2 The public `consolidate_utxos` RPC shall merge many UTXOs of a coin into
a single self-directed output under configurable merge conditions, with an
optional broadcast flag; when broadcast is not requested it returns the
constructed transaction without sending it.

R38.5.3 The coin's header/byte handling shall use the configuration-selected
chain-variant model defined in
[chapter 37](37-utxo-spv-and-block-header-validation.md) §37.5 (shared
contract). Chapter 37 additionally binds the SPV activation configuration,
header-store, and confirmation-proof behaviour a UTXO coin may separately
opt into; this chapter does not restate it.

R38.5.4 The coin's **signature-hash algorithm** is selected by a separate
coin-configuration contract, bound in
[chapter 58](58-utxo-signature-version-and-radiant-sighash.md) §58.2: the
`signature_version` key over a closed set of four values, the `fork_id` key
that forms the signed sighash byte, and the per-coin final-hashing selection
that is orthogonal to both. That contract reaches **every** signing site of
the coin, the `sign_raw_transaction` RPC of R38.5.1 included — which matters
for R38.5.1 specifically, because that RPC accepts caller-supplied scripts
and so can reach parts of a signature-hash algorithm the project's own
transaction construction never does (ch. 58 R58.3.5). `signature_version` and
`chain_variant` are independent: neither implies the other, and the one coin
whose dialect reloaded does not yet support needs no `chain_variant` at all
(ch. 58 R58.2.6).

> **As-built note (2026-10-25).** The dialect the last sentence refers to
> — Radiant's `fork_id_rxd` — is implemented in PR #65 (stacked on PR #54,
> pending merge; ch. 58 §58.5 as-built statement). The sentence remains
> true as a statement of configuration: Radiant still needs no
> `chain_variant`. The cleared rule text above is unchanged.

## 38.5A Transaction history: order, persistence, and row identity (T-DOC)

> **Scope.** This section binds the order in which transaction-history rows are
> served and persisted, when the persisted history is written, and what one row
> stands for, for three surfaces: the legacy `my_tx_history` method over the
> per-coin history file of a UTXO coin (Electrum and native modes); the same
> legacy method over the history of a QRC20 token; and the mmrpc-2.0
> `my_tx_history` method over the SQLite history store. It exists because a
> client that merges successive pages by position (the desktop wallet does)
> depends on a deterministic, stable order. Shielded history is bound by
> [Chapter 39](39-zcash---z_coin-shielded-coin.md) §39.8.5 (R39.8.9a,
> R39.8.9f--R39.8.9h) and Sia history by [Chapter 53](53-sia-transaction-history.md)
> (R53.4.5); both follow the same unconfirmed-first, newest-first,
> identifier-tie-break convention stated here. Field names of the two methods
> are the dictated public wire contract; nothing below adds or renames a field.

> **Dirty-side derivation record (sanitized).** R38.5A.1--R38.5A.8, the notes
> under them and T38.5A.1--T38.5A.6 were authored by the KDF Spec Reader under
> the chapter-01 two-team workflow (§1.4A), from the reference states
> `v2.6.0-beta` and `v3.0.0-beta` (chapter 02 §2.15A), which agree on every
> fact bound below: the legacy history collector, the QRC20 history collector,
> the legacy method's paging, the SQLite store's page query and the mmrpc-2.0
> method's coin-family routing are identical at the two states. Only the
> externally observable contract was carried across. The KDF Dirty Gate
> passed this section on 2026-10-26 (discretionary-expression measure 0.05, no
> leaks to scrub); the provenance footer records its scope attestation.

R38.5A.1 **Order of the legacy UTXO history (persisted and served).** The
history of a UTXO coin shall be a sequence sorted by this key, in this
precedence: (1) rows whose `block_height` is 0 (unconfirmed) come before every
confirmed row; (2) confirmed rows are ordered by `block_height` **descending**
(newest block first); (3) rows with equal `block_height` -- including several
unconfirmed rows -- are ordered by `internal_id` **ascending**, compared
bytewise. For a UTXO coin the `internal_id` is the transaction hash in its
displayed byte order, so the tie-break is ascending transaction hash. This key
is a total order over distinct `internal_id` values; the resulting sequence is
therefore deterministic, independent of the order in which the backend listed
the transactions and of the order in which the collector processed them, and
identical for Electrum and native modes, whose listings differ only as a
source. Two rows never compare equal unless they share an `internal_id`, which
R38.5A.4 forbids.

> **Reloaded divergence (2026-10-26).** Reloaded's legacy UTXO collector keeps
> rows in a hash map and persists its iteration order, and its comparator is
> not a total order for two rows of height 0, so rows of equal height -- and
> several unconfirmed rows in particular -- are served in an order that can
> differ between two writes of the same set. This violates R38.5A.1 (3). The
> fix is a comparator that applies the full key above, including the bytewise
> `internal_id` tie-break at equal height, before every write (R38.5A.3).

R38.5A.2 **Serving the legacy history.** The legacy `my_tx_history` method
shall serve the persisted sequence as stored -- it does not re-sort at read
time, so R38.5A.1 is a property of every write. `page_number` paging skips
`(page_number - 1) * limit` rows; `from_id` paging skips every row up to and
including the row whose `internal_id` equals the supplied value, and an unknown
`from_id` is an error, not an empty page; `max: true` returns every row and
reports the row count as `limit`. `confirmations` is computed at read time per
row: 0 when the row's `block_height` is 0 or exceeds the current tip,
otherwise `tip - block_height + 1`. The response reports `total` (the row
count), `skipped`, `current_block`, `sync_status`, the echoed `from_id` and
`page_number`, and `total_pages` computed from `total` and the requested
`limit`.

R38.5A.3 **When the legacy UTXO history is written.** Every write shall first
sort the whole sequence by R38.5A.1 and then replace the stored history
atomically (a complete new file substituted for the old one, never a partial
overwrite). Within one collection pass the history is written (a) after rows
are removed because the backend listing no longer contains their hashes, if
any were removed, and (b) *(informative, see the note below)* after **each**
transaction that is newly added or whose stored row was updated -- not once at
the end of the pass. A client reading between two writes sees a consistent,
fully sorted subset of the final result, never an unsorted or truncated file.
A pass that changes nothing writes nothing.

> **Informative: write cadence.** Item (b) records the reference's write
> cadence. No history method can observe how often the background collector
> writes, so (b) binds nothing: an implementation conforms whether it writes
> after each transaction or once per pass, provided every write satisfies the
> first sentence of this rule. T38.5A.2 and the persistence bullet of R38.5A.8
> are informative for the same reason.

R38.5A.4 **One row per transaction (UTXO).** For a UTXO coin the legacy history
shall contain exactly one row per transaction hash, and the row's
`internal_id` shall equal that hash. The collector keys its working set by
transaction hash, so a hash the backend lists more than once collapses into a
single row: the native wallet listing yields one entry per affected own
address and per direction, so a self-send (one send entry and one receive
entry), a payment with several outputs to own addresses, and a transaction that
touches several own addresses each appear once; an Electrum listing yields one
entry per transaction per script hash. The single row's amounts are net over
the whole transaction: `spent_by_me` is the sum of the values of all inputs the
wallet owns, `received_by_me` the sum of all outputs the wallet owns,
`my_balance_change` their difference, `total_amount` the sum of all input
values, and the fee the difference between total inputs and total outputs
(coinbase and other input-less transactions take their fee from the
backend-reported per-input fee fields, which is 0 for ordinary chains). Address
lists are deduplicated. The collector never writes two rows with one
`internal_id`; a persisted history that nevertheless contains them (external
corruption) collapses to one row at the collector's next write, because its
keyed working set holds one row per hash. A history whose stored form cannot be
decoded is discarded and rebuilt from the backend; an absent history reads as
empty.

> **Reloaded divergence (2026-10-26, fixed in the same change).** Reloaded keyed
> the rows it loaded from the history file by the first 32 bytes of the
> `tx_hash` text (the first 32 hexadecimal characters) instead of by the parsed
> transaction hash, so no loaded row ever matched a listed hash. The first pass
> after every restart re-fetched each listed transaction as new and wrote it
> beside the stored row: the history then held two rows, with one
> `internal_id`, for every transaction it had before the restart. The working
> set is now keyed by the hash parsed from `tx_hash`, so a history already
> doubled on disk collapses to one row per hash at the next write. A stored row
> whose `tx_hash` does not parse as a hash is dropped, with an error log, and
> fetched again from the backend as a new transaction.

R38.5A.5 **Maturation of a stored row.** A stored row with `block_height` 0, or
with the all-ones sentinel value that older histories may still hold for an
unconfirmed transaction, shall have its `block_height`
replaced in place when the backend listing later reports a positive height for
its hash; the row then moves from the unconfirmed group to its block's position
at the next write. A stored row with a zero `timestamp`, and for FIRO a row
whose fee is negative, shall be re-fetched and replaced whole. The collector's
need-to-run test on each pass is "balance changed since the last pass, or any
stored row still needs one of these updates"; otherwise the pass is skipped.

R38.5A.6 **Order of the mmrpc-2.0 history page.** The page query over the
SQLite history store shall order by: (1) confirmation status ascending, where
unconfirmed (stored `block_height` 0) sorts before confirmed; (2)
`block_height` descending; (3) `internal_id` ascending. The `internal_id` is
stored as a lowercase hexadecimal string, so its lexicographic order equals the
bytewise order of equal-length identifiers; for a UTXO coin it is the
transaction hash. `total` is the count of rows matching the request's address
filter in that same order, and `FromId` paging locates the supplied
`internal_id` in that order; a `FromId` not present in the filtered set yields
an empty page with `skipped` 0 and the full `total`, not an error. The browser
(WASM) store sorts the filtered set with the same three-part key, so both
platforms serve the same page. `confirmations` is computed at read time as
`current_block + 1 - block_height`, or 0 when `block_height` exceeds the tip
(an unconfirmed row with `block_height` 0 therefore reports `current_block + 1`
under the reference arithmetic -- see the note below).

> **Reloaded divergence (2026-10-26, fixed in the same change).** Reloaded's
> SQLite page query and its `FromId` offset broke ties at equal height by the
> table's integer row identifier (insertion order) instead of by `internal_id`;
> both now use the key above. Reloaded serves the mmrpc-2.0 method only in
> native builds (a browser build answers `NotSupportedInWasm`), so the sentence
> on the browser store has no Reloaded counterpart today. Reloaded's read-time
> `confirmations` already reports 0 for a stored height of 0, the fix the
> finding below proposes.

> **Code-quality finding (informative) -- v2 confirmations of an unconfirmed
> row.** The read-time arithmetic of the mmrpc-2.0 method does not special-case
> a stored height of 0, so an unconfirmed row reports `tip + 1` confirmations,
> while the legacy method (R38.5A.2) and the shielded method (R39.8.9e) report 0
> for the same situation. The finding fell out of reading the two formulas side
> by side. Proposed fix: report 0 when the stored height is 0, matching the
> legacy surface; the stored confirmation-status column already distinguishes
> the case.

R38.5A.7 **Which coin families the mmrpc-2.0 method serves, and from where.**
The mmrpc-2.0 `my_tx_history` method shall serve its page from the SQLite
history store for exactly these families: standard UTXO coins, Qtum, BCH, SLP
tokens, Tendermint coins and Tendermint tokens. Every other family -- QRC20
tokens, EVM coins and tokens, shielded coins (R39.8.0c), Lightning -- shall be
rejected with `NotSupportedFor`. There is no fallback from the mmrpc-2.0
method to a legacy per-coin history for any family. The store of a UTXO or
Qtum coin is populated only by the v2 background collector, which the
task-based activation (`task::enable_utxo::init` / `task::enable_qtum::init`
with `tx_history` true, [Chapter 55](55-coin-activation-wire-contract.md))
starts; the flat legacy activation methods with `tx_history` true start the
legacy file collector of R38.5A.1--R38.5A.5 instead. A coin activated by a
legacy method therefore answers the mmrpc-2.0 method with
`StorageIsNotInitialized`, and a coin activated by the task method serves the
legacy method from a history file the legacy collector never wrote (empty, or
stale from an earlier legacy activation). The two stores are independent; a
transaction is never written to both by one activation.

> **Reloaded divergence (2026-10-26).** Reloaded serves the mmrpc-2.0 method for
> UTXO coins from the legacy history file today, so its v2 page order is
> whatever the legacy file holds (R38.5A.1 and its divergence note), and the
> two methods share one store. The dictated page order of R38.5A.6 and the
> legacy order of R38.5A.1 coincide for a UTXO coin (unconfirmed first, height
> descending, hash ascending), so a Reloaded build that fixes the R38.5A.1
> divergence serves conforming v2 pages from the shared file; the independence
> of the two stores is not itself a wire-visible property and is not required
> for parity.

R38.5A.8 **QRC20 token history.** The legacy `my_tx_history` method for a QRC20
token is served from the token's own history collector and shall follow the
same persisted order as R38.5A.1 and the same read-time rules as R38.5A.2, with
two differences:

- *Row identity.* One row stands for one token-transfer event, not for one
  transaction. Its `internal_id` is the transaction hash followed by the index
  of the output that carried the contract call and the index of the transfer
  event within that output's receipt, each as an 8-byte big-endian integer, so
  a transaction with several transfers legitimately yields several rows. Those
  rows share `block_height` and sort adjacently and deterministically because
  their identifiers share the leading hash bytes and differ only in the two
  index fields. The `block_height` of such a row is the receipt's block
  number, and the row's fee details carry the transaction's miner fee (its
  QTUM fee less all gas the transaction consumed) together with the gas limit,
  gas price and gas used of the row's own contract call. On load, a stored
  history with two rows of one `internal_id`, or with an `internal_id` of the
  wrong shape, is discarded whole and rebuilt from the backend rather than
  silently collapsed.
- *Persistence (informative as to cadence, see the note under R38.5A.3).* The
  token collector writes its history **once per pass**, after
  every listed transaction has been processed, and only when the pass changed
  something; it does not write after each transaction. Each write sorts the
  whole sequence by R38.5A.1 and replaces the stored history atomically.

Maturation follows R38.5A.5 for the height (updated in place from the listing)
and the timestamp (re-fetched from the parent transaction); a cached transfer
set whose rows disagree about their transaction hash is discarded and re-fetched
whole.

> **Code-quality finding (informative) -- native-mode height fallback.** In
> native mode the legacy UTXO collector pairs each listed transaction hash with
> the wallet listing's *position of the transaction within its block*, and
> uses that number as the fallback `block_height` when the verbose lookup
> reports none and as the in-place height update of R38.5A.5. The verbose
> lookup normally supplies the real height, and a row whose timestamp is still
> zero is re-fetched whole, so the wrong value is usually overwritten; a
> confirmed transaction whose verbose lookup lacks a height but carries a
> timestamp would keep a block index as its height. The finding fell out of
> reading the two listing paths side by side. Proposed fix: pair the hash with
> the listing's block height, or with 0 when the entry is unconfirmed, in
> native mode as the Electrum path already does.
>
> *Reloaded (2026-10-26):* Reloaded had the same defect and now pairs each
> hash with the listing's `blockheight`, or with 0 when the daemon reports
> none, so the existing re-fetch supplies the real height.

R38.5A.9 **A malformed transaction does not stop the history.** When the
history collector builds the details of one transaction, two inputs it cannot
interpret shall yield an error for that transaction only:

- an input whose previous-output index is beyond the end of the previous
  transaction's output list, both when the input's spent value and address are
  read and when the KMD reward of the input is accumulated;
- for BCH and SLP, a transaction that has no outputs, and an SLP token genesis
  whose decimals field is empty.

The collector logs the error, skips that transaction, and continues with the
remaining ones. It never panics, and it never stops the history task for the
coin. The skipped transaction is not written to the history; it is tried
again on a later pass.

R38.5A.10 **An unconfirmed row of the BCH and SLP history matures.** When the
mmrpc-2.0 history collector of a BCH coin or SLP token finds a stored
unconfirmed row whose transaction hash the backend now lists with a positive
height, it shall give the row that height and the block's time, as R38.5A.5
does for the legacy file. The stored row and the listing are matched by the
transaction hash itself, never by comparing the hash bytes with the text of
its hexadecimal form.

> **Provenance of R38.5A.9 and R38.5A.10 (2026-10-26).** Clean-room original.
> R38.5A.9 was written from a behavioral description only. Part of it matches
> the reference at both reference states: the per-transaction error for an
> out-of-range previous-output index, the log, skip and continue of the legacy
> collector, and the error for a BCH transaction with no outputs. The rest of
> R38.5A.9 is a Reloaded requirement: the error in the KMD reward
> accumulation, the error for an SLP genesis with an empty decimals field, and
> skip and continue in the mmrpc-2.0 BCH and SLP collector. R38.5A.10 states
> what Reloaded's collector was written to do. The reference also matches the
> row by the hash value and gives it the listed height and block time.
>
> **Dirty Gate (2026-10-26, post-hoc).** PASS, score 0.01 (threshold 0.10),
> compared by reading against both reference rows (`v2.6.0-beta` and
> `v3.0.0-beta`) over the UTXO transaction-history subsystem. No reference
> expression crossed. The gate confirmed reference parity for the
> out-of-range-index error and the log-skip-continue behavior of the legacy
> collector, the BCH no-outputs error, and R38.5A.10's hash-value matching
> with height and block time. It did not confirm reference parity for the
> KMD-reward-accumulation error clause, the empty-decimals SLP genesis clause,
> or skip-and-continue in the mmrpc-2.0 (BCH/SLP) collector; those are
> Reloaded requirements, and the parity statement above is limited to the
> confirmed items.
>
> **Reloaded divergence (2026-10-26, fixed in the same change).** Reloaded read
> the previous transaction's output at the input's index without a bounds
> check in the details builder and in the KMD reward accumulation, so one
> malformed transaction ended the history task with a panic. Its BCH and SLP
> collector compared a listed hash's 32 bytes with the 64-character
> hexadecimal text of the stored row's hash. Those never match, so an
> unconfirmed BCH or SLP row kept height 0 after its block was mined.

T38.5A.1 *Deterministic legacy order.* A test shall seed a legacy history with
at least two unconfirmed rows and two rows at one confirmed height, written in
two different insertion orders, and assert that both writes produce the same
file sequence: unconfirmed rows first in ascending hash order, then confirmed
rows by height descending with ascending hash at equal height; `my_tx_history`
pages shall return that sequence unchanged across `page_number` and `from_id`
requests.

T38.5A.2 *Per-transaction persistence (informative; not a conformance test,
see the note under R38.5A.3).* A test may drive one collection pass
over a backend fixture listing N new transactions and assert that the stored
history is written N times (plus once if rows were pruned), that every
intermediate file is sorted by R38.5A.1, and that the final file equals the
expected sequence.

T38.5A.3 *One row per hash.* A test shall feed the legacy collector a native
wallet listing in which one hash appears as both a send and a receive entry and
another hash appears for two own addresses, and assert that each hash produces
exactly one row whose `internal_id` equals the hash and whose `spent_by_me`,
`received_by_me`, `my_balance_change` and `total_amount` are the net values of
R38.5A.4; the same test over an Electrum fixture shall produce identical rows.

T38.5A.4 *Row maturation moves the row.* A test shall store a height-0 row, run a
pass whose listing reports a positive height for it, and assert that the row
keeps its `internal_id`, acquires that height, and is served after the
remaining unconfirmed rows at its block's position.

T38.5A.5 *mmrpc-2.0 page order and routing.* A test over a seeded SQLite history
store with unconfirmed rows and several rows at one height shall assert the
three-part order of R38.5A.6 for `PageNumber` and `FromId` paging, an empty page
with full `total` for an unknown `FromId`, and `NotSupportedFor` for a QRC20
token, an EVM coin and a shielded coin.

T38.5A.6 *QRC20 rows.* A test over a QRC20 fixture transaction carrying two
transfer events shall assert two rows with distinct `internal_id` values that
share the leading transaction-hash bytes, adjacent in the served order with the
lower output/event index first, and a single history write for the pass.

T38.5A.7 *Malformed transaction.* A test shall build the history details of a
transaction with an input whose previous-output index is past the previous
transaction's outputs, and of a BCH transaction with no outputs, and assert a
per-transaction error, not a panic. The KMD reward accumulation shall get the
same out-of-range input and return an error. Where the loop can be driven, the
remaining transactions of the pass shall still be stored.

T38.5A.8 *BCH and SLP maturation.* A test shall store an unconfirmed BCH row
and present a listing that reports its hash at a positive height, and assert
that the stored row takes that height.

---

## Part B -- Required, NOT yet in reloaded (T-PORT)

> **Status of Part B:** required ports. Each item below was checked against the
> reloaded UTXO tree during step 7; unless an item is marked inline as already
> as-built, it was found absent and implemented in step 7. (§38.6.6 carries its
> own as-built note, 2026-10-25: partly built.) Since then some of these ports
> have been built on dev; each such rule carries its own as-built note
> (R38.6.3 and R38.6.8, verified 2026-10-26; §38.6.4 partly, verified
> 2026-10-27), and "NOT yet in reloaded" in the
> headings above describes the state at the time of the audit, not today's tree.

## 38.6 Required UTXO feature ports

### 38.6.1 PoSV (proof-of-stake-velocity) coins
R38.6.1 The project shall support PoSV/PoS-style UTXO coins: serialize,
deserialize and sign transactions that carry an `n_time` field, gated by a
coins-config flag (`isPoS`) declaring the coin as PoS/PoSV. Acceptance: such a
coin's withdraw produces a transaction whose `n_time` is present and accepted by
the network.
>
> **Status: as-built (T-DOC).** Verified present in the reloaded UTXO tree during
> step 7. The `isPoS` coins-config flag drives an optional `n_time` field on the
> transaction primitive: it is written by the witness-stripped serializer and
> recovered by the PoS deserialization path (auto-detected by the multi-format
> transaction decoder). A serialize/deserialize round-trip regression test locks
> the contract. No further feature code was required.

### 38.6.2 Taproot output handling
R38.6.2 Until full Taproot spending is supported, a withdraw whose destination is
a Taproot (witness v1 / bech32m) address shall be rejected with a clear
unsupported-address error rather than mis-encoded. Separately, the verbose-tx
parser shall be able to **recognise** Taproot output address types returned by
`blockchain.transaction.get`. Acceptance: withdraw to a bech32m address is
refused; a verbose tx with a Taproot output parses without error.

### 38.6.3 P2PK show & spend
R38.6.3 The project shall display a P2PK balance as part of the legacy
(P2PKH) address balance and shall be able to spend P2PK inputs in withdraws and
swaps. For P2PK inputs (whose scriptSig carries only the signature, not the
pubkey), the expected pubkey shall be validated against the signature rather than
extracted from the scriptSig. Acceptance: a P2PK UTXO contributes to balance and
can be spent.
>
> **Status: as-built on dev (verified 2026-10-26), in part.** P2PK outputs count
> in the legacy address balance, and P2PK inputs can be spent in Electrum mode.
> Native (coin daemon) mode has no P2PK path yet; see
> [`UPSTREAM-PARITY-GAPS.md`](./UPSTREAM-PARITY-GAPS.md) #16.

R38.6.3a *One round trip per read (passed the KDF Dirty Gate on 2026-10-26; see §38.9).*
For an Electrum-backed coin with a legacy address, the P2PK lookup that
R38.6.3 adds to a balance read or an unspent-list read shall travel in the
same Electrum JSON-RPC batch as the P2PKH lookup it accompanies, so that
each such read costs the server exactly one request message. It shall never
be issued as a separate sequential request.

*Rationale (this project's own measurement, 2026-10-26).* Public ElectrumX
servers limit each client session: after a burst of about 26 requests, a
session is served about one request message per second, and a JSON-RPC
batch counts as one message whatever its length. Idle time does not reset
the limit; only a new session does. A long-lived node session therefore
pays about one second per sequential request, and a second request per read
doubles the time of every balance and unspent read of a legacy-address coin.
Nothing here is visible to a peer.

*Acceptance.* A balance read and an unspent-list read of a legacy Electrum
address each send one request message to the server, carrying both
lookups. A segwit or non-legacy address sends the P2PKH lookup alone, as
before.

> **As-built note (2026-10-27, clean-side record).** Implemented in PR #114 for
> balance reads and unspent-list reads. The P2PK lookups that the send and
> withdraw paths use to classify inputs are not balance or unspent-list reads.
> They still cost one separate request each on a legacy address, which is
> follow-up work.

### 38.6.4 Electrum connection prioritisation
R38.6.4 The Electrum client shall support configurable minimum and maximum
connected-server counts and shall prioritise servers by their order in the
configured list (including a single-server mode). Block-count queries across
servers shall be performed sequentially rather than all in parallel. Server-
version negotiation shall be race-free. A transaction wait-for-confirmation shall
use a bounded timeout and retry if the tx is not yet on chain. Acceptance: with a
list of servers, connections are bounded and ordered by priority; a downed server
does not stall block-count discovery.

> **Shielded coins are carved out of this client, with one exception.** R38.6.4
> binds every Electrum-backed UTXO coin. A ZHTLC (Zcash-family shielded) coin is
> not one: it is served by its `lightwalletd` endpoints. What serves each of its
> functions is [chapter 39](39-zcash---z_coin-shielded-coin.md) §39.11.1. The
> exception is chapter 39 R39.11.25: when no `lightwalletd` endpoint can answer
> three swap-path questions (already-sent, spend search, spend watch), the coin
> asks the servers of its `electrum_servers`. That client follows R38.6.4's order
> and time budget, opens its connections on first use, and serves nothing else of
> the coin.
>
> **Status update (reloaded, 2026-10-28).** The carve-out is **implemented** since
> 2026-10-12: a Light-mode shielded coin's backend is `lightwalletd` alone, and no
> Electrum connection is opened for it. The exception of chapter 39 R39.11.25
> (maintainer Decision AK of 2026-10-28) is **implemented**: the shielded coin
> builds the shared client on the fallback's first use and asks one server at a time,
> in this requirement's order and per-attempt budget. Read spreading (R38.6.4a) is not
> applied to it.
> Nothing about R38.6.4 itself changes either way.

R38.6.4a *Spreading read-only queries across sessions (Reloaded-original,
maintainer Decision R of 2026-10-27; passed the KDF Dirty Gate on 2026-10-27).*
1. *What spreads.* With `electrum_spread_reads` enabled, the client sends
   each read-only query of a coin to the next connected session in turn,
   round-robin over the sessions R38.6.4 keeps connected (the maintained set
   of R38.6.4b). Read-only queries
   are balance, unspent-list, history, header, block-count, transaction-get,
   fee-estimate and relay-fee requests, and the R38.6.3a batches that carry
   them.
2. *What does not.* Transaction broadcasts, subscriptions and the
   keep-alive ping keep their current routing.
3. *Failover.* A query that fails on its session falls back to the other
   connected sessions, exactly as a query does today (R38.6.4f).
4. *Consistency.* The recently-spent-output cache already shields a read
   that follows this node's own broadcast from a session that has not yet
   seen the broadcast. That shielding shall continue to hold whichever
   session answers the read.
5. *Switch.* `electrum_spread_reads` is a top-level `MM2.json` boolean,
   default `true`. With `false`, every query follows the priority order of
   R38.6.4.

*Rationale (this project's own measurement, 2026-10-26).* The per-session
limit of R38.6.3a applies to each session separately. With three connected
servers, spreading triples the read budget an order-posting burst can draw
on.

> **Upstream divergence (informative).** R38.6.4 prioritises servers by their
> configured order. This sub-rule deliberately departs from that ordering for
> read-only queries. Under `docs/COMPAT_SWITCHES.md`, "The default-side rule",
> this is arm 2: no peer observes it, and it changes only which of the
> operator's own configured servers answers a read.
>
> **Compatibility:** for behaviour matching GLEEC KDF, set
> `electrum_spread_reads` to `false`.

> **As-built note (2026-10-28).** Reloaded implements R38.6.4b–R38.6.4k and the
> spreading of read-only queries in R38.6.4a (PR #128). The "Closed divergence
> (2026-10-27)" note after R38.6.4k records what the tree did before. R38.6.4j
> is a Reloaded-original rule and is marked so. R38.6.4l, the measured server
> order, is implemented: `mixed` and `measured` are recognized, and the default
> stays `random` (D38.6.4l.1).
>
> The spreading is a rotating start index over the maintained connections in
> `route_inner` of the pool, applied to read-only requests only. Each such
> request takes the next value of a per-coin counter, modulo the size of the
> maintained set, and tries the connections from there round the set; a
> request that fails on its connection continues with the next one in that
> order and then with the all-servers retry of R38.6.4f item 2, exactly as
> before. The number of rounds still to make, which R38.6.4j item 2 divides the
> remaining budget by, is counted over the rotated order, so the budget
> arithmetic is unchanged. Directed requests (R38.6.4g) do not use the counter.
> The classification is by the Electrum method name of the request, closed and
> exact: `blockchain.scripthash.get_balance`, `.listunspent` and `.get_history`
> (balance, unspent-list, history), `blockchain.block.header` and `.headers`
> (header), `blockchain.transaction.get`, `blockchain.estimatefee` and
> `blockchain.relayfee`, and `blockchain.headers.subscribe`. The last is the
> block-count query: this client reads the chain tip only through that request,
> uses its answer once and relies on no subscription it places, so it is
> classified as the read it is (the maintainer confirmed this on 2026-10-28). A
> batch is read-only only if every element is.
> Every other method keeps its routing, among them `blockchain.transaction.broadcast`,
> `blockchain.scripthash.subscribe`, `blockchain.contract.event.subscribe`,
> `server.ping`, `server.version`, `blockchain.transaction.get_merkle` and the
> Qtum contract queries (these last two are not on the list of item 1). Item 4
> holds without further code: the recently-spent-output cache is applied to the
> unspent list in the coin layer, after the read, and does not depend on which
> session answered; a test lets the broadcast go to the first server and the
> next two reads be answered by the second and third, which still list the spent
> output. `electrum_spread_reads` is read at coin activation; a value that is not
> a boolean is taken as `true` with one warning (the chapter is silent on it).

R38.6.4b **Pool composition and bounds.** (Reference-bound at the rows
`v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, which agree.)
1. The servers listed in the activation request ([chapter 55](55-coin-activation-wire-contract.md)
   R55.4.3, R55.5.10) form the coin's pool for the coin's lifetime. A server's
   position in the list is its priority; the first entry ranks highest. The
   pool is neither extended nor reordered after activation; a server can only
   leave it (R38.6.4d item 3).
2. Within the pool the client keeps a *maintained set*: the servers whose
   connections it holds open, pings, and routes ordinary requests to. The
   set's size is kept between `min_connected` (the floor; default 1) and
   `max_connected` (the ceiling; default the number of listed servers).
3. The floor shall be at least 1 and shall not exceed the ceiling; a request
   violating either fails activation as unusable connection settings
   (chapter 55 R55.5.14, `Internal`). Neither bound is checked against the
   list length: a ceiling above the list length is accepted and behaves as the
   list length, and a floor above the list length is accepted, can never be
   met, and by itself fails nothing -- the client keeps every server it can
   reach and runs its maintenance on the regular cadence.
4. A shielded coin has no such pool (the carve-out above).

R38.6.4b-R **Server order drawn once at activation (Reloaded-original,
maintainer decision 2026-10-28; amends R38.6.4b item 1 for Reloaded).**
1. The top-level `MM2.json` string `electrum_server_order` selects the
   coin's priority order. Two values are recognized:
   - `random` (the default): building the coin puts the listed servers into a
     uniformly random order once;
   - `listed`: the listed order is kept, as in R38.6.4b item 1.

   An absent switch means `random`. Any other value means `random`, and the
   activation logs one warning naming the switch and the value.

   The selected order is the coin's priority order wherever R38.6.4b –
   R38.6.4k-R speak of priority or rank:
   - the maintained set, and maintenance with displacement (R38.6.4c);
   - routing, including the retry's visiting order (R38.6.4f);
   - the activation probe's offer rule (R38.6.4k-R item 3).
2. The order is held only in the running coin instance. It is not persisted,
   and a re-activation draws a fresh order. A token or coin that reuses a
   platform's Electrum client reuses that client's order.
3. Two further values, `mixed` and `measured`, select the measured server
   order of R38.6.4l and are recognized values like `random` and `listed`;
   the fallback of item 1 applies to every other value. R38.6.4l does not
   change the default, which stays `random`.

*Rationale.* Every wallet ships the same coins file, so ranking by list
position sends every user to the same first server and leaves the later
servers almost unused. A one-time draw spreads users across the listed
servers while each node's own order stays stable, as the EVM node list already
does (chapter 35 R35.1.7).

> **Reloaded divergence (informative).** The reference ranks by list position.
> The draw has no wire effect and no peer observes it: it changes only which of
> the operator's own configured servers a node uses. By the second arm of the
> default-side rule of `docs/COMPAT_SWITCHES.md`, Reloaded's variant is the
> default.
>
> **Compatibility:** for behavior matching GLEEC KDF, set
> `electrum_server_order` to `listed`.

R38.6.4c **Maintenance of the set.**
1. A maintenance pass runs at activation, then every 5 minutes, and at once
   whenever the maintained count falls below the floor.
2. A pass considers the pool's servers that are not maintained, in priority
   order, and attempts to connect each candidate for as long as either the
   maintained count is below the ceiling or the candidate outranks the
   lowest-ranked maintained server. A candidate whose connection is
   established and whose version negotiation succeeds (R38.6.4e item 1) joins
   the set; if the set then exceeds the ceiling, its lowest-ranked member is
   demoted. Once a candidate neither fits under the ceiling nor outranks the
   lowest-ranked member, the pass ends: no lower-ranked candidate can qualify.
3. Within a pass, attempts are made one server at a time, each bounded by
   the establishment timeout of R38.6.4e item 1. The pass does not fail the
   coin; a candidate that cannot be connected is left for a later pass
   (R38.6.4d items 1 and 2) or removed (R38.6.4d item 3).
4. A pass that ends below the floor while candidates remain is repeated after
   a short pause (about one second) rather than after the 5-minute cadence.
   When the floor cannot be met because the pool holds fewer servers than the
   floor, the 5-minute cadence applies.
5. A demoted server is no longer pinged and no longer receives ordinary
   requests. Its connection may be closed at once or left to lapse under the
   idle cutoff of R38.6.4e item 3 (informative: the reference lets it lapse).

R38.6.4d **Suspension and permanent removal.**
1. Every time a server's connection ends or an attempt to establish one
   fails -- a dropped transport, the idle cutoff, a failed connection attempt,
   a request failure that made the client close the connection, or the
   release of a connection opened for a single request (R38.6.4f item 2,
   R38.6.4g) -- the server is *suspended*: maintenance passes do not consider
   it until the suspension lapses. The first suspension -- at activation, or
   after a successful connection -- lasts 20 s; each further disconnection or
   failed attempt before the next successful connection doubles it, up to a
   cap of 12 hours. A successful connection resets the server, so that its
   next suspension is again 20 s.
2. A suspended server is skipped by a maintenance pass unless the pass finds
   fewer unsuspended candidates than it needs to reach the floor; then the
   pass considers suspended candidates as well, still in priority order.
   Suspension governs maintenance only: the all-servers retry of R38.6.4f
   item 2 and a directed request (R38.6.4g) connect a suspended server anyway.
3. A failure that retrying cannot cure removes the server from the pool for
   the coin's lifetime: an address that cannot be parsed into a host and port;
   a transport the build does not support (`WS`/`WSS` on a native build,
   `TCP`/`SSL` in a browser); a TLS address without a usable host name; and a
   version negotiation the server rejects (R38.6.4e item 1). The removal takes
   effect no later than the next maintenance pass, whether the failure was met
   by a pass or by a request. Name-resolution failures, a refused or
   timed-out connection, a TLS handshake failure, and a dropped transport are
   recoverable: they suspend (item 1) and do not remove. A removed server no
   longer counts toward the floor; once every server has been removed, every
   request fails as R38.6.4i describes.

R38.6.4e **Timeouts, version negotiation and liveness.**
1. Establishing a connection -- resolving the address, connecting, the TLS
   handshake where `SSL`/`WSS` is configured, and the protocol-version
   negotiation (`server.version` with the client's supported range) -- shall
   complete within the server's `timeout_sec` (chapter 55 R55.5.10; default
   20 s) or fail as a recoverable failure (R38.6.4d item 1). Negotiation is
   part of establishment: a connection whose negotiation did not succeed is
   never used for another request. A server that rejects the offered range is
   removed (R38.6.4d item 3); a version answer the client cannot read is a
   recoverable failure. Two concurrent attempts to establish the same server's
   connection shall not both connect; the second waits for the first (the
   race-freedom R38.6.4 already requires).
2. A request sent over an established connection shall be answered within
   20 s or fail for that server. `timeout_sec` does not bound requests.
3. A connection over which no response has arrived for 60 s shall be closed
   (idle cutoff), no earlier than 60 s of silence; this counts as a
   disconnection under R38.6.4d item 1.
4. Every 30 s the client sends `server.ping` over every maintained
   connection, concurrently, and waits for all of them; a connection whose
   ping fails is closed (R38.6.4d item 1, R38.6.4f item 1). Connections
   outside the maintained set are not pinged.
5. Informative (Reloaded): a connection attempt whose caller has given up
   finishes in the background, and a connection it makes after its call has
   ended is released at once (R38.6.4d item 1).

R38.6.4f **Routing of ordinary requests.**
1. An ordinary request is tried over the maintained connections one at a time
   in priority order; the first answer ends the request. A connection whose
   attempt fails -- transport failure, or the per-request bound of R38.6.4e
   item 2 -- is closed, which demotes and suspends its server (R38.6.4c,
   R38.6.4d) and, if the set thereby falls below the floor, triggers a
   maintenance pass. A protocol-level error answer from the server is an
   answer, not a transport failure: it ends the request and closes nothing.
2. A request that fails over every maintained connection, or finds the set
   empty, is retried over *all* servers still in the pool, connecting them as
   needed, with up to `max_connected` servers in flight at a time; the first
   answer ends the request. Connections this retry opened to servers outside
   the maintained set are released afterwards (closed, hence suspended under
   R38.6.4d item 1); a losing parallel attempt may be released at once or
   left to lapse under the idle cutoff of R38.6.4e item 3. The order in which
   the retry visits the pool is not bound (informative: the reference visits
   it in no particular order; Reloaded visits the servers not yet tried in this
   call before those that already failed in it, and R38.6.4i still names each
   server once).
3. Informative: while the retry of item 2 connects servers in parallel, a
   maintenance pass may be connecting as well, so the number of open
   connections can briefly exceed the ceiling during this recovery. The
   reference tolerates the exceedance; Reloaded may too, provided the set
   itself never exceeds the ceiling once the recovery ends.
4. `server.ping` is exempt from item 2: a ping that fails everywhere is not
   retried over the pool. Broadcasts, subscriptions and the ping keep the
   routing R38.6.4a item 2 names; with `electrum_spread_reads` enabled,
   R38.6.4a decides on which maintained connection a read-only query starts,
   and the failover of item 1 continues from there.

R38.6.4g **Directed requests.** A request addressed to one named server --
header retrieval from a chosen server, the per-server block-count survey, and
the version negotiation itself -- uses that server's connection, opening it
first if it is closed (within the bound of R38.6.4e item 1) and, if the request
opened it and the server is not maintained, releasing it afterwards (R38.6.4d
item 1). The version-negotiation request never opens a connection; it runs
inside establishment. A directed request naming a server that is not in the
pool fails without any connection attempt.

R38.6.4h **Subscriptions survive a disconnect.** A script-hash subscription
(`blockchain.scripthash.subscribe`) placed over a connection that ends shall be
placed again over a maintained connection -- any one that accepts it -- and
shall keep delivering to the same watchers. While no maintained connection
exists, the client keeps retrying until one does. (Reloaded already re-arms
every watched hash on each newly connected server for R38.6.5; this rule binds
the outcome, not the mechanism.) A message from a server that carries a
`method` is a notification and shall never be taken for the answer to a
request. The Qtum contract-event subscription
(`blockchain.contract.event.subscribe`) is outside this rule; Reloaded does not
place it again after a disconnect, as before.

R38.6.4i **Failure reporting.** A request that failed on every server it was
tried on shall report a failure naming the coin and, for each server tried,
that server's address (`host:port`) together with that server's failure. A
failure that surfaces for one server alone (R38.6.4g) names that server's
address and the coin. The wording is informative; the content is binding.

R38.6.4j **Per-attempt budget derived from the call budget (Reloaded-original,
2026-10-27).**
1. Every client call that may try more than one server carries an overall
   budget *B*. *B* defaults to 60 s -- the figure Reloaded's request path
   already uses as its budget -- and a caller with a tighter deadline may pass
   a smaller one.
2. Before each round of attempts, the round's budget is the smaller of (a) the
   establishment bound of R38.6.4e item 1 plus the per-request bound of item 2
   and (b) (*B* − 5 s) / *N*, where *N* is the number of rounds the call may
   still make: the maintained connections still to be tried one at a time
   (R38.6.4f item 1) plus the retry rounds of R38.6.4f item 2, each of up to
   `max_connected` servers in parallel. The round's budget is never below 5 s
   and bounds everything the round does, connecting included.
3. When *B* is exhausted the call ends with the failure of R38.6.4i even if
   servers remain untried.
4. Item 2 counts the retry rounds as the number of servers in the pool
   divided by `max_connected`, rounded up, and takes the call's *remaining*
   budget as *B*. The 5 s reserve is not spent, so a call whose servers all
   fail ends no later than *B*.

*Rationale.* Under R38.6.4e as the reference applies it, every attempt gets the
same 20 s regardless of how many servers remain, so a call made while several
maintained servers are dead can spend its caller's whole deadline on the dead
ones before reaching a live one. Deriving the per-round budget from *B* and *N*
guarantees that, with *B* intact, every server gets its turn inside the call
and a single live server answers. Item 2 only ever tightens the reference's
bounds; with one server to try, the reference's figures apply unchanged.
This is a Reloaded improvement, not a reference behavior.

R38.6.4k **No wait for the pool at activation.** (Reference-bound.) Building
the coin shall not wait for any connection: the first maintenance pass starts
in the background when the client is created, and the first request after the
build -- the block-height query of chapter 55 §55.5 -- connects the pool itself
through R38.6.4f item 2 if the set is still empty. An unreachable server set is
therefore first observed by that query and reported as chapter 55 R55.5.14 and
§55.3 bind (`Transport`). Nothing in the activation path depends on the floor
having been reached.

R38.6.4k-R **Parallel probe at activation (Reloaded-original, maintainer
decision 2026-10-27; replaces R38.6.4k for Reloaded).**
1. Building the coin starts an *activation probe*: every server in the pool
   is connected in parallel. Each connection is an establishment under
   R38.6.4e item 1, including version negotiation, regardless of
   `max_connected` and of suspension.
2. The build waits until the first server's establishment succeeds, or until
   every attempt has ended, or until the call budget *B* of R38.6.4j (default
   60 s) is spent, whichever comes first. Each attempt is bounded by its
   establishment bound and by what remains of *B*.
3. On the first success the build returns. Each server whose establishment
   succeeded, then or later during the probe, is offered to the maintained set
   by the rule of R38.6.4c item 2: it joins while the set is below the ceiling
   or while it outranks the set's lowest-ranked member, which is then demoted.
   Otherwise it is released at once, which suspends it under R38.6.4d item 1.
   While the probe runs, the open connections may briefly exceed the ceiling,
   as R38.6.4f item 3 allows. Once the probe has ended, the set never exceeds
   the ceiling.
4. A failed attempt suspends its server like any failed attempt (R38.6.4d item
   1). A failure that retrying cannot cure removes the server (R38.6.4d item 3).
5. When no attempt succeeds within *B*, the build fails with a transport-class
   failure. It names the coin and, for each server tried, its `host:port` and
   its failure (R38.6.4i), and is reported as `Transport` (chapter 55
   R55.5.14).
6. The first maintenance pass (R38.6.4c item 1) then runs as usual. The
   block-height query that follows the build is routed by R38.6.4f.

*Rationale.* A wallet maintainer reported activation failing, and the analysis
found the case. With `max_connected` below the number of listed servers (a
desktop wallet sends 1), the routing of R38.6.4f item 2 visits the pool
`max_connected` servers at a time. Each round spends its R38.6.4j budget on the
dead servers listed first, so a live server listed last answers only after
those rounds. Probing every server at once finds the live one in the time of
its own connection, and an operator whose servers are all unreachable gets the
failure, naming each server, from the activation itself.

> **Reloaded divergence (informative).** The reference rows do not wait at
> activation (R38.6.4k). Reloaded's probe is visible only locally: it opens
> one connection per listed server for the duration of the probe and changes
> when an unreachable server set is reported, not how (`Transport` either
> way). No peer observes it, so by the second arm of the default-side rule of
> `docs/COMPAT_SWITCHES.md` there is no switch.

> **Closed divergence (2026-10-27).** The gaps below are closed: Reloaded now
> follows R38.6.4b–R38.6.4k. One consequence of R38.6.4c item 3 is worth
> knowing: the maintenance pass connects candidates one at a time, so with
> silent servers listed ahead of a live one, the live server joins the
> maintained set only after the earlier attempts have run out their
> establishment bound. Requests made meanwhile are answered through R38.6.4f
> item 2 within the budget of R38.6.4j. The list below records the behavior
> before the fix.
>
> **Reloaded divergence (2026-10-27, before the fix).** The tree then did this:
> - *Pool management.* The builder opens the listed servers in configured
>   order up to the ceiling and then waits up to about 5 s for the floor to be
>   reached, swapping a server that has not connected for the next pending one
>   during that wait; activation fails if the floor is still not reached, with
>   an error that lists every configured server. After activation nothing
>   manages the set: there is no priority-driven promotion or demotion, no
>   periodic pass, and a pending server beyond the ceiling is never used. Each
>   opened server reconnects on its own after a drop, with a delay that grows
>   by 5 s per failure up to 60 s; that is a per-server reconnect delay, not
>   the suspension of R38.6.4d, and it never takes a server out of
>   consideration.
> - *Routing.* An ordinary request is tried over the connected servers one at
>   a time; the servers that failed are rotated to the back of the list, so
>   priority is lost after the first failure. There is no retry over all
>   configured servers, and a disconnected server is skipped, not reconnected
>   for the request.
> - *Timeouts.* There is no bound on connection establishment. A request's
>   per-server wait is `timeout_sec` when set -- Reloaded applies it to
>   requests, where the reference applies it to establishment -- and otherwise
>   a 60 s budget divided by the number of servers from that one to the end of
>   the list, so a call can wait well over 60 s. The idle cutoff is 60 s and
>   the ping cadence 30 s, as bound; the ping goes to every connected server.
> - *Version negotiation* runs after the connection is announced, on a
>   separate task; a server whose version is unsupported is removed, as
>   bound, but a request can reach a server before its negotiation ends.
> - *Failure reporting.* A request that fails on every connected server
>   reports the per-server failures without the servers' addresses.
> - *Activation wait.* The up-to-5 s wait for the floor is the divergence from
>   R38.6.4k that chapter 55 R55.5.14's status note already records (the
>   observable discriminant is `Transport` either way). Implementing R38.6.4f
>   item 2 removes the need for it: the block-height query connects the pool.

R38.6.4l **Measured server order (Reloaded-original, maintainer design
approval 2026-10-28; binds the `mixed` and `measured` values of R38.6.4b-R
item 3).**
1. *Values.* With `electrum_server_order` set to `mixed`, the rank is a
   weighted random draw that favors well-measured servers (item 5). With
   `measured`, the rank follows measured quality alone (item 5). Under
   `random` and `listed` nothing in this rule applies: no measurement
   request is sent and no trigger runs. The default stays `random`
   (R38.6.4b-R item 1) until a field run on RIN with `mixed` has passed;
   `mixed`, the intended default, then becomes the default in a separate
   change (D38.6.4l.1).
2. *Measurements.* The header request below is the Electrum protocol method
   `blockchain.headers.subscribe`. Its answer is the server's tip header with
   its `height`, and the session then receives a header notification for
   each new tip. A server's *latency* is the time from sending a request to
   receiving its answer. Measurements happen on three occasions only:
   - *Activation round.* The activation probe (R38.6.4k-R) sends one header
     request over each connection right after its version negotiation. The
     round trip is the server's latency in the round, and the answered
     `height` is its height. The header request is part of the server's
     probe attempt, so the probe ends when every attempt, header request
     included, has ended, or when *B* is spent (R38.6.4k-R item 2); the build
     still returns on the first successful establishment. A server whose
     establishment or header request did not complete during the probe has no
     measurement in the round.
   - *While running.* For each maintained connection, the round trip of
     every ordinary request and of every `server.ping` (R38.6.4e item 4) is a
     sample of its *smoothed latency*. The first sample after the connection
     joins the set starts the average, and every later sample *s* updates it
     to 0.2 · *s* + 0.8 · the previous value. The height of each header
     notification is the connection's current height. Apart from the header
     request a connection sends when it joins (item 6), nothing is sent for
     this.
   - *Re-measure round* (item 7). Each server in the pool that is not
     maintained, not suspended and not removed (R38.6.4d) is connected under
     R38.6.4e item 1 and sent one header request, as at activation. Its
     connection is then released (R38.6.4d item 1), except for the server
     that item 7 chooses as the replacement. Whether these connections are
     made one at a time or in parallel is not bound.
3. *What measuring never does.*
   - It never sends an address-history, balance, unspent-list or other
     script-hash query, and never a fee estimate.
   - A connection opened only to measure carries version negotiation and the
     header request alone. No script-hash subscription is placed on it unless
     it joins the maintained set.
   - Nothing measures on a schedule. Beyond the running samples of item 2,
     servers are measured only in the activation round and in re-measure
     rounds, and a coin starts a re-measure round at most once per 5 minutes.
4. *Eligibility and quality.* For one measurement round:
   - the *best tip* is the highest height measured in the round or currently
     reported by a maintained connection (D38.6.4l.3);
   - the *best latency* is the lowest latency measured in the round.

   A server is *eligible* when all of these hold:
   - it was measured in the round;
   - it is not removed (R38.6.4d item 3) and not poor-demoted (item 7), and
     it was not suspended when it was measured. The release that follows a
     measurement, and the release of R38.6.4k-R item 3, suspend the server
     under R38.6.4d item 1 but do not make it ineligible in that round
     (D38.6.4l.8);
   - its height is at most 1 block below the best tip;
   - its latency is at most the larger of 4 × the best latency and 300 ms.

   The *quality* of an eligible server is *q* = best latency / its latency,
   so the fastest eligible server has *q* = 1.
5. *The rank.*
   - `mixed`: the eligible servers are ranked by a weighted random draw
     without replacement. Each server's weight is *w* = 0.5 · *q* / Σ*q* +
     0.5 / *n*, computed once over the *n* eligible servers. Each place goes
     to one of the servers not yet placed, drawn with probability
     proportional to its weight.
   - `measured`: the eligible servers are ranked by descending *q*. Servers
     of equal *q* are ordered by a uniform random draw.
   - Under both values, the non-eligible servers follow in a uniform random
     order. They stay in the pool for the retry over every server (R38.6.4f
     item 2) and as candidates that can fill the floor (R38.6.4c).

   The result is the coin's rank wherever R38.6.4b – R38.6.4k-R speak of
   priority or rank: the maintained set and displacement (R38.6.4c), routing
   and the retry's visiting order (R38.6.4f), and the probe's offer rule
   (R38.6.4k-R item 3). The rank from the activation round takes effect when
   the probe ends; until then the uniform draw of R38.6.4b-R item 1 serves as
   the rank (D38.6.4l.2). After that the rank changes only by a replacement
   (item 7). The spreading of read-only queries (R38.6.4a) is unchanged.
   R38.6.4b-R item 2 holds for the measured order and its measurements: they
   are not persisted, a re-activation measures afresh, and a token that
   reuses a platform's Electrum client shares that client's rank,
   measurements and re-measure limit.
6. *A poor connection.* A maintained connection is *poor* when one of these
   holds:
   - *P1 stale, two or more maintained connections:* on two consecutive
     header notifications over this connection, its height is 2 or more
     blocks below the highest height any maintained connection has reported.
   - *P1 stale, one maintained connection:* no header notification has
     arrived over it for longer than the larger of 3 × the coin's
     `avg_blocktime` and 10 minutes, counted from its joining or from its
     last notification. A coin without `avg_blocktime`, or with one that is
     not a positive number, uses 10 minutes. For RIN, with an `avg_blocktime`
     of 60 s, the limit is 10 minutes.
   - *P2 slow:* after at least 5 samples since joining, its smoothed latency
     exceeds all three of: 4 × its own reference latency; 2 × the best
     latency of the activation round; and 1 s. Its reference latency is its
     latency in the activation round, or, without one, its latency in the
     re-measure round in which it was drawn. A connection with neither is
     not judged by P2 (D38.6.4l.5).

   A connection that joins the maintained set without a header request sent
   over it, for example one that joined through an ordinary maintenance pass,
   sends one header request right after it joins. Its answer is the
   connection's current height, and the connection then receives header
   notifications like one that joined from a measurement (D38.6.4l.4). P1
   judges only connections whose header request has been answered, and
   counts only those connections. A failing connection is not
   poor: R38.6.4d item 1 and R38.6.4f item 1 already close it and suspend its
   server.
7. *Replacing a poor connection.* On P1 or P2:
   1. The server is marked *poor-demoted* for 15 minutes from the trigger; a
      further trigger restarts the 15 minutes. The mark is not a suspension:
      the server stays in the maintained set and keeps serving, but it is
      not eligible while marked.
   2. The coin starts a re-measure round (item 2). If its last re-measure
      round started less than 5 minutes ago, the round waits until 5 minutes
      have passed, and the poor server stays meanwhile (D38.6.4l.6).
   3. One replacement is chosen from the round's eligible servers: under
      `mixed`, drawn with the weights of item 5 computed over them; under
      `measured`, the one of highest *q*, with equal *q* settled by a uniform
      random draw. The replacement takes the poor server's place in the
      rank, and the poor server moves to the end of the rank; the other
      servers keep their order (D38.6.4l.7). The replacement joins over the
      connection it was measured on, by the join-and-demote rule of R38.6.4c
      item 2, and the poor server is demoted (R38.6.4c item 5).
   4. If the round has no eligible server, the poor server stays maintained
      and the rank is unchanged. The trigger stays armed: while the condition
      still holds, this item runs again under the 5-minute limit of step 2.

   A trigger never reorders the other servers and never redraws the rank.

*Rationale.* A uniform draw spreads users but ignores whether a server is
slow or behind the chain. Measuring through the header request costs one
small request per server at activation, carries no user data, and goes
through the server's own index, so it measures the server and not only the
route; its answer also shows how far behind the server is. `mixed` keeps half
of each weight uniform so that nodes in one region do not all pick the same
fastest server, overload it and then move together to the next one. That
herding is why `measured` is opt-in. The thresholds, the 5-sample minimum,
the 15-minute mark and the 5-minute limit keep one bad sample from moving a
node, and the absence of scheduled probes keeps the load on volunteer-run
public servers where it was.

> **Reloaded divergence (informative).** The reference ranks by list position.
> Measuring adds one header request per listed server at activation and one
> connection per candidate at a re-measure. It has no wire effect and no peer
> observes it: it changes only which of the operator's own configured servers
> a node uses. By the second arm of the default-side rule of
> `docs/COMPAT_SWITCHES.md`, a Reloaded variant is the default; until the
> field run of D38.6.4l.1 has passed, that variant is `random`.
>
> **Compatibility:** for behavior matching GLEEC KDF, set
> `electrum_server_order` to `listed`.

T38.6.4.1 *Bounds and validation.* A test shall activate against an in-process
Electrum test server set with `min_connected` 0, with `min_connected` above
`max_connected`, and with `max_connected` above the list length, and assert
the first two fail as unusable settings (`Internal`) and the third activates
with a set no larger than the list; a floor above the list length shall
activate and keep every reachable server (R38.6.4b).

T38.6.4.2 *Priority maintenance.* With three test servers and `max_connected`
2, a test shall start with the highest-ranked server down, assert the set is
servers 2 and 3, bring server 1 up, trigger a pass, and assert the set is
servers 1 and 2 with server 3 demoted; with the pass cadence shortened for the
test, the same shall happen without an explicit trigger (R38.6.4c items 1–2, 5).

T38.6.4.3 *Floor refill.* With `min_connected` 2 and three servers, a test shall
drop one maintained connection and assert that a pass runs at once and refills
the set from the remaining candidate; with no candidate left it shall assert
the pass repeats on the short pause and that the coin stays usable over the
remaining connection (R38.6.4c items 1, 3–4).

T38.6.4.4 *Suspension.* A test shall make a server fail two connection attempts
and assert that passes skip it for 20 s and then 40 s; then connect it, drop
it, and assert a 20 s suspension; then assert a suspended server is still
connected when the floor cannot otherwise be reached, and by a directed
request (R38.6.4d items 1–2). A test with a long-running clock stub shall
assert the 12-hour cap.

T38.6.4.5 *Permanent removal.* A test shall list an unparseable address, a `WS`
server on a native build, and a server that rejects the offered version range,
and assert each leaves the pool after at most one pass and never counts toward
the floor; a server whose address does not resolve shall stay in the pool and
be suspended (R38.6.4d item 3).

T38.6.4.6 *Timeouts and liveness.* Against a server that accepts the TCP
connection and never answers, a test shall assert establishment fails at
`timeout_sec` (default 20 s) and the server is suspended; against a connected
server that stops answering, a request fails at 20 s and the connection is
closed; a connection idle for 60 s is closed; pings arrive every 30 s on
maintained connections and never on a demoted one (R38.6.4e).

T38.6.4.7 *Routing and retry.* With the set at servers 1 and 2 and server 1
made to fail a request, a test shall assert the answer came from server 2,
server 1 left the set and is suspended, and a pass was triggered if below the
floor; with every maintained server failing, it shall assert the request is
answered by a non-maintained server, that server is released afterwards, and
the set never exceeds the ceiling once recovery ends; a ping failing everywhere
shall not touch the non-maintained servers (R38.6.4f).

T38.6.4.8 *Directed request.* A test shall direct a header request to a
non-maintained server, assert it was connected for the request and released
afterwards, that a version-negotiation request to a closed server opens
nothing, and that a request naming a server outside the pool fails without a
connection attempt (R38.6.4g).

T38.6.4.9 *Subscription re-arm.* A test shall subscribe a script hash, drop the
connection carrying it, and assert the subscription is placed again on a
remaining maintained connection and a later notification reaches the same
watcher (R38.6.4h).

T38.6.4.10 *Failure content.* A test shall make every server fail and assert
the reported failure contains the coin ticker and each tried server's
`host:port` (R38.6.4i).

T38.6.4.11 *Derived budget.* With four servers in the set, the first three
black-holing requests, and *B* 60 s, a test shall assert the call is answered
by the fourth server within 60 s; with *B* 20 s and two dead servers it shall
assert the call fails no later than 20 s and names both servers (R38.6.4j).

T38.6.4.12 *No activation wait.* A test shall activate with every server slow
to accept and assert the build returns before any connection exists, and that
the block-height query then connects and answers; with no server reachable,
the task status shall be `Transport` (R38.6.4k, chapter 55 R55.5.14).
For Reloaded, T38.6.4.12-R replaces it.

T38.6.4.12-R *Parallel activation probe.* Against stub servers, a test shall
assert each of the following (R38.6.4k-R):
- with three listed servers, `max_connected` 1, the first two silent and the
  third live, the build returns after the live server's establishment and well
  inside one R38.6.4j round budget, and the live server is maintained;
- with every server live and `max_connected` 1, after the probe ends exactly
  one server is maintained, the highest-ranked one among those that connected,
  and the others are released and suspended;
- a server whose probe attempt failed is suspended, and one that rejects the
  version range is removed;
- with every server dead, the build fails within *B* with a failure naming the
  coin and each server's `host:port`, reported as `Transport`;
- the build never waits longer than *B*.

T38.6.4.13-R *Server order drawn at activation.* With an injected permutation,
a test shall assert that the maintained set, the routing order, the retry's
visiting order, displacement and the probe's offer rule all follow the drawn
order. With `electrum_server_order` set to `listed`, the listed order is kept.
Each of these settings shall parse as stated:
- an absent switch means `random`;
- `random` means `random`;
- `listed` means `listed`;
- `mixed` and `measured` parse as T38.6.4l.1 states;
- any other value, including a different letter case or a non-string, means
  `random` with one warning.

Two activations shall draw independently of each other. Over many draws with
the real random source, each server shall come first roughly equally often,
within generous bounds (R38.6.4b-R).

The tests of R38.6.4l run in-process against the stub Electrum server of
`mm2src/coins/utxo/rpc_clients/electrum_stub_server.rs`, with the per-method
reply delay, settable tip height and pushed header notifications that the
plan adds to it, an injected random source, and an injected clock or
shortened pool timings. Latencies are set in milliseconds.

T38.6.4l.1 *Switch values.* `mixed` and `measured` shall parse as themselves,
without a warning; an absent switch, `random`, `listed` and every other value
shall parse as T38.6.4.13-R states. Under `random` and `listed`, no stub shall
receive a header request during the activation probe, and no re-measure round
shall run (R38.6.4l item 1).

T38.6.4l.2 *Activation measurement.* Under `mixed` with three stubs, each stub
shall receive exactly one header request during the probe, right after
`server.version`. The latencies and heights the rank is computed from shall be
the stubs' reply delays and tip heights. A silent stub shall have no
measurement (R38.6.4l item 2).

T38.6.4l.3 *Eligibility.* With tips at *H*, *H* − 1 and *H* − 2, the third
server shall not be eligible. With a best latency of 50 ms, 300 ms shall be
eligible and 301 ms not; with a best latency of 100 ms, 400 ms shall be
eligible and 401 ms not. A removed, a poor-demoted and an unmeasured server
shall not be eligible, and a server released after its measurement shall
stay eligible in that round. The non-eligible servers shall be ranked after
every eligible one (R38.6.4l items 4–5).

T38.6.4l.4 *`mixed` weights and draw.* With three eligible servers at 50, 100
and 150 ms, the weights shall be 29/66, 20/66 and 17/66 (about 0.44, 0.30
and 0.26, within 0.0001),
and an injected random source shall yield the order the weighted draw
without replacement gives. Over 2,000 draws with the real random source,
each server's share of first places shall lie within 0.05 of its weight
(R38.6.4l item 5).

T38.6.4l.5 *`measured` order.* With eligible servers at 50, 100, 100 and
150 ms, the server at 50 ms shall rank first and the one at 150 ms last. Two
injected random sources shall order the two 100 ms servers both ways
(R38.6.4l item 5).

T38.6.4l.6 *The rank in use.* With one stub answering its header request
after the others but within *B*, the build shall return on the first
establishment and the probe shall end only after that answer. The rank shall
be the uniform draw of R38.6.4b-R until the probe ends, and the measured rank
afterwards. The maintained set, routing, the retry's visiting order and
displacement shall then follow the measured rank. A re-activation shall
measure again (R38.6.4l item 5).

T38.6.4l.7 *P1 with two or more maintained connections.* With two maintained
stubs, one at height *H* and one at *H* − 2, one notification of the lagging
stub shall not trigger, and two consecutive ones shall trigger. A stub at
*H* − 1 shall never trigger (R38.6.4l item 6).

T38.6.4l.8 *P1 with one maintained connection.* With `max_connected` 1 and no
header notification, a RIN-like coin with `avg_blocktime` 60 shall trigger
after 10 minutes and not before. A coin with `avg_blocktime` 600 shall
trigger after 30 minutes, and one without `avg_blocktime` after 10 minutes.
A notification shall restart the count. A maintained connection over which no
header notification can arrive shall never trigger P1 (R38.6.4l item 6).

T38.6.4l.9 *P2.* With an activation latency of 50 ms for the server and a best
activation latency of 40 ms:
- four samples of 2,000 ms shall not trigger, and the fifth shall;
- after four samples of 50 ms, samples of 2,000 ms shall give smoothed
  latencies of 440, 752 and 1,001.6 ms, and only the third of them shall
  trigger;
- the same three samples shall not trigger for a server whose activation
  latency was 400 ms;
- a connection with no reference latency shall never trigger P2
(R38.6.4l item 6).

T38.6.4l.10 *Poor-demotion.* A triggered server shall stay maintained and keep
answering. A re-measure round 14 minutes after the trigger shall not choose
it even if it is the fastest, and a round after 15 minutes may. A second
trigger shall restart the 15 minutes (R38.6.4l item 7 step 1).

T38.6.4l.11 *Re-measure limit.* Two triggers 1 minute apart shall start their
rounds at least 5 minutes apart. Each candidate stub shall accept at most one
measuring connection per 5 minutes. Over 6 hours of clock time with no
trigger, no stub outside the maintained set shall receive a header request
(R38.6.4l items 3, 7 step 2).

T38.6.4l.12 *Single-slot replacement.* With `max_connected` 2, rank *A*, *B*,
*C*, *D*, *A* and *B* maintained, and *B* triggered, an injected draw choosing
*D* shall leave *A* and *D* maintained and the rank *A*, *D*, *C*, *B*. *D*
shall have joined over its measuring connection without a second connection,
and *C*'s measuring connection shall have been released. Under `measured`, the
candidate of highest quality shall be chosen (R38.6.4l item 7 step 3).

T38.6.4l.13 *No eligible candidate.* With every candidate at least 2 blocks
behind, a triggered server shall stay maintained, the rank shall be unchanged,
and the next round shall start no earlier than 5 minutes later while the
condition holds (R38.6.4l item 7 step 4).

T38.6.4l.14 *Nothing private while measuring.* With a watched script hash and
every trigger above exercised, a stub that was measured but did not join
shall have received only `server.version` and the header request. No stub
shall have received a script-hash method, `blockchain.estimatefee` or
`blockchain.relayfee` over a measuring connection (R38.6.4l item 3).

T38.6.4l.15 *Header request on joining.* Under `mixed`, a server that joins
the maintained set through an ordinary maintenance pass, with no measuring
connection, shall receive exactly one header request right after joining. A
later notification 2 or more blocks below the other maintained connections, on
two consecutive notifications, shall trigger P1 for it. Under `random`, the
same join shall send no header request (R38.6.4l items 1 and 6).

D38.6.4l.1 *(Resolved 2026-10-28, maintainer decision.)* *When `mixed`
becomes the default.* The plan intends `mixed` as the default and left the
moment to the maintainer: with the implementation, or after a field run on a
few coins, RIN first. The maintainer chose the field run. The default stays
`random` until a field run on RIN with `mixed` has passed; the switch to
`mixed` is then a separate change.

D38.6.4l.2 *(Resolved 2026-10-29, maintainer decision: kept as written.)* *The rank during the probe.* The probe offers servers to the set as
they connect (R38.6.4k-R item 3), before the activation round's measurements
are complete. This rule keeps the uniform draw until the probe ends, so the
first maintained set follows that draw, and the measured rank reaches the set
through later maintenance passes. The rule also counts the header request
as part of each probe attempt, which can lengthen the probe, not the build,
within *B*. The alternative, a probe that holds its connections until the
round ends so that the first set follows the measured rank, was not chosen.

D38.6.4l.3 *(Resolved 2026-10-29, maintainer decision: kept as written.)* *The best tip of a re-measure round.* The plan says "any server
reported in the current measurement round". This rule also counts the heights
of the maintained connections, so a candidate behind a server already in use
is never eligible. Counting the round's measurements only was not chosen.

D38.6.4l.4 *(Resolved 2026-10-28, maintainer decision.)* *Header
notifications on every maintained connection.* A connection receives header
notifications only if a header request was sent over it. That holds for one
that joined from a measurement; one that joined through an ordinary
maintenance pass would receive none, never trigger P1 and not count for the
comparison. The maintainer decided that each connection joining under `mixed`
or `measured` without a header request sends one, so P1 covers every
maintained connection (item 6, T38.6.4l.15). The cost is one small request per
joining connection, over a connection that is in use anyway.

D38.6.4l.5 *(Resolved 2026-10-29, maintainer decision: kept as written.)* *The reference figures of P2.* The plan names the server's
"activation latency" and the pool's "best activation latency". This rule
reads them literally, uses the re-measure latency only for a server not
measured at activation, and does not judge a server with neither. The
smoothed latency starts at the first sample after joining, not at the
measurement. Using the latest round's figures was not chosen.

D38.6.4l.6 *(Resolved 2026-10-29, maintainer decision: kept as written.)* *A trigger within 5 minutes of a round.* The plan does not say what
happens then. This rule waits for the limit and does not reuse the earlier
round's measurements.

D38.6.4l.7 *(Resolved 2026-10-29, maintainer decision: kept as written.)* *Where the poor server goes.* The plan says the replacement takes
"that one slot" and the rest keeps its place. This rule moves the poor server
to the end of the rank, so that a later maintenance pass does not bring it
back ahead of the other candidates. Swapping the two places was not chosen.

D38.6.4l.8 *(Resolved 2026-10-29, maintainer decision: kept as written.)* *Suspension by release.* Releasing a measured connection suspends
its server (R38.6.4d item 1), and the plan makes a suspended server
ineligible. Read together, no released server could ever be chosen. This
rule applies the suspension test to the server's state when it was measured.

D38.6.4l.9 *(Resolved 2026-10-29, maintainer decision: kept as written.)* *A warning for an absent switch.* The plan's sentence "An absent
switch, or an unrecognized value, means the default and logs one warning
naming the value" can be read as a warning for an absent switch too. This
rule keeps R38.6.4b-R item 1: no warning when the switch is absent.

> **Coverage note (2026-10-27).** The deterministic tests cover T38.6.4.1 –
> T38.6.4.12 against an in-process stub server set, with two exceptions:
> - The first two cases of T38.6.4.1 (a floor of 0, a floor above the
>   ceiling) are covered at the settings-validation level and its mapping to
>   `Internal`.
> - The `Transport` task status of T38.6.4.12 is covered at the error-mapping
>   level.
>
> An end-to-end run of both through the task RPC needs the Docker test
> environment.

### 38.6.5 UTXO balance event streaming
R38.6.5 For Electrum-backed UTXO coins the project shall emit balance-change
events over the streaming infrastructure, registering the addresses to be watched
at enable time (and re-registering as needed). Acceptance: a balance change on a
watched address produces a streamed balance event.

### 38.6.6 Fee policy, the whole-kilobyte fixed-fee option, and the UTXO trading floors

> **Rewritten 2026-10-25 by the Spec Reader; gated by the KDF Dirty
> Gate in five passes on 2026-10-25, PASS** (see the fee-policy item of
> §38.9). The earlier text of this
> rule described the floor only loosely ("on the order of" ten times the
> fee for about 496 bytes) and called the fixed-fee option a "dingo"-style
> flag without naming its key or its effect. The rules below replace it.
> Both reference designations — `v2.6.0-beta` (netid `8762`) and
> `v3.0.0-beta` (netid `6133`) — agree on every rule in this subsection;
> no `NetConfig` selection is involved.

> **As-built status (2026-10-25, clean-side record).** Partly built.
> Reloaded commit `b43b5530a` implemented a fee-derived floor for non-`BTC`
> UTXO coins, so the Part B note above and the §38.0 scope list, which
> both called this item absent, were stale for the floor. What reloaded
> has and lacks is listed in R38.6.6i. This note describes this
> project's own code and is not corpus-sourced.

R38.6.6a *Fee-policy classification.* A UTXO coin's fee policy is read from
two coins-configuration keys, `txfee` (a JSON non-negative integer, base
units per kilobyte) and `dingo_fee` (a JSON boolean):

| `txfee` | `dingo_fee` | Fee policy |
| --- | --- | --- |
| absent, or not a non-negative JSON integer | any | **plain fixed**, 1,000 base units per kilobyte |
| `0` | any | **dynamic** (the rate is estimated per transaction) |
| a positive integer `r` | `true` | **whole-kilobyte fixed**, `r` per started kilobyte |
| a positive integer `r` | absent, `false`, or not a JSON boolean | **plain fixed**, `r` per kilobyte |

`dingo_fee` has no effect on its own: it changes a fixed fee's arithmetic
only. Every coin with a nonzero `txfee`, and every coin with none, is a
fixed-fee coin for R38.6.6e; only `txfee: 0` makes a coin dynamic. The
option first appears at `v2.6.0-beta`. In the public coins file it is
carried by six entries — `DINGO`, `DEV`, `NYC`, `SHIC`, `TRMP`, `XVG` —
and on every one of them `dust` equals `txfee`.

R38.6.6b *Fee arithmetic.* For a transaction of size `s` bytes (virtual
size where the coin has segregated witness), the fee in base units is:

- *plain fixed `r`:* `floor(r × s / 1000)` — proportional, truncated;
- *whole-kilobyte fixed `r`:* `r × ceil(s / 1000)`;
- *dynamic, estimated rate `e` per kilobyte:* `floor(e × s / 1000)`;
- in every case, if the coin forces the node's minimum relay fee, at
  least `floor(relay_rate × s / 1000)`.

The whole-kilobyte option adds no per-output surcharge of any kind; the
public coins file instead sets `dust` equal to the fee on those coins, so
no output below one kilobyte's fee can be built. A fee preview that must
allow for a change output not yet present adds the increase that a
34-byte change output would cause under the same rule (for the
whole-kilobyte option: one more `r` only if the output pushes the size
into a further started kilobyte). Before `v2.6.0-beta` the reference, like
the baseline anchor (R1) and reloaded today, rounded **every** fixed-fee
coin's size up to a whole kilobyte.

R38.6.6c *HTLC spend-fee estimate.* The fee estimate for spending or
refunding a UTXO HTLC — in the legacy protocol and in the version-two
protocol alike — is R38.6.6b applied to a reference size of 496 bytes,
with no volatility increase for a dynamic rate on a spend the node builds
and broadcasts itself (a spend pre-signed for a watcher does carry the
coin's volatility allowance; [Chapter 09](09-watcher-reward-infrastructure.md)). So a
plain fixed `r` gives `floor(0.496 × r)`, a whole-kilobyte `r` gives `r`,
and a dynamic rate `e` gives `floor(0.496 × e)`. This is the estimate
[Chapter 16](16-swap-v2-pre-burn-output.md) R14A binds for the version-two
spends, and it applies unchanged to the legacy spends. The 496-byte size
first appears at `v2.4.0-beta`; the proportional plain-fixed arithmetic at
`v2.6.0-beta`. A spend or refund whose estimated fee is not smaller than
the HTLC value is refused without broadcasting
([Chapter 60](60-trading-floors-and-refusal-contract.md) R60.5.6).

R38.6.6d *Minimum transaction amount.* A UTXO coin's minimum transaction
amount is its dust amount `D` — the coin's `dust` key (a JSON
non-negative integer, base units) or, without one, 1,000 base units —
expressed exactly as `D / 10^decimals`.

R38.6.6e *Minimum trading volume.* Let `D` be as in R38.6.6d and let `F`
be zero for a dynamic-fee coin and, for a fixed-fee coin, the R38.6.6c
estimate under the coin's fixed policy (`floor(0.496 × r)` for plain fixed,
`r` for whole-kilobyte). The minimum trading volume is
`V = max(10 × D, 10 × F)` base units, reported as the exact rational
`V / 10^decimals`. The only rounding is the truncation inside `F`, before
the multiplication; nothing is rounded afterwards. The value does not
depend on any fee estimate, relay fee, or network state, so it stays
fixed for the life of an order and a swap. It first appears in this form
at `v2.6.0-beta`. In the `v2.5.0-beta`, `v2.5.1-beta` and `v2.5.2-beta`
releases the rule was `10 × D` for every UTXO coin; before `v2.5.0-beta`
it was `10 × D` for every UTXO coin except the ticker `BTC` (R38.6.6f).

R38.6.6f *No ticker exception.* Every ticker follows R38.6.6e, Bitcoin
included. `BTC` and `BTC-segwit` are dynamic-fee coins without a `dust`
key, so their floor is 0.0001 BTC. The baseline anchor (R1) instead fixed the
floor of the exact ticker `BTC` — and of no other ticker, `BTC-segwit`
included — at 0.00777; the reference dropped that exception at
`v2.5.0-beta`.

R38.6.6g *Scope within the UTXO layer.* R38.6.6a–f bind the standard UTXO
coin, the BCH platform coin, the Qtum platform coin, and the transparent
fee fields of a ZHTLC shielded coin ([Chapter 39](39-zcash---z_coin-shielded-coin.md)
R39.5.2). An **SLP token**'s minimum trading volume and minimum
transaction amount are both one smallest token unit, `10^-decimals` of
the token, in both designations. A **QRC20 token** is bound by
[Chapter 57](57-evm-fee-inputs.md) R57.7A.6 (one smallest unit; minimum
transaction amount zero), cleared there on 2026-10-24.

R38.6.6h *Worked values* (from the public coins file's `txfee` / `dust` /
`dingo_fee` values; eight decimals throughout):

| Coin | Fee policy | `dust` | Minimum trading volume | Minimum transaction amount |
| --- | --- | --- | --- | --- |
| KMD | plain fixed 1,000 | default 1,000 | 0.0001 | 0.00001 |
| LTC | plain fixed 1,000 | 5,460 | 0.000546 | 0.0000546 |
| DOGE | plain fixed 1,000,000 | 1,000,000 | 0.1 | 0.01 |
| RVN | plain fixed 1,000,000 | default | 0.0496 | 0.00001 |
| ZEC | plain fixed 100,000 (70,000 in another published copy) | default | 0.00496 (0.003472) | 0.00001 |
| QTUM | plain fixed 400,000 | 72,800 | 0.01984 | 0.000728 |
| DINGO | whole-kilobyte 100,000,000 | 100,000,000 | 10 | 1 |
| BTC, BTC-segwit | dynamic | default | 0.0001 | 0.00001 |

How the values are consumed, and where a value too small for the fees a
swap deducts makes a swap fail, is
[Chapter 60](60-trading-floors-and-refusal-contract.md).

R38.6.6i *Reloaded requirements.* Against R38.6.6a–g reloaded today:

1. computes the fee-derived floor for every UTXO coin except the exact
   ticker `BTC`, which it still fixes at 0.00777 — it shall drop the
   exception (R38.6.6f);
2. rounds the fee term `F` **up** where the reference truncates — it
   shall truncate (R38.6.6e). No entry of the public coins file is
   affected today: every coin whose fee term dominates has a `txfee`
   for which `0.496 × r` is a whole number;
3. has no whole-kilobyte option: it ignores `dingo_fee`, rounds every
   fixed-fee coin's transactions up to a whole kilobyte, and computes the
   floor's fee term proportionally for all — it shall implement R38.6.6a–b
   and the whole-kilobyte `F`. On the six option-carrying entries the
   floor happens to agree today because their `dust` dominates;
4. estimates the legacy HTLC spend fee over 305 bytes, rounded up to a
   whole kilobyte for fixed-fee coins — it shall use R38.6.6c, as its
   version-two spends already do for every policy but the whole-kilobyte
   one;
5. matches R38.6.6d and the SLP values of R38.6.6g.

None of these needs a compatibility switch: items 1, 2 and 4 change only
what this node does to its own orders and transactions, and item 3
repairs, rather than breaks, the version-two spend-fee agreement for a
whole-kilobyte taker coin, on which both peers must compute the same
figure ([Chapter 60](60-trading-floors-and-refusal-contract.md) R60.6.2).
Acceptance: the values of R38.6.6h and the
tests of [Chapter 60](60-trading-floors-and-refusal-contract.md) T60.1–T60.2.

> **As-built note (2026-10-25, clean-side record).** Items 1 and 2 are
> implemented in PR #66: the `BTC` exception is gone and the fee term is
> truncated, with T60.1 (less its whole-kilobyte cases) and T60.2 as
> tests. Items 3 and 4 remain, for the UTXO fee-policy PR of plan
> `trading-floors-parity`. The cleared rule text above is unchanged.
>
> **As-built note (2026-10-25, clean-side record).** Items 3 and 4 are
> implemented in PR #72. `dingo_fee` selects the whole-kilobyte policy,
> plain fixed fees are proportional, the forced minimum relay fee is
> proportional too, and the legacy and version-two HTLC spend estimates use
> R38.6.6c over 496 bytes. The whole-kilobyte floor term is `r`. An explicit
> per-request withdraw fee, which this subsection does not cover, keeps its
> existing per-started-kilobyte meaning. R38.6.6i is now fully as-built.

R38.6.6j *Fee-estimate reuse and short cache (Reloaded-original, maintainer
Decision R of 2026-10-27; passed the KDF Dirty Gate on 2026-10-27).*
1. *One read per request.* An order-posting request — `setprice`,
   `update_maker_order`, `buy`, `sell`, `trade_preimage`, `max_maker_vol`,
   `max_taker_vol`, and the maker balance watch of chapter 60 R60.4.5 —
   obtains a dynamic-fee coin's current fee estimate at most once, and every
   consumer within that request uses the same value: the pre-trade check of
   chapter 60 R60.6.7, the sender and receiver trade fees, and the
   maximum-volume calculation. The same applies to the minimum relay fee of
   a coin that forces it.
2. *Optional short cache.* A fee estimate obtained by an order-posting
   request may be reused by later order-posting requests for the same coin
   for at most `fee_estimate_cache_secs` seconds. This is a top-level
   `MM2.json` integer from 0 to 30, default 15; 0 disables the cache, and a
   value outside the range fails node start with an error naming the key.
3. *Never on swap execution.* A swap's own transactions — payments,
   spends, refunds, and every fee a peer recomputes or validates (chapter 16
   R14A, R17) — always use a fresh estimate. Neither the reuse of item 1
   nor the cache of item 2 applies to them.
4. *Arithmetic unchanged.* R38.6.6b–e are applied to the reused value
   exactly as to a fresh one.

*Rationale (this project's own measurement, 2026-10-26).* Each Electrum
session is served about one request message per second once its burst is
spent (R38.6.3a). Today a single `setprice` with a dynamic-fee leg reads the
same estimate up to three times in sequence.

> **Upstream divergence (informative; passed the KDF Dirty Gate on 2026-10-27).** The reference obtains the
> fee estimate separately for each consumer and caches nothing. This sentence
> rests on a KDF Spec Reader summary that was relayed before its gate pass; the
> Dirty Gate verified it as accurate at both reference designations. Under `docs/COMPAT_SWITCHES.md`,
> "The default-side rule", this is arm 2: no wire effect, the node's own
> requests only. With `fee_estimate_cache_secs` set to 0, a request still
> reuses its single reading (item 1), which reaches the same values, but no
> value is reused across requests.
>
> **Compatibility:** for behaviour matching GLEEC KDF most closely, set
> `fee_estimate_cache_secs` to `0`.

> **As-built note (2026-10-27, clean-side record).** Item 1 is implemented in PR
> #114. A scope entered by each order-posting entry point and by the maker
> balance-update handler shares one reading per coin, for the fee estimate and
> the forced minimum relay fee alike. The scope ends with the request, and
> work spawned outside it reads fresh.
>
> **As-built note (2026-10-28, clean-side record).** Items 2–4 are implemented
> (PR #128). The cache belongs to the node (it lives in the coins context of the
> node's `MmCtx`) and is reached only through a request scope, which each entry
> point now opens with the node's context: code outside a scope neither reads
> from it nor writes to it, so every swap path reads fresh (item 3). Its key is
> the kind of reading and the coin's ticker (the platform coin's, for the gas
> price of an EVM chain); an entry holds the value and the time at which the
> fetch finished, and is reusable while it is younger than the lifetime of the
> coin that asks (R38.6.6k below; a clock that steps back makes it too old,
> never too young). A failed fetch is not stored. The cached value is the value the
> fetch returned, handed to R38.6.6b–e unchanged (item 4). The first consumer of
> a request takes a young entry instead of fetching, and later consumers of the
> same request reuse that value as in item 1, also with the cache at 0.
> The top-level `fee_estimate_cache_secs` is validated at node start; a value
> that is not an integer from 0 to 30 ends the start with an error naming the key. Disabling a
> coin, and registering a coin, drop that coin's entries, so a new instance never
> reads what the previous one cached; a request still running on the old
> instance can store one more value afterwards, which the lifetime bounds.
>
> *The minimum relay fee.* Item 1 says the one-reading rule applies to the
> minimum relay fee of a coin that forces it; item 2 speaks of "a fee estimate".
> The first pass read item 2 literally and cached only the estimate across
> requests. The maintainer decided on 2026-10-28 that the forced relay fee is
> cached across requests too, under the same lifetime as the estimate
> (R38.6.6k); it is implemented that way, as a reading of its own kind with its
> own entry and its own adaptive state.

R38.6.6k *Per-coin cache lifetime (Reloaded-original, maintainer Decisions AD
and AI of 2026-10-28; written on the clean side from those decisions and a
market maker's field report, no forbidden-class input).* The lifetime of a
reading cached under R38.6.6j item 2 (and chapter 57 R57.2.6), for a given
coin, is the first of these that applies:
1. *Per-coin override.* The coin's configuration entry carries
   `fee_estimate_cache_secs`, an integer from 0 to 600; 0 disables the cache
   for that coin. For a token the key is read from the token's own entry, and
   when absent there from its platform coin's entry. A value that is not an
   integer from 0 to 600 fails the coin's activation with an error naming the
   key.
2. *Explicit node setting.* The top-level `MM2.json` key
   `fee_estimate_cache_secs` of R38.6.6j item 2 is present: its value applies
   to every coin without a per-coin override.
3. *Adaptive default.* Otherwise the lifetime adapts to how often the value
   actually changes:
   - It starts at the *start value*. That is the coin's `avg_blocktime` in
     whole seconds (for a token: its own entry's, else its platform coin's),
     raised to at least 2 and lowered to at most 600. A coin with no
     `avg_blocktime`, or one that is not a positive number, starts at
     15 seconds.
   - Each time a fresh reading (one fetched because no reading was young
     enough) equals the previous fresh reading of the same entry, the
     lifetime doubles, up to 600 seconds.
   - When a fresh reading differs from the previous one, the lifetime returns
     to the start value.
   - A failed fetch changes neither the lifetime nor the previous reading.
   - The adaptive state belongs to the cached entry: per coin, and per chain
     for the shared gas-price reading of chapter 57 R57.2.7, whose start value
     comes from the platform coin's entry.

*The forced minimum relay fee (maintainer decision of 2026-10-28).* The
minimum relay fee of a coin that forces it (R38.6.6j item 1) is cached across
order-posting requests like the fee estimate, under the same lifetime.

No source makes a reading live longer than 600 seconds, and none makes it
live forever. R38.6.6j items 3 and 4 hold for every lifetime: a swap's own
transactions always read fresh, and the arithmetic is unchanged.

*Rationale.* A fee estimate or a gas price changes with blocks. A market
maker that re-posts hundreds of orders against one coin within one block
needs one reading, not one per order. A chain with a nominally fixed fee,
such as a validator-set gas floor, reaches the 600-second cap by itself, so
it converges to one reading per 10 minutes without configuration. A changed
value resets the lifetime at once. An explicit node setting ranks above the
adaptive default so that one top-level switch still restores the reference
behaviour.

*Acceptance.* With no top-level key and no per-coin override, the first
lifetime is as follows: up to 600 s for a coin with `avg_blocktime` 600, up
to 2 s for one with `avg_blocktime` 1, and up to 15 s for one without
`avg_blocktime`. A coin starting at 15 s whose fresh readings keep returning
the same value reaches lifetimes of 30, 60, 120, 240, 480 and then 600 s, and
stays at 600 s. Its first differing reading brings it back to 15 s. With
top-level `fee_estimate_cache_secs: 0`, no coin without a per-coin override
reuses a reading across requests. A per-coin override wins over both and does
not adapt. A per-coin value of 601 fails that coin's activation. The forced
minimum relay fee follows the same lifetimes.

> **Compatibility:** for behaviour matching GLEEC KDF most closely, set the
> top-level `fee_estimate_cache_secs` to `0` and set no per-coin
> `fee_estimate_cache_secs`.

> **As-built note (2026-10-28, clean-side record).** R38.6.6k is implemented
> (PR #128), including the adaptive default (Decision AI) and the cached relay
> fee. What is stored where:
> - *The kind of lifetime* is not stored on the coin: the cache asks for it, for
>   the coin that is asking, whenever it reads or stores, and resolves it from
>   the node's `MM2.json` and the coin's entry of its `coins` list by the order of
>   the rule (`fee_estimate_lifetime_of`): a fixed number of seconds (the coin's
>   own key, else the explicit top-level one) or the adaptive default. The
>   platform of a token is the `protocol.protocol_data.platform` of its own
>   entry, which is how the fall-back to the platform's entry of items 1 and 3 is
>   found.
> - *The adaptive state* is a field of the cache entry, which is keyed by the kind
>   of reading and the key coin: the coin itself for a UTXO reading, the platform
>   coin for the shared gas price. An entry holds the last fresh reading, when it
>   was obtained and its current adaptive lifetime in seconds. A new entry starts
>   at the start value of the key coin's entry (`avg_blocktime` truncated to whole
>   seconds and clamped to 2 to 600, else 15; an `avg_blocktime` that is not a
>   positive number is ignored, and for a token the platform's is then used).
> - *A coin on the adaptive default* reuses a reading while it is younger than the
>   entry's adaptive lifetime; a coin with a fixed lifetime judges age against that
>   fixed lifetime and never adapts. A fixed 0 neither reads nor writes.
> - *Equality* is exact equality of the fetched value (`u64` sat/kB, `U256` wei,
>   `BigDecimal` relay fee).
> - *A fixed coin's fetch* is stored in the entry too, and updates the adaptive
>   state: otherwise a coin on the adaptive default would go on reusing a
>   reading that a fixed-lifetime coin has already seen change. A differing value
>   always resets the lifetime to the start value. An equal value doubles it only
>   if the previous reading was at least as old as the entry's lifetime, that is
>   if this really was a fetch that no young enough reading prevented; a fetch that
>   a coin with a short fixed lifetime made earlier, and two concurrent fetches
>   of the same moment, therefore do not count as evidence of stability. A clock
>   that stepped back counts as no evidence either.
> - *A failed fetch* stores nothing and changes neither the lifetime nor the
>   previous reading.
> - *Never on swap execution:* outside a request scope nothing is read or
>   written, the adaptive state included.
> - Disabling or registering a coin drops its entries and with them their
>   adaptive state.
>
> A per-coin value is validated when the coin is activated: the UTXO configuration
> builder, the EVM platform builder and the ERC-20 token builders (the token's own
> entry, else its platform's) fail the activation with an error naming the key.
> Not covered: a QRC20 or SLP token has no fee reading of its own (its fee is read
> from the platform coin's client, keyed and judged by the platform coin), so its
> own entry is neither validated nor consulted. An invalid value found when a
> lifetime is asked for after activation (it cannot normally happen) is logged and
> the coin reads afresh.

### 38.6.7 FIRO Spark verbose-tx support
R38.6.7 The project shall parse FIRO Spark verbose transactions (the
Spark-specific script/output types) so FIRO activates and transacts. Acceptance:
a FIRO Spark verbose tx parses and its details render.

### 38.6.8 Verus-family verbose-tx script labels
R38.6.8 The project shall parse Verus / VRSC-family verbose transactions whose
`scriptPubKey.type` is `cryptocondition`, including vARRR, vDEX, CHIPS, and
other coins using the same verbose transaction shape. Acceptance: a verbose
transaction output with `type: "cryptocondition"` deserializes without falling
back to an error path that skips transaction-history processing.
>
> **Status: as-built on dev (verified 2026-10-26).** The verbose-transaction
> parser reads the `cryptocondition` script label.

### 38.6.9 Node naming and credentials in texts (Electrum servers and the native daemon)

These rules apply the host-only node naming of
[chapter 35](35-evm-v2-activation-rpcs.md) R35.1.14 and the credentials rule
of R35.1.17 to the two backends of a UTXO coin. They bind no wire shape.

R38.6.9a **Electrum servers are named by host and port only (Reloaded;
clean-room original; maintainer decision 2026-10-26; no switch).** Every log
line, error text and status message the UTXO coin layer emits that names an
Electrum server shall name it as `host:port`, the form R38.6.4i already uses
for failure reports. It shall carry nothing else of the configured address: no
userinfo, path, query or fragment, and no URL built from the address. The
protocol (`TCP`, `SSL`, `WS`, `WSS`) may be named next to it. This holds on
the native build, where an address is a host and port, and on the browser
build, where the configured address of a `WS` or `WSS` server can carry more.
In particular:

1. A server address that cannot be reduced to a host and a port (one removed
   under R38.6.4d item 3) is named by a fixed placeholder and its position in
   the activation request's server list, never by the configured string.
2. The WebSocket URL the browser build connects to, and any lower-layer text
   (WebSocket, TLS, DNS) embedded in an emitted text, appear in the
   `host:port` form only.
3. The activation log line for a server that will not be used names the
   server as item 1 or the main rule says.

R35.1.17 applies to the Electrum client, its pool and per-server entries, and
the server entries of the activation request: no rendering of them prints
more of an address than `host:port`.

R38.6.9b **The native daemon (Reloaded; clean-room original; maintainer
decision 2026-10-26; no switch).** A UTXO coin in native mode talks to a local
coin daemon over JSON-RPC with HTTP Basic authentication, built from the RPC
port, user name and password in the daemon's configuration file.

1. Every log line and error text that names the daemon shall name it in the
   host-only form of R35.1.14 (`http://127.0.0.1:<port>`), including a
   lower-layer text embedded in it.
2. No rendering (R35.1.17) of the native client, of a value that holds it, or
   of any value that holds the daemon credentials shall print the RPC user
   name, the RPC password or the `Authorization` header value, whole, in part
   or encoded (D38.6.9a).
3. An error about reading the daemon's configuration file may name the file
   and a missing key, never the value of a key.
4. *(Added 2026-10-29, maintainer decision, D38.6.9b.)* When the daemon
   answers a request with a non-success HTTP status, the error text and any
   log line about it shall not quote the response body or the request's
   parameters. They carry the request's method name, the HTTP status, the
   size of the body in bytes and, when the body is a JSON-RPC response with an
   `error` object, that object's `code` and `message` (the daemon's own error
   code and message), and nothing else of the body. A body that is not such
   a response, for example an HTML page from a proxy, is represented by its
   size only. The same holds for a success answer whose body cannot be
   parsed: its text is never quoted. The shared JSON-RPC client's texts for
   an answer of the wrong shape follow this rule too, so the Electrum texts
   that use them quote no answer either.

None of these rules has an opt-back switch, for the reason R35.1.14 gives: the
backend is the operator's own and no peer observes it, so by the second arm
of the default-side rule (`docs/COMPAT_SWITCHES.md`) the Reloaded form is the
default. The scope limit of R35.1.17 for the RPC wire dump applies. Both are
recorded in `docs/GLEEC_COMPATIBILITY.md` with "none -- no switch exists".

> **Status (2026-10-28).** Implemented. Every log line and error names an
> Electrum server as `host:port` through the shared helper
> (`coins/node_url.rs`), on both builds and on the shielded-coin Electrum
> fallback; an address that cannot be parsed is named by a placeholder and its
> position. An address with userinfo or a path before the port is rejected
> when the server is added. The native daemon client's debug form shows the
> `Authorization` value as set, not its content. The browser build is checked
> to compile; its runtime is not tested.

T38.6.9.1 *Electrum naming.* On the native build, an activation lists a server
whose address carries userinfo (`user:secret@host:50002`) and a valid server
that refuses connections. Every log line, the removal error and the
activation failure contain `host:port` or the placeholder with the list
position, and none contains `user` or `secret`. The debug forms of the client,
its pool and the parsed request contain neither. On the browser build, once a
browser test harness exists, the same holds for a `WSS` address with a path
and a query string.

T38.6.9.2 *Native daemon credentials.* Build a native client from a
configuration file with a known user name and password. Its debug form, and
the debug form of the coin's RPC client, contain neither of them nor the
encoded header value. A request to a closed port fails with an error that
names `http://127.0.0.1:<port>` and nothing more of the endpoint.

T38.6.9.3 *Native daemon response bodies (added 2026-10-29).* Point a native
client at a stub daemon and send a request whose parameters carry a known
marker.
- The stub answers HTTP 500 with a JSON-RPC error object of code -26 and a
  short message, plus a further member that carries a second marker. The
  error text contains the method name, the status, -26, the message and the
  body's size, and neither marker.
- The stub answers HTTP 401 with an HTML page that carries a marker. The
  error text contains the method name, the status and the body's size, and no
  marker.
- The stub answers HTTP 200 with a body that is not JSON and carries a
  marker. The error text contains no marker.

No captured log line contains a marker in any case.

D38.6.9a *(Resolved 2026-10-29, maintainer decision: kept as written.)* *Is
the RPC user name a credential?* R38.6.9b item 2 treats it as one, because it
is half of the Basic-authentication pair. The maintainer kept that reading.

D38.6.9b *(Resolved 2026-10-29, maintainer decision: changed.)* *Response
bodies in native-daemon errors.* A request the daemon answered with a
non-success HTTP status yielded an error that quoted the request and the
response body. The question was shared with chapter 36 D36.3. The maintainer
decided that such an error carries the daemon's error code and message and
the body's size only, as R38.6.9b item 4 now states. Chapter 20 R-N3 states
the same for the Sia wallet daemon.

## 38.8 Software global-HD UTXO accounts & address derivation (T-PORT)

> **Status of §38.8:** required port. The reloaded UTXO HD path can only obtain
> an account extended public key through a hardware device; a **software**
> (non-hardware) global-HD account therefore cannot create an HD account or
> derive HD addresses. This section binds the software-HD UTXO behaviour as
> first-class, testable requirements. It is the coin-side consumer of Chapter 5
> §5.9A (R29–R31), which binds the crypto substrate (software wallet identity,
> software account-xpub derivation and canonical serialisation, and source
> selection by key-pair policy). §38.8 binds the UTXO account bootstrap, the
> `get_new_address` advance, and the scan/gap behaviour for both BIP-44 and
> BIP-84 coins. It cross-references Chapter 5 R15–R20, R29–R31; Chapter 7 R28;
> §38.3.3 (HD purpose generality); and Chapter 45 R45.4.8 (HD-mode identity
> selection).

R38.8.1 **Software-HD storage availability.** When a UTXO coin is activated in
HD mode under a software global-HD account (key-pair policy `GlobalHDAccount`,
selected by Chapter 45 R45.4.8 / Chapter 7 R28; no hardware device present), the
coin's per-wallet HD-account storage MUST initialise successfully using the
software wallet-identity digest of Chapter 5 R29 (the in-context
`RIPEMD160(SHA256(pubkey))` identity, equal to the daemon-wide `mm2_rmd160`).
It MUST NOT be refused on the grounds that no hardware device is connected. Iguana mode remains unsupported for HD (the request is
refused); hardware mode is unchanged. The on-disk identity namespacing
(`mm2_rmd160`, `hd_wallet_rmd160` columns) is unchanged; in software mode both
take the same software-derived value.

R38.8.2 **Account-0 bootstrap at activation.** Activating a software-HD UTXO
coin MUST establish HD account `0` so the coin is usable immediately after
activation. The observable contract matches the existing enable-HD-wallet flow:
HD accounts are loaded from storage at activation, and **if none exist the
default account `0` is created at activation time** (its account-level extended
public key derived in software per Chapter 5 R30 at the configured
`purpose'/coin_type'/0'` path), persisted with its canonical `account_xpub`, and
returned in the activation balance result; if accounts already exist they are
loaded and re-bound. After a successful software-HD activation the coin exposes
at least account `0`. This bootstrap MUST NOT require a `trezor_coin` config
field and MUST NOT contact a hardware device.

R38.8.3 **`get_new_address` advance (software HD).** For a software-HD UTXO coin
the public `get_new_address` RPC MUST advance the **external-chain** (BIP-44
chain `External`) address index under the requested `account_id` (default
account `0`) and return the freshly derived address together with its full
`derivation_path`, its `chain`, and its `balance`. The returned
`derivation_path` MUST carry the coin's configured BIP-43 purpose (44, 49, or 84
per §38.3.3) and an address index one greater than the previous external
known-addresses count for that account; successive calls MUST return successive
addresses. The derived address MUST equal the address a conformant reference
wallet derives from the same mnemonic at the same full derivation path
(`purpose'/coin_type'/account'/0/address_index`), which follows from the
canonical account-xpub contract of Chapter 5 R30. The request/response field
names are the existing wire surface: request `coin`, `account_id`, `chain`;
response `new_address` carrying `address`, `derivation_path`, `chain`,
`balance`.

R38.8.4 **`can_get_new_address` and gap-limit semantics.** The new-address
preconditions and gap-limit accounting that govern hardware HD MUST hold
identically for software HD: an address may be added when within the account's
gap limit of the last used address; scanning for new addresses
(`scan_for_new_addresses`) under account `0` discovers used addresses up to the
gap limit and advances the known-addresses count accordingly. These semantics
MUST work for both BIP-44 (legacy/P2PKH) and BIP-84 (native segwit / P2WPKH)
UTXO coins; the only per-coin difference is the address encoding, not the
derivation or gap accounting.

R38.8.5 **Minimum activation address count and empty-balance shape.** The
API-v2 UTXO activation parameters accept the optional non-negative integer
field `min_addresses_number`. In HD mode, after applying `scan_policy`, each
loaded or newly created HD account MUST have at least that many known
external-chain addresses. Missing addresses are derived at the next contiguous
indices, their known-address count is persisted, and their address,
`derivation_path`, `chain`, and ticker-keyed balance are included in the
activation result. Accounts that already meet the requested minimum MUST NOT
advance. An omitted or zero minimum MUST preserve the existing count and MUST
NOT force an address to be generated. The field has no effect in Iguana mode.

Every HD account's `total_balance` MUST contain the activated coin's ticker
even when the account has no known addresses or every balance is zero. The
empty-account representation is therefore a one-entry ticker-keyed map whose
value is a zero `CoinBalance`, not an untyped empty object. This preserves the
wire shape consumed by wallet SDKs and keeps an intentionally empty HD wallet
distinguishable from a malformed balance response.

> **Cross-reference (informative).** The full wire contract of the UTXO-family
> task activations (`task::enable_utxo`, `task::enable_qtum`,
> `task::enable_bch`) -- request fields, success object including the Iguana
> form, progress values, error set and failure mapping -- and of the legacy
> `enable`/`electrum` methods is bound in
> [Chapter 55](55-coin-activation-wire-contract.md) §55.4–§55.8; the HD
> semantics above are what its success object reports.

R38.8.6 **Tests (two-direction, observable).**
- *Segwit (BIP-84) software-HD activation & advance.* A software-HD UTXO segwit
  coin configured with an `m/84'/<coin_type>'` `derivation_path` activates,
  exposes account `0`, and successive `get_new_address` calls return successive
  external addresses whose reported `derivation_path` carries purpose `84'` and
  an incrementing address index; the returned addresses match those a reference
  wallet derives from the same mnemonic at those paths.
- *Legacy (BIP-44) software-HD equivalence.* A software-HD UTXO coin configured
  with an `m/44'/<coin_type>'` `derivation_path` behaves equivalently
  (activates, exposes account `0`, advances external addresses; addresses match
  the reference wallet).
- *Negative (Iguana refusal).* With the daemon in Iguana (non-HD) mode, an HD
  request — coin activation in HD mode or `get_new_address` — is refused (HD is
  unavailable in Iguana mode); no software HD account is created.
- *Minimum-address activation.* Deserialising an HD UTXO activation with
  `min_addresses_number: 1` and enabling an account with zero known addresses
  persists and returns external address `0`. Re-enabling an account that
  already meets the minimum does not advance it; omitting the field or setting
  it to zero does not create an address.
- *Empty-balance response.* Aggregating no HD address balances still returns
  the activated ticker mapped to a zero `CoinBalance`.

## 38.7 Acceptance criteria (chapter)

- Baseline (Part A) RPCs `sign_raw_transaction` and `consolidate_utxos` behave
  per §38.5; Qtum staking uses `validator_address`; maturity is config-driven.
- KMD reward computation follows R38.4.1–R38.4.1B on every surface of
  R38.4.1A, and the exact-value tests of R38.4.3 pass (issue #11 vector:
  3,488 base units, not 1,744,260).
- Each Part-B item (R38.6.1--R38.6.9) is implemented with the acceptance test
  stated inline, and its coins-config keys / RPC field additions are documented
  alongside the implementation. For §38.6.6 that means the values of
  R38.6.6h and chapter 60 T60.1–T60.2, with every item of R38.6.6i closed.
- §38.8 software global-HD UTXO behaviour (R38.8.1--R38.8.6) is implemented: a
  software-HD UTXO coin (BIP-84 and BIP-44) activates without a hardware device,
  exposes account `0`, honours the requested minimum external-address count,
  returns ticker-keyed balances even for empty accounts, and `get_new_address`
  returns successive external addresses matching a reference wallet;
  Iguana-mode HD requests stay refused.

## 38.10 External References (§38.4)

- KIP-0001, *Reduce the KMD Active User Reward* (Komodo Improvement
  Proposal), <https://github.com/KomodoPlatform/kips/blob/main/kip-0001.mediawiki>.
- Komodo daemon public source: the interest rule in `komodo_interest.cpp`
  (`komodo_interestnew`) and the Season-7 hard-fork height constant
  `nS7HardforkHeight = 3484958` in `komodo_hardfork.cpp`,
  <https://github.com/KomodoPlatform/komodo> (the public node consensus rule
  this project must interoperate with; permitted-input class R3/R4).
- KDF Reloaded issue #11 (KMD swaps fail) and the controlled KMD mainnet
  runs of 2026-09-27: the network rejected an over-claiming taker-fee
  transaction with `16: bad-txns-in-belowout`, and electrum's `interest`
  field reported a much lower allowance (permitted-input class R6),
  <https://github.com/kdf-reloaded/kdf/issues/11>.

## 38.9 Provenance Footer

- *Inputs:* the project's own revision history and current tree, for the
  T-DOC baseline of §38.1-§38.5 (Qtum split/staking-param naming,
  config-driven maturity, baseline address types, KMD rewards/dust
  policy, `sign_raw_transaction`, `consolidate_utxos`, the chain-variant
  model shared with [Chapter 37](37-utxo-spv-and-block-header-validation.md)
  §37.5 -- by public behaviour and config-key shape only, no code
  transcribed); published BIP-341/342 (Taproot), BIP-44/BIP-84 (HD
  derivation), and Electrum protocol documentation (public specifications
  for the §38.6/§38.8 port targets); KIP-0001 and the public Komodo daemon
  interest rule, plus the issue #11 KMD mainnet observations (§38.4,
  §38.10); [Chapter 5](05-hd-wallet-support.md)
  §5.9A (the software-HD crypto substrate §38.8 consumes); [Chapter 7](07-wallet-lifecycle-and-key-export.md)
  (a cross-referenced sibling chapter); [Chapter 45](45-startup-configuration-and-environment-tolerance.md)
  (a cross-referenced sibling chapter).
- *Permitted-input classes used:* baseline/as-built source (the shipped
  UTXO maintenance surface, for the T-DOC majority); external public
  specification (BIP-341/342, BIP-44/BIP-84, the Electrum protocol);
  cross-chapter contracts (Chapters 5, 7, 37, 45). For the §38.6/§38.8
  scope determination itself -- identifying which post-2022 UTXO
  features (PoSV, Taproot, P2PK, connection prioritisation, balance
  streaming, fixed-fee policy, FIRO Spark) exist upstream and are
  genuinely absent from reloaded's baseline -- Forbidden corpus, under
  the chapter-01 two-team clean-room workflow; see below.
- *Sibling-allowlist consultations:* [Chapter 5](05-hd-wallet-support.md)
  §5.9A, [Chapter 37](37-utxo-spv-and-block-header-validation.md) §37.5.
- *Two-team workflow record:* the Spec Reader role (AGENTS.md §2) was
  used on 2026-10-26 for §38.5A (R38.5A.1--R38.5A.8, T38.5A.1--T38.5A.6 and
  the notes under them). It read the reference states `v2.6.0-beta` and
  `v3.0.0-beta` of chapter 02 §2.15A, which agree on every fact bound there.
  **The KDF Dirty Gate passed §38.5A on 2026-10-26**: verdict PASS,
  discretionary-expression measure 0.05 (FAIL threshold 0.10), no leaks to
  scrub, and the chapter 02 §2.15A reference-state check passed. Scope
  attestation, in corpus-free terms: *extent* -- the reference's
  transaction-history surfaces at both states: the legacy per-coin history
  method with its file persistence and ordering, the legacy UTXO collector
  in Electrum and native modes and its transaction-details builder, the
  QRC20 collector, the mmrpc-2.0 history method, the v2 history store with
  its SQLite and browser backends, the UTXO v2 history loop, the UTXO coin
  builder and the activation paths that start either collector; about 18
  source files, plus a mechanical diff between the two states and one
  third-party SQL-builder crate consulted for join semantics; *method* --
  the candidate hunks and the corresponding reference regions read in full,
  a targeted search of the reference for every identifier, number, phrase
  and ordering claim of the candidate, and the inter-state diff; the
  mechanical gate tool was not run because the candidate has no code
  blocks; *measure* -- 0.05. Every identifier the section shares with the
  reference is a wire-visible field, method, error or paging token, or an
  on-disk column described by its behavior. After the gate, the clean
  editor marked R38.5A.3 (b), T38.5A.2 and the persistence bullet of
  R38.5A.8 informative, since write cadence is not wire-observable.
  Earlier, the same role was used, and the Dirty Gate passed, for the
  absence-verification and
  feature-scope determination of §38.6/§38.8 only -- identifying that PoSV support, Taproot output
  handling, P2PK balance/spend, Electrum connection prioritisation, UTXO
  balance event streaming, fixed-fee/min-volume policy, and FIRO Spark
  verbose-tx support exist upstream and are absent from reloaded's
  baseline. The behavioural contracts themselves (R38.6.x, R38.8.x) are
  independently authored specification bound to public protocol
  documentation (BIPs, Electrum protocol) and this project's own
  software-HD substrate, not corpus expression. §38.4 (R38.4.1–R38.4.3)
  was re-derived on 2026-09-27 for issue #11 under the same workflow. The
  reward rule is bound to the public Komodo daemon consensus rule and
  KIP-0001 (§38.10). The corpus was consulted only to confirm that both
  reference lineages apply the same public rule and to supply the
  code-quality finding. No other section of this chapter draws on the
  two-team workflow except §38.5A, above, and §38.6.6, below.
- *Fee-policy item (§38.6.6, 2026-10-25).* R38.6.6a–i, the §38.0 scope
  line and the Part B status note were rewritten by the Spec Reader under
  the two-team workflow at the reference designations `v2.6.0-beta` and
  `v3.0.0-beta`. Corpus-sourced: the classification of R38.6.6a, the
  arithmetic of R38.6.6b–c, the floor rule of R38.6.6e, the removal
  designation of R38.6.6f, the SLP values of R38.6.6g, and every "first
  appears" designation. Not corpus-sourced: the public coins-file values
  of R38.6.6a and R38.6.6h, the baseline's behaviour (R38.6.6b's last
  sentence, R38.6.6f's baseline sentence), and the as-built note and
  R38.6.6i, which describe this repository. **Gated by the KDF Dirty
  Gate in five passes on 2026-10-25** (chapter 01 T1, R40, R43),
  together with chapter 60: each pass PASS, discretionary-expression
  measure 0.02, finding list empty, no remedy class. The clearance covers
  the combined extent below, subject to stated gaps (a)–(c); gap (d) is
  closed. The full attestation is kept in
  [Chapter 60](60-trading-floors-and-refusal-contract.md) §60.12. After
  pass 3, at the gate's request, the last sentence of R38.6.6e was
  reworded to name the releases that had ten times the dust for every
  UTXO coin; pass 4 cleared it. Scope attestation, in corpus-free terms:
  *extent, pass 1* — the trading-floor subsystem, including the
  fee-policy classification, the fee arithmetic with the change-output
  allowance, the HTLC spend-fee estimate and spend and refund refusal
  conditions, compared in full at `v3.0.0-beta` and cross-checked at
  `v2.6.0-beta` for the floor rule (R38.6.6e) and the classification
  (R38.6.6a); *extent, pass 2* — the first-appearance history behind this
  subsection's designations: the spend reference size at `v1.0.0-beta`,
  `v2.0.0-beta`, `v2.3.0-beta`, `v2.4.0-beta` and `v2.5.0-beta`; the
  floor rule and its Bitcoin exception at `v2.4.0-beta`, `v2.5.0-beta`,
  `v2.5.1-beta` and `v2.5.2-beta`; the whole-kilobyte option and
  fixed-fee arithmetic at `v2.5.2-beta` and `v2.6.0-beta`; and the
  spend-refusal condition at the baseline anchor (R1), `v1.0.0-beta`,
  `v1.0.1-beta`, `v2.0.0-beta`, `v1.1.0` and `v1.0.1172` — every
  first-appearance statement of R38.6.6a, R38.6.6c, R38.6.6e and R38.6.6f
  being found consistent; *extent, pass 3* — a final sweep of the whole
  candidate, this subsection included, against the comments, string
  literals and prose documentation of the compared material; *extent,
  pass 4* — the regions changed after pass 3 (as listed by the
  commissioning context, not re-derived by the gate), against the floor
  rule and its Bitcoin exception at each of the thirteen beta releases
  from `v1.0.0-beta` to `v2.4.0-beta` (twelve distinct commits;
  `v1.0.0-beta` and `v1.0.1-beta` share one) and against the comments,
  string literals and changelog of the compared material, finding the
  reworded R38.6.6e sentence consistent; releases of the older lineage
  that predate the baseline anchor (R1) were not compared for it, and for
  the baseline anchor (R1) itself it rests on chapter 60 V60.1; *extent,
  pass 5* — a leak check of that pass's changed lines (the chapter 02
  §2.15A rows and closing note, and the conversion of baseline
  references to the row name), and a chapter 02 R13 check of the whole
  candidate, which found that every state name has a row and that every
  state compared in passes 1–4 matches its row's identifier; *gaps* —
  (a) no later unreleased v3 state was compared in any pass, and none is
  cleared; (b) the order-builder refusals and the spend constructions
  chapter 60 names were token-inspected only, though pass 3's mechanical
  sweep covered them too; (c) the `v2.0.0-beta` designation chapter 60
  §60.1 cites rests on chapter 57 §57.1's earlier clearance, not on this
  one; (d) closed by pass 3, which confirmed that only one rule-text line
  changed after pass 1; *method* — side-by-side reading, a mechanical
  token search (in pass 2 also a count across the designations named),
  and a per-token forced-versus-free test against the compared material,
  the baseline anchor (R1) and this repository; in pass 3, a word six-gram overlap
  test (no overlap), a five-gram near-match check (non-discretionary
  residue only), and a re-reading of this subsection's rule text, and
  every other added line that is not record text, against the pass-1
  reading; in pass 4, side-by-side reading, a mechanical presence count
  of the rule and the exception across the thirteen releases, a word
  five-gram overlap test (no overlap), and the forced-versus-free test
  on the new tokens; in pass 5, the forced-versus-free test on the
  changed lines, a mechanical extraction and table lookup of every state
  name, read-only resolution of each compared name, and a cross-check of
  the new pull-request rows against public merge metadata; *measure* —
  0.02 in each pass, against the standard that any single free
  discretionary match, transcribed body or control flow, or
  discretionary wording fails, and in pass 5 also zero findings against
  chapter 02 R13. *Post-pass metadata normalization (2026-10-25, after
  pass 5):* chapter 02 §2.15A gained two convention sentences. One makes
  *the baseline* and *the anchor* short forms of the row *baseline anchor
  (R1)*; the other makes every table date a UTC date. The date of the row
  `v1.0.4-beta` was corrected to its UTC commit date, and the redundant
  time-zone remarks were removed from the rows `PR #2112 (merged
  2025-03-24)` and `PR #2454 (merged 2025-07-30)`. Dates are metadata,
  not expression and not a state's identity, so no gate pass was run;
  the full record is in chapter 60 §60.12.
- *Electrum connection-pool item (§38.6.4, 2026-10-27).* R38.6.4b–R38.6.4i,
  R38.6.4k, the as-built note before R38.6.4b, the Reloaded divergence note
  after R38.6.4k and T38.6.4.1–T38.6.4.12 were written by the Spec Reader
  under the two-team workflow at the rows `v2.6.0-beta` and `v3.0.0-beta` of
  chapter 02 §2.15A, which agree on every fact bound there (within the
  connection-pool subsystem the only difference between the rows is lint
  attributes; the activation builder differs between the rows in an
  unrelated area).
  Corpus-sourced: the pool and maintained-set model, the floor and ceiling
  defaults and their validation, the maintenance cadence and
  promotion/demotion rule, the suspension figures (20 s first, doubling,
  12-hour cap, reset on connection) and the suspended-server exception,
  the recoverable/irrecoverable classification, the establishment,
  request, idle-cutoff and ping figures, the sequential-then-all routing
  with its concurrency and release behavior, the directed-request
  behavior, the subscription re-arm, the failure content, and the
  no-wait-at-activation rule. Not corpus-sourced: the wire names
  (`min_connected`, `max_connected`, `timeout_sec`, the Electrum protocol
  method strings), which are public, and everything that describes this
  repository (the as-built note, the Reloaded divergence note, the
  parenthetical in R38.6.4h). R38.6.4j is Reloaded-original: the
  derivation of a per-round budget from the call budget was designed in
  this project; the only reference fact it rests on is the equal
  per-request bound of R38.6.4e item 2. The same pass added
  cross-references from chapter 55 R55.4.3, R55.5.10 and R55.5.14 to this
  subsection and two cross-references inside R38.6.4a; those carry no new
  reference content. *Dirty Gate clearance (chapter 01 T1, R40).* PASS on
  2026-10-27, no leaks; every bound fact confirmed at both rows. *Extent:*
  the Electrum connection-pool and connection-management subsystem of the
  UTXO RPC-client layer (connection management, per-server context,
  establishment and request transport, client routing, protocol constants),
  the Electrum branch of UTXO coin activation building, and the shared
  JSON-RPC error wrapper, at `v2.6.0-beta` and `v3.0.0-beta`; nine source
  files read at the later row, plus a mechanical diff between the rows over
  the eight subsystem files, the activation builder and the RPC-client
  module. *Method:* reading of the candidate hunks against the reference,
  plus the mechanical diff between the rows; the gate script was not run
  because the candidate is prose. *Measure:* 0.03 for this chapter and 0.00
  for the chapter 55 cross-references, against the 0.10 threshold. The two
  softenings made afterwards by a context that never read the corpus (the
  idle cutoff "no earlier than 60 s", and the release of a losing parallel
  attempt in R38.6.4f item 2) follow the gate's own advisory notes and add
  no reference content.
- *Reloaded-original additions with the implementation (2026-10-27/28).* A
  context that never read the corpus wrote the following from the
  maintainer's decision of 2026-10-27 and this repository's own code and
  tests:
  - R38.6.4k-R with its rationale and divergence note, and T38.6.4.12-R;
  - R38.6.4b-R (the `electrum_server_order` switch, maintainer decision of
    2026-10-28) with its rationale and divergence note, and T38.6.4.13-R;
  - the Reloaded sentences in R38.6.4e item 5, R38.6.4f item 2, R38.6.4h and
    R38.6.4j item 4;
  - the "no later than" wording of T38.6.4.11;
  - the coverage note after T38.6.4.12-R;
  - the as-built and closed-divergence notes.

  They describe Reloaded's own behavior. They add no reference fact beyond
  those cleared above, and they need no gate pass.
- *Forbidden corpus:* consulted only through the two-team workflow, for
  the items this footer names; never otherwise.
- *Addition of 2026-10-26, cleared by the KDF Dirty Gate.* R38.6.3a was written by
  a session that never read the corpus. Its rationale and acceptance come
  from this project's own Electrum measurements and code reading. Before
  writing it, however, that session had received a KDF Spec Reader's
  behavioural summary of the reference, relayed before any gate pass, and
  the batching it requires coincides with that summary. Its provenance is
  therefore recorded here rather than claimed as clean-room-original.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-10-26, measure
  0.02, no leaks, with the pre-gate summary declared as an input. *Extent:*
  the Electrum-client balance-read and unspent-list-read surface within the
  scope of §38.6.3, at `v2.6.0-beta` and `v3.0.0-beta`. *Method:* reading
  plus mechanical text search for identifiers, phrases and figures; the
  gate script was not run because there are no code blocks. *Measure:*
  0.02 against the 0.10 threshold. The gate confirmed the rationale's rate
  limit measurements as independent of the reference.
- *Reloaded-original additions (2026-10-27), cleared by the KDF Dirty Gate.* R38.6.4a and R38.6.6j were
  written by a session that never read the corpus, from the maintainer's
  Decisions P and R of 2026-10-27 and this project's own Electrum measurements and code reading. The informative notes saying
  the reference reads its fee estimate or gas price per consumer and caches
  nothing rest on a KDF Spec Reader summary relayed before its gate pass, and
  are declared here as that input. *Dirty Gate clearance (chapter 01 T1,
  R40).* PASS on 2026-10-27, measure 0.01, no leaks; the declared input
  was verified accurate at both rows, and the switch names do not occur in the
  reference. *Extent:* the UTXO coin layer's dynamic fee-rate and minimum-relay-fee acquisition surface used by trade-fee, trade-preimage and maximum-volume consumers, and the Electrum client's fee-estimate, relay-fee and server-selection surface, at `v2.6.0-beta` and `v3.0.0-beta`.
  *Method:* reading plus mechanical text search against both rows; the gate
  script was not run because there are no code blocks. *Measure:* 0.01
  against the 0.10 threshold.
- *Reloaded-original addition (2026-10-28), clean side.* R38.6.6k was written
  by a session that never read the corpus, from the maintainer's Decisions AD
  and AI of 2026-10-28, his relay-fee decision of the same day and a market
  maker's field report. It takes no forbidden-class
  input, so chapter 01 R38 is not engaged and no gate pass is required.
- *Reloaded-original addition (2026-10-28), clean side.* R38.6.4l with its
  rationale and divergence note, T38.6.4l.1–T38.6.4l.15, D38.6.4l.1–D38.6.4l.9,
  the amendment of R38.6.4b-R item 3, the matching line of T38.6.4.13-R and
  the sentence on R38.6.4l in the as-built note were written by a session that
  never read the corpus. They rest on the maintainer's design approval of
  2026-10-28 (`docs/plans/electrum-measured-server-order.md`), his decisions
  of the same day on D38.6.4l.1 and D38.6.4l.4, the public Electrum protocol
  and this repository's own code. No reference input: it
  takes no forbidden-class input, so chapter 01 R38 is not engaged and no
  gate pass is required.
- *Reloaded-original addition (2026-10-28), clean side.* §38.6.9 (R38.6.9a,
  R38.6.9b, the status note, T38.6.9.1, T38.6.9.2, D38.6.9a and D38.6.9b) was
  written by a session that never read the corpus, from the maintainer
  decision of 2026-10-26 (host-only node naming everywhere, except the RPC
  wire dump, which keeps its own secrets switch) and this repository's own
  code. It takes no forbidden-class input, so chapter 01 R38 is not engaged
  and no gate pass is required.
- *Reloaded-original addition (2026-10-29), clean side.* R38.6.9b item 4,
  T38.6.9.3 and the resolutions of D38.6.9a, D38.6.9b and D38.6.4l.2, .3 and
  .5 to .9 were written by a session that never read the corpus. They rest on
  the maintainer decision AU of 2026-10-29, the public JSON-RPC error format
  and this repository's own code. They take no forbidden-class input, so
  chapter 01 R38 is not engaged and no gate pass is required.
