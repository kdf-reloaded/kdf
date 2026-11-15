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
| `tokens_params[].protocol` | object (coin-protocol form) | optional | Custom-token activation: when present, the token is activated from this protocol object instead of the coins configuration (R36.1.4 step 4). A custom token is wallet-only (Reloaded; as a custom EVM token is, ch. 35 R35.2.7). Any other member of a token request (e.g. `required_confirmations`) is accepted and ignored. |
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

> **As built (Reloaded; PR #319; informative).** `path_to_address` and
> `tokens_params[].protocol` are implemented as follows.
>
> - **`path_to_address`.** The activation reads the login's key policy. Under an
>   HD login the coin's key is derived from the seed at the coin's
>   `derivation_path` extended by `path_to_address` (account hardened, then the
>   chain, then the address index), with the same derivation the UTXO coins use
>   for their activated address; the account id is derived from that key and the
>   coin's public key is derived from the same private key, never taken from the
>   wallet's root key. A coin configuration without `derivation_path`, a
>   `path_to_address` that does not parse, and a path that cannot be derived
>   each fail with `PlatformCoinCreationError`; the request member is parsed
>   under an HD login only, and under an iguana login it is ignored whatever it
>   holds, as before. The task variant (§36.6) uses the same build.
> - **`tokens_params[].protocol`.** An entry with a `protocol` is built from it;
>   an entry without one is built from the coins configuration. A `protocol`
>   for a ticker that has a coins-configuration entry fails with
>   `CustomTokenError`; the check precedes the one on the protocol's type. All
>   entries are resolved, then checked against the platform
>   (`PlatformCoinMismatch`), then built, so a failure leaves nothing registered.
>   A malformed `protocol` object is a request-parse failure (ch. 55 §55.1).
> - **A custom token is wallet-only,** as a custom EVM token is (ch.35 R35.2.7):
>   it is refused when an order is placed, whatever the coins file and the build
>   switch say.

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
| `protocol` | object (coin-protocol form) | optional | Custom-token activation from the supplied `TENDERMINTTOKEN` protocol instead of the coins configuration. The token is wallet-only (Reloaded; ch. 35 R35.2.7). |

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

> **As built (Reloaded; PR #319; informative).** `enable_tendermint_token` has its
> own handler with a request type that carries the optional `protocol` beside
> `ticker` and `activation_params`. The steps are those of the generic
> single-token activation, with the protocol resolved as in the as-built note
> under R36.1.2: the inline `protocol` when present (a `CustomTokenError`,
> HTTP 400, when the ticker is in the coins configuration; an
> `UnexpectedTokenProtocol` when it is not `TENDERMINTTOKEN`), else the coins
> configuration. The token built from an inline `protocol` is marked custom and
> is wallet-only.

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

### 36.3.1A Priority gas prices in the `TENDERMINT` `protocol_data` (gated text; passed the KDF Dirty Gate on 2026-11-08)

> **Provenance status: passed the KDF Dirty Gate on 2026-11-08, measure
> 0.03** (chapter 01 R40; attestation in the Provenance Footer). This
> subsection was written by the dirty-side specification author on
> 2026-11-08 (Spec Reader round 0.4.0-11) from the reference state
> `tendermint-priority-fee (as of 2025-07-24)` compared against its base
> `dev (as of 2025-07-18)` (both rows of chapter 02 §2.15A), from the public
> Cosmos chain-registry schema, and from this repository's own tree. The
> reference state is an **unmerged draft**: neither `v2.6.0-beta` nor
> `v3.0.0-beta` carries any of it (§36.0.1 is unaffected), it was never
> approved, it was in conflict with the reference's development branch when
> the record was archived, and no later state of it exists. The maintainer
> decided on 2026-11-08 (decision FN (a)) to include the feature in
> Reloaded; whether the *field names* below are kept for parity with that
> draft is open (D36.4).

A Cosmos chain publishes three gas-price levels for each fee token in the
public Cosmos chain registry (`fees.fee_tokens[].low_gas_price`,
`average_gas_price`, `high_gas_price`, in base denom per gas unit). The
reference draft lets a coins-file entry carry those three figures so that a
caller may pick a level by name instead of a number.

R36.3.5 *The `priority_gas_prices` member.* The `TENDERMINT` `protocol_data`
object MAY carry one further optional member:

| `protocol_data` key | JSON type | Required? | Functional role |
| --- | --- | --- | --- |
| `priority_gas_prices` | array of exactly three numbers | optional | The chain's published gas prices in base denom per gas unit, in the fixed order **low, average, high** (positions 0, 1, 2). A named level in a withdraw, staking or IBC fee descriptor (chapter 49 R49.29) or a swap fee policy (chapter 35 R35.6.8) selects the figure at the matching position; the selected figure then enters the fee formula of chapter 18 §18.6B in place of the default gas price. |

Shape and validation at the reference state:

1. the value is an array of **exactly three** JSON numbers (integers are
   accepted as numbers); an array of any other length, a non-array value or
   a non-numeric element fails parsing of the coin's protocol descriptor,
   which is the same failure class as a missing required field of R36.3.1
   (`CoinProtocolParseError`, R36.3.2); a JSON `null` is treated as absent;
2. the figures are **not** otherwise validated: no positivity check, no
   ordering check (low ≤ average ≤ high is not enforced), no relation to
   `gas_price` is checked;
3. when the member is absent, every named level resolves to the coin's
   default gas price (the `gas_price` member of R36.3.1, or the built-in
   default 0.25 when that is absent too); activation and every operation
   succeed exactly as before, so absence is **benign**;
4. the member is read at platform activation and held for the coin's
   lifetime; it is not re-read, and no RPC changes it.

R36.3.6 *Tokens have no priority prices of their own.* The `TENDERMINTTOKEN`
`protocol_data` carries no such member. A token's transactions are priced
with its platform coin's `priority_gas_prices`, as they are priced with the
platform coin's `gas_price` today (R36.3.3, `gas_price` row). A
`priority_gas_prices` member on a token entry, if present, is an unknown
member and is ignored under the extra-config-keys note of §36.3.1.

> **Reloaded status (informative; this repository's tree, 2026-11-08).**
> Reloaded's Tendermint protocol descriptor has no `priority_gas_prices`
> member; because the descriptor tolerates unknown members (§36.3.3), a
> coins file that already carries the array activates today with the array
> silently dropped, and every level resolves to the default price. The
> coins file this repository ships carries the member on no entry as of
> this writing.

> **Upstream divergence (informative).** The draft validates neither the
> count semantics beyond the fixed length nor the sign of the figures. A
> Reloaded implementation SHOULD refuse, at platform-coin construction
> (`PlatformCoinCreationError`, the class R36.3.2 uses for `decimals`),
> an array containing a non-positive or non-finite figure, because such a
> figure makes the fee of chapter 18 §18.6B zero or undefined; whether to
> also require the non-decreasing order is left to D36.4.

T36.16 *`priority_gas_prices` shape (R36.3.5).* A `TENDERMINT` entry with
`priority_gas_prices: [0.015, 0.025, 0.04]` activates and the three levels
resolve to 0.015, 0.025 and 0.04 (observable through the fee amount of a withdraw
carrying the priority fee descriptor of chapter 49 R49.29); an entry with a
two-element array, with a four-element array, with a string element and with
an object in place of the array each fails activation with
`CoinProtocolParseError` and registers no coin; an entry without the member,
and one with `priority_gas_prices: null`, activate and every level resolves
to the default price. A `TENDERMINTTOKEN` entry carrying the member
activates with the member ignored (R36.3.6).

T36.17 *Non-positive figure (divergence note).* An entry with
`priority_gas_prices: [0, 0.025, 0.04]` fails activation with
`PlatformCoinCreationError` and registers no coin. *(Reloaded-original; the
reference draft accepts it.)*

> **As built (2026-11-09; R36.3.5, R36.3.6, T36.16, T36.17).**
> The `TENDERMINT` arm of the coin protocol carries `priority_gas_prices` as an optional array of exactly
> three `f64` (`null` and absent are `None`; it is not serialized when absent), so every shape of R36.3.5 item 1
> fails `CoinProtocolParseError` through the existing protocol parse. The platform builder reads the same
> member into its protocol info once and refuses a figure that is zero, negative or not finite with
> `PlatformCoinCreationError` (the init-error kind `PriorityGasPriceNotPositive`) before the account is
> derived; the order of the figures is not checked (D36.4). A `TENDERMINTTOKEN` entry is parsed without the
> member, which is ignored as an unknown key (R36.3.6). Tests: T36.16 and T36.17 in
> `lp_coins_context.rs`, `tendermint_with_tokens_activation/wire_tests.rs` and
> `tendermint_swap_flow_tests/priority_fee_tests.rs`.

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
> `protocol_data` keys so surplus config does not fail activation. The
> `priority_gas_prices` member of §36.3.1A (gated text) is not yet parsed
> and is dropped the same way.
>
> *As built 2026-11-09 (clean-side note): implemented; see the As-built note of §36.3.1A.*

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

## 36.5A Node naming, credentials and node responses in logs (Reloaded-original)

The rules of this section bind what the Tendermint coin layer's texts and
renderings may say about an RPC node of `nodes[]` (R36.1.2). They bind no wire
shape: the human-readable `error` text stays unbound except for what these
rules forbid it to carry.

R36.5A.1 **Host-only naming of Tendermint nodes (Reloaded; clean-room
original; maintainer decision 2026-10-26; no switch).** Every log line, error
text and status message the Tendermint coin layer emits that names a node
shall name it in the host-only form of [chapter 35](35-evm-v2-activation-rpcs.md)
R35.1.14: scheme, host and, when present, port, with the userinfo, path, query
and fragment stripped. This covers at least:

- the per-node logs of node selection, including a failed health check and
  the fallback check that follows it;
- the error a caller receives when no node is usable, and the per-node
  details it lists;
- the activation error for a node URL that cannot be used. It shall not echo
  the configured string: an entry without a usable scheme and host is named
  by a fixed placeholder;
- any lower-layer text (HTTP client, TLS, DNS) embedded in one of these
  texts. A full URL that arrives from a lower layer is replaced by its
  host-only form before the text is logged or returned.

The rule applies to the native and the browser build alike.

R36.5A.2 **Credentials in renderings (Reloaded; clean-room original;
maintainer decision 2026-10-26; no switch).** Chapter 35 R35.1.17 applies to
the Tendermint coin layer: the coin's node list, each per-node client, and the
node entries of the activation request render a node URL in host-only form
only, and no rendering prints a header value or other credential the
configuration supplies. The scope limit of R35.1.17 for the RPC wire dump
applies here too.

R36.5A.3 **Node response bodies are not logged (Reloaded; clean-room
original; maintainer decision 2026-10-28, D36.3).** No log line, at any
level, shall carry the body of a node's response, in whole or in part. A log
line about a response may carry the request kind, the node in host-only form,
the outcome and the body's size in bytes. This rule binds log lines only. An error text
that quotes the node's own error code and message is governed by R36.5A.1
alone.

These three rules state Reloaded's own behavior; this chapter records nothing
about the reference's logging. They have no opt-back switch, for the reason
chapter 35 R35.1.14 gives: node access reaches no peer and the rules only
protect the operator, so by the second arm of the default-side rule
(`docs/COMPAT_SWITCHES.md`) the Reloaded form is the default, and an opt-in to
print credentials would be an opt-in to weaken security. They are recorded in
`docs/GLEEC_COMPATIBILITY.md` with "none -- no switch exists".

> **Status (2026-10-28).** Implemented. Node-selection logs and errors name
> each node host-only, an unparseable URL is not echoed, the per-node HTTP
> client's debug form is host-only, and a response is logged by its size only.
> T36.15 covers the texts; the coin-level case of T36.14 has no test yet,
> because no test builds a full Tendermint coin.

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
> wire contract of this chapter: items 1-4, 7 and 10-15 below. Items 5 and 9
> are features (signing policies via `activation_params` in all three forms of
> R36.1.3, transaction history) and are deferred to
> `docs/plans/tendermint-activation-features.md`. Two further features, HD
> `path_to_address` and custom tokens, were deferred the same way and have been
> delivered since (PR #319); their items are removed from the list and the
> as-built notes under R36.1.2 and R36.2.4 describe them. The source numbers of
> the other items are unchanged.
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
> and the interim rejection of `activation_params`. Remaining: items 5 and 9
> (deferred to the Tendermint plan). The list is kept as the record of what
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
7. **Token platform check.** Reloaded does not check that an inline token's
   `platform` equals the activated platform. Bound: `PlatformCoinMismatch`
   (unit, HTTP 400).
9. **`tx_history`.** Reloaded accepts `true` but starts no history fetching.
   Bound: background history fetching starts after activation.
   *Status note (2026-11-15, Reloaded-original).* Closed by PR #363: `tx_history:
   true` starts the fetcher of R36.14.3 and the divergence recorded here no
   longer exists (§36.14.1).
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
>
> **Status note (2026-11-15, Reloaded-original).** Reloaded now declares the
> `priv_key_policy` request member of `enable_tendermint_with_assets` and of
> `task::enable_tendermint::init`. An absent value, `ContextPrivKey` and
> `IguanaPrivKey` activate as before; a hardware value (`Trezor`, bare string or
> `{"type": "Trezor"}`) is refused before any network access with the typed
> Tendermint init error `HardwarePolicyNotSupported`, answered as
> `PlatformCoinCreationError` (`{ticker, error}`, HTTP 500, the same placement as
> the interim rejection of `activation_params`) with the text "the hardware key
> policy Trezor is not supported for Tendermint coins in this version; activate
> with the local key". It is not `UnexpectedDeviceActivationPolicy`. This is a
> deliberate divergence from the reference, which ignores the member, until
> hardware signing for Tendermint coins exists. Any other value of the member
> (for example the WalletConnect form `{"type": "WalletConnect", "data": ...}`
> sent top-level, or an unknown name) fails request parsing before the
> activation starts, answered as `InvalidRequest` (string, HTTP 400) instead of
> activating with the local key; the WalletConnect form is accepted only once
> its activation (plan item 1) is built.

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
>
> **As built (Reloaded; PR #319).** Done. Reloaded derives the public key of an
> HD-activated coin from the activated private key, and a test signs a
> withdrawal at a non-default path and verifies the signature against that
> path's public key and against neither the default path's key nor the wallet's
> root key.

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

T36.14 *Host-only naming and credentials (R36.5A.1, R36.5A.2).* Activate with
two nodes whose URLs carry userinfo, a path segment and a query string, both
refusing connections. The activation error, each node-selection log line and
the debug form of the coin's node list and of the parsed request contain each
node's host-only form and none of the userinfo, path or query parts. A node
URL that cannot be used fails activation with an error that does not contain
the configured string. Run on the native build; on the browser build once a
browser test harness exists.

T36.15 *No response bodies in logs (R36.5A.3).* With log capture at the most
verbose level, a request whose answer carries a marker string succeeds, and
no captured line contains the marker. A line that reports the answer may
carry its size.

T36.16 *Custom tokens.* An inline `TENDERMINTTOKEN` `protocol` in `tokens_params`
activates beside the platform and on `enable_tendermint_token`; the token is
marked custom and is wallet-only; a configured token activated in the same call
is unchanged. A `protocol` for a configured ticker is the `CustomTokenError` of
T36.6, a `protocol` that names another platform is `PlatformCoinMismatch`, and a
`protocol` of another type is `UnexpectedTokenProtocol`.

T36.17 *HD key.* Under an HD login the address of the activated coin equals the
BIP-44 address at the coin's `derivation_path` and `path_to_address`, computed
independently of the wallet code from the test mnemonic (`m/44'/118'/0'/0/0` by
default, another path for another `path_to_address`); the coin's public key is
that of the activated private key; a withdrawal signed at a non-default path
verifies against that path's public key and against neither the default path's
key nor the wallet's root key. Under an iguana login the address is that of the
single key, whatever `path_to_address` holds. A coin configuration without
`derivation_path`, and an unparseable or underivable `path_to_address`, fail
under an HD login with `PlatformCoinCreationError`.

## 36.9 Deferred Work

D36.1 Hardware-wallet (Trezor/Ledger-native) Tendermint activation is not part
of the reference and is not specified here.

D36.2 The task-managed single-token family (`task::enable_tendermint_token::*`)
is not routed by the reference and is deferred.

D36.3 *(Resolved 2026-10-28, maintainer decision, for Tendermint; the second
question resolved 2026-10-29, maintainer decision.)* *Node response bodies in logs.* R36.5A.3 forbids
response bodies in log lines at every level. A looser reading would allow them
at trace level only. The stricter reading was taken because a trace level can
be switched on in the field, and a body then reaches a shared log. The
maintainer kept the rule: a log line names a body's size only. The second
question was whether the same rule should bind the other node families,
whose error texts carried the body of a non-success answer. The maintainer
decided on 2026-10-29 that it does, for error texts as well as log lines: the
native UTXO daemon's errors carry the daemon's error code and message and the
body's size only (chapter 38 R38.6.9b item 4), and so do the Sia wallet
daemon's (chapter 20 R-N3).

D36.4 *(Resolved: the maintainer's decision of 2026-11-09, option (a), the
draft's names and shapes, with the three figures accepted in any order (no
non-decreasing requirement); to be reassessed after field use. Previously open -- maintainer decision; gated text of §36.3.1A, passed the KDF
Dirty Gate on 2026-11-08.)* *Field-name parity for the priority gas prices.*
The member name `priority_gas_prices`, its fixed three-element array shape and
the level names used by the fee descriptor of chapter 49 R49.29 (`Low`,
`Average`, `High`, `Default`) are those of the unmerged reference draft named
in §36.3.1A. No released designation carries them, so there is no deployed
coins file or client to be compatible with. Options: (a) keep the draft's
names and shapes so that a coins file written for the draft works unchanged;
(b) choose Reloaded's own names (for example an object `{"low": …, "average":
…, "high": …}` mirroring the chain-registry keys, and `Medium` in place of
`Average` to match the swap fee policy of chapter 35 R35.6.3), documented in
`docs/COMPAT_SWITCHES.md` as a Reloaded-only surface; (c) accept both. The
rules of §36.3.1A, chapter 49 §49.2A and chapter 35 R35.6.8 are written in the
draft's names under option (a) and are to be renamed consistently if (b) or
(c) is chosen. The same decision settles whether the non-decreasing order of
the three figures is required (the divergence note of §36.3.1A).

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
- §36.14.1 (transaction history): the CometBFT RPC specification (`tx_search`
  with its event query language, paging and ordering, `abci_info`); the Cosmos
  SDK module documentation for the bank events (`coin_spent`, `coin_received`,
  `transfer`), the staking events (`delegate`, `unbond`) and the distribution
  event (`withdraw_rewards`); the ibc-go transfer module documentation
  (`ibc_transfer`, `fungible_token_packet`, `non_fungible_token_packet`); the
  irismod HTLC module documentation (`create_htlc`, `claim_htlc`); the Cosmos
  SDK protobuf definitions `cosmos.tx.v1beta1.Tx` (body memo, auth info, fee)
  and `cosmos.base.tendermint.v1beta1.Service` (`GetBlockByHeight`).
- §36.14.2 and §36.14.3 (key-less activation): the public WalletConnect
  Cosmos method reference (`cosmos_getAccounts`, `cosmos_signDirect`,
  `cosmos_signAmino`).

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
- *Clean-context addition (2026-10-28, forbidden corpus not consulted):*
  §36.5A (R36.5A.1 -- R36.5A.3 and the status note), T36.14, T36.15 and D36.3
  are Reloaded-original. They rest on the maintainer decision of 2026-10-26
  (host-only node naming everywhere, except the RPC wire dump, which keeps its
  own secrets switch), his decision of 2026-10-28 on D36.3 and this
  repository's own code. They take no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required. The resolution of D36.3's second question (2026-10-29, decision
  AU) was added on the same terms.
- *Clean-context addition (2026-11-09, forbidden corpus not consulted):* the
  as-built notes under R36.1.2 and R36.2.4, the as-built note under the HD
  code-quality finding of §36.7, the wallet-only sentences of the
  `tokens_params[].protocol` and `protocol` rows, and the removal of the former
  items 6 and 8 of §36.7 describe this repository's code. They take no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required.
- *Scoped addition (2026-11-08) -- §36.3.1A (R36.3.5, R36.3.6, the two
  informative notes, T36.16), the sentence added to the §36.3.3 status note,
  and D36.4.* **KDF Dirty Gate: PASS on 2026-11-08, measure 0.03, no
  findings** (chapter 01 R40; the attestation at the end of this item closes
  it). *Inputs:* the forbidden corpus at the rows `tendermint-priority-fee
  (as of 2025-07-24)` and `dev (as of 2025-07-18)` of chapter 02 §2.15A,
  consulted **only** to establish the name, JSON shape, validation and
  absent-value behavior of the optional priority-gas-price member of the
  `TENDERMINT` protocol descriptor, that a token descriptor has no such
  member, and the draft's review state as a fact (unmerged, never approved,
  in conflict at archive time, no later state); the released designations
  `v2.6.0-beta` and `v3.0.0-beta` were checked only for the absence of the
  member. The three-level model and the level names rest on the public
  Cosmos chain-registry schema (chapter 01 R3/R6). Behavior and dictated
  coins-file member names only; no private identifier, function body, helper
  decomposition, error, log, comment, test body or review text was
  reproduced. The Reloaded status note and T36.17 rest on this repository's
  own tree and are Reloaded-original. *Dirty Gate clearance (chapter 01 T1,
  R40):* PASS, 2026-11-08. Extent: the Tendermint fee-computation and
  fee-descriptor surface — the Cosmos withdraw, staking and IBC fee
  descriptors, the swap fee-policy RPC pair and its policy enum, and the
  Tendermint protocol descriptor's gas-price members — compared at the row
  tendermint-priority-fee (as of 2025-07-24) against its base row dev (as of
  2025-07-18) over the complete difference between those two states (nine
  source files, two of them test files), together with the public
  description, two review comments and three discussion comments of the
  draft pull request those rows record; and at the rows v2.6.0-beta and
  v3.0.0-beta for the released fee-policy method strings, request member and
  enum values, the explicit Cosmos fee descriptor, the handling of a foreign
  fee form on a Cosmos request, and the staking and IBC gas-limit defaults.
  Method: reading of the full inter-state difference and of the
  release-state passages, combined with mechanical text search of the
  candidate's added lines against identifier, numeric-constant,
  string-literal, name and number lists drawn from the compared states; no
  code-block tool run was needed (the candidate embeds only two one-line
  JSON wire examples). Measure: discretionary-similarity 0.03, read against
  the per-token forced-vs-free standard of chapter 01 R35/R39 (dictated
  wire, config, error-discriminant and contract-constant tokens excluded;
  FAIL on any private identifier, body or control-flow transcription,
  error/log wording, internal module tree or internal test name) — below
  threshold, PASS.
- *Clean-context addition (2026-11-09, forbidden corpus not consulted):* the
  recorded resolution of D36.4, the as-built note under §36.3.1A and the
  clean-side note closing the §36.3.3 status note describe the maintainer's
  decision of 2026-11-09 and this repository's code. They take no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is required.
- *Scoped addition (2026-11-15, Spec Reader round 0.5.0-3) -- §36.14
  (R36.14.1 -- R36.14.18, the informative notes, D36.10 -- D36.18,
  T36.30 -- T36.44).* **KDF Dirty Gate: PASS on 2026-11-15, measure 0.05, no findings**
  (chapter 01 R40; the attestation closes this item). *Inputs:* the forbidden corpus at the rows `v2.6.0-beta`
  and `v3.0.0-beta` of chapter 02 §2.15A, read read-only and consulted
  **only** to establish, for the Tendermint coin and token: which
  transactions the background history fetcher turns into rows, the members
  and amounts of each row, the fee attribution between a token row and the
  platform coin, the node queries, page size and order of the fetch, the
  persisted form and identifier of a row, the history-state values and when
  they change, and the answers of both `my_tx_history` methods with and
  without `tx_history`; what a WalletConnect activation reads from the
  session, which wallet method signs a transaction, and the error type a
  failed signature surfaces as; what a public-key activation allows, the
  members of the unsigned-transaction result, and what the Ledger flag
  changes; and the two rows' agreement on these points (a read-only
  difference between the two rows over the history fetcher, the history
  store, the history method, the Tendermint activation, the WalletConnect
  layer and the external-data request, all found identical, and over the
  Tendermint coin and token files, which differ only in parts this section
  does not describe). Behavior,
  public wire shape and persisted-row format only; no private identifier,
  function body, helper decomposition, error, log, comment or test text was
  reproduced. Event and attribute names are those of the public chain
  modules (Cosmos SDK bank, staking and distribution; ibc-go transfer;
  irismod HTLC), method names those of the public CometBFT RPC and of the
  public WalletConnect Cosmos method reference. The *Our side* notes, the
  options of the D-items and the test rows rest on this repository's own
  tree and on Chapters 05, 10, 18, 22, 51, 53 and 60; the test figures are
  computed from the rules, not taken from any test.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-15, measure
  0.05 on the discretionary-expression basis of chapter 01 R35 and R39,
  judged per token, no findings (the round as a whole: PASS, measure 0.05;
  chapter 18 §18.11.20: 0.04); the section passed two full passes, the
  second after three passages were reworded. Reference states (chapter 02
  R13): exactly two states are named, `v2.6.0-beta` and `v3.0.0-beta`, both
  rows of §2.15A, compared at the rows' full identifiers; no untabled state
  was named or compared. Extent: the Tendermint transaction-history surface
  (the background fetcher's node queries, paging, waiting and retry; the
  derivation of rows from execution-result events; amounts; fee attribution;
  the row identifier and stored form; the history-state values); the
  mmrpc-2.0 and legacy history methods and the indexed history store as they
  serve this family, and the history stream's payload; the serialized
  transaction-type, HTLC-message-type and history-state value sets; the
  Tendermint activation request forms; the WalletConnect and public-key
  activation paths (session, chain and account checks, the key-record and
  enumeration sources, key decoding, address derivation, the Ledger flag);
  the WalletConnect signing path and its failure mapping; the
  unsigned-document forms of withdraw and staking, the wallet-only decision,
  and the withdraw fee and identifier — 16 source files and 2 test files
  read in whole or in the relevant parts at `v2.6.0-beta`, `v3.0.0-beta`
  covered by a row-to-row difference over the same areas, read in full
  wherever the rows differ; 28 source files per row fed the mechanical
  lists; the staking request handlers, the balance stream and the browser
  history store covered by the mechanical lists only. Second pass: the full
  added ranges read again and compared at both rows (mechanical search over
  160 source-file instances across the two rows; six files read at
  `v2.6.0-beta`, row-to-row difference for `v3.0.0-beta`). Method: reading
  at the tabled states; mechanical comparison against a compound-identifier
  list and a word 4-gram list of comment and string-literal text from the
  compared files at both rows; search for every backticked token, each match
  judged forced or free; scan for identifier-shaped words outside backticks;
  row-to-row difference; the additions embed no code blocks, so the
  repository's clean-room gate tool was not applicable. The comparison
  concerns expression; it does not attest the accuracy of the statements.

---

## 36.14 Transaction history and key-less activation at the reference

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-15,
> measure 0.05 (attestation in §36.13).**
> This section was written on 2026-11-15 by the KDF Spec Reader (Spec Reader
> round 0.5.0-3) under the two-team procedure of chapter 01 §1.4A, from the
> reference rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A (read
> read-only; no untabled state was read), from the public interface
> definitions of the Cosmos SDK bank, staking and distribution modules, the
> ibc-go transfer module, the irismod HTLC module and the CometBFT RPC, from
> the public WalletConnect Cosmos method reference, and from this
> repository's own tree. **The two rows agree on everything in this
> section**: the parts of the reference it describes do not differ between
> them, so nothing here is network- or reference-version-dependent (R36.0.1
> continues to hold). Numbering: rules R36.14.1 -- R36.14.18, decisions
> D36.10 -- D36.18 and tests T36.30 -- T36.44, chosen apart from the
> chapter's existing numbers (T36.16 and T36.17 are each already used
> twice). The section **completes** R36.1.2 (the `tx_history` row), R36.1.3
> (forms 1 and 2), R36.1.4 (steps 3 and 6) and §36.7 (items 5 and 9) and
> does not restate them; where it finds one of them imprecise, the note of
> §36.14.4 says so. The swap side of a coin activated without a local key is
> in [Chapter 18](18-tendermint-ibc-htlc.md) §18.11.20.

Scope of the 0.5.0 cycle, by the maintainer's decisions of 2026-11-15, which
this section serves and does not itself decide: transaction history is built
first; both key-less activation forms are built (WalletConnect, and the
public-key form as a watch-only coin); a hardware `priv_key_policy` gets a
typed refusal (§36.14.4); and no history row is written in 0.5.0 for an
HTLC that the chain refunds on its own, while showing it is to be planned
(D36.14).

### 36.14.1 Transaction history

R36.14.1 **Surface and store.** A Tendermint platform coin and a Tendermint
token are served by the mmrpc-2.0 `my_tx_history` method, with the shared
request and result members that [Chapter 53](53-sia-transaction-history.md)
R53.2.8 lists; the `target` member is ignored for this family
([Chapter 05](05-hd-wallet-support.md)). The rows live in the indexed
history store of the **platform coin**, one store per platform ticker (the
SQL store on a native build, the IndexedDB store in the browser). A token
has no store of its own and reads its rows from its platform coin's store.
Rows are written only by the platform coin's background fetcher, which
`tx_history: true` starts (R36.1.2, R36.1.4 step 6); neither
`enable_tendermint_token` nor a `tokens_params` entry has a history switch.
The legacy flat `my_tx_history` method reads a different store, which this
family never writes: for a Tendermint coin or token it answers an empty
`transactions` list with `total` 0, whatever `tx_history` was.

R36.14.2 **What a coin's and a token's history holds.** A row belongs to a
token when it carries that token's id, and to the platform coin when it
carries none. The token id is the SHA-256 of the token's `denom` string in
lower case: 32 bytes, written as 64 hex characters wherever it appears on
the wire. The history of a token is the platform store's rows with its
token id; the history of the platform coin is the rows without a token id;
in both cases only the rows whose `from` or `to` list contains the
activated account address are served. So:

- the platform coin's history holds its plain transfers in both directions,
  its IBC transfers, the HTLC creations and claims in the platform denom,
  delegations, undelegations and reward claims, and one fee row for every
  token transaction this account paid for (R36.14.8);
- a token's history holds its plain transfers, its IBC transfers and the
  HTLC creations and claims in its denom, and never a fee.


R36.14.3 **Source, paging and order of the fetch.** The fetcher reads the
account's transactions from a node with the CometBFT `tx_search` method,
using two event queries in turn: first `coin_spent.spender = '<address>'`,
then `coin_received.receiver = '<address>'` (the bank module's events for
every debit and credit of an account), each with 50 results per page, in
ascending order (oldest first) and without inclusion proofs. Each query is
walked from its first page to its last, so a pass covers the account's
whole history; no height bound is put into a query. A transaction that both
queries return is stored once (R36.14.10). For each transaction the block
time is read with a block query at the transaction's height. When a pass is
complete the fetcher waits: every 30 seconds it reads the balances of the
platform coin and of the activated tokens, and a change in any of them
starts another pass, which continues each query at the page where the
previous pass ended (reading that page again when it was not full). A node
failure during a pass or during the balance read is followed by a pause of
30 seconds and a retry that resumes at the page that failed; it does not
show in the history state (R36.14.11).

R36.14.4 **Transactions that yield no row.** A transaction is left out
entirely when its execution result code is not zero; when the node's block
answer carries no time for its height; when it cannot be decoded, has no
message, or has an empty fee list; and when none of its events yields a
transfer under R36.14.5.

R36.14.5 **The transfers of a transaction.** Rows are derived from the
events of the transaction's execution result, not from its messages. An
event repeated identically in direct succession counts once. Exactly one
of the following applies, in this precedence:

1. the result has a staking `delegate` event: the transaction is a
   delegation, with one transfer from the event's `delegator` to its
   `validator` of the event's `amount`;
2. otherwise it has a staking `unbond` event: an undelegation, with one
   transfer from the `delegator`, without a recipient, of amount zero;
3. otherwise it has a distribution `withdraw_rewards` event: a reward claim,
   with one transfer from the `validator` to the `delegator` of the event's
   `amount`;
4. otherwise every bank `transfer` event yields one transfer per coin of
   its `amount` attribute, from its `sender` to its `recipient`. When the
   result holds more than one event of the kinds this rule names, a
   `transfer` event whose `amount` text equals the transaction's fee coin
   (the integer amount immediately followed by the denomination) is the fee
   payment and is left out. Transfers with the same sender, recipient and
   denomination are merged by adding their amounts.

In cases 1 to 3 only the first event of that kind is read and no other
event of the transaction is. In case 4 the parties of a transfer are
corrected from a module event when the result has one: with an HTLC
`create_htlc` event the sender and recipient become that event's `sender`
and `receiver`, and the transfer is an HTLC creation; with an HTLC
`claim_htlc` event the sender becomes that event's `sender` while the
recipient stays the bank transfer's, and the transfer is an HTLC claim;
otherwise, with an `ibc_transfer` event (outgoing) or a
`fungible_token_packet` or `non_fungible_token_packet` event (incoming),
the sender and recipient become that event's `sender` and `receiver`, and
the transfer is an IBC transfer. One module event corrects one transfer.

An amount is a coin text of the form `<integer><denomination>`. A transfer
is kept only when its denomination is the platform `denom` or the `denom`
of a `protocol_data` in the coins configuration; the row's `coin` is the
platform ticker or that entry's ticker. An attribute key may arrive
base64-encoded (older nodes), in which case its value is base64-decoded;
both forms are read.

R36.14.6 **Rows and their `transaction_type`.** Each transfer becomes one
row. "This account" is the activated address; a transfer is *sent by this
account* when its sender, after the correction of R36.14.5, is that
address.

| Transfer | `transaction_type` | `from` | `to` |
| --- | --- | --- | --- |
| plain, platform denom | `"StandardTransfer"` | sender | recipient |
| plain, token denom | `{"TokenTransfer": <token id>}` | sender | recipient |
| IBC, either direction | `{"TendermintIBCTransfer": {"token_id": <token id or null>}}` | sender (an address of the other chain when incoming) | recipient (an address of the other chain when outgoing) |
| HTLC creation sent by this account | `{"CustomTendermintMsg": {"msg_type": "SendHtlcAmount", "token_id": <token id or null>}}` | this account | empty |
| HTLC creation sent by another account | no row | | |
| HTLC claim sent by this account | `{"CustomTendermintMsg": {"msg_type": "SignClaimHtlc", "token_id": <token id or null>}}` | this account | empty |
| HTLC claim sent by another account | `{"CustomTendermintMsg": {"msg_type": "ClaimHtlcAmount", "token_id": <token id or null>}}` | this account | empty |
| delegation | `"StakingDelegation"` | delegator | validator |
| undelegation | `"RemoveDelegation"` | this account | empty |
| reward claim | `"ClaimDelegationRewards"` | validator | delegator |

`from` and `to` are JSON arrays of at most one address. A value without
data is a JSON string; a value with data is an object with that single
key. `token_id` is `null` for the platform denom and the token id of
R36.14.2 otherwise; the three staking values carry no token id whatever
the denomination. `msg_type` has exactly the three values shown. An HTLC
row never names the counterparty: its `to` is empty in every case.

R36.14.7 **Amounts.** With `a` the transfer's amount:

- `spent_by_me` is `a` when the transfer is sent by this account and is not
  an HTLC claim, and zero otherwise;
- `received_by_me` is `a` when the transfer is not sent by this account, and
  also when sender and recipient are both this account; zero otherwise;
- `total_amount` is `a`, except that it is zero for an HTLC claim that this
  account sent and that pays another account;
- the row that bears the fee (R36.14.8) adds the fee to `total_amount` and
  to `spent_by_me`;
- `my_balance_change` is `received_by_me` minus `spent_by_me`.

All four are decimal numbers in display units, scaled with the decimals of
the coin or token the request names. An HTLC claim that this account sends
for its own HTLC therefore shows `received_by_me` = `a`, `spent_by_me` =
the fee and `total_amount` = `a` plus the fee when the denom is the
platform's.

R36.14.8 **Fee attribution between a token row and the platform coin.**

1. Every row carries `fee_details` of the whole transaction, whoever paid
   it: `{"type": "Tendermint", "coin": <platform ticker>, "amount": <fee in
   platform display units>, "gas_limit": <integer>}`, taken from the first
   coin of the transaction's fee and from its gas limit.
2. The fee enters the amounts of at most one row per transaction: the first
   transfer that is in the platform denom and sent by this account.
3. When this account sent a token transfer and no row of the transaction
   has taken the fee under item 2, one extra row is stored for the
   **platform coin**: `transaction_type` `"FeeForTokenTx"`, `coin` the
   platform ticker, `to` empty, `total_amount` and `spent_by_me` the fee,
   `received_by_me` zero, `my_balance_change` minus the fee, and every
   other member (hash, `from`, height, time, memo, `fee_details`) as in the
   token row. A transaction gets at most one such row.
4. A token row's amounts therefore never contain the fee, and the platform
   coin's history shows the fee of a token transaction as a row of its own.
   A fee that another account paid (an incoming transfer, an IBC packet
   delivered by a relayer) enters no amount; it is visible in `fee_details`
   only.


R36.14.9 **The other members of a row.** `tx_hash` is the node's hash of
the transaction in upper-case hex; `block_height` its height; `timestamp`
the block time in seconds; `memo` the transaction's memo, always present
and possibly empty; `kmd_rewards` is absent. `confirmations` is computed
when the row is served: the current height minus the row's height plus
one, or zero when the row's height is above the current height. For
`tx_hex` see D36.12.

R36.14.10 **Identifier, stored form and order.** `internal_id` is 33 bytes
(66 hex characters), built from the position of the transfer among the
transfers of its transaction (counted from zero in the order R36.14.5
yields them) and from the transaction hash: the position as a
little-endian integer of the build's native word size (8 bytes on a 64-bit
native build, 4 bytes in the 32-bit browser build), followed by the
characters of the upper-case hex hash, cut to 33 bytes in total and
written in reverse byte order. The fee row of R36.14.8 item 3 carries the
same 33 bytes in forward order. A row whose identifier is already stored is
not written again; this is what makes a repeated pass, the second query
and a restart add nothing. A stored row holds its three amounts as
base-unit integers and a zero balance change, and the scaling of R36.14.7
is applied when the row is served; a fee row is stored in display units
and served as stored. Answers list rows by descending block height and,
within a height, by ascending identifier. Paging by page number or by
`FromId` follows the shared contract; a `FromId` that matches no row
yields an empty `transactions` list with `skipped` 0 and the true `total`,
not an error.

R36.14.11 **`sync_status`.** The history state of a platform coin is
reported by `my_tx_history` (both methods) as `sync_status`; a token
reports its platform coin's state.

| State | When |
| --- | --- |
| `{"state": "NotEnabled"}` | the coin was activated without `tx_history: true`; it never changes |
| `{"state": "NotStarted"}` | from activation with `tx_history: true` until the first complete pass ends, **including the whole time the first pass is fetching** |
| `{"state": "Finished"}` | from the end of the first complete pass, for the rest of the coin's life: later passes, the waits between them and the pauses after a node failure all report `Finished` |
| `{"state": "Error", "additional_info": {"message": <text>}}` | the fetcher has stopped for good because the history store could not be prepared or written; the text is not bound |

The reference never reports `InProgress` for this family.

R36.14.12 **`tx_history` false versus true in the answers.**

| Activation | mmrpc-2.0 `my_tx_history` (coin and its tokens) | legacy `my_tx_history` |
| --- | --- | --- |
| `tx_history: false`, and this node's database has no history store for the platform ticker | error `StorageIsNotInitialized` (string data, HTTP 500) | success, empty list, `sync_status` `NotEnabled` |
| `tx_history: false`, and a store exists from an earlier activation with `tx_history: true` | success from the stored rows, which are no longer extended; `sync_status` `NotEnabled` | as above |
| `tx_history: true` | success with the rows fetched so far; `sync_status` per R36.14.11 | success, empty list, `sync_status` per R36.14.11 |

With `tx_history: true` the store is prepared by the fetcher's first step,
after the activation has answered, so a request in that short interval can
still get `StorageIsNotInitialized`. The other errors are the shared ones:
`CoinIsNotActive` (HTTP 404), `RpcError` (HTTP 500) when the current height
cannot be read from a node, `StorageError` (HTTP 500).

> **Code-quality findings (informative; reference behavior that Reloaded
> should not copy as it stands; each names its trigger and a proposed
> remedy; D36.10 -- D36.14 put the choices to the maintainer).**
>
> 1. *Stored units (trigger: a code comment states the problem).* Storing
>    base-unit integers and scaling them when a row is served (R36.14.10)
>    repeats the unit conversion on every read. When a stored
>    amount is not a non-negative integer that fits 64 bits, serving the
>    row aborts the request instead of answering an error. Remedy: answer
>    `StorageError`; never abort.
> 2. *Earlier identifier forms (trigger: a code comment states the
>    problem).* Before it writes a row, the reference deletes a row stored
>    under either of two earlier 32-byte forms of the identifier (the first
>    32 bytes of the byte string of R36.14.10, in reverse and in forward
>    order). The step runs on every pass at both rows and removes nothing
>    from a current store. It matters only for a store written by an older
>    reference version (D36.11).
> 3. *Full re-read at every start (trigger: apparent with little extra
>    effort).* The reference works out the height of its newest stored row
>    at start and then does not use it: every start walks the whole history
>    again and relies on the identifier check to add nothing. Remedy: bound
>    the queries by height (the CometBFT query language has a height
>    clause), or accept the cost.
> 4. *A failed start is silent (trigger: apparent).* If the first balance
>    read fails when the fetcher starts, the fetcher ends at once: nothing
>    is retried, and the state stays `NotStarted` for the coin's life.
>    Remedy: retry with the 30-second pause of R36.14.3.
> 5. *Fee-only and staking amounts (trigger: apparent).* On a chain whose
>    bank module emits a `transfer` event for the fee payment, a
>    transaction of this account with no other transfer (a governance
>    vote, for example) appears as a `StandardTransfer` from this account
>    to the chain's fee-collector account whose amount is the fee, with the
>    fee added once more under R36.14.8 item 2, so `total_amount` and
>    `spent_by_me` are twice the fee. A reward claim counts the claimed
>    amount as received and leaves the fee this account paid out of every
>    amount. A transaction that delegates to, undelegates from or claims
>    from several validators yields one row, for the first event only.
> 6. *Silently missing rows (trigger: apparent).* A transfer whose
>    base-unit amount does not fit 64 bits is left out without an error
>    (for a coin of 18 decimals that is any amount above about 18.4 whole
>    coins); a transfer whose amount text happens to equal the fee coin's
>    text is taken for the fee payment and left out; a transaction with an
>    empty fee list is left out. Remedy: arbitrary-precision amounts, and
>    identify the fee payment by its recipient as well as its amount.
> 7. *Token label and custom tokens (trigger: apparent).* A denomination is
>    resolved against the whole coins configuration and the first entry
>    with that `denom` wins, whatever its `platform`, so a row can carry
>    the ticker of another chain's entry; a custom token (R36.1.2, not in
>    the configuration) never gets a row. Remedy: D36.13.
> 8. *Stream payload (trigger: contradiction between two things read side
>    by side).* New rows are offered to the transaction-history stream of
>    [Chapter 10](10-sse-streaming.md) in their stored form, with
>    base-unit amounts and a zero balance change, while `my_tx_history`
>    serves the same rows in display units. Chapter 10 R43 binds the
>    served form; Reloaded SHOULD stream what it would serve.
> 9. *Two identifiers for one transaction (trigger: contradiction between
>    two things read side by side).* The `internal_id` a `withdraw` result
>    reports is a SHA-256 over the hash text (and the token id), not the
>    identifier of R36.14.10, so a client cannot match a withdraw result
>    to its later history row by `internal_id`; `tx_hash` matches.
> 10. *Pairing by position (trigger: apparent).* In a transaction with
>     several bank transfers and several HTLC or IBC events (a relayer's
>     batch), a module event is applied to whichever transfer is read
>     first, not to the transfer it belongs to.
>

D36.10 *(maintainer decision.)* *History state while fetching.* Options:
(a) the reference's values exactly (R36.14.11): `NotStarted` through the
first pass, `Finished` ever after, a failed start silent; (b) `InProgress`
while the first pass runs, `Finished` once it has completed and through
every later pass, `Error` only when the fetcher has stopped for good, and
a failed start retried (finding 4); (c) as (b), and `InProgress` during
later passes too. Clients already handle `InProgress` for the UTXO family.
Recommended: (b); the content of `additional_info` for `InProgress` is
then Reloaded's own (the node's `tx_search` answer carries the total
count, so a remaining-transactions figure is available).

> **Status note (2026-11-15, Reloaded-original).** Decided: (b). The history
> state is `InProgress` during the first pass, with `additional_info`
> `{"transactions_left": <n>}` where `n` is what remains of the current query's
> `tx_search` total (`null` until the first answer arrives); it is `Finished`
> after the first complete pass and through every later pass, wait and pause;
> it is `Error` with `additional_info` `{"message": <text>}` only when the
> fetcher has stopped for good or the history store cannot be prepared or
> written. A failed first balance read is retried after the 30-second pause,
> not silent. Built in PR #363; tests T36.38.

D36.11 *(maintainer decision.)* *Interchange with a store written by the
reference.* A user who moves a data directory between the reference and
Reloaded brings Tendermint history rows along. Options: (a) Reloaded
writes the identifier of R36.14.10 byte for byte and the stored form of
R36.14.10 (base-unit amounts scaled when served), and carries the clean-up
of finding 2, so that either program continues the other's store without
duplicate rows; the identifier then depends on the build's word size, as
at the reference, and only the first 25 characters of the hash take part
in it on a 64-bit build; (b) Reloaded uses an identifier and a stored form
of its own and rebuilds the Tendermint history store when it first opens
one it did not write (the history is always re-creatable from the chain,
R36.14.3), accepting that the reference would show duplicates if the
directory were moved back. Recommended: (a), with finding 1's remedy; it
is the persistent-data compatibility the project keeps by default.

> **Status note (2026-11-15, Reloaded-original).** Decided: (a). The
> identifier (R36.14.10, from the transfer position and the upper-case hash)
> and the stored form (base-unit amounts scaled when served, the fee row in
> display units, a zero stored balance change) are byte-compatible with the
> reference as R36.14.10 states it (unit-tested against the chapter's wording;
> not yet checked against a store written by GLEEC KDF; the position count follows
> R36.14.19), and each write first deletes the rows under the two earlier
> 32-byte identifier forms (finding 2). Finding 1's remedy applies: a stored
> amount that is not a non-negative integer is answered as `StorageError`,
> never an abort. The identifier depends on the build's word size, as at the
> reference. The identifier and the stored form are each computed in one
> function (`history_internal_id`, `stored_form` and `served_form` of
> `mm2src/coins/tendermint/tendermint_tx_history.rs`). Built in PR #363; test
> T36.37.

D36.12 *(maintainer decision.)* *Amounts of fee-only transactions and
reward claims, and the content of `tx_hex`.* At the reference a row's
`tx_hex` is the encoded value of the transaction's **first message**, not
the signed transaction, so it cannot be broadcast or decoded as a
transaction; and the amounts of finding 5 are wrong. Options: (a) the
reference's values for all three; (b) Reloaded counts the fee once in a
fee-only row, counts the fee of a reward claim in `spent_by_me` and
`total_amount`, and writes the raw signed transaction to `tx_hex`.
Recommended: (b). Under D36.11 (a) a row is written by whichever program
sees the transaction first and is not rewritten, so the two programs can
show different amounts for these kinds only. Whether a multi-validator
staking transaction gets one row per validator is part of the same
decision; recommended: yes.

> **Status note (2026-11-15, Reloaded-original).** Decided: (b). A row of a
> fee-only transaction counts the fee once (the transfer that is the fee
> payment is the row, and the fee is not added again); a reward claim counts
> its fee in `spent_by_me` and `total_amount`; `tx_hex` is the raw signed
> transaction; a transaction that delegates to, undelegates from or claims from
> several validators gets one row per validator, the first of them bearing the
> fee. Built in PR #363; test T36.34.

D36.13 *(maintainer decision.)* *Which denominations get rows, and under
which ticker.* Options: (a) the reference's lookup (finding 7); (b) a
denomination is resolved, in this order, to the platform coin, to a token
activated on this platform (custom tokens included), and to a coins
configuration entry whose `platform` is this coin; anything else gets no
row. Recommended: (b). Under (b) a token activated after the transfers
happened still shows them only when the fetcher reads those transactions
again, which D36.11 and finding 3 bear on.

> **Status note (2026-11-15, Reloaded-original).** Provisional: the decision
> is suspended pending the §36.14.6 precision round; the code resolves
> denominations as (b) in isolation. A
> denomination resolves, in this order, to the platform coin, to a token
> activated on this platform (a custom token included), and to a
> coins-configuration entry of type `TENDERMINTTOKEN` whose `platform` is this
> coin; the row's `coin` and decimals are those of the match. Anything else
> gets no row. The fetcher resolves again for every page, so a token activated
> later is recognized from the next page on; a transfer read before that
> shows only when its transaction is read again. Built in PR #363; tests T36.31,
> T36.35.

D36.14 *(decided for 0.5.0 by the maintainer's decision of 2026-11-15: no
row; the planning question is open.)* *Balance changes that have no
transaction.* The reference's history has **no row and no other record**
for an HTLC that the chain refunds on its own, and none for an
undelegation that completes: both are effects of block processing, not of
a transaction, so `tx_search` never returns them (R36.14.3), and the
reference synthesizes nothing. The account's balance grows; the balance
read of R36.14.3 notices the change and starts a pass, which finds nothing
new. The history then shows the HTLC creation as spent and nothing coming
back, so the rows no longer add up to the balance. What the swap records
show is in [Chapter 18](18-tendermint-ibc-htlc.md) R18.11.58. For the
plan, the options are: (a) a row written by the swap layer when it
observes the `Refunded` state (the marker of Chapter 18 D18.11.3 names the
HTLC id and the closing height), covering only swaps this node ran;
(b) rows derived from block-level events (CometBFT `block_search` and
`block_results`), covering every refund and completed undelegation of the
account, on nodes that index block events; (c) none. A row of either kind
needs an identifier that is not a transaction hash and a
`transaction_type` value; a value outside R36.14.6 is unknown to existing
clients.

> **Status note (2026-11-15, Reloaded-original).** Decided for 0.5.0: no
> row. Nothing is synthesized for a chain-side HTLC refund or a completed
> undelegation; the balance read of R36.14.3 notices the change and the pass it
> starts finds no new transaction. The planning question stays open in
> `docs/plans/tendermint-activation-features.md`.

> **As-built note (2026-11-15, Reloaded-original).** R36.14.1 -- R36.14.12 are
> built in PR #363 in `mm2src/coins/tendermint/tendermint_tx_history.rs`.
> `rows_for_tx` classifies one transaction from its result events (R36.14.5 --
> R36.14.9); `history_internal_id`, `fee_row_internal_id`,
> `obsolete_internal_ids`, `stored_form` and `served_form` hold R36.14.10 and
> D36.11 (a); `start_tendermint_history` runs the fetcher (R36.14.3, R36.14.4,
> R36.14.11, D36.10); `history_scope` and `served_row` serve `my_tx_history`
> (`mm2src/coins/my_tx_history_v2.rs`), whose store read takes the new
> `HistoryCoinType::AccountCoin` and `AccountToken` scopes so that only the rows
> naming the activated address are served (R36.14.2). The transaction-type set
> gained `FeeForTokenTx`, `CustomTendermintMsg` and `TendermintIBCTransfer`;
> `TransactionDetails` gained the optional `memo`, which a history row always
> carries (R36.14.9). Readings kept as built: (1) the fee payment of R36.14.5 item 4 is looked
> for when the result has more than one `transfer` event, and only the first
> such event is left out; (2) a transfer is the fee payment itself (D36.12 (b),
> counted once) only when it is the sole `transfer` event, moves exactly the fee
> text, and no decoded message of the transaction (a bank send to that
> recipient of that amount, an IBC transfer or an HTLC creation) pays it, so a
> genuine send of exactly the fee amount stays a transfer; (3) a reward claim
> bears the fee although its sender is the validator (D36.12 (b)); (4) a
> transaction with several staking events of one type gets one row per
> validator (D36.12 (b)), the events after the first following the first
> event's transfers in the list, so no position that R36.14.19 item 2 computes
> moves. The position, the pairing of module events, the staking events
> without their attributes and the fee forms follow R36.14.19 -- R36.14.22
> (see the status note below). What is still provisional: the set of
> denominations that resolve (D36.13, D36.21), the fee list and the fee
> payment text (D36.19), the rows for a transaction without a fee or a staking
> event without `delegator` (D36.20); the unit test `what_yields_no_row` is
> marked for the first of them. The block time is read with the SDK's `GetBlockByHeight` query. The fetcher and the method
> exist on native builds only (native only in 0.5.0): the browser build has no
> versioned history store for any coin family (`NotSupportedInWasm`), a
> limitation shared by every history family and not a Tendermint one. Tests: T36.30 -- T36.39 in
> `mm2src/coins/tendermint/tendermint_swap_flow_tests/tx_history_tests.rs`.

> **Status note (2026-11-15, Reloaded-original; belongs under §36.14.6 once the
> branch carries it).** Built of R36.14.19 -- R36.14.22 in `rows_for_tx` and
> the functions it calls in `tendermint_tx_history.rs`: the repeated-event
> comparison per event type (`drop_repeated_events`); the transfer list read
> from the last `transfer` event to the first, a coin counted only with an
> `amount`, its party attributes, a u64 integer and a denomination that
> resolves, module correction before merging (`transfers_of`, `push_merged`);
> the identifier bytes of item 7, including the 32-bit form
> (`history_internal_id_for_word`); the pairing of R36.14.20 by order, which is
> D36.22 (a), isolated in `module_event_queue` and `apply_module_event`; the
> staking events without `delegator`, `validator` or a counted amount giving no
> row, `unbond` included; a first fee entry above u64 leaving the transaction
> out; the fee attributed once to the first written entry this account sent
> (a fee row when that entry is in a token denomination). Tests T36.45 --
> T36.48 (the reference's values) are in `tx_history_tests.rs`. Waiting for
> D36.19 -- D36.22 and D36.13: `fee_payment_text` (D36.19), `resolve_denom` and
> `counted_coins` (D36.13, D36.21), and no row for a transaction with an empty
> fee list or a staking event without `delegator` (D36.20), which T36.48 (iv)
> and T36.47 (i) record as not written; the empty-fee-list case has no unit test
> yet.

### 36.14.2 WalletConnect activation

R36.14.13 **Accepted form.** At both rows the reference accepts exactly
form 2 of R36.1.3, `{"type": "WalletConnect", "params": {"session_topic":
<string>}}`, and no other spelling (form 3 is Reloaded's addition).
`session_topic` is any JSON string; its content is not checked when the
request is parsed, and a topic that names no live session fails in step 3
of R36.1.4 as R36.14.14 describes. When `activation_params` is present,
the node's own login takes no part in the activation: neither the context
signing policy nor `path_to_address` is read.

R36.14.14 **What is read from the session.** The activation reads the
following, in this observable order; the first failure ends it with
`PlatformCoinCreationError` (R36.1.6):

1. *Session.* A live, settled session with that topic exists.
2. *Chain.* The session grants the chain `cosmos:<chain_id>`, where
   `chain_id` is the coin's `protocol_data` member (R36.3.1): the `cosmos`
   namespace lists it among its `chains`; or, when that namespace lists no
   chains, a key record of the session (below) has that chain id; or the
   session has a namespace keyed by the full chain identifier.
3. *Account.* The `cosmos` namespace's `accounts` hold an account of that
   chain (a CAIP-10 identifier whose chain reference is `chain_id`).
4. *Public key.* When the session's properties carry key records
   ([Chapter 22](22-walletconnect-v2.md) §22.8.1.6), the **first** record
   supplies `pubKey`, `algo` and `isNanoLedger`, and the wallet is **not
   contacted**. Otherwise the node sends one `cosmos_getAccounts` request
   to the wallet and takes the first account of the answer; the activation
   fails when the wallet refuses, does not answer within the request's
   time-to-live (five minutes), or returns no account.
5. *Key form.* `algo` must be `secp256k1` or `tendermint/PubKeySecp256k1`,
   and the key must be a valid secp256k1 public key. Key bytes given as
   text are read as hex when every character is a hex digit and the length
   is even, and as base64 otherwise; an enumeration answer may also give
   them as an array of byte values.
6. *Address.* The activated address is the bech32 account of that public
   key under the coin's own `account_prefix`
   ([Chapter 18](18-tendermint-ibc-htlc.md) §18.2.2). The address the
   session reports for the account is **not compared** with it; there is
   no address-prefix check beyond the chain check of item 2.
7. *Ledger flag.* The coin is treated as Ledger-backed when the first key
   record's `isNanoLedger` is true, and as not Ledger-backed when the
   session carries no key records.

Nothing else is taken from the session at activation. The activation
result is the ordinary R36.1.5 result.

R36.14.15 **Signing.** A coin activated this way has every transaction
signed by the wallet and broadcasts it itself. The wallet method is chosen
by the Ledger flag alone and is the same for a `withdraw`, a staking
request and a swap transaction: `cosmos_signAmino` when the session is
Ledger-backed, `cosmos_signDirect` otherwise ([Chapter 22](22-walletconnect-v2.md)
RP3, §22.8.1.2 for both payloads). Because the reference's Amino form
covers the bank send only (R36.14.18), a Ledger-backed session can sign a
plain `withdraw` and nothing else. Before each request the node checks
again that the session exists and grants the chain. A `withdraw` answers
with a **signed** transaction (`tx_hex`, `tx_hash`), as a local-key
withdraw does, and broadcasting it stays the caller's ordinary second
step; the `from` selector of the request is ignored; the fee is computed
from the fixed assumed gas of Chapter 18 R18.11.9, because a simulation
would need a signature. When the wallet refuses, does not answer within
the time-to-live, or the session is gone or no longer grants the chain,
the request fails with its **internal-error type**: `InternalError` for
`withdraw` (string data, HTTP 500; the text is not bound), and the
internal-error type of the staking requests likewise. The reference has no
distinct error type for these cases.

> **Upstream divergence and code-quality findings (informative).**
>
> 1. *First key record (trigger: a code comment states the problem).* The
>    reference takes the public key and the Ledger flag from the first key
>    record of the session, whichever chain that record belongs to, and
>    so the flag can be that of an account on another chain. For a wallet session that
>    spans chains with different key derivations the activated address is
>    then not the wallet's account on the coin's chain. Chapter 22
>    §22.8.1.7 already binds the record that matches the signing account;
>    D36.16.
> 2. *Enumeration request (trigger: apparent).* The reference sends the
>    session's account address as the parameter of `cosmos_getAccounts`,
>    where the public method reference and Chapter 22 §22.8.1.2 define no
>    parameter. Reloaded follows Chapter 22.
> 3. *The signed echo (trigger: contradiction between two things read side
>    by side).* For `cosmos_signDirect` the reference assembles the
>    transaction from the body and auth-info bytes the wallet echoes
>    (Chapter 22 requires the echo) without comparing the body with the one
>    it asked to have signed, whereas its public-key path does make that
>    comparison (Chapter 18 R18.11.52). Proposed: Reloaded compares the
>    echoed body with the requested one and fails the request on a
>    difference; the auth-info bytes may legitimately differ.
> 4. *The Amino answer (trigger: contradiction between two things read
>    side by side).* The reference reads the wallet's answer in the
>    direct-mode shape for both methods, while the public Amino method
>    answers with the Amino document. Whether a Ledger-backed WalletConnect
>    `withdraw` completes at the reference **could not be established** by
>    reading; T36.44 is the lab row. Reloaded follows Chapter 22 §22.8.1.2
>    for the Amino answer and its broadcast assembly.
>

D36.15 *(maintainer decision.)* *Error type for a wallet refusal or a lost
session.* Options: (a) the reference's `InternalError`, which a client
cannot tell from a fault of the node; (b) one typed answer for "the
external wallet did not sign" (refusal, no answer in time, session gone or
chain no longer granted): either `SigningError`, a type of the reference's
withdraw error set that the reference does not use for this family and
that Reloaded's set does not have yet, or a new type; its HTTP status is
part of the decision; the same for the staking requests. Recommended: (b);
no client written against the reference can depend on a type the
reference never sent for this case.

D36.16 *(maintainer decision.)* *Which key record, and the address
check.* Options: (a) the reference: the first key record, no comparison
with the session's account (R36.14.14 items 4, 6 and 7); (b) the key
record whose chain id is the coin's, falling back to `cosmos_getAccounts`
when there is none, and a refusal with `PlatformCoinCreationError` when
the address derived from the key differs from the session's account for
that chain. Recommended: (b).

### 36.14.3 Public-key activation, the `WithPubkey` form

R36.14.16 **Account.** The activated address is the bech32 account of the
supplied key bytes under `account_prefix`, computed by the secp256k1 rule
of Chapter 18 §18.2.2 for **both** key types. No node, wallet or session
is contacted for the key, and the node's own login takes no part
(R36.14.13). `is_ledger_connection` only sets the Ledger flag of
R36.14.18.

R36.14.17 **What such a coin can do at the reference.** The same holds
for a token on such a platform coin.

| Operation | Ledger flag off | Ledger flag on |
| --- | --- | --- |
| activation, balance, address, `tx_history` and `my_tx_history` | as for any coin | as for any coin |
| `withdraw`, same chain | **succeeds** with an *unsigned* transaction: the result has no `tx_hex` and no `tx_hash`, and carries instead a member `sign_doc` with `body_bytes` and `auth_info_bytes` (arrays of byte values), `chain_id` (string) and `account_number` (integer) | **succeeds** with an unsigned transaction: the result carries `legacy_amino_json` (the Amino sign document) and `original_tx_type_url` (the protobuf type URL of the message) |
| `withdraw` to another chain (IBC) | succeeds as above | fails with `InternalError` |
| delegation, undelegation, reward claim | succeed with the unsigned form above | fail with the request's internal-error type |
| `setprice`, `buy`, `sell` | **accepted**; no refusal is made for the missing key (the swap itself: Chapter 18 R18.11.50 and R18.11.52) | refused as a wallet-only coin, with the ordinary legacy answer for such a coin ([Chapter 60](60-trading-floors-and-refusal-contract.md)); the text is not bound |
| private-key display, public-key query | fail | fail |

The unsigned result is otherwise an ordinary withdraw result (amounts,
`fee_details`, `from`, `to`, `transaction_type`, `memo`); nothing is
signed or broadcast by the node, and the caller's own wallet signs and
broadcasts the document. The `from` selector is ignored and the fee comes
from the fixed assumed gas (Chapter 18 R18.11.9).

R36.14.18 **What the Ledger flag changes.** The flag is set by
`is_ledger_connection: true` or by a Ledger-backed WalletConnect session
(R36.14.14 item 7). It has four effects and no other:

1. the platform coin **and every token on it** report wallet-only, so
   `setprice`, `buy` and `sell` refuse them (R36.14.17);
2. an unsigned transaction is produced in the Amino form instead of the
   `sign_doc` form, and a WalletConnect request uses `cosmos_signAmino`;
3. the Amino form exists for the bank send only, so every other message
   (IBC transfer, staking, HTLC creation and claim, the burn-form fee)
   fails when it is built;
4. the fee amount of a `withdraw` is three times the computed amount.


D36.17 *(maintainer decision.)* *An `ed25519` key in the public-key
form.* R36.1.3 binds both key types. At the reference an `ed25519` key
activates an address computed by the secp256k1 rule (R36.14.16), which is
not the account the public Cosmos SDK address rule gives that key, and
such a key cannot serve as an HTLC key, which is a 33-byte secp256k1 key
([Chapter 51](51-legacy-v1-swap-state-machine.md)). Options: (a) the
reference's derivation; (b) refuse `ed25519` as a request-parse failure,
which amends R36.1.3 and T36.8; (c) accept it and derive the address by
the public SDK rule for that key type, the coin being watch-only.
Recommended: (c), or (b) if (c) is not wanted; not (a), which shows an
account that is not the key's.

D36.18 *(maintainer decision.)* *What a watch-only coin's `withdraw`
answers in Reloaded.* The public-key form is built in 0.5.0 as a
watch-only coin, and Chapter 18 R18.11.23a already refuses its orders.
Options for `withdraw` and the staking requests: (a) the reference's
unsigned results of R36.14.17, in both forms, which involve no key and
are what lets a client's own wallet sign; (b) a typed refusal. Recommended:
(a). The external-signing channel of Chapter 18 R18.11.52 is not needed
for (a) and stays unbuilt while R18.11.23a stands.

### 36.14.4 Notes on existing rules, and our side

> **Notes on existing rules (this section edits none of them).**
>
> 1. *R36.1.2, `priv_key_policy`.* The row binds "accepted and ignored",
>    which is the reference's behavior (the member is not read, and no
>    Tendermint activation is ever treated as a hardware request,
>    R36.6.7). By the maintainer's decision of
>    2026-11-15 Reloaded refuses a hardware value with a typed error; the
>    shared set already has `UnexpectedDeviceActivationPolicy` (no data,
>    HTTP 400, R36.1.7), which the finding of §36.7 proposed. The rule that
>    implements the decision takes no forbidden-class input and is the
>    clean side's to write; it replaces the row for hardware values only.
> 2. *R36.1.4 step 3.* It says a WalletConnect activation contacts the
>    WalletConnect peer. At both rows it does so only when the session
>    carries no key records (R36.14.14 item 4).
> 3. *R36.1.3 form 1.* It calls the coin watch-only. At the reference the
>    coin is more than that: `withdraw` yields a document for an external
>    signer and, without the Ledger flag, the coin may place orders
>    (R36.14.17). "Watch-only" is Reloaded's scope for the form (D36.18).
> 4. *[Chapter 22](22-walletconnect-v2.md) RP8.3, Cosmos bullet.* It names
>    the wallet's account enumeration as the source of the public key; at
>    the reference the session's key records come first and the
>    enumeration is the fallback (R36.14.14 item 4). §22.8.1.7's matching
>    record and §22.8.1.2's parameterless enumeration differ from the
>    reference as findings 1 and 2 of §36.14.2 record.
> 5. *[Chapter 18](18-tendermint-ibc-htlc.md) §18.7.4.* It sketches a
>    history binding that classifies by message type. The reference
>    classifies by result events (R36.14.5), which is what makes incoming
>    transfers, relayed IBC packets and claims by another account visible.
> 6. *[Chapter 10](10-sse-streaming.md) R43.* See finding 8 of §36.14.1.
>

> **Our side (informative; this repository's tree, 2026-11-15).**
> `tx_history: true` is accepted and starts nothing (§36.7 item 9); a
> Tendermint coin's history state is always `NotEnabled`; on a native
> build the mmrpc-2.0 `my_tx_history` answers `NotSupportedFor` (HTTP 400)
> for a Tendermint coin or token. The transaction-type set has no `FeeForTokenTx`,
> `CustomTendermintMsg` or `TendermintIBCTransfer` value yet, while the
> three `msg_type` values and the `Tendermint` fee-details form of
> R36.14.8 item 1 exist. A non-null `activation_params` is rejected (the
> interim rule of §36.7), the activation policy has the local-key form
> only, and the Cosmos request builders of Chapter 22 exist without an
> activation path that reaches them.

### 36.14.5 Tests

Rows T36.30 -- T36.39 run against a mocked node; the platform coin has 6
decimals, the token 6 decimals, and every fee is 5,000 base units of the
platform denom (0.005). Figures are computed from the rules.

T36.30 *Plain transfers (R36.14.6, R36.14.7).* An outgoing bank send of
1,000,000 base units yields one `"StandardTransfer"` row with
`total_amount` 1.005, `spent_by_me` 1.005, `received_by_me` 0 and
`my_balance_change` -1.005; an incoming send of 2,000,000 yields 2, 0, 2
and 2; a send of 300,000 to the account itself yields 0.305, 0.305, 0.3
and -0.005. Each row's `fee_details` is `{"type": "Tendermint", "coin":
<platform ticker>, "amount": "0.005", "gas_limit": <the transaction's>}`
and its `confirmations` is the current height minus its height plus one.

T36.31 *Token transfer and its fee row (R36.14.2, R36.14.8).* An outgoing
token send of 2,500,000 base units yields, in the token's history, one
row `{"TokenTransfer": <token id>}` with 2.5, 2.5, 0 and -2.5 and `coin`
the token ticker; and in the platform coin's history one row
`"FeeForTokenTx"` with the same `tx_hash`, `to` empty, 0.005, 0.005, 0
and -0.005. The token id is the SHA-256 of the lower-cased denom. An
incoming token send yields the token row only.

T36.32 *HTLC rows (R36.14.6, R36.14.7).* A creation of 1,000,000 in the
platform denom by this account yields `{"CustomTendermintMsg":
{"msg_type": "SendHtlcAmount", "token_id": null}}`, `from` this account,
`to` empty, with 1.005, 1.005, 0 and -1.005. A claim that this account
sends for an HTLC paying it yields `"SignClaimHtlc"` with 1.005, 0.005, 1
and 0.995. A claim sent by another account that pays this account yields
`"ClaimHtlcAmount"` with 1, 0, 1 and 1. A creation in the token denom
yields the token row (with the token id) and one `"FeeForTokenTx"` row.
A creation by another account yields no row.

T36.33 *IBC rows (R36.14.5, R36.14.6).* An outgoing IBC transfer yields
`{"TendermintIBCTransfer": {"token_id": null}}` with `from` this account
and `to` the receiver's address on the other chain; an incoming one, in a
transaction a relayer paid for, yields `from` the sender's address on the
other chain, `to` this account, 0 spent, and the relayer's fee in
`fee_details` only. The token variants carry the token id.

T36.34 *Staking rows (R36.14.5, R36.14.6; D36.12).* A delegation of
1,000,000 yields `"StakingDelegation"`, `from` the delegator, `to` the
validator, with 1.005, 1.005, 0. An undelegation yields
`"RemoveDelegation"`, `from` this account, `to` empty, with 0.005, 0.005,
0. A reward claim of 12,345 base units yields `"ClaimDelegationRewards"`,
`from` the validator, `to` this account, `received_by_me` 0.012345; under
D36.12 (a) `spent_by_me` is 0 and `total_amount` 0.012345, under (b)
0.005 and 0.017345. A fee-only transaction yields, under D36.12 (a), a
`"StandardTransfer"` to the fee-collector account with `total_amount`
0.01; under (b) 0.005.

T36.35 *Left out (R36.14.4, R36.14.5).* A transaction with a non-zero
result code, one with an empty fee list, one whose block has no time, and
a transfer in a denomination that R36.14.5 (or D36.13) does not resolve
each add no row and do not stop the pass.

T36.36 *No duplicates (R36.14.3, R36.14.10).* A transaction returned by
both queries, a second pass after a balance change, and a restart of the
node with the same database each leave the row count unchanged. A node
failure in the middle of a page is followed by a retry after which the
rows of that page are present exactly once.

T36.37 *Identifier and order (R36.14.10; D36.11 (a)).* For a transaction
with hash `H` (64 upper-case hex characters) on a 64-bit native build,
the row of its first transfer has an `internal_id` of 33 bytes: the ASCII
codes of the first 25 characters of `H` in reverse order, then eight zero
bytes; the row of its second transfer ends in `00 00 00 00 00 00 00 01`
instead; the fee row of a token transaction has the bytes of its token
row in the opposite order. Rows at heights 10, 12 and 12 are served as
the two rows of height 12 in ascending identifier order, then the row of
height 10. A `FromId` that matches no row yields an empty list and the
true `total`.

T36.38 *History state (R36.14.11; D36.10).* Without `tx_history` the
state is `NotEnabled` before and after any number of requests. With it,
under D36.10 (a), the state is `NotStarted` until the first pass ends and
`Finished` from then on, including during a later pass and during a node
outage; under (b) it is `InProgress` during the first pass. A store that
cannot be written yields `Error` with a `message`.

T36.39 *`tx_history` false and true (R36.14.12).* A coin activated
without `tx_history` on a fresh database answers the mmrpc-2.0 method
with `StorageIsNotInitialized` (HTTP 500), for the coin and for a token on
it; the same coin on a database that holds an earlier history answers
with those rows and `NotEnabled`; the legacy method answers an empty
list in both cases and with `tx_history: true`.

T36.40 *WalletConnect activation (R36.14.14; D36.16).* With a session
whose properties carry a key record for the coin's chain, activation
sends no request to the wallet and the address is the bech32 account of
that record's key under `account_prefix`. With a session without key
records, activation sends exactly one `cosmos_getAccounts` request. An
unknown topic, a session that does not grant the chain, a session without
an account for the chain, an `algo` other than the two of R36.14.14 item
5, and a wallet that refuses the enumeration each yield
`PlatformCoinCreationError` and register no coin. Under D36.16 (b), a key
record of another chain placed first is not used, and a key whose address
differs from the session's account is refused.

T36.41 *WalletConnect signing (R36.14.15; D36.15).* A `withdraw` on a
session that is not Ledger-backed sends one `cosmos_signDirect` request
and answers with `tx_hex` and `tx_hash`; nothing is broadcast. A wallet
refusal, an unanswered request and a deleted session each fail the
`withdraw` with the type D36.15 selects. On a Ledger-backed session a
same-chain `withdraw` sends `cosmos_signAmino` and an IBC `withdraw`
fails without sending a request.

T36.42 *Public-key activation (R36.14.16, R36.14.17; D36.18 (a)).* With a
33-byte secp256k1 key and `is_ledger_connection: false` the coin
activates at the bech32 account of that key without contacting a wallet;
`withdraw` answers without `tx_hex` and `tx_hash` and with a `sign_doc`
whose `chain_id` is the coin's and whose `body_bytes` decode to a
transaction body holding the one bank-send message; a delegation answers
in the same form; the private-key display fails.

T36.43 *Ledger flag (R36.14.18).* With `is_ledger_connection: true` a
same-chain `withdraw` answers with `legacy_amino_json` and
`original_tx_type_url` and a fee three times that of the same request
without the flag; an IBC `withdraw` and a delegation fail; `setprice`,
`buy` and `sell` refuse the coin and a token on it as wallet-only.

T36.44 *Lab rows against reference release binaries (permitted-input
class R6, black box).* (i) For one funded account with transfers, a token
transfer, an IBC transfer and a delegation, the rows a reference binary
and Reloaded serve agree in `internal_id`, `transaction_type`, `from`,
`to` and amounts, except for the kinds D36.12 changes. (ii) A
Ledger-backed WalletConnect `withdraw` at the reference: whether it
completes (finding 4 of §36.14.2). (iii) Whether a reference node's
balance stream reports the balance change of an automatic HTLC refund;
this could not be established by reading, because it depends on which
block-level events the node delivers to a subscription.
