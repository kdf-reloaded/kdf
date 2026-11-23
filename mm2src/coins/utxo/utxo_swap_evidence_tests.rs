//! CRD ch.51 §51.6G (R100, T42, T43) for the UTXO, QTUM and BCH families: the swap chain reads over Electrum are
//! judged on two servers. Offline, over scripted Electrum servers on distinct loopback addresses (distinct hosts).

use super::*;
use crate::swap_evidence::{is_undetermined, swap_evidence_policy_from, SwapEvidencePolicy};
use crate::utxo::bch::bch_coin_over_fields_for_test;
use crate::utxo::qtum::QtumCoin;
use crate::utxo::rpc_clients::electrum_stub_server::{StubBehavior, StubServer};
use crate::utxo::utxo_common::{dhash160, evidence_nodes, payment_script, EvidenceNodes};
use crate::utxo::utxo_standard::{utxo_standard_coin_with_priv_key, UtxoStandardCoin};
use crate::utxo::utxo_tests::{electrum_client_for_test, utxo_coin_fields_for_test, utxo_coin_from_fields};
use crate::{CanRefundHtlc, FoundSwapTxSpend, MarketCoinOps, SwapOps, ValidatePaymentInput};
use chain::{OutPoint, TransactionInput};
use common::block_on;
use keys::KeyPair;
use mm2_core::mm_ctx::MmCtxBuilder;
use script::{Builder, Opcode};
use serialization::serialize;

const TIME_LOCK: u32 = 100_000;
const SECRET: [u8; 32] = [7; 32];
const HOSTS: [&str; 3] = ["127.0.0.1", "127.0.0.2", "127.0.0.3"];

/// The two keys, the swap's secret hash, and the transactions of one swap payment.
struct Swap {
    my_pub: Vec<u8>,
    other_pub: Vec<u8>,
    secret_hash: Vec<u8>,
    payment: UtxoTx,
}

fn txid_hex(tx: &UtxoTx) -> String { format!("{:?}", tx.hash().reversed()) }

impl Swap {
    fn new() -> Swap {
        let my = KeyPair::random_compressed();
        let other = KeyPair::random_compressed();
        let secret_hash = dhash160(&SECRET).to_vec();
        let redeem = payment_script(TIME_LOCK, &secret_hash, my.public(), other.public());
        let mut payment = UtxoTx::default();
        payment.version = 1;
        payment.inputs.push(TransactionInput {
            previous_output: OutPoint {
                hash: Default::default(),
                index: 0,
            },
            script_sig: vec![1, 2, 3].into(),
            sequence: 0,
            script_witness: vec![],
        });
        payment.outputs.push(TransactionOutput {
            value: 100_000,
            script_pubkey: Builder::build_p2sh(&dhash160(&redeem).into()).to_bytes(),
        });
        Swap {
            my_pub: my.public().to_vec(),
            other_pub: other.public().to_vec(),
            secret_hash,
            payment,
        }
    }

    fn redeem(&self) -> Vec<u8> {
        payment_script(
            TIME_LOCK,
            &self.secret_hash,
            &Public::from_slice(&self.my_pub).unwrap(),
            &Public::from_slice(&self.other_pub).unwrap(),
        )
        .to_vec()
    }

    /// A spend of the payment by the secret `secret`, or by the time lock path when it is `None`.
    fn spend(&self, secret: Option<[u8; 32]>) -> UtxoTx {
        let script_sig = match secret {
            Some(secret) => Builder::default()
                .push_bytes(&[1; 9])
                .push_bytes(&secret)
                .push_opcode(Opcode::OP_0)
                .push_data(&self.redeem())
                .into_bytes(),
            None => Builder::default()
                .push_bytes(&[1; 9])
                .push_opcode(Opcode::OP_1)
                .push_data(&self.redeem())
                .into_bytes(),
        };
        let mut spend = UtxoTx::default();
        spend.version = 1;
        spend.inputs.push(TransactionInput {
            previous_output: OutPoint {
                hash: self.payment.hash(),
                index: 0,
            },
            script_sig,
            sequence: 0,
            script_witness: vec![],
        });
        spend.outputs.push(TransactionOutput {
            value: 90_000,
            script_pubkey: vec![0xaa; 23].into(),
        });
        spend
    }
}

/// The history of the payment's script hash a server shows: the payment, and the spend when there is one.
fn show_history(server: &StubServer, swap: &Swap, spend: Option<&UtxoTx>) {
    let mut history = vec![json!({"tx_hash": txid_hex(&swap.payment), "height": 100})];
    server.set_params_result(
        "blockchain.transaction.get",
        json!([txid_hex(&swap.payment), false]),
        json!(hex::encode(serialize(&swap.payment).take())),
    );
    if let Some(spend) = spend {
        history.push(json!({"tx_hash": txid_hex(spend), "height": 101}));
        server.set_params_result(
            "blockchain.transaction.get",
            json!([txid_hex(spend), false]),
            json!(hex::encode(serialize(spend).take())),
        );
    }
    server.set_result("blockchain.scripthash.get_history", json!(history));
}

/// An 80-byte header (hex) whose time field is `time`.
fn header_with_time(time: u32) -> String {
    let mut header = vec![0u8; 80];
    header[68..72].copy_from_slice(&time.to_le_bytes());
    hex::encode(header)
}

/// A chain of eleven headers ending at `tip`, every header with the time `time`.
fn show_chain(server: &StubServer, tip: u64, time: u32) {
    for height in tip - 10..=tip {
        server.set_chain_header(height, &header_with_time(time));
    }
    server.set_tip(tip);
}

async fn start_servers(count: usize) -> Vec<StubServer> {
    let mut servers = Vec::new();
    for host in HOSTS.iter().take(count) {
        servers.push(StubServer::start_on(host, StubBehavior::Answer).await);
    }
    servers
}

fn adopted_fields(servers: &[&StubServer], field: Option<&str>) -> UtxoCoinFields {
    let addrs: Vec<String> = servers.iter().map(|server| server.addr()).collect();
    let addr_refs: Vec<&str> = addrs.iter().map(String::as_str).collect();
    // The client builder blocks on its own runtime, so it is built on a thread outside the test's runtime.
    let client = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let client = electrum_client_for_test(&addr_refs);
                // A directed read to a server whose connection is still being set up fails in transport, so each
                // server is asked once until it answers (an error answer is an answer too).
                for addr in &addrs {
                    for _ in 0..200 {
                        match block_on(client.blockchain_headers_subscribe_from(addr).compat()) {
                            Err(e) if e.error.is_transport() => {
                                std::thread::sleep(std::time::Duration::from_millis(20))
                            },
                            _ => break,
                        }
                    }
                }
                client
            })
            .join()
            .expect("the client builds")
    });
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Electrum(client), None, false);
    fields.swap_evidence = swap_evidence_policy_from("DOC", field, &addrs).unwrap();
    fields
}

fn utxo_coin(servers: &[&StubServer], field: Option<&str>) -> UtxoStandardCoin {
    utxo_coin_from_fields(adopted_fields(servers, field))
}

async fn search_spend<C: SwapOps>(coin: &C, swap: &Swap) -> Result<Option<FoundSwapTxSpend>, String> {
    coin.search_for_swap_tx_spend_my(
        TIME_LOCK,
        &swap.my_pub,
        &swap.other_pub,
        &swap.secret_hash,
        &serialize(&swap.payment).take(),
        0,
        &None,
    )
    .await
}

fn is_claimed(found: &Result<Option<FoundSwapTxSpend>, String>) -> bool {
    matches!(found, Ok(Some(FoundSwapTxSpend::Spent(_))))
}

/// S: two servers that show the same claim decide it; two that show nothing decide "not spent".
#[test]
fn spend_two_servers_agree() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::new();
        let claim = swap.spend(Some(SECRET));
        show_history(&servers[0], &swap, Some(&claim));
        show_history(&servers[1], &swap, Some(&claim));
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        assert!(is_claimed(&search_spend(&coin, &swap).await));

        let refund = swap.spend(None);
        show_history(&servers[0], &swap, Some(&refund));
        show_history(&servers[1], &swap, Some(&refund));
        assert!(matches!(
            search_spend(&coin, &swap).await,
            Ok(Some(FoundSwapTxSpend::Refunded(_)))
        ));

        show_history(&servers[0], &swap, None);
        show_history(&servers[1], &swap, None);
        assert!(matches!(search_spend(&coin, &swap).await, Ok(None)));
    });
}

/// S: two servers that differ are undetermined, and the third server is not asked. The pool picks the pair, so
/// the three servers show three different things and exactly two of them are asked.
#[test]
fn spend_servers_that_differ_are_undetermined_and_no_third_is_asked() {
    block_on(async {
        let servers = start_servers(3).await;
        let swap = Swap::new();
        // A refund, nothing, and a claim with a secret that does not match (no proof): three decisions.
        show_history(&servers[0], &swap, Some(&swap.spend(None)));
        show_history(&servers[1], &swap, None);
        show_history(&servers[2], &swap, Some(&swap.spend(Some([9; 32]))));
        let coin = utxo_coin(&[&servers[0], &servers[1], &servers[2]], None);
        let error = search_spend(&coin, &swap).await.expect_err("the servers differ");
        assert!(is_undetermined(&error), "{error}");
        assert!(error.contains("disagree"), "{error}");
        let asked: usize = servers
            .iter()
            .map(|server| server.requests("blockchain.scripthash.get_history"))
            .sum();
        assert_eq!(asked, 2, "one pair is asked and no third server");
        assert!(!error.contains("://"));
    });
}

/// S: one server answering is too few answers.
#[test]
fn spend_one_answering_is_too_few() {
    block_on(async {
        let servers = [
            StubServer::start_on(HOSTS[0], StubBehavior::Answer).await,
            StubServer::start_on(HOSTS[1], StubBehavior::Answer).await,
        ];
        let swap = Swap::new();
        show_history(&servers[0], &swap, None);
        show_history(&servers[1], &swap, None);
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        servers[1].set_behavior(StubBehavior::Down).await;
        let error = search_spend(&coin, &swap).await.expect_err("one answer");
        assert!(is_undetermined(&error), "{error}");
        assert!(error.contains("fewer than two"), "{error}");
    });
}

/// S: a trusted node's answer alone decides, and a silent trusted node is undetermined whatever the others say.
#[test]
fn spend_trusted_answering_and_trusted_silent() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::new();
        let claim = swap.spend(Some(SECRET));
        show_history(&servers[0], &swap, Some(&claim));
        show_history(&servers[1], &swap, None);
        let trusted = servers[0].addr();
        let coin = utxo_coin(&[&servers[0], &servers[1]], Some(&trusted));
        assert!(is_claimed(&search_spend(&coin, &swap).await));
        assert_eq!(servers[1].requests("blockchain.scripthash.get_history"), 0);

        servers[0].set_behavior(StubBehavior::Down).await;
        let error = search_spend(&coin, &swap)
            .await
            .expect_err("the trusted node is silent");
        assert!(is_undetermined(&error), "{error}");
        assert!(error.contains("trusted node did not answer"), "{error}");
    });
}

/// S: a claim whose secret hashes to the swap's secret hash, shown by one server only, is taken; a claim with
/// another secret is not.
#[test]
fn spend_a_claim_with_the_matching_secret_on_one_server_is_taken() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::new();
        let claim = swap.spend(Some(SECRET));
        show_history(&servers[0], &swap, Some(&claim));
        show_history(&servers[1], &swap, None);
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        assert!(is_claimed(&search_spend(&coin, &swap).await));

        // The same shape of claim with a secret that does not match proves nothing: the servers disagree.
        let forged = swap.spend(Some([9; 32]));
        show_history(&servers[0], &swap, Some(&forged));
        let error = search_spend(&coin, &swap).await.expect_err("no proof");
        assert!(is_undetermined(&error), "{error}");
    });
}

/// The wait for the spend (no secret hash in its signature): the taken spend, or the error of the wait, for a
/// payment of `swap`, with a deadline of `wait_secs` from now.
async fn wait_for_spend<C: MarketCoinOps>(coin: &C, swap: &Swap, wait_secs: u64) -> Result<UtxoTx, String> {
    let wait_until = common::now_ms() / 1000 + wait_secs;
    coin.wait_for_tx_spend(&serialize(&swap.payment).take(), wait_until, 0, &None)
        .compat()
        .await
        .map(|tx| {
            let bytes = tx.tx_hex();
            crate::utxo::utxo_common::read_utxo_tx(&bytes).expect("a utxo transaction")
        })
        .map_err(|e| format!("{e:?}"))
}

/// R100 item 3 on the wait for the spend: a claim shown by ONE server is taken although the other server is at an
/// older tip and shows no spend.
#[test]
fn wait_for_spend_takes_a_claim_shown_by_one_server() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::new();
        let claim = swap.spend(Some(SECRET));
        show_history(&servers[0], &swap, Some(&claim));
        show_history(&servers[1], &swap, None);
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        let taken = wait_for_spend(&coin, &swap, 60).await.expect("the claim is taken");
        assert_eq!(txid_hex(&taken), txid_hex(&claim));
    });
}

/// A spend whose redeem script does not hash to the payment's P2SH is not taken from one server.
#[test]
fn wait_for_spend_does_not_take_a_forged_redeem_script() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::new();
        // A redeem script of another lock time commits to the same secret hash but is not the payment's.
        let forged_redeem = payment_script(
            TIME_LOCK + 1,
            &swap.secret_hash,
            &Public::from_slice(&swap.my_pub).unwrap(),
            &Public::from_slice(&swap.other_pub).unwrap(),
        )
        .to_vec();
        let mut forged = swap.spend(Some(SECRET));
        forged.inputs[0].script_sig = Builder::default()
            .push_bytes(&[1; 9])
            .push_bytes(&SECRET)
            .push_opcode(Opcode::OP_0)
            .push_data(&forged_redeem)
            .into_bytes();
        show_history(&servers[0], &swap, Some(&forged));
        show_history(&servers[1], &swap, None);
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        let error = wait_for_spend(&coin, &swap, 0)
            .await
            .expect_err("a forged spend is not taken");
        assert!(!error.is_empty());
    });
}

/// A claim with a secret that does not satisfy the redeem script's hash is not taken from one server.
#[test]
fn wait_for_spend_does_not_take_a_wrong_secret() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::new();
        let wrong = swap.spend(Some([9; 32]));
        show_history(&servers[0], &swap, Some(&wrong));
        show_history(&servers[1], &swap, None);
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        let error = wait_for_spend(&coin, &swap, 0)
            .await
            .expect_err("a wrong secret is not taken");
        assert!(!error.is_empty());
    });
}

/// The look on every server: a hash-valid claim on ANY server is found, whichever pair the pool asks first; a
/// refund or a forged claim on the third server is ignored.
#[test]
fn the_every_server_look_finds_a_proof_on_a_third_server_and_ignores_the_rest() {
    block_on(async {
        let servers = start_servers(3).await;
        let swap = Swap::new();
        let coin = utxo_coin(&[&servers[0], &servers[1], &servers[2]], None);
        let payment_bytes = serialize(&swap.payment).take();
        let look = || {
            coin.search_for_swap_tx_spend_my_on_every_server(
                TIME_LOCK,
                &swap.my_pub,
                &swap.other_pub,
                &swap.secret_hash,
                &payment_bytes,
            )
        };
        for shown in 0..3 {
            // The claim is on server `shown` only, the other two show nothing.
            for (index, server) in servers.iter().enumerate() {
                let claim = swap.spend(Some(SECRET));
                show_history(server, &swap, (index == shown).then_some(&claim));
            }
            assert!(
                matches!(look().await, Some(FoundSwapTxSpend::Spent(_))),
                "the claim on server {shown} is found"
            );
        }
        // A refund, or a claim with another secret, on one server is no finding while the others show nothing.
        for ignored in [swap.spend(None), swap.spend(Some([9; 32]))] {
            for shown in 0..3 {
                for (index, server) in servers.iter().enumerate() {
                    show_history(server, &swap, (index == shown).then_some(&ignored));
                }
                assert!(look().await.is_none(), "an unproven spend on server {shown} is ignored");
            }
        }
    });
}

/// S, in every family: an undetermined read is `Err` that the engine's test accepts.
#[test]
fn an_undetermined_spend_read_is_marked_in_utxo_qtum_and_bch() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::new();
        show_history(&servers[0], &swap, Some(&swap.spend(None)));
        show_history(&servers[1], &swap, None);
        let new_fields = || adopted_fields(&[&servers[0], &servers[1]], None);
        let utxo = utxo_coin_from_fields(new_fields());
        let qtum = QtumCoin::from(UtxoArc::new(new_fields()));
        let bch = bch_coin_over_fields_for_test(new_fields());
        for (family, found) in [
            ("UTXO", search_spend(&utxo, &swap).await),
            ("QTUM", search_spend(&qtum, &swap).await),
            ("BCH", search_spend(&bch, &swap).await),
        ] {
            let error = found.expect_err(family);
            assert!(is_undetermined(&error), "{family}: {error}");
        }
    });
}

/// The three families answer the policy their stored policy gives, now that R100 is on for them.
#[test]
fn the_three_families_answer_their_stored_policy() {
    block_on(async {
        let servers = start_servers(2).await;
        let new_fields = || adopted_fields(&[&servers[0], &servers[1]], None);
        let utxo = utxo_coin_from_fields(new_fields());
        let qtum = QtumCoin::from(UtxoArc::new(new_fields()));
        let bch = bch_coin_over_fields_for_test(new_fields());
        for policy in [
            utxo.swap_evidence_policy(),
            qtum.swap_evidence_policy(),
            bch.swap_evidence_policy(),
        ] {
            assert!(matches!(policy, SwapEvidencePolicy::TwoNodes { .. }), "{policy:?}");
        }
    });
}

/// P: the payment is fetched from two servers that must agree on its bytes.
#[test]
fn payment_validation_two_servers_agree_and_differ() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::new();
        let bytes = serialize(&swap.payment).take();
        servers[0].set_result("blockchain.transaction.get", json!(hex::encode(&bytes)));
        servers[1].set_result("blockchain.transaction.get", json!(hex::encode(&bytes)));
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        let input = ValidatePaymentInput {
            payment_tx: bytes.clone(),
            time_lock: TIME_LOCK,
            taker_pub: swap.other_pub.clone(),
            maker_pub: swap.my_pub.clone(),
            secret_hash: swap.secret_hash.clone(),
            amount: big_decimal_from_sat(100_000, 8),
            swap_contract_address: None,
            try_spv_proof_until: 0,
            confirmations: 0,
        };
        coin.validate_maker_payment(input.clone()).compat().await.unwrap();

        // One server shows other bytes: undetermined (after the retries of the read).
        servers[1].set_result("blockchain.transaction.get", json!("00"));
        let error = coin
            .validate_maker_payment(input)
            .compat()
            .await
            .expect_err("the servers differ");
        assert!(is_undetermined(&error), "{error}");
    });
}

/// The already-sent read: the payment found or not, with its txid, on two servers.
#[test]
fn already_sent_two_servers_agree_and_differ() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::new();
        show_history(&servers[0], &swap, None);
        show_history(&servers[1], &swap, None);
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        let sent = |coin: &UtxoStandardCoin| {
            coin.check_if_my_payment_sent(TIME_LOCK, &swap.my_pub, &swap.other_pub, &swap.secret_hash, 0, &None)
                .compat()
        };
        // The payment pays the script of the history it is asked for: any history is the payment's.
        assert!(sent(&coin).await.unwrap().is_some());
        servers[1].set_result("blockchain.scripthash.get_history", json!([]));
        let error = sent(&coin).await.expect_err("found on one server only");
        assert!(is_undetermined(&error), "{error}");
        servers[0].set_result("blockchain.scripthash.get_history", json!([]));
        assert!(sent(&coin).await.unwrap().is_none());
    });
}

/// Whether the refund gate is open now; `Err` is the read's error.
async fn can_refund(coin: &UtxoStandardCoin) -> Result<bool, String> {
    can_refund_of(coin.can_refund_htlc(TIME_LOCK as u64).compat().await)
}

fn can_refund_of(gate: Result<CanRefundHtlc, String>) -> Result<bool, String> {
    gate.map(|gate| matches!(gate, CanRefundHtlc::CanRefundNow))
}

/// T: two servers whose median time past is past the lock agree, even when the two times differ; a time that is
/// not past is a disagreement.
#[test]
fn mtp_two_servers_agree_when_both_are_past_the_lock() {
    block_on(async {
        let servers = start_servers(2).await;
        show_chain(&servers[0], 30, TIME_LOCK + 5_000);
        show_chain(&servers[1], 30, TIME_LOCK + 90_000);
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        assert_eq!(can_refund(&coin).await, Ok(true));

        // One server is not past the lock yet.
        show_chain(&servers[1], 30, TIME_LOCK - 5_000);
        let error = can_refund(&coin).await.expect_err("the servers differ");
        assert!(is_undetermined(&error), "{error}");
        assert!(error.contains("disagree"), "{error}");
    });
}

/// T: tips more than the pool's tolerance apart are undetermined; tips at the tolerance are compared.
#[test]
fn mtp_tips_beyond_the_pool_tolerance_are_undetermined() {
    block_on(async {
        let servers = start_servers(2).await;
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        let UtxoRpcClientEnum::Electrum(client) = &coin.as_ref().rpc_client else {
            panic!("an Electrum coin");
        };
        let tolerance = client.tolerance_blocks();
        assert!((1..=20).contains(&tolerance), "{tolerance}");
        show_chain(&servers[0], 30, TIME_LOCK + 5_000);
        show_chain(&servers[1], 30 + tolerance + 1, TIME_LOCK + 5_000);
        let error = can_refund(&coin).await.expect_err("one block beyond the tolerance");
        assert!(is_undetermined(&error), "{error}");
        assert!(error.contains("tips"), "{error}");

        show_chain(&servers[1], 30 + tolerance, TIME_LOCK + 5_000);
        assert_eq!(can_refund(&coin).await, Ok(true));
    });
}

/// T: a trusted node's median time past decides alone; a silent trusted node is undetermined.
#[test]
fn mtp_trusted_answering_and_trusted_silent() {
    block_on(async {
        let servers = start_servers(2).await;
        show_chain(&servers[0], 30, TIME_LOCK + 5_000);
        show_chain(&servers[1], 90, TIME_LOCK - 5_000);
        let trusted = servers[0].addr();
        let coin = utxo_coin(&[&servers[0], &servers[1]], Some(&trusted));
        assert_eq!(can_refund(&coin).await, Ok(true));
        servers[0].set_behavior(StubBehavior::Down).await;
        let error = can_refund(&coin).await.expect_err("the trusted node is silent");
        assert!(is_undetermined(&error), "{error}");
    });
}

/// T, in every family: an undetermined refund gate is `Err` that the engine's test accepts.
#[test]
fn an_undetermined_refund_gate_is_marked_in_utxo_qtum_and_bch() {
    block_on(async {
        let servers = start_servers(2).await;
        show_chain(&servers[0], 30, TIME_LOCK + 5_000);
        show_chain(&servers[1], 60, TIME_LOCK + 5_000);
        let new_fields = || adopted_fields(&[&servers[0], &servers[1]], None);
        let utxo = utxo_coin_from_fields(new_fields());
        let qtum = QtumCoin::from(UtxoArc::new(new_fields()));
        let bch = bch_coin_over_fields_for_test(new_fields());
        for (family, error) in [
            (
                "UTXO",
                can_refund_of(utxo.can_refund_htlc(TIME_LOCK as u64).compat().await),
            ),
            (
                "QTUM",
                can_refund_of(qtum.can_refund_htlc(TIME_LOCK as u64).compat().await),
            ),
            (
                "BCH",
                can_refund_of(bch.can_refund_htlc(TIME_LOCK as u64).compat().await),
            ),
        ] {
            let error = error.expect_err(family);
            assert!(is_undetermined(&error), "{family}: {error}");
        }
    });
}

/// The confirmation decision: two servers that both show enough confirmations; a server that shows fewer.
#[test]
fn confirmation_decision_goes_through_two_servers() {
    use crate::utxo::utxo_common::agreed_confirmed;
    block_on(async {
        let servers = start_servers(2).await;
        let shown = |confirmations: u32| {
            json!({"hex": "00", "txid": "00".repeat(32), "version": 1, "locktime": 0, "vin": [], "vout": [],
                   "confirmations": confirmations})
        };
        servers[0].set_result("blockchain.transaction.get", shown(3));
        servers[1].set_result("blockchain.transaction.get", shown(3));
        let fields = adopted_fields(&[&servers[0], &servers[1]], None);
        let nodes = evidence_nodes(&fields, &fields.swap_evidence).expect("two servers");
        let txid = H256Json::from([5u8; 32]);
        assert_eq!(
            agreed_confirmed(&fields.swap_evidence, &nodes, &txid, 2, false).await,
            Ok(true)
        );
        assert_eq!(
            agreed_confirmed(&fields.swap_evidence, &nodes, &txid, 4, false).await,
            Ok(false)
        );
        servers[1].set_result("blockchain.transaction.get", shown(1));
        let error = agreed_confirmed(&fields.swap_evidence, &nodes, &txid, 2, false)
            .await
            .expect_err("the servers differ");
        assert!(is_undetermined(&error), "{error}");
    });
}

/// Native mode and a family that has not adopted R100 read as before: one node, no agreement.
#[test]
fn native_mode_and_not_adopted_keep_the_single_node_reads() {
    block_on(async {
        let servers = start_servers(2).await;
        let fields = adopted_fields(&[&servers[0], &servers[1]], None);
        assert!(evidence_nodes(&fields, &fields.swap_evidence).is_some());
        assert!(evidence_nodes(&fields, &SwapEvidencePolicy::NotAdopted).is_none());
        assert!(evidence_nodes(&fields, &SwapEvidencePolicy::OwnDaemon).is_none());
        let native = utxo_coin_fields_for_test(
            UtxoRpcClientEnum::Native(crate::utxo::utxo_tests::native_client_for_test()),
            None,
            false,
        );
        assert!(evidence_nodes(&native, &SwapEvidencePolicy::TwoNodes { hosts: vec![] }).is_none());
        let _: Option<EvidenceNodes> = None;
    });
}

/// Admission: a coin over one Electrum server is refused at the order, and accepted with `swap_evidence_node`.
#[test]
fn a_one_server_coin_is_refused_and_accepted_with_the_evidence_node() {
    block_on(async {
        let server = StubServer::start(StubBehavior::Answer).await;
        server.set_result("blockchain.headers.subscribe", json!({"height": 777, "hex": "00"}));
        let conf = json!({"coin": "DOC", "asset": "DOC", "rpcport": 62415});
        let activate = |extra: Json| {
            let conf = conf.clone();
            let addr = server.addr();
            async move {
                let mut req = json!({"method": "electrum", "servers": [{"url": addr}]});
                if let Some(field) = extra.as_str() {
                    req["swap_evidence_node"] = json!(field);
                }
                let ctx = MmCtxBuilder::new().into_mm_arc();
                let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
                utxo_standard_coin_with_priv_key(&ctx, "DOC", &conf, &params, &[1u8; 32])
                    .await
                    .unwrap()
            }
        };
        let refused = activate(Json::Null).await;
        let text = refused.swap_evidence_refusal().expect("one server is refused");
        assert!(text.contains("swap_evidence_node"), "{text}");
        let accepted = activate(json!(server.addr())).await;
        assert_eq!(accepted.swap_evidence_refusal(), None);
    });
}
