# Coin activation: where the API reference and GLEEC KDF disagree

KDF Reloaded aims to answer coin-activation requests the way existing wallets
expect. Two outside sources describe that behavior: the public Komodo DeFi
Framework API reference (komodoplatform.com docs, called "the docs" below), and
the reference implementation, GLEEC KDF `v3.0.0-beta` (called "GLEEC KDF").
They do not always agree. This page lists every coin-activation case where they
disagree, and says which one Reloaded follows and why. It also lists the cases
where Reloaded deliberately differs from both.

The binding rules live in the CRD chapters cited in each row, mainly
[Chapter 55](reloaded-rewrite/55-coin-activation-wire-contract.md). This page
only summarizes them. It describes the behavior the chapters bind; the
"Reloaded status" notes in those chapters record what is not implemented yet.

## How Reloaded decides

Rules 1 and 2 are the maintainer's general rules. Rule 3 summarizes the maintainer's decisions on individual cases (`nft_req`, the Tendermint WalletConnect form); a new case of that kind still goes to the maintainer.

1. **Docs explicit: follow the docs.** Where the docs are explicit (the field
   table and the example agree, or only an example exists and nothing on the
   site contradicts it), Reloaded does what the docs say.
2. **Docs silent, ambiguous or self-contradictory: follow GLEEC KDF.** This
   includes every HTTP status code, because the docs never state one. Reloaded
   then does what GLEEC KDF `v3.0.0-beta` does. A chapter marks a docs error
   it corrects this way as a *public-docs erratum*.
3. **A request form only one side accepts: accept both.** If accepting both
   forms cannot break either kind of client, Reloaded accepts both.
4. **Deliberate Reloaded divergences are listed separately.** They are in the
   last section of this page, each with its compatibility switch where one
   exists. Switches are `MM2.json` keys; see
   [GLEEC_COMPATIBILITY.md](GLEEC_COMPATIBILITY.md) and
   [COMPAT_SWITCHES.md](COMPAT_SWITCHES.md).

Unless a row says otherwise, the legacy reference GLEEC KDF `v2.6.0-beta`
behaves the same as `v3.0.0-beta` for the cases below.

Shorthand: "string", "object" and "integer" are the JSON type of `error_data`.
"Status level" means the error is reported inside a `task::<family>::status`
answer (HTTP 200, `"status": "Error"`). "mmrpc level" means the error is the
response itself, with its own HTTP status.

## Envelope and task framework

These apply to every `task::` activation family.

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| HTTP status of any activation error | Never stated | Fixed status per `error_type` and method family | GLEEC statuses | Docs silent (rule 2) | R55.1.5 |
| `Transport` HTTP status | Not stated | 502 for platform-with-tokens methods, 500 for standalone, token and layer-2 methods | Same split | Docs silent (rule 2) | R55.1.5 |
| Status-level error placement | Some task pages' tables describe a `details.result` wrapper; every example puts the error object directly in `details` | Error object directly in `details` | Directly in `details` | Docs contradict themselves (rule 2); erratum | R55.1.6 |
| `cancel` error `error_data` type | Tables say string; every example shows an integer | Integer (the task id) | Integer | Docs contradict themselves (rule 2); erratum | R55.2.15 |
| `TaskFinished` on `cancel` | Example is headed as a success | mmrpc-level error, HTTP 409 | Error, HTTP 409 | Docs wrong (rule 2); erratum | R55.2.15 |

## EVM platform (`enable_eth_with_tokens`, `task::enable_eth`)

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| Every node unresponsive | `Transport`, string (example message "Failed to get client version for all nodes") | No probe before use; the failed chain-height read is answered as `Internal`, HTTP 500 | `Transport`, string, HTTP 502 | Docs explicit (rule 1); 502 is the platform-family status for `Transport` (rule 2) | R35.1.6, R55.3.2 |
| Node proxy flag name | CoinNode table says `komodo_auth`; every example uses `komodo_proxy` | Uses `komodo_proxy` | `komodo_proxy`; `komodo_auth` is ignored | Docs contradict themselves (rule 2); erratum | R35.1.1 |
| `nodes[].ws_url` | Shown in examples next to an HTTP `url` | Does not use it | Accepted and ignored | Docs show it but give it no effect (rule 2); erratum | R35.1.1 |
| `swap_v2_contracts` members | Links to a table copied from TokensRequest; the example shows the real members | Uses the example's members | The example's members | Docs contradict themselves (rule 2); erratum | R35.1.1 |
| WalletConnect `priv_key_policy` | `{"type":"WalletConnect","data":"<topic>"}`, with `data` typed as a list | Requires `{"type":"WalletConnect","params":{"session_topic":"<topic>"}}` | The GLEEC form today; the docs form will be accepted too once EVM WalletConnect signing is implemented (it is not available in this build) | One form per side (rule 3), as decided for Tendermint | R35.1.4, erratum (e) |
| `required_confirmations` default | 3 | Token config value, else the platform's value; for the platform, coin config value, else 1 | Platform: coin config, else 3; token: token config, else the platform's value | Maintainer decision: keep the configured value (as GLEEC) but fall back to the documented 3, the safer number and the rule the docs' BCH page states | R35.1.1, erratum (g) |
| `swap_contract_address`, `fallback_swap_contract` | Both required | Swap contract required unless the coin is wallet-only; fallback optional | GLEEC rule | Chapter lists the docs as an erratum (rule 2) | R35.1.1 |
| HD `path_to_address.chain` | Table says integer; examples use strings | Strings (`"External"`, `"Internal"`) | Strings | Docs contradict themselves (rule 2); erratum | R35.1.1 |
| `tx_history`, `requires_notarization`, gas-station fields | Documented, with an effect | Not parsed at all | Accepted with the documented effect; a malformed value is treated as absent | Docs explicit (rule 1) | R35.1.1a |
| Error names in the method table | `PrivKeyNotAllowed` and `InternalError` | `PrivKeyPolicyNotAllowed` and `Internal` (the docs' error catalogue agrees) | Catalogue names | Docs contradict themselves (rule 2) | R35.1.5, R36.1.7 |
| `task::enable_eth` `priv_key_policy` type | Task page types it as a string | A string is a request-parse failure; the object form is required | Object form | Docs contradict the method's own shape (rule 2) | R35.3.1 |
| `task::enable_eth` terminal errors | Lists `TaskTimedOut`, `CoinCreationError`, `HwError`, in a `details.result` wrapper | The platform errors directly in `details`; hardware failures as `Internal`; never `CoinCreationError` or `HwError` | GLEEC behavior | Docs incomplete and contradict the examples (rule 2); erratum | R35.3.5, R35.3.2 |
| `task::enable_eth` in-progress values | Lists `ActivatingCoin`, `RequestingWalletBalance`, `Finishing`, `WaitingForTrezorToConnect`, `FollowHwDeviceInstructions` | Reports only `ActivatingCoin` plus the hardware values | Draws from the docs list; may skip phases that do not apply | Docs explicit (rule 1) | R35.3.3 |

## ERC20 and NFT (`enable_erc20`, `task::enable_erc20`, `enable_nft`, `nft_req`)

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| `enable_erc20` `token_contract_address` case | Lower-case hex (example) | EIP-55 checksummed (mixed case); `v2.6.0-beta` used lower case | Lower case | Docs explicit (rule 1) | R35.2.3 |
| `task::enable_erc20` request, result and errors | Named only in the changelog; no request, result or errors documented | As bound, e.g. `TokenIsAlreadyActivated` with object `{ticker}`, `TokenProtocolParseError` at HTTP 400 | GLEEC behavior | Docs silent (rule 2) | R35.3A.1, R35.3A.3 |
| `task::enable_erc20` `token_contract_address` case | Not documented | EIP-55 checksummed | Checksummed (unlike the one-shot method) | Docs silent (rule 2) | R35.3A.5 |
| `nft_req` form | Bare `NftProvider` (table and example agree) | Requires the `{"provider": NftProvider}` wrapper | Accepts both | One form per side (rule 3); erratum | R35.1.4b |
| `nfts_infos` type | Documented as a list; every example is an object map | Object map | Object map | Docs contradict themselves (rule 2); erratum | R35.1.3 |

## Standalone coins and Trezor (`task::enable_utxo`, `task::enable_qtum`, `task::init_trezor`)

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| `priv_key_policy` form | Object `{"type": "ContextPrivKey"}` / `{"type": "Trezor"}` | Accepts only the bare string; rejects the object | Accepts both, with the same effect | One form per side (rule 3) | R55.5.11 |
| `task::enable_utxo` error set | Names only `TaskTimedOut`, `CoinCreationError`, `HwError` | The full standalone set | Full set | Docs incomplete (rule 2); erratum | R55.6.2, R55.5.4 |
| `task::enable_qtum` status formats | Defers to an overview page that has none | Standalone-family formats | Standalone-family formats | Docs silent (rule 2); erratum | R55.7.1 |
| `task::enable_qtum` HD fields | `gap_limit`, `min_addresses_number`, `scan_policy` marked required | Optional | Optional | Chapter lists the docs as an erratum (rule 2) | R55.7.1, R55.5.10 |
| `task::init_trezor` success details | Field table is flat; the status example wraps the fields in a `result` object | Flat in `details` | Flat | Docs contradict themselves (rule 2); erratum | R55.11.5 |

## Tendermint (`enable_tendermint_with_assets`, `enable_tendermint_token`, `task::enable_tendermint`)

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| `protocol` inside `error_data` for a UTXO coin without protocol data | `{"type": "UTXO"}`, no `protocol_data` key | Emits `"protocol_data": null` | Omits `protocol_data` | Docs explicit (rule 1) | R36.0.3 |
| `required_confirmations`, `requires_notarization`, `priv_key_policy` | Listed (defaults 3 and false; policy as a string) | Ignores all three | Accepted and ignored | Docs state no observable effect (rule 2) | R36.1.2 |
| WalletConnect request form in `activation_params` | `{"priv_key_policy": {"type": "WalletConnect", "data": "<topic>"}}` | Rejects it as a parse failure; accepts the `WithPubkey` form and `{"type": "WalletConnect", "params": {"session_topic": "<topic>"}}` | Will accept all three forms (deferred); until then see the interim divergence below | One form per side (rule 3) | R36.1.3, §36.7 |
| `tokens_balances` type | Table says an array of AddressInfo; every example shows a ticker-keyed map | Ticker-keyed map | Map | Docs contradict themselves (rule 2) | R36.1.5 |
| Every node unresponsive | Lists `Transport` among the error types, without an example | Never produces `Transport`; wraps node failures in `PlatformCoinCreationError` | `PlatformCoinCreationError`, object `{ticker, error}`, HTTP 500 (token method: `CouldNotFetchBalance`, string, 500) | Docs ambiguous (rule 2) | R36.1.6, R36.2.4 |
| Where `task::enable_tendermint` reports errors | Error examples are drawn as mmrpc-level answers from the one-shot path | Only the already-active check at `init` (mmrpc level); everything else at status level | GLEEC placement | Docs ambiguous (rule 2) | R36.6.3, R48.5.4 |

## Lightning (`task::enable_lightning`)

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| `activation_params.color` default | `2b6680` | `000000` | `2b6680` | Docs are explicit (rule 1); maintainer decision; cosmetic | R41.2.4 |
| Second `LightningActivationParams` definition | The page also defines one with `backup_password` and `node_pubkey` | Not part of the activation request | Not part of the request | Docs contradict themselves (rule 2) | R41.2.4 |
| Per-channel defaults key in the coin config | Spelled `our_channels_config` (table, first example) and `our_channels_configs` (second example) | Reads `our_channels_configs` | `our_channels_configs`; the other spelling may be accepted as an alias | Docs contradict themselves (rule 2) | R41.2.11 |

## Z-coin (`task::enable_z_coin`)

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| "No Zcash params" error placement | Shown once as a bare error object with no envelope, once inside a status `Error` | Only inside a status `Error` | Status level only | Docs contradict themselves (rule 2) | R39.10.11 |
| Z-coin initialization error names in the catalogue | Lists `HardwareWalletsAreNotSupportedYet`, `CouldNotGetBalance`, `CouldNotGetBlockCount` | Never on the wire; translated to the standalone set | Never emitted | Docs do not show them on the wire (rule 2) | R39.10.13 |
| In-progress values | List omits `UpdatingBlocksCache`, `BuildingWalletDb`, `TemporaryError` (examples show the first two as objects) and includes `FollowHwDeviceInstructions` | The Z-coin vocabulary, without `FollowHwDeviceInstructions` | GLEEC vocabulary; never `FollowHwDeviceInstructions` | Docs contradict their examples (rule 2) | R39.10.7, R39.10.8 |
| `get_balances` | Remarks that the result "structure may vary" | Ignores `get_balances` for this coin | Ignored; `wallet_balance` is always present | Docs ambiguous (rule 2) | R39.10.5 |

## Sia (`task::enable_sia`, `enable_sia`)

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| The whole activation surface | Not documented (no page on the stable or dev docs tree) | Defines method names, request, result, errors and statuses | GLEEC behavior | Docs silent (rule 2) | R46.1.1, R46.5.1, R46.5.2 |
| Sia error names in the catalogue | Lists a Sia initialization enum (`CouldNotGetBalance`, `CouldNotGetBlockCount`, `HardwareWalletsAreNotSupportedYet`) and build and config enums with placeholder variants | Never on the wire; translated to the standalone set | Never emitted | Docs do not show them on the wire (rule 2) | R46.5.3 |

For the HD `wallet_balance` shape, see the divergences below.

## Solana

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| The whole activation surface | Not documented (only the protocol names inside a published parse-error example) | `experimental::enable_solana_with_assets` and `experimental::enable_solana_token`, with `SOLANA` / `SOLANATOKEN` coin configuration | In this beta, Solana is switched off (`SOLANA_ACTIVATION_AVAILABLE` is `false`): `enable_solana_with_tokens` answers `PlatformCoinCreationError` and `enable_spl` answers `Internal`, both HTTP 500 with "Solana support is not available in this build", and nothing is registered. Planned to return in the next beta, when Reloaded will add the GLEEC methods and config forms as an extra compatibility surface (deferred); its own surface stays primary (see below) | Docs silent (rule 2); maintainer decisions (2026-10-11, 2026-10-26) | §40.1-§40.3, ch. 40 maintainer decision and beta availability note, §55.3 table |

## Legacy `enable` and `electrum`

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| EVM gas-station fields on legacy `enable` | Documents `gas_station_url`, `gas_station_decimals`, `gas_station_policy` | No longer reads them on this path | Accepts them and may honor them | Docs explicit (rule 1) | R55.4.4 |
| Success body fields | Some examples show `locked_by_swaps`, or omit `coin` / `unspendable_balance` | Exactly the fields of the docs' field table | Exactly the table's fields; no `locked_by_swaps` | Docs contradict themselves (rule 2); erratum | R55.4.5 |

## BCH and SLP (`task::enable_bch`, `enable_bch_with_tokens`, `enable_slp`)

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule |
| --- | --- | --- | --- | --- | --- |
| `enable_bch_with_tokens` with Electrum servers down | `PlatformCoinCreationError`, object `{ticker, error}` | Connects lazily, so it surfaces at the first block-height query as `Transport`, string, HTTP 502 | `PlatformCoinCreationError`, object, HTTP 500; detected before success is reported | Docs explicit (rule 1) | R55.3.2, R55.9.4 |
| `enable_bch_with_tokens` `utxo_merge_params`, `requires_notarization` | `utxo_merge_params` required; `requires_notarization` defaults to true | `utxo_merge_params` optional; `requires_notarization` defaults from the coin config | GLEEC behavior | `utxo_merge_params`: accepting its absence is a superset; `requires_notarization`: maintainer decision to follow the coin config | R55.9.2 |
| `enable_bch_with_tokens` `tx_history` | The field table lists it twice | Default `false` | Default `false` | Docs contradict themselves (rule 2) | R55.9.2 |
| `enable_bch_with_tokens` status | Marked deprecated | Still routed | Still routed | Docs do not remove it (rule 2) | R55.9.1 |
| `task::enable_bch` field placement | Field table puts the fields at the top level of `params`; the example nests them in `activation_params` | Nested in `activation_params` | Nested | Docs contradict themselves (rule 2); erratum | R55.8.3 |
| `task::enable_bch` `tx_history` | The field table lists it twice | Default `false` | Default `false` | Docs contradict themselves (rule 2) | R55.8.3 |
| `task::enable_bch` status | A deprecation banner links to the UTXO page | Current, routed method | Routed on every target | Docs ambiguous (rule 2); erratum | R55.8.1, R55.8.3 |

## Deliberate Reloaded divergences

In these cases Reloaded does what neither GLEEC KDF nor, where the docs say
anything, the docs do. A divergence that changes behavior has a switch where
the chapter names one. An additive extension (a method or spelling the other
side never sends) needs no switch, because a client written for GLEEC KDF never
uses it. The extensions are also listed in
[RELOADED_VS_GLEEC.md](../RELOADED_VS_GLEEC.md).

| Case | Public docs say | GLEEC KDF does | Reloaded does | Why | CRD rule | Switch |
| --- | --- | --- | --- | --- | --- | --- |
| Legacy `enable`: coin left registered after a failed balance read | Not stated | `electrum` deregisters the coin; `enable` leaves it registered, so a retry fails as already active | Deregisters in both methods, so a retry succeeds; no wire field changes | Makes the two methods consistent | R55.4.6 | `legacy_enable_keeps_coin_on_balance_failure` (default `false`; `true` restores GLEEC behavior, not recommended) |
| Sia `wallet_balance` under an HD wallet | Not documented | Always the single-address `"Iguana"` form, also under HD | HD per-account report (`"wallet_type": "HD"`) with the same single address and balance | Same balance shape as the other coins of an HD wallet | R46.2.4 | `sia_activation_single_address_balance` (default `false`; `true` restores the `"Iguana"` form) |
| Tendermint `activation_params` (interim) | Shows a WalletConnect form | Accepts the public-key and WalletConnect forms and honors them | Rejects any non-null `activation_params` with `PlatformCoinCreationError`, object, HTTP 500, until the forms are implemented | Never silently fall back to the hot local key when the caller asked for a device-backed or watch-only activation | §36.7 (overrides R36.1.3, R36.1.7) | None; removed when the Tendermint plan's item 1 lands |
| Tendermint malformed request input | Not stated | A malformed `WithPubkey` key aborts handling (panic); WalletConnect without a WalletConnect context aborts | `InvalidRequest` for the bad key; `PlatformCoinCreationError` for the missing context | A crash cannot be a parity target | R36.1.3, §36.7 | None |
| Cancelling the losing task of two concurrent activations | Not stated | Cancel removes whatever coin is registered under the ticker, including one another activation registered | Removes only a coin this task registered itself | Corrects the reference; no wire field changes | R55.5.8, R35.3A.7 | None |
| Solana `tx_history: true` (compatibility surface) | Not documented | Aborts handling of the request after the result is computed | The flag has no effect; activation proceeds | A crash cannot be a parity target | R40.2.5 | None |
| Z-coin activation on WebAssembly | Not stated | Activates on WASM | Routed on WASM, but ends with `CoinCreationError`: the WASM build has no shielded wallet database, so it cannot report a real balance, and a zero balance would misstate funds | Honest failure instead of a placeholder balance | R39.10.1, §39.10 status item 1 | None |
| EVM `required_confirmations` default (mmrpc 2.0 methods) | 3 | Coin config, else 1 | Coin config, else 3; legacy EVM `enable` keeps its own default | Maintainer decision: keep the configured value, fall back to the documented 3 | R35.1.1, erratum (g) | None |
| Z-coin `skip_sync_params` with an explicit `sync_params` and an existing block cache | Not stated | Rewinds and rescans from `sync_params` anyway | Resumes from the prior sync state, as the flag describes | Implements the flag's described intent | R39.6.2, §39.10.6 | None |
| Z-coin `{"date": T}` sync start | Not stated | Estimates the start height from `avg_blocktime`; with no start point and no local state, a missing `avg_blocktime` fails with `CoinCreationError` | Resolves the date against the backend's block timestamps | Only `first_sync_block.requested` can differ; wire shapes are unchanged | R39.8.0g | None |
| Z-coin Light-mode backend | The activation examples send `electrum_servers` | Uses Electrum for the transparent side, connected lazily | lightwalletd is the sole backend: a non-empty `light_wallet_d_servers` list is mandatory, `electrum_servers` is accepted and ignored, and no Electrum connection is opened. Every endpoint unreachable: `LightwalletdUnreachable` (`Transport`); every endpoint excluded, or a fresh wallet with no strict-majority branch: `LightwalletdExcluded` (`CoinCreationError`) | Maintainer decision (2026-10-11): no working ElectrumX provider remains for ARRR, and lightwalletd serves the transparent side too; accepting the field keeps deployed wallets' requests valid | R39.11.5, R39.11.6 | None (no working provider for the removed path) |
| TRX and TRC20 on legacy `enable` / `electrum` | Not stated | Rejects them; requires an mmrpc 2.0 activation | In this beta, refuses them too: TRON is switched off (`TRON_ACTIVATION_AVAILABLE` is `false`) and both methods answer "TRON support is not available in this build" (string error, HTTP 500). When TRON returns (planned for the next beta), activates them on the legacy path | Reloaded has no mmrpc 2.0 TRON activation yet; additive; switched off for this beta (maintainer decision, 2026-10-26) | R55.4.7, D55.5 | None (additive) |
| Solana primary activation surface | Not documented | Routes only the `experimental::` methods | `enable_solana_with_tokens` and `enable_spl` stay routed as the primary contract; in this beta both refuse every request (see [Solana](#solana)) | GLEEC marks its Solana activation experimental; additive | Ch. 40 maintainer decision | None (additive) |
| Flat activation aliases | Not stated | Routes none of them | Routes `init_utxo*`, `init_qtum*`, `init_trezor*`, `init_z_coin*` (no flat cancel), the one-shot `enable_lightning` and the `init_lightning*` / `cancel_init_lightning` aliases; each behaves like its `task::` method | Backward compatibility with baseline-era clients; uniform naming; additive | R55.6.3, R55.7.1, R55.11.1, R39.10.2, R41.2.2 | None (additive) |
| Z-coin scan-pacing spellings | Not stated | Not accepted | Also accepts `blocks_per_iteration`, `scan_interval` and `inter_iteration_interval_ms` | Superset of the reference wire; additive | R39.6.2, §39.10.2 | None (additive) |

## See also

- [Chapter 55](reloaded-rewrite/55-coin-activation-wire-contract.md): the
  shared wire contract, the precedence rule and the cross-family table of
  unreachable-backend outcomes.
- Per-family chapters:
  [35 EVM](reloaded-rewrite/35-evm-v2-activation-rpcs.md),
  [19 NFT](reloaded-rewrite/19-nft-module-layout.md),
  [48 task framework](reloaded-rewrite/48-platform-coin-task-activation.md),
  [36 Tendermint](reloaded-rewrite/36-tendermint-v2-activation-rpcs.md),
  [41 Lightning](reloaded-rewrite/41-lightning-network.md),
  [39 Z-coin](reloaded-rewrite/39-zcash---z_coin-shielded-coin.md),
  [46 Sia](reloaded-rewrite/46-sia-v2-activation-rpcs.md),
  [40 Solana](reloaded-rewrite/40-solana-coin.md),
  [47 MetaMask](reloaded-rewrite/47-metamask-integration.md).
- [GLEEC_COMPATIBILITY.md](GLEEC_COMPATIBILITY.md): every setting needed for
  GLEEC-equivalent operation.
- [RELOADED_VS_GLEEC.md](../RELOADED_VS_GLEEC.md): the catalogue of all
  differences from GLEEC KDF.
