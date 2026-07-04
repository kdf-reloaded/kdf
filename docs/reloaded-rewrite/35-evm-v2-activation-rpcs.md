# Chapter 35 -- EVM (Ethereum) V2 Activation & Token RPC Surface

**Status:** driving-spec (required port). This chapter specifies an **activation
RPC layer** over EVM coin code that already exists in reloaded; it does not
introduce a new coin.

> **One-sentence claim:** the project shall expose the standard Komodo DeFi
> Framework **mmrpc 2.0** activation surface for EVM (Ethereum-family) coins --
> a one-shot "platform coin with tokens" call, a single-token call, a
> long-running task variant, an on-chain token-info query, and read/write of the
> per-coin swap gas-fee policy -- on top of the EVM coin type that reloaded
> already ships.

> **Treatment:** **T-PORT.** The EVM coin type and its legacy `enable` path are
> present in reloaded (see §35.7). What is required is the **activation
> integration** (the platform-coin-with-tokens and token activation trait
> implementations for the EVM coin/token, plus the dispatcher routes for the
> method strings named below). The wire contract distilled here is the source of
> truth for that port.

> **Binding scope (R36).** Requirements bind observable behaviour, the public
> mmrpc-2.0 method strings and their request/response JSON field names, and
> externally *dictated* interop (EVM JSON-RPC, EIP-1559 fee semantics, ERC-20
> contract ABI calls, CAIP-style chain identification). Those public contracts
> are the source of truth, not this project's code. Private Rust types, helper
> decomposition, and internal module structure are informative and are **not**
> bound by this chapter.

> **Source of truth (informative).** The method strings, JSON field names, and
> error discriminants below are the published Komodo DeFi Framework API contract
> (the public KDF API documentation / `komodo-docs` coin-activation and
> EVM/token sections). Where this chapter and the public API docs disagree, the
> public API docs govern.

---

## 35.0 Executive summary

EVM coins follow the project's **platform-coin-with-tokens** activation model:
the native gas coin (ETH and other EVM chains) is the *platform coin*, and ERC-20
contracts are its *child tokens*. The surface specified here is:

| Method string | Tier | Targets | Purpose |
| --- | --- | --- | --- |
| `enable_eth_with_tokens` | mmrpc 2.0 | all (incl. WASM) | activate the EVM platform coin plus an inline batch of ERC-20 tokens in one call |
| `enable_erc20` | mmrpc 2.0 | all (incl. WASM) | activate one ERC-20 token against an already-active EVM platform |
| `task::enable_eth::{init,status,user_action,cancel}` | mmrpc 2.0 (task) | all (incl. WASM) | long-running variant of EVM platform activation |
| `get_token_info` | mmrpc 2.0 | all | read an ERC-20 contract's on-chain info (symbol, decimals) |
| `get_swap_gas_fee_policy` | mmrpc 2.0 | all | read the per-coin swap gas-fee policy |
| `set_swap_gas_fee_policy` | mmrpc 2.0 | all | set the per-coin swap gas-fee policy |

All methods use the mmrpc-2.0 envelope (`{"mmrpc":"2.0","method":...,"params":
{...},"id":...}`) and, on success, return `{"mmrpc":"2.0","result":{...},"id":
...}`. On error they return the standard mmrpc-2.0 error envelope carrying
`error`, `error_path`, `error_trace`, `error_type`, and `error_data`; only the
**`error_type` discriminant** and its HTTP status are bound below (the human-
readable `error` text is not part of the contract).

EVM activation is available on **all targets, including WASM**. The browser
(WASM) build additionally supports an external-signer activation policy (see
R35.1.4).

---

## 35.1 `enable_eth_with_tokens` -- platform coin with tokens

R35.1.1 The public RPC `enable_eth_with_tokens` shall activate an EVM platform
coin together with an inline list of ERC-20 tokens in a single mmrpc-2.0 call.
Its `params` object shall carry:

- `ticker` (string, required) -- the platform coin ticker to activate.
- `nodes` (array, required, non-empty) -- the EVM JSON-RPC endpoints. Each entry
  is an object carrying `url` (string, required) and an optional boolean
  selecting whether the endpoint is reached through the project's proxy
  (defaulting to off).
- `erc20_tokens_requests` (array, optional, default empty) -- the ERC-20 tokens
  to enable alongside the platform. Each entry carries `ticker` (string,
  required) and the per-token activation parameters of R35.2.
- `swap_contract_address` (string, optional) -- the swap (HTLC) contract address
  used for atomic swaps on this chain.
- `fallback_swap_contract` (string, optional) -- a fallback swap contract
  address.
- `swap_v2_contracts` (object, optional) -- the V2 swap contract address set (see
  the swap-v2 EVM chapter, ch. 17) when V2 swaps are used.
- `contract_supports_watchers` (boolean, optional, default false) -- whether the
  configured swap contract supports the watcher protocol.
- `required_confirmations` (integer, optional) -- confirmations required for swap
  transactions on the platform coin.
- `priv_key_policy` (tagged object, optional) -- the signing policy (R35.1.4),
  defaulting to the in-context wallet private key.
- `path_to_address` (object, optional) -- HD account/chain/address selector
  picking which derived address is used for swaps (defaults to the first
  non-change address of the first account).
- `gap_limit` (integer, optional) -- HD address-scan gap limit.
- HD balance-enumeration parameters (optional), namely a `scan_policy`
  controlling whether/when previously unknown HD addresses are rescanned, and an
  optional minimum-addresses-to-scan count. These are flattened into `params`.
- `get_balances` (boolean, optional, default **true**) -- when true the result
  includes per-address balances; when false the balance maps may be omitted to
  speed up activation.
- an optional NFT-provider activation block (see the NFT chapter, ch. 19) for
  enabling NFT support on the platform at activation time.

R35.1.2 The activation flow shall: validate that `nodes` is non-empty and each
`url` is a syntactically valid URL; construct the multi-node EVM JSON-RPC client
with failover across the supplied nodes; resolve the chain identity from coin
configuration (the chain id is required for EVM coins/tokens); build the platform
coin; then activate each requested ERC-20 token against it. No EVM node endpoints
are embedded in the project -- the node list is entirely caller-supplied.

R35.1.3 The success `result` shall report, for the wallet's active address(es):
- `current_block` -- the current EVM chain height;
- the platform coin balance(s) keyed by address;
- the ERC-20 token balance(s) keyed by address; and
- the NFT ownership summary keyed by address (when NFT support is active).

The result is one of two shapes selected by wallet kind: a single-address
("Iguana") shape that reports the current block, a per-address platform-balance
map, a per-address token-balance map, and the NFT summary; or an HD-wallet shape
that reports the current block, the platform `ticker`, an aggregated wallet
balance report covering the platform coin and its tokens, and the NFT summary.
The two shapes are distinguished structurally (the response is an untagged
union), so a client matches on the fields present.

R35.1.4 The `priv_key_policy` selector is a tagged object (tag field `type`,
payload field `params`) over the published signing policies: the in-context
wallet key (default); a Trezor hardware policy; a WalletConnect policy carrying
the WalletConnect session topic; and, **on the WASM target only**, an external
browser-signer (MetaMask-style) policy. Hardware and external-signer policies
require the long-running task variant (§35.3) because they may need interactive
user actions.

R35.1.5 `error_type` discriminants for this method, and their HTTP status, form
the bound error contract. The discriminants shall include at least:

| `error_type` | HTTP status |
| --- | --- |
| `PlatformIsAlreadyActivated` | 400 |
| `PlatformConfigIsNotFound` | 400 |
| `UnexpectedPlatformProtocol` | 400 |
| `TokenConfigIsNotFound` | 400 |
| `UnexpectedTokenProtocol` | 400 |
| `InvalidPayload` | 400 |
| `AtLeastOneNodeRequired` | 400 |
| `UnexpectedDeviceActivationPolicy` | 400 |
| `PlatformCoinMismatch` | 400 |
| `CoinProtocolParseError` | 500 |
| `TokenProtocolParseError` | 500 |
| `PlatformCoinCreationError` | 500 |
| `PrivKeyPolicyNotAllowed` | 500 |
| `UnexpectedDerivationMethod` | 500 |
| `Transport` | 502 |
| `Internal` | 500 |

A request whose chain id is unset for an EVM coin/token, or whose swap-contract /
fallback-swap-contract / derivation-path values are malformed, shall be rejected
as an invalid-payload / invalid-address class error rather than partially
activating the coin.

---

## 35.2 `enable_erc20` -- single token on an active platform

R35.2.1 The public RPC `enable_erc20` shall activate a single ERC-20 token
against an already-active EVM platform coin. Its `params` shall carry:
- `ticker` (string, required) -- the token ticker;
- `protocol` (object, optional) -- an explicit coin-protocol override (R35.5
  shape); when omitted the protocol is taken from coin configuration;
- `activation_params` (object, required) -- the per-token parameters, currently
  an optional `required_confirmations` (integer) for the token's swaps.

R35.2.2 The token's platform binding, decimals, and on-chain contract address are
taken from coin configuration; activation does not introspect the contract's
`decimals()` -- configuration must declare it.

R35.2.3 The success `result` shall report:
- the token balance(s) keyed by address;
- the platform coin ticker the token is bound to;
- the token contract address; and
- the token's required confirmations.

R35.2.4 `enable_erc20` shares its activation handler with NFT activation; the
published alias `enable_nft` routes to the same handler with an NFT-shaped
protocol/params. This chapter binds the ERC-20 contract; NFT specifics are bound
by ch. 19.

R35.2.5 `error_type` discriminants and HTTP status for this method shall include
at least:

| `error_type` | HTTP status |
| --- | --- |
| `TokenIsAlreadyActivated` | 400 |
| `PlatformCoinIsNotActivated` | 400 |
| `TokenConfigIsNotFound` | 400 |
| `UnexpectedTokenProtocol` | 400 |
| `InvalidPayload` | 400 |
| `PlatformCoinMismatch` | 400 |
| `TokenProtocolParseError` | 500 |
| `UnsupportedPlatformCoin` | 500 |
| `UnexpectedDerivationMethod` | 500 |
| `CouldNotFetchBalance` | 500 |
| `InvalidConfig` | 500 |
| `Transport` | 500 |
| `PrivKeyPolicyNotAllowed` | 500 |
| `Internal` | 500 |

---

## 35.3 `task::enable_eth::{init,status,user_action,cancel}` -- task variant

R35.3.1 EVM platform activation shall also be exposed as the long-running
task-RPC family `task::enable_eth::init`, `task::enable_eth::status`,
`task::enable_eth::user_action`, and `task::enable_eth::cancel`, following the
same init/status/user_action/cancel pattern documented for `task::enable_utxo`,
`task::enable_qtum`, and the Z-coin task trio (ch. 39). `init` accepts the same
activation parameters as `enable_eth_with_tokens` (§35.1) and returns a task id;
`status` polls progress and ultimately yields the same success result shape as
§35.1.3; `cancel` aborts an in-flight activation.

R35.3.2 The task variant is the required path for activation policies that need
interactive user actions -- in particular the Trezor hardware policy. When the
wallet is hardware-backed, `status` shall surface in-progress states asking the
user to connect the device and to confirm the public key, and the confirmation
shall be supplied via `task::enable_eth::user_action`.

R35.3.3 `status` shall report observable in-progress states covering at least
activating the coin, requesting balances, and completion, plus the hardware
interaction states of R35.3.2 when applicable. A task that exceeds its activation
deadline shall fail with a timeout-class `error_type`.

R35.3.4 A parallel single-token task family
`task::enable_erc20::{init,status,user_action,cancel}` shall exist as the task
variant of `enable_erc20`. Its `init` parameters extend the single-token
parameters of §35.2 with the HD balance-enumeration parameters and the
`path_to_address` selector (so HD wallets can pick the swap address); its result
matches §35.2.3.

R35.3.5 The task-variant error contract reuses the platform-activation
discriminants of R35.1.5 (for `task::enable_eth`) and the single-token
discriminants of R35.2.5 (for `task::enable_erc20`), plus task-framework
discriminants for unknown-task and task-timeout conditions.

R35.3.6 The `task::enable_eth` family is delivered by the shared **platform-coin
task-activation framework of ch. 48**, which wraps the one-shot
`enable_eth_with_tokens` activation of §35.1 as its unit of work (so the one-shot
and task variants share a single activation path). Under reloaded's shipped EVM
signing policies -- local/context (Iguana, HD) and, on WASM, MetaMask (ch. 47) --
activation completes without any `user_action`; the `user_action` method is
routed for wire parity and for the hardware (Trezor) policy that the published
surface targets (ch. 48 §48.6).

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
override by `enable_erc20`) follows the project's public coin-protocol JSON
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
- `Legacy` -- pre-EIP-1559 single gas-price pricing (the default);
- `Low` -- low EIP-1559 priority-fee tier;
- `Medium` -- medium EIP-1559 priority-fee tier;
- `High` -- high EIP-1559 priority-fee tier.

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
by EIP-1559 fee-history semantics, not by this chapter.

---

## 35.7 Implementation-substrate note (informative)

This note orients the implementer; it is not normative.

**Already present in reloaded:**
- the EVM coin type (the public `EthCoin` newtype and its `MmCoinEnum` variant),
  its coin-type enumeration (native ETH vs. ERC-20 vs. NFT), and the legacy
  builder that creates an EVM coin from coin config + request
  (`eth_coin_from_conf_and_request`);
- ETH/ERC-20 are activatable today only via the **legacy `enable`** RPC;
- the **generic** V2 activation entrypoints exist and are already used for other
  platforms -- the generic platform-coin-with-tokens activator and the generic
  single-token activator (the same machinery that drives BCH/SLP and Solana/SPL
  activation today).

**What must be added (scope of this chapter's port):**
- implement the **platform-coin-with-tokens activation trait** for the EVM
  platform coin and the **token activation traits** (one-shot and task) for the
  ERC-20 token, so the generic activators above can drive EVM activation,
  including the platform/token request and result shapes of §35.1--§35.3;
- add the **dispatcher routes** for the method strings in §35.0 so the mmrpc-2.0
  layer reaches these handlers;
- wire `get_token_info`, `get_swap_gas_fee_policy`, and `set_swap_gas_fee_policy`
  handlers and routes;
- ensure the surface compiles and is routed on **all targets including WASM**
  (the WASM-only external-signer policy of R35.1.4 is gated to the WASM target).

The placement of these implementations (which crate/module they live in) should
follow the existing activation layout used by the other platform coins; their
internal decomposition is the implementer's choice.

---

## 35.8 Acceptance criteria

- `enable_eth_with_tokens` activates an EVM platform coin plus zero or more
  ERC-20 tokens in one call and returns `current_block` together with per-address
  platform and token balances (and the NFT summary when enabled), in either the
  single-address or HD-wallet result shape (R35.1).
- A malformed request (empty `nodes`, invalid node `url`, unset chain id, or a
  malformed swap-contract / derivation-path value) is rejected with the bound
  `error_type` and HTTP status of R35.1.5 without partially activating the coin.
- `enable_erc20` adds a single ERC-20 token to an already-active EVM platform and
  returns the token balances, platform ticker, contract address, and required
  confirmations (R35.2); `enable_nft` reaches the same handler (R35.2.4).
- `task::enable_eth::{init,status,user_action,cancel}` activates the EVM platform
  with the same parameters and result as the one-shot call, and the Trezor policy
  drives the connect/confirm user-action states through `status` /`user_action`
  (R35.3).
- `get_token_info` returns the ERC-20 contract `symbol` and `decimals` (plus the
  configured ticker when known) for an active EVM platform, and rejects
  non-token / non-EVM protocols with the bound errors (R35.4).
- `get_swap_gas_fee_policy` / `set_swap_gas_fee_policy` round-trip a policy in the
  set `{Legacy, Low, Medium, High}` for an EVM coin, use exactly those method
  strings and the `swap_gas_fee_policy` field, and reject unsupported coins
  (R35.6).
- The entire surface builds and is routed on native **and** WASM targets, with
  the external-signer activation policy available only on WASM (R35.1.4, R35.7).
