//! Tests of the sending side of a TRON V1 swap (CRD ch.21 §21.11B.2, D21.B5, D21.B6, D21.B9, §21.11C; ch.51
//! R93, R95, R98, R99), against a scripted node on the loopback interface: the real coin code and the real HTTP
//! layer run, only the node's answers are canned. The selectors are recomputed here from the dictated
//! signatures.

use super::fee_limit::SWAP_FEE_LIMIT_MARGIN_PERCENT;
use super::ids_tests::{digest, secret_hash, LOCK_TIME};
use super::send::{payment_sent_from, TronSendError, TRC20_PAYMENT_ENERGY_ESTIMATE, TRC20_REFUND_ENERGY_ESTIMATE,
                  TRC20_SPEND_ENERGY_ESTIMATE, TRX_PAYMENT_ENERGY_ESTIMATE, TRX_REFUND_ENERGY_ESTIMATE,
                  TRX_SPEND_ENERGY_ESTIMATE};
use super::tx::TronSwapTx;
use super::TronSwapError;
use crate::eth::addr_from_raw_pubkey;
use crate::eth::tron::activation::tron_coin_from_conf_and_request_with_switch;
use crate::eth::tron::api::TronApiClient;
use crate::eth::tron::nodes::TronNode;
use crate::eth::tron::proto::{ContractType, TransferContract};
use crate::eth::tron::test_node::{Reply, StubNode};
use crate::eth::tron::tx_builder::{build_trigger_smart_contract, tapos_from_block};
use crate::eth::tron::wallet_path_tests::{trc20_coin, trx_coin, NOW_BLOCK_BODY, TRC20_CONTRACT};
use crate::eth::tron::{Network, TronAddress};
use crate::eth::EthCoin;
use crate::{CoinProtocol, DexFee, DexFeeBurnDestination, FeeApproxStage, MarketCoinOps, MmCoin,
            NegotiateSwapContractAddrErr, SwapOps, TradeFee, TradePreimageError, TradePreimageValue, TransactionEnum,
            TransactionErr};
use bigdecimal::BigDecimal;
use common::block_on;
use ethereum_types::{Address, U256};
use futures::compat::Future01CompatExt;
use kdf_crypto::{keccak256, ripemd160};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_number::MmNumber;
use prost::Message as _;
use serde_json::{json, Value as Json};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

/// The address of the test key (the secret key 1), which signs for the test coin.
const SENDER_WIRE_HEX: &str = "417e5f4552091a69125d5dfcb7b8c2659029395bdf";
/// The compressed public key of the secret key 2: the other side of a test swap, and the test DEX-fee key.
const OTHER_PUB_HEX: &str = "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";
/// The number of the stub's head block.
const HEAD_BLOCK: u64 = 58_251_145;
/// 1000 SUN per bandwidth unit and 420 SUN per energy unit.
const PARAMETERS: &str =
    r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420}]}"#;
/// Bandwidth quotas that cover every transaction of a test, so that a fee is its energy part alone.
const FREE_BANDWIDTH: &str = r#"{"freeNetLimit":100000}"#;
/// The energy every successful dry run of the stub reports.
const ENERGY: u64 = 31_000;

fn selector(signature: &str) -> String { hex::encode(&keccak256(signature.as_bytes()).as_slice()[..4]) }

fn eth_payment_selector() -> String { selector("ethPayment(bytes32,address,bytes32,uint64)") }
fn erc20_payment_selector() -> String { selector("erc20Payment(bytes32,uint256,address,address,bytes32,uint64)") }
fn approve_selector() -> String { selector("approve(address,uint256)") }
fn allowance_selector() -> String { selector("allowance(address,address)") }
fn transfer_selector() -> String { selector("transfer(address,uint256)") }
fn payments_selector() -> String { selector("payments(bytes32)") }
fn rate_selector() -> String { selector("basisPointsRate()") }
fn maximum_selector() -> String { selector("maximumFee()") }

fn sender() -> TronAddress { TronAddress::from_hex(SENDER_WIRE_HEX).unwrap() }

fn other_pub() -> Vec<u8> { hex::decode(OTHER_PUB_HEX).unwrap() }

fn other_address() -> TronAddress { TronAddress::from_evm_address(addr_from_raw_pubkey(&other_pub()).unwrap()) }

/// The swap contract selected for the test swap. It is not the contract of any test coin's activation.
fn selected_contract() -> TronAddress { TronAddress::from_evm_address(Address::from([0x5c; 20])) }

fn selected_contract_bytes() -> Option<crate::BytesJson> {
    Some(selected_contract().to_evm_address().as_bytes().to_vec().into())
}

fn token() -> TronAddress { TronAddress::parse(TRC20_CONTRACT).unwrap() }

fn dec(text: &str) -> BigDecimal { BigDecimal::from_str(text).unwrap() }

/// A 32-byte ABI word of `address`.
fn address_word(address: &TronAddress) -> String { format!("{:0>64}", hex::encode(address.to_evm_address())) }

fn uint_word(value: u64) -> String { format!("{value:064x}") }

/// The swap id of the test swap: SHA-256 over the little-endian lock time and the 32-byte secret hash.
fn swap_id() -> [u8; 32] {
    let mut input = LOCK_TIME.to_le_bytes().to_vec();
    input.extend_from_slice(&secret_hash());
    digest(&input)
}

/// What the scripted node answers.
#[derive(Clone)]
struct Chain {
    /// The allowance the token reports. A broadcast approval sets it.
    allowance: Arc<Mutex<U256>>,
    /// The `state` of every `payments(id)` read.
    payment_state: u64,
    /// The `paymentHash` of every `payments(id)` read.
    stored_hash: [u8; 32],
    /// The selector (hex) of the call whose dry run reverts.
    reverting: Option<String>,
    /// The `energy_used` of every other dry run.
    energy: u64,
    /// The execution result in the receipt of every transaction.
    receipt: &'static str,
    resources: &'static str,
    /// The token balance of every address.
    token_units: u64,
    /// An endpoint that answers HTTP 500.
    failing: Option<&'static str>,
    /// The rows of the account's transaction listing.
    listing: Vec<Json>,
    /// How far the head block's time is from the local time of each request, in milliseconds.
    head_offset_ms: i64,
    /// What the token's `basisPointsRate()` returns; `None` for a token without the getter (the call reverts).
    fee_rate: Option<u64>,
    /// What the token's `maximumFee()` returns; `None` for a token without the getter.
    fee_maximum: Option<u64>,
}

impl Chain {
    fn new() -> Self {
        Chain {
            allowance: Arc::new(Mutex::new(U256::zero())),
            payment_state: 0,
            stored_hash: [0xab; 32],
            reverting: None,
            energy: ENERGY,
            receipt: "SUCCESS",
            resources: FREE_BANDWIDTH,
            token_units: 1_000_000_000,
            failing: None,
            listing: Vec::new(),
            head_offset_ms: 0,
            fee_rate: None,
            fee_maximum: None,
        }
    }

    fn with_allowance(self, units: u64) -> Self {
        *self.allowance.lock().unwrap() = U256::from(units);
        self
    }

    fn start(self) -> StubNode {
        StubNode::start(move |path, body| {
            if self.failing.is_some_and(|failing| path.starts_with(failing)) {
                return Reply::status(500, "stub failure");
            }
            let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
            match path {
                "/wallet/getnowblock" => {
                    let head_ms = common::now_ms() as i64 + self.head_offset_ms;
                    let stamp = format!("\"timestamp\":{head_ms}");
                    let head = NOW_BLOCK_BODY.replace("\"timestamp\":1700000000000", &stamp);
                    assert_ne!(head, NOW_BLOCK_BODY, "the body carries the timestamp that is replaced");
                    Reply::ok(&head)
                },
                "/wallet/getchainparameters" => Reply::ok(PARAMETERS),
                "/wallet/getaccountresource" => Reply::ok(self.resources),
                "/wallet/getaccount" => Reply::ok(r#"{"balance":50000000,"create_time":1700000000000}"#),
                "/wallet/broadcasthex" => {
                    let bytes = hex::decode(request["transaction"].as_str().unwrap_or_default()).unwrap_or_default();
                    let tx = TronSwapTx::from_bytes(&bytes).expect("a signed transaction");
                    if let Ok(trigger) = tx.trigger() {
                        if hex::encode(&trigger.data[..4]) == approve_selector() {
                            *self.allowance.lock().unwrap() = U256::from_big_endian(&trigger.data[36..68]);
                        }
                    }
                    Reply::ok(&json!({ "result": true, "txid": format!("{:x}", tx.txid()) }).to_string())
                },
                "/wallet/gettransactioninfobyid" => Reply::ok(
                    &json!({
                        "id": request["value"],
                        "blockNumber": HEAD_BLOCK,
                        "receipt": { "result": self.receipt },
                    })
                    .to_string(),
                ),
                "/wallet/triggerconstantcontract" => self.constant_call(&request),
                listing if listing.starts_with("/v1/accounts/") => {
                    Reply::ok(&json!({ "success": true, "data": self.listing, "meta": {} }).to_string())
                },
                other => Reply::status(404, &format!("unexpected endpoint {other}")),
            }
        })
    }

    fn constant_call(&self, request: &Json) -> Reply {
        let Some(data) = request["data"].as_str() else {
            // The parameter form: the token balance read.
            return Reply::ok(&json!({ "constant_result": [uint_word(self.token_units)] }).to_string());
        };
        let called = &data[..8];
        let reverted = json!({
            "result": { "result": true, "message": "REVERT opcode executed" },
            "energy_used": 500,
            "constant_result": [""],
        });
        let setting = |value: Option<u64>| match value {
            Some(value) => json!({ "result": { "result": true }, "constant_result": [uint_word(value)] }),
            None => reverted.clone(),
        };
        let answer = if called == rate_selector() {
            setting(self.fee_rate)
        } else if called == maximum_selector() {
            setting(self.fee_maximum)
        } else if called == allowance_selector() {
            let allowance = format!("{:064x}", *self.allowance.lock().unwrap());
            json!({ "result": { "result": true }, "constant_result": [allowance] })
        } else if called == payments_selector() {
            let lock_time = uint_word(u64::from(LOCK_TIME));
            let record = format!(
                "{}{}{}",
                hex::encode(self.stored_hash),
                lock_time,
                uint_word(self.payment_state)
            );
            json!({ "result": { "result": true }, "energy_used": 643, "constant_result": [record] })
        } else if self.reverting.as_deref() == Some(called) {
            json!({
                "result": { "result": true, "message": "REVERT opcode executed" },
                "energy_used": 500,
                "constant_result": [""],
            })
        } else {
            json!({
                "result": { "result": true },
                "energy_used": self.energy,
                "constant_result": [""],
                "transaction": { "ret": [{}] },
            })
        };
        Reply::ok(&answer.to_string())
    }
}

/// The transactions the node was asked to broadcast, in order.
fn broadcasts(node: &StubNode) -> Vec<TronSwapTx> {
    node.bodies_for("/wallet/broadcasthex")
        .iter()
        .map(|body| TronSwapTx::from_bytes(&hex::decode(body["transaction"].as_str().unwrap()).unwrap()).unwrap())
        .collect()
}

/// The selectors (hex) of every call the node was asked to dry-run or read in the call-data form, in order.
fn all_constant_calls(node: &StubNode) -> Vec<String> {
    node.bodies_for("/wallet/triggerconstantcontract")
        .iter()
        .filter_map(|body| body["data"].as_str().map(|data| data[..8].to_owned()))
        .collect()
}

/// [`all_constant_calls`] without the reads of the token's transfer-fee setting, which have their own tests.
fn constant_calls(node: &StubNode) -> Vec<String> {
    all_constant_calls(node)
        .into_iter()
        .filter(|called| *called != rate_selector() && *called != maximum_selector())
        .collect()
}

fn send_maker_payment(coin: &EthCoin, hash: &[u8], amount: &str) -> Result<TransactionEnum, TransactionErr> {
    block_on(
        coin.send_maker_payment(
            LOCK_TIME,
            &[],
            &other_pub(),
            hash,
            dec(amount),
            &selected_contract_bytes(),
        )
        .compat(),
    )
}

fn tron_tx(tx: TransactionEnum) -> TronSwapTx {
    match tx {
        TransactionEnum::TronTransaction(tx) => tx,
        other => panic!("not a TRON transaction: {:?}", other.tx_hash()),
    }
}

/// The `fee_limit` of a swap call whose dry run used [`ENERGY`]: its cost plus the swap margin.
fn swap_fee_limit() -> i64 {
    let cost = ENERGY * 420;
    (cost + cost * SWAP_FEE_LIMIT_MARGIN_PERCENT / 100) as i64
}

// ---------------------------------------------------------------------------------------------------------------
// Payments
// ---------------------------------------------------------------------------------------------------------------

/// §21.11B.2 `ethPayment(bytes32 id, address receiver, bytes32 secretHash, uint64 lockTime)`: the amount is the
/// call value, the contract is the one selected for the swap, and the secret hash is the 32 bytes given.
#[test]
fn a_trx_payment_is_an_eth_payment_call_with_the_amount_as_call_value() {
    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    assert_eq!(coin.swap_contract_address, Address::zero(), "not the coin's own field");

    let tx = tron_tx(send_maker_payment(&coin, &secret_hash(), "5.25").expect("a payment"));

    let sent = broadcasts(&node);
    assert_eq!(sent, vec![tx.clone()]);
    let trigger = tx.trigger().unwrap();
    assert_eq!(trigger.owner, sender());
    assert_eq!(trigger.contract, selected_contract());
    assert_eq!(trigger.call_value, 5_250_000);
    let expected = format!(
        "{}{}{}{}{}",
        eth_payment_selector(),
        hex::encode(swap_id()),
        address_word(&other_address()),
        hex::encode(secret_hash()),
        uint_word(u64::from(LOCK_TIME)),
    );
    assert_eq!(hex::encode(&trigger.data), expected);
    assert_eq!(tx.raw().fee_limit, swap_fee_limit());
    // The dry run was made for the same call, with the call value.
    let dry_runs = node.bodies_for("/wallet/triggerconstantcontract");
    assert_eq!(dry_runs.len(), 1);
    assert_eq!(dry_runs[0]["data"], json!(expected));
    assert_eq!(dry_runs[0]["call_value"], json!(5_250_000));
    assert_eq!(dry_runs[0]["contract_address"], json!(selected_contract().to_base58()));
}

/// The taker's payment is the same call, locked for the maker.
#[test]
fn a_taker_payment_is_locked_for_the_maker() {
    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let payment = coin.send_taker_payment(
        LOCK_TIME,
        &[],
        &other_pub(),
        &secret_hash(),
        dec("1"),
        &selected_contract_bytes(),
    );
    let tx = tron_tx(block_on(payment.compat()).expect("a payment"));
    let trigger = tx.trigger().unwrap();
    assert_eq!(trigger.call_value, 1_000_000);
    assert_eq!(hex::encode(&trigger.data[36..68]), address_word(&other_address()));
}

/// CRD ch.51 R72A, ch.21 R21.11C.1: the TRON payment never reaches the EVM family's 32-to-20 derivation. The
/// contract gets the 32 bytes, not their RIPEMD-160, and a 20-byte hash, which the EVM path takes, is refused
/// before any node is asked.
#[test]
fn the_tron_payment_never_narrows_the_secret_hash() {
    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let tx = tron_tx(send_maker_payment(&coin, &secret_hash(), "1").expect("a payment"));
    let data = tx.trigger().unwrap().data;
    let narrowed = ripemd160(&secret_hash());
    assert_eq!(data[68..100], secret_hash());
    assert!(
        !data.windows(20).any(|window| window == narrowed.as_slice()),
        "the narrowed value is nowhere in the call"
    );

    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let error = send_maker_payment(&coin, narrowed.as_slice(), "1").unwrap_err();
    assert!(error.get_tx().is_none());
    assert!(
        error.get_plain_text_format().contains("32-byte secret hash, got 20"),
        "{}",
        error.get_plain_text_format()
    );
    assert!(node.requests().is_empty(), "{:?}", node.requests());
}

/// §21.11B.2 `erc20Payment(bytes32 id, uint256 amount, address tokenAddress, address receiver, bytes32
/// secretHash, uint64 lockTime)`. An allowance that covers the amount is used as it is: no approval is sent.
#[test]
fn a_token_payment_with_a_sufficient_allowance_sends_no_approval() {
    let node = Chain::new().with_allowance(7_000_000).start();
    let (_ctx, coin) = block_on(trc20_coin(&node.url()));

    let tx = tron_tx(send_maker_payment(&coin, &secret_hash(), "7").expect("a payment"));

    assert_eq!(broadcasts(&node), vec![tx.clone()]);
    let trigger = tx.trigger().unwrap();
    assert_eq!(trigger.contract, selected_contract());
    assert_eq!(trigger.call_value, 0);
    let expected = format!(
        "{}{}{}{}{}{}{}",
        erc20_payment_selector(),
        hex::encode(swap_id()),
        uint_word(7_000_000),
        address_word(&token()),
        address_word(&other_address()),
        hex::encode(secret_hash()),
        uint_word(u64::from(LOCK_TIME)),
    );
    assert_eq!(hex::encode(&trigger.data), expected);
    assert_eq!(tx.raw().fee_limit, swap_fee_limit());
    // The allowance was asked of the token for the coin's address and the selected contract.
    let reads = node.bodies_for("/wallet/triggerconstantcontract");
    let read = reads
        .iter()
        .find(|body| {
            body["data"]
                .as_str()
                .is_some_and(|data| data.starts_with(&allowance_selector()))
        })
        .expect("the allowance was read");
    assert_eq!(read["contract_address"], json!(token().to_base58()));
    assert_eq!(
        read["data"],
        json!(format!(
            "{}{}{}",
            allowance_selector(),
            address_word(&sender()),
            address_word(&selected_contract())
        ))
    );
    assert_eq!(constant_calls(&node), vec![
        allowance_selector(),
        erc20_payment_selector()
    ]);
}

/// D21.B6: an allowance of zero is raised by one approval of exactly the amount, and the payment follows. The
/// returned transaction is the payment.
#[test]
fn a_token_payment_without_an_allowance_approves_the_exact_amount_first() {
    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trc20_coin(&node.url()));

    let tx = tron_tx(send_maker_payment(&coin, &secret_hash(), "7").expect("a payment"));

    let sent = broadcasts(&node);
    assert_eq!(sent.len(), 2);
    let approval = sent[0].trigger().unwrap();
    assert_eq!(approval.contract, token());
    assert_eq!(
        hex::encode(&approval.data),
        format!(
            "{}{}{}",
            approve_selector(),
            address_word(&selected_contract()),
            uint_word(7_000_000)
        )
    );
    assert_eq!(sent[1], tx);
    assert_eq!(hex::encode(&tx.trigger().unwrap().data[..4]), erc20_payment_selector());
    // The receipt of the approval was read before the payment was dry-run.
    let receipts = node.bodies_for("/wallet/gettransactioninfobyid");
    assert!(!receipts.is_empty());
    assert_eq!(receipts[0]["value"], json!(format!("{:x}", sent[0].txid())));
    assert_eq!(constant_calls(&node), vec![
        allowance_selector(),
        approve_selector(),
        erc20_payment_selector()
    ]);
}

/// D21.B6: an allowance that is not zero and below the amount is set to zero first, then to the amount.
#[test]
fn a_short_allowance_is_reset_to_zero_before_the_approval() {
    let node = Chain::new().with_allowance(6_999_999).start();
    let (_ctx, coin) = block_on(trc20_coin(&node.url()));

    let tx = tron_tx(send_maker_payment(&coin, &secret_hash(), "7").expect("a payment"));

    let sent = broadcasts(&node);
    assert_eq!(sent.len(), 3);
    let approved: Vec<String> = sent[..2]
        .iter()
        .map(|approval| {
            let trigger = approval.trigger().unwrap();
            assert_eq!(trigger.contract, token());
            assert_eq!(hex::encode(&trigger.data[..4]), approve_selector());
            hex::encode(&trigger.data[36..])
        })
        .collect();
    assert_eq!(approved, vec![uint_word(0), uint_word(7_000_000)]);
    assert_eq!(sent[2], tx);
}

/// An approval whose receipt does not report success stops the payment: nothing more is sent, and the error
/// carries no transaction, because the approval is not the payment.
#[test]
fn a_failed_approval_stops_before_the_payment() {
    let mut chain = Chain::new();
    chain.receipt = "REVERT";
    let node = chain.start();
    let (_ctx, coin) = block_on(trc20_coin(&node.url()));

    let error = send_maker_payment(&coin, &secret_hash(), "7").unwrap_err();

    assert!(error.get_tx().is_none());
    let text = error.get_plain_text_format();
    assert!(
        text.contains("the token approval failed") && text.contains("REVERT"),
        "{text}"
    );
    let sent = broadcasts(&node);
    assert_eq!(sent.len(), 1);
    assert_eq!(hex::encode(&sent[0].trigger().unwrap().data[..4]), approve_selector());
}

/// A payment whose dry run reverts is refused before anything is signed.
#[test]
fn a_reverting_dry_run_refuses_the_payment_before_signing() {
    let mut chain = Chain::new();
    chain.reverting = Some(eth_payment_selector());
    let node = chain.start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));

    let error = send_maker_payment(&coin, &secret_hash(), "5").unwrap_err();

    assert!(error.get_tx().is_none());
    assert!(
        error.get_plain_text_format().contains("REVERT opcode executed"),
        "{}",
        error.get_plain_text_format()
    );
    assert!(broadcasts(&node).is_empty());
    assert!(node.bodies_for("/wallet/getnowblock").is_empty(), "nothing was built");
}

/// D21.B5: a call whose energy alone costs more than the highest `fee_limit` is refused typed and never
/// broadcast, because a limit below the need would be charged and the call would still fail.
#[test]
fn an_energy_cost_above_the_cap_is_refused_without_a_broadcast() {
    let mut chain = Chain::new();
    // 300 000 x 420 = 126 000 000 SUN, above the 100 TRX maximum.
    chain.energy = 300_000;
    let node = chain.start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));

    let error = block_on(super::send::send_payment(
        &coin,
        &selected_contract(),
        &other_address(),
        &secret_hash(),
        LOCK_TIME,
        &dec("5"),
    ))
    .unwrap_err();

    assert!(
        matches!(error, TronSendError::Swap(TronSwapError::FeeLimit(_))),
        "{error}"
    );
    assert!(broadcasts(&node).is_empty());
}

/// A broadcast that the node refuses returns the signed payment with the error.
#[test]
fn a_refused_broadcast_keeps_the_signed_payment() {
    let mut chain = Chain::new();
    chain.failing = Some("/wallet/broadcasthex");
    let node = chain.start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));

    let error = send_maker_payment(&coin, &secret_hash(), "5").unwrap_err();

    let kept = tron_tx(error.get_tx().expect("the signed payment"));
    assert!(matches!(error, TransactionErr::TxRecoverable(..)));
    assert_eq!(kept.trigger().unwrap().call_value, 5_000_000);
}

/// R21.11C.7: a head block whose time is more than 30 seconds from the local time refuses the payment and the
/// DEX fee, of TRX and of a token. Nothing is broadcast, and the error is a plain one that keeps no transaction.
#[test]
fn a_stale_head_refuses_the_send_without_a_broadcast() {
    for head_offset_ms in [-45_000, 45_000] {
        let mut chain = Chain::new().with_allowance(5_000_000);
        chain.head_offset_ms = head_offset_ms;
        let node = chain.start();
        for coin in [block_on(trx_coin(&node.url())).1, block_on(trc20_coin(&node.url())).1] {
            for error in [
                send_maker_payment(&coin, &secret_hash(), "5").unwrap_err(),
                send_taker_fee(&coin, &standard_fee("0.5")).unwrap_err(),
            ] {
                assert!(matches!(error, TransactionErr::Plain(_)), "{head_offset_ms}");
                assert!(error.get_tx().is_none());
                assert!(
                    error.get_plain_text_format().contains("too far from the local time"),
                    "{}",
                    error.get_plain_text_format()
                );
            }
        }
        assert!(broadcasts(&node).is_empty(), "{head_offset_ms}");
    }
}

/// R21.11C.7 for the TRX DEX-fee transfer: the head time is judged node by node. With one node stale and one in
/// tolerance the transfer is sent on the fresh head; with every node stale it is the head-time error and nothing
/// is broadcast.
#[test]
fn the_trx_dex_fee_transfer_judges_the_head_time_node_by_node() {
    let two_nodes = |offsets: [i64; 2]| {
        let nodes = offsets.map(|head_offset_ms| {
            let mut chain = Chain::new();
            chain.head_offset_ms = head_offset_ms;
            chain.start()
        });
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let mut key = [0u8; 32];
        key[31] = 1;
        let request = json!({ "urls": [nodes[0].url(), nodes[1].url()] });
        let protocol = CoinProtocol::TRX { network: Network::Nile };
        let coin = block_on(crate::eth::tron::tron_coin_from_conf_and_request(
            &ctx,
            "TRX",
            &json!({}),
            &request,
            &key,
            protocol,
        ))
        .expect("a TRX coin activates against stub nodes");
        let result = send_taker_fee(&coin, &standard_fee("0.5"));
        let sent: usize = nodes.iter().map(|node| broadcasts(node).len()).sum();
        (result, sent)
    };

    let (result, sent) = two_nodes([-45_000, 0]);
    let tx = tron_tx(result.expect("a fee transaction"));
    assert_eq!(sent, 1);
    let local_ms = common::now_ms() as i64;
    assert!(
        (tx.raw().expiration - 60_000 - local_ms).abs() < 10_000,
        "built on the fresh head"
    );

    let (result, sent) = two_nodes([-45_000, 45_000]);
    let error = result.unwrap_err();
    assert!(matches!(error, TransactionErr::Plain(_)));
    assert!(
        error.get_plain_text_format().contains("the local clock may be wrong"),
        "{}",
        error.get_plain_text_format()
    );
    assert_eq!(sent, 0);
}

/// A TRX amount beyond the protocol's signed 64-bit field is a typed error before any node is asked.
#[test]
fn an_amount_beyond_the_field_is_refused() {
    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let error = send_maker_payment(&coin, &secret_hash(), "9223372036854.775808").unwrap_err();
    assert!(
        error.get_plain_text_format().contains("unusable amount"),
        "{}",
        error.get_plain_text_format()
    );
    assert!(node.requests().is_empty());
}

// ---------------------------------------------------------------------------------------------------------------
// Token transfer fee (maintainer decision of 2026-11-22)
// ---------------------------------------------------------------------------------------------------------------

/// The sender and the receiver figure of a token coin bound to a swap contract, on `chain`.
fn token_start_figures(chain: Chain) -> (StubNode, [Result<TradeFee, TradePreimageError>; 2]) {
    let node = chain.with_allowance(5_000_000).start();
    let (_ctx, coin) = bound_coin(&node.url(), true);
    let stage = FeeApproxStage::StartSwap;
    let sender = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(dec("5")), stage.clone()));
    let receiver = block_on(coin.get_receiver_trade_fee(stage).compat());
    (node, [
        sender.map_err(|e| e.into_inner()),
        receiver.map_err(|e| e.into_inner()),
    ])
}

/// A token whose transfer fee is zero is swapped: its rate is zero, or its maximum is zero, or it has no such
/// setting at all (the getter reverts). The setting is read from the token with two constant calls, and the
/// maximum only when the rate is not zero.
#[test]
fn a_token_without_a_transfer_fee_is_allowed() {
    for (fee_rate, fee_maximum, reads) in [
        (Some(0), Some(7), vec![rate_selector()]),
        (Some(10), Some(0), vec![rate_selector(), maximum_selector()]),
        (None, None, vec![rate_selector()]),
    ] {
        let mut chain = Chain::new().with_allowance(7_000_000);
        chain.fee_rate = fee_rate;
        chain.fee_maximum = fee_maximum;
        let node = chain.clone().start();
        let (_ctx, coin) = block_on(trc20_coin(&node.url()));
        tron_tx(send_maker_payment(&coin, &secret_hash(), "7").expect("a payment"));
        // The setting is read first, from the token, as plain constant calls.
        let calls = node.bodies_for("/wallet/triggerconstantcontract");
        assert_eq!(all_constant_calls(&node)[..reads.len()], reads[..], "{fee_rate:?}");
        assert_eq!(calls[0]["contract_address"], json!(token().to_base58()));
        assert_eq!(calls[0]["data"], json!(rate_selector()));
        assert_eq!(calls[0]["call_value"], json!(0));

        let (_node, figures) = token_start_figures(chain);
        for figure in figures {
            assert!(figure.is_ok(), "{fee_rate:?} {fee_maximum:?}: {figure:?}");
        }
    }
}

/// A token with a rate and a maximum that are both not zero is refused: when a swap starts, by both fee figures,
/// and before the payment, with no approval sent. A rate without a readable maximum is refused the same way.
#[test]
fn a_token_with_a_transfer_fee_is_refused_at_the_start_and_before_the_payment() {
    for fee_maximum in [Some(5_000_000), None] {
        let mut chain = Chain::new();
        chain.fee_rate = Some(10);
        chain.fee_maximum = fee_maximum;

        let node = chain.clone().start();
        let (_ctx, coin) = block_on(trc20_coin(&node.url()));
        let error = send_maker_payment(&coin, &secret_hash(), "7").unwrap_err();
        assert!(matches!(error, TransactionErr::Plain(_)));
        let text = error.get_plain_text_format();
        assert!(
            text.contains("the token takes a fee on a transfer (rate 10 basis points"),
            "{text}"
        );
        assert!(broadcasts(&node).is_empty(), "no approval and no payment");
        assert_eq!(all_constant_calls(&node), vec![rate_selector(), maximum_selector()]);

        let (node, figures) = token_start_figures(chain);
        for figure in figures {
            let error = figure.unwrap_err();
            assert!(
                matches!(&error, TradePreimageError::InternalError(text) if text.contains("takes a fee on a transfer")),
                "{error}"
            );
        }
        assert!(constant_calls(&node).is_empty(), "nothing else was asked");
    }
}

/// A failed read of the setting, or an answer that is not one word, is a typed error and never "no fee".
#[test]
fn a_failed_transfer_fee_read_is_an_error() {
    let mut failing = Chain::new().with_allowance(7_000_000);
    failing.failing = Some("/wallet/triggerconstantcontract");
    let node = failing.clone().start();
    let (_ctx, coin) = block_on(trc20_coin(&node.url()));
    let error = send_maker_payment(&coin, &secret_hash(), "7").unwrap_err();
    assert!(error.get_plain_text_format().contains("TRON node request failed"));
    assert!(broadcasts(&node).is_empty());
    let (_node, figures) = token_start_figures(failing);
    for figure in figures {
        let error = figure.unwrap_err();
        assert!(matches!(error, TradePreimageError::Transport(_)), "{error}");
    }

    // Two bytes where a 32-byte word belongs.
    let node = StubNode::start(|path, _| match path {
        "/wallet/triggerconstantcontract" => Reply::ok(r#"{"result":{"result":true},"constant_result":["0a0b"]}"#),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    });
    let (_ctx, coin) = block_on(trc20_coin(&node.url()));
    let error = block_on(super::send::ensure_no_token_transfer_fee(&coin, &token())).unwrap_err();
    assert!(matches!(error, TronSendError::Swap(TronSwapError::Node(_))), "{error}");
}

/// A TRX leg has no token: its payment and its fee figures make no read of a transfer-fee setting.
#[test]
fn a_trx_leg_reads_no_transfer_fee_setting() {
    let node = Chain::new().start();
    let (_ctx, coin) = bound_coin(&node.url(), false);
    descriptors(&coin, "5");
    tron_tx(send_maker_payment(&coin, &secret_hash(), "5").expect("a payment"));
    let calls = all_constant_calls(&node);
    assert!(!calls.is_empty());
    assert!(
        !calls.contains(&rate_selector()) && !calls.contains(&maximum_selector()),
        "{calls:?}"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// DEX fee
// ---------------------------------------------------------------------------------------------------------------

fn standard_fee(amount: &str) -> DexFee { DexFee::Standard(MmNumber::from(dec(amount))) }

fn send_taker_fee(coin: &EthCoin, dex_fee: &DexFee) -> Result<TransactionEnum, TransactionErr> {
    block_on(coin.send_taker_fee(dex_fee, &other_pub(), &[]).compat())
}

/// D21.B9: the DEX fee of TRX is a `TransferContract` of the fee amount to the fee address.
#[test]
fn a_trx_dex_fee_is_a_transfer_to_the_fee_address() {
    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));

    let tx = tron_tx(send_taker_fee(&coin, &standard_fee("0.5")).expect("a fee transaction"));

    assert_eq!(broadcasts(&node), vec![tx.clone()]);
    let [contract] = tx.raw().contract.as_slice() else {
        panic!("one contract")
    };
    assert_eq!(contract.r#type, ContractType::TransferContract as i32);
    let transfer = TransferContract::decode(contract.parameter.as_ref().unwrap().value.as_slice()).unwrap();
    assert_eq!(transfer.owner_address, sender().to_bytes().to_vec());
    assert_eq!(
        TronAddress::from_bytes(&transfer.to_address).unwrap().to_base58(),
        other_address().to_base58()
    );
    assert_eq!(transfer.amount, 500_000);
    assert_eq!(tx.raw().fee_limit, 0);
    assert!(node.bodies_for("/wallet/triggerconstantcontract").is_empty());
}

/// D21.B9: the DEX fee of a token is a `transfer(address,uint256)` call of the token, with the `fee_limit` of a
/// swap call.
#[test]
fn a_token_dex_fee_is_a_transfer_call_of_the_token() {
    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trc20_coin(&node.url()));

    let tx = tron_tx(send_taker_fee(&coin, &standard_fee("0.5")).expect("a fee transaction"));

    assert_eq!(broadcasts(&node), vec![tx.clone()]);
    let trigger = tx.trigger().unwrap();
    assert_eq!(trigger.contract, token());
    assert_eq!(trigger.call_value, 0);
    assert_eq!(
        hex::encode(&trigger.data),
        format!(
            "{}{}{}",
            transfer_selector(),
            address_word(&other_address()),
            uint_word(500_000)
        )
    );
    assert_eq!(tx.raw().fee_limit, swap_fee_limit());
}

/// A swap that owes no DEX fee sends nothing, and the burn form is a typed error: neither asks a node.
#[test]
fn no_fee_and_the_burn_form_send_nothing() {
    let node = Chain::new().start();
    for coin in [block_on(trx_coin(&node.url())).1, block_on(trc20_coin(&node.url())).1] {
        let burn = DexFee::WithBurn {
            fee_amount: MmNumber::from(dec("0.5")),
            burn_amount: MmNumber::from(dec("0.1")),
            burn_destination: DexFeeBurnDestination::PreBurnAccount {
                burn_pubkey: other_pub(),
            },
        };
        for (dex_fee, reason) in [
            (DexFee::NoFee, "the swap owes no DEX fee"),
            (burn, "a TRON coin pays no burn share"),
        ] {
            let error = send_taker_fee(&coin, &dex_fee).unwrap_err();
            assert!(error.get_tx().is_none());
            assert!(
                error.get_plain_text_format().contains(reason),
                "{}",
                error.get_plain_text_format()
            );
        }
    }
    assert!(node.requests().is_empty(), "{:?}", node.requests());
}

// ---------------------------------------------------------------------------------------------------------------
// Own payment
// ---------------------------------------------------------------------------------------------------------------

fn my_payment_sent(coin: &EthCoin, hash: &[u8]) -> Result<Option<TransactionEnum>, String> {
    block_on(
        coin.check_if_my_payment_sent(LOCK_TIME, &[], &[], hash, 0, &selected_contract_bytes())
            .compat(),
    )
}

/// A signed payment call of the test swap, as the chain would hold it.
fn listed_payment(contract: &TronAddress, id: [u8; 32]) -> TronSwapTx { listed_payment_from(&sender(), contract, id) }

/// [`listed_payment`] made by the caller `owner`.
fn listed_payment_from(owner: &TronAddress, contract: &TronAddress, id: [u8; 32]) -> TronSwapTx {
    let data = super::encode_call(&super::TronSwapCall::EthPayment {
        id,
        receiver: other_address(),
        secret_hash: secret_hash(),
        lock_time: u64::from(LOCK_TIME),
    })
    .unwrap();
    let tapos = tapos_from_block(HEAD_BLOCK, &[7; 32]);
    let raw = build_trigger_smart_contract(owner, contract, data, 1_000_000, &tapos, 1, 2, 3);
    TronSwapTx::new(raw, vec![9; 65])
}

/// A row of the account's transaction listing for `tx` with the execution result `result`.
fn listing_row(tx: &TronSwapTx, result: &str) -> Json {
    json!({
        "txID": format!("{:x}", tx.txid()),
        "ret": [{ "contractRet": result, "fee": 0 }],
        "signature": [hex::encode(tx.signature())],
        "raw_data_hex": hex::encode(tx.raw().encode_to_vec()),
    })
}

/// The client of a node that serves the account listing: the stub counts as a keyed TronGrid node.
fn keyed_client(node: &StubNode) -> TronApiClient {
    TronApiClient::new_with_history_nodes("TRX", vec![TronNode::plain(node.url())], &[true], 5.)
}

/// State 0: the contract holds no payment under the id, so none was sent. The read is one plain call of
/// `payments(id)` on the selected contract.
#[test]
fn an_uninitialized_payment_was_not_sent() {
    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));

    assert_eq!(my_payment_sent(&coin, &secret_hash()).map(|tx| tx.is_none()), Ok(true));

    let reads = node.bodies_for("/wallet/triggerconstantcontract");
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0]["contract_address"], json!(selected_contract().to_base58()));
    assert_eq!(
        reads[0]["data"],
        json!(format!("{}{}", payments_selector(), hex::encode(swap_id())))
    );
}

/// States 1 to 3 mean the payment was sent: its transaction is returned when the listing has it. Rows of other
/// swaps, of other contracts and of failed calls are no candidates.
#[test]
fn a_sent_spent_or_refunded_payment_is_found_in_the_listing() {
    let payment = listed_payment(&selected_contract(), swap_id());
    let rows = vec![
        json!({ "txID": "00", "note": "a row of another kind" }),
        listing_row(&listed_payment(&selected_contract(), [0x33; 32]), "SUCCESS"),
        listing_row(&listed_payment(&token(), swap_id()), "SUCCESS"),
        listing_row(&listed_payment(&selected_contract(), swap_id()), "REVERT"),
        // The right contract, method and id from another caller is not our payment.
        listing_row(
            &listed_payment_from(&other_address(), &selected_contract(), swap_id()),
            "SUCCESS",
        ),
        listing_row(&payment, "SUCCESS"),
    ];
    // Without our own row the other caller's payment is not taken for ours.
    let mut foreign = Chain::new();
    foreign.payment_state = 1;
    foreign.listing = rows[..rows.len() - 1].to_vec();
    let node = foreign.start();
    let missing = block_on(payment_sent_from(
        &keyed_client(&node),
        &sender(),
        &selected_contract(),
        &swap_id(),
        None,
    ))
    .unwrap_err();
    assert!(matches!(missing, TronSendError::Undetermined(_)), "{missing}");

    for state in [1, 2, 3] {
        let mut chain = Chain::new();
        chain.payment_state = state;
        chain.listing = rows.clone();
        let node = chain.start();
        let found = block_on(payment_sent_from(
            &keyed_client(&node),
            &sender(),
            &selected_contract(),
            &swap_id(),
            None,
        ))
        .unwrap_or_else(|e| panic!("state {state}: {e}"));
        assert_eq!(found, Some(payment.clone()), "state {state}");
    }
}

/// States 1 to 3 without the transaction: the answer is the typed "undetermined" error, never "not sent". That
/// is so when the listing does not have the payment, and when no node of the coin serves a listing.
#[test]
fn a_sent_payment_that_cannot_be_found_is_undetermined() {
    for state in [1, 2, 3] {
        let mut chain = Chain::new();
        chain.payment_state = state;
        let node = chain.start();
        let missing = block_on(payment_sent_from(
            &keyed_client(&node),
            &sender(),
            &selected_contract(),
            &swap_id(),
            None,
        ))
        .unwrap_err();
        assert!(
            matches!(missing, TronSendError::Undetermined(_)),
            "state {state}: {missing}"
        );

        let (_ctx, coin) = block_on(trx_coin(&node.url()));
        let error = my_payment_sent(&coin, &secret_hash()).unwrap_err();
        assert!(
            error.contains("Undetermined whether the payment was sent"),
            "state {state}: {error}"
        );
    }
}

/// With the amount and the other side's key known, a stored payment hash that is not the one of this swap's terms
/// is the typed "another payment" error at once: never "not sent", never undetermined, and nothing is looked up.
/// An equal hash goes on to the lookup.
#[test]
fn an_id_that_holds_another_payment_is_a_typed_error() {
    let for_amount = |coin: &EthCoin, amount: &str| {
        let contract = selected_contract_bytes();
        let hash = secret_hash();
        block_on(
            coin.check_if_my_payment_sent_for_amount(LOCK_TIME, &[], &other_pub(), &hash, 0, &contract, &dec(amount))
                .compat(),
        )
    };
    for is_token in [false, true] {
        let token_address = token();
        let asset = is_token.then_some(&token_address);
        let ours = super::tron_payment_hash(
            &other_address(),
            &sender(),
            &secret_hash(),
            asset,
            U256::from(5_000_000u64),
        );
        let coin_of = |node: &StubNode| {
            if is_token {
                block_on(trc20_coin(&node.url())).1
            } else {
                block_on(trx_coin(&node.url())).1
            }
        };
        for state in [1, 2, 3] {
            // Another payment under the id: the hash of another amount is not ours.
            let mut chain = Chain::new();
            chain.payment_state = state;
            chain.stored_hash = ours;
            let node = chain.start();
            let error = for_amount(&coin_of(&node), "5.000001").unwrap_err();
            assert!(error.contains("the swap id holds another payment"), "{error}");
            assert!(!error.contains("Undetermined"), "{error}");
            assert_eq!(
                node.requests().len(),
                1,
                "one read and no lookup: {:?}",
                node.requests()
            );

            // Our terms: the lookup goes on as without the amount (this coin has no node that lists).
            let error = for_amount(&coin_of(&node), "5").unwrap_err();
            assert!(error.contains("Undetermined whether the payment was sent"), "{error}");

            // With a node that lists, the equal hash finds the payment.
            let payment = listed_payment(&selected_contract(), swap_id());
            let mut chain = Chain::new();
            chain.payment_state = state;
            chain.stored_hash = ours;
            chain.listing = vec![listing_row(&payment, "SUCCESS")];
            let node = chain.start();
            let lookup = |hash: &[u8; 32]| {
                block_on(payment_sent_from(
                    &keyed_client(&node),
                    &sender(),
                    &selected_contract(),
                    &swap_id(),
                    Some(hash),
                ))
            };
            assert_eq!(lookup(&ours).unwrap(), Some(payment));
            assert!(matches!(lookup(&[0x01; 32]), Err(TronSendError::OtherPayment)));
        }
        // State 0 is "not sent" whatever hash the empty record shows.
        let node = Chain::new().start();
        assert_eq!(for_amount(&coin_of(&node), "5").map(|tx| tx.is_none()), Ok(true));
    }
}

/// A read that fails, a state outside 0 to 3 and a secret hash of another width are errors, never "not sent".
#[test]
fn a_failed_payment_read_is_an_error() {
    let mut failing = Chain::new();
    failing.failing = Some("/wallet/triggerconstantcontract");
    let node = failing.start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let error = my_payment_sent(&coin, &secret_hash()).unwrap_err();
    assert!(error.contains("TRON node request failed"), "{error}");

    let mut unknown = Chain::new();
    unknown.payment_state = 4;
    let node = unknown.start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let error = my_payment_sent(&coin, &secret_hash()).unwrap_err();
    assert!(error.contains("unknown payment state 4"), "{error}");

    let node = Chain::new().start();
    let (_ctx, coin) = block_on(trx_coin(&node.url()));
    let error = my_payment_sent(&coin, &[0x11; 20]).unwrap_err();
    assert!(error.contains("32-byte secret hash, got 20"), "{error}");
    assert!(node.requests().is_empty());
}

// ---------------------------------------------------------------------------------------------------------------
// Fee figures
// ---------------------------------------------------------------------------------------------------------------

const BOUND_CONTRACT: &str = "TTTzsLDg81qoUkiEbeHsKbXZ4rWi1uFpo3";
const FALLBACK_CONTRACT: &str = "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL";

fn bound_contract() -> TronAddress { TronAddress::parse(BOUND_CONTRACT).unwrap() }

/// A coin that can trade on the first supported network, activated with the swap switch on and bound to
/// [`BOUND_CONTRACT`].
fn bound_coin(node_url: &str, is_token: bool) -> (MmArc, EthCoin) {
    bound_coin_on(mm2_net_config::SUPPORTED_NETIDS[0], node_url, is_token)
}

/// [`bound_coin`] on the network `netid`.
fn bound_coin_on(netid: u16, node_url: &str, is_token: bool) -> (MmArc, EthCoin) {
    let ctx = MmCtxBuilder::new().with_conf(json!({ "netid": netid })).into_mm_arc();
    let req = json!({
        "urls": [node_url],
        "swap_contract_address": BOUND_CONTRACT,
        "fallback_swap_contract": FALLBACK_CONTRACT,
    });
    let (ticker, conf, protocol) = if is_token {
        ("USDT-TRC20", json!({ "decimals": 6 }), CoinProtocol::TRC20 {
            platform: "TRX".to_owned(),
            contract_address: TRC20_CONTRACT.to_owned(),
        })
    } else {
        ("TRX", json!({}), CoinProtocol::TRX { network: Network::Nile })
    };
    let mut key = [0u8; 32];
    key[31] = 1;
    let coin = block_on(tron_coin_from_conf_and_request_with_switch(
        &ctx, ticker, &conf, &req, &key, protocol, true,
    ))
    .expect("a trading coin activates against a stub node");
    (ctx, coin)
}

/// The four fee descriptors of the coin: the payment, the spend, the DEX-fee send and the legacy figure.
fn descriptors(coin: &EthCoin, volume: &str) -> [TradeFee; 4] {
    let stage = FeeApproxStage::TradePreimage;
    [
        block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(dec(volume)), stage.clone())).expect("sender"),
        block_on(coin.get_receiver_trade_fee(stage.clone()).compat()).expect("receiver"),
        block_on(coin.get_fee_to_send_taker_fee(dec("0.5"), stage)).expect("the DEX-fee send"),
        block_on(coin.get_trade_fee().compat()).expect("the legacy figure"),
    ]
}

/// `energy` at 420 SUN, as an amount of TRX.
fn energy_fee(energy: u64) -> MmNumber { MmNumber::from(BigDecimal::new((energy * 420).into(), 6)) }

/// CRD ch.51 R93, T39 at the coin: every fee figure of a TRX coin and of a token names TRX and is not paid from
/// the traded volume. With bandwidth covered, the figures are the energy of the step at the energy price: the
/// dry run for the payment and for the token's DEX-fee transfer, the fixed estimates for the spend and for the
/// legacy figure, and nothing for a TRX transfer. The payment figure includes the fixed estimate of the refund.
#[test]
fn every_fee_descriptor_names_trx_and_is_not_paid_from_the_volume() {
    for (is_token, expected) in [
        (false, [
            ENERGY + TRX_REFUND_ENERGY_ESTIMATE,
            TRX_SPEND_ENERGY_ESTIMATE,
            0,
            TRX_PAYMENT_ENERGY_ESTIMATE,
        ]),
        (true, [
            ENERGY + TRC20_REFUND_ENERGY_ESTIMATE,
            TRC20_SPEND_ENERGY_ESTIMATE,
            ENERGY,
            TRC20_PAYMENT_ENERGY_ESTIMATE,
        ]),
    ] {
        // The figures are the same on every supported network: only the DEX-fee address differs.
        for netid in mm2_net_config::SUPPORTED_NETIDS.iter().skip(1) {
            let node = Chain::new().with_allowance(5_000_000).start();
            let (_ctx, coin) = bound_coin_on(*netid, &node.url(), is_token);
            for (fee, energy) in descriptors(&coin, "5").iter().zip(expected) {
                assert_eq!(fee.coin, "TRX", "token: {is_token}, netid {netid}");
                assert!(!fee.paid_from_trading_vol);
                assert_eq!(fee.amount, energy_fee(energy), "token: {is_token}, netid {netid}");
            }
        }
        let node = Chain::new().with_allowance(5_000_000).start();
        let (_ctx, coin) = bound_coin(&node.url(), is_token);
        let fees = descriptors(&coin, "5");
        for (fee, energy) in fees.iter().zip(expected) {
            assert_eq!(fee.coin, "TRX", "token: {is_token}");
            assert!(!fee.paid_from_trading_vol);
            assert_eq!(fee.amount, energy_fee(energy), "token: {is_token}");
        }
        // The estimates are made against the contract of the coin's activation.
        let dry_runs = node.bodies_for("/wallet/triggerconstantcontract");
        let payment_selector = if is_token {
            erc20_payment_selector()
        } else {
            eth_payment_selector()
        };
        let payment = dry_runs
            .iter()
            .find(|body| {
                body["data"]
                    .as_str()
                    .is_some_and(|data| data.starts_with(&payment_selector))
            })
            .expect("the payment was dry-run");
        assert_eq!(payment["contract_address"], json!(bound_contract().to_base58()));
        assert!(broadcasts(&node).is_empty());
    }
}

/// Without a quota that covers it, the bandwidth of the transaction is added at the bandwidth price.
#[test]
fn bandwidth_is_added_when_no_quota_covers_it() {
    let mut chain = Chain::new();
    chain.resources = "{}";
    let node = chain.start();
    let (_ctx, coin) = bound_coin(&node.url(), false);
    let [sender_fee, _, dex_fee_send, _] = descriptors(&coin, "5");
    let energy_part = energy_fee(ENERGY + TRX_REFUND_ENERGY_ESTIMATE).to_decimal();
    let bandwidth_part = sender_fee.amount.to_decimal() - energy_part;
    // The payment and the refund are a few hundred bytes each: between 0.4 and 2 TRX at 1000 SUN per byte.
    assert!(
        bandwidth_part > dec("0.4") && bandwidth_part < dec("2"),
        "{bandwidth_part}"
    );
    assert!(dex_fee_send.amount.to_decimal() > dec("0.1"));
}

/// The mirror of CRD ch.57 R57.5.3b: the approval is part of the payment fee only when the allowance is below
/// the volume, with the approval of zero when the allowance is not zero. The payment itself cannot be simulated
/// then and is priced with its fixed estimate. The refund is part of the figure in every case.
#[test]
fn the_approval_is_part_of_the_payment_fee_only_when_the_allowance_is_short() {
    let sender_fee = |allowance: u64| {
        let node = Chain::new().with_allowance(allowance).start();
        let (_ctx, coin) = bound_coin(&node.url(), true);
        let stage = FeeApproxStage::TradePreimage;
        let fee = block_on(coin.get_sender_trade_fee(TradePreimageValue::UpperBound(dec("5")), stage)).expect("a fee");
        (fee.amount, constant_calls(&node))
    };

    let (enough, calls) = sender_fee(5_000_000);
    let refund = TRC20_REFUND_ENERGY_ESTIMATE;
    assert_eq!(enough, energy_fee(ENERGY + refund));
    assert_eq!(calls, vec![allowance_selector(), erc20_payment_selector()]);

    let (none, calls) = sender_fee(0);
    assert_eq!(none, energy_fee(ENERGY + TRC20_PAYMENT_ENERGY_ESTIMATE + refund));
    assert_eq!(calls, vec![allowance_selector(), approve_selector()]);

    let (short, calls) = sender_fee(4_999_999);
    assert_eq!(short, energy_fee(2 * ENERGY + TRC20_PAYMENT_ENERGY_ESTIMATE + refund));
    assert_eq!(calls, vec![
        allowance_selector(),
        approve_selector(),
        approve_selector()
    ]);
}

/// A payment whose dry run the contract refuses is priced with the fixed estimate, so that the balance checks
/// of the caller can report the shortage.
#[test]
fn a_refused_payment_dry_run_is_priced_with_the_fixed_estimate() {
    let mut chain = Chain::new();
    chain.reverting = Some(eth_payment_selector());
    let node = chain.start();
    let (_ctx, coin) = bound_coin(&node.url(), false);
    let stage = FeeApproxStage::TradePreimage;
    let fee = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(dec("5")), stage)).expect("a fee");
    assert_eq!(
        fee.amount,
        energy_fee(TRX_PAYMENT_ENERGY_ESTIMATE + TRX_REFUND_ENERGY_ESTIMATE)
    );
}

/// CRD ch.51 R95, T40 at the coin: the figures of a token name TRX, not the token, so the caller's check of
/// that coin answers `NotSufficientBaseCoinBalance` for a short TRX balance. A token balance below the DEX fee
/// is reported for the token itself.
#[test]
fn a_token_fee_is_owed_in_trx_and_a_short_token_balance_names_the_token() {
    let node = Chain::new().with_allowance(5_000_000).start();
    let (_ctx, coin) = bound_coin(&node.url(), true);
    for fee in descriptors(&coin, "5") {
        assert_ne!(fee.coin, coin.ticker());
        assert_eq!(fee.coin, coin.platform_ticker());
        assert!(fee.amount.to_decimal() > dec("0"));
    }

    let mut chain = Chain::new();
    chain.reverting = Some(transfer_selector());
    chain.token_units = 499_999;
    let node = chain.start();
    let (_ctx, coin) = bound_coin(&node.url(), true);
    let error = block_on(coin.get_fee_to_send_taker_fee(dec("0.5"), FeeApproxStage::TradePreimage))
        .unwrap_err()
        .into_inner();
    match error {
        TradePreimageError::NotSufficientBalance {
            coin,
            available,
            required,
        } => {
            assert_eq!(coin, "USDT-TRC20");
            assert_eq!(available, dec("0.499999"));
            assert_eq!(required, dec("0.5"));
        },
        other => panic!("unexpected error: {other}"),
    }
}

/// CRD ch.51 R95 item 5: a node that refuses an estimate, or does not answer a read the estimate needs, gives
/// `Transport`.
#[test]
fn an_estimate_the_node_refuses_is_a_transport_error() {
    let stage = FeeApproxStage::TradePreimage;
    for failing in [
        "/wallet/triggerconstantcontract",
        "/wallet/getchainparameters",
        "/wallet/getaccountresource",
    ] {
        let mut chain = Chain::new().with_allowance(5_000_000);
        chain.failing = Some(failing);
        let node = chain.start();
        for is_token in [false, true] {
            let (_ctx, coin) = bound_coin(&node.url(), is_token);
            let sender = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(dec("5")), stage.clone()))
                .unwrap_err()
                .into_inner();
            assert!(
                matches!(sender, TradePreimageError::Transport(_)),
                "{failing}: {sender}"
            );
        }
    }

    // The dry run of the token's DEX-fee transfer reverts although the balance covers the fee, and the dry run
    // of an approval reverts.
    let mut chain = Chain::new();
    chain.reverting = Some(transfer_selector());
    let node = chain.start();
    let (_ctx, coin) = bound_coin(&node.url(), true);
    let error = block_on(coin.get_fee_to_send_taker_fee(dec("0.5"), stage.clone()))
        .unwrap_err()
        .into_inner();
    assert!(matches!(error, TradePreimageError::Transport(_)), "{error}");

    let mut chain = Chain::new();
    chain.reverting = Some(approve_selector());
    let node = chain.start();
    let (_ctx, coin) = bound_coin(&node.url(), true);
    let error = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(dec("5")), stage))
        .unwrap_err()
        .into_inner();
    assert!(matches!(error, TradePreimageError::Transport(_)), "{error}");
}

/// A swap that owes no DEX fee has a zero figure for sending it, named by the platform coin as the EVM family
/// names it, and asks no node.
#[test]
fn the_fee_to_send_no_dex_fee_is_zero() {
    let node = Chain::new().start();
    for is_token in [false, true] {
        let (_ctx, coin) = bound_coin(&node.url(), is_token);
        let fee = block_on(coin.get_fee_to_send_taker_fee(dec("0"), FeeApproxStage::StartSwap)).expect("a figure");
        assert_eq!(fee, TradeFee {
            coin: "TRX".to_owned(),
            amount: MmNumber::from(0),
            paid_from_trading_vol: false,
        });
    }
    assert!(node.requests().is_empty());
}

/// A coin without a swap contract cannot trade: its payment and spend figures are refused before any node is
/// asked.
#[test]
fn a_coin_without_a_swap_contract_has_no_swap_fee_figure() {
    let node = Chain::new().start();
    for coin in [block_on(trx_coin(&node.url())).1, block_on(trc20_coin(&node.url())).1] {
        let stage = FeeApproxStage::TradePreimage;
        let sender = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(dec("1")), stage.clone()))
            .unwrap_err()
            .into_inner();
        let receiver = block_on(coin.get_receiver_trade_fee(stage).compat())
            .unwrap_err()
            .into_inner();
        for error in [sender, receiver] {
            assert!(
                matches!(&error, TradePreimageError::InternalError(text) if text.contains("no swap contract")),
                "{error}"
            );
        }
        let legacy = block_on(coin.get_trade_fee().compat()).unwrap_err();
        assert!(legacy.contains("no swap contract"), "{legacy}");
    }
    assert!(node.requests().is_empty());
}

// ---------------------------------------------------------------------------------------------------------------
// Negotiated contract (CRD ch.51 R98, R99)
// ---------------------------------------------------------------------------------------------------------------

/// The contract a TRON coin declares for the negotiation is the 20-byte payload, never the 21-byte wire form,
/// and the generic accept-or-fallback rule selects by it: the primary and the fallback are accepted in the
/// 20-byte form, the 21-byte form and any other address are not, and no member selects the fallback.
#[test]
fn the_negotiated_contract_is_the_20_byte_payload() {
    let node = Chain::new().start();
    for is_token in [false, true] {
        let (_ctx, coin) = bound_coin(&node.url(), is_token);
        let primary = bound_contract();
        let fallback = TronAddress::parse(FALLBACK_CONTRACT).unwrap();
        let payload = |address: &TronAddress| address.to_evm_address().as_bytes().to_vec();

        let declared = MmCoin::swap_contract_address(&coin).expect("a declared contract").0;
        assert_eq!(declared.len(), 20);
        assert_eq!(declared, payload(&primary));
        assert_ne!(declared, primary.to_bytes().to_vec());

        let select = |declared: Option<&[u8]>| {
            coin.negotiate_swap_contract_addr(declared)
                .map(|selected| selected.map(|bytes| bytes.0))
                .map_err(|e| e.into_inner())
        };
        assert_eq!(select(Some(&payload(&primary))), Ok(Some(payload(&primary))));
        assert_eq!(select(Some(&payload(&fallback))), Ok(Some(payload(&fallback))));
        assert_eq!(select(None), Ok(Some(payload(&fallback))));
        assert!(matches!(
            select(Some(&primary.to_bytes())),
            Err(NegotiateSwapContractAddrErr::InvalidOtherAddrLen(_))
        ));
        assert!(matches!(
            select(Some(&[])),
            Err(NegotiateSwapContractAddrErr::InvalidOtherAddrLen(_))
        ));
        assert!(matches!(
            select(Some(&[0x77; 20])),
            Err(NegotiateSwapContractAddrErr::UnexpectedOtherAddr(_))
        ));
    }
    assert!(node.requests().is_empty());
}
