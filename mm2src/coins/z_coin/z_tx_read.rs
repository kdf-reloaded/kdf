//! Reading a ZHTLC coin's swap-path transactions through the Zcash library reader
//! (CRD ch.39 R39.11.16, R39.11.16a).
//!
//! The shared UTXO parser applies the version-4 field layout to any overwintered transaction of
//! version 4 or above and derives a double-SHA256 identifier, so a version-5 or version-6 transaction
//! is mis-parsed and given a wrong identifier by construction. Every swap-path read of a ZHTLC coin
//! therefore goes through this module, which holds the only parse of those bytes: the Zcash reader
//! accepts Sprout, 3, 4, 5 and 6 and derives each version's identifier by that version's own rule.
//!
//! Nothing here is reached by a non-ZHTLC coin: the functions take a [`ZCoin`] or a Zcash transaction.
//! The decisions made on the parsed data are the decisions the shared helpers make, and where a shared
//! helper's rule has a part that does not depend on the parser (secret extraction, the spend search,
//! the wait loop) that part is the shared one, called with the facts this module read.

use super::z_lightwalletd::{DetectionLog, FundingFound, HtlcIndexError, LightwalletdClient, OutputRef, SentOutcome,
                            SpendOutcome};
use super::ZCoin;
use crate::utxo::rpc_clients::{UtxoRpcClientEnum, UtxoRpcClientOps};
use crate::utxo::utxo_common::{self, classify_swap_spend, ensure_payment_pays_htlc, payment_script, PaymentOutputs,
                               SwapSpendKind, DEFAULT_SWAP_VOUT};
use crate::utxo::{sat_from_big_decimal, UtxoCommonOps};
use crate::{FoundSwapTxSpend, HtlcPaymentCheck, TransactionEnum, TransactionFut, ValidatePaymentInput};
use common::executor::Timer;
use common::log;
use futures::compat::Future01CompatExt;
use futures::{FutureExt, TryFutureExt};
use futures01::Future;
use kdf_crypto::dhash160;
use keys::hash::H256;
use keys::Public;
use primitives::bytes::Bytes;
use rpc::v1::types::H256 as H256Json;
use script::{Builder, Script};
use zcash_primitives::transaction::Transaction as ZTransaction;
use zcash_protocol::consensus::BranchId;

/// Reads `bytes` with the Zcash library reader.
///
/// The branch-identifier argument only matters for the pre-version-5 layouts; versions 5 and 6 carry
/// their own, so one fixed value reads every version this coin's network accepts.
pub(crate) fn read_tx(bytes: &[u8]) -> Result<ZTransaction, String> {
    ZTransaction::read(bytes, BranchId::Sapling).map_err(|e| format!("cannot read the transaction: {}", e))
}

/// The transaction identifier in internal byte order, the order `find_output_spend` takes.
pub(crate) fn txid_internal(tx: &ZTransaction) -> H256 { H256::from(*tx.txid().as_ref()) }

/// The transaction identifier in display order, the order the caller-facing surface uses (R39.8.7a).
pub(crate) fn txid_display(tx: &ZTransaction) -> H256Json { txid_internal(tx).reversed().into() }

/// The script-pubkey of transparent output `index`, or an error naming the missing output.
fn script_pubkey_at(tx: &ZTransaction, index: usize) -> Result<Vec<u8>, String> {
    tx.transparent_bundle()
        .and_then(|bundle| bundle.vout.get(index))
        .map(|out| out.script_pubkey().0 .0.clone())
        .ok_or_else(|| format!("the transaction has no transparent output {}", index))
}

fn sat_at(tx: &ZTransaction, index: usize) -> Option<u64> {
    tx.transparent_bundle()
        .and_then(|bundle| bundle.vout.get(index))
        .map(|out| u64::from(out.value()))
}

/// The input scripts of `tx`'s transparent bundle (none if it has none).
fn script_sigs(tx: &ZTransaction) -> Vec<Script> {
    tx.transparent_bundle()
        .map(|bundle| {
            bundle
                .vin
                .iter()
                .map(|input| Script::from(input.script_sig().0 .0.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The output check of the shared payment validator, over the Zcash reader's view: output 0 must pay
/// the expected P2SH script-pubkey exactly `amount` satoshis.
fn check_payment_output(tx: &ZTransaction, expected_script_pubkey: &[u8], amount: u64) -> Result<(), String> {
    let actual_script = script_pubkey_at(tx, DEFAULT_SWAP_VOUT);
    let actual_value = sat_at(tx, DEFAULT_SWAP_VOUT);
    match (actual_script, actual_value) {
        (Ok(script), Some(value)) if value == amount && script == expected_script_pubkey => Ok(()),
        (Ok(script), Some(value)) => {
            let kind = if script != expected_script_pubkey {
                "script mismatch"
            } else {
                "amount mismatch"
            };
            ERR!(
                "Provided payment tx output {}: actual value {} script {:?}, expected value {} script {:?}",
                kind,
                value,
                script,
                amount,
                expected_script_pubkey
            )
        },
        _ => ERR!("Provided payment tx output missing output"),
    }
}

/// `validate_maker_payment` / `validate_taker_payment` for a ZHTLC coin: the same P2SH script-pubkey
/// check against the same expected redeem script, the same output value check, and a byte-for-byte
/// comparison of the supplied payment with the bytes the backend returns (R39.11.16).
///
/// There is no SPV step: the shared validator performs none for a node-served or lightwalletd-served
/// coin (no block headers are parsed for either), so none is dropped here.
pub(crate) fn validate_payment(
    coin: &ZCoin,
    input: ValidatePaymentInput,
    first_pub: Vec<u8>,
    second_pub: Vec<u8>,
) -> Box<dyn Future<Item = (), Error = String> + Send> {
    let first_pub = try_fus!(Public::from_slice(&first_pub));
    let second_pub = try_fus!(Public::from_slice(&second_pub));
    let tx = try_fus!(read_tx(&input.payment_tx));
    let amount = try_fus!(sat_from_big_decimal(&input.amount, coin.as_ref().decimals));
    let expected_redeem = payment_script(input.time_lock, &input.secret_hash, &first_pub, &second_pub);
    let expected_script_pubkey: Bytes = Builder::build_p2sh(&dhash160(&expected_redeem).into()).into();
    let coin = coin.clone();
    let payment_bytes = input.payment_tx;
    let txid = txid_display(&tx);

    let fut = async move {
        let mut attempts = 0;
        let tx_from_rpc = loop {
            match coin.as_ref().rpc_client.get_transaction_bytes(&txid).compat().await {
                Ok(t) => break t,
                Err(e) => {
                    if attempts > 2 {
                        return ERR!("Got error {:?} after 3 attempts of getting tx {:?} from RPC", e, txid);
                    };
                    attempts += 1;
                    log::info!("Error {:?} getting the tx {:?} from rpc", e, txid);
                    Timer::sleep(10.).await;
                },
            }
        };
        if payment_bytes != tx_from_rpc.0 {
            return ERR!(
                "Provided payment tx {:?} doesn't match tx data from rpc {:?}",
                txid,
                tx_from_rpc
            );
        }

        check_payment_output(&tx, expected_script_pubkey.as_slice(), amount)
    };
    Box::new(fut.boxed().compat())
}

/// `wait_for_confirmations` for a ZHTLC coin.
pub(crate) fn wait_for_confirmations(
    coin: &ZCoin,
    tx: &[u8],
    confirmations: u64,
    requires_nota: bool,
    wait_until: u64,
    check_every: u64,
) -> Box<dyn Future<Item = (), Error = String> + Send> {
    let tx = try_fus!(read_tx(tx));
    coin.as_ref().rpc_client.wait_for_confirmations(
        txid_display(&tx),
        u32::from(tx.expiry_height()),
        confirmations as u32,
        requires_nota,
        wait_until,
        check_every,
    )
}

/// Turns an undetermined detection into the message the caller sees, and records the named conditions
/// on the sync-error channel (R39.8.0au, R39.11.17): they leave a swap decision short of an answer
/// while nothing has strictly failed.
fn report_undetermined(coin: &ZCoin, error: HtlcIndexError) -> String {
    let message = error.to_string();
    if error.is_named_condition() {
        coin.set_shielded_sync_error(&message);
    }
    message
}

/// The funding output of a swap payment, for the lightwalletd detection.
fn watched_output(tx: &ZTransaction, vout: usize) -> Result<OutputRef, String> {
    Ok(OutputRef {
        txid: *tx.txid().as_ref(),
        vout: vout as u32,
        script_pubkey: script_pubkey_at(tx, vout)?,
    })
}

/// Looks for the spend of `output` over lightwalletd and returns it as the Zcash transaction it is.
/// `None` is the index's own positive statement that the output is unspent; anything it could not
/// determine is an error. `polling` is set by the wait loop, whose detections repeat every few seconds
/// and so log at `debug` (R39.11.4h(d)).
async fn find_spend_over_lightwalletd(
    coin: &ZCoin,
    client: &LightwalletdClient,
    output: &OutputRef,
    from_block: u64,
    polling: bool,
) -> Result<Option<(ZTransaction, usize)>, String> {
    let log = DetectionLog {
        coin: &coin.as_ref().conf.ticker,
        polling,
    };
    match client.0.detect_output_spend(output, from_block, &log).await {
        Ok(SpendOutcome::NotSpent(_)) => Ok(None),
        Ok(SpendOutcome::Spent(spend)) => Ok(Some((read_tx(&spend.tx)?, spend.input_index))),
        Err(e) => Err(report_undetermined(coin, e)),
    }
}

/// `wait_for_tx_spend` for a ZHTLC coin.
pub(crate) fn wait_for_tx_spend(coin: &ZCoin, tx_bytes: &[u8], from_block: u64, wait_until: u64) -> TransactionFut {
    let tx = try_tx_fus!(read_tx(tx_bytes));
    if let UtxoRpcClientEnum::LightWalletD(client) = &coin.as_ref().rpc_client {
        let output = try_tx_fus!(watched_output(&tx, DEFAULT_SWAP_VOUT));
        let (coin, client, tx_debug) = (coin.clone(), client.clone(), format!("{:?}", tx));
        let fut = async move {
            loop {
                match find_spend_over_lightwalletd(&coin, &client, &output, from_block, true).await {
                    Ok(Some((spend, _))) => return Ok(TransactionEnum::from(spend)),
                    Ok(None) => (),
                    // Undetermined is not a spend and not an answer: keep waiting, say why.
                    Err(e) => log::error!("HTLC spend detection of tx {} could not tell: {}", tx_debug, e),
                }
                if common::now_ms() / 1000 > wait_until {
                    return TX_PLAIN_ERR!(
                        "Waited too long until {} for transaction {} {} to be spent ",
                        wait_until,
                        tx_debug,
                        DEFAULT_SWAP_VOUT,
                    );
                }
                Timer::sleep(10.).await;
            }
        };
        return Box::new(fut.boxed().compat());
    }
    let script_pubkey = try_tx_fus!(script_pubkey_at(&tx, DEFAULT_SWAP_VOUT));
    utxo_common::wait_for_output_spend_of(
        coin.as_ref(),
        txid_internal(&tx),
        script_pubkey.into(),
        format!("{:?}", tx),
        DEFAULT_SWAP_VOUT,
        from_block,
        wait_until,
    )
}

/// What the HTLC detection in flight for the payment script built from these parameters read last, or
/// `None` when there is none, the keys are not keys, or the coin is not served over lightwalletd.
pub(crate) fn htlc_detection_progress(
    coin: &ZCoin,
    time_lock: u32,
    first_pub: &[u8],
    second_pub: &[u8],
    secret_hash: &[u8],
) -> Option<String> {
    let UtxoRpcClientEnum::LightWalletD(client) = &coin.as_ref().rpc_client else {
        return None;
    };
    let (first, second) = (
        Public::from_slice(first_pub).ok()?,
        Public::from_slice(second_pub).ok()?,
    );
    let hash: [u8; 20] = dhash160(&payment_script(time_lock, secret_hash, &first, &second))
        .as_slice()
        .try_into()
        .ok()?;
    client.0.detection_progress(&hash)
}

/// `check_payment_pays_htlc` for a ZHTLC coin: the payment is read with the Zcash reader and checked by the
/// one shared check, [`ensure_payment_pays_htlc`], whose only failure is the script mismatch.
pub(crate) fn check_payment_pays_htlc(
    time_lock: u32,
    first_pub: &Public,
    second_pub: &Public,
    secret_hash: &[u8],
    payment_tx: &[u8],
) -> Result<HtlcPaymentCheck, String> {
    let tx = read_tx(payment_tx)?;
    let payment = PaymentOutputs {
        tx_hash: txid_internal(&tx),
        output0_script_pubkey: script_pubkey_at(&tx, 0)?.into(),
        spent_script_pubkey: script_pubkey_at(&tx, DEFAULT_SWAP_VOUT)?.into(),
    };
    Ok(
        match ensure_payment_pays_htlc(time_lock, first_pub, second_pub, secret_hash, &payment) {
            Ok(()) => HtlcPaymentCheck::Pays,
            Err(_) => HtlcPaymentCheck::PaysAnotherScript {
                payment_id: format!("{:?}", payment.tx_hash.reversed()),
            },
        },
    )
}

/// `extract_secret` for a ZHTLC coin: the shared extraction rule over the spend's input scripts.
pub(crate) fn extract_secret(secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
    let spend_tx = read_tx(spend_tx)?;
    utxo_common::extract_secret_from_script_sigs(secret_hash, script_sigs(&spend_tx))
}

/// `search_for_swap_tx_spend_my` / `_other` for a ZHTLC coin.
///
/// The payment is read with the Zcash reader and checked against the HTLC script exactly as the shared
/// search does. Over lightwalletd the search is the dual-encoding index probe (R39.11.4d) and the spend
/// is classified by the shared rule; elsewhere the search itself is the shared one.
pub(crate) async fn search_for_swap_tx_spend(
    coin: &ZCoin,
    time_lock: u32,
    first_pub: &Public,
    second_pub: &Public,
    secret_hash: &[u8],
    tx: &[u8],
    search_from_block: u64,
) -> Result<Option<FoundSwapTxSpend>, String> {
    let tx = read_tx(tx)?;
    let output0_script_pubkey = script_pubkey_at(&tx, 0)?;
    let spent_script_pubkey = script_pubkey_at(&tx, DEFAULT_SWAP_VOUT)?;
    let payment = PaymentOutputs {
        tx_hash: txid_internal(&tx),
        output0_script_pubkey: output0_script_pubkey.into(),
        spent_script_pubkey: spent_script_pubkey.into(),
    };
    if let UtxoRpcClientEnum::LightWalletD(client) = &coin.as_ref().rpc_client {
        ensure_payment_pays_htlc(time_lock, first_pub, second_pub, secret_hash, &payment)?;
        let output = watched_output(&tx, DEFAULT_SWAP_VOUT)?;
        return match find_spend_over_lightwalletd(coin, client, &output, search_from_block, false).await? {
            None => Ok(None),
            Some((spend, input_index)) => classify_spend(spend, input_index).map(Some),
        };
    }
    utxo_common::search_for_swap_output_spend_of(
        coin.as_ref(),
        time_lock,
        first_pub,
        second_pub,
        secret_hash,
        payment,
        DEFAULT_SWAP_VOUT,
        search_from_block,
    )
    .await
}

/// Classifies a found spend by the script of the input that spends the HTLC output, by the shared rule.
fn classify_spend(spend: ZTransaction, input_index: usize) -> Result<FoundSwapTxSpend, String> {
    let script = script_sigs(&spend)
        .into_iter()
        .nth(input_index)
        .ok_or_else(|| format!("the spending transaction has no transparent input {}", input_index))?;
    match classify_swap_spend(&script) {
        Some(SwapSpendKind::Spent) => Ok(FoundSwapTxSpend::Spent(spend.into())),
        Some(SwapSpendKind::Refunded) => Ok(FoundSwapTxSpend::Refunded(spend.into())),
        None => ERR!(
            "Couldn't find required instruction in script_sig of input {} of tx {:?}",
            input_index,
            spend.txid()
        ),
    }
}

/// The wallet's own send record as an answer to the already-sent question (R39.11.26).
///
/// A recorded transaction counts when it has a transparent output of exactly `script_pubkey` and it
/// is mined, or unmined and not past its expiry height judged against the fresh tip read
/// (R39.11.10a, the tip `wait_for_confirmations` uses). An expiry height of zero never expires.
/// The tip is read only when no mined record counts, so a mined record is answered with no request.
///
/// `None` means "no counting record", never "not sent": a database that cannot be read, a tip that
/// cannot be read and an expired record all leave the question to the servers.
async fn recorded_own_send(coin: &ZCoin, client: &LightwalletdClient, script_pubkey: &[u8]) -> Option<ZTransaction> {
    let ticker = &coin.as_ref().conf.ticker;
    let records = match coin
        .z_fields
        .shielded_history
        .recorded_transactions_containing(script_pubkey)
    {
        Ok(records) => records,
        Err(e) => {
            log::warn!(
                "ZCoin {}: the wallet database's send record could not be read ({}); asking the servers",
                ticker,
                e
            );
            return None;
        },
    };
    let mut unmined = Vec::new();
    for record in records {
        let Ok(tx) = read_tx(&record.raw) else { continue };
        let pays_htlc = tx.transparent_bundle().is_some_and(|bundle| {
            bundle
                .vout
                .iter()
                .any(|out| out.script_pubkey().0 .0.as_slice() == script_pubkey)
        });
        if !pays_htlc {
            continue;
        }
        match record.mined_height {
            Some(height) => {
                log::info!(
                    "ZCoin {}: the wallet's own send record answers the already-sent question: payment {} \
                     (mined at height {}); no server asked",
                    ticker,
                    hex::encode(txid_display(&tx).0),
                    height
                );
                return Some(tx);
            },
            None => unmined.push(tx),
        }
    }
    if unmined.is_empty() {
        return None;
    }
    let tip = match client.0.confirmation_tip().await {
        Ok(tip) => tip,
        Err(e) => {
            log::warn!(
                "ZCoin {}: the tip could not be read to judge the expiry of the recorded send ({}); \
                 asking the servers",
                ticker,
                e
            );
            return None;
        },
    };
    let tx = unmined.into_iter().find(|tx| {
        let expiry = u64::from(u32::from(tx.expiry_height()));
        expiry == 0 || tip <= expiry
    })?;
    log::info!(
        "ZCoin {}: the wallet's own send record answers the already-sent question: payment {} \
         (unmined, within its expiry height {} at tip {}); no address query made",
        ticker,
        hex::encode(txid_display(&tx).0),
        u32::from(tx.expiry_height()),
        tip
    );
    Some(tx)
}

/// `check_if_my_payment_sent` for a ZHTLC coin. Yields the Zcash-transaction arm.
pub(crate) fn check_if_my_payment_sent(
    coin: ZCoin,
    time_lock: u32,
    my_pub: &[u8],
    other_pub: &[u8],
    secret_hash: &[u8],
    search_from_block: u64,
) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
    let my_public = try_fus!(Public::from_slice(my_pub));
    let script = payment_script(
        time_lock,
        secret_hash,
        &my_public,
        &try_fus!(Public::from_slice(other_pub)),
    );
    let hash = dhash160(&script);
    let fut = async move {
        match &coin.as_ref().rpc_client {
            // Over lightwalletd the answer comes from the dual-encoding index probe. "Not sent" is only
            // returned when a healthy endpoint answered every read over a gap-free window; anything
            // less is an error, because an empty answer here would double-fund the payment.
            UtxoRpcClientEnum::LightWalletD(client) => {
                let script_pubkey = Builder::build_p2sh(&hash.into()).to_bytes();
                // The wallet's own record of what it sent answers "sent" before any server is asked
                // (R39.11.26); it never answers "not sent".
                if let Some(tx) = recorded_own_send(&coin, client, &script_pubkey).await {
                    return Ok(Some(TransactionEnum::from(tx)));
                }
                let log = DetectionLog {
                    coin: &coin.as_ref().conf.ticker,
                    polling: false,
                };
                let outcome = client
                    .0
                    .detect_funding(&script_pubkey, search_from_block, &log)
                    .await
                    .map_err(|e| report_undetermined(&coin, e))?;
                match outcome {
                    SentOutcome::NotSent => Ok(None),
                    SentOutcome::Sent(FundingFound::Transaction { tx, .. }) => {
                        Ok(Some(TransactionEnum::from(read_tx(&tx)?)))
                    },
                    SentOutcome::Sent(FundingFound::Listed { txid }) => {
                        let raw = client.0.transaction(txid).await.map_err(|e| {
                            format!("the payment was found but its transaction could not be fetched: {}", e)
                        })?;
                        Ok(Some(TransactionEnum::from(read_tx(&raw.data)?)))
                    },
                }
            },
            UtxoRpcClientEnum::Native(client) => {
                let target_addr = keys::Address {
                    t_addr_prefix: coin.as_ref().conf.p2sh_t_addr_prefix,
                    prefix: coin.as_ref().conf.p2sh_addr_prefix,
                    hash: hash.into(),
                    checksum_type: coin.as_ref().conf.checksum_type,
                    hrp: coin.as_ref().conf.bech32_hrp.clone(),
                    addr_format: coin.addr_format().clone(),
                };
                let target_addr = target_addr.to_string();
                let is_imported = try_s!(client.is_address_imported(&target_addr).await);
                if !is_imported {
                    return Ok(None);
                }
                let received_by_addr = try_s!(client.list_received_by_address(0, true, true).compat().await);
                for item in received_by_addr {
                    if item.address == target_addr && !item.txids.is_empty() {
                        let tx_bytes = try_s!(client.get_transaction_bytes(&item.txids[0]).compat().await);
                        let tx = read_tx(tx_bytes.0.as_slice())?;
                        return Ok(Some(tx.into()));
                    }
                }
                Ok(None)
            },
            UtxoRpcClientEnum::Electrum(_) => ERR!("a ZHTLC coin is never served by an Electrum client"),
        }
    };
    Box::new(fut.boxed().compat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::z_coin::z_lightwalletd::txid_display_to_internal;
    use crate::z_coin::z_tx_read_fixtures::{V4_HEIGHT_176_HEX, V6_HEIGHT_68370_HEX};
    use chain::Transaction as UtxoTx;
    use kdf_crypto::dhash256;
    use serialization::deserialize;
    use zcash_primitives::transaction::TxVersion;

    /// Internal-order identifier of the version-4 recording (height 176).
    const V4_TXID_INTERNAL: &str = "c4b5c4ebc84166dcecd6e682586d1fb0af8db05ddd948269b03f4adb4d5a8f81";
    /// Internal-order identifier of the version-6 recording (height 68370).
    const V6_TXID_INTERNAL: &str = "1b05636c5c796730b8a87588c27b2f93c00e792412ab0ac3e124c69902088098";

    fn bytes_of(hex_str: &str) -> Vec<u8> { hex::decode(hex_str).expect("fixture is valid hex") }

    fn compact(len: usize) -> Vec<u8> {
        assert!(len < 0xfd);
        vec![len as u8]
    }

    /// A hand-assembled transparent-only transaction, so a test can choose the output script, value
    /// and input script. `v5` selects the ZIP-225 layout, otherwise version 4.
    fn transparent_tx(v5: bool, out_script: &[u8], out_value: u64, in_script: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        if v5 {
            b.extend_from_slice(&0x8000_0005u32.to_le_bytes());
            b.extend_from_slice(&0x26A7_270Au32.to_le_bytes());
            b.extend_from_slice(&0xC2D6_D0B4u32.to_le_bytes()); // NU5 branch id
            b.extend_from_slice(&0u32.to_le_bytes()); // lock time
            b.extend_from_slice(&1_000_000u32.to_le_bytes()); // expiry height
        } else {
            b.extend_from_slice(&0x8000_0004u32.to_le_bytes());
            b.extend_from_slice(&0x892F_2085u32.to_le_bytes());
        }
        b.push(1); // one input
        b.extend_from_slice(&[0x11; 32]);
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&compact(in_script.len()));
        b.extend_from_slice(in_script);
        b.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
        b.push(1); // one output
        b.extend_from_slice(&out_value.to_le_bytes());
        b.extend_from_slice(&compact(out_script.len()));
        b.extend_from_slice(out_script);
        if v5 {
            b.extend_from_slice(&[0, 0, 0]); // no Sapling spends, no Sapling outputs, no Orchard actions
        } else {
            b.extend_from_slice(&0u32.to_le_bytes()); // lock time
            b.extend_from_slice(&1_000_000u32.to_le_bytes()); // expiry height
            b.extend_from_slice(&0i64.to_le_bytes()); // value balance
            b.extend_from_slice(&[0, 0, 0]); // no Sapling spends, outputs or joinsplits
        }
        b
    }

    fn p2sh_script(redeem: &[u8]) -> Vec<u8> {
        let mut s = vec![0xa9, 0x14];
        s.extend_from_slice(dhash160(redeem).as_slice());
        s.push(0x87);
        s
    }

    /// An HTLC-spend-shaped input script: `<sig> <secret> OP_0 <redeem>`.
    fn spend_script_sig(secret: &[u8; 32], redeem: &[u8]) -> Vec<u8> {
        let mut s = vec![0x01, 0xaa, 0x20];
        s.extend_from_slice(secret);
        s.push(0x00);
        s.push(redeem.len() as u8);
        s.extend_from_slice(redeem);
        s
    }

    /// T39.11.e, the defect. For a recorded version-6 transaction the shared parser has no identifier to
    /// offer at all -- it refuses the bytes, because it applies the version-4 field layout to them --
    /// and the one identifier rule it owns, double-SHA256 of the bytes, is not the version-6 identifier.
    /// The Zcash reader reads the bytes and returns exactly the chain's identifier.
    ///
    /// Were the shared parser ever to accept such bytes (an "extension"), it would label them with the
    /// double-SHA256 digest asserted unequal below; the assertion that it refuses them today is the pin
    /// that must be revisited, not relaxed, if that happens (R39.11.16a: it shall not be extended).
    #[test]
    fn a_v6_transaction_has_no_txid_from_the_shared_parser_and_the_right_one_from_the_zcash_reader() {
        let raw = bytes_of(V6_HEIGHT_68370_HEX);

        let routed = read_tx(&raw).expect("the Zcash reader reads version 6");
        assert_eq!(routed.version(), TxVersion::V6);
        assert_eq!(hex::encode(txid_internal(&routed).as_slice()), V6_TXID_INTERNAL);
        assert_ne!(
            txid_internal(&routed),
            dhash256(&raw),
            "a version-6 identifier is the ZIP-244 digest, not a double-SHA256 of the bytes"
        );

        let shared = deserialize::<_, UtxoTx>(raw.as_slice());
        assert!(shared.is_err(), "the shared parser cannot read a version-6 transaction");

        // The shared parser's identifier rule, on a transaction it does read, is the double-SHA256 of the
        // bytes -- the rule this transaction's real identifier disagrees with.
        let v4_raw = bytes_of(V4_HEIGHT_176_HEX);
        let v4: UtxoTx = deserialize(v4_raw.as_slice()).expect("the shared parser reads version 4");
        assert_eq!(v4.hash(), dhash256(&v4_raw));
    }

    /// T39.11.e, R39.11.16a(a): version 4 is still read, with the identifier, the transparent view and
    /// the bytes the shared parser produces for it.
    #[test]
    fn a_v4_transaction_reads_identically_through_the_routed_and_the_shared_parser() {
        let raw = bytes_of(V4_HEIGHT_176_HEX);
        let routed = read_tx(&raw).expect("the Zcash reader reads version 4");
        assert_eq!(routed.version(), TxVersion::V4);
        assert_eq!(hex::encode(txid_internal(&routed).as_slice()), V4_TXID_INTERNAL);

        let shared: UtxoTx = deserialize(raw.as_slice()).expect("the shared parser reads version 4");
        assert_eq!(txid_internal(&routed), shared.hash());
        assert_eq!(u32::from(routed.expiry_height()), shared.expiry_height);
        let bundle = routed.transparent_bundle().expect("recording has a transparent bundle");
        assert_eq!(bundle.vin.len(), shared.inputs.len());
        assert_eq!(bundle.vout.len(), shared.outputs.len());
        for (z, u) in bundle.vout.iter().zip(shared.outputs.iter()) {
            assert_eq!(u64::from(z.value()), u.value);
            assert_eq!(z.script_pubkey().0 .0, u.script_pubkey.to_vec());
        }
        let mut written = Vec::new();
        routed.write(&mut written).expect("writing succeeds");
        assert_eq!(written, raw, "round trip");
    }

    /// R39.11.16a(c): the three byte-order boundaries, on an identifier that is not byte-order
    /// symmetric. Internal order is what `find_output_spend` and the lightwalletd lookup take; display
    /// order is the caller-facing form; the conversion between them is the one reversal.
    #[test]
    fn the_identifier_byte_orders_are_pinned_at_each_boundary() {
        let routed = read_tx(&bytes_of(V6_HEIGHT_68370_HEX)).expect("readable");
        let internal = txid_internal(&routed);
        let display = txid_display(&routed);
        assert_eq!(hex::encode(internal.as_slice()), V6_TXID_INTERNAL);
        let mut reversed = internal.take();
        reversed.reverse();
        assert_ne!(
            reversed,
            internal.take(),
            "the fixture's identifier is not byte-order symmetric"
        );
        assert_eq!(display.0, reversed, "display order is the reversal of internal order");
        assert_eq!(
            txid_display_to_internal(&display),
            internal.take(),
            "lookup takes internal order"
        );
        assert_eq!(display, H256Json::from(internal.take()).reversed());
    }

    /// T39.11.e: for every version the old parser supported (here 4), each step's decision is the same
    /// under the routed and the shared parser; for version 5 the routed parser reads what the shared
    /// one cannot.
    #[test]
    fn the_payment_output_decision_is_the_same_under_both_parsers_for_v4_and_the_routed_one_reads_v5() {
        let redeem = b"a stand-in redeem script".to_vec();
        let expected = p2sh_script(&redeem);
        let amount = 123_456u64;
        let secret = [7u8; 32];

        for v5 in [false, true] {
            let raw = transparent_tx(v5, &expected, amount, &spend_script_sig(&secret, &redeem));
            let routed = read_tx(&raw).expect("routed reader accepts the transaction");
            assert_eq!(routed.version(), if v5 { TxVersion::V5 } else { TxVersion::V4 });
            assert_eq!(check_payment_output(&routed, &expected, amount), Ok(()));
            assert!(check_payment_output(&routed, &expected, amount + 1)
                .unwrap_err()
                .contains("amount mismatch"));
            assert!(check_payment_output(&routed, &p2sh_script(b"another"), amount)
                .unwrap_err()
                .contains("script mismatch"));

            let secret_hash = dhash160(&secret);
            let routed_secret = extract_secret(secret_hash.as_slice(), &raw);
            assert_eq!(routed_secret, Ok(secret.to_vec()), "v5={v5}");
            assert!(extract_secret(dhash160(b"wrong").as_slice(), &raw).is_err());

            let shared: Result<UtxoTx, _> = deserialize(raw.as_slice());
            if v5 {
                // Version 5 is outside what the shared parser lays out.
                assert!(shared.is_err(), "the shared parser cannot read version 5");
                assert_ne!(txid_internal(&routed), dhash256(&raw), "ZIP-244, not double-SHA256");
            } else {
                let shared = shared.expect("the shared parser reads version 4");
                assert_eq!(shared.hash(), txid_internal(&routed));
                assert_eq!(shared.outputs[0].script_pubkey.to_vec(), expected);
                assert_eq!(shared.outputs[0].value, amount);
                assert_eq!(
                    utxo_common::extract_secret(secret_hash.as_slice(), &raw),
                    routed_secret,
                    "secret extraction decides the same under both parsers"
                );
            }
        }
    }

    /// The wait step takes the expiry height from the routed reader.
    #[test]
    fn the_expiry_height_comes_from_the_routed_reader_for_both_versions() {
        for v5 in [false, true] {
            let raw = transparent_tx(v5, &p2sh_script(b"r"), 1, &[0x00]);
            assert_eq!(
                u32::from(read_tx(&raw).expect("readable").expiry_height()),
                1_000_000,
                "v5={v5}"
            );
        }
    }

    /// A payment with no transparent output 0 is refused, not defaulted.
    #[test]
    fn a_payment_without_a_transparent_output_is_refused() {
        let routed = read_tx(&bytes_of(V6_HEIGHT_68370_HEX)).expect("readable");
        assert!(check_payment_output(&routed, &p2sh_script(b"r"), 1).is_err());
        assert!(script_pubkey_at(&routed, 0).is_err());
    }

    /// R39.11.16a(b), T39.11.e: no swap step of a ZHTLC coin reaches the shared UTXO parser. The
    /// swap-operations file and the HTLC builder must name none of the shared parser-bound helpers
    /// and must not call the shared deserializer; the market-coin operations in `z_coin.rs` that read
    /// transaction bytes are checked on the lines that make them.
    #[test]
    fn no_swap_step_of_a_zhtlc_coin_reaches_the_shared_utxo_parser() {
        let swap_ops = include_str!("z_swap_ops.rs");
        let htlc = include_str!("z_htlc.rs");
        let coin = include_str!("../z_coin.rs");
        let shared_helpers = [
            "utxo_common::validate_maker_payment",
            "utxo_common::validate_taker_payment",
            "utxo_common::check_if_my_payment_sent",
            "utxo_common::search_for_swap_tx_spend",
            "utxo_common::extract_secret",
            "utxo_common::wait_for_confirmations",
            "utxo_common::wait_for_output_spend(",
            "utxo_common::tx_enum_from_bytes",
        ];
        for source in [swap_ops, htlc, coin] {
            for helper in shared_helpers {
                // The wasm build keeps its own stub, which is a separate file and not built here.
                let lines: Vec<&str> = source.lines().collect();
                for (n, line) in lines.iter().enumerate() {
                    if line.contains(helper) && !line.trim_start().starts_with("//") {
                        let wasm_gated = lines[n.saturating_sub(3)..n]
                            .iter()
                            .any(|l| l.contains("target_arch = \"wasm32\"") && !l.contains("not(target_arch"));
                        assert!(wasm_gated, "line {} reaches {}", n + 1, helper);
                    }
                }
            }
        }
        for source in [swap_ops, htlc] {
            assert!(
                !source
                    .lines()
                    .any(|l| l.contains("deserialize(") && !l.trim_start().starts_with("//")),
                "the swap path must not call the shared deserializer"
            );
            assert!(
                !source
                    .lines()
                    .any(|l| l.contains("UtxoTx") && !l.trim_start().starts_with("//")),
                "the swap path must not name the shared UTXO transaction type"
            );
        }
    }

    /// A live ARRR (pre-Ironwood, version 4) taker payment that was refunded on chain, and its refund; public
    /// chain data. The payment's HTLC: lock time 1790664056, refund key = the taker's, spend key = the
    /// maker's, secret hash160 `cb03f4f7...`.
    const LIVE_PAYMENT_HEX: &str = include_str!("test-fixtures/arrr-htlc-payment-75566e4d.hex");
    const LIVE_REFUND_HEX: &str = include_str!("test-fixtures/arrr-htlc-refund-0496f6d3.hex");
    const LIVE_PAYMENT_TXID_DISPLAY: &str = "75566e4d322874d79db2661d334fa404e449df15723b482957815fef203cf1e2";
    const LIVE_PAYMENT_TXID_INTERNAL: &str = "e2f13c20ef5f815729483b7215df49e404a44f331d66b29dd77428324d6e5675";
    const LIVE_TIME_LOCK: u32 = 1_790_664_056;
    const LIVE_TAKER_KEY: &str = "03cd3144d880ce80ce9f2f4c24feb12d011bd6f067cdb44dcd2c41438923d64fe2";
    const LIVE_MAKER_KEY: &str = "02ccd091588f2acb3b5320dc53950f7b00fff8dfae39286d745e96bed48f49c580";
    const LIVE_SECRET_HASH: &str = "cb03f4f75479eb6731672f14069d05e1a08d8349";

    fn live_key(hex_key: &str) -> Public { Public::from_slice(&bytes_of(hex_key)).expect("a valid public key") }

    fn live_payment_outputs() -> PaymentOutputs {
        let tx = read_tx(&bytes_of(LIVE_PAYMENT_HEX.trim())).expect("the live payment is readable");
        PaymentOutputs {
            tx_hash: txid_internal(&tx),
            output0_script_pubkey: script_pubkey_at(&tx, 0).expect("output 0").into(),
            spent_script_pubkey: script_pubkey_at(&tx, DEFAULT_SWAP_VOUT).expect("output 0").into(),
        }
    }

    /// T39.5.4: with the swap's own key pair the live payment pays the rebuilt HTLC script.
    #[test]
    fn ensure_payment_pays_htlc_accepts_live_arrr_payment_with_swap_key() {
        let payment = live_payment_outputs();
        assert_eq!(hex::encode(payment.tx_hash.as_slice()), LIVE_PAYMENT_TXID_INTERNAL);
        // The taker's payment: the taker holds the refund key, the maker the spend key.
        let verdict = ensure_payment_pays_htlc(
            LIVE_TIME_LOCK,
            &live_key(LIVE_TAKER_KEY),
            &live_key(LIVE_MAKER_KEY),
            &bytes_of(LIVE_SECRET_HASH),
            &payment,
        );
        assert_eq!(verdict, Ok(()));
    }

    /// T39.5.4: a key that is not the swap's (here: any other, such as a persistent key) is the hard stop,
    /// and the message names the payment by its display-order transaction id.
    #[test]
    fn ensure_payment_pays_htlc_rejects_with_persistent_key_and_prints_display_txid() {
        let payment = live_payment_outputs();
        let persistent = live_key("02d74dc5ec4c823f40dae5c563d7b22aab52c80f9f18226f47ea6d83107618df62");
        let err = ensure_payment_pays_htlc(
            LIVE_TIME_LOCK,
            &persistent,
            &live_key(LIVE_MAKER_KEY),
            &bytes_of(LIVE_SECRET_HASH),
            &payment,
        )
        .unwrap_err();
        assert!(
            err.contains("does not pay the HTLC script this swap negotiated"),
            "{err}"
        );
        assert!(
            err.contains(LIVE_PAYMENT_TXID_DISPLAY),
            "display-order txid missing: {err}"
        );
        assert!(
            !err.contains(LIVE_PAYMENT_TXID_INTERNAL),
            "byte-reversed txid printed: {err}"
        );
    }

    /// R39.5.4: the key choice asks `check_payment_pays_htlc`. The live payment, built with the swap's own
    /// key pair, is paid by the swap key and is another script for any other key; the answer names the
    /// payment in display order.
    #[test]
    fn check_payment_pays_htlc_matches_the_swap_key_and_names_the_payment_otherwise() {
        let payment = bytes_of(LIVE_PAYMENT_HEX.trim());
        let secret_hash = bytes_of(LIVE_SECRET_HASH);
        let (taker, maker) = (live_key(LIVE_TAKER_KEY), live_key(LIVE_MAKER_KEY));
        assert_eq!(
            check_payment_pays_htlc(LIVE_TIME_LOCK, &taker, &maker, &secret_hash, &payment),
            Ok(HtlcPaymentCheck::Pays)
        );
        let persistent = live_key("02d74dc5ec4c823f40dae5c563d7b22aab52c80f9f18226f47ea6d83107618df62");
        assert_eq!(
            check_payment_pays_htlc(LIVE_TIME_LOCK, &persistent, &maker, &secret_hash, &payment),
            Ok(HtlcPaymentCheck::PaysAnotherScript {
                payment_id: LIVE_PAYMENT_TXID_DISPLAY.to_owned()
            })
        );
        assert!(check_payment_pays_htlc(LIVE_TIME_LOCK, &taker, &maker, &secret_hash, &[0x01, 0x02]).is_err());
    }

    /// R39.5.4, the fallback: a payment built with the persistent key is paid by the persistent key and is
    /// another script for the swap's per-swap key, so a recovery that tries the swap key first and the
    /// persistent key next lands on the persistent key.
    #[test]
    fn a_payment_built_with_the_persistent_key_matches_only_the_persistent_key() {
        let (persistent, per_swap, maker) = (
            live_key("02d74dc5ec4c823f40dae5c563d7b22aab52c80f9f18226f47ea6d83107618df62"),
            live_key(LIVE_TAKER_KEY),
            live_key(LIVE_MAKER_KEY),
        );
        let secret_hash = bytes_of(LIVE_SECRET_HASH);
        let redeem = payment_script(LIVE_TIME_LOCK, &secret_hash, &persistent, &maker);
        let raw = transparent_tx(false, &p2sh_script(&redeem), 100_000, &[0x00]);

        assert!(matches!(
            check_payment_pays_htlc(LIVE_TIME_LOCK, &per_swap, &maker, &secret_hash, &raw),
            Ok(HtlcPaymentCheck::PaysAnotherScript { .. })
        ));
        assert_eq!(
            check_payment_pays_htlc(LIVE_TIME_LOCK, &persistent, &maker, &secret_hash, &raw),
            Ok(HtlcPaymentCheck::Pays)
        );
        // The counterparty's payment: this node's key is the spend key.
        let redeem = payment_script(LIVE_TIME_LOCK, &secret_hash, &maker, &persistent);
        let raw = transparent_tx(false, &p2sh_script(&redeem), 100_000, &[0x00]);
        assert_eq!(
            check_payment_pays_htlc(LIVE_TIME_LOCK, &maker, &persistent, &secret_hash, &raw),
            Ok(HtlcPaymentCheck::Pays)
        );
    }

    /// T39.5.4: the live refund's input script classifies as a refund, and it spends the live payment.
    #[test]
    fn classify_live_arrr_refund_as_refunded() {
        let refund = read_tx(&bytes_of(LIVE_REFUND_HEX.trim())).expect("the live refund is readable");
        let scripts = script_sigs(&refund);
        assert_eq!(scripts.len(), 1);
        assert_eq!(classify_swap_spend(&scripts[0]), Some(SwapSpendKind::Refunded));
        let bundle = refund.transparent_bundle().expect("transparent input");
        assert_eq!(
            &bundle.vin[0].prevout().hash()[..],
            txid_internal(&read_tx(&bytes_of(LIVE_PAYMENT_HEX.trim())).unwrap()).as_slice(),
            "the refund spends the live payment"
        );
    }

    /// Opt-in corpus check (`ARRR_IRONWOOD_FIXTURES` pointing at `local/arrr-ironwood-fixtures`, which is
    /// untracked third-party data): every recorded transaction is read by the routed reader with the
    /// chain's identifier, and the shared parser refuses every version-6 one.
    #[test]
    fn the_offline_corpus_is_read_by_the_routed_reader_and_refused_by_the_shared_one() {
        let Some(dir) = std::env::var_os("ARRR_IRONWOOD_FIXTURES") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("tx_index.json")).expect("index")).expect("json");
        let (mut v4, mut v6) = (0usize, 0usize);
        for entry in index.as_array().expect("array") {
            let txid_internal_hex = entry["txid_internal"].as_str().expect("txid");
            let height = entry["height"].as_u64().expect("height");
            let file = std::fs::read_dir(dir.join("raw"))
                .expect("raw dir")
                .map(|e| e.expect("entry").file_name().into_string().expect("utf8"))
                .find(|n| n.starts_with(&format!("h{height}_")) && n.contains(&txid_internal_hex[..12]))
                .expect("raw file");
            let raw = bytes_of(
                std::fs::read_to_string(dir.join("raw").join(file))
                    .expect("read")
                    .trim(),
            );
            let routed = read_tx(&raw).expect("routed reader reads the recording");
            assert_eq!(
                hex::encode(txid_internal(&routed).as_slice()),
                txid_internal_hex,
                "height {height}"
            );
            match routed.version() {
                TxVersion::V4 => v4 += 1,
                TxVersion::V6 => {
                    v6 += 1;
                    assert!(
                        deserialize::<_, UtxoTx>(raw.as_slice()).is_err(),
                        "height {height}: the shared parser cannot read version 6"
                    );
                    assert_ne!(
                        txid_internal_hex,
                        hex::encode(dhash256(&raw).as_slice()),
                        "height {height}"
                    );
                },
                other => panic!("height {height}: unexpected version {other:?}"),
            }
        }
        assert_eq!((v4, v6), (1, 18));
    }
}
