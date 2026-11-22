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

> **Switched on as wallet-only (2026-11-22).** `SOLANA_ACTIVATION_AVAILABLE`
> and the "Solana support is not available in this build" refusal are removed
> (S9, PR #475): `enable_solana_with_tokens`, `enable_spl` and both
> `experimental::` methods activate SOL and SPL tokens on native and browser
> builds, and the legacy `enable` / `electrum` answer the mmrpc 2.0 rejection
> of CRD ch. 55 R55.4.7. Solana stays wallet-only: every order and swap entry
> point refuses a Solana coin or SPL token (the untradable-coin check, CRD
> ch. 60 R60.6.5), and there is no transaction history until S10 (CRD ch. 40
> D40.4 and its status note of this date). The box above describes the tree
> until this date.

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
| S2 | Accept both coin-configuration forms: the reference's `SOLANA` with `protocol_data: {}` and `SOLANATOKEN` with `platform`, `decimals`, `mint_address` and `program_id`, besides Reloaded's own forms; each token keeps its `program_id` (the `SPLTOKEN` form means the SPL Token program); a test per form | done, PR #361 (WASM in PR #430) |
| S1 | Bind the primary surface (`enable_solana_with_tokens`, `enable_spl`) in ch. 40: request fields, results and a per-method error table, as the wire-parity plan (`docs/plans/rpc-api-reference-wire-parity.md`, R43.8.4) requires; failures map as on the compatibility surface (§40.2-§40.3), as typed errors; the platform's decimals fixed at 9 (R40.6B.2) | done, PR #371 (ch. 40 §40.3A) |
| S3 | Every operation an ordinary RPC reaches on a Solana coin answers a typed "not supported for this coin" error instead of ending the request; operations only a swap reaches stay behind the trading refusal. The minimum transaction amount becomes one lamport for SOL and one smallest unit of its own decimals for each SPL token (section "Trading floors"); `show_priv_key` answers the 64-byte keypair in base58 (maintainer decision of 2026-11-15) | done: part a PR #401, part b (`disable_coin`) PR #413 |
| S4 | Keys under each login: under a global-HD login an ed25519 key derived from the wallet seed at the coin's `derivation_path` and `path_to_address`; a typed refusal for hardware-wallet and WalletConnect requests; the single-key rule of R40.1.4 unchanged; the legacy passphrase key never used for Solana; ch. 05 R46 and ch. 40 R40.1.4 reconciled by status notes; layouts `bip44_change` (default, `m/44'/501'/N'/0'`) and `bip44` (`m/44'/501'/N'`) chosen per request, nothing stored | done, PR #392 (ch. 40 §40.1A) |
| S5 | Move the Solana crates to the 3.x line, which no longer pins `ed25519-dalek` 1 (see `solana-sdk-upgrade.md`), with address and signature regression vectors; before S6 and S8 | done, PR #465 (3.x crates, `spl-token` 9; regression vectors; RUSTSEC-2022-0093 cleared, RUSTSEC-2024-0344 stays through the peer-to-peer library) |
| S6 | Withdraw fixes for classic SPL tokens: the account and balance derived with the token's `program_id` (a wallet without an account reads zero), the whole amount for SPL `max`, rent counted and shown, the four-member `fee_details`, owner addresses only as recipients, the mint-owner check of R40.2.2; a token whose `program_id` is the Token-2022 program is refused at activation with a structured error (maintainer decision of 2026-11-15, ch. 40 D40.6B.4 (a)) | done, PR #425 |
| S6b | Full Token-2022 support: balance, source account and transfer under the token's own program, rent for the real account size, a stated rule for mints with a transfer fee (ch. 40 D40.6B.4 (b)) | after S9 |
| S6c | A rent-exempt guard for SOL withdraws: a withdraw that would leave the sender's account between zero and Solana's rent-exempt minimum, or that sends less than that minimum to an address without an account, answers a structured error instead of a transaction the node refuses at broadcast (code-quality finding in ch. 40 R40.6B.10; kept out of S6 on 2026-11-18) | after S6 |
| S7 | Compatibility methods `experimental::enable_solana_with_assets` and `experimental::enable_solana_token` over the same activation code, with the §40.2-§40.3 shapes, `getHealth` node selection and the §40.7 tests | done, PR #426 (WASM in PR #430) |
| S8 | WASM routing for both surfaces; a build probe early in the cycle decides how it is built (for example a node client on the target-neutral HTTP layer) | done, PR #430 (on the 2.x crates; WASM re-checked on the 3.x crates in PR #465) |
| S9 | Switch Solana back on (CRD ch. 40 D40.4): set `SOLANA_ACTIVATION_AVAILABLE` to `true`, re-enable `docs/DISABLED_TESTS.md` group 6, retire the refusal tests and the beta notes in ch. 40, ch. 55, `RELOADED_VS_GLEEC.md`, `docs/GLEEC_COMPATIBILITY.md` and the README; devnet lab rows before it is marked ready | done, PR #475 (the constant and its refusal removed, so no route reads a switch; the four activation methods activate on native and browser builds; legacy `enable` / `electrum` answer the R55.4.7 rejection; group 6 re-enabled; refusal tests replaced by tests that the four methods reach the activation; Solana wallet-only, no orders, swaps or history) |
| S10 | Transaction history for SOL and SPL, Reloaded's own design on the public JSON-RPC | done, PR #494 (design in CRD ch. 40 §40.6C; maintainer decisions of 2026-11-22: the whole history of each address, paced; a failed transaction whose fee was charged is a fee-only row; the coin's node pool, no new setting; every token account the wallet owns for the mint) |
| S11-D0 | Deployment of the public swap program for Reloaded (CRD ch. 40 R40.6D.1), operated by the maintainer: record the license: the evidence is the crate manifest's `license` field, read on 2026-11-22, which declares MIT at the state `satomic-swap fix/htlc-correctness (as of 2026-10-28)` of CRD ch. 02 §2.15A, while the repository has no license file; the Anchor and SPL crates (Apache-2.0) are compiled into the program, not into KDF; if the repository's owner states other terms, this step stops and the maintainer decides; a clean review of the program source at that state (a findings note, not an audit); a build of that reviewed state with only its declared program address changed to Reloaded's own, built twice with equal results, and its hash on chain verified after deployment (CRD ch. 40 R40.6D.1); the third-party devnet program is not matched: a build of the reviewed state did not equal it, and the commit the repository's record names for it is not in its public history; a deployment script in this lane; deployments without an upgrade authority: on devnet with a lab key and faucet SOL (the devnet lab runs on this deployment, so it tests the same bytes as mainnet; the public devnet program stays upgradeable by a third-party key), then on mainnet from the maintainer's own wallet; each program address recorded in a dated status note of ch. 40 §40.6D and in the operator's how-to for a local coins entry (a proposal to the coins repository follows after the release). Rent measured on 2026-11-22 for Reloaded's build (270,408 bytes): 1.37455148 SOL for the program data and 0.00083312 SOL for the program account, locked for good because the program cannot be closed, plus transaction fees; the deployment buffer needs about the same amount again for the duration of the deployment | devnet done (`Hebok2uz2WrP6aaMAiiQYX1VzusCUC8TuCXXphJpksxd`, built from the reviewed state with only its declared address changed, two equal builds, hash on chain `7d253acf…` equal, no upgrade authority); mainnet deployment is decided after 0.5.0 |
| S11a | CRD ch. 40 §40.6D: Solana and SPL in the legacy swap protocol, Reloaded's own design on the public program's recorded interface (maintainer decisions of 2026-11-22) | in progress, PR #501 |
| S11b | SOL legs: the program check at activation (R40.6D.2), contract field, keys, amounts in whole units, payment, validation, claim and refund, refund gate, spend search and secret, dex fee with its memo, trade-fee answers and admission, the evidence rule (R40.6D.3 to R40.6D.16) | in progress, PR #503 (stacked on PR #501; lands with the swap operations unreachable) |
| S11c | SPL legs: the same for tokens, with the vault, the receiver's token account and the SPL dex fee | postponed until after 0.5.0 |
| S11d | Switch-on per coins entry that names a program (R40.6D.2), the SHA-256 pair rule (R40.6D.4), the whole-unit volume check at order placement and both swap starts (R40.6D.6), admission (R40.6D.14) including chapter 51 R100's refusal of a swap when the coin's pool has fewer than two nodes and no evidence node is named, preimages and floors; devnet swaps between two Reloaded nodes (both roles, claim and refund, a restart mid-swap) before it is marked ready; a mainnet check with RIN once S11-D0 has deployed the program | postponed until after 0.5.0 |

## Open points

- **Maintainer decision (2026-11-22), later the same day:** Solana swaps are
  postponed until after 0.5.0. S11b lands with the swap operations unreachable;
  S11c and S11d resume after 0.5.0; the mainnet deployment is decided then. The
  deployment gate is a reproducible build of the reviewed source with its hash
  verified on chain; the third-party devnet program is not matched (a build of
  the reviewed state did not equal it, and the commit named for it is not in
  the repository's public history). A release does not change the coins repository; an operator names
  the program in a local coins entry.
- **Maintainer decisions (2026-11-22), swaps (S11):** the swap client targets
  the public program of CRD ch. 40 F40.6B.5, and Reloaded deploys that
  program itself on devnet and mainnet, immutable, after a license check, a
  clean source review and a reproducible build matched to the devnet program;
  the program address comes from the coins entry (S11-D0). The dex fee goes to
  the Solana address of the network's ed25519 dex-fee key; an SPL fee creates
  the fee owner's token account for a mint that has none, once; the minimum
  trading volume stays 0.00777 for SOL and SPL; devnet swaps run before the
  switch-on is marked ready, and a mainnet check with RIN follows the mainnet
  deployment.
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
  the coin again (S3), both `experimental::` methods (S7), the node pool (S7):
  an unhealthy node skipped, the random spread (two nodes each see requests
  from separate activations), `listed` keeping the order, and a set-aside node
  recovering, and address parity with GLEEC KDF as a black box. An optional mainnet check at the
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
