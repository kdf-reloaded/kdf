//! TRON broadcast of a signed transaction for the generic wallet path.
//!
//! `MarketCoinOps::send_raw_tx` / `send_raw_tx_bytes` of a TRX or TRC20 coin submit the hex of the signed protobuf
//! transaction to `/wallet/broadcasthex` (CRD ch.21 §21.9) instead of the EVM provider. The returned string is the
//! transaction id in the form the EVM path returns: lowercase hex, no `0x` prefix.

use super::api::TronApiClient;

/// Broadcasts the signed protobuf transaction `tx` (raw bytes) through `api` and returns its transaction id,
/// lowercase hex without a `0x` prefix.
///
/// The node must answer `result: true` and name a 32-byte `txid`: an answer without the success flag, or
/// without a usable `txid`, is an error and never an empty or invented id. A rejection (`result: false`) is
/// reported with the node's `code` and `message`. No node URL or API key appears in an error: the client
/// names nodes host-only and scrubs lower-layer texts (CRD ch.21 R-N1).
///
/// # Errors
///
/// A text error when `api` is `None` (the coin carries no TRON node client), when `tx` is empty, when no node
/// can be reached or answers with a usable body, when the node rejects the transaction, or when the accepted
/// answer carries no valid `txid`.
pub(crate) async fn broadcast_raw_tx(api: Option<TronApiClient>, tx: Vec<u8>) -> Result<String, String> {
    let api = api.ok_or_else(|| "TRON broadcast failed: TRON API client missing on this coin".to_owned())?;
    if tx.is_empty() {
        return Err("TRON broadcast failed: empty transaction".to_owned());
    }
    let resp = api
        .broadcast_hex(&hex::encode(&tx))
        .await
        .map_err(|e| format!("TRON broadcast failed: {e}"))?;
    if resp.result != Some(true) {
        return Err("TRON broadcast failed: the node answer carries no success flag".to_owned());
    }
    let txid = resp
        .txid
        .ok_or_else(|| "TRON broadcast failed: the node accepted the transaction but returned no txid".to_owned())?;
    normalize_txid(&txid)
}

/// The transaction id as lowercase hex without `0x`, or an error when it is not exactly 32 bytes of hex.
fn normalize_txid(txid: &str) -> Result<String, String> {
    let digits = txid.strip_prefix("0x").unwrap_or(txid);
    match hex::decode(digits) {
        Ok(bytes) if bytes.len() == 32 => Ok(hex::encode(bytes)),
        _ => Err("TRON broadcast failed: the node returned a txid that is not 32 bytes of hex".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TXID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    #[test]
    fn txid_is_normalized_to_lowercase_without_prefix() {
        assert_eq!(normalize_txid(TXID).unwrap(), TXID);
        assert_eq!(normalize_txid(&TXID.to_uppercase()).unwrap(), TXID);
        assert_eq!(normalize_txid(&format!("0x{TXID}")).unwrap(), TXID);
    }

    #[test]
    fn malformed_txid_is_an_error() {
        let bad_ids: Vec<String> = vec![
            String::new(),
            "0x".to_owned(),
            "abcd".to_owned(),
            TXID[..62].to_owned(),
            format!("{TXID}00"),
            "zz".to_owned(),
            "g".repeat(64),
        ];
        for bad in &bad_ids {
            assert!(normalize_txid(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn missing_client_is_an_error_not_a_panic() {
        let err = common::block_on(broadcast_raw_tx(None, vec![1, 2, 3])).unwrap_err();
        assert!(err.contains("TRON API client missing"), "{err}");
    }

    #[test]
    fn empty_transaction_is_refused_before_any_request() {
        // A client over an unreachable node: reaching it would give a different error text.
        let api = TronApiClient::new(vec!["http://127.0.0.1:1".to_owned()]);
        let err = common::block_on(broadcast_raw_tx(Some(api), Vec::new())).unwrap_err();
        assert!(err.contains("empty transaction"), "{err}");
    }
}
