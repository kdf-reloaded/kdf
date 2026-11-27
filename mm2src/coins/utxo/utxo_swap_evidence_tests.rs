//! CRD ch.51 §51.6G (R100, T42, T43) for the UTXO, QTUM and BCH families: the swap chain reads over Electrum are
//! judged on two servers. Offline, over scripted Electrum servers on distinct loopback addresses (distinct hosts).

use super::*;
use crate::swap_evidence::{is_undetermined, swap_evidence_policy_from, ReadyMode, SwapEvidencePolicy};
use crate::utxo::bch::bch_coin_over_fields_for_test;
use crate::utxo::qtum::QtumCoin;
use crate::utxo::rpc_clients::electrum_stub_server::{StubBehavior, StubServer};
use crate::utxo::utxo_common::{self, dhash160, evidence_nodes, payment_script, EvidenceNodes};
use crate::utxo::utxo_standard::{utxo_standard_coin_with_priv_key, UtxoStandardCoin};
use crate::utxo::utxo_tests::{electrum_client_for_test, utxo_coin_fields_for_test, utxo_coin_from_fields};
use crate::{CanRefundHtlc, FoundSwapTxSpend, MarketCoinOps, SwapOps, ValidatePaymentInput};
use chain::{OutPoint, TransactionInput};
use common::block_on;
use keys::KeyPair;
use mm2_core::mm_ctx::MmCtxBuilder;
use script::{Builder, Opcode};
use serialization::serialize;

pub(crate) const TIME_LOCK: u32 = 100_000;
pub(crate) const SECRET: [u8; 32] = [7; 32];
const HOSTS: [&str; 3] = ["127.0.0.1", "127.0.0.2", "127.0.0.3"];

/// The two keys, the swap's secret hash, and the transactions of one swap payment.
pub(crate) struct Swap {
    pub(crate) my_pub: Vec<u8>,
    pub(crate) other_pub: Vec<u8>,
    pub(crate) secret_hash: Vec<u8>,
    pub(crate) payment: UtxoTx,
    /// The index of the HTLC output in the payment: 0 for a plain UTXO payment, 1 for an SLP one.
    htlc_vout: usize,
}

pub(crate) fn txid_hex(tx: &UtxoTx) -> String { format!("{:?}", tx.hash().reversed()) }

impl Swap {
    pub(crate) fn new() -> Swap { Swap::with_htlc_at(0) }

    /// An SLP-shaped payment (htlc_vout 1): an OP_RETURN output at 0 and the HTLC at 1.
    pub(crate) fn slp_shaped() -> Swap { Swap::with_htlc_at(1) }

    fn with_htlc_at(htlc_vout: usize) -> Swap {
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
        if htlc_vout == 1 {
            payment.outputs.push(TransactionOutput {
                value: 0,
                script_pubkey: Builder::default()
                    .push_opcode(Opcode::OP_RETURN)
                    .push_bytes(b"SLP")
                    .into_bytes(),
            });
        }
        payment.outputs.push(TransactionOutput {
            value: 100_000,
            script_pubkey: Builder::build_p2sh(&dhash160(&redeem).into()).to_bytes(),
        });
        Swap {
            my_pub: my.public().to_vec(),
            other_pub: other.public().to_vec(),
            secret_hash,
            payment,
            htlc_vout,
        }
    }

    pub(crate) fn redeem(&self) -> Vec<u8> {
        payment_script(
            TIME_LOCK,
            &self.secret_hash,
            &Public::from_slice(&self.my_pub).unwrap(),
            &Public::from_slice(&self.other_pub).unwrap(),
        )
        .to_vec()
    }

    /// A spend of the payment by the secret `secret`, or by the time lock path when it is `None`.
    pub(crate) fn spend(&self, secret: Option<[u8; 32]>) -> UtxoTx {
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
                index: self.htlc_vout as u32,
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
pub(crate) fn show_history(server: &StubServer, swap: &Swap, spend: Option<&UtxoTx>) {
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
pub(crate) fn show_chain(server: &StubServer, tip: u64, time: u32) {
    for height in tip - 10..=tip {
        server.set_chain_header(height, &header_with_time(time));
    }
    server.set_tip(tip);
}

pub(crate) async fn start_servers(count: usize) -> Vec<StubServer> {
    let mut servers = Vec::new();
    for host in HOSTS.iter().take(count) {
        servers.push(StubServer::start_on(host, StubBehavior::Answer).await);
    }
    servers
}

pub(crate) fn adopted_fields(servers: &[&StubServer], field: Option<&str>) -> UtxoCoinFields {
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

/// The shared search of `_my` (or `_other`, whose keys are the other way round: the payment is the other side's)
/// at the HTLC output of the payment, as the SLP coin calls it.
async fn search_spend_at_htlc_output(
    coin: &UtxoStandardCoin,
    swap: &Swap,
    mine: bool,
) -> Result<Option<FoundSwapTxSpend>, String> {
    let policy = crate::MmCoin::swap_evidence_policy(coin);
    let tx = serialize(&swap.payment).take();
    if mine {
        utxo_common::search_for_swap_tx_spend_my(
            coin.as_ref(),
            &policy,
            TIME_LOCK,
            &swap.my_pub,
            &swap.other_pub,
            &swap.secret_hash,
            &tx,
            swap.htlc_vout,
            0,
        )
        .await
    } else {
        utxo_common::search_for_swap_tx_spend_other(
            coin.as_ref(),
            &policy,
            TIME_LOCK,
            &swap.other_pub,
            &swap.my_pub,
            &swap.secret_hash,
            &tx,
            swap.htlc_vout,
            0,
        )
        .await
    }
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

/// S: an SLP-shaped payment (OP_RETURN at output 0, the HTLC at output 1) is searched at output 1: the claim and
/// the refund are found by `_my` and `_other`, and nothing is found when nothing spent it.
#[test]
fn spend_of_an_slp_shaped_payment_is_found_at_output_one() {
    block_on(async {
        let servers = start_servers(2).await;
        let swap = Swap::slp_shaped();
        let claim = swap.spend(Some(SECRET));
        show_history(&servers[0], &swap, Some(&claim));
        show_history(&servers[1], &swap, Some(&claim));
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        assert!(is_claimed(&search_spend_at_htlc_output(&coin, &swap, true).await));
        assert!(is_claimed(&search_spend_at_htlc_output(&coin, &swap, false).await));

        let refund = swap.spend(None);
        show_history(&servers[0], &swap, Some(&refund));
        show_history(&servers[1], &swap, Some(&refund));
        assert!(matches!(
            search_spend_at_htlc_output(&coin, &swap, true).await,
            Ok(Some(FoundSwapTxSpend::Refunded(_)))
        ));
        assert!(matches!(
            search_spend_at_htlc_output(&coin, &swap, false).await,
            Ok(Some(FoundSwapTxSpend::Refunded(_)))
        ));

        show_history(&servers[0], &swap, None);
        show_history(&servers[1], &swap, None);
        assert!(matches!(
            search_spend_at_htlc_output(&coin, &swap, true).await,
            Ok(None)
        ));
    });
}

/// S: a wrong HTLC script at the searched output is a hard stop naming the output, for an SLP-shaped payment
/// (output 1) and for a plain one (output 0). No server is asked for a history.
#[test]
fn spend_of_a_payment_with_a_wrong_htlc_script_is_a_hard_stop_naming_the_output() {
    block_on(async {
        let servers = start_servers(2).await;
        let coin = utxo_coin(&[&servers[0], &servers[1]], None);
        for htlc_vout in [0usize, 1] {
            let mut swap = Swap::with_htlc_at(htlc_vout);
            swap.payment.outputs[htlc_vout].script_pubkey =
                Builder::build_p2sh(&dhash160(b"another script").into()).to_bytes();
            show_history(&servers[0], &swap, None);
            show_history(&servers[1], &swap, None);
            for found in [
                search_spend_at_htlc_output(&coin, &swap, true).await,
                search_spend_at_htlc_output(&coin, &swap, false).await,
            ] {
                let error = found.expect_err("the output does not pay the HTLC");
                assert!(
                    error.contains(&format!("output {} does not pay the HTLC script", htlc_vout)),
                    "{error}"
                );
                assert!(!is_undetermined(&error), "{error}");
            }
        }
        let asked: usize = servers
            .iter()
            .map(|server| server.requests("blockchain.scripthash.get_history"))
            .sum();
        assert_eq!(asked, 0);
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
        let activate = |extra: Json, allow: bool| {
            let conf = conf.clone();
            let addr = server.addr();
            async move {
                let mut req = json!({"method": "electrum", "servers": [{"url": addr}]});
                if let Some(field) = extra.as_str() {
                    req["swap_evidence_node"] = json!(field);
                }
                let ctx = MmCtxBuilder::new()
                    .with_conf(json!({ "swap_evidence_allow_single_host": allow }))
                    .into_mm_arc();
                let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
                utxo_standard_coin_with_priv_key(&ctx, "DOC", &conf, &params, &[1u8; 32])
                    .await
                    .unwrap()
            }
        };
        // the single-host switch off: today's refusal
        let refused = activate(Json::Null, false).await;
        let text = refused.swap_evidence_refusal().expect("one server is refused");
        assert!(text.contains("swap_evidence_node"), "{text}");
        let accepted = activate(json!(server.addr()), false).await;
        assert_eq!(accepted.swap_evidence_refusal(), None);
        // the switch on (the default): one configured host is relaxed and admitted without the field
        let relaxed = activate(Json::Null, true).await;
        assert_eq!(relaxed.swap_evidence_refusal(), None);
    });
}

// ---- The readiness check before a swap starts (maintainer decision of 2026-11-25/26 under R100) ----

mod readiness {
    use super::*;
    use crate::swap_evidence::{relax_single_host, NOT_READY_MARKER};
    use crate::utxo::rpc_clients::ElectrumClient;
    use crate::MmCoin;

    fn client_of(coin: &UtxoStandardCoin) -> ElectrumClient {
        match &coin.as_ref().rpc_client {
            UtxoRpcClientEnum::Electrum(client) => client.clone(),
            _ => panic!("an Electrum coin"),
        }
    }

    fn pings(servers: &[StubServer]) -> Vec<usize> {
        servers.iter().map(|server| server.requests("server.ping")).collect()
    }

    /// Two of three hosts answering: ready, and nothing extra is sent; one of three: not ready.
    #[test]
    fn two_of_three_hosts_answering_is_ready_one_is_not() {
        block_on(async {
            let servers = start_servers(3).await;
            let coin = utxo_coin(&[&servers[0], &servers[1], &servers[2]], None);
            // Every server answered while the coin was built: the healthy case sends no probe.
            let before = pings(&servers);
            assert_eq!(coin.swap_evidence_ready().await, Ok(()));
            assert_eq!(pings(&servers), before, "the healthy case sends nothing extra");

            // The third host is down and nothing is known of any host: the probes find two answering.
            servers[2].set_behavior(StubBehavior::Down).await;
            client_of(&coin).forget_readiness_answers();
            assert_eq!(coin.swap_evidence_ready().await, Ok(()), "two of three answer");

            let servers = start_servers(3).await;
            let coin = utxo_coin(&[&servers[0], &servers[1], &servers[2]], None);
            servers[1].set_behavior(StubBehavior::Down).await;
            servers[2].set_behavior(StubBehavior::Down).await;
            let client = client_of(&coin);
            client.forget_readiness_answers();
            let error = coin.swap_evidence_ready().await.expect_err("one of three answers");
            assert_eq!(
                error,
                "DOC: swap evidence not available (fewer than two hosts answering)"
            );
            assert!(!error.contains("127.0.0"), "the text names no host: {error}");
        });
    }

    /// Unknown hosts are probed with one `server.ping` each, only while the answering hosts are not enough, and a
    /// host is not probed again within the window.
    #[test]
    fn unknown_hosts_are_probed_once_per_window() {
        block_on(async {
            let servers = start_servers(2).await;
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            let client = client_of(&coin);
            servers[1].set_behavior(StubBehavior::Down).await;
            client.forget_readiness_answers();

            let before = pings(&servers);
            let error = coin.swap_evidence_ready().await.expect_err("one host answers");
            assert!(error.contains(NOT_READY_MARKER), "{error}");
            assert_eq!(
                pings(&servers)[0],
                before[0] + 1,
                "the unknown host that is up got one ping"
            );

            // Within the window: the first host's probe answer counts, the second is not probed again, even once
            // it is back.
            servers[1].set_behavior(StubBehavior::Answer).await;
            let before = pings(&servers);
            assert!(coin.swap_evidence_ready().await.is_err());
            assert_eq!(pings(&servers), before, "no probe within the window");
        });
    }

    /// The probe of a host that accepts the connection and never answers ends at the 5 s bound.
    #[test]
    fn a_silent_host_is_given_up_at_the_probe_bound() {
        block_on(async {
            let servers = start_servers(2).await;
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            let client = client_of(&coin);
            servers[1].set_behavior(StubBehavior::Down).await;
            client.forget_readiness_answers();
            servers[1].set_behavior(StubBehavior::Silent).await;
            let started = std::time::Instant::now();
            assert!(coin.swap_evidence_ready().await.is_err());
            let elapsed = started.elapsed();
            assert!(
                elapsed.as_secs_f64() >= 4.9 && elapsed.as_secs_f64() < 7.,
                "the probe bound was not honored: {elapsed:?}"
            );
        });
    }

    /// A trusted server: ready while it answers, not while it does not, whatever the other server does.
    #[test]
    fn a_trusted_server_up_and_down() {
        block_on(async {
            let servers = start_servers(2).await;
            let trusted = servers[0].addr();
            let coin = utxo_coin(&[&servers[0], &servers[1]], Some(&trusted));
            assert_eq!(coin.swap_evidence_ready().await, Ok(()));
            servers[0].set_behavior(StubBehavior::Down).await;
            client_of(&coin).forget_readiness_answers();
            assert_eq!(
                coin.swap_evidence_ready().await,
                Err("DOC: swap evidence not available (the trusted node is not answering)".to_owned())
            );
        });
    }

    /// The single-host switch: a coin relaxed to its one host is ready only while that host answers.
    #[test]
    fn a_relaxed_single_host_coin_is_ready_only_while_its_host_answers() {
        block_on(async {
            let servers = start_servers(1).await;
            let mut fields = adopted_fields(&[&servers[0]], None);
            let configured = vec![servers[0].addr()];
            let (policy, relaxed) = relax_single_host(fields.swap_evidence.clone(), None, &configured, true);
            assert!(relaxed);
            fields.swap_evidence = policy;
            let coin = utxo_coin_from_fields(fields);
            assert_eq!(coin.swap_evidence_ready().await, Ok(()));
            servers[0].set_behavior(StubBehavior::Down).await;
            client_of(&coin).forget_readiness_answers();
            assert_eq!(
                coin.swap_evidence_ready().await,
                Err("DOC: swap evidence not available (the trusted node is not answering)".to_owned())
            );
        });
    }

    /// The own daemon: ready while it answers `getblockcount`; a coin with too few nodes is never ready; a family
    /// that has not adopted R100 always is.
    #[test]
    fn own_daemon_too_few_nodes_and_not_adopted() {
        block_on(async {
            use crate::utxo::rpc_clients::NativeClientImpl;
            use mocktopus::mocking::{MockResult, Mockable};

            let native = || {
                utxo_coin_fields_for_test(
                    UtxoRpcClientEnum::Native(crate::utxo::utxo_tests::native_client_for_test()),
                    None,
                    false,
                )
            };
            let mut fields = native();
            fields.conf.ticker = "OWNUP".into();
            NativeClientImpl::get_block_count
                .mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(800u64))));
            assert_eq!(
                utxo_common::swap_evidence_ready(&fields, &SwapEvidencePolicy::OwnDaemon, ReadyMode::Strict).await,
                Ok(())
            );
            let mut fields = native();
            fields.conf.ticker = "OWNDOWN".into();
            NativeClientImpl::get_block_count.mock_safe(|_| {
                let error = common::jsonrpc_client::JsonRpcError {
                    client_info: "test".into(),
                    request: common::jsonrpc_client::JsonRpcRequestEnum::Single(
                        common::jsonrpc_client::JsonRpcRequest {
                            jsonrpc: "2.0".into(),
                            id: "1".into(),
                            method: "getblockcount".into(),
                            params: vec![],
                        },
                    ),
                    error: common::jsonrpc_client::JsonRpcErrorType::Transport("down".into()),
                };
                MockResult::Return(Box::new(futures01::future::err(error)))
            });
            assert_eq!(
                utxo_common::swap_evidence_ready(&fields, &SwapEvidencePolicy::OwnDaemon, ReadyMode::Strict).await,
                Err("OWNDOWN: swap evidence not available (the own daemon is not answering)".to_owned())
            );
            assert_eq!(
                utxo_common::swap_evidence_ready(
                    &fields,
                    &SwapEvidencePolicy::Insufficient { hosts: 1 },
                    ReadyMode::Strict
                )
                .await,
                Err("OWNDOWN: swap evidence not available (fewer than two nodes listed)".to_owned())
            );
            assert_eq!(
                utxo_common::swap_evidence_ready(&fields, &SwapEvidencePolicy::NotAdopted, ReadyMode::Strict).await,
                Ok(())
            );
        });
    }
}

// ---- The reads of the version 2 coin methods (CRD ch.51 R100, ch.52 R18) ----

mod swap_v2_reads {
    use super::*;
    use crate::{FindPaymentSpendError, FundingTxSpend, SearchForFundingSpendErr, TakerCoinSwapOpsV2};

    /// A spend of the output the swap's payment has at its HTLC position, in the shape of the funding's cooperative
    /// branch (neither the timelock selector nor a 32-byte secret after the signature).
    fn cooperative_spend(swap: &Swap) -> UtxoTx {
        let mut spend = swap.spend(None);
        spend.inputs[0].script_sig = Builder::default()
            .push_bytes(&[1; 9])
            .push_bytes(&[2; 9])
            .push_bytes(&[3; 9])
            .into_bytes();
        spend
    }

    /// A spend of the payment at its second input: the first input spends another output.
    fn spend_at_second_input(swap: &Swap) -> UtxoTx {
        let mut spend = swap.spend(None);
        let first = TransactionInput {
            previous_output: OutPoint {
                hash: [9u8; 32].into(),
                index: 0,
            },
            script_sig: vec![1, 2, 3].into(),
            sequence: 0,
            script_witness: vec![],
        };
        spend.inputs.insert(0, first);
        spend
    }

    async fn search_funding(
        coin: &UtxoStandardCoin,
        swap: &Swap,
    ) -> Result<Option<FundingTxSpend<UtxoStandardCoin>>, SearchForFundingSpendErr> {
        coin.search_for_taker_funding_spend(&swap.payment, 0, &swap.secret_hash)
            .await
    }

    async fn find_payment_spend(
        coin: &UtxoStandardCoin,
        swap: &Swap,
        wait_secs: i64,
    ) -> Result<UtxoTx, FindPaymentSpendError> {
        let wait_until = (common::now_ms() / 1000) as i64 + wait_secs;
        coin.find_taker_payment_spend_tx(&swap.payment, 0, wait_until as u64)
            .await
            .map_err(|e| e.into_inner())
    }

    fn marker_of_rpc(error: &SearchForFundingSpendErr) -> &str {
        match error {
            SearchForFundingSpendErr::Rpc(text) => text,
            other => panic!("not a transient read error: {other:?}"),
        }
    }

    fn marker_of_transport(error: &FindPaymentSpendError) -> &str {
        match error {
            FindPaymentSpendError::Transport(text) => text,
            other => panic!("not an undetermined read: {other:?}"),
        }
    }

    /// The search for the funding spend: two servers that show the same spend decide it, in each of its three
    /// kinds, and two that show nothing decide "not spent".
    #[test]
    fn funding_spend_two_servers_agree_on_each_kind() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            for server in &servers {
                show_history(server, &swap, None);
            }
            assert!(matches!(search_funding(&coin, &swap).await, Ok(None)));

            let cooperative = cooperative_spend(&swap);
            for server in &servers {
                show_history(server, &swap, Some(&cooperative));
            }
            assert!(matches!(
                search_funding(&coin, &swap).await,
                Ok(Some(FundingTxSpend::TransferredToTakerPayment(_)))
            ));

            let timelock = swap.spend(None);
            for server in &servers {
                show_history(server, &swap, Some(&timelock));
            }
            assert!(matches!(
                search_funding(&coin, &swap).await,
                Ok(Some(FundingTxSpend::RefundedTimelock(_)))
            ));

            let secret_refund = swap.spend(Some(SECRET));
            for server in &servers {
                show_history(server, &swap, Some(&secret_refund));
            }
            assert!(matches!(
                search_funding(&coin, &swap).await,
                Ok(Some(FundingTxSpend::RefundedSecret { secret, .. })) if secret == SECRET
            ));
        });
    }

    /// Servers that differ, or fewer than two that answer, leave the search undetermined: a transient read error
    /// with the marker, never "not spent".
    #[test]
    fn funding_spend_differing_or_failing_servers_are_undetermined() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            show_history(&servers[0], &swap, Some(&cooperative_spend(&swap)));
            show_history(&servers[1], &swap, None);
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            let error = search_funding(&coin, &swap).await.expect_err("the servers differ");
            assert!(is_undetermined(marker_of_rpc(&error)), "{error}");
            assert!(marker_of_rpc(&error).contains("disagree"), "{error}");
            assert!(!marker_of_rpc(&error).contains("://"), "{error}");

            // One server answers, the other fails.
            show_history(&servers[1], &swap, Some(&cooperative_spend(&swap)));
            servers[1].set_behavior(StubBehavior::Down).await;
            let error = search_funding(&coin, &swap).await.expect_err("one answer");
            assert!(is_undetermined(marker_of_rpc(&error)), "{error}");
            assert!(marker_of_rpc(&error).contains("fewer than two"), "{error}");
        });
    }

    /// The trusted node's answer alone decides, and a silent trusted node is undetermined.
    #[test]
    fn funding_spend_of_the_trusted_node_is_taken_alone() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            show_history(&servers[0], &swap, Some(&cooperative_spend(&swap)));
            show_history(&servers[1], &swap, None);
            let trusted = servers[0].addr();
            let coin = utxo_coin(&[&servers[0], &servers[1]], Some(&trusted));
            assert!(matches!(
                search_funding(&coin, &swap).await,
                Ok(Some(FundingTxSpend::TransferredToTakerPayment(_)))
            ));
            assert_eq!(servers[1].requests("blockchain.scripthash.get_history"), 0);

            servers[0].set_behavior(StubBehavior::Down).await;
            let error = search_funding(&coin, &swap)
                .await
                .expect_err("the trusted node is silent");
            assert!(is_undetermined(marker_of_rpc(&error)), "{error}");
            assert!(marker_of_rpc(&error).contains("trusted node did not answer"), "{error}");
        });
    }

    /// A secret refund whose secret hashes to the funding's secret hash proves itself and is taken from one
    /// server; one with another secret proves nothing.
    #[test]
    fn funding_spend_by_a_matching_secret_is_taken_on_one_server() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            show_history(&servers[0], &swap, Some(&swap.spend(Some(SECRET))));
            show_history(&servers[1], &swap, None);
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            assert!(matches!(
                search_funding(&coin, &swap).await,
                Ok(Some(FundingTxSpend::RefundedSecret { .. }))
            ));

            show_history(&servers[0], &swap, Some(&swap.spend(Some([9; 32]))));
            let error = search_funding(&coin, &swap).await.expect_err("no proof");
            assert!(is_undetermined(marker_of_rpc(&error)), "{error}");
        });
    }

    /// A coin with no agreed-reads policy asks one node, and a failed read is an error, never "not spent".
    #[test]
    fn funding_spend_without_a_policy_reads_one_node_and_a_failure_is_an_error() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            show_history(&servers[0], &swap, None);
            show_history(&servers[1], &swap, None);
            let mut fields = adopted_fields(&[&servers[0], &servers[1]], None);
            fields.swap_evidence = SwapEvidencePolicy::NotAdopted;
            let coin = utxo_coin_from_fields(fields);
            assert!(matches!(search_funding(&coin, &swap).await, Ok(None)));
            servers[0].set_behavior(StubBehavior::Down).await;
            servers[1].set_behavior(StubBehavior::Down).await;
            assert!(matches!(
                search_funding(&coin, &swap).await,
                Err(SearchForFundingSpendErr::Rpc(_))
            ));
        });
    }

    /// The search for the spend of the taker payment: two servers that show the same spend decide it.
    #[test]
    fn payment_spend_two_servers_agree() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            let claim = swap.spend(Some(SECRET));
            for server in &servers {
                show_history(server, &swap, Some(&claim));
            }
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            let found = find_payment_spend(&coin, &swap, 60).await.expect("the spend is found");
            assert_eq!(txid_hex(&found), txid_hex(&claim));
        });
    }

    /// Servers that differ, one answering, and a silent trusted node end the call at once with the marker; no claim
    /// proves itself here, so one server's claim is not taken.
    #[test]
    fn payment_spend_differing_or_failing_servers_are_undetermined() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            show_history(&servers[0], &swap, Some(&swap.spend(Some(SECRET))));
            show_history(&servers[1], &swap, None);
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            let error = find_payment_spend(&coin, &swap, 60)
                .await
                .expect_err("the servers differ");
            assert!(is_undetermined(marker_of_transport(&error)), "{error}");
            assert!(marker_of_transport(&error).contains("disagree"), "{error}");

            servers[1].set_behavior(StubBehavior::Down).await;
            let error = find_payment_spend(&coin, &swap, 60).await.expect_err("one answer");
            assert!(is_undetermined(marker_of_transport(&error)), "{error}");
            assert!(marker_of_transport(&error).contains("fewer than two"), "{error}");

            let trusted = servers[0].addr();
            let coin = utxo_coin(&[&servers[0], &servers[1]], Some(&trusted));
            let found = find_payment_spend(&coin, &swap, 60)
                .await
                .expect("the trusted node answers");
            assert_eq!(found.inputs.len(), 1);
            servers[0].set_behavior(StubBehavior::Down).await;
            let error = find_payment_spend(&coin, &swap, 60)
                .await
                .expect_err("the trusted node is silent");
            assert!(
                marker_of_transport(&error).contains("trusted node did not answer"),
                "{error}"
            );
        });
    }

    /// A spend that spends the payment at another input than the first is not a spend the steps can tell the kind
    /// of: the call answers it as undetermined, in the agreed read and in the single-node read.
    #[test]
    fn a_spend_at_another_input_than_the_first_is_undetermined() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            let spend = spend_at_second_input(&swap);
            for server in &servers {
                show_history(server, &swap, Some(&spend));
            }
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            let error = find_payment_spend(&coin, &swap, 60)
                .await
                .expect_err("not at the first input");
            assert!(is_undetermined(marker_of_transport(&error)), "{error}");

            let mut fields = adopted_fields(&[&servers[0], &servers[1]], None);
            fields.swap_evidence = SwapEvidencePolicy::NotAdopted;
            let coin = utxo_coin_from_fields(fields);
            let error = find_payment_spend(&coin, &swap, 60)
                .await
                .expect_err("not at the first input");
            assert!(is_undetermined(marker_of_transport(&error)), "{error}");
        });
    }

    /// The time limit: a call whose limit has not passed asks its servers before it can answer "timed out", and
    /// answers "timed out" after a determined read that showed no spend; a limit that has passed answers it at once.
    #[test]
    fn payment_spend_asks_before_it_times_out() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            for server in &servers {
                show_history(server, &swap, None);
            }
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            let error = find_payment_spend(&coin, &swap, 1).await.expect_err("no spend in time");
            assert!(matches!(error, FindPaymentSpendError::Timeout { .. }), "{error}");
            let asked: usize = servers
                .iter()
                .map(|server| server.requests("blockchain.scripthash.get_history"))
                .sum();
            assert!(asked >= 2, "the servers were not asked before the timeout: {asked}");

            let error = find_payment_spend(&coin, &swap, -5)
                .await
                .expect_err("the limit has passed");
            assert!(matches!(error, FindPaymentSpendError::Timeout { .. }), "{error}");
        });
    }

    /// A transaction as a server shows it with `confirmations` confirmations.
    fn shown(confirmations: u32) -> Json {
        json!({"hex": "00", "txid": "00".repeat(32), "version": 1, "locktime": 0, "vin": [], "vout": [],
               "confirmations": confirmations})
    }

    async fn wait_for_confirmations(
        coin: &UtxoStandardCoin,
        swap: &Swap,
        required: u64,
        wait_secs: u64,
    ) -> Result<(), String> {
        let wait_until = common::now_ms() / 1000 + wait_secs;
        coin.wait_for_confirmations(&serialize(&swap.payment).take(), required, false, wait_until, 1)
            .compat()
            .await
    }

    /// The confirmations: two servers that both show enough take the wait; two that agree on too few leave "not
    /// confirmed" as a determined timeout, without the marker.
    #[test]
    fn confirmations_two_servers_agree() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            for server in &servers {
                server.set_result("blockchain.transaction.get", shown(3));
            }
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            wait_for_confirmations(&coin, &swap, 2, 5).await.expect("confirmed");

            let error = wait_for_confirmations(&coin, &swap, 4, 2)
                .await
                .expect_err("not confirmed in time");
            assert!(error.contains("Waited too long"), "{error}");
            assert!(!is_undetermined(&error), "{error}");
        });
    }

    /// Servers that differ, one that fails, and a silent trusted node leave the confirmation undetermined: the wait
    /// ends with the marker at its limit, never as "not confirmed". The trusted node's answer alone decides.
    #[test]
    fn confirmations_differing_or_failing_servers_are_undetermined() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            servers[0].set_result("blockchain.transaction.get", shown(3));
            servers[1].set_result("blockchain.transaction.get", shown(1));
            let coin = utxo_coin(&[&servers[0], &servers[1]], None);
            let error = wait_for_confirmations(&coin, &swap, 2, 2)
                .await
                .expect_err("the servers differ");
            assert!(is_undetermined(&error), "{error}");

            let trusted = servers[0].addr();
            let trusting_coin = utxo_coin(&[&servers[0], &servers[1]], Some(&trusted));
            wait_for_confirmations(&trusting_coin, &swap, 2, 5)
                .await
                .expect("the trusted node alone decides");

            servers[0].set_behavior(StubBehavior::Down).await;
            servers[1].set_result("blockchain.transaction.get", shown(3));
            let error = wait_for_confirmations(&coin, &swap, 2, 2)
                .await
                .expect_err("one answer");
            assert!(is_undetermined(&error), "{error}");
            let error = wait_for_confirmations(&trusting_coin, &swap, 2, 2)
                .await
                .expect_err("the trusted node is silent");
            assert!(is_undetermined(&error), "{error}");
        });
    }

    /// A coin with no agreed-reads policy keeps its single-node wait, and a wait whose last read failed to reach
    /// a server ends with the marker, not as "not confirmed".
    #[test]
    fn confirmations_without_a_policy_say_undetermined_when_the_last_read_failed() {
        block_on(async {
            let servers = start_servers(2).await;
            let swap = Swap::new();
            for server in &servers {
                server.set_result("blockchain.transaction.get", shown(3));
            }
            let mut fields = adopted_fields(&[&servers[0], &servers[1]], None);
            fields.swap_evidence = SwapEvidencePolicy::NotAdopted;
            let coin = utxo_coin_from_fields(fields);
            wait_for_confirmations(&coin, &swap, 2, 5).await.expect("confirmed");

            let error = wait_for_confirmations(&coin, &swap, 4, 2).await.expect_err("too few");
            assert!(error.contains("Waited too long"), "{error}");
            assert!(!is_undetermined(&error), "{error}");

            servers[0].set_behavior(StubBehavior::Down).await;
            servers[1].set_behavior(StubBehavior::Down).await;
            let error = wait_for_confirmations(&coin, &swap, 2, 2)
                .await
                .expect_err("no server answers");
            assert!(error.contains("Waited too long"), "{error}");
            assert!(is_undetermined(&error), "{error}");
        });
    }
}
