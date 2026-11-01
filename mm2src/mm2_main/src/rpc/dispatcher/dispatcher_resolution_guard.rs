//! Regression guard for the v2 RPC dispatcher routing contract.
//!
//! This guard exists because a namespaced RPC surface (`task::enable_utxo::init`) was once
//! silently missing from the dispatcher, causing GUI activation to fail with `NoSuchMethod`.
//! These tests assert that every canonical namespaced wire method we commit to keeps a routing
//! entry in `dispatcher.rs`, so a namespace router or arm cannot be removed unnoticed.
//!
//! Scope: this verifies that the *route exists* (the method string is matched and dispatched).
//! It deliberately does not exercise handler logic — that is covered by handler-level tests.
//! The canonical wire names are tracked in `docs/reloaded-rewrite/rpc-method-census.md`.

/// The full source of the v2 dispatcher. The method-name literals asserted below live only in
/// this guard file (not in `dispatcher.rs`), so the substring checks are not self-satisfying.
const DISPATCHER_SOURCE: &str = include_str!("dispatcher.rs");

/// Asserts that `prefix` has a `strip_prefix` router and that every `arm` is routed under it.
fn assert_namespace_routed(prefix: &str, arms: &[&str]) {
    assert!(
        DISPATCHER_SOURCE.contains(&format!("strip_prefix(\"{prefix}\")")),
        "v2 dispatcher lost the `{prefix}` namespace router (strip_prefix missing)"
    );
    for arm in arms {
        assert!(
            DISPATCHER_SOURCE.contains(&format!("\"{arm}\"")),
            "v2 dispatcher lost routing for `{prefix}{arm}`"
        );
    }
}

#[test]
fn task_namespace_methods_are_routed() {
    assert_namespace_routed("task::", &[
        "enable_utxo::init",
        "enable_utxo::status",
        "enable_utxo::user_action",
        "enable_utxo::cancel",
        "enable_qtum::init",
        "enable_qtum::status",
        "enable_qtum::user_action",
        "enable_qtum::cancel",
        "enable_bch::init",
        "enable_bch::status",
        "enable_bch::user_action",
        "enable_bch::cancel",
        "enable_eth::init",
        "enable_eth::status",
        "enable_eth::user_action",
        "enable_eth::cancel",
        "enable_erc20::init",
        "enable_erc20::status",
        "enable_erc20::user_action",
        "enable_erc20::cancel",
        "enable_tendermint::init",
        "enable_tendermint::status",
        "enable_tendermint::user_action",
        "enable_tendermint::cancel",
        "enable_z_coin::init",
        "enable_z_coin::status",
        "enable_z_coin::user_action",
        "enable_z_coin::cancel",
        "enable_lightning::init",
        "enable_lightning::status",
        "enable_lightning::user_action",
        "enable_lightning::cancel",
        "init_trezor::init",
        "init_trezor::status",
        "init_trezor::user_action",
        "init_trezor::cancel",
        "account_balance::init",
        "account_balance::status",
        "create_new_account::init",
        "create_new_account::status",
        "create_new_account::user_action",
        "scan_for_new_addresses::init",
        "scan_for_new_addresses::status",
        "withdraw::init",
        "withdraw::status",
        "withdraw::user_action",
        "withdraw::cancel",
    ]);
}

#[test]
fn flat_task_aliases_stay_routed() {
    // The baseline flat aliases of the task methods follow the namespaced ones (CRD ch. 55 R55.6.3,
    // R55.7.1, R55.11.1).
    for method in [
        "init_utxo",
        "init_utxo_status",
        "init_utxo_user_action",
        "init_qtum",
        "init_qtum_status",
        "init_qtum_user_action",
        "init_trezor",
        "init_trezor_status",
        "init_trezor_user_action",
        "init_z_coin",
        "init_z_coin_status",
        "init_z_coin_user_action",
    ] {
        assert!(
            DISPATCHER_SOURCE.contains(&format!("\"{method}\"")),
            "v2 dispatcher lost routing for the flat task alias `{method}`"
        );
    }
}

#[test]
fn sia_v2_task_methods_are_routed() {
    // Sia (ch. 46) standalone-coin V2 task activation surface. Sia is not a
    // platform-with-tokens coin and is routed on all targets (native + WASM),
    // alongside the UTXO/Qtum task arms rather than in the native-only block.
    assert_namespace_routed("task::", &[
        "enable_sia::init",
        "enable_sia::status",
        "enable_sia::user_action",
        "enable_sia::cancel",
    ]);
    // The legacy flat `enable_sia` alias reaches the same `init` handler (§46.6).
    assert!(
        DISPATCHER_SOURCE.contains("\"enable_sia\""),
        "v2 dispatcher lost routing for the legacy flat `enable_sia` alias"
    );
}

#[test]
fn lightning_namespace_methods_are_routed() {
    assert_namespace_routed("lightning::", &[
        "channels::open_channel",
        "channels::close_channel",
        "channels::update_channel",
        "channels::get_channel_details",
        "channels::get_claimable_balances",
        "channels::list_open_channels_by_filter",
        "channels::list_closed_channels_by_filter",
        "nodes::connect_to_node",
        "nodes::add_trusted_node",
        "nodes::list_trusted_nodes",
        "nodes::remove_trusted_node",
        "payments::generate_invoice",
        "payments::send_payment",
        "payments::get_payment_details",
        "payments::list_payments_by_filter",
    ]);
}

#[test]
fn preexisting_namespaces_remain_routed() {
    assert_namespace_routed("stream::", &[
        "balance::enable",
        "disable",
        "fee_estimator::enable",
        "heartbeat::enable",
        "network::enable",
        "order_status::enable",
        "orderbook::enable",
        "shutdown_signal::enable",
        "swap_status::enable",
        "tx_history::enable",
    ]);
    assert!(
        DISPATCHER_SOURCE.contains("#[cfg(all(unix, not(target_arch = \"wasm32\")))]"),
        "shutdown-signal streaming route lost its native non-Windows cfg gate"
    );
    assert_namespace_routed("gui_storage::", &[
        "enable_account",
        "add_account",
        "delete_account",
        "get_accounts",
        "get_account_coins",
        "get_enabled_account",
        "set_account_name",
        "set_account_description",
        "set_account_balance",
        "activate_coins",
        "deactivate_coins",
    ]);
    assert_namespace_routed("experimental::staking::", &[
        "delegate",
        "undelegate",
        "claim_rewards",
        "query::delegations",
        "query::ongoing_undelegations",
        "query::validators",
    ]);
    assert_namespace_routed("experimental::1inch_v6_0::", &[
        "classic_swap_contract",
        "classic_swap_quote",
        "classic_swap_create",
        "classic_swap_liquidity_sources",
        "classic_swap_tokens",
    ]);
}

#[test]
fn evm_v2_flat_methods_are_routed() {
    // EVM (Ethereum) V2 activation & token RPC surface (CRD ch. 35). These are
    // flat (un-namespaced) mmrpc-2.0 methods routed on all targets, including
    // WASM, so they must appear in the main dispatcher match (not the
    // native-only block).
    for method in [
        "enable_eth_with_tokens",
        "enable_erc20",
        "enable_nft",
        "get_token_info",
        "get_swap_gas_fee_policy",
        "set_swap_gas_fee_policy",
    ] {
        assert!(
            DISPATCHER_SOURCE.contains(&format!("\"{method}\"")),
            "v2 dispatcher lost routing for the flat EVM method `{method}`"
        );
    }
}

#[test]
fn metamask_connect_methods_are_routed() {
    // MetaMask connection task surface (CRD ch. 47) is WASM-only; the dispatcher
    // arms are `#[cfg(target_arch = "wasm32")]`-gated, but this guard reads
    // `dispatcher.rs` as text so the method strings are asserted on every target.
    for method in [
        "connect_metamask::init",
        "connect_metamask::status",
        "connect_metamask::cancel",
    ] {
        assert!(
            DISPATCHER_SOURCE.contains(&format!("\"{method}\"")),
            "v2 dispatcher lost routing for the WASM-only MetaMask method `{method}`"
        );
    }
}

#[test]
fn tendermint_v2_flat_methods_are_routed() {
    // Tendermint (Cosmos) V2 activation & token RPC surface (CRD ch. 36). These
    // are flat (un-namespaced) mmrpc-2.0 methods routed on all targets,
    // including WASM, so they must appear in the main dispatcher match (not the
    // native-only block).
    for method in ["enable_tendermint_with_assets", "enable_tendermint_token"] {
        assert!(
            DISPATCHER_SOURCE.contains(&format!("\"{method}\"")),
            "v2 dispatcher lost routing for the flat Tendermint method `{method}`"
        );
    }
}

#[test]
fn z_coin_tx_history_method_is_routed() {
    // Shielded-coin transaction-history method (CRD ch. 39 §39.8). It is
    // native-only, so the dispatcher arm lives in the `#[cfg(not(wasm))]`
    // native-only block; this guard reads `dispatcher.rs` as text so the method
    // string is asserted on every target.
    assert!(
        DISPATCHER_SOURCE.contains("\"z_coin_tx_history\""),
        "v2 dispatcher lost routing for the native-only method `z_coin_tx_history`"
    );
}

#[test]
fn trezor_connection_status_method_is_routed() {
    // Trezor hardware-wallet status is native-only; the dispatcher arm lives in
    // the native method block and must keep the flat mmrpc 2.0 method string.
    assert!(
        DISPATCHER_SOURCE.contains("\"trezor_connection_status\""),
        "v2 dispatcher lost routing for the native-only method `trezor_connection_status`"
    );
}

#[test]
fn z_coin_activation_methods_are_routed_on_every_target() {
    // CRD ch. 39 R39.10.1-R39.10.2: the shielded coin's task methods and flat aliases are routed on
    // native and WASM alike, so none of them may sit inside a native-only block.
    let native_only_methods_at = DISPATCHER_SOURCE
        .find("native_only_methods => match native_only_methods")
        .expect("native-only method block not found");
    let native_only_task_at = DISPATCHER_SOURCE
        .find("native_only_task => match native_only_task")
        .expect("native-only task block not found");
    // The flat aliases live in the main dispatcher, the namespaced methods in the task dispatcher; each
    // must sit before the native-only block of its own dispatcher.
    for (methods, native_only_block_at) in [
        (
            ["init_z_coin", "init_z_coin_status", "init_z_coin_user_action"].as_slice(),
            native_only_methods_at,
        ),
        (
            [
                "enable_z_coin::init",
                "enable_z_coin::status",
                "enable_z_coin::user_action",
                "enable_z_coin::cancel",
            ]
            .as_slice(),
            native_only_task_at,
        ),
    ] {
        for method in methods {
            let arm_at = DISPATCHER_SOURCE
                .find(&format!("\"{method}\" =>"))
                .unwrap_or_else(|| panic!("v2 dispatcher lost routing for `{method}`"));
            assert!(
                arm_at < native_only_block_at,
                "`{method}` is routed only inside a native-only block"
            );
        }
    }
}
