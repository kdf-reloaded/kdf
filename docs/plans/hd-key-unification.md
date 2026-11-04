# Plan: one key per coin and one trading identity under an HD login

**Status:** planning — **Last reviewed:** 2026-11-04

> **Context.** Under an HD login (`enable_hd: true`) the node holds three different secp256k1
> keys, where an iguana login holds one:
> - the **passphrase-hash key**: the hash of the mnemonic text, the key the swap and ordermatch
>   code reads from the node context (`mm2src/crypto/src/crypto_ctx.rs`, marked
>   `DEFERRED(hd-dispatch)`);
> - the **internal key** at `m/44'/141'/2147483647/0/0`: the `get_public_key` answer, and the
>   key of every coin enabled through legacy `enable`/`electrum`;
> - each coin's **HD key**: the key at `path_to_address` (UTXO) or the signer (EVM).
>
> Swaps and ordermatch mix these keys. An iguana login is not affected, because there the
> three are one key. The maintainer decided on 2026-11-04 (Decision BL) to fix all of it in
> batch 2 of 0.4.0, with the refusal of the swaps that cannot work (H1) as an ordinary batch
> item. All of the work is Reloaded-original. Where a step should match GLEEC KDF, the open
> behavior questions are listed under open points; wording that depends on them stays
> gate-pending until the Spec Reader's answers have passed the KDF Dirty Gate.

## What a user sees today (HD login only)

1. **ETH and ERC20 swaps cannot succeed.** The swap puts the passphrase-hash key on the wire but
   pays from the coin's HD signer. The other side's sender check fails. The dex fee is lost, and
   a payment already sent waits for its refund time. Nothing refuses these swaps up front.
2. **Swap proceeds on an address no view shows.** A UTXO coin enabled through legacy `enable`
   under an HD login receives its spend or refund on the passphrase-hash address. The funds are
   recoverable by logging in with the same passphrase without `enable_hd`.
3. **A maker's orders may vanish from other nodes.** Orders are signed with the passphrase-hash
   key, but the keep-alive goes out for the internal key, and `orderbook` marks own orders by the
   internal key. Not yet observed at run time (row HD1).
4. **A UTXO coin activated at another `path_to_address`** puts that address's key on the wire
   but funds the swap from account 0, external chain, index 0.
5. **Funds shown but not spendable:** Sia (`withdraw` ignores `from`) and ERC20 (`from`
   refused) show balances on HD addresses they cannot spend from.
6. **Single-address views:** balance events and history cover only the activated address
   (UTXO, EVM, Sia). Legacy and mmrpc 2.0 UTXO `withdraw`, legacy `my_balance` and
   `get_enabled_coins` are refused.
7. **Tendermint and BCH-with-tokens** run on the passphrase-hash key, not on an HD derivation.

Who logs in with HD: the Flutter Komodo DeFi wallet (supported since 0.1.x) and API users who
set `enable_hd`.

## Work packages

| Id | What | CRD | State |
|---|---|---|---|
| H0 | **Docs agree with each other and with the code.** Six contradictions: the HD withdraw without `from` (ch.49 against RELOADED_VS_GLEEC and the CHANGELOG), the Trezor EVM activation status (ch.50 against ch.35), the §38.8 status lines, the Sia result shape (ch.20 against ch.46), the HD swap fix credited to ch.51, and the unverified parity claim for the swap fallback. | ch.49, ch.50/ch.35, ch.38, ch.20, ch.51, RELOADED_VS_GLEEC | Docs PR #228 |
| H1 | **Refuse what cannot work, before negotiation.** A V1 swap leg is refused when the key it would put on the wire is not the key the coin pays from. Under an HD login that covers ETH/ERC20, coins enabled with a single key (legacy `enable`/`electrum`, `priv_key_policy: IguanaPrivKey`), and UTXO coins at a `path_to_address` other than 0/0/0. Checked at order posting, `trade_preimage` and matching; one fixed error text names the coin, the reason and the way out; an own order that fails is cancelled. The `MM2.json` switch `swap_key_check` (default `true`) turns it off. An iguana login never meets it. | ch.05 §5.12B (R36–R40, T15–T18); the reference has no such check (ch.05 §5.9B R34) | Spec PR #231; code PR #233 |
| H2 | **Lab confirmation of the ordermatch effect** (row HD1). | — | Batch 2, with H1 |
| H3 | **One swap key per coin.** The V1 key on the wire is the coin's own key for every family: the EVM signer's key; a UTXO coin funds from the address of its HTLC key; a single-key coin under HD uses its own key. Lifts the H1 refusals one by one. | ch.05, ch.51 (V1), ch.15 R36 (V2 unchanged) | Batch 2, after H1; gate-pending (Q1, Q4) |
| H4 | **One trading identity under HD.** One pubkey for order signing, keep-alive, `is_mine`, the stale-order sweep, `sender_pubkey` and `my_persistent_pub`. Wire-visible at upgrade (see below). | ch.32, ch.59, ch.05 | With H3, in one batch; gate-pending (Q1, Q2) |
| H5 | **UTXO operations outside swaps.** Legacy and mmrpc 2.0 `withdraw` use the HD default sender; `my_address()` answers the enabled address so legacy `my_balance` and `get_enabled_coins` work; the `task::withdraw` default follows `path_to_address`; `fetch_utxos` and `consolidate_utxos` under HD; `task::create_new_account` works under a software HD login (today it asks for a hardware device first and fails, for UTXO, QTUM and EVM alike). | ch.38 §38.8, ch.49, ch.20 §20.10 D1 note | After H3/H4; gate-pending (Q5) |
| H6 | **Every HD address covered.** Balance events and history for every known HD address (UTXO/QTUM, EVM, Sia), the history `target` selector, `withdraw` `from` for ERC20 and Sia. The history cache is rebuilt, not migrated. | ch.38, ch.35, ch.53 D53.5, ch.49 | Independent; gate-pending (Q6, Q9) |
| H7 | **Family activations.** Tendermint on an HD derivation (`path_to_address`, `priv_key_policy`; Tendermint activation plan items 2 and 5); BCH-with-tokens on the same key as `enable_bch::init` or HD; legacy `enable` under HD refused or bound to HD. | ch.36, ch.55, UPSTREAM-PARITY-GAPS (legacy EVM `enable`) | Independent; gate-pending (Q3, Q8) |

## Rules that hold across the steps

- **A swap keeps the key its Started event recorded.** No step changes the key of a swap that
  has already started. A resumed V1 swap restores its saved key pair; for a coin that saves none
  (EVM, single-key coins), the key recorded on the wire at the start is the one it keeps. H3
  and H4 ship with a test that resumes a swap saved by the previous build.
- **No database migration.** `my_swaps` holds no keys; a saved swap keeps what it recorded;
  `stats_swaps` rows keep the pubkey they were written with.
- **An iguana login does not change.** Every step is a no-op when the three keys are one key,
  and each PR proves that with a test on an iguana login.

## The trading-identity change at upgrade (H4)

After H4, an HD maker's orders and swap messages carry a different pubkey than before. Other
nodes see a new maker. What the operator does:

- cancel open orders before the upgrade, or let the node re-post them after it (the orders
  restored from the database are re-signed under the new identity; the old ones expire on other
  nodes at the maker-order timeout);
- swaps in flight finish under the identity they started with;
- swap statistics show both pubkeys for one node across the upgrade.

The CHANGELOG carries this note for the release that ships H4.

## SwapLab rows

| Row | Step | What | Duration |
|---|---|---|---|
| HD1 | H2 | One HD-login maker (RIN, `task::enable_utxo`, default path) and one iguana peer. Watch the peer's `orderbook` for the maker's order, the maker's own `orderbook` `is_mine`, and the keep-alives per pubkey on both nodes. | ~1 h |
| HD2 | H1 | HD login: `setprice`, `buy`, `sell` and `trade_preimage` for ETH (or an ERC20) and for RIN enabled through legacy `electrum` return the fixed error; RIN through `task::enable_utxo` at the default path trades normally. | ~30 min |
| HD3 | H3 | HD-login swaps RIN/ETH both directions against an iguana peer; spend and refund outputs on HD addresses the wallet shows. | ~2 h |
| HD4 | H3/H4 | A swap started by the previous build under an HD login, resumed by the new build, completes on its recorded keys. | ~1 h |
| HD5 | H4 | HD maker with open orders across the upgrade: orders re-posted under the new identity, `is_mine` true, the old orders gone from a peer within the maker-order timeout. | ~1 h |

## Open points

Behavior questions for the Spec Reader (dispatched 2026-11-04; answers reach this plan only after
the KDF Dirty Gate):

- **Q1.** Which key the reference puts on the V1 swap wire, and as the persistent pubkey, under an
  HD login, per activation route.
- **Q2.** Which key signs orders and keep-alives under HD, and which pubkey marks an order as the
  node's own.
- **Q3.** Whether legacy `enable`/`electrum` is accepted under HD, and with which key.
- **Q4.** Which address funds, receives change from, and receives the spend and refund of a UTXO
  V1 swap with a non-default `path_to_address`.
- **Q5.** Legacy and mmrpc 2.0 UTXO `withdraw`, legacy `my_balance` and `get_enabled_coins` under
  HD.
- **Q6.** Whether history covers every HD address, and how `target` is handled.
- **Q7.** Whether the reference refuses a swap up front when the keys do not match.
- **Q8.** Which key `enable_bch_with_tokens` and `enable_tendermint_with_assets` use under HD.
- **Q9.** Whether Sia `withdraw` honors `from` under HD.
