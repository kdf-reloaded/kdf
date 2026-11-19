//! Unit tests of the TRON withdraw fee estimate and amounts (CRD ch.21 R21.10B.7, D21.10B.1).
//!
//! Every test runs the real withdraw of a TRX or TRC20 coin against a stub node on the loopback interface; only
//! the node's answers are canned. The expected figures are computed by hand from the sizes below.
//!
//! Sizes. A TRX transfer whose amount encodes in `n` bytes serializes, signed, to `200 + n` bytes (the raw body is
//! `130 + n`: 4 for the block bytes, 10 for the block hash, 7 for the expiry, `102 + n` for the contract, 7 for
//! the timestamp; it is framed in 3 bytes and followed by the 67 bytes of the signature field). With 64 for the
//! contract's result its bandwidth is `264 + n`: 267 for 1 TRX, which encodes in 3 bytes. A TRC20 transfer whose
//! `fee_limit` encodes in 4 bytes has a raw body of 211 bytes (4 + 10 + 7, 177 for the contract, 7, and 6 for the
//! limit), serializes to 281 bytes and has a bandwidth of 345.

use super::proto;
use super::test_node::{Reply, StubNode};
use super::wallet_path_tests::{trc20_coin, trx_coin, NOW_BLOCK_BODY, TRC20_CONTRACT};
use super::withdraw::withdraw_tron;
use super::TronAddress;
use crate::eth::EvmWithdrawSender;
use crate::{TransactionDetails, TxFeeDetails, WithdrawError, WithdrawRequest};
use bigdecimal::BigDecimal;
use common::block_on;
use prost::Message as _;
use serde_json::{json, Value as Json};
use std::str::FromStr;

/// The recipient of every test withdraw.
const RECEIVER: &str = "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL";
/// The address of the test key (the secret key 1), the sender of every test withdraw.
const SENDER_WIRE_HEX: &str = "417e5f4552091a69125d5dfcb7b8c2659029395bdf";
/// The time of the stub's head block, in milliseconds.
const BLOCK_TIME_MS: i64 = 1_700_000_000_000;

/// 1000 SUN per bandwidth unit, 420 SUN per energy unit, 1 TRX to create an account, 0.1 TRX in place of the
/// bandwidth of an account-creating transfer. No bandwidth rate: the default of 1 applies.
const PARAMETERS: &str = r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420},{"key":"getCreateNewAccountFeeInSystemContract","value":1000000},{"key":"getCreateAccountFee","value":100000},{"key":"getMaintenanceTimeInterval"}]}"#;

/// What the stub node of a fee test answers.
#[derive(Clone)]
struct Chain {
    /// The TRX balance of every account the chain has created, in SUN.
    trx_sun: u64,
    /// The token balance of every address, in token units.
    token_units: u64,
    /// Whether the chain has created the recipient's account.
    recipient_exists: bool,
    /// The `getaccountresource` answer.
    resources: &'static str,
    /// The `getchainparameters` answer.
    parameters: &'static str,
    /// The `energy_used` of the transfer's dry run, when the answer carries one.
    energy: Option<u64>,
    /// An endpoint that answers HTTP 500.
    failing: Option<&'static str>,
}

impl Chain {
    /// 50 TRX and 5 tokens (6 decimals), an existing recipient, the free bandwidth of a fresh account, no staked
    /// resources, a dry run of 14 000 energy.
    fn new() -> Self {
        Chain {
            trx_sun: 50_000_000,
            token_units: 5_000_000,
            recipient_exists: true,
            resources: r#"{"freeNetLimit":600}"#,
            parameters: PARAMETERS,
            energy: Some(14_000),
            failing: None,
        }
    }

    fn start(self) -> StubNode {
        StubNode::start(move |path, body| {
            if self.failing == Some(path) {
                return Reply::status(500, "stub failure");
            }
            let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
            match path {
                "/wallet/getnowblock" => Reply::ok(NOW_BLOCK_BODY),
                "/wallet/getaccount" => {
                    if request["address"] == RECEIVER && !self.recipient_exists {
                        Reply::ok("{}")
                    } else {
                        Reply::ok(&json!({ "balance": self.trx_sun, "create_time": BLOCK_TIME_MS }).to_string())
                    }
                },
                "/wallet/getaccountresource" => Reply::ok(self.resources),
                "/wallet/getchainparameters" => Reply::ok(self.parameters),
                "/wallet/triggerconstantcontract" => match request["function_selector"].as_str() {
                    Some("balanceOf(address)") => {
                        Reply::ok(&json!({ "constant_result": [format!("{:064x}", self.token_units)] }).to_string())
                    },
                    Some("transfer(address,uint256)") => {
                        let mut answer = json!({ "result": { "result": true }, "constant_result": ["00"] });
                        if let Some(energy) = self.energy {
                            answer["energy_used"] = json!(energy);
                        }
                        Reply::ok(&answer.to_string())
                    },
                    _ => Reply::status(404, "{}"),
                },
                other => Reply::status(404, &format!("unexpected endpoint {other}")),
            }
        })
    }
}

fn dec(text: &str) -> BigDecimal { BigDecimal::from_str(text).unwrap_or_else(|e| panic!("{text}: {e}")) }

/// A withdraw request of `amount` to [`RECEIVER`], or of the whole balance for `None`.
fn request(coin: &str, amount: Option<&str>) -> WithdrawRequest {
    let mut request = json!({ "coin": coin, "to": RECEIVER });
    match amount {
        Some(amount) => request["amount"] = json!(amount),
        None => request["max"] = json!(true),
    }
    serde_json::from_value(request).unwrap_or_else(|e| panic!("a withdraw request: {e}"))
}

/// Runs the withdraw `request` of a TRX coin, or of a TRC20 coin when `trc20`, against `node`.
fn withdraw(node: &StubNode, trc20: bool, mut request: WithdrawRequest) -> Result<TransactionDetails, WithdrawError> {
    block_on(async {
        let (_ctx, coin) = if trc20 {
            trc20_coin(&node.url()).await
        } else {
            trx_coin(&node.url()).await
        };
        request.coin = coin.ticker.clone();
        withdraw_tron(coin.clone(), request, EvmWithdrawSender::activated(&coin))
            .await
            .map_err(|e| e.into_inner())
    })
}

fn withdrawn(node: &StubNode, trc20: bool, request: WithdrawRequest) -> TransactionDetails {
    withdraw(node, trc20, request).unwrap_or_else(|e| panic!("the withdraw must succeed (trc20={trc20}): {e:?}"))
}

fn refused(node: &StubNode, trc20: bool, request: WithdrawRequest) -> WithdrawError {
    match withdraw(node, trc20, request) {
        Ok(details) => panic!(
            "the withdraw must be refused (trc20={trc20}), got {:?}",
            details.tx_hash
        ),
        Err(e) => e,
    }
}

/// The fee details of `details` as the JSON a client reads.
fn fee_json(details: &TransactionDetails) -> Json {
    let fee = details.fee_details.as_ref().expect("a TRON withdraw reports its fee");
    assert!(matches!(fee, TxFeeDetails::Tron(_)));
    serde_json::to_value(fee).expect("JSON")
}

/// The raw body of the signed transaction of `details`. The bandwidth the details report must be the size of
/// that transaction plus 64, unless a fixed fee replaced the bandwidth (`bandwidth_used` 0).
fn raw_of(details: &TransactionDetails) -> proto::TransactionRaw {
    let tx = proto::Transaction::decode(details.tx_hex.0.as_slice()).expect("a protobuf transaction");
    assert_eq!(tx.signature.len(), 1);
    assert_eq!(tx.signature[0].len(), 65);
    let reported = fee_json(details)["bandwidth_used"].as_u64().expect("bandwidth_used");
    if reported != 0 {
        assert_eq!(
            reported,
            details.tx_hex.0.len() as u64 + 64,
            "bandwidth is the size plus 64"
        );
    }
    tx.raw_data.expect("raw data")
}

/// The amount of the TRX transfer in `raw`, in SUN.
fn trx_amount_of(raw: &proto::TransactionRaw) -> i64 {
    let any = raw.contract[0].parameter.as_ref().expect("a contract parameter");
    proto::TransferContract::decode(any.value.as_slice())
        .expect("a transfer contract")
        .amount
}

/// The amount of the TRC20 transfer in `raw`, in token units: the last word of the call data.
fn trc20_amount_of(raw: &proto::TransactionRaw) -> u64 {
    let any = raw.contract[0].parameter.as_ref().expect("a contract parameter");
    let call = proto::TriggerSmartContract::decode(any.value.as_slice()).expect("a contract call");
    assert_eq!(call.data.len(), 68);
    assert!(call.data[36..60].iter().all(|byte| *byte == 0));
    let mut word = [0u8; 8];
    word.copy_from_slice(&call.data[60..68]);
    u64::from_be_bytes(word)
}

fn sender_base58() -> String {
    TronAddress::from_hex(SENDER_WIRE_HEX)
        .expect("valid wire form")
        .to_base58()
}

// ---------------------------------------------------------------------------------------------------------------
// Bandwidth: the node's whole-draw rule
// ---------------------------------------------------------------------------------------------------------------

/// A transfer of 1 TRX is 267 bandwidth. It is free when the staked quota covers all of it, free when the free
/// quota covers all of it, and otherwise all of it is burned at 1000 SUN: 0.267 TRX, also where the two quotas
/// together would have covered it.
#[test]
fn trx_bandwidth_is_drawn_whole_from_one_quota_or_burned_whole() {
    for (resources, fee) in [
        // Staked covers the whole size; no free bandwidth is left.
        (r#"{"NetLimit":267,"freeNetLimit":600,"freeNetUsed":600}"#, "0"),
        // Staked is one short, free covers the whole size.
        (r#"{"NetLimit":300,"NetUsed":34,"freeNetLimit":267}"#, "0"),
        // 200 staked and 200 free are left: 400 together, yet neither covers 267.
        (r#"{"NetLimit":200,"freeNetLimit":600,"freeNetUsed":400}"#, "0.267"),
        // One short in each quota.
        (r#"{"NetLimit":266,"freeNetLimit":266}"#, "0.267"),
        // An empty answer is all zero.
        ("{}", "0.267"),
    ] {
        let node = Chain {
            resources,
            ..Chain::new()
        }
        .start();
        let details = withdrawn(&node, false, request("TRX", Some("1")));
        let raw = raw_of(&details);
        assert_eq!(trx_amount_of(&raw), 1_000_000);
        assert_eq!(raw.fee_limit, 0, "a TRX transfer has no fee limit");
        assert_eq!(
            fee_json(&details),
            json!({
                "type": "Tron",
                "coin": "TRX",
                "bandwidth_used": 267,
                "energy_used": 0,
                "bandwidth_fee": fee,
                "energy_fee": "0",
                "total_fee": fee,
            }),
            "{resources}"
        );
        assert_eq!(details.total_amount, dec("1"));
        assert_eq!(
            details.spent_by_me,
            dec("1") + dec(fee),
            "the fee is spent with the amount"
        );
        assert_eq!(details.received_by_me, dec("0"));
        assert_eq!(details.my_balance_change, dec("0") - (dec("1") + dec(fee)));
        assert_eq!(details.from, [sender_base58()]);
        assert_eq!(details.to, [RECEIVER]);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// A recipient the chain has not created
// ---------------------------------------------------------------------------------------------------------------

/// T21.10B.11: a TRX transfer to an absent address adds the account-creation fee of 1 TRX. Its bandwidth is free
/// when the staked quota covers it; otherwise the fixed 0.1 TRX replaces it and no bandwidth is used. The free
/// quota does not count.
#[test]
fn a_trx_transfer_to_an_absent_address_pays_the_account_creation_fee() {
    let covered = Chain {
        recipient_exists: false,
        resources: r#"{"NetLimit":267}"#,
        ..Chain::new()
    }
    .start();
    let details = withdrawn(&covered, false, request("TRX", Some("1")));
    raw_of(&details);
    assert_eq!(
        fee_json(&details),
        json!({
            "type": "Tron",
            "coin": "TRX",
            "bandwidth_used": 267,
            "energy_used": 0,
            "bandwidth_fee": "0",
            "energy_fee": "0",
            "total_fee": "1",
            "account_creation_fee": "1",
        })
    );
    assert_eq!(details.spent_by_me, dec("2"));

    let fixed = Chain {
        recipient_exists: false,
        resources: r#"{"NetLimit":266,"freeNetLimit":600}"#,
        ..Chain::new()
    }
    .start();
    let details = withdrawn(&fixed, false, request("TRX", Some("1")));
    raw_of(&details);
    assert_eq!(
        fee_json(&details),
        json!({
            "type": "Tron",
            "coin": "TRX",
            "bandwidth_used": 0,
            "energy_used": 0,
            "bandwidth_fee": "0.1",
            "energy_fee": "0",
            "total_fee": "1.1",
            "account_creation_fee": "1",
        })
    );
    assert_eq!(details.spent_by_me, dec("2.1"));
}

/// The bandwidth of an account-creating transfer is the size times the chain's rate: 267 x 2 = 534.
#[test]
fn the_bandwidth_rate_scales_the_bandwidth_of_an_account_creating_transfer() {
    const WITH_RATE: &str = r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420},{"key":"getCreateNewAccountFeeInSystemContract","value":1000000},{"key":"getCreateAccountFee","value":100000},{"key":"getCreateNewAccountBandwidthRate","value":2}]}"#;
    for (resources, used, bandwidth_fee) in [(r#"{"NetLimit":534}"#, 534, "0"), (r#"{"NetLimit":533}"#, 0, "0.1")] {
        let node = Chain {
            recipient_exists: false,
            resources,
            parameters: WITH_RATE,
            ..Chain::new()
        }
        .start();
        let fee = fee_json(&withdrawn(&node, false, request("TRX", Some("1"))));
        assert_eq!(fee["bandwidth_used"], used, "{resources}");
        assert_eq!(fee["bandwidth_fee"], bandwidth_fee, "{resources}");
        assert_eq!(fee["account_creation_fee"], "1");
    }
}

/// A node that reports no positive account-creation fee fails such a withdraw with `InternalError`; the same node
/// serves a transfer to an existing account.
#[test]
fn an_absent_recipient_without_a_positive_creation_fee_is_an_internal_error() {
    for parameters in [
        r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420},{"key":"getCreateAccountFee","value":100000}]}"#,
        r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420},{"key":"getCreateNewAccountFeeInSystemContract"},{"key":"getCreateAccountFee","value":100000}]}"#,
        r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420},{"key":"getCreateNewAccountFeeInSystemContract","value":-5},{"key":"getCreateAccountFee","value":100000}]}"#,
    ] {
        let absent = Chain {
            recipient_exists: false,
            parameters,
            ..Chain::new()
        }
        .start();
        let error = refused(&absent, false, request("TRX", Some("1")));
        assert!(matches!(error, WithdrawError::InternalError(_)), "{error:?}");

        let existing = Chain {
            parameters,
            ..Chain::new()
        }
        .start();
        assert_eq!(
            fee_json(&withdrawn(&existing, false, request("TRX", Some("1"))))["total_fee"],
            "0"
        );
    }
}

/// T21.10B.11: a TRC20 transfer to an absent address has no account-creation fee, and the recipient's account is
/// not read at all.
#[test]
fn a_trc20_transfer_to_an_absent_address_has_no_account_creation_fee() {
    let node = Chain {
        recipient_exists: false,
        ..Chain::new()
    }
    .start();
    let details = withdrawn(&node, true, request("USDT-TRC20", Some("1")));
    let fee = fee_json(&details);
    assert!(fee.get("account_creation_fee").is_none(), "{fee}");
    assert_eq!(fee["total_fee"], "5.88");
    let asked: Vec<Json> = node.bodies_for("/wallet/getaccount");
    assert!(!asked.is_empty());
    assert!(asked.iter().all(|body| body["address"] == sender_base58()), "{asked:?}");
}

// ---------------------------------------------------------------------------------------------------------------
// Energy and the fee limit
// ---------------------------------------------------------------------------------------------------------------

/// The dry run uses 14 000 energy and 4 000 staked energy is left: 10 000 x 420 SUN = 4.2 TRX. The fee limit counts
/// the whole energy: 14 000 x 420 = 5 880 000 SUN, plus a fifth, 7 056 000 SUN. The fee is paid in TRX, so only
/// the amount is spent of the token.
#[test]
fn a_trc20_transfer_pays_the_energy_shortfall_and_carries_the_fee_limit_with_its_margin() {
    let node = Chain {
        resources: r#"{"freeNetLimit":600,"EnergyLimit":10000,"EnergyUsed":6000}"#,
        ..Chain::new()
    }
    .start();
    let details = withdrawn(&node, true, request("USDT-TRC20", Some("1")));
    let raw = raw_of(&details);
    assert_eq!(raw.fee_limit, 7_056_000);
    assert_eq!(trc20_amount_of(&raw), 1_000_000);
    assert_eq!(
        fee_json(&details),
        json!({
            "type": "Tron",
            "coin": "TRX",
            "bandwidth_used": 345,
            "energy_used": 14000,
            "bandwidth_fee": "0",
            "energy_fee": "4.2",
            "total_fee": "4.2",
        })
    );
    assert_eq!(details.coin, "USDT-TRC20");
    assert_eq!(details.total_amount, dec("1"));
    assert_eq!(details.spent_by_me, dec("1"), "the fee is not spent of the token");
    assert_eq!(details.my_balance_change, dec("-1"));

    // The dry run is asked for the sender, with the transfer's own parameters.
    let dry_runs: Vec<Json> = node
        .bodies_for("/wallet/triggerconstantcontract")
        .into_iter()
        .filter(|body| body["function_selector"] == "transfer(address,uint256)")
        .collect();
    assert_eq!(dry_runs.len(), 1);
    assert_eq!(dry_runs[0]["owner_address"], sender_base58());
    assert_eq!(dry_runs[0]["contract_address"], TRC20_CONTRACT);
}

/// No staked energy and no bandwidth left: 14 000 x 420 = 5.88 TRX of energy and 345 x 1000 = 0.345 TRX of
/// bandwidth.
#[test]
fn a_trc20_transfer_without_resources_burns_for_energy_and_bandwidth() {
    let node = Chain {
        resources: "{}",
        ..Chain::new()
    }
    .start();
    let details = withdrawn(&node, true, request("USDT-TRC20", Some("1")));
    raw_of(&details);
    let fee = fee_json(&details);
    assert_eq!(fee["bandwidth_used"], 345);
    assert_eq!(fee["bandwidth_fee"], "0.345");
    assert_eq!(fee["energy_fee"], "5.88");
    assert_eq!(fee["total_fee"], "6.225");
}

/// D21.10B.1: 200 000 energy cost 84 TRX; with the margin that would be 100.8 TRX, so the fee limit is the
/// maximum of 100 TRX.
#[test]
fn the_fee_limit_is_held_at_the_maximum() {
    let node = Chain {
        trx_sun: 200_000_000,
        energy: Some(200_000),
        ..Chain::new()
    }
    .start();
    let details = withdrawn(&node, true, request("USDT-TRC20", Some("1")));
    assert_eq!(raw_of(&details).fee_limit, 100_000_000);
    assert_eq!(fee_json(&details)["total_fee"], "84");
}

/// D21.10B.1: 250 000 energy cost 105 TRX, above the maximum of 100 TRX. The withdraw is refused with both
/// figures, nothing is signed and no broadcast reaches the node.
#[test]
fn an_energy_cost_above_the_maximum_is_refused_before_broadcast() {
    let node = Chain {
        trx_sun: 500_000_000,
        energy: Some(250_000),
        ..Chain::new()
    }
    .start();
    match refused(&node, true, request("USDT-TRC20", Some("1"))) {
        WithdrawError::UnsupportedError(text) => {
            assert!(text.contains("105000000"), "the estimate is named: {text}");
            assert!(text.contains("100000000"), "the maximum is named: {text}");
        },
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(
        node.requests().iter().all(|(path, _)| !path.contains("broadcast")),
        "no broadcast request reaches the node"
    );
}

/// A dry run without a positive energy figure fails the withdraw: no figure stands in for it.
#[test]
fn a_dry_run_without_a_positive_energy_figure_is_a_transport_error() {
    for energy in [None, Some(0)] {
        let node = Chain { energy, ..Chain::new() }.start();
        let error = refused(&node, true, request("USDT-TRC20", Some("1")));
        assert!(matches!(error, WithdrawError::Transport(_)), "{energy:?}: {error:?}");
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Amounts
// ---------------------------------------------------------------------------------------------------------------

/// T21.10B.12: `max` sends the balance less the fee of the transaction that carries that amount. With nothing but
/// burned bandwidth, a balance of 268.5 TRX encodes in 5 bytes (269 bandwidth) and what is left of it in 4 (268):
/// the fee settles at 0.268 TRX and the amount at 268.232 TRX.
#[test]
fn trx_max_sends_the_balance_less_the_fee_of_the_transaction_that_carries_it() {
    let node = Chain {
        trx_sun: 268_500_000,
        resources: "{}",
        ..Chain::new()
    }
    .start();
    let details = withdrawn(&node, false, request("TRX", None));
    assert_eq!(trx_amount_of(&raw_of(&details)), 268_232_000);
    let fee = fee_json(&details);
    assert_eq!(fee["bandwidth_used"], 268);
    assert_eq!(fee["total_fee"], "0.268");
    assert_eq!(details.total_amount, dec("268.232"));
    assert_eq!(details.spent_by_me, dec("268.5"), "the whole balance is spent");
    assert_eq!(details.my_balance_change, dec("-268.5"));
}

/// `max` with free bandwidth sends the whole balance.
#[test]
fn trx_max_without_a_fee_sends_the_whole_balance() {
    let node = Chain::new().start();
    let details = withdrawn(&node, false, request("TRX", None));
    assert_eq!(trx_amount_of(&raw_of(&details)), 50_000_000);
    assert_eq!(details.spent_by_me, dec("50"));
}

/// T21.10B.12: a zero balance answers `ZeroBalanceToWithdrawMax`; a balance that does not exceed the fee answers
/// `AmountTooLow` with an amount of 0 and the fee as threshold.
#[test]
fn trx_max_of_a_balance_not_above_the_fee_is_refused() {
    let empty = Chain {
        trx_sun: 0,
        resources: "{}",
        ..Chain::new()
    }
    .start();
    let error = refused(&empty, false, request("TRX", None));
    assert!(matches!(error, WithdrawError::ZeroBalanceToWithdrawMax), "{error:?}");

    // 0.2 TRX encodes in 3 bytes: 267 bandwidth, a fee of 0.267 TRX. A balance of exactly the fee is refused too.
    for trx_sun in [200_000, 267_000] {
        let node = Chain {
            trx_sun,
            resources: "{}",
            ..Chain::new()
        }
        .start();
        match refused(&node, false, request("TRX", None)) {
            WithdrawError::AmountTooLow { amount, threshold } => {
                assert_eq!(amount, dec("0"));
                assert_eq!(threshold, dec("0.267"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

/// An amount plus its fee above the balance answers `NotSufficientBalance` with the sum as `required`; an amount
/// above the balance answers it with the amount.
#[test]
fn a_trx_amount_plus_fee_above_the_balance_is_refused() {
    let node = Chain {
        trx_sun: 1_200_000,
        resources: "{}",
        ..Chain::new()
    }
    .start();
    match refused(&node, false, request("TRX", Some("1"))) {
        WithdrawError::NotSufficientBalance {
            coin,
            available,
            required,
        } => {
            assert_eq!(coin, "TRX");
            assert_eq!(available, dec("1.2"));
            assert_eq!(required, dec("1.267"));
        },
        other => panic!("unexpected error: {other:?}"),
    }
    // 0.933 TRX plus 0.267 TRX is the whole balance: accepted.
    let details = withdrawn(&node, false, request("TRX", Some("0.933")));
    assert_eq!(details.spent_by_me, dec("1.2"));

    match refused(&node, false, request("TRX", Some("1.3"))) {
        WithdrawError::NotSufficientBalance { required, .. } => assert_eq!(required, dec("1.3")),
        other => panic!("unexpected error: {other:?}"),
    }
}

/// T21.10B.13: `max` for a token sends the whole token balance.
#[test]
fn trc20_max_sends_the_whole_token_balance() {
    let node = Chain::new().start();
    let details = withdrawn(&node, true, request("USDT-TRC20", None));
    assert_eq!(trc20_amount_of(&raw_of(&details)), 5_000_000);
    assert_eq!(details.total_amount, dec("5"));
    assert_eq!(details.spent_by_me, dec("5"));
    assert_eq!(fee_json(&details)["coin"], "TRX");
}

/// T21.10B.13: a TRC20 withdraw whose sender holds less TRX than the fee answers
/// `NotSufficientPlatformBalanceForFee` with the platform ticker, the TRX balance and the fee. The fee is 5.88 TRX.
#[test]
fn a_trc20_withdraw_without_the_trx_for_its_fee_is_refused() {
    let node = Chain {
        trx_sun: 5_879_999,
        ..Chain::new()
    }
    .start();
    for amount in [Some("1"), None] {
        match refused(&node, true, request("USDT-TRC20", amount)) {
            WithdrawError::NotSufficientPlatformBalanceForFee {
                coin,
                available,
                required,
            } => {
                assert_eq!(coin, "TRX");
                assert_eq!(available, dec("5.879999"));
                assert_eq!(required, dec("5.88"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }
    // Exactly the fee is enough.
    let exact = Chain {
        trx_sun: 5_880_000,
        ..Chain::new()
    }
    .start();
    withdrawn(&exact, true, request("USDT-TRC20", Some("1")));
}

// ---------------------------------------------------------------------------------------------------------------
// Other request members
// ---------------------------------------------------------------------------------------------------------------

/// A `memo` answers `UnsupportedError` before a node is asked.
#[test]
fn a_memo_is_refused_before_a_node_is_asked() {
    let node = Chain::new().start();
    for trc20 in [false, true] {
        let mut with_memo = request("TRX", Some("1"));
        with_memo.memo = Some("a note".to_owned());
        let error = refused(&node, trc20, with_memo);
        assert!(matches!(error, WithdrawError::UnsupportedError(_)), "{error:?}");
    }
    assert!(node.requests().is_empty(), "the refusal comes before any node request");
}

/// `expiration_seconds` sets the expiry that many seconds after the reference block's time, 60 by default.
#[test]
fn the_expiry_counts_from_the_reference_blocks_time() {
    let node = Chain::new().start();
    for trc20 in [false, true] {
        let default = withdrawn(&node, trc20, request("TRX", Some("1")));
        assert_eq!(raw_of(&default).expiration, BLOCK_TIME_MS + 60_000);

        let mut hour = request("TRX", Some("1"));
        hour.expiration_seconds = Some(3600);
        assert_eq!(
            raw_of(&withdrawn(&node, trc20, hour)).expiration,
            BLOCK_TIME_MS + 3_600_000
        );
    }
    // The member is read from the request JSON.
    let parsed: WithdrawRequest =
        serde_json::from_value(json!({ "coin": "TRX", "to": RECEIVER, "amount": "1", "expiration_seconds": 120 }))
            .expect("a withdraw request");
    assert_eq!(parsed.expiration_seconds, Some(120));
    assert_eq!(
        raw_of(&withdrawn(&node, false, parsed)).expiration,
        BLOCK_TIME_MS + 120_000
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Failed reads: no fallback figures
// ---------------------------------------------------------------------------------------------------------------

/// A failed `getaccountresource` or `getchainparameters` read fails the withdraw, for TRX and for a token.
#[test]
fn a_failed_resource_or_parameter_read_fails_the_withdraw() {
    for failing in ["/wallet/getaccountresource", "/wallet/getchainparameters"] {
        for trc20 in [false, true] {
            let node = Chain {
                failing: Some(failing),
                ..Chain::new()
            }
            .start();
            let error = refused(&node, trc20, request("TRX", Some("1")));
            assert!(
                matches!(error, WithdrawError::Transport(_)),
                "{failing} (trc20={trc20}): {error:?}"
            );
            assert_eq!(node.bodies_for(failing).len(), 1, "{failing} was asked");
        }
    }
}

/// A resource answer that does not decode, and parameters without a usable price, fail the withdraw too.
#[test]
fn an_unusable_resource_or_parameter_answer_fails_the_withdraw() {
    let bad_resources = Chain {
        resources: r#"{"NetLimit":"many"}"#,
        ..Chain::new()
    }
    .start();
    let no_energy_price = Chain {
        parameters: r#"{"chainParameter":[{"key":"getTransactionFee","value":1000}]}"#,
        ..Chain::new()
    }
    .start();
    for node in [&bad_resources, &no_energy_price] {
        for trc20 in [false, true] {
            let error = refused(node, trc20, request("TRX", Some("1")));
            assert!(matches!(error, WithdrawError::Transport(_)), "trc20={trc20}: {error:?}");
        }
    }
}

/// A failed read of the recipient's account fails a TRX withdraw: it is never taken for an existing or an absent
/// account.
#[test]
fn a_failed_recipient_read_fails_a_trx_withdraw() {
    let node = StubNode::start(|path, body| {
        let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
        match path {
            "/wallet/getaccount" if request["address"] == RECEIVER => Reply::status(500, "stub failure"),
            "/wallet/getaccount" => Reply::ok(r#"{"balance":50000000,"create_time":1700000000000}"#),
            "/wallet/getnowblock" => Reply::ok(NOW_BLOCK_BODY),
            "/wallet/getaccountresource" => Reply::ok("{}"),
            "/wallet/getchainparameters" => Reply::ok(PARAMETERS),
            other => Reply::status(404, &format!("unexpected endpoint {other}")),
        }
    });
    let error = refused(&node, false, request("TRX", Some("1")));
    assert!(matches!(error, WithdrawError::Transport(_)), "{error:?}");
}
