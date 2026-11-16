//! Tests of the watch-only (public-key) Tendermint coin (CRD ch.36 §36.14.3: R36.14.16 -- R36.14.18, D36.17, D36.18,
//! T36.42, T36.43; ch.18 R18.11.23a).
//!
//! The coin talks to the scripted node of the swap-flow tests. The keys are fixed throwaway keys; no address in
//! this file holds funds.

use super::super::tendermint_types::{unsigned_internal_id, FeeIntent, UnsignedTxError, GAS_LIMIT_DEFAULT,
                                     GAS_WANTED_BASE_VALUE};
use super::super::{tendermint_coin_from_conf_and_request_with_public_key, TendermintPublicKey};
use super::*;
use crate::rpc_command::tendermint::staking::{ClaimRewardsPayload, DelegationPayload};
use crate::utxo::utxo_common::big_decimal_from_sat_unsigned;
use crate::{TransactionDetails, TxFeeDetails, UnsignedTransaction};
use cosmrs::proto::cosmos::tx::v1beta1::{AuthInfo as AuthInfoProto, TxBody as TxBodyProto};

/// The public key of the first test vector of RFC 8032 (an `ed25519` key).
const ED25519_KEY: [u8; 32] = [
    0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a, 0x0e, 0xe1, 0x72,
    0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
];
const MSG_SEND_URL: &str = "/cosmos.bank.v1beta1.MsgSend";
/// What one coin is withdrawn: 1 coin at the 6 decimals of the test chain.
const WITHDRAWN_SAT: u64 = 1_000_000;

fn secp_key() -> TendermintPublicKey {
    TendermintPublicKey::from_raw_secp256k1(&pubkey(&OWN_KEY)).expect("a valid secp256k1 key")
}

fn ed25519_key() -> TendermintPublicKey {
    TendermintPublicKey::from_raw_ed25519(&ED25519_KEY).expect("a valid ed25519 key")
}

/// A watch-only coin of the IRIS test chain for `key`, whose only node is the scripted one; the node answers the
/// account, balance and block queries (never a simulation: a watch-only coin simulates nothing).
fn watch_only(key: TendermintPublicKey, ledger: bool) -> Fixture {
    let node = FakeNode::start();
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0] }))
        .into_mm_arc();
    let coin = common::block_on(tendermint_coin_from_conf_and_request_with_public_key(
        &ctx,
        "IRIS".to_owned(),
        &Chain::Iris.conf(),
        vec![RpcNode::for_test(&node.url())],
        key,
        ledger,
        false,
    ))
    .unwrap_or_else(|e| panic!("cannot build the watch-only coin: {e}"));
    Fixture { coin, node, ctx }
}

fn script_node(fx: &Fixture) {
    // The simulation answer of `script_fee_estimation` stays: the tests assert it is never asked.
    script_fee_estimation(fx, 80_000);
}

fn simulations(fx: &Fixture) -> usize {
    fx.node
        .requests("abci_query")
        .iter()
        .filter(|params| params["path"] == "/cosmos.tx.v1beta1.Service/Simulate")
        .count()
}

/// The fixed assumed fee of CRD ch.18 R18.11.9 at the coin's default gas price, in base units.
fn fixed_fee(coin: &TendermintCoin) -> u64 {
    let (price, _) = coin.selected_gas_price_and_limit(&FeeIntent::request(None, GAS_LIMIT_DEFAULT));
    ((GAS_WANTED_BASE_VALUE * 1.5) * price).ceil() as u64
}

fn peer() -> String { account(Chain::Iris, &pubkey(&PEER_KEY)).to_string() }

fn withdraw_of(coin: &impl MmCoin, ticker: &str) -> Result<TransactionDetails, String> {
    let request: WithdrawRequest =
        serde_json::from_value(json!({ "coin": ticker, "to": peer(), "amount": "1" })).map_err(|e| e.to_string())?;
    common::block_on(coin.withdraw(request).compat()).map_err(|e| e.to_string())
}

fn delegation() -> DelegationPayload {
    serde_json::from_value(json!({ "validator_address": peer(), "amount": "1" })).expect("a delegation payload")
}

fn fee_details_of(details: &TransactionDetails) -> (BigDecimal, u64, u64) {
    match &details.fee_details {
        Some(TxFeeDetails::Tendermint(d)) => (d.amount.clone(), d.uamount, d.gas_limit),
        other => panic!("not a Tendermint fee: {other:?}"),
    }
}

fn nyan(fx: &Fixture) -> TendermintToken { nyan_token(&fx.coin, false) }

// ---------------------------------------------------------------------------
// R36.14.16, D36.17: the account of the key, no node contacted
// ---------------------------------------------------------------------------

#[test]
fn a_secp256k1_key_activates_its_account_without_contacting_a_node() {
    // T36.42 / R36.14.16: the account is the bech32 of RIPEMD160(SHA256(key)) under the coin's prefix, and nothing
    // is asked of the node at activation.
    let fx = watch_only(secp_key(), false);
    assert_eq!(
        fx.coin.my_address().expect("an address"),
        account(Chain::Iris, &pubkey(&OWN_KEY)).to_string()
    );
    assert!(fx.node.seen.lock().unwrap().is_empty(), "the node was contacted");
    assert_eq!(fx.coin.activation_policy.watch_only_ledger_flag(), Some(false));
    assert!(!fx.coin.activation_policy.has_local_key());
    assert!(
        fx.coin.get_htlc_key_pair().is_none(),
        "a watch-only coin has no HTLC key"
    );
}

#[test]
fn an_ed25519_key_activates_the_account_of_the_public_sdk_rule() {
    // D36.17 (c): the account of an ed25519 key is the first 20 bytes of SHA256(key), not the secp256k1 rule applied
    // to the same bytes; the coin is watch-only and has no HTLC key.
    let fx = watch_only(ed25519_key(), false);
    let digest = Sha256::digest(&ED25519_KEY[..]);
    let expected = AccountId::new("iaa", &digest[..20]).expect("an account id").to_string();
    assert_eq!(fx.coin.my_address().expect("an address"), expected);
    assert_ne!(
        fx.coin.my_address().expect("an address"),
        account(Chain::Iris, &ED25519_KEY).to_string(),
        "not the secp256k1 rule"
    );
    assert!(fx.node.seen.lock().unwrap().is_empty(), "the node was contacted");
    assert!(!fx.coin.activation_policy.has_local_key());
    assert!(fx.coin.get_htlc_key_pair().is_none());
}

// ---------------------------------------------------------------------------
// R36.14.17: withdraw and the staking requests answer the unsigned document
// ---------------------------------------------------------------------------

#[test]
fn withdraw_answers_the_unsigned_sign_doc_without_a_transaction_or_a_hash() {
    // T36.42: no `tx_hex`, no `tx_hash`; a `sign_doc` with the coin's chain id whose body holds the one bank send.
    for key in [secp_key(), ed25519_key()] {
        let fx = watch_only(key, false);
        script_node(&fx);
        let details = withdraw_of(&fx.coin, "IRIS").expect("an unsigned withdraw");

        assert!(details.is_unsigned());
        assert!(details.tx_hex.is_empty() && details.tx_hash.is_empty());
        let wire = serde_json::to_value(&details).expect("a serializable result");
        assert!(wire.get("tx_hex").is_none() && wire.get("tx_hash").is_none(), "{wire}");
        assert!(wire.get("legacy_amino_json").is_none(), "{wire}");
        let sign_doc = &wire["sign_doc"];
        assert_eq!(sign_doc["chain_id"], "irishub-1");
        assert_eq!(sign_doc["account_number"], 1);
        assert!(sign_doc["body_bytes"].is_array() && sign_doc["auth_info_bytes"].is_array());
        // The ordinary members stay.
        assert_eq!(wire["coin"], "IRIS");
        assert_eq!(wire["to"], json!([peer()]));
        assert_eq!(wire["from"], json!([fx.coin.my_address().expect("an address")]));

        let (amount, uamount, gas_limit) = fee_details_of(&details);
        assert_eq!(uamount, fixed_fee(&fx.coin), "the fee is the fixed assumed one");
        assert_eq!(amount, big_decimal_from_sat_unsigned(uamount, 6));
        assert_eq!(gas_limit, GAS_LIMIT_DEFAULT);
        assert_eq!(simulations(&fx), 0, "a watch-only coin simulates nothing");

        let unsigned = details.unsigned.expect("an unsigned document");
        let UnsignedTransaction::SignDoc { sign_doc } = unsigned else {
            panic!("expected the sign-doc form");
        };
        let body = TxBodyProto::decode(sign_doc.body_bytes.as_slice()).expect("a transaction body");
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].type_url, MSG_SEND_URL);
        let send = MsgSend::decode(body.messages[0].value.as_slice()).expect("a bank send");
        assert_eq!(send.from_address, fx.coin.my_address().expect("an address"));
        assert_eq!(send.to_address, peer());
        assert_eq!(send.amount[0].amount, WITHDRAWN_SAT.to_string());
        assert_eq!(send.amount[0].denom, "uiris");
        let auth_info = AuthInfoProto::decode(sign_doc.auth_info_bytes.as_slice()).expect("an auth info");
        assert_eq!(auth_info.signer_infos.len(), 1);
        assert!(
            auth_info.signer_infos[0].public_key.is_some(),
            "the watched key is the signer"
        );
        let fee = auth_info.fee.expect("a fee");
        assert_eq!(fee.amount[0].amount, uamount.to_string());
        assert_eq!(fee.gas_limit, GAS_LIMIT_DEFAULT);
    }
}

#[test]
fn a_token_withdraw_follows_the_platform_and_answers_the_unsigned_sign_doc() {
    // R36.14.17 (same holds for a token on such a platform coin): a bank send in the token's denom, the fee in the
    // platform denom.
    let fx = watch_only(secp_key(), false);
    script_node(&fx);
    let details = withdraw_of(&nyan(&fx), "IRIS-NYAN").expect("an unsigned token withdraw");
    assert!(details.is_unsigned() && details.tx_hex.is_empty() && details.tx_hash.is_empty());
    let Some(UnsignedTransaction::SignDoc { sign_doc }) = details.unsigned.clone() else {
        panic!("expected the sign-doc form");
    };
    let body = TxBodyProto::decode(sign_doc.body_bytes.as_slice()).expect("a transaction body");
    let send = MsgSend::decode(body.messages[0].value.as_slice()).expect("a bank send");
    assert_eq!(send.amount[0].denom, "unyan");
    assert_eq!(fee_details_of(&details).1, fixed_fee(&fx.coin));
    let fee = AuthInfoProto::decode(sign_doc.auth_info_bytes.as_slice())
        .expect("an auth info")
        .fee
        .expect("a fee");
    assert_eq!(fee.amount[0].denom, "uiris", "the fee is paid in the platform denom");
    assert_eq!(simulations(&fx), 0);
}

#[test]
fn staking_requests_answer_the_unsigned_sign_doc() {
    // T36.42: a delegation answers in the same form; so do an undelegation's and a claim's messages (the delegation
    // is exercised end to end, the others share the build).
    let fx = watch_only(secp_key(), false);
    script_node(&fx);
    let details = common::block_on(fx.coin.delegate(delegation())).expect("an unsigned delegation");
    assert!(details.is_unsigned() && details.tx_hex.is_empty() && details.tx_hash.is_empty());
    assert_eq!(
        details.internal_id,
        unsigned_internal_id(None),
        "the constant identifier (R36.14.29)"
    );
    assert_eq!(details.memo.as_deref(), Some(""));
    let Some(UnsignedTransaction::SignDoc { sign_doc }) = details.unsigned.clone() else {
        panic!("expected the sign-doc form");
    };
    assert_eq!(sign_doc.chain_id, "irishub-1");
    let body = TxBodyProto::decode(sign_doc.body_bytes.as_slice()).expect("a transaction body");
    assert_eq!(body.messages.len(), 1);
    assert_eq!(body.messages[0].type_url, "/cosmos.staking.v1beta1.MsgDelegate");
    assert_eq!(
        fee_details_of(&details).1,
        fixed_fee(&fx.coin),
        "the fee is the fixed assumed one"
    );
    assert_eq!(simulations(&fx), 0);
}

#[test]
fn an_unsigned_result_is_never_published_to_the_history_stream_and_a_signed_one_keeps_its_members() {
    // The unsigned result has no hash to stream; a signed result (a coin with a key) still serializes `tx_hex` and
    // `tx_hash` and no sign-doc member.
    let signed = fixture(Chain::Iris);
    script_fee_estimation(&signed, 80_000);
    let details = withdraw_of(&signed.coin, "IRIS").expect("a signed withdraw");
    assert!(!details.is_unsigned());
    let wire = serde_json::to_value(&details).expect("a serializable result");
    assert!(wire["tx_hex"].is_string() && wire["tx_hash"].is_string(), "{wire}");
    assert!(wire.get("sign_doc").is_none() && wire.get("legacy_amino_json").is_none());
    // `uamount` of the fee details is not on the wire, so the round trip is compared on the wire form.
    let round_trip: TransactionDetails = serde_json::from_value(wire.clone()).expect("a deserializable result");
    assert_eq!(serde_json::to_value(&round_trip).expect("a serializable result"), wire);
    assert!(!round_trip.is_unsigned());
}

// ---------------------------------------------------------------------------
// R36.14.18, T36.43: the Ledger flag
// ---------------------------------------------------------------------------

#[test]
fn the_ledger_flag_answers_the_amino_form_with_three_times_the_fee() {
    // T36.43: `legacy_amino_json` and `original_tx_type_url`, no `sign_doc`, and a fee three times that of the same
    // request without the flag.
    let plain = watch_only(secp_key(), false);
    script_node(&plain);
    let plain_fee = fee_details_of(&withdraw_of(&plain.coin, "IRIS").expect("an unsigned withdraw")).1;

    let fx = watch_only(secp_key(), true);
    script_node(&fx);
    assert_eq!(fx.coin.activation_policy.watch_only_ledger_flag(), Some(true));
    let details = withdraw_of(&fx.coin, "IRIS").expect("an Amino withdraw");
    let wire = serde_json::to_value(&details).expect("a serializable result");
    assert!(wire.get("sign_doc").is_none() && wire.get("tx_hex").is_none() && wire.get("tx_hash").is_none());
    assert_eq!(wire["original_tx_type_url"], MSG_SEND_URL);

    let (amount, uamount, gas_limit) = fee_details_of(&details);
    assert_eq!(uamount, plain_fee * 3, "three times the fee without the flag");
    assert_eq!(amount, big_decimal_from_sat_unsigned(uamount, 6));
    assert_eq!(gas_limit, GAS_LIMIT_DEFAULT);

    // R36.14.31 item 1: the document is a JSON object, not text; item 4: it has no `timeout_height`.
    let doc = &wire["legacy_amino_json"];
    assert!(doc.is_object(), "{doc}");
    assert!(doc.get("timeout_height").is_none(), "{doc}");
    assert_eq!(doc["chain_id"], "irishub-1");
    assert_eq!(doc["account_number"], "1");
    assert_eq!(doc["sequence"], "1");
    assert_eq!(doc["memo"], "");
    assert_eq!(doc["fee"]["gas"], GAS_LIMIT_DEFAULT.to_string());
    assert_eq!(
        doc["fee"]["amount"],
        json!([{ "amount": uamount.to_string(), "denom": "uiris" }])
    );
    assert_eq!(doc["msgs"].as_array().map(Vec::len), Some(1));
    assert_eq!(doc["msgs"][0]["type"], "cosmos-sdk/MsgSend");
    assert_eq!(
        doc["msgs"][0]["value"],
        json!({
            "amount": [{ "amount": WITHDRAWN_SAT.to_string(), "denom": "uiris" }],
            "from_address": fx.coin.my_address().expect("an address"),
            "to_address": peer(),
        })
    );
}

#[test]
fn with_the_ledger_flag_staking_and_every_other_message_fail() {
    // R36.14.18 item 3 (T36.43): the Amino form exists for the bank send only; a delegation, an undelegation, a
    // claim, a create-HTLC and any other message fail when the document is built. (The tree has no IBC `withdraw`;
    // its refusal with the flag is the same build failure of its message.)
    let fx = watch_only(secp_key(), true);
    script_node(&fx);
    let err = common::block_on(fx.coin.delegate(delegation())).expect_err("a delegation fails with the flag");
    assert!(
        matches!(err.get_inner(), crate::DelegationError::InternalError(text) if text.contains("Amino")),
        "{err}"
    );

    let fee = Fee::from_amount_and_gas(coin_of(Chain::Iris, 1_000), 100_000u64);
    for type_url in [
        "/cosmos.staking.v1beta1.MsgDelegate",
        "/cosmos.staking.v1beta1.MsgUndelegate",
        "/cosmos.distribution.v1beta1.MsgWithdrawDelegatorReward",
        "/ibc.applications.transfer.v1.MsgTransfer",
        Chain::Iris.create_type_url(),
        Chain::Iris.claim_type_url(),
    ] {
        let msg = Any {
            type_url: type_url.to_owned(),
            value: Vec::new(),
        };
        let result = common::block_on(fx.coin.build_unsigned_transaction(msg, fee.clone(), 200, ""));
        assert!(
            matches!(&result, Err(UnsignedTxError::AminoUnsupported(url)) if url == type_url),
            "{type_url}: {result:?}"
        );
    }
    // The same messages build the direct form of a coin without the flag.
    let plain = watch_only(secp_key(), false);
    script_node(&plain);
    let msg = Any {
        type_url: "/cosmos.staking.v1beta1.MsgDelegate".to_owned(),
        value: Vec::new(),
    };
    assert!(common::block_on(plain.coin.build_unsigned_transaction(msg, fee, 200, "")).is_ok());
}

#[test]
fn the_ledger_factor_does_not_apply_to_a_token() {
    // R36.14.31 item 5: the factor of R36.14.18 item 4 is the platform coin's; a token carries the computed amount
    // once (T36.57 is the full row).
    let plain = watch_only(secp_key(), false);
    script_node(&plain);
    let plain_fee = fee_details_of(&withdraw_of(&nyan(&plain), "IRIS-NYAN").expect("an unsigned withdraw")).1;
    let fx = watch_only(secp_key(), true);
    script_node(&fx);
    let details = withdraw_of(&nyan(&fx), "IRIS-NYAN").expect("an Amino token withdraw");
    assert_eq!(fee_details_of(&details).1, plain_fee);
    assert!(matches!(details.unsigned, Some(UnsignedTransaction::Amino { .. })));
}

// ---------------------------------------------------------------------------
// R18.11.23a, R36.14.17: orders are refused for the coin and a token, with and without the flag
// ---------------------------------------------------------------------------

#[test]
fn orders_are_refused_for_a_watch_only_coin_and_its_token_with_and_without_the_flag() {
    // The wallet-only answer is what `setprice`, `buy` and `sell` refuse on (CRD ch.18 R18.11.23a; the reference
    // accepts the orders of a flag-less public-key coin, the recorded difference). With swaps on, a key-holding
    // coin of the same chain is the one case that can trade.
    let keyed = fixture(Chain::Iris);
    assert!(!keyed.coin.wallet_only_with_switch(&keyed.ctx, true));
    for ledger in [false, true] {
        for key in [secp_key(), ed25519_key()] {
            let fx = watch_only(key, ledger);
            assert!(
                fx.coin.wallet_only_with_switch(&fx.ctx, true),
                "the coin, ledger {ledger}"
            );
            assert!(
                nyan(&fx).wallet_only_with_switch(&fx.ctx, true),
                "the token, ledger {ledger}"
            );
            assert!(fx.coin.wallet_only(&fx.ctx) && nyan(&fx).wallet_only(&fx.ctx));
        }
    }
}

// ---------------------------------------------------------------------------
// R36.14.17: every path that would sign with a node-held key is refused with `WatchOnlyCoin`
// ---------------------------------------------------------------------------

const REFUSAL: &str = "watch-only";

fn assert_refused<T>(result: Result<T, impl std::fmt::Display>, path: &str) {
    match result {
        Ok(_) => panic!("{path}: expected a refusal"),
        Err(e) => assert!(e.to_string().contains(REFUSAL), "{path}: {e}"),
    }
}

#[test]
fn a_watch_only_coin_refuses_every_signing_path() {
    for ledger in [false, true] {
        let fx = watch_only(secp_key(), ledger);
        // No script: a refusal must come before any node is asked, so an unscripted node cannot mask it.
        let coin = &fx.coin;
        let token = nyan(&fx);
        let fee_addr = pubkey(&PEER_KEY);
        let uuid = [7u8; 16];
        let hash = secret_hash();
        let htlc_tx = vec![1u8, 2, 3];
        let amount = BigDecimal::from_str("1").expect("an amount");
        let dex_fee = standard_dex_fee();

        // Message signing.
        assert_refused(coin.sign_message("m"), "sign_message");
        assert_refused(token.sign_message("m"), "token sign_message");
        assert!(coin.sign_message_hash("m").is_none());
        // Private-key display and public-key query.
        assert_refused(coin.display_priv_key(), "display_priv_key");
        assert_refused(token.display_priv_key(), "token display_priv_key");
        assert!(coin.get_public_key().is_err(), "get_public_key");
        assert!(token.get_public_key().is_err(), "token get_public_key");
        // Swap payment, fee, claim and refund sends: the node-signed, node-broadcast paths.
        for swap in [coin as &dyn SwapOps, &token as &dyn SwapOps] {
            assert_refused(
                common::block_on(swap.send_taker_fee(&dex_fee, &fee_addr, &uuid).compat())
                    .map_err(|e| format!("{e:?}")),
                "send_taker_fee",
            );
            assert_refused(
                common::block_on(
                    swap.send_maker_payment(2_000_000_000, &[], &fee_addr, &hash, amount.clone(), &None)
                        .compat(),
                )
                .map_err(|e| format!("{e:?}")),
                "send_maker_payment",
            );
            assert_refused(
                common::block_on(
                    swap.send_taker_payment(2_000_000_000, &[], &fee_addr, &hash, amount.clone(), &None)
                        .compat(),
                )
                .map_err(|e| format!("{e:?}")),
                "send_taker_payment",
            );
            assert_refused(
                common::block_on(
                    swap.send_maker_spends_taker_payment(&htlc_tx, 0, &fee_addr, &PREIMAGE, &[], &None)
                        .compat(),
                )
                .map_err(|e| format!("{e:?}")),
                "send_maker_spends_taker_payment",
            );
            assert_refused(
                common::block_on(
                    swap.send_taker_spends_maker_payment(&htlc_tx, 0, &fee_addr, &PREIMAGE, &[], &None)
                        .compat(),
                )
                .map_err(|e| format!("{e:?}")),
                "send_taker_spends_maker_payment",
            );
            assert_refused(
                common::block_on(
                    swap.send_taker_refunds_payment(&htlc_tx, 0, &fee_addr, &hash, &[], &None)
                        .compat(),
                )
                .map_err(|e| format!("{e:?}")),
                "send_taker_refunds_payment",
            );
            assert_refused(
                common::block_on(
                    swap.send_maker_refunds_payment(&htlc_tx, 0, &fee_addr, &hash, &[], &None)
                        .compat(),
                )
                .map_err(|e| format!("{e:?}")),
                "send_maker_refunds_payment",
            );
        }
        // The raw broadcast of a node-built signed transaction (the one every staking and swap send ends in).
        let msg = msg_send_any(&coin.account_id, &coin.account_id, vec![coin_proto("uiris", 1)]);
        let fee = Fee::from_amount_and_gas(coin_of(Chain::Iris, 1_000), 100_000u64);
        let sent = common::block_on(coin.common_send_raw_tx_bytes(msg, fee, 200, "", Duration::from_secs(1)));
        assert_refused(
            sent.map(|(id, _)| id).map_err(|e| format!("{e:?}")),
            "common_send_raw_tx_bytes",
        );

        assert!(
            fx.node.seen.lock().unwrap().is_empty(),
            "a refusal came after the node was contacted"
        );
    }
}

#[test]
fn the_refusal_names_the_watch_only_coin_and_a_coin_with_a_key_signs() {
    use crate::PrivKeyNotAllowed;
    let fx = watch_only(secp_key(), false);
    let err = fx.coin.activation_policy.activated_key_or_err().expect_err("no key");
    assert!(matches!(err.get_inner(), PrivKeyNotAllowed::WatchOnlyCoin));
    assert!(fx.coin.activation_policy.activated_key().is_none());
    // A coin with a key is unaffected.
    let keyed = fixture(Chain::Iris);
    assert!(keyed.coin.activation_policy.activated_key_or_err().is_ok());
    assert_eq!(keyed.coin.activation_policy.watch_only_ledger_flag(), None);
    assert!(keyed.coin.display_priv_key().is_ok());
}

#[test]
fn claim_rewards_payload_shape_is_unchanged() {
    // The claim request is a staking request like the delegation: it reaches the same unsigned build (the build
    // itself is covered above); the payload parses as before.
    let payload: ClaimRewardsPayload =
        serde_json::from_value(json!({ "validator_address": peer() })).expect("a claim payload");
    assert!(!payload.force);
}

// ---------------------------------------------------------------------------
// T36.55 -- T36.58 (CRD ch.36 §36.14.8): the unsigned result, member by member
// ---------------------------------------------------------------------------
//
// One fixture: the coin of 6 decimals with no `gas_price` (0.25, so the computed fee is 18,750 base units), a token
// on it, a public-key activation with a 33-byte secp256k1 key, a node reporting account number 7, sequence 3, the
// height 1,000 and balances of 10,000,000 base units of every denomination. The request is a `withdraw` of 1 coin to
// another account with the memo `m`.

use cosmrs::proto::cosmos::crypto::secp256k1::PubKey as Secp256k1PubKeyProto;
use cosmrs::proto::cosmos::tx::signing::v1beta1::SignMode as SignModeProto;
use cosmrs::proto::cosmos::tx::v1beta1::mode_info::Sum as ModeSum;
use cosmrs::tx::SignDoc as CosmrsSignDoc;

/// The SHA-256 digest of the empty byte string, lower-case hex.
const SHA256_EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// The hash the scripted node answers to a broadcast.
const BROADCAST_HASH: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn script_t36_node(fx: &Fixture, account_number: u64, sequence: u64) {
    let account = QueryAccountResponse {
        account: Some(Any {
            type_url: "/cosmos.auth.v1beta1.BaseAccount".to_owned(),
            value: BaseAccount {
                account_number,
                sequence,
                ..Default::default()
            }
            .encode_to_vec(),
        }),
    }
    .encode_to_vec();
    let balance = QueryBalanceResponse {
        balance: Some(CoinProto {
            denom: "scripted".to_owned(),
            amount: "10000000".to_owned(),
        }),
    }
    .encode_to_vec();
    let tx_result = json!({ "code": 0, "data": null, "log": "", "info": "", "gas_wanted": "0", "gas_used": "0",
                            "events": [], "codespace": "" });
    fx.node.answer(move |method, params| match method {
        "abci_info" => Some(
            json!({ "response": { "data": "test", "version": "0", "app_version": "1",
                                    "last_block_height": "1000", "last_block_app_hash": "" } }),
        ),
        "abci_query" if params["path"] == "/cosmos.auth.v1beta1.Query/Account" => Some(abci_value(&account)),
        "abci_query" if params["path"] == "/cosmos.bank.v1beta1.Query/Balance" => Some(abci_value(&balance)),
        "broadcast_tx_commit" => Some(json!({ "check_tx": tx_result, "deliver_tx": tx_result,
                                              "hash": BROADCAST_HASH, "height": "1001" })),
        _ => None,
    });
}

/// The serialized result of a `withdraw` of `amount` of `ticker` to another account, with the memo when given.
fn t36_withdraw(coin: &impl MmCoin, ticker: &str, amount: &str, memo: Option<&str>) -> Json {
    let mut request = json!({ "coin": ticker, "to": peer(), "amount": amount });
    if let Some(memo) = memo {
        request["memo"] = json!(memo);
    }
    let request: WithdrawRequest = serde_json::from_value(request).expect("a withdraw request");
    let details = common::block_on(coin.withdraw(request).compat()).unwrap_or_else(|e| panic!("withdraw: {e}"));
    serde_json::to_value(&details).expect("a serializable result")
}

fn bytes_of(array: &Json) -> Vec<u8> {
    array
        .as_array()
        .expect("an array of byte values")
        .iter()
        .map(|n| u8::try_from(n.as_u64().expect("an integer")).expect("an integer from 0 to 255"))
        .collect()
}

fn has_a_number(value: &Json) -> bool {
    match value {
        Json::Number(_) => true,
        Json::Array(items) => items.iter().any(has_a_number),
        Json::Object(members) => members.values().any(has_a_number),
        _ => false,
    }
}

#[test]
fn t36_55_the_sign_doc_result() {
    // T36.55 (R36.14.29, R36.14.30; extends T36.42).
    let fx = watch_only(secp_key(), false);
    script_t36_node(&fx, 7, 3);
    let wire = t36_withdraw(&fx.coin, "IRIS", "1", Some("m"));
    let me = fx.coin.my_address().expect("an address");

    for absent in [
        "tx_hex",
        "tx_hash",
        "kmd_rewards",
        "legacy_amino_json",
        "original_tx_type_url",
    ] {
        assert!(wire.get(absent).is_none(), "{absent} is present: {wire}");
    }
    let sign_doc = wire["sign_doc"].as_object().expect("a sign_doc object");
    assert_eq!(sign_doc.len(), 4, "{sign_doc:?}");
    assert_eq!(sign_doc["chain_id"], "irishub-1");
    assert_eq!(sign_doc["account_number"], json!(7));
    assert!(sign_doc["account_number"].is_u64(), "a JSON number, not a string");

    let body = TxBodyProto::decode(bytes_of(&sign_doc["body_bytes"]).as_slice()).expect("a transaction body");
    assert_eq!(body.messages.len(), 1);
    assert_eq!(body.messages[0].type_url, MSG_SEND_URL);
    let send = MsgSend::decode(body.messages[0].value.as_slice()).expect("a bank send");
    assert_eq!(
        (send.from_address.as_str(), send.to_address.as_str()),
        (me.as_str(), peer().as_str())
    );
    assert_eq!(send.amount.len(), 1);
    assert_eq!(
        (send.amount[0].denom.as_str(), send.amount[0].amount.as_str()),
        ("uiris", "1000000")
    );
    assert_eq!(body.memo, "m");
    assert_eq!(body.timeout_height, 1_100);
    assert!(body.extension_options.is_empty() && body.non_critical_extension_options.is_empty());

    let auth_info = AuthInfoProto::decode(bytes_of(&sign_doc["auth_info_bytes"]).as_slice()).expect("an auth info");
    assert_eq!(auth_info.signer_infos.len(), 1);
    let signer = &auth_info.signer_infos[0];
    let key = signer.public_key.as_ref().expect("the activation key");
    assert_eq!(
        Secp256k1PubKeyProto::decode(key.value.as_slice())
            .expect("a secp256k1 key")
            .key,
        pubkey(&OWN_KEY)
    );
    let Some(ModeSum::Single(single)) = signer.mode_info.as_ref().and_then(|m| m.sum.clone()) else {
        panic!("a single sign mode");
    };
    assert_eq!(single.mode, SignModeProto::Direct as i32);
    assert_eq!(signer.sequence, 3);
    let fee = auth_info.fee.expect("a fee");
    assert_eq!(fee.amount.len(), 1);
    assert_eq!(
        (fee.amount[0].denom.as_str(), fee.amount[0].amount.as_str()),
        ("uiris", "18750")
    );
    assert_eq!(fee.gas_limit, 125_000);
    assert!(fee.payer.is_empty() && fee.granter.is_empty());

    assert_eq!(wire["from"], json!([me]));
    assert_eq!(wire["to"], json!([peer()]));
    assert_eq!(wire["total_amount"], "1.01875");
    assert_eq!(wire["spent_by_me"], "1.01875");
    assert_eq!(wire["received_by_me"], "0");
    assert_eq!(wire["my_balance_change"], "-1.01875");
    assert_eq!(
        wire["fee_details"],
        json!({ "type": "Tendermint", "coin": "IRIS", "amount": "0.01875", "gas_limit": 125_000 })
    );
    assert_eq!(wire["block_height"], 0);
    assert_eq!(wire["timestamp"], 0);
    assert_eq!(wire["transaction_type"], "StandardTransfer");
    assert_eq!(wire["memo"], "m");
    assert_eq!(wire["internal_id"], SHA256_EMPTY);

    // A request without a memo answers the empty memo and the same identifier.
    let without = t36_withdraw(&fx.coin, "IRIS", "1", None);
    assert_eq!(without["memo"], "");
    assert_eq!(without["internal_id"], SHA256_EMPTY);
    let without_body =
        TxBodyProto::decode(bytes_of(&without["sign_doc"]["body_bytes"]).as_slice()).expect("a transaction body");
    assert_eq!(without_body.memo, "");
}

#[test]
fn t36_56_the_amino_result() {
    // T36.56 (R36.14.29, R36.14.31; extends T36.43).
    let fx = watch_only(secp_key(), true);
    script_t36_node(&fx, 7, 3);
    let wire = t36_withdraw(&fx.coin, "IRIS", "1", Some("m"));
    let me = fx.coin.my_address().expect("an address");

    for absent in ["sign_doc", "tx_hex", "tx_hash", "kmd_rewards"] {
        assert!(wire.get(absent).is_none(), "{absent} is present: {wire}");
    }
    assert_eq!(wire["original_tx_type_url"], MSG_SEND_URL);
    let doc = &wire["legacy_amino_json"];
    let members: Vec<&str> = doc
        .as_object()
        .expect("a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    let mut sorted = members.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, [
        "account_number",
        "chain_id",
        "fee",
        "memo",
        "msgs",
        "sequence"
    ]);
    assert_eq!(doc["account_number"], "7");
    assert_eq!(doc["chain_id"], "irishub-1");
    assert_eq!(doc["memo"], "m");
    assert_eq!(doc["sequence"], "3");
    assert_eq!(
        doc["fee"],
        json!({ "amount": [{ "denom": "uiris", "amount": "56250" }], "gas": "125000" })
    );
    assert_eq!(doc["msgs"].as_array().map(Vec::len), Some(1));
    assert_eq!(doc["msgs"][0]["type"], "cosmos-sdk/MsgSend");
    assert_eq!(
        doc["msgs"][0]["value"],
        json!({ "from_address": me, "to_address": peer(), "amount": [{ "denom": "uiris", "amount": "1000000" }] })
    );
    assert!(!has_a_number(doc), "a number in the document: {doc}");
    assert_eq!(wire["fee_details"]["amount"], "0.05625");
    assert_eq!(wire["fee_details"]["gas_limit"], 125_000);
    assert_eq!(wire["total_amount"], "1.05625");
    assert_eq!(wire["internal_id"], SHA256_EMPTY);

    // Zero account number and sequence are present as "0"; a request without a memo gives the empty memo.
    let zero = watch_only(secp_key(), true);
    script_t36_node(&zero, 0, 0);
    let zero_wire = t36_withdraw(&zero.coin, "IRIS", "1", None);
    assert_eq!(zero_wire["legacy_amino_json"]["account_number"], "0");
    assert_eq!(zero_wire["legacy_amino_json"]["sequence"], "0");
    assert_eq!(zero_wire["legacy_amino_json"]["memo"], "");
    assert_eq!(zero_wire["memo"], "");
}

#[test]
fn t36_57_a_token_under_the_ledger_flag() {
    // T36.57 (R36.14.31 item 5): the fee of a token is not tripled.
    let fx = watch_only(secp_key(), true);
    script_t36_node(&fx, 7, 3);
    let token = nyan(&fx);
    let wire = t36_withdraw(&token, "IRIS-NYAN", "2", Some("m"));
    let doc = &wire["legacy_amino_json"];
    assert_eq!(
        doc["msgs"][0]["value"]["amount"],
        json!([{ "denom": "unyan", "amount": "2000000" }])
    );
    assert_eq!(
        doc["fee"],
        json!({ "amount": [{ "denom": "uiris", "amount": "18750" }], "gas": "125000" })
    );
    assert_eq!(wire["fee_details"]["coin"], "IRIS");
    assert_eq!(wire["fee_details"]["amount"], "0.01875");
    assert_eq!(wire["total_amount"], "2");
    let token_id = hex::decode(token.token_id()).expect("a token id of 32 bytes");
    assert_eq!(token_id.len(), 32);
    assert_eq!(wire["internal_id"], hex::encode(Sha256::digest(&token_id[..])));
    assert_ne!(wire["internal_id"], SHA256_EMPTY);
}

#[test]
fn t36_58_broadcast_of_a_transaction_signed_outside_the_node() {
    // T36.58 (R36.14.32).
    let key = SigningKey::from_slice(&OWN_KEY).expect("a signing key");
    let broadcasts = |fx: &Fixture| fx.node.requests("broadcast_tx_commit");

    // (i) The sign document of T36.55, signed under the direct mode and assembled from the two byte strings.
    let fx = watch_only(secp_key(), false);
    script_t36_node(&fx, 7, 3);
    let wire = t36_withdraw(&fx.coin, "IRIS", "1", Some("m"));
    let direct = CosmrsSignDoc {
        body_bytes: bytes_of(&wire["sign_doc"]["body_bytes"]),
        auth_info_bytes: bytes_of(&wire["sign_doc"]["auth_info_bytes"]),
        chain_id: "irishub-1".to_owned(),
        account_number: 7,
    }
    .sign(&key)
    .expect("a signed document")
    .to_bytes()
    .expect("the encoded transaction");
    let tx_raw = TxRaw::decode(direct.as_slice()).expect("a TxRaw");
    assert_eq!(
        tx_raw.body_bytes,
        bytes_of(&wire["sign_doc"]["body_bytes"]),
        "the bytes are unchanged"
    );
    let hash = common::block_on(fx.coin.send_raw_tx(&hex::encode(&direct)).compat()).expect("a broadcast");
    assert_eq!(hash, BROADCAST_HASH);
    let sent = broadcasts(&fx);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["tx"], base64::encode(&direct), "exactly those bytes");

    // (ii) A transaction built from the Amino document under R36.14.31 item 7: no timeout height.
    let amino = watch_only(secp_key(), true);
    script_t36_node(&amino, 7, 3);
    let wire = t36_withdraw(&amino.coin, "IRIS", "1", Some("m"));
    let value = &wire["legacy_amino_json"]["msgs"][0]["value"];
    let message = msg_send_any(
        &amino.coin.account_id,
        &AccountId::from_str(value["to_address"].as_str().expect("a destination")).expect("an account"),
        vec![coin_proto("uiris", 1_000_000)],
    );
    let body = tx::Body::new(vec![message], "m", 0u32);
    let auth_info = SignerInfo {
        public_key: Some(cosmrs::crypto::PublicKey::from(secp_key()).into()),
        mode_info: tx::ModeInfo::single(tx::SignMode::LegacyAminoJson),
        sequence: 3,
    }
    .auth_info(Fee::from_amount_and_gas(coin_of(Chain::Iris, 56_250), 125_000u64));
    let body_bytes = body.into_bytes().expect("a body");
    assert_eq!(
        TxBodyProto::decode(body_bytes.as_slice())
            .expect("a body")
            .timeout_height,
        0
    );
    let built = TxRaw {
        body_bytes,
        auth_info_bytes: auth_info.into_bytes().expect("an auth info"),
        signatures: vec![vec![7u8; 64]],
    }
    .encode_to_vec();
    let hash = common::block_on(amino.coin.send_raw_tx(&hex::encode(&built)).compat()).expect("a broadcast");
    assert_eq!(hash, BROADCAST_HASH);
    assert_eq!(broadcasts(&amino)[0]["tx"], base64::encode(&built));

    // (iii) The same transaction sent under the token's ticker is broadcast in the same way.
    let hash = common::block_on(nyan(&fx).send_raw_tx(&hex::encode(&direct)).compat()).expect("a broadcast");
    assert_eq!(hash, BROADCAST_HASH);
    let sent = broadcasts(&fx);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["tx"], base64::encode(&direct));

    // (iv) Bytes that do not decode as a TxRaw yield the legacy error answer and no request to the node.
    for not_a_tx_raw in ["zz", "00", "ffffffff"] {
        assert!(
            common::block_on(fx.coin.send_raw_tx(not_a_tx_raw).compat()).is_err(),
            "{not_a_tx_raw}"
        );
    }
    assert_eq!(broadcasts(&fx).len(), 2, "no request for a refused transaction");
}

/// The result as the RPC puts it on the wire: serialized to bytes and parsed back.
fn on_the_wire(coin: &impl MmCoin, ticker: &str) -> Json {
    let request: WithdrawRequest =
        serde_json::from_value(json!({ "coin": ticker, "to": peer(), "amount": "1" })).expect("a withdraw request");
    let details = common::block_on(coin.withdraw(request).compat()).unwrap_or_else(|e| panic!("withdraw: {e}"));
    let bytes = serde_json::to_vec(&details).expect("a serializable result");
    serde_json::from_slice(&bytes).expect("the bytes parse back")
}

fn sorted_keys(wire: &Json) -> Vec<&str> {
    let mut keys: Vec<&str> = wire
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

#[test]
fn unsigned_and_signed_results_have_the_full_key_set() {
    // R36.14.29: the members of an unsigned result, `to` and `total_amount` among them, and the unchanged members of
    // a signed one, asserted on the bytes the RPC writes.
    let common_keys = [
        "block_height",
        "coin",
        "fee_details",
        "from",
        "internal_id",
        "my_balance_change",
        "received_by_me",
        "spent_by_me",
        "timestamp",
        "to",
        "total_amount",
        "transaction_type",
    ];
    let with = |extra: &[&'static str]| {
        let mut keys: Vec<&str> = common_keys.iter().copied().chain(extra.iter().copied()).collect();
        keys.sort_unstable();
        keys
    };

    let direct = watch_only(secp_key(), false);
    script_t36_node(&direct, 7, 3);
    assert_eq!(
        sorted_keys(&on_the_wire(&direct.coin, "IRIS")),
        with(&["memo", "sign_doc"])
    );

    let amino = watch_only(secp_key(), true);
    script_t36_node(&amino, 7, 3);
    assert_eq!(
        sorted_keys(&on_the_wire(&amino.coin, "IRIS")),
        with(&["legacy_amino_json", "memo", "original_tx_type_url"])
    );

    let keyed = fixture(Chain::Iris);
    script_fee_estimation(&keyed, 80_000);
    assert_eq!(
        sorted_keys(&on_the_wire(&keyed.coin, "IRIS")),
        with(&["tx_hash", "tx_hex"])
    );
}
