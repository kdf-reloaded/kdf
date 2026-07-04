# RPC Method-Name Census (upstream parity reference)

## Scope & method

This document is a **wire-level method-name identifier census**. Every string listed
below is an externally observable JSON-RPC `method` value that the upstream Komodo DeFi
Framework dispatchers route to a handler, extracted from the upstream corpus snapshot's
two dispatch entry points (the v2 dispatcher and the legacy/v1 dispatcher) and every
sub-namespace router they delegate to (`task::`, `stream::`, `gui_storage::`,
`experimental::` and its children `staking::` / `liquidity_routing::` / `1inch_v6_0::`,
and `lightning::`).

Method-name strings are **dictated interop / public wire contract**, not protected
expression. This census deliberately contains **only**: the exact wire method strings,
the dispatch surface each lives on, its compile-time platform gate, a short neutral
functional label, and cross-name alias notes. It contains **no** handler bodies, private
identifiers, control flow, error/log literals, or internal module structure.

Platform gate legend:
- **ALL** — routed on every build target.
- **NATIVE_ONLY** — compiled only for non-WASM targets (`#[cfg(not(target_arch = "wasm32"))]`).
- **WASM_ONLY** — compiled only for the WASM target (`#[cfg(target_arch = "wasm32")]`).
- **NATIVE_NON_WINDOWS** — native targets excluding Windows.

Protocol-version note: the framework exposes two request envelopes. The legacy/v1
envelope routes through the LEGACY surface; the `mmrpc: "2.0"` envelope routes through the
V2 surface and all its sub-namespace routers. A handful of method names exist on **both**
surfaces (same wire name, different envelope) — these are flagged in the alias column as
`v1+v2 (envelope)`.

---

## Surface: V2 (flat methods, `mmrpc: "2.0"`)

| method string | platform gate | functional label | alias-of (other names for same function) |
|---|---|---|---|
| `account_balance` | ALL | account balance query | task variant `task::account_balance::*` |
| `active_swaps` | ALL | list active swaps | `active_swaps` on LEGACY (v1+v2 envelope) |
| `add_node_to_version_stat` | ALL | register node for version stats | — |
| `approve_token` | ALL | approve token spending allowance | — |
| `get_token_allowance` | ALL | query token spending allowance | — |
| `best_orders` | ALL | best orders for coin | `best_orders` on LEGACY (v1+v2 envelope) |
| `clear_nft_db` | ALL | clear NFT database | — |
| `consolidate_utxos` | ALL | consolidate UTXOs | — |
| `delete_wallet` | ALL | delete stored wallet | — |
| `enable_bch_with_tokens` | ALL | activate BCH platform with tokens | — |
| `enable_slp` | ALL | activate SLP token | — |
| `enable_eth_with_tokens` | ALL | activate ETH platform with tokens | task variant `task::enable_eth::*` |
| `enable_erc20` | ALL | activate ERC20 token | alias `enable_nft`; task variant `task::enable_erc20::*` |
| `enable_nft` | ALL | activate NFT (ETH token) | alias `enable_erc20` (same handler) |
| `enable_sia` | ALL | activate Sia coin | task variant `task::enable_sia::*` |
| `enable_tendermint_with_assets` | ALL | activate Tendermint platform with assets | task variant `task::enable_tendermint::*` |
| `enable_tendermint_token` | ALL | activate Tendermint token | — |
| `fetch_utxos` | ALL | fetch UTXO set | — |
| `get_current_mtp` | ALL | current median time past | — |
| `get_enabled_coins` | ALL | list enabled coins | `get_enabled_coins` on LEGACY (v1+v2 envelope) |
| `get_locked_amount` | ALL | locked balance amount | — |
| `get_mnemonic` | ALL | retrieve wallet mnemonic | — |
| `get_my_address` | ALL | own address for coin | — |
| `get_new_address` | ALL | derive new address | task variant `task::get_new_address::*` |
| `get_private_keys` | ALL | export private keys | — |
| `get_nft_list` | ALL | list owned NFTs | — |
| `get_nft_metadata` | ALL | NFT metadata query | — |
| `get_nft_transfers` | ALL | NFT transfer history | — |
| `get_public_key` | ALL | own public key | — |
| `get_public_key_hash` | ALL | own public key hash | — |
| `get_raw_transaction` | ALL | raw transaction by hash | — |
| `get_shared_db_id` | ALL | shared database identifier | — |
| `get_token_info` | ALL | token contract info | — |
| `get_wallet_names` | ALL | list stored wallet names | — |
| `max_maker_vol` | ALL | maximum maker volume | — |
| `my_recent_swaps` | ALL | recent swaps list | `my_recent_swaps` on LEGACY (v1+v2 envelope) |
| `my_swap_status` | ALL | swap status by uuid | `my_swap_status` on LEGACY (v1+v2 envelope) |
| `my_tx_history` | ALL | transaction history | `my_tx_history` on LEGACY (v1+v2 envelope) |
| `orderbook` | ALL | orderbook for pair | `orderbook` on LEGACY (v1+v2 envelope) |
| `recreate_swap_data` | ALL | reconstruct swap data | — |
| `refresh_nft_metadata` | ALL | refresh NFT metadata | — |
| `remove_node_from_version_stat` | ALL | unregister node from version stats | — |
| `sign_message` | ALL | sign message | — |
| `sign_raw_transaction` | ALL | sign raw transaction | — |
| `start_simple_market_maker_bot` | ALL | start market maker bot | — |
| `start_version_stat_collection` | ALL | start version stat collection | — |
| `stop_simple_market_maker_bot` | ALL | stop market maker bot | — |
| `stop_version_stat_collection` | ALL | stop version stat collection | — |
| `trade_preimage` | ALL | trade preimage estimate | `trade_preimage` on LEGACY (v1+v2 envelope) |
| `trezor_connection_status` | ALL | Trezor connection status | — |
| `update_nft` | ALL | update NFT records | — |
| `change_mnemonic_password` | ALL | change mnemonic password | — |
| `update_version_stat_collection` | ALL | update version stat collection | — |
| `verify_message` | ALL | verify signed message | — |
| `withdraw` | ALL | withdraw funds | `withdraw` on LEGACY (v1+v2 envelope); task variant `task::withdraw::*` |
| `peer_connection_healthcheck` | ALL | peer connection healthcheck | — |
| `withdraw_nft` | ALL | withdraw NFT | — |
| `get_eth_estimated_fee_per_gas` | ALL | estimated ETH gas fee | — |
| `get_swap_gas_fee_policy` | ALL | read swap gas fee policy | — |
| `set_swap_gas_fee_policy` | ALL | set swap gas fee policy | — |
| `send_asked_data` | ALL | supply requested data | — |
| `z_coin_tx_history` | ALL | Z-coin transaction history | — |
| `wc_new_connection` | ALL | WalletConnect new connection | — |
| `wc_get_session` | ALL | WalletConnect get session | — |
| `wc_get_sessions` | ALL | WalletConnect list sessions | — |
| `wc_delete_session` | ALL | WalletConnect delete session | — |
| `wc_ping_session` | ALL | WalletConnect ping session | — |

---

## Surface: V2 task router (`task::` prefix, init/status/user_action/cancel families)

The full wire string is `task::<family>::<action>`. Actions present per family are listed
explicitly below; not every family has all four siblings.

| method string | platform gate | functional label | alias-of (other names for same function) |
|---|---|---|---|
| `task::account_balance::init` | ALL | task: account balance (start) | flat `account_balance` |
| `task::account_balance::status` | ALL | task: account balance (status) | — |
| `task::account_balance::cancel` | ALL | task: account balance (cancel) | — |
| `task::create_new_account::init` | ALL | task: create account (start) | — |
| `task::create_new_account::status` | ALL | task: create account (status) | — |
| `task::create_new_account::user_action` | ALL | task: create account (user action) | — |
| `task::create_new_account::cancel` | ALL | task: create account (cancel) | — |
| `task::enable_bch::init` | ALL | task: activate BCH (start) | — |
| `task::enable_bch::status` | ALL | task: activate BCH (status) | — |
| `task::enable_bch::user_action` | ALL | task: activate BCH (user action) | — |
| `task::enable_bch::cancel` | ALL | task: activate BCH (cancel) | — |
| `task::enable_qtum::init` | ALL | task: activate QTUM (start) | — |
| `task::enable_qtum::status` | ALL | task: activate QTUM (status) | — |
| `task::enable_qtum::user_action` | ALL | task: activate QTUM (user action) | — |
| `task::enable_qtum::cancel` | ALL | task: activate QTUM (cancel) | — |
| `task::enable_utxo::init` | ALL | task: activate UTXO (start) | — |
| `task::enable_utxo::status` | ALL | task: activate UTXO (status) | — |
| `task::enable_utxo::user_action` | ALL | task: activate UTXO (user action) | — |
| `task::enable_utxo::cancel` | ALL | task: activate UTXO (cancel) | — |
| `task::enable_eth::init` | ALL | task: activate ETH (start) | flat `enable_eth_with_tokens` |
| `task::enable_eth::status` | ALL | task: activate ETH (status) | — |
| `task::enable_eth::user_action` | ALL | task: activate ETH (user action) | — |
| `task::enable_eth::cancel` | ALL | task: activate ETH (cancel) | — |
| `task::enable_erc20::init` | ALL | task: activate ERC20 (start) | flat `enable_erc20` |
| `task::enable_erc20::status` | ALL | task: activate ERC20 (status) | — |
| `task::enable_erc20::user_action` | ALL | task: activate ERC20 (user action) | — |
| `task::enable_erc20::cancel` | ALL | task: activate ERC20 (cancel) | — |
| `task::enable_tendermint::init` | ALL | task: activate Tendermint (start) | flat `enable_tendermint_with_assets` |
| `task::enable_tendermint::status` | ALL | task: activate Tendermint (status) | — |
| `task::enable_tendermint::user_action` | ALL | task: activate Tendermint (user action) | — |
| `task::enable_tendermint::cancel` | ALL | task: activate Tendermint (cancel) | — |
| `task::get_new_address::init` | ALL | task: derive address (start) | flat `get_new_address` |
| `task::get_new_address::status` | ALL | task: derive address (status) | — |
| `task::get_new_address::user_action` | ALL | task: derive address (user action) | — |
| `task::get_new_address::cancel` | ALL | task: derive address (cancel) | — |
| `task::scan_for_new_addresses::init` | ALL | task: scan addresses (start) | — |
| `task::scan_for_new_addresses::status` | ALL | task: scan addresses (status) | — |
| `task::scan_for_new_addresses::cancel` | ALL | task: scan addresses (cancel) | — |
| `task::init_trezor::init` | ALL | task: init Trezor (start) | — |
| `task::init_trezor::status` | ALL | task: init Trezor (status) | — |
| `task::init_trezor::user_action` | ALL | task: init Trezor (user action) | — |
| `task::init_trezor::cancel` | ALL | task: init Trezor (cancel) | — |
| `task::withdraw::init` | ALL | task: withdraw (start) | flat `withdraw` |
| `task::withdraw::status` | ALL | task: withdraw (status) | — |
| `task::withdraw::user_action` | ALL | task: withdraw (user action) | — |
| `task::withdraw::cancel` | ALL | task: withdraw (cancel) | — |
| `task::enable_sia::init` | ALL | task: activate Sia (start) | flat `enable_sia` |
| `task::enable_sia::status` | ALL | task: activate Sia (status) | — |
| `task::enable_sia::user_action` | ALL | task: activate Sia (user action) | — |
| `task::enable_sia::cancel` | ALL | task: activate Sia (cancel) | — |
| `task::enable_z_coin::init` | ALL | task: activate Z-coin (start) | — |
| `task::enable_z_coin::status` | ALL | task: activate Z-coin (status) | — |
| `task::enable_z_coin::user_action` | ALL | task: activate Z-coin (user action) | — |
| `task::enable_z_coin::cancel` | ALL | task: activate Z-coin (cancel) | — |
| `task::enable_lightning::init` | NATIVE_ONLY | task: activate Lightning (start) | — |
| `task::enable_lightning::status` | NATIVE_ONLY | task: activate Lightning (status) | — |
| `task::enable_lightning::user_action` | NATIVE_ONLY | task: activate Lightning (user action) | — |
| `task::enable_lightning::cancel` | NATIVE_ONLY | task: activate Lightning (cancel) | — |
| `task::connect_metamask::init` | WASM_ONLY | task: connect MetaMask (start) | — |
| `task::connect_metamask::status` | WASM_ONLY | task: connect MetaMask (status) | — |
| `task::connect_metamask::cancel` | WASM_ONLY | task: connect MetaMask (cancel) | — |

---

## Surface: STREAM (`stream::` prefix, SSE streamer activation)

| method string | platform gate | functional label | alias-of (other names for same function) |
|---|---|---|---|
| `stream::balance::enable` | ALL | enable balance event stream | — |
| `stream::network::enable` | ALL | enable network event stream | — |
| `stream::heartbeat::enable` | ALL | enable heartbeat event stream | — |
| `stream::fee_estimator::enable` | ALL | enable fee estimator stream | — |
| `stream::swap_status::enable` | ALL | enable swap status stream | — |
| `stream::order_status::enable` | ALL | enable order status stream | — |
| `stream::tx_history::enable` | ALL | enable tx history stream | — |
| `stream::orderbook::enable` | ALL | enable orderbook stream | — |
| `stream::shutdown_signal::enable` | NATIVE_NON_WINDOWS | enable shutdown signal stream | — |
| `stream::disable` | ALL | disable a streamer | — |

---

## Surface: GUI_STORAGE (`gui_storage::` prefix)

| method string | platform gate | functional label | alias-of (other names for same function) |
|---|---|---|---|
| `gui_storage::activate_coins` | ALL | mark coins activated | — |
| `gui_storage::add_account` | ALL | add GUI account | — |
| `gui_storage::deactivate_coins` | ALL | mark coins deactivated | — |
| `gui_storage::delete_account` | ALL | delete GUI account | — |
| `gui_storage::enable_account` | ALL | set enabled account | — |
| `gui_storage::get_accounts` | ALL | list GUI accounts | — |
| `gui_storage::get_account_coins` | ALL | list account coins | — |
| `gui_storage::get_enabled_account` | ALL | get enabled account | — |
| `gui_storage::set_account_balance` | ALL | set account balance | — |
| `gui_storage::set_account_description` | ALL | set account description | — |
| `gui_storage::set_account_name` | ALL | set account name | — |

---

## Surface: LIGHTNING (`lightning::` prefix)

Entire namespace is **NATIVE_ONLY**.

| method string | platform gate | functional label | alias-of (other names for same function) |
|---|---|---|---|
| `lightning::channels::close_channel` | NATIVE_ONLY | close Lightning channel | — |
| `lightning::channels::get_channel_details` | NATIVE_ONLY | channel details | — |
| `lightning::channels::get_claimable_balances` | NATIVE_ONLY | claimable channel balances | — |
| `lightning::channels::list_closed_channels_by_filter` | NATIVE_ONLY | list closed channels (filter) | — |
| `lightning::channels::list_open_channels_by_filter` | NATIVE_ONLY | list open channels (filter) | — |
| `lightning::channels::open_channel` | NATIVE_ONLY | open Lightning channel | — |
| `lightning::channels::update_channel` | NATIVE_ONLY | update Lightning channel | — |
| `lightning::nodes::add_trusted_node` | NATIVE_ONLY | add trusted node | — |
| `lightning::nodes::connect_to_node` | NATIVE_ONLY | connect to node | — |
| `lightning::nodes::list_trusted_nodes` | NATIVE_ONLY | list trusted nodes | — |
| `lightning::nodes::remove_trusted_node` | NATIVE_ONLY | remove trusted node | — |
| `lightning::payments::generate_invoice` | NATIVE_ONLY | generate Lightning invoice | — |
| `lightning::payments::get_payment_details` | NATIVE_ONLY | payment details | — |
| `lightning::payments::list_payments_by_filter` | NATIVE_ONLY | list payments (filter) | — |
| `lightning::payments::send_payment` | NATIVE_ONLY | send Lightning payment | — |

---

## Surface: OTHER — `experimental::` (unstable APIs, V2 envelope)

Includes the `experimental::` direct methods plus its child routers
`staking::`, `liquidity_routing::`, and `1inch_v6_0::`.

| method string | platform gate | functional label | alias-of (other names for same function) |
|---|---|---|---|
| `experimental::enable_solana_with_assets` | ALL | activate Solana platform with assets | — |
| `experimental::enable_solana_token` | ALL | activate Solana token | — |
| `experimental::staking::claim_rewards` | ALL | claim staking rewards | — |
| `experimental::staking::delegate` | ALL | delegate stake | — |
| `experimental::staking::undelegate` | ALL | undelegate stake | — |
| `experimental::staking::query::delegations` | ALL | query delegations | — |
| `experimental::staking::query::ongoing_undelegations` | ALL | query ongoing undelegations | — |
| `experimental::staking::query::validators` | ALL | query validators | — |
| `experimental::liquidity_routing::find_best_quote` | ALL | find best routed quote | — |
| `experimental::liquidity_routing::get_quotes_for_tokens` | ALL | quotes for tokens | — |
| `experimental::liquidity_routing::execute_routed_trade` | ALL | execute routed trade | — |
| `experimental::1inch_v6_0::classic_swap_contract` | ALL | 1inch classic swap contract | — |
| `experimental::1inch_v6_0::classic_swap_quote` | ALL | 1inch classic swap quote | — |
| `experimental::1inch_v6_0::classic_swap_create` | ALL | 1inch classic swap create | — |
| `experimental::1inch_v6_0::classic_swap_liquidity_sources` | ALL | 1inch liquidity sources | — |
| `experimental::1inch_v6_0::classic_swap_tokens` | ALL | 1inch swap tokens | — |

---

## Surface: LEGACY (v1 envelope, flat method names)

| method string | platform gate | functional label | alias-of (other names for same function) |
|---|---|---|---|
| `active_swaps` | ALL | list active swaps | `active_swaps` on V2 (v1+v2 envelope) |
| `all_swaps_uuids_by_filter` | ALL | swap uuids by filter | — |
| `ban_pubkey` | ALL | ban pubkey | — |
| `best_orders` | ALL | best orders for coin | `best_orders` on V2 (v1+v2 envelope) |
| `buy` | ALL | place buy order | — |
| `cancel_all_orders` | ALL | cancel all orders | — |
| `cancel_order` | ALL | cancel order | — |
| `coins_needed_for_kick_start` | ALL | coins needing kick start | — |
| `convertaddress` | ALL | convert address format | — |
| `convert_utxo_address` | ALL | convert UTXO address | — |
| `disable_coin` | ALL | disable coin | — |
| `electrum` | ALL | activate coin via Electrum | — |
| `enable` | ALL | activate coin (native node) | — |
| `get_enabled_coins` | ALL | list enabled coins | `get_enabled_coins` on V2 (v1+v2 envelope) |
| `get_directly_connected_peers` | ALL | directly connected peers | — |
| `get_gossip_mesh` | ALL | gossip mesh peers | — |
| `get_gossip_peer_topics` | ALL | gossip peer topics | — |
| `get_gossip_topic_peers` | ALL | gossip topic peers | — |
| `get_my_peer_id` | ALL | own peer id | — |
| `get_relay_mesh` | ALL | relay mesh peers | — |
| `get_trade_fee` | ALL | trade fee for coin | — |
| `help` | ALL | list legacy methods | — |
| `import_swaps` | ALL | import swap records | — |
| `kmd_rewards_info` | ALL | KMD rewards info | — |
| `list_banned_pubkeys` | ALL | list banned pubkeys | — |
| `max_taker_vol` | ALL | maximum taker volume | — |
| `metrics` | ALL | runtime metrics | — |
| `min_trading_vol` | ALL | minimum trading volume | — |
| `my_balance` | ALL | coin balance | — |
| `my_orders` | ALL | own orders | — |
| `my_recent_swaps` | ALL | recent swaps list | `my_recent_swaps` on V2 (v1+v2 envelope) |
| `my_swap_status` | ALL | swap status by uuid | `my_swap_status` on V2 (v1+v2 envelope) |
| `my_tx_history` | ALL | transaction history | `my_tx_history` on V2 (v1+v2 envelope) |
| `orders_history_by_filter` | ALL | order history by filter | — |
| `order_status` | ALL | order status | — |
| `orderbook` | ALL | orderbook for pair | `orderbook` on V2 (v1+v2 envelope) |
| `orderbook_depth` | ALL | orderbook depth | — |
| `recover_funds_of_swap` | ALL | recover swap funds | — |
| `sell` | ALL | place sell order | — |
| `show_priv_key` | ALL | show private key | — |
| `send_raw_transaction` | ALL | broadcast raw transaction | — |
| `set_required_confirmations` | ALL | set required confirmations | — |
| `set_requires_notarization` | ALL | set notarization requirement | — |
| `setprice` | ALL | set maker price | — |
| `stats_swap_status` | ALL | stats swap status | — |
| `stop` | ALL | stop the framework | — |
| `trade_preimage` | ALL | trade preimage estimate | `trade_preimage` on V2 (v1+v2 envelope) |
| `unban_pubkeys` | ALL | unban pubkeys | — |
| `update_maker_order` | ALL | update maker order | — |
| `validateaddress` | ALL | validate address | — |
| `version` | ALL | framework version | — |
| `withdraw` | ALL | withdraw funds | `withdraw` on V2 (v1+v2 envelope) |

---

## Alias / version-drift summary (the parity-critical set)

Functional concepts reachable under **more than one** wire method-name string. A downstream
parity diff must preserve every name in each group, not just one.

1. **`active_swaps`** — LEGACY `active_swaps` + V2 `active_swaps`.
2. **`best_orders`** — LEGACY `best_orders` + V2 `best_orders`.
3. **`get_enabled_coins`** — LEGACY `get_enabled_coins` + V2 `get_enabled_coins`.
4. **`my_recent_swaps`** — LEGACY `my_recent_swaps` + V2 `my_recent_swaps`.
5. **`my_swap_status`** — LEGACY `my_swap_status` + V2 `my_swap_status`.
6. **`my_tx_history`** — LEGACY `my_tx_history` + V2 `my_tx_history`.
7. **`orderbook`** — LEGACY `orderbook` + V2 `orderbook`.
8. **`trade_preimage`** — LEGACY `trade_preimage` + V2 `trade_preimage`.
9. **`withdraw`** — LEGACY `withdraw` + V2 flat `withdraw` + V2 task family `task::withdraw::{init,status,user_action,cancel}`.
10. **ERC20/NFT activation** — V2 flat `enable_erc20` **and** V2 flat `enable_nft` route to the same activation handler; the same concept also has a task family `task::enable_erc20::{init,status,user_action,cancel}`.
11. **`get_new_address`** — V2 flat `get_new_address` + V2 task family `task::get_new_address::{init,status,user_action,cancel}`.
12. **`account_balance`** — V2 flat `account_balance` + V2 task family `task::account_balance::{init,status,cancel}`.
13. **ETH activation** — V2 flat `enable_eth_with_tokens` + V2 task family `task::enable_eth::{init,status,user_action,cancel}`.
14. **Sia activation** — V2 flat `enable_sia` + V2 task family `task::enable_sia::{init,status,user_action,cancel}`.
15. **Tendermint activation** — V2 flat `enable_tendermint_with_assets` + V2 task family `task::enable_tendermint::{init,status,user_action,cancel}`.

> Note on groups 9–15: the flat name and the `task::`-namespaced family expose the *same
> functional capability* under different request shapes (one-shot vs. long-running task).
> They are interface aliases for parity purposes; a fork that ships only one of the two
> names for any of these concepts diverges from upstream and breaks at least one client.

---

## Distinct method-string totals per surface

| surface | distinct method strings |
|---|---|
| V2 (flat) | 67 |
| V2 task router | 61 |
| STREAM | 10 |
| GUI_STORAGE | 11 |
| LIGHTNING | 15 |
| OTHER (`experimental::` incl. children) | 16 |
| LEGACY | 52 |
| **TOTAL (all surfaces)** | **232** |
