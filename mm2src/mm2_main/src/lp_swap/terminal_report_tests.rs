//! CRD ch.51 R50A tests T26, T27 and T28: the terminal swap-status report is made for every legacy swap end
//! (privacy coins included), its redaction touches exactly the specified fields, and a receiver stores only the
//! author's own persistent key.
//!
//! The tests run the real load/redact, store and publish steps of [`broadcast_my_swap_status`] (the price
//! fetch of the live path is replaced by a canned price-service response) and read the published payload from
//! the P2P command channel.

use super::*;
use crate::mm2::database::init_and_migrate_db;
use crate::mm2::lp_network::P2PContext;
use crate::mm2::lp_swap::maker_swap::{MakerSavedEvent, MakerSwapData, MakerSwapEvent, TakerNegotiationData};
use crate::mm2::lp_swap::taker_swap::{MakerNegotiationData, TakerSavedEvent, TakerSwapData, TakerSwapEvent};
use common::block_on;
use crypto::privkey::SerializableSecp256k1Keypair;
use db_common::sqlite::rusqlite::Connection;
use futures::channel::mpsc;
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_p2p::atomicdex_behaviour::AdexBehaviourCmd;
use rpc::v1::types::{Bytes as BytesJson, H256 as H256Json, H264 as H264Json};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn maker_persistent_key() -> String { format!("02{}", "11".repeat(32)) }
fn taker_persistent_key() -> String { format!("03{}", "22".repeat(32)) }
fn maker_wire_key() -> String { "11".repeat(32) }
fn taker_wire_key() -> String { "22".repeat(32) }
fn zero_32() -> String { "00".repeat(32) }
fn zero_33() -> String { "00".repeat(33) }
const PRICE_RESPONSE: &str = r#"{"ARRR":{"last_price":"1.5"},"RIN":{"last_price":"0.02"},"KMD":{"last_price":"0.25"}}"#;

fn key33(hex: &str) -> H264Json { hex.parse().unwrap() }

fn key32(hex: &str) -> H256Json { hex.parse().unwrap() }

#[derive(Clone, Copy, Debug)]
enum Role {
    Maker,
    Taker,
}

#[derive(Clone, Copy, Debug)]
enum Outcome {
    Success,
    NegotiationRefusal,
    PaymentStageFailure,
}

const ROLES: [Role; 2] = [Role::Maker, Role::Taker];
const OUTCOMES: [Outcome; 3] = [
    Outcome::Success,
    Outcome::NegotiationRefusal,
    Outcome::PaymentStageFailure,
];

fn tx_id() -> TransactionIdentifier { json::from_value(json!({"tx_hex": "0a0b0c", "tx_hash": "0d0e0f"})).unwrap() }

fn swap_error() -> SwapError { SwapError::from("test error") }

fn maker_started(uuid: Uuid, maker_coin: &str, taker_coin: &str, ephemeral: bool) -> MakerSwapData {
    MakerSwapData {
        taker_coin: taker_coin.into(),
        maker_coin: maker_coin.into(),
        taker: key32(&taker_wire_key()),
        secret: key32(&"ab".repeat(32)),
        secret_hash: Some(BytesJson::from(vec![0x5e; 20])),
        my_persistent_pub: key33(&maker_persistent_key()),
        lock_duration: 7800,
        maker_amount: "1.25".parse().unwrap(),
        taker_amount: "2.5".parse().unwrap(),
        maker_payment_confirmations: 1,
        maker_payment_requires_nota: Some(false),
        taker_payment_confirmations: 2,
        taker_payment_requires_nota: Some(true),
        maker_payment_lock: 1_700_000_000,
        uuid,
        started_at: 1_699_990_000,
        maker_coin_start_block: 11,
        taker_coin_start_block: 12,
        maker_payment_trade_fee: Some(SavedTradeFee::default()),
        taker_payment_spend_trade_fee: Some(SavedTradeFee::default()),
        maker_coin_swap_contract_address: Some(BytesJson::from(vec![0x01; 20])),
        taker_coin_swap_contract_address: Some(BytesJson::from(vec![0x02; 20])),
        maker_coin_htlc_privkey: Some(SerializableSecp256k1Keypair::new([7; 32]).unwrap()),
        maker_coin_htlc_pubkey: Some(key33(&maker_persistent_key())),
        taker_coin_htlc_privkey: Some(SerializableSecp256k1Keypair::new([8; 32]).unwrap()),
        taker_coin_htlc_pubkey: Some(key33(&taker_persistent_key())),
        p2p_privkey: ephemeral.then(|| SerializableSecp256k1Keypair::new([9; 32]).unwrap()),
    }
}

fn taker_started(uuid: Uuid, maker_coin: &str, taker_coin: &str, ephemeral: bool) -> TakerSwapData {
    TakerSwapData {
        taker_coin: taker_coin.into(),
        maker_coin: maker_coin.into(),
        maker: key32(&maker_wire_key()),
        my_persistent_pub: key33(&taker_persistent_key()),
        lock_duration: 7800,
        maker_amount: "1.25".parse().unwrap(),
        taker_amount: "2.5".parse().unwrap(),
        maker_payment_confirmations: 1,
        maker_payment_requires_nota: Some(false),
        taker_payment_confirmations: 2,
        taker_payment_requires_nota: Some(true),
        taker_payment_lock: 1_700_000_000,
        uuid,
        started_at: 1_699_990_000,
        maker_payment_wait: 1_699_995_000,
        maker_coin_start_block: 11,
        taker_coin_start_block: 12,
        fee_to_send_taker_fee: Some(SavedTradeFee::default()),
        taker_payment_trade_fee: Some(SavedTradeFee::default()),
        maker_payment_spend_trade_fee: Some(SavedTradeFee::default()),
        maker_coin_swap_contract_address: Some(BytesJson::from(vec![0x01; 20])),
        taker_coin_swap_contract_address: Some(BytesJson::from(vec![0x02; 20])),
        maker_coin_htlc_privkey: Some(SerializableSecp256k1Keypair::new([7; 32]).unwrap()),
        maker_coin_htlc_pubkey: Some(key33(&maker_persistent_key())),
        taker_coin_htlc_privkey: Some(SerializableSecp256k1Keypair::new([8; 32]).unwrap()),
        taker_coin_htlc_pubkey: Some(key33(&taker_persistent_key())),
        p2p_privkey: ephemeral.then(|| SerializableSecp256k1Keypair::new([9; 32]).unwrap()),
    }
}

fn maker_negotiated() -> TakerNegotiationData {
    TakerNegotiationData {
        taker_payment_locktime: 1_699_999_000,
        taker_pubkey: key33(&taker_persistent_key()),
        maker_coin_swap_contract_addr: Some(BytesJson::from(vec![0x03; 20])),
        taker_coin_swap_contract_addr: Some(BytesJson::from(vec![0x04; 20])),
        maker_coin_htlc_pubkey: Some(key33(&taker_persistent_key())),
        taker_coin_htlc_pubkey: Some(key33(&taker_persistent_key())),
    }
}

fn taker_negotiated() -> MakerNegotiationData {
    MakerNegotiationData {
        maker_payment_locktime: 1_699_999_000,
        maker_pubkey: key33(&maker_persistent_key()),
        secret_hash: BytesJson::from(vec![0x5e; 20]),
        maker_coin_swap_contract_addr: Some(BytesJson::from(vec![0x03; 20])),
        taker_coin_swap_contract_addr: Some(BytesJson::from(vec![0x04; 20])),
        maker_coin_htlc_pubkey: Some(key33(&maker_persistent_key())),
        taker_coin_htlc_pubkey: Some(key33(&maker_persistent_key())),
    }
}

fn maker_events(started: MakerSwapData, outcome: Outcome) -> Vec<MakerSavedEvent> {
    use MakerSwapEvent::*;
    let tail = match outcome {
        Outcome::Success => vec![
            Negotiated(maker_negotiated()),
            TakerFeeValidated(tx_id()),
            MakerPaymentSent(tx_id()),
            TakerPaymentReceived(tx_id()),
            TakerPaymentWaitConfirmStarted,
            TakerPaymentValidatedAndConfirmed,
            TakerPaymentSpent(tx_id()),
            TakerPaymentSpendConfirmStarted,
            TakerPaymentSpendConfirmed,
            Finished,
        ],
        Outcome::NegotiationRefusal => vec![NegotiateFailed(swap_error()), Finished],
        Outcome::PaymentStageFailure => vec![
            Negotiated(maker_negotiated()),
            TakerFeeValidated(tx_id()),
            MakerPaymentSent(tx_id()),
            TakerPaymentValidateFailed(swap_error()),
            Finished,
        ],
    };
    std::iter::once(Started(started))
        .chain(tail)
        .enumerate()
        .map(|(i, event)| MakerSavedEvent {
            timestamp: 1_699_990_000_000 + i as u64 * 1000,
            event,
        })
        .collect()
}

fn taker_events(started: TakerSwapData, outcome: Outcome) -> Vec<TakerSavedEvent> {
    use TakerSwapEvent::*;
    let tail = match outcome {
        Outcome::Success => vec![
            Negotiated(taker_negotiated()),
            TakerFeeSent(tx_id()),
            MakerPaymentReceived(tx_id()),
            MakerPaymentWaitConfirmStarted,
            MakerPaymentValidatedAndConfirmed,
            TakerPaymentSent(tx_id()),
            MakerPaymentSpent(tx_id()),
            MakerPaymentSpendConfirmed,
            Finished,
        ],
        Outcome::NegotiationRefusal => vec![NegotiateFailed(swap_error()), Finished],
        Outcome::PaymentStageFailure => vec![
            Negotiated(taker_negotiated()),
            TakerFeeSent(tx_id()),
            MakerPaymentReceived(tx_id()),
            MakerPaymentValidateFailed(swap_error()),
            Finished,
        ],
    };
    std::iter::once(Started(started))
        .chain(tail)
        .enumerate()
        .map(|(i, event)| TakerSavedEvent {
            timestamp: 1_699_990_000_000 + i as u64 * 1000,
            event,
        })
        .collect()
}

/// A finished saved swap of `role` for the coin pair, with every start-event field set to a non-default value.
/// `ephemeral` is whether the swap signed its peer-to-peer traffic with a per-swap key.
fn record(uuid: Uuid, role: Role, maker_coin: &str, taker_coin: &str, outcome: Outcome, ephemeral: bool) -> SavedSwap {
    let success_events = vec!["Started".to_string(), "Finished".to_string()];
    let error_events = vec!["StartFailed".to_string(), "NegotiateFailed".to_string()];
    match role {
        Role::Maker => SavedSwap::Maker(MakerSavedSwap {
            uuid,
            my_order_uuid: Some(uuid),
            events: maker_events(maker_started(uuid, maker_coin, taker_coin, ephemeral), outcome),
            maker_amount: Some("1.25".parse().unwrap()),
            maker_coin: Some(maker_coin.into()),
            taker_amount: Some("2.5".parse().unwrap()),
            taker_coin: Some(taker_coin.into()),
            gui: Some("test-gui".into()),
            mm_version: Some("test-version".into()),
            success_events,
            error_events,
        }),
        Role::Taker => SavedSwap::Taker(TakerSavedSwap {
            uuid,
            my_order_uuid: Some(uuid),
            events: taker_events(taker_started(uuid, maker_coin, taker_coin, ephemeral), outcome),
            maker_amount: Some("1.25".parse().unwrap()),
            maker_coin: Some(maker_coin.into()),
            taker_amount: Some("2.5".parse().unwrap()),
            taker_coin: Some(taker_coin.into()),
            gui: Some("test-gui".into()),
            mm_version: Some("test-version".into()),
            success_events,
            error_events,
        }),
    }
}

/// A node: an in-memory SQLite store, a database directory, and a P2P command channel the test reads from.
struct TestNode {
    ctx: MmArc,
    rx: mpsc::Receiver<AdexBehaviourCmd>,
    dir: std::path::PathBuf,
}

impl TestNode {
    fn new() -> TestNode {
        let dir = std::env::temp_dir().join(format!("kdf-terminal-report-{}", Uuid::new_v4()));
        let ctx = MmCtxBuilder::default()
            .with_conf(json!({"netid": 8762, "dbdir": dir.to_str().unwrap()}))
            .into_mm_arc();
        let _ = ctx
            .sqlite_connection
            .pin(Arc::new(Mutex::new(Connection::open_in_memory().unwrap())));
        block_on(init_and_migrate_db(&ctx)).unwrap();
        let (tx, rx) = mpsc::channel(64);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        for path in [
            my_swaps_dir(&ctx),
            crate::mm2::lp_swap::maker_swap::stats_maker_swap_dir(&ctx),
            crate::mm2::lp_swap::taker_swap::stats_taker_swap_dir(&ctx),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        TestNode { ctx, rx, dir }
    }

    /// The node's own persisted record, as the run loop leaves it before the terminal report.
    fn store_own_record(&self, swap: &SavedSwap) { block_on(swap.save_to_db(&self.ctx)).unwrap(); }

    /// Makes the terminal report exactly as [`broadcast_my_swap_status`] does (load and redact, store natively,
    /// publish); the live path's price fetch is replaced by the canned price-service response.
    fn report(&self, uuid: Uuid) {
        block_on(async {
            let status = load_redacted_my_swap(&self.ctx, uuid).await.unwrap();
            let fiat = fiat_snapshot_from_price_response(&status, PRICE_RESPONSE);
            save_stats_swap_with_snapshot(&self.ctx, &status, fiat.as_ref())
                .await
                .unwrap();
            publish_swap_status(&self.ctx, status);
        });
    }

    /// Everything published since the last call, as `(topics, payload)`.
    fn published(&mut self) -> Vec<(Vec<String>, Vec<u8>)> {
        let mut published = vec![];
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                AdexBehaviourCmd::PublishMsg { topics, msg } => published.push((topics, msg)),
                _ => panic!("only publications are expected on the command channel"),
            }
        }
        published
    }

    fn stats_rows(&self) -> i64 {
        self.ctx
            .sqlite_connection()
            .query_row("SELECT COUNT(*) FROM stats_swaps", [], |row| row.get(0))
            .unwrap()
    }

    /// `(maker_pubkey, taker_pubkey, maker_coin_usd_price, taker_coin_usd_price)` of the row for `uuid`.
    #[allow(clippy::type_complexity)]
    fn stats_row(&self, uuid: Uuid) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
        self.ctx
            .sqlite_connection()
            .query_row(
                "SELECT maker_pubkey, taker_pubkey, CAST(maker_coin_usd_price AS TEXT), \
                 CAST(taker_coin_usd_price AS TEXT) FROM stats_swaps WHERE uuid = ?1",
                [uuid.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
    }

    /// A payload of another node arrives: decode it as the status object (CRD ch.51 R49) and store it, with the
    /// prices of the canned price-service response.
    fn receive(&self, payload: &[u8]) {
        let status: SwapStatus = json::from_slice(payload).unwrap();
        let fiat = fiat_snapshot_from_price_response(&status.data, PRICE_RESPONSE);
        block_on(save_stats_swap_with_snapshot(&self.ctx, &status.data, fiat.as_ref())).unwrap();
    }
}

impl Drop for TestNode {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); }
}

fn only_publication(published: Vec<(Vec<String>, Vec<u8>)>, uuid: Uuid) -> Vec<u8> {
    assert_eq!(published.len(), 1, "exactly one report per termination");
    let (topics, payload) = published.into_iter().next().unwrap();
    assert_eq!(topics, vec![swap_topic(&uuid)]);
    payload
}

// ---------------------------------------------------------------------------------------------------------
// T26
// ---------------------------------------------------------------------------------------------------------

/// T26: the report is made whichever side holds a privacy coin, for every outcome and both roles.
#[test]
fn t26_report_is_published_for_privacy_and_transparent_swaps_alike() {
    // (maker coin, taker coin): privacy coin on the maker side, on the taker side, and transparent only.
    let pairs = [("ARRR", "RIN"), ("RIN", "ARRR"), ("RIN", "KMD")];
    for (maker_coin, taker_coin) in pairs {
        let privacy_swap = maker_coin == "ARRR" || taker_coin == "ARRR";
        for role in ROLES {
            for outcome in OUTCOMES {
                let mut node = TestNode::new();
                let uuid = Uuid::new_v4();
                // The role's own coin being the privacy coin means the swap signed with a per-swap key.
                let own_coin_is_privacy = match role {
                    Role::Maker => maker_coin == "ARRR",
                    Role::Taker => taker_coin == "ARRR",
                };
                let own = record(uuid, role, maker_coin, taker_coin, outcome, own_coin_is_privacy);
                node.store_own_record(&own);

                node.report(uuid);

                let ctx_line = format!(
                    "{:?} {:?} {}/{} privacy={}",
                    role, outcome, maker_coin, taker_coin, privacy_swap
                );
                let payload = only_publication(node.published(), uuid);
                let published: Value = json::from_slice(&payload).unwrap();
                let object = published.as_object().unwrap();
                assert_eq!(object.len(), 2, "{}: exactly two members", ctx_line);
                assert_eq!(object["method"], "swapstatus", "{}", ctx_line);
                assert_eq!(object["data"]["uuid"], uuid.to_string(), "{}", ctx_line);
                assert_eq!(
                    object["data"]["type"],
                    match role {
                        Role::Maker => "Maker",
                        Role::Taker => "Taker",
                    },
                    "{}",
                    ctx_line
                );
                // The swap is marked finished before the report is made.
                let last_event = object["data"]["events"].as_array().unwrap().last().unwrap().clone();
                assert_eq!(last_event["event"]["type"], "Finished", "{}", ctx_line);
                // Native: the redacted record was stored in the node's own statistics store.
                assert_eq!(node.stats_rows(), 1, "{}", ctx_line);
            }
        }
    }
}

/// The body of the function `name` in `source`: from its signature to the first closing brace in column 0.
fn function_body<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source.find(name).unwrap();
    let len = source[start..].find("\n}\n").unwrap();
    &source[start..start + len]
}

/// T26 (structure guard): the terminal arm of both legacy run loops makes the report unconditionally. A
/// condition on the coins (the former privacy-coin suppression) must not come back. The run loops need a full
/// swap harness to execute, so this guards their source.
#[test]
fn t26_run_loops_report_without_a_coin_condition() {
    for (name, source, run_fn) in [
        (
            "maker_swap.rs",
            include_str!("maker_swap.rs"),
            "pub async fn run_maker_swap",
        ),
        (
            "taker_swap.rs",
            include_str!("taker_swap.rs"),
            "pub async fn run_taker_swap",
        ),
    ] {
        let body = function_body(source, run_fn);
        assert!(
            !body.contains("is_privacy"),
            "{} must not branch on a privacy coin",
            name
        );
        assert!(!body.contains("to_broadcast"), "{} must not gate the report", name);
        assert_eq!(
            body.matches("broadcast_my_swap_status(&ctx,").count(),
            1,
            "{} makes the report in exactly one place",
            name
        );
        // From the terminal arm to the report, the only construct is the logging of a failure.
        let call = body.find("broadcast_my_swap_status(&ctx,").unwrap();
        let arm = body[..call].rfind("None => {").unwrap();
        let between = body[arm..call].replace("if let Err(e) = ", "");
        assert!(
            !between.contains("if ") && !between.contains("match "),
            "{}: nothing may make the report conditional",
            name
        );
    }
}

// ---------------------------------------------------------------------------------------------------------
// T27
// ---------------------------------------------------------------------------------------------------------

fn start_event_data(swap: &mut Value) -> &mut serde_json::Map<String, Value> {
    swap["events"][0]["event"]["data"].as_object_mut().unwrap()
}

/// T27: the redaction touches exactly the specified fields of the start event, nothing else.
#[test]
fn t27_redaction_set_is_exact() {
    for role in ROLES {
        for ephemeral in [true, false] {
            let uuid = Uuid::new_v4();
            let stored = record(uuid, role, "RIN", "KMD", Outcome::Success, ephemeral);
            let stored_json = json::to_value(&stored).unwrap();

            let mut redacted = record(uuid, role, "RIN", "KMD", Outcome::Success, ephemeral);
            redacted.hide_secrets();
            let redacted_json = json::to_value(&redacted).unwrap();

            // The expected record: the stored one with exactly the specified fields changed.
            let mut expected = stored_json.clone();
            {
                let data = start_event_data(&mut expected);
                match role {
                    Role::Maker => {
                        data.insert("secret".into(), json!(zero_32()));
                        data.insert("taker".into(), json!(zero_32()));
                    },
                    Role::Taker => {
                        data.insert("maker".into(), json!(zero_32()));
                    },
                }
                if ephemeral {
                    data.insert("my_persistent_pub".into(), json!(zero_33()));
                }
                data.insert("maker_coin_htlc_privkey".into(), Value::Null);
                data.insert("taker_coin_htlc_privkey".into(), Value::Null);
                data.insert("p2p_privkey".into(), Value::Null);
            }
            assert_eq!(redacted_json, expected, "{:?} ephemeral={}", role, ephemeral);

            // The listed fields, one by one, in the published record.
            let published_start = redacted_json["events"][0]["event"]["data"].clone();
            let stored_start = stored_json["events"][0]["event"]["data"].clone();
            assert_eq!(published_start["maker_coin_htlc_privkey"], Value::Null);
            assert_eq!(published_start["taker_coin_htlc_privkey"], Value::Null);
            assert_eq!(published_start["p2p_privkey"], Value::Null);
            assert_ne!(
                stored_start["maker_coin_htlc_privkey"],
                Value::Null,
                "fixture sets a value"
            );
            assert_ne!(
                stored_start["taker_coin_htlc_privkey"],
                Value::Null,
                "fixture sets a value"
            );
            match role {
                Role::Maker => {
                    assert_eq!(published_start["secret"], zero_32());
                    assert_eq!(published_start["taker"], zero_32());
                    assert_ne!(stored_start["secret"], zero_32(), "fixture sets a value");
                    assert_ne!(stored_start["taker"], zero_32(), "fixture sets a value");
                },
                Role::Taker => {
                    assert_eq!(published_start["maker"], zero_32());
                    assert_ne!(stored_start["maker"], zero_32(), "fixture sets a value");
                },
            }
            if ephemeral {
                assert_eq!(published_start["my_persistent_pub"], zero_33());
            } else {
                assert_eq!(published_start["my_persistent_pub"], stored_start["my_persistent_pub"]);
                assert_ne!(published_start["my_persistent_pub"], zero_33());
            }

            // Every other event, the negotiation event's key fields included, is byte-equal.
            assert_eq!(
                redacted_json["events"].as_array().unwrap()[1..],
                stored_json["events"].as_array().unwrap()[1..]
            );
        }
    }
}

/// T27: the node's own stored record (its `my_swaps` copy) is not redacted by making the report, and the
/// published payload is the redacted copy.
#[test]
fn t27_own_stored_record_is_not_redacted_by_the_report() {
    for role in ROLES {
        let mut node = TestNode::new();
        let uuid = Uuid::new_v4();
        let own = record(uuid, role, "ARRR", "RIN", Outcome::Success, true);
        let own_json = json::to_value(&own).unwrap();
        node.store_own_record(&own);

        node.report(uuid);

        let reloaded = block_on(SavedSwap::load_my_swap_from_db(&node.ctx, uuid))
            .unwrap()
            .unwrap();
        assert_eq!(json::to_value(&reloaded).unwrap(), own_json, "{:?}", role);

        let published: Value = json::from_slice(&only_publication(node.published(), uuid)).unwrap();
        let start = &published["data"]["events"][0]["event"]["data"];
        assert_eq!(start["p2p_privkey"], Value::Null);
        assert_eq!(start["maker_coin_htlc_privkey"], Value::Null);
        assert_eq!(start["taker_coin_htlc_privkey"], Value::Null);
        assert_eq!(start["my_persistent_pub"], zero_33());
        assert_ne!(own_json["events"][0]["event"]["data"]["p2p_privkey"], Value::Null);
    }
}

// ---------------------------------------------------------------------------------------------------------
// T28
// ---------------------------------------------------------------------------------------------------------

/// T28: a maker report creates the row with the maker's own key only; the matching taker report creates no
/// second row and adds the taker's own key only.
#[test]
fn t28_receiver_stores_own_role_keys_only() {
    let mut maker_node = TestNode::new();
    let mut taker_node = TestNode::new();
    let receiver = TestNode::new();
    let uuid = Uuid::new_v4();

    maker_node.store_own_record(&record(uuid, Role::Maker, "RIN", "KMD", Outcome::Success, false));
    taker_node.store_own_record(&record(uuid, Role::Taker, "RIN", "KMD", Outcome::Success, false));
    maker_node.report(uuid);
    taker_node.report(uuid);
    let maker_report = only_publication(maker_node.published(), uuid);
    let taker_report = only_publication(taker_node.published(), uuid);

    // The authors' own stores already hold their own key only.
    assert_eq!(
        maker_node.stats_row(uuid).0.as_deref(),
        Some(maker_persistent_key().as_str()),
        "maker's own store"
    );
    assert_eq!(maker_node.stats_row(uuid).1, None, "maker's own store");
    assert_eq!(taker_node.stats_row(uuid).0, None, "taker's own store");
    assert_eq!(
        taker_node.stats_row(uuid).1.as_deref(),
        Some(taker_persistent_key().as_str()),
        "taker's own store"
    );

    receiver.receive(&maker_report);
    assert_eq!(receiver.stats_rows(), 1);
    let (maker_pubkey, taker_pubkey, maker_price, taker_price) = receiver.stats_row(uuid);
    assert_eq!(maker_pubkey.as_deref(), Some(maker_persistent_key().as_str()));
    assert_eq!(taker_pubkey, None);
    assert_eq!(maker_price.as_deref(), Some("0.02"));
    assert_eq!(taker_price.as_deref(), Some("0.25"));

    receiver.receive(&taker_report);
    assert_eq!(receiver.stats_rows(), 1, "no second row");
    let (maker_pubkey, taker_pubkey, maker_price, taker_price) = receiver.stats_row(uuid);
    assert_eq!(
        maker_pubkey.as_deref(),
        Some(maker_persistent_key().as_str()),
        "maker key unchanged"
    );
    assert_eq!(taker_pubkey.as_deref(), Some(taker_persistent_key().as_str()));
    assert_eq!(maker_price.as_deref(), Some("0.02"));
    assert_eq!(taker_price.as_deref(), Some("0.25"));
}

/// T28: a report whose author signed with a per-swap key (a privacy-coin author) carries an all-zero own key,
/// and the column stays unset (NULL, not an all-zero string); the same for a taker author.
#[test]
fn t28_zeroed_own_key_leaves_the_column_unset() {
    for (role, maker_coin, taker_coin) in [(Role::Maker, "ARRR", "RIN"), (Role::Taker, "RIN", "ARRR")] {
        let mut author = TestNode::new();
        let receiver = TestNode::new();
        let uuid = Uuid::new_v4();
        author.store_own_record(&record(uuid, role, maker_coin, taker_coin, Outcome::Success, true));
        author.report(uuid);
        let report = only_publication(author.published(), uuid);

        receiver.receive(&report);

        assert_eq!(receiver.stats_rows(), 1, "{:?}: the row is still created", role);
        let (maker_pubkey, taker_pubkey, ..) = receiver.stats_row(uuid);
        assert_eq!(maker_pubkey, None, "{:?}", role);
        assert_eq!(taker_pubkey, None, "{:?}", role);
        assert_eq!(author.stats_row(uuid).0, None, "{:?}: author's own store", role);
        assert_eq!(author.stats_row(uuid).1, None, "{:?}: author's own store", role);
    }
}

/// T28: a payload that is neither the signed envelope nor the status object is dropped with no store change.
#[test]
fn t28_undecodable_payload_is_dropped() {
    let receiver = TestNode::new();
    let uuid = Uuid::new_v4();
    for payload in [
        b"not a message".as_slice(),
        br#"{"method":"swapstatus"}"#.as_slice(),
        br#"{"method":"swapstatus","data":{"type":"Nobody"}}"#.as_slice(),
    ] {
        block_on(process_msg(receiver.ctx.clone(), &uuid.to_string(), payload));
        assert_eq!(receiver.stats_rows(), 0);
    }
}
