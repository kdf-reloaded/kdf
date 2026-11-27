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

/// The readiness check before a swap starts (maintainer decision of 2026-11-25/26 under R100) of the bchd nodes:
/// two distinct bchd hosts must answer (a validation within the window counts, otherwise one probe each); the one
/// host the single-host switch trusts must answer; a family that has not adopted R100 is ready.
#[test]
fn slp_bchd_readiness() {
    use crate::swap_evidence::{shared_readiness_probes, NOT_READY_MARKER};
    use crate::utxo::bchd_grpc::{bchd_readiness_scope, bchd_ready_with};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PLATFORM: &str = "BCHRDY";
    let scope = bchd_readiness_scope(PLATFORM);

    let two_nodes = SwapEvidencePolicy::TwoNodes {
        hosts: vec!["a".into(), "b".into()],
    };
    let urls = |list: &[&str]| list.iter().map(|url| (*url).to_owned()).collect::<Vec<String>>();
    let probes = std::sync::Arc::new(AtomicUsize::new(0));
    let probe = |answers: bool| {
        let probes = probes.clone();
        move |_: &String| {
            let probes = probes.clone();
            move |_bound: f64| {
                probes.fetch_add(1, Ordering::Relaxed);
                async move { answers }
            }
        }
    };
    // One attempt per series: a failed probe is a failed host at once (the retries have their own tests).
    let run = crate::swap_evidence::ReadyRun {
        mode: crate::swap_evidence::ReadyMode::Strict,
        settings: crate::swap_evidence::ReadinessSettings {
            attempts: 1,
            ..Default::default()
        },
    };

    // Both hosts served a validation just now: ready with no probe.
    let both = urls(&["https://rdy-bchd1.example", "https://rdy-bchd2.example"]);
    for url in &both {
        shared_readiness_probes().note_answer(&scope, &crate::swap_evidence::node_name(url), common::now_float());
    }
    assert_eq!(
        block_on(bchd_ready_with(
            "FUSD",
            PLATFORM,
            &two_nodes,
            &both,
            false,
            run,
            probe(false)
        )),
        Ok(())
    );
    assert_eq!(probes.load(Ordering::Relaxed), 0, "nothing extra is sent");

    // One host answered, the other fails its probe: not ready.
    let one = urls(&["https://rdy-bchd3.example", "https://rdy-bchd4.example"]);
    shared_readiness_probes().note_answer(&scope, &crate::swap_evidence::node_name(&one[0]), common::now_float());
    let error = block_on(bchd_ready_with(
        "FUSD",
        PLATFORM,
        &two_nodes,
        &one,
        false,
        run,
        probe(false),
    ))
    .unwrap_err();
    assert_eq!(
        error,
        "FUSD: swap evidence not available (fewer than two hosts answering)"
    );
    assert!(error.contains(NOT_READY_MARKER) && !error.contains("example"));
    assert_eq!(
        probes.load(Ordering::Relaxed),
        1,
        "one probe, of the host not known to answer"
    );

    // The single-host switch: the one configured bchd host must answer.
    let single = urls(&["https://rdy-bchd5.example:8335"]);
    assert_eq!(
        block_on(bchd_ready_with(
            "FUSD",
            PLATFORM,
            &two_nodes,
            &single,
            true,
            run,
            probe(true)
        )),
        Ok(())
    );
    let silent = urls(&["https://rdy-bchd6.example:8335"]);
    assert_eq!(
        block_on(bchd_ready_with(
            "FUSD",
            PLATFORM,
            &two_nodes,
            &silent,
            true,
            run,
            probe(false)
        )),
        Err("FUSD: swap evidence not available (the trusted node is not answering)".to_owned())
    );
    // Without the switch one host is too few: the token's policy is `Insufficient` then; a family that has not
    // adopted R100 is ready.
    let one_host = crate::utxo::bch::slp_policy_over(&two_nodes, &silent, false);
    assert_eq!(one_host, SwapEvidencePolicy::Insufficient { hosts: 1 });
    assert_eq!(
        block_on(bchd_ready_with(
            "FUSD",
            PLATFORM,
            &one_host,
            &silent,
            false,
            run,
            probe(true)
        )),
        Err("FUSD: swap evidence not available (fewer than two nodes listed)".to_owned())
    );
    // Another platform keeps its own record of the same host.
    let before = probes.load(Ordering::Relaxed);
    assert_eq!(
        block_on(bchd_ready_with(
            "FUSD",
            "BCHRDY2",
            &two_nodes,
            &single,
            true,
            run,
            probe(true)
        )),
        Ok(())
    );
    assert_eq!(
        probes.load(Ordering::Relaxed),
        before + 1,
        "the answer recorded under another platform does not count"
    );
    assert_eq!(
        block_on(bchd_ready_with(
            "FUSD",
            PLATFORM,
            &SwapEvidencePolicy::NotAdopted,
            &silent,
            false,
            run,
            probe(false)
        )),
        Ok(())
    );
}

/// An SLP token is ready only while its BCH platform's Electrum servers are: with the platform down to one
/// answering host, the token is not ready, and the text names the token.
#[test]
fn slp_token_readiness_follows_its_platform() {
    use crate::swap_evidence::shared_readiness_probes;
    use crate::utxo::rpc_clients::electrum_stub_server::StubBehavior;

    block_on(async {
        let servers = start_servers(2).await;
        let bchd = ["https://plat-bchd1.example", "https://plat-bchd2.example"];
        let token = slp_token(adopted_fields(&[&servers[0], &servers[1]], None), &bchd);
        let scope = crate::utxo::bchd_grpc::bchd_readiness_scope(token.platform_coin.ticker());
        for url in bchd {
            shared_readiness_probes().note_answer(&scope, &crate::swap_evidence::node_name(url), common::now_float());
        }
        assert_eq!(token.swap_evidence_ready().await, Ok(()));
        servers[1].set_behavior(StubBehavior::Down).await;
        match &token.platform_coin.as_ref().rpc_client {
            UtxoRpcClientEnum::Electrum(client) => client.forget_readiness_answers(),
            _ => panic!("an Electrum platform"),
        }
        assert_eq!(
            token.swap_evidence_ready().await,
            Err("FUSD: swap evidence not available (fewer than two hosts answering)".to_owned())
        );
    });
}

/// The bchd readiness probe has a real bound: a node that accepts the connection and never answers ends the probe
/// within [`crate::swap_evidence::READY_PROBE_TIMEOUT_SECS`], and the connection is closed then.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn slp_bchd_readiness_probe_is_bounded_on_a_silent_node() {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf); // the request, never answered
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream.read(&mut buf).map_err(|e| e.kind())
    });

    let started = Instant::now();
    let answered = block_on(crate::utxo::bchd_grpc::bchd_readiness_probe(
        &format!("http://127.0.0.1:{port}"),
        crate::swap_evidence::READY_PROBE_TIMEOUT_SECS,
    ));
    let elapsed = started.elapsed();
    assert!(!answered);
    assert!(
        elapsed < Duration::from_secs_f64(crate::swap_evidence::READY_PROBE_TIMEOUT_SECS + 1.),
        "the probe ended after {elapsed:?}"
    );
    assert_eq!(peer.join().unwrap(), Ok(0), "the node sees its connection closed");
}
