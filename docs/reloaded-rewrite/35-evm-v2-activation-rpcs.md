# Chapter 35 -- EVM (Ethereum) V2 Activation & Token RPC Surface

**Status:** driving-spec (required port). This chapter specifies an **activation
RPC layer** over EVM coin code that already exists in reloaded; it does not
introduce a new coin.

> **One-sentence claim:** the project shall expose the standard Komodo DeFi
> Framework **mmrpc 2.0** activation surface for EVM (Ethereum-family) coins --
> a one-shot "platform coin with tokens" call, a single-token call (and its NFT
> alias), a long-running task variant of each, an on-chain token-info query, and
> read/write of the per-coin swap gas-fee policy -- on top of the EVM coin type
> that reloaded already ships, wire-compatible per method on request fields,
> success shape, `error_type` set, failure-to-`error_type` mapping, `error_data`
> shape, HTTP status and numeric encoding.

> **Treatment:** **T-PORT.** The EVM coin type and its legacy `enable` path are
> present in reloaded (see §35.7). What is required is the **activation
> integration** (the platform-coin-with-tokens and token activation trait
> implementations for the EVM coin/token, the task variants, and the dispatcher
> routes for the method strings named below). The wire contract distilled here
> is the source of truth for that port.

> **Binding scope (R36).** Requirements bind observable behaviour, the public
> mmrpc-2.0 method strings, their request/response JSON field names and types,
> the `error_type` discriminants, the JSON shape of each `error_data`, the HTTP
> status of each mmrpc-level error, and externally *dictated* interop (EVM
> JSON-RPC, EIP-1559 fee semantics, EIP-55 address checksums, ERC-20 contract ABI
> calls, CAIP-style chain identification). The chapter also binds the two
> non-activation EVM coin-layer contracts that the activation surface selects or
> presupposes: the multi-node access contract over the `nodes` list (§35.1.F),
> the per-coin swap gas-fee policy (§35.6) and the byte-level
> contract for reading a serialized EVM transaction the project did not build
> (§35.6A), whose dictated interop is EIP-2718 envelope framing, EIP-2930 and
> EIP-1559 payload layout, EIP-155 replay protection, keccak-256 and secp256k1
> public-key recovery. The human-readable `error` text, `error_path` and
> `error_trace` are **not** bound, except that the two Reloaded-only rules
> R35.1.14 and R35.1.15 constrain what an `error` text or log line may say
> about a node, and the Reloaded-only rule R35.1.17 constrains what any
> rendering of a value may print of a credential. Private Rust types, helper
> decomposition, and internal module structure are informative and are
> **not** bound by this chapter.

> **Sources of truth (binding order).**
> 1. The public Komodo DeFi Framework API reference
>    (`komodoplatform.com/en/docs/komodo-defi-framework/api/`: the v20
>    `coin_activation/enable_eth_with_tokens`, `coin_activation/enable_erc20`,
>    `coin_activation/task_managed/enable_eth`, `non_fungible_tokens/enable_nft`
>    pages, `common_structures/{activation,wallet,nfts,error_types}` and the v20
>    index) **governs where it is explicit**: a field table and its example agree,
>    or an example stands uncontradicted.
> 2. Where the reference is silent, ambiguous or contradicts itself -- including
>    every HTTP status code, which the reference never states -- the observable
>    behaviour of the **GLEEC KDF reference version `v3.0.0-beta`** (the most
>    recent GLEEC release tag available, dated 2026-04-01) governs. That behaviour
>    was established by the KDF Spec Reader and is stated here in behavioural
>    terms only.
> 3. Where the legacy reference `v2.6.0-beta` (netid 8762, AGENTS.md §3) differs
>    on an activation wire shape, an `Upstream divergence (informative)` note
>    records it; the two are not blended.
>
> Each request-field row and error row below carries a **Basis** marker:
> **D** = the public reference is explicit; **G** = the reference is silent,
> ambiguous or self-contradictory and the reference version decides; **D/G** =
> the reference is explicit about part of the contract and the reference version
> decides the rest (the row says which part).

---

## 35.0 Executive summary

EVM coins follow the project's **platform-coin-with-tokens** activation model:
the native gas coin (ETH and other EVM chains) is the *platform coin*, and ERC-20
contracts are its *child tokens*. The NFT pseudo-coin of a platform (ch. 19) is
activated either inline with the platform (`nft_req`, R35.1.4b) or afterwards
through the single-token handler (`enable_nft`, R35.2.4). The surface specified
here is:

| Method string | Tier | Targets | Purpose |
| --- | --- | --- | --- |
| `enable_eth_with_tokens` | mmrpc 2.0 | all (incl. WASM) | activate the EVM platform coin plus an inline batch of ERC-20 tokens (and optionally its NFT pseudo-coin) in one call |
| `enable_erc20` | mmrpc 2.0 | all (incl. WASM) | activate one ERC-20 token against an already-active EVM platform |
| `enable_nft` | mmrpc 2.0 | all (incl. WASM) | alias of the single-token handler for the NFT pseudo-coin (NFT specifics: ch. 19) |
| `task::enable_eth::{init,status,user_action,cancel}` | mmrpc 2.0 (task) | all (incl. WASM) | long-running variant of `enable_eth_with_tokens` |
| `task::enable_erc20::{init,status,user_action,cancel}` | mmrpc 2.0 (task) | all (incl. WASM) | long-running variant of `enable_erc20` (§35.3A) |
| `get_token_info` | mmrpc 2.0 | all | read an ERC-20 contract's on-chain info (symbol, decimals) |
| `get_swap_gas_fee_policy` | mmrpc 2.0 | all | read the per-coin swap gas-fee policy |
| `set_swap_gas_fee_policy` | mmrpc 2.0 | all | set the per-coin swap gas-fee policy |

Two sections of this chapter are not activation RPCs. §35.6 binds the per-coin
swap gas-fee policy, which decides which EIP-2718 transaction form the coin
*emits*; §35.6A binds the contract for *reading* a serialized EVM transaction the
project did not build -- the counterparty-facing side of the same choice, because
a peer whose policy is any non-legacy tier hands us a typed envelope. They sit
here because no other chapter owns the EVM coin layer as a whole, and because the
second is a direct consequence of the first.

**Shared conventions (binding).**

- **Envelope.** All methods use the mmrpc-2.0 request envelope with the method's
  fields directly in `params` (there is no `activation_params` wrapper on the
  platform or task-platform methods). Success is
  `{"mmrpc":"2.0","result":...,"id":...}`; errors use the error envelope
  `error, error_path, error_trace, error_type, error_data, id` with
  `error_type`/`error_data` adjacently tagged and `error_data` **absent** for a
  unit discriminant. The envelope, the `id` echo, and the HTTP-status policy are
  bound once in [Chapter 55](55-coin-activation-wire-contract.md) §55.1.
- **Request-parse failures.** A request that fails to deserialize -- a missing
  required field, a field of the wrong JSON type, an unknown enum value (for
  example `rpc_mode: "Metamask"` or `priv_key_policy: {"type":"Metamask"}` on a
  native build), a malformed address string -- is rejected before activation
  starts with the dispatcher-level `InvalidRequest` discriminant, `error_data` a
  string, HTTP 400 (ch. 55 §55.1). Unrecognised extra fields are **ignored**.
- **Task framework.** `status`, `user_action` and `cancel` of every `task::`
  family in this chapter use the shared task-framework contract of ch. 55 §55.2
  (task-status envelope, `NoSuchTask` / `TaskFinished` with integer
  `error_data`, `user_action`/`cancel` success `"success"`). This chapter binds
  only the per-family `init` request, in-progress / awaiting values, terminal
  result, and terminal error set.
- **Numeric encoding.** Every balance and amount (`spendable`, `unspendable`,
  NFT `amount`) is a JSON **string** holding a decimal number; there are no
  `*_rat` / `*_fraction` companions anywhere in activation. `current_block`,
  `required_confirmations`, `task_id`, `account_index`, `gap_limit`,
  `min_addresses_number`, `gas_station_decimals` and `mm2` are JSON integers.
- **Addresses.** Wallet addresses used as map keys and in `address` fields are
  0x-prefixed EIP-55 checksummed strings (TRON addresses for a TRX platform:
  ch. 21).

---

## 35.1 `enable_eth_with_tokens` -- platform coin with tokens

### 35.1.A Request

R35.1.1 The public RPC `enable_eth_with_tokens` shall activate an EVM platform
coin together with an inline list of ERC-20 tokens in a single mmrpc-2.0 call.
Its `params` object shall accept exactly the fields below (plus any unrecognised
field, which is ignored). "Absent" means the key is missing or `null`.

| Field | JSON type | Required / default | Basis | Effect |
| --- | --- | --- | --- | --- |
| `ticker` | string | required | D | Platform coin ticker. Its coins-config `protocol` must be the EVM platform protocol (`{"type":"ETH","protocol_data":{"chain_id":<integer>}}`, §35.5) or the TRON platform protocol of ch. 21. |
| `nodes` | array of CoinNode | documented as required; absent is treated as empty | D/G | EVM JSON-RPC endpoints, all caller-supplied (no endpoint is embedded). Empty or absent → `AtLeastOneNodeRequired` (G). |
| `nodes[].url` | string | required | D | Endpoint URL. Accepted schemes: `http`, `https` (HTTP JSON-RPC) and `ws`, `wss` (WebSocket JSON-RPC); the transport is selected by the scheme of `url` itself. An unparseable URL or any other scheme → `InvalidPayload` (G). TRX platform: HTTP(S) only (ch. 21). |
| `nodes[].komodo_proxy` | boolean | default `false` | D (examples) | The endpoint sits behind the Komodo signed proxy; requests to it carry the proxy authentication signature derived from the node's P2P identity. The reference's CoinNode *table* names this flag `komodo_auth`; every example and the reference version use `komodo_proxy` (see erratum note). `komodo_auth` is not recognised (ignored). |
| `nodes[].ws_url` | string | ignored | G | Appears in reference examples next to an HTTP `url`; the reference version does not consume it. Shall be accepted and ignored. |
| `erc20_tokens_requests` | array of TokensRequest | required (may be `[]`) | D | Tokens to activate with the platform. Absent → `InvalidRequest`. |
| `erc20_tokens_requests[].ticker` | string | required | D | Token ticker. |
| `erc20_tokens_requests[].required_confirmations` | integer | default: token config value, else the platform's effective value | G | Swap confirmations for this token. The reference table states a default of `3`; the reference version falls back as stated here. |
| `erc20_tokens_requests[].protocol` | coin-protocol object (§35.5) | optional | G | Inline protocol for a **custom token** that is not in the coins config (R35.2.7). |
| `swap_contract_address` | string (0x-hex EVM address) | documented as required; reference version: required unless the coin config marks the coin `wallet_only` | D/G | V1 swap (HTL) contract. Absent for a trading-capable coin, or the zero address → `InvalidPayload`. Ignored for a wallet-only coin. Malformed hex → `InvalidRequest`. |
| `fallback_swap_contract` | string (0x-hex EVM address) | documented as required; reference version: optional | D/G | Fallback V1 swap contract. The zero address → `InvalidPayload` (trading-capable coins). |
| `swap_v2_contracts` | object | optional; mandatory for a trading-capable coin when the node configuration enables trading protocol v2 (`use_trading_proto_v2`) | D/G | Members `maker_swap_v2_contract`, `taker_swap_v2_contract`, `nft_maker_swap_v2_contract`, each a required 0x-hex address string (ch. 17). Missing when mandatory, or any member the zero address → `InvalidPayload`; a missing member → `InvalidRequest`. |
| `contract_supports_watchers` | boolean | default `false` | G | Whether the V1 swap contract supports the watcher protocol. |
| `required_confirmations` | integer | default: coin config `required_confirmations`, else `3` | D/G | Swap confirmations for the platform coin. Maintainer decision (2026-10-11): config, else `3`. The docs table states `3`; the reference version uses config, else `1`; the docs' own BCH page states this config-then-3 rule. Legacy EVM `enable` / `electrum` (ch. 55 §55.4) keep their own default. |
| `requires_notarization` | boolean | default `false` | D | No effect on EVM coins. |
| `mm2` | integer | optional | D/G | Accepted as an unsigned 8-bit integer and not otherwise enforced (G); a non-integer or out-of-range value → `InvalidRequest`. |
| `tx_history` | boolean | default `false` | D | When `true`, background transaction-history preloading for the platform starts after activation (the documented effect). |
| `gas_station_url` | string | optional | D (examples) | Gas-station endpoint used for legacy gas-price estimation. |
| `gas_station_decimals` | integer | default `8` | D | Decimals that denominate the gas-station response in gwei. |
| `gas_station_policy` | object `{"policy": "MeanAverageFast" \| "Average"}` | default `MeanAverageFast` | D | How the gas price is derived from the station response. |
| `swap_gas_fee_policy` | string `Legacy` \| `Low` \| `Medium` \| `High` | default: coin config value, else `Legacy` | G | Initial swap gas-fee policy of the coin (§35.6). Unknown value → `InvalidRequest`. |
| `rpc_mode` | string `Default` \| `Metamask` | default `Default` | D | `Metamask` exists only on the WASM target (R35.1.4a). |
| `priv_key_policy` | tagged object | default `{"type":"ContextPrivKey"}` | D/G | Signing policy (R35.1.4). |
| `get_balances` | boolean | default `true` | D | When `false`, single-address results omit balances (R35.1.3). |
| `nft_req` | object | optional | D/G | Inline NFT activation (R35.1.4b). |
| `path_to_address` | object `{account_id: integer, chain: "External" \| "Internal", address_id: integer}` | default `{account_id: 0, chain: "External", address_id: 0}` | D/G | HD only: which derived address becomes the enabled (swap) address. The reference describes `chain` as an integer but its examples and the reference version use the strings. |
| `gap_limit` | integer | default `20` | D/G | HD only: address gap limit. |
| `scan_policy` | string `do_not_scan` \| `scan_if_new_wallet` \| `scan` | default `scan_if_new_wallet` | D/G | HD only: address-scan policy at activation (semantics as ch. 38 §38.8). |
| `min_addresses_number` | integer | optional | D | HD only: minimum known external addresses per account (semantics as ch. 38 R38.8.5). |

R35.1.1a The fields `tx_history`, `requires_notarization`, `gas_station_url`,
`gas_station_decimals` and `gas_station_policy` are not consumed by the
reference version at all. A malformed value in any of these five fields shall
**not** fail activation; the field is then treated as absent. When well-formed,
`tx_history` and the gas-station fields shall have the documented effect.

### 35.1.B Activation flow and failure precedence

R35.1.2 Activation shall proceed through the following phases, and when a
request has several faults the reported error shall be the one of the earliest
failing phase:

1. **Policy gate (one-shot only):** a hardware (`Trezor`) `priv_key_policy` is
   rejected (`UnexpectedDeviceActivationPolicy`).
2. **Already active:** `ticker` is already an active coin
   (`PlatformIsAlreadyActivated`).
3. **Platform config:** config entry present and its protocol parseable and of
   the EVM/TRON platform kind (`PlatformConfigIsNotFound`,
   `CoinProtocolParseError`, `UnexpectedPlatformProtocol`). An EVM platform
   protocol without an integer `chain_id` is a protocol parse failure.
4. **Platform construction:** signing-policy preparation (MetaMask session,
   WalletConnect session, hardware context), swap-contract validation, key /
   address derivation (including HD `path_to_address` and the config
   `derivation_path`), node-list validation, and `rpc_mode` / policy consistency.
   No network round trip to the EVM nodes is required in this phase.
5. **Tokens**, batch-wise in three passes, each pass covering every requested
   token in request order before the next pass starts: (a) config/protocol
   resolution (or custom-token validation) of all tokens; (b) platform
   consistency (`PlatformCoinMismatch`) of all tokens; (c) construction of each
   token in turn. The first failing token of the earliest failing pass decides
   the error. A token whose config lacks `decimals` (or declares `0`) has its
   decimals read from the contract on-chain during its construction.
6. **Inline NFT** (when `nft_req` is present).
7. **Result:** chain height, public key, balances (or token tickers) and the NFT
   summary.
8. **Registration** of the platform, its tokens and its NFT pseudo-coin; a
   concurrent registration of the same ticker yields
   `PlatformIsAlreadyActivated`.

A failure in any phase shall leave neither the platform coin nor any of its
tokens or NFT pseudo-coin registered (no partial activation). Activation shall
succeed when, for every request it needs, at least one of the supplied nodes
answers (multi-node failover); a subset of unresponsive nodes is not an error.

### 35.1.C Success result

R35.1.3 The success `result` is one of two shapes, selected by the platform's
derivation mode and distinguished structurally (untagged union); a client
matches on the fields present.

**(a) Single-address shape** -- used for the context key in Iguana mode and for
the MetaMask and WalletConnect policies:

| Field | JSON type | Presence | Content |
| --- | --- | --- | --- |
| `current_block` | integer | always | Current chain height. |
| `eth_addresses_infos` | object (map) | always | Exactly one entry, keyed by the wallet address, value an AddressInfo whose `balances` (when present) is a single BalanceInfo `{spendable, unspendable}` of the platform coin. |
| `erc20_addresses_infos` | object (map) | always | Exactly one entry, keyed by the same address, value an AddressInfo whose `balances` (when present) is a map *token ticker → BalanceInfo* covering every activated token (an empty object when no tokens were requested). |
| `nfts_infos` | object (map) | **always** | NFT summary: `{}` when `nft_req` was not supplied; otherwise a map keyed `"<token_address>,<token_id>"` whose values carry `token_address`, `token_id` (decimal string), `chain`, `contract_type`, `amount` (decimal string) as bound by ch. 19 §19.6.2. |

AddressInfo carries `derivation_method` (`{"type":"Iguana"}` in this shape),
`pubkey`, and then, depending on `get_balances`:

- `get_balances: true` (default): `balances` is present on both entries;
  `tickers` is absent.
- `get_balances: false`: `balances` is **omitted** from both entries; the
  `erc20_addresses_infos` entry carries `tickers`, a JSON array of the activated
  token tickers (set semantics, order not significant); the
  `eth_addresses_infos` entry carries neither `balances` nor `tickers`.

`pubkey` is the uncompressed secp256k1 public key of the wallet address as
lower-case hex **with** the leading `04` byte and **without** a `0x` prefix
(130 hex characters).

**(b) HD shape** -- used whenever the platform's address is derived
hierarchically: the daemon runs in HD (mnemonic) mode, or the `Trezor` policy
(task path only):

| Field | JSON type | Content |
| --- | --- | --- |
| `current_block` | integer | Current chain height. |
| `ticker` | string | The platform ticker. |
| `wallet_balance` | object | HD wallet-balance report: `{"wallet_type":"HD","accounts":[...]}`; each account carries `account_index` (integer), `derivation_path` (string), `total_balance` (map ticker → BalanceInfo) and `addresses` (array of `{address, derivation_path, chain, balance}` with `balance` a map ticker → BalanceInfo). Each balance map covers the platform ticker and every activated token ticker. Report shape and account/scan semantics are those of the HD activation report shared with the UTXO task family (ch. 55, ch. 38 §38.8). |
| `nfts_infos` | object (map) | **Always** present; `{}` (inline NFT activation is not available in HD mode, R35.1.4b). |

In the HD shape `get_balances` has no effect (balances are always reported)
(G: the reference shows no HD example with `get_balances: false`; the reference
version ignores the flag in HD mode).

The success HTTP status is 200.

### 35.1.D Signing policy, RPC mode, inline NFT

R35.1.4 `priv_key_policy` is an **object** tagged by `type`, with any payload in
a sibling `params` object (G for the payload key; see erratum). A bare string
(for example `"priv_key_policy": "Trezor"`) is a request-parse failure
(`InvalidRequest`). The variants are:

| `type` | `params` | Targets | Path | Effect |
| --- | --- | --- | --- | --- |
| `ContextPrivKey` (default) | none | all | one-shot and task | The daemon's own key: single-address in Iguana mode; HD derivation at the coin config `derivation_path` and `path_to_address` in HD mode. |
| `Trezor` | none | native (hardware support) | **task only** (R35.3.2) | Address and public key come from the connected device; HD shape. On the one-shot call → `UnexpectedDeviceActivationPolicy`. |
| `Metamask` | none | **WASM only** | one-shot and task | Signer is the connected MetaMask session (ch. 47); requires `rpc_mode: "Metamask"` (R35.1.4a). On a native build the value does not exist → `InvalidRequest`. |
| `WalletConnect` | `{"session_topic": "<hex string>"}` | all | one-shot and task | Signer is the external wallet of an established WalletConnect session (ch. 22, `wc_new_connection`); the address and public key are obtained from that wallet during activation; single-address shape. |

Reloaded may additionally accept `Iguana` / `IguanaPrivKey` as aliases of
`ContextPrivKey`.

R35.1.4a `rpc_mode` selects the JSON-RPC transport. `Default` uses `nodes`.
`Metamask` (WASM only) routes chain requests through the connected MetaMask
provider. On WASM, `rpc_mode` and `priv_key_policy` shall be **both** MetaMask
or **neither**; any other combination fails with `PlatformCoinCreationError`.
Under MetaMask: no connected session, or an active-account mismatch →
`Transport`; the provider reports the chain as not registered, or any other
provider failure while selecting the chain → `PlatformCoinCreationError`; the
provider answers with a chain id other than the coin's → `Internal`. A TRX
platform with `rpc_mode: "Metamask"` → `Internal`.

R35.1.4b `nft_req` activates the platform's NFT pseudo-coin (`NFT_<chain>`,
ch. 19) as part of the same call. Accepted forms:

- the reference-version form `{"provider": NftProvider}`, and
- the documented form, a bare `NftProvider` (G/D: the reference's table and
  example agree on the bare form, but the reference version requires the
  `provider` wrapper; both shall be accepted).

`NftProvider` is `{"type":"Moralis","info":{"url":<string URL>,"komodo_proxy":<boolean, default false>}}`
(ch. 19 §19.6.2). An unknown provider `type` or a syntactically invalid `url` is
a request-parse failure (`InvalidRequest`). Inline NFT activation performs the
same initial owned-inventory fetch as `enable_nft` for the platform's single
address and reports it in `nfts_infos`. Failures:

| Condition | `error_type` | HTTP |
| --- | --- | --- |
| Platform address is HD-derived (HD mode or hardware policy) | `UnexpectedDerivationMethod` | 500 |
| Platform ticker does not map to one of the NFT chains of ch. 19 §19.3, or the platform is TRX | `Internal` | 500 |
| Provider unreachable, non-success HTTP status, or unparseable envelope | `Transport` | 502 |
| Provider rejected the request as invalid, or an inventory entry lacks its contract type | `InvalidPayload` | 400 |
| Local storage or other internal failure | `Internal` | 500 |

An inline-NFT failure fails the whole activation (R35.1.2). In HD mode the
reference directs clients to activate NFTs with `enable_nft` after activation
instead (D).

### 35.1.E Error contract

R35.1.5 The `error_type` set, failure mapping, `error_data` shape and HTTP status
of `enable_eth_with_tokens` shall be exactly the following. All rows are
mmrpc-level for the one-shot call; for `task::enable_eth` see R35.3.5 for
placement.

| `error_type` | Failure class | `error_data` | HTTP | Basis |
| --- | --- | --- | --- | --- |
| `PlatformIsAlreadyActivated` | `ticker` already active, or lost a concurrent registration race | string: the ticker that is already active | 400 | D |
| `PlatformConfigIsNotFound` | no coins-config entry for `ticker` | string: `ticker` | 400 | D |
| `CoinProtocolParseError` | platform config `protocol` missing or unparseable (including an EVM protocol without integer `chain_id`) | object `{ticker: string, error: string}` | 500 | D |
| `UnexpectedPlatformProtocol` | platform protocol parses but is not an EVM/TRON platform protocol | object `{ticker: string, protocol: object}` -- `protocol` is the serialized coin protocol (`type`, plus `protocol_data` where the variant has it) | 400 | D |
| `TokenConfigIsNotFound` | a requested token without inline `protocol` has no config entry | string: the token ticker | 400 | D |
| `TokenProtocolParseError` | a token's config `protocol` missing or unparseable | object `{ticker: string, error: string}` | 500 | D |
| `UnexpectedTokenProtocol` | a token's protocol is not ERC-20 (TRC-20 under a TRX platform), or its `contract_address` is not a 0x-prefixed address in exact EIP-55 checksummed form | object `{ticker: string, protocol: object}` (serialized coin protocol incl. `protocol_data`) | 400 | D |
| `PlatformCoinMismatch` | a token's protocol names a platform other than `ticker` | absent | 400 | G |
| `CustomTokenError` | an inline token `protocol` was rejected (R35.2.7) | the custom-token sub-error, externally tagged (R35.2.7) | 500 | G |
| `AtLeastOneNodeRequired` | `nodes` empty or absent | string (human-readable) | 400 | G |
| `InvalidPayload` | node URL unparseable or of an unsupported scheme; swap contract absent for a trading-capable coin or zero; `swap_v2_contracts` mandatory but absent, or a member zero; fallback contract zero; coin config `derivation_path` unusable for an HD or hardware policy; `path_to_address` not derivable; inline-NFT provider rejected the request | string | 400 | G |
| `UnexpectedDeviceActivationPolicy` | `Trezor` policy on the one-shot call | absent | 400 | G |
| `WalletConnectError` | WalletConnect policy: WalletConnect context unavailable, unknown/expired session, relay failure, or the wallet declined to reveal its account | string | 400 | G |
| `PlatformCoinCreationError` | WASM MetaMask: `rpc_mode`/policy mismatch, or the provider cannot select the coin's chain | object `{ticker: string, error: string}` | 500 | D (shape from the reference's sibling platform pages) / G (mapping) |
| `UnexpectedDerivationMethod` | inline NFT or a token step required a single address but the platform is HD-derived | string (human-readable) | 500 | D (name) / G |
| `PrivKeyPolicyNotAllowed` | the selected signing policy cannot perform an operation activation needs (no such path exists in the reference version; reserved) | string | 500 | G (the method table spells it `PrivKeyNotAllowed`; the error catalogue and the reference version use `PrivKeyPolicyNotAllowed`) |
| `FailedSpawningBalanceEvents` | reserved (balance-event streaming could not be started) | string | 400 | G |
| `Transport` | the EVM nodes (or the MetaMask provider, or the inline-NFT provider) gave no usable answer -- R35.1.6 | string (human-readable) | 502 | D |
| `Internal` | hardware-policy failures (R35.3.2); HD wallet storage failure; inline-NFT chain unsupported; MetaMask chain-id mismatch; any other unexpected failure | string (human-readable) | 500 | D (name/shape) / G (mapping) |
| `NoSuchTask` | task variant only, framework-originated | integer: task id | 400 | G |
| `TaskTimedOut` | task variant only, framework-originated timeout | object `{duration: {secs: integer, nanos: integer}}` | 500 | G |

The reference's Error Types table lists `InternalError`; the discriminant on the
wire is `Internal` (G: the catalogue and the reference version agree on
`Internal`).

R35.1.6 **Backend failure mapping (binding).** For every point at which
activation needs an answer from a backend, the outcome shall be:

| Failure point | Condition | `error_type` | `error_data` | HTTP | Basis |
| --- | --- | --- | --- | --- | --- |
| Any node-reachability probe the implementation performs before use | no node answers | `Transport` | string | 502 | D |
| Chain-height read (phase 7) | no node returns a usable answer (connection failure, timeout, HTTP non-success status such as 502, JSON-RPC error, unparseable response) | `Transport` | string | 502 | D |
| On-chain token `decimals()` read (phase 5) | as above | `Transport` | string | 502 | G |
| Platform or token balance read, single-address or HD (phase 7) | as above | `Transport` | string | 502 | G |
| Inline-NFT inventory fetch (phase 6) | provider unreachable / non-success / unparseable | `Transport` | string | 502 | G |
| MetaMask provider (WASM) | no session / account mismatch / provider error on a chain request | `Transport` | string | 502 | G |
| WalletConnect relay or wallet | session/relay failure | `WalletConnectError` | string | 400 | G |
| Some, not all, nodes unresponsive | at least one node answers each request | success | -- | 200 | G |

The mandatory case -- **every supplied node unresponsive** (for example every
node answering HTTP 502) -- shall produce `Transport` with a **string**
`error_data` and HTTP 502, regardless of which phase first touches the network.
The reference's example for this case carries the message "Failed to get client
version for all nodes" (public-docs citation; the message text is not bound).

> **Upstream divergence (informative).** In the reference version
> `v3.0.0-beta` no reachability probe happens before use, and a chain-height
> read that gets no usable answer is reported as `Internal` (HTTP 500); with
> every node down and all token decimals present in config, that is the first
> network failure, so the reference version answers `Internal` where the public
> reference documents `Transport`. The public reference is explicit for this
> case and governs. In `v2.6.0-beta` an on-chain decimals read that gets no
> usable answer is reported as `Internal`; `v3.0.0-beta` reports it as
> `Transport`, which this chapter binds.

> **Upstream divergence (informative).** `v2.6.0-beta` treats
> `swap_contract_address` as mandatory at request-parse level for every coin (an
> absent value is `InvalidRequest`, a zero value `InvalidPayload`);
> `v3.0.0-beta` accepts its absence for a wallet-only coin and reports its
> absence for a trading-capable coin as `InvalidPayload`. This chapter binds the
> `v3.0.0-beta` behaviour; both answer HTTP 400 for a trading-capable coin.

> **Public-docs erratum (informative).** The public reference (a) names the
> proxy flag `komodo_auth` in its CoinNode table while every example uses
> `komodo_proxy`; (b) shows `ws_url`, which is not consumed; (c) links
> `swap_v2_contracts` to a table copied from TokensRequest -- the real members are
> those of its example; (d) documents `nfts_infos` as a "list" although every
> example (and the reference version) returns an object map; (e) shows the
> WalletConnect policy as `{"type":"WalletConnect","data":"<topic>"}` and types
> `data` as a list -- the reference version requires
> `{"type":"WalletConnect","params":{"session_topic":"<topic>"}}`, which this
> chapter binds (when EVM WalletConnect signing is implemented, the docs form
> shall be accepted alongside it, as ch. 36 R36.1.3 does for Tendermint by
> maintainer decision); (f) spells `PrivKeyNotAllowed` and `InternalError` in the method
> table; (g) states a default of `3` for `required_confirmations`; the reference
> version uses the coin config, else 1. By maintainer decision (2026-10-11) this
> chapter binds the coin config, else 3 -- the rule the docs' own BCH page
> states -- so a coin without a configured value waits for 3 confirmations, the
> safer of the two documented numbers; (h) marks
> `swap_contract_address` and `fallback_swap_contract` as required while the
> reference version accepts their absence as described in R35.1.1 -- accepting
> their absence is a superset that cannot break a client that sends them; (i) shows the
> bare `NftProvider` for `nft_req` while the reference version requires the
> `provider` wrapper -- both are accepted (R35.1.4b); (j) documents the
> HD-only `chain` as an integer while its examples use strings.

> **Upstream divergence (informative).** The reference version does not parse
> `tx_history`, `requires_notarization` or the gas-station fields of this method
> (no background history for an EVM platform activated this way, no gas-station
> pricing); this chapter keeps their documented effect (R35.1.1a), which changes
> no wire shape.

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`:
> the full request table, the R35.1.2 precedence including the three-pass token
> batch, both result shapes with `nfts_infos` and `get_balances`, the HD address
> at the coin's `derivation_path` plus `path_to_address` (Reloaded previously
> bound the daemon's internal identity key; see the upgrade note in
> `CHANGELOG.md`), the `04`-prefixed `pubkey`, the R35.1.5 error table and the
> R35.1.6 backend-failure mapping (every node unresponsive is `Transport`,
> string, 502). Remaining deviations:
> 1. The `WalletConnect` policy parses but answers `WalletConnectError` (400):
>    WalletConnect signing for EVM coins is not available in this build.
> 2. `ws://` / `wss://` node URLs are rejected as `InvalidPayload`, because the
>    node transport is HTTP-only.
> 3. `komodo_proxy` is parsed but requests are not proxy-signed.
> 4. `contract_supports_watchers` is parsed only.
> 5. Under MetaMask (WASM) chain requests still go to `nodes`, not through the
>    provider.

---

### 35.1.F Multi-node access contract

> **Clearance (chapter 01 T1, R40).** This section and the material that
> cites it (the T35.24 -- T35.33 items of §35.8A, D35.8, the acceptance
> criterion of §35.8 that names §35.1.F, and the cross-reference sentences
> added to chapters 17, 21 and 57 on the same day) were authored by the KDF
> Spec Reader on 2026-10-26 from the reference rows `v2.6.0-beta` and
> `v3.0.0-beta` (chapter 02 §2.15A). **They passed the KDF Dirty Gate on
> 2026-10-26** before clean-side implementation; nothing needed scrubbing.
>
> - *Extent.* The EVM coin layer's multi-node access subsystem at both rows:
>   the HTTP and WebSocket node transports, the per-request rotation layer,
>   node setup in both activation routes (v2 `nodes`, legacy `urls`), the
>   ERC-20 token-metadata reads, the signed-transaction send path with its
>   nonce gathering and nonce-increase wait, and the RPC error
>   classification. Quantity: seven source files per row, fourteen file
>   states in total, plus the difference between the two rows; every
>   candidate hunk in the four chapters was read in full. Both rows were
>   confirmed to exist, and every fact bound in R35.1.7 -- R35.1.13 was
>   confirmed to hold at both.
> - *Method.* By reading the reference and the candidate, combined with a
>   mechanical pattern scan of the candidate diff for commit identifiers,
>   pull-request numbers, corpus path fragments, source-file suffixes and
>   private-identifier candidates drawn from the reading.
>   `tools/clean_room_gate.py` was not used: the candidate embeds no fenced
>   code block.
> - *Measure.* 0.02 for this chapter and 0.00 for each of the chapter 17,
>   21 and 57 sentences, all below the 0.10 threshold. No quoted or
>   near-verbatim reference text, no non-wire-visible identifier, no
>   control-flow transcription and no reference string literal was found;
>   the R35.1.15 message form is Reloaded-original.

This section binds how the EVM coin layer uses the `nodes` list of R35.1.1
(and the `urls` list of the legacy `enable` route, ch. 55 §55.4) **after**
activation has accepted it: which node a JSON-RPC request goes to, when the
next node is tried, what the caller sees when every node fails, how long a
node may hold a request, and what a log line or error text may say about a
node. It applies to every request the coin layer issues on behalf of the
platform coin, its ERC-20 tokens and its NFT pseudo-coin under
`rpc_mode: "Default"`, over both the HTTP and the WebSocket transport; the
MetaMask provider path (R35.1.4a) has a single provider and is outside it.
Reloaded's node transport is HTTP-only and rejects `ws://` / `wss://` node
URLs at activation (deviation 2 in the note closing §35.1.E), so the
WebSocket parts of the rules below bind Reloaded only if a WebSocket
transport is added.
The two reference rows agree on every rule below unless a rule says
otherwise; the Reloaded-only rules R35.1.14 -- R35.1.17 are marked as such.
The labels (a) -- (e) used below are the five questions settled by the
2026-10-26 pass: (a) batch responses, (b) execution errors, (c) the scope of
stickiness, (d) the scope of the cutoff, (e) re-sending a signed transaction.

R35.1.7 **Node order at activation (G, both rows).** The configured node list
shall be put into a uniformly random order **once**, when the coin is built,
and that order shall be the starting order of R35.1.9. The order is held only
in the running coin instance: it is not persisted, and a re-activation of the
same coin draws a fresh order. A token activated against the platform
(`erc20_tokens_requests`, `enable_erc20`, `task::enable_erc20`) starts from a
copy of the platform's list in the platform's then-current order; afterwards
the token's order evolves on its own under R35.1.9. *Reloaded divergence
(D35.8 (i)):* a Reloaded token **shares** the platform's live order instead,
so a node the platform has demoted is demoted for its tokens too; this is
observable only in which node receives a token's requests.

R35.1.8 **Per-request failover (G, both rows).** For every request other than
the signed-transaction broadcast of R35.1.12, the nodes shall be tried one at
a time in the current order, and the request ends with the answer of the
**first** node that returns a decodable, successful JSON-RPC result. Each of
the following outcomes is a **node failure** and advances the request to the
next node:

- the connection cannot be made, or the transport fails while sending or
  receiving;
- the per-node cutoff of R35.1.10 elapses;
- the HTTP status is not a success (2xx) status;
- the body is not a decodable JSON-RPC response, **or** decodes to a batch
  envelope where a single response was requested (item (a) of the 2026-10-26
  question list: the reference never issues batch requests, so a batch
  response is simply an undecodable answer; per-element semantics of a batch
  are therefore a *Reloaded choice* if batching is ever introduced, with the
  constraint that an undecodable batch envelope fails the node as a whole);
- a success (2xx) status whose body carries a JSON-RPC **error object** --
  for any method and any error code. This includes an execution revert or a
  gas-estimation failure (`eth_call`, `eth_estimateGas`; codes `3` and
  `-32000`) and a transaction-pool rejection (`eth_sendRawTransaction`
  through the per-request path, R35.1.12 last paragraph). No request kind is
  exempt, and there is no "deterministic rejection, stop here" short-circuit
  on this path (item (b), first half).

A request whose first node answers is not retried anywhere else; a subset of
failing nodes is not an error (R35.1.2 last paragraph).

R35.1.9 **Sticky last-good node (G, both rows).** When a node answers, the
list shall be **rotated** so that the answering node becomes first: the nodes
that failed before it in this request move to the tail in their previous
relative order, and the nodes after it keep their relative order behind it.
The resulting order is the starting order of the next request on the same
coin instance, is shared by every caller of that instance, and lives only as
long as the instance (item (c); see R35.1.7 for tokens and re-activation).

R35.1.10 **Per-node cutoff; no overall deadline (G, both rows).** Each node
attempt shall be bounded by a **10-second** wall-clock cutoff that covers the
whole attempt -- connection, request, reading the complete body and decoding
it. A lower-layer per-request timeout may exist but shall not be longer than
needed to be subsumed by the cutoff (the reference's lower layer uses 30 s,
which the 10 s cutoff always pre-empts). There is **no** overall deadline
beyond the sum of the per-node cutoffs: a request over *N* nodes may take up
to about *N* × 10 s before R35.1.11 applies (item (d)).

> **Informative (both rows).** The reference lets only one rotation run at a
> time per coin instance: a second request on the same instance waits until
> the first has finished its walk through the list. Whether to serialize is a
> *Reloaded choice*; whichever is chosen, the order of R35.1.9 must remain
> well defined under concurrent requests. *Reloaded divergence (D35.8 (iii)):*
> requests on one coin instance run **concurrently**. Each request walks a
> snapshot of the order and, when a node answers, stores the rotation of that
> snapshot, the last store winning; the stored order is therefore always a
> complete permutation of the nodes, and a node reaches the front only by
> answering. No lock guarding the order is held while a node is being asked.

R35.1.11 **All nodes failed (G, both rows).** When every node fails the caller
shall receive a single error that is:

- the failure of the **last node whose failure was not a cutoff** -- a
  transport failure, a non-2xx status, an undecodable body, or a JSON-RPC
  error object -- even when a later node failed by cutoff; or
- a generic "nodes unreachable" transport-class error when **every** node
  failed by cutoff.

A JSON-RPC error object that reaches the caller this way is the responding
node's error as the node sent it (code, message and any data), rendered by
the RPC client layer and carrying **no** node URL (the reference's JSON-RPC
error texts never name the node); the coin layer classes it as an
invalid-response failure, not a transport failure. So when every node answers
an `eth_call` with the same revert, the caller sees the **last** node's revert
code and message (item (b), second half). Transport-class failures (connection,
non-2xx, undecodable, cutoff) class as `Transport` for the activation rows of
R35.1.6 and ch. 55 §55.3; how each RPC maps the two classes onto its
`error_type` set is bound by that RPC's chapter.

> **Reloaded divergence (coin-layer class, maintainer decision 2026-10-26).**
> Reloaded keeps the two classes apart up to its RPC client layer -- a
> JSON-RPC error object reaches the caller as the client's error-response
> kind, a transport-class failure as its transport kind -- but its EVM coin
> layer then reports both as a transport failure, as it did before this
> section. The coin-layer distinction is not made because a wait loop of the
> swap path retries a read only on a transport failure and returns at once on
> any other, so classing a node's JSON-RPC error differently would turn a
> retried read into an immediate failure. No RPC's `error_type`, `error_data`
> shape or HTTP status differs: the `withdraw`, trade-preimage and balance
> error mappings already report an invalid-response failure as `Transport`.

R35.1.12 **Signed-transaction broadcast (G, both rows).** A transaction the
coin signs itself (swap payments, spends, refunds, approvals, `withdraw`) is
**not** broadcast through R35.1.8. Instead:

1. **Nonce.** The pending nonce of the sending address
   (`eth_getTransactionCount` with the `pending` block tag) is requested from
   **every** node concurrently. Node failures are logged, not returned. If no
   node answers, the round is repeated after one second, up to five more
   times, before the send fails. The highest nonce wins, and the set of nodes
   that reported it is remembered. *Reloaded (informative):* each node is
   asked over its own single-node connection bounded at 30 s, the bound of
   step 2, not the R35.1.10 cutoff; the rule above binds no per-node bound for
   this step.
2. **Send.** `eth_sendRawTransaction` is sent **concurrently to every node of
   that set**, each with the lower-layer per-request timeout (30 s in the
   reference) and without the R35.1.10 cutoff or the R35.1.9 rotation. The
   broadcast succeeds as soon as **one** of them accepts; it fails only when
   all of them fail, with the last failure's text. There is no re-send to a
   further node after an acceptance, so a node that answers "already known",
   "nonce too low" or "replacement underpriced" because a sibling node took
   the transaction first does not fail the broadcast (item (e), first half).
3. **Wait.** After acceptance the sender polls step 1 every second until the
   reported pending nonce exceeds the one it used; this wait has no bound in
   the reference (a `Code-quality finding` is recorded below).

A caller-supplied raw transaction submitted through the `send_raw_transaction`
RPC is **not** a signed-by-the-coin send: it is broadcast through R35.1.8, so a
JSON-RPC rejection from the first node leads to a submission to the next node,
and when every node rejects it the caller sees the last node's rejection
(item (e), second half).

> **Code-quality finding (informative; source: the reference, both rows).**
> The nonce-increase wait of step 3 loops without an upper bound while the
> nodes keep failing or keep reporting the old nonce; a transaction dropped by
> every pool after acceptance stalls the sending step forever. Proposed fix:
> bound the wait (for example by the coin's swap-transaction confirmation
> budget) and surface a distinct timeout failure so the swap state machine can
> act. Reloaded's own send path has the same shape and is covered by the same
> proposal.

R35.1.13 **Token-metadata reads (informative; Reloaded choice).** In both
reference rows the reads of an ERC-20 contract's `symbol` and `decimals`
(`get_token_info`, and the on-chain decimals read of R35.1.2 phase 5) do not
use R35.1.8: they pick one node -- the first whose most recent request did
not fail, else the first that answers a client-version probe within 30 s --
and issue the contract call to it with no further failover on that call.
Reloaded may route these reads through R35.1.8 instead, which is a strict
superset in tolerance and changes no wire shape. *Reloaded divergence
(D35.8 (iv)):* Reloaded does: both reads go through R35.1.8.

> **Logging and redaction in the reference (informative, both rows).** A
> per-node failure is logged at debug level naming the request's method; a
> cutoff is logged the same way. Nothing is redacted: a transport-class error
> text (cutoff at the lower layer, non-2xx status, undecodable body) carries
> the node's **full URL**, including any path segment or query string that
> embeds an API key, and so does the all-failed error of R35.1.11 when it is
> such a failure. These texts are not bound (binding scope, R36).

R35.1.14 **Host-only node naming (Reloaded; clean-room original; no switch).**
Every log line and every error text the EVM coin layer emits that names a
node shall render the node **host-only**: scheme, host and, when present,
port -- `https://rpc.example.org:8545` -- with the path, query, fragment and
userinfo stripped. A configured URL that cannot be parsed into a scheme and a
host is never echoed: it is named by a fixed placeholder, and a log line under
R35.1.16 adds its position in the list, as for Electrum servers and
lightwalletd endpoints (maintainer decision 2026-10-29, chapter 39 D39.11.m).
This covers at least: the activation-time node probe and
its per-node failure logs; the per-node failure and cutoff logs of R35.1.8 and
R35.1.10; the all-failed error of R35.1.11, which shall list the hosts tried
and nothing more of their URLs; the nonce and broadcast logs of R35.1.12; and
any lower-layer error text (HTTP client, TLS, DNS, WebSocket) that is embedded
in an emitted text -- a full URL arriving from a lower layer shall be replaced
by its host-only form before the text is logged or returned. The rule is not
limited to the transport module: an activation log that prints the URL it is
about to probe is in scope. The naming part of this rule applies to TRON node
URLs too (ch. 21 §21.9 R-N1); TRON requests do not go through the EVM
transport, so R35.1.8 -- R35.1.12 do not apply to them. This is a
deliberate divergence from the reference with **no opt-back switch**: node
access is not peer-visible and the change only improves operator security, so
by the second arm of the default-side rule (`docs/COMPAT_SWITCHES.md`) the
Reloaded form is the default, and an opt-in to print credentials would be an
opt-in to weaken security, which this repository's contributor rules bar
(precedent: D35.1). The divergence is to be recorded in
`docs/GLEEC_COMPATIBILITY.md` with "none -- no switch exists".

The same host-only rendering applies to the URLs of the EVM fee-estimation
services the coin layer contacts -- the `gas_station_url` of R35.1.1 and the
URL of the configured gas-API provider -- in every log line and error text
that names one, including lower-layer texts embedded in them, because such a
URL carries a provider API key just as a node URL does (Reloaded;
clean-room original; maintainer decision 2026-10-26; no switch, for the same
reason). This sentence binds only Reloaded's own texts and states nothing
about the reference's.

> **Pointer (2026-10-28).** The maintainer decision of 2026-10-26 extends
> host-only naming to every node family. The host-only form defined above is
> the form those rules use: TRON ([chapter 21](21-tron-integration.md) §21.9
> R-N1), Tendermint ([chapter 36](36-tendermint-v2-activation-rpcs.md)
> §36.5A), Electrum servers and the native UTXO daemon
> ([chapter 38](38-utxo-coin-maintenance-and-features.md) §38.6.9),
> lightwalletd ([chapter 39](39-zcash---z_coin-shielded-coin.md) §39.11.6),
> Solana ([chapter 40](40-solana-coin.md) §40.5A) and the Sia wallet daemon
> ([chapter 20](20-siacoin-integration.md) §20.8). R35.1.17 adds the rule on
> credentials in renderings, and the scope limit for the RPC wire dump that
> all of these rules share.

R35.1.15 **Naming the node that answered with a JSON-RPC error (Reloaded;
clean-room original).** When a node answers a request with a JSON-RPC error
object (R35.1.8 last bullet), the coin layer shall log one line at **warn**
level of the form `EVM RPC <host> answered <method> with error <code>:
<message>`, where `<host>` is the R35.1.14 host-only form, `<method>` the
JSON-RPC method string, and `<code>` / `<message>` the node's error code and
message. When a JSON-RPC error object is what reaches the caller under
R35.1.11, the error message returned to the caller shall end with
` (node <host>)` naming the node whose error it is, so that a revert reported
by one of several nodes can be attributed. The appended fragment is part of
the human-readable message only; it changes no `error_type`, `error_data`
shape or HTTP status.

R35.1.16 **Coin and list position in node log lines (Reloaded; clean-room
original; maintainer instruction 2026-10-28).** Every log line of the
R35.1.14 scope that names an EVM node shall also say which coin's node it is
and where that node stands in the coin's list:

1. **Coin.** The line starts with `[<ticker>]`, the ticker of the coin whose
   activation request supplied the node list (`nodes` of the v2 route, `urls`
   of the legacy `enable` route). An ERC-20 token activated on its platform
   has no node list of its own: a line about a request it makes names the
   platform, because the platform's list is the one the node belongs to.
2. **Position.** Right after the host-only form of R35.1.14 the line carries
   ` (node <i> of <n>)`, where `<n>` is the length of that list and `<i>` the
   node's 1-based position in it **as the request gave it**, not its place in
   the order drawn at activation (R35.1.7) or in the current rotation
   (R35.1.9). Two nodes on one host with different paths, which R35.1.14
   renders alike, are told apart by their positions.

This covers the activation-time probe's per-node lines, the per-node failure
and cutoff lines of R35.1.8 and R35.1.10, the warn line of R35.1.15 and the
per-node lines of the nonce gathering and the broadcast of R35.1.12; a
per-node line of R35.1.12 that so far named no node shall name it in this
form. Examples:

- `[ETC] Couldn't get client version for node https://rpc.example.org (node 2 of 4), <reason>`
- `[ETC] EVM RPC https://rpc.example.org (node 2 of 4) answered <method> with error <code>: <message>`

The rule binds log lines only. The texts returned to a caller -- the
all-failed error of R35.1.11 and the ` (node <host>)` suffix of R35.1.15 --
are unchanged, because the request that receives them already names the
coin. The RPC wire dump, which has its own switch, is outside this rule.
TRON, which is off in this beta, is not bound by it; where TRON code shares
the EVM transport, its lines take the same form. A log line is not
peer-visible and no wire shape changes, so no switch exists, for the reason
given in R35.1.14.

> **Closed divergence (2026-10-26).** The gaps described below are closed:
> Reloaded now follows R35.1.8 -- R35.1.12 and R35.1.14 -- R35.1.15, with the
> D35.8 choices and the two Reloaded notes of this section. It also bounds a
> node exchange that the cutoff abandons, so the abandoned exchange no longer
> stays open in the background. The paragraph that follows records the
> behavior before the fix.
>
> **Reloaded divergence (2026-10-26, before the fix).** Reloaded put the node
> list into random order once at activation (R35.1.7 -- parity) and probed
> each node with a client-version request before use (R35.1.6, the documented
> `Transport` case). Thereafter it diverged from R35.1.8 -- R35.1.11: on every
> request the nodes are tried in a **fixed** order with no rotation on
> success; the **first success (2xx) body is accepted even when it carries a
> JSON-RPC error object**, so a node that reverts or rejects is never failed
> over; each node attempt is allowed **60 s**; and when every node fails the
> error text returned to the caller lists every node's **full URL** with its
> failure, as does the activation probe's per-node log. The signed-transaction
> send asks every node for the pending nonce as R35.1.12 step 1 does, with the
> same six rounds when no node answers, but it accepts a round only when
> every node that answered reports the **same** nonce; while they disagree it
> logs the two extremes and asks again every second without a bound, so one
> lagging node holds the send indefinitely instead of the highest nonce
> winning. It then broadcasts through the same fixed-order path instead of
> concurrently to the highest-nonce nodes (step 2). The shuffle and the probe
> were already in place.

> **Code-quality finding (informative; source: this repository, trigger:
> contradiction between a comment and the code it documents).** The module
> comment of Reloaded's multi-URL EVM transport states that its error strings
> never carry RPC URLs, while the all-failed error that module builds embeds
> every configured URL together with that node's failure text. R35.1.14
> resolves the contradiction in favor of the comment, and the rewritten
> module now keeps to it.

R35.1.17 **Credentials are never rendered (Reloaded; clean-room original;
maintainer decision 2026-10-26; no switch).** No rendering of a value shall
print a credential. A rendering is the value's debug form, its display form,
and any log line, error text or status message built from either. A credential
is:

- a password, API key or token held in a configuration value, a client or a
  node entry;
- the value of an HTTP `Authorization` header, or of any other request header
  that the configuration supplies;
- the userinfo, path, query or fragment of a node URL or a service URL.

A URL inside a rendering appears in the host-only form of R35.1.14. A
rendering may say that a secret is set (for example `password: <set>`), but it
shall show no part of it. The rule binds every type that holds such a value --
a transport, a node entry, an activation request, a configuration object --
whether or not anything renders that type today. It covers a type defined by a
dependency as soon as Reloaded renders that type or a value that contains it.
For the EVM coin layer this means the node list, the transport that holds it,
and the fee-estimation service URLs of R35.1.14's second paragraph.

*Scope shared with R35.1.14 and the rules that point to it.* These rules do
not govern the RPC wire dump (`docs/DEV_BUILD.md` §4.1). The dump is a
debugging build feature, off by default, and keeps its own secrets switch
(`MM2_RPC_WIRE_DUMP_SECRETS`); what it redacts is that feature's own
contract (maintainer decision 2026-10-26). No other output is exempt.

*URLs in the redacted wire dump (maintainer decision 2026-10-28, D35.10).* In
the dump's redacted mode, the default whenever the dump is on, every URL with
a scheme and a host inside a dumped string value -- a whole value or a URL
inside a longer text -- shall keep its scheme, host and port. Its userinfo,
path, query and fragment shall each be replaced by the fixed marker
`<redacted>`, so the dump still shows which of these parts the URL had. A path
of `/` alone is kept. Nothing else in the dump changes for this rule: every
other member, such as the coin ticker, the method, amounts and flags, is
dumped as before, and so is a server address without a scheme (an Electrum
`host:port`). With `MM2_RPC_WIRE_DUMP_SECRETS=1` the dump shows URLs in full.

The divergence has no opt-back switch, for the reason R35.1.14 gives: it
reaches no peer and only protects the operator's credentials, so by the
second arm of the default-side rule (`docs/COMPAT_SWITCHES.md`) the Reloaded
form is the default, and an opt-in to print credentials would be an opt-in to
weaken security. It is recorded in `docs/GLEEC_COMPATIBILITY.md` with
"none -- no switch exists".

> **Status (2026-10-28).** Implemented. The EVM transport, its node entries,
> the activation request and the fee-service URLs have debug forms that show
> URLs host-only and secrets as set (T35.36). The RPC wire dump marks the
> removed URL parts in its redacted mode (T35.35).
>
> *Shared HTTP driver (2026-10-29, PR #165).* An HTTP request that finishes
> after its caller has stopped waiting is logged once, at the normal level. The
> line names the host-only form of the request URL and how the request ended:
> its own time bound was reached, an answer arrived and was discarded, or it
> failed. A failure is named only by its kind, never by its error text,
> because the transport's error text carries the full URL. The line used to
> name nothing. This is the usual trace of an EVM node that hit the failover
> cutoff, since the transport's own line for that attempt is at debug level.
> The `mm2_net` tests check the line for a URL carrying userinfo, a key in the
> path and a token in the query.
>
> *Errors of the shared HTTP layer (2026-10-29, PR #165).* Every error the
> shared HTTP layer (`mm2_net`) builds names its request URL in the host-only
> form, on the native and the wasm target. Every text built from such an error
> follows, as does every error type that copies it: NFT providers, the swap
> aggregator API, the market-maker price service, the Telegram notifier, the
> gRPC-web client, Tendermint RPC on wasm and the gas station. An error text a
> lower layer produces for the request has the URL removed as well. Before,
> such errors rendered the full URL, path and query included. The Telegram
> notifier's URL carries its bot token, though no caller shows its errors
> today. The tests use a URL carrying userinfo, a key in the path and a token
> in the query, plus a Telegram-shaped URL.

---

## 35.2 `enable_erc20` -- single token on an active platform (and `enable_nft`)

R35.2.1 The public RPC `enable_erc20` shall activate a single ERC-20 token
against an already-active EVM platform coin. Its `params` shall carry:

| Field | JSON type | Required / default | Basis | Effect |
| --- | --- | --- | --- | --- |
| `ticker` | string | required | D | Token ticker. |
| `activation_params` | object | required (may be `{}`) | D/G | Per-token parameters. Absent → `InvalidRequest` (G). |
| `activation_params.required_confirmations` | integer | default: token config value, else the platform's effective value | D | Swap confirmations for the token. |
| `protocol` | coin-protocol object (§35.5) | optional | G | Inline protocol for a custom token (R35.2.7). |

R35.2.2 Token construction: the platform binding and contract address come from
the token's config protocol (or the inline `protocol`); `decimals` comes from the
token config and, when the config lacks it or declares `0`, is read from the
contract on-chain over the platform's nodes. The configured `contract_address`
must be a 0x-prefixed address in exact EIP-55 checksummed form.

R35.2.3 The success `result` shall carry:

| Field | JSON type | Content | Basis |
| --- | --- | --- | --- |
| `balances` | object (map) | address → BalanceInfo `{spendable, unspendable}` of the token. On a single-address platform: exactly one entry, the platform's wallet address. On an HD platform: one entry per known address of the platform's HD wallet (the same addresses the `task::enable_erc20` report lists), see R35.2.5 | D |
| `platform_coin` | string | the platform ticker | D |
| `token_contract_address` | string | the token contract address, 0x-prefixed **lower-case** hex | D |
| `required_confirmations` | integer | the token's effective swap confirmations | D |

There is no `get_balances` option on this method; the balance is always read.

In HD mode `balances` carries one entry per known address; the result object has no other shape change. The balance of every address is read before the call answers: unlike `task::enable_erc20` there is no task channel, so no progress is reported and a wallet with many addresses simply makes the caller wait.

> **Upstream divergence (informative).** `v3.0.0-beta` renders
> `token_contract_address` in EIP-55 checksummed (mixed-case) form;
> `v2.6.0-beta` and the public reference example use lower-case hex. The public
> example is uncontradicted and governs. For a TRC-20 token under a TRX platform
> the reference version renders the TRON base58 form (ch. 21).

R35.2.4 `enable_nft` is dispatched to the same single-token handler. The handler
selects the token kind from the resolved protocol: an ERC-20 (or TRC-20)
protocol takes the ERC-20 parameters above; an NFT protocol takes
`activation_params.provider` and returns `{nfts, platform_coin}` -- request and
result shapes, the provider descriptor, and the NFT activation behaviour are
bound by ch. 19 §19.6.2. The **wire error contract of `enable_nft` is the
single-token contract of R35.2.5**, with the NFT-specific failure mapping of
R35.2.8.

R35.2.5 The `error_type` set, failure mapping, `error_data` shape and HTTP status
of `enable_erc20` (and `enable_nft`) shall be exactly the following; all are
mmrpc-level.

| `error_type` | Failure class | `error_data` | HTTP | Basis |
| --- | --- | --- | --- | --- |
| `TokenIsAlreadyActivated` | `ticker` already active, or lost a concurrent registration race | string: the token ticker | 400 | D |
| `TokenConfigIsNotFound` | no config entry and no inline `protocol` | string: the token ticker | 400 | D |
| `TokenProtocolParseError` | token config `protocol` missing or unparseable | object `{ticker: string, error: string}` | 500 | G |
| `UnexpectedTokenProtocol` | protocol is not ERC-20 / TRC-20 / NFT, or the ERC-20 `contract_address` is not in exact EIP-55 form | object `{ticker: string, protocol: object}` | 400 | G |
| `CustomTokenError` | inline `protocol` rejected (R35.2.7) | externally tagged custom-token sub-error (R35.2.7) | 400 | G |
| `PlatformCoinIsNotActivated` | the platform named by the token protocol is not active | string: the **platform** ticker | 400 | D |
| `UnsupportedPlatformCoin` | the platform is active but is not an EVM platform coin | object `{platform_coin_ticker: string, token_ticker: string}` | 500 | G |
| `UnexpectedDerivationMethod` | the platform is HD-derived under the hardware policy; or, for `enable_nft`, any HD-derived platform (HD mode or hardware policy); the one-shot call needs a single address. For an ERC-20 token on a **software** HD platform (HD mode) this error is returned only when `erc20_hd_activation` is `false` (below) | the serialized derivation sub-error: the string `"ExpectedSingleAddress"` for this case | 500 | G |
| `Transport` | on-chain decimals read or token balance read got no usable answer from the nodes (R35.1.6 conditions); NFT provider unreachable | string | **500** | G |
| `InvalidPayload` | NFT provider rejected the request; an inventory entry lacks its contract type | string | 400 | G |
| `PrivKeyPolicyNotAllowed` | reserved (as R35.1.5) | string | 500 | G |
| `PlatformCoinMismatch` | reserved on this method | absent | 400 | G |
| `CouldNotFetchBalance` | reserved (balance failures are reported as `Transport`) | string | 500 | G |
| `InvalidConfig` | reserved | string | 500 | G |
| `Internal` | token kind and parameters disagree (an ERC-20 ticker with a well-formed `activation_params.provider`, an NFT ticker without a usable one); NFT chain unsupported; other unexpected failures | string | 500 | G |

> **Upstream divergence: `enable_erc20` on a software HD platform (HD mode only, not hardware policy).** The reference
> refuses an HD-derived platform on the one-shot ERC-20 call with
> `UnexpectedDerivationMethod`, so a client that only knows `enable_erc20`
> cannot add a token after startup in HD mode. Reloaded activates the token
> instead. A token is enabled on the enabled address of its platform
> (`activation_params.path_to_address`, when given, is only validated against
> that address and refused with `Internal` when it selects another), so the
> one-shot call has no ambiguity to resolve; it reports every known address
> in `balances` (R35.2.3) and registers exactly the coin `task::enable_erc20`
> registers. Single-address platforms, `enable_nft` and every other row above
> are unchanged.
>
> **Which half of the old guard survived.** The reference's guard covered two
> cases, HD mode and hardware policy. Only HD mode is lifted. A
> hardware-backed platform is still refused on the one-shot call, exactly as
> before (identical `error_type`, `error_data` and status), so there is nothing
> divergent to document for it: the task variant exists partly to provide
> `user_action`, the channel on which a hardware device confirms, and a
> one-shot call has none, so admitting a hardware platform would hang or fail
> obscurely. `erc20_hd_activation` has no effect on hardware platforms.
>
> **Per-setting documentation, `erc20_hd_activation`** (`MM2.json` boolean,
> default `true`; not acknowledgement-gated). Only an explicit `false`
> disables the HD support and restores the refusal described above, with the
> identical `error_type`, `error_data` and HTTP status; any other value, or
> its absence, leaves it on.
> **Compatibility:** for behaviour matching GLEEC KDF (and the upstream
> pre-divergence Komodo DeFi Framework), set this to `false`.

Note the deliberate difference from R35.1.5: `Transport` on this method is HTTP
**500**, not 502, and `TokenProtocolParseError` is 500 while
`UnexpectedTokenProtocol` and `CustomTokenError` are 400 (G).

R35.2.6 The checks of `enable_erc20` shall be applied in this order: already
active → config/protocol (or custom-token validation) → platform active →
platform is EVM → token construction (custom-token duplicate-activation check,
decimals) → single-address requirement → balance read → registration. A failure
shall leave the token unregistered and shall not register the token's info on
the platform.

R35.2.7 **Custom tokens (G).** When `protocol` is supplied (here, or on an
`erc20_tokens_requests` entry of §35.1), the token is activated from that
protocol instead of from the coins config, as a wallet-only coin. It is rejected
with `CustomTokenError` when: the ticker already exists in the coins config;
the contract address is already configured under another ticker on the same
platform; a token with the same platform and contract address is already active;
or the contract address cannot be parsed. The `error_data` of `CustomTokenError`
is the externally tagged sub-error:

| Case | `error_data` |
| --- | --- |
| ticker already in config | `{"DuplicateTickerInConfig": {"ticker_in_config": string}}` |
| contract already configured | `{"DuplicateContractInConfig": {"ticker_in_config": string}}` |
| same contract already active | `{"TokenWithSameContractAlreadyActivated": {"ticker": string, "contract_address": string}}` |
| unparseable contract address | `"InvalidTokenAddress"` |

> **Reloaded status (informative).** Implemented (kdf-reloaded/kdf#16): a
> custom token reports itself wallet-only on both activation routes, so order
> placement (`buy`, `sell`, `setprice`) rejects it like any other wallet-only
> coin. Tests assert that a custom token is wallet-only and a configured token
> is not, and that order placement rejects a wallet-only coin. Remaining: a few
> order-book read paths (trade preimage, best orders, order book and its depth
> and events) test wallet-only status by looking the ticker up in the coins
> configuration, which a custom token does not have; they do not place orders,
> so no swap can start, but they can list or price a custom-token pair.

R35.2.8 **`enable_nft` failure mapping (G).** Against the single-token table:
an NFT ticker with no config entry → `TokenConfigIsNotFound`; NFT pseudo-coin
already active → `TokenIsAlreadyActivated` (string, the NFT ticker); backing
platform not active → `PlatformCoinIsNotActivated` (string, the platform
ticker); backing platform not EVM → `UnsupportedPlatformCoin`; platform
HD-derived → `UnexpectedDerivationMethod`; unsupported chain → `Internal`;
`activation_params` without a usable `provider` (absent, an unknown provider
`type`, or a syntactically invalid `url`) → `Internal` (500), not a
request-parse failure; provider unreachable / non-success / unparseable → `Transport` (500); provider
rejected the request → `InvalidPayload`.

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`:
> custom tokens, the token-construction mapping, lower-case
> `token_contract_address`, the token-config confirmation fallback, token
> registration on the platform only after success, the full error set, and
> `enable_nft` served by the single-token handler; custom tokens are wallet-only
> (R35.2.7). Remaining: a provider rejection maps to `InvalidPayload` only for an
> indexer HTTP 400.

---

## 35.3 `task::enable_eth::{init,status,user_action,cancel}` -- task variant

R35.3.1 EVM platform activation shall also be exposed as the task family
`task::enable_eth::init`, `task::enable_eth::status`,
`task::enable_eth::user_action`, `task::enable_eth::cancel`.

- `init` `params`: exactly the `enable_eth_with_tokens` fields of R35.1.1, flat
  in `params`, plus the optional task-framework `client_id` (ch. 55 §55.2).
  `priv_key_policy` is the **object** of R35.1.4; the reference's task-page table
  types it as a string, but a string is a request-parse failure in the reference
  version (G). All signing policies are accepted, including `Trezor`.
- `init` success: `{"task_id": <integer>}`, returned before activation
  completes.
- `status` terminal success: the `enable_eth_with_tokens` result of R35.1.3,
  byte-for-byte (single-address or HD shape, `nfts_infos` always present), in
  `details` of `{"status":"Ok","details":...}`.
- `user_action` and `cancel`: ch. 55 §55.2.

R35.3.2 **Trezor path.** Under `priv_key_policy: {"type":"Trezor"}`:

- a hardware-wallet context must already exist (established by
  `task::init_trezor`, ch. 55); the coin config must declare its hardware coin
  name (`trezor_coin`) and a usable `derivation_path`;
- the address and account public key are read from the device; the platform is
  HD-derived and the terminal result is the HD shape of R35.1.3(b) for the
  account/address selected by `path_to_address`;
- device interaction surfaces through `status` as the in-progress and awaiting
  values of R35.3.3, and PIN / passphrase are supplied through
  `task::enable_eth::user_action` with
  `{"action_type":"TrezorPin","pin":"<pin-matrix response>"}` or
  `{"action_type":"TrezorPassphrase","passphrase":"<passphrase>"}` (shared
  hardware user-action contract, ch. 55 / ch. 50).

Failure mapping (status-level, G): no hardware-wallet context, a coin config
without hardware support, a device error, a device-interaction timeout, or a
user action of the wrong kind (a passphrase while a PIN is awaited, or the
reverse; `user_action` itself still answers `"success"`, ch. 55 R55.2.11) →
`Internal` (string); an unusable `derivation_path` → `InvalidPayload` (string).

R35.3.3 **In-progress and awaiting values.** `status` with
`"status":"InProgress"` carries `details` as one of the strings
`ActivatingCoin`, `RequestingWalletBalance`, `Finishing`,
`WaitingForTrezorToConnect`, `FollowHwDeviceInstructions` (D: the reference's
task page lists exactly these). The initial value is `ActivatingCoin`. A device
button/confirmation request is reported as `FollowHwDeviceInstructions`. With
`"status":"UserActionRequired"`, `details` is the string `EnterTrezorPin` or
`EnterTrezorPassphrase`. Clients shall treat any in-progress value as progress
and keep polling; the implementation may omit phases that do not apply
(the reference version reports only `ActivatingCoin` plus the hardware values
for this family). The framework imposes no activation deadline of its own (ch. 48
§48.3).

R35.3.4 (Moved.) The single-token task family is specified in §35.3A.

R35.3.5 **Error placement (G).**

| Placement | `error_type` | Condition | `error_data` | HTTP |
| --- | --- | --- | --- | --- |
| `init`, mmrpc-level | `InvalidRequest` | request-parse failure | string | 400 |
| `init`, mmrpc-level | `PlatformIsAlreadyActivated` | `ticker` already active when `init` is called | string: ticker | 400 |
| `init`, mmrpc-level | `Internal` | the task could not be created | string | 500 |
| `status`, task-level (`"status":"Error"`, full error object in `details`) | every row of R35.1.5 except `UnexpectedDeviceActivationPolicy` (`PlatformIsAlreadyActivated` here only for a registration race after `init`) | as R35.1.5 (including the backend mapping of R35.1.6 and the Trezor mapping of R35.3.2) | as R35.1.5 | the `status` response itself is HTTP 200 |
| `status` / `user_action` / `cancel`, mmrpc-level | task-framework errors | ch. 55 §55.2 | ch. 55 §55.2 | ch. 55 §55.2 |

In particular, platform config, protocol, node, token and NFT failures are
**not** reported synchronously by `init`; they appear as the task's terminal
error.

> **Public-docs erratum (informative).** The reference's task page lists
> `TaskTimedOut`, `CoinCreationError` and `HwError` as the possible terminal
> errors and describes a `details.result` wrapper; the reference version reports
> the platform discriminants of R35.1.5 directly in `details` (hardware failures
> as `Internal`), and never `CoinCreationError` or `HwError` for this family.

R35.3.6 The `task::enable_eth` family is delivered by the shared platform-coin
task-activation framework of [Chapter 48](48-platform-coin-task-activation.md),
whose unit of work is the activation of §35.1 (one activation path for one-shot
and task). Cancellation follows ch. 48 §48.7 and ch. 55 §55.2.

> **Reloaded status (informative).** Implemented on `fix/activation-api-parity`:
> the synchronous already-active check, the Trezor failure mapping to
> `Internal`, `FollowHwDeviceInstructions`, and the HD shape for the Trezor
> result. The §35.1 remaining deviations apply. A real Trezor flow is covered
> only by the emulator-gated tests.

---

## 35.3A `task::enable_erc20::{init,status,user_action,cancel}` -- token task variant

R35.3A.1 The task family `task::enable_erc20::init`, `::status`,
`::user_action`, `::cancel` shall exist on all targets as the task variant of
`enable_erc20`. It is delivered by the token-task substrate of
[Chapter 48](48-platform-coin-task-activation.md) (§48.11) and its framework
methods follow ch. 55 §55.2. (G for every rule of §35.3A: the public
reference names `task::enable_erc20::init` only in its changelog and documents
no request, result or error; the error catalogue lists its error enum by name.)

R35.3A.2 `init` `params`:

| Field | JSON type | Required / default | Effect |
| --- | --- | --- | --- |
| `ticker` | string | required | Token ticker. |
| `protocol` | coin-protocol object | optional | Custom token (R35.2.7). |
| `activation_params` | object | required (may be `{}`) | |
| `activation_params.required_confirmations` | integer | as R35.2.1 | |
| `activation_params.path_to_address` | object | default `{account_id:0, chain:"External", address_id:0}` | HD: enabled address of the token. |
| `activation_params.scan_policy` | string | default `scan_if_new_wallet` | HD scan policy. |
| `activation_params.min_addresses_number` | integer | optional | HD minimum addresses. |
| `client_id` | integer | optional (ch. 55 §55.2) | Task-event routing. |

`init` success: `{"task_id": <integer>}`.

R35.3A.3 `init` mmrpc-level errors (checked before the task is spawned, in this
order):

| `error_type` | Condition | `error_data` | HTTP |
| --- | --- | --- | --- |
| `InvalidRequest` | request-parse failure | string | 400 |
| `TokenIsAlreadyActivated` | `ticker` already active | **object** `{ticker: string}` | 400 |
| `TokenConfigIsNotFound` | no config and no inline `protocol` | string: ticker | 400 |
| `TokenProtocolParseError` | config protocol missing/unparseable | object `{ticker, error}` | **400** |
| `UnexpectedTokenProtocol` | not an ERC-20 (or TRC-20) protocol, or contract address not in exact EIP-55 form | object `{ticker, protocol}` | 400 |
| `CustomTokenError` | inline `protocol` rejected | as R35.2.7 | 400 |
| `PlatformCoinIsNotActivated` | platform not active | string: platform ticker | 400 |
| `UnsupportedPlatformCoin` | platform active but not EVM | object `{platform_coin_ticker, token_ticker}` | 500 |
| `Internal` | task could not be created | string | 500 |

Note that this family's already-activated `error_data` is an object, unlike the
string of the one-shot method, and its protocol-parse status is 400, unlike the
500 of the one-shot method.

R35.3A.4 In-progress values (strings): `ActivatingCoin` (initial),
`RequestingWalletBalance`, `Finishing`, `WaitingForTrezorToConnect`,
`FollowHwDeviceInstructions`; awaiting values `EnterTrezorPin`,
`EnterTrezorPassphrase`, answered through `user_action` with the hardware
user-action payloads of R35.3.2. The hardware values occur only when the
platform was activated under the `Trezor` policy.

R35.3A.5 Terminal success `details`:

| Field | JSON type | Content |
| --- | --- | --- |
| `ticker` | string | the token ticker |
| `platform_coin` | string | the platform ticker |
| `token_contract_address` | string | contract address in EIP-55 checksummed form (TRON base58 under TRX) |
| `current_block` | integer | current chain height |
| `required_confirmations` | integer | effective swap confirmations |
| `wallet_balance` | object | `{"wallet_type":"Iguana","address":string,"balance":{<token ticker>: BalanceInfo}}` for a single-address platform, or the HD report of R35.1.3(b) whose balance maps contain the token ticker only |

Unlike the one-shot `enable_erc20`, this family works on an HD-derived platform.

R35.3A.6 Terminal (status-level) errors:

| `error_type` | Condition | `error_data` |
| --- | --- | --- |
| `Transport` | on-chain decimals read or chain-height read got no usable answer (R35.1.6 conditions); also an invalid-payload class failure during token construction | string |
| `CouldNotFetchBalance` | balance enumeration got no usable answer | string |
| `HwError` | the platform uses the `Trezor` policy but no hardware-wallet context is available | string: the hardware error variant name (e.g. `"NotInitialized"`) |
| `TaskTimedOut` | framework-originated timeout | object `{duration: {secs, nanos}}` |
| `TokenIsAlreadyActivated` | lost a concurrent registration race | object `{ticker}` |
| `CustomTokenError` | custom-token duplicate-activation check failed | as R35.2.7 |
| `PlatformCoinMismatch` | reserved | absent |
| `Internal` | derivation or signing-policy mismatch, HD address derivation or account-creation failure, device-interaction failure, a user action of the wrong kind, other unexpected failures | string |

The `status` response carrying a terminal error is HTTP 200; the error object
sits directly in `details` (ch. 55 §55.2).

R35.3A.7 `cancel` of an in-flight token task shall undo that task's own
registration: if the cancelled task had already registered the token when
cancellation is processed, the token is removed again; a token registered by
another activation stays (ch. 55 §55.2 for the wire contract, R55.5.8 for the
scope of the cleanup).

> **Reloaded status (informative).** Routed on `fix/activation-api-parity` on
> every target, with its own registry and the full contract of §35.3A; see
> Chapter 48 §48.11 for the substrate's remaining limits.

---

## 35.4 `get_token_info` -- on-chain ERC-20 contract info

R35.4.1 The public RPC `get_token_info` shall, given a token's coin-protocol
descriptor naming its platform and contract address, return that contract's
on-chain information. Its `params` shall carry a single `protocol` object in the
coin-protocol shape of R35.5 selecting the token's platform and contract address.

R35.4.2 The platform coin named by the protocol must already be active; the call
queries the contract over the platform's EVM JSON-RPC client (ERC-20 ABI calls).

R35.4.3 The success `result` shall carry:
- an optional `config_ticker` -- the ticker under which this contract is known in
  coin configuration, when one is configured (omitted otherwise); and
- a tagged token-info object (tag field `type` with value `ERC20`, payload field
  `info`) whose `info` carries the contract's on-chain `symbol` (string) and
  `decimals` (integer).

R35.4.4 `error_type` discriminants and HTTP status shall include at least:

| `error_type` | HTTP status |
| --- | --- |
| `NoSuchCoin` | 404 |
| `UnsupportedTokenProtocol` | 400 |
| `InvalidRequest` | 400 |
| `RetrieveInfoError` | 500 |

A `protocol` that is not a token protocol, or whose platform is not an EVM
platform coin, shall be rejected as an invalid-request / unsupported-protocol
class error; a contract read that fails on-chain shall be a retrieve-info class
error.

---

## 35.5 Coin-protocol descriptor (dictated config contract)

R35.5.1 The `protocol` object used by `get_token_info` (and accepted as an
inline custom-token protocol by `enable_erc20`, `task::enable_erc20::init` and
`erc20_tokens_requests` entries) follows the project's public coin-protocol JSON
contract: a tagged object with tag field `type` and payload field
`protocol_data`. The EVM cases are:

```jsonc
// EVM platform coin protocol.
"protocol": { "type": "ETH",
              "protocol_data": { "chain_id": 1 } }

// ERC-20 token protocol.
"protocol": { "type": "ERC20",
              "protocol_data": { "platform": "ETH",
                                 "contract_address": "0x..." } }
```

The `ETH` / `ERC20` type strings, the `chain_id`, `platform`, and
`contract_address` field names, and the EVM chain id values are the dictated
public config contract; the Rust types that deserialise them are discretionary.
`chain_id` is a required integer for the EVM platform protocol (R35.1.2 phase 3).
A configured ERC-20 `contract_address` is accepted only in exact EIP-55
checksummed form (R35.1.5 `UnexpectedTokenProtocol`). The TRON cases (`TRX`,
`TRC20`) are bound by ch. 21; the NFT pseudo-coin case (`NFT`) by ch. 19 §19.13.

---

## 35.6 Swap gas-fee policy RPCs (EIP-1559 dictated semantics)

R35.6.1 The public RPC `get_swap_gas_fee_policy` shall read the swap gas-fee
policy currently in effect for a coin. Its `params` carry `coin` (string,
required). Its `result` is the policy value of R35.6.3.

R35.6.2 The public RPC `set_swap_gas_fee_policy` shall set the swap gas-fee policy
for a coin. Its `params` carry `coin` (string, required) and `swap_gas_fee_policy`
(the policy value of R35.6.3; optional, defaulting to the legacy policy). Its
`result` echoes the policy now in effect.

> **Naming (binding).** The method strings are exactly `get_swap_gas_fee_policy`
> and `set_swap_gas_fee_policy`, and the set-request field is exactly
> `swap_gas_fee_policy`. These are **not** `*_swap_transaction_fee_policy`.

R35.6.3 The policy is a published string enum over exactly four variants:
- `Legacy` -- pre-EIP-1559 single gas-price pricing (the built-in default, used when neither
  the activation parameter nor the coin configuration names a policy);
- `Low` -- low EIP-1559 priority-fee tier;
- `Medium` -- medium EIP-1559 priority-fee tier;
- `High` -- high EIP-1559 priority-fee tier.

The same enum is accepted as the optional `swap_gas_fee_policy` field of
`enable_eth_with_tokens` (R35.1.1), where it sets the coin's initial policy.

> **Initial policy and shipped configuration (informative).** A platform coin's
> initial policy is, in order: the `swap_gas_fee_policy` activation parameter, the
> `swap_gas_fee_policy` key of its coin-configuration entry, else `Legacy`; a
> token starts with its platform's policy. The legacy `enable` / `electrum` route
> resolves the policy the same way (R35.6.7, D35.6). The coins file the
> project ships sets `"Medium"` for ETH (chain 1), POL (137 — the entry
> formerly keyed as MATIC), AVAX (43114), ETH-ARB20 (42161), ETH-BASE (8453),
> XDAI (100) and MOVR (1285), and for nine further platform entries (snapshot
> of 2026-10-18: FLR, GLEEC, GLEECT, HYPE, MNT, MON, TAO, XDC, XPL) —
> sixteen in all; so on those coins, and on
> their tokens, swap transactions are EIP-1559 (type `0x02`) unless the operator
> overrides the policy at activation or with `set_swap_gas_fee_policy`; a coin
> without the key, such as BNB, stays untyped. `Legacy` being the default is
> therefore not a statement about what most EVM swaps emit. Platform activation
> logs one INFO line naming the effective policy and which of the three sources
> set it, and the effective cap on the emitted transaction type (D35.7) and its
> source, e.g. `MATIC: swap gas-fee policy Medium (source: coin config); max tx
> type 2 (source: coin config)`.

R35.6.4 These RPCs apply only to EVM-family coins (coins whose swaps price gas via
the EIP-1559 / legacy gas model). A request for a coin that does not support the
policy shall be rejected.

R35.6.5 `error_type` discriminants and HTTP status shall include at least:

| `error_type` | HTTP status |
| --- | --- |
| `NoSuchCoin` | 400 |
| `NotSupported` | 400 |

R35.6.6 The selected policy governs how the EVM coin prices gas for subsequent
swap transactions (legacy gas price vs. an EIP-1559 max-fee / max-priority-fee
pair at the chosen tier). The mapping from tier to concrete fee values is dictated
by EIP-1559 fee-history semantics ([Chapter 56](56-evm-eip1559-fee-estimation.md)
R1–R3; its R6 requires the gas-limit estimation request of a swap step to carry
the same tier pair the transaction is signed with), not by this chapter. Which serialized
transaction form each policy value produces, and which forms the project must
read as a consequence, are bound in §35.6A (R35.6A.17, R35.6A.2).

> **Where the policy is actually consulted (cross-reference).**
> [Chapter 57](57-evm-fee-inputs.md) R57.4.1 binds the consumer side of this
> rule: every swap-side fee figure — the flat trade fee, the sender-side and
> receiver-side preimages, and the DEX-fee preimage — must be computed from the
> fee form this policy resolves to, and ch. 57 records that reloaded currently
> computes all four from the legacy gas price unconditionally, never consulting
> the policy. So on the coins the shipped configuration sets to `"Medium"`
> (named in the note after R35.6.3), the policy is honoured for the swap
> transactions this chapter's §35.6A covers while the **fee figures quoted for
> those swaps** are still legacy-priced. That is a parity gap recorded in
> ch. 57, not a property of this chapter's contract. Withdrawals are separate
> again: they never consult the policy on either side (ch. 57 R57.7.2).

R35.6.7 **The initial policy does not depend on the activation route.** The
precedence of R35.6.3 -- activation-request parameter, else the
`swap_gas_fee_policy` key of the coin's own configuration entry (a token falling
back to its platform coin's entry), else the built-in `Legacy` default -- applies
to **every** activation route for an EVM platform coin: the legacy `enable` /
`electrum` route as much as the v2 one-shot and the task family. A coin whose
configuration names a policy therefore starts on that policy however it was
activated, and two coins activated through different routes from the same
configuration emit the same serialized swap-transaction form. The request
parameter exists on the legacy route too; its wire placement there is
[Chapter 55](55-coin-activation-wire-contract.md) R55.4.4.

> **Upstream divergence (informative) -- the policy request field is parsed
> leniently on the legacy route.** Both reference versions named in §35.6A's
> derivation record resolve the initial policy with the single precedence
> R35.6.7 states, applied identically on the legacy route and on the v2 and task
> routes; the reference shows **no split** by activation method. Reloaded
> resolves it the same way on every route (decision D35.6): a coin the shipped
> configuration puts on a non-legacy tier starts on that tier however it was
> activated, so the same coin activated through `enable` / `electrum` and through
> the v2 or task routes emits the same serialized swap-transaction form. The one
> remaining difference between the routes is how a *request-parameter* value that
> names none of the four variants is treated, and it is the specified behaviour
> on both sides: the v2 and task routes reject it as `InvalidRequest` (R35.1.1),
> while the legacy route ignores it and falls through to the configured or
> built-in policy (ch. 55 R55.4.4). The two parses are deliberately separate and
> MUST NOT be unified. The divergence history is confined to the *emitting*
> side: what a build will read is fixed independently of its policy and of the
> route that activated the coin (R35.6A.19).

> **Code-quality finding (informative) -- the policy was stored but not
> applied; now implemented.** The policy is accepted by
> `set_swap_gas_fee_policy`, held per coin, and echoed by
> `get_swap_gas_fee_policy`. It used to be that no transaction-construction path
> read it: the swap signing path resolved a single gas price and built the
> untyped legacy form unconditionally, so an operator who set `High` got exactly
> the pricing and the transaction form of `Legacy` while both RPCs reported
> `High`, and the RPC round-trip tests passed because they exercised only the
> store-and-echo behaviour. The swap signing path now reads the stored policy:
> `Legacy` keeps the single-gas-price legacy form (byte-identical to before);
> `Low`, `Medium` and `High` emit an EIP-1559 (type `0x02`) transaction whose
> `max_fee_per_gas` / `max_priority_fee_per_gas` are the matching tier of the
> EIP-1559 fee estimate of [Chapter 10](10-sse-streaming.md) (one tier
> definition shared with the fee-estimation RPC and stream). A failed estimate
> fails the send; it does not fall back to the legacy form. Tests construct and
> sign the transaction for each policy and assert the emitted form, the tier's
> fee fields, and a round trip through the project's own typed decoder. Resolved
> as D35.4. Not verified against a live network send.
> *(Trigger: a contradiction between two things read side by side while
> establishing §35.6A -- a stored policy with no reader in the path the rule
> says it governs. Finding is in this repository's own code, not in any
> reference material.)*

---

## 35.6A Foreign-transaction byte-decoding contract (EIP-2718 dictated)

> **Dirty-side derivation record (sanitized).** This section was authored by the
> KDF Spec Reader under the chapter-01 two-team workflow. The transaction
> framing, payload layout, signing-hash construction, identifier construction and
> signature-parity rules it states are dictated by the public EIP-2718, EIP-2930,
> EIP-1559 and EIP-155 specifications and by keccak-256 / secp256k1 recovery; what
> was established through the workflow is the *acceptance set* the reference
> lineage applies to those forms, the error class a rejection takes, and the
> release designations at which the acceptance set changed. Reference versions:
> the GLEEC KDF release tags `v3.0.0-beta` (reference) and `v2.6.0-beta` (legacy
> cross-check), with earlier public release designations named in R35.6A.18 only
> as the version boundary. It carries only observable behaviour, the dictated
> wire forms, and behavioural findings; it carries no private implementation
> expression. **It passed the KDF Dirty Gate before clean-side implementation**,
> together with the §35.6 finding, the §35.7 paragraph, the acceptance criteria,
> §35.8B, §35.8C and this chapter's footer, and with the matching §17.5.1
> paragraph and footer item of [Chapter 17](17-swap-v2-evm-path.md). The edits
> made after that clearance were re-gated in a second pass, and the material a
> third Spec Reader pass added on 2026-10-16 (listed in §35.9's footer) was gated
> in a third. The *Clearance record* below records each of the three passes
> separately.

> **Clearance record (chapter 01 T1).** This material was gated in **three
> passes**, and each pass's extent, method and measure are recorded under its
> own heading below, carried forward in corpus-free terms. No pass stands in for
> another: each compared a different extent, so a later pass's elements do not
> replace an earlier one's, and a reader weighing a given section reads the pass
> that cleared it. The first pass cleared the three CRD files this work touches.
> The second re-gated the edits made after that clearance rather than accepting
> the assessment that they were mechanical; it covered five files -- the three
> CRD files plus the two operator-facing documents (`RELOADED_VS_GLEEC.md` and
> `docs/GLEEC_COMPATIBILITY.md`) the first pass had not seen -- and closed the
> gap the first pass had declared, by opening material that run could not. The
> third gated the third Spec Reader pass's additions, judged in the context of
> the whole chapter; on one axis its extent is narrower than the second's, as
> its own entry states. A further pass that gated only an earlier edit of this
> record, and took no forbidden-class input, is not counted among the three.
>
> *First pass.*
>
> - *Extent.* The EVM coin layer's decoding of a serialized transaction the
>   project did not construct, its reconstruction of a transaction from a node's
>   reply, its transaction-type model, the application of the per-coin swap
>   gas-fee policy, the derivation of fee details in transaction history, and
>   the swap-step paths that read a counterparty's serialized transaction.
>   Reference: the v3 development lineage as held locally, plus a confirmation
>   that the four published release designations this chapter names exist as
>   release tags of that lineage; the named releases were **not** each compared
>   separately. Quantity: one principal coin-layer file read in the relevant
>   regions and five further files searched for the same subject matter, six in
>   total.
> - *Method.* By reading, combined with mechanical token extraction over the
>   added lines only -- bracketed tokens, quoted literals, fenced blocks and
>   code-shaped bare words -- and targeted search over the compared material.
>   `tools/clean_room_gate.py` was not used: it measures source files, and the
>   candidate embeds no fenced code block.
> - *Measure.* 0.02 on the discretionary-expression basis, read against the
>   standard that dictated-interop and pinned-interface matches are excluded
>   from the measured set however dense they are, and that any genuine
>   discretionary expression fails regardless of the dictated content around
>   it. The figure is non-nil because three near-edge statements were each
>   examined on their own and found to carry no expression.
> - *Standing finding.* The tag-existence confirmation above was not repeated
>   by either later pass, and neither later pass disturbed it. It **stands**,
>   and it is what R35.6A.18's version boundary, D35.3's peer floor and this
>   chapter's footer locator rest on.
> - *Declared limit, retired.* That run could not open a third-party dependency
>   outside the compared material that carries part of the behaviour two of the
>   informative findings below describe, so those two findings were judged on
>   this section's own form rather than by comparison. The limit belonged to
>   that run only: the second pass compared the dependency, and the limit is
>   **not** live.
>
> *Second pass.*
>
> - *Extent.* The same subject matter as the first pass, against the same
>   lineage and with `v3.0.0-beta` and `v2.6.0-beta` as the named reference
>   points, again not compared release by release. Unlike the first pass, it
>   also compared the third-party dependency that carries the transaction-type
>   model and the two behaviours the informative findings below describe.
>   Quantity: three files of the principal implementation set -- one read in the
>   relevant regions, two consulted only for the dependency declaration -- and
>   four files of that dependency, read in the regions carrying type dispatch,
>   identifier derivation and the unsigned-signature convention; seven in total.
> - *Method.* By reading, combined with mechanical token extraction over the
>   added lines only, each extracted token put individually to the
>   forced-versus-free test, and targeted search over the compared material.
>   Every extracted token resolved to dictated or public contract: EIP-2718 type
>   bytes and RLP leading-byte markers, mmrpc error-envelope field names, public
>   RPC method strings and their serialized policy values, one trait method on
>   the relicensed base's own public surface, published release designations,
>   and paths inside this repository. The method strings and policy values were
>   checked against this repository's own source rather than assumed.
>   `tools/clean_room_gate.py` was not used, for the first pass's reason.
> - *Measure.* 0.02 on the same basis and standard, derived afresh in that pass
>   rather than inherited from the first. The non-nil figure records the
>   near-edge behavioural statements in the informative notes below and their
>   two operator-facing restatements; each was examined on its own and found to
>   carry behaviour and consequence rather than expression.
>
> *Third pass (2026-10-16).*
>
> - *Extent.* The subject matter of the third Spec Reader pass's additions and
>   the sections they bind: how an EVM coin's initial swap gas-fee policy is
>   resolved, and in what precedence between the activation request, the coin's
>   own configuration entry and the built-in default, on each of the legacy, v2
>   one-shot and task routes; the token case of that resolution and of the
>   per-coin cap on the highest transaction type the coin may send, including a
>   token's fallback to its platform coin's entry; where that cap is read, what
>   its absence means, and what happens at signing when a transaction exceeds
>   it; how a policy tier maps to fee values and so to the form a signed swap
>   transaction takes; the coin layer's byte-decode entry for a transaction the
>   project did not build, and whether it consults any coin state; the
>   reconstruction of a transaction from a node's reply and its type dispatch;
>   and, as context, the platform-coin-with-tokens and single-token activation
>   routes. Reference: the same lineage and the same two named reference points,
>   not compared release by release. Quantity: four files of the principal
>   implementation set, each read in the regions carrying that behaviour rather
>   than in full. **Narrower than the second pass on one axis:** this pass did
>   not re-open the third-party dependency the second pass compared. R35.6A.19's
>   statement that decoding is independent of coin state was checked at the coin
>   layer's own byte-decode entry, which this pass did compare, so that
>   statement was not left unexamined; the dependency itself was last compared
>   by the second pass.
> - *Method.* By reading, with the forced-versus-free test applied individually
>   to every token of the lines identified to the gate as new -- R35.6.7,
>   R35.6A.19, the two divergence notes after them, the acceptance criterion
>   citing R35.6A.19, D35.6, D35.7 and the D35.3 clause on the reference's typed emission -- and to
>   the rest of the chapter on a skim basis. Every token in the new lines
>   resolved to dictated or public contract: coin-configuration keys and their
>   value types, public RPC method strings with their request fields and
>   serialized enum values, EIP-2718 type bytes, published release designations,
>   public chain identifiers and tickers, one trait method on the relicensed
>   base's own public surface, and cross-references within this document set.
>   The one message string the chapter quotes was checked to match, and to be
>   attributed to, the public documentation rather than the compared material.
>   **Declared limitation:** mechanical text search over the compared material
>   was refused by the environment, so the gate navigated it by reading selected
>   regions directly. This pass's coverage is therefore region-based, not an
>   exhaustive search. `tools/clean_room_gate.py` was not used: the new material
>   embeds no fenced code block.
> - *Measure.* 0.02 on the same basis and standard, derived in this pass and
>   not inherited from either earlier one. The figure is non-nil because four
>   near-edge statements in the new material were each examined on their own
>   before being admitted: that emission is gated at two separate points, the
>   effective value of an absent configuration key, the mapping of two
>   configuration keys onto a named set of chains, and the statement that one
>   neighbouring configuration key is never read. Each carries behaviour, a
>   configuration-contract value or a consequence, not expression. The leak
>   report records no finding for the new lines or for any other line of the
>   chapter, so nothing failed even under the skim.

> **Why the chapter carries it.** §35.6 lets an operator put a coin on any of
> three EIP-1559 priority tiers. A peer that has done so signs its swap
> transactions as EIP-1559 envelopes, and every swap step in which we read that
> peer's bytes then depends on decoding one. A reader that handles only the
> untyped legacy form cannot complete a swap with such a peer at all, and the
> failure surfaces as an unreadable payload rather than as anything that names
> the cause. The contract below is therefore an interoperability requirement of
> the policy §35.6 already binds, not a separate feature.

### Where the contract applies

R35.6A.1 The contract binds **every** path that reads a serialized EVM
transaction the project did not itself construct. At minimum:

- the coin-layer byte-decode entry (`MarketCoinOps::tx_enum_from_bytes`);
- the V2 swap protocol's transaction parser and its preimage parser, including
  the EVM stand-in preimage that carries a serialized funding transaction
  ([Chapter 17](17-swap-v2-evm-path.md) §17.5.1);
- every V1 and V2 swap step that reads a counterparty's serialized dex-fee,
  payment, funding, spend or refund transaction before validating it, spending
  from it, refunding it, or searching for its spend -- including the watcher
  paths, which read the same bytes on behalf of a third party;
- the raw-transaction read RPC and the transaction-history fee computation,
  which reconstruct a transaction a node reported.

A decoder that serves one of these and not the others is not conformant: the
bytes reaching them come from the same counterparty.

### Accepted and rejected transaction forms

R35.6A.2 The following three forms MUST decode successfully:

| Form | Leading byte | Dictating specification |
| --- | --- | --- |
| Untyped legacy transaction | an RLP list header (`0xc0` or above) | EIP-155 (and pre-EIP-155 without the chain-id triplet) |
| Typed, access-list transaction | `0x01` | EIP-2930 |
| Typed, priority-fee transaction | `0x02` | EIP-1559 |

R35.6A.3 Every other value in the EIP-2718 reserved type range -- explicitly
including `0x03` (EIP-4844 blob transactions) and `0x04` (EIP-7702
authorization-list transactions), and also `0x00` and every unassigned value up
to `0x7f` -- MUST be rejected.

Rejecting `0x03` and `0x04` is **parity, not divergence.** The reference lineage
has no representation for a blob transaction or an authorization-list
transaction at either reference version named in the derivation record: its
transaction model admits exactly the three forms of R35.6A.2, and any other type
value is an input-decode failure. The refusal there is **explicit rather than
incidental** -- the lineage recognises an unsupported-type outcome as such,
rather than leaving the payload to fail some later check for an unrelated
reason -- so the behaviour is a decision in the reference and not an artefact of
one. Because both sides refuse, no opt-in compatibility switch is required for
these two types (contrast `docs/COMPAT_SWITCHES.md`, which governs behaviour
that departs from an applicable reference contract).

R35.6A.4 **Framing.** A typed transaction is the concatenation of a single type
byte and the RLP-encoded, signature-bearing payload for that type; it is
therefore *not* an RLP list at its outermost level, which is what distinguishes
it from a legacy transaction on the first byte alone. A decoder MUST dispatch on
that first byte before attempting any RLP list decode, and MUST NOT treat a
typed envelope's failure to be a list as a diagnosis of the payload.

### Identifier, serialization fidelity and sender

R35.6A.5 **Identifier.** The transaction identifier is the keccak-256 digest of
the complete serialized form as presented: for a typed transaction the whole
envelope *including* the type byte, for a legacy transaction the RLP list. The
digest MUST NOT be taken over the payload with the type byte stripped. This is
the identifier the chain, the counterparty and the reference lineage all use, so
it is the only value for which our swap state, our history records and our
reported `tx_hex` can agree with a peer's.

R35.6A.6 **Serialization fidelity.** Re-serializing a decoded foreign
transaction MUST reproduce, byte for byte, the bytes that were decoded. The
requirement is unconditional, and it is stronger than reassembling the fields in
the canonical order: a reassembly agrees with the input only when the input was
itself canonically encoded, and the one case where they diverge is exactly the
case in which a reported identifier would stop matching the reported bytes.

R35.6A.7 **Signed message and sender.** For a typed transaction the signed
message is the keccak-256 digest of the type byte followed by the RLP encoding
of the *unsigned* payload for that type -- the payload that carries the chain id
as its first element and the access list as its last, and that omits the three
signature elements. The signature's y-parity is the secp256k1 recovery
parameter, and the sender is the address derived from the recovered public key.
The EIP-155 transformation of the legacy signature's recovery byte MUST NOT be
applied to a typed transaction, and a legacy signing payload MUST NOT be
substituted for the typed one: either substitution yields a different digest and
therefore recovers a *different, wrong* sender while still succeeding, which is
the failure mode this rule exists to exclude.

R35.6A.8 A typed transaction whose signature parity is any value other than the
two the specification admits MUST be rejected at decode.

R35.6A.9 **Chain-id of a typed envelope.** A typed envelope carries its chain id
inside the signed payload, and a conformant decoder MAY expose it. Decoding MUST
NOT be conditioned on that chain id matching the active coin's chain: the
reference lineage applies no such check on this path, so rejecting a mismatch at
decode time would refuse input the reference accepts. Whether a chain-binding
check belongs at a swap-validation site is D35.2.

### Strictness required beyond the reference

The two rules in this subsection reject input the reference lineage accepts.
Both concern input that no chain can produce, and both exist because accepting
it would let a counterparty control a value we report as fact. They are
deliberate divergences carrying no opt-in switch (D35.1).

R35.6A.10 **Trailing bytes.** Input carrying any byte beyond the end of the one
transaction it encodes MUST be rejected, for both the legacy and the typed form.
An accepted suffix changes the identifier the project reports while leaving every
decoded field identical, which makes the identifier of a counterparty's
transaction malleable by that counterparty.

R35.6A.11 **All-zero signature on a typed transaction.** A typed transaction
whose two signature scalars are both zero MUST be rejected. It MUST NOT be
treated as an unsigned transaction and MUST NOT be attributed to the
all-ones placeholder sender that the pre-EIP-155 unsigned-transaction convention
defines. No typed transaction format has an unsigned on-the-wire form, so the
convention does not reach them; and a decode that *succeeds* and reports a
deterministic placeholder address as the sender is worse than one that fails,
because a caller comparing senders receives an answer rather than an error.

### Fee fields of a decoded foreign transaction

R35.6A.12 An EIP-1559 envelope carries **no gas price** -- it carries a maximum
fee per gas and a maximum priority fee per gas, both of which are caps and
neither of which is an amount paid. Both values MUST NOT be substituted for a
price anywhere in the project: neither may be reported as one, and neither may be
compared against one. An EIP-2930 envelope and a legacy transaction do each
carry a genuine gas price.

R35.6A.13 Where a price actually paid is needed for a decoded foreign
transaction -- the fee details of a transaction-history record are the case that
exists -- it MUST be taken from the chain: the receipt's effective gas price, or
the price the node reports for the transaction. It MUST NOT be taken from the
envelope's caps, and a missing value MUST NOT be reported as zero. Reporting a
cap as a fee overstates it; reporting zero understates it; both are wrong in a
field an operator reads as an amount.

### Transactions reconstructed from a node's reply

R35.6A.14 Where a transaction is reconstructed from a node's JSON-RPC
transaction object rather than from raw bytes, a typed transaction MUST be
reconstructed under its own type's rules -- the typed signing message of
R35.6A.7, the parity recovery parameter, that type's fee fields and its access
list -- and never through the legacy rules. Routing a typed transaction through
the legacy reconstruction produces a usable object with a wrong sender, and the
paths that consume it (transaction history, spend search, payment checks) have
no way to notice. A node-reported transaction type outside the accepted set of
R35.6A.2 MUST be rejected.

R35.6A.15 The identifier recomputed from the reconstructed envelope MUST be
compared against the identifier the node reported, and a disagreement MUST be
rejected rather than resolved in favour of either value. (The reference lineage
adopts the node-reported identifier without recomputing it; see the divergence
note below.)

### Error surface

R35.6A.16 A rejection under any rule of this section is an **input-decode
failure of the coin layer's existing byte-decode error class**. This section
introduces no new mmrpc `error_type` discriminant, no new `error_data` shape and
no new HTTP status, and the discriminant a caller observes MUST NOT depend on
*why* the payload was refused: an unsupported type, a malformed payload, a
trailing suffix and an unrecoverable signature all surface in the same class.
That matches the reference lineage, where an unsupported type byte and a
payload that is not a transaction at all reach the caller as the same decode
failure.

The human-readable text is not bound by this chapter (the *Binding scope (R36)*
note at the head of the chapter). Distinguishing the cases in that text is
therefore permitted, and is worth doing -- an unsupported-type refusal and a
corrupt-payload refusal are diagnosed very differently by an operator -- but it
carries no wire meaning and no caller may branch on it.

### Which form the project emits, and the peer-version floor

R35.6A.17 On the sending side the form follows the policy of R35.6.3: the
`Legacy` policy emits an untyped EIP-155 legacy transaction, and the `Low`,
`Medium` and `High` tiers emit an EIP-1559 (type `0x02`) transaction. The
project emits no EIP-2930 transaction; it must nevertheless *read* one
(R35.6A.2), because the reference lineage reads one and a third-party signer may
produce one.

> **Reloaded status.** Both sides are implemented. The *reading* side is
> described above. On the *emitting* side the swap signing path follows the
> policy: `Legacy` builds the untyped legacy form with a single gas price, and
> `Low` / `Medium` / `High` build and sign an EIP-1559 (type `0x02`) transaction
> with no access list (signing message `keccak256(0x02 || rlp(unsigned
> fields))`, y-parity signature, identifier = hash of the whole envelope). A
> typed transaction a remote wallet signs through the WalletConnect path
> ([Chapter 22](22-walletconnect-v2.md)) remains the wallet's choice and not the
> policy's. Emitting a typed transaction carries the peer floor of R35.6A.18.
> Verified by construction, signing and decoder round-trip tests; network
> acceptance of an emitted typed swap transaction is not yet verified.

R35.6A.18 **Version boundary (recorded).** Typed-transaction decoding entered
the reference lineage within its public release history rather than being present
throughout it: a build at or before the public release designation `v2.0.0-beta`
reads the untyped legacy form only, and decoding of types `0x01` and `0x02` is
present from `v2.1.0-beta` onwards and unchanged at `v2.6.0-beta` and
`v3.0.0-beta`. Two consequences bind:

1. Accepting types `0x01` and `0x02` on the reading side restores parity with
   every reference build from `v2.1.0-beta` onwards. It does not add a
   capability the lineage lacks, and it needs no divergence entry.
2. A peer on a build at or before `v2.0.0-beta` cannot read a typed transaction
   we emit. This is a reason to keep `Legacy` as the built-in default policy
   (R35.6.3), and to be aware that the shipped coin configuration overrides it
   on several chains (see the note after R35.6.3), rather than a reason to restrict what we read; the reading and
   emitting sides have different compatibility floors and MUST NOT be collapsed
   into one decision. See D35.3.

R35.6A.19 **Acceptance is independent of emission.** The set of forms a build
will *read* MUST NOT depend on the form it *emits*: not on the coin's swap
gas-fee policy (R35.6.3), not on any configuration that restricts the form the
coin may send, and not on which activation route activated the coin. This is the
reference behaviour at both reference versions named in the derivation record --
the coin layer's byte-decode entry is a function of the bytes alone and consults
no coin state, so a reference build that *sends* only untyped legacy
transactions nevertheless reads, validates, re-serializes and recovers the
sender of an EIP-2930 or EIP-1559 transaction a counterparty sent, on every path
of R35.6A.1, the swap steps that read a maker's or a taker's payment included.
Emission there is separately restricted (see the note below); reading is not.
With R35.6A.18 this fixes the acceptance set of a reference build at the three
forms of R35.6A.2 for every build from the public release designation
`v2.1.0-beta` onwards, whatever its policy and whatever its configuration.

> **Upstream divergence (informative) -- per-coin cap on the emitted
> transaction type.** The reference lineage gates emission twice, not once. Next
> to the policy it reads an integer coin-configuration key `max_eth_tx_type`,
> the highest EIP-2718 transaction type the coin may send, on every activation
> route and with a token falling back to its platform coin's entry. When the key
> is absent the effective maximum is `0` -- untyped legacy only. A transaction
> whose type exceeds the coin's maximum is **refused at signing**: the send
> fails and does not fall back to a lower type, so a coin configured with a
> non-legacy policy but without a sufficient maximum cannot send a swap
> transaction at all. The public coins configuration the project ships sets
> `"max_eth_tx_type": 2` on each of the six coins that also carry a non-legacy
> `swap_gas_fee_policy`, and sets it explicitly to `0` on BNB (chain 56), which
> is how the reference keeps that chain on the untyped form. The cap applies
> only to transactions the project itself builds; it never reaches decoding
> (R35.6A.19).
>
> Reloaded honours the key (decision D35.7) exactly as the reference does by
> default: the integer key is read from the coin's own entry and, for a token
> that has none, from its platform coin's entry; an absent key means `0`; a
> transaction whose type exceeds the maximum is refused at signing and is neither
> rebuilt in a lower type nor broadcast. A value of the key that is not a
> non-negative integer is treated as absent. Reloaded offers **one opt-in
> divergence**: the node-level boolean `max_eth_tx_type_absent_unrestricted`
> (default `false`). When it is the boolean `true`, an absent key means
> unrestricted instead of `0`, so a coin whose configuration is silent about the
> cap keeps emitting typed transactions. The reason for offering it is that
> reading the absent key as `0` silently disables typed transactions on every
> coin whose configuration is silent about the cap; the shipped configuration
> sets the key on exactly the coins it means to speak about. Only the boolean
> `true` turns the switch on, so a typo or another value leaves the default. The
> switch never overrides a key that is present. It is listed in
> `GLEEC_COMPATIBILITY.md`.
>
> **Compatibility:** the default matches the reference lineage (and GLEEC KDF);
> no setting is needed for that behaviour. GLEEC KDF has no equivalent of the
> switch; its absent-key behavior is fixed.
>
> A further shipped key, `use_access_list`, appears on many EVM entries,
> including three of the six policy coins (AVAX, MATIC, MOVR, but not ETH,
> ETH-ARB20 or ETH-BASE). The reference's EVM coin layer does not
> read it and neither reference version emits an EIP-2930 transaction, so
> R35.6A.17's "the project emits no EIP-2930 transaction" is parity and the key
> is inert on both sides.

> **Upstream divergence (informative) -- strictness on input no chain can
> produce.** R35.6A.10 and R35.6A.11 each reject a payload the reference lineage
> accepts: a transaction followed by trailing bytes (which the reference folds
> into the identifier it reports), and a typed transaction with an all-zero
> signature (which the reference reports under the pre-EIP-155 placeholder
> sender). Neither payload can be a confirmed chain transaction, so no honest
> counterparty's bytes decode differently on the two sides, and no swap outcome
> changes for traffic that exists. Both ship as documented divergences with
> **no opt-in switch**, for the reasons recorded in D35.1; the operator-facing
> compatibility documents carry the entries, sourced from this section.

> **Upstream divergence (informative) -- identifier cross-check.** R35.6A.15
> requires a reconstructed typed transaction's recomputed identifier to agree
> with the node-reported one. The reference lineage carries the node-reported
> identifier through without recomputation, so a node reporting an identifier
> that does not match the bytes it also reported is detected here and not there.
> The check can only fire on an inconsistent or hostile node, and its effect is
> a refusal rather than a different value.

> **Upstream divergence (informative) -- chain id of a typed envelope.** The
> reference lineage's chain-id accessor derives the value from the legacy
> signature convention, and therefore reports *no chain id* for a typed
> transaction even though the envelope carries one; nothing on the
> foreign-transaction path consumes that accessor there. Exposing the envelope's
> chain id (R35.6A.9) is a superset with no wire effect, and R35.6A.9 forbids
> acting on it at decode time.

> **Code-quality finding (informative) -- identifier malleability from a
> trailing suffix.** In the reference lineage the identifier of a decoded
> transaction is taken over the whole input presented to the decoder, while the
> decoded fields are taken only from the extent the transaction's own length
> header declares. A caller that appends bytes therefore obtains a decoded
> transaction with unchanged fields and a changed identifier, and nothing
> refuses the input. The repair is the pair of rules this section already
> states: bound the digest to the declared extent of the one transaction, and
> refuse input longer than that.
> *(Trigger: apparent with little extra effort -- the identifier and the decoded
> fields of one and the same transaction are observably taken over different
> extents of the input.)*

> **Code-quality finding (informative) -- unsigned-transaction convention
> applied to typed forms.** In the reference lineage the condition that treats a
> transaction as unsigned, and so attributes it to the placeholder sender,
> depends on the two signature scalars alone. It therefore holds for every
> transaction form rather than only for the untyped one, although only the
> untyped form has an unsigned on-the-wire representation for the convention to
> describe. The repair is to restrict the convention to the untyped form, which
> is R35.6A.11.
> *(Trigger: apparent with little extra effort -- a pre-EIP-155 convention
> observably reaching transaction forms that postdate it and define no such
> state.)*

> **Code-quality finding (informative) -- zero substituted for a missing
> price in reported fees.** In the reference lineage the fee details of a
> transaction-history record take the node-reported price and substitute zero
> when the node does not report one, which is precisely the case a typed
> transaction can present. The record then states a fee of zero for a
> transaction that paid a fee. The repair is R35.6A.13: take the receipt's
> effective gas price, which is the amount actually paid and is reported for
> every transaction form.
> *(Trigger: apparent with little extra effort -- a zero stand-in for a value
> the source of the data does not always supply, in a field reported to an
> operator as an amount.)*

---

## 35.7 Implementation-substrate note (informative)

This note orients the implementer; it is not normative.

**Already present in reloaded:**
- the EVM coin type (the public `EthCoin` newtype and its `MmCoinEnum` variant),
  its coin-type enumeration (native ETH vs. ERC-20 vs. NFT), and the legacy
  builder that creates an EVM coin from coin config + request;
- the generic V2 activation entrypoints (platform-coin-with-tokens, single
  token) and the platform task substrate of ch. 48;
- one-shot `enable_eth_with_tokens`, `enable_erc20`, `enable_nft` (separate NFT
  handler), `get_token_info`, `get_swap_gas_fee_policy`,
  `set_swap_gas_fee_policy`, and `task::enable_eth::*`, routed on native and
  WASM, with the deviations recorded in the `Reloaded status` notes of §35.1,
  §35.2, §35.3 and §35.3A.

**Already present in reloaded, for §35.6A:** the vendored EVM transaction
module that the EVM coin decodes foreign transaction bytes through is the single
decoder every path of R35.6A.1 reaches, so the contract has one implementation
site rather than one per path. The untyped legacy form, and decoding of the two
accepted typed forms with the identifier, serialization-fidelity, signing-message
and refusal rules of §35.6A, are implemented there; the node-reply
reconstruction path of R35.6A.14 and the fee-source rule of R35.6A.13 are
implemented in the EVM coin itself. The provenance of that module is recorded in
[Chapter 34](34-provenance-ledger.md) §34.2 and is unaffected by the typed-form
work, which rests on the public specifications named in the footer and on a
dependency the project already carries.

**What must be added or changed:** the wire contract of §§35.1--35.3A where the
status notes record a deviation, the `task::enable_erc20::*` family, and the
wire-shape tests of §35.8A. The placement of these implementations should follow
the existing activation layout used by the other platform coins; their internal
decomposition is the implementer's choice.

---

## 35.8 Acceptance criteria

- `enable_eth_with_tokens` activates an EVM platform coin plus zero or more
  ERC-20 tokens in one call and returns `current_block`, the per-address platform
  and token information, and an always-present `nfts_infos`, in the
  single-address or HD shape (R35.1.3); `get_balances: false` omits balances and
  lists token `tickers` (single-address shape).
- With every supplied node unresponsive the call fails with `Transport`, a
  string `error_data`, HTTP 502 (R35.1.6).
- Malformed requests are rejected with the bound discriminant, `error_data`
  shape and HTTP status of R35.1.5 without partially activating the coin.
- `enable_erc20` adds a single ERC-20 token to an already-active single-address
  EVM platform and returns `balances`, `platform_coin`, lower-case
  `token_contract_address`, and `required_confirmations` (R35.2); `enable_nft`
  reaches the same handler and error contract (R35.2.4).
- `task::enable_eth::*` and `task::enable_erc20::*` deliver the same results as
  their one-shot counterparts (respectively the token-task result of R35.3A.5),
  with the error placement of R35.3.5 / R35.3A.3 / R35.3A.6, and the Trezor
  policy drives the bound in-progress / awaiting values through `status` /
  `user_action` (R35.3.2, R35.3.3).
- `get_token_info` returns the ERC-20 contract `symbol` and `decimals` (plus the
  configured ticker when known) for an active EVM platform, and rejects
  non-token / non-EVM protocols with the bound errors (R35.4).
- `get_swap_gas_fee_policy` / `set_swap_gas_fee_policy` round-trip a policy in the
  set `{Legacy, Low, Medium, High}` for an EVM coin, use exactly those method
  strings and the `swap_gas_fee_policy` field, and reject unsupported coins
  (R35.6). A round-trip alone does **not** satisfy R35.6.6: the stored policy
  must also change how a subsequent swap transaction is priced and which
  serialized form it takes (R35.6A.17). That part is implemented -- D35.4.
- The entire surface builds and is routed on native **and** WASM targets, with
  the MetaMask policy and `rpc_mode: "Metamask"` available only on WASM
  (R35.1.4, R35.1.4a).
- A counterparty's serialized transaction decodes on every path of R35.6A.1 when
  it is an untyped legacy, an EIP-2930 or an EIP-1559 transaction, and is
  refused when it is any other EIP-2718 type, with the identifier, the recovered
  sender and the re-serialized bytes all equal to what the chain reports
  (§35.6A).
- Node access follows §35.1.F: a node that answers HTTP 200 with a JSON-RPC
  error object is failed over like a dead node (R35.1.8), the answering node
  is tried first next time (R35.1.9), no node holds a request longer than 10 s
  (R35.1.10), the all-failed error is the last non-cutoff failure or a generic
  unreachable error (R35.1.11), signed transactions are broadcast to the
  highest-nonce nodes concurrently (R35.1.12), and no emitted text names a
  node by more than scheme, host and port (R35.1.14, R35.1.15).
- A swap completes against a peer whose swap gas-fee policy is a non-legacy tier
  (R35.6.3) -- the case that fails outright when only the untyped legacy form can
  be read.
- The three forms of R35.6A.2 decode on a coin whose policy is `Legacy` exactly
  as they do on a coin whose policy is a non-legacy tier, and on a coin
  activated through `enable` / `electrum` exactly as on one activated through v2
  or a task (R35.6A.19).

## 35.8A Wire-shape tests (binding)

Each test below serializes the response or error exactly as the dispatcher
would and asserts the `error_type` string, the JSON type of `error_data` (and its
field names and types for objects, or its absence), and the HTTP status. A test
that only checks an internal variant does not satisfy these rules.

- **T35.1** `enable_eth_with_tokens` with every node answering HTTP 502 (and,
  separately, refusing connections): `Transport`, string, 502 -- both with all
  token decimals in config and with a token whose decimals must be read
  on-chain.
- **T35.2** Chain-height read failing after a successful node probe (or with no
  probe): `Transport`, string, 502.
- **T35.3** Balance read failing (single-address): `Transport`, string, 502.
- **T35.4** Absent/zero `swap_contract_address` on a trading-capable coin, zero
  fallback, invalid node URL, unsupported URL scheme: `InvalidPayload`, string,
  400; absent `swap_contract_address` on a wallet-only coin succeeds.
- **T35.5** Empty and absent `nodes`: `AtLeastOneNodeRequired`, string, 400.
  Absent `erc20_tokens_requests`: `InvalidRequest`, string, 400.
- **T35.6** `Trezor` policy on the one-shot call: `UnexpectedDeviceActivationPolicy`,
  no `error_data` key, 400.
- **T35.7** Token whose protocol names another platform: `PlatformCoinMismatch`,
  no `error_data`, 400. Token with a non-checksummed configured contract
  address: `UnexpectedTokenProtocol`, object `{ticker, protocol}`, 400.
- **T35.8** `PrivKeyPolicyNotAllowed` serializes under that exact name with a
  string `error_data`, HTTP 500.
- **T35.9** `rpc_mode: "Metamask"` and `priv_key_policy: {"type":"Metamask"}` on
  a native build: `InvalidRequest`, string, 400; a string-form
  `priv_key_policy`: `InvalidRequest`.
- **T35.10** Success shapes: single-address with `get_balances` true and false
  (`balances` omitted, `tickers` only on the token entry, `nfts_infos` = `{}`);
  `pubkey` is 130 hex characters starting `04` without `0x`; HD shape with
  `ticker`, `wallet_balance.wallet_type = "HD"` and `nfts_infos`.
- **T35.11** `nft_req` accepted in both the wrapped and the bare form.
- **T35.12** `enable_erc20`: decimals read failing → `Transport`, string, **500**;
  balance read failing → `Transport`, string, 500; `token_contract_address`
  is lower-case; `PlatformCoinIsNotActivated` carries the platform ticker as a
  string; each `CustomTokenError` case serializes as tabled in R35.2.7.
- **T35.13** `enable_nft` failure classes of R35.2.8 serialize with the
  single-token discriminants.
- **T35.14** `task::enable_eth::init` on an active ticker: mmrpc-level
  `PlatformIsAlreadyActivated`, string, 400; a config-not-found ticker yields a
  `task_id` and later a status-level `PlatformConfigIsNotFound`.
- **T35.15** Trezor task: no hardware context → status-level `Internal`, string;
  a device button request is reported as `FollowHwDeviceInstructions`.
- **T35.16** `task::enable_erc20::init` errors of R35.3A.3 (in particular
  `TokenIsAlreadyActivated` with object `{ticker}` and
  `TokenProtocolParseError` at 400); terminal success shape of R35.3A.5 for a
  single-address platform; terminal `Transport` / `CouldNotFetchBalance` per
  R35.3A.6.
- **T35.24** *Failover on a JSON-RPC error body (R35.1.8).* Two stub nodes;
  the first answers HTTP 200 with a JSON-RPC error object (once with code
  `-32000` and a revert message, once with code `3`), the second answers the
  same `eth_call` with a result: the call returns the second node's result,
  the first node's error is logged at warn in the R35.1.15 form, and the
  second node is first in order for the next request (R35.1.9). Repeat with
  the first node answering HTTP 502, refusing the connection, and returning an
  undecodable body and a batch envelope: every variant advances to the second
  node.
- **T35.25** *All nodes revert (R35.1.11, item (b)).* Three stub nodes each
  answer `eth_call` with a JSON-RPC error object carrying distinct messages:
  the caller's error carries the **third** node's code and message, ends with
  ` (node <host>)` naming the third node's host-only form, and classes as an
  invalid-response (not `Transport`) failure. Variant: nodes one and two answer
  JSON-RPC errors and node three exceeds the cutoff: the caller's error is node
  **two**'s. Variant: every node exceeds the cutoff: a generic
  transport-class error that names no node.
- **T35.26** *Stickiness (R35.1.9).* Nodes A, B, C, D in that order; A and B
  fail, C answers: the next request goes to C first, then D, then A, then B.
  A second coin instance (re-activation) does not inherit the order.
- **T35.27** *Per-node cutoff (R35.1.10, item (d)).* A stub node that accepts
  the connection and then streams a body slower than the cutoff: the request
  moves to the next node after 10 s (± scheduler slack) even though the
  lower-layer timeout has not elapsed; with four such nodes the request fails
  after about 40 s and not earlier.
- **T35.28** *Host-only rendering (R35.1.14).* Activate with a node URL
  carrying userinfo, a path segment and a query string
  (`https://user:secret@rpc.example.org:8545/v3/APIKEY?token=X`) and make
  every node fail: the activation probe log, each per-node failure log, the
  all-failed error returned through the RPC envelope, and any embedded
  lower-layer text contain `https://rpc.example.org:8545` and contain none of
  `user`, `secret`, `/v3/APIKEY`, `APIKEY`, `token=X`. Repeat with a URL
  whose lower layer fails at DNS resolution, and, once a WebSocket transport
  exists, with a `wss://` URL; until then a `wss://` URL is rejected at
  activation and the variant asserts that the rejection names none of the
  URL's userinfo, path or query.
- **T35.29** *Error message names the node (R35.1.15).* The ` (node <host>)`
  suffix is present on a JSON-RPC-error outcome of R35.1.11 and absent on a
  transport-class outcome; `error_type`, `error_data` shape and HTTP status
  are unchanged by the suffix.
- **T35.30** *All-failed message lists hosts only (R35.1.11, R35.1.14).* With
  every node failing by non-2xx status, the single returned error names each
  node tried in host-only form and no node more than once.
- **T35.31** *Signed broadcast (R35.1.12, item (e)).* Three stub nodes report
  pending nonces 5, 7, 7: the signed transaction is sent to nodes two and
  three only, concurrently; node two answers a JSON-RPC error of the
  "already known" kind while node three accepts: the send succeeds. Variant:
  both reject: the send fails with the last rejection. Variant: no node
  answers the nonce request: six rounds one second apart, then failure.
- **T35.32** *Caller-supplied raw transaction (R35.1.12 last paragraph, item
  (e)).* `send_raw_transaction` with node one answering a JSON-RPC rejection
  and node two accepting: the RPC returns node two's hash; with both
  rejecting: node two's rejection text, suffixed per R35.1.15.
- **T35.33** *Batch envelope and token-metadata reads (R35.1.8 item (a),
  R35.1.13).* A node that answers a single `eth_call` with a batch envelope is
  failed over; `get_token_info` against a first node that answers the
  `decimals()` call with a JSON-RPC error and a second node that answers
  correctly returns the second node's values (binding for Reloaded, which
  takes the R35.1.13 choice per D35.8 (iv)).
- **T35.34** *Coin and list position in node log lines (R35.1.16).* A coin
  `ETC` with four configured nodes, of which the second as given fails the
  activation probe: the probe's line for it starts with `[ETC]` and carries
  the host-only form followed by `(node 2 of 4)`, whatever order activation
  drew. A per-node failure and a JSON-RPC error answer logged by the
  failover path, and a failing node of the nonce gathering, carry the same
  prefix and position. Two coins with nodes on one host and different paths
  produce lines that differ in the ticker; two nodes of one coin on one host
  produce lines that differ in the position. The returned texts of R35.1.11
  and R35.1.15 are unchanged.
- **T35.35** *URLs in the redacted wire dump (the rule after R35.1.17,
  D35.10).* Feed the dump's redaction an activation request for RIN and one for
  an EVM coin, whose server and node entries carry: a URL with userinfo, a path and a
  query; a URL with a path only; a URL whose path is `/`; an Electrum
  `host:port`; and, in another member, an error text that embeds a URL with a
  query. The redacted output keeps the ticker and every other member
  unchanged. Each URL keeps its scheme, host and port, shows `<redacted>` in
  place of each part it had, and contains none of the original userinfo,
  path or query. The `/` path and the `host:port` entry are unchanged. With
  the secrets switch on, the output equals the input.
- **T35.36** *Credentials in renderings (R35.1.17).* Build the EVM transport
  from a node URL carrying userinfo, a path segment and a query string (the
  URL of T35.28), and configure a `gas_station_url` and a gas-API provider URL
  that each carry a key in the path and in the query. The debug form of the
  transport, of each node entry and of the parsed activation request contains
  each URL's host-only form and contains none of the userinfo, path or query
  parts; the same holds for the display form of every error the activation
  and a failing request return. A node entry or request that carries no
  secret renders as before. Applies to the TRON client too (ch. 21 §21.9).

## 35.8B Foreign-transaction decoding tests (binding)

The tests below are binding on §35.6A. Each one asserts against a value the
**chain** reports, never against a value this project computed: the vectors are
signed transactions fetched from a public chain by identifier, so the bytes, the
identifier and the sender correspond by construction and cannot all be wrong
together. A test that round-trips our own encoder against our own decoder proves
nothing about any of these rules and does not satisfy them.

- **T35.17** *Real-chain vectors.* For each of: an untyped legacy transaction, an
  EIP-2930 transaction with a non-empty access list, an EIP-1559 transaction with
  no access list, and an EIP-1559 transaction with a non-empty access list --
  assert that the decoded identifier equals the chain-reported identifier
  (R35.6A.5), that the recovered sender equals the chain-reported sender
  (R35.6A.7), and that re-serializing the decoded transaction yields the original
  bytes exactly (R35.6A.6). Use more than one chain, so that a chain-id-dependent
  mistake cannot pass.
- **T35.18** *The digest covers the envelope.* Assert that the digest taken over
  the typed payload with the type byte stripped differs from the chain-reported
  identifier. Without this, a decoder that happens to hash the right bytes is
  indistinguishable from one that is pinned to (R35.6A.5).
- **T35.19** *Refusal set.* Type bytes `0x00`, `0x03`, `0x04` and at least one
  unassigned value below `0x80` are each refused; a payload that is not a
  transaction at all is refused; each refusal surfaces in the single error class
  of R35.6A.16 and none of them is distinguishable from the others by any
  machine-readable field.
- **T35.20** *Refusals that protect attribution.* A vector with one byte of its
  signature altered does not recover the chain-reported sender; a typed envelope
  whose two signature scalars are zero is refused rather than attributed to the
  placeholder sender (R35.6A.11); a valid envelope with one byte appended is
  refused (R35.6A.10); a typed envelope with an out-of-range signature parity is
  refused (R35.6A.8).
- **T35.21** *Both production entry points.* The coin-layer byte-decode entry and
  the V2 swap-protocol transaction parser each accept an EIP-2930 and an
  EIP-1559 vector and yield the identifier and sender T35.17 asserts. Testing
  the decoder in isolation does not satisfy this: R35.6A.1 is a statement about
  call paths.
- **T35.22** *Node-reply reconstruction.* A node-reported EIP-2930 and
  EIP-1559 transaction object yields the same sender and identifier as decoding
  the same transaction's raw bytes (R35.6A.14); an object whose reported
  identifier disagrees with the recomputed envelope is refused (R35.6A.15); a
  node-reported type outside the accepted set is refused; the legacy branch's
  results are unchanged.
- **T35.23** *Fee reporting.* For an EIP-1559 transaction the history record's
  fee is computed from the receipt's effective gas price and is non-zero, and
  neither the maximum-fee nor the maximum-priority-fee value appears as the price
  (R35.6A.12, R35.6A.13). For a legacy transaction the reported fee is unchanged
  from the pre-typed behaviour.

## 35.8C Deferred Work

**D35.1** *Decision taken -- the two stricter refusals ship as documented
divergences with no opt-in switch.* R35.6A.10 (trailing bytes) and R35.6A.11
(all-zero typed signature) refuse input the applicable reference contract
accepts, which on its face engages this repository's rule that a deliberate
divergence ships with a scoped opt-back mechanism and its documentation in the
same change. The decision is that **no switch is offered**, on three grounds:

1. Neither input can be a confirmed chain transaction, so no honest
   counterparty's bytes decode differently on the two sides and no swap outcome
   changes for traffic that exists.
2. Accepting either lets a counterparty choose a value the project then reports
   as fact -- the transaction identifier in the first case, the sender in the
   second.
3. A switch here would be an opt-in to *weaken* validation, which this
   repository's contributor rules bar outright. An opt-back exists to let an
   operator recover a reference behaviour that is merely different; it is not a
   mechanism for restoring a reference behaviour that is worse.

This follows the precedent of the Electrum-dependency removal, which shipped as
a divergence stated plainly rather than as a switch that would not have been
safe to flip. The divergence is recorded in the operator-facing compatibility
documents; §35.6A is the source for those entries.

The decision remains the maintainer's to revisit: electing a switch would change
nothing in §35.6A except the addition of the opt-back and its entries in the
divergence documentation. Nothing else in this chapter depends on the outcome.

**D35.2** *Maintainer decision -- chain binding of a counterparty's
transaction.* R35.6A.9 keeps decoding chain-agnostic because the reference
lineage does. Whether a swap-validation site should additionally require a typed
envelope's chain id to equal the active coin's chain is open. It would be a
divergence in the direction of more validation, and it interacts with coins that
share an address space across chains; it is not settled by this chapter and MUST
NOT be added as a side effect of decoding work.

**D35.3** *Outgoing typed emission and the peer floor.* R35.6A.18 records that a
peer on a reference build at or before `v2.0.0-beta` cannot read a typed
transaction. The *built-in* default is the legacy policy (R35.6.3), but the
shipped coins configuration already sets a non-legacy tier per chain on six
coins (the note after R35.6.3), so on those chains the project emits a typed
swap transaction without the operator asking for one -- and the reference does
the same there. The peer floor is therefore already load-bearing on those six
chains, not hypothetical: a peer on a build at or before `v2.0.0-beta` cannot
read what we send them on ETH, MATIC, AVAX, ETH-ARB20, ETH-BASE or MOVR. The
open question is whether the emitted form should be gated on a negotiated peer
capability; it was not answered when the policy was made effective (D35.4), and
it is unaffected by the resolutions of D35.6 and D35.7.

**D35.4** *(Resolved.)* *The swap gas-fee policy had no effect on a transaction.*
R35.6.6 requires the stored policy to govern how the coin prices gas, and
R35.6A.17 requires it to decide which serialized form the coin emits; before
this resolution the policy was stored and echoed while every locally signed swap
transaction was an untyped legacy one priced from a single gas price. Resolved:
(a) the fee per non-legacy tier is the matching tier of the EIP-1559 estimate
that [Chapter 10](10-sse-streaming.md) exposes (configured gas API when present,
otherwise the internal fee-history estimator), so there is one tier definition;
(b) a type-`0x02` transaction is constructed and signed, and decodes back
through the §35.6A decoder with the same sender and identifier; (c) `Legacy`
remains the *built-in* default, but the shipped coins configuration overrides it
on six coins, so this resolution did make the peer floor of D35.3 load-bearing
on those chains -- see the corrected D35.3. Remaining: network acceptance of an emitted typed
swap transaction and the suitability of the tier values under real congestion
have not been exercised against a live chain.

**D35.5** *Chapter shape.* This chapter's canonical sections were, before this
pass, an *Acceptance criteria* section and a *Wire-shape tests* section with no
*Deferred Work* section at all, which chapter 01 R19 requires. This section
closes that gap for the deferrals above; the chapter still carries no
*Baseline Verifications* section, which R19 permits only when the chapter binds
no such claims. Confirming that is a separate pass over §§35.1--35.4.

**D35.6** *(Resolved -- maintainer decision, 2026-10-16.)* *The policy on the legacy
activation route.* R35.6.7 states the reference's single precedence, applied on
every activation route. Reloaded used to resolve the policy only on the v2 and
task routes, so a coin on one of the six chains the shipped configuration puts on
`Medium` emitted type-`0x02` swap transactions when activated through v2 or the
task family and untyped legacy ones when activated through `enable` /
`electrum`. Three directions had been open: (a) resolve the policy on the legacy
route too; (b) keep the split as a deliberate divergence; (c) accept the request
field on the legacy route but leave the configuration key unread there. The
maintainer chose (a), deliberately, paired with D35.7. The legacy route now
resolves request parameter, else configuration key, else built-in `Legacy`, with
the same resolver as the other routes, so the emitted form is a property of the
coin and not of the caller's choice of activation method. It also accepts the
`swap_gas_fee_policy` request field (ch. 55 R55.4.4) and parses it leniently: an
unrecognised value is ignored, not rejected, whereas the v2 and task routes
reject it (R35.1.1). The leniency is the specified behavior on that route and the
two parses are not unified. The divergence was emission-only (R35.6A.19) and
could not make a counterparty's transaction unreadable.

**D35.7** *(Resolved -- maintainer decision, 2026-10-16; default inverted the same day.)* *The per-coin cap on the
emitted transaction type.* The reference's second emission gate, the
`max_eth_tx_type` coin-configuration key described in the divergence note after
R35.6A.19, is honoured. Three positions had been open: adopt the reference
exactly (an absent key means `0`), do not adopt it at all, or honour the key
where present and treat its absence as unrestricted. The maintainer first chose
the third, then ruled that a compatibility switch exists to opt into a
divergence, so its default must be the reference behaviour at least before 1.0.
The default is therefore the reference reading: an absent key means `0`. A chain
the shipped configuration pins at `0` (BNB today) stays on the untyped form even
if an operator sets a non-legacy tier. A send whose type exceeds the cap is
refused at signing, in the signer, so every typed construction path is covered;
the send fails and no lower type is substituted. The divergence is opt-in: the
node-level boolean `max_eth_tx_type_absent_unrestricted`, default `false`, which
when `true` makes an absent key mean unrestricted. Activation logs the effective
cap and its source next to the policy. R35.6A.19 holds throughout: nothing here
narrows what the project reads.

> **Coverage gap on the withdraw path (cross-reference).** "Refused at
> signing, in the signer, so every typed construction path is covered" holds
> only for paths that reach the typed signer. The EVM **withdraw** paths do
> not: they always build the untyped envelope today, so the cap is never
> tested there — vacuously satisfied rather than enforced.
> [Chapter 57](57-evm-fee-inputs.md) R57.7.4 binds the cap check onto the
> withdraw path explicitly, **before** the transaction is built, as part of
> closing that chapter's R57.7.6; and [Chapter 19](19-nft-module-layout.md)
> §19.7 records the same for the two non-fungible withdrawals. Nothing in
> D35.7's resolution changes; the check simply has to be made at a second
> place once that path can emit a typed transaction.

**D35.8** *(Resolved -- maintainer decision, 2026-10-26.)* *Reloaded choices
left open by §35.1.F.* Four points the reference rows do not decide, or decide
in a way Reloaded need not copy, were open, and are resolved as follows. None
is peer-visible, so none carries a switch.

- (i) **A token shares its platform's live node order** (R35.1.7), instead of
  starting from a copy.
- (ii) **A batch fails the node as a whole**: should batching ever be
  introduced, a batch response in which any element carries a JSON-RPC error
  object, or which does not decode, is one node failure for the whole batch
  (R35.1.8, item (a)).
- (iii) **Requests on one coin instance run concurrently**, with the
  last-store-wins order of the note after R35.1.10. The maintainer approved
  this on a written scenario analysis covering racing requests, flapping
  nodes, lock discipline, connection growth, slow or malicious nodes and the
  signed broadcast. The analysis found no blocked state and no new attack
  surface specific to concurrency.
- (iv) **The token-metadata reads go through the per-request failover**
  (R35.1.13).

The signed-transaction broadcast of R35.1.12 is implemented as bound there --
concurrently to every node that reported the highest nonce -- confirmed by the
same decision. Two Reloaded notes record what the implementation does where
the reference rows bind nothing or Reloaded departs: the coin-layer class
after R35.1.11, and the per-node bound of the nonce reads in R35.1.12 step 1.
The `docs/GLEEC_COMPATIBILITY.md` row for R35.1.14 / R35.1.15 is added with
the implementing change.

**D35.9** *(Open -- to be evaluated; maintainer decision, 2026-10-26.)* *A bound
on the nonce-increase wait.* Step 3 of R35.1.12 has no upper bound, in Reloaded
as in both reference rows (see the code-quality finding there). To be
evaluated: an upper bound on the wait, the distinct failure it surfaces, how
each swap step that can reach it handles that failure without losing track of
a transaction that is mined after all, and the scope of the lock held while
waiting. Any bound is a Reloaded divergence, not peer-visible. Nothing is
implemented; the evaluation belongs to the swap and transaction-path hardening
plan (`docs/plans/swap-transaction-path-hardening.md`).

**D35.10** *(Resolved 2026-10-28, maintainer decision.)* *Node URLs in the
redacted RPC wire dump.* R35.1.17 leaves the RPC wire dump to its own switch.
In its default, redacted mode the dump redacted members whose key names a
secret, and a node `url` member is not one of them, so an activation request
in the dump showed each node URL in full. The maintainer decided that the
redacted mode strips URLs to the host, with the condition that nothing which
is useful and not secret is stripped. The rule after R35.1.17 therefore
rewrites URLs only: it keeps scheme, host and port, marks each removed part
instead of dropping it silently, and leaves every other member as it was. A
URL's path is removed even though it is often harmless, because hosted
providers put the API key there and the dump cannot tell the two apart. The
change is to the dump's own redaction, not to R35.1.14 or R35.1.17. T35.35
tests it.

## 35.9 Provenance Footer

- *Inputs:* the published Komodo DeFi Framework API reference (the
  `enable_eth_with_tokens`, `enable_erc20`, `task_managed/enable_eth`,
  `enable_nft` pages, `common_structures/{activation,wallet,nfts,enums,error_types}`
  and the v20 index) -- governing where explicit; the EVM JSON-RPC, EIP-55,
  EIP-1559, ERC-20 ABI and CAIP-style chain identification specifications; for
  §35.6A additionally EIP-2718 (typed-transaction envelope framing and the
  reserved type range), EIP-2930 (access-list transaction payload), EIP-1559
  (priority-fee transaction payload), EIP-155 (legacy replay protection), and the
  keccak-256 and secp256k1 public-key-recovery specifications;
  reloaded's existing EVM coin type, activation entrypoints and task substrate
  (§35.7); signed transactions fetched from public EVM chains by identifier,
  with the identifier and sender those chains report, as the vectors §35.8B
  binds; the public coins configuration file the project ships (the
  `swap_gas_fee_policy`, `max_eth_tx_type` and `use_access_list` keys of its EVM
  entries, and the chain ids and tickers they sit on);
  [Chapter 10](10-sse-streaming.md) (the EIP-1559 fee-per-gas estimate
  named as the natural fee source in D35.4);
  [Chapter 17](17-swap-v2-evm-path.md) (V2 swap contracts);
  [Chapter 19](19-nft-module-layout.md) (NFT activation and pseudo-coin);
  [Chapter 21](21-tron-integration.md) (TRON platform specifics);
  [Chapter 22](22-walletconnect-v2.md) (WalletConnect sessions);
  [Chapter 38](38-utxo-coin-maintenance-and-features.md) (HD activation
  semantics); [Chapter 47](47-metamask-integration.md) (MetaMask policy);
  [Chapter 48](48-platform-coin-task-activation.md) (task substrate);
  [Chapter 50](50-evm-trezor-signing.md) (Trezor EVM path);
  [Chapter 55](55-coin-activation-wire-contract.md) (envelope, HTTP policy, task
  framework, backend-failure table).
- *Permitted-input classes used:* external public specification (the published
  KDF API reference; EVM/EIP/ERC/CAIP specifications); baseline source;
  behavioural observation of public networks (the chain-fetched transaction
  vectors of §35.8B, and the identifier and sender those chains report);
  cross-chapter contracts; Interop / wire-and-API-bound reuse (R29/R31) for the
  dictated method strings, JSON field names, serialized discriminants,
  `error_data` shapes, and the EIP-2718 envelope framing and payload layouts of
  §35.6A.
- *Sibling-allowlist consultations:* the chapters listed under *Inputs*.
- *Forbidden corpus:* consulted by the KDF Spec Reader (reference versions
  `v3.0.0-beta` and `v2.6.0-beta`) solely to decide behaviour where the public
  reference is silent, ambiguous or self-contradictory (marked **G**), including
  every HTTP status; and, for §35.6A, to establish which EIP-2718 transaction
  types the reference lineage accepts and refuses when reading a transaction it
  did not build, the error class a refusal takes, and the public release
  designations at which that acceptance set changed (named in R35.6A.18; those
  designations are the reference lineage's own public release tags and are the
  most precise locator this record gives). The transaction framing, payload
  layout, identifier, signing-message and signature-parity rules of §35.6A rest
  on the public specifications listed under *Inputs*. No private implementation
  expression is normative in this chapter. §35.6A and the sections cleared with
  it have **passed the KDF Dirty Gate** in two passes. A third Spec Reader pass
  on 2026-10-16 consulted the same two reference versions for three further
  behavioural facts: the single
  precedence by which the initial swap gas-fee policy is resolved on every
  activation route including the legacy one (R35.6.7 and the divergence note
  after it); the independence of the acceptance set from the policy, from any
  emission restriction and from the activation route (R35.6A.19); and the
  existence and semantics of a public coin-configuration key that caps the
  highest transaction type a coin may send, including its absent-key value and
  its refuse-rather-than-downgrade outcome, plus the observation that the
  neighbouring `use_access_list` key is not read at all (the divergence note
  after R35.6A.19). Those two rules, those two notes, the acceptance criterion
  that cites R35.6A.19, D35.6, D35.7 and the clause of the corrected D35.3
  stating that the reference emits a typed swap transaction on the same six
  chains **passed the KDF Dirty Gate** in a third pass on 2026-10-16 (PASS,
  0.02). The two earlier clearances do not cover that material, and the third
  does not replace them: §35.6A's *Clearance record* records the extent, method
  and measure of each of the three passes separately, as chapter 01 T1
  requires. The remainder of the D35.3 and D35.4 corrections made in the same
  pass rests on this repository's own code and on the public coins
  configuration. Every other section of this chapter rests on the inputs listed
  above.
- *Clean-context additions (forbidden corpus not consulted):* the informative note
  *Initial policy and shipped configuration* after R35.6.3, the "built-in default"
  wording of R35.6.3, and the reworded item 2 of R35.6A.18 were added on
  2026-10-15 and 2026-10-16 (commit `99a38e59b`) by a context that never read the
  forbidden corpus. They rest on this repository's own source (the activation
  precedence in `coins_activation` and `coins`, the signing path) and on the
  public coins configuration, which sets `swap_gas_fee_policy` for six EVM
  coins. The *Forbidden corpus* entry above does not apply to these lines, and
  they rely on none of the clearances recorded in §35.6A; the third pass read
  them only within its skim of the rest of the chapter.
- *Clean-context additions, second batch (forbidden corpus not consulted):* the
  resolutions of D35.6 and D35.7, the rewritten divergence note after R35.6.7,
  the second paragraph of the divergence note after R35.6A.19 (Reloaded's
  handling of the cap, its absent-key divergence and the compatibility switch),
  and the updates to the informative note after R35.6.3 were written on
  2026-10-16 by a context that never read the forbidden corpus. They state
  Reloaded's own decided behavior and rest on this repository's source and the
  public coins configuration. The reference facts they refer to (the single
  precedence, the absent-key value of `0`, refusal rather than downgrade) are
  those already cleared in the third pass and are not restated with new content.
  The Dirty Gate clearance recorded above covers neither the new paragraph nor
  the resolutions.
- *Fourth Spec Reader pass (2026-10-26), §35.1.F:* the forbidden corpus was
  consulted at the reference rows `v2.6.0-beta` and `v3.0.0-beta` (chapter 02
  §2.15A) solely to establish the observable multi-node access behavior bound
  in R35.1.7 -- R35.1.13 and recorded in the informative notes of §35.1.F:
  once-per-activation random order; per-request failover and which outcomes
  (including a success-status body carrying a JSON-RPC error object) advance
  to the next node; rotation of the answering node to the front; the 10 s
  per-node cutoff and the absence of an overall deadline; the single error a
  caller receives when every node fails; the concurrent highest-nonce
  broadcast of signed transactions and its unbounded nonce wait; the
  single-node token-metadata reads; and the unredacted full-URL error texts.
  The two rows agree on all of them. R35.1.14 and R35.1.15 and the
  `Reloaded divergence (2026-10-26)` note rest on this repository's own code
  and on the maintainer's instruction and consulted no corpus. The pass
  **passed the KDF Dirty Gate on 2026-10-26**; its extent, method and measure
  are recorded in the clearance statement at the head of §35.1.F. The
  clean-side edits made after that clearance (the dates, the clearance
  record, the HTTP-only scope sentence, the TRON naming-only wording, the
  nonce-agreement fact in the divergence note and the `wss://` condition in
  T35.28) were written by a context that never read the forbidden corpus and
  rest on this repository's own code.
- *Clean-side addition (2026-10-28), R35.1.16 and T35.34:* written by a
  context that never read the forbidden corpus, from an operator's report
  that a node failure line did not say which coin it belonged to and from
  the maintainer's instruction to keep host-only naming and add the coin
  ticker and the node's position in its list. They rest on this
  repository's own code and consulted no corpus, so chapter 01 R38 is not
  engaged. The §35.1.F wording now naming R35.1.14 -- R35.1.16 as the
  Reloaded-only rules is part of the same edit. The Dirty Gate clearance at
  the head of §35.1.F does not cover them.
- *Clean-context additions, node naming and credentials (2026-10-28, forbidden
  corpus not consulted):* R35.1.17 with its scope sentence and status note,
  the pointer after R35.1.14, the R35.1.17 mentions in the binding-scope note
  and the §35.1.F lead-in, the wire-dump rule after R35.1.17, T35.36, T35.35
  and D35.10 are Reloaded-original. They rest on the maintainer decision of
  2026-10-26 (host-only node naming everywhere, including fee-service URLs,
  except the RPC wire dump, which keeps its own secrets switch), his decision
  of 2026-10-28 on D35.10 and this repository's own code. They take no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required.
- *Clean-context addition (2026-10-29, forbidden corpus not consulted):* the
  *Shared HTTP driver* paragraph of the R35.1.17 status note is
  Reloaded-original. It records the maintainer decision of 2026-10-29 to
  name the host of an abandoned request, from this repository's own code, so
  chapter 01 R38 is not engaged. The same holds for the *Errors of the shared
  HTTP layer* paragraph, which applies R35.1.17 to the errors of that layer.
