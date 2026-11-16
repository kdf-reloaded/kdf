# Plan: Tendermint activation features

**Status:** in progress (items 2, 3 and 5's refusal are done; item 4, transaction history, is built in PR #363 (0.5.0); the public-key form (item 1b) is built (0.5.0); open: the WalletConnect form (item 1a), item 6 (not in 0.5.0), the hardware signing package (item 7), the per-swap lock choice and the chain-refund path (CRD ch.18 D18.11.1–D18.11.3), and turning Tendermint swaps on again; deferred by maintainer decision 2026-10-11, custom tokens and the HD `path_to_address` shipped in PR #319, the swap-path fixes and the priority fees in PRs #299 and #323) — **Last reviewed:** 2026-11-16

> **Context:** created 2026-10-11 on
> `fix/activation-api-parity`. That branch brings Tendermint activation to wire
> parity (error names, `error_data` shapes, HTTP statuses, error placement and
> failure mapping, CRD ch. 36). The features below are what the reference
> implementation (GLEEC KDF `v3.0.0-beta`) supports and Reloaded does not; they
> were deferred by maintainer decision on 2026-10-11 and belong to a separate
> branch.

> **Beta availability (2026-10-26).** By maintainer decision Tendermint coins
> and tokens are wallet-only in 0.3.0-beta.1 and on `dev` as of 2026-11-08 (Tendermint
> swaps were added to the 0.4.0 scope on 2026-11-08 and put aside again on
> 2026-11-10): while
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
Reloaded rejects a request that carries a WalletConnect `activation_params` (the public-key form is built, 2026-11-16, item 1b) with
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
     maintainer decided to accept both forms. The decision is taken; its
     implementation has not started (no PR, no code).
   - Malformed input is the dispatcher's `InvalidRequest`, never a crash (the
     reference panics on some malformed keys; Reloaded must not).
   - When this lands, drop the interim rejection above.
   - *State of the parts (2026-11-15).* The coin-side WalletConnect signer exists
     (`mm2src/coins/tendermint/wc_integration.rs`, `WcCoinSigner`, with unit tests).
     What does not exist is the activation side: no activation policy variant
     holds it and no construction site builds a coin with it, so a request cannot
     reach it. The WalletConnect form (item 1a) and the public-key form (item 1b)
     are the two remaining pieces of this item.
   - *Item 1b built (2026-11-16, 0.5.0).* The public-key form activates a
     watch-only coin: `TendermintActivationPolicy::PublicKey` holds the key and
     the Ledger flag, the account is derived by the public SDK rule of the key
     type (secp256k1 or ed25519, D36.17 (c)), and nothing is contacted at
     activation. A malformed `activation_params` is now an `InvalidRequest`
     parse failure at the request. `withdraw` and the staking requests answer
     the unsigned documents of R36.14.17 (D36.18 (a)), with the Amino form and
     the threefold fee under the Ledger flag; every path that would sign with a
     node-held key is refused with `PrivKeyNotAllowed::WatchOnlyCoin`; orders
     stay refused (R18.11.23a, D18.11.10 (a)). Lab row L4 (ATOM mainnet, any
     funded address) is owed before READY. The WalletConnect forms stay
     refused until item 1a.
     The unsigned result follows CRD ch.36 §36.14.8 (R36.14.29 -- R36.14.32;
     status note of 2026-11-16 there).
     This tree has no IBC `withdraw`, so a watch-only coin cannot produce an
     unsigned IBC document; that is a separate item (the IBC send route).
2. **HD-wallet mode**: honor `path_to_address` under the coin's derivation path
   (R36.1.2, R36.1.4 step 3). The ch. 36 code-quality finding applies: derive
   the public key from the activated private key, not the wallet's root public
   key, and verify with an HD signing test.
   *Implemented (PR #319).* Under an HD login the coin's key is derived from the
   seed at the coin's `derivation_path` plus `path_to_address`, with the
   derivation the UTXO coins use; an iguana login is unchanged. The public key is
   derived from the activated private key, and a test signs a withdrawal at a
   non-default path and verifies it against that path's public key. Tendermint no
   longer runs on the passphrase-hash key; the address changes at the upgrade
   (changelog upgrade note).
3. **Custom tokens**: the `protocol` member of `tokens_params[]` and of
   `enable_tendermint_token`, with `CustomTokenError`
   (`{"DuplicateTickerInConfig": {"ticker_in_config": ...}}`).
   A custom token must be wallet-only, as for EVM (ch. 35 R35.2.7); the EVM
   side was fixed by `e18eca319` of 2026-10-12, merged with PR #21 on
   2026-10-13 (issue #16, closed).
   *Implemented (PR #319).* Both methods accept the inline `TENDERMINTTOKEN`
   protocol; a duplicate of a configured ticker is the `CustomTokenError`; the
   token is marked custom and is wallet-only.
4. **`tx_history: true`** starts background transaction-history fetching
   (Reloaded accepts the flag and does nothing).
   *Built in PR #363 (0.5.0).* `tx_history: true` on the platform coin starts the
   fetcher of CRD ch.36 R36.14.3; `my_tx_history` serves the platform coin and
   its tokens from the platform's store (R36.14.1, R36.14.2). Decided
   2026-11-15: the history state is `InProgress` during the first pass
   (D36.10 (b)); the store is meant to be byte-compatible with the reference as R36.14.10
   states it (D36.11 (a); unit-tested against the chapter's wording and checked in the lab
   (TH-3, 2026-11-15 and 2026-11-16) with a reference build on the lab network as
   the other writer, in both directions: a fresh store written by each program,
   each program continuing the other's store, and a store written by an earlier
   head backfilled and then listed by the other program, with the same rows,
   identifiers and address-index entries; the other reference binary refuses
   that network and was not run; the position counting follows the cleared
   §36.14.6); amounts follow D36.12 (b); denominations resolve as at the
   reference (D36.13 (a'): the platform `denom`, else the first coins-file entry
   of any platform), and the store is read again from the chain when that lookup
   changes (D36.21 (b)); the fee, the empty fee list and the staking events
   without a delegator follow D36.19 (b) and D36.20 (b); no row for a
   chain-side HTLC refund or a completed undelegation (D36.14), which stays a
   planning question. Native builds only. Limitation: the history of a custom
   token that is absent from the coins file does not exist in 0.5.0. A later
   package, not scheduled: a Reloaded-side identifier scheme for transfers the
   reference does not count (such custom-token transfers). *Address index for every store-served
   family (built in PR #380, the follow-up to PR #363, 0.5.0).* The store's address index
   is created and written for every coin the SQLite history store serves (BCH and
   SLP tokens included; their listing is unchanged), and the BCH/SLP fetcher
   prepares it at start; in PR #363 only Tendermint coins wrote it. Owed before the 0.5.0 beta
   freeze: the SLP token store comparison against the reference writer. The BCH row (one transaction, fresh stores
   of both programs, each listing the other's) was run on 2026-11-16 with equal results, and the
   interrupted-first-pass leg of the TH-2 row on a local chain passed in the lab on 2026-11-16.
5. **Hardware `priv_key_policy`.** The reference ignores `priv_key_policy` for
   Tendermint, so `"Trezor"` silently activates with the context key (ch. 36
   code-quality finding).
   *Decision (2026-11-15).* An explicit typed refusal of a hardware policy is
   built (the change that adds this note): the request member is declared, an
   absent value or `ContextPrivKey` activates as before, and a hardware value
   is refused before any network access with a typed Tendermint init error,
   answered as `PlatformCoinCreationError` like the interim rejection of
   `activation_params`. A deliberate divergence from the reference until
   hardware signing exists (ch. 36 §36.7, status note of 2026-11-15). Any
   other value of the member, such as the WalletConnect form, fails request
   parsing (`InvalidRequest`, HTTP 400) until item 1 builds its activation.
   Building
   Trezor signing for Cosmos coins is a future package, item 7.
6. **Task-managed single token family** `task::enable_tendermint_token::*`
   (D36.2). *Not in 0.5.0 (decision 2026-11-15).*
7. **Trezor signing for Cosmos coins** (future package, not scheduled). A
   device-backed signer for Tendermint activations; it would replace the
   refusal of item 5 for the policies it serves.

## Order

Items 2 and 3 are done, item 4 is built (PR #363) and item 1b, the public-key
form, is built (0.5.0). The remaining order: WalletConnect (item 1a), then item
5's future package (item 7). Item 6 is not in 0.5.0.

## Lab rows owed before swaps are switched on

These rows run on a chain with the HTLC module before `TENDERMINT_SWAPS_AVAILABLE`
becomes `true`, in the change that switches swaps on:

- **Trade-fee estimate under the swap fee policy (PR #323).** With a funded account
  and a lab-only `priority_gas_prices` array on the coin, `get_trade_fee` under each
  policy value (`Legacy`, `Low`, `Medium`, `High`) gives fees in the ratio of the
  selected gas prices (CRD ch.18 T18.6B.1, ch.35 T35.38). PR #323 covers this with
  unit tests only.
  *2026-11-10: PASSED on a local single-validator chain with the HTLC module: all four
  policy values gave fees in the ratio of the selected gas prices.*
- **Activation-feature rows (PR #319).**
  *2026-11-09: PASSED on the PR #319 build:* the activation and balance of a custom IBC
  token; the refusal of a duplicate ticker; the refusal of a wallet-only order; HD
  addresses equal to an independent BIP-44 derivation for the default path and for
  `address_id` 1; the iguana address identical to that of a control build.
- **Trade-fee estimate under each swap-fee-policy level (PR #323).**
  *2026-11-10: PASSED on a local single-validator chain: all four policy values gave
  fees in the ratio of the gas prices.*

### What the WalletConnect lab row needs on the maintainer's side

A WalletConnect-capable Cosmos wallet (mobile or extension) holding a small ATOM
balance, the pairing performed against a lab node, and about two hours of
availability for the row.

## Latent hazard: a key-less policy on the legacy HTLC path

Found 2026-10-18 while designing the WalletConnect availability switches of CRD
ch. 22 §22.9B. This is not a feature of its own; it is a constraint that items 1
and 5 above must satisfy, because both introduce an activation policy that has
no local key. The hazard is Tendermint's and exists independently of
WalletConnect -- any key-less policy triggers it.

**The path.** `TendermintCoin::get_htlc_key_pair` resolves the activation
policy's local key and returns `Option<KeyPair>`
(`mm2src/coins/tendermint/tendermint_swap_ops.rs:892`, via
`TendermintActivationPolicy::activated_key_or_err`); a policy with no local key
yields `None`. The legacy (v1) maker and taker state machines call it at swap
start (`mm2src/mm2_main/src/lp_swap/maker_swap.rs:616`,
`mm2src/mm2_main/src/lp_swap/taker_swap.rs:1113`) and pass the result to
`derive_htlc_pubkeys` (`mm2src/mm2_main/src/lp_swap.rs:1027`), which resolves it
as `map_or(node_pubkey, |k| k.public())` (lines 1035 and 1038): a coin that
returns no key pair does not fail the swap -- it **silently substitutes this
node's own persistent secp256k1 public key**. Tendermint does not override
`SwapOps::derive_htlc_pubkey`, so the default (the secp256k1 case of ch. 51 R64)
carries that key unchanged into the negotiation as the coin's HTLC pubkey.

**Why that matters.** Both legs read the announcement:

- The counterparty derives the HTLC recipient account from it --
  `send_htlc_for_denom` builds `AccountId::new(prefix, dhash160(other_pub))`
  (`tendermint_swap_ops.rs:173`) -- so the counterparty's payment is locked to
  an account derived from our node's internal key, not to the account the
  paired external wallet controls.
- Our own leg then fails late rather than early: every broadcast signs with
  `activated_key_or_err` (`mm2src/coins/tendermint/tendermint_helpers.rs:595`),
  which errors for a key-less policy, so the swap aborts mid-flight with a
  transaction error.

The dangerous property is the **silent substitution**, not the late failure. A
fix that only turns the late failure into a cleaner error still puts a pubkey
the wallet cannot sign with on the wire and still lets the counterparty lock
funds to the wrong account.

**Latent today, not live.** `TendermintActivationPolicy` has exactly one
variant, `PrivateKey(PrivKeyPolicy<TendermintKeyPair>)`
(`mm2src/coins/tendermint/tendermint_types.rs:242`). That alone is not the
guarantee: `PrivKeyPolicy` does have a key-less variant, `Trezor`, whose
`key_pair()` returns `None` (`mm2src/coins/lp_coins_context.rs:305`, `325`), and
`TendermintActivationPolicy::public_key` already handles it with an
`Unsupported` error. The guarantee is the construction site --
`tendermint_coin_from_conf_and_request` is the only place in the tree that
builds a Tendermint activation policy, and it always builds
`PrivKeyPolicy::KeyPair` or `HDWallet`, both from a concrete private key
(`mm2src/coins/tendermint/tendermint_coin.rs:174` and `192`). Nothing can hand
Tendermint `PrivKeyPolicy::Trezor` today. The hazard goes live the moment item 1
or item 5 adds a second policy variant or a second construction site.

*Update (2026-11-16, item 1b).* The second policy variant now exists:
`TendermintActivationPolicy::PublicKey` (a watch-only coin). It holds no local
key, so the wallet-only predicate refuses its orders (`has_local_key` is false for
it), a token uses its platform coin's policy, `get_htlc_key_pair` answers `None`,
and every swap send, claim, refund, message-signing and private-key path refuses
with `PrivKeyNotAllowed::WatchOnlyCoin` before any node is asked. The gate
described below stands unchanged; only the order-time refusal is relied on.

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

*Implemented (PR #319).* The wallet-only predicate of the Tendermint coin and
token (`tendermint_wallet_only`, `mm2src/coins/tendermint/tendermint_types.rs`)
now also answers `true` when the activation policy holds no local private key,
so order placement refuses the coin; a token uses its platform coin's policy.
The test builds the one key-less policy that exists today
(`PrivKeyPolicy::Trezor`) and checks a coin and a token of it. The refusal is
the order-time one only; a swap that has already started never asks.
The construction site now builds two policies, `PrivKeyPolicy::KeyPair` (iguana login) and
`PrivKeyPolicy::HDWallet` (HD login); both hold a local key, so the hazard stays latent.

**The gate must cover tokens as well as the platform coin.**
`TendermintToken::get_htlc_key_pair` delegates straight to the platform coin
(`mm2src/coins/tendermint/tendermint_token.rs:401`), so every IBC/CW20-style
token on a key-less platform coin inherits the hazard unchanged. A fix or a
regression test written only against the platform coin would miss them.

## Future package: show an HTLC auto-refund in the coin history

*Not in 0.5.0 (decision 2026-11-15).* An HTLC that expires and refunds itself on
the chain would appear in the coin's transaction history. It needs the history
fetch of item 4 first, and a check with the wallet applications before a new
transaction type is added to the history output.
