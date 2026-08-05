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
(`denom`) are taken from the token's `TENDERMINTTOKEN` `protocol_data` (§36.3.2):
`platform`, `decimals`, and `denom`. Activation resolves the token against the
named active platform coin and reads its balance over the platform's connection.

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

## 36.3 Coin-protocol `protocol_data` descriptors (dictated config contract)

The Tendermint platform coin and Tendermint token are each selected in coin
configuration by a `protocol` object whose `type` string is `TENDERMINT` or
`TENDERMINTTOKEN` respectively, and whose `protocol_data` object carries the
protocol-specific fields distilled below. The `type` tag string, the
`protocol_data` container, and every field name and value form listed here are
the dictated public coin-config / Cosmos-interop contract; the Rust types that
deserialise them are discretionary. The `protocol_data` fields are **the source
of the platform coin's and token's chain parameters** -- they are *not* read from
the top-level coin-conf object nor from the activation request. Activation MUST
therefore parse them from `protocol_data` and MUST NOT drop fields it later needs.

### 36.3.1 `TENDERMINT` platform `protocol_data`

R36.3.1 The `TENDERMINT` `protocol_data` object shall carry the following fields.
Each row states the wire key, its JSON type, whether it is required, and the
functional role activation must give it.

| `protocol_data` key | JSON type | Required? | Functional role |
| --- | --- | --- | --- |
| `denom` | string | **required** | The platform chain's native base denomination (the smallest-unit bank denom, e.g. `uatom`, `uiris`, `uosmo`). It is the denom the platform coin queries for its own balance, denominates transaction fees in, and signs bank/HTLC/IBC messages against. Without it the platform coin has no base unit and cannot report balance, build fees, or sign. |
| `decimals` | integer | **required** | The number of decimal places between the base denom and one whole coin. Used to convert base-unit balances and amounts to and from human-readable decimal values. It must be **18 or lower**; a value above 18 shall fail activation as invalid protocol data (a bounded-input check, not a free-form message). Without it, all balance and amount conversion for the platform coin is undefined. |
| `account_prefix` | string | **required** | The bech32 human-readable prefix (HRP) of the chain's account addresses (e.g. `cosmos`, `iaa`, `osmo`, per BIP-173). Used to derive the activated account address from the public key. Without it no address can be formed, so the coin cannot activate. |
| `chain_id` | string | **required** | The Cosmos/Tendermint chain identifier (e.g. `cosmoshub-4`, `irishub-1`, `osmosis-1`). Bound into the signing document for every transaction and into the WalletConnect/Keplr session. Without it transactions cannot be signed or broadcast to the correct chain. |
| `gas_price` | number (float) | optional | The default gas price (in the base denom per gas unit) used for fee estimation when a withdraw/transaction does not override it. When absent, a built-in default gas price applies. Its absence is **benign** -- activation and all operations still succeed using the default. |
| `ibc_channels` | object (map) | optional | A map whose **keys are the bech32 account-prefix (HRP) of a target chain** and whose **values are integer IBC channel numbers** on this chain's transfer port toward that target (the integer `N` corresponds to the ICS-20 channel identifier `channel-N`). Used to resolve the outbound IBC transfer channel for a given destination prefix during IBC transfer / cross-chain HTLC routing. When absent it defaults to an empty map. Its absence disables *manually-configured* channel lookup for a destination prefix; a healthy-channel discovery path may still resolve channels at runtime, so absence degrades but does not categorically break IBC routing. |
| `chain_registry_name` | string | optional / informational | The chain's canonical name in the Cosmos chain registry (e.g. `cosmoshub`, `irishub`, `osmosis`). It is **not consumed by any activation or swap code path** distilled here; it is carried in config for external/registry-driven tooling. Its absence is benign for all in-framework functional paths. |

> **Note (extra config keys).** Real coin configs may also carry additional
> Tendermint tuning keys in `protocol_data` (e.g. a minimum-balance-for-IBC-routing
> hint). Any such field is optional and, when consumed, only adjusts a threshold
> with a built-in default; unknown/unconsumed keys must not cause activation to
> fail. The coin-protocol deserialiser therefore must **not** be locked to
> reject unknown `protocol_data` fields for these arms.

R36.3.2 During platform activation the fields of R36.3.1 shall be consumed as
follows: `account_prefix` derives the activated address; `denom` + `decimals`
denominate and scale the platform balance reported in the activation result and
every later balance/amount conversion; `chain_id` parameterises transaction
signing and the external-signer session; `gas_price` seeds fee estimation (or the
default when omitted); and `ibc_channels` seeds the destination-prefix -> channel
map used by the IBC/HTLC layer of ch. 18. `chain_registry_name` is retained by
config but need not be consumed. A `TENDERMINT` `protocol_data` missing any of the
four required fields (`denom`, `decimals`, `account_prefix`, `chain_id`), or with
`decimals` above 18, shall fail activation with an invalid-protocol-data error
under the R36.1.6 contract and shall not partially activate the coin.

### 36.3.2 `TENDERMINTTOKEN` token `protocol_data`

R36.3.3 The `TENDERMINTTOKEN` `protocol_data` object shall carry the following
fields.

| `protocol_data` key | JSON type | Required? | Functional role |
| --- | --- | --- | --- |
| `platform` | string | **required** | The ticker of the `TENDERMINT` platform coin the token rides on. Token activation resolves the token against this already-active platform; a mismatch or inactive platform is rejected (R36.2.4). |
| `decimals` | integer | **required** | The token's decimal precision, used to scale its base-denom balance to a human value. |
| `denom` | string | **required** | The token's on-chain base denomination -- either a native bank denom or an IBC denom of the form `ibc/<HASH>` (the IBC-derived denom hash). It is the denom the token queries for its balance and transacts against on the platform's connection. |
| `gas_price` | number (float) | optional / informational | A per-token gas-price hint. Tendermint tokens transact over the **platform coin's** connection and use the platform coin's gas/fee settings, so this key is **not consumed** by token activation or token operation in the behaviour distilled here. Its presence or absence is benign. |

R36.3.4 During token activation the token is constructed from `decimals` and
`denom` and bound to the platform named by `platform`; the token's balance is then
read on the platform coin's connection and returned per R36.2.3. The token does
**not** open its own chain connection and does **not** source `denom`/`decimals`
from anywhere other than its own `protocol_data`.

### 36.3.3 Reloaded status and required additions

> **Upstream divergence (informative).** Reloaded currently under-captures both
> Tendermint `protocol_data` arms and, because the coin-protocol enum does not
> reject unknown fields, silently **drops** the surplus config keys rather than
> erroring on them. The functional impact of each dropped field is:
>
> - **`TENDERMINT.denom` (dropped -> functional gap, MUST add):** reloaded's
>   `TENDERMINT` arm captures only `account_prefix` and `chain_id`, so the
>   platform coin has **no base denomination**. Without `denom` the platform coin
>   cannot query its own balance, denominate fees, or sign bank/HTLC/IBC messages.
>   `denom` is required and must be added.
> - **`TENDERMINT.decimals` (dropped -> functional gap, MUST add):** likewise
>   dropped by reloaded. Without it, platform balance/amount scaling is undefined
>   (and the `decimals <= 18` bound cannot be enforced). Required; must be added.
> - **`TENDERMINT.ibc_channels` (dropped -> partial gap, SHOULD add):** without
>   the configured destination-prefix -> channel map, manually-configured IBC
>   channel resolution is unavailable; runtime healthy-channel discovery may still
>   cover some routes, but configured IBC transfer/HTLC routing toward a given
>   destination prefix is not guaranteed. Add to restore configured IBC routing.
> - **`TENDERMINT.gas_price` (dropped -> benign):** absence falls back to the
>   built-in default gas price; activation and operations still succeed. Adding it
>   only restores per-chain fee tuning; safe to defer.
> - **`TENDERMINT.chain_registry_name` (dropped -> benign):** not consumed by any
>   distilled functional path; safe to ignore.
> - **`TENDERMINTTOKEN.gas_price` (dropped -> benign):** tokens use the platform
>   coin's fee settings; the token-level key is not consumed. Safe to ignore.
> - **`TENDERMINTTOKEN.platform` / `decimals` / `denom`:** already captured by
>   reloaded and consumed correctly (R36.3.3--R36.3.4); no change needed.
>
> Net: reloaded's `TENDERMINT` arm must be extended to carry `denom` and
> `decimals` (required) and `ibc_channels` (recommended); `gas_price` and
> `chain_registry_name` are optional/benign. The `TENDERMINTTOKEN` arm needs no
> functional change. The enum must continue to tolerate unknown `protocol_data`
> keys (no `deny_unknown_fields`) so benign surplus keys do not fail activation.

> **Status update (reloaded).** Implemented (commit 40645ebad). The `TENDERMINT`
> arm now carries all three required/recommended fields:
>
> - **`denom` (required):** platform coin base denomination (e.g., `"uatom"`);
>   passed to `TendermintProtocolInfo` and consumed by balance queries, fee
>   calculations, and bank/HTLC/IBC message construction.
> - **`decimals` (required):** platform coin display decimals, bounded by a custom
>   deserializer at ≤18; enables correct scaling of amounts and fee representation.
> - **`ibc_channels` (recommended):** optional map from destination bech32 HRP to
>   ICS-20 channel number; passed to `TendermintProtocolInfo.ibc_channels` and
>   available to the IBC/HTLC layer via `ibc_channel_for_prefix()` for configured
>   channel routing (ch. 18 §18.4). Defaults to empty when absent.
>
> The `TENDERMINTTOKEN` arm captures `platform`, `decimals`, and `denom` correctly
> (no change needed). Benign fields (`gas_price`, `chain_registry_name`) continue
> to be silently dropped; the enum still tolerates unknown `protocol_data` keys
> so surplus config does not fail activation.

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
- A `TENDERMINT` platform coin activates only when its `protocol_data` supplies
  all four required fields -- `denom`, `decimals`, `account_prefix`, `chain_id` --
  and `decimals <= 18`; the platform balance is reported in the correct base
  denom scaled by `decimals`, the address uses the `account_prefix` HRP, and
  transactions sign against `chain_id`. A `protocol_data` missing any required
  field, or with `decimals > 18`, fails activation without partially activating
  (R36.3.1--R36.3.2).
- A `TENDERMINT` coin whose `protocol_data` supplies `ibc_channels` can resolve
  an outbound IBC transfer channel for a configured destination account-prefix
  (mapping prefix -> integer channel number `N` -> `channel-N`); omitting
  `gas_price` or `chain_registry_name` does not prevent activation or operation
  (R36.3.1).
- The `TENDERMINTTOKEN` protocol resolves `platform`, `decimals`, and `denom`
  from its `protocol_data`, accepting both bank and IBC (`ibc/<HASH>`) denoms; a
  token-level `gas_price` key, if present, is accepted and ignored (R36.3.2).
- Surplus/unknown `protocol_data` keys on either arm do not cause activation to
  fail (R36.3.3).
- Both method strings are routed and build on native **and** WASM targets (R36.5).
- The task-based `task::enable_tendermint::*` family is delivered via the shared
  platform-coin task-activation framework of ch. 48, wrapping the one-shot
  activation of §36.1 as its unit of work (R36.6).
