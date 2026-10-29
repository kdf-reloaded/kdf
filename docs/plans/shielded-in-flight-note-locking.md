# Plan: shielded in-flight note locking (ARRR taker swaps)

**Status:** in progress (swap and `withdraw` paths shipped; T39.8.0h covered by the harness 2026-10-25; open: two-daemon detection, F3) — **Last reviewed:** 2026-10-29

> **Postponed until after the beta, 2026-10-25.** The two open items, detection of two daemons on one
> seed with separate wallet databases and F3 (the post-broadcast sync trigger, latency only), are not
> worked on before the beta. A documented limitation is enough for the beta: the two-daemon case is
> listed under *Known limitations* in `CHANGELOG.md` (run one instance per seed), and F3 changes
> latency, not correctness, because the lock is recorded in the wallet database. Nothing here is marked
> done by this note.
>
> **Wait budget aligned with the reference, 2026-10-26.** A send that must wait for an earlier
> unmined send now waits up to 600 seconds in total with 15-second retries (was 300 and 10), matching
> the reference's budget. Only the two `SendPacing` defaults and the test that pins them changed; the
> rule that a send waits only while this process's in-flight total could cover the shortfall is
> unchanged. Provenance: Spec Reader triage 2026-10-26.
>
> **Added to the two-daemon item, 2026-10-26.** The change of an own unmined send now shows as
> unspendable balance from the wallet database itself (CRD ch.39 R39.8.0ag1). The change of a send made
> by *another* instance on the same seed still does not show until mined. The mempool poll decrypts
> with the external viewing key only, and change goes to the internal (change) address. Closing that
> means also trial-decrypting mempool outputs with the internal viewing key, de-duplicated against
> recorded notes by transaction and output. It belongs with the two-daemon item and is postponed with
> it. No code yet.
>
> **T39.8.0h harness built, 2026-10-25.** A test harness now drives a real Light-mode `ZCoin` over
> a real shielded wallet database in a temporary directory, with the lightwalletd backend replaced by
> the scripted transport the lightwalletd tests already had (`mm2src/coins/z_coin/z_note_lock_tests.rs`).
> A "broadcast" is whatever the script answers, a "scan" is a compact block built from the real
> transaction and fed to the real scanner, a "restart" is a new coin over the same database directory
> with an empty in-memory in-flight map, and expiry is reached by advancing the scanned tip, since the
> release is height-driven. Every T39.8.0h case named below is now covered on the send path: the record
> is written only after the backend accepts; a rejected broadcast records nothing, leaves the balance
> unchanged and the retry reuses the note; a second send is refused the committed note and ends in the
> typed bounded-wait failure while `withdraw` names the cause; the exclusion survives a restart and the
> explanation does not (R39.8.0as with D39.8.0e, both pinned); scanning the containing block frees the
> change and retires the spent note with no double count; a never-mined transaction releases its note
> at expiry and not a block earlier; two concurrent sends over two notes select disjoint notes and both
> broadcast; a waiting send re-selects when a concurrent scan lands. The one production change is a
> `SendPacing` field on the coin, installed by the builder with the former literals (300 s in-flight
> wait retried every 10 s, 4000 s confirmation wait polled every 10 s) and pinned by a test, so the
> bounded wait can be exercised in milliseconds; production behavior is unchanged. Still open:
> detection of two daemons on one seed with separate wallet databases, and F3. The tests make real
> Sapling proofs and need the parameter files; they say so and return where those are absent. The
> proofs are deliberate, not an oversight: the failure this plan exists for is a note selected twice,
> and only a real selection, a real spend and a real scan can show it, so the roughly three minutes
> they add to the shielded test job are the price of the coverage. Do not replace them with mocks.
>
> **Shortfall named, 2026-10-21.** A withdrawal short only because its notes are committed to a
> recorded, unconfirmed transaction now fails with `BalanceLockedByUnconfirmedTransaction`
> (HTTP 400; coin, available, required, committed) instead of a bare `NotSufficientBalance`
> showing `available: 0` beside a healthy balance. The check lives in `ZCoin::gen_tx`
> (`mm2src/coins/z_coin/z_coin_ops.rs`, `explain_shortfall`), after the in-flight map is
> pruned; `send_outputs` is untouched and keeps waiting out the same shortfall. Locking
> behaviour is unchanged. **Limit:** the committed amount comes from the per-process in-memory
> map, so after a restart the explanation is gone and the bare `NotSufficientBalance` returns,
> although the exclusion itself (the wallet database record) survives. Recorded in CRD ch.39
> D39.8.0e and `docs/GLEEC_COMPATIBILITY.md`.
>
> **Withdraw path closed, 2026-10-19.** A user sent 0.01 ARRR to themselves, never saw it in the
> history, and the next send failed with `bad-txns-duplicate-nullifier-requirements-not-met`:
> `withdraw` built a transaction and recorded nothing, and the client's `send_raw_transaction`
> went through generic UTXO code that knows nothing of shielded notes, so the next withdrawal
> re-selected the same note. The fix is in `ZCoin::send_raw_tx_bytes` (`mm2src/coins/z_coin.rs`,
> with `record_broadcast_in_wallet_db` in `z_coin_ops.rs`): after the backend accepts the bytes,
> the transaction is read back from them and recorded exactly as the swap send path records
> its own (CRD ch.39 R39.8.0ap, D39.8.0e). The hook is on `ZCoin` rather than in `utxo_common`
> because that code is shared by every UTXO coin. `withdraw` still builds and returns and does
> not broadcast; its request and response are unchanged, so the GLEEC two-step contract holds.
> A build that is never submitted records nothing, so a cancelled send strands no notes. The fee
> recorded is derived from the bytes (value leaving the Sapling pool less the transparent outputs;
> a transaction with transparent inputs or a non-Sapling bundle is not recorded). `withdraw` with
> `max` no longer computes a negative amount when the spendable balance does not cover the fee.
> The in-memory `in_flight_spends` map now also drops entries whose expiry has passed. ARRR is
> lightwalletd-only, so there is no Native-mode path to cover.
>
> **Still to do for `done`:** the T39.8.0h cases for a rejected broadcast, a restart, the
> mined-and-scanned transition and two concurrent sends (CRD ch.39 D39.8.0f) -- *since covered, see
> above*; detection of two
> daemons on one seed with separate wallet databases (this fix does not make that safe; a
> possible cheap detection is noticing a nullifier on chain that this wallet did not create);
> F3 (post-broadcast sync trigger, latency only). Deferred, not planned: showing built-but-unsent
> transactions to the user (D39.8.0g).
>
> **Re-checked against the tree 2026-10-18.** Selection, construction, broadcast and recording
> run as one critical section in `ZCoin::send_outputs`
> (`mm2src/coins/z_coin/z_coin_ops.rs`), and both v1 ARRR swap sends — the dex fee and the
> HTLC payment (`mm2src/coins/z_coin/z_htlc.rs`) — go through it, so a taker fee followed by a
> taker payment can no longer pick the same note. The residual gap is the one named below:
> `withdraw` builds through `gen_tx` and the caller broadcasts, so a withdrawal is not
> recorded (CRD ch.39 D39.8.0e) -- *since fixed, see above* -- and T39.8.0h's
> broadcast/restart/concurrency cases are still uncovered (D39.8.0f) -- *since covered by the
> harness of 2026-10-25, see above*.
>
> **Context:** implemented 2026-09-22; specification is now CRD ch.39 §39.8.0.7
> (R39.8.0ao–R39.8.0at, T39.8.0h), which supersedes the design sketch below.
>
> Created 2026-09-22 after a question about whether reloaded has an equivalent
> of the GLEEC KDF feature that keeps ARRR taker swaps from failing when the
> dex-fee transaction is still in flight. It did not. This document records
> what was verified in this repository, what the real failure window turned out
> to be (not the one originally suspected), and the shape of the fix.
>
> **Progress (2026-09-22).** The chapter was authored by KDF Spec Reader and
> passed KDF Dirty Gate (ch.39 score 0.03; ch.52, amended for a cross-chapter
> inconsistency, score 0.02). Implemented since: the send path records each
> broadcast in the shielded wallet database (F1), performs selection through
> recording under one exclusive section with the waits outside it, and fixes the
> confirmation-deadline unit defect (F4). F2's separate in-memory exclude list
> was **dropped**: widening the section achieves the same guarantee without a
> second mechanism, and an in-memory map is retained only to tell a transient
> shortfall from an absolute one, never as an exclusion. F3's post-broadcast sync
> trigger remains unimplemented and is now latency-only, since correctness no
> longer depends on scan freshness.
>
> Two corrections to the analysis below, both found while implementing and left
> in place so the reasoning stays auditable: `set_transaction_status` does **not**
> release a note lock in this dependency generation (the exclusion predicate
> ignores the column it writes), so no revocable unlock exists; and the wallet
> database's "transaction is scanned" predicate tested only for row existence,
> which our own recording would have satisfied immediately — that predicate was
> corrected to require a mined height, with a regression test.
>
> Known gaps (updated 2026-10-29): none in test coverage. Every case T39.8.0h names is
> covered for the Light-mode send path: the withdrawal-side cases and expiry release by
> tests over a real shielded wallet database (a scanned note, a real spend proof, a recorded
> transaction), and the broadcast-rejection, restart, scan-transition and concurrent-send
> cases by the harness of 2026-10-25 (CRD ch.39 D39.8.0f, closed). The open items are the two
> named in the Status line, both after the beta (CRD ch.39 D39.8.0j). The testing section
> below states the cases as they were specified.

## The protocol constraint this plan lives under

A Sapling note can only be spent by proving a Merkle path from its note
commitment to an anchor that consensus recognises — a note-commitment-tree root
from an already-mined block. A note created by a transaction that is still in
the mempool has no position in that tree, so **no valid spend can be
constructed for it**. This is not a wallet limitation and no implementation can
remove it.

Two consequences shape everything below:

1. "Make unconfirmed change spendable" is not an available option. The only
   thing a wallet can do is stop *mis-selecting* the notes it has already
   committed to an in-flight transaction, and spend a different confirmed note
   instead.
2. ARRR is shielded-only, so there is no transparent-change escape hatch of the
   kind other UTXO coins fall back on.

## Current state

Verified against this repository, not assumed. Two earlier assumptions were
wrong and are corrected here explicitly.

**Note selection passes an empty exclude list.** The light/Electrum path
(`gen_tx_from_shielded_wallet_db`, `mm2src/coins/z_coin/z_coin_ops.rs`) calls
`select_spendable_notes(account_id, …, ConfirmationsPolicy::MIN, &[])` — the
final argument, the set of notes to exclude from selection, is a literal `&[]`.

**Nothing records a broadcast transaction into the wallet database.** A grep
over `mm2src/` for `store_sent_tx`, `store_decrypted_tx`,
`store_transactions_to_be_sent`, `create_proposed_transactions`,
`decrypt_and_store_transaction` and `put_tx_data` returns no production call
site. The single raw `INSERT INTO sapling_received_note_spends` in the tree
(`mm2src/coins/z_coin/z_coin_wallet_db.rs`) is inside a `#[test]` fixture.
Broadcast goes out through `send_raw_transaction` with no wallet-DB write, so
the wallet only learns that it spent its own note when the mined block is
scanned.

This matters because upstream already has the machinery. `zcash_client_sqlite`
0.21.1's spendable-note query excludes any note that appears in
`sapling_received_note_spends` with an unmined-but-unexpired spending
transaction. The table and the exclusion SQL exist; reloaded simply never
writes a row.

**Correction 1 — the dex fee *is* waited on.** `send_outputs`
(`z_coin_ops.rs`) does not return after broadcasting. It calls
`wait_for_confirmations(…, confirmations = 1, …)`, and both `z_send_dex_fee`
and `z_send_htlc` (`mm2src/coins/z_coin/z_htlc.rs`) go through it. So the
taker-fee transaction is already **mined** before the swap advances toward the
taker payment. The originally suspected failure — taker payment built against a
still-in-mempool fee — is not reachable through the swap path.

**Correction 2 — the real window is scan lag, not confirmation lag.** Being
mined is not enough for the light-mode wallet; the block must also be *scanned*
before the spend is recorded and the change becomes visible. Three things
combine:

- `post_activation_shielded_sync` (`mm2src/coins/z_coin.rs`) runs on a
  `SHIELDED_SYNC_POLL_PERIOD_SECS = 30.` timer, so a freshly mined block waits
  up to one poll period plus fetch-and-scan time before it lands in the wallet
  database.
- In light mode `sapling_state_synced` is set to `true` and the legacy
  commitment-tree loop returns immediately (`z_coin.rs`), so `gen_tx`'s
  `while !self.is_sapling_state_synced()` gate is a no-op for exactly the mode
  that needs it.
- `gen_tx_from_shielded_wallet_db` gates on `shielded_wallet_db_scan_complete()`,
  which records that the **last scan attempt succeeded** — not that the scan has
  reached the chain tip, and not that it covers the transaction we just sent.

So between "our transaction was mined" and "our transaction was scanned" the
wallet database still shows the spent input note as unspent and eligible, and
the exclude list is empty. A second ARRR send in that window re-selects the same
note, producing a transaction with a duplicate nullifier that the network
rejects.

**Reachability.** The 1-confirmation wait does not protect against this, because
what the selection query depends on is the scan, not the confirmation. Any two
ARRR spends separated by less than the scan lag can collide:

- taker fee followed by taker payment, when the maker coin's confirmation wait
  is shorter than the remaining scan lag (a maker payment that is already
  confirmed on arrival makes that wait near-zero);
- two concurrent swaps using ARRR as the taker coin;
- a withdrawal issued while a swap is in progress.

`z_unspent_mutex` does not help: it is held only for the duration of a single
`gen_tx` call, and the hazard spans broadcast to scan.

**Balance is inconsistent in the same window.** In light mode `my_balance`
(`z_coin.rs`) reports `spendable` from the scanned wallet database and
`unspendable` from mempool-derived pending receipts. `refresh_pending_receipts`
correctly skips transactions already scanned, so a mined-and-scanned
transaction is not double-counted. But nothing deducts an input note that has
been spent by a transaction that is not yet scanned. While our own send sits in
the mempool, the input note is still counted in `spendable` *and* its change is
counted in `unspendable` — the same value, reported twice. Because swap balance
checks read `my_spendable_balance()`, this over-reports funds that cannot be
spent.

**A latent unit defect, found while verifying the above.** `send_outputs`
passes `wait_until: now_ms() + 4000`. `wait_for_confirmations`
(`mm2src/coins/utxo/rpc_clients.rs`) compares it as `now_ms() / 1000 >
wait_until`, i.e. `wait_until` is a **seconds** timestamp; every other call site
in the tree builds it as `now_ms() / 1000 + N`. Passing milliseconds into a
seconds field yields a deadline roughly fifty thousand years out, so the
timeout never fires. The apparent intent was a 4000-second (~66 minute) cap. A
transaction that stays in the mempool without ever being mined or dropped
therefore blocks the swap thread indefinitely instead of failing. This is
independent of the note-locking work and is separately shippable.

## Goal

Stop reloaded from selecting a shielded note it has already committed to a
broadcast transaction, and make the light-mode balance consistent while such a
transaction is in flight. No change to swap protocol, wire format, or
netid-`8762`/`6133` observable behaviour.

Non-goal: making unconfirmed change spendable. See the protocol constraint
above.

## Design

### F1 — Record our own broadcast transactions (the fix)

After a successful broadcast, decrypt the transaction we just built with our own
viewing key and store it through `WalletWrite`, so the wallet database records
the transaction as unmined, marks the spent input notes via
`sapling_received_note_spends`, and registers the change output.

This is the correct fix rather than a workaround, because it makes reloaded use
the mechanism upstream already provides:

- `select_spendable_notes` then excludes the committed notes on its own, with no
  new argument and no bespoke bookkeeping;
- the wallet-summary balance query uses the same exclusion clause, so **the
  balance double-count is fixed by the same change** — F1 subsumes a separate
  balance patch;
- the state survives a restart, because it lives in the wallet database;
- when the block is finally scanned, upstream reconciles the already-recorded
  transaction with the mined one. No custom invalidation logic is needed.

It also creates **no GLEEC compatibility divergence**. The rows go into the
existing `<TICKER>_RELOADED_WALLET.db` under the schema upstream already
defines. No new file and no schema change, so nothing here touches the shielded
database isolation recorded in `RELOADED_VS_GLEEC.md`.

**Resolved (2026-09-22).** The entry point is
`WalletWrite::store_transactions_to_be_sent`, whose documentation states it is
for transactions constructed by the wallet and "must be called before the
transactions are sent to the network". `zcash_client_sqlite` 0.21.1's
implementation (`store_transaction_to_be_sent` in `src/wallet.rs`) does exactly
what is needed for an externally-built transaction: it calls `put_tx_data`, then
iterates `sent_tx.tx().sapling_bundle().shielded_spends()` and calls
`mark_sapling_note_spent` per nullifier, which inserts into
`sapling_received_note_spends` by matching `WHERE nf = :nf`. Its own comment
describes the purpose in the same terms this plan does:

> Mark notes as spent. This locks the notes so they aren't selected again by a
> subsequent call to `create_spend_to_address()` before this transaction has been
> mined (at which point the notes get re-marked as spent).

Nothing about the derivation depends on the transaction having come from
upstream's proposal API — only on its Sapling bundle, which ours has. So the
mechanism is present and reloaded simply never calls it.

Two consequences for the implementation:

- `SentTransaction` carries no shielded-spend field (`utxos_spent` is
  transparent-only), so the spends are derived from the bundle and the outputs
  slice may be left empty. Declaring no outputs means the change note is not
  written as a received note, which is harmless: it is unspendable until mined
  either way, it is already surfaced as a pending mempool receipt, and scanning
  records it for real when the block lands.
- `SentTransaction::new` takes a `time::OffsetDateTime`, so `time` has to become
  a direct dependency of `coins`. It is already in the tree via the Zcash stack
  (0.3.47), so this adds no new code to the dependency graph.

### F2 — Populate the exclude argument (interim, and defence in depth)

Keep an in-memory set of note references committed to transactions that have
been broadcast but not yet observed as scanned, and pass it as the `exclude`
argument that is currently `&[]`. Drop entries once
`transaction_is_scanned` reports the transaction.

Weaker than F1 — it is lost across a restart and does not correct the balance —
but it is small, touches one call site, and is worth keeping afterwards as a
guard for the gap between broadcast and the F1 write.

### F3 — Make the freshness gates mean what they claim

- Replace the `shielded_wallet_db_scan_complete()` gate in
  `gen_tx_from_shielded_wallet_db` with one that also requires the scan to have
  reached at least the height of the most recent transaction we broadcast.
- Trigger a sync pass immediately after a broadcast instead of waiting up to a
  full 30-second poll period. The periodic timer stays as the floor, exactly as
  `faster-transaction-visibility.md` argues for its own refresh trigger; this
  should reuse that mechanism rather than add a second one.

F3 shrinks the window; it does not close it. It is worth doing for latency, not
as a correctness substitute for F1.

### F4 — Fix the `wait_until` unit defect

`now_ms() + 4000` → `now_ms() / 1000 + 4000`, matching every other call site.

Note that this is a real behaviour change: today the wait is unbounded, and
afterwards it fails after ~66 minutes. That is the evident original intent and
the safer behaviour, but it turns a hang into an error and should ship on its
own commit so it can be reverted independently.

## Out of scope

- **Native mode.** The daemon tracks its own mempool spends and
  `z_list_unspent(min_conf = 1)` already excludes them; the native path is
  correct as it stands.
- **Migrating `ZCoin` to swap v2.** v2 sends a single funding transaction
  carrying the dex fee, which removes the two-transaction sequence entirely, but
  `ZCoin` implements no v2 trait today and that is a much larger project. Worth
  recording as the eventual structural answer.
- **Changing the swap state machine ordering.** The fix belongs in the coin, not
  the protocol.

## Sequencing

Each stage is independently shippable and independently revertible.

1. **F4** — the unit fix. Unrelated to the rest, smallest, no dependencies.
2. **F2** — the exclude list. Mitigates the failure without touching the
   database, so it can ship while F1 is still being designed.
3. **F1** — sent-transaction recording. The real fix; subsumes the balance
   double-count.
4. **F3** — gate tightening and post-broadcast sync trigger. Latency polish,
   best done once F1 makes correctness independent of it.

## Risks

| Risk | Mitigation |
|---|---|
| `WalletWrite` does not populate spend rows for an externally-built transaction | Resolve before committing to F1; F2 is the fallback and is already useful |
| Recording an unmined transaction that is never mined leaves notes locked | Upstream's exclusion clause already expires by `expiry_height`; confirm the expiry path is exercised by a test rather than assumed |
| Double-count between a recorded transaction and the later scan of its block | Upstream reconciles by txid; assert it with a test that records, then scans the same transaction |
| F4 converts a hang into a swap failure | Ship separately; the hang is not the safer behaviour, but the change should be visible on its own |

## Testing

No live ARRR network is available in the development sandbox, and the
characteristic failure here is silent, so fixture-level tests are the acceptance
gate rather than a formality.

- Recording a built transaction marks its input notes spent and its change
  received; a subsequent `select_spendable_notes` does not return the spent
  note. This is the core regression test.
- Balance during the in-flight window counts the input note in neither
  `spendable` nor twice overall.
- Scanning the block containing an already-recorded transaction produces no
  duplicate note and no balance change.
- An expired unmined transaction releases its notes.
- `exclude` (F2) removes exactly the listed notes and nothing else.
- Native mode behaviour is unchanged.

## Open decisions

0. **Record before or after broadcast.** Upstream's contract says before. The
   draft records *after* a successful broadcast instead, deliberately: recording
   first means a transaction that fails to broadcast still leaves its notes
   locked until `expiry_height` — roughly forty minutes on ARRR — so a user
   retrying an aborted swap would hit insufficient funds caused by our own fix.
   Recording after leaves a window between broadcast and the write, but that
   window is bounded by two statements rather than by the network. If the window
   is judged unacceptable, the clean fix is to widen `z_unspent_mutex` to cover
   build, broadcast and record as one critical section, rather than to move the
   write earlier. This needs a decision before the change ships.
1. Whether F2 ships as a permanent guard or is removed once F1 lands.
2. Whether F3's post-broadcast trigger should reuse the refresh-trigger
   abstraction from `faster-transaction-visibility.md` or stay z_coin-local.
3. Whether a CRD requirement is needed for in-flight note locking, or whether it
   is covered as an implementation detail of existing shielded-balance
   requirements. R39.8.0ag governs how pending amounts are *reported*; it does
   not speak to note selection.

## References

- `RELOADED_VS_GLEEC.md` — shielded database isolation; F1 deliberately stays
  inside it.
- [`faster-transaction-visibility.md`](faster-transaction-visibility.md) —
  pending receipts (workstream B) and the refresh-trigger abstraction F3 should
  reuse.
- [`arrr-ironwood-compatibility.md`](arrr-ironwood-compatibility.md) — notes that
  a later librustzcash generation gains an explicit `LockFilter` on
  `select_spendable_notes`; F2's exclude argument is the current-generation
  equivalent.
- CRD ch.39 §39.8.0.5, §39.8.0.6, R39.8.0ag — shielded background sync and
  pending-receipt reporting.
