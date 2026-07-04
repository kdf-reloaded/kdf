# Chapter 36 -- Tendermint (Cosmos) V2 Activation & Token RPC Surface

**Status:** driving-spec (required port). This chapter specifies an **activation
RPC layer** over Tendermint (Cosmos-family) coin code that already exists in
reloaded; it does not introduce a new coin.

> **One-sentence claim:** the project shall expose the standard Komodo DeFi
> Framework **mmrpc 2.0** activation surface for Tendermint (Cosmos-family)
> coins -- a one-shot "platform coin with tokens" call and a single-token call --
> on top of the Tendermint coin and token types that reloaded already ships.

> **Treatment:** **T-PORT.** The Tendermint coin and token types, and their
> activation-parameter scaffolding, are present in reloaded (see §36.5). What is
> required is the **activation integration** (the platform-coin-with-tokens and
> token activation trait implementations for the Tendermint coin/token, plus the
> dispatcher routes for the method strings named below). The wire contract
> distilled here is the source of truth for that port.

> **Binding scope (R36).** Requirements bind observable behaviour, the public
> mmrpc-2.0 method strings and their request/response JSON field names, and
> externally *dictated* interop (Cosmos / Tendermint RPC endpoints, IBC denom and
> channel identification, the Cosmos secp256k1 public-key / account-id model,
> CAIP-style chain identification). Those public contracts are the source of
> truth, not this project's code. Private Rust types, helper decomposition, and
> internal module structure are informative and are **not** bound by this
> chapter.

> **Source of truth (informative).** The method strings, JSON field names, and
> error discriminants below are the published Komodo DeFi Framework API contract
> (the public KDF API documentation / `komodo-docs` coin-activation and
> Tendermint/token sections). Where this chapter and the public API docs
> disagree, the public API docs govern.

---

## 36.0 Executive summary

Tendermint coins follow the project's **platform-coin-with-tokens** activation
model: the native chain coin (the Cosmos-SDK fee/staking coin, e.g. ATOM, IRIS,
OSMO) is the *platform coin*, and its IBC / native (CW20-style and bank) assets
are its *child tokens*. The surface specified here is:

| Method string | Tier | Targets | Purpose |
| --- | --- | --- | --- |
| `enable_tendermint_with_assets` | mmrpc 2.0 | all (incl. WASM) | activate the Tendermint platform coin plus an inline batch of Tendermint (IBC / native) tokens in one call |
| `enable_tendermint_token` | mmrpc 2.0 | all (incl. WASM) | activate one Tendermint token against an already-active Tendermint platform |

Both methods use the mmrpc-2.0 envelope (`{"mmrpc":"2.0","method":...,"params":
{...},"id":...}`) and, on success, return `{"mmrpc":"2.0","result":{...},"id":
...}`. On error they return the standard mmrpc-2.0 error envelope carrying
`error`, `error_path`, `error_trace`, `error_type`, and `error_data`; only the
**`error_type` discriminant** and its HTTP status are bound below (the human-
readable `error` text is not part of the contract).

Both methods are driven by the **same generic activation framework** used by the
EVM (ch. 35), BCH/SLP, and Solana/SPL activation surfaces: the generic
platform-coin-with-tokens activator and the generic single-token activator. The
Tendermint port supplies the platform/token trait implementations that let those
generic activators drive Tendermint activation.

Tendermint activation is available on **all targets, including WASM**.

A long-running, **task-based** activation variant
(`task::enable_tendermint::{init,status,user_action,cancel}`) is also part of the
published KDF surface; it is specified in §36.6 and is delivered by the shared
platform-coin task-activation framework of ch. 48 (which wraps the one-shot
activation of this chapter as its unit of work).

---

## 36.1 `enable_tendermint_with_assets` -- platform coin with tokens

R36.1.1 The public RPC `enable_tendermint_with_assets` shall activate a
Tendermint platform coin together with an inline list of Tendermint tokens in a
single mmrpc-2.0 call. The mmrpc-2.0 `params` object carries the platform
`ticker` (string, required) plus the platform activation parameters of R36.1.2,
flattened at the same level as `ticker`.

R36.1.2 The platform activation parameters shall carry:

- `nodes` (array, required, non-empty) -- the Tendermint RPC endpoints. Each
  entry is an object carrying `url` (string, required) and an optional boolean
  selecting whether the endpoint is reached through the project's proxy
  (defaulting to off). No Tendermint node endpoints are embedded in the project;
  the node list is entirely caller-supplied.
- `tokens_params` (array, optional, default empty) -- the Tendermint tokens to
  enable alongside the platform. Each entry carries `ticker` (string, required)
  and the per-token activation parameters of R36.2.1.
- `tx_history` (boolean, optional, default **false**) -- whether to enable
  transaction-history tracking for the platform coin.
- `get_balances` (boolean, optional, default **true**) -- when true the result
  includes balances; when false the balance fields may be omitted to speed up
  activation (see R36.1.5).
- `path_to_address` (object, optional) -- the HD account/change/address selector
  picking which derived address is used (defaults to the first non-change
  address of the first account).
- `activation_params` (tagged object, optional) -- an alternative
  public-key / external-signer activation policy (R36.1.3). When omitted, the
  platform is activated with the in-context wallet private key (the default
  signing policy).

R36.1.3 The `activation_params` selector, when present, is a tagged object (tag
field `type`, payload field `params`) over the published non-private-key signing
policies:

- a watch-only / public-key policy carrying the caller-supplied Cosmos
  secp256k1 `pubkey` and an `is_ledger_connection` boolean flag indicating
  whether the public key originates from a Ledger device; and
- a WalletConnect policy carrying the WalletConnect session topic
  (`session_topic`).

The `pubkey` value follows the dictated Cosmos public-key JSON shape (a tagged
key object whose inner `value` carries the base64-encoded secp256k1 key); the
account id is derived from that key and the chain's address prefix. WalletConnect
activation is bound by ch. 22; this chapter binds only the activation-parameter
field names.

R36.1.4 The activation flow shall: reject an empty `nodes` list; construct the
multi-node Tendermint RPC client with failover across the supplied nodes; resolve
the chain identity and address prefix from coin configuration (the chain id is
required for Tendermint coins); build the platform coin under the selected
signing policy; then activate each requested token against it. A token whose
declared platform does not match the platform coin being activated shall be
rejected rather than partially activating the platform.

R36.1.5 The success `result` shall report:
- `ticker` -- the activated platform coin ticker;
- `address` -- the activated Cosmos account address;
- `current_block` -- the current Tendermint chain height;
- `balance` -- the platform coin balance (a `{spendable, unspendable}` balance
  object), present only when `get_balances` is true;
- `tokens_balances` -- a map of token ticker to balance object, present only when
  `get_balances` is true; and
- `tokens_tickers` -- the set of activated token tickers, present when
  `get_balances` is false (so a caller that skipped balances still learns which
  tokens were activated).

The `balance` / `tokens_balances` fields and the `tokens_tickers` field are
mutually exclusive views selected by the `get_balances` flag; absent fields are
omitted from the JSON object rather than emitted as null.

R36.1.6 `error_type` discriminants for this method, and their HTTP status, form
the bound error contract. Because the method is driven by the generic
platform-coin-with-tokens activator, the discriminants are those of the generic
platform-with-tokens error contract and shall include at least:

| `error_type` | HTTP status |
| --- | --- |
| `PlatformIsAlreadyActivated` | 400 |
| `PlatformConfigIsNotFound` | 400 |
| `AtLeastOneNodeRequired` | 400 |
| `UnexpectedPlatformProtocol` | 400 |
| `TokenConfigIsNotFound` | 400 |
| `UnexpectedTokenProtocol` | 400 |
| `CoinProtocolParseError` | 500 |
| `TokenProtocolParseError` | 500 |
| `PlatformCoinCreationError` | 500 |
| `PrivKeyNotAllowed` | 500 |
| `UnexpectedDerivationMethod` | 500 |
| `Transport` | 500 |
| `Internal` | 500 |

A request with an empty `nodes` list shall be rejected with `AtLeastOneNodeRequired`;
a request for an already-active platform shall be rejected with
`PlatformIsAlreadyActivated`; an unknown or mistyped platform/token in coin
configuration shall be rejected with the corresponding config-not-found /
unexpected-protocol discriminant rather than partially activating the coin.

---

## 36.2 `enable_tendermint_token` -- single token on an active platform

R36.2.1 The public RPC `enable_tendermint_token` shall activate a single
Tendermint token against an already-active Tendermint platform coin. The
mmrpc-2.0 `params` object carries:
- `ticker` (string, required) -- the token ticker; and
- `activation_params` (object, required) -- the per-token activation parameters.
  The Tendermint token currently defines no additional activation fields, so this
  is an (initially empty) object reserved for forward-compatible token options;
  it must still be present.

R36.2.2 The token's platform binding, decimals, and on-chain denomination
(`denom`) are taken from coin configuration (the token-protocol descriptor names
the platform, decimals, and denom). Activation resolves the token against the
named active platform coin and reads its balance.

R36.2.3 The success `result` shall report:
- `balances` -- a map of the activated address to its token balance object; and
- `platform_coin` -- the ticker of the platform coin the token is bound to.

R36.2.4 `error_type` discriminants for this method, and their HTTP status, are
those of the generic single-token error contract and shall include at least:

| `error_type` | HTTP status |
| --- | --- |
| `TokenIsAlreadyActivated` | 400 |
| `PlatformCoinIsNotActivated` | 400 |
| `TokenConfigIsNotFound` | 400 |
| `UnexpectedTokenProtocol` | 400 |
| `TokenProtocolParseError` | 500 |
| `UnsupportedPlatformCoin` | 500 |
| `UnexpectedDerivationMethod` | 500 |
| `Transport` | 500 |
| `Internal` | 500 |

A token whose platform coin is not active shall be rejected with
`PlatformCoinIsNotActivated`; a token already active shall be rejected with
`TokenIsAlreadyActivated`.

---

## 36.3 Token-protocol descriptor (dictated config contract)

R36.3.1 The Tendermint token-protocol descriptor used by coin configuration (and
resolved during token activation) follows the project's public coin-protocol JSON
contract. The Tendermint token case carries `platform` (string -- the platform
coin ticker the token belongs to), `decimals` (integer -- the token's decimal
precision), and `denom` (string -- the Cosmos / IBC on-chain denomination, e.g. a
`u<base>` bank denom or an `ibc/<HASH>` IBC denom). The `platform`, `decimals`,
and `denom` field names and the IBC denom format are the dictated public config /
interop contract; the Rust types that deserialise them are discretionary.

R36.3.2 The Tendermint platform-protocol descriptor carries the chain
identification (chain id and address prefix) and the average-block-time metadata
required to compute swap lock-time and confirmation behaviour. The chain id and
address prefix are dictated by the target Cosmos chain, not by this chapter.

---

## 36.4 Relationship to the IBC/HTLC swap layer (informative)

This activation surface sits **above** the Tendermint IBC/HTLC swap internals
specified in ch. 18. Activation establishes the platform coin, its RPC client,
the active address, and the set of enabled tokens; the swap, IBC-transfer, and
HTLC behaviours those activated coins then participate in are bound by ch. 18 and
the swap-v2 chapters. This chapter binds only the *activation* request/response
contract.

---

## 36.5 Implementation-substrate note (informative)

This note orients the implementer; it is not normative.

**Already present in reloaded:**
- the Tendermint coin type (the public `TendermintCoin` newtype wrapping a shared
  coin implementation) and the Tendermint token type (the public
  `TendermintToken` newtype), both already `MmCoinEnum` variants with staking,
  IBC, HTLC, swap, market, RPC, and WalletConnect operations;
- the activation-parameter scaffolding -- the token activation-parameter type and
  the Tendermint token-protocol descriptor type -- already present alongside the
  token;
- the **generic** V2 activation entrypoints -- the generic
  platform-coin-with-tokens activator and the generic single-token activator --
  the same machinery that already drives BCH/SLP, Solana/SPL, and EVM (ch. 35)
  activation;
- the legacy single-coin enable path explicitly rejects the Tendermint and
  Tendermint-token protocols and directs callers to these two mmrpc-2.0 methods,
  so the method strings are already the intended activation entry points.

**What must be added (scope of this chapter's port):**
- implement the **platform-coin-with-tokens activation trait** for the Tendermint
  platform coin and the **token activation trait** for the Tendermint token, so
  the generic activators above can drive Tendermint activation, including the
  platform/token request and result shapes of §36.1--§36.2;
- add the **dispatcher routes** for the two method strings in §36.0 so the
  mmrpc-2.0 layer reaches these handlers;
- ensure the surface compiles and is routed on **all targets including WASM**.

The placement of these implementations (which crate/module they live in) should
follow the existing activation layout used by the other platform coins (the
coins-activation crate); their internal decomposition is the implementer's
choice.

---

## 36.6 Task-based activation (delivered via ch. 48 substrate)

R36.6.1 The published KDF surface also exposes a long-running, task-based
Tendermint platform-activation family
`task::enable_tendermint::{init,status,user_action,cancel}`, following the same
init/status/user_action/cancel pattern as the other `task::enable_*` families.
This variant is the intended path for activation policies that need interactive
user actions (e.g. hardware-backed signing).

R36.6.2 This task variant is delivered by the **shared platform-coin
task-activation framework of ch. 48** -- the third sibling alongside the
standalone-coin and l2 task substrates. The framework wraps the one-shot
activation of §36.1 as its unit of work; the same dependency was recorded for the
EVM `task::enable_eth` family (ch. 35 §35.3), and ch. 48 unblocks both.

R36.6.3 `task::enable_tendermint::init` shall accept the same activation
parameters as `enable_tendermint_with_assets` (§36.1) and ultimately yield the
same success result shape (R36.1.5), with `status` surfacing in-progress states
and `user_action` supplying interactive confirmations, per ch. 48 §§48.1--48.3.
The family shall not be stubbed: it performs the real one-shot activation as its
unit of work, so the one-shot `enable_tendermint_with_assets` of §36.1 and the
task variant share a single activation path.

---

## 36.7 Acceptance criteria

- `enable_tendermint_with_assets` activates a Tendermint platform coin plus zero
  or more Tendermint tokens in one call and returns `ticker`, `address`,
  `current_block`, and -- when `get_balances` is true -- the platform `balance`
  and per-token `tokens_balances`, or, when `get_balances` is false, the
  `tokens_tickers` set (R36.1).
- A request with an empty `nodes` list is rejected with `AtLeastOneNodeRequired`,
  and an already-active platform with `PlatformIsAlreadyActivated`, using the
  bound HTTP status of R36.1.6 without partially activating the coin.
- `enable_tendermint_token` adds a single Tendermint token to an already-active
  Tendermint platform and returns the token `balances` map and the bound
  `platform_coin` ticker (R36.2); a token whose platform is not active is
  rejected with `PlatformCoinIsNotActivated`.
- Both methods are driven by the generic platform-coin-with-tokens / single-token
  activators (R36.0, R36.5) and carry the generic activation `error_type`
  discriminants and HTTP statuses of R36.1.6 and R36.2.4.
- The token-protocol descriptor resolves `platform`, `decimals`, and `denom` from
  coin configuration, accepting both bank and IBC (`ibc/<HASH>`) denoms (R36.3).
- Both method strings are routed and build on native **and** WASM targets (R36.5).
- The task-based `task::enable_tendermint::*` family is delivered via the shared
  platform-coin task-activation framework of ch. 48, wrapping the one-shot
  activation of §36.1 as its unit of work (R36.6).
