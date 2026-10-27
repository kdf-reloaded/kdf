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
  partly built (its as-built note lists what remains), and P2PK (R38.6.3) and
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
> (R38.6.3 and R38.6.8, verified 2026-10-26), and "NOT yet in reloaded" in the
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

### 38.6.4 Electrum connection prioritisation
R38.6.4 The Electrum client shall support configurable minimum and maximum
connected-server counts and shall prioritise servers by their order in the
configured list (including a single-server mode). Block-count queries across
servers shall be performed sequentially rather than all in parallel. Server-
version negotiation shall be race-free. A transaction wait-for-confirmation shall
use a bounded timeout and retry if the tx is not yet on chain. Acceptance: with a
list of servers, connections are bounded and ordered by priority; a downed server
does not stall block-count discovery.

> **Shielded coins are carved out of this client.** R38.6.4 binds every
> Electrum-backed UTXO coin. A ZHTLC (Zcash-family shielded) coin is not one: it
> is served entirely by its `lightwalletd` endpoints, opens no Electrum
> connection, and therefore has no connection pool for this requirement's
> floor/ceiling and prioritisation semantics to govern. What serves each of its
> functions instead is [chapter 39](39-zcash---z_coin-shielded-coin.md) §39.11.1.
>
> **Status update (reloaded).** The carve-out is a **required change, not
> implemented.** Today a Light-mode shielded coin's transparent side *is* backed
> by this client under this requirement's policy, so R38.6.4 does currently bind
> it. The removal is specified in chapter 39 R39.11.1, R39.11.3 and R39.11.7, and
> none of it exists yet; chapter 39's §39.11 status notes record the boundary.
> Nothing about R38.6.4 itself changes either way.

R38.6.4a *Spreading read-only queries across sessions (Reloaded-original,
maintainer Decision R of 2026-10-27; passed the KDF Dirty Gate on 2026-10-27).*
1. *What spreads.* With `electrum_spread_reads` enabled, the client sends
   each read-only query of a coin to the next connected session in turn,
   round-robin over the sessions R38.6.4 keeps connected. Read-only queries
   are balance, unspent-list, history, header, block-count, transaction-get,
   fee-estimate and relay-fee requests, and the R38.6.3a batches that carry
   them.
2. *What does not.* Transaction broadcasts, subscriptions and the
   keep-alive ping keep their current routing.
3. *Failover.* A query that fails on its session falls back to the other
   connected sessions, exactly as a query does today.
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
- Each Part-B item (R38.6.1--R38.6.8) is implemented with the acceptance test
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
