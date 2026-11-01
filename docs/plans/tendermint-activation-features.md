# Plan: Tendermint activation features

**Status:** parked (deferred by maintainer decision 2026-10-11; nothing started) — **Last reviewed:** 2026-10-29

> **Context:** created 2026-10-11 on
> `fix/activation-api-parity`. That branch brings Tendermint activation to wire
> parity (error names, `error_data` shapes, HTTP statuses, error placement and
> failure mapping, CRD ch. 36). The features below are what the reference
> implementation (GLEEC KDF `v3.0.0-beta`) supports and Reloaded does not; they
> were deferred by maintainer decision on 2026-10-11 and belong to a separate
> branch.

> **Beta availability (2026-10-26).** By maintainer decision Tendermint coins
> and tokens are wallet-only in the coming beta: while
> `TENDERMINT_SWAPS_AVAILABLE` (`coins/lp_coins_ops.rs`) is `false`, activation,
> balances and `withdraw` work, while `setprice`, `buy`, `sell` and
> `start_swap` refuse the coin and an order with a Tendermint coin saved
> by an earlier version is cancelled at startup; a
> swap already in progress still completes or is recovered (CRD ch. 18 and
> ch. 36 availability notes). Swaps are planned to return in a later release.
> Until a dedicated Tendermint swap plan exists, turning swaps back on (setting
> the constant to `true` and retiring its refusal tests) is recorded here; its
> place in the order below is the maintainer's call.

## Interim behavior on `fix/activation-api-parity`

Reloaded used to accept any `activation_params` and ignore it, so a request for
a public-key (Ledger / watch-only) or WalletConnect activation silently
activated with the local context key. A user who asked for a device-backed
activation got a hot-key one without knowing. Until the features below land,
Reloaded rejects a request that carries `activation_params` with
`PlatformCoinCreationError` (`{ticker, error}`, HTTP 500) -- the discriminant the
reference uses for an unsupported signing context -- instead of activating with
the local key. This is a deliberate, temporary divergence recorded in ch. 36
§36.7; the reference accepts these requests.

## Features to implement

References are to CRD ch. 36 (Dirty-Gate-passed).

1. **`activation_params` signing policies** (R36.1.3):
   - *Public-key form* `{"type": "WithPubkey", "params": {"pubkey": {"type":
     "secp256k1" | "ed25519", "value": [<bytes>]}, "is_ledger_connection":
     <bool>}}`: watch-only / Ledger-flagged activation from a caller-supplied
     key.
   - *WalletConnect form* `{"type": "WalletConnect", "params":
     {"session_topic": <string>}}` (ch. 22 for the WalletConnect side).
   - *Public-docs WalletConnect form* `{"priv_key_policy": {"type":
     "WalletConnect", "data": <topic>}}`, treated exactly as the WalletConnect
     form. The public docs show this form; the reference rejects it. The
     maintainer decided to accept both forms.
   - Malformed input is the dispatcher's `InvalidRequest`, never a crash (the
     reference panics on some malformed keys; Reloaded must not).
   - When this lands, drop the interim rejection above.
2. **HD-wallet mode**: honor `path_to_address` under the coin's derivation path
   (R36.1.2, R36.1.4 step 3). The ch. 36 code-quality finding applies: derive
   the public key from the activated private key, not the wallet's root public
   key, and verify with an HD signing test.
3. **Custom tokens**: the `protocol` member of `tokens_params[]` and of
   `enable_tendermint_token`, with `CustomTokenError`
   (`{"DuplicateTickerInConfig": {"ticker_in_config": ...}}`).
   A custom token must be wallet-only, as for EVM (ch. 35 R35.2.7); the EVM
   implementation gap is tracked in kdf-reloaded/kdf#16.
4. **`tx_history: true`** starts background transaction-history fetching
   (Reloaded accepts the flag and does nothing).
5. **Hardware `priv_key_policy`.** The reference ignores `priv_key_policy` for
   Tendermint, so `"Trezor"` silently activates with the context key (ch. 36
   code-quality finding). Ch. 36 currently binds the reference behavior. Decide
   whether Reloaded refuses it with `UnexpectedDeviceActivationPolicy` as a
   deliberate deviation; the same safety argument as the interim rejection of
   `activation_params` applies.
6. **Task-managed single token family** `task::enable_tendermint_token::*`
   (D36.2), if wanted.

## Order

1 (it removes the interim rejection), then 2, 3, 4. Items 5 and 6 need a
maintainer decision first.

## Latent hazard: a key-less policy on the legacy HTLC path

Found 2026-10-18 while designing the WalletConnect availability switches of CRD
ch. 22 §22.9B. This is not a feature of its own; it is a constraint that items 1
and 5 above must satisfy, because both introduce an activation policy that has
no local key. The hazard is Tendermint's and exists independently of
WalletConnect -- any key-less policy triggers it.

**The path.** `TendermintCoin::get_htlc_key_pair` resolves the activation
policy's local key and returns `Option<KeyPair>`
(`mm2src/coins/tendermint/tendermint_swap_ops.rs:711`, via
`TendermintActivationPolicy::activated_key_or_err`); a policy with no local key
yields `None`. The legacy (v1) maker and taker state machines call it at swap
start (`mm2src/mm2_main/src/lp_swap/maker_swap.rs:492`,
`mm2src/mm2_main/src/lp_swap/taker_swap.rs:1006`) and pass the result to
`derive_htlc_pubkeys` (`mm2src/mm2_main/src/lp_swap.rs:970`), which resolves it
as `map_or(&node_pubkey, |k| k.public())` (lines 979 and 982): a coin that
returns no key pair does not fail the swap -- it **silently substitutes this
node's own persistent secp256k1 public key**. Tendermint does not override
`SwapOps::derive_htlc_pubkey`, so the default (the secp256k1 case of ch. 51 R64)
carries that key unchanged into the negotiation as the coin's HTLC pubkey.

**Why that matters.** Both legs read the announcement:

- The counterparty derives the HTLC recipient account from it --
  `send_htlc_for_denom` builds `AccountId::new(prefix, dhash160(other_pub))`
  (`tendermint_swap_ops.rs:162`) -- so the counterparty's payment is locked to
  an account derived from our node's internal key, not to the account the
  paired external wallet controls.
- Our own leg then fails late rather than early: every broadcast signs with
  `activated_key_or_err` (`mm2src/coins/tendermint/tendermint_helpers.rs:533`),
  which errors for a key-less policy, so the swap aborts mid-flight with a
  transaction error.

The dangerous property is the **silent substitution**, not the late failure. A
fix that only turns the late failure into a cleaner error still puts a pubkey
the wallet cannot sign with on the wire and still lets the counterparty lock
funds to the wrong account.

**Latent today, not live.** `TendermintActivationPolicy` has exactly one
variant, `PrivateKey(PrivKeyPolicy<TendermintKeyPair>)`
(`mm2src/coins/tendermint/tendermint_types.rs:182`). That alone is not the
guarantee: `PrivKeyPolicy` does have a key-less variant, `Trezor`, whose
`key_pair()` returns `None` (`mm2src/coins/lp_coins_context.rs:251`, `269`), and
`TendermintActivationPolicy::public_key` already handles it with an
`Unsupported` error. The guarantee is the construction site --
`tendermint_coin_from_conf_and_request` is the only place in the tree that
builds a Tendermint activation policy, and it always builds
`PrivKeyPolicy::KeyPair` from a concrete private key
(`mm2src/coins/tendermint/tendermint_coin.rs:95`-`99`). Nothing can hand
Tendermint `PrivKeyPolicy::Trezor` today. The hazard goes live the moment item 1
or item 5 adds a second policy variant or a second construction site.

**Not covered by the existing early-refusal duties.** Ch. 22 RP11.1 binds the
swap-protocol-v2 HTLC paths and the watcher-message paths; ch. 47 R47.5.13a
binds the EVM-family HTLC paths plus those same two, and explicitly declines to
be a blanket ban on every key-less policy. Neither reaches the Tendermint
**legacy (v1)** HTLC paths described above.

**Remedy: an unconditional gate, not a switch.** Refuse the operation at the
earliest lifecycle point (order placement / swap start, consistent with
R47.5.13a) when the coin's activation policy has no local key. There is nothing
here an operator should be able to turn on, so this is deliberately *not* a
compatibility switch and takes no default-side weighing under
[`../COMPAT_SWITCHES.md`](../COMPAT_SWITCHES.md).

**The gate must cover tokens as well as the platform coin.**
`TendermintToken::get_htlc_key_pair` delegates straight to the platform coin
(`mm2src/coins/tendermint/tendermint_token.rs:331`), so every IBC/CW20-style
token on a key-less platform coin inherits the hazard unchanged. A fix or a
regression test written only against the platform coin would miss them.
