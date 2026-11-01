//! Opt-in live measurement against the ARRR test chain: does Pirate's transparent address index
//! resolve a funded HTLC under the P2SH `b...` form, under the R-address form of the same
//! twenty bytes, or under neither?
//!
//! The test broadcasts a real transaction and spends real (test-chain) coins, so it is inert
//! unless `ARRR_TESTCHAIN_LIVE_DIR` names an instance directory holding `MM2.json` (the
//! `passphrase` is read from it) and `coins` (the ARRR entry of the test chain). The wallet
//! database is kept in `<dir>/db-live-probe`, never in the instance's own `dbdir`. Without the
//! variable the test returns immediately, so CI can never broadcast.
//!
//! Results are printed (`--nocapture`). "Found" is reported separately from "failed": an
//! empty success and a transport or service error are different answers.
//!
//! A second test, [`live_spend_detection_of_a_redeemed_htlc`], funds a fresh HTLC, redeems it in the
//! same run (where the secret is in hand) and asserts that the production detection -- the routed
//! `check_if_my_payment_sent` and `search_for_swap_tx_spend` -- finds the payment, reports the unspent
//! output as unspent, finds the redeem, and that `extract_secret` recovers the secret from it. It
//! broadcasts two transactions.

use super::z_htlc::z_p2sh_spend;
use super::z_lightwalletd::{GrpcTransport, LightwalletdTransport};
use super::z_tx_read;
use super::{z_coin_from_conf_and_params, z_send_htlc, ZCoin, ZcoinBackend, ZcoinProtocolInfo};
use crate::lp_coins_traits::MarketCoinOps;
use crate::utxo::utxo_common::payment_script;
use crate::utxo::{UtxoActivationParams, UtxoAddressFormat};
use crate::{FoundSwapTxSpend, TransactionEnum};
use chain::constants::SEQUENCE_FINAL;
use common::executor::Timer;
use common::{block_on, now_ms};
use crypto::privkey::key_pair_from_secret;
use crypto::CryptoCtx;
use futures::compat::Future01CompatExt;
use kdf_crypto::dhash160;
use keys::Address;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use rpc::v1::types::H256 as H256Json;
use script::{Builder as ScriptBuilder, Opcode};
use serde_json::{json, Value as Json};

const LIVE_DIR_VAR: &str = "ARRR_TESTCHAIN_LIVE_DIR";
const START_HEIGHT: u64 = 175;

fn describe<T>(r: &Result<T, String>, ok: impl Fn(&T) -> String) -> String {
    match r {
        Ok(v) => format!("OK     {}", ok(v)),
        Err(e) => format!("FAILED {}", e),
    }
}

async fn probe(
    transport: &GrpcTransport,
    server: &str,
    label: &str,
    address: &str,
    from: u64,
    to: u64,
    txid_hex: &str,
) {
    println!("--- {label}: {address}");
    let txids = transport.taddr_txids(server, address, from, to).await;
    match &txids.error {
        Some(e) => println!(
            "  GetTaddressTxids    FAILED after {} tx(s): {} (serve_failure={})",
            txids.txs.len(),
            e.message,
            e.serve_failure
        ),
        None => println!("  GetTaddressTxids    OK     {} tx(s) returned", txids.txs.len()),
    }
    // A returned transaction is the HTLC when its raw bytes hash to the txid; compare by length of
    // evidence only here, the txid check is done on the transaction bytes below.
    let found_in_txids = txids.txs.iter().any(|t| {
        zcash_primitives::transaction::Transaction::read(&t.data[..], zcash_protocol::consensus::BranchId::Sapling)
            .map(|tx| hex::encode(tx.txid().as_ref()) == txid_hex)
            .unwrap_or(false)
    });
    println!("  GetTaddressTxids    contains the HTLC txid: {found_in_txids}");

    let utxos = transport
        .address_utxos(server, address)
        .await
        .map_err(|e| e.to_string());
    println!(
        "  GetAddressUtxos     {}",
        describe(&utxos, |u| format!(
            "{} utxo(s) {:?}",
            u.len(),
            u.iter()
                .map(|x| (hex::encode(x.txid), x.index, x.value_zat, x.height))
                .collect::<Vec<_>>()
        ))
    );
    let balance = transport
        .taddr_balance(server, vec![address.to_owned()])
        .await
        .map_err(|e| e.to_string());
    println!("  GetTaddressBalance  {}", describe(&balance, |b| format!("{b} zat")));
}

/// Builds the live coin from the instance directory, or `None` when the variable is unset.
fn live_coin() -> Option<(MmArc, ZCoin, Vec<String>)> {
    let dir = std::env::var_os(LIVE_DIR_VAR)?;
    let dir = std::path::PathBuf::from(dir);
    let mm2: Json = serde_json::from_slice(&std::fs::read(dir.join("MM2.json")).unwrap()).unwrap();
    let passphrase = mm2["passphrase"].as_str().unwrap().to_owned();
    let coins: Json = serde_json::from_slice(&std::fs::read(dir.join("coins")).unwrap()).unwrap();
    let mut conf = coins
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["coin"] == "ARRR")
        .cloned()
        .unwrap();
    // The chain is past its Ironwood activation, which this build gates by default; the gate is
    // lifted so the build reaches the network and the network's own answer is what is measured.
    conf["protocol"]["protocol_data"]["consensus_params"]["ironwood_gate_disabled"] = json!(true);
    let servers: Vec<String> = conf["light_wallet_d_servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect();
    let mut protocol_data = conf["protocol"]["protocol_data"].clone();
    // Same shape the activation layer feeds the builder.
    let protocol_info: ZcoinProtocolInfo = serde_json::from_value(protocol_data.take()).unwrap();

    let db_root = dir.join("db-live-probe");
    std::fs::create_dir_all(&db_root).unwrap();
    let ctx = MmCtxBuilder::default()
        .with_conf(json!({ "dbdir": db_root.to_str().unwrap(), "netid": mm2["netid"].clone() }))
        .into_mm_arc();
    let crypto_ctx = CryptoCtx::init_with_iguana_passphrase(ctx.clone(), &passphrase).unwrap();
    let priv_key = crypto_ctx.mm2_internal_privkey_secret();
    let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable", "coin": "ARRR" })).unwrap();

    let coin: ZCoin = block_on(z_coin_from_conf_and_params(
        &ctx,
        "ARRR",
        &conf,
        &params,
        priv_key.as_slice(),
        0,
        None,
        protocol_info,
        ZcoinBackend::Light {
            light_wallet_d_servers: servers.clone(),
            electrum_servers: Vec::new(),
        },
    ))
    .unwrap();
    // The context is returned with the coin: the coin does not keep it alive on its own.
    Some((ctx, coin, servers))
}

#[test]
fn live_probe_of_the_transparent_index_for_a_funded_htlc() {
    let Some((_ctx, coin, servers)) = live_coin() else {
        return;
    };
    println!("address: {}", coin.my_z_address_encoded());

    block_on(async {
        coin.admitted_lightwalletd_servers().await.unwrap();
        while !coin.is_sapling_state_synced() {
            assert!(!coin.sapling_loop_stopped(), "the commitment-tree sync loop died");
            Timer::sleep(1.).await;
        }
        let tip = coin.current_block().compat().await.unwrap();
        println!("tip at start: {tip}");
        coin.fetch_lightwalletd_compact_blocks_to_height(tip, Some(START_HEIGHT), false, &|_, _| {})
            .await
            .unwrap();
        coin.scan_shielded_wallet_db_to_height(tip, |_, _| {}).unwrap();
        println!("balance: {:?}", coin.my_balance().compat().await.unwrap());

        // The HTLC.
        let my_pub = *coin.utxo_arc.priv_key_policy.key_pair_or_err().unwrap().public();
        let other_pub = *key_pair_from_secret(&[3; 32]).unwrap().public();
        let secret: [u8; 32] = rand::random();
        let secret_hash = dhash160(&secret);
        let time_lock = (now_ms() / 1000) as u32 + 2 * 24 * 3600;
        let script = payment_script(time_lock, secret_hash.as_slice(), &my_pub, &other_pub);
        let hash160 = dhash160(&script);
        println!("payment script dhash160 (hex): {}", hex::encode(hash160.as_slice()));

        let tx = z_send_htlc(
            &coin,
            time_lock,
            &my_pub,
            &other_pub,
            secret_hash.as_slice(),
            "0.001".parse().unwrap(),
        )
        .await
        .unwrap();
        let txid_internal: [u8; 32] = *tx.txid().as_ref();
        let txid_hex = hex::encode(txid_internal);
        let txid_display = H256Json::from(txid_internal).reversed();
        println!("HTLC txid (display order): {}", hex::encode(txid_display.0));
        println!("HTLC txid (internal order): {txid_hex}");

        // Wait for it to be mined.
        let mined_height = loop {
            match coin.raw_transaction_with_height(&txid_display).await {
                Ok((_, Some(h))) => break h,
                Ok((_, None)) => println!("not mined yet"),
                Err(e) => println!("transaction not retrievable yet: {e}"),
            }
            Timer::sleep(20.).await;
        };
        println!("mined at height {mined_height}");
        // Let the index and both endpoints catch up past the block.
        Timer::sleep(30.).await;
        let tip = coin.current_block().compat().await.unwrap();

        let p2sh = Address {
            prefix: coin.utxo_arc.conf.p2sh_addr_prefix,
            t_addr_prefix: coin.utxo_arc.conf.p2sh_t_addr_prefix,
            hash: hash160.into(),
            checksum_type: coin.utxo_arc.conf.checksum_type,
            addr_format: UtxoAddressFormat::Standard,
            hrp: None,
        };
        let r_addr = Address {
            prefix: 60,
            t_addr_prefix: 0,
            hash: hash160.into(),
            checksum_type: coin.utxo_arc.conf.checksum_type,
            addr_format: UtxoAddressFormat::Standard,
            hrp: None,
        };
        println!("p2sh prefix {} t-prefix {}", p2sh.prefix, p2sh.t_addr_prefix);
        let transport = GrpcTransport::default();
        for server in &servers {
            println!("===== endpoint {server} (tip {tip})");
            probe(
                &transport,
                server,
                "P2SH form",
                &p2sh.to_string(),
                START_HEIGHT,
                tip,
                &txid_hex,
            )
            .await;
            probe(
                &transport,
                server,
                "R-address form",
                &r_addr.to_string(),
                START_HEIGHT,
                tip,
                &txid_hex,
            )
            .await;
        }
    });
}

/// The tip, once the wallet has been brought up to it. Needed before a second send, so the first
/// one's change is spendable.
async fn sync_wallet(coin: &ZCoin, from: Option<u64>) {
    let tip = coin.current_block().compat().await.unwrap();
    coin.fetch_lightwalletd_compact_blocks_to_height(tip, from, false, &|_, _| {})
        .await
        .unwrap();
    coin.scan_shielded_wallet_db_to_height(tip, |_, _| {}).unwrap();
}

async fn wait_mined(coin: &ZCoin, txid_display: &H256Json) -> u64 {
    loop {
        match coin.raw_transaction_with_height(txid_display).await {
            Ok((_, Some(h))) => return h,
            Ok((_, None)) => println!("not mined yet"),
            Err(e) => println!("transaction not retrievable yet: {e}"),
        }
        Timer::sleep(20.).await;
    }
}

/// Detection of a *spend*: funds a fresh HTLC, checks the routed `check_if_my_payment_sent` and
/// `search_for_swap_tx_spend` before and after it exists, redeems it with the secret that is in hand,
/// and polls the routed search until it reports the redeem. Every undetermined answer along the way is
/// printed; none of them may be taken for "not spent" and the final assertions fail if one was.
#[test]
fn live_spend_detection_of_a_redeemed_htlc() {
    let Some((_ctx, coin, _servers)) = live_coin() else {
        return;
    };
    block_on(async {
        coin.admitted_lightwalletd_servers().await.unwrap();
        while !coin.is_sapling_state_synced() {
            assert!(!coin.sapling_loop_stopped(), "the commitment-tree sync loop died");
            Timer::sleep(1.).await;
        }
        sync_wallet(&coin, Some(START_HEIGHT)).await;
        println!("balance: {:?}", coin.my_balance().compat().await.unwrap());

        let my_key_pair = *coin.utxo_arc.priv_key_policy.key_pair_or_err().unwrap();
        let my_pub = *my_key_pair.public();
        // The redeemer: the secret path of the script is signed by the second key.
        let redeemer = key_pair_from_secret(&[3; 32]).unwrap();
        let other_pub = *redeemer.public();
        let secret: [u8; 32] = rand::random();
        let secret_hash = dhash160(&secret);
        let time_lock = (now_ms() / 1000) as u32 + 2 * 24 * 3600;
        let redeem_script = payment_script(time_lock, secret_hash.as_slice(), &my_pub, &other_pub);
        println!(
            "redeem script hash160: {}",
            hex::encode(dhash160(&redeem_script).as_slice())
        );

        // Before anything exists: a healthy index must say "not sent", and not fail.
        let start = coin.current_block().compat().await.unwrap();
        let before = z_tx_read::check_if_my_payment_sent(
            coin.clone(),
            time_lock,
            &*my_pub,
            &*other_pub,
            secret_hash.as_slice(),
            start,
        )
        .compat()
        .await;
        println!(
            "check_if_my_payment_sent before sending: {:?}",
            before.as_ref().map(|t| t.is_some())
        );
        println!("sync-error channel: {:?}", coin.shielded_sync_error());
        assert!(
            matches!(before, Ok(None)),
            "expected a clean \"not sent\", got {:?}",
            before
        );

        let tx = z_send_htlc(
            &coin,
            time_lock,
            &my_pub,
            &other_pub,
            secret_hash.as_slice(),
            "0.001".parse().unwrap(),
        )
        .await
        .unwrap();
        let txid_internal: [u8; 32] = *tx.txid().as_ref();
        let txid_display = H256Json::from(txid_internal).reversed();
        println!("HTLC txid (display order): {}", hex::encode(txid_display.0));
        let mut tx_bytes = Vec::new();
        tx.write(&mut tx_bytes).unwrap();
        let funded_at = wait_mined(&coin, &txid_display).await;
        println!("funded at height {funded_at}");
        Timer::sleep(30.).await;

        let sent = z_tx_read::check_if_my_payment_sent(
            coin.clone(),
            time_lock,
            &*my_pub,
            &*other_pub,
            secret_hash.as_slice(),
            start,
        )
        .compat()
        .await;
        println!(
            "check_if_my_payment_sent after funding: {:?}",
            sent.as_ref().map(|t| t.as_ref().map(|t| t.tx_hash()))
        );
        match sent {
            Ok(Some(TransactionEnum::ZTransaction(found))) => assert_eq!(*found.txid().as_ref(), txid_internal),
            other => panic!("the funded HTLC was not found as sent: {:?}", other),
        }

        let unspent = z_tx_read::search_for_swap_tx_spend(
            &coin,
            time_lock,
            &my_pub,
            &other_pub,
            secret_hash.as_slice(),
            &tx_bytes,
            funded_at,
        )
        .await;
        println!(
            "search_for_swap_tx_spend while unspent: {:?}",
            unspent.as_ref().map(|s| s.is_some())
        );
        assert!(
            matches!(unspent, Ok(None)),
            "an unspent HTLC must read as unspent: {:?}",
            unspent
        );

        // Redeem it, with the secret.
        let script_data = ScriptBuilder::default()
            .push_data(&secret)
            .push_opcode(Opcode::OP_0)
            .into_script();
        sync_wallet(&coin, None).await;
        let spend = z_p2sh_spend(
            &coin,
            tx,
            time_lock,
            SEQUENCE_FINAL,
            redeem_script,
            script_data,
            redeemer.private().secret.as_slice(),
        )
        .await
        .unwrap();
        let mut spend_bytes = Vec::new();
        spend.write(&mut spend_bytes).unwrap();
        println!("redeem txid (internal order): {}", hex::encode(spend.txid().as_ref()));

        // Poll the routed search until it reports the redeem. A node without its transaction index
        // answers with the gap condition for as long as the redeem is the thing it cannot serve; that
        // is printed, and it fails the test at the deadline rather than passing as "not spent".
        let deadline = now_ms() / 1000 + 15 * 60;
        let found = loop {
            let result = z_tx_read::search_for_swap_tx_spend(
                &coin,
                time_lock,
                &my_pub,
                &other_pub,
                secret_hash.as_slice(),
                &tx_bytes,
                funded_at,
            )
            .await;
            match result {
                Ok(Some(found)) => break found,
                Ok(None) => println!("search says the HTLC is still unspent"),
                Err(e) => println!("search could not tell: {e}"),
            }
            println!("sync-error channel: {:?}", coin.shielded_sync_error());
            assert!(
                now_ms() / 1000 < deadline,
                "the redeem was not detected within 15 minutes"
            );
            Timer::sleep(20.).await;
        };
        let FoundSwapTxSpend::Spent(found_tx) = found else {
            panic!("the redeem was classified as a refund");
        };
        assert_eq!(found_tx.tx_hex(), spend_bytes, "the detected spend is the redeem");
        let recovered = z_tx_read::extract_secret(secret_hash.as_slice(), &found_tx.tx_hex()).unwrap();
        assert_eq!(
            recovered,
            secret.to_vec(),
            "extract_secret recovers the secret from the detected spend"
        );
        println!("detected the redeem and recovered the secret");
    });
}
