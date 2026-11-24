//! CRD ch.51 §51.6G (R100, T42, T43) for SLP tokens: the platform reads behind a refund or a claim (the refund
//! gate, the wait for the spend) are judged on two Electrum servers of the BCH platform, under SLP's own policy.
//! An SLP coin with fewer than two bchd hosts is `Insufficient` and refused at admission. Offline, over scripted
//! Electrum servers on distinct loopback addresses.

use super::*;
use crate::swap_evidence::{family_adopted, is_undetermined, SwapEvidencePolicy, SwapFamily};
use crate::utxo::bch::bch_coin_over_fields_and_bchd_for_test;
use crate::utxo::rpc_clients::electrum_stub_server::StubBehavior;
use crate::utxo::utxo_swap_evidence_tests::{adopted_fields, show_chain, show_history, start_servers, txid_hex, Swap,
                                            SECRET, TIME_LOCK};
use crate::{CanRefundHtlc, MarketCoinOps, SwapOps};
use common::block_on;
use serialization::serialize;

const TWO_BCHD: [&str; 2] = ["https://bchd1.example", "https://bchd2.example"];

fn slp_token(fields: UtxoCoinFields, bchd: &[&str]) -> SlpToken {
    SlpToken::new(
        4,
        "FUSD".into(),
        H256::default(),
        bch_coin_over_fields_and_bchd_for_test(fields, bchd),
        0,
    )
}

async fn can_refund(token: &SlpToken) -> Result<bool, String> {
    token
        .can_refund_htlc(TIME_LOCK as u64)
        .compat()
        .await
        .map(|gate| matches!(gate, CanRefundHtlc::CanRefundNow))
}

#[test]
fn slp_is_adopted_and_answers_the_policy_over_two_bchd_hosts() {
    block_on(async {
        assert!(family_adopted(SwapFamily::Slp));
        let servers = start_servers(2).await;
        let two = slp_token(adopted_fields(&[&servers[0], &servers[1]], None), &TWO_BCHD);
        assert!(matches!(
            two.swap_evidence_policy(),
            SwapEvidencePolicy::TwoNodes { .. }
        ));
        assert_eq!(two.swap_evidence_refusal(), None);
    });
}

#[test]
fn slp_with_fewer_than_two_bchd_hosts_is_refused_at_admission() {
    block_on(async {
        let servers = start_servers(2).await;
        let fields = || adopted_fields(&[&servers[0], &servers[1]], None);
        // One host, and two entries of one host, are one node.
        for bchd in [vec!["https://bchd1.example"], vec![
            "https://bchd1.example:8335",
            "http://BCHD1.example:8336",
        ]] {
            let token = slp_token(fields(), &bchd);
            assert_eq!(token.swap_evidence_policy(), SwapEvidencePolicy::Insufficient {
                hosts: 1
            });
            let refusal = token.swap_evidence_refusal().expect("refused");
            assert!(refusal.contains("swap_evidence_node"), "{refusal}");
        }
        let none = slp_token(fields(), &[]);
        assert_eq!(none.swap_evidence_policy(), SwapEvidencePolicy::Insufficient {
            hosts: 0
        });
        assert!(none.swap_evidence_refusal().is_some());
    });
}

#[test]
fn slp_with_one_electrum_host_is_refused_even_with_two_bchd_hosts() {
    block_on(async {
        let servers = start_servers(1).await;
        let token = slp_token(adopted_fields(&[&servers[0]], None), &TWO_BCHD);
        assert!(matches!(
            token.swap_evidence_policy(),
            SwapEvidencePolicy::Insufficient { .. }
        ));
        assert!(token.swap_evidence_refusal().is_some());
    });
}

/// The refund gate waits while the platform's median time past, agreed on two servers, is below the lock time.
#[test]
fn slp_refund_gate_waits_while_the_agreed_mtp_is_below_the_lock_time() {
    block_on(async {
        let servers = start_servers(2).await;
        show_chain(&servers[0], 30, TIME_LOCK - 10);
        show_chain(&servers[1], 30, TIME_LOCK - 10);
        let token = slp_token(adopted_fields(&[&servers[0], &servers[1]], None), &TWO_BCHD);
        assert_eq!(can_refund(&token).await, Ok(false));
    });
}

#[test]
fn slp_refund_gate_two_electrum_hosts_agree() {
    block_on(async {
        let servers = start_servers(2).await;
        show_chain(&servers[0], 30, TIME_LOCK + 5_000);
        show_chain(&servers[1], 30, TIME_LOCK + 90_000);
        let token = slp_token(adopted_fields(&[&servers[0], &servers[1]], None), &TWO_BCHD);
        assert_eq!(can_refund(&token).await, Ok(true));
    });
}

#[test]
fn slp_refund_gate_disagreement_is_undetermined() {
    block_on(async {
        let servers = start_servers(2).await;
        show_chain(&servers[0], 30, TIME_LOCK + 5_000);
        show_chain(&servers[1], 30, TIME_LOCK - 5_000);
        let token = slp_token(adopted_fields(&[&servers[0], &servers[1]], None), &TWO_BCHD);
        let error = can_refund(&token).await.expect_err("the servers differ");
        assert!(is_undetermined(&error), "{error}");
        assert!(error.contains("disagree"), "{error}");
        assert!(!error.contains("://"), "{error}");
    });
}

#[test]
fn slp_refund_gate_one_answering_server_is_undetermined() {
    block_on(async {
        let servers = start_servers(2).await;
        show_chain(&servers[0], 30, TIME_LOCK + 5_000);
        show_chain(&servers[1], 30, TIME_LOCK + 5_000);
        let token = slp_token(adopted_fields(&[&servers[0], &servers[1]], None), &TWO_BCHD);
        servers[1].set_behavior(StubBehavior::Down).await;
        let error = can_refund(&token).await.expect_err("one answer");
        assert!(is_undetermined(&error), "{error}");
        assert!(error.contains("fewer than two"), "{error}");
    });
}

#[test]
fn slp_refund_gate_below_two_bchd_hosts_never_opens() {
    block_on(async {
        let servers = start_servers(2).await;
        show_chain(&servers[0], 30, TIME_LOCK + 5_000);
        show_chain(&servers[1], 30, TIME_LOCK + 5_000);
        // Both servers agree, but the coin is `Insufficient`: the read is undetermined and the gate stays shut.
        let token = slp_token(adopted_fields(&[&servers[0], &servers[1]], None), &[
            "https://bchd1.example",
        ]);
        let error = can_refund(&token).await.expect_err("insufficient");
        assert!(is_undetermined(&error), "{error}");
    });
}

#[test]
fn slp_wait_for_spend_takes_an_agreed_claim() {
    block_on(async {
        let servers = start_servers(2).await;
        // The payment of an SLP swap: the OP_RETURN at output 0 and the HTLC at output 1; the claim spends output 1.
        let swap = Swap::slp_shaped();
        let claim = swap.spend(Some(SECRET));
        show_history(&servers[0], &swap, Some(&claim));
        show_history(&servers[1], &swap, Some(&claim));
        let token = slp_token(adopted_fields(&[&servers[0], &servers[1]], None), &TWO_BCHD);
        let wait_until = common::now_ms() / 1000 + 60;
        let taken = token
            .wait_for_tx_spend(&serialize(&swap.payment).take(), wait_until, 0, &None)
            .compat()
            .await
            .expect("the claim is taken");
        assert_eq!(
            txid_hex(&crate::utxo::utxo_common::read_utxo_tx(&taken.tx_hex()).expect("a utxo tx")),
            txid_hex(&claim)
        );
    });
}
