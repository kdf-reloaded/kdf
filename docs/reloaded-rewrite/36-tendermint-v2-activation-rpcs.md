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
   *Status note (2026-11-16, Reloaded-original).* Narrowed: the public-key form
   (form 1) is built (§36.14.3, as-built note) and malformed input is now
   `InvalidRequest`; the interim refusal above remains for the WalletConnect
   forms (forms 2 and 3) only, until their activation is built.
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
>
> **Status note (2026-11-17, Reloaded-original).** Decided by the maintainer
> for every coin family; this change does Tendermint. The 2026-11-15 note's
> "`IguanaPrivKey` activates as before" is superseded under an HD login. There
> a request with `priv_key_policy: IguanaPrivKey` (bare string or
> `{"type": "IguanaPrivKey"}`) is honored: the coin signs with the single
> internal key of the node's crypto context, the key the UTXO family takes for
> the same request, which under an HD login is the seed-derived internal key
> and not the key of an iguana login with the same passphrase. The coin's
> address is the bech32 account of that key under the coin's `account_prefix`;
> `path_to_address` plays no part, so a value that does not parse is not an
> error in this case. An absent value and `ContextPrivKey` keep R36.1.2 and
> R36.1.4 step 3 under an HD login (the key at `path_to_address`) and the single
> key under an iguana login; `IguanaPrivKey` under an iguana login is unchanged.
> The hardware refusal stays first and applies under both logins; when
> `activation_params` is present (public-key or WalletConnect form) the login
> and `priv_key_policy` play no part, as before (R36.14.13). Tokens of the
> platform follow the platform coin's key, and the activation result has the
> same shape as under an iguana login (`address` of the single key). The task
> variant (`task::enable_tendermint::init`) shares the build and behaves the
> same. Tendermint swaps are off, so no swap-key question arises now; the
> question is open for when they return. This is a deliberate divergence from
> the reference, which accepts and ignores the member and uses the HD key.

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
- *Scoped addition (2026-11-15, Spec Reader round 0.5.0-6) -- §36.14.6
  (R36.14.19 -- R36.14.22, the informative finding, D36.19 -- D36.22,
  T36.45 -- T36.48).* **KDF Dirty Gate: PASS on 2026-11-15, measure 0.05, no findings**
  (chapter 01 R40; the attestation closes this item). *Inputs:* the forbidden corpus at the rows `v2.6.0-beta`
  and `v3.0.0-beta` of chapter 02 §2.15A, read read-only and consulted
  **only** to establish, for the Tendermint history fetcher: which
  transfers of a transaction take a position in the row identifier and in
  which order they are counted; which module event corrects which transfer
  and what happens when the two are unequal in number; what a staking
  event without a delegator attribute yields; and which entry of a fee
  list is read, in which units, what becomes of the other entries and of
  an empty list; and the two rows' agreement on these points (the history
  fetcher and the history store are the same at both rows, and the
  denomination lookup was compared at both). Behavior, public wire shape
  and persisted-row format only; no private identifier, function body,
  helper decomposition, error, log, comment or test text was reproduced.
  Event and attribute names are those of the public chain modules (Cosmos
  SDK bank, staking and distribution; ibc-go transfer; irismod HTLC). The
  options of the D-items rest on this chapter's own text; the test figures
  are computed from the rules, not taken from any test. No cleared
  sentence of §36.14.1 -- §36.14.5 was edited.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-15, measure
  0.05 on the discretionary-expression basis of chapter 01 R35
  (dictated-interop and pinned-interface content excluded), no findings;
  every matching token was judged forced (public chain-module event and
  attribute names, public RPC and response field names, serialized type
  values, coins-configuration keys); the subsection passed two full passes,
  the second after four passages were reworded. Reference states (chapter 02
  R13): no findings; the only state names are `v2.6.0-beta` and
  `v3.0.0-beta`, both rows of §2.15A, read at the rows' full identifiers.
  Extent: the Tendermint transaction-history fetcher, the denomination
  lookup it uses, and the transaction-history store, at the rows
  `v2.6.0-beta` and `v3.0.0-beta`; 32 files per row in the mechanical
  comparison, one read in full, the lookup read at both rows (first pass: 34
  source files per row, plus the subsystem's test and test-support code and
  2 fixture files per row for the question whether the acceptance rows'
  figures occur there: none does). Classification: the byte layout of the
  row identifier is dictated by the persisted-row and wire contract; the
  reading order, the merging rule and the counted set are observable
  behavior binding through the persisted-row contract under the
  byte-compatibility decision; the pairing order is observable behavior,
  binding only under D36.22 (a). Method: reading of both added ranges
  against the subject matter at the tabled states; search for every
  backticked token, each match judged forced or free; scan for
  identifier-shaped words outside backticks against a compound-identifier
  list; word 4-gram comparison against comment and string-literal text at
  both rows, and a 3-gram comparison for the file read in full; row-to-row
  difference; the additions embed no code blocks, so the repository's
  clean-room gate tool was not applicable. The comparison concerns
  expression, not the accuracy of the statements.
- *Scoped addition (2026-11-15, Spec Reader round 0.5.0-7) -- §36.14.7
  (R36.14.23 -- R36.14.28, the informative notes, D36.23 -- D36.25,
  T36.49 -- T36.54).* **KDF Dirty Gate: PASS on 2026-11-15, measure 0.01, no findings**
  (chapter 01 R40; the attestation closes this item). *Inputs:* the forbidden corpus at the rows `v2.6.0-beta`
  and `v3.0.0-beta` of chapter 02 §2.15A, read read-only and consulted
  **only** to establish: for the indexed history store that the
  reference's coin families share, the names and declared columns of its
  tables and the stored forms of their values, which addresses are indexed
  for a row, when index entries are written and when they are removed, the
  rule by which the versioned history method selects, orders and counts
  rows, and what becomes of a row without an entry, of an entry without a
  row and of a store without the index table; for the Tendermint history
  fetcher, the requests of a pass, their order and paging, what ends a
  query and a pass, what starts a later pass and which pages it reads, how
  a node is chosen for a request, and what a block answer without a time
  yields; and the two rows' agreement on these points (the history store,
  the versioned history method, the Tendermint fetcher, the other
  families' fetcher as far as it uses the index, and the activation path
  that starts the fetcher are the same at both rows; the Tendermint coin's
  node choice, block read, balance read and denomination lookup were
  compared and are the same at both rows). Behavior, public wire shape and
  persisted schema only; no private identifier, function body, helper
  decomposition, query text, error, log, comment or test text was
  reproduced. Table, column, index and member names are those of the
  persisted store; method, service, event and attribute names are those of
  the public CometBFT RPC and of the public Cosmos SDK and ibc-go modules;
  the answer to a page number beyond the last page is that of the public
  CometBFT RPC. The *Our side* note rests on this repository's tree and on
  Chapter 38 §38.5A; item 5 of R36.14.26 and the options of the D-items
  rest on this chapter's own text; the test figures are computed from the
  rules, not taken from any test. No cleared sentence of §36.14.1 --
  §36.14.6 was edited.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-15, measure
  0.01 on the discretionary-expression basis of chapter 01 R35
  (dictated-interop and pinned-interface content excluded), no findings; the
  subsection passed on a second full pass, after the passages named by the
  first were removed or reworded (no statement of the relative order of two
  writes or two removals inside one database transaction remains). Matching
  tokens, all judged forced: persisted table, column and index names and the
  column declarations; persisted member names of the browser store;
  serialized transaction-type values, sync-status states and typed-error
  tokens; public RPC request and response field names; public CometBFT
  method and response-field names; public Cosmos SDK service, event and
  attribute names. Reference states (chapter 02 R13): no findings; the only
  state names are `v2.6.0-beta` and `v3.0.0-beta`, both rows of §2.15A,
  compared at the rows' full identifiers; the row-agreement statements were
  checked by row-to-row difference. Extent: the indexed transaction-history
  store shared by the coin families, in its native and browser forms; the
  versioned history method; the Tendermint history fetcher; the other
  families' fetcher as far as it uses the address index; the activation path
  that starts the fetcher; the Tendermint coin's node choice, block read,
  balance read and denomination lookup; the store's test code for scenario
  and name overlap — 3 units read in full and 4 in the parts concerned;
  mechanical comparison over 103 source files at the first row and 128 at
  the second; a wider word 4-gram sweep over 734 and 772 source files;
  fixture data files not included (a gap left visible). Classification: the
  table name pattern, the three tables, the columns and stored forms are
  dictated by the persisted-store contract; the selection, ordering,
  counting and paging rule is observable behavior of a public method over
  that contract, stated in words; the first-pass sequence and paging are
  observable in the requests. Method: reading of both ranges in full; search
  for every backticked token at both rows, each match judged forced or free;
  scan for identifier-shaped words outside backticks; word 4-gram and 3-gram
  comparison against comment, string-literal and code text; row-to-row
  difference; the additions embed no code blocks, so the repository's
  clean-room gate tool was not applicable. The comparison concerns
  expression, not the accuracy of the statements.
- *Scoped addition (2026-11-16, Spec Reader round 0.5.0-8) -- §36.14.8
  (R36.14.29 -- R36.14.35, the informative note, T36.55 -- T36.60).*
  **KDF Dirty Gate: PASS on 2026-11-16, measure 0.03, no findings**
  (chapter 01 R40; the attestation closes this item). *Inputs:* the forbidden corpus at the rows `v2.6.0-beta`
  and `v3.0.0-beta` of chapter 02 §2.15A, read read-only and consulted
  **only** to establish: for a Tendermint coin or token activated in the
  public-key form, which members the `withdraw` result has and lacks when
  it carries an unsigned transaction and what their values are; the JSON
  types and encodings of the members of `sign_doc` and the values its two
  byte strings hold; the members of the Amino document, their JSON types,
  which of them are present when zero or empty and which are never
  written, and the member that stands beside it; to which requests the
  fee factor of R36.14.18 item 4 applies; which request broadcasts a
  transaction that was signed outside the node and what that request
  checks; for the versioned and for the legacy history method, at which
  point the current height is read, the order of the conditions around
  that read, the error type and status when it fails, and whether the
  path is shared by the coin families the method serves; for the
  Tendermint height read, the requests it makes and the time given to a
  node; and the two rows' agreement on these points (the withdraw path of
  the coin and of the token, both forms of the unsigned document, the
  result type, the broadcast method, both history methods, the node
  choice and the height read were compared by row-to-row difference and
  are the same at both rows). Behavior and public wire shape only; no
  private identifier, function body, helper decomposition, error, log,
  comment or test text was reproduced. Member names are those of the wire
  results; message, sign-mode, type and method names are those of the
  public Cosmos SDK and of the public CometBFT RPC. The meaning of the
  members of the sign document, the canonical form of the Amino
  document, the content of a signed transaction and what a signer does
  with either document are the public conventions, which the subsection
  names instead of describing the reference. The *Our side* note rests on
  this repository's tree; the legacy error shape is that of Chapter 55
  R55.1.10; the test inputs were chosen by the author and the figures are
  computed from the rules, not taken from any test. No cleared sentence
  of §36.14.1 -- §36.14.7 was edited.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-16, measure
  0.03 on the discretionary-expression basis, no findings. Reference states
  (chapter 02 R13): PASS; the rows `v2.6.0-beta` and `v3.0.0-beta`, both
  tabled in §2.15A. Extent: the Tendermint coin-family subsystem as §36.14.8
  scopes it, at the two rows: the withdraw path of the platform coin and of
  the token, both forms of the unsigned sign document, the withdraw result
  type and its serialization, the legacy raw-transaction broadcast path, the
  versioned and the legacy transaction-history serving methods with their
  error type and status mapping, the node choice and the current-height
  read, and the test code of the same subsystem; 11 source files at each row
  compared mechanically, 4 read in their relevant parts at the first row,
  row-to-row difference over the same set. Method: reading, combined with
  mechanical comparison (identifier-shaped token intersection at both rows;
  5-, 4- and 3-word sequence comparison against comment and string-literal
  text; row-to-row difference); the additions embed no code blocks.

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
> `mm2src/coins/tendermint/tendermint_tx_history.rs`). The address index of the
> store is written as R36.14.23 -- R36.14.26 state, so that the reference lists our rows
> (checked on 2026-11-15 against a reference release build: from a fresh store written by the head 358a56944 of this change the reference listed the same 4 rows as this build, with equal (internal_id, address) index sets of 10 entries; a store written by the previous head of this change, after the backfill, was listed the same way) Checked again on 2026-11-16 on the head 4d7fb6326 of this change (two runs; the reference release build as the other writer): in every continuation leg (each program on a copy of the other's fresh store, and both on a store written by an earlier head of this change and backfilled) both programs ended with the same 4 listed rows, 6 stored rows and 10 index entries; the fresh first passes differed only in which transactions the node answered with..
> Built in PR #363; test T36.37.

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

> **Status note (2026-11-15, Reloaded-original).** Decided: (a'), the
> reference's lookup (finding 7, R36.14.19 item 4), for the counting and for the
> rows. A denomination resolves to the platform coin when it equals the platform
> `denom` exactly; otherwise to the first coins-configuration entry, in the order
> of the file and of whatever platform, whose `protocol_data` `denom` equals it
> without regard to letter case, and the transfer takes that entry's ticker and
> decimals. The row's `coin` is the ticker of the activated token when an
> activated token matches that entry, else the entry's ticker. A custom token
> that is absent from the coins file resolves to nothing: it takes no position
> and gets no row (documented limitation: the history of custom tokens is absent
> in 0.5.0). A coins-file token of another platform is counted and shown under
> its entry's ticker, in the history store of this platform coin. The lookup is
> read once at the start of a pass (`resolution_map`, `resolve_denom`); a change
> of what it resolves rebuilds the store (D36.21). Built in PR #363; tests
> T36.31, T36.35, T36.45 (ii).

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
> (see the status note below). Provisional: none open after the decisions
> of 2026-11-15 (D36.13, D36.19 -- D36.22). Reading kept as built: the coins-file lookup of D36.13 (a') takes
> "the first entry" among the entries whose `protocol_data` names a `denom` and a
> numeric `decimals`; an entry without either is skipped. The block time is read with the SDK's `GetBlockByHeight` query. The fetcher and the method
> exist on native builds only (native only in 0.5.0): the browser build has no
> versioned history store for any coin family (`NotSupportedInWasm`), a
> limitation shared by every history family and not a Tendermint one. Tests: T36.30 -- T36.39 in
> `mm2src/coins/tendermint/tendermint_swap_flow_tests/tx_history_tests.rs`.
>
> Address index (R36.14.23 -- R36.14.26 as built). The SQLite store keeps three
> tables per coin, `<T>_tx_history`, `<T>_tx_address` (`id INTEGER NOT NULL
> PRIMARY KEY`, `internal_id VARCHAR(255) NOT NULL`, `address TEXT NOT NULL`) and
> `<T>_tx_cache`, with every `-` of the ticker replaced by `_` (`table_ticker`), so tickers that differ only in a dash versus an underscore share one set of tables (reference-bound).
> An entry exists for each distinct address in the `from` and `to` lists of a
> stored row (`write_address_index`); it is written in the transaction that
> writes the row, and deleted with the row by its identifier
> (`delete_address_index`, also by `clear_history`). A page of the fetcher is
> stored in one database transaction, the removals of the earlier identifier
> forms included (`TxHistoryStorage::store_page`). The account-scoped read
> selects by the `token_id` column and by an entry for the account's address
> (`address_entry_condition`), counts a row once, and takes nothing from
> `details_json`. The preparation (`backfill_address_index`, called by the
> fetcher after `init`) creates the address table when absent, removes entries
> whose row is missing and writes the entries of every stored row that has none.
> Tendermint only for now: the address table is created, and entries are written
> and deleted, only for a coin whose table exists (created by the Tendermint
> fetcher, or already present in a store written elsewhere), so the stores of
> other families stay as they were: in this change only Tendermint coins write the
> index. A follow-up change of the same release adds it for every family the store
> serves (BCH and SLP tokens). Checked on 2026-11-15 against a reference release
> build: from a fresh store written by the head 358a56944 of this change the
> reference listed the same 4 rows as this build, with equal (internal_id, address)
> index sets of 10 entries; a store written by the previous head of this change,
> after the backfill, was listed the same way.
>
> **Status note (2026-11-16, Reloaded-original).** The scope of the address index
> is now every family the SQLite history store serves, not Tendermint only: `init`
> creates the three tables of R36.14.23 for every coin that opens the store,
> `write_address_index` and `delete_address_index` no longer test for the address
> table (the test, `address_index_enabled`, is removed), `clear_history` empties it
> for every coin, and the preparation step (`backfill_address_index`: orphan
> clean-up and backfill, R36.14.26 items 2 and 5) runs when any fetcher of the
> store starts, that is the Tendermint fetcher and the BCH/SLP fetcher
> (`Init` state of `mm2src/coins/utxo/bch_and_slp_tx_history.rs`). The listing of
> BCH and SLP tokens stays scoped by the token id alone; no row, no row content and
> no listing of another family changes, only the address table and its entries are
> added. Tests: `init_creates_the_address_table_and_every_store_writes_the_index`
> and `preparing_an_existing_bch_style_store_backfills_and_cleans_orphans` in the
> test module of the store, and
> `starting_the_fetcher_prepares_the_address_index_of_an_existing_store` in the
> BCH/SLP fetcher's tests. A failed preparation is logged as a warning and the BCH/SLP fetcher goes on (no new stop path for that family; the Tendermint fetcher keeps its `Error` state). The sentences above that say "Tendermint only for now"
> describe the state of PR #363. The SLP token rows rest on unit tests; no live SLP row is run,
> because the coins file the lab uses has no SLP token entry; the live store comparison covers BCH alone.
> Checked on 2026-11-16 on the head 79a4c484b of this change with the Komodo DeFi Framework 2.6.0-beta reference build
> as the other writer: for a BCH account with one transaction, fresh stores of both programs held the same row,
> identifier and two address-index entries, and each program listed the other's store (one row). The comparison of an
> SLP token store against the reference writer is a lab row, pending.

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

> **Status note (2026-11-16, Reloaded-original).** Decided 2026-11-15:
> (c). An `ed25519` public key in the public-key form is accepted, and its
> account is the bech32 of the first 20 bytes of the SHA-256 of the 32 key
> bytes (the public Cosmos SDK rule for that key type) under the coin's
> `account_prefix`; a `secp256k1` key keeps the rule of R36.14.16. The coin is
> watch-only and, like every coin of the form, cannot serve as an HTLC key
> (`get_htlc_key_pair` answers none, and its orders are refused, R18.11.23a).
> A key whose bytes are not a valid key of its type or have the wrong length
> is a request-parse failure (`InvalidRequest`, R36.1.3).

D36.18 *(maintainer decision.)* *What a watch-only coin's `withdraw`
answers in Reloaded.* The public-key form is built in 0.5.0 as a
watch-only coin, and Chapter 18 R18.11.23a already refuses its orders.
Options for `withdraw` and the staking requests: (a) the reference's
unsigned results of R36.14.17, in both forms, which involve no key and
are what lets a client's own wallet sign; (b) a typed refusal. Recommended:
(a). The external-signing channel of Chapter 18 R18.11.52 is not needed
for (a) and stays unbuilt while R18.11.23a stands.

> **Status note (2026-11-16, Reloaded-original).** Decided 2026-11-15: (a),
> built as stated in the as-built note below.

> **As built (Reloaded, 2026-11-16, 0.5.0; informative).** The public-key form
> builds a coin whose activation policy holds the key and the Ledger flag and
> no private key. Activation contacts no node, wallet or session for the key and
> the node's own login plays no part; a WalletConnect form is still refused with
> `PlatformCoinCreationError` until its activation is built. A malformed
> `activation_params` (any other shape, an unknown or missing `type`, a key type
> other than the two, a `value` that is not an array of bytes, a key that is not
> valid for its type) fails request parsing (`InvalidRequest`).
>
> - *`withdraw`, same chain.* Without the Ledger flag the result carries no
>   `tx_hex` and no `tx_hash` and a `sign_doc` member `{body_bytes,
>   auth_info_bytes, chain_id, account_number}` (arrays of byte values, the
>   coin's chain id, the account number the node reports; R36.14.29,
>   R36.14.30). The body holds the one bank send, the request's memo and the
>   timeout height of the node's chain height plus 100; the auth info carries the
>   watched public key, the direct sign mode, the node's sequence and the fee.
>   The fee is the fixed assumed one (Chapter 18 R18.11.9): nothing is
>   simulated. The `from` selector is ignored. With the Ledger flag the result
>   carries `legacy_amino_json` and `original_tx_type_url`
>   (`/cosmos.bank.v1beta1.MsgSend`) and no `sign_doc` (R36.14.31). The
>   document is a JSON object with the members `account_number`, `chain_id`,
>   `fee`, `memo`, `msgs` (one `cosmos-sdk/MsgSend`) and `sequence`, every number
>   a decimal string and no `timeout_height`; the fee amount of the platform
>   coin is three times the computed one, in both `fee_details` and the
>   document (R36.14.18 item 4).
> - *Staking requests* (delegation, undelegation, reward claim) answer the same
>   direct form. With the Ledger flag they fail with the request's
>   internal-error type, because the Amino form exists for the bank send only
>   (R36.14.18 item 3); the same build fails for any message other than the
>   bank send. This tree has no IBC `withdraw`, so the R36.14.17 IBC rows (the
>   unsigned result without the flag, the failure with it) are not buildable
>   here; that is not a gap of this change.
> - *A token* on a watch-only platform follows the platform: the same unsigned
>   results (a bank send in the token's denomination, the fee in the platform
>   denomination). Under the Ledger flag the factor three is the platform coin's
>   only: a token's document and `fee_details` carry the computed amount once
>   (R36.14.31 item 5).
> - *Orders* of the coin and of a token are refused as wallet-only with and
>   without the flag (R18.11.23a, D18.11.10 (a)); the reference accepts the
>   orders of a public-key coin without the flag, the recorded difference.
> - *Refused paths.* Every path that would sign with a node-held key is refused
>   with the typed error `PrivKeyNotAllowed::WatchOnlyCoin` ("The coin is
>   watch-only and holds no signing key"), before any node is contacted: the
>   taker-fee send, the maker and taker payment sends, the maker and taker
>   spends (claims), the maker and taker refund calls, the node's own signing
>   and broadcast of any transaction it built (the one every staking and swap
>   send ends in), message signing and private-key display (the answers carry
>   that error's text), and the public-key query (the existing typed
>   `UnexpectedDerivationMethod::IguanaPrivKeyUnavailable` answer; no new variant,
>   because that enum is matched exhaustively elsewhere). Allowed: the broadcast
>   of a transaction the caller signed elsewhere (`send_raw_transaction`) is not a
>   signing path and is unchanged; it is the watch-only user's path after the
>   unsigned document is signed.
> - *Tests:* T36.42 and T36.43 are covered by the unit tests in
>   `tendermint_swap_flow_tests/watch_only_tests.rs` (secp256k1 and
>   ed25519 accounts without node contact, the unsigned sign document with the
>   body decoded to one bank send, the token, the delegation, the Amino form
>   with the threefold fee, the failing staking and non-bank messages with the
>   flag, the order refusals, and each refused signing path), and the
>   request-parse and activation tests of the wire tests of
>   `tendermint_with_tokens_activation`; T36.55 -- T36.58 by the tests named
>   `t36_55_*` -- `t36_58_*` of the same file (see the status note at the end of
>   §36.14.8).

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

### 36.14.6 Precisions to §36.14.1

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-15,
> measure 0.05 (attestation in §36.13).**
> This subsection was written on 2026-11-15 by the KDF Spec Reader (Spec
> Reader round 0.5.0-6) under the two-team procedure of chapter 01 §1.4A,
> from the reference rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02
> §2.15A (read read-only; no untabled state was read), from the public
> interface definitions of the Cosmos SDK bank, staking and distribution
> modules, the ibc-go transfer module and the irismod HTLC module, and from
> this chapter's own text. **The two rows agree on everything in this
> subsection**: the part of the reference it describes is the same at both.
> It edits no sentence of §36.14.1 -- §36.14.5. Each rule names the rule it
> completes; where a rule here is more exact than the sentence it completes,
> the rule here governs. It serves the maintainer's decision that Reloaded's
> Tendermint history store is byte-compatible with the reference's (D36.11
> (a): the same row identifier and the same stored form), and settles four
> points only. Numbering: rules R36.14.19 -- R36.14.22, decisions D36.19 --
> D36.22 and tests T36.45 -- T36.48.

R36.14.19 **Which transfers are counted, and in which order (completes
R36.14.5 and R36.14.10).** The position that enters `internal_id` is the
place of a transfer in the **transfer list** of its transaction, counted
from zero. The list is built from the events of the execution result by
the seven items below; an implementation that follows them computes the
reference's identifier for every row.

1. *Repeated events.* Events are compared within their own type, in the
   order of emission. An event is dropped when it is identical to the
   event of the same type emitted just before it: the same attributes in
   the same order, each with the same key, value and index flag, in the
   form the node delivered them. Events of other types lying between two
   identical events do not keep them apart; a different event of the same
   type between them does. This is the exact sense of "repeated
   identically in direct succession" in R36.14.5, and it holds for every
   event type, module events included.
2. *Staking transactions (cases 1 to 3 of R36.14.5).* The one event read
   is the first emitted of its type after item 1. Its `amount` text is
   read coin by coin (the coins are separated by commas), in the order of
   the text, and each coin that meets item 4 is one candidate with the
   parties R36.14.5 names. An amount of two resolvable denominations
   therefore yields two transfers, not one.
3. *All other transactions (case 4).* The bank `transfer` events are read
   **from the last emitted to the first**, the reverse of their order in
   the result, once the fee payment has been left out under R36.14.5.
   Within one event the coins of the `amount` text are read in the order
   of the text. Each coin that meets item 4 is one candidate.
4. *Conditions for a candidate.* A coin of an event is a candidate only
   when the event has an `amount` attribute; the coin's denomination
   resolves under the closing paragraph of R36.14.5 (the platform `denom`
   compared exactly, otherwise the first coins-configuration entry whose
   `protocol_data` `denom` equals it without regard to letter case); its
   integer fits an unsigned 64-bit integer; and the event has the party
   attributes its type needs: `sender` and `recipient` for `transfer`,
   `delegator` and `validator` for `delegate` and `withdraw_rewards`,
   `delegator` for `unbond`. A coin that fails any condition is **not
   counted**: it takes no position and uses up no module event
   (R36.14.20).
5. *Correction, then merging.* A candidate of case 4 is first corrected
   from a module event when one is left (R36.14.20). Then, when the list
   already holds a transfer with the same sender and the same recipient
   (both as corrected) and the same denomination text, the candidate's
   amount is added to that transfer and the candidate takes no position
   of its own; the earlier transfer keeps its kind. Otherwise the
   candidate is appended to the list. Staking candidates are merged by
   the same test.
6. *What counts.* Every transfer in the list has a position, **whether or
   not a row is written for it**. Two kinds of list entry write no row
   and still count: an HTLC creation whose sender is not this account
   (R36.14.6), and a transfer whose identifier is already stored. A
   transfer between two other accounts is in the list, counts, and is
   stored as a row; it is only never served (R36.14.2). What does not
   count is exactly this: an event dropped under item 1, the fee payment
   left out by R36.14.5, a coin that fails item 4, a candidate merged
   under item 5, and a module event, which is never a transfer itself.
7. *The bytes.* With `p` the position and `H` the 64 characters of the
   upper-case hex hash, the 33 bytes of a transfer row's `internal_id`
   are, on a 64-bit native build, the character codes of the first 25
   characters of `H` in reverse order followed by `p` as an 8-byte
   big-endian integer; in the 32-bit browser build, the first 29
   characters of `H` in reverse order followed by `p` as a 4-byte
   big-endian integer. The fee row of R36.14.8 item 3 has `p` as a
   little-endian integer of 8 or 4 bytes followed by the same 25 or 29
   characters in forward order, `p` being the position of the token
   transfer that caused it. This is R36.14.10 read from the other end.
   The two builds give different identifiers for one transaction;
   Reloaded reproduces that.


The reverse reading order of item 3, the comparison of item 1 and the
merging of item 5 have no use of their own that the result shows; under
the byte-compatibility decision Reloaded reproduces all three as they
stand, because each one moves positions.

> **Code-quality finding (informative; trigger: apparent with little
> extra effort; D36.21).** *The position depends on the configuration.*
> A coin whose denomination does not resolve is not counted (item 4), so
> the positions of a transaction's other transfers change on the day that
> denomination becomes resolvable, which a coins-configuration update
> does. Every start reads the whole history again (finding 3 of
> §36.14.1). The transaction is then listed anew: a transfer that now has
> a position an older row already uses is never written, and a transfer
> whose position moved is written a second time under its new identifier,
> with the fee added again when it bears the fee. The history then shows
> one transfer twice and another not at all.

R36.14.20 **Which module event corrects which transfer (completes
R36.14.5, "One module event corrects one transfer", and finding 10 of
§36.14.1).** The pairing is by order. It is not by position in the
result and not by content; "position" in the heading of finding 10
means this order of reading. Each candidate of case 4, in the order of
R36.14.19 item 3 and before it is merged, takes the first module event
that no candidate has taken yet. The module events stand in this fixed
order: the `create_htlc` events; then the `claim_htlc` events; then, and
only when no HTLC event is left, the `non_fungible_token_packet` events,
the `ibc_transfer` events and the `fungible_token_packet` events; within
one type, from the last emitted to the first.

- *More candidates than module events.* The candidates read first are
  corrected; the others stay plain bank transfers.
- *More module events than candidates.* The events left over yield
  nothing: no transfer, no position, no row.
- *A module event without the attributes the correction needs* (`sender`
  and `receiver` for a creation and for the three IBC types, `sender` for
  a claim) is used up all the same and corrects nothing: the candidate
  that took it stays a plain transfer with the bank event's parties, and
  no later candidate gets that event.
- *Several coins in one `transfer` event* are separate candidates: with
  one module event and an amount of two resolvable coins, the first coin
  of the text is corrected and the second is a plain transfer.
- *HTLC and IBC events in one transaction.* Every HTLC event is taken
  before any IBC event.
- *Staking events are never paired.* A `delegate`, `unbond` or
  `withdraw_rewards` event decides the whole transaction (R36.14.5, cases
  1 to 3): no bank transfer and no other module event of that
  transaction is read, so a transaction that delegates and also sends
  yields the staking row only.


When a transaction has module events of one type only, as many as it has
candidates and emitted in the same order as their transfers, each
transfer gets its own event. Otherwise the event a transfer takes need
not be the one emitted for it, and the row carries the parties of the
event taken. Reloaded reproduces the pairing under the
byte-compatibility decision, because the corrected parties decide which
transfers merge and with that the positions; D36.22 puts the
alternative.

R36.14.21 **Staking events without a `delegator` attribute (completes
R36.14.4 and R36.14.5, cases 1 to 3).** Older versions of the Cosmos SDK
staking and distribution modules emit `delegate`, `unbond` and
`withdraw_rewards` events without a `delegator` attribute. For such an
event the reference writes **no row**. It reports no error and does not
stop the pass, and the history state is unchanged. It attributes the
event by nothing else: not by the `sender` of a `message` event, not by
the transaction's messages, not by the bank events of the same
transaction. Because the staking event has precedence (R36.14.5), the
bank `transfer` events of the transaction are not read either, so the
transaction is left out entirely under R36.14.4 and nothing is stored
for it; the balance changes and the history has no row for the change.

The same outcome, no row and no error, follows for a `delegate` or
`withdraw_rewards` event without a `validator` attribute, and for any of
the three events whose `amount` attribute is absent, is empty (a reward
claim that pays nothing), names no denomination after the integer, or
names only denominations that do not resolve. This holds for `unbond`
too, although the row's amount is zero whatever the text says. An
attribute whose key arrives base64-encoded is read as R36.14.5 states
and counts as present.

R36.14.22 **A fee of several coins or of another denomination, and an
empty fee list (completes R36.14.4, R36.14.5 case 4 and R36.14.8).**

1. *The entry read.* Only the **first** entry of the transaction's fee
   list is read, in the order in which the transaction encodes the list.
   Its integer is taken as base units of the **platform coin**, whatever
   its denomination. `fee_details` is therefore always `coin` the
   platform ticker, `amount` that integer scaled with the platform
   coin's decimals, and `gas_limit` the transaction's. The denomination
   of the entry is used for one thing only, the fee-payment text of
   R36.14.5.
2. *The other entries* are read nowhere: they are not in `fee_details`,
   enter no amount, and cause no fee row.
3. *An integer that does not fit an unsigned 64-bit integer* in the first
   entry leaves the transaction out entirely, without a row and without
   an error, like the cases of R36.14.4.
4. *Amounts.* The integer of item 1 is what R36.14.8 item 2 adds, in base
   units, to the stored `total_amount` and `spent_by_me`, and what the
   fee row of R36.14.8 item 3 holds in platform display units. The fee
   is attributed once, in the order of the transfer list (R36.14.19) and
   only among the entries for which a row is written: the first of them
   that this account sent takes it, into its own amounts when it is in
   the platform denom and as a fee row when it is in a token denom. No
   later entry of the transaction takes it.
5. *Recognizing the fee payment.* R36.14.5 leaves out a `transfer` event
   whose whole `amount` text equals the text of the first fee entry. The
   bank event of a fee of several coins lists all of them in one text,
   which does not equal it, so the fee payment is **not** left out: each
   of its coins that resolves becomes a transfer from the payer to the
   chain's fee-collector account, with a position of its own. Where the
   fee is deducted before the messages run, which is the Cosmos SDK's
   standard order, these transfers are read last and take the highest
   positions.
6. *An empty fee list.* The transaction is left out. **Nothing is
   stored**: no transfer row, no fee row, no
   row in the platform coin's or any token's history, and nothing is
   offered to the history stream. Every later pass that reads the
   transaction again does the same. A chain that accepts transactions
   without a fee therefore has no history for them at all.


Items 1, 2 and 5 misstate amounts whenever the first fee entry is not in
the platform denom: the platform coin's history shows a spend of that
integer in platform units that never took place, and the denomination
that did pay shows nothing (D36.19).

D36.19 *(maintainer decision.)* *A fee that is not one coin of the
platform denom.* Options: (a) the reference's values exactly (R36.14.22
items 1, 2 and 5); (b) Reloaded reads the fee entry in the platform denom
wherever it stands in the list and uses it for `fee_details` and for
R36.14.8 items 2 and 3; when the list has no such entry, the fee enters
no amount and causes no fee row, and `fee_details` names the ticker the
first entry's denomination resolves to, scaled with that coin's
decimals, or is `null` when it resolves to none; and the fee payment is
recognized by the text of the whole fee list, so that it is left out for
several coins as it is for one. Identifiers and the stored form are the
same under both options: where the fee payment is read last (R36.14.22
item 5), leaving it out moves no other position. Under (b) a store that
both programs have written can show different amounts for these
transactions, as under D36.12, and the reference adds its own rows for
the fee payment when it reads such a transaction. Recommended: (b).

> **Status note (2026-11-15, Reloaded-original).** Decided: (b). The fee
> payment is recognized by the text of the whole fee list (`fee_payment_text`),
> so it is left out for several coins as for one. The fee entry in the platform
> `denom` is read wherever it stands in the list and gives `fee_details` and the
> fee of R36.14.8 items 2 and 3. A list without a platform entry enters no
> amount and causes no fee row; `fee_details` then names the ticker the first
> entry's denomination resolves to, scaled with that coin's decimals, or is
> `null` when it resolves to none. A fee entry that is read (the first entry, and
> the platform entry) and does not fit an unsigned 64-bit integer leaves the
> transaction out. Amounts can differ from the reference's for these
> transactions, as under D36.12. Built in PR #363; tests T36.48 (i) -- (iii).

D36.20 *(maintainer decision.)* *Transactions the reference leaves out
for want of a fee or of a `delegator` attribute.* Options: (a) no row, as
at the reference (R36.14.21, R36.14.22 item 6); (b) Reloaded writes the
rows: for an empty fee list, the rows of R36.14.5 -- R36.14.7 with a fee
of zero and no fee row; for a staking event without `delegator`, the row
of R36.14.6 with the delegator taken from the `delegator_address` of the
transaction's staking message of the same kind (the public Cosmos SDK
message definitions), when the transaction has exactly one such message.
The identifier rule is unchanged. The reference never writes a row for
these transactions, so no identifier can collide and no row is doubled;
a store moved back to the reference keeps and serves the rows.
Recommended: (b).

> **Status note (2026-11-15, Reloaded-original).** Decided: (b). A
> transaction with an empty fee list gets the rows of R36.14.5 -- R36.14.7 with a
> fee of zero (`fee_details` the platform ticker, amount 0) and no fee row; an
> empty fee list has no fee payment to recognize. A `delegate`, `unbond` or
> `withdraw_rewards` event without `delegator` takes the `delegator_address` of
> the transaction's staking message of the same kind (`MsgDelegate`,
> `MsgUndelegate`, `MsgWithdrawDelegatorReward`) when the transaction has exactly
> one such message; with none or several it gives no row, as before. The
> identifier rule is unchanged. The reference writes no row for these
> transactions. Built in PR #363; tests T36.47 (i), T36.48 (iv).

D36.21 *(maintainer decision.)* *Positions when the resolvable
denominations change.* Options: (a) the reference as it stands (the
finding under R36.14.19); (b) the same counting, and Reloaded keeps
beside the store the set of denominations its lookup resolved when the
rows were written, and rebuilds the platform coin's history store from
the chain when a pass would resolve a different set; a rebuilt store
holds what the reference writes into an empty store under the same
configuration, so it stays byte-compatible; (c) counting over another
set of transfers, which gives identifiers the reference does not
compute. This bears on D36.13: under D36.13 (b) Reloaded would give rows
to transfers the reference's lookup does not count (a custom token) and
none to some it does count (a configuration entry of another platform),
and a transfer the reference does not count has no reference position.
Byte-compatible identifiers need the counting of R36.14.19 item 4 for
every transaction with transfers in more than one denomination, whatever
D36.13 decides about the ticker a row carries. Recommended: (b), with
the counting of R36.14.19 item 4.

> **Status note (2026-11-15, Reloaded-original).** Decided: (b), with the
> counting of R36.14.19 item 4. Beside the platform coin's store the fetcher
> keeps a record of what every denomination of its lookup resolved to when the
> rows were written (the ticker, the decimals and whether it is the platform
> coin or a token; `get_resolved_denoms`, `set_resolved_denoms` of the history
> storage, one row in `<TICKER>_tx_resolved_denoms`). At the start of a pass the
> lookup is compared with the record. When it differs, the store of the platform
> coin (its rows and those of its tokens) is cleared, the fetcher starts again at
> the first page and the history state is `InProgress`, so the store becomes what
> the reference writes into an empty store under the same configuration; the
> new record is written. A store with no record gets the record without a
> rebuild. Any change of the lookup, not only one that concerns this account,
> triggers one rebuild. Built in PR #363; tests for the record, the rebuild and
> the unchanged case.

D36.22 *(maintainer decision.)* *Pairing of module events.* Options: (a)
the reference's pairing by order (R36.14.20), with the wrong parties it
gives when transfers and module events are unequal in number; (b)
Reloaded pairs a module event with the transfer it belongs to (an IBC
packet event names its amount and denomination), accepting that in the
transactions where this changes which transfers merge, the positions and
with them the identifiers differ from the reference's. Recommended: (a);
the cases are batches of other parties' packets, and a row of this
account in them is still recoverable from the chain.

> **Status note (2026-11-15, Reloaded-original).** Decided: (a), the pairing
> by order of R36.14.20, as built (`module_event_queue`, `apply_module_event`);
> a row of this account in a batch of other parties' packets stays recoverable
> from the chain. Tests T36.46 (v) and (vi) are as built.

**Tests.** Rows T36.45 -- T36.48 run against a mocked node on a 64-bit
native build unless a row says otherwise, with the conventions of
§36.14.5: 6 decimals for the platform coin and for the token, a fee of
5,000 base units of the platform denom. Unless a row says otherwise, the
first event of each result is the fee payment's `transfer` event, whose
text equals the fee coin and which is left out. `A` is this account; `X`,
`Y`, `S` and `B` are others. "Position `n`" means an identifier that ends
in the 8-byte big-endian integer `n` (R36.14.19 item 7). Figures are
computed from the rules.

T36.45 *Positions (R36.14.19).* (i) Transfers `A` to `X` of 1,000,000 and
then `A` to `Y` of 2,000,000, both in the platform denom, emitted in that
order: the row to `Y` has position 0 and bears the fee (`total_amount`
2.005); the row to `X` has position 1 and `total_amount` 1. (ii) The same
with a transfer in an unresolvable denomination emitted between the two:
the same two identifiers. (iii) A transfer `X` to `Y` and then `X` to `A`:
the row to `A` has position 0; the row `X` to `Y` is stored with position
1 and is in no `my_tx_history` answer. (iv) `A` to `X` of 1,000,000, `A`
to `X` of 2,000,000, then `A` to `Y` of 3,000,000: two rows, to `Y` at
position 0 and to `X` at position 1 with amount 3. (v) Two identical
events `A` to `X` of 1,000,000 with only an event of another type between
them yield one transfer of 1; with a `transfer` event `A` to `Y` between
them they yield the row to `X` at position 0 with amount 2 and the row to
`Y` at position 1. (vi) One `transfer` event from `A` whose `amount` text
names 2,500,000 of the token and then 1,000,000 of the platform denom:
the token row has position 0; a `"FeeForTokenTx"` row carries the bytes
of the token row in the opposite order; the platform row has position 1
and `total_amount` 1, without the fee. (vii) A transfer `X` to `A` of
1,000,000, then a transfer from `X` to the HTLC module account, then a
`create_htlc` event with `sender` `X` and a `receiver`: one row is
stored, the one to `A`, with position 1, and no row with position 0.
(viii) Row (i) in the 32-bit browser build: the row to `X` has an
identifier of the first 29 characters of the hash in reverse order
followed by `00 00 00 01`.

T36.46 *Pairing (R36.14.20).* (i) Transfers `A` to the HTLC module
account of 1,000,000 and then `A` to `X` of 2,000,000, and one
`create_htlc` event with `sender` `A` and a `receiver`: the transfer of
2,000,000 is the `"SendHtlcAmount"` row at position 0 (`to` empty,
`total_amount` 2.005), and the transfer of 1,000,000 is a
`"StandardTransfer"` to the module account at position 1. (ii) One
transfer from `A`, one `create_htlc` event as in (i) and one
`claim_htlc` event: one `"SendHtlcAmount"` row; nothing for the claim,
whichever of the two events was emitted first. (iii) Two transfers from
`A`, one `create_htlc` and one `ibc_transfer` event, each with `sender`
and `receiver`: the transfer emitted last is the HTLC creation, the
other the IBC transfer. (iv) Two transfers from `A` to different
recipients and one `create_htlc` event without `receiver`: two
`"StandardTransfer"` rows with the bank events' parties. (v) A
resolvable transfer to `A`, then an unresolvable one, and one
`fungible_token_packet` event from `S` to `A`: one IBC row from `S` to
`A` at position 0. (vi) A resolvable transfer to `A`, its packet event
from `S` to `A`, an unresolvable transfer to `B` and its packet event
to `B`, emitted in that order: under D36.22 (a) the one row stored, at
position 0, has the parties of the packet event to `B`; under D36.22
(b) it is the IBC transfer from `S` to `A`. (vii) A result with a
`delegate` event of `A` and a bank transfer from `A` to `X`: one
`"StakingDelegation"` row at position 0 and no row for the transfer.

T36.47 *Staking events without a delegator (R36.14.21; D36.20).* (i) A
`delegate` event with `validator` and `amount` only, in a result that
also has a `message` event naming `A` as sender and the bank events of
the delegation: no row; the pass goes on to the next transaction and the
history state is as before. Under D36.20 (b): the delegation row of
T36.34. (ii) The same event with a `delegator` attribute whose key and
value are base64-encoded: the delegation row. (iii) An `unbond` event
without `delegator`: no row; with `delegator` and an `amount` that is an
integer without a denomination: no row; with both in full: the
`"RemoveDelegation"` row. (iv) A `withdraw_rewards` event without
`delegator`, one without `validator`, and one with an empty `amount`:
no row each. (v) A `withdraw_rewards` event of `A` whose `amount` names
two resolvable denominations: two `"ClaimDelegationRewards"` rows, at
positions 0 and 1 in the order of the text, both in the platform coin's
history.

T36.48 *Fee forms (R36.14.22; D36.19, D36.20).* Each transaction is a
bank send of 1,000,000 base units of the platform denom from `A` to `X`.
(i) A fee of one entry, 7,000 base units of the token denom, with the
fee payment's event text equal to it: one row with `fee_details` `coin`
the platform ticker and `amount` 0.007, `total_amount` and `spent_by_me`
1.007, and no row in the token's history. Under D36.19 (b):
`fee_details` `coin` the token ticker, `amount` 0.007, `total_amount`
and `spent_by_me` 1. (ii) A fee of two entries, 300 of the token denom
first and 5,000 of the platform denom second, with one fee-payment event
naming both: three rows, the send at position 0 with `total_amount`
1.0003, a `{"TokenTransfer": <token id>}` row from `A` to the
fee-collector account of 0.0003 at position 1 and a `"StandardTransfer"`
to the fee-collector account of 0.005 at position 2; `fee_details`
`amount` is 0.0003 in each, and there is no `"FeeForTokenTx"` row. Under
D36.19 (b): the one row at position 0 with `total_amount` 1.005 and
`fee_details` `amount` 0.005. (iii) A fee whose first entry is 2 to the
power 64: no row. (iv) An empty fee list: no row in any history and
nothing on the history stream, on the first pass and on a second one.
Under D36.20 (b): one row with `total_amount` 1 and `fee_details`
`amount` 0.

> **Status note (2026-11-15, Reloaded-original).** Built of R36.14.19 -- R36.14.22 in `rows_for_tx` and
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
> T36.48 are in `tx_history_tests.rs`, the reference's values where the
> decisions left them. Decisions D36.19 -- D36.22 and D36.13 are taken (the status notes
> at each); nothing of R36.14.19 -- R36.14.22 is provisional any more. The
> comparison of repeated events (item 1) is made on the attributes in the form
> the node delivered them, key, value and index flag in order (`HistoryEvent`
> keeps them beside the decoded pairs), so two events that differ only in the
> index flag, or in the encoding of a key, are both counted. T36.48 (iv) and
> T36.47 (i) run in their (b) forms; an empty fee list is checked on a first and
> a second pass by `transactions_without_a_row_do_not_stop_the_pass`.
>
> Observed form of D36.22 (a): a packet received through a relayer, the fee
> paid by a fee granter and a tip paid on to a payee, gives two rows for the
> receiving account. The tip transfer, read first, takes the packet event and
> becomes the IBC row (the packet's parties, the amount of the tip transfer),
> and the escrow transfer stays a plain row; test
> `a_relayed_ibc_receive_with_a_tip_gives_two_rows_by_order_pairing`.
>
> Observed 2026-11-15 on ATOM mainnet with a load-balanced public node, this
> build, 8 fresh first passes (one reference run alongside): the node's answers
> to the same two queries differed between runs minutes apart (spender total 2
> in 6 runs, 1 in 1, receiver total 3 in 5, 1 in 2, empty once); a transaction
> absent from both answers was not stored (3 of 8 runs for the received IBC
> transfer); in 1 run a listed height's block query returned a short answer
> without a block time and that transaction was not stored (R36.14.4), warned
> about since this change; listings equalled the stored sets and a later pass
> changed nothing. No cause beyond the node's answers is claimed.

### 36.14.7 The store's address index and the first pass

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-15,
> measure 0.01 (attestation in §36.13).**
> This subsection was written on 2026-11-15 by the KDF Spec Reader (Spec
> Reader round 0.5.0-7) under the two-team procedure of chapter 01 §1.4A,
> from the reference rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02
> §2.15A (read read-only; no untabled state was read), from the public
> interface definitions of the CometBFT RPC and of the Cosmos SDK bank
> module and block service, from [Chapter
> 38](38-utxo-coin-maintenance-and-features.md) §38.5A, from this
> repository's own tree and from this chapter's own text. **The two rows
> agree on everything in this subsection**: the parts of the reference it
> describes are the same at both. It edits no sentence of §36.14.1 --
> §36.14.6. Each rule names the rule it completes; where a rule here is more
> exact than the sentence it completes, the rule here governs. It serves the
> maintainer's decision that Reloaded's Tendermint history store is one that
> either program can continue (D36.11 (a)), and answers two questions only:
> what the address index of the history store is, and what the first pass of
> the fetcher reads. Each rule says in its heading whether it concerns the
> **shared store**, that is every coin family served from the indexed
> history store (Chapter 38 R38.5A.7), or **this family** only. Numbering:
> rules R36.14.23 -- R36.14.28, decisions D36.23 -- D36.25 and tests T36.49
> -- T36.54.

R36.14.23 **The tables of a coin's history store (shared store; completes
R36.14.1 and R36.14.10).** On a native build the indexed history store of
a coin is three tables in the node's SQLite database. `<T>` is the
store's ticker, for this family the platform ticker (R36.14.1), with
every `-` in it replaced by `_`.

| Table | Columns, as declared | Holds |
| --- | --- | --- |
| `<T>_tx_history` | `id INTEGER NOT NULL PRIMARY KEY`, `tx_hash VARCHAR(255) NOT NULL`, `internal_id VARCHAR(255) NOT NULL UNIQUE`, `block_height INTEGER NOT NULL`, `confirmation_status INTEGER NOT NULL`, `token_id VARCHAR(255) NOT NULL`, `details_json TEXT` | one row per history row |
| `<T>_tx_address` | `id INTEGER NOT NULL PRIMARY KEY`, `internal_id VARCHAR(255) NOT NULL`, `address TEXT NOT NULL` | the address index: which addresses a history row concerns |
| `<T>_tx_cache` | `tx_hash VARCHAR(255) NOT NULL UNIQUE`, `tx_hex TEXT NOT NULL` | one entry per transaction hash |


The stored forms are these.

- `internal_id`, in both tables: the identifier bytes as lower-case hex
  text, 66 characters for a row of this family (R36.14.10). The two tables
  are related by equality of this text and by nothing else.
- `tx_hash`: the row's `tx_hash` text as the row carries it.
- `block_height`: the row's height. `confirmation_status`: 0 when that
  height is zero and 1 otherwise; always 1 for this family.
- `token_id`: the token id of R36.14.2 as 64 lower-case hex characters
  when the row's `transaction_type` is `TokenTransfer`, or is
  `TendermintIBCTransfer` or `CustomTendermintMsg` with a `token_id` that
  is not null; the empty text for every other row, the `FeeForTokenTx`
  row and the three staking rows included.
- `details_json`: the row in its stored JSON form (R36.14.10).
- `tx_hex` of the cache table: the row's `tx_hex` bytes as lower-case
  hex. A hash that already has an entry keeps it.
- `id`, in both tables: assigned by the database. It carries no meaning
  and need not agree between two programs.


The address table has no uniqueness rule beyond its `id`: it accepts the
same pair of `internal_id` and `address` more than once, and nothing in
the schema ties an entry to a row. The table names, the declarations and
the stored forms above are the persisted contract that two programs
working on one database share; Reloaded creates and writes them exactly
so. The three tables are created, each only when absent, every time a
history fetcher starts for the coin (R36.14.12).

The reference also asks, with a create-when-absent statement, for an
index named `internal_id_idx` on the `internal_id` column, first of the
row table and then of the address table. An index name is unique within
a SQLite database, so the index exists on the first table for which it
was ever asked in that database and on no other. It is not part of the
contract: Reloaded MAY index either table under names of its own and
MUST NOT depend on an index of that name being present or being on a
particular table.

In the browser build there is no address table. Each stored row object
carries two members, `from_addresses` and `to_addresses`, which hold the
distinct strings of the row's `from` and of its `to`, and the selection
of R36.14.25 tests those members where the native build tests the index.

R36.14.24 **The entries of the address index (shared store, except item
2; completes R36.14.2 and R36.14.10).**

1. *Which entries.* A stored row has one entry for each distinct string
   in the union of its `from` and `to` arrays: `internal_id` is the
   row's, `address` is that string exactly as it stands in the row, with
   no change of letter case, prefix or encoding. A string that stands in
   both arrays yields one entry; a row whose arrays are both empty has no
   entry. The entries of a row can therefore be derived from its
   `details_json` alone.
2. *This family.* With the parties R36.14.6 gives a row:

   | Row | Entries |
   | --- | --- |
   | plain transfer, token transfer, IBC transfer | the sender and the recipient: this account and the other party, the other party being an address of the other chain for an IBC transfer; one entry when both are this account |
   | a transfer between two other accounts (R36.14.19 item 6) | those two accounts, and none for this account |
   | HTLC creation, HTLC claim (all three `msg_type` values) | this account only |
   | delegation | the delegator and the validator's operator address |
   | undelegation | this account only |
   | reward claim | the validator's operator address and the delegator |
   | fee row of a token transaction (R36.14.8 item 3) | this account only, under the fee row's own identifier |

   A token row lives in the platform coin's row table (R36.14.1), and its
   entries live in the platform coin's address table.
3. *When.* The entries of a row are written in the same database
   transaction as the row and its cache entry. For this
   family all rows of one fetched page (R36.14.3) are written in one
   transaction with their entries: the page is stored whole or not at
   all, and a page that cannot be stored ends the fetcher with the
   `Error` state (R36.14.11). An entry is never written without its row,
   and is not changed afterwards; a later change of a row's height, which
   other families make, leaves its entries as they are.
4. *Removal.* Entries are removed only together with a row and by its
   identifier: every entry with that `internal_id` and the row, in one
   transaction. The cache entry stays. Two things remove a row. For this
   family it is the clean-up of the earlier identifier forms (finding 2 of
   §36.14.1): for every transfer the fetcher reads, on every pass and
   before the identifier check of R36.14.10, a row stored under either of
   the two earlier 32-byte forms is removed with its entries; where no
   row is stored under such a form nothing is removed, entries under that
   form included. For the UTXO families it is a row of height zero whose
   transaction the node's listing no longer contains.
5. *No removal of a history.* The reference has no operation that removes
   a coin's history. Deactivating the coin, restarting, and activating it
   again with or without `tx_history` leave the three tables as they are,
   and no method empties or drops them.


R36.14.25 **Which rows the versioned `my_tx_history` serves (shared
store; completes R36.14.2 and R36.14.10, and Chapter 38 R38.5A.6 as to
what its address filter is).** A request stands for a set of addresses
and a token id. For this family the set is the one activated account
address in its bech32 text, whatever `target` says; the token id is the
64 lower-case hex characters of R36.14.2 for a token and the empty text
for the platform coin.

- *Selection.* A row of `<T>_tx_history` is selected when its `token_id`
  column equals the request's token id **and** `<T>_tx_address` holds at
  least one entry whose `internal_id` equals the row's and whose
  `address` equals an address of the request's set. Both comparisons are
  exact comparisons of text. A selected row counts once, however many
  entries match it. Nothing inside `details_json` takes part: not `from`,
  not `to`, not `coin`, not `transaction_type`.
- *Order.* By the columns, as Chapter 38 R38.5A.6 states: `confirmation_status`
  ascending, then `block_height` descending, then the `internal_id` text
  ascending.
- *`total`* is the number of selected rows. It counts rows, not
  transactions, so a token transaction with its fee row adds one to the
  token's total and one to the platform coin's.
- *Page number `p`.* The first `(p - 1) × limit` selected rows are left
  out and `skipped` is that number, also when it exceeds `total`.
- *`FromId`.* The anchor is looked up among the selected rows in their
  order. When it is the `k`-th of them, counted from one, the rows after
  it are served and `skipped` is `k`. When it is not among them, because
  no row has that identifier, or the row belongs to another token id, or
  the row has no entry for an address of the set, the answer is the one
  R36.14.10 states: an empty list, `skipped` 0 and the true `total`.
- *Content.* Each served row is its `details_json`, with the scaling of
  R36.14.7 for this family.


This is the exact sense of "only the rows whose `from` or `to` list
contains the activated account address are served" in R36.14.2: the test
is made on the index and on the `token_id` column, not on the row's
content. A program that writes rows without entries, or with a
`token_id` column that differs from the row's `transaction_type`, writes
rows that the other program lists differently or not at all.

R36.14.26 **Rows without entries, entries without rows, and a store
without the index (shared store unless an item says otherwise; completes
R36.14.12 and D36.11).**

1. *A row without an entry for the requesting address* is not served, is
   not counted in `total` and is no anchor for `FromId`. It still exists
   for everything that goes by identifier or by hash. For this family the
   fetcher therefore finds its identifier stored, and writes neither the
   row again nor any entry for it (R36.14.10): no later pass and no
   restart makes the row visible. The reference repairs nothing.
2. *Entries whose row is missing* select nothing, and nothing removes
   them (R36.14.24 item 4 removes through a stored row). Reloaded
   removes them when it prepares a coin's store.
3. *An entry stored twice* changes nothing: the row is served once and
   counted once.
4. *A store with the row table and the cache table but without the
   address table.* The reference takes such a store as prepared, because
   the test behind `StorageIsNotInitialized` (R36.14.12) looks for those
   two tables only. For a coin activated without `tx_history` every
   request of the versioned method then fails with `StorageError`
   (HTTP 500). For a coin activated with `tx_history: true` the fetcher
   creates the address table, empty, at its first step; from then on the
   method succeeds with an empty list and `total` 0 whatever the row
   table holds, and item 1 applies to every row in it.
5. *Reloaded.* Reloaded writes the entries of R36.14.24 with every row,
   removes them with the row, and selects by R36.14.25. When it prepares
   a coin's store it creates the address table when absent and adds, for
   every stored row that has no entry at all, the entries of R36.14.24
   item 1 derived from that row's `details_json`; a row that has at least
   one entry is left as it is, and a second preparation adds nothing.
   This step has no counterpart at the reference. It is what makes a
   store written without the index one that the reference lists in full.


> **Our side (informative; this repository's tree, 2026-11-15).** The
> indexed history store of this repository is the shared one: it has the
> row table and the cache table, named from the ticker as it stands,
> without the replacement of `-`; it has no address table; it fills the
> `token_id` column for `TokenTransfer` only; and its listing selects on
> the token id alone and counts every row of that id. It serves BCH and
> SLP tokens today (Chapter 38, the note under R38.5A.7). The index
> therefore belongs in the shared store, for every family that store
> serves or comes to serve, and not in a layer of this family.

> **Code-quality finding (informative; trigger: apparent with little
> extra effort; D36.23).** *One store for every account under a ticker.*
> The tables are named from the ticker alone, so every account that is
> activated under that ticker with the same database writes to and reads
> from the same three tables; the index is what keeps their rows apart.
> A row is written once, with the amounts, the `transaction_type` and,
> for the HTLC and undelegation rows, the `from` of the account that was
> active when it was written (R36.14.6, R36.14.7), and R36.14.24 indexes
> it under both parties. A served row carries those values whichever
> account requests it; the remedy is D36.23.

D36.23 *(maintainer decision.)* *Rows of another account in the same
store.* Options: (a) the reference as it stands (the finding above);
(b) the same tables and rows, and Reloaded keeps beside the store the
account address for which it was filled and rebuilds the platform coin's
history store from the chain when the coin is activated for a different
account; a rebuilt store holds what the reference writes into an empty
store for that account, so it stays one that either program can
continue, and the earlier account's rows are re-creatable in the same
way. Option (b) does not detect a store that the reference filled for
two accounts. Recommended: (b).

> **Status note (2026-11-15, Reloaded-original).** Decided: (a), the reference as it
> stands. Reloaded keeps no account record beside the store and does not rebuild it
> when the coin is activated for a different account (the record of the resolved
> denominations, D36.21, stays). A row carries the values of the account that wrote
> it and is listed under each of its own addresses; the second account's rows are
> written beside it, once each. Built in PR #363; test T36.54
> (`a_second_account_on_the_same_store_is_listed_with_the_rows_of_the_first`).

R36.14.27 **The first pass (this family; completes R36.14.3, R36.14.4
and R36.14.11).**

1. *Sequence.* The fetcher prepares the store (R36.14.23); walks the
   query `coin_spent.spender = '<address>'` from page 1 to its end; walks
   the query `coin_received.receiver = '<address>'` from page 1 to its
   end; and only then sets the state `Finished`. It runs no other query:
   none by a `transfer` or `message` attribute, and none bounded by
   height.
2. *Paging and the end of a query.* A request names a page `n`, 50
   results per page, ascending order. The transactions of the answer are
   worked through in the node's order, with one block read per
   transaction that passed the result-code test. The rows of the page are
   then written together (R36.14.24 item 3) and are served by
   `my_tx_history` from that moment, while the state is still
   `NotStarted`. The query ends when `n × 50` is at least the
   `total_count` of that same answer; otherwise page `n + 1` is
   requested. `total_count` is read from every answer anew. The query
   keeps a page number for the next pass: `n + 1` when its last page held
   50 transactions, and `n` otherwise.
3. *Which node.* The pages of one query in one pass come from one node,
   chosen when the query starts: the first of the configured nodes, in
   their current order, that answers a CometBFT `health` request. Every
   block read and every balance read chooses its node in the same way at
   the moment it is made. The requests of one pass need not all reach
   the same node.
4. *A transaction in which this account is only the recipient.* The bank
   module emits a `coin_received` event with the `receiver` for every
   credit, whoever signed the transaction and whoever paid for it. Such a
   transaction is therefore returned by the second query, and by that
   query only. It is processed exactly as a transaction this account
   sent: no rule of the fetch depends on which of the two queries
   returned a transaction or on which pass reads it. A transfer that a
   relayer delivered yields the IBC row of R36.14.6, from the packet
   event's `sender` to this account, with entries for both addresses,
   nothing spent, and the relayer's fee in `fee_details` only (R36.14.8
   item 4).
5. *What `my_tx_history` lists for an account that has only received
   funds.* Until the store is prepared, `StorageIsNotInitialized`
   (R36.14.12). Then success with `NotStarted` and the rows of the pages
   written so far. At `Finished`: the first query was answered with no
   transaction and a `total_count` of 0, which ends it at page 1; the
   list holds one row for every received transfer that R36.14.4 --
   R36.14.6 and R36.14.19 -- R36.14.22 yield, and `total` is their
   number.
6. *When a received transaction that was on the chain before the pass
   began is nevertheless absent after the first `Finished`.* The cases
   are these:
   - (a) *the node did not return it.* The answer to `tx_search` is
     taken as it comes and is compared with no other source. The
     transaction is written by a later pass that reads its page again
     (R36.14.28 item 2).
   - (b) *the block read carried no time.* The block of the
     transaction's height is read through the node's `abci_query` method
     from the Cosmos SDK `GetBlockByHeight` service. An answer without a
     block or without a header time leaves the transaction out
     (R36.14.4): no error, no pause, no retry, and the pass goes on to
     `Finished`. The result code of the answer is not inspected; the
     remedy is D36.24.
   - (c) *no pass writes it*: one of the cases of R36.14.4, R36.14.21 and
     R36.14.22 items 3 and 6 applies, or its denomination does not
     resolve (R36.14.19 item 4).
   - (d) *it is stored and not served*: the pairing of R36.14.20 gave its
     transfer the parties of another module event of the same
     transaction, so the row has no entry for this account; or its
     identifier was already stored without such an entry (R36.14.26
     item 1). No pass changes either.

   Cases (a) and (b) depend on the node and can differ from one pass to
   the next; cases (c) and (d) are the same in every pass.


R36.14.28 **What reads a transaction after the first pass (this family;
completes R36.14.3).**

1. *Only a pass reads transactions, and three things start one.* (i) A
   balance read, made every 30 seconds, that differs from the previous
   one. The balances compared are those of the platform coin and of the
   tokens activated at the moment of the read, taken as a set, so a
   token activated on the platform since the previous read counts as a
   change even when its balance is zero. (ii) The retry 30 seconds after
   a node failure, a failed balance read included. (iii) A start of the
   coin. No timer, no new block and no `my_tx_history` request starts a
   pass.
2. *What a pass started by (i) or (ii) reads.* Each query from the page
   number it kept (R36.14.27 item 2), the first query and then the
   second, each to its end. Earlier pages are not requested again while
   the coin stays active. A transaction left out under R36.14.27 item 6
   (a) or (b) is therefore read again at the next balance change when it
   stands on the kept page, and otherwise only at the next start of the
   coin, which begins both queries at page 1: the page numbers are not
   persisted (finding 3 of §36.14.1).
3. *A transaction received after the first pass* changes a compared
   balance when it is in the platform denom or in the denom of an
   activated token; the next balance read then starts a pass, which
   writes it. A credit in any other denomination changes no compared
   balance and is read only when something else starts a pass. The same
   holds for credits and debits that cancel out between two balance
   reads.
4. *A kept page past the end.* When a query ended on a page of 50
   transactions its kept page is one that does not exist until that
   query has a further transaction. The CometBFT RPC answers a page
   number beyond the last page of a result with an error. The reference
   handles that answer as it handles a node failure (R36.14.3). The
   remedy is D36.25.


> **Code-quality findings (informative; trigger for both: apparent with
> little extra effort).** *A transaction skipped for want of a block
> time* (R36.14.27 item 6 (b)) leaves no trace, and the pass that
> skipped it reports `Finished`; it is read again only under R36.14.28
> item 2 (D36.24). *A kept page past the end* (R36.14.28 item 4) is
> handled as a node failure where no node has failed; the remedy is
> D36.25.

D36.24 *(maintainer decision.)* *A block answer without a time.*
Options: (a) the reference as it stands: the transaction is left out of
that pass; (b) Reloaded asks the other configured nodes for the block,
and when none has it, writes no row and keeps for that query the page
of the earliest transaction left out for this reason, so that every
later pass reads from there again. The rows written are the same under
both options, so the store stays one that either program can continue.
Recommended: (b).

> **Status note (2026-11-15, Reloaded-original).** Decided (b). When the block answer
> for a transaction's height carries no block or no time, every other configured
> node is asked once (`block_time_from_any_node`); a node that fails counts as not
> having the block. When none has it, no row is written and the query's page cursor
> is set to the earliest page of the pass that held such a transaction, so every
> later pass reads from there again (`pass`, `read_page`). The warning of the left
> out transaction stays. Built in PR #363; tests T36.53 (iii),
> `a_block_that_the_first_node_lacks_is_asked_of_the_other_nodes` and
> `a_block_that_no_node_has_keeps_the_earliest_such_page_for_the_next_pass`.

D36.25 *(maintainer decision.)* *A query that ended on a full page.*
Options: (a) the reference as it stands (R36.14.28 item 4); (b) Reloaded
never asks for a page past the last one it has read: a query that ended
on a full page starts the next pass on that page again, where the
identifier check of R36.14.10 passes over the rows already stored, and
goes on to a further page only when the answer's `total_count` shows
one; the second query runs in every pass. The rows written are the same
under both options. Recommended: (b).

> **Status note (2026-11-15, Reloaded-original).** Decided (b): a query that
> ended on a full page starts the next pass on that page again, and the identifier
> check of R36.14.10 passes over the rows already stored. As built: the kept page
> advances only when the answer's `total_count` shows more, both queries run in
> every pass, and no request is made past the last page. Test T36.53 (iv),
> `a_query_that_ended_on_a_full_page_is_read_again_from_that_page_without_asking_past_the_end`.

**Tests.** Rows T36.49 -- T36.54 run against a mocked node on a 64-bit
native build, with the conventions of §36.14.5: 6 decimals for the
platform coin and for the token, a fee of 5,000 base units of the
platform denom. `A` is this account; `X`, `Y` and `B` are other accounts
of the chain, `S` an address of another chain, `V` a validator's
operator address. "The contract" means the tables, forms and selection
of R36.14.23 -- R36.14.25 applied directly to the database by the test,
without either program. Figures are computed from the rules.

T36.49 *Index entries (R36.14.23, R36.14.24).* After one pass: (i) a
send from `A` to `X` has two entries, `A` and `X`, under the row's
identifier in lower-case hex; (ii) a send from `A` to `A` has one;
(iii) in a transaction with a transfer from `X` to `Y` and one from `X`
to `A`, the row from `X` to `Y` has the entries `X` and `Y` and none for
`A`; (iv) a token send from `A` to `X` has the entries `A` and `X` for
the token row, whose `token_id` column is the 64 hex characters of the
token id, and one entry `A` for the fee row, under the fee row's own
identifier and with an empty `token_id` column; (v) an HTLC creation by
`A` and a claim by another account that pays `A` have one entry each,
`A`; (vi) a delegation has `A` and `V`, an undelegation `A`, a reward
claim `V` and `A`; (vii) an incoming IBC transfer has `S` and `A`. For a
platform ticker that contains `-`, the three tables carry `_` in its
place. (viii) A store that fails while a page is being written holds no
row and no entry of that page afterwards, and the state is `Error`.

T36.50 *Selection, `total` and paging (R36.14.25, R36.14.26).* A store
is prepared by the contract with six rows: `r1` and `r2` at height 12,
platform coin, `r1` with entries `A` and `X` and `r2` with entry `A`,
the identifier of `r1` sorting before that of `r2`; `r3` at height 10,
platform coin, entries `X` and `Y`; `r4` at height 11, platform coin,
no entry; `r5` at height 11, token, entries `A` and `X`; `r6` at height
9, platform coin, the entry `A` stored twice. For the platform coin and
`A`: `total` is 3 and the order is `r1`, `r2`, `r6`; with `limit` 2,
page 2 holds `r6` with `skipped` 2 and page 3 is empty with `skipped`
4; `FromId` of `r1` serves `r2` and `r6` with `skipped` 1; `FromId` of
`r3`, of `r4`, of `r5` and of an unknown identifier each give an empty
list, `skipped` 0 and `total` 3. For the token: `r5`, `total` 1. After
the entry of `r4` for `A` is added, `total` is 4 and `r4` stands between
`r2` and `r6`. An entry for `A` under an identifier that no row has
changes no answer.

T36.51 *A store written by one program, continued and served by the
other (R36.14.23 -- R36.14.26; D36.11 (a)).* (i) *Served.* A store
prepared by the contract with the rows and entries of T36.30, T36.31 and
T36.33 is served by Reloaded, for a coin activated without
`tx_history`, with those rows, the totals of R36.14.25 and
`sync_status` `NotEnabled`. (ii) *Continued.* The same store with
`tx_history: true` and a node that returns the same transactions and
one more: after the pass the store holds the earlier rows and entries
unchanged, each once, and the new row with its entries. (iii) *Written.*
After Reloaded's pass over those transactions into an empty database,
the contract applied to that database gives the same rows, order and
`total` as Reloaded's own answers, for the platform coin and for the
token, and the address table holds exactly the entries of T36.49. (iv)
*A store without the index.* A database with the row table and the
cache table only, holding those rows: after Reloaded prepares the store
the address table exists with the entries of T36.49 and the answers are
those of (i); a second preparation adds no entry. (v) *An earlier
identifier form.* A row stored under a 32-byte identifier of finding 2
of §36.14.1, with its entries, and a node that returns its transaction:
after the pass that row and its entries are gone and the row of
R36.14.10 is stored with its entries. (vi) *Lab row against reference
release binaries (permitted-input class R6, black box; extends
T36.44).* One database filled by Reloaded and then opened by a
reference binary, and one filled by a reference binary and then opened
by Reloaded: each program serves the other's rows with the same
`total`, the same pages by page number and the same pages by `FromId`,
for the platform coin and for a token, and a pass of the second program
adds no row.

T36.52 *First pass, an account with one received transfer only
(R36.14.27).* The node answers the first query with no transaction and
a `total_count` of 0, and the second with one transaction and a
`total_count` of 1. (i) The transaction is a relayer's: its events are
the relayer's fee payment, a `transfer` of 1,000,000 base units of the
platform denom from a module account to `A`, and a
`fungible_token_packet` event from `S` to `A`. The fetcher's history
requests are, in this order, one for page 1 of the first query, one for
page 1 of the second and one block read, with no request for a second
page; the state is then `Finished`. The platform coin's history has `total` 1:
`{"TendermintIBCTransfer": {"token_id": null}}`, `from` `S`, `to` `A`,
`total_amount` 1, `spent_by_me` 0, `received_by_me` 1,
`my_balance_change` 1, the relayer's fee in `fee_details`; the address
table has the entries `S` and `A`. A request made before that page is
written answers an empty list with `NotStarted`. (ii) The transaction is
a bank send from `X` to `A`: one `"StandardTransfer"` row. (iii) A
second query with 120 transactions is read in three requests, pages 1 to
3, and keeps page 3; with 100 transactions it is read in two requests
and keeps page 3.

T36.53 *What a later pass reads (R36.14.28; D36.24, D36.25).* (i) After
T36.52 (i), with unchanged balances over several balance reads, no
`tx_search` request is sent. After a balance change one request is sent
for page 1 of each query, the first query first; a further received
transaction on that page is written and the earlier row is stored
once. (ii) A token activated on the platform after the first pass, with
a balance of zero, starts one pass. (iii) The block read for the
received transaction's height answers without a time during the first
pass and with a time afterwards: under D36.24 (a) the state is
`Finished` with an empty list, and the row is present after the pass
that the next balance change starts; under (b), with a second
configured node that has the block, the row is present at the first
`Finished`. (iv) *Input:* a first query of exactly 50 transactions; a
node that answers a request for page 2 of that query with an error;
after the first pass, one further transaction in the second query's
answer and a changed balance. *Expected:* under D36.25 (a), requests
for page 2 of the first query 30 seconds apart and no request of the
second query; under (b), one request for page 1 of the first query
that adds no row, then the second query, and the row of the further
transaction.

T36.54 *Two accounts under one ticker (D36.23).* *Input:* a database
whose store was filled for `A` with one send of 1,000,000 base units of
the platform denom from `A` to `B`; the coin activated for `B` on that
database. *Expected in `B`'s history:* under D36.23 (a), one row with
`spent_by_me` 1.005 and `received_by_me` 0; under (b), one row with
`spent_by_me` 0, `received_by_me` 1 and `my_balance_change` 1.

> **Status note (2026-11-15, Reloaded-original).** Built of R36.14.23 --
> R36.14.26: the table names with `-` replaced by `_`, the declarations of the
> address table, the entries per row, their removal with the row, the page stored
> in one transaction, the selection through the index and the preparation step of
> R36.14.26 item 5 (including the removal of entries without a row), in
> `mm2src/coins/sql_tx_history_storage.rs` and `store` of
> `mm2src/coins/tendermint/tendermint_tx_history.rs`. Tests T36.49 (i) -- (viii),
> T36.50, T36.51 (iv) and (v) and T36.52 are in
> `mm2src/coins/tendermint/tendermint_swap_flow_tests/tx_history_tests.rs` and in
> the test module of the store. T36.51 (i) -- (iii) and (vi) (the store written by
> the other program as the contract, and the lab row) are lab rows and are not
> built as unit tests. R36.14.27 and R36.14.28 are met by the fetcher as it stands
> and by the decisions D36.23 -- D36.25 as noted at each.

### 36.14.8 Precisions: the unsigned transaction of a watch-only coin, and a history request while no node answers

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-16,
> measure 0.03 (attestation in §36.13).**
> This subsection was written on 2026-11-16 by the KDF Spec Reader (Spec
> Reader round 0.5.0-8) under the two-team procedure of chapter 01 §1.4A,
> from the reference rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02
> §2.15A (read read-only; no untabled state was read), from the public
> transaction and signing definitions of the Cosmos SDK (the
> `cosmos.tx.v1beta1` messages, the direct sign mode, the legacy Amino JSON
> sign mode and its sign document, the bank module's send message), from the
> public CometBFT RPC, from [Chapter 18](18-tendermint-ibc-htlc.md),
> [Chapter 38](38-utxo-coin-maintenance-and-features.md) §38.5A and [Chapter
> 55](55-coin-activation-wire-contract.md) R55.1.10, from this repository's
> own tree and from this chapter's own text. **The two rows agree on
> everything in this subsection**: the parts of the reference it describes
> are the same at both. It edits no sentence of §36.14.1 -- §36.14.7. Each
> rule names the rule it completes; where a rule here is more exact than the
> sentence it completes, the rule here governs. It answers two questions
> only: what exactly the unsigned transaction is that `withdraw` answers for
> a coin activated in the public-key form (§36.14.3), as a contract for the
> external signer that consumes it; and what a history request answers while
> no configured node answers. Each rule says in its heading whether it
> concerns **this family** only or a **shared serving path**, that is every
> coin family the method serves. Numbering: rules R36.14.29 -- R36.14.35 and
> tests T36.55 -- T36.60; the subsection raises no decision.

R36.14.29 **The `withdraw` result that carries an unsigned transaction
(this family; completes R36.14.17).** For a coin or token activated in
the public-key form, `withdraw` (both the legacy and the mmrpc-2.0 form,
which answer the same payload) answers the ordinary result object with
the members below. The unsigned transaction is not wrapped: its members
stand directly among the other members of the result.

| Member | In the unsigned result |
| --- | --- |
| `tx_hex`, `tx_hash` | **absent** (not null, not empty) |
| `sign_doc` | present when the Ledger flag is off (R36.14.30); absent otherwise |
| `legacy_amino_json`, `original_tx_type_url` | present when the Ledger flag is on (R36.14.31); absent otherwise |
| `from` | one entry, the activated address |
| `to` | one entry, the request's `to` as sent |
| `total_amount`, `spent_by_me`, `received_by_me`, `my_balance_change` | as for a signed `withdraw` of the same request, computed with the fee of the unsigned transaction |
| `fee_details` | `{"type": "Tendermint", "coin": <platform ticker>, "amount": <fee in display units>, "gas_limit": <gas limit>}`, the same fee and gas limit that the unsigned transaction carries |
| `coin` | the ticker the request named |
| `block_height`, `timestamp` | 0 |
| `internal_id` | 32 bytes as hex text: for the platform coin the SHA-256 digest of the empty byte string, for a token the SHA-256 digest of the 32 bytes of its token id (R36.14.2) |
| `transaction_type` | as for a signed `withdraw` (`"StandardTransfer"`, `{"TokenTransfer": <token id>}`, or the IBC form) |
| `memo` | a string: the request's `memo`, or the empty string when the request has none |
| `kmd_rewards` | absent |

The `internal_id` of an unsigned result is therefore the same for every
unsigned result of a coin. It identifies no transaction, which has no
hash before it is signed, and a client must not use it as a key.
Reloaded answers the same members with the same values (D36.18 (a)).
The node keeps nothing about an unsigned transaction after it has
answered: no later request refers back to it.

R36.14.30 **The `sign_doc` form (this family; completes R36.14.17 and
T36.42).** `sign_doc` is a JSON object with exactly four members. Their
names and meaning are those of the public Cosmos SDK message
`cosmos.tx.v1beta1.SignDoc`; only the JSON encoding is the reference's
own.

| Member | JSON type and encoding | Content |
| --- | --- | --- |
| `body_bytes` | array of integers from 0 to 255, one per byte, in order; not a hex or base64 string | the protobuf serialization of the transaction body (`TxBody`) |
| `auth_info_bytes` | array of integers, as above | the protobuf serialization of the `AuthInfo` |
| `chain_id` | string | the coin's `chain_id` (R36.3.1) |
| `account_number` | JSON **number** (an unsigned 64-bit integer), not a string | the account number the node reports for the activated address |

The order of the four members in the object carries no meaning. What the
two byte strings hold is fixed by the public definitions once the
following values are given, and Reloaded produces byte strings that
decode to the same values.

1. *Body.* Exactly one message, the message of the operation (for a
   same-chain `withdraw` the bank module's `/cosmos.bank.v1beta1.MsgSend`
   from the activated address to the `to` address with one coin: the
   denomination of the coin or token and the amount in base units); the
   `memo` of R36.14.29; a `timeout_height` of the chain height the node
   read while it answered the request, plus 100. No other member of the
   body is set.
2. *Authorization.* Exactly one signer entry: the public key the coin
   was activated with, as the public Cosmos SDK key message of the key
   type the activation declared (D36.17 concerns the `ed25519` case); the
   single sign mode `SIGN_MODE_DIRECT`; and as `sequence` the sequence
   the node reports for the account while it answers the request. The
   fee has exactly one coin, in the platform denomination also for a
   token; for a same-chain `withdraw` its amount is that of Chapter 18
   R18.11.9 and R18.6B.2. The gas limit is that of Chapter 18 R18.6B.3;
   payer and granter are empty.
3. *What the signer does* is the public convention of the direct sign
   mode and is not restated here: it signs the protobuf serialization of
   the `SignDoc` that these four values form and puts the two byte
   strings, unchanged, with its signature into a `TxRaw`.

Two consequences follow from the values alone. The document can be
accepted by the chain only up to the height in its body, about 100
blocks after the request. And it is bound to one sequence: two requests
answered before either transaction is broadcast carry the same
sequence, and the chain accepts only one of the two transactions.

R36.14.31 **The `legacy_amino_json` form (this family; completes
R36.14.17, R36.14.18 and T36.43).** With the Ledger flag the result
carries two members in place of `sign_doc`.

1. *`legacy_amino_json`* is a JSON **object**, not a string that holds a
   serialized document. It is the sign document of the public Cosmos
   SDK legacy Amino JSON sign mode (`StdSignDoc`) and departs from that
   definition in no member it carries.
2. *`original_tx_type_url`* is a string, the protobuf type URL of the
   message that the document's one `msgs` entry stands for. Because the
   Amino form exists for the bank send only (R36.14.18 item 3), it is
   always `/cosmos.bank.v1beta1.MsgSend`. The Amino document does not
   carry this URL; the member supplies it for the protobuf message of
   the transaction that is broadcast.
3. *The members of the document.*

   | Member | JSON type | Value |
   | --- | --- | --- |
   | `account_number` | string, decimal digits | the account number the node reports; present also when it is zero, as `"0"` |
   | `chain_id` | string | the coin's `chain_id` |
   | `fee` | object with exactly `amount` and `gas` | `amount`: an array with exactly one coin object `{"denom": <platform denom>, "amount": <base units as a decimal string>}`, present with that one entry also when the amount is zero; `gas`: the gas limit as a decimal string |
   | `memo` | string | the `memo` of R36.14.29; present also when empty, as `""` |
   | `msgs` | array with exactly one entry | `{"type": "cosmos-sdk/MsgSend", "value": {"from_address": <activated address>, "to_address": <destination address>, "amount": [{"denom": <denom of the coin or token>, "amount": <base units as a decimal string>}]}}` |
   | `sequence` | string, decimal digits | the sequence the node reports for the account; present also when it is zero, as `"0"` |

   Every number in the document is a JSON string. The document has no
   other member: no `timeout_height`, and no `payer` or `granter` in
   `fee`.
4. *Timeout height.* The public definition leaves `timeout_height` out
   of the document when the transaction has none, and the reference
   never writes it. The transaction a signer builds from this document
   therefore has **no** timeout height, unlike the transaction of the
   `sign_doc` form (R36.14.30 item 1). This is a difference between the
   two forms and not a departure from the public definition.
5. *Fee.* The fee in the document is the fee of the result
   (R36.14.29). For the **platform coin** it is three times the computed
   amount (R36.14.18 item 4), multiplied as an integer of base units
   after the rounding of Chapter 18 R18.6B.2. For a **token** on that
   platform the factor is not applied: the document and `fee_details`
   carry the computed amount once. This is the exact scope of R36.14.18
   item 4. The `gas` member is the gas limit of Chapter 18 R18.6B.3 in
   both cases and is not multiplied.
6. *Order and canonical form.* The order in which the reference writes
   the members of the document and of its inner objects is not the
   canonical order of the sign mode at every level, and it is not part
   of the contract: Reloaded MAY write the members in any order. The
   bytes that are signed are not the bytes of the answer. They are the
   canonical JSON of the public sign mode (the members of every object
   sorted by name, no insignificant whitespace), which the signer
   produces itself from the document.
7. *The transaction the signer builds* is fixed by the public
   convention and by items 2 to 4: a body with the protobuf form of the
   one message, the same memo and no timeout height; one signer entry
   with the account's public key, the sign mode
   `SIGN_MODE_LEGACY_AMINO_JSON` and the document's sequence; and the
   document's fee and gas limit.

R36.14.32 **Having the signed transaction broadcast (this family;
completes R36.14.17).** The reference has no request that takes a
signature, or a signed document, and completes the transaction: no
member of an unsigned result is ever sent back to the node. The caller
has two ways, and both begin with its own wallet assembling the signed
transaction (`TxRaw`) by the public convention.

1. The wallet broadcasts the transaction itself, to any node of the
   chain. The node that answered `withdraw` takes no further part and
   learns of the transaction from the chain (the history fetcher,
   R36.14.28).
2. The caller sends the legacy method `send_raw_transaction` with
   `coin` (the platform coin or a token on it) and `tx_hex`, the
   protobuf serialization of the `TxRaw` as hex text. The node needs no
   key for this. It checks only that the bytes decode as a `TxRaw`; it
   does not compare them with any document it answered earlier. It
   submits them with the CometBFT `broadcast_tx_commit` method, so the
   answer waits for the transaction's block, and answers `{"tx_hash":
   <the node's hash, upper-case hex>}`. A transaction that the node's
   check or the block's execution refuses yields the legacy error
   answer. The `tx_json` member of that method does not serve this
   family.

The exchange of Chapter 18 R18.11.52 (a data-needed event answered by
`send_asked_data` with a transaction hash) belongs to swap transactions
only; a `withdraw` never starts it.

R36.14.33 **A versioned history request while the current height cannot
be read (shared serving path; completes R36.14.9 and R36.14.12).** The
mmrpc-2.0 `my_tx_history` reads the chain's current height from a node
on **every** request, before it reads any row, and keeps no earlier
height to fall back on. When that read fails, the request fails with the
error type `RpcError`, string data (the text is not bound) and HTTP 500.
No stored row is served: the answer is an error answer and has no
`transactions`, `current_block` or `sync_status`, so the question of a
`confirmations` value does not arise. The store is not changed by the
failure, and the first request whose height read succeeds serves the
stored rows again, with `confirmations` by R36.14.9 from the height then
read.

The conditions are tested in this order, and the first that holds
decides the answer.

1. The coin is not active: `CoinIsNotActive` (HTTP 404).
2. This node's database has no history store for the store's ticker:
   `StorageIsNotInitialized` (HTTP 500). The test contacts no node, so
   the first row of the table of R36.14.12 answers this error whether or
   not a node answers.
3. The height read fails: `RpcError` (HTTP 500). The second and third
   rows of that table answer this error while no node answers.
4. Everything else, namely a `target` that a family refuses and a store
   that cannot be read (`StorageError`), is tested only after the height
   was read.

This is a property of the serving path, not of this family: the method
is one and the same for every family that Chapter 38 R38.5A.7 lists as
served from the history store, in the native and in the browser build,
and only the height read itself belongs to the family. For this family
the height read is the following, for a token through its platform coin.
A node is chosen as in R36.14.27 item 3: the configured nodes are asked
a CometBFT `health` request one after the other, in their current
order. The chosen node is then asked the CometBFT `abci_info` request,
and the height is the `last_block_height` of its answer. The read fails
when no configured node answers `health`, and when the chosen node does
not answer `abci_info`. At the reference each node is given up to 15
seconds for its `health` answer, so with `n` configured nodes that do
not respond the error can take up to `n × 15` seconds to arrive; the
figure is the reference's and is not part of the contract. Reloaded
answers the same error type and status in the same order of conditions.

R36.14.34 **The legacy method in the same situation (shared legacy path;
completes R36.14.1 and R36.14.12).** The legacy flat `my_tx_history`
also reads the current height on every request, for every coin family,
after it has loaded its own store, and it fails the request when the
read fails. The answer is the legacy error answer of Chapter 55
R55.1.10: HTTP 500 and an object with only an `error` string, whose
text is not bound. The last sentence of R36.14.1 and the "success,
empty list" cells of R36.14.12 therefore hold **while a node answers
the height read**. While none does, the legacy method answers the
legacy error for a Tendermint coin or token as well, although it has no
row whose confirmations the height would serve. A successful legacy
answer for this family carries the height read as `current_block`. In
this situation the legacy method differs from the versioned one in two
points: it has no `StorageIsNotInitialized` case, and its error has the
legacy shape.

R36.14.35 **`sync_status` while no node answers (this family; completes
R36.14.11 and T36.38).** The history state is carried only by a
successful answer of one of the two methods. While no configured node
answers the height read, neither method answers successfully, so no
state can be read at all. The `Finished` that R36.14.11 keeps through a
node failure, and that T36.38 expects "during a node outage", is
therefore observable in two cases only: when a node still answers
`health` and `abci_info` while the fetcher's own requests fail, and
after the outage has ended. A fixture for T36.38 uses the first case.

> **Our side (informative; this repository's tree at the base of this
> round, 2026-11-16).** Both history methods already read the height in
> the place R36.14.33 and R36.14.34 give it: the versioned method tests
> the store first and answers `RpcError` (HTTP 500) when the height read
> fails, and the legacy method loads its store and then answers the
> legacy error. The Tendermint height read is the `abci_info` request
> through the node choice of R36.14.27. Nothing is to be changed for
> R36.14.33 -- R36.14.35. The `withdraw` result type of this tree always
> carries `tx_hex` and has no unsigned form; R36.14.29 -- R36.14.31
> describe what the public-key form adds to it.

**Tests.** Rows T36.55 -- T36.58 share one fixture: a platform coin with
6 decimals, platform denomination `D`, chain id `C` and no `gas_price`
in its configuration (the price is then 0.25, Chapter 18 R18.11.6), so
the computed fee is `ceil(50,000 × 1.5 × 0.25)` = 18,750 base units
(0.01875); a token on it with 6 decimals and denomination `K`; the coin
activated in the public-key form with a 33-byte secp256k1 key whose
account is `A`; a mocked node that reports account number 7 and sequence
3 for `A`, the height 1,000, and balances of 10,000,000 base units of
`D` and of `K`. The request is a `withdraw` of 1 coin to another account
`B` of the same chain, with `memo` `"m"` and no `fee` member. Figures
are computed from the rules.

T36.55 *The `sign_doc` result (R36.14.29, R36.14.30; extends T36.42).*
Ledger flag off. *Expected:* the result has no `tx_hex`, `tx_hash`,
`kmd_rewards`, `legacy_amino_json` or `original_tx_type_url` member;
`sign_doc` has exactly four members; `chain_id` is the string `C`;
`account_number` is the JSON number 7; `body_bytes` and
`auth_info_bytes` are arrays of integers from 0 to 255. Decoded,
`body_bytes` is a body with one `/cosmos.bank.v1beta1.MsgSend` from `A`
to `B` of 1,000,000 `D`, the memo `m` and the timeout height 1,100;
`auth_info_bytes` has one signer entry with the activation key, the mode
`SIGN_MODE_DIRECT` and the sequence 3, a fee of one coin of 18,750 `D`
and the gas limit 125,000. Beside it: `from` `[A]`, `to` `[B]`,
`total_amount` 1.01875, `spent_by_me` 1.01875, `received_by_me` 0,
`my_balance_change` -1.01875, `fee_details` `{"type": "Tendermint",
"coin": <platform ticker>, "amount": "0.01875", "gas_limit": 125000}`,
`block_height` 0, `timestamp` 0, `transaction_type`
`"StandardTransfer"`, `memo` `"m"`, and an `internal_id` equal to the
SHA-256 digest of the empty byte string. A second request without
`memo` answers `memo` `""` and the same `internal_id`. The same request
through the legacy `withdraw` answers the same members.

T36.56 *The Amino result (R36.14.29, R36.14.31; extends T36.43).* Ledger
flag on. *Expected:* no `sign_doc`, `tx_hex` or `tx_hash`;
`original_tx_type_url` is `/cosmos.bank.v1beta1.MsgSend`;
`legacy_amino_json` is a JSON object with exactly the members
`account_number` `"7"`, `chain_id` `C`, `fee`, `memo` `"m"`, `msgs` and
`sequence` `"3"`; `fee` is exactly `amount`, an array of the one coin
with `denom` `D` and `amount` `"56250"`, and `gas` `"125000"`; `msgs`
has one entry of `type` `cosmos-sdk/MsgSend` whose `value` has
`from_address` `A`, `to_address` `B` and an `amount` array of the one
coin with `denom` `D` and `amount` `"1000000"`. No value in the document
is a JSON number, and the document has no `timeout_height`. Beside it:
`fee_details` with `amount` `"0.05625"` and `gas_limit` 125000,
`total_amount` 1.05625. With a node that reports sequence 0 and account
number 0, both members are present as `"0"`; without `memo` in the
request, `memo` is present as `""`. A fixture compares the document as
a JSON value, not as text (R36.14.31 item 6).

T36.57 *A token under the Ledger flag (R36.14.31 item 5).* Ledger flag
on; a `withdraw` of 2 units of the token to `B`. *Expected:* the `msgs`
entry carries one coin with `denom` `K` and `amount` `"2000000"`; the
fee carries one coin with `denom` `D` and `amount` `"18750"` (not
tripled); `fee_details` names the platform ticker with `amount`
`"0.01875"`; `total_amount` is 2; `internal_id` is the SHA-256 digest of
the token id's 32 bytes.

T36.58 *Broadcast of a transaction signed outside the node (R36.14.32).*
(i) The `sign_doc` of T36.55 is signed by a test key under the public
direct sign mode and assembled into a `TxRaw` from the two byte strings
unchanged; `send_raw_transaction` with `coin` and the hex of that
`TxRaw` makes the mocked node receive one `broadcast_tx_commit` request
with exactly those bytes, and answers `tx_hash`. (ii) The same with a
`TxRaw` built from the document of T36.56 under R36.14.31 item 7, whose
body has no timeout height. (iii) The same `tx_hex` sent with the
token's ticker as `coin` is broadcast in the same way. (iv) A `tx_hex`
that does not decode as a `TxRaw` (for instance text that is not hex)
yields the legacy error answer and no request to the node.

T36.59 *Versioned history while no node answers (R36.14.33).* *Input:* a
platform coin and a token on it, activated with `tx_history: true`,
whose store holds rows from a completed first pass; then every
configured node refuses connections. *Expected:* (i) the mmrpc-2.0
`my_tx_history` for the coin and for the token answers HTTP 500 with
the error type `RpcError` and string data, and no `result`; (ii) the
row count of the store is unchanged afterwards; (iii) once a node
answers again at height `h`, the same request answers the stored rows
with `current_block` `h` and, for each row at or below `h`,
`confirmations` equal to `h` minus the row's height plus one; (iv) with
two configured nodes of
which only the first refuses connections, the request succeeds; (v) a
coin activated without `tx_history` on a database that has no store for
its ticker answers `StorageIsNotInitialized` (HTTP 500) while every
node refuses connections, not `RpcError`; (vi) a coin activated without
`tx_history` on a database that holds an earlier store answers
`RpcError` in the same situation; (vii) a node that answers `health`
and fails `abci_info` yields `RpcError`.

T36.60 *Legacy history and the state while no node answers (R36.14.34,
R36.14.35).* With the input of T36.59: (i) the legacy `my_tx_history`
for the coin and for the token answers HTTP 500 and an object whose
only member is an `error` string; (ii) once a node answers again at
height `h` it answers an empty `transactions` list, `total` 0,
`current_block` `h` and the `sync_status` of R36.14.11; (iii) with a
node that answers `health` and `abci_info` and fails every history
query of the fetcher, both methods answer successfully with
`sync_status` `{"state": "Finished"}`.

> **Status note (2026-11-16, Reloaded-original; as built after the clearance
> of this subsection).** The unsigned result of a watch-only coin was compared
> with R36.14.29 -- R36.14.32 member by member and corrected where it differed.
>
> - *R36.14.31 item 1 and item 4.* `legacy_amino_json` is a JSON object, not
>   text, and has no `timeout_height`. (The earlier text form and the included
>   timeout height were this tree's own and are gone.)
> - *R36.14.31 item 5.* The factor three of R36.14.18 item 4 applies to the
>   platform coin only; a token's document and `fee_details` carry the computed
>   amount once.
> - *R36.14.29, `internal_id`.* An unsigned result carries the constant
>   identifier: the SHA-256 digest of the empty byte string for the platform
>   coin, the SHA-256 digest of the 32 bytes of the token id for a token. It
>   identifies no transaction. The staking requests of a watch-only coin
>   (D36.18 (a)) use the platform coin's constant.
> - *R36.14.29, `memo`.* The shared `withdraw` request has a `memo` member
>   (absent is the empty memo). It is the memo of the transaction body and of the
>   Amino document, and an unsigned result answers it as `memo`, `""` when the
>   request has none. A signed Tendermint `withdraw` takes the same memo in its
>   body (it was always empty before); every other coin family ignores the member.
> - *R36.14.29 other members; R36.14.30.* `tx_hex`, `tx_hash` and `kmd_rewards`
>   are absent, `from` is the activated address, `to` the request's `to`,
>   `fee_details` the Tendermint form with the platform ticker, `block_height`
>   and `timestamp` 0, `transaction_type` as for a signed `withdraw` (a token's
>   signed `withdraw` answers `"StandardTransfer"` in this tree, and so does its
>   unsigned result); `sign_doc` has the four members in the encodings of
>   R36.14.30 (the body's timeout height is the height read plus 100). No
>   difference was found.
> - *R36.14.32.* No difference: `send_raw_transaction` takes the hex of a
>   `TxRaw` for the platform coin or a token, checks that it decodes, submits it
>   with `broadcast_tx_commit` and answers the node's upper-case hash; bytes that
>   do not decode give the legacy error and no node request. The node never
>   reads an unsigned result back.
> - *R36.14.33 -- R36.14.35.* The "Our side" note above holds in this tree and
>   nothing was changed: the versioned `my_tx_history` tests the store first and
>   answers `RpcError` (HTTP 500) when the height read fails, the legacy method
>   loads its store and then answers the legacy error, and the state is read
>   from a successful answer only. The `RpcError` observed on 2026-11-16 while no
>   node answered is therefore the conforming answer, not a difference.
> - *Tests.* T36.55 (`t36_55_the_sign_doc_result`), T36.56
>   (`t36_56_the_amino_result`), T36.57
>   (`t36_57_a_token_under_the_ledger_flag`) and T36.58
>   (`t36_58_broadcast_of_a_transaction_signed_outside_the_node`) are in
>   `tendermint_swap_flow_tests/watch_only_tests.rs`; T36.59
>   (`t36_59_versioned_history_while_no_node_answers`,
>   `t36_59_a_node_that_answers_health_and_fails_abci_info_gives_rpc_error_and_serving_resumes`)
>   and T36.60 (`t36_60_legacy_history_while_every_node_refuses_connections`,
>   `t36_60_the_state_is_finished_while_a_node_answers_and_the_fetcher_fails`) in
>   `tendermint_swap_flow_tests/tx_history_tests.rs`. T36.60 (iii) leaves the
>   fetcher unrun after its first pass (a failing fetcher would pause and
>   retry); the legacy `withdraw` of T36.55 shares the result type and its
>   serialization with the mmrpc-2.0 form and is not run separately.
