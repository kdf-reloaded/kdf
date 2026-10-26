# Chapter 36 -- Tendermint (Cosmos) V2 Activation & Token RPC Surface

**Status:** driving-spec (required port). This chapter specifies an **activation
RPC layer** over Tendermint (Cosmos-family) coin code that already exists in
reloaded; it does not introduce a new coin.

> **One-sentence claim:** the project shall expose the standard Komodo DeFi
> Framework **mmrpc 2.0** activation surface for Tendermint (Cosmos-family)
> coins -- a one-shot "platform coin with tokens" call, a single-token call, and
> the task-managed platform activation family -- with request fields, success
> shapes, `error_type` discriminants, `error_data` shapes, HTTP statuses and
> error placement that are wire-identical to the reference implementation named
> in §36.0.1.

> **Treatment:** **T-PORT.** The Tendermint coin and token types, and their
> activation-parameter scaffolding, are present in reloaded (see §36.5). What is
> required is the **activation integration** (the platform-coin-with-tokens and
> token activation trait implementations for the Tendermint coin/token, plus the
> dispatcher routes for the method strings named below). The wire contract
> distilled here is the source of truth for that port.

> **Binding scope (R36).** Requirements bind observable behaviour, the public
> mmrpc-2.0 method strings and their request/response JSON field names, the
> `error_type` discriminants with their `error_data` shapes and HTTP statuses,
> and externally *dictated* interop (Cosmos / Tendermint RPC endpoints, IBC denom
> and channel identification, the Cosmos secp256k1 public-key / account-id
> model, CAIP-style chain identification). Private Rust types, helper
> decomposition, and internal module structure are informative and are **not**
> bound by this chapter. Human-readable `error` text, `error_path` and
> `error_trace` are never bound.

> **Source of truth (informative).** The method strings, JSON field names, and
> error discriminants below follow the published Komodo DeFi Framework API
> documentation (the `enable_tendermint_with_assets`, `enable_tendermint_token`
> and `task::enable_tendermint` pages and the error-type catalogue). Where those
> pages are explicit (field table and example agree, or only an example exists
> and nothing contradicts it), they govern. Where they are silent, ambiguous or
> contradict themselves -- including every HTTP status code, which the public
> docs state nowhere -- this chapter binds what the reference implementation of
> §36.0.1 does, by maintainer decision.

> **Availability in the beta (maintainer decision 2026-10-26).** The
> activation surface of this chapter is unaffected: Tendermint platform coins
> and tokens activate, report balances and `withdraw` as bound here. Tendermint
> swap support is not finished, so for the coming beta every activated
> Tendermint coin and token is wallet-only: while the build constant
> `TENDERMINT_SWAPS_AVAILABLE` (`coins/lp_coins_ops.rs`) is `false`,
> `setprice`, `buy`, `sell` and `start_swap` refuse it with the usual "… is
> wallet only" answer and an order with a Tendermint coin saved by an
> earlier version is cancelled at startup; a swap that
> has already started still completes or is recovered. Swaps are planned to
> return in a later release ([Chapter 18](18-tendermint-ibc-htlc.md)).

---

## 36.0 Executive summary

Tendermint coins follow the project's **platform-coin-with-tokens** activation
model: the native chain coin (the Cosmos-SDK fee/staking coin, e.g. ATOM, IRIS,
OSMO) is the *platform coin*, and its IBC / native bank assets are its *child
tokens*. The surface specified here is:

| Method string | Tier | Targets | Purpose |
| --- | --- | --- | --- |
| `enable_tendermint_with_assets` | mmrpc 2.0 | all (incl. WASM) | activate the Tendermint platform coin plus an inline batch of Tendermint (IBC / native) tokens in one call (§36.1) |
| `enable_tendermint_token` | mmrpc 2.0 | all (incl. WASM) | activate one Tendermint token against an already-active Tendermint platform (§36.2) |
| `task::enable_tendermint::{init,status,user_action,cancel}` | mmrpc 2.0 (task) | all (incl. WASM) | task-managed platform activation wrapping §36.1 (§36.6) |

All methods use the mmrpc-2.0 envelope. The envelope, the adjacently tagged
`error_type` / `error_data` error object (a unit variant omits `error_data`),
the `id` echo, the HTTP-status policy, the task-status object and the
task-framework errors of `status` / `user_action` / `cancel` (`NoSuchTask`,
`TaskFinished`, ...) are bound once for every activation family by
[Chapter 55](55-coin-activation-wire-contract.md) §55.1 and §55.2; this chapter
binds only the Tendermint-specific request fields, success shapes and the
failure -> `error_type` mapping of each method.

There is **no** `task::enable_tendermint_token::*` family: the reference routes
none, and this chapter binds none.

### 36.0.1 Reference version

R36.0.1 The reference implementation for every behaviour this chapter binds
where the public docs are silent is the GLEEC KDF release tag **`v3.0.0-beta`**.
For every Tendermint activation wire shape bound in this chapter (request
fields, success shapes, error discriminants, `error_data` shapes, HTTP
statuses, placement), the netid-8762 legacy reference `v2.6.0-beta` behaves
identically; no Tendermint activation behaviour is network- or
reference-version-dependent.

### 36.0.2 Shared numeric and shape conventions

R36.0.2 In every success result of this chapter, balances (`spendable`,
`unspendable`) are JSON **strings** holding a plain decimal number; no
`*_rat` / `*_fraction` form is emitted. `current_block` and task ids are JSON
integers. A balance object is always `{"spendable": <string>, "unspendable":
<string>}`; for Tendermint the `unspendable` member is always `"0"`.

R36.0.3 Where an `error_data` object below carries a `protocol` member, its
value is the offending coin's protocol re-serialized in the canonical
coin-configuration form `{"type": <string>, "protocol_data": <object>}`, with
`protocol_data` omitted for a protocol that carries no protocol data (the
public-docs example `{"type": "UTXO"}`). It is not a verbatim copy of the
configuration text.

> **Upstream divergence (informative).** For a `UTXO` protocol without protocol
> data the reference (both tags) emits `"protocol_data": null` rather than
> omitting the member. The public-docs example omits it and is explicit, so the
> omitted form is bound; clients written against either form read only
> `protocol.type` in practice.

---

## 36.1 `enable_tendermint_with_assets` -- platform coin with tokens

R36.1.1 The public RPC `enable_tendermint_with_assets` shall activate a
Tendermint platform coin together with an inline list of Tendermint tokens in a
single mmrpc-2.0 call. All request fields of R36.1.2 sit **flat** in `params`
(there is no `activation_params` wrapper around them). Fields not listed are
ignored without error.

R36.1.2 **Request fields.**

| Field | JSON type | Required / default | Effect |
| --- | --- | --- | --- |
| `ticker` | string | **required** | Platform coin ticker; must name a `TENDERMINT`-protocol entry in the coins configuration. |
| `nodes` | array of node objects | **required** (the array itself; an empty array is accepted by the parser and then fails per R36.1.6) | Caller-supplied Tendermint RPC endpoints with failover; no endpoints are embedded in the project. |
| `nodes[].url` | string | **required** | Tendermint RPC endpoint URL. A URL that cannot be turned into an HTTP client fails per R36.1.6. |
| `nodes[].komodo_proxy` | boolean | optional, default `false` | When `true`, requests to that endpoint are signed for the komodo-defi-proxy with this KDF instance's P2P identity key. |
| `nodes[].api_url`, `nodes[].grpc_url`, `nodes[].ws_url` | string | optional | Accepted (they appear in the public-docs examples) and **ignored**. |
| `tokens_params` | array of token requests | **required** (may be empty) | Tendermint tokens to activate alongside the platform. |
| `tokens_params[].ticker` | string | **required** | Token ticker. |
| `tokens_params[].protocol` | object (coin-protocol form) | optional | Custom-token activation: when present, the token is activated from this protocol object instead of the coins configuration (R36.1.4 step 4). Any other member of a token request (e.g. `required_confirmations`) is accepted and ignored. |
| `tx_history` | boolean | optional, default `false` | When `true`, background transaction-history fetching is started for the platform coin after successful activation. |
| `get_balances` | boolean | optional, default `true` | Selects the balance view or the ticker view of the result (R36.1.5). |
| `path_to_address` | object `{account_id: integer, chain: "External"\|"Internal", address_id: integer}` | optional, default `{0, "External", 0}` | In HD-wallet mode selects which derived address is activated (under the coin's configured derivation path); without HD mode it has no effect. |
| `activation_params` | tagged object | optional | Non-private-key signing policy (R36.1.3). When omitted, the platform activates under the context signing policy. |
| `priv_key_policy` | string | optional | Accepted and **ignored** (the docs list `ContextPrivKey` / `Trezor`; the reference never treats a Tendermint request as a hardware-policy request). |
| `required_confirmations`, `requires_notarization`, `mm2` | integer / boolean / integer | optional | Accepted and **ignored**. A Tendermint coin always reports zero required confirmations and no notarization requirement, irrespective of these fields. |

> **Upstream divergence (informative).** The public docs list
> `required_confirmations` with default 3 and `requires_notarization` with
> default false, and `priv_key_policy` as a string; the reference ignores all
> three for Tendermint. Because the docs do not state what observable effect the
> fields have, the reference behaviour is bound. The docs mark `nodes`,
> `ticker` and `tokens_params` required, which the reference also enforces.

R36.1.3 **`activation_params` forms.** When present, `activation_params` shall
be accepted in each of the following forms:

1. *Public-key form* -- `{"type": "WithPubkey", "params": {"pubkey": <key>,
   "is_ledger_connection": <boolean>}}`. `is_ledger_connection` is required. The
   key object is `{"type": "secp256k1" | "ed25519", "value": [<integer 0..255>,
   ...]}` -- the raw public-key bytes as a JSON array of integers (not base64,
   not hex). The activated account id is derived from this key and the chain's
   `account_prefix`; the coin activates watch-only (Keplr-style, flagged as a
   Ledger connection when `is_ledger_connection` is true).
2. *WalletConnect form* -- `{"type": "WalletConnect", "params":
   {"session_topic": <string>}}`. The account and its public key are fetched
   over the WalletConnect session for the coin's `chain_id` (ch. 22 binds the
   WalletConnect side); whether the session is a Ledger-backed one is taken
   from the session.
3. *Public-docs WalletConnect form* -- `{"priv_key_policy": {"type":
   "WalletConnect", "data": <session topic string>}}`, shown by the published
   example. It shall be treated exactly as form 2 with `session_topic` = `data`.

Any other shape of `activation_params` (unknown `type`, missing `type` or
`params` for forms 1-2, a key `type` other than the two listed, a `value` that is
not an array of byte integers, a key of invalid length) is a request-parse
failure and shall be reported as the dispatcher-level `InvalidRequest` of
[Chapter 55](55-coin-activation-wire-contract.md) §55.1 -- never as a crash.

> **Upstream divergence (informative).** The reference accepts forms 1 and 2
> only; it rejects form 3 (the published example) as a request-parse failure.
> Form 3 is bound because the public docs are explicit about it and accepting it
> cannot break a client that uses forms 1-2.

R36.1.4 **Activation semantics.** The observable outcome of a request shall be
as if the following checks and actions ran in this order, the first failing one
determining the error of R36.1.6:

1. If a coin with `ticker` is already active, reject (`PlatformIsAlreadyActivated`).
2. Resolve the platform's coins-configuration entry and its protocol
   (`PlatformConfigIsNotFound`, `CoinProtocolParseError`,
   `UnexpectedPlatformProtocol`).
3. Build the platform coin without contacting any node: validate the protocol
   data (`decimals` at most 18), the coin configuration (e.g. `avg_blocktime`
   present and in range), the signing policy (R36.1.3, HD derivation config and
   `path_to_address` in HD mode; a hardware-wallet context cannot sign for
   Tendermint), and that `nodes` is non-empty and every `url` yields a client.
   Any failure here is `PlatformCoinCreationError`. A WalletConnect activation
   contacts the WalletConnect peer in this step; its failure is also
   `PlatformCoinCreationError`.
4. Resolve every `tokens_params` entry: from the coins configuration when no
   `protocol` is supplied, from the supplied `protocol` otherwise (custom
   token; supplying a `protocol` for a ticker that **is** in the configuration
   is rejected). Then check that every token's protocol names this platform as
   its `platform`. Token Tendermint objects are created without contacting any
   node.
5. Query the chain over the node set: the current height and, when
   `get_balances` is true, the platform and token balances. This is the first
   step that contacts a node, so unreachable / failing nodes surface here
   (`PlatformCoinCreationError`).
6. When `tx_history` is true, start background history fetching.
7. Register the platform coin and its tokens; a concurrent activation that
   registered the same platform first yields `PlatformIsAlreadyActivated`.

No failure leaves the platform or any of its tokens partially registered.

R36.1.5 **Success result** (HTTP 200):

| Field | JSON type | Presence | Content |
| --- | --- | --- | --- |
| `ticker` | string | always | the platform ticker as requested |
| `address` | string | always | the activated bech32 account address (HRP = `account_prefix`) |
| `current_block` | integer | always | current Tendermint chain height |
| `balance` | balance object | only when `get_balances` is true | platform balance in display units (R36.0.2) |
| `tokens_balances` | object (map) | only when `get_balances` is true | token ticker -> balance object, one entry per token activated in this call; `{}` when none |
| `tokens_tickers` | array of strings | only when `get_balances` is false | tickers of the tokens activated in this call (order unspecified; `[]` when none) |

Absent fields are omitted, never emitted as `null`.

> **Upstream divergence (informative).** The public docs' response table types
> `tokens_balances` as an array of AddressInfo objects, while every published
> example shows the ticker-keyed map above; the reference emits the map. The map
> is bound.

R36.1.6 **Errors of `enable_tendermint_with_assets`** (all mmrpc-level). The
bound mapping is:

| `error_type` | Returned when | `error_data` | HTTP |
| --- | --- | --- | --- |
| `PlatformIsAlreadyActivated` | the platform ticker is already active (step 1), or lost a registration race (step 7) | string: the platform ticker | 400 |
| `PlatformConfigIsNotFound` | no coins-configuration entry for `ticker` | string: the requested ticker | 400 |
| `CoinProtocolParseError` | the platform's `protocol` object does not parse (incl. a missing required `protocol_data` field, §36.3) | object `{ticker: string, error: string}` | 500 |
| `UnexpectedPlatformProtocol` | the platform's protocol is not `TENDERMINT` | object `{ticker: string, protocol: object}` (R36.0.3) | 400 |
| `TokenConfigIsNotFound` | a `tokens_params` ticker without `protocol` has no coins-configuration entry | string: the token ticker | 400 |
| `TokenProtocolParseError` | a token's `protocol` does not parse | object `{ticker: string, error: string}` | 500 |
| `UnexpectedTokenProtocol` | a token's protocol is not `TENDERMINTTOKEN` | object `{ticker: string, protocol: object}` (R36.0.3) | 400 |
| `CustomTokenError` | a `tokens_params` entry carries `protocol` for a ticker that exists in the configuration | object, externally tagged: `{"DuplicateTickerInConfig": {"ticker_in_config": string}}` | 500 |
| `PlatformCoinMismatch` | a token's protocol `platform` differs from the requested platform ticker | absent (unit variant) | 400 |
| `PlatformCoinCreationError` | any failure of step 3 (incl. an **empty `nodes` array**, an unusable node URL, `decimals` above 18, missing `avg_blocktime`, unsupported signing context, WalletConnect account fetch failure) or of step 5 (incl. **all nodes unreachable / failing**) | object `{ticker: string, error: string}` -- `ticker` is the platform ticker | 500 |
| `Internal` | an internal failure (context, history storage, token address derivation) | string | 500 |

**Unresponsive nodes.** When every node in `nodes` is unreachable, times out or
answers with an HTTP error, `enable_tendermint_with_assets` shall return
`PlatformCoinCreationError` with `error_data` `{ticker, error}` and HTTP **500**.
It shall **not** return `Transport` and shall not return HTTP 502.

> **Upstream divergence (informative).** The public Tendermint page lists
> `Transport` among the error types without an example; the reference never
> produces it for Tendermint (node failures are wrapped into
> `PlatformCoinCreationError`). This differs from the EVM family, whose
> unresponsive-node error is `Transport` (ch. 35); the per-family table is in
> [Chapter 55](55-coin-activation-wire-contract.md) §55.3.

R36.1.7 **Shared discriminants not produced by Tendermint.** The
platform-with-tokens `error_type` set shared with EVM / BCH / Solana also
contains `Transport` (string, 502), `PrivKeyPolicyNotAllowed` (string, 500 --
the catalogue name; the docs' method tables say `PrivKeyNotAllowed`, which is
not a wire name), `UnexpectedDerivationMethod` (string, 500),
`AtLeastOneNodeRequired` (string, 400), `InvalidPayload` (string, 400),
`FailedSpawningBalanceEvents` (string, 400), `WalletConnectError` (string,
400), `UnexpectedDeviceActivationPolicy` (absent, 400), `NoSuchTask` (integer,
400) and `TaskTimedOut` (object `{duration: {secs, nanos}}`, 500). Tendermint
activation shall not produce any of them; their shapes and statuses are bound
where they are produced (ch. 35, ch. 55).

---

## 36.2 `enable_tendermint_token` -- single token on an active platform

R36.2.1 **Request fields.**

| Field | JSON type | Required / default | Effect |
| --- | --- | --- | --- |
| `ticker` | string | **required** | Token ticker. |
| `activation_params` | object | **required** (may be `{}`) | Present for forward compatibility; its members (e.g. the docs' `required_confirmations`) are accepted and ignored. A request without it is a request-parse failure (`InvalidRequest`, ch. 55 §55.1). |
| `protocol` | object (coin-protocol form) | optional | Custom-token activation from the supplied `TENDERMINTTOKEN` protocol instead of the coins configuration. |

R36.2.2 The token's platform binding, decimals, and on-chain denomination are
taken from its `TENDERMINTTOKEN` `protocol_data` (§36.3.2). Activation shall,
in observable order: reject an already-active token; resolve the token's
configuration / protocol (or the custom protocol); find the named platform
among the active coins; check that it is a Tendermint platform coin; build the
token on the platform's connection; read the token balance over the platform's
nodes; register the token; and only then add it to the platform's token set,
so a failed activation leaves the platform's token set unchanged.

R36.2.3 **Success result** (HTTP 200):

| Field | JSON type | Content |
| --- | --- | --- |
| `balances` | object (map) | exactly one entry: the activated account address -> balance object (R36.0.2) |
| `platform_coin` | string | the ticker of the platform coin the token is bound to |

R36.2.4 **Errors of `enable_tendermint_token`** (all mmrpc-level):

| `error_type` | Returned when | `error_data` | HTTP |
| --- | --- | --- | --- |
| `TokenIsAlreadyActivated` | the token is already active, or lost a registration race | string: the token ticker | 400 |
| `TokenConfigIsNotFound` | no coins-configuration entry and no `protocol` supplied | string: the token ticker | 400 |
| `TokenProtocolParseError` | the token's `protocol` does not parse | object `{ticker: string, error: string}` | 500 |
| `UnexpectedTokenProtocol` | the token's protocol is not `TENDERMINTTOKEN` | object `{ticker: string, protocol: object}` (R36.0.3) | 400 |
| `CustomTokenError` | `protocol` supplied for a ticker that exists in the configuration | object `{"DuplicateTickerInConfig": {"ticker_in_config": string}}` | 400 |
| `PlatformCoinIsNotActivated` | the token's `platform` is not an active coin | string: the **platform** ticker | 400 |
| `UnsupportedPlatformCoin` | the token's `platform` is active but is not a Tendermint platform coin | object `{platform_coin_ticker: string, token_ticker: string}` | 500 |
| `CouldNotFetchBalance` | the token balance could not be read -- incl. **all platform nodes unreachable / failing** | string | 500 |
| `Internal` | token construction or address derivation failed internally | string | 500 |

The shared single-token `error_type` set also contains `Transport` (string,
500), `InvalidConfig` (string, 500), `InvalidPayload` (string, 400),
`PrivKeyPolicyNotAllowed` (string, 500), `UnexpectedDerivationMethod` (500) and
`PlatformCoinMismatch` (absent, 400); `enable_tendermint_token` shall not
produce them.

**Unresponsive nodes.** When the platform's nodes are all unreachable or
failing, `enable_tendermint_token` shall return `CouldNotFetchBalance` (string)
with HTTP **500**.

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
| `decimals` | integer (0-255 by type) | **required** | The number of decimal places between the base denom and one whole coin. Used to convert base-unit balances and amounts to and from human-readable decimal values. It must be **18 or lower**; a value above 18 shall fail activation with `PlatformCoinCreationError` (R36.1.6), not with a protocol parse error. Without it, all balance and amount conversion for the platform coin is undefined. |
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
four required fields (`denom`, `decimals`, `account_prefix`, `chain_id`) shall
fail activation with `CoinProtocolParseError`; one whose `decimals` exceeds 18
shall fail with `PlatformCoinCreationError` (R36.1.6). Neither partially
activates the coin.

### 36.3.2 `TENDERMINTTOKEN` token `protocol_data`

R36.3.3 The `TENDERMINTTOKEN` `protocol_data` object shall carry the following
fields.

| `protocol_data` key | JSON type | Required? | Functional role |
| --- | --- | --- | --- |
| `platform` | string | **required** | The ticker of the `TENDERMINT` platform coin the token rides on. Token activation resolves the token against this already-active platform; inline activation rejects a token whose `platform` differs from the platform being activated (`PlatformCoinMismatch`, R36.1.6); single-token activation rejects an inactive or non-Tendermint platform (R36.2.4). |
| `decimals` | integer | **required** | The token's decimal precision, used to scale its base-denom balance to a human value. |
| `denom` | string | **required** | The token's on-chain base denomination -- either a native bank denom or an IBC denom of the form `ibc/<HASH>` (the IBC-derived denom hash). It is the denom the token queries for its balance and transacts against on the platform's connection. |
| `gas_price` | number (float) | optional / informational | A per-token gas-price hint. Tendermint tokens transact over the **platform coin's** connection and use the platform coin's gas/fee settings, so this key is **not consumed** by token activation or token operation in the behaviour distilled here. Its presence or absence is benign. |

R36.3.4 During token activation the token is constructed from `decimals` and
`denom` and bound to the platform named by `platform`; the token's balance is then
read on the platform coin's connection and returned per R36.2.3. The token does
**not** open its own chain connection and does **not** source `denom`/`decimals`
from anywhere other than its own `protocol_data`.

### 36.3.3 Reloaded status and required additions

> **Status update (reloaded).** Implemented (commit 40645ebad). The `TENDERMINT`
> arm carries `denom`, `decimals` and `ibc_channels` in addition to
> `account_prefix` and `chain_id`; the `TENDERMINTTOKEN` arm captures
> `platform`, `decimals`, and `denom`. Benign fields (`gas_price`,
> `chain_registry_name`) are silently dropped; the enum tolerates unknown
> `protocol_data` keys so surplus config does not fail activation.

> **Reloaded status (informative).** Reloaded enforces the `decimals <= 18`
> bound while *parsing* the coin protocol, so a value above 18 currently fails
> with `CoinProtocolParseError` (HTTP 500) instead of the bound
> `PlatformCoinCreationError` (HTTP 500, `{ticker, error}`). The bound must move
> from protocol parsing to platform-coin construction (R36.3.2).

---

## 36.4 Relationship to the IBC/HTLC swap layer (informative)

This activation surface sits **above** the Tendermint IBC/HTLC swap internals
specified in ch. 18. Activation establishes the platform coin, its RPC client,
the active address, and the set of enabled tokens; the swap, IBC-transfer, and
HTLC behaviours those activated coins then participate in are bound by ch. 18 and
the swap-v2 chapters. This chapter binds only the *activation* request/response
contract. In this beta the activated coins take part in no new swap (see the
availability note at the top of this chapter).

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
  Tendermint-token protocols and directs callers to these mmrpc-2.0 methods.

> **Status update (reloaded).** Implemented. The platform-coin-with-tokens
> activation trait is implemented for the Tendermint platform coin and the token
> activation trait for the Tendermint token; `enable_tendermint_with_assets` and
> `enable_tendermint_token` are both routed on native and WASM. The long-running
> task family `task::enable_tendermint::*` is also implemented, via the
> [Chapter 48](48-platform-coin-task-activation.md) substrate (§36.6). The wire
> deviations still open against this chapter are listed in §36.7.

The placement of these implementations (which crate/module they live in) should
follow the existing activation layout used by the other platform coins (the
coins-activation crate); their internal decomposition is the implementer's
choice.

---

## 36.6 Task-based activation (delivered via [Chapter 48](48-platform-coin-task-activation.md) substrate)

R36.6.1 The published KDF surface exposes the task-managed Tendermint platform
activation family `task::enable_tendermint::{init,status,user_action,cancel}`,
routed on the same targets as `enable_tendermint_with_assets`. It is delivered
by the shared platform-coin task-activation framework of
[Chapter 48](48-platform-coin-task-activation.md), which runs the one-shot
activation of §36.1 as its unit of work (the family is not a stub).

R36.6.2 **`init` request and response.** `task::enable_tendermint::init` shall
accept exactly the `params` of `enable_tendermint_with_assets` (R36.1.2,
R36.1.3), flat in `params`, and on acceptance return
`{"task_id": <integer>}` (HTTP 200).

R36.6.3 **`init` error placement.** Before spawning the task, `init` shall
reject synchronously, at **mmrpc level**, only:

| `error_type` | Returned when | `error_data` | HTTP |
| --- | --- | --- | --- |
| `PlatformIsAlreadyActivated` | a coin with `ticker` is already active | string: the platform ticker | 400 |
| `Internal` | the task could not be created | string | 500 |

plus the request-parse failure of ch. 55 §55.1. **Every other** error of
R36.1.6 -- including `PlatformConfigIsNotFound`, the protocol / token errors,
`PlatformCoinMismatch` and `PlatformCoinCreationError` for unresponsive nodes --
shall be reported at **status level**: `task::enable_tendermint::status`
returns HTTP 200 with `{"status": "Error", "details": <error object>}`, the
error object carrying the same `error_type` and `error_data` shape as in
R36.1.6 (ch. 55 §55.2 binds the status object).

> **Upstream divergence (informative).** Every error example on the public
> `task::enable_tendermint` page is drawn as an mmrpc-level response whose
> trace points at the one-shot activation path, so the page does not establish
> where the task variant reports them. The reference reports all of them except
> the already-active check at status level; that placement is bound.

R36.6.4 **`status`.** Request `{task_id: integer, forget_if_finished: boolean
(default true)}`. While running, the Tendermint task reports
`{"status": "InProgress", "details": "ActivatingCoin"}` and no other in-progress
value; it never reports `UserActionRequired` (Tendermint has no interactive
signing policy). On success it reports `{"status": "Ok", "details": <result>}`
where `<result>` is exactly the R36.1.5 result, including the
`get_balances`-dependent view. The family-wide in-progress value set and the
task-framework errors of `status` are bound by ch. 48 and ch. 55 §55.2.

R36.6.5 **`user_action`.** Routed; since no Tendermint task ever awaits a user
action, a call returns the task-framework error of ch. 55 §55.2 for its case.
The request payload shape is the shared hardware user-action shape of ch. 55
§55.2.

R36.6.6 **`cancel`.** Request `{task_id: integer}`; success `"success"`.
Cancelling a running task stops it; it never de-registers a coin that a
finished task registered. Task-framework errors (`NoSuchTask`, `TaskFinished`)
are bound by ch. 55 §55.2.

R36.6.7 The one-shot `enable_tendermint_with_assets` never rejects a request as
a hardware-policy request (Tendermint has none), so
`UnexpectedDeviceActivationPolicy` is not produced by either path.

> **Status update (reloaded).** `task::enable_tendermint::{init,status,
> user_action,cancel}` is routed via the
> [Chapter 48](48-platform-coin-task-activation.md) substrate on native and
> WASM, wrapping the one-shot activation unchanged as its unit of work. The
> placement and in-progress deviations still open are listed in §36.7.

---

## 36.7 Reloaded status (informative)

> **Maintainer decision (binding, 2026-10-11).** This branch implements the
> wire contract of this chapter: items 1-4, 7 and 10-15 below. Items 5, 6, 8 and
> 9 are features (signing policies via `activation_params` in all three forms of
> R36.1.3, HD `path_to_address`, custom tokens, transaction history) and are
> deferred to `docs/plans/tendermint-activation-features.md`.
>
> **Deliberate divergence (binding, interim).** Until R36.1.3 is implemented, a
> request that carries a non-null `activation_params` shall be rejected with
> `PlatformCoinCreationError` (`{ticker, error}`, HTTP 500) -- the discriminant
> the reference uses for an unsupported signing context (R36.1.4 step 3) --
> rather than activated with the local context key. The reference accepts these
> requests and honors them. The silent fallback to the local key is not kept,
> because a caller that asked for a device-backed or watch-only activation must
> not get a hot-key one without knowing. This overrides R36.1.3 and R36.1.7 for
> `activation_params` only, and is removed when the plan's item 1 lands.

> **Implemented on `fix/activation-api-parity`:** items 1-4, 7 and 10-15 below
> and the interim rejection of `activation_params`. Remaining: items 5, 6, 8 and
> 9 (deferred to the Tendermint plan). The list is kept as the record of what
> changed.

Current reloaded behaviour that deviates from the bound contract (behavioural
terms; the clean implementer works from this list):

1. **Empty `nodes`.** Reloaded returns `AtLeastOneNodeRequired` as a unit
   variant (no `error_data`, HTTP 400). Bound: `PlatformCoinCreationError`
   `{ticker, error}`, HTTP 500 (R36.1.6).
2. **Unresponsive nodes / height or balance query failure.** Reloaded returns
   `Transport` (string, HTTP 502). Bound: `PlatformCoinCreationError`
   `{ticker, error}`, HTTP 500 (R36.1.6).
3. **`decimals` above 18.** Reloaded fails at protocol parsing
   (`CoinProtocolParseError`). Bound: `PlatformCoinCreationError` (R36.3.2).
4. **`tokens_params` absent.** Reloaded defaults it to an empty list. Bound:
   required; absence is the request-parse failure of ch. 55 §55.1.
5. **`activation_params`.** Reloaded accepts any JSON and ignores it, so a
   public-key or WalletConnect request silently activates with the local
   context key. Bound: the three forms of R36.1.3 with their effects; malformed
   input is `InvalidRequest`.
6. **`path_to_address` / HD mode.** Reloaded ignores `path_to_address` and
   always activates the context key's address. Bound: in HD-wallet mode the
   address at `path_to_address` under the coin's derivation path; missing
   derivation config or an invalid path is `PlatformCoinCreationError`.
7. **Token platform check.** Reloaded does not check that an inline token's
   `platform` equals the activated platform. Bound: `PlatformCoinMismatch`
   (unit, HTTP 400).
8. **Custom tokens.** Reloaded accepts no `protocol` member on
   `tokens_params[]` or on `enable_tendermint_token`. Bound: custom-token
   activation and `CustomTokenError` (R36.1.6, R36.2.4). When custom Tendermint
   tokens are implemented, each shall be wallet-only, as a custom EVM token is
   (ch. 35 R35.2.7); see `docs/plans/tendermint-activation-features.md` item 3.
9. **`tx_history`.** Reloaded accepts `true` but starts no history fetching.
   Bound: background history fetching starts after activation.
10. **`protocol` member of `UnexpectedPlatformProtocol` /
    `UnexpectedTokenProtocol`.** Reloaded serializes its own internal protocol
    representation; bound: the canonical coin-configuration form (R36.0.3). A
    wire-shape test must pin it for at least a `UTXO`, an `ERC20` and a
    `TENDERMINTTOKEN` protocol.
11. **Shared platform enum.** Reloaded names the private-key-policy variant
    `PrivKeyNotAllowed`, carries `AtLeastOneNodeRequired` without data, and
    lacks `CustomTokenError`, `PlatformCoinMismatch` and the other variants of
    R36.1.7. Their binding is shared with ch. 35 / ch. 55.
12. **`enable_tendermint_token` balance failure.** Reloaded returns
    `Transport` (string, HTTP 500). Bound: `CouldNotFetchBalance` (string,
    HTTP 500).
13. **`enable_tendermint_token` side effect.** Reloaded adds the token to the
    platform's token set *before* reading the balance, so a failed activation
    leaves a stale entry that later platform balance views include. Bound: add
    only after successful registration (R36.2.2).
14. **Task `init` placement.** Reloaded `init` does not reject an
    already-active platform at mmrpc level; the failure appears only in
    `status`. Bound: mmrpc-level `PlatformIsAlreadyActivated`, HTTP 400
    (R36.6.3).
15. **Task in-progress values.** Reloaded reports `ActivatingCoin` and then
    `RequestingWalletBalance`. Bound: `ActivatingCoin` only (R36.6.4).

> **Code-quality finding (informative; trigger: a code comment states the
> problem).** The reference treats every Tendermint activation request as a
> non-hardware request, acknowledged in the reference as a placeholder until a
> device policy exists. Observable consequence: a request carrying
> `priv_key_policy: "Trezor"` silently activates with the context key instead of
> being refused. Reloaded binds the reference behaviour (R36.1.2) for parity;
> proposed hardening for both lineages: refuse a hardware `priv_key_policy` on
> Tendermint with `UnexpectedDeviceActivationPolicy` once the maintainer accepts
> that deviation.

> **Code-quality finding (informative; trigger: apparent with little extra
> effort).** In the reference, a malformed `WithPubkey` key (a `value` that is
> not an array of byte integers, or bytes of the wrong length for the key type)
> aborts request handling with a panic rather than producing a request-parse
> error, and a WalletConnect activation assumes the WalletConnect context
> exists and aborts otherwise. Reloaded shall instead return `InvalidRequest`
> (R36.1.3) and `PlatformCoinCreationError` respectively.

> **Code-quality finding (informative; trigger: apparent with little extra
> effort).** In the reference's HD-wallet mode, the activated key pair pairs the
> derived private key with the wallet's *root* public key, while the account
> address is derived from the derived private key. Anything that exposes or
> embeds the coin's public key (transaction signer information, public-key
> queries) is then inconsistent with the activated address. Proposed fix for
> Reloaded: derive the public key from the activated private key; verify with an
> HD-mode signing test before relying on HD Tendermint swaps.

---

## 36.8 Tests

T36.1 *Unresponsive-node mapping.* With a `nodes` list whose every endpoint
refuses connections, `enable_tendermint_with_assets` returns HTTP 500,
`error_type` `PlatformCoinCreationError`, and `error_data` an object with
exactly the string members `ticker` (= requested ticker) and `error`.

T36.2 *Empty `nodes`.* `nodes: []` yields HTTP 500 `PlatformCoinCreationError`
with object `error_data` `{ticker, error}`; no coin is registered.

T36.3 *`decimals` bound.* A `TENDERMINT` config with `decimals` 19 yields HTTP 500
`PlatformCoinCreationError` (not `CoinProtocolParseError`).

T36.4 *Missing `tokens_params`.* A request without `tokens_params` is rejected
with the request-parse error of ch. 55 §55.1.

T36.5 *Platform mismatch.* An inline token whose `platform` names another
Tendermint coin yields HTTP 400 `PlatformCoinMismatch` with no `error_data`
key, and neither the platform nor any token is registered.

T36.6 *Custom token.* An inline token with a `protocol` for a ticker present in
the configuration yields HTTP 500 `CustomTokenError` with `error_data`
`{"DuplicateTickerInConfig": {"ticker_in_config": <ticker>}}`; the same on
`enable_tendermint_token` yields HTTP 400.

T36.7 *Protocol payload shape.* `UnexpectedPlatformProtocol` for a UTXO-protocol
ticker serializes `error_data.protocol` as `{"type": "UTXO"}` (no `protocol_data`
key) when the UTXO coin has no protocol data; for an
`ERC20`-protocol ticker as `{"type": "ERC20", "protocol_data": {...}}`.

T36.8 *`activation_params` forms.* Each of the three forms of R36.1.3
deserializes to the intended policy; a key `value` given as a string, a key
`type` other than `secp256k1`/`ed25519`, and a wrong-length key each yield
`InvalidRequest` (no panic).

T36.9 *Result views.* With `get_balances` true the result has `balance` and a
ticker-keyed `tokens_balances` object and no `tokens_tickers`; with false it has
`tokens_tickers` and neither balance member; all balances are strings and
`unspendable` is `"0"`.

T36.10 *Token balance failure.* `enable_tendermint_token` against a platform
whose nodes are all unreachable returns HTTP 500 `CouldNotFetchBalance` with a
string `error_data`, and the platform's token set is unchanged afterwards.

T36.11 *Token platform errors.* `enable_tendermint_token` for a token whose
platform is inactive returns HTTP 400 `PlatformCoinIsNotActivated` whose
`error_data` is the platform ticker; for a platform that is an active
non-Tendermint coin returns HTTP 500 `UnsupportedPlatformCoin` with
`{platform_coin_ticker, token_ticker}`.

T36.12 *Task placement.* `task::enable_tendermint::init` for an already-active
platform returns HTTP 400 mmrpc-level `PlatformIsAlreadyActivated`; for an
unknown ticker it returns a `task_id`, and `status` then returns HTTP 200
`{"status":"Error","details":{..., "error_type":"PlatformConfigIsNotFound",
"error_data":"<ticker>"}}`.

T36.13 *Task in-progress value.* While a Tendermint task is running its status
`details` is the string `ActivatingCoin`.

## 36.9 Deferred Work

D36.1 Hardware-wallet (Trezor/Ledger-native) Tendermint activation is not part
of the reference and is not specified here.

D36.2 The task-managed single-token family (`task::enable_tendermint_token::*`)
is not routed by the reference and is deferred.

## 36.10 Baseline Verifications

V36.1 The dispatcher routes `enable_tendermint_with_assets`,
`enable_tendermint_token` and the four `task::enable_tendermint::*` strings on
native and WASM.

V36.2 The legacy `enable` path still rejects the `TENDERMINT` and
`TENDERMINTTOKEN` protocols (activation only through this chapter's methods).

## 36.11 Acceptance criteria

- `enable_tendermint_with_assets` accepts the R36.1.2 fields (flat), the three
  `activation_params` forms, and returns the R36.1.5 result in both
  `get_balances` views.
- Every failure class of R36.1.6 / R36.2.4 yields the bound `error_type`,
  `error_data` shape and HTTP status; unresponsive nodes yield
  `PlatformCoinCreationError` (500) on the platform method and
  `CouldNotFetchBalance` (500) on the token method.
- The `TENDERMINT` / `TENDERMINTTOKEN` `protocol_data` contract of §36.3 holds,
  with `decimals > 18` failing as `PlatformCoinCreationError`.
- `task::enable_tendermint::init` rejects only an already-active platform at
  mmrpc level; all other activation errors appear at status level with the same
  shapes; in-progress `details` is `ActivatingCoin`.
- The Tests of §36.8 pass.

## 36.12 External References

- Komodo DeFi Framework API documentation: `enable_tendermint_with_assets`,
  `enable_tendermint_token`, `task::enable_tendermint`, common structures
  (activation, wallet), error-type catalogue.
- Cosmos SDK / Tendermint RPC, ICS-20 IBC transfer, BIP-173 bech32.

## 36.13 Provenance Footer

- *Inputs:* the published Komodo DeFi Framework API documentation (the
  Tendermint/token activation method family, its request/response JSON field
  names, examples and error discriminants); the Cosmos/Tendermint RPC endpoint
  contract, IBC denom and channel identification, the Cosmos secp256k1
  public-key/account-id model, and CAIP-style chain identification; reloaded's
  existing Tendermint coin/token types and activation code (§36.5, §36.7);
  [Chapter 18](18-tendermint-ibc-htlc.md); [Chapter 22](22-walletconnect-v2.md);
  [Chapter 35](35-evm-v2-activation-rpcs.md);
  [Chapter 48](48-platform-coin-task-activation.md);
  [Chapter 55](55-coin-activation-wire-contract.md).
- *Permitted-input classes used:* external public specification (the
  published KDF API documentation; the Cosmos/Tendermint RPC/IBC/secp256k1/
  CAIP specifications); baseline and as-built source (reloaded's Tendermint
  coin/token types and activation code); cross-chapter contracts (Chapters 18,
  22, 35, 48, 55); interop / wire-and-API-bound reuse (R29/R31) for the
  dictated method strings, JSON field names and error discriminants.
- *Sibling-allowlist consultations:* [Chapter 18](18-tendermint-ibc-htlc.md),
  [Chapter 22](22-walletconnect-v2.md), [Chapter 35](35-evm-v2-activation-rpcs.md),
  [Chapter 48](48-platform-coin-task-activation.md),
  [Chapter 55](55-coin-activation-wire-contract.md).
- *Forbidden corpus:* consulted, under the chapter-01 two-team clean-room
  workflow, at the GLEEC KDF tags `v3.0.0-beta` (reference) and `v2.6.0-beta`
  (legacy comparison), for upstream wire parity of the Tendermint activation
  methods only -- specifically the request-field acceptance of R36.1.2-R36.1.3,
  the failure -> `error_type` mapping, `error_data` shapes, HTTP statuses and
  placement of R36.1.6, R36.2.4 and R36.6.3, the `decimals` failure class of
  R36.3.2, and the in-progress value of R36.6.4. No source text, private
  identifier, internal decomposition, or error/log string literal was carried
  into this chapter; the code-quality findings of §36.7 are restated as
  behaviour.
