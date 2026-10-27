# Plan: finish Solana support

**Status:** parked (nothing started; awaits a dedicated session) — **Last reviewed:** 2026-10-25

> **Context:** created 2026-10-11 on
> `fix/activation-api-parity` when the coin-activation wire-parity work found
> that Reloaded's Solana activation differs from the reference implementation in
> every wire aspect. To be executed in a dedicated session.

> **Beta availability (2026-10-26).** By maintainer decision Solana (SOL, SPL)
> is switched off for the coming beta: while `SOLANA_ACTIVATION_AVAILABLE`
> (`coins/lp_coins_ops.rs`) is `false`, `enable_solana_with_tokens`,
> `enable_spl` and the legacy `enable` / `electrum` answer "Solana support is
> not available in this build" (CRD ch. 40 availability note). Solana is
> planned to return in the next beta; turning it back on (work package 7) is
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
  `SOLANATOKEN` coin-configuration forms -- is added **alongside** the primary
  surface as a compatibility surface (ch. 40 §40.1-§40.3, D40.3).

## Current gaps

From ch. 40 §40.6 and `docs/reloaded-rewrite/UPSTREAM-PARITY-GAPS.md`:

1. The `experimental::` methods are not routed.
2. Solana is routed on native targets only; the compatibility surface is bound
   for all targets, including WASM.
3. Coin configuration: Reloaded parses `{"type": "SOLANA"}` without
   `protocol_data` and `SPLTOKEN` with `token_contract_address`. The reference
   uses `SOLANA` with `protocol_data: {}` and `SOLANATOKEN` with
   `mint_address` and `program_id`. A coins file written for the reference
   cannot be read by Reloaded today.
4. The primary surface's own request, result and error shapes are not bound in
   the CRD; ch. 40 binds only the compatibility surface.
5. Errors: block-height and balance failures map to `Internal`; no node-health
   selection; no mint-owner check; no `PlatformCoinMismatch` or
   `CustomTokenError`; token-creation failures map to `Internal`.
6. Token-2022: the balance must be read from the associated token account
   derived with the token's `program_id` (ch. 40 code-quality finding; the
   reference has this defect, Reloaded must not copy it).
7. HD-wallet, hardware-wallet and WalletConnect policies (D40.2).
8. Transaction history (D40.1).
9. Market, swap, raw-transaction and fee-preimage operations that are still
   unimplemented in the Solana/SPL modules (`UPSTREAM-PARITY-GAPS.md`,
   "Solana / SPL swap and history surface").

## Work packages, in order

1. **Bind the primary surface in ch. 40.** Write the request field table,
   success shapes and per-method error table
   (`error_type | failure class | error_data shape | HTTP status`) for
   `enable_solana_with_tokens` and `enable_spl`, as the wire-parity plan
   (`docs/plans/rpc-api-reference-wire-parity.md`, R43.8.4) requires for every
   method. Decide the primary surface's backend-failure mapping (item 5); the
   reference's mapping in §40.2-§40.3 is the natural default, so a client that
   handles one surface handles both.
2. **Accept both coin-configuration forms.** Parse the reference's `SOLANA` /
   `SOLANATOKEN` forms in addition to Reloaded's, with a test per form. This
   is the change with the widest effect, because coins files are shared between
   implementations; do it before adding the methods.
3. **Add the compatibility methods** `experimental::enable_solana_with_assets`
   and `experimental::enable_solana_token` over the same activation code as the
   primary methods, with the §40.2-§40.3 request and result shapes and the
   §40.7 tests.
4. **WASM routing** for both surfaces, if the Solana client stack builds for
   `wasm32-unknown-unknown`; otherwise record the limitation in ch. 40.
5. **Token-2022 balance fix** (item 6).
6. **Key policies** (item 7), then **history** (item 8), then the
   **swap and market surface** (item 9), each as its own CRD pass where the
   reference behavior is needed (Spec Reader, Dirty Gate, Coder).
7. **Switch Solana back on** (CRD ch. 40 D40.4): set
   `SOLANA_ACTIVATION_AVAILABLE` to `true`, re-enable the Solana docker tests
   of `docs/DISABLED_TESTS.md` group 6, and retire the refusal tests and the
   beta notes in ch. 40, ch. 55 and `RELOADED_VS_GLEEC.md`. The maintainer
   decides which of the packages above must land first.

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
