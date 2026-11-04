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
> item. The reference target is recorded in CRD ch.05 §5.9B (KDF Dirty Gate PASS 2026-11-04):
> under an HD login GLEEC KDF never uses the passphrase hash; its trading identity is the
> internal key, and every coin's swap key is the coin's own key at `path_to_address`, which also
> funds, signs and receives. Where a step leaves that target, the maintainer decides (decisions
> BY to CC under open points).

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
| H1 | **Refuse what cannot work, before negotiation.** A V1 swap leg is refused when the key it would put on the wire is not the key the coin pays from. Under an HD login that covers ETH/ERC20, coins enabled with a single key (legacy `enable`/`electrum`, `priv_key_policy: IguanaPrivKey`), and UTXO coins at a `path_to_address` other than 0/0/0. Checked at order posting, `trade_preimage` and matching; one fixed error text names the coin, the reason and the way out. An iguana login never meets it. | ch.05, new section; divergence note gate-pending (Q7) | Batch 2, after H0 |
| H2 | **Lab confirmation of the ordermatch effect** (row HD1). | — | Batch 2, with H1 |
| H3 | **One swap key per coin.** The V1 HTLC key is the coin's own key at `path_to_address` for every family: the EVM signer's key; for a UTXO coin the same key funds the payment, receives the change, and is paid the spend and refund, so funding moves from account 0 / External / 0 to `path_to_address`; a shielded coin keeps its per-swap key and Sia its padded ed25519 key. Lifts the H1 refusals one by one. | target ch.05 §5.9B R33, ch.51 R74; ch.15 R36 (V2 unchanged) | Batch 2, after H1 |
| H4 | **One trading identity under HD: the internal key.** Order signing, `sender_pubkey` and the V1 persistent pubkey (start event, swap statistics) move from the passphrase hash to the internal key, which keep-alive, `is_mine` and the stale-order sweep already use; the relay's own libp2p identity is checked against the same target. Then every HD negotiation is shape 3 (ch.51 R59). Wire-visible at upgrade (see below). | target ch.05 §5.9B R32–R33; ch.32, ch.59 | With H3, in one batch |
| H5 | **UTXO operations outside swaps.** The direct `withdraw` under HD (refused today; the reference serves it with `from`, decision BZ); a withdraw without `from` (the reference fails it, Reloaded's task route uses the enabled address, decision BY); legacy `my_balance` and `get_enabled_coins` (they fail at the reference too, decision CA); `fetch_utxos` and `consolidate_utxos` under HD; `task::create_new_account` under a software HD login (today it asks for a hardware device first and fails, for UTXO, QTUM and EVM alike). | target ch.49 R49.4, R49.27, ch.05 §5.9B R35; ch.38 §38.8, ch.20 §20.10 D1 note | After H3/H4 |
| H6 | **Every HD address covered.** History v2 under HD takes an account or address `target`, an account covering every known external and internal address (the reference target; EVM history is legacy only there); balance events for every known HD address; ERC20 `withdraw` with `from`; Sia: the reference offers no account or address RPCs and ignores `from`, while Reloaded shows balances on HD addresses it cannot spend (decision CC). The history cache is rebuilt, not migrated. | target ch.05 §5.9B R35; ch.38, ch.35, ch.53 D53.5, ch.49 R49.27 | Independent |
| H7 | **Family activations.** `enable_tendermint_with_assets` and `enable_bch_with_tokens` on the coin key at `path_to_address` under the coin's `derivation_path` (the reference target; a Tendermint configuration without `derivation_path` fails there; Tendermint activation plan items 2 and 5); legacy `enable`/`electrum` under HD: the reference builds and registers the coin with the coin key, then fails the response at `address`, while Reloaded binds the internal key and answers (decision CB). | target ch.05 §5.9B R35, ch.55 R55.4.10; ch.36, UPSTREAM-PARITY-GAPS (legacy EVM `enable`) | Independent |

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

After H4, an HD maker's orders and swap messages carry the internal key (the key
`get_public_key` reports) instead of the passphrase hash. Other nodes see a new maker. What the
operator does:

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

**Reference answers** (Spec Reader 2026-11-04, KDF Dirty Gate PASS; the facts are in the CRD):

- Trading identity, V1 persistent pubkey and the orderbook `address` of an own order: the internal
  key (ch.05 §5.9B R32–R33).
- V1 HTLC key: the coin key at `path_to_address`, which also funds, signs and receives; every HD
  negotiation is shape 3 (§5.9B R33, ch.51 R74). No up-front key check exists or is needed
  (§5.9B R34), so H1 is a Reloaded stop-gap.
- Legacy `enable`/`electrum` under HD: the coin is built with the coin key and registered, then
  the response fails at `address`; the coin stays enabled (ch.55 R55.4.10).
- Withdraw: `from` required for UTXO, QTUM and BCH on both routes; SLP and shielded refuse
  `from`; Sia ignores it (ch.49 R49.4 note, R49.27, R49.27a).
- Legacy `my_balance` and `get_enabled_coins` fail under HD at the reference too (§5.9B R35).
- History v2 needs an account or address `target` under HD; Tendermint ignores it; EVM is legacy
  only (§5.9B R35).

**Maintainer decisions pending** (asked 2026-11-04):

- **BY.** An HD UTXO withdraw without `from`: keep Reloaded's fallback to the enabled address as a
  documented divergence (ch.49 R49.4 as amended), restore parity, or keep it with a parity switch.
- **BZ.** The direct `withdraw` under HD: serve it with `from` as the reference does, or leave it
  refused.
- **CA.** Legacy `my_balance` and `get_enabled_coins` under HD: keep failing as the reference
  does, or answer the enabled address (a wire divergence with a switch).
- **CB.** Legacy `enable`/`electrum` under HD: bind the coin key and answer successfully, match
  the reference exactly (register, then fail the response), or refuse it.
- **CC.** Sia's HD account and address RPCs, which the reference does not offer: keep them and
  make `withdraw` honor `from`, hide balances on addresses other than the first, or keep them
  with a known-limitation note.
