//! Tests of the swap-contract binding at activation on the legacy route (CRD ch.21 R21.B2, D21.B7, §21.11C). The
//! coin is built the way the legacy `enable` builds it, against a node that is never asked.

use crate::eth::tron::activation::tron_coin_from_conf_and_request_with_switch;
use crate::eth::tron::{tron_coin_from_conf_and_request, tron_swap_contracts, Network, TronAddress, TronSwapContracts};
use crate::eth::{EthCoin, EthCoinBuildError};
use crate::{CoinProtocol, MmCoin, NegotiateSwapContractAddrErr, SwapOps};
use common::block_on;
use ethereum_types::Address;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use serde_json::{json, Value as Json};

const SWAP_CONTRACT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const TOKEN_CONTRACT: &str = "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf";
const NODE: &str = "http://127.0.0.1:1";

fn priv_key() -> [u8; 32] {
    // Deterministic non-zero key (do not use in production).
    let mut key = [0u8; 32];
    key[31] = 1;
    key
}

fn swap_contract() -> TronAddress { TronAddress::parse(SWAP_CONTRACT).expect("valid") }

fn fallback() -> TronAddress { TronAddress::from_evm_address(Address::from([0x7f; 20])) }

fn zero() -> TronAddress { TronAddress::from_evm_address(Address::zero()) }

/// The four accepted forms of `address`: base58check, the hex wire form, the same behind `0x`, and the EVM `0x`
/// 20-byte form.
fn forms(address: &TronAddress) -> [String; 4] {
    [
        address.to_base58(),
        address.to_hex(),
        format!("0x{}", address.to_hex()),
        format!("0x{}", hex::encode(address.to_evm_address())),
    ]
}

/// A coins-config entry of a TRX coin that can trade.
fn trading() -> Json { json!({}) }

/// A coins-config entry of a wallet-only TRX coin.
fn wallet_only() -> Json { json!({ "wallet_only": true }) }

/// The legacy activation of a TRX coin with the config `conf` and the request members `members`.
fn activate(ctx: &MmArc, conf: &Json, members: Json, swaps_available: bool) -> Result<EthCoin, String> {
    let mut req = json!({ "urls": [NODE] });
    for (key, value) in members.as_object().expect("an object") {
        req[key] = value.clone();
    }
    block_on(tron_coin_from_conf_and_request_with_switch(
        ctx,
        "TRX",
        conf,
        &req,
        &priv_key(),
        CoinProtocol::TRX { network: Network::Nile },
        swaps_available,
    ))
}

fn assert_unbound(coin: &EthCoin) {
    assert_eq!(coin.swap_contract_address, Address::zero());
    assert_eq!(coin.fallback_swap_contract, None);
    assert!(matches!(
        coin.negotiate_swap_contract_addr(None).map_err(|e| e.into_inner()),
        Err(NegotiateSwapContractAddrErr::NoSwapContract)
    ));
}

/// The generic rule of R21.B2: nothing for a wallet-only coin, a non-zero contract for a coin that can trade.
#[test]
fn the_binding_rule_is_the_evm_rule() {
    let contract = swap_contract().to_evm_address();
    let other = fallback().to_evm_address();
    let invalid = |result: Result<Option<TronSwapContracts>, _>| {
        matches!(
            result.map_err(|e: mm2_err_handle::prelude::MmError<EthCoinBuildError>| e.into_inner()),
            Err(EthCoinBuildError::InvalidPayload(_))
        )
    };

    // A coin that can trade, with the switch on.
    assert_eq!(
        tron_swap_contracts(&trading(), Some(contract), None, true).unwrap(),
        Some(TronSwapContracts {
            swap_contract_address: contract,
            fallback_swap_contract: None,
        })
    );
    assert_eq!(
        tron_swap_contracts(&trading(), Some(contract), Some(other), true).unwrap(),
        Some(TronSwapContracts {
            swap_contract_address: contract,
            fallback_swap_contract: Some(other),
        })
    );
    assert!(invalid(tron_swap_contracts(&trading(), None, None, true)));
    assert!(invalid(tron_swap_contracts(&trading(), None, Some(other), true)));
    assert!(invalid(tron_swap_contracts(
        &trading(),
        Some(Address::zero()),
        None,
        true
    )));
    assert!(invalid(tron_swap_contracts(
        &trading(),
        Some(contract),
        Some(Address::zero()),
        true
    )));

    // A wallet-only coin ignores the members whatever they are; so does every coin with the switch off.
    for (conf, swaps_available) in [(wallet_only(), true), (wallet_only(), false), (trading(), false)] {
        for (swap, fallback) in [
            (None, None),
            (Some(contract), Some(other)),
            (Some(Address::zero()), Some(Address::zero())),
        ] {
            assert_eq!(
                tron_swap_contracts(&conf, swap, fallback, swaps_available).unwrap(),
                None
            );
        }
    }
}

/// With the switch on, a coin that can trade is bound to the contracts of the request, in every accepted form,
/// normalized to the 20-byte payload.
#[test]
fn a_trading_coin_is_bound_to_the_contracts_in_every_address_form() {
    let ctx = MmCtxBuilder::new().into_mm_arc();
    for (swap_form, fallback_form) in forms(&swap_contract()).into_iter().zip(forms(&fallback())) {
        let coin = activate(
            &ctx,
            &trading(),
            json!({ "swap_contract_address": swap_form, "fallback_swap_contract": fallback_form }),
            true,
        )
        .unwrap_or_else(|e| panic!("{swap_form}: {e}"));
        assert_eq!(
            coin.swap_contract_address,
            swap_contract().to_evm_address(),
            "{swap_form}"
        );
        assert_eq!(
            coin.fallback_swap_contract,
            Some(fallback().to_evm_address()),
            "{fallback_form}"
        );
        assert_eq!(
            MmCoin::swap_contract_address(&coin).map(|bytes| bytes.0),
            Some(swap_contract().to_evm_address().as_bytes().to_vec())
        );
        assert_eq!(
            coin.negotiate_swap_contract_addr(None).unwrap().map(|bytes| bytes.0),
            Some(fallback().to_evm_address().as_bytes().to_vec())
        );
    }

    // The fallback is optional.
    let coin = activate(
        &ctx,
        &trading(),
        json!({ "swap_contract_address": SWAP_CONTRACT }),
        true,
    )
    .unwrap();
    assert_eq!(coin.swap_contract_address, swap_contract().to_evm_address());
    assert_eq!(coin.fallback_swap_contract, None);
}

/// With the switch on, a coin that can trade is refused without a usable swap contract, before any node is asked.
#[test]
fn a_trading_coin_without_a_usable_contract_is_refused() {
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let refused = |members: Json| activate(&ctx, &trading(), members, true).unwrap_err();

    assert!(refused(json!({})).contains("swap_contract_address is required"));
    for form in forms(&zero()) {
        let error = refused(json!({ "swap_contract_address": form }));
        assert!(
            error.contains("swap_contract_address can't be zero address"),
            "{form}: {error}"
        );
        let error = refused(json!({ "swap_contract_address": SWAP_CONTRACT, "fallback_swap_contract": form }));
        assert!(
            error.contains("fallback_swap_contract can't be zero address"),
            "{form}: {error}"
        );
    }
    for (members, member) in [
        (json!({ "swap_contract_address": "nope" }), "swap_contract_address"),
        (json!({ "swap_contract_address": 7 }), "swap_contract_address"),
        (
            json!({ "swap_contract_address": SWAP_CONTRACT, "fallback_swap_contract": "0xnothex" }),
            "fallback_swap_contract",
        ),
    ] {
        let error = refused(members);
        assert!(
            error.contains(&format!("{member} is not a valid swap contract address")),
            "{error}"
        );
    }
}

/// A wallet-only coin accepts and ignores the members, with the switch on too: it carries no binding, whatever
/// the request says.
#[test]
fn a_wallet_only_coin_ignores_the_members() {
    let ctx = MmCtxBuilder::new().into_mm_arc();
    for members in [
        json!({}),
        json!({ "swap_contract_address": SWAP_CONTRACT, "fallback_swap_contract": fallback().to_base58() }),
        json!({ "swap_contract_address": zero().to_base58() }),
        json!({ "swap_contract_address": "nope", "fallback_swap_contract": 7 }),
    ] {
        for swaps_available in [true, false] {
            let coin = activate(&ctx, &wallet_only(), members.clone(), swaps_available)
                .unwrap_or_else(|e| panic!("{members}: {e}"));
            assert_unbound(&coin);
        }
    }
}

/// With the switch off, which is the state of this build, nothing changes: every TRON coin is wallet-only, so a
/// coin whose config lets it trade activates with or without the members and carries no binding.
#[test]
fn with_the_switch_off_no_tron_coin_is_bound() {
    const { assert!(!crate::TRON_SWAPS_AVAILABLE) };
    let ctx = MmCtxBuilder::new().into_mm_arc();
    for members in [
        json!({}),
        json!({ "swap_contract_address": SWAP_CONTRACT, "fallback_swap_contract": fallback().to_base58() }),
        json!({ "swap_contract_address": "nope" }),
    ] {
        let mut req = json!({ "urls": [NODE] });
        for (key, value) in members.as_object().expect("an object") {
            req[key] = value.clone();
        }
        // The route itself, which reads the switch.
        let coin = block_on(tron_coin_from_conf_and_request(
            &ctx,
            "TRX",
            &trading(),
            &req,
            &priv_key(),
            CoinProtocol::TRX { network: Network::Nile },
        ))
        .unwrap_or_else(|e| panic!("{members}: {e}"));
        assert_unbound(&coin);
        assert_unbound(&activate(&ctx, &trading(), members, false).unwrap());
    }
}

/// A TRC20 coin of the legacy route is bound by its own request, and a token built on a bound platform shares the
/// platform's binding.
#[test]
fn tokens_carry_the_binding() {
    let conf = json!({ "coins": [{
        "coin": "USDT-TRC20",
        "decimals": 6,
        "protocol": { "type": "TRC20", "protocol_data": { "platform": "TRX", "contract_address": TOKEN_CONTRACT } }
    }] });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    let token_conf = crate::coin_conf(&ctx, "USDT-TRC20");
    let req = json!({
        "urls": [NODE],
        "swap_contract_address": SWAP_CONTRACT,
        "fallback_swap_contract": fallback().to_hex(),
    });

    let standalone = block_on(tron_coin_from_conf_and_request_with_switch(
        &ctx,
        "USDT-TRC20",
        &token_conf,
        &req,
        &priv_key(),
        CoinProtocol::TRC20 {
            platform: "TRX".to_owned(),
            contract_address: TOKEN_CONTRACT.to_owned(),
        },
        true,
    ))
    .expect("a token with configured decimals builds without a node request");
    assert_eq!(standalone.swap_contract_address, swap_contract().to_evm_address());
    assert_eq!(standalone.fallback_swap_contract, Some(fallback().to_evm_address()));

    let platform = activate(
        &ctx,
        &trading(),
        json!({ "swap_contract_address": SWAP_CONTRACT, "fallback_swap_contract": fallback().to_hex() }),
        true,
    )
    .unwrap();
    let token = block_on(platform.trc20_token_from_conf_or_contract(
        "USDT-TRC20".to_owned(),
        TronAddress::parse(TOKEN_CONTRACT).expect("valid").to_evm_address(),
        1,
        None,
    ))
    .expect("a token with configured decimals builds without a node request");
    assert_eq!(token.swap_contract_address, swap_contract().to_evm_address());
    assert_eq!(token.fallback_swap_contract, Some(fallback().to_evm_address()));
}
