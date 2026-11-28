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

> **Status note (2026-11-18; Spec Reader round 0.5.0-12; passed the KDF
> Dirty Gate on 2026-11-18, measure 0.03).** §38.6.10 is new. It records how the reference reads
> Dash-layout special transactions (a 16-bit type beside a 16-bit version
> and an extra payload after the lock-time) for every UTXO coin, DASH and
> Kerrigan included, what that means for history, balances and swaps, and a
> gap in this repository's reader. No sentence above or below it is changed.

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
- **§38.5B (T-DOC, reference-bound; cleared by the Dirty Gate on 2026-10-30):** the recently-spent
  output record of a UTXO coin -- which paths write it (swap-side sends after
  an accepted broadcast; neither `withdraw` nor `send_raw_transaction`), how
  the unspent listing is composed from the backend answer and the record, the
  rejection shape of a refused broadcast on each RPC path, and the record's
  lifetime -- distilled from the reference states through the two-team
  workflow and cleared by the Dirty Gate (§38.9). Reloaded matched the reference
  here until maintainer Decision BE (2026-10-30): a withdraw's spend is now
  recorded once its broadcast is accepted (R38.5B.9); D38.5B.1--D38.5B.4 hold
  the decisions.
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
  *Status note (2026-11-15, clean side):* the label "NOT yet in reloaded"
  records the state at the audit. By the 0.4.0-beta.1 release the Electrum
  connection pool of §38.6.4 is built, with the staleness checks of R38.6.4o
  and the measured order of R38.6.4l; each rule of §38.6 carries its own
  as-built or status note, and those notes, not this label, give the state.
- **§38.8 (T-PORT, required, built on dev; status corrected 2026-11-04):**
  software (non-hardware) global-HD UTXO accounts -- storage availability,
  account-`0` bootstrap at activation, `get_new_address` advance, and gap/scan
  semantics for BIP-44 and BIP-84 coins -- consuming the Chapter 5 §5.9A
  software-HD crypto substrate. The status note at the head of §38.8 lists
  what remains.

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
constructed transaction without sending it. *(Pointer added 2026-11-07;
passed the KDF Dirty Gate with its target on 2026-11-07.)* Under a global-HD login the
reference serves this RPC, `fetch_utxos` and the automatic merge as
[Chapter 5](05-hd-wallet-support.md) §5.9B R35b records: consolidation and
the automatic merge on the enabled address only, `fetch_utxos` over every
known address. *As built (2026-11-07, clean-side record; plan item H5.3).*
Reloaded serves this RPC, `fetch_utxos` and the automatic merge under a
software global-HD login; the as-built note under Chapter 5 R35b records how.

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
> both merged; ch. 58 §58.5 as-built statement). The sentence remains
> true as a statement of configuration: Radiant still needs no
> `chain_variant`. The cleared rule text above is unchanged.

> **Pointer (2026-11-05).** A second signature-digest mechanism, independent
> of `signature_version` and `fork_id`, is bound in
> [chapter 61](61-utxo-sighash-suffix.md): a coin entry may name a
> sighash suffix that is appended after the sighash type in every
> input digest of the coin (Bitcoin II, from block 57,750). It reaches every
> signing site, the `sign_raw_transaction` RPC included. The cleared rule text
> above is unchanged.

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

> *Reloaded status (clean side, 2026-11-22):* a Solana platform coin and its
> SPL tokens with history enabled are also served by the mmrpc-2.0 method
> from the per-ticker history list, as TRON coins are (chapter 40 §40.6C,
> PR #494).

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

## 38.5C Stored history rows the backend no longer lists (T-DOC, reference-bound)

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-16,
> measure 0.04 (attestation in §38.9).**
> This section was written on 2026-11-16 by the KDF Spec Reader (Spec Reader
> round 0.5.0-9) under the two-team procedure of chapter 01 §1.4A, from the
> reference lineage at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter
> 02 §2.15A (read read-only; no untabled state was read), from the public
> Electrum protocol documentation, from this repository's own tree and from
> this chapter's own text. **The two rows agree on everything in this
> section**: the legacy history collector, the collector of the versioned
> method, the history store and both method tables are the same at both. It
> edits no sentence of §38.5A, §38.5B or §38.6.4Q. Each rule names the rule
> it completes; where a rule here is more exact than the sentence it
> completes, the rule here governs. The section is numbered 38.5C because
> 38.5B was already bound; it stands directly after §38.5A, whose rules it
> completes. Numbering: R38.5C.1 -- R38.5C.5, D38.5C.1 -- D38.5C.2 and
> T38.5C.1 -- T38.5C.10.

*Scope.* What becomes of a stored history row when the backend's listing no
longer contains its transaction hash, or reports it at another height -- the
state a store is in after the coin's server followed a chain that was later
abandoned. Three surfaces: the legacy history file of a UTXO coin
(R38.5C.1 -- R38.5C.2), the SQLite history store of the versioned
`my_tx_history` for the four families whose collector writes it
(R38.5C.3 -- R38.5C.4), and the means of clearing or rebuilding one coin's
stored history (R38.5C.5). The QRC20 token collector is bound by R38.6.4Q.6
item 2, which already says that a stored transaction its listing no longer
contains is dropped; nothing is added for it. The Tendermint collector is
chapter 36's.

R38.5C.1 **Legacy file: every unlisted row is removed (completes R38.5A.3
item (a)).**

1. *Which rows.* In a pass that received the backend's listing, every stored
   row whose transaction hash the listing does not contain is removed --
   confirmed and unconfirmed rows alike. No property of the row is looked at:
   not its height, not its age, not its amounts.
2. *When.* The removal is the first thing done with the listing, before any
   detail request of the pass, and the history is written at once if at
   least one row was removed (R38.5A.3). A pass that did not get a listing --
   the request failed and is repeated, or the pass was skipped under the
   need-to-run test of R38.5A.5 -- removes nothing.
3. *What else.* Nothing else is removed with the row. The row is all the
   legacy collector stores for a transaction; the node's cache of verbose
   transactions is not touched by the removal.
4. *Afterwards.* A removed hash that a later listing contains again is
   treated as a transaction never seen: its details are requested and a new
   row is written, with the height that listing reports.
5. *What the removal rests on.* One successful listing. With an Electrum
   backend that is one answer to `blockchain.scripthash.get_history` for the
   wallet's address, in which an unconfirmed transaction has a height of 0
   or below; the collector reads every such height as 0. The collector does
   not ask a second server before it removes, and it cannot tell a
   transaction that left the chain from one the answering server does not
   know. The rows hold nothing that the backend does not supply, so a row
   removed on a server's incomplete answer returns under item 4.

R38.5C.2 **Legacy file: what a listing does not change (completes
R38.5A.5).** A stored row whose hash is still listed is changed only as
R38.5A.5 says: a height is filled in when the stored one is 0 or the old
sentinel, and the row is fetched again when its timestamp is 0. Therefore:

1. a stored row with a positive height keeps that height and its timestamp
   when the listing reports its hash at a **different** positive height;
2. a stored row with a positive height keeps it when the listing reports its
   hash as unconfirmed;
3. neither case makes a pass necessary under the need-to-run test, so while
   the balance is unchanged no pass runs at all.

R38.5C.3 **SQLite store: an unlisted unconfirmed row is removed (completes
R38.5A.7 and R38.5A.10).** One collector serves standard UTXO coins, Qtum,
BCH and, through the BCH coin, SLP tokens.

1. *A pass and its addresses.* A pass asks the backend for the listing of a
   set of the wallet's addresses: all of them in the first pass and whenever
   the store holds an unconfirmed row for one of them (looked at every 30 s),
   otherwise those whose balance changed; after a pass that was given up,
   the addresses of that pass as well.
2. *Which rows are looked at.* After the listing has arrived and before any
   detail request, the collector takes the stored rows that are unconfirmed
   (stored `block_height` 0) **and** that the address index ties to one of
   the pass's addresses. No other stored row is looked at in the pass.
3. *What happens to each.* Listed at a positive height: the row gets that
   height and the block's time (R38.5A.10). Listed without a height: the row
   is left as it is. Not listed, or its stored hash cannot be read as a
   hash: the row is removed.
4. *What a removal covers.* The row in the history table and the row's
   entries in the address index, in one database transaction. The raw
   transaction kept in the store's transaction cache stays. The tables are
   the three of the coin's store, whose names end in `_tx_history`,
   `_tx_address` and `_tx_cache`.
5. *Afterwards.* As R38.5C.1 item 4: a removed hash that a later listing
   contains is fetched as new.
6. *Failures.* When the block time of a row to be matured cannot be read,
   the pass is given up and repeated after 30 s; rows already handled keep
   their change. A failure of the store ends the collector for the life of
   the coin instance.

R38.5C.4 **SQLite store: a confirmed row is never looked at again (completes
R38.5A.7).**

1. A stored confirmed row whose hash the listing no longer contains
   **stays**. No pass removes it, and the versioned method goes on serving it
   at its stored height, with `confirmations` counted from that height
   (R38.5A.6).
2. A changed height is **not** taken over. A listed hash that the store
   already holds -- in any row -- is passed over without a detail request,
   so a stored confirmed row keeps its height and its time when the listing
   reports the hash at another height or as unconfirmed.
3. The two stores therefore differ for the same event: the legacy file drops
   a confirmed row that is no longer listed (R38.5C.1), the SQLite store
   keeps it. Neither takes over a changed height (R38.5C.2).

> **Code-quality finding (informative; trigger 1 -- apparent when the row
> builder and the collector's row selection are read side by side).** An SLP transaction yields two rows: the BCH row, and a token row
> whose addresses are written in the token address form. The collector
> selects the rows of R38.5C.3 item 2 by the BCH coin's own address form, so
> a token row is never among them. A token row stored while unconfirmed is
> thus neither given its height nor removed, and it stays unconfirmed; and
> when the BCH row of that transaction is removed as unlisted, the token row
> that remains makes the store count the hash as held, so the BCH row is not
> written again if the hash is listed later. R38.5A.10 states what the
> collector is required to do for both rows; the reference does it for the
> BCH row. Proposed fix: select the unconfirmed rows of the store by
> transaction hash or without an address condition, and remove or mature all
> rows of one hash together. *Reloaded (this repository on dev, 2026-11-16):*
> its BCH and SLP collector takes every unconfirmed row of the store,
> without an address condition, so it has no such gap.

R38.5C.5 **Clearing or rebuilding one coin's stored history.**

1. *Requests.* Neither method table routes, at either row, a method that
   clears, rebuilds or reads again one coin's stored history, for the file or
   for the SQLite store. Both forms of `my_tx_history` only read.
2. *Activation.* No activation member of these families asks for the stored
   history to be cleared or rebuilt. `tx_history` true starts the collector
   over whatever is stored, and the collector asks for details only of
   hashes it does not hold and of rows that R38.5A.5 has it fetch again.
3. *Start of the legacy collector.* The file is read once, when the
   collector starts; from then on the collector works from memory and every
   write replaces the file from memory (R38.5A.3). A file that cannot be
   decoded is deleted and the history is built again from the backend
   (R38.5A.4). On a native build one conversion of rows written by old
   versions runs once per coin and address and is recorded in a marker file;
   it rebuilds nothing. The file is `<ticker>_<address>.json` in the
   directory `TRANSACTIONS` of the node's database directory, with a `:` in
   the address written as `_`, and the marker is
   `<ticker>_<address>_migration` beside it.
4. *Start of the versioned collector.* The three tables are created when
   they are absent. They are never emptied and never dropped.
5. *What a user has.* The reference offers a user nothing to reset the
   history of one coin. What follows from items 3 and 4 is by hand only: the
   legacy file can be deleted while the coin is not active, and the next
   activation with `tx_history` true then builds it again (deleted while the
   collector runs, it comes back at the collector's next write, unchanged);
   the SQLite store has no file of its own per coin, so its rows can be
   removed only with a database tool while the node is stopped. Neither is
   documented behavior of the reference, and the second was not tried.

> **Compatibility (this repository on dev, 2026-11-16; its own code, not the
> reference).**
>
> 1. *Legacy file.* Reloaded's legacy UTXO collector removes **no** row: a
>    stored row whose hash the listing no longer contains stays in the file,
>    confirmed or not, and is served. This does not conform to R38.5A.3
>    item (a) and R38.5C.1. The baseline did not remove such rows either.
> 2. *Versioned method, standard UTXO and Qtum.* Reloaded serves it from the
>    legacy file (the divergence note under R38.5A.7), so item 1 holds for it
>    as well.
> 3. *Versioned method, BCH and SLP.* Reloaded's collector removes an
>    unlisted unconfirmed row and matures a listed one, for the BCH row and
>    the token row alike (the finding above); a confirmed row is not looked
>    at again, and a listed hash the store holds is passed over. **Parity**
>    with R38.5C.4.
> 4. *Changed height.* Reloaded's legacy collector fills a height in only
>    when the stored one is 0 or the sentinel. **Parity** with R38.5C.2.
> 5. *Reset.* Reloaded has no request, activation member or start-up step
>    that clears one coin's history. **Parity** with R38.5C.5.
>

D38.5C.1 *(Open, 2026-11-16.)* **A stored row for a transaction that is no
longer on the chain.** Reproducing the reference, the versioned method keeps
serving a confirmed row whose transaction the backend no longer lists
(R38.5C.4 item 1), and Reloaded's legacy file keeps every such row
(compatibility note, item 1); the row states a payment the chain does not
hold. Options: (a) the reference as it stands: the legacy file drops every
unlisted row, the SQLite store drops unconfirmed ones only; (b) one rule for
both stores: a row is removed, confirmed or not, when a successful listing
that covers **every** own address the row concerns does not contain its
hash, and all rows of one hash are removed together; (c) option (b), and in
addition no row is removed while the coin is in the state of R38.6.4o item
12, in which its servers disagree about the chain. *Recommendation:* (c).
Under each option Reloaded's legacy collector first has to meet R38.5A.3
item (a).

*Status note (2026-11-16, clean side; maintainer decision). D38.5C.1 is
decided:* option (c), with a Reloaded refinement for how absence is
established. It applies to the legacy history file, the QRC20 token
collector (in place of R38.6.4Q.6 item 2) and the SQLite store alike, and
replaces R38.5C.1 and R38.5C.3 items 2 and 3 for removal. For confirmed rows
of the SQLite store it is a recorded departure from R38.5C.4 item 1.

1. *When a row is removed.* A stored row, confirmed or not, is removed only
   when **two successful listings in a row**, at least one block apart, both
   answered by up-to-date servers (R38.6.4o item 4), do not contain its
   transaction hash. The two listings must together cover every own address
   the row concerns. All rows of one transaction hash (the several rows of a
   QRC20 transaction, the BCH row and the SLP token row) are removed together.
   A coin with a single connected server needs two listings from that server,
   at least one block apart.
   "One block apart" is judged from the chain tips observed with the two
   listings: for an Electrum server, its last reported tip; for a native
   daemon, its block count read with the listing.
2. *What never removes.* A failed listing, a listing cut off by its time
   budget or by a `history too large` answer, a listing from an out-of-date
   server, and any listing while the coin is in the disagreement state of
   R38.6.4o item 12.
3. *Rows cannot stay forever.* A row found absent once carries a mark: the
   height at which its absence was first seen. While any row carries such a
   mark, the collector's need-to-run test (R38.5A.5) is true, so the second
   listing comes at the next pass after a new block, not only at a balance
   change. A listing that contains the hash clears the mark. What holds a
   removal back is named in item 2, plus the absence of a new block. Each ends
   by itself when its cause ends; none needs a user action.
   The marks are kept in memory only; after a restart of the node the
   counting starts again, which can delay a removal but never causes one.
4. *Write and log.* A pass that removed rows writes the history at once
   (R38.5A.3) and logs one INFO line naming the coin and the number of rows
   removed. No line is logged per row.
5. *Afterwards.* A removed hash that a later listing contains is fetched as
   new, as in R38.5C.1 item 4.

D38.5C.2 *(Open, 2026-11-16.)* **A stored confirmed row whose transaction is
now at another height.** Neither store of the reference takes the new height
over, and no pass runs for it while the balance is unchanged (R38.5C.2,
R38.5C.4 item 2); the row then reports a height, a time and a count of
confirmations that are not those of the transaction. Options: (a) the
reference as it stands; (b) in a pass that runs, a listed positive height
that differs from the stored one is taken over together with the block's
time, and a stored confirmed row that the listing reports as unconfirmed
goes back to height 0; (c) option (b), and a pass is also started when the
coin's client sees that the chain it follows has changed, since the
need-to-run test alone does not start one. *Recommendation:* (c); what
makes the client see the change is this repository's own design.

*Status note (2026-11-16, clean side; maintainer decision). D38.5C.2 is
decided:* option (c), in two steps. It is a recorded departure from R38.5C.2
and R38.5C.4 item 2 for both stores.

1. *In a pass that runs.* A listed positive height that differs from a
   stored row's height is taken over, together with that block's time. A
   stored confirmed row that the listing reports as unconfirmed goes back to
   height 0. All rows of the hash change together.
2. *Starting a pass.* A pass is also started when the coin's client sees
   that the chain it follows has changed. One WARN line names the coin and
   the height from which the chain changed, if known. How the client sees
   the change is this repository's own design, specified when it is built;
   until then, item 1 applies in the passes that run anyway.

*Amendment (2026-11-18, clean side; Reloaded-original; implements item 2 of
the status note above). How the client sees that its chain changed.*
1. *The chain view.* Each Electrum client keeps the base headers (the first
   80 bytes, as compared by item 12 of R38.6.4o, D38.6.4o.14) of the last
   2*T* + 2 heights of the chain it follows, *T* being the coin's tolerance
   (R38.6.4o item 1). The view is fed only by header notifications and
   header answers of up-to-date maintained connections (R38.6.4o item 4), and
   not at all while the coin is in the disagreement state (item 12). No block
   identifier is computed: the identifier function differs between header
   families, and comparing base headers at the same height works for all of
   them.
2. *One check per new tip.* When an up-to-date connection first reports a
   height above the highest in the view, the client sends that connection one
   directed `blockchain.block.headers` request (R38.6.4g) for the heights from
   the highest in the view up to the one below the new tip. It compares each
   returned base header with the view at the same height.
   - All equal: the chain only grew; the view takes the new headers, the new
     tip's included, and drops the oldest.
   - A difference: the chain changed. One more directed request over the
     whole view finds the lowest height that differs, the height from which
     the chain changed; a difference at the bottom of the view is reported as
     "at or below" its lowest height. The view is then replaced by the new
     chain's headers.
   - A request that fails or is not answered within the per-request bound
     concludes nothing; the next tip is checked as usual.
3. *What a change starts.* The client sends a chain-changed event to every
   history collector of the coin instance: the legacy UTXO collector, the
   QRC20 collector and the BCH/SLP collector. The event makes the collector
   run a pass at once, whatever its need-to-run test says. That pass applies
   item 1 of the status note above and counts absences under D38.5C.1.
4. *The log.* One WARN line per change: `<ticker>: the Electrum servers report
   a reorganization of the chain from height <H>` (or `at or below height
   <L>`).
   Nothing is said about consequences. Every change is reported, a change
   back to a chain the view held before included, as in a race of two blocks
   at the tip: each is a reorganization.
5. *Cost and bounds.* At most one small directed request per new block per
   coin instance, plus one when a change is found, always to a connection
   that is already maintained. A chain that only grows always compares equal,
   so no header family can make every block look like a change. A single
   server answering a made-up header can cause at most one extra history pass
   and one WARN line. It cannot remove a row, which needs two listings under
   D38.5C.1.
6. *One connected server.* The same applies to that server's headers alone.
7. *Edges.*
   - When the view is empty, or a new tip is more than 2*T* + 2 heights above
     its highest, the view starts again from the new tip, with no request, no
     event and no WARN: there is nothing to compare with.
   - A tip at or below the view's highest height is not checked. A
     replacement at the same height is therefore seen when the next higher
     tip arrives.
   - A header list that cannot be split into equal parts of at least 80 bytes
     matching the requested count concludes nothing. A coin whose servers
     never return such a list therefore never sees a change, and relies on
     the passes that run anyway.
   - With `electrum_tip_checks` `false` (R38.6.4o item 11) the view is not
     fed and no change is seen; the history then relies on the passes that
     run anyway.

*Acceptance rows for a mock backend.* Each row names what the mock answers
and what the node does. *Parity* is the reference behavior bound above;
where a D-item would change the expected result, the row says so.

T38.5C.1 *Legacy file, unlisted rows.* With a stored history of three rows --
one confirmed, one unconfirmed, one confirmed -- and a listing that contains
the hash of the third only, one pass shall leave a file with the third row
alone, written before the mock receives any detail request of that pass.

T38.5C.2 *Legacy file, failed listing.* With the same stored history and a
mock that answers the listing request with an error, the file shall be
unchanged after the pass and the request shall be repeated.

T38.5C.3 *Legacy file, a hash comes back.* After T38.5C.1, a balance change
and a listing that contains a removed hash again shall produce one detail
request for it and one row with the height the listing reports.

T38.5C.4 *Legacy file, changed height.* With a stored confirmed row and a
listing that reports its hash at another positive height, in a pass that
runs because the balance changed. Parity: the row keeps its height. Under
D38.5C.2 (b) the row takes the listed height and that block's time and moves
to its place in the order of R38.5A.1.

T38.5C.5 *SQLite store, unlisted unconfirmed row.* With a store that holds
one unconfirmed row and a listing without its hash, one pass shall remove
the row and its address-index entries; the versioned `my_tx_history` then
reports a `total` lower by one, and the raw transaction is still in the
transaction cache.

T38.5C.6 *SQLite store, unlisted confirmed row.* With a store that holds one
confirmed row and a listing without its hash. Parity: the row is served
unchanged after any number of passes. Under D38.5C.1 (b) it is removed in
the first pass whose listing covers the row's own addresses.

T38.5C.7 *SQLite store, changed height.* With a stored confirmed row and a
listing that reports its hash at another positive height. Parity: no detail
request is made for the hash and the row is unchanged. Under D38.5C.2 (b)
the row takes the listed height and that block's time.

T38.5C.8 *SLP token row.* With a BCH store that holds the BCH row and the
token row of one unconfirmed SLP transaction: (a) a listing that reports the
hash at a positive height shall give **both** rows that height and the
block's time; (b) a listing without the hash shall remove both rows, and a
later listing with the hash shall write both again (R38.5A.10; the finding
under R38.5C.4).

T38.5C.9 *Rebuilding the legacy file.* With the coin not active and its
history file deleted, an activation with `tx_history` true shall request the
details of every listed hash and write a file equal, row for row, to the one
a first activation writes. With the file replaced by bytes that do not
decode, the same shall hold (R38.5A.4).

T38.5C.10 *Activation does not rebuild.* With a non-empty stored history
that holds every listed hash complete, an activation with `tx_history` true
shall make one listing request and no detail request, and shall leave the
stored history as it was (R38.5C.5 item 2).

*Not established.* (1) Native mode was read only as far as the listing is a
source; whether a daemon's wallet listing still contains a transaction that
left the chain was not examined. (2) The browser (WASM) form of either store
was not examined. (3) What the cache of verbose transactions holds for a
transaction that left the chain, and what reads it, was not followed beyond
the fact that the legacy collector's removal does not touch it. (4) The
finding under R38.5C.4 and the hand procedure of R38.5C.5 item 5 for the
SQLite store were read, not reproduced in a run. (5) Nothing was measured
against a running reference binary or a live server.

## 38.5B Spent-output bookkeeping across user withdrawals and broadcasts (T-DOC, reference-bound)

> **Cleared (chapter 01 R40).** R38.5B.1--R38.5B.8, D38.5B.1--D38.5B.3,
> T38.5B.1--T38.5B.5 and the notes under them were written on 2026-10-30 by the
> KDF Spec Reader under the chapter-01 two-team workflow (§1.4A), from the
> reference states `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A (which
> agree on every fact bound here) and from the row *baseline anchor (R1)*, and
> were cleared by the KDF Dirty Gate on 2026-10-30 (record in §38.9). The
> field observation and the decision items describe this repository and the
> maintainer's choices, not the reference.

> **Scope.** A UTXO coin keeps, per activated coin instance, an in-memory
> record of the outputs this node has recently spent by transactions it built
> and broadcast itself, together with the outputs those transactions created
> that pay back to the coin's own address script (its change). The unspent
> listing consults the record so that an output already spent by this node --
> but still listed by a backend that has not yet indexed the broadcast -- is
> dropped from the list and the spending transaction's own-script outputs are
> offered in its place. The record exists at the row *baseline anchor (R1)*
> and in this repository; this section binds which paths write it, how the
> listing is composed from the backend answer and the record, what a backend
> rejection of a broadcast looks like on each RPC path, and the record's
> lifetime. It binds no wire shape beyond the legacy error body of R38.5B.6.
> The field names of the `withdraw` request and the `send_raw_transaction`
> carriers are bound by [Chapter 49](49-withdrawal-task-path.md) and
> [Chapter 20](20-siacoin-integration.md) R-W8, not here.

R38.5B.1 **Who writes the record.** (Reference-bound at both rows; also the
baseline's behaviour.) A spend is entered into the recently-spent record only
by the coin layer's own build-and-broadcast operations: the sends from the
coin's address that the swap engine orders (payments, fee outputs, spends of
own-address inputs during a swap) and the UTXO merge operation of R38.5.2 when
it is asked to broadcast. The entry is made **after** the backend has accepted
the broadcast, never at build or signing time; a broadcast the backend rejects
enters nothing. The entry records the inputs the transaction consumed and
those of its outputs that pay to the coin's own address script; outputs to
any other script are not recorded.

*Clarification (2026-11-15, clean side).* "Only by" stays true, but not every
send the swap engine orders is entered. A QRC20 token's swap payment, spend,
refund and fee transfer neither hold the token's record lock through the
broadcast nor enter anything, in this tree as in the reference (R38.6.4Q.7
items 3 and 6). For the QTUM family the record therefore shields only a QTUM
swap send that follows a QTUM swap send. The options are in D38.6.4Q.4.

R38.5B.2 **Withdraw neither writes nor broadcasts.** (Reference-bound at both
rows; also the baseline's behaviour.) The user withdraw path -- the direct
`withdraw` method and the `task::withdraw::*` family of
[Chapter 49](49-withdrawal-task-path.md) -- obtains its candidate inputs through
the same listing as every other spend (R38.5B.4) and therefore honours the
record, but it does not write to it: not when the transaction is built, not
when it is signed, and not afterwards. For a UTXO coin the withdraw path has
no broadcast step of its own at either row, so no broadcast-time hook exists
either; the request's `broadcast` member (chapter 49 R49.1, a coin-family
field) is not consumed by the UTXO family, and the external-wallet signing
route of the UTXO family at the later row never asks the external wallet to
broadcast. The signed transaction is returned to the caller, who broadcasts
it (or does not) by a separate call. Two withdraws issued in succession
without an intervening broadcast that the backend has indexed therefore
select from the same candidate set and may spend the same inputs; the
reference does not prevent this.

R38.5B.3 **`send_raw_transaction` does not write the record.**
(Reference-bound at both rows; also the baseline's behaviour.) The legacy
`send_raw_transaction` method decodes the supplied carrier and hands the
transaction bytes to the backend's broadcast call. It does not parse the
transaction, does not determine which of the node's own outputs the
transaction spends or creates, and writes nothing to the recently-spent
record on success or on failure. There is no mmrpc-2.0 broadcast method for
UTXO coins at either row, and the only mmrpc-2.0 paths that produce a UTXO
transaction (`withdraw`, `task::withdraw::*`) do not broadcast (R38.5B.2).
Consequently a withdraw-then-broadcast pair performed by a client leaves the
record exactly as it was: the inputs that pair spent stay eligible for the
next listing until the backend itself stops listing them.

> **Reloaded divergence (maintainer Decision BE, 2026-10-30).** R38.5B.2 and
> R38.5B.3 describe the reference and this repository before Decision BE. They
> no longer hold for a UTXO coin's own withdraw followed by
> `send_raw_transaction` of that withdraw's bytes: R38.5B.9 records the spend
> at the accepted broadcast. That a withdraw writes nothing to the record at
> build or signing time, and that a broadcast of bytes the node did not build
> writes nothing, still hold.

> **Re-verified 2026-11-03 (KDF Spec Reader; same rows; passed the KDF
> Dirty Gate on 2026-11-03, measure 0.00, see §38.9).** Asked whether the reference's withdraw records the
> inputs it spent, or its unconfirmed change output, so that a second
> withdraw issued seconds later excludes the spent inputs and may spend the
> change: it does not, at either row. The one mechanism the reference has
> is the in-memory recently-spent record of R38.5B.1 — a **local
> spent-outpoint record**, keyed to the coin's single address script, with
> the lifetime of R38.5B.8 (the coin instance; no expiry, no size bound) —
> and the withdraw path neither writes it nor consults the backend's
> mempool; its candidate set is the backend's live unspent listing filtered
> by that record (R38.5B.4). A second withdraw therefore excludes the first
> one's inputs only once the backend's own listing stops showing them (an
> Electrum server that has indexed the client's broadcast), and may spend
> the unconfirmed change only once the listing shows it; the withdraw's
> process-wide build lock covers building and signing only (R38.5B.5) and
> orders nothing after the signed transaction is returned. Reloaded's
> R38.5B.9 (Decision BE) is the divergence that closes this for a withdraw
> whose bytes the node itself broadcasts; two withdraws both built before
> the first broadcast still select from the same set, as in the reference.

R38.5B.4 **Composition of the unspent listing.** (Reference-bound at both
rows; also the baseline's behaviour.) Every listing a spend is built from --
the single-address list and the multi-address map, in their all-outputs and
mature-only forms -- is composed as follows, in order:

1. a fresh query to the backend for the address's unspent outputs (Electrum
   `blockchain.scripthash.listunspent`, or the native daemon's unspent
   listing); the answer is not cached and no earlier answer is reused;
2. under the record's lock, every listed output the record knows as spent by
   this node is removed and the record's replacement outputs for it are
   added; the replacement is applied again to what was added, until no listed
   output is known as spent (a chain of own spends resolves to the last
   change output);
3. sorting into the coin's canonical input order and removal of duplicates.

Because step 1 is a live query, a listing issued after a broadcast reflects
the backend's view at that instant. For an Electrum server that has indexed
the transaction, the view already omits the spent inputs and lists the
unconfirmed change; until then it still lists the spent inputs. Nothing in
the coin layer refreshes or invalidates a listing after a broadcast: the next
caller simply queries again. For spendability, an unconfirmed output that is
not a coinbase output counts as mature; only coinbase outputs are withheld
until the configured maturity (R38.2). An unconfirmed change output is thus
eligible as soon as the backend lists it.

*Status note (2026-11-20, clean side; maintainer decision).* In this tree the
maturity check of a listing does not fetch the transaction of an output that
the record knows as an output of a transaction this node built, for every
UTXO coin, whether step 2 added it or the backend listed it. Such an output
belongs to a transaction this node built and signed, so it is never a
coinbase or coinstake output and counts as mature. The fetch was a read
spread over the servers whose error answer is final (R38.6.4f item 1), so a
server that had not yet seen the fresh transaction could fail the next spend
from the same address. Every other listed output keeps the check as before.

R38.5B.5 **Serialization of own-address spends.** (Reference-bound at both
rows; also the baseline's behaviour.) Two locks order concurrent spends from
the coin's address. The listing of R38.5B.4 returns the record's lock to its
caller still held; a build-and-broadcast path of R38.5B.1 keeps it from the
listing through the broadcast and the record entry, so a concurrent listing
waits until the entry is made. The withdraw path releases the record's lock
as soon as the listing returns and instead holds a process-wide UTXO build
lock from the listing through signing, released when the signed transaction
is returned. Two withdraws are therefore serialized against each other during
building, but nothing orders a withdraw's result against a later listing once
it has returned.

*Status note (2026-11-15, clean side; defect fix).* In this tree the QTUM
delegation builds, adding and removing a delegation, hold the same
process-wide build lock from their listing through signing. Before this fix
the adding build asked for the lock without waiting for it, so the lock was
never held, and the removing build did not ask for it; either could be built
at the same time as a withdraw from the same address and, while both were
building, select the same inputs.

R38.5B.6 **Rejection shape on the legacy broadcast path.** (Reference-bound
at both rows; also the baseline's behaviour.) When the backend refuses a
`send_raw_transaction` broadcast -- a mempool conflict (the transaction spends
an input that a transaction already in the backend's mempool spends) included
-- the method fails with HTTP status 500 and the legacy error body
`{"error": "<text>"}`. The text is the baseline's legacy error form (a
source-location prefix followed by the rendered error), and the rendered
error carries the backend's JSON-RPC error object, server message included,
as the shared JSON-RPC client renders a server-side error response. No part
of the server's message is matched, classified or translated anywhere in the
UTXO coin layer at either row: every backend refusal -- conflict, missing
input, fee too low, already known -- takes this one shape and differs only in
the embedded text. The success body is `{"tx_hash": "<hex>"}`.

R38.5B.7 **Rejection shape on mmrpc-2.0 paths.** (Reference-bound at both
rows.) No mmrpc-2.0 UTXO path performs a broadcast, so a backend rejection of
a broadcast cannot arise on one: the `status: "Error"` payload of chapter 49
R49.20 never carries a broadcast refusal for a UTXO coin, and the typed
withdrawal error enumeration is never reached by a UTXO broadcast failure.
The swap engine's own broadcasts (R38.5B.1) report a refusal to the swap
state machine as a failed send that carries the signed transaction for
recovery; that path is not an RPC and has no RPC error shape.

R38.5B.8 **Lifetime of the record.** (Reference-bound at both rows; also the
baseline's behaviour.) The record has no time-to-live, no size bound and no
eviction. An entry lives until the coin instance is dropped (deactivation) or
until the record is replaced wholesale, which happens at both rows only when
a hardware wallet's enabled address is (re)established: the record is then
replaced by an empty one keyed to the new address's script. The record is
keyed to exactly one script -- the coin's single address at activation -- and
tracks replacement outputs only for that script; for an HD coin with several
addresses, outputs paying to other addresses of the same coin are never
offered as replacements. An unbroadcast withdraw never had an entry, so
nothing of it expires; a build-and-broadcast whose broadcast failed has no
entry either (R38.5B.1).

> **Reloaded divergence (maintainer Decision BE, 2026-10-30).** The record's
> lifetime and its single-script keying are unchanged. The pending entries of
> R38.5B.9 are not entries of the record: they have their own short lifetime
> and hide nothing, and an entry they promote into the record then has the
> lifetime described here.

> *Consequence (informative).* Because nothing evicts an entry, a recorded
> transaction that later leaves the mempool unconfirmed (replaced, expired or
> never relayed) keeps its inputs hidden from the listing and keeps its change
> offered as spendable until deactivation. The reference accepts this; this
> section records it so that a Reloaded divergence (D38.5B.1) is weighed
> against it. Decision BE accepts it for the beta and extends it to promoted
> withdraw entries (D38.5B.2).

R38.5B.9 **A withdraw's spend is recorded when its broadcast is accepted.**
(Reloaded-original; maintainer Decision BE, 2026-10-30, option (b) of
D38.5B.1. A deliberate divergence from the reference of the kind
`docs/COMPAT_SWITCHES.md` "The default-side rule" arm 2: no peer observes it.
It carries no compatibility switch, D38.5B.3.)

1. *Pending entry.* When the user withdraw path of a UTXO coin -- the direct
   `withdraw` method and the `task::withdraw::*` family -- has built and signed
   its transaction, it keeps a pending entry for it in memory: the inputs the
   transaction consumes with their values, those outputs that pay to the
   record's script (R38.5B.8), and the transaction's hash. The entry is keyed
   by a digest (SHA-256) of the exact serialized transaction bytes that the
   withdraw returns as the transaction hex, so the key does not depend on the
   coin's transaction-hash algorithm. The record itself is not written at that
   time (R38.5B.2 stands for build and signing). The record's lock is taken
   only for this short entry, after signing, never held across signing.
2. *Promotion.* When the legacy `send_raw_transaction` method hands exactly
   those bytes to the backend and the backend accepts the broadcast, the
   pending entry is removed and its inputs and own-script outputs are entered
   into the recently-spent record as an entry of R38.5B.1. The broadcast
   helper that does this is the shared one of the standard UTXO coin, BCH and
   Qtum; the byte-carrier form of the same helper behaves alike. A refused
   broadcast promotes nothing and leaves the pending entry to age out.
   Bytes that match no pending entry -- a transaction built elsewhere, or
   re-serialized by the caller -- promote nothing and change nothing, exactly
   as before.
3. *The pending entries are not the record.* A pending entry is neither
   consulted by the listing (R38.5B.4) nor offered as a replacement, so it
   hides no input and offers no output; an unbroadcast withdraw therefore
   hides nothing. A pending entry is dropped ten minutes after it was made, or
   earlier as the oldest when more than 64 are held; both bounds only limit
   memory. They must not be confused with the lifetime of the record
   (R38.5B.8, D38.5B.2): an entry promoted into the record has that lifetime,
   which has no expiry.
4. *Not covered.* SLP, shielded (ZHTLC), Sia, EVM, Tendermint and Lightning
   coins keep their own withdraw and broadcast paths and are unchanged. The
   record's single-script keying (R38.5B.8) is unchanged, so only a change
   paying to the coin's single address is offered as a replacement; the
   spent inputs are excluded for any sender address (D38.5B.4).
5. *Unchanged.* R38.5B.1, R38.5B.4--R38.5B.7: the listing composition, the
   locks, and the legacy rejection shape (HTTP 500 and the `error` body, with
   no classification of the server's text).
6. *Effect.* A withdraw issued after the broadcast of an earlier withdraw was
   accepted excludes that withdraw's inputs and may spend its change, whether
   or not the backend has indexed the broadcast. Two withdraws both built
   before the first broadcast still select the same inputs, as the reference
   does.

> **Code-quality finding (informative; corpus, trigger 2 -- a comment states
> the problem).** The reference's own source acknowledges, at both rows, that
> the single-script keying of R38.5B.8 is a known limitation for HD wallets
> and names per-script keying as the intended improvement. Proposed treatment
> for this repository: key the record by address script (one record per
> script of the coin), so that every address of an HD coin gets the
> protection of R38.5B.4 step 2; the behaviour of R38.5B.1--R38.5B.7 is
> unchanged by that keying. This repository's own activation code carries
> the same admission.

> **Field observation (this repository, 2026-10-30; permitted-input class
> R6, not corpus).** On Reloaded dev at the state the maintainer named
> (the dev state of 2026-10-30 that the maintainer named), a client issued two
> `withdraw` + `send_raw_transaction` pairs for DOGE from one address three
> seconds apart. Both withdraws selected the same input; the Electrum server
> rejected the second broadcast as a mempool conflict, surfaced exactly as
> R38.5B.6 describes (HTTP 500, `error` body with the server's text). A third
> withdraw 46 s later spent the unconfirmed change of the first, as R38.5B.4
> predicts. This repository's code matches the reference at every point of
> R38.5B.1--R38.5B.8: the withdraw path reads the listing and discards the
> record's lock without writing, and no broadcast RPC writes the record. The
> field behaviour is therefore parity, not drift. Whether Reloaded should
> diverge is D38.5B.1--D38.5B.3.

D38.5B.1 *(Decided 2026-10-30, maintainer Decision BE: option (b), in the
pending-entry form of R38.5B.9, which needs neither a parse of the broadcast
transaction nor a lookup of the consumed outputs' values.)* **Should Reloaded
record a withdraw's spend?** Options, each a deliberate divergence from the reference
of the kind `docs/COMPAT_SWITCHES.md` "The default-side rule" arm 2 (no peer
observes it): (a) keep parity (R38.5B.2 as written); (b) enter the spend at
`send_raw_transaction` time for a UTXO coin, by parsing the broadcast
transaction, matching its inputs and outputs against the coin's own address
script and entering the inputs and own-script outputs once the backend
accepts the broadcast -- which keeps the "only after acceptance" invariant of
R38.5B.1 and needs the consumed outputs' values, which the raw transaction
does not carry (they are available from the coin's transaction cache or the
backend); (c) enter the spend when the withdraw is built, which covers the
case the field observation shows but breaks R38.5B.1's invariant: a withdraw
the client never broadcasts would hide its inputs for the life of the coin
instance unless an expiry (D38.5B.2) is added. Option (b) preserves every
invariant of this section; (c) alone is not recommended.

D38.5B.2 *(Decided 2026-10-30, maintainer Decision BE: no expiry of recorded
entries for the beta, parity with R38.5B.8. The pending entries of R38.5B.9
keep their own short lifetime and cap, which only bound memory and hide
nothing. A later expiry as a multiple of the chain's measured block time is
a post-beta plan: `docs/plans/withdraw-entry-expiry-by-block-time.md`.)*
**Expiry.** The reference has none
(R38.5B.8). If D38.5B.1 chooses (c), or independently to close the
consequence note under R38.5B.8, an entry could be dropped when the backend
confirms the spending transaction (its inputs are then gone from the
backend's own listing and the entry is redundant), when the backend reports
the transaction unknown after a bounded time, or after a fixed time-to-live.
Any expiry must keep the transitive replacement of R38.5B.4 step 2 consistent
(dropping a middle link of a chain must not resurrect an input the later link
still spends).

D38.5B.3 *(Decided 2026-10-30, maintainer Decision BE: unconditional, no
switch.)* **Switch.** Whether a divergence
under D38.5B.1 is unconditional or carries a compatibility switch (default
on, `false` restoring reference behaviour) as R38.6.4a and R38.6.4l do. No
peer observes it, so the switch would exist only for A/B comparison against
the reference.

D38.5B.4 *(Deferred past the beta, maintainer Decision BE, 2026-10-30.)*
**Per-script keying of the record.** The code-quality finding above proposes
one record per address script of the coin, so that every address of an HD coin
gets the protection of R38.5B.4 step 2 and its change can be offered as a
replacement. Decision BE defers it: the field defect concerned a single
address, and the change touches the coin builders and the hardware-wallet
address re-establishment hook. It is carried in the post-beta plan
`docs/plans/withdraw-entry-expiry-by-block-time.md`.

### Tests (§38.5B)

T38.5B.1 *Record written only after acceptance.* A test with a mocked backend
shall drive a swap-side send from the coin's address, let the backend accept
the broadcast, and then have the same backend keep listing the spent input;
the next listing shall omit that input and offer the transaction's own-script
output in its place. A second run in which the backend rejects the broadcast
shall leave the next listing unchanged.

T38.5B.2 *Withdraw alone writes nothing to the record.* A test shall build
two withdraws in succession from one address against a backend whose listing
does not change between them, without broadcasting either, and assert that
both returned transactions spend the same input and that the listing after
both is unchanged (R38.5B.2 and R38.5B.9 item 3).

T38.5B.3 *An accepted broadcast of a withdraw's bytes records the spend.* A
test shall build a withdraw, broadcast its returned transaction hex through
`send_raw_transaction` against a backend that accepts it and keeps listing the
input, and assert that the next listing omits that input and offers the
transaction's own-script output in its place, and that a second withdraw
excludes the first one's input and may spend its change (R38.5B.9 items 2
and 6). The same assertions shall hold for the task-based withdraw.

T38.5B.4 *Legacy rejection shape.* A test shall make the mocked backend answer
a broadcast with a JSON-RPC error object and assert that
`send_raw_transaction` returns HTTP 500 with a JSON body whose single member
`error` is a string containing the server's message text, and that a second
backend error with different text yields the same status and the same body
shape.

T38.5B.5 *Unconfirmed change is spendable; coinbase is not.* A test shall
present a listing with an unconfirmed non-coinbase output and an immature
coinbase output and assert that only the former is offered to a spend.

T38.5B.6 *A refused broadcast records nothing.* A test shall build a withdraw
and broadcast it against a backend that refuses it; the next listing shall be
unchanged and a following withdraw shall select the same input, and the
pending entry shall still age out (R38.5B.9 item 2).

T38.5B.7 *An unbroadcast withdraw hides nothing.* A test shall build a
withdraw, never broadcast it, and assert that the listing is unchanged at
once and after the pending lifetime has passed, that no phantom change output
is offered, and that the pending entry is gone after that lifetime
(R38.5B.9 item 3).

T38.5B.8 *Bytes the node did not build promote nothing.* A test shall
broadcast a transaction that no withdraw built, and one whose hex a caller
re-serialized after a withdraw, against a backend that accepts them, and
assert that the record and the next listing are unchanged (R38.5B.9
item 2).

T38.5B.9 *The pending entries are bounded.* A test shall build more withdraws
than the cap without broadcasting and assert that no more than 64 pending
entries are kept and that the oldest are the ones dropped, and that an entry
older than ten minutes is dropped when the next entry is made (R38.5B.9
item 3).

T38.5B.10 *An entry promoted into the record does not expire.* A test shall
promote a withdraw's entry and assert that the listing still omits the
input and offers the change after a period far beyond the pending lifetime
(R38.5B.9 item 3, D38.5B.2).

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
maintainer Decision R of 2026-10-27; passed the KDF Dirty Gate on 2026-10-27.
Items 6–8 and the changes to items 1 and 3 were added on 2026-11-02 on the
clean side, maintainer Decision BI, with no forbidden-class input. The floor
of two in item 6 was added on 2026-11-06 on the clean side, maintainer
Decision CX, with no forbidden-class input.)*
1. *What spreads.* With `electrum_spread_reads` enabled, the client sends
   each read-only query of a coin to the next connected session in turn,
   round-robin over the near sessions (item 6) of the sessions R38.6.4 keeps
   connected (the maintained set of R38.6.4b). Read-only queries
   are balance, unspent-list, history, header, block-count, transaction-get,
   fee-estimate and relay-fee requests, and the R38.6.3a batches that carry
   them.
2. *What does not.* Transaction broadcasts, subscriptions and the
   keep-alive ping keep their current routing.
3. *Failover.* A query that fails on its session falls back to the other
   connected sessions, exactly as a query does today (R38.6.4f): first the
   other near sessions in turn order, then the maintained sessions that are not
   near, in priority order, and then the retry of R38.6.4f item 2.
4. *Consistency.* The recently-spent-output cache already shields a read
   that follows this node's own broadcast from a session that has not yet
   seen the broadcast. That shielding shall continue to hold whichever
   session answers the read.
5. *Switch.* `electrum_spread_reads` is a top-level `MM2.json` boolean,
   default `true`. With `false`, every query follows the priority order of
   R38.6.4.
6. *Near sessions.* Among the maintained sessions that have a smoothed
   latency (item 7), let *f* be the lowest. A maintained session is *near*
   when its smoothed latency is at most the larger of *F* × *f* and *f* + *A*,
   where *F* is the near factor and *A* the near offset of item 8. A session
   without a smoothed latency is not near while any maintained session has
   one: a session that has answered nothing yet takes no read before it has
   answered once (D38.6.4a.4). When no maintained session has a smoothed
   latency, every maintained session is near. *At least two are near:* when two or more
   sessions are maintained and the limit leaves only one of them near, the
   maintained session with the next-lowest smoothed latency is near as well;
   between equal figures, the one earlier in priority order is. A coin with
   two maintained sessions therefore always spreads over both, and one with
   more always over at least its two fastest. The near sessions are judged
   when the query starts, from the figures at that moment, and the turn of
   item 1 runs over them in priority order. A session that is not near still
   answers a query when the near sessions fail (item 3), and it becomes near
   again as soon as its smoothed latency is within the limit or it is among
   the two fastest. A session that is out of date (R38.6.4o item 4) is not
   near, and the near sessions and the floor of two are chosen among the
   up-to-date sessions only (R38.6.4o item 5).
7. *Latency under every order.* The smoothed latency of R38.6.4l item 2
   (*While running*) is kept for every maintained session, whatever
   `electrum_server_order` says. Every ordinary request and every
   `server.ping` (R38.6.4e item 4) answered over the session is a sample; the
   first sample after the session joins the maintained set starts the
   average. A broadcast or a subscription answered over the session is a
   sample too, and so is the answer to the header request the session sends
   on joining (R38.6.4o item 3) or to the header request of the measurement
   it joined from (R38.6.4l item 2), so a joining session that answers has a
   figure at once. Nothing else is sent for it. Under `random` and `listed` there is still
   no activation round, no re-measure round and no trigger (R38.6.4l items 1, 6
   and 7); the header request on joining is sent under every order
   (R38.6.4o item 3). A session that leaves the
   maintained set loses its smoothed latency.
8. *Near factor and near offset.* Two top-level `MM2.json` numbers, read at
   coin activation:
   - `electrum_spread_near_factor`, *F*, default 2 (D38.6.4a.1). `0` turns
     item 6 off: every maintained session is near, as before items 6–8 were
     added. A value above 0 and below 1 gives the same limit as 1, because
     the *f* + *A* term of item 6 then decides.
   - `electrum_spread_near_offset_ms`, *A*, in milliseconds, default 20
     (D38.6.4a.1).

   For either key, a value that is not a non-negative number is taken as the
   default, with one warning naming the key and the value; an absent key logs
   nothing. With `electrum_spread_reads` `false` neither has an effect.

*Rationale (this project's own measurement, 2026-10-26).* The per-session
limit of R38.6.3a applies to each session separately. With three connected
servers, spreading triples the read budget an order-posting burst can draw
on.

*Rationale for items 6–8 (this project's own measurements, 2026-10-31 and
2026-11-01).* Spreading over every maintained session sends one read in *N*
to each of *N* sessions, the slowest included. In a lab loop of 200 RIN
orders, three servers, one of them about 227 ms away and the other two about
25 ms, made the loop 48 % longer than with every read on one near server. On
RIN's later list of six servers the medians of a balance read from the lab
host were about 25, 27 and 32 ms for three servers, 108 ms for a fourth, and
230 and 266 ms for the last two. Spreading over all six costs about 114 ms per
read on average; over the three near ones, about 28 ms. With *F* = 2 the limit
is the larger of 50 ms and 45 ms, which keeps exactly the three. While at
least two other sessions stay near, a session that a server's per-session
limit slows down (R38.6.3a) leaves the near set and recovers its budget while
the ping keeps measuring it. When every session is
slowed, all of them stay near, so a coin whose servers all limit each session
keeps the full spread. The offset (20 ms by default) keeps sessions within a
few milliseconds of a very fast one near.

*Rationale for the floor of two (this project's own lab runs, 2026-11-06).*
Some servers answer a session about once a second after its first few
requests (R38.6.3a). One such answer, about 1,000 ms, lifts the smoothed
latency of a server that otherwise answers in about 40 ms past the near limit
for a while. With two servers, the other one then takes every read until the
first comes back, and reaches its own limit sooner. In a lab loop of 200 DOGE
orders against two such servers, the same build took 336 s with items 6–8,
about 12 % longer than its 300 s with the near factor at 0, which matches
0.3.0-beta.1. With the floor, a coin with two servers spreads over both, as before items
6–8, and a coin with more servers leaves a distant one out only while two
others stay near.

*Herding (informative).* Nodes in one region find the same servers near and
spread over them. That concentrates load by region, not globally. The
operator's list decides which servers exist, and `0` restores the spread over
every maintained session.

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
>
> **As-built note (2026-11-02), items 6–8.** The near decision is a pure function in
> `electrum_measure.rs` (`near_sessions`). It is also the only place that holds the
> form of the limit (D38.6.4a.2). The two defaults are one constant each there:
> `NEAR_FACTOR_DEFAULT` and `NEAR_OFFSET_DEFAULT_MS`.
> - *The floor of two (2026-11-06, D38.6.4a.3).* The floor of item 6 lives in `near_sessions`
>   only: after the flags of the limit, when two or more sessions are given and fewer than
>   two are near, the session with the next-lowest figure after the fastest is set near as
>   well, the earlier one in priority order between equal figures. `in_turn_for` is
>   unchanged, since it consumes the flags.
> - *The turn.* When a read-only query starts, `in_turn_for` in the pool first sets
>   the out-of-date connections aside (R38.6.4o item 5), then reads each remaining
>   slot's smoothed latency and asks for the near ones. It rotates them by
>   the read counter modulo their number, then appends the other up-to-date slots in
>   priority order and the out-of-date ones after them. The budget arithmetic of R38.6.4j is unchanged, since it counts
>   over that list.
> - *Samples.* The pool records the smoothed latency of every answered request and
>   ping under every order. P2 is judged only under `mixed` and `measured`. The header
>   request on joining is sent under every order unless `electrum_tip_checks` is
>   `false` (R38.6.4o item 3). Its answer, and the answer to the activation or
>   re-measure header request of the connection a session joined from, is a sample
>   (2026-11-09, D38.6.4a.4): `header_measurement` records it for a maintained
>   connection, and the probe's and the round's joins record it through
>   `record_first_sample`, which does nothing when the session has a sample already.
>   P2's count of 5 samples includes it. `near_sessions` makes a session without a
>   figure not near while any session has one (the floor of two picks only among
>   sessions with figures), and every session near when none has.
> - *The switches.* `electrum_spread_near_factor` and `electrum_spread_near_offset_ms`
>   are read once per coin activation, in the UTXO coin builder next to
>   `electrum_spread_reads`. The Z-coin Electrum fallback goes through the same builder.
>   An invalid value logs `electrum_spread_near_factor <value> is not a non-negative
>   number; using 2` or `electrum_spread_near_offset_ms <value> is not a non-negative
>   number; using 20`; the figure is the default constant.

*Amendment (2026-11-15, clean side; Reloaded-original; interim until
D38.6.4Q.1 and D38.6.4Q.4 are implemented). The QTUM family's unspent lists
are not spread, and they and its broadcasts take the listed order.*
1. For a coin instance whose coins-file `protocol.type` is `QTUM` or `QRC20`,
   an unspent-list request (`blockchain.scripthash.listunspent`), and an
   R38.6.3a batch that carries one, does not take a turn under item 1. It
   does not advance the turn counter.
2. For such an instance, an unspent-list request (item 1) and a transaction
   broadcast (`blockchain.transaction.broadcast`) try the up-to-date
   maintained sessions first and then the out-of-date ones (R38.6.4o item 5),
   and within each group in the order of the activation request's server list
   (R38.6.4b item 1), whatever `electrum_server_order` says. These two request
   kinds of this one family therefore do not follow the configured order
   (R38.6.4b-R, R38.6.4l) until the shared client of D38.6.4Q.1 and the shared
   record of D38.6.4Q.4 land. Every other request of those instances, and
   every request of every other coin, keeps its routing; every other read of
   those instances still spreads. `electrum_spread_reads` `false` still turns
   all spreading off.
3. *Reason.* Item 4 relies on the recently-spent record. For the QTUM family
   that record does not cover a QRC20 token's broadcasts, and the listings of
   QTUM and of its tokens do not consult one shared record (R38.6.4Q.7 item 6;
   §38.6.4Q, note 2 on existing text). QTUM and each token also have pools of
   their own (R38.6.4Q.1), and under `random`, `mixed` and `measured` each pool
   draws its own order. A listing sent in turn, or in a pool's own order, to a
   session that has not yet seen this node's last broadcast from another
   instance of the family could therefore offer an input that broadcast
   already spent. With items 1 and 2, every instance of the family that names
   the same servers tries the same first session for its listings and its
   broadcasts, as at the reference, where every pool ranks by list position
   and reads are not spread.
4. *Limits.* This narrows the exposure and does not close it:
   - a session can answer a listing before it has indexed a broadcast it took;
   - a broadcast that failed over to another session leaves the gap open;
   - instances whose activation requests name different servers, or list them
     in a different order, do not share a first session;
   - a session can be out of date in one instance's pool and up to date in
     another's;
   - with `max_connected` below the number of listed servers, two pools can
     maintain different subsets.
   The shared client of D38.6.4Q.1 and the shared record of D38.6.4Q.4 close
   the gap, and this amendment then lapses.
5. No peer observes it. A user sees no change in amounts or timing, only that
   the QTUM family's unspent lists and broadcasts go to the servers in list
   order instead of taking turns or following the measured order.

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

   *Status note (2026-11-14, maintainer decision).* The default is `mixed`
   since 2026-11-14: an absent switch, and every value items 1 and 3 do not
   recognize, now mean `mixed`, and the warning of item 1 names `mixed`.
   `random` stays a recognized value and gives the order that was the default
   before; `listed` keeps the listed order of R38.6.4b item 1. The sentences
   above that name `random` as the default record the rule as it stood before
   that date.

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
   race-freedom R38.6.4 already requires). *(Added 2026-11-02.)* The address
   is resolved through the node's resolver (chapter 45 §45.6B). The addresses
   it returns are tried in turn within this bound, and the TLS handshake
   checks the hostname as configured.
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

   *Amended 2026-11-09 (clean side; R38.6.4j, D38.6.4j.1):* the servers in
   flight form a sliding window: as soon as one attempt ends without an
   answer, the next server in the visiting order starts, so one attempt that
   is slow to fail never holds back servers that could answer.
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

   *Amended 2026-11-09 (clean side; R38.6.4o item 5):* connections that are
   out of date (R38.6.4o item 4) are tried after the up-to-date ones, for
   reads and broadcasts alike.

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

*Clarification (2026-11-15, clean side).* The contract-event subscription of
the last sentence is this project's own: this tree places it when a client
enables the balance stream for a QRC20 token (§38.6.5 decision 2). The
reference places no contract-event subscription at all (R38.6.4Q.5), so "as
before" refers to this tree, and placing the subscription again after a
disconnect would restore nothing the reference does. Whether this tree places
it again is open as D38.6.4Q.3.

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

   *Amended 2026-11-09 (clean side; D38.6.4j.1):* with the sliding window of
   R38.6.4f item 2 the budget is computed before each attempt starts, not
   before each round: it is the smaller of (a) and (b) above, where the retry
   rounds in *N* are counted as the retry's servers not yet started, this one
   included, divided by `max_connected` and rounded up. It is never below 5 s
   and bounds everything the attempt does, connecting included.
3. When *B* is exhausted the call ends with the failure of R38.6.4i even if
   servers remain untried.
4. Item 2 counts the retry rounds as the number of servers in the pool
   divided by `max_connected`, rounded up, and takes the call's *remaining*
   budget as *B*. The 5 s reserve is not spent, so a call whose servers all
   fail ends no later than *B*.

   *Amended 2026-11-09 (clean side; D38.6.4j.1):* the count above holds before
   the retry has started; during the retry, the count of the amendment to
   item 2 applies. The call's *remaining* budget is still taken as *B*.

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

> **As-built note (2026-11-09), items 12 and 13 (disagreement about the chain).** This note supersedes the
> *bound above the agreed tip* paragraph and the names of the tests of T38.6.4o.6 and T38.6.4o.8 above; item 4's
> *ahead* is still an out-of-date state.
> - *Pure.* `electrum_measure.rs`: `common_height`, `is_far_ahead` and `header_differences` (the pairs measured at
>   one height with different header bytes). `eligible_servers` no longer excludes a header that differs; the
>   two-sided height bound is unchanged.
> - *The pool.* `electrum_pool.rs`: `due_comparisons` (the three triggers of item 12, the 5-minute limit unless
>   a height moved by more than *T*), `compare_pair` (one directed `blockchain.block.header` per server, bytes
>   compared; a server that does not answer is not judged), `run_comparisons`, `disagreement_servers` and
>   `disagreement_text` (the two error texts), `update_disagreement` (ERROR on entering, WARN on leaving; run from
>   `refresh_out_of_date`) and `chain_disagreement`. Comparisons that fall due after the activation run on the
>   maintenance loop (`Wake::Compare`). A pair found different at one height, in a measurement round or between
>   two maintained connections, is kept in `pending` and compared once both heights are at least *T* above it. A
>   connection keeps the last header bytes it carried with their height (`HeaderTrack::last_header`, from the answer
>   to the header request and from every notification); `note_same_height_differences` runs when either arrives.
> - *Activation.* `activation_probe` logs the WARN of item 13 (whatever the switch says) and records the time of
>   the first connection; `judge_activation` waits until the probe has ended, every connected server's header
>   request has ended (`header_resolved`) and no comparison is running, or until `activation_window` (5 s) after the
>   first connection, running the due comparisons meanwhile, then fails with the text of the activation. The
>   builder calls it after the probe and maps the text to the new `UtxoCoinBuildError::ElectrumServersDisagree`,
>   which the task activation reports as a coin creation error and the legacy `electrum` and `enable` return as
>   their text. A silent server therefore costs the window, 5 s, not its establishment bound; the lab case of a
>   server that never answers builds in about 5 s longer than before.
> - *After activation.* `MmCoin::chain_disagreement` (default `None`) is answered for the UTXO coin types through
>   `electrum_chain_disagreement`. It refuses a withdrawal (`WithdrawError::ChainDisagreement`, in `withdraw` and
>   `init_withdraw`) and a new order (`create_maker_order_impl`, `buy_impl`, `sell_impl` and `lp_auto_buy`) with the
>   error text. A new swap
>   is not started: the maker start `lp_connect_start_bob` and the taker start `lp_connected_alice` log the error
>   and return before anything moves. They are the only places in this tree that call `run_maker_swap` or
>   `run_taker_swap` with `StartNew` (both handle the two atomic-lock-time versions), no other path starts a new
>   swap from a match, and the `KickStart` in `swap_rpc.rs` restarts a saved swap. An SLP token answers from its BCH
>   platform coin's client; QRC20 and Qtum do the same; a ZCoin has no override, since its Electrum client is only a
>   fallback for swap-path reads. `electrum_servers` has the field `chain_disagreement`. With `electrum_tip_checks` `false` nothing of this
>   runs.
>
>   *Correction (2026-11-15, clean side).* A QRC20 token does not answer from its platform coin's client: it is
>   built with Electrum client fields of its own (the token builder builds them from the token's entry), and its
>   `chain_disagreement` reads that client (`qrc20_mm_coin.rs`). Its disagreement state is therefore the state of
>   its own pool, not of the QTUM pool, and a QTUM coin and its tokens can be in different states. A Qtum coin
>   answers from its own client, as every platform coin does. The SLP and ZCoin parts of the sentence above are
>   unchanged. Whether a token should share its platform's pool is open in
>   the plan `qtum-qrc20-stabilization`.
> - *Tests.* `t38_6_4o_6_a_far_ahead_server_fails_the_activation`,
>   `t38_6_4o_8_different_headers_fail_the_activation_only_below_the_tip`, `t38_6_4o_12_two_servers_stale_or_diverged`,
>   `t38_6_4o_13_the_activation_window_does_not_wait_for_a_silent_server`,
>   `t38_6_4o_14_the_disagreement_state_after_activation`, `t38_6_4o_15_fewer_than_three_servers_are_flagged` and
>   `t38_6_4o_16_a_pair_is_compared_at_most_once_per_limit`, with `set_block_header` and `set_default_block_header`
>   on the stub. The old far-ahead test is now `t38_6_4o_6_a_far_ahead_server_is_kept_out_of_the_reads` (item 4).
>   `t38_6_4o_7_different_headers_in_notifications_start_a_comparison` covers item 7 for notifications.
>   A comparison runs as a task of its own and counts itself through a drop guard, so one that is slower than the
>   activation window finishes and records its result (`t38_6_4o_12_a_comparison_slower_than_the_window_is_not_dropped`).
>   A pair that did not answer records nothing and is tried again after the comparison limit; a pair that was found
>   to differ is compared again at the same height after the limit, so a resolved fork ends the state
>   (`t38_6_4o_12_a_resolved_fork_ends_the_state`). `pending` holds at most one entry per pair, keeps the first
>   difference found, and drops entries for servers that left the pool and entries older than ten comparison limits.
>   T38.6.4o.14 is tested at the pool (the state, the two log lines, `electrum_servers`, `chain_disagreement`) and at
>   the withdrawal (`a_withdrawal_is_refused_while_the_servers_disagree` in `rpc_command/electrum_servers.rs`). The
>   refusals of a new order and of the maker swap start are tested in `ordermatch_tests.rs`
>   (`a_chain_disagreement_refuses_a_new_order`, `a_chain_disagreement_starts_no_new_maker_swap`, with the helper
>   `chain_disagreement` mocked); the taker start has the same check and no test of its own.

> **As-built note (2026-11-09), the activation stall (plan step 0).**
> - *R38.6.4k-R item 3 and R38.6.4l item 2.* Before, `run_probe` offered a connection to the maintained set
>   only when its activation header request had been answered or had failed, so under `mixed` and `measured` the
>   set was empty when the build returned. Now, at the establishment, a connection that the rule of R38.6.4c
>   item 2 lets in (`would_join`) joins at once, the activation header request runs over it afterwards and its
>   measurement feeds the rank as before; one that does not join is held until its answer and released then,
>   so every server is still measured. `ServerSlot` state `probing` marks a connection whose probe request is in
>   flight: it sends no header request of its own on joining, and when a higher-ranked server displaces it, it
>   stays open until its measurement has ended. The release at the end of the probe leaves a maintained
>   connection alone. The test is `t38_6_4l_2_the_set_is_not_empty_when_the_build_returns_under_a_measured_order`.
> - *R38.6.4f item 2 and R38.6.4j items 2 and 4.* The retry in `route_inner` keeps `max_connected` attempts in
>   flight and starts the next server in the visiting order as soon as one ends without an answer. The budget of
>   an attempt is `round_budget` over the call's remaining budget, `rounds_of(servers not yet started, width)`
>   rounds and the attempt's own establishment-plus-request cap. The maintained connections tried one at a time
>   before the retry count the retry as before, `rounds_of(servers in the pool, width)`. A call that has no time
>   left starts no further attempt, and the ones in flight end within the budget they started with. The
>   re-measure round of R38.6.4o item 8 has its own window and is unchanged.
> - *R38.6.4j item 1.* `with_call_budget` in `electrum_pool.rs` runs a future with a caller's budget in force
>   for the Electrum calls it starts while it is polled (a thread-local set around each poll; the smaller of it
>   and `timings.call_budget` is the budget of the call). The legacy `electrum` and `enable` activation read,
>   `legacy_activation_response` in `lp_commands_legacy.rs`, wraps `my_balance()` in it with its bound of 20 s.
>   The budget applies to calls the future makes inline; a call made from a task spawned by the read does not
>   see it. The v2 activation tasks have no fixed outer bound on their balance read, so none is passed there.
> - *Tests.* `t38_6_4_14_a_dead_and_a_refusing_server_at_activation` (20 runs each under `mixed` and `random`; the
>   silent server is a stub that accepts the connection and never answers, since a listener that never accepts
>   cannot be told from it in-process; the refusing one is a stub that refuses the connection),
>   `t38_6_4j_1_the_retry_is_a_sliding_window`, `the_rounds_of_a_sliding_retry_count_the_servers_not_yet_started`,
>   `t38_6_4a_9_a_session_that_has_answered_nothing_takes_no_read` and the changed T38.6.4a.1 test in
>   `electrum_measure.rs`. The tests of T38.6.4a.2 and T38.6.4a.5 warm the sessions with a ping, since a read no
>   longer gives a second session a first figure. The P2 test counts the header answer as the first of the five
>   samples.
> - *A side effect of D38.6.4a.4.* With the default `electrum_tip_checks` `true`, a session that joins answers its
>   header request on joining within its round trip and has a figure at once. With `electrum_tip_checks` `false`,
>   no header request is sent on joining under `random` and `listed`, so a session that joins gets its first
>   figure from its first answered ping (R38.6.4e item 4, every 30 s). Until then it takes no read while another
>   maintained session has a figure; it still answers when those fail (R38.6.4a item 3).

R38.6.4l **Measured server order (Reloaded-original, maintainer design
approval 2026-10-28; binds the `mixed` and `measured` values of R38.6.4b-R
item 3).**
1. *Values.* With `electrum_server_order` set to `mixed`, the rank is a
   weighted random draw that favors well-measured servers (item 5). With
   `measured`, the rank follows measured quality alone (item 5). Under
   `random` and `listed` nothing in this rule applies except the running
   smoothed latency of item 2, which R38.6.4a item 7 keeps: no measurement
   request is sent and no trigger runs. The header request a connection sends
   on joining (item 6, R38.6.4o item 3) is not a measurement and is sent under
   every order. The default stays `random`
   (R38.6.4b-R item 1) until a field run on RIN with `mixed` has passed;
   `mixed`, the intended default, then becomes the default in a separate
   change (D38.6.4l.1).

   *Status note (2026-11-14, maintainer decision).* `mixed` is the default
   since 2026-11-14 (R38.6.4b-R, status note after item 3); that is the
   separate change of D38.6.4l.1.
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
     sample of its *smoothed latency*. R38.6.4a item 7 keeps this figure
     under every value of `electrum_server_order`. The first sample after the connection
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
   - the *best tip* is the agreed tip (R38.6.4o item 2) over the heights
     measured in the round and those currently reported by the maintained
     connections, one height per server (D38.6.4l.3, D38.6.4o.2);
   - the *best latency* is the lowest latency measured in the round.

   A server is *eligible* when all of these hold:
   - it was measured in the round;
   - it is not removed (R38.6.4d item 3) and not poor-demoted (item 7), and
     it was not suspended when it was measured. The release that follows a
     measurement, and the release of R38.6.4k-R item 3, suspend the server
     under R38.6.4d item 1 but do not make it ineligible in that round
     (D38.6.4l.8);
   - its height is at most *T* blocks below and at most *T* blocks above the
     best tip, *T* being the coin's tolerance (R38.6.4o items 1 and 2,
     D38.6.4o.1, D38.6.4o.9);
   - its header agrees with the round's (R38.6.4o item 7);
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
   (item 7). The rank does not decide which maintained connection a read-only
   query starts on; R38.6.4a does, with the same smoothed latency (R38.6.4a
   items 6 and 7).
   R38.6.4b-R item 2 holds for the measured order and its measurements: they
   are not persisted, a re-activation measures afresh, and a token that
   reuses a platform's Electrum client shares that client's rank,
   measurements and re-measure limit.
6. *A poor connection.* A maintained connection is *poor* when one of these
   holds:
   - *P1 stale, two or more maintained connections:* on two consecutive
     header notifications over this connection, its height is more than *T*
     blocks below the running agreed tip (R38.6.4o items 1, 2 and 4); or it
     is out of date as *stuck* (R38.6.4o item 4).
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
      have passed, and the poor server stays meanwhile (D38.6.4l.6). After a
      round that found no eligible replacement the least time grows as
      R38.6.4o item 8 states.
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
> *Status note (2026-11-14, maintainer decision):* since 2026-11-14 that
> variant is `mixed` (R38.6.4b-R, status note after item 3).
>
> **Compatibility:** for behavior matching GLEEC KDF, set
> `electrum_server_order` to `listed`.

R38.6.4m **TLS server authentication for Electrum servers (Reloaded-original;
maintainer decision 2026-11-04).**

> **As-built status (2026-11-07).** All items describe the code. Items 1–5 and
> 7–9 were built before; item 6, the warning, is built together with R38.6.4n.
> The warning is logged where a server joins the coin's pool at activation
> (`PoolShared::add_server_at` in `electrum_pool.rs`), from what the server's
> parsed settings (`ServerSpec::tls_notice` in `electrum_rpc_client.rs`) say, so
> it is logged once per server per activation, never for a server that was
> removed, and never on a browser build, where `tls_notice` has nothing to report.

1. *Scope.* This rule governs an Electrum server with `protocol` `SSL` on a
   native build. `TCP` carries no TLS. On a browser build the TLS of `WSS`
   belongs to the browser, and neither `disable_cert_verification` nor
   `spki_sha256` (R38.6.4n) has an effect there; item 9 says how such a server
   entry is treated.
2. *Host name.* The address shall name its host as a DNS name. An address
   whose host is an IP literal is refused before any connection is attempted.
   This is the "TLS address without a usable host name" of R38.6.4d item 3, so
   the server is removed for the coin's lifetime. The configured host name is
   sent as the TLS server name indication (SNI) in every handshake.
3. *Protocol and algorithms.* The client offers TLS 1.3 and TLS 1.2 and nothing
   older.
   - Cipher suites. TLS 1.3: AES-256-GCM, AES-128-GCM and ChaCha20-Poly1305.
     TLS 1.2: ECDHE with an ECDSA or RSA certificate, and AES-GCM or
     ChaCha20-Poly1305.
   - Key exchange groups: X25519, P-256 and P-384.
   - Handshake signature schemes: ECDSA P-256 with SHA-256, ECDSA P-384 with
     SHA-384, Ed25519, and RSA PKCS#1 v1.5 and RSA-PSS with SHA-256, SHA-384 or
     SHA-512, for RSA keys of 2048 to 8192 bits.
   - Not accepted: SHA-1 schemes, RSA keys below 2048 bits, P-521 keys and DSA
     keys.
   - These sets are those of the TLS library's default provider at the version
     in `Cargo.lock`. A library update that changes them is a change to this
     rule and is recorded in the CHANGELOG.
4. *Default verification* (`disable_cert_verification` absent or `false`, no
   `spki_sha256`). The server's certificate chain shall verify to a root of the
   bundled `webpki-roots` store. The end-entity certificate shall:
   - be valid at the time of the handshake;
   - not be a CA certificate;
   - allow server authentication, if it carries an extended key usage;
   - carry the configured host name in its subject alternative names (a
     common name alone does not count).

   A self-signed certificate therefore fails: as a CA certificate used as an
   end entity when it is marked as one, otherwise for its unknown issuer.
5. *`disable_cert_verification` `true`* (no `spki_sha256`).
   - *Skipped:* every check of item 4, that is the chain, the trust anchor, the
     validity period, the CA flag, the extended key usage and the name.
   - *Kept:* the certificate shall still parse as an X.509 version 3
     certificate, and the server's handshake signature shall verify with that
     certificate's public key under one of the schemes of item 3.
   - *Result:* the connection is encrypted and bound to the key the server
     presented, but the server is not authenticated. A self-signed version 3
     certificate with an accepted key is accepted. A version 1 certificate, or
     a key outside item 3, is refused even so.
6. *Warning.* At activation, a server to which item 5 applies logs
   one WARN line, once per activation of the coin:

   `<TICKER>: Electrum server <host:port> has disable_cert_verification set: the connection is encrypted, but the server is not authenticated`

   The server is named by host and port only (R38.6.9a). No warning is logged
   for a `TCP` server or for a server with `spki_sha256` (R38.6.4n item 4).
7. *Failure class.* A failure under items 3–5 is a TLS handshake failure. It is
   recoverable, and the server is suspended (R38.6.4d item 1). Its text is the
   TLS library's description of the failure. It appears, under the server's
   host and port, in the per-server failure list of R38.6.4i and in the
   activation error that no Electrum server could be connected.
8. *Compatibility.* No switch. The default verification of item 4, the meaning
   of `disable_cert_verification` and the refusal of IP literals are this
   project's established behavior. Any difference from the reference on a given
   certificate is recorded from black-box runs of the release binaries, not
   from this rule.
9. *Browser build.* On a browser build, a server entry with
   `disable_cert_verification` `true` has no effect beyond what the browser
   does, and logs nothing. A server entry with `spki_sha256` on a browser build:
   R38.6.4n item 6.

> **Reference behavior (Spec Reader 2026-11-07, rows `v2.6.0-beta` and
> `v3.0.0-beta` of chapter 02 §2.15A, identical at both; passed the KDF
> Dirty Gate on 2026-11-07, measure 0.03).** The
> reference matches items 1--5, 7 and 9 of R38.6.4m, so there is **no
> divergence** on what `disable_cert_verification` does, and the black-box lab
> result of 2026-11-04 (a self-signed version 3 certificate accepted under the
> flag) is the expected outcome at both rows:
>
> - *Per-server TLS surface.* A server entry carries exactly `url`,
>   `protocol`, `disable_cert_verification` (default `false`) and
>   `timeout_sec`; there is no other per-server TLS knob -- no pin, no
>   per-server trust store, no minimum version or cipher setting.
> - *Skipped under the flag:* the whole server-certificate verification --
>   chain and trust anchor, validity period, CA flag, extended key usage, and
>   the host-name match against the certificate.
> - *Kept under the flag:* the handshake signature is verified with the
>   presented end-entity certificate's public key under the TLS library's
>   default schemes (the set of item 3), which requires that certificate to
>   parse as an X.509 version 3 certificate; the protocol versions (TLS 1.3
>   and 1.2 only), cipher suites and key-exchange groups of item 3; and the
>   host-name precheck of item 2, which refuses an IP literal before any
>   connection as a non-recoverable failure, so the server is dropped from the
>   pool (R38.6.4d item 3). The configured host name is sent as SNI and is
>   not checked against the certificate.
> - *Failure class when a certificate is refused by default* (unknown issuer,
>   CA certificate used as end entity, expired, name mismatch) and when a
>   handshake fails under the flag (version 1 certificate, unsupported key or
>   scheme, no common version or suite): a **temporary**, recoverable
>   connection failure, so the server is retried under the pool's suspension
>   rules (item 7; R38.6.4d item 1). Only a malformed address, an IP literal
>   under `SSL`, a browser-only protocol on a native build and a version
>   mismatch are non-recoverable.
> - *Logging.* The reference logs no warning for a server configured with the
>   flag; the line it logs when a connection is established marks that the
>   connection was made with certificate verification disabled. Item 6 of
>   R38.6.4m therefore adds a warning the reference does not have.
> - *Browser build.* `WSS` is the browser's TLS; the flag has no effect there
>   (item 9).
>
> The TLS library of the reference is an older major version than Reloaded's;
> the behaviors above are what both versions do, not a statement that the
> two libraries are identical in every rejection text.

R38.6.4n **Electrum server key pin `spki_sha256` (Reloaded-original;
maintainer decision 2026-11-04; built).** Opt-in: a server entry without the key
behaves exactly as R38.6.4m.

> **As-built note (2026-11-07).** The key is carried on the server entry
> (`ElectrumRpcRequest::spki_sha256` in `electrum_types.rs`) as the JSON value
> it was configured with, so that a malformed value removes only its own server
> (item 5) and never fails the whole activation request. It is read when the
> server joins the pool (`server_spec` in `electrum_rpc_client.rs`), which turns
> it into 1 to 4 digests next to `skip_validation` in the server's settings. The
> verifier is `PinnedKeyVerification` in `rpc_clients.rs`, a sibling of the
> verifier of R38.6.4m item 5: it parses the end-entity certificate with the TLS
> library's own parser, hashes the DER `SubjectPublicKeyInfo` that parser
> returns, and compares it with the pins; the handshake signature is verified as
> in item 5. A connection to a pinned server builds its own TLS configuration,
> so that the verifier can tell the session that the key matched no pin. A
> pinned server therefore never resumes a TLS session: every connection makes a
> full handshake, and the key is checked each time. The configuration is not to
> be shared between connections, because it carries that signal. On that
> signal the session reports exactly the text of item 5, and `establish` in
> `electrum_pool.rs` logs the WARN of item 5 once for every refusal. The INFO line
> of item 4 is logged next to the warning of R38.6.4m item 6, where the server
> joins the pool. A browser build removes a server entry that carries the key
> (`server_spec`, item 6). The Z-coin Electrum fallback builds its entries
> without a pin. Malformed pins are checked before the protocol, so a malformed
> pin on a `TCP` server is reported as malformed. The operator description of
> item 7 is the section "Electrum Servers over TLS" of `docs/NETWORK_CONFIG.md`.

1. *Key and value.* Each Electrum server entry may carry `spki_sha256`, next to
   `url`, `protocol`, `disable_cert_verification` and `timeout_sec`, in the
   activation request (`mode.rpc_data.servers[]`, chapter 55 R55.5.10) and in
   the legacy `electrum` request.
   - The value is a string, or an array of 1 to 4 strings, so that an operator
     can pin the next key before a server changes it.
   - Each string is the SHA-256 digest of the DER `SubjectPublicKeyInfo` of the
     server's end-entity certificate: 64 hexadecimal digits in either case.
     Colons between byte pairs are accepted and ignored.
   - The pin is on the key, not the certificate, so it survives a certificate
     renewal that keeps the key.
2. *Verification when a pin is set* (`protocol` `SSL`, native build).
   - The end-entity certificate shall parse as in R38.6.4m item 5, and the
     SHA-256 of its `SubjectPublicKeyInfo` shall equal one of the configured
     pins.
   - The handshake signature shall verify with that key, as in R38.6.4m item 5,
     so the server proves that it holds the pinned key.
   - The chain, the trust anchor, the validity period, the CA flag, the
     extended key usage and the name are not checked; the pin replaces them.
     This is what lets a self-signed certificate be used safely. It also means
     that an expired certificate with the pinned key is accepted.
3. *A pin never widens acceptance beyond the pinned key.* Only a server that
   holds the private key of a pinned `SubjectPublicKeyInfo` can complete the
   handshake, with or without `disable_cert_verification`.
   - A certificate wrongly issued for the host name by a trusted CA, a
     compromised or wrongly trusted root, and a changed DNS answer are each no
     longer enough. For that server, a pin is a stronger check than the
     default verification of R38.6.4m item 4.
   - What the pin gives up is the expiry check: a pinned key that has leaked
     stays accepted until the operator removes its pin. The default
     verification has a comparable gap, because it checks no revocation, so it
     accepts a leaked key until its certificate expires.
   - Checking the validity period in addition to the pin was not adopted:
     self-signed Electrum certificates often run past their dates, and a lapsed
     date would then lock out a correctly pinned server.
4. *Interplay with `disable_cert_verification`.* With a pin set, the pin
   decides, whatever the flag says; the flag is then redundant, and the warning
   of R38.6.4m item 6 is not logged. The combination is not refused, so that an
   entry that sets the flag stays usable when a pin is added. One INFO line at
   activation names the server as pinned:

   `<TICKER>: Electrum server <host:port> is pinned by spki_sha256`
5. *Error classes.*
   - *Malformed pin:* a removal-class configuration failure (R38.6.4d item 3),
     detected when the server entry is read, with no connection attempted. A
     pin is malformed when a value is not 64 hexadecimal digits once the colons
     are removed, when the array is empty or has more than 4 entries, or when an
     entry is not a string. Text: `spki_sha256 is not a 64-digit hexadecimal
     SHA-256 digest`; the configured value is not echoed.
   - *Pin on a `TCP` server:* also removal-class, with the text `spki_sha256
     needs protocol SSL`. A pin that cannot be enforced must not look enforced.
   - *Mismatch at the handshake:* a TLS handshake failure, recoverable, and the
     server is suspended (R38.6.4d item 1). A mismatch can come from an attacker
     on one network path and go away with it; removal would hand that attacker a
     lasting denial of service. Text: `the server's key does not match
     spki_sha256`. It is logged at WARN on every occurrence, by host and port,
     because a mismatch is either a key change or an attack.
6. *Browser build.* The browser does the TLS of `WSS` and cannot be told to pin,
   so a server entry with `spki_sha256` on a browser build is removed with the
   text `spki_sha256 is not supported in a browser`.
7. *Operator guidance.* The operator documentation gives the value as computed
   on the server from its key file,
   `openssl pkey -in server.key -pubout -outform der | sha256sum` (the first 64
   hexadecimal digits). The server operator publishes it next to the server's
   address, for example in the coins-file entry. It also gives the variant that
   reads the key from a running server,
   `openssl s_client -connect <host>:<port> -servername <host> </dev/null 2>/dev/null | openssl x509 -pubkey -noout | openssl pkey -pubin -outform der | sha256sum`,
   and says that this is trust on first use done by hand: the value is only as
   good as the network path it was read over, so it is to be compared with the
   value the server operator publishes. It advises pinning the next key before
   rotating.
8. *Compatibility.* An opt-in key that changes nothing unless set; no switch.
   Whether a reference binary ignores or refuses a server entry carrying the
   key is measured black-box and recorded in `docs/GLEEC_COMPATIBILITY.md` with
   the implementation.

R38.6.4o **Server staleness: tolerance, agreed tip, out-of-date connections
(Reloaded-original, maintainer decisions of 2026-11-09; amends R38.6.4a items 6
and 7, R38.6.4f item 4 and R38.6.4l items 1, 4, 6 and 7).** Plan
[`electrum-server-staleness`](../plans/electrum-server-staleness.md). Every item
holds under every value of `electrum_server_order` unless it says otherwise.
1. *Tolerance.* A coin's *tolerance* *T*, in blocks, is
   ceil(180 s / `avg_blocktime`), but at least 1 and at most 20. A coin without
   `avg_blocktime`, or with one that is not a positive number, has *T* = 1.
   Examples: BTC (600 s) 1, LTC (150 s) 2, RIN, KMD and PIVX (60 s) 3, DGB
   (15 s) 12, a coin with 1.5 s blocks 20. *T* is computed once at activation
   (D38.6.4o.1).
2. *Agreed tip.* For a set of heights, one per server:
   - with 3 or more heights, the agreed tip is the highest height that at
     least two of the servers have reached, that is, the second-highest height
     of the set;
   - with exactly 2 heights, it is the higher of the two;
   - with 1 height or none, there is no agreed tip, and no server is judged by
     height.

   A server's *lag* is the agreed tip minus its height, or 0 when its height is
   at or above the agreed tip. Its *lead* is its height minus the agreed tip,
   or 0 when its height is at or below the agreed tip. A lead of more than *T*
   cannot be judged from heights alone: such a server may report a height
   that does not exist, or follow another network, or be the one server that
   followed a network upgrade while the others stopped. Item 12 therefore
   treats it as a disagreement about the chain, not as a server to hide
   (D38.6.4o.2, D38.6.4o.9).
3. *Heights of maintained connections.* Each connection that joins the
   maintained set sends one header request (`blockchain.headers.subscribe`)
   right after it joins, unless a header request was already sent over it (an
   activation-round or re-measure measurement). Its answer is the connection's
   height, and the server then pushes a header notification for each new tip;
   the height of each notification is the connection's current height. This
   extends D38.6.4l.4 from `mixed` and `measured` to every order
   (D38.6.4o.4). An answer or notification whose height cannot be read is
   ignored: it neither sets nor changes the connection's height. A connection
   whose header request has not been answered has no height and is judged by
   none of items 4 and 5.
4. *Out of date.* The *running agreed tip* is the agreed tip (item 2) over the
   heights of the maintained connections that have one. A maintained
   connection is *out of date* while either holds:
   - *behind:* its lag to the running agreed tip is more than *T*;
   - *ahead:* its lead over the running agreed tip is more than *T*; this also
     puts the coin into the disagreement state of item 12;
   - *stuck:* there are two or more maintained connections with a height, it
     has received no header notification for longer than *P* = the larger of
     3 × `avg_blocktime` and 10 minutes (10 minutes for a coin without a valid
     `avg_blocktime`), counted from the answer to its header request or from
     its last notification, and at least one other maintained connection has
     received a notification within that time (D38.6.4o.7). When no maintained
     connection has received a notification within *P*, none is stuck: the
     chain itself may have stopped.

   The state is judged anew whenever a header request is answered, a
   notification arrives, or a read-only query starts, from the figures at that
   moment; a connection is up to date again as soon as neither condition
   holds. Being out of date is not a suspension and not a demotion: the
   connection stays maintained, keeps its rank and keeps answering pings.

   *Amended 2026-11-15 (clean side; Reloaded-original; maintainer decision of
   2026-11-15; replaces the stuck condition above and amends D38.6.4o.7).*
   - *stuck:* there are two or more maintained connections with a height, and
     another maintained connection has reported a height above this
     connection's height for longer than *P*. The time is counted from the
     moment the first maintained connection reported the lowest height above
     this connection's height, not from this connection's own last
     notification. *P* is the larger of 2 × `avg_blocktime` and 180 s; for a
     coin without a valid `avg_blocktime` it is 10 minutes.
   - The pool keeps, per coin instance, the moment each height was first
     reported by a maintained connection, through a header notification or
     the answer to a header request. It keeps only the heights within *T* of
     the running agreed tip, above the agreed tip minus *T* and up to the
     agreed tip plus *T*, and above the lowest height of its maintained
     connections. A connection further behind is behind, which wins over
     stuck; higher heights do not count (below); and a height every
     connection has reached is no longer awaited.
   - A connection that keeps sending notifications without reaching the
     reported height is stuck too. Under the earlier own-silence condition it
     was not.
   - When no other connection has reported a higher height, none is stuck,
     however long the silence: the chain itself may have stopped, or a block
     may be slow.
   - Only heights up to the running agreed tip plus *T* count as reported
     above. A height more than *T* above the agreed tip does not start the
     stuck period; it is judged by the ahead condition and the disagreement of
     item 12.
   - The behind and ahead conditions are unchanged and still win over stuck.
     The minimum of 180 s is the time *T* covers (item 1): a connection is
     never called stuck sooner than a lag the behind condition still accepts.
   - *Reason.* The own-silence condition marked every connection that was not
     first with a new block whenever the time between two blocks exceeded the
     period, and the mark cleared about a second later when its own
     notification came. In a 72-hour field run of this project (2026-11-10 to
     2026-11-13, four nodes), 405 of 411 such marks cleared within 1 s; one
     cleared within 30 s, and the other 5 were each followed by a second mark
     of the same server and cleared with it in the same second. No mark stayed
     uncleared. The
     marks also made the connection poor (R38.6.4l items 6 and 7) and made a
     re-measure round due.
   - *Known limit.* The lone-connection rule of R38.6.4l item 6 has no other
     connection to count from. It keeps its limit (the larger of 3 ×
     `avg_blocktime` and 10 minutes) and still marks a lone connection after
     a block gap longer than that limit.
5. *Reads and broadcasts avoid out-of-date connections.*
   - *Reads (amends R38.6.4a item 6).* An out-of-date connection is not near.
     The near sessions, and the floor of two near sessions, are chosen among
     the maintained sessions that are up to date. Out-of-date sessions follow
     every up-to-date session in the order a read-only query tries them, in
     priority order among themselves, so they still answer when every
     up-to-date session fails. When no maintained session is up to date, item
     6 of R38.6.4a applies to all of them as before.
   - *Broadcasts (amends R38.6.4f item 4).* A transaction broadcast tries the
     up-to-date maintained connections in priority order first, then the
     out-of-date ones in priority order, then the retry of R38.6.4f item 2.
   - With `electrum_spread_reads` `false`, reads follow the same order as
     broadcasts: up-to-date connections in priority order first.
6. *Swap confirmation waits stick to one server.* The wait for a
   transaction's confirmations (`wait_for_confirmations` of an Electrum-backed
   coin) chooses one server when it starts: the first maintained connection
   in priority order that is up to date (item 4); if none is, the first
   maintained connection in priority order; if the set is empty, the first
   server of the rank that is not removed. Every transaction request and
   block-count request of that wait goes to the chosen server as a directed
   request (R38.6.4g). When a request to it fails, the wait chooses again by
   the same rule, leaving out the servers that failed during this wait while
   another choice exists, and continues on the new server. The answer that
   ends the wait therefore always comes from the server the wait is polling,
   never from whichever server a spread read reached. No request is added
   (D38.6.4o.3).

   *Status note (2026-11-21, clean side; Reloaded-original; maintainer
   decision of 2026-11-21).* While `electrum_tip_checks` is `true`, the wait
   also chooses again in two more cases. (i) After every poll that does not
   end the wait, the state of item 4 of the polled server is looked at; when
   that server is out of date, the wait continues on the first up-to-date
   maintained connection in priority order that has not failed during this
   wait, or, when there is none, on the first up-to-date one that has; a
   server taken this way no longer counts as failed, and the wait polls it
   only after its usual pause. The out-of-date server is not counted as failed
   and a later choice may return to it. (ii) Three error answers in a row of
   the polled server to the transaction request count as a failure of that
   server for this wait: it is left out while another choice exists, as after
   a failed request, and the wait chooses again. An answer that is not an
   error resets the count. An up-to-date server that answers too few
   confirmations keeps the wait, as before. Every change of server logs one
   INFO line naming both servers by host and port (§38.6.9) and the reason.
   With `electrum_tip_checks` `false` neither case applies. This is prompted
   by a field report in which a taker payment had its confirmations 88 minutes
   before the wait's deadline, and the wait still timed out on the server it
   had chosen.
7. *Headers at the same height (amends R38.6.4l item 4).* Two servers that
   answer different header bytes for the same height, in a measurement round
   or in their header notifications, are not judged by the majority: the
   difference starts the comparison of item 12 for those two servers, and
   until it has ended neither is excluded for it. A difference at the tip
   alone is an ordinary race between two new blocks and resolves itself
   (D38.6.4o.8, D38.6.4o.12).

   *Amended 2026-11-10 (clean side; D38.6.4o.14):* "different header bytes"
   means a different base header: the first 80 bytes of the header differ. The
   bytes after them are not compared, so two servers that answer the same
   block of a merged-mining (AuxPoW) coin with different merged-mining proofs
   start no comparison.
8. *Re-measure backoff (amends R38.6.4l item 7 step 2).* A re-measure round
   that finds no eligible replacement doubles the least time before the coin's
   next re-measure round: 5, 10, 20, 40, then 60 minutes, and 60 minutes from
   then on. The least time returns to 5 minutes when a round finds a
   replacement or when no maintained connection is poor any more. A re-measure
   round connects at most 4 servers at a time.
9. *Warnings.* Each line names the server by host and port only (R38.6.9a)
   and is logged at WARN once per change of state, not on every notification:
   - on becoming out of date as *behind*:
     `<ticker>: Electrum server <host:port> is <lag> blocks behind the other servers (tip <height>)`;
   - on becoming out of date as *ahead*:
     `<ticker>: Electrum server <host:port> is <lead> blocks ahead of the other servers (tip <height>)`;
   - on becoming out of date as *stuck*:
     `<ticker>: Electrum server <host:port> has sent no new block for <seconds> s while another server did`;
   - on becoming up to date again:
     `<ticker>: Electrum server <host:port> is up to date again`;
   - on a replacement under R38.6.4l item 7:
     `<ticker>: Electrum server <host:port> was replaced by <host:port> (<reason>)`,
     with the reason `behind`, `stuck` or `slow`;
   - in a measurement round, for each measured server whose lag or lead to the
     round's agreed tip is more than *T*, once per round, the *behind* or
     *ahead* line above;
   - on entering the disagreement state of item 12, at ERROR, the text of the
     activation error of item 12; on leaving it, at WARN:
     `<ticker>: the Electrum servers agree about the chain again`.

   *Status note (2026-11-28, clean side; Reloaded-original; maintainer
   decision of 2026-11-28; amends the logging of items 4 and 9, not the
   states).* The *behind* and *ahead* lines on a change of state are logged
   only once that state has lasted 5 seconds, with the lag or lead and the tip
   at that moment; a change between behind and ahead starts the time again.
   An episode that ends sooner logs nothing, and the *up to date again* line
   is logged only for an episode whose line was logged. The node judges the
   connection again when the time has passed, so a lasting state is logged
   even when no further header arrives; that judgment is a full one, as on a
   header event, so the stuck state and the disagreement state of item 12 can
   also be entered at that moment, by the same rules. The state itself is still judged at
   once as item 4 says, so reads, broadcasts, demotion, replacement and the
   disagreement state of item 12 are unchanged. The *stuck* line and the lines
   of a measurement round are unchanged. *Reason.* In a field run of this
   project on coins with bursty blocks and a tolerance of one block, each node
   logged about ten behind or ahead lines a day; each state ended within one
   second, with no demotion and no replacement.
10. *The `electrum_servers` request (Reloaded-only RPC).* An mmrpc 2.0 method
    `electrum_servers` with the parameter `coin` (a ticker) answers, for an
    active coin served by the Electrum client, a snapshot of its pool:
    ```json
    {
      "coin": "RIN",
      "server_order": "random",
      "tolerance_blocks": 3,
      "agreed_tip": 1234567,
      "chain_disagreement": null,
      "servers": [
        {
          "server": "electrum1.example.org:50002",
          "rank": 0,
          "maintained": true,
          "near": true,
          "height": 1234567,
          "lag": 0,
          "out_of_date": null,
          "latency_ms": 31.5,
          "suspended": false,
          "poor_demoted": false,
          "removed": false
        }
      ]
    }
    ```
    - `server_order` is the value in force (R38.6.4b-R, R38.6.4l).
    - `agreed_tip` is the running agreed tip of item 4, `null` when there is
      none.
    - `chain_disagreement` is `null` outside the disagreement state of item
      12, and otherwise the text of its error.
    - `servers` lists every server of the pool, in the coin's current rank;
      `rank` is the position in it, from 0. `server` is the host-only label of
      R38.6.9a.
    - `height` is `null` for a server without a height; `lag` is `null` when
      `height` or `agreed_tip` is, and is negative for a server above the
      agreed tip (its lead, negated).
    - `out_of_date` is `null` when the server is up to date or not maintained,
      otherwise `"behind"`, `"ahead"` or `"stuck"`.
    - `latency_ms` is the smoothed latency of R38.6.4l item 2, `null` without
      one.
    - `near` is the near decision of R38.6.4a item 6 with item 5 here, judged
      at the moment of the request; `false` for a server that is not
      maintained.

    Errors: a ticker that is not active is `NoSuchCoin`; an active coin not
    served by the Electrum client (native daemon, lightwalletd, any non-UTXO
    coin) is `CoinIsNotElectrum`. The request changes nothing, sends nothing
    to any server and needs the RPC password like every other request.

11. *Switch.* `electrum_tip_checks` is a top-level `MM2.json` boolean, read at
    coin activation, default `true`. With `false`: under `random` and `listed`
    no header request is sent on joining (item 3; under `mixed` and `measured`
    D38.6.4l.4 still applies), no connection is ever out of date for items 5
    and 6, the first three warnings of item 9 are not logged, and item 12 does
    not apply: activation does not wait for heights, never fails for a
    disagreement, and the disagreement state is never entered. Items 1, 2,
    7 and 8 still shape the measured order of R38.6.4l, which is not the
    default and has its own compatibility value. A value that is not a boolean
    is taken as `true`, with one warning naming the key and the value; an
    absent key or a JSON `null` logs nothing (D38.6.4o.5).
12. *Disagreement about the chain (D38.6.4o.9 – D38.6.4o.13).* The servers of
    a coin *disagree about the chain* when either holds:
    - *far ahead:* with three or more heights, a server's lead over the agreed
      tip is more than *T*;
    - *diverged:* a comparison finds that two servers answer different header
      bytes for the same height.

    A *comparison* of two servers asks each of them, with one directed request
    (R38.6.4g) `blockchain.block.header`, for the header at a *common height*
    and compares the bytes. The common height is the lower of the two servers'
    heights minus *T*, but not below 0, so that a race between two new blocks
    at the tip never counts; for a difference found at one height (item 7) it
    is that height, once both servers report heights at least *T* above it. A
    comparison runs:
    - when exactly two heights are known and they are more than *T* apart.
      If the headers agree, the two are on the same chain and the lower server
      is only behind: item 4 applies, and the higher one is used. If they
      differ, the servers have diverged. With two servers a stale one cannot
      be told from one that followed a network upgrade by heights alone, and
      the comparison tells them apart;
    - at activation, between the server at the agreed tip and each other
      server whose height is within *T* of it;
    - when item 7 finds different headers at the same height.

    Two servers are compared at most once per 5 minutes, unless one of their
    heights has changed by more than *T* since the last comparison. A server
    that does not answer a comparison within the per-request bound of
    R38.6.4e item 2 is not judged by it.
    - *At activation.* When the build has its first connection, it waits
      until every other server has a height and every comparison has ended,
      or until 5 s have passed since that first connection, whichever comes
      first, and judges on what arrived. A server that has not answered by
      then is not judged (D38.6.4o.10). When the servers disagree, the
      activation fails with the error
      `<ticker>: the Electrum servers disagree about the chain (<host:port> at <height>, <host:port> at <height>, ...; tolerance <T> blocks). The node cannot tell which one is right; check the servers or remove the wrong ones from the coin's list`,
      naming every server with a height, by host and port (R38.6.9a). A task
      activation reports it as an activation error of the coin; the legacy
      `electrum` and `enable` methods return it as their error text. The coin
      is not registered.
    - *After activation.* When the servers come to disagree later, the coin
      enters the *disagreement state* (D38.6.4o.11). While in it, the coin
      refuses to place a new order with it as the base or the rel coin, to
      start a new swap with it, and to build a withdrawal, each with the error
      `<ticker>: the Electrum servers disagree about the chain (<host:port> at <height>, <host:port> at <height>, ...; tolerance <T> blocks). New orders, swaps and withdrawals are refused until they agree again; remove the servers that follow the other chain from the coin's list, or activate the coin again with servers that agree`,
      which is also the text of the ERROR line of item 9. A disagreement can
      last for weeks when a network splits, so the text names the servers and
      the remedy. Balance,
      history and other reads keep working, and a swap that has already
      started goes on. Item 4 still keeps a far-ahead server out of the
      reads. The state ends as soon as neither condition holds.

    *Amended 2026-11-10 (clean side; D38.6.4o.14):* a comparison compares the
    base headers, the first 80 bytes of the two answers, and *diverged* means
    that they differ. Two answers that agree in those 80 bytes and differ only
    after them name the same block.

    *Amended 2026-11-10 (clean side; D38.6.4o.15):* after activation the coin
    enters the disagreement state only when the disagreement is *confirmed*,
    that is, when it has lasted the *confirmation period*: two block times
    (two `avg_blocktime`), and at least 60 s.
    - A server far ahead counts once it has been seen far ahead for the
      confirmation period with no look in between that found none.
    - A divergence counts once a comparison of the same two servers at the same
      height, made at least the confirmation period after the first comparison
      that found it, still finds it. That second comparison is made when the
      period has passed, without waiting for the 5-minute limit between
      comparisons.

    The ERROR line, the refusals and the `chain_disagreement` field of
    `electrum_servers` begin with the confirmation; the per-server WARN lines of
    item 9 are unchanged. The judgment at activation is unchanged: it does not
    wait for a confirmation (D38.6.4o.10). The state still ends as soon as
    neither condition holds. The length of the period is watched in field runs
    and evaluated; it can change.
13. *Fewer than three servers.* When a coin is activated with fewer than three
    servers in its pool that are not removed, the activation logs one WARN:
    `<ticker>: only <n> Electrum servers are listed; with fewer than three the node cannot always tell a stale server from one on another chain`
    (D38.6.4o.13). It is logged whatever the value of `electrum_tip_checks`.

*Rationale.* Field reports show stale servers hundreds of blocks behind,
while healthy servers on fast chains are routinely a few blocks apart because
blocks take seconds to propagate and indexers lag under load. A tolerance
generous in time catches every real case without marking healthy servers; the
cap of 20 blocks only guards against a nonsensical block time in a coins file.
Taking the second-highest height as the tip keeps one server, whether
misconfigured (a testnet server in a mainnet list) or lying, from making every
honest server look behind. With two servers the choice is binary either way,
and a server that fell behind is far more common than one that claims a
height that does not exist. The stuck condition needs no trust in any claimed
height: a server that stays silent while another one reports new blocks is
behind, whatever it claims. Keeping reads, broadcasts and confirmation waits
off out-of-date connections is cheap and reversible, so it acts at once;
demotion in the rank keeps its two-notification and time rules. A swap's
confirmation wait polls one server so that the weakest of several servers
cannot end it early. Header agreement catches a server on another chain at
the same height without any extra request. The backoff keeps a halted chain
from turning every node into a periodic prober of the public servers.

> **Reloaded divergence (informative).** The reference ranks by list position
> and has none of these checks. No peer observes them: they change only which
> of the operator's own configured servers a node uses for what, and the
> `electrum_servers` request is additive. By the second arm of the default-side
> rule of `docs/COMPAT_SWITCHES.md`, Reloaded's variant is the default.
>
> **Compatibility:** for behavior matching GLEEC KDF, set `electrum_tip_checks`
> to `false`, together with `electrum_server_order` `listed` and
> `electrum_spread_reads` `false`. The confirmation wait of item 6 then polls
> the first maintained connection in priority order, as the reference's
> priority routing does. GLEEC KDF has no equivalent of `electrum_servers`.

> **As-built note (2026-11-09).** Items 1 to 11 are implemented, with the bound
> above the agreed tip of D38.6.4o.9. The default flip of D38.6.4o.6 is not part
> of it: `electrum_server_order` stays `random`.
>
> *Status note (2026-11-14).* The default flip is made on 2026-11-14 by a
> separate change: an absent `electrum_server_order` now means `mixed`
> (R38.6.4b-R, status note after item 3).
>
> *Pure decisions*, in `electrum_measure.rs`, so that the tests are
> deterministic: `tolerance_blocks` (item 1), `agreed_tip`, `lag`, `lead`,
> `signed_lag` and `is_beyond_tolerance` / `is_outside_tolerance` (item 2),
> `out_of_date_states` with `TipFigure` and `OutOfDate` (item 4; *behind* and
> *ahead* win over *stuck*),
> `eligible_servers` with the two-sided height bound and its header agreement
> (R38.6.4l item 4 and item 7 here; `consensus_header` finds the bytes that two or more measured servers
> share at the agreed tip height) and `remeasure_gap` (item 8).
>
> *Heights and times.* `HeaderTrack` in `electrum_rpc_client.rs` keeps, for each
> connection, its height, the time of its first header answer and the time of
> its last header notification. A later answer to a header request (a
> block-count read over the connection) moves the height only and is not a
> notification, so it neither starts nor ends a silence. An answer or a
> notification whose height cannot be read leaves all of it as it was.
>
> *The pool*, in `electrum_pool.rs`. `join_maintained` sends the header request
> on joining when the tip checks are on or the order is measured (item 3).
> `refresh_out_of_date` judges the maintained connections anew, logs each
> change of state once and returns the states (item 9, first three lines); it
> runs when a header request is answered, when a notification arrives and when
> a read-only query, a broadcast or a confirmation wait starts. `in_turn_for`
> and `split_by_state` apply item 5: up-to-date connections first, then the
> out-of-date ones, in priority order among themselves, and every connection
> counts as up to date when none is. The turn, the near decision (`near_flags`)
> and the floor of two run over the up-to-date connections only. A broadcast
> and, with `electrum_spread_reads` `false`, a read take the same order, so the
> first of the up-to-date connections in priority order goes first.
> `choose_wait_server` is item 6. `run_remeasure_round` connects at most
> `REMEASURE_IN_FLIGHT` (4) candidates at a time, logs the replacement line and
> the lines for measured servers that are behind, and moves `backoff_level`;
> `remeasure_gap` turns it into the least time before the next round, which the
> measure loop uses. `servers_view` is item 10. `parse_tip_checks` and
> `electrum_tip_checks` are item 11; the switch is read once per coin activation
> in the UTXO coin builder, next to `electrum_spread_reads`, and the Z-coin
> Electrum fallback goes through the same builder.
>
> *The confirmation wait.* `UtxoRpcClientEnum::wait_for_confirmations` sends an
> Electrum client to `wait_for_confirmations_on_one_server`, which asks the
> chosen server with `transaction_get_verbose_from` and
> `blockchain_headers_subscribe_from`. Native and lightwalletd paths are
> unchanged. A failure in transport makes the wait choose again; an error
> answer of the server (for example a transaction it does not know yet) is an
> answer, as in R38.6.4f item 1, and the wait stays on that server. Where the
> wait has no other choice it goes on with the server it has, after the usual
> pause.
>
> *The request.* `electrum_servers` is `rpc_command/electrum_servers.rs`, routed
> in the v2 dispatcher; a ZHTLC coin, a native coin and any other coin answer
> `CoinIsNotElectrum`. `near` is the near decision even with
> `electrum_spread_reads` `false`; `latency_ms` is `null` for a server that is
> not maintained, which has lost its smoothed latency (R38.6.4a item 7).
>
> *Points the chapter leaves open, as implemented.* (1) A server both measured
> in a round and reported by its maintained connection counts once, with its
> measured height. (2) For header agreement, two different bytes that are each
> shared by as many servers as any other bytes leave no consensus, so nobody
> is excluded; a server whose answer carried no readable header bytes is not
> compared. (3) The reason of a replacement is `behind`, else `stuck`, else
> `slow`; a lone connection that went silent (R38.6.4l item 6) is `stuck`.
> (4) The least times of item 8 are those between the starts of two rounds: 5,
> 10, 20, 40, 60, 60. The measure loop also stops calling for a round once no
> connection is poor any more. (5) A header notification wakes the measure
> loop when the tip checks are on, so that a connection that turns stuck or
> recovers is noticed at once.
>
> *The bound above the agreed tip (D38.6.4o.9).* A measured server is eligible
> only within *T* of the agreed tip on both sides, and a maintained connection
> more than *T* above the running agreed tip is out of date as *ahead*: it takes
> no read and no broadcast before the others, and the node logs the *ahead* line
> once. A measurement round logs the *behind* or the *ahead* line for each
> measured server outside the tolerance. Being ahead is not a trigger of
> R38.6.4l item 6 and gives no reason for a replacement; with exactly two
> servers the agreed tip is the higher one, so nobody is ahead. The `lag` of the
> `electrum_servers` answer is signed, through `signed_lag`: negative above the
> agreed tip, `null` where the distance does not fit in 64 bits.
>
> *Tests.* Pure: `t38_6_4o_1_the_tolerance_from_avg_blocktime`,
> `t38_6_4o_2_the_agreed_tip_is_the_second_highest_height`,
> `a_connection_is_behind_only_beyond_the_tolerance`,
> `a_connection_is_stuck_when_another_one_hears_news_and_it_does_not`,
> `a_liar_above_the_others_is_ahead_and_does_not_make_them_look_behind`,
> `t38_6_4o_8_a_header_that_differs_from_two_agreeing_ones_is_not_eligible` and
> `the_remeasure_gap_doubles_up_to_the_cap` in `electrum_measure.rs`. Against the
> stub server, in `electrum_pool_tests.rs`:
> `t38_6_4o_3_a_connection_behind_is_out_of_date_at_once_and_reversibly`,
> `t38_6_4o_4_two_servers_one_far_behind`,
> `t38_6_4o_5_a_silent_connection_is_stuck_while_another_one_hears_news`,
> `t38_6_4o_6_a_liar_does_not_steer`,
> `t38_6_4o_7_the_confirmation_wait_sticks_to_one_server`,
> `t38_6_4o_8_a_header_that_differs_makes_a_server_not_eligible`,
> `t38_6_4o_9_rounds_without_a_replacement_back_off_and_the_poor_condition_resets_them`,
> `t38_6_4o_9_a_round_connects_at_most_four_servers_at_a_time`,
> `t38_6_4o_10_the_view_lists_the_pool_and_sends_nothing` and
> `t38_6_4o_11_the_switch_turns_the_checks_off` with
> `t38_6_4o_11_with_the_checks_off_the_wait_polls_the_first_connection`; the
> request itself in `rpc_command/electrum_servers.rs`. The stub server has
> `set_header_hex` for the header bytes of each tip. The tests of R38.6.4a and
> R38.6.4l that are not about R38.6.4o build their pool with the tip checks off,
> so that they stay about those rules.
>
> *Known limitations (code read of 2026-11-09).*
> - The confirmation wait of item 6 chooses again only when a request to its
>   server fails. When the chosen server turns out of date during the wait, the
>   wait keeps polling it until its deadline. A behind or stuck server reports
>   too few confirmations, never too many, so the cost is a longer wait or a
>   timed-out swap step, not a false confirmation. Choosing again when the
>   chosen server turns out of date is a candidate for a later change.
>
>   *Status note (2026-11-21).* Closed by the status note of 2026-11-21 under
>   item 6: the wait now chooses again when its server turns out of date and
>   after three error answers in a row.
> - The per-round WARN of item 9 (a measured server behind or ahead) is logged
>   in every re-measure round, without the once-per-change state of the WARNs
>   for maintained connections, and can repeat the text of such a WARN for the
>   same server. Rounds are 5 to 60 minutes apart (item 8).
> - With exactly two servers the agreed tip is the higher one (D38.6.4o.2): a
>   single server that claims a height that does not exist makes the honest
>   one look behind, and reads then prefer it. With three or more servers the
>   bound above the agreed tip (D38.6.4o.9) covers that case.
> - When a comparison of item 12 is still running as an activation fails, it
>   ends later and can log the after-activation ERROR line of item 12 for the
>   coin that was not registered. It has no effect beyond that log line.

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

*Status note (2026-11-14).* Since the default is `mixed` (R38.6.4b-R, status
note after item 3), an absent switch means `mixed`, and every other value,
including a different letter case or a non-string, means `mixed` with one
warning; `random` still means `random`. The test asserts these.

T38.6.4.14 *A dead and a refusing server at activation.* With three stubs,
one that never accepts a connection, one that refuses every connection at
once and one good one, `max_connected` 2 and an injected draw that ranks the
good stub last, under `mixed` and under `random`: when the build returns, the
maintained set shall hold the good stub (R38.6.4k-R item 3, D38.6.4l.2), and a
balance read with the caller's budget of 20 s (shortened timings scale alike)
shall be answered by the good stub well within that budget, in every one of
20 runs (R38.6.4f item 2, R38.6.4j item 1).

T38.6.4j.1 *Sliding retry window.* With an empty maintained set, `max_connected`
2 and three servers in the visiting order dead (never accepts), refusing (fails
at once) and good, the good server shall be asked as soon as the refusing one
has failed, not after the dead one's attempt ends, and shall answer the
request. Each attempt's budget shall follow R38.6.4j item 2 with the servers
not yet started (R38.6.4f item 2, R38.6.4j).

Two activations shall draw independently of each other. Over many draws with
the real random source, each server shall come first roughly equally often,
within generous bounds (R38.6.4b-R).

T38.6.4a.1 *Near sessions from figures.* A test of the near-session decision
alone (R38.6.4a item 6), with smoothed latencies given in milliseconds:
- 25, 27, 32, 108, 230 and 266 with *F* = 2: the first three are near;
- the same figures with *F* = 0: all six are near;
- 25, 27 and 32 with *F* = 0.5, taken as 1: all three are near, through the
  offset term (20 ms by default);
- one session without a smoothed latency among the six of the first case: it
  is not near (D38.6.4a.4);
- no session with a smoothed latency: all are near;
- 38 and 230 with *F* = 2: both are near (the floor of two);
- 25, 230 and 266 with *F* = 2: the first two are near, the third is not;
- 230, 25 and 230 with *F* = 2: the first two are near (the lowest figure,
  then the earlier of the two equal ones), the third is not;
- 25, 230 and no smoothed latency with *F* = 2: the first two are near (the
  floor of two picks among sessions with a figure), the third is not
  (D38.6.4a.4);
- a single session with 230: it is near.

T38.6.4a.2 *Reads avoid a far session.* With three maintained stubs, two
answering in about 5 ms and one in about 200 ms, and each with at least one
sample, 30 read-only queries in a row shall all be answered by the two fast
stubs, in turn, each answering 15 within one.

T38.6.4a.3 *A far session is still a fallback.* In the setting of T38.6.4a.2,
with both fast stubs failing a read-only query, the slow stub shall answer it.

T38.6.4a.4 *A recovered session comes back.* In the setting of T38.6.4a.2,
with the slow stub's delay dropped to about 5 ms and the ping interval
shortened, the slow stub shall answer no read-only query among the first
ones after the drop. It shall answer them again once its smoothed latency
is within the limit, and its figure shall be within the limit when it first
answers one.

T38.6.4a.5 *Samples under `random` and `listed`.* Under `listed` and under
`random`, ordinary requests and pings shall produce a smoothed latency for each
maintained session. With `electrum_tip_checks` `false`, no header request shall
be sent on joining; with the default `true`, exactly one is (R38.6.4o item 3;
compare T38.6.4l.15).

T38.6.4a.6 *Near factor and offset values.* An absent
`electrum_spread_near_factor` means 2, without a warning. `0` makes every
maintained session near. `0.5` means 1. A string, a negative number and `null`
mean 2, each with one warning naming the key. An absent
`electrum_spread_near_offset_ms` means 20, without a warning. `0` leaves the
factor alone to decide, and `50` admits a session up to 50 ms behind the
fastest. A string, a negative number and `null` mean 20, each with one
warning naming the key. With `electrum_spread_reads` `false`, read-only queries
follow the priority order whatever the two values.

T38.6.4a.7 *Writes keep their routing.* In the setting of T38.6.4a.2 with the
slow stub first in priority, a broadcast and a subscription shall go to the
slow stub.

T38.6.4a.8 *Two sessions keep both in turn.* With two maintained stubs, one
answering in about 5 ms and one in about 200 ms, and each with at least one
sample, 30 read-only queries in a row shall be answered by both stubs in
turn, each answering 15 within one.

T38.6.4a.9 *A session that has answered nothing.* With two maintained stubs,
one with a smoothed latency and one joined whose header request on joining is
not answered, every read-only query shall go to the first; once the second
answers its header request it shall have a figure and be judged by the limit
and the floor of two as usual. With no maintained session having a figure,
both shall be near (item 6, item 7, D38.6.4a.4).

The tests of R38.6.4l run in-process against the stub Electrum server of
`mm2src/coins/utxo/rpc_clients/electrum_stub_server.rs`, with the per-method
reply delay, settable tip height and pushed header notifications that the
plan adds to it, an injected random source, and an injected clock or
shortened pool timings. Latencies are set in milliseconds. Unless a test says
otherwise, its coin has no `avg_blocktime`, so its tolerance is *T* = 1
(R38.6.4o item 1) and the block figures below are those of that tolerance.

T38.6.4l.1 *Switch values.* `mixed` and `measured` shall parse as themselves,
without a warning; an absent switch, `random`, `listed` and every other value
shall parse as T38.6.4.13-R states. Under `random` and `listed`, no stub shall
receive a header request as part of its probe attempt (each receives exactly
one after it joins the maintained set, R38.6.4o item 3), and no re-measure
round shall run (R38.6.4l item 1).

T38.6.4l.2 *Activation measurement.* Under `mixed` with three stubs, each stub
shall receive exactly one header request during the probe, right after
`server.version`. The latencies and heights the rank is computed from shall be
the stubs' reply delays and tip heights. A silent stub shall have no
measurement (R38.6.4l item 2).

T38.6.4l.3 *Eligibility.* With tips at *H*, *H* and *H* − 2, the third
server shall not be eligible; with tips at *H*, *H* − 1 and *H* − 2 the best
tip is *H* − 1 (R38.6.4o item 2) and all three shall be eligible. With a best latency of 50 ms, 300 ms shall be
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
(R38.6.4l item 6). At pool level the answer to the header request on joining
is the first of a connection's samples (R38.6.4a item 7), so a pool test
reaches the five samples with four further answers.

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
two consecutive notifications, shall trigger P1 for it. Under `random` with
`electrum_tip_checks` `false`, the same join shall send no header request;
with the default `true` it sends one, but no trigger runs (R38.6.4l items 1
and 6, R38.6.4o items 3 and 11).

T38.6.4m *TLS server authentication (R38.6.4m).*
- (a) The default configuration refuses a self-signed certificate, in both the
  CA-flagged and the unflagged form, and a certificate whose subject
  alternative names do not match the host name; it accepts a certificate
  chained to a test trust anchor.
- (b) With `disable_cert_verification`, a self-signed version 3 certificate, an
  expired one and one with a mismatched name are accepted, on TLS 1.2 and on
  TLS 1.3.
- (c) With `disable_cert_verification`, a version 1 certificate and an RSA-1024
  certificate are refused. These need static DER fixtures.
- (d) An IP literal is refused before connecting, and the server is removed.
- (e) The WARN line of item 6 appears exactly once per activation, names only
  `host:port`, and does not appear for `TCP` or for a pinned server.

All five are covered. (a), (b) and (d) by `tls_tests` and the pool tests that
existed before; (c) by `t38_6_4m_c_a_version_1_certificate_and_an_rsa_1024_certificate_are_refused_under_the_flag`
(static DER fixtures in `mm2src/coins/utxo/test-fixtures/electrum-tls/`, checked at the
verifier because a `rustls` server cannot sign with a 1024-bit RSA key); (e) by
`t38_6_4m_e_the_warning_is_logged_once_per_activation_for_host_and_port_only`.

T38.6.4n *Electrum server key pin (R38.6.4n).* With test certificates on a
loopback server:
- a matching pin is accepted on TLS 1.2 and on TLS 1.3, and a second pin in an
  array matches;
- a mismatch is refused with the text of item 5, logged at WARN, and the server
  is suspended, not removed;
- a self-signed, expired and misnamed certificate with the pinned key is
  accepted;
- a server presenting another key under the same subject is refused;
- malformed pins and a pin on `TCP` are removed with their texts, with no
  connection attempted;
- the flag together with a pin behaves as the pin alone, with no WARN of
  R38.6.4m item 6;
- one INFO line of item 4 per activation.

Covered by the `t38_6_4n_*` tests of `tls_tests` in `electrum_rpc_client.rs`: the
matching pin on both versions and in every accepted form, the mismatch text, the
three certificate defects with the pinned key, another key under the same subject,
the flag together with a pin, a pin on a version 1 and on an RSA-1024 certificate,
the pin forms and their malformed counterparts, the classification of an entry when
it is read, the removal without any connection, the suspension and the WARN of a
mismatch, and the INFO line (in the test of T38.6.4m (e)); the request test in
`coins_activation` reads the key from an activation request.

T38.6.4o.1 *Tolerance.* `avg_blocktime` 600, 150, 60, 15, 10 and 1.5 shall
give *T* = 1, 2, 3, 12, 18 and 20; 0.5 shall give 20 (the cap); an absent key,
0, −60, `"60"` and `null` shall give 1 (R38.6.4o item 1).

T38.6.4o.2 *Agreed tip.* Heights {*H*} shall give no agreed tip; {*H*,
*H* − 1000} shall give *H*; {*H*, *H*, *H* − 1} shall give *H*; {*H* +
1,000,000, *H*, *H* − 1} shall give *H*; {*H* + 5, *H* + 3, *H*} shall give
*H* + 3. A server above the agreed tip shall have lag 0 and a lead of its
distance above it (item 2).

T38.6.4o.3 *Behind, at once and reversibly.* Under `random`, with three
maintained stubs and a coin with `avg_blocktime` 60 (*T* = 3), each stub shall
receive exactly one header request after joining. With tips *H*, *H* and *H*
− 4, the third shall be out of date as *behind* after its first answer, 30
read-only queries shall be answered by the first two only, and one WARN line
of item 9 shall name the third stub. When the third stub notifies *H*, it
shall be up to date again, with one WARN line, and shall take reads again. A
stub at *H* − 3 shall never be out of date (items 3–5, 9).

T38.6.4o.4 *Two servers, one far behind (the field case).* Under the defaults,
with two maintained stubs at *H* and *H* − 1000 and a coin with
`avg_blocktime` 60, every read-only query and every broadcast shall go to the
stub at *H*, although the floor of two near sessions would otherwise keep both
in turn (items 2, 5).

T38.6.4o.5 *Stuck.* With three maintained stubs and the shortened timings of
the test harness, two stubs notifying new tips and one silent, the silent one
shall become out of date as *stuck* after *P* and not before, and shall leave
the read turn. With every stub silent for longer than *P*, none shall be stuck.
With a single maintained stub, item 4 shall not apply; the lone-connection rule
of R38.6.4l item 6 is unchanged (item 4).

T38.6.4o.6 *A far-ahead server fails the activation.* With three stubs at *H*
+ 1,000,000, *H* and *H*, the activation shall fail with the error of item 12
naming all three servers, and the coin shall not be registered. A stub at *H*
+ *T* instead shall not fail it. With `electrum_tip_checks` `false`, the first
case shall activate (items 2, 11, 12; D38.6.4o.9).

T38.6.4o.7 *Confirmation wait sticks.* With two maintained stubs, *A* first in
priority order and *B* second, *A* answering the transaction with 0
confirmations and *B* with 3, a wait for 1 confirmation shall not end while
*A* keeps answering, whatever the read counter, and every transaction request
of the wait shall reach *A*. When *A* starts to fail, the wait shall continue
on *B* and end on its answer. With *A* out of date, the wait shall choose *B*
from the start (item 6).

T38.6.4o.8 *Different headers.* With three stubs at the same height *H*,
two answering the same header bytes at *H* − *T* and one other bytes there,
the activation shall fail with the error of item 12. With three stubs that
differ only in the header at *H* and agree at *H* − *T*, the activation shall
succeed (a race at the tip), and no stub shall be excluded for it (items 7,
12; D38.6.4o.12).

T38.6.4o.9 *Backoff.* With `max_connected` 1, two listed stubs and every
candidate ineligible, successive re-measure rounds shall start 5, 10, 20, 40,
60 and 60 minutes apart (shortened timings scale alike). When the poor
condition clears, the next round shall again wait 5 minutes. No round shall
have more than 4 connection attempts in flight (item 8).

T38.6.4o.10 *The `electrum_servers` request.* For an active coin with three
stubs, the answer shall list three servers in rank order with host-only
labels, the tolerance, the agreed tip, each height and lag, and
`out_of_date` `"behind"` for a stub more than *T* behind. No stub shall
receive any request because of it. An inactive ticker shall answer
`NoSuchCoin`, and an active coin served by a native daemon shall answer
`CoinIsNotElectrum` (item 10).

T38.6.4o.11 *Switch.* With `electrum_tip_checks` `false` under `random`, no
stub shall receive a header request after joining, a stub 1000 blocks behind
shall stay in the read turn, and a confirmation wait shall poll the first
maintained connection in priority order. A non-boolean value shall be taken as
`true` with one warning; an absent key and `null` shall log nothing (item 11).

T38.6.4o.12 *Two servers, stale or diverged.* With two stubs at *H* and *H* −
1000 that answer the same header at *H* − 1000 − *T*, the activation shall
succeed, every read shall go to the stub at *H*, and the other shall be out of
date as *behind*. With different headers there, the activation shall fail with
the error of item 12 (items 4, 12; D38.6.4o.13).

T38.6.4o.13 *The activation window.* With three stubs, one of them silent, the
activation shall judge the other two and return within 5 s (shortened timings
scale alike) of its first connection, and the silent stub shall not be named
in any error (item 12; D38.6.4o.10).

T38.6.4o.14 *Disagreement after activation.* With three maintained stubs at
*H*, when one moves to *H* + *T* + 1 while the others stay, the coin shall
enter the disagreement state with one ERROR line. A new order with the coin as
base or rel, the start of a new swap and a withdrawal shall be refused with
the error of item 12, while a balance read succeeds. When the others reach
the same height, the state shall end with one WARN line and the refusals
shall stop. `electrum_servers` shall show `chain_disagreement` during the
state and `null` after it (items 9, 10, 12; D38.6.4o.11).

T38.6.4o.15 *Fewer than three servers.* Activating a coin with two servers,
and with one, shall log the WARN of item 13 once; with three it shall not
(item 13).

T38.6.4o.16 *Comparison limit.* Two servers compared once shall not be
compared again within 5 minutes unless one of their heights has moved by more
than *T* (item 12).

T38.6.4o.17 *Merged-mining headers.* Two servers that answer the same 80-byte
base header with different bytes after it, in their header notifications and
below the tip, shall activate the coin, shall start no comparison from the
notifications, and shall not put the coin into the disagreement state, also
after both have moved more than *T* blocks on; with two such servers far apart
the activation shall succeed. A difference inside the 80 bytes shall still fail
the activation (items 7 and 12, D38.6.4o.14).

T38.6.4o.18 *Same block.* Two headers shall count as the same block when their
first 80 bytes are equal, whatever follows them; a difference in the 80th byte
shall count; headers shorter than 80 bytes shall be compared whole (items 7
and 12, D38.6.4o.14).

T38.6.4o.19 *A short lead.* After activation, a server more than *T* blocks
ahead of the other two for less than the confirmation period shall put the
coin into no disagreement state and log no ERROR line; a server that stays
that far ahead shall put the coin into the state once the period has passed,
and not before, with one ERROR line (item 12, D38.6.4o.15).

T38.6.4o.20 *A divergence confirmed.* After activation, two servers found at
different headers that answer the same header when they are compared again
after the confirmation period shall put the coin into no disagreement state;
when the difference is still there, the coin shall enter the state, and not
before the period has passed (item 12, D38.6.4o.15).

T38.6.4o.21 *The confirmation period.* With an `avg_blocktime` of 600 s the
period shall be 1,200 s, with 60 s it shall be 120 s, and with 10 s or none it
shall be 60 s (item 12, D38.6.4o.15).

T38.6.4o.22 *An unanswered second look.* After activation, when the comparison
that would confirm a divergence goes unanswered, the pair shall stay
unconfirmed, the coin shall not enter the disagreement state, and no further
wake shall be scheduled for the period end that has passed; the pair is tried
again after the comparison limit (item 12, D38.6.4o.15).

T38.6.4o.23 *The wait leaves a server that turns out of date.* With two
maintained stubs, *A* chosen first and up to date when the wait starts and
answering 0 confirmations, and *B* answering 3: when *A* turns out of date
(item 4) during a wait for 1 confirmation, the wait shall continue on *B* and
end on its answer, with one INFO line naming *A*, *B* and the reason. While
*A* stays up to date, the wait shall stay on *A* (T38.6.4o.7). When a request
to *A* fails first, so that the wait moves to *B*, and *B* then turns out of
date while *A* is up to date again, the wait shall return to *A* and end on its
answer. With
`electrum_tip_checks` `false` the wait shall stay on *A* (item 6, status note
of 2026-11-21).

T38.6.4o.24 *The wait leaves a server that answers errors.* With two
maintained stubs, *A* chosen first and *B* answering 3 confirmations: when *A*
answers the transaction request with an error three times in a row, the wait
shall continue on *B* and end on its answer. When *A* answers two errors and
then 0 confirmations, the wait shall stay on *A*, and the count shall start
again. With `electrum_tip_checks` `false`, error answers of *A* shall keep the
wait on *A* until its deadline (item 6, status note of 2026-11-21).

D38.6.4l.1 *(Resolved 2026-10-28, maintainer decision.)* *When `mixed`
becomes the default.* The plan intends `mixed` as the default and left the
moment to the maintainer: with the implementation, or after a field run on a
few coins, RIN first. The maintainer chose the field run. The default stays
`random` until a field run on RIN with `mixed` has passed; the switch to
`mixed` is then a separate change.

*Status note (2026-11-14, maintainer decision).* The maintainer made `mixed`
the default on 2026-11-14, for the release 0.4.0-beta.1, in a separate change
(R38.6.4b-R, status note after item 3).

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

D38.6.4a.1 *(Decided 2026-11-02, maintainer Decisions 8 and 22; under
monitoring; item 6 amended by D38.6.4a.3.)* *The defaults of item 8.* The
near sessions of item 6 are on by default, with *F* = 2 and *A* = 20 ms. A latency study of 2026-11-02 checked
the rule against the server lists coins actually carry, on 13 coins with one to
six servers. It found the rule helpful where a coin's servers differ and
neutral where they do not, and the maintainer kept the rule and both defaults.
The decision is under monitoring, not closed. The maintainer confirms or
changes it on the behavior seen in the field and on users' reports. That is
also why both values are switches. Decision 22 also chose a check of the
chain tip, so that a near server two or more blocks behind the best tip stops
being near (plan [`order-posting-latency`](../plans/order-posting-latency.md),
work package 5). That check is now R38.6.4o items 4 and 5, with the limit of
the coin's tolerance instead of two blocks (D38.6.4o.1).

D38.6.4a.2 *(Decided 2026-11-02, maintainer Decision 15: kept as written; under
monitoring.)* *The form of the limit.* Item 6 bounds a
near session by the larger of *F* × *f* and *f* + *A*. The maintainer's own
wording was "twice the fastest plus 20 ms", that is *F* × *f* + *A*. The sum
is always the looser of the two. When *f* is at least *A* / (*F* − 1), 20 ms
with the defaults, it admits sessions up to *A* further behind. Below that it
admits them up to (*F* − 1) × *f* further behind. Example: with *f* = 25 ms the
larger-of form gives 50 ms, the sum 70 ms. The maintainer kept the larger-of
form of item 6. The latency study of 2026-11-02 found the same near sessions
under both forms on every one of its 13 sampled coins.

D38.6.4a.3 *(Decided 2026-11-06, maintainer Decision CX.)* *A floor of two
near sessions.* The DOGE row of the read-spreading timing runs
(`docs/release-tests/0.4.0-beta.1.md`) found a loop of 200 orders about 11 %
slower under items 6–8 than on 0.3.0-beta.1, on a coin with two servers that
each limit a session. A run of the same build with the near factor at 0 took
the time of 0.3.0-beta.1, and the default setting reproduced the slowdown, so
item 6 was the cause. The maintainer chose a floor of two near sessions, over
keeping item 6 as it was, over a hysteresis on leaving the near set, and over
the factor 0 as the default. The defaults of D38.6.4a.1 stay, under
monitoring.

D38.6.4a.4 *(Decided 2026-11-09, maintainer.)* *No reads for a session that has
answered nothing.* A field case with one server that never answered and two
that did showed that "no figure, so near" lets a joined but silent connection
take its turn of the reads, and every such read waits for its attempt bound
before the failover. A session without a figure is therefore not near while
another maintained session has one, and the answer to the header request on
joining counts as a sample, so an answering session gets a figure at once.

D38.6.4j.1 *(Decided 2026-11-09, maintainer.)* *A sliding retry window.* With
fixed groups of `max_connected`, a group whose members are one server that
never accepts and one that refuses at once waits for the first server's whole
attempt, and the group that holds a good server is not reached in time; a
field activation failed that way under a 20 s caller deadline. The retry now
starts the next server as soon as an attempt ends without an answer, and each
attempt's budget is derived from the servers not yet started. With
`max_connected` servers in flight the bound of the whole retry is unchanged.

D38.6.4m.1 *(Resolved 2026-11-05 by a lab run.)* *A refusal under the flag in the
field.* The item was raised because a server with `disable_cert_verification` was
reported refused. A lab run with seven certificates against this build and two
reference builds (`docs/release-tests/0.4.0-beta.1.md`, "Electrum TLS") found
accept and refuse identical on all three. Under the flag, self-signed, expired
and wrong-name certificates were accepted. Only the refusals by design of items
3 and 5 of R38.6.4m remained: a version 1 certificate and an RSA key below 2048
bits. An IP literal and a TLS version below 1.2 are also refused by design.

D38.6.4n.1 *(Open, 2026-11-05.)* *Pass-through by the coins file and the
wallets.* Whether the coins file and the wallet GUIs pass `spki_sha256` through
to the activation request. Until they do, the pin is usable only from activation
requests written by hand.

D38.6.4n.2 *(Open, 2026-11-05.)* *Trust on first use.* Whether a node should
remember the key a server first presents and warn on a change. It is not part of
R38.6.4n. It would be weaker than a published pin, because the first contact
itself is not authenticated.

D38.6.4o.1 *(Decided 2026-11-09, maintainer.)* *The tolerance.* A window of
180 s converted to blocks by `avg_blocktime`, at least 1 and at most 20 blocks.
Alternatives were a window of 120 s or 300 s and a cap of 5 or 10 blocks, or
none. A cap of 5–10 would let the fastest coins tolerate only 15–100 s of
spread, below their natural spread; every coin with a block time of 18 s or
more is unaffected by any cap between 10 and 20. The figure replaces the fixed
1-block eligibility limit and 2-block P1 limit of R38.6.4l items 4 and 6; for a
coin without `avg_blocktime` (*T* = 1) both stay as they were.

D38.6.4o.2 *(Decided 2026-11-09, maintainer.)* *The agreed tip.* The
second-highest height with 3 or more servers, the higher of two with 2.
Alternatives were the median and the plain maximum of R38.6.4l as written
before. The median would let a stale majority define the tip; the maximum lets
one server define it.

D38.6.4o.3 *(Decided 2026-11-09, maintainer.)* *Confirmation waits.* Sticky to
one server only. Corroborating a "confirmed" answer by a second server for
coins without `spv_conf` was considered and left for a later change.

*Status note (2026-11-21, maintainer decision).* The wait still polls one
server at a time and still trusts that server's answer alone, but it no
longer stays with a server that is out of date or that answers errors three
times in a row (status note of 2026-11-21 under item 6). Corroboration
remains open.

D38.6.4o.4 *(Decided 2026-11-09, maintainer.)* *Header request on joining under
every order.* Yes: without heights no connection can be judged under the
default order. The cost is one small request per joining connection, over a
connection that is in use anyway. Measurement rounds, triggers and
replacements stay limited to `mixed` and `measured`.

D38.6.4o.5 *(Decided 2026-11-09, maintainer.)* *Observability and the switch.*
WARN lines once per change of state and the read-only `electrum_servers`
request. The switch `electrum_tip_checks` is the opt-back mechanism
`docs/COMPAT_SWITCHES.md` requires; its default follows the second arm of the
default-side rule.

D38.6.4o.6 *(Decided 2026-11-09, maintainer.)* *`mixed` as the default.* The
change of D38.6.4l.1 becomes due after a 72-hour field run with this rule in
place: on RIN, on DGB with four or more servers, on one coin with 600 s blocks,
and in a row with two servers, one of them far behind. It passes when every
replacement has a logged reason, no server is marked behind or stuck while it
was within *T* of the agreed tip within 60 s of the mark, loop and swap times
stay within 10 % of `random`, and no read reaches the far-behind server after
activation. The default changes in a separate commit after the run passes.

*Status note (2026-11-14, maintainer decision).* The maintainer decided on
2026-11-14 to make `mixed` the default for the release 0.4.0-beta.1; the
change is made in a separate commit (R38.6.4b-R, status note after item 3).

D38.6.4o.7 *(Decided 2026-11-09, maintainer.)* *The stuck period.* The larger of
3 × `avg_blocktime` and 10 minutes, the figure of the lone-connection rule of
R38.6.4l item 6. Judging staleness by the tip header's timestamp was
considered and not chosen: it needs per-family header parsing, says little on
proof-of-stake chains, and trusts the local clock.

*Status note (2026-11-15, clean side; maintainer decision).* For the stuck
condition of item 4 this decision is replaced by the amendment of 2026-11-15
under item 4: the larger of 2 × `avg_blocktime` and 180 s, counted from another
connection's first report of a higher height. The lone-connection rule of
R38.6.4l item 6 keeps the figure above.

D38.6.4o.8 *(Decided 2026-11-09, maintainer.)* *Header agreement.* Applied in
measurement rounds only, at the agreed tip height only, and only with two
agreeing servers. A natural reorganization at the tip resolves itself in the
next round, because nothing is suspended.

D38.6.4o.9 *(Decided 2026-11-09, maintainer; replaces the derived rule that
first stood here, under which a far-ahead server was excluded from the reads
and warned about.)* *A server far ahead fails the activation.* A server more
than the tolerance above the height the others agree on is not hidden: it may
be the one server that followed a network upgrade while the others stopped, so
the node cannot know which side is right. The activation fails with a clear
error instead (item 12).

D38.6.4o.10 *(Decided 2026-11-09, maintainer.)* *When activation judges.* After
the first connection, the activation waits up to 5 s for the other servers'
heights and comparisons, then judges on what arrived. Waiting for every server
would let a server that never answers delay the activation by its whole
establishment bound again; judging only on what is known when the build
returns would almost never see more than one height.

D38.6.4o.11 *(Decided 2026-11-09, maintainer.)* *A disagreement after
activation.* The coin refuses new orders, new swaps and withdrawals until the
servers agree again, logged at ERROR; reads and running swaps go on. Logging
only, or following the higher server, were not chosen.

D38.6.4o.12 *(Decided 2026-11-09, maintainer.)* *Different headers at the
same height.* A fork whose sides both go on is treated like a far-ahead server:
it fails the activation, and later it puts the coin into the disagreement
state. The majority is not followed silently. The comparison is made below the
tip, so a race between two new blocks does not count.

D38.6.4o.13 *(Decided 2026-11-09, maintainer.)* *Two servers.* With two
servers far apart, a header comparison at a common height tells a stale server
on the same chain (the higher one is used) from two servers on different
chains (the activation fails). Lists of fewer than three servers get a warning
at activation, because with them the node cannot always tell the cases apart.

D38.6.4o.14 *(Recorded 2026-11-10, clean side; defect fix.)* *The base header
names the block.* On merged-mining (AuxPoW) coins a header carries, after its
80-byte base header, a merged-mining proof that the block hash does not cover,
and two Electrum servers can store one block with different proofs. On DOGE,
two public servers answered the same height with 763 and 727 bytes whose first
80 bytes were equal. Comparing whole headers treated that block as two chains:
the comparison at that height kept finding a difference, the repeated
comparison at the same height never ended the state, and the coin refused new
orders, swaps and withdrawals until it was activated again. The comparisons
therefore compare the first 80 bytes. Those bytes start with the version, the
previous block hash and the merkle root in the Bitcoin-style layout that UTXO
headers share, the longer Zcash-style and proof-of-stake headers included, so
a real fork still differs in them; no per-coin setting is needed. On
Zcash-style headers the time and the nonce follow byte 80, so two sibling
blocks with the same parent and the same merkle root would compare equal at
the fork height. Such siblings usually differ in their coinbase and so in their
merkle root; in any case the previous block hash differs at every later
height, and the comparisons are made below the tip.

D38.6.4o.15 *(Decided 2026-11-10, maintainer.)* *A disagreement after
activation is confirmed before it stops trading.* A field log of a maker
running 16 hours with many coins showed the disagreement state begin and end
again within seconds or minutes:
- VRSC entered it 43 times. One of three servers was 4 to 6 blocks ahead of the
  other two, with *T* = 3, and the others caught up after 0 to 11 s.
- NENG entered it 5 times (4 blocks ahead, up to 8 s), and DPC and FIRO once
  each (2 blocks ahead with *T* = 1, up to 2 s).
- CHTA entered it 5 times with a difference between headers that was gone at
  the next comparison, after 210 to 606 s.

Each entry logged an ERROR line and refused the maker's orders while it
lasted. The field log does not show why the servers differed for these short
times; a disagreement that lasts is the case the state is for. After
activation, therefore, a disagreement puts the coin into the state only after
it has lasted two block times, and at least 60 s. The activation itself keeps
its 5 s judgment. The length of the period is watched in field runs and
evaluated.

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
>
> **Coverage note (2026-11-09).** The deterministic tests cover T38.6.4o.1 –
> T38.6.4o.11 against the in-process stub server and as pure functions, as the
> as-built note after R38.6.4o lists them. The tests of T38.6.4a.5 and T38.6.4l.15
> keep their variants with `electrum_tip_checks` `false` and add the default
> one. The request
> `electrum_servers` is tested at the handler, with a native coin for
> `CoinIsNotElectrum` and an inactive ticker for `NoSuchCoin`; its routing in the
> dispatcher has no test of its own.

*Status note (2026-11-23, clean side; Reloaded-original). The swap evidence of
CRD ch.51 R100 for UTXO coins, BCH and QTUM over Electrum.* It changes no
sentence above.

1. *Directed reads.* Each read a swap step rests on under R100 goes to one named
   server and reads that server at its own tip: the counterparty's payment by
   its txid (compared byte for byte), the first history entry of the payment
   address (whether the own payment was sent), the spend of the payment's output
   (the searches and the wait for a spend), the confirmations of a payment, and
   the median time past of the refund gate, whose tip and headers come from the
   same server.
2. *Which servers.* The pool's usable servers, in the pool's own order of
   preference, one per host. Each is named by host and port only.
3. *Tip gap.* Two servers whose tips lie further apart than the pool's own
   tolerance in blocks, the one that judges a server up to date, leave the
   refund gate's read undetermined when they agree.
4. *A claim proves itself from the payment.* A spend of the payment's output
   proves itself when it takes the claim path, its redeem script hashes to the
   payment output's script hash, and its secret satisfies the hash that redeem
   script commits to. Where the swap's secret hash is known, the secret is
   checked against it. Such a claim is taken from one server.
5. *Confirmations.* The wait for confirmations stays on one server as before;
   its final decision that a payment has enough confirmations must then be
   agreed by two servers, or given by the trusted node.
6. *Native mode.* A coin served by its own daemon reads that daemon as before
   (ch.51 R100 item 6).
7. *Every server.* The look on every server that follows a failed spend search
   is, under R100, the agreed search of a pair, and in addition every other
   server is asked for a claim that proves itself; such a claim is taken from
   any server, and every other answer of those servers is ignored. When the
   pair's search is undetermined and no server shows such a claim, the look
   finds no spend, and the step stays undetermined and waits: the answer is
   never "not spent" (ch.51 R100 item 3).

### 38.6.4Q QTUM and QRC20 over Electrum: clients, methods and shared wallet state

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-15,
> measure 0.02 (attestation in §38.9).**
> This subsection was written on 2026-11-15 by the KDF Spec Reader (Spec
> Reader round 0.5.0-1) under the two-team procedure of chapter 01 §1.4A,
> from four inputs: the reference lineage at the rows `v2.6.0-beta` and
> `v3.0.0-beta` of chapter 02 §2.15A (read read-only; no untabled state was
> read); the row *baseline anchor (R1)*; this repository's own tree (dev as
> of 2026-11-15); and the public Qtum ElectrumX document that defines the
> server's QRC20 methods (named in R38.6.4Q.8).

*Scope.* R38.6.4b–R38.6.4k bind the Electrum client of one coin. This
subsection states how the reference uses that client for the QTUM family:
the QTUM coin, its QRC20 tokens and QTUM delegation. It covers how many
clients and sessions there are, what follows when a server answers a QTUM
contract method with an error, whether contract events are subscribed, how
spends of the family's shared QTUM outputs are ordered and remembered, what
disabling a token stops, and which wire methods a server has to answer. It
describes the reference. Where the reference behavior is a defect or a waste
that Reloaded should not copy, a D-item gives options and a recommendation;
nothing here chooses for Reloaded. Row identity, order and persistence of
token history stay in R38.5A.8, the spent-output record of a standard UTXO
coin in §38.5B, and the legacy activation contract in
[Chapter 55](55-coin-activation-wire-contract.md) §55.4. Native (coin
daemon) mode is out of scope.

*The two rows.* On every point of this subsection the rows `v2.6.0-beta`
and `v3.0.0-beta` give the same answer; no difference between them was found
for any rule below. Where a rule also says *baseline*, the behavior is that
of the row *baseline anchor (R1)* as well, and of this tree unless a note
says otherwise.

R38.6.4Q.1 **One Electrum client per activated instance.** (Reference-bound
at both rows, which agree. That each instance has a client of its own is
also the baseline's behaviour.)
1. QTUM and each QRC20 token are separate coin instances. Each builds its
   own Electrum client from the `servers`, `min_connected` and
   `max_connected` of its own activation request (chapter 55 R55.4.3). A
   QRC20 token is activated by the legacy methods of chapter 55 §55.4 only
   (`electrum` for Electrum servers); neither row has a task activation or a
   platform-with-tokens activation for it.
2. A token does not use, look up or require the QTUM instance. It activates
   while QTUM is not active, keeps working after QTUM is disabled
   (R38.6.4Q.10 item 2), and may name other servers than QTUM does.
3. Each client is a complete pool of R38.6.4b–R38.6.4i, with its own
   maintained set, version negotiation, ping, maintenance passes and
   suspensions. A request of one instance never travels over a session of
   another.
4. *Count.* The node's sessions are the sum of the maintained sets of the
   active instances. With QTUM and two tokens active and the same single
   server in each request, the node keeps **three** sessions to that server.
   With the same *S* servers in each request and the default bounds it keeps
   up to three sessions to each server, 3 × *S* in all. With the same list
   and `max_connected` 1 in each request it keeps three, all to the first
   reachable server of the list.


R38.6.4Q.2 **What the instances share.** (Reference-bound at both rows; also
the baseline's behaviour.)
1. *Shared.* With one wallet key behind them, QTUM and its tokens have one
   QTUM address: the QTUM outputs of that address pay the miner fee and the
   gas of every token transaction and of delegation, as well as QTUM's own
   sends. One process-wide build lock (R38.5B.5), which is the lock of
   every UTXO-family coin in the process, not of the QTUM family alone. On
   a native build, the on-disk cache of verbose transactions: a token keys
   it by its platform coin's ticker, so QTUM and its tokens read and write
   one cache.
2. *Not shared.* The Electrum client and its sessions (R38.6.4Q.1). The
   recently-spent record of §38.5B: one per instance, empty at activation.
   The history collector, the stored history and the history state. The
   background work of an instance, which ends with that instance
   (R38.6.4Q.10).


R38.6.4Q.3 **An error answer ends the request.** (Reference-bound at both
rows; the general rule is R38.6.4f item 1.) The QTUM-specific requests
(R38.6.4Q.8) are ordinary requests of R38.6.4f: they are tried over the
instance's maintained sessions one at a time in priority order, and the
first session that returns an answer ends the request.
1. A JSON-RPC error answer is an answer. So is a result that cannot be read
   as the expected shape. No other server is asked for that request, the
   session stays open and maintained, and the caller gets the failure.
2. Only a transport failure or the per-request bound of R38.6.4e item 2
   moves a request on to the next session and then to the retry over all
   servers (R38.6.4f item 2). In that retry, too, the first answer to arrive
   ends the request, an error answer included.
3. Because routing follows priority, the next request starts at the same
   server. A server that answers a contract method with an error therefore
   fails that method for as long as it is the highest-ranked maintained
   session, however many servers of the list would answer it correctly.
4. While the instance has no maintained session yet -- usual for the first
   requests of an activation, which does not wait for the pool (R38.6.4k) --
   a request goes straight to the retry of item 2. There the server whose
   answer arrives first decides, whatever its rank.


R38.6.4Q.4 **What a failed contract request does to activation and
balance.** (Reference-bound at both rows.)
1. *Activation, decimals.* When the token's coins-file entry has no
   `decimals`, building the token reads the value from the contract with one
   `blockchain.contract.call`. If that request fails, the build fails and
   nothing is registered. Nothing is repeated.
2. *Activation, balance.* The balance read that the `electrum` response
   needs (chapter 55 R55.4.6) is, for a token, one `blockchain.contract.call`.
   If it fails or exceeds the 20 s bound, the token is deregistered again
   and the request fails with the legacy error body (chapter 55 R55.1.10).
   Nothing is repeated inside the activation. An activation therefore
   fails whenever the answer that ends one of these two requests is an
   error answer (R38.6.4Q.3 items 1 and 4), although other listed servers
   would answer correctly.
3. *Balance.* Every balance read of a token sends one fresh
   `blockchain.contract.call`; the token balance is not kept between reads.
   A failed read is returned to its caller as a failure (`my_balance`
   answers an error), and neither an earlier balance nor a zero is put in
   its place. The token stays active, and the next read asks again.
4. *History.* R38.6.4Q.6 item 4.


R38.6.4Q.5 **No contract-event subscription.** (Reference-bound at both
rows. Item 1 is also the baseline's behaviour.)
1. The reference never sends `blockchain.contract.event.subscribe`: not at
   activation, not when a stream is enabled, and not after a session is
   re-established. After a reconnect there is no contract-event subscription
   to place again.
2. A token's balance and history are refreshed by polling alone: on request,
   and by the collector of R38.6.4Q.6 when it runs. The balance stream is
   not available for a token: `stream::balance::enable` refuses a QRC20
   token as an unsupported coin (`CoinNotSupported`, HTTP 501).
3. The one subscription the reference places again when a session ends is
   the script-hash subscription of R38.6.4h. It serves QTUM itself, as it
   serves any Electrum-backed UTXO coin.


R38.6.4Q.6 **The token history collector's requests.** (Reference-bound at
both rows; also the baseline's behaviour.) The collector runs only for a
token activated with `tx_history` `true`. What a row is, the order and the
persistence are R38.5A.8; this rule binds what the collector asks the
servers, and when.
1. *Start of a pass.* One balance read (`blockchain.contract.call`). If the
   balance equals the one the last completed pass saw, and no stored row
   lacks its timestamp or its block height, the pass ends there and the next
   begins 30 s later. The first pass after activation always continues.
2. *History request.* One `blockchain.contract.event.get_history` for the
   wallet's address, the token contract and the ERC-20 `Transfer` topic.
   From the answer the collector takes the distinct pairs of `tx_hash` and
   `height`. A stored transaction that the answer no longer lists is
   dropped.
3. *Details.* For a listed transaction that is not stored: one
   `blochchain.transaction.get_receipt`, then the verbose
   `blockchain.transaction.get` of the transaction and of every distinct
   transaction whose outputs it spends (the miner fee needs them). For a
   stored transaction nothing is asked, except one verbose
   `blockchain.transaction.get` when a row lacks its timestamp; a missing
   block height is filled from the answer of item 2.
4. *Failures.* A failed balance read is repeated after 10 s, without limit.
   A failed history request is repeated after 10 s, without limit, with one
   exception: an error answer whose error object is exactly
   `{"code": 1, "message": "history too large"}` ends the collector for the
   rest of the instance's life and sets the token's history state to an
   error with code `-1`, which `my_tx_history` reports as `sync_status`. A
   failure while reading the details of one transaction leaves that
   transaction out, and the pass goes on.
5. *After a pass that asked for the history* the next pass begins at once,
   and item 1 decides whether it continues.


> **Code-quality finding (informative; trigger 1 -- apparent when items 1
> and 4 are read together; it holds for the reference at both rows and for
> this repository's own collector).** A transaction left out under item 4 is
> not stored, and item 1 looks only at the balance and at the stored rows.
> The collector therefore does not ask for the history again until the
> balance changes or the token is activated anew, and a transfer whose
> details one server failed to deliver stays out of `my_tx_history` for that
> long. Proposed fix: a pass that left a listed transaction out counts as
> incomplete, and the next pass asks for the history again after the retry
> delay, whatever the balance.

*Status note (2026-11-17, clean side; decision within the lane). The
code-quality finding above is fixed as proposed:* a pass that left a listed
transaction out, because a detail request failed, counts as incomplete. The
next pass asks for the history again after the retry delay, whatever the
balance. The delay doubles while passes stay incomplete, up to the cap of the
failed-request delay, and is reset by a complete pass.

*Amendment (2026-11-17, clean side; Reloaded-original; maintainer decision).
The history collector's end state when no server serves the history.* When
three passes in a row end because every server tried answered the history
request of R38.6.4Q.6 item 2 with an error, the collector stops:

- the token's history state becomes an error with code `-2` that names the
  cause: no listed server answers the contract-event history; `my_tx_history`
  reports it as `sync_status`;
- the collector starts again when the token is activated again;
- `history too large` keeps its own end state, code `-1` (R38.6.4Q.6 item 4).

*Counting (2026-11-18, clean side; decision within the lane).* A pass counts
only when every server tried answered with an error answer. A pass that gets a
history answer, an unreadable answer or `history too large` resets the count.
A pass in which a server failed by transport neither counts nor resets it, so
a node that is offline never reaches this end state.

This is a recorded divergence from R38.6.4Q.6 item 4, under which a failed
history request is repeated without limit.

Basis: on 2026-11-17 the four `cipig.net` QTUM servers answered every
contract-event history request with an internal error, so a token listing only
them never got a history.

R38.6.4Q.7 **Ordering and remembering spends of the shared QTUM outputs.**
(Reference-bound at both rows; also the baseline's behaviour. §38.5B binds
the standard UTXO path; this rule states how the QTUM family differs from
it.) Each of the following spends QTUM outputs of the one shared address.
1. *A QTUM send that the swap engine orders* follows R38.5B.1 and R38.5B.5.
   It holds the lock of QTUM's own record from its listing through the
   broadcast and the entry, enters QTUM's record, and does not take the
   process-wide build lock.
2. *A QTUM withdraw* follows R38.5B.2 and R38.5B.5: the process-wide build
   lock from the listing through signing, no broadcast, no entry.
3. *A token's own broadcast* -- a swap payment, spend or refund, or a fee
   transfer -- holds the process-wide build lock from before its listing
   until its broadcast has been answered. Its listing passes through the
   token's own record, whose lock is released as soon as the listing
   returns. After the broadcast nothing is entered in any record: not in the
   token's, and not in QTUM's.
4. *A token withdraw* holds the process-wide build lock from before its
   token balance read through signing. It does not broadcast and enters
   nothing.
5. *Delegation.* Adding a delegation asks for the process-wide build lock
   but does not wait for it, so the lock is never held. Removing a
   delegation does not ask for it. Both build from a listing filtered by
   QTUM's record, return the signed transaction without broadcasting it, and
   enter nothing.
6. *Consequences.*
   - A token's record never receives an entry; it stays empty for the
     instance's life.
   - No record knows what a token's broadcast spent, and a token's listing
     does not know what QTUM's sends spent. Whether a second spend avoids
     the inputs of a first one therefore rests on the backend's own listing
     alone in every pairing except a QTUM swap send after a QTUM swap send:
     QTUM after a token, a token after QTUM, a token after a token (the same
     one or another), and anything after the broadcast of a withdraw or a
     delegation.
   - The process-wide lock puts token broadcasts, token withdraws and
     withdraw builds one after another. It does not supply the missing
     record.
   - A token's broadcast and a QTUM swap send hold different locks and can
     list at the same time.
   - While a token waits for the answer to its broadcast, no UTXO-family
     coin in the process can build a withdraw.


> **Code-quality finding (informative; trigger 2 -- a comment states the
> problem; the comment is in this repository's own source, carried from the
> baseline, and the reference keeps the behaviour at both rows).** At the
> token's broadcast the source says that this path ought to be reworked to
> use the recently-spent record, and that the tokens and their platform
> coin ought to work from one such record. Item 6 is the effect, and the
> options are in D38.6.4Q.4.

> **Code-quality finding (informative; trigger 1 -- apparent; it holds for
> the reference at both rows, for the baseline and for this repository on
> dev).** Item 5: the request for the lock when a delegation is added has no
> effect, because the build does not wait until the lock is granted. A
> delegation can thus be built while a withdraw or a token broadcast is
> building from the same address, and can select the same inputs. Proposed
> fix: wait for the lock and hold it from the listing through signing, when
> a delegation is added and when one is removed. No peer observes the
> difference.

*Status note (2026-11-15, clean side; this repository).* The finding above
describes dev before PR #354. Since that PR both delegation builds, adding and
removing, wait for the process-wide build lock and hold it from the listing
through signing (the status note after R38.5B.5). Item 5 still describes the
reference.

R38.6.4Q.8 **Wire methods of the QTUM family.** (The method names, the
parameter order and the answer members are the server's dictated interface:
for the event, receipt and token-information methods, the Qtum ElectrumX
server's public document `docs/qrc20-integration.md` of
`qtumproject/qtum-electrumx-server`, fetched 2026-11-15; for
`blockchain.contract.call`, the baseline's client. Which of them the
reference sends, and when, is reference-bound at both rows and is also the
baseline's behaviour.) Addresses, call data, topics and transaction hashes
travel as hex strings without a `0x` prefix.
1. `blockchain.contract.call`. Parameters in order: contract address, call
   data, sender. The reference always sends an empty string as the sender.
   Of the answer it reads `executionResult.output` only, and decodes it as
   the return value of the called function. It is sent for `decimals()`
   (R38.6.4Q.4 item 1), for `balanceOf(address)` (every token balance read),
   and in swaps for `allowance(address,address)` on the token and for the
   payment-state query of the swap contract.
2. `blockchain.contract.event.get_history`. Parameters in order: the 20-byte
   hash of an address, a contract address, a topic. The answer is a list of
   objects with `tx_hash`, `height` and `log_index`. It is sent by the token
   history collector (R38.6.4Q.6 item 2), by a token's swap searches with
   the `Transfer` topic, and by the QTUM delegation status check, which
   sends it twice for the wallet's address and the delegation contract: once
   with the topic of an added delegation and once with that of a removed
   one.
3. `blochchain.transaction.get_receipt`. The first segment is spelled
   `blochchain`; that is the server's own spelling and the only name the
   reference sends. One parameter, the transaction hash. The answer is a
   list of receipts in the form of Qtum Core's `gettransactionreceipt`. In
   every receipt the reference needs `blockHash`, `blockNumber`,
   `transactionHash`, `transactionIndex`, `outputIndex`, `from`,
   `cumulativeGasUsed`, `gasUsed` and `log` (each entry with `address`,
   `topics` and `data`); `to`, `contractAddress`, `excepted` and
   `exceptedMessage` may be absent. A receipt without a needed member makes
   the whole answer unreadable (R38.6.4Q.3 item 1). It is sent by the
   collector (R38.6.4Q.6 item 3), by a token's swap validation and swap
   searches, and by the delegation status check when more delegations were
   added than removed.
4. *Never sent, at either row:* `blockchain.contract.event.subscribe`
   (R38.6.4Q.5) and `blockchain.token.get_info`.
5. *Standard methods over a token's own client.* `server.version` and
   `server.ping` (R38.6.4e); the verbose `blockchain.transaction.get`
   (R38.6.4Q.6 item 3, and for the maturity of the outputs a spend lists);
   `blockchain.scripthash.get_balance` and
   `blockchain.scripthash.listunspent` for the QTUM of the shared address;
   `blockchain.transaction.broadcast` for the token's own broadcasts; and
   the block-count query of R38.6.4 (`blockchain.headers.subscribe`), which
   `my_tx_history`, confirmation waits and the maturity check use. A
   token's servers therefore have to answer the standard methods as well as
   the three QTUM ones.


R38.6.4Q.9 **Request order at a token's activation.** (Reference-bound at
both rows.) For a token activated through `electrum`:
1. the client is created, and its sessions are established in the
   background as R38.6.4c and R38.6.4k bind, each with its own
   `server.version`; steps 2 and 4 do not wait for them (R38.6.4Q.3
   item 4);
2. if the coins-file entry has no `decimals`, one `blockchain.contract.call`
   for `decimals()`;
3. the token is registered; with `tx_history` `true` the collector starts
   and runs beside step 4, and its first request is a balance read of its
   own;
4. one `blockchain.contract.call` for `balanceOf(address)`, bounded to 20 s,
   for the response.

The activation asks for nothing else: no block height, no history, no token
information, no subscription.

R38.6.4Q.10 **Disabling and re-enabling a token.** (Reference-bound at both
rows. The baseline and this tree differ in item 1; see the note.)
1. Disabling a token removes it from the registry and ends its background
   work at once. The history collector stops wherever it is -- in a wait, in
   a request or in the middle of a pass -- and the token's Electrum client
   stops with it, so its sessions close. Nothing waits for the pass to
   finish, and what the pass had not yet saved is dropped. The collector
   also checks at the start of each pass whether its ticker is still
   registered and ends if it is not; at the two rows that check is a second
   line only.
2. Disabling QTUM does not disable its QRC20 tokens and is not held back by
   them. A token activated through the legacy methods is not registered as
   depending on QTUM, and it has its own client (R38.6.4Q.1).
3. Enabling the token again builds a new instance: a new Electrum client
   with new sessions, an empty record, and, with `tx_history` `true`, a new
   collector. That collector loads the stored history and makes a first
   pass that continues (R38.6.4Q.6 item 1): one history request, and detail
   requests only for transactions that are not stored or lack a timestamp.
   However short the time between the two requests, exactly one collector
   and one client serve the token afterwards.
4. The `disable_coin` request itself -- when it is refused, its response
   members, and its rule for a platform coin with dependent tokens -- is not
   bound here.


> **Note (this repository on dev, 2026-11-15; its own code, not the
> reference).** In this tree, as at the baseline, disabling a token removes
> its ticker from the registry and stops nothing. The collector is not tied
> to the token instance's lifetime, so it runs on until its next registry
> check, which comes after its current wait: 30 s when idle, longer while it
> backs off (§38.6.5 item 8). Until then it keeps its Electrum client and
> that client's sessions. When the token is enabled again before that
> check, the check finds the ticker registered and the old collector goes on
> beside the new one: two collectors and two clients for one token, both
> writing the same stored history. The options are in D38.6.4Q.5.

> **Notes on existing text of this chapter (informative).**
> 1. *R38.6.4h, last sentence, and §38.6.5 decision 2.* The contract-event
>    subscription they speak of is this project's own. The reference has
>    none (R38.6.4Q.5), so "as before" in R38.6.4h refers to this tree and
>    not to a reference behavior, and placing the subscription again after a
>    disconnect would restore nothing the reference has.
> 2. *R38.6.4a item 4.* The item relies on the recently-spent record to
>    shield a read that follows this node's own broadcast when another
>    session answers it. For a token's broadcast nothing is recorded, and a
>    token's listing does not consult QTUM's record (R38.6.4Q.7 item 6). The
>    shield therefore does not exist for the QTUM family except between two
>    QTUM swap sends. In the reference the listing normally goes to the
>    session that took the broadcast; with `electrum_spread_reads` it may go
>    to another one, which makes the gap wider in this tree than in the
>    reference.
> 3. *§38.5B scope, R38.5B.1 and R38.5B.5.* They describe the standard path.
>    A QRC20 token has a record like every instance, but its swap-ordered
>    sends neither hold that record's lock through the broadcast nor enter
>    anything (R38.6.4Q.7 item 3). R38.5B.1's "only by" stays true; a reader
>    should not infer that every swap-ordered send is recorded.
> 4. *As-built note of 2026-11-09 (items 12 and 13 of R38.6.4o).* Its
>    sentence that QRC20 and Qtum "do the same" as an SLP token can be read
>    as saying that a token answers from QTUM's client. In this tree, as in
>    the reference, a QRC20 token has a client of its own and answers from
>    that one.
> 5. *R38.1.1.* The process-wide build lock couples the QTUM family to
>    every other UTXO-family coin (R38.6.4Q.7 item 6, last point), which is
>    the kind of interference R38.1.1 means to exclude.
> 6. *§38.6.5 item 8.* The backoff of the QRC20 history loop it mentions is
>    this project's own. The reference repeats after a fixed 10 s
>    (R38.6.4Q.6 item 4).
> 7. *Chapter 10 R25 (another chapter, noted here because it bears on
>    R38.6.4Q.5).* This project's balance stream accepts every activated
>    coin, a QRC20 token included. The reference refuses a token
>    (R38.6.4Q.5 item 2). Chapter 10 does not record that difference.
>

D38.6.4Q.1 *(Open, 2026-11-15.)* **Sessions of the QTUM family.** The
reference keeps one client per instance (R38.6.4Q.1), so each server holds one
session per active instance, each with its own negotiation, ping and
maintenance. That is a waste on the node and on public servers. This tree does
the same; under R38.6.4b-R each of its instances also orders the server list
on its own. Options: (a) parity, one client per instance; (b) instances of one
platform whose activation requests name the same servers use one client, which
lives as long as any of them is active; (c) a token uses the QTUM instance's
client while QTUM is active and builds its own otherwise. Whatever is chosen,
what R38.6.4Q.1 and R38.6.4Q.10 make observable has to hold: a token activates
without QTUM; a token keeps working after QTUM is disabled; disabling one
instance or failing one activation closes no session another instance uses;
and the servers a token's request names are either honored or the difference
is documented. R38.6.4b-R item 2 already binds the server order of a reused
client. *Recommendation:* (b). No peer and no server contract observes the
number of sessions, so this falls under the second arm of the default-side
rule of `docs/COMPAT_SWITCHES.md`.

*Status note (2026-11-15 and 2026-11-18, clean side; maintainer decisions).
D38.6.4Q.1 is decided:* option (b), on 2026-11-15. Its details were decided
on 2026-11-18.

1. *When instances share.* Instances of the QTUM family (`protocol.type`
   `QTUM` or `QRC20`) use one Electrum client when every input that shapes
   the client is exactly equal: the platform (a token's
   `protocol.protocol_data.platform`, QTUM's own ticker), the server list as
   the activation request gives it (in order, and per entry), `min_connected`
   and `max_connected`, the coin entry's `listed_order_for_spends` and
   `block_interval`, and the builder arguments (ping, version negotiation
   and its client name, metrics). An instance whose inputs differ in any of
   them builds its own client, so the servers a token's request names are
   always honored. Every other coin builds its own client as before.
2. *Lifetime.* The node keeps the shared clients in a registry of its context,
   by weak reference, so the registry owns none of them. A client lives as
   long as any instance holds it, and the last holder's release closes it. A
   token activates without QTUM and keeps working after QTUM is disabled.
   Disabling one instance, or failing one activation, closes no session that
   another instance uses. R38.6.4b-R item 2 binds the server order of a
   reused client.
3. *Reuse.* A reused client is not probed again. While its servers disagree,
   an instance that would reuse it is refused as a fresh build would be
   (R38.6.4o item 12), and holds no part of it. A build that failed is not
   remembered: an instance that waited for it builds from the start.
4. *Lock order.* The activation locks of R55.1.12 (chapter 55), then one
   build lock per set of inputs, then the registry itself, and the coin
   registry only after both registry locks are released. `disable_coin`
   never takes a registry lock, and never waits for an activation lock while
   it holds the coin registry. A build sends no transaction.
5. *Naming.* A shared client goes by the platform ticker in its log lines,
   its error texts and its transport metrics, so each connection is counted
   once, and a token's transport errors name the platform. The
   `electrum_servers` request for a token reports the shared client.
6. *Subscriptions of a disabled instance.* On disable, an instance's
   remembered subscriptions, script-hash and contract-event alike, are
   removed from the shared client, unless another live instance remembers
   the same one. They are then no longer placed again after a reconnect.

*Status note (2026-11-22, clean side; maintainer decision of 2026-11-21).*
A client of the QTUM family's registry (item 2), shared or built for one
instance whose inputs differ, writes one INFO line when it is created,
naming the platform ticker and the instance that built it; one each time an
instance reuses it, naming that instance; and one when the last holder lets
go of it and it closes. Each line lists the servers in the configured order
as `host:port` only, as the server names of R38.6.9a; an entry without a
usable host and port is shown as a placeholder. A failed build and a refused
reuse (item 3) write nothing, and an instance that lets go while others still
hold the client writes nothing. No line is written per request or on a
timer. Every other coin writes none of these lines.

*Status note (2026-11-22, clean side; defect fix).* In this tree a shared
client keeps the subscriptions of item 6 and of the D38.6.4Q.3 status note
per instance as follows.

1. *Removal.* An instance is remembered for a subscription before its
   request is sent, so a disable while the request waits for its answer
   removes it, and the answer adds nothing back. A removed instance stays
   retired on the client: a later request of it, from a handle that outlived
   the removal, sends nothing and is remembered nowhere. This costs one entry
   per removed instance for the life of the client.
2. *Placed over the current session.* A request of an instance that
   remembers the subscription sends nothing only while the subscription is
   placed over the current session. When placing it again over a newer
   session failed, the next request of that instance sends it. The placing
   again itself is not retried (D38.6.4Q.3 status note, item 2). It leaves
   a subscription that has a request in flight over the current session to
   that request; when that request fails and the subscription is still
   remembered, the placing again is asked for once more.
3. *One request at a time.* An instance has at most one request in flight
   per subscription and session; another request of the same instance
   meanwhile sends nothing and is answered as placed, even when the request
   in flight then fails. A request without an answer holds this only until
   it is answered, its call ends at the request time limits of the client,
   or its caller drops it; it then counts as failed.
4. *First placement.* A subscription whose first placement for an instance
   failed or was dropped is not remembered for that instance.

*Status note (2026-11-23, Reloaded-original).* Two rules of the note above
are refined; both are defect fixes of this tree.

1. *Placing again.* The placing again after a change of session leaves
   alone a subscription that is already placed over the current session. It
   places again only what the change of session lost, and it never sends a
   second subscription over the same session. For a contract-event
   subscription this keeps the bound of the D38.6.4Q.3 status note, item 2:
   at most once per session. Before, a placing again that came after an
   instance's own request over the same session sent the subscription a
   second time.
2. *First placement that failed.* Item 4 above holds unless the subscription
   is placed over the current session when the request fails or is dropped.
   That happens when the session changes while the request waits and the
   placing again places the subscription over the new session. The server
   then holds the subscription, so the instance stays remembered for it: a
   later request of that instance sends nothing, and the subscription is
   placed again after later changes of session. Before, the instance was
   forgotten, and the server held a subscription that the client no longer
   remembered. The rule of item 2 above, that the placing again is asked for
   once more after a request it left alone has failed, is unchanged.

D38.6.4Q.2 *(Open, 2026-11-15.)* **An error answer to a QTUM contract read.**
In the reference it is final (R38.6.4Q.3), and one server's error fails an
activation, deregisters the token or fails a balance read although other
listed servers answer correctly (R38.6.4Q.4). That is a defect for every list
with more than one server. Options: (a) parity; (b) for the three read methods
of R38.6.4Q.8 items 1–3 only, an error answer or an unreadable answer sends
the request on to the next server, and the failure is reported as R38.6.4i
binds only when every server tried has failed, within the budget of R38.6.4j;
(c) as (b), but only for an error answer that says the server does not know
the method. Under (b) and (c) the `history too large` answer of R38.6.4Q.6
item 4 stays final, and a refused broadcast stays as bound in R38.5B.6. The
observable requirement: a token activates, reads its balance and collects its
history as long as one listed server answers the contract methods correctly.
*Recommendation:* (b). It departs, for three methods, from item 1 of R38.6.4f,
and no peer observes it; because that item is reference-bound text, the
departure needs a maintainer decision.

*Status note (2026-11-17, clean side; maintainer decision). D38.6.4Q.2 is
decided:* option (b), with one narrowing, together with the spreading of
R38.6.4a. It is a recorded departure from R38.6.4f item 1 for three methods.

1. *Failover.* A JSON-RPC error answer to `blockchain.contract.call`,
   `blockchain.contract.event.get_history` or
   `blochchain.transaction.get_receipt` moves the request to the next session,
   then to the retry of R38.6.4f item 2, exactly as a transport failure does.
   Unlike a transport failure, it does not close the session, and a server
   that answered the request with an error is not asked again within the same
   request. A connection the retry opened is released as after any answer. If
   every server tried answers with an error, the request fails with the
   failures of R38.6.4i, within the budget of R38.6.4j.
2. *What stays final.* The exact answer
   `{"code": 1, "message": "history too large"}` (R38.6.4Q.6 item 4), and an
   answer that cannot be read as the expected shape. The second is the
   narrowing of option (b): it can only be judged by the caller, after the
   pool has returned.
3. *Spreading.* The three methods join the read-only queries of R38.6.4a
   item 1. They take turns among the near sessions and try up-to-date sessions
   first (R38.6.4o item 5). This amends R38.6.4a item 1 for Reloaded.
4. *Later.* A per-server mark "does not serve the contract methods" is decided
   only after measurements show that its repeated cost matters.

D38.6.4Q.3 *(Open, 2026-11-15.)* **The contract-event subscription after a
reconnect.** Parity asks for nothing here, because the reference has no such
subscription (R38.6.4Q.5). This tree places it when a client enables the
balance stream for a token (§38.6.5 decision 2; chapter 10 R25, amendment) and
does not place it again after a disconnect (R38.6.4h). Options: (a) keep that,
the token then falls back to polling (§38.6.5 decision 4); (b) place it again
on a maintained session after a disconnect, with the outcome R38.6.4h binds
for a script-hash subscription; (c) remove the subscription and poll, as the
reference does. Under every option an error answer, or no answer, to the
subscription leaves the token active and polling, and a server that does not
know the method fails neither activation nor balance nor history.
*Recommendation:* (b).

*Status note (2026-11-17, clean side; maintainer decision). D38.6.4Q.3 is
decided:* option (b). The contract-event subscription that the balance stream
places for a token (§38.6.5 decision 2) is kept, and polling stays.

1. *Placed again.* After a disconnect, the subscription is placed again on a
   maintained session, as R38.6.4h binds for a script-hash subscription, with
   one difference: a placement that fails is not retried (item 2). This amends
   the last sentence of R38.6.4h for Reloaded.
2. *Answers.* A server that refuses the subscription or answers it with an
   error breaks neither activation, nor balance, nor history, and the
   subscription is not asked again in a loop: at most once per newly
   maintained session.
3. *Measured basis (2026-11-17, this project's own probes).* The public Qtum
   servers `s1`, `s3`, `s4` and `s8.electrum.qtum.site` accept the
   subscription. Servers without the contract-event index answer it with an
   internal error.

D38.6.4Q.4 *(Open, 2026-11-15.)* **One record and one ordering for the shared
QTUM outputs.** The reference keeps a record per instance, records nothing for
a token's broadcast, and does not hold the lock for a delegation (R38.6.4Q.7);
the source itself names the missing shared record. This tree does the same,
and read spreading widens the gap (note 2 above). Options: (a) parity; (b) one
recently-spent record per platform address, used by QTUM and all its tokens,
into which every broadcast that this node made and the backend accepted is
entered when it spends QTUM outputs of that address -- QTUM sends, token
broadcasts, and the withdraws and delegations promoted as in R38.5B.9 --
together with one lock per platform address held from the listing through the
broadcast and the entry, in place of the process-wide lock for this family;
(c) the smallest step: a token's broadcast enters the token's own record,
which covers two sends of the same token and nothing else. The observable
requirement: once the backend has accepted a transaction this node built, no
later transaction this node builds from the same QTUM address selects one of
its inputs, and its change is offered, whichever of QTUM, a token or
delegation built either one and whichever session answers the listing.
*Recommendation:* (b). No peer observes it.

*Status note (2026-11-15 and 2026-11-19, clean side; maintainer decisions).
D38.6.4Q.4 is decided:* option (b), on 2026-11-15. Its key was decided on
2026-11-19.

1. *Key.* One recently-spent record per platform and address script: the
   platform (a token's `protocol.protocol_data.platform`, QTUM's own ticker)
   and the script of the address whose outputs are spent. QTUM and its
   tokens use one record wherever they spend from the same address. Under an
   HD login, QTUM's enabled address and the tokens' address are different
   scripts, so each has its own record. Every other coin keeps its record
   per instance, as before.
2. *Lifetime.* The node keeps the shared records in a registry of its
   context, by weak reference. An instance takes the record of its activated
   address when it is built. A record lives as long as any instance holds
   it; when the last one is disabled, the record ends, as an instance's own
   record does today. A spend from another address of an HD wallet keeps the
   behavior of an instance's own record: it is entered into the record of
   the instance's activated address, and no lasting record is kept for that
   other address.
3. *What is entered.* Every transaction of the QTUM family that this node
   built and the backend accepted, when it spends outputs of the address:
   QTUM's swap sends and merges, every token broadcast (payments, fees,
   spends and refunds, which pay their gas from the address), and the
   withdraws and delegations promoted as in R38.5B.9. A refused broadcast
   enters nothing.
4. *Ordering.* The record's own lock is the lock of the address. Every build
   of the QTUM family that spends outputs of the address takes it when the
   unspent listing has been answered, filters the listing through the record
   under it, and holds it through the broadcast and the entry, or, for a
   build that is not broadcast at once (a withdraw or a delegation), through
   signing and the pending entry of R38.5B.9. The listing request itself is
   sent before the lock is taken, so two builds can ask at the same time;
   their selections are made one after the other. It replaces the
   process-wide lock for this family; the other UTXO coins keep the
   process-wide lock. A withdraw holds the lock through signing; with a
   hardware wallet that includes the wait for the device, during which the
   listings of that address wait too. A pending entry
   does not hide inputs: two builds made before either is broadcast can
   select the same input, and the backend refuses the second broadcast.
5. *Lock order.* The record's lock is a lock of the send path. It is never
   taken while the activation locks of R55.1.12 (chapter 55), the client
   build lock or a registry of the status note after D38.6.4Q.1 are held,
   and `disable_coin` does not take it.

D38.6.4Q.5 *(Open, 2026-11-15.)* **Ending a token's background work when it
is disabled.** The reference ends it at once (R38.6.4Q.10 item 1); this tree
does not, and a quick disable and enable leaves two collectors (the note
under R38.6.4Q.10). Options: (a) the reference behavior: disabling ends the
token's collector and client at once; (b) keep the registry check, but let it
tell the instance apart and not only the ticker (the Reloaded status note
after chapter 55 R55.4.9 records that the registry has no instance identity
today); (c) both. The observable requirement: once `disable_coin` has
answered, the token sends no further request to any server, and after a
disable and an enable exactly one collector and one client serve it.
*Recommendation:* (a), with (b) wherever work cannot be ended at once.

*Status note (2026-11-16, clean side; decision within the maintainer's lane
plan). D38.6.4Q.5 is decided:* option (c), that is (a) and (b).

1. Disabling a token ends its history collector and its client at once.
2. A collector's registry check also compares the coin instance, not only the
   ticker, so the collector of a disabled instance ends even if the token was
   enabled again.
3. After a disable and an enable, exactly one collector and one client serve
   the token.

*Acceptance rows for a mock Electrum server.* Each row names what the mock
has to answer and what the node does. *Parity* is the reference behavior
bound above; where a D-item would change the expected result, the row says
so. For this tree the mock also answers the requests that three rules add
to a client: R38.6.4k-R, R38.6.4l and R38.6.4o.

T38.6.4Q.1 *Sessions per family.* With one mock server, a test shall activate
QTUM and two tokens, each naming that server. Parity: the mock holds three
sessions, each with its own `server.version` and its own `server.ping` every
30 s, and disabling one token closes exactly one of them. Under D38.6.4Q.1 (b)
or (c) the mock holds one session, and disabling a token closes none while
another instance is active (R38.6.4Q.1).

T38.6.4Q.2 *Methods at activation.* A mock that answers `server.version`,
`server.ping` and `blockchain.contract.call` shall be enough to activate a
token whose coins-file entry has `decimals`, with `tx_history` `false`. The
test shall assert that exactly one `blockchain.contract.call` arrives, with
the token contract, the `balanceOf` call for the wallet's address and an empty
sender, and that the response carries the balance decoded from
`executionResult.output`. With `decimals` removed from the entry, a
`decimals()` call shall arrive before it. In both runs the mock shall receive
neither `blockchain.token.get_info` nor `blockchain.contract.event.subscribe`
(R38.6.4Q.8, R38.6.4Q.9).

T38.6.4Q.3 *An error answer at activation.* (a) With one mock server that
answers `blockchain.contract.call` with a JSON-RPC error, a test shall call
`electrum` for a token. The request fails with HTTP 500 and the legacy error
body; the mock's session is still open; the token is not active; and a
second `electrum` call is accepted and fails the same way, not as already
active. Without `decimals` in the entry the request fails at the build and
nothing is registered. (b) With two mock servers, the first answering the
call with an error at once and the second answering correctly after a delay,
parity is the same failure, because the first answer to arrive ends the
request. Under D38.6.4Q.2 (b) the activation of (b) succeeds from the second
mock (see R38.6.4Q.3 and its companion R38.6.4Q.4).

T38.6.4Q.4 *An error answer to a balance read.* With a token active on two
mock servers and both sessions maintained, the higher-ranked mock shall start
answering `blockchain.contract.call` with an error. The test shall assert that
`my_balance` fails, that the other mock received no `blockchain.contract.call`
for it, that the erring mock's session stays open, that the token is still
listed as enabled, and, once the mock answers correctly again, that the next
`my_balance` sends a new request and returns the balance. No earlier value
shall be served in between. Under D38.6.4Q.2 (b) the other mock is asked and
`my_balance` succeeds throughout (R38.6.4Q.3 item 3 and R38.6.4Q.4 item 3).

T38.6.4Q.5 *A collector pass.* With `tx_history` `true`, the mock shall
answer the balance read, a history list of two transactions of which one
carries two transfers, the receipts, and the verbose transactions of both and
of the transactions they spend from. The test shall assert that the receipt
request is spelled `blochchain.transaction.get_receipt`; that for each
transaction the receipt is asked before the verbose transaction; that each
spent-from transaction is asked once per transaction; that the stored rows
are those of R38.5A.8; and that the next pass, with the balance unchanged,
sends one `blockchain.contract.call` and nothing else, about 30 s later
(R38.6.4Q.6 items 1–3).

T38.6.4Q.6 *History failures.* (a) The mock answers
`blockchain.contract.event.get_history` with an error other than the one of
(b): the collector asks again after about 10 s (in this tree, after its own
backoff), the history state is not an error, and the history is stored once
the mock answers. (b) The mock answers with the error object
`{"code": 1, "message": "history too large"}`: no further history request
arrives for that instance, `my_tx_history` reports `sync_status` as an error
with code `-1`, and `my_balance` still answers. (c) The mock answers the
receipt request of one of two listed transactions with an error: the rows of
the other are stored and the failed one is absent. At parity it stays absent
while the balance is unchanged; with the fix proposed in the finding that
follows R38.6.4Q.6 it appears after a later pass (item 4 of that rule).

T38.6.4Q.7 *No subscription to restore.* The mock records every method name.
Parity: activating QTUM and a token, dropping every session and letting the
clients reconnect produces no `blockchain.contract.event.subscribe` at any
time. In this tree the method arrives only after a client has enabled the
balance stream for the token; a mock that answers it with an error, or not at
all, shall leave activation, balance and history working; and what arrives
after a reconnect follows the decision of D38.6.4Q.3 (R38.6.4Q.5).

T38.6.4Q.8 *Spends of the shared outputs.* The mock accepts every broadcast
and keeps listing the spent output in `blockchain.scripthash.listunspent`, as
a server that has not indexed the transaction does. The test shall run these
pairs from one address: a token broadcast then a QTUM swap send; a QTUM swap
send then a token broadcast; a broadcast of token A then one of token B; two
broadcasts of the same token. Parity: in each pair the second transaction
selects the input the first one spent; only two QTUM swap sends in a row avoid
it (T38.5B.1). Under option (b) of D38.6.4Q.4 the second transaction of every
pair avoids the input and may spend the change (R38.6.4Q.7).

T38.6.4Q.9 *Delegation.* (a) The delegation status check shall succeed against
a mock that answers `blockchain.contract.event.get_history` twice and, when
more delegations were added than removed,
`blochchain.transaction.get_receipt`; an error answer to any of the three
fails the request. (b) With a mock that delays its answer to
`blockchain.scripthash.listunspent`, a test shall start adding a delegation
and a withdraw from the same address together. Parity: both list at the same
time and can select the same input. With the fix proposed in the second
finding under R38.6.4Q.7 the second build lists only after the first has
signed (item 5 of that rule; R38.6.4Q.8).

T38.6.4Q.10 *Disable stops the token's requests.* With a token active,
`tx_history` `true` and its collector in the 30 s wait, a test shall call
`disable_coin`. Reference behavior: the token's sessions close and the mock
receives no further request from them. The test shall then call `electrum` for
the token inside what would have been that wait and assert that afterwards the
mock holds the sessions of one client for the token and receives one balance
read per idle interval, not two. A second test shall disable QTUM while a
token is active and assert that the token's `my_balance` still answers
(R38.6.4Q.10).

T38.6.4Q.11 *The process-wide lock.* With a mock that delays its answer to a
token's `blockchain.transaction.broadcast`, a test shall request a withdraw of
another UTXO coin, served by a second mock, while the token's broadcast is
pending. Parity: the withdraw does not return before the token's broadcast has
been answered. Under D38.6.4Q.4 (b) it returns without waiting (R38.6.4Q.7
item 6).

> **Measured (2026-11-16, clean side; this repository on dev; plan
> `qtum-qrc20-stabilization`).**
> Offline tests against the stub Electrum server
> (`qrc20/qrc20_electrum_stub_tests.rs`, `t38_6_4q_*`) pin this tree's
> behavior for rows T38.6.4Q.2 to T38.6.4Q.6. They run with
> `electrum_server_order` `listed` and `electrum_spread_reads` `false`, so that
> "the highest-ranked session" is deterministic. The activation is driven
> through the legacy coin initialization plus the balance read and
> deregistration of the `electrum` handler, not through the RPC itself.
>
> 1. *T38.6.4Q.2.* The activation sends `blockchain.headers.subscribe` and
>    then exactly one `blockchain.contract.call`, with the token contract,
>    the `balanceOf` call data and an empty sender. The balance is decoded
>    from `executionResult.output`. Without `decimals`, a `decimals()` call
>    comes first. Neither `blockchain.token.get_info` nor
>    `blockchain.contract.event.subscribe` is sent. *Difference from
>    R38.6.4Q.9:* this tree asks for the tip (`blockchain.headers.subscribe`)
>    before the balance call, and with two servers it also asks
>    `blockchain.block.header` of both (by R38.6.4o; not asserted here).
> 2. *T38.6.4Q.3.* As the reference.
>    - An error answer to the balance read fails the activation, without a
>      repeat. The token is deregistered and the session stays open. A second
>      attempt fails the same way, not as already active.
>    - Without `decimals`, the build fails on the `decimals()` call and
>      nothing is registered.
>    - With two servers, the first server's error answer ends the request,
>      although the second would answer correctly.
> 3. *T38.6.4Q.4.* As the reference. An error answer of the highest-ranked
>    session fails `my_balance`. The other server is not asked, the session
>    stays open and the token stays active. Each read asks again, no earlier
>    value is served, and after recovery the next read succeeds.
> 4. *T38.6.4Q.5.* The request order is: the balance read, then
>    `blockchain.contract.event.get_history`, then per transaction the
>    receipt (`blochchain.transaction.get_receipt`) and the verbose
>    transaction, and each spent-from transaction once. The next pass with
>    an unchanged balance sends one `blockchain.contract.call` and nothing
>    else.
> 5. *T38.6.4Q.6.*
>    - (a) A failed history request is asked again after this tree's
>      backoff: 10, 20, 40, 80, 160 and then 300 s (§38.6.5 item 8), and the
>      history state is not an error.
>    - (b) Only the exact error object `{"code": 1, "message": "history too
>      large"}` ends the collector, with the state's code −1; the balance
>      still answers.
>    - (c) A failed receipt leaves that transaction out. The other is stored.
>      While the balance is unchanged, no pass asks again, as the finding
>      after R38.6.4Q.6 says.
>
> Not measured here: rows T38.6.4Q.1 and T38.6.4Q.7 to T38.6.4Q.11 (next
> package); the dropping of an unlisted transaction (R38.6.4Q.6 item 2),
> which this tree does not do (§38.5C, decided 2026-11-16); and the default
> server order and read spreading.

> **Measured, second part (2026-11-17, clean side; this repository on dev;
> plan `qtum-qrc20-stabilization`).** Offline tests against the stub
> Electrum server (`qrc20/qrc20_electrum_stub_tests.rs`, `t38_6_4q_*`), with
> the same settings as the first part, pin rows T38.6.4Q.1 and T38.6.4Q.7 to
> T38.6.4Q.11.
>
> 1. *T38.6.4Q.1, as the reference.* QTUM and two tokens naming one server
>    hold three sessions there, each with its own `server.version` and header
>    subscription. Disabling a token without history collection closes
>    exactly one session. Disabling QTUM closes only QTUM's session; a token
>    keeps its session and its balance.
> 2. *T38.6.4Q.7, as the reference for activation and reconnects.* No
>    `blockchain.contract.event.subscribe` is sent at activation or after the
>    sessions are dropped and placed again. The header subscription is placed
>    again once per client. The subscription that the balance stream places
>    (§38.6.5 decision 2) was exercised on the token's client only:
>    - an error answer, or no answer, leaves the balance and the history
>      working;
>    - after a reconnect it is not placed again.
>    Enabling the stream itself is not covered here.
> 3. *T38.6.4Q.8, as the reference.* From one address, with a server that
>    keeps listing a spent output:
>    - a token broadcast and a QTUM swap send, in either order, select the
>      same input;
>    - so do two tokens, and the same token twice;
>    - only two QTUM swap sends in a row avoid it.
>    A token broadcast and a QTUM swap send started together list at the same
>    time, and both spend the one output.
> 4. *T38.6.4Q.9.*
>    - (a) As the reference: the status check asks the added and the removed
>      delegations' history, and then the receipt; an error answer to any of
>      them fails it.
>    - (b) *Differs from the reference:* a delegation and a withdraw from one
>      address list one after the other, because both builds wait for the
>      process-wide lock (PR #354).
> 5. *T38.6.4Q.10, differs from the reference.*
>    - Disabling a token whose collector waits stops nothing at once. Its
>      session stays open without requests until the collector's next
>      registry check, up to 30 s later, which ends it and closes the session.
>    - Enabling the token again within that wait leaves two collectors and two
>      clients, each reading the balance once per interval.
>    This is the case of the note after R38.6.4Q.10, decided as D38.6.4Q.5.
> 6. *T38.6.4Q.11, as the reference.* A withdraw of another UTXO coin waits
>    until a token's pending broadcast is answered.

*Not established.* (1) The session count of R38.6.4Q.1 item 4 follows from
how the reference builds its clients; it was not measured against a running
reference binary and a live server in this pass. (2) Which error answers
real Qtum servers give to the three QTUM methods, under which conditions
and with which codes, is a property of those servers and was not examined;
the reference treats every error answer alike except the one of R38.6.4Q.6
item 4. (3) Whether real servers accept the empty sender of R38.6.4Q.8
item 1 in every version was not checked; the reference sends nothing else.
(4) The finding under R38.6.4Q.6 was read, not reproduced in a run. (5) A
token's swap paths were read only for the set of methods they send
(R38.6.4Q.8), not request by request. (6) The `disable_coin` contract
(R38.6.4Q.10 item 4) and native mode were not examined.

> **Pointer (2026-11-22, KDF Spec Reader, round 0.5.0-23; gate status as
> chapter 51 §51.6E states).** Point (5) above is taken further, for the
> legacy swap's spend search and secret extraction, in
> [chapter 51](51-legacy-v1-swap-state-machine.md) §51.6E.
> R89 records how a QRC20 spend or refund is found (candidates from the
> token's `Transfer` history and the presence of the swap event, the match
> by the call data), that the `excepted` value is read for the payment only,
> and that the event is not tied to the matched call's output. R90 records
> the UTXO and SLP search (classification by the input that spends the swap
> output) and the secret extraction. R88 records refund eligibility: the
> wall clock alone for QRC20 and SLP, the median time past for a standard
> UTXO coin, BCH and the QTUM base coin. The rules of this chapter are
> unchanged.

> **Pointer (2026-11-22).** [Chapter 39](39-zcash---z_coin-shielded-coin.md)
> §39.11.5A, R39.11.5A.7, records the reference's QRC20 already-sent query:
> the state call, the transfer-history and receipt reads, and its "not sent"
> answer when the contract state says the payment exists but no matching
> transaction is found. The pointer is a Spec Reader addition and passed the
> KDF Dirty Gate with that subsection on 2026-11-22.

> **Clean-side addition of 2026-11-23 (Reloaded-original; as built; no claim
> about the reference).** The swap reads of a QRC20 token over Electrum
> follow [chapter 51](51-legacy-v1-swap-state-machine.md) §51.6G R100. A
> token read through the operator's own daemon reads as before.
>
> 1. *Directed reads.* `blockchain.contract.call`,
>    `blochchain.transaction.get_receipt` and
>    `blockchain.contract.event.get_history` each have a form addressed to
>    one named server (R38.6.4g). It is answered by that server or fails
>    naming it, and it does not move on to another server. Every swap read
>    first reads the server's own tip (`blockchain.headers.subscribe`), then
>    the read itself on the same server. Two answers whose tips are further
>    apart than the tolerance of R38.6.4o item 1 do not decide.
> 2. *Decisions.* Two answers are compared on a decision, never on a height,
>    a confirmation count or a time:
>    - payment: the state of `payments(id)` and the payment call details
>      read from the server's receipt of the payment transaction. The
>      validation repeats the read until its deadline while it is
>      undetermined or the servers agree that the contract does not know the
>      payment yet;
>    - confirmation wait: the wait polls one server as before (R38.6.4o item
>      6); its end is then confirmed by a pair: "has the required
>      confirmations" and "the swap contract calls completed";
>    - already sent: "not sent" (the contract does not know the payment), or
>      the payment transaction;
>    - spend: the state of `payments(id)` decides. "Not spent" is the state
>      `sent` on both servers. Only a server whose state is `spent` or
>      `refunded` is asked for the event history, to name the transaction,
>      which is bound to its call and receipt as chapter 51 §51.6E R89
>      binds it. A claim so found carries a secret whose hash is the
>      payment's secret hash and is taken from one server;
>    - refund gate: each server's median time past at its own tip is past
>      the lock time or is not.
> 3. *Claims and refunds.* The claim of the side that reveals its own secret,
>    and a refund, check the payment (state and call details) on a pair
>    first. The claim made with a secret that a claim on the other chain
>    already revealed is read from one routed server and is not held back by
>    an undetermined read.
> 4. *Undetermined.* An undetermined read is an error whose text starts with
>    `swap evidence undetermined: ` and names servers by host and port only.
>    It is never answered as valid, not sent, not spent or an open gate.
> 5. *Capability.* A token whose server list has two or more hosts and no
>    `swap_evidence_node` is admitted for swaps only once two distinct hosts
>    of its client have answered `blockchain.contract.event.get_history`
>    with a result since the client was built. A token with
>    `swap_evidence_node` is admitted only once its trusted server has
>    answered that request with a result: under a trusted server the spend
>    read of a claimed payment still needs that server's event history to
>    name the claim and read the secret. An error answer does not count.
>    The client records it per server for every such request, routed or
>    directed, so the instances that share a client (D38.6.4Q.1) share the
>    record. Until then the token answers as a coin with too few nodes:
>    orders and swap starts are refused, and the reads of a running swap
>    wait. The refusal text lists the hosts that have and have not served
>    the request, or names the trusted server that has not. Only a token in
>    native mode needs no such proof.
> 6. *Probe.* After the activation of such a token, each server concerned
>    that has not served the request is asked once, in the background, for
>    the token's own transfer history: every server of a token over two or
>    more hosts, and the trusted server alone of a token with
>    `swap_evidence_node`. The activation does not wait for it and does not
>    fail on it. While the proof is missing, a refusal and an undetermined
>    read start the probe again for the servers still missing. One server
>    is probed at most once a minute and never while a probe of it runs. A
>    probe ends when the token that started it is disabled or dropped, and
>    none starts for a disabled token. One log line names the hosts that
>    have and have not served the request.
> 7. *Refund gate.* The refund gate of a token is the gate of the UTXO
>    Electrum client (chapter 51 §51.6G R100), given the token's policy of
>    item 5: the median time past is read through that client's directed
>    header reads, and the tips are compared within its tolerance.

### 38.6.5 UTXO balance event streaming
R38.6.5 For Electrum-backed UTXO coins the project shall emit balance-change
events over the streaming infrastructure, registering the addresses to be watched
at enable time (and re-registering as needed). Acceptance: a balance change on a
watched address produces a streamed balance event.

> **Decisions and principles (implemented 2026-08-09/11; recorded here 2026-11-02).**
> Reloaded's own design choices for this rule; no peer observes them.
>
> 1. **No subscription cap.** A wallet subscribes one script hash per activated
>    Electrum coin, or one per known address under an HD wallet. A stress test with
>    10,000 watchers registered and notified within milliseconds, so the ceiling is
>    orders of magnitude above any real wallet.
> 2. **A QRC20 balance wakes on its own contract-event subscription.** A token balance
>    is contract storage and need not change the holder's UTXOs, so each token wakes on
>    its per-token `blockchain.contract.event.subscribe` notification, not on the QTUM
>    script-hash notification. After a disconnect it is not placed again (R38.6.4h).
>    *Clarification (2026-11-15, clean side):* this subscription is Reloaded's own;
>    the reference places none (R38.6.4Q.5; see the clarification after R38.6.4h).
> 3. **The history loop wakes on the same signal.** The UTXO transaction-history loop
>    waits for its interval or for a script-hash notification, whichever comes first,
>    and still re-reads balance and history itself.
> 4. **Polling stays the correctness floor.** A notification only shortens the wait. A
>    missed, dropped or silently dead subscription degrades to polling, never to a
>    wallet that stops updating, and a failed re-subscription leaves the coin polling
>    without an error for the user.
> 5. **A notification only means "something changed".** No balance change is derived
>    from it; the existing balance and history reads stay authoritative.
> 6. **Notifications coalesce.** Several notifications that arrive before a refresh
>    completes cause one refresh, not a queue.
> 7. **The 10-second poll floor stays** (chapter 10 R25). Lowering it would be a
>    separate decision, because of the load it puts on public Electrum servers.
>
> **Addition (2026-11-09; Reloaded-original, clean side; PR #327).**
>
> 8. **A repeating failure is logged once and backs off.**
>    - **Logging:** when the history loop's balance read or history request fails
>      with the same error text again and again, the node logs it once, then at most
>      one summary line per 10 minutes with the count and the latest text, then one
>      line when the request succeeds again. Two texts that differ only in their numbers, such as a time or
>      a count of seconds, count as the same error. A different error text is logged
>      anew. The QRC20 history loop logs the same way.
>    - **Backoff:** while the UTXO history request keeps failing, the wait before the
>      next attempt grows from 10 s to 60 s, then doubles up to 300 s, and returns to
>      10 s after the first successful history request, or as soon as the loop goes
>      idle again because the balance is read and unchanged. After the server
>      recovers, the history can thus resume up to 300 s later. The QRC20 loop keeps
>      its own backoff.
>    - **Unaffected:** item 7's floor applies to the healthy loop and is unchanged.
>      The BCH and SLP history state machine keeps its own retry lines and cooldown.
>    - **Tests:** the throttle and the delays are unit-tested; the loop's use of them
>      is covered by construction.
>    - *Clarification (2026-11-15, clean side):* this backoff, and the QRC20 loop's
>      history backoff, are Reloaded's. The reference repeats a failed QRC20 history
>      request after a fixed 10 s (R38.6.4Q.6 item 4); a failed QRC20 balance read is
>      repeated after 10 s in both. No peer observes the difference.

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

> **Compatibility row (this repository, 2026-11-05; not corpus-sourced).**
> **Parity** since PR #247: a transaction with a change output pays the rule
> above once over its whole size, change output included, under every
> policy. Before, Reloaded's transaction builder added the change output's
> share under a dynamic rate (which an explicit `UtxoFixed` or `UtxoPerKbyte`
> withdraw fee also uses, R38.6.6l, R38.6.6m) as `floor(e × 34 / 1000)`,
> rounded on its own. The fee was then
> `floor(e × (s − 34) / 1000) + floor(e × 34 / 1000)`, which can be one base
> unit below `floor(e × s / 1000)`. A black-box comparison of the release
> binaries on 2026-11-05 (`docs/release-tests/0.4.0-beta.1.md`) showed the
> reference charging the single rounding. No peer observes the difference,
> so arm 1 of `docs/COMPAT_SWITCHES.md` "The default-side rule" does not bind
> the default, and the change has no switch.

T38.6.6b.1 *One rounding with a change output (R38.6.6b).* With a dynamic
rate, or an explicit `UtxoFixed` or `UtxoPerKbyte` withdraw fee, of
`e` = 1,234 base units per 1,000 bytes, and P2PKH inputs and outputs, a
transaction with a change output and an estimated size of 670 bytes shall pay
826 base units, and one of 2,002 bytes 2,470: `floor(e × s / 1000)` with `s`
including the change output. They shall not pay 825 and 2,469, the figures
of a separately rounded change output. Under the plain fixed policy the same
single rounding holds. Under the whole-kilobyte policy the change output adds
one more `r` only when it pushes the size into a further started kilobyte.

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

> *Reloaded status (clean side, 2026-11-18):* by the maintainer's decisions of
> 2026-11-18 a ZEC entry that is `overwintered`, has `txversion` 5 and names a
> Zcash branch id from NU7 on pays ZIP 317's conventional fee instead of the
> policies of R38.6.6a -- R38.6.6b ([Chapter 39](39-zcash---z_coin-shielded-coin.md)
> R39.11.4V.10 status note of 2026-11-18). For such an entry `F` in R38.6.6e is
> the conventional fee of a version-1 HTLC claim, 10000 base units (two logical
> actions), not an estimate over 496 bytes. With the default dust of 1,000 base
> units ZEC's minimum trading volume is therefore 0.001 ZEC, which is also the
> default minimum volume of a new ZEC order (chapter 60 R60.4.2); the value
> stays independent of any fee estimate or network state. The ZEC row of
> R38.6.6h keeps the per-kilobyte figures of a version-4 entry.

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
> *Amended 2026-11-03:* the explicit `UtxoFixed` descriptor on a UTXO
> withdraw now follows R38.6.6l item 2 (maintainer Decision 27, PR #226); the
> SLP token withdraw kept the per-started-kilobyte meaning (compatibility row
> under R38.6.6l) until PR #234, merged 2026-11-05, applied R38.6.6m there
> (Decision BV, 2026-11-04).

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
>
> *Correction (2026-11-15, clean side).* The sentence above holds for SLP, not for
> QRC20. A QRC20 token is built by the UTXO configuration builder from its own
> entry, so its own `fee_estimate_cache_secs` is validated at activation, and it
> reads its fee through its own Electrum client under its own ticker
> (`utxo_common::get_tx_fee` on the token's fields), not through the platform
> coin's client. The lifetime of that reading follows the token's own
> `fee_estimate_cache_secs`, else the QTUM entry's, else the node's setting or
> the adaptive default (R38.6.6k items 1 to 3; `request_fee_scope::fee_estimate_lifetime`).

R38.6.6l *Explicit withdraw fee descriptors: the reference's arithmetic
(reference-bound; authored by the KDF Spec Reader 2026-11-03 from the rows
`v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at both, and
compared with the row *baseline anchor (R1)*; **passed the KDF Dirty Gate
on 2026-11-03**, measure 0.00, see §38.9).* A UTXO `withdraw` or `task::withdraw::init` request may carry a
`fee` descriptor of type `UtxoFixed` or `UtxoPerKbyte`, each with an
`amount` in coin units ([Chapter 49](49-withdrawal-task-path.md) R49.1); any
other descriptor type fails the request with the invalid-fee-policy error.
At both release rows:

1. *The descriptor replaces the coin's policy.* For that one transaction the
   coin's own fee policy of R38.6.6a is not consulted; the descriptor's
   `amount`, converted to base units by the coin's decimals, is the rate `a`.
2. *Both descriptors are a rate per 1,000 bytes, applied proportionally and
   truncated.* For a transaction of size `s` bytes (virtual size for a
   segregated-witness sender) the fee is `floor(a × s / 1000)` under
   **either** descriptor. `UtxoFixed` is **not** charged as `a` per
   transaction and **not** rounded up to a started kilobyte — not even on a
   coin whose own policy is the whole-kilobyte one (R38.6.6a) — and
   `UtxoPerKbyte` is not rounded up either. At the release rows the two
   descriptors are therefore indistinguishable in every observable: the fee
   charged, the change-output allowance (`floor(a × 34 / 1000)` for both,
   R38.6.6b) and the `fee_details.amount` of the response, which reports the
   fee actually charged (inputs minus outputs).
3. *Relay floor.* When the coin forces the node's minimum relay fee
   (R38.6.6b, last bullet), the floor `floor(relay_rate × s / 1000)` applies
   to both descriptors as to any fee.
4. *Baseline anchor.* At the baseline the two descriptors differed:
   `UtxoFixed` was applied per **started** kilobyte, `a × ceil(s / 1000)`
   (the baseline's arithmetic for every fixed fee, R38.6.6b's last sentence),
   and `UtxoPerKbyte` proportionally. The reference's change of every fixed
   fee to proportional arithmetic at `v2.6.0-beta` (R38.6.6b) carried the
   explicit `UtxoFixed` descriptor with it.

> **Compatibility row (this repository, 2026-11-03; not corpus-sourced).**
> **Parity** on the UTXO `withdraw` and `task::withdraw::init` since PR #226:
> the maintainer adopted item 2 (Decision 27, 2026-11-03), and Reloaded
> applies `UtxoFixed` exactly like `UtxoPerKbyte`, `floor(a × s / 1000)`, as
> the reference at both rows (Komodo DeFi Framework 2.6.0-beta and GLEEC KDF
> 3.0.0-beta) does. Before, Reloaded applied `UtxoFixed` per started
> kilobyte, `a × ceil(s / 1000)` — the baseline's meaning, kept deliberately
> when the fee policy of R38.6.6a–b was ported (as-built note under
> R38.6.6i) — so a Reloaded node never paid less than the entered `amount`
> and paid up to one `a` more than the reference (measured values of
> 2026-10-31 in `docs/release-tests/0.3.0-beta.1.md`). **Open:** the SLP token
> withdraw on BCH still applies `UtxoFixed` per started kilobyte; how the
> reference applies a fee descriptor there is not established, and the
> question is queued for the KDF Spec Reader. *Answered 2026-11-04:
> R38.6.6m (proportional, as here); parity on the SLP route since PR #234.* No peer observes either
> behavior (a `withdraw` is the node's own transaction), so arm 1 of
> `docs/COMPAT_SWITCHES.md` "The default-side rule" does not bind the default.

T38.6.6l *Explicit withdraw fee (R38.6.6l).* Against a mocked backend, a
withdraw with `fee: {"type":"UtxoFixed","amount":"<a>"}` and one with
`fee: {"type":"UtxoPerKbyte","amount":"<a>"}` for the same inputs and
outputs shall produce, under the reference arithmetic, the same fee
`floor(a × s / 1000)` for a 245-byte and for a 1,404-byte transaction, on a
plain-fixed coin and on a whole-kilobyte coin alike, and the response's
`fee_details.amount` shall equal inputs minus outputs. The maintainer
adopted item 2 on 2026-11-03 (Decision 27), so Reloaded's tests assert the
reference arithmetic, and each test shall state which arithmetic it asserts.
*(Before that decision the `UtxoFixed` expectation for Reloaded was
`a × ceil(s / 1000)`, `a` and `2a` for the two sizes.)* *Clean-side note
(2026-11-03).* Where the builder adds the change output with the allowance of
item 2, the figure both descriptors give is
`floor(a × (s − 34) / 1000) + floor(a × 34 / 1000)`, which can be one base unit
below `floor(a × s / 1000)`; the test asserts the two descriptors equal and the
figure this split gives. With P2PKH inputs of 148 bytes the larger size reached
is 1,429 bytes rather than 1,404. *Since PR #247 (R38.6.6b, T38.6.6b.1)* the
builder charges `floor(a × s / 1000)` once, change output included, and the
tests assert that figure.

R38.6.6m *Explicit fee descriptors on the SLP token withdraw (reference-bound;
**passed the KDF Dirty Gate on 2026-11-04**, measure 0.02; Spec Reader 2026-11-04, rows `v2.6.0-beta` and
`v3.0.0-beta` of chapter 02 §2.15A, identical at both).* An SLP token
`withdraw` accepts the same two descriptors as R38.6.6l, `UtxoFixed` and
`UtxoPerKbyte`, with `amount` in the platform coin's units, and fails any
other type with the invalid-fee-policy error. At both rows the arithmetic is
R38.6.6l item 2 exactly: the descriptor's `amount`, in platform base units,
is a rate per 1,000 bytes applied **proportionally and truncated**,
`floor(a × s / 1000)`, under either descriptor; neither is charged per
started kilobyte. The only started-kilobyte arithmetic the reference's
transaction builder has is the whole-kilobyte coin policy of R38.6.6a, which
a withdraw descriptor never selects. The response's `fee_details` reports the
fee actually charged, in the platform coin and named by its ticker. A request
that carries `from` fails as unsupported (chapter 49 R49.27).

*Pointer (2026-11-04; passed the KDF Dirty Gate on 2026-11-04, measure 0.00;
provenance and clearance in chapter 49 §49.7).* The wire form of that
`from` refusal -- `error_type` `UnsupportedError`, HTTP 400, string
`error_data` -- and the distinct `InvalidFeePolicy` discriminant of the
wrong-type descriptor above are bound in chapter 49 R49.27a.

> **Compatibility row (this repository, 2026-11-04; not corpus-sourced).**
> **Parity** on the SLP token withdraw since PR #234: the maintainer adopted
> R38.6.6m (Decision BV, 2026-11-04), and Reloaded applies `UtxoFixed` there
> exactly like `UtxoPerKbyte`, as the UTXO route does since Decision 27. It is
> a parity change without a switch. Before, Reloaded's SLP withdraw applied
> `UtxoFixed` per started kilobyte, `a × ceil(s / 1000)` -- the "Open" item of
> R38.6.6l's row -- so it paid up to one `a` more than the reference and never
> less. No peer observes either behavior, so arm 1 of
> `docs/COMPAT_SWITCHES.md` "The default-side rule" does not bind the default.

T38.6.6m *Explicit SLP withdraw fee (R38.6.6m).* Against a mocked BCH
backend, an SLP token withdraw with `fee: {"type":"UtxoFixed","amount":"<a>"}`
and one with `fee: {"type":"UtxoPerKbyte","amount":"<a>"}` for the same inputs
and outputs shall charge the same fee `floor(a × s / 1000)` for one
transaction under and one over 1,000 bytes and report it in `fee_details` in
the platform coin; a third request with an EVM-form descriptor shall fail with
the invalid-fee-policy error. *Clean-side note (2026-11-05).* As under
T38.6.6l, where the builder adds the change output the figure both descriptors
give is `floor(a × (s − 34) / 1000) + floor(a × 34 / 1000)`, which can be one
base unit below `floor(a × s / 1000)`. The tests of PR #234 assert the two
descriptors equal and this figure, at 457 and 1,493 bytes; the SLP OP_RETURN
output makes these sizes differ from T38.6.6l's. *Since PR #247 (R38.6.6b,
T38.6.6b.1)* the figure is `floor(a × s / 1000)` once, and the tests assert it.

R38.6.6n *Required inputs count toward the input total (reference-bound;
Spec Reader micro-run 0.4.0-2c, 2026-11-04, rows `v2.6.0-beta` and
`v3.0.0-beta` of chapter 02 §2.15A, identical at both).* When a UTXO-family
transaction is built with inputs the caller requires to be spent (the
token-carrying inputs of an SLP withdraw or SLP send, the HTLC output of an
SLP spend or refund, or any required input given to the generic send path),
the value of every required input is part of the transaction's input total
for all purposes — the sufficiency check, the change computation,
`spent_by_me`, and the fee reported in `fee_details` — so that a required
input's coin value returns to the sender as change under the ordinary dust
rule and `fee_details.amount` equals the sum of all inputs minus the sum of
all outputs. A change output is added whenever the computed change is not
below the coin's dust amount; only a change strictly below dust is left to
the miner, and that remainder is included in the reported fee on every
withdraw path.

> **Compatibility row (this repository, 2026-11-04; not corpus-sourced).**
> **Parity** since PR #236 (maintainer-side routing 2026-11-04: a correctness
> fix, no switch). Before, Reloaded's UTXO transaction builder appended
> required inputs to the transaction without adding their value to its input
> total. Their coin value was therefore neither returned as change nor
> reported: on an SLP withdraw, SLP send, or SLP HTLC spend or refund, the
> dust each token input or HTLC output carried (546 base units on BCH) went
> to the miner. `fee_details.amount` and `spent_by_me` of an SLP withdraw
> omitted it, and the sufficiency check ignored it. Two further differences
> closed with it: a change exactly equal to the dust amount went to the miner
> instead of becoming a change output, and the SLP withdraw's `fee_details`
> left out a sub-dust remainder that the UTXO withdraw already reports. No
> peer observes either side, because these are the node's own transactions,
> so arm 1 of `docs/COMPAT_SWITCHES.md` "The default-side rule" does not bind
> the default.

T38.6.6n *Required inputs and the dust boundary (R38.6.6n).* Against a
mocked BCH backend, an SLP token withdraw spending two token inputs of 546
base units each shall produce a platform-coin change output whose value
includes those 1,092 base units, and `fee_details.amount` shall equal the
transaction's inputs minus outputs; the same shall hold for a generic send
with one required input. *(Reloaded-original additions.)* A transaction whose
computed change equals the dust amount exactly shall carry a change output of
that value; and an SLP token withdraw whose change falls below dust shall
report in `fee_details.amount` the computed fee plus that remainder, again
equal to inputs minus outputs.

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
> position, except on the shielded-coin fallback, which names it by the
> placeholder alone (item 1 is not met there; ch.39 D39.11.o). An address with
> userinfo or a path before the port is rejected
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

### 38.6.10 Dash-layout special transactions

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-18,
> measure 0.03 (attestation in §38.9).**
> This section was written on 2026-11-18 by the KDF Spec Reader (Spec Reader
> round 0.5.0-12)
> under the two-team procedure of chapter 01 §1.4A, from the reference lineage
> at the rows `v2.6.0-beta`, `v3.0.0-beta`, `main (as of 2026-10-07)` and
> `add-krgn-chain-variant (as of 2026-04-18)` of chapter 02 §2.15A (read
> read-only), the published Dash special-transaction format (R3: DIP-2) and
> the Kerrigan source (R3, MIT, main branch as of 2026-10-19), the public coins
> file, and this repository's own tree. It edits no sentence of this chapter.
> Numbering: R38.6.10.1 -- R38.6.10.6 and T38.6.10.1 -- T38.6.10.4.

*Public format (R3).* A Dash-layout transaction starts with a 32-bit word whose
low 16 bits are the version and whose high 16 bits are the type. Inputs,
outputs and the lock-time follow as in Bitcoin. When the version is 3 or higher
and the type is not 0, a compact-size-prefixed extra payload follows the
lock-time. The transaction identifier is the double-SHA256 of the whole
serialization, payload included. Dash's types are 1 to 4 (masternode provider
transactions), 5 (the coinbase special transaction carried by every block),
6 (quorum commitment, no inputs and no outputs), 7 (hard-fork signal),
8 (asset lock) and 9 (asset unlock, no inputs). Kerrigan uses the same layout
and the same types and adds type 10, a Sapling transaction whose shielded part
travels in the payload, and type 11; its ordinary transactions are version 2.
Dash has no witness serialization.

**R38.6.10.1** *Reference states.* The rows `v2.6.0-beta`, `v3.0.0-beta`,
`main (as of 2026-10-07)` and `add-krgn-chain-variant (as of 2026-04-18)` agree
on every point of R38.6.10.2 -- R38.6.10.4.

**R38.6.10.2** *How the reference reads such a transaction.* No coin-entry key
is involved (chapter 45 §45.3A): the same reading applies to every UTXO coin,
and DASH, whose entry carries no format key at all, is read by it like any
other. When the low 16 bits of the leading word are 3, whatever the high 16
bits hold, the reference reads a compact-size-prefixed payload after the
lock-time (after the Zcash-family body, for an overwintered transaction)
whenever bytes remain. It keeps the payload as opaque bytes and keeps the whole
32-bit word as the version. It does not look at the type, and it reads no
payload for any other version. When it writes the transaction again, as it
does to compute the identifier, it writes the word unchanged and then the
payload with its length prefix, so the identifier of a special transaction with
inputs is the chain's transaction id. A payload of length zero, which the Dash
rules do not produce for a special type, is read but not written back.

**R38.6.10.3** *Special transactions with no inputs.* The reference takes an
empty input list at the start of a non-overwintered transaction as the marker
of a witness serialization. A quorum commitment or an asset unlock is read
correctly by its Zcash-family reading, which does not do that. Whether one of
its other readings accepts such a transaction first, in a wrong shape, depends
on the bytes: the case at risk is an asset unlock with exactly one output,
because the output count then equals the witness flag. This pass did not
establish the outcome on real transactions. A Kerrigan Sapling transaction
with no transparent inputs falls under the same case.

**R38.6.10.4** *History, balances and swaps under the reference.*

- *History.* A special transaction that is read correctly gives an ordinary row
  with the chain's identifier. Its amounts and fee come from the transparent
  inputs and outputs only, because the payload is never interpreted, so value
  that a Kerrigan Sapling payload moves into or out of the shielded pool is
  not shown as such. A transaction that cannot be read is skipped with a log
  line by the legacy history loop and tried again on a later pass. The
  version-two history instead enters its error cooldown and tries the same
  transaction again, so it does not advance past that transaction.
- *Balances and spendable outputs.* These come from the backend's balance and
  unspent-output answers. No transaction is read to form them, and an output of
  a special transaction (a coinbase payout, for instance) is spent through the
  identifier the backend reports. Special transactions do not affect them.
- *Swaps.* Every transaction the reference builds, payments included, uses the
  entry's `txversion` (1 when absent, as for DASH and the proposed Kerrigan
  entry) with type 0, so it is never a special transaction. Validating a
  counterparty's payment reads it with the same reader and checks its outputs;
  nothing in validation looks at the type or a payload. In a swap between two
  reference nodes, no swap transaction is special.

**R38.6.10.5** *Requirement.* The project shall read and write a transaction
whose leading word has its top bit clear, a version (low 16 bits) of 3 or
higher and a type (high 16 bits) other than 0 by the public format: it reads the
compact-size-prefixed payload after the lock-time, keeps it, and writes it back
with its length prefix, so the identifier it computes is the chain's
transaction id. For such a transaction, an empty input list is not a witness
marker. A type-0 transaction and every transaction with the top bit set keep
their present reading. On every transaction the Dash and Kerrigan chains carry,
this matches R38.6.10.2. It differs only where the reference's outcome is not
established (R38.6.10.3) and for a zero-length payload, which the chains do not
produce. The payload is not interpreted.

> **Code-quality finding (informative; this repository's own code).** This
> repository's transaction reader has no payload step, and it accepts a reading
> that stops before the end of the bytes. A special transaction with inputs is
> therefore read as if it had no payload: the trailing bytes are ignored, and
> the identifier computed from the re-written bytes is not the chain's
> transaction id. The UTXO history uses that computed identifier as the row's
> transaction hash and internal id. A DASH wallet that receives a coinbase
> payout or takes part in a masternode provider transaction, or a Kerrigan
> wallet that shields or unshields, would get a history row whose identifier
> the chain does not know. R38.6.10.5 is the fix. Whether the reader should
> also refuse bytes left over after a complete reading is a wider question,
> left to the orchestrator.

> **Status note (2026-11-22, Reloaded-original, clean side; maintainer decision
> of 2026-11-21).** R38.6.10.5 is built. The shared transaction reader treats a
> transaction as special when its leading word has the top bit clear, a version
> of 3 or higher in the low 16 bits and a type other than 0 in the high 16 bits.
> For such a transaction, in every reading path, it reads no time field, takes
> an empty input list as an empty input list rather than as a witness marker,
> and, when bytes follow the lock time, reads the compact-size-prefixed payload
> into a field of its own. The writer puts the payload back after the lock time
> with its prefix, so the computed identifier, and with it the identifier of a
> history row, is the chain's transaction id. A payload whose prefix claims more
> bytes than remain is refused in every path. A special-shaped transaction with
> nothing after its lock time is read without a payload, as R38.6.10.2 describes
> for the reference, so a transaction of another standard-layout chain whose
> version word only happens to have this shape keeps its reading. Such a
> transaction with a witness would not have its witness marker recognized, and
> on a chain read with the time-field layout such a word would be read without
> its time field and trailing string; no listed chain is known to produce
> either. The whole leading word is kept as the version, so the type is the high
> 16 bits of it; nothing interprets the payload, and no special transaction is
> built. Leftover bytes after a complete reading are still not refused in
> general (the finding above stays open on that point). PIVX special
> transactions are not covered by this rule; their reading is not established.
> Tests: T38.6.10.1 with a DASH coinbase (type 5), provider registration (type
> 1), asset unlock (type 9, no inputs, one output), a version-3 type-0
> transaction and a Kerrigan Sapling transaction (type 10, no transparent
> inputs, one output), all from the public chains; T38.6.10.2 to T38.6.10.4 as
> stated; and a special-shaped transaction without a payload. A DASH quorum
> commitment (type 6, no inputs and no outputs) is also read with its payload
> and the chain's identifier by the shared reader.

**R38.6.10.6** *Kerrigan headers are not this section's subject.* How the
reference reads Kerrigan's headers, the median-time-past and the HTLC
lock-times is recorded in chapter 37 §37.5.11.

**T38.6.10.1** *Real-chain vectors.* Pinned from the public chains: a DASH
coinbase special transaction (type 5), a DASH provider registration (type 1), a
DASH asset unlock (type 9, no inputs, one output), a DASH version-3 type-0
transaction, and a Kerrigan Sapling transaction (type 10). Each one is read,
written back byte-exact, and its computed identifier equals the chain's
transaction id.

**T38.6.10.2** *Unchanged readings.* An overwintered version-3 transaction (the
low 16 bits of its leading word are also 3) and a segwit transaction keep their
present results.

**T38.6.10.3** *History identifier.* A history row built from a special
transaction carries the chain's transaction id as its hash and internal id.

**T38.6.10.4** *Truncated payload.* A special transaction whose payload length
prefix is longer than the remaining bytes is refused as malformed; it is not
read as a transaction without a payload.

## 38.8 Software global-HD UTXO accounts & address derivation (T-PORT)

> **Status of §38.8:** required port, built on dev *(corrected 2026-11-04)*.
> When this section was written, the reloaded UTXO HD path could only obtain
> an account extended public key through a hardware device, so a **software**
> (non-hardware) global-HD account could not create an HD account or derive HD
> addresses. The software source of Chapter 5 §5.9A, the account-`0`
> bootstrap, `get_new_address`, the scan and gap-limit rules and
> `min_addresses_number` are now built. Remaining: `task::create_new_account`
> for an account other than `0` failed under a software global-HD login until
> PR #279 (merged 2026-11-07; update below), because it asked for a hardware
> device first (the Chapter 20 §20.10 D1 note records it); and `path_to_address` is honored for a software global-HD
> account only, not under the Trezor policy (Chapter 55 §55.5). Both are items
> of plan [`hd-key-unification`](../plans/hd-key-unification.md). *(Pointer
> added 2026-11-07; passed the KDF Dirty Gate with its target on 2026-11-07.)* The
> reference contract for both -- and for the listing of the address at
> `path_to_address` from the first activation on, and for `fetch_utxos`,
> `consolidate_utxos` and the automatic merge under HD -- is recorded in
> [Chapter 5](05-hd-wallet-support.md) §5.9B R35a--R35c. This section
> binds the software-HD UTXO behaviour as first-class, testable requirements.
> It is the coin-side consumer of Chapter 5
> §5.9A (R29–R31), which binds the crypto substrate (software wallet identity,
> software account-xpub derivation and canonical serialisation, and source
> selection by key-pair policy). §38.8 binds the UTXO account bootstrap, the
> `get_new_address` advance, and the scan/gap behaviour for both BIP-44 and
> BIP-84 coins. It cross-references Chapter 5 R15–R20, R29–R31; Chapter 7 R28;
> §38.3.3 (HD purpose generality); and Chapter 45 R45.4.8 (HD-mode identity
> selection).
>
> *Update (2026-11-07, plan items H5.0 and H5.4).* The first remaining item is
> built: `task::create_new_account` serves a software global-HD login (Chapter
> 5 R35c and its as-built note). Activation lists the address at
> `path_to_address` from the first activation on (Chapter 5 R35a). Under the
> Trezor policy, `path_to_address` is still not honored for a UTXO coin.

R38.8.1 **Software-HD storage availability.** When a UTXO coin is activated in
HD mode under a software global-HD account (key-pair policy `GlobalHDAccount`,
selected by Chapter 45 R45.4.8 / Chapter 7 R28) with the coin's own key policy
being the context key rather than `Trezor` — whether a hardware-wallet context
happens to exist is immaterial (Chapter 5 §5.10B R52) — the
coin's per-wallet HD-account storage MUST initialise successfully using the
software wallet-identity digest of Chapter 5 R29 (the in-context
`RIPEMD160(SHA256(pubkey))` identity, equal to the daemon-wide `mm2_rmd160`).
It MUST NOT be refused on the grounds that no hardware device is connected. Iguana mode remains unsupported for HD (the request is
refused); a coin activated under the `Trezor` key policy is unchanged and is
namespaced by the device digest (Chapter 5 §5.10B R55). The on-disk identity
namespacing (`mm2_rmd160`, `hd_wallet_rmd160` columns) is unchanged; for a
context-key coin both take the same software-derived value. *(Re-keyed
2026-11-08 on the coin's policy; passed the KDF Dirty Gate with Chapter 5
§5.10B on 2026-11-08.)*

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

*Status note (2026-11-22), the page total is a map (as built; clean side, not
corpus-sourced).* `account_balance` of every HD family now answers
`page_balance` as a ticker-keyed map of the same type as an address's
`balance`, where it answered one `{spendable, unspendable}` balance
([Chapter 49](49-withdrawal-task-path.md) R49.35). Where the reference answers
`{}` -- a page that lists no address, and the `total_balance` of an account with
no known address -- this repository keeps the form of this rule: a one-entry map
holding the requested coin's ticker and a zero balance. That is a recorded
divergence, decided by the maintainer on 2026-11-22. A UTXO or QTUM coin's page
total therefore holds its own ticker, summed over the addresses of the returned
page; `total_balance` of the task form and of the activation report is formed as
before.

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
- §38.5B (cleared by the Dirty Gate on 2026-10-30; R38.5B.9 by maintainer
  Decision BE): T38.5B.1 to T38.5B.10 pass.
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
- Qtum ElectrumX server, *QRC20 integration* (`docs/qrc20-integration.md`):
  the server's definition of the QRC20 methods, their parameters and their
  answer members named in R38.6.4Q.8 (permitted-input class R3/R4), fetched
  2026-11-15,
  <https://github.com/qtumproject/qtum-electrumx-server/blob/master/docs/qrc20-integration.md>.
- Qtum Core public source: the `gettransactionreceipt` RPC, whose receipt form
  R38.6.4Q.8 item 3 refers to, defined in `src/rpc/blockchain.cpp`,
  <https://github.com/qtumproject/qtum> (permitted-input class R3/R4).

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
- *Spent-output bookkeeping item (§38.5B, 2026-10-30) -- cleared by the Dirty
  Gate (chapter 01 R40).* R38.5B.1--R38.5B.8, the scope note, the consequence
  note, the code-quality finding, D38.5B.1--D38.5B.3, T38.5B.1--T38.5B.5,
  the §38.0 scope line, the §38.7 bullet and the pointer sentence in
  [Chapter 49](49-withdrawal-task-path.md) §49.6 were written by the Spec
  Reader under the two-team workflow at the rows `v2.6.0-beta` and
  `v3.0.0-beta` of chapter 02 §2.15A, which agree on every fact bound there
  (the record type, the withdraw builder and the listing functions are
  identical between the rows; the broadcast client differs between the rows
  only outside the broadcast call), and at the row *baseline anchor (R1)*.
  Corpus-sourced: the confirmation that each fact of R38.5B.1--R38.5B.8
  persists at both rows, the external-wallet signing sentence of R38.5B.2
  (later row only), the mmrpc-2.0 absence facts of R38.5B.3 and R38.5B.7, and
  the admission the code-quality finding reports. Baseline-derived
  (permitted-input class R1, independently checkable in this repository's
  own history): the existence of the record, the after-acceptance entry of
  R38.5B.1, the withdraw builder's discarding of the record's lock
  (R38.5B.2), the forwarding-only `send_raw_transaction` (R38.5B.3), the
  listing composition and maturity rule (R38.5B.4), the two locks
  (R38.5B.5), the legacy error body (R38.5B.6) and the absence of eviction
  (R38.5B.8). Not corpus-sourced: the field observation (class R6, this
  project's own run of 2026-10-30), the decision items, the tests, the
  parity statement about this repository, and the Electrum method string
  (public protocol). The Spec Reader read no state not already tabled in
  chapter 02 §2.15A, so no row was added. *Dirty Gate clearance, 2026-10-30:*
  PASS. Scope attestation: the extent examined was the UTXO coin layer's
  spent-output bookkeeping (the recently-spent record and its lifetime), the
  user withdraw builder for UTXO coins (direct and task-based), the legacy
  raw-transaction broadcast RPC and its error rendering through the shared
  JSON-RPC client, the unspent-listing composition, the hardware-wallet
  address re-establishment hook, and the legacy and mmrpc-2.0 dispatch surface
  for broadcast; compared at `v2.6.0-beta` (13 files read or searched),
  `v3.0.0-beta` (the same subsystem; 8 of those files diffed against the
  earlier row plus a repository-wide text search; the record type, the
  withdraw builder and the listing functions are unchanged between the rows)
  and the baseline anchor (R1) (6 files read or searched); a repository-wide
  mechanical search for mempool-conflict matching at the two later rows
  returned nothing in the UTXO and RPC layers. Method: reading combined with
  mechanical text search (git grep and git diff at the tabled commits);
  `tools/clean_room_gate.py` was not run (the section has no code blocks).
  Measure: 0.03 on the discretionary-expression basis against the R35
  residual-similarity standard.
- *Reloaded-original addition (2026-10-30, maintainer Decision BE), clean
  side.* R38.5B.9, the divergence notes after R38.5B.3 and R38.5B.8, the
  decided states of D38.5B.1--D38.5B.3, D38.5B.4, the rewritten T38.5B.2 and
  T38.5B.3, T38.5B.6--T38.5B.10 and the §38.7 bullet were written by a session
  that never read the corpus. They rest on the maintainer's Decision BE of
  2026-10-30, the cleared text of R38.5B.1--R38.5B.8 and this repository's own
  code. They take no forbidden-class input, so chapter 01 R38 is not engaged
  and no further gate pass is required.
- *Reloaded-original amendment (2026-11-02, maintainer Decision BI), clean side.*
  R38.6.4a items 6–8 (the near offset of item 8 added the same day), the
  changes to its items 1 and 3, its second rationale and herding note, its
  as-built note for items 6–8, T38.6.4a.1–T38.6.4a.7, D38.6.4a.1 and
  D38.6.4a.2, and the cross-references in R38.6.4l items 1, 2 and 5 were
  written by a session that never read the corpus. They rest on the
  maintainer's Decision BI of 2026-10-31, this project's own lab timing run of
  2026-10-31, its own latency benchmark of 2026-11-01 and this repository's
  code. They take no forbidden-class input, so chapter 01 R38 is not engaged
  and no gate pass is required.
- *Clean-context addition (2026-11-02, forbidden corpus not consulted):* the
  sentence on resolving the address in R38.6.4e item 1 is Reloaded-original. It
  points to chapter 45 §45.6B, from the maintainer's decision of 2026-11-02 and
  this repository's own code, so chapter 01 R38 is not engaged.
- *Spec Reader additions (2026-11-03) -- R38.6.6l, T38.6.6l and the
  re-verification note after R38.5B.3.* **KDF Dirty Gate: PASS on
  2026-11-03, measure 0.00.** The forbidden corpus was consulted at the
  reference designations `v2.6.0-beta` and `v3.0.0-beta`, which agree,
  and at the row *baseline anchor (R1)*, **only** to establish how an explicit `UtxoFixed` or
  `UtxoPerKbyte` withdraw fee descriptor is applied (the descriptor
  replaces the coin's policy; both are a proportional, truncated rate per
  1,000 bytes at the release rows; `UtxoFixed` was per started kilobyte at
  the baseline; the relay floor and change allowance apply alike) and to
  re-confirm R38.5B.2, R38.5B.4, R38.5B.5 and R38.5B.8 for the withdraw path
  (no spent-outpoint or change recording by a withdraw, no mempool query,
  the record's lifetime). Corpus-sourced: items 1–4 of R38.6.6l and the
  reference facts of the re-verification note. Not corpus-sourced: the
  compatibility row, T38.6.6l and every Reloaded statement, which rest on
  this repository's own code, `docs/GLEEC_COMPATIBILITY.md` and R38.5B.9.
  Only behavior, the public request members (`fee`, `type`, `UtxoFixed`,
  `UtxoPerKbyte`, `amount`, `fee_details`) and arithmetic a client can
  observe cross into the chapter; no private name, body, control flow or
  literal was reproduced.
- *Clean-side amendment (2026-11-03) -- maintainer Decision 27.* After the
  maintainer adopted R38.6.6l item 2, a session that never read the corpus
  updated the compatibility row under R38.6.6l, the last sentence of
  T38.6.6l with its clean-side note, and the as-built note under R38.6.6i
  (PR #226). They rest on that
  decision and this repository's code, take no forbidden-class input, and so
  do not engage chapter 01 R38.
- *Scoped addition (2026-11-04) -- R38.6.6m, T38.6.6m, its compatibility row
  and the one-sentence resolution appended to R38.6.6l's row.* **KDF Dirty
  Gate: PASS on 2026-11-04, measure 0.02.** *Inputs:* the forbidden corpus at the rows `v2.6.0-beta`
  and `v3.0.0-beta` of chapter 02 §2.15A, identical at both, consulted
  **only** to establish how the SLP token withdraw maps the two explicit fee
  descriptors onto the transaction builder's rate, the arithmetic that rate
  produces, the unit and ticker of the reported fee, and the refusal of
  `from`. Behavior and dictated request and response members only; no private
  identifier, function body, helper decomposition, error, log or comment text
  was reproduced. The compatibility row rests on this repository's own code;
  T38.6.6m is Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-04, measure
  0.02, no findings. *Extent:* the SLP token withdraw's fee-descriptor
  handling and the transaction builder's fee arithmetic, at `v3.0.0-beta` with
  `v2.6.0-beta` as cross-check; for the whole batch, 30 source files and one
  dependency manifest were read at `v3.0.0-beta`, 29 of them checked by hash
  against the tagged states of both rows. *Method:* side-by-side reading of
  the compared regions against every added or changed line, plus that hash
  check; the gate script was not run because the candidate has no code blocks.
  *Measure:* 0.02, read against the standard that any discretionary finding
  fails a file; dictated-interop and public-surface tokens were excluded from
  the measured set.
- *Clean-side correction (2026-11-04) -- the §38.8 bullet of §38.0 and the
  status note at the head of §38.8.* They now say that §38.8 is built and
  name what remains. Written by a session that never read the corpus, from
  this repository's own activation, account and address code. It takes no
  forbidden-class input and needs no Dirty Gate pass.
- *Scoped addition (2026-11-04) -- R38.6.6n, T38.6.6n and its compatibility
  row.* The rule's two sentences and the first sentence of T38.6.6n are the
  Reloaded-original wording proposed by the Spec Reader micro-run 0.4.0-2c
  (gated 2026-11-04, micro-run 0.4.0-2c, PASS 0.03) and are placed here
  verbatim. *Inputs:* the forbidden corpus at the rows `v2.6.0-beta` and
  `v3.0.0-beta` of chapter 02 §2.15A, identical at both, consulted **only** to
  establish whether the UTXO transaction builder counts the value of inputs
  the caller requires toward its input total (sufficiency, change,
  `spent_by_me`, reported fee) and on which side of the dust amount a change
  output is added. Behavior and public response members only; no private
  identifier, function body, control flow, error, log or comment text was
  reproduced. **KDF Dirty Gate:** the report the wording comes from passed;
  placement gated 2026-11-04, KDF Dirty Gate: PASS, measure 0.02 on the R35
  discretionary-expression basis; extent = UTXO transaction building, SLP
  withdraw/HTLC/send and generic send paths at rows `v2.6.0-beta` and
  `v3.0.0-beta`; method = reading plus mechanical diff. The
  compatibility row and the Reloaded-original additions to T38.6.6n rest on
  this repository's own code and take no forbidden-class input.
- *Clean-side amendment (2026-11-05) -- PR #234, maintainer Decision BV.* After
  the code change, a session that never read the corpus updated the
  compatibility row under R38.6.6m, the resolution sentence in R38.6.6l's row
  and the clean-side note under T38.6.6m. They rest on that decision and this
  repository's code, take no forbidden-class input, and so do not engage
  chapter 01 R38.
- *Clean-side amendment (2026-11-05) -- PR #247, the change output under one
  rounding.* A session that never read the corpus added the compatibility row
  under R38.6.6b and T38.6.6b.1, and appended a sentence each to the clean-side
  notes under T38.6.6l and T38.6.6m. They rest on R38.6.6b as cleared above, on
  a black-box comparison of the release binaries and on this repository's own
  code. They take no forbidden-class input, so they do not engage chapter 01
  R38.
- *Clean-side addition (2026-11-05) -- R38.6.4m, R38.6.4n, T38.6.4m, T38.6.4n,
  D38.6.4m.1, D38.6.4n.1 and D38.6.4n.2.* Written by a session that never read
  the forbidden corpus. The inputs are this repository's own code (the
  Electrum TLS configuration, the server specification and the pool), the
  public documentation and behavior of the TLS crates it depends on (`rustls`,
  `rustls-webpki`, `ring`, `webpki-roots`) at the versions in `Cargo.lock`, and
  the maintainer's decision of 2026-11-04. They take no forbidden-class input,
  so chapter 01 R38 is not engaged and no gate pass is required.
- *Reloaded-original amendment (2026-11-06, maintainer Decision CX), clean side.*
  The floor of two in R38.6.4a item 6 and its last sentence, the changed
  sentence of the second rationale, the rationale for the floor, the new cases
  of T38.6.4a.1, T38.6.4a.8, D38.6.4a.3, the floor bullet of the as-built
  note for items 6–8 and the cross-reference in D38.6.4a.1 were written by a
  session that never read the corpus. They rest on the
  maintainer's Decision CX of 2026-11-06, this project's own lab timing runs of
  2026-11-06 and this repository's code. They take no forbidden-class input, so
  chapter 01 R38 is not engaged and no gate pass is required.
- *Scoped addition (2026-11-07) -- the reference-behavior note under
  R38.6.4m, and the pointers in R38.5.2 and the §38.8 status note.* **KDF
  Dirty Gate: PASS on 2026-11-07, measure 0.03.** *Inputs:* the forbidden corpus at the rows `v2.6.0-beta` and
  `v3.0.0-beta` of chapter 02 §2.15A, identical at both, consulted **only**
  to establish the per-server TLS members of an Electrum server entry, which
  certificate checks `disable_cert_verification` turns off and which remain,
  how a refused certificate or failed handshake is classified for the pool,
  how an IP literal is treated, and whether the flag is warned about; together
  with the public documentation of the TLS library version the reference pins.
  Behavior and dictated request members only; no private identifier, function
  body, helper decomposition, error, log or comment text was reproduced. The
  two pointers carry no reference content of their own; their targets are
  chapter 05 R35a--R35c, which passed the same gate on 2026-11-07.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-07, measure
  0.03, no findings. *Extent:* Electrum per-server TLS settings, the
  certificate-verification switch, connection-failure classification and the
  connected-line logging, at `v3.0.0-beta` with `v2.6.0-beta` as cross-check,
  plus two manifest or lock files for the TLS library version; for the whole
  batch, about 24 source files were read in full or in the relevant regions at
  the `dev (as of 2026-03-27)` row, whose source tree is identical to
  `v3.0.0-beta`, and the same modules were diffed against `v2.6.0-beta`.
  *Method:* reading, with mechanical cross-checks (diffs between the tabled
  states; pattern searches of the added lines for paths, identifiers and
  markers); the gate script was not run because the candidate has no code
  blocks. *Measure:* 0.03, read against the standard that any discretionary
  finding fails a chapter; dictated-interop and public-surface tokens were
  excluded from the measured set.
- *Reloaded-original addition (2026-11-09, maintainer decisions D38.6.4o.1 –
  D38.6.4o.8; D38.6.4o.9 as amended below), clean side -- R38.6.4o, T38.6.4o.1 –
  T38.6.4o.11, D38.6.4o.1 – D38.6.4o.9, and the amendments they name in R38.6.4a items 6 and 7, R38.6.4f
  item 4, R38.6.4l items 1, 4, 6 and 7, the test preamble of R38.6.4l,
  T38.6.4a.5, T38.6.4l.1, T38.6.4l.3, T38.6.4l.15 and D38.6.4a.1.* Written by a session that never read
  the forbidden corpus. The inputs are this repository's own code and chapter
  text, the coins file's `avg_blocktime` values, field reports of stale
  Electrum servers, the public Electrum protocol, and the maintainer's
  decisions of 2026-11-09. They take no forbidden-class input, so chapter 01
  R38 is not engaged and no gate pass is required.
- *Reloaded-original amendment (2026-11-09, maintainer decisions D38.6.4a.4 and
  D38.6.4j.1), clean side -- R38.6.4a items 6 and 7, R38.6.4f item 2, R38.6.4j
  items 2 and 4, T38.6.4.14, T38.6.4j.1, T38.6.4a.9, D38.6.4a.4 and D38.6.4j.1.*
  Written by a session that never read the forbidden corpus. The inputs are
  this repository's code, a field report of a failed activation, this
  project's own lab reproduction of it, and the maintainer's decisions of
  2026-11-09. They take no forbidden-class input, so chapter 01 R38 is not
  engaged and no gate pass is required.
- *Reloaded-original amendment (2026-11-09, maintainer decisions D38.6.4o.9 –
  D38.6.4o.13), clean side -- R38.6.4o items 2, 4, 7, 9, 10, 11, 12 and 13,
  T38.6.4o.6 – T38.6.4o.8, T38.6.4o.12 – T38.6.4o.16 and D38.6.4o.9 – D38.6.4o.13.*
  Written by a session that never read the forbidden corpus. D38.6.4o.9
  replaces a rule this chapter recorded earlier the same day and the
  maintainer did not confirm. The inputs are this repository's code, the
  public Electrum protocol and the maintainer's decisions of 2026-11-09. They
  take no forbidden-class input, so chapter 01 R38 is not engaged and no gate
  pass is required.
- *Reloaded-original addition (2026-11-09, PR #327), clean side -- §38.6.5
  item 8.* Inputs: this repository's code and a field report of an Electrum
  server with an expired certificate; no forbidden-class input, so chapter 01
  R38 is not engaged and no gate pass is required.
- *Reloaded-original amendment (2026-11-10, D38.6.4o.14 and maintainer
  decision D38.6.4o.15), clean side -- R38.6.4o items 7 and 12 (dated amendment
  paragraphs), T38.6.4o.17 – T38.6.4o.22, D38.6.4o.14 and D38.6.4o.15.* Written
  by a session that never read the forbidden corpus. The inputs are this
  repository's code, the public Electrum protocol, a field report and a field
  log of a maker with refused orders, this project's own queries of two public
  DOGE Electrum servers, and the maintainer's decision of 2026-11-10. They take
  no forbidden-class input, so chapter 01 R38 is not engaged and no gate pass
  is required.
- *Reloaded-original amendment (2026-11-14, maintainer decision), clean side --
  dated status notes after R38.6.4b-R item 3, R38.6.4l item 1 and its
  divergence note, T38.6.4.13-R, the as-built note of R38.6.4o, D38.6.4l.1 and
  D38.6.4o.6: the default of
  `electrum_server_order` is `mixed`.* Written by a session that never read the
  forbidden corpus. The input is the maintainer's decision of 2026-11-14. It
  takes no forbidden-class input, so chapter 01 R38 is not engaged and no gate
  pass is required.
- *Reloaded-original corrections (2026-11-15), clean side -- dated notes after the
  §38.6 bullet of §38.0, after the R38.6.4o as-built note's sentence on which
  client a token answers from, and after the per-coin fee-lifetime passage of
  R38.6.6k: QRC20 tokens use Electrum client fields of their own.* Written by a
  session that never read the forbidden corpus. The input is this repository's
  code. It takes no forbidden-class input, so chapter 01 R38 is not engaged and
  no gate pass is required.
- *QTUM and QRC20 over Electrum (§38.6.4Q, 2026-11-15) -- KDF Dirty Gate:
  PASS on 2026-11-15, measure 0.02, no findings.* R38.6.4Q.1–R38.6.4Q.10, the findings and notes under them,
  D38.6.4Q.1–D38.6.4Q.5, T38.6.4Q.1–T38.6.4Q.11 and the closing *Not
  established* paragraph were written by the Spec Reader under the two-team
  workflow (chapter 01 §1.4A) in Spec Reader round 0.5.0-1. They passed
  the gate under chapter 01 R40; the attestation below closes this item.
  *Inputs:* (1) The forbidden
  corpus at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A,
  consulted **only** to establish, at each row: that every activated
  instance builds its own Electrum client, and that a QRC20 token has no
  activation path besides the legacy one; that an error answer ends a
  request; which QTUM-specific wire methods are sent, by which flows and in
  which order, and which are never sent; the request order, the waits and
  the failure handling of the token history collector; which spends take
  the process-wide build lock or a record's lock, and which enter a record;
  that disabling an instance ends its background work at once, and that a
  token is not registered as depending on QTUM; that the balance stream
  refuses a token; and that the two rows agree on all of it. Between the
  rows, the token history collector, the QTUM-specific request surface, the
  delegation path, the withdraw path, the connection pool, the shared
  JSON-RPC layer, the background-task mechanism, the legacy activation and
  deactivation requests and the balance-stream activation are identical;
  the token and QTUM coin types, the shared UTXO helpers, the coin registry
  and the activation builder differ only in areas this subsection does not
  touch. What crosses is behavior and dictated interface; no private
  identifier, function body, helper decomposition, error, log or comment
  text was reproduced. The one literal, the `history too large` error
  object, is the server's own wire answer and is in the baseline. (2) The
  row *baseline anchor (R1)* and this repository's dev branch as of
  2026-11-15, for every fact marked *baseline*, the findings and notes that
  name this repository or this tree, the notes on existing text, and the
  method and member names of the QTUM client surface. (3) The public
  document `docs/qrc20-integration.md` of
  `qtumproject/qtum-electrumx-server`, fetched 2026-11-15, for the names,
  the parameter order and the answer members of the server's QRC20 methods
  (chapter 01 R3 and R15). No row was added to chapter 02 §2.15A: every
  state read already had one, and both rows' identifiers were confirmed by
  read-only inspection of the historical record's reference metadata before
  they were named. The same pass edited no other chapter and no existing
  sentence of this one.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-15, measure
  0.02 on the discretionary-expression basis of chapter 01 R35 as R39
  borrows it, no findings. Reference states (chapter 02 R13): no finding;
  the names used are `v2.6.0-beta`, `v3.0.0-beta` and the baseline anchor
  (R1), each a row of §2.15A and each the state actually compared. Extent:
  compared against the rows `v2.6.0-beta` and `v3.0.0-beta`, and against the
  baseline anchor row for the facts marked as baseline; subject areas: the
  QTUM-family Electrum request surface; the QRC20 token history collector;
  the QRC20 token and QTUM coin types, including token broadcast, token
  withdraw and delegation; the Electrum client's request routing, timing
  values and connection pool; the legacy activation and deactivation
  requests; the coin registry; the balance-stream activation; the shared
  UTXO listing, build-lock and spent-output record paths; the
  background-task mechanism. Quantities: mechanical comparison of 51 files
  at each release row and 24 files at the baseline anchor row; reading per
  release row of 10 files in full or in the parts that bear on the subject,
  2 further files through the difference between the two rows only, 4
  further files by targeted search; 5 files by targeted reading at the
  baseline anchor row. Method: reading; per-token forced-versus-free
  classification of every backticked token plus a scan for identifier-shaped
  tokens outside backticks; a mechanical three- and four-word sequence
  overlap; the additions embed no code blocks, so the repository's
  clean-room gate tool was not applicable. Limit: the verdict covers the two
  added ranges only and clears expression and reference states, not factual
  accuracy.
- *Reloaded-original clarifications (2026-11-15), clean side -- after R38.6.4h,
  after §38.6.5 decision 2 and item 8, after R38.5B.1, a status note after the
  delegation finding under R38.6.4Q.7, and two §38.10 references.* Written by a
  session that never read the forbidden corpus. The inputs are this repository's
  code, the cleared §38.6.4Q (its notes 1, 3 and 6 on existing text), PR #354,
  and the two public sources now listed in §38.10. It takes no forbidden-class
  input, so chapter 01 R38 is not engaged and no gate pass is required.
- *Reloaded-original amendment (2026-11-15), clean side -- after the as-built notes
  of R38.6.4a: the QTUM family's unspent lists are not spread, and they and its
  broadcasts take the listed order (interim until D38.6.4Q.1 and D38.6.4Q.4).* Written by a session that never read the forbidden corpus. The
  inputs are this repository's code and the cleared §38.6.4Q (R38.6.4Q.1,
  R38.6.4Q.7 and its note 2 on existing text). It takes no forbidden-class input, so chapter 01 R38
  is not engaged and no gate pass is required.
- *Reloaded-original amendment (2026-11-15), clean side -- under R38.6.4o item 4
  and a status note after D38.6.4o.7: a stuck connection is judged from another
  connection's first report of a higher height, with the period of the larger of
  2 × `avg_blocktime` and 180 s.* Written by a session that never read the
  forbidden corpus, on a maintainer decision. The inputs are this repository's
  code and this project's own field-run counts. It takes no forbidden-class
  input, so chapter 01 R38 is not engaged and no gate pass is required.
- *Scoped addition (2026-11-16, Spec Reader round 0.5.0-9) -- §38.5C
  (R38.5C.1 -- R38.5C.5, the code-quality finding, the compatibility note,
  D38.5C.1 -- D38.5C.2, T38.5C.1 -- T38.5C.10 and the not-established
  paragraph); no other text of this chapter was changed.* **KDF Dirty Gate:
  PASS on 2026-11-16, measure 0.04, no findings** (chapter 01 R40; the
  attestation closes this item). *Inputs:* the forbidden
  corpus at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A,
  read read-only and consulted **only** to establish: for the legacy UTXO
  history collector, which stored rows a pass removes when the backend's
  listing no longer contains their hashes, at which point of the pass,
  whether anything else stored is removed with a row, what a failed listing
  changes, and which stored values a listing does not change; for the
  collector of the versioned method that standard UTXO coins, Qtum, BCH and
  SLP tokens share, which addresses a pass covers, which stored rows a pass
  looks at, what becomes of an unconfirmed row that is listed, listed without
  a height or not listed, which tables a removal covers, what becomes of a
  confirmed row that is no longer listed or is listed at another height, and
  in which address form a token row is indexed and stored rows are selected;
  for both stores, whether any routed method, activation member or start-up
  step clears or rebuilds one coin's stored history, where the legacy
  history file and its marker file are kept and how they are named, and the
  name endings of the store's three tables; and the two rows' agreement (the
  legacy collector, the versioned collector, the history store and both
  method tables are the same at both rows). Behavior, public wire names and
  persisted names only; no private identifier, function body, helper
  decomposition, query text, error, log, comment or test text was
  reproduced. File, directory and table names are those of the persisted
  store; the method name `blockchain.scripthash.get_history` and the meaning
  of its heights are those of the public Electrum protocol documentation.
  The compatibility note and the last sentence of the finding rest on this
  repository's tree at the base of this round (the dev branch as of
  2026-11-16) and on the baseline; D38.5C.1 -- D38.5C.2 are
  Reloaded-original. No row was added to chapter 02 §2.15A: both states read
  already had one, and each row's identifier was confirmed, by read-only
  inspection of the historical record's reference metadata, to be the commit
  its release tag names.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-16, measure
  0.04, no findings. Reference states (chapter 02 R13): PASS; the rows
  `v2.6.0-beta` and `v3.0.0-beta`, and "the baseline" refers to the row
  baseline anchor (R1). Extent: the legacy UTXO history collector, the
  collector of the versioned history method shared by standard UTXO coins,
  Qtum, BCH and SLP, the SQLite history store with its schema, the history
  file and marker naming, both method tables, the reference's history-store
  tests, and one statement checked at the row baseline anchor (R1); 8 source
  files read in the relevant parts at `v3.0.0-beta`, with row agreement
  confirmed by mechanical difference; mechanical comparison over 29 files at
  `v3.0.0-beta` and 28 at `v2.6.0-beta`. Method: reading, plus mechanical
  comparison (5-word overlap of the gated lines against comment and literal
  wording; identifier-shaped tokens and identifier-derived phrases against
  the identifier set); the additions embed no code blocks.
- *Measured note (2026-11-16), clean side -- before "Not established" in
  §38.6.4Q: this repository's behavior for rows T38.6.4Q.2 to T38.6.4Q.6, from
  offline tests against the stub Electrum server.* Written by a session that
  never read the forbidden corpus. The input is this repository's code and its
  tests. It takes no forbidden-class input, so chapter 01 R38 is not engaged
  and no gate pass is required.
- *Measured note, second part (2026-11-17), clean side -- before "Not
  established" in §38.6.4Q: this repository's behavior for rows T38.6.4Q.1
  and T38.6.4Q.7 to T38.6.4Q.11, from offline tests against the stub Electrum
  server.* Written by a session that never read the forbidden corpus. The input
  is this repository's code and its tests. It takes no forbidden-class input,
  so chapter 01 R38 is not engaged and no gate pass is required.
- *Maintainer decisions (2026-11-16), clean side -- status notes after D38.5C.1
  and D38.5C.2: removal of unlisted rows after two listings, and changed
  heights taken over.* Written by a session that never read the forbidden
  corpus, from the maintainer's decisions and this repository's code. It takes
  no forbidden-class input, so chapter 01 R38 is not engaged and no gate pass
  is required.
- *Reloaded-original amendment (2026-11-18), clean side -- after the status
  note on D38.5C.2: how the client sees that its chain changed (a view of base
  headers, one directed header request per new tip, a chain-changed event to
  the history collectors, one WARN line).* Written by a session that never
  read the forbidden corpus, from the maintainer's decision of 2026-11-16 and
  this repository's code. It takes no forbidden-class input, so chapter 01 R38
  is not engaged and no gate pass is required.
- *Scoped addition (2026-11-18, Spec Reader round 0.5.0-12) -- §38.6.10 (R38.6.10.1 -- R38.6.10.6, the code-quality finding and
  T38.6.10.1 -- T38.6.10.4) and the status note at the head of this
  chapter; cleared together with chapter 37 §37.5.11. No other text of this
  chapter was changed.*
  **KDF Dirty Gate: PASS on 2026-11-18, measure 0.03, no findings** (chapter
  01 R40; the attestation closes this item). *Inputs:* as the clearance
  block of the section states them.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-18, measure
  0.03, no leak findings. *Extent:* the UTXO header reads for every family,
  the transaction reader and writer including the special-transaction
  payload, the choice of lock-time and sequence for claims and refunds, the
  median-time query, refund readiness and the errors of the version-two
  history, at the rows `v2.6.0-beta`, `v3.0.0-beta`, `main (as of
  2026-10-07)` and `add-krgn-chain-variant (as of 2026-04-18)`.
- *Maintainer decisions (2026-11-16 to 2026-11-18), clean side -- status notes
  after D38.6.4Q.2, D38.6.4Q.3 and D38.6.4Q.5 and after the code-quality
  finding of R38.6.4Q.6, and the Reloaded-original amendment after it (the
  history collector's end state, with its counting).* Written by a session
  that never read the forbidden corpus, from the maintainer's decisions, this
  project's own probes of public servers and this repository's code. It takes
  no forbidden-class input, so chapter 01 R38 is not engaged and no gate pass
  is required.
- *Maintainer decisions (2026-11-15 and 2026-11-18), clean side -- status
  note after D38.6.4Q.1 (one Electrum client for the QTUM family: when
  instances share, lifetime, reuse, lock order, naming, and the
  subscriptions of a disabled instance).* Written by a session that never
  read the forbidden corpus, from the maintainer's decisions and this
  repository's code. It takes no forbidden-class input, so chapter 01 R38 is
  not engaged and no gate pass is required.
- *Status note added 2026-11-18 -- after R38.6.6e.* Reloaded-original, clean
  side: it records the maintainer's decisions of 2026-11-18 on the ZIP 317 fee
  for ZEC and its minimum trading volume, written from this repository's code
  and the public ZIP 317; no forbidden-class input, chapter 01 R38 not engaged.
- *Maintainer decision (2026-11-20), clean side -- status note after
  R38.5B.4 (no maturity fetch for an output the record supplied, for every
  UTXO coin).* Written by a session that never read the forbidden corpus,
  from the maintainer's decision and this repository's code. It takes no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required.
- *Maintainer decisions (2026-11-15 and 2026-11-19), clean side -- status
  note after D38.6.4Q.4 (one recently-spent record per platform and address
  for the QTUM family: key, lifetime, what is entered, ordering, and lock
  order).* Written by a session that never read the forbidden corpus, from
  the maintainer's decisions and this repository's code. It takes no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required.
- *Maintainer decision (2026-11-21), clean side -- status notes after
  R38.6.4o item 6, after the first known limitation of the R38.6.4o as-built
  note and after D38.6.4o.3, and T38.6.4o.23 and T38.6.4o.24 (a swap's
  confirmation wait chooses again when its server turns out of date or
  answers errors three times in a row).* Written by a session that never read
  the forbidden corpus, from the maintainer's decision, a field report, this
  project's own reads of public LCC Electrum servers and this repository's
  code. It takes no forbidden-class input, so chapter 01 R38 is not engaged
  and no gate pass is required.
- *Maintainer decision (2026-11-21), clean side -- status note after the
  code-quality finding under R38.6.10.5 (the Dash-layout special-transaction
  reader as built).* Written by a session that never read the forbidden
  corpus, from the maintainer's decision, the cleared section, the published
  Dash special-transaction format, public DASH and Kerrigan chain data and
  this repository's code. It takes no forbidden-class input, so chapter 01
  R38 is not engaged and no gate pass is required.
- *Maintainer decision (2026-11-21), clean side -- status note of
  2026-11-22 after the D38.6.4Q.1 status note (INFO lines for the creation,
  reuse and release of a QTUM-family Electrum client).* Written by a session
  that never read the forbidden corpus, from the maintainer's decision and
  this repository's code. It takes no forbidden-class input, so chapter 01
  R38 is not engaged and no gate pass is required.
- *Defect fix (2026-11-22), clean side -- status note after the D38.6.4Q.1
  status note (subscriptions of a shared client: a removal while a request
  waits, a removed instance's later request, placement over the current
  session, one request at a time).* Written by a session that never read the
  forbidden corpus, from this repository's code and its tests. It takes no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required.
- *Pointer added 2026-11-22, KDF Spec Reader, round 0.5.0-23 -- the pointer
  at the end of §38.6.4Q.* It names chapter 51 §51.6E (R88, R89, R90) and
  summarizes what those rules record; its reference content is that of its
  target and is judged with it. **KDF Dirty Gate: PASS on 2026-11-22, measure
  0.03, no findings**, as recorded in chapter 51 §51.16.
- *Maintainer decision (2026-11-22), clean side -- status note after R38.8.5
  (the page total of `account_balance` is a ticker-keyed map; the empty forms
  keep this rule's ticker entry).* Written by a session that never read the
  forbidden corpus, from the maintainer's decision, the cleared text of
  chapter 49 §49.2.2 and this repository's code. It takes no forbidden-class
  input, so chapter 01 R38 is not engaged and no gate pass is required.
- *Defect fix (2026-11-23), Reloaded-original, clean side -- status note
  after the status note of 2026-11-22 on the subscriptions of a shared client
  (the placing again leaves a subscription placed over the current session
  alone; a failed first placement keeps an instance whose subscription was
  placed over the current session meanwhile).* Written by a session that
  never read the forbidden corpus, from this repository's code and its tests.
  It takes no forbidden-class input, so chapter 01 R38 is not engaged and no
  gate pass is required.
- *Clean-side addition of 2026-11-23 (PR #522):* the status note before
  §38.6.4Q on the swap evidence of ch.51 R100 for UTXO coins, BCH and QTUM over
  Electrum. Reloaded-original; written from this repository's code and tests;
  no reference input and no forbidden-class input; no sentence of the chapter
  is changed.
- *Clean-side addition of 2026-11-28 (PR #553):* the status note after R38.6.4o item 9
  on logging the behind and ahead lines only once the state has lasted 5
  seconds. Reloaded-original; it records the maintainer's decision of
  2026-11-28; no reference input and no forbidden-class input; no sentence of
  the chapter is changed.
