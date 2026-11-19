# Plan: finish Solana support

**Status:** in progress (S0 and S2 done in PR #361; S1 and S3 to S9 before Solana is switched back on) — **Last reviewed:** 2026-11-15

> **Context:** created 2026-10-11 on
> `fix/activation-api-parity` when the coin-activation wire-parity work found
> that Reloaded's Solana activation differs from the reference implementation in
> every wire aspect. To be executed in a dedicated session.

> **Beta availability (2026-10-26).** By maintainer decision Solana (SOL, SPL)
> is switched off in 0.3.0-beta.1 and on `dev` as of 2026-11-08: while `SOLANA_ACTIVATION_AVAILABLE`
> (`coins/lp_coins_ops.rs`) is `false`, `enable_solana_with_tokens`,
> `enable_spl` and the legacy `enable` / `electrum` answer "Solana support is
> not available in this build" (CRD ch. 40 availability note). The release
> Solana returns in is not set; turning it back on (work package 7) is
> part of this plan. The decision below stands for that return.

## Decision this plan follows

Recorded in CRD ch. 40 (maintainer decision of 2026-10-11):

- Reloaded's own activation surface, `enable_solana_with_tokens` and
  `enable_spl`, is the **primary** contract and stays routed. The reference
  implementation (GLEEC KDF `v3.0.0-beta`) marks its Solana activation as
  experimental and its key-policy handling as not production-ready; Reloaded's
  surface is further along and is not thrown out.
- Backward compatibility still matters. The reference's surface -- the
  `experimental::enable_solana_with_assets` and
  `experimental::enable_solana_token` methods and the `SOLANA` /
  `SOLANATOKEN` coin-configuration forms -- is to be added **alongside** the
  primary surface as a compatibility surface (ch. 40 §40.1-§40.3, D40.3). The
  decision is taken; its implementation has not started (no PR, no code; see
  gaps 1 to 3 below).

## Current gaps

From ch. 40 §40.6, `docs/reloaded-rewrite/UPSTREAM-PARITY-GAPS.md` and a check of
the code on 2026-11-15:

1. The `experimental::` methods are not routed.
2. Solana is routed on native targets only; the compatibility surface is bound
   for all targets, including WASM. The Solana crates themselves are not known
   to block a WASM build; the node client is, because it uses the native-only
   HTTP transport.
3. Coin configuration: Reloaded parses `{"type": "SOLANA"}` without
   `protocol_data` and `SPLTOKEN` with `token_contract_address`. The reference
   uses `SOLANA` with `protocol_data: {}` and `SOLANATOKEN` with
   `mint_address` and `program_id`. A coins file written for the reference
   cannot be read by Reloaded today.
4. The primary surface's own request, result and error shapes are not bound in
   the CRD; ch. 40 binds only the compatibility surface.
5. Errors: block-height and balance failures map to `Internal`; no mint-owner
   check; no `PlatformCoinMismatch` or `CustomTokenError`; token-creation
   failures map to `Internal`. Nodes are reached through a failover pool (in
   order, a failed node set aside for a while), but no `getHealth` request
   selects them as R40.1.5 binds.
6. Token-2022: Reloaded's token configuration carries the `program_id` (PR
   #361), but nothing uses it yet: the token's account, balance, transfer and
   receiver-account creation all use the SPL Token program. All four must use the token's `program_id` (the ch. 40
   code-quality finding names the balance; the reference has that defect,
   Reloaded must not copy it).
7. HD-wallet, hardware-wallet and WalletConnect policies (D40.2). Today the
   activation takes the legacy passphrase key in every login. Under a global-HD
   login it therefore activates where ch. 40 R40.1.4 binds a refusal, and
   ch. 05 R46 item 3 names Solana among the users of that key. The two chapters
   are reconciled with the key-policy package.
8. Transaction history (D40.1).
9. Market, swap, raw-transaction and fee-preimage operations that are still
   unimplemented in the Solana/SPL modules (`UPSTREAM-PARITY-GAPS.md`,
   "Solana / SPL swap and history surface"). Trading is refused before any of
   them is reached (ch. 60 R60.6.5), but several others are reached by ordinary
   RPCs (history, address conversion, raw transaction, confirmation settings)
   and end the request abnormally.
10. Withdraw: an SPL `withdraw` with `max: true` transfers the requested amount,
    not the balance it reports; the rent deposit of a receiver's token account
    that the withdraw creates is not counted in the balance check or in
    `fee_details`.

## Work packages, in order

Each package is one pull request that carries its code and every document it
affects. Packages S0 to S9 change no swap behavior. The compatibility surface
already has cleared text in ch. 40 §40.1-§40.3; the other packages are
Reloaded's own design on the public Solana JSON-RPC and SPL program
specifications unless a reference question below changes them.

| # | Package | State |
| --- | --- | --- |
| S0 | Correct this plan, ch. 40 §40.6 and `docs/DISABLED_TESTS.md` group 6 against the code of 2026-11-15 | done, PR #361 |
| S2 | Accept both coin-configuration forms: the reference's `SOLANA` with `protocol_data: {}` and `SOLANATOKEN` with `platform`, `decimals`, `mint_address` and `program_id`, besides Reloaded's own forms; each token keeps its `program_id` (the `SPLTOKEN` form means the SPL Token program); a test per form | done on native builds, PR #361 (WASM with S8) |
| S1 | Bind the primary surface (`enable_solana_with_tokens`, `enable_spl`) in ch. 40: request fields, results and a per-method error table, as the wire-parity plan (`docs/plans/rpc-api-reference-wire-parity.md`, R43.8.4) requires; failures map as on the compatibility surface (§40.2-§40.3), as typed errors; the platform's decimals fixed at 9 (R40.6B.2) | done, PR #371 (ch. 40 §40.3A) |
| S3 | Every operation an ordinary RPC reaches on a Solana coin answers a typed "not supported for this coin" error instead of ending the request; operations only a swap reaches stay behind the trading refusal. The minimum transaction amount becomes one lamport for SOL and one smallest unit of its own decimals for each SPL token (section "Trading floors"); `show_priv_key` answers the 64-byte keypair in base58 (maintainer decision of 2026-11-15) | done: part a PR #401, part b (`disable_coin`) PR #413 |
| S4 | Keys under each login: under a global-HD login an ed25519 key derived from the wallet seed at the coin's `derivation_path` and `path_to_address`; a typed refusal for hardware-wallet and WalletConnect requests; the single-key rule of R40.1.4 unchanged; the legacy passphrase key never used for Solana; ch. 05 R46 and ch. 40 R40.1.4 reconciled by status notes; layouts `bip44_change` (default, `m/44'/501'/N'/0'`) and `bip44` (`m/44'/501'/N'`) chosen per request, nothing stored | done, PR #392 (ch. 40 §40.1A) |
| S5 | Move the Solana crates to the 3.x line, which no longer pins `ed25519-dalek` 1 (see `solana-sdk-upgrade.md`), with address and signature regression vectors; before S6 and S8 | open (in this cycle by maintainer decision, 2026-11-15) |
| S6 | Withdraw fixes for classic SPL tokens: the account and balance derived with the token's `program_id` (a wallet without an account reads zero), the whole amount for SPL `max`, rent counted and shown, the four-member `fee_details`, owner addresses only as recipients, the mint-owner check of R40.2.2; a token whose `program_id` is the Token-2022 program is refused at activation with a structured error (maintainer decision of 2026-11-15, ch. 40 D40.6B.4 (a)) | done, PR #425 |
| S6b | Full Token-2022 support: balance, source account and transfer under the token's own program, rent for the real account size, a stated rule for mints with a transfer fee (ch. 40 D40.6B.4 (b)) | after S9 |
| S6c | A rent-exempt guard for SOL withdraws: a withdraw that would leave the sender's account between zero and Solana's rent-exempt minimum, or that sends less than that minimum to an address without an account, answers a structured error instead of a transaction the node refuses at broadcast (code-quality finding in ch. 40 R40.6B.10; kept out of S6 on 2026-11-18) | after S6 |
| S7 | Compatibility methods `experimental::enable_solana_with_assets` and `experimental::enable_solana_token` over the same activation code, with the §40.2-§40.3 shapes, `getHealth` node selection and the §40.7 tests | after S2 and S6 |
| S8 | WASM routing for both surfaces; a build probe early in the cycle decides how it is built (for example a node client on the target-neutral HTTP layer) | open; probe after S2 |
| S9 | Switch Solana back on (CRD ch. 40 D40.4): set `SOLANA_ACTIVATION_AVAILABLE` to `true`, re-enable `docs/DISABLED_TESTS.md` group 6, retire the refusal tests and the beta notes in ch. 40, ch. 55, `RELOADED_VS_GLEEC.md`, `docs/GLEEC_COMPATIBILITY.md` and the README; devnet lab rows before it is marked ready | after S1 to S8 (maintainer decision, 2026-11-15) |
| S10 | Transaction history for SOL and SPL, Reloaded's own design on the public JSON-RPC | after S9 (planned for this cycle) |
| S11 | Swap and market surface, after a statement of what a Solana swap payment is on chain and after the swap architecture decisions | waits |

## Open points

- **Maintainer decisions (2026-11-15):** S1 to S8 all land before the
  switch-on, WASM included; the minimum transaction amount is set in S3
  (section "Trading floors"); the crate upgrade (S5) happens in this cycle,
  before S6 and S8.
- **Maintainer decision (2026-11-15), keys:** under a global-HD login Solana
  activates with an ed25519 key derived from the wallet seed at the coin's
  path; hardware-wallet and WalletConnect requests get a typed refusal; the
  passphrase-hash key is never used for Solana under HD; an activation that
  asks for `IguanaPrivKey` gets the wallet's single internal key (decision of
  2026-11-17) (S4, before the
  switch-on). The path layout matches the reference if the reference derives
  HD Solana keys; otherwise it follows the layout common Solana wallets use,
  under a named public standard, with vectors from a public test mnemonic.
- **Order (maintainer decision of 2026-11-18, replaces the earlier order that put
  S5 before S6 and S8):** S5 waits for the Zcash library upgrade that lifts the
  `digest` pin. S6 and S7 are built on the current 2.x crates, S8 after S5 when
  that lands in time and otherwise on 2.x with a WASM re-check after S5, then S5
  (which also ports S6's token-account and transfer calls), and S9 last.
- **Maintainer decisions (2026-11-15), later the same day:** the HD path offers
  both layouts, `m/44'/501'/N'/0'` by default and `m/44'/501'/N'` through a
  request parameter whose design is under review (S4); a Token-2022 token is
  refused at activation until full support lands after the switch-on (S6,
  S6b); `show_priv_key` answers the 64-byte keypair in base58 (S3).
- **Chosen for planning (2026-11-15):** the primary surface's failures map as on
  the compatibility surface (S1); a one-window build probe decides how WASM is
  built (S8); Solana returns wallet-only, and swaps follow only after the
  reference question on the on-chain payment is answered and the swap
  architecture is decided (S11); history after the switch-on in this cycle
  (S10).
- **Reference questions** (one Spec Reader round on ch. 40, gated before use):
  activation drift at the newest reference line; key handling under HD,
  hardware-wallet and WalletConnect logins; whether and how Solana swaps are
  done on chain; history; the `withdraw` answer; the answer of the other RPCs on
  Solana coins; the reading of coins-file variants.
- **Lab:** rows on the public devnet (faucet funds only): activation on both
  surfaces and both configuration forms, SOL and SPL withdraws, a Token-2022
  mint refused at activation (S6; its withdraw rows follow with S6b), message
  signing and the key-policy refusal, `disable_coin` on a token, on a platform
  without tokens and on a platform with tokens, each followed by activating
  the coin again (S3), and address parity with GLEEC KDF as a black box. An optional mainnet check at the
  switch-on needs a small SOL amount.

## Out of scope here

Retiring the primary surface. It stays, per the decision above.

## Trading floors (added 2026-10-25)

The reference implements no minimum trading volume for Solana or SPL: its
request ends abnormally (CRD ch. 60 row P1, finding F6). Its minimum
transaction amount is one lamport for SOL and for every SPL token, whatever the
token's decimals. Reloaded reports 0.00777 and 0 today and does not crash, so
there is no parity target. Choose Solana's floors when this plan designs Solana
trading, and record the choice in ch. 40 §40.6A. Decided by the maintainer on
2026-10-25 under plan `trading-floors-parity`.

Decided by the maintainer on 2026-11-15: the minimum transaction amount is set
now, in package S3: one lamport (0.000000001) for SOL, and one smallest unit of
the token's own decimals for each SPL token (10^-decimals; one lamport would be
below one unit of most tokens). The minimum trading volume stays at 0.00777
until Solana trading is designed (S11); while trading is refused it cannot be
observed.
