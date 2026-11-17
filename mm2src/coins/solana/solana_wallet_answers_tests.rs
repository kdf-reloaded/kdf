//! Offline tests of the wallet-surface answers of a Solana coin and of an SPL token (CRD ch.40 R40.6B.14, D40.6B.1
//! option (b), T40.6B.16 to T40.6B.19 and T40.6B.22). Each method answers on the platform coin and on a token; none
//! ends abnormally. No test prints or compares key material in a message: key-derived checks are `assert!` over
//! booleans with fixed texts.

use super::*;
use crate::my_tx_history_v2::{my_tx_history_v2_rpc, MyTxHistoryErrorV2, MyTxHistoryRequestV2};
use crate::solana::solana_common_tests::{solana_coin_for_test, spl_coin_for_test, SolanaNet};
use crate::{ConfirmationsReq, MmCoinEnum, RawTransactionError, RequiresNotaReq, SignRawTransactionRequest, SplToken};
use base58::{FromBase58, ToBase58};
use common::{block_on, Future01CompatExt};
use mm2_core::mm_ctx::MmCtxBuilder;
use serde_json::json;

const PASSPHRASE: &str = "spice describe gravity federal blast come thank unfair canal monkey style afraid";
const USDC_MINT: &str = "CpMah17kQEL2wqyMKt3mZBdTnZbkbfx4nqmQMFDP5vwp";

fn sol_and_token(decimals: u8) -> (MmArc, SolanaCoin, SplToken) {
    let (ctx, sol) = solana_coin_for_test(PASSPHRASE.to_string(), SolanaNet::Testnet);
    let token = spl_coin_for_test(
        sol.clone(),
        "USDC".to_string(),
        decimals,
        Pubkey::from_str(USDC_MINT).unwrap(),
    );
    (ctx, sol, token)
}

fn register(ctx: &MmArc, coin: MmCoinEnum) {
    let ticker = coin.ticker().to_string();
    let cctx = crate::CoinsContext::from_ctx(ctx).unwrap();
    block_on(async { cctx.coins.lock().await.insert(ticker, coin) });
}

/// A base58 text of a 32-byte value that is `len` characters long, found by varying the first byte.
fn address_of_len(len: usize) -> Option<String> {
    (0..=255u8)
        .map(|first| {
            let mut key = [0xAB_u8; 32];
            key[0] = first;
            key.to_base58()
        })
        .find(|text| text.len() == len)
}

#[test]
fn t40_6b_17_validate_address_accepts_32_byte_addresses_of_any_length() {
    let (_, sol, token) = sol_and_token(6);
    let short = address_of_len(43).expect("a 32-byte value that encodes to 43 characters");
    let long = address_of_len(44).expect("a 32-byte value that encodes to 44 characters");
    for address in [short, long, sol.my_address.clone()] {
        let on_sol = sol.validate_address(&address);
        let on_token = token.validate_address(&address);
        assert!(
            on_sol.is_valid && on_sol.reason.is_none(),
            "SOL must accept a 32-byte address"
        );
        assert!(
            on_token.is_valid && on_token.reason.is_none(),
            "the token must accept a 32-byte address"
        );
    }
}

#[test]
fn t40_6b_17_validate_address_refuses_other_text_with_a_reason() {
    let (_, sol, token) = sol_and_token(6);
    let thirty_three = [7_u8; 33].to_base58();
    let thirty_one = [7_u8; 31].to_base58();
    for text in [
        "",
        "abc",
        "0OIl",
        "not an address",
        thirty_three.as_str(),
        thirty_one.as_str(),
    ] {
        let on_sol = sol.validate_address(text);
        let on_token = token.validate_address(text);
        assert!(
            !on_sol.is_valid && on_sol.reason.is_some(),
            "SOL must refuse text that is not 32 bytes of base58, with a reason"
        );
        assert!(
            !on_token.is_valid && on_token.reason.is_some(),
            "the token must refuse text that is not 32 bytes of base58, with a reason"
        );
    }
}

#[test]
fn t40_6b_18_sign_then_verify_round_trip_on_sol_and_token() {
    let (_, sol, token) = sol_and_token(6);
    let address = sol.my_address.clone();
    let sol_signature = sol.sign_message("a message").unwrap();
    let token_signature = token.sign_message("a message").unwrap();
    assert!(sol_signature == token_signature, "a token signs with the platform key");
    let signature_bytes = sol_signature.from_base58().unwrap();
    assert!(signature_bytes.len() == 64, "the signature is 64 bytes in base58");

    assert!(
        sol.verify_message(&sol_signature, "a message", &address).unwrap(),
        "SOL: same message"
    );
    assert!(
        token.verify_message(&token_signature, "a message", &address).unwrap(),
        "token: same message"
    );
    assert!(
        !sol.verify_message(&sol_signature, "another message", &address).unwrap(),
        "SOL: changed message"
    );
    assert!(
        !token
            .verify_message(&token_signature, "another message", &address)
            .unwrap(),
        "token: changed message"
    );
}

#[test]
fn t40_6b_16_show_priv_key_is_the_64_byte_keypair_in_base58() {
    let (_, sol, token) = sol_and_token(6);
    let pubkey = sol.key_pair.pubkey().to_bytes();
    for text in [sol.display_priv_key().unwrap(), token.display_priv_key().unwrap()] {
        let bytes = text.from_base58().unwrap();
        assert!(bytes.len() == 64, "the key is 64 bytes");
        assert!(bytes[32..] == pubkey[..], "the second half is the public key");
        assert!(
            bytes[..32] == sol.key_pair.secret_bytes()[..],
            "the first half is the secret"
        );
    }
}

#[test]
fn t40_6b_16_public_key_and_unused_hash_do_not_end_abnormally() {
    let (_, sol, token) = sol_and_token(6);
    assert!(
        sol.get_public_key().unwrap() == sol.my_address,
        "the public key is the base58 address"
    );
    assert!(
        token.get_public_key().unwrap() == sol.my_address,
        "a token reports the platform key"
    );
    assert!(sol.sign_message_hash("m").is_none(), "no pre-hash exists");
    assert!(token.sign_message_hash("m").is_none(), "no pre-hash exists");
}

#[test]
fn t40_6b_16_min_tx_amount_and_min_trading_vol() {
    let (_, sol, token6) = sol_and_token(6);
    let (_, _, token9) = sol_and_token(9);
    assert!(
        sol.min_tx_amount() == BigDecimal::from_str("0.000000001").unwrap(),
        "SOL: one lamport"
    );
    assert!(
        token6.min_tx_amount() == BigDecimal::from_str("0.000001").unwrap(),
        "6 decimals"
    );
    assert!(
        token9.min_tx_amount() == BigDecimal::from_str("0.000000001").unwrap(),
        "9 decimals"
    );
    assert!(sol.min_trading_vol() == MmNumber::from("0.00777"), "SOL: unchanged");
    assert!(
        token6.min_trading_vol() == MmNumber::from("0.00777"),
        "token: unchanged"
    );
}

#[test]
fn t40_6b_16_confirmation_settings_are_stored_per_coin_and_read_back() {
    let (_, sol, token) = sol_and_token(6);
    assert!(
        sol.required_confirmations() == 1 && !sol.requires_notarization(),
        "SOL defaults"
    );
    assert!(
        token.required_confirmations() == 1 && !token.requires_notarization(),
        "token defaults"
    );

    sol.set_required_confirmations(5);
    sol.set_requires_notarization(true);
    assert!(
        sol.required_confirmations() == 5 && sol.requires_notarization(),
        "SOL reads back"
    );
    assert!(
        token.required_confirmations() == 1 && !token.requires_notarization(),
        "the token is unchanged"
    );

    token.set_required_confirmations(3);
    token.set_requires_notarization(true);
    assert!(
        token.required_confirmations() == 3 && token.requires_notarization(),
        "token reads back"
    );
    assert!(sol.required_confirmations() == 5, "SOL is unchanged by the token");
}

#[test]
fn t40_6b_16_set_confirmations_rpcs_answer_with_the_stored_value() {
    let (ctx, sol, token) = sol_and_token(6);
    register(&ctx, MmCoinEnum::from(sol));
    register(&ctx, MmCoinEnum::from(token));
    for coin in ["SOL", "USDC"] {
        let answer = block_on(crate::set_required_confirmations_core(ctx.clone(), ConfirmationsReq {
            coin: coin.to_string(),
            confirmations: 7,
        }))
        .expect("set_required_confirmations answers");
        assert!(answer.confirmations == 7, "the answer reports the stored value");
        let answer = block_on(crate::set_requires_notarization_core(ctx.clone(), RequiresNotaReq {
            coin: coin.to_string(),
            requires_notarization: true,
        }))
        .expect("set_requires_notarization answers");
        assert!(answer.requires_notarization, "the answer reports the stored value");
    }
}

#[test]
fn t40_6b_16_refusals_are_typed_errors_naming_the_coin() {
    let (_, sol, token) = sol_and_token(6);
    let coins = [("SOL", &sol as &dyn MmCoin), ("USDC", &token as &dyn MmCoin)];
    for (ticker, coin) in coins {
        let err = coin.convert_to_address("addr", json!({})).unwrap_err();
        assert!(err.contains(ticker), "convertaddress names the coin");

        let err = block_on(
            coin.get_raw_transaction(crate::RawTransactionRequest {
                coin: ticker.to_string(),
                tx_hash: "00".to_string(),
            })
            .compat(),
        )
        .unwrap_err();
        assert!(
            matches!(err.get_inner(), RawTransactionError::NotImplemented { coin } if coin == ticker),
            "get_raw_transaction is NotImplemented naming the coin"
        );

        let err = block_on(coin.get_trade_fee().compat()).unwrap_err();
        assert!(err.contains(ticker), "get_trade_fee names the coin");

        assert!(
            matches!(coin.history_sync_status(), HistorySyncState::NotEnabled),
            "the history state is NotEnabled"
        );
        assert!(
            block_on(coin.process_history_loop(MmCtxBuilder::new().into_mm_arc()).compat()).is_ok(),
            "the history loop ends at once"
        );
        let names_coin = match coin.tx_enum_from_bytes(&[1]) {
            Err(text) => text.contains(ticker),
            Ok(_) => false,
        };
        assert!(names_coin, "tx decoding is an error naming the coin");
    }
}

#[test]
fn t40_6b_16_sign_raw_transaction_is_not_implemented_naming_the_coin() {
    let (_, sol, token) = sol_and_token(6);
    let request: SignRawTransactionRequest =
        serde_json::from_value(json!({"coin": "SOL", "type": "UTXO", "tx": {"tx_hex": "00"}})).unwrap();
    let coins: [(&str, &dyn MarketCoinOps); 2] = [("SOL", &sol as &dyn MmCoin), ("USDC", &token as &dyn MmCoin)];
    for (ticker, coin) in coins {
        let err = block_on(coin.sign_raw_tx(&request).compat()).unwrap_err();
        assert!(
            matches!(err.get_inner(), RawTransactionError::NotImplemented { coin } if coin == ticker),
            "sign_raw_transaction is NotImplemented naming the coin"
        );
    }
}

#[test]
fn t40_6b_16_trade_fee_estimates_are_errors_not_panics() {
    let (_, sol, token) = sol_and_token(6);
    let coins = [("SOL", &sol as &dyn MmCoin), ("USDC", &token as &dyn MmCoin)];
    for (ticker, coin) in coins {
        let value = TradePreimageValue::Exact(BigDecimal::from(1));
        let err = block_on(coin.get_sender_trade_fee(value, FeeApproxStage::TradePreimage)).unwrap_err();
        assert!(
            err.get_inner().to_string().contains(ticker),
            "the sender fee names the coin"
        );
        let err = block_on(coin.get_receiver_trade_fee(FeeApproxStage::TradePreimage).compat()).unwrap_err();
        assert!(
            err.get_inner().to_string().contains(ticker),
            "the receiver fee names the coin"
        );
        let err =
            block_on(coin.get_fee_to_send_taker_fee(BigDecimal::from(1), FeeApproxStage::TradePreimage)).unwrap_err();
        assert!(
            err.get_inner().to_string().contains(ticker),
            "the taker-fee estimate names the coin"
        );
    }
}

#[test]
fn t40_6b_16_both_forms_of_my_tx_history_answer_with_an_error() {
    let (ctx, sol, token) = sol_and_token(6);
    register(&ctx, MmCoinEnum::from(sol));
    register(&ctx, MmCoinEnum::from(token));
    for ticker in ["SOL", "USDC"] {
        // The legacy form: a structured error naming the coin.
        let err = block_on(crate::my_tx_history(ctx.clone(), json!({ "coin": ticker }))).unwrap_err();
        assert!(err.contains(ticker), "the legacy form names the coin");
        // The versioned form: `NotSupportedFor` with the ticker.
        let request: MyTxHistoryRequestV2 = serde_json::from_value(json!({ "coin": ticker })).unwrap();
        let Err(err) = block_on(my_tx_history_v2_rpc(ctx.clone(), request)) else {
            panic!("the versioned form must answer with an error");
        };
        assert!(
            matches!(err.get_inner(), MyTxHistoryErrorV2::NotSupportedFor(name) if name == ticker),
            "the versioned form is NotSupportedFor"
        );
    }
}

#[test]
fn t40_6b_14_every_swap_operation_answers_an_error_on_sol_and_token() {
    let (_, sol, token) = sol_and_token(6);
    let tx = TransactionEnum::from(crate::UtxoTx::default());
    let dex_fee = DexFee::Standard(MmNumber::from(1));
    let payment_input = || ValidatePaymentInput {
        payment_tx: vec![1],
        time_lock: 1,
        taker_pub: vec![2],
        maker_pub: vec![3],
        secret_hash: vec![4],
        amount: BigDecimal::from(1),
        swap_contract_address: None,
        try_spv_proof_until: 0,
        confirmations: 1,
    };
    let no_contract: Option<BytesJson> = None;
    let coins = [("SOL", &sol as &dyn MmCoin), ("USDC", &token as &dyn MmCoin)];
    for (_ticker, coin) in coins {
        assert!(
            block_on(coin.send_taker_fee(&dex_fee, &[1], &[2]).compat()).is_err(),
            "send_taker_fee must answer an error"
        );
        assert!(
            block_on(
                coin.send_maker_payment(1, &[1], &[2], &[3], BigDecimal::from(1), &no_contract)
                    .compat()
            )
            .is_err(),
            "send_maker_payment must answer an error"
        );
        assert!(
            block_on(
                coin.send_taker_payment(1, &[1], &[2], &[3], BigDecimal::from(1), &no_contract)
                    .compat()
            )
            .is_err(),
            "send_taker_payment must answer an error"
        );
        assert!(
            block_on(
                coin.send_maker_spends_taker_payment(&[1], 1, &[2], &[3], &[4], &no_contract)
                    .compat()
            )
            .is_err(),
            "send_maker_spends_taker_payment must answer an error"
        );
        assert!(
            block_on(
                coin.send_taker_spends_maker_payment(&[1], 1, &[2], &[3], &[4], &no_contract)
                    .compat()
            )
            .is_err(),
            "send_taker_spends_maker_payment must answer an error"
        );
        assert!(
            block_on(
                coin.send_taker_refunds_payment(&[1], 1, &[2], &[3], &[4], &no_contract)
                    .compat()
            )
            .is_err(),
            "send_taker_refunds_payment must answer an error"
        );
        assert!(
            block_on(
                coin.send_maker_refunds_payment(&[1], 1, &[2], &[3], &[4], &no_contract)
                    .compat()
            )
            .is_err(),
            "send_maker_refunds_payment must answer an error"
        );
        let fee_args = ValidateFeeArgs {
            fee_tx: &tx,
            expected_sender: &[1],
            fee_addr: &[2],
            dex_fee: &dex_fee,
            min_block_number: 0,
            uuid: &[3],
        };
        assert!(
            block_on(coin.validate_fee(fee_args).compat()).is_err(),
            "validate_fee must answer an error"
        );
        assert!(
            block_on(coin.validate_maker_payment(payment_input()).compat()).is_err(),
            "validate_maker_payment must answer an error"
        );
        assert!(
            block_on(coin.validate_taker_payment(payment_input()).compat()).is_err(),
            "validate_taker_payment must answer an error"
        );
        assert!(
            block_on(
                coin.check_if_my_payment_sent(1, &[1], &[2], &[3], 0, &no_contract)
                    .compat()
            )
            .is_err(),
            "check_if_my_payment_sent must answer an error"
        );
        assert!(
            block_on(coin.search_for_swap_tx_spend_my(1, &[1], &[2], &[3], &[4], 0, &no_contract)).is_err(),
            "search_for_swap_tx_spend_my must answer an error"
        );
        assert!(
            block_on(coin.search_for_swap_tx_spend_other(1, &[1], &[2], &[3], &[4], 0, &no_contract)).is_err(),
            "search_for_swap_tx_spend_other must answer an error"
        );
        assert!(
            coin.extract_secret(&[1], &[2]).is_err(),
            "extract_secret must answer an error"
        );
        assert!(
            coin.negotiate_swap_contract_addr(None).is_err(),
            "negotiate_swap_contract_addr must answer an error"
        );
        assert!(coin.get_htlc_key_pair().is_none(), "get_htlc_key_pair must answer None");
        assert!(
            coin.swap_contract_address().is_none(),
            "swap_contract_address must answer None"
        );
        assert!(
            block_on(coin.wait_for_confirmations(&[1], 1, false, 0, 1).compat()).is_err(),
            "wait_for_confirmations must answer an error"
        );
        assert!(
            block_on(coin.wait_for_tx_spend(&[1], 0, 0, &no_contract).compat()).is_err(),
            "wait_for_tx_spend must answer an error"
        );
    }
}
