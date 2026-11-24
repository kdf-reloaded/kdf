//! Discovery of the transaction that consumed a payment (CRD ch.21 D21.B8, ch.51 R91).
//!
//! The primary source is the contract event listing of an event service, the fallback a walk over the blocks from
//! the payment's block through the ordinary node API (transaction-info logs). Both only name candidates: a
//! candidate counts after the transaction is fetched from the node by id and is a `receiverSpend` (a refund: a
//! `senderRefund`) call of the selected contract for this payment id whose successful receipt carries the matching
//! event. Anything else is skipped.
//!
//! # Invariants
//!
//! - A source that is down, or a cap that is reached, is a typed error: never "not spent".
//! - A node failure while a candidate is checked is an error, not a skipped candidate.

use super::{fetch_swap_tx, is_foreign};
use crate::eth::tron::api::{TransactionLog, TronApiClient};
use crate::eth::tron::swap::{call_of, decode_event, TronSwapCall, TronSwapError, TronSwapEvent, TronSwapTx};
use crate::eth::tron::TronAddress;
use common::log::warn;
use ethereum_types::H256;

/// The most event pages one search reads (200 rows each) before it reports the cap.
pub(in crate::eth::tron::swap) const EVENT_PAGE_CAP: u32 = 25;
/// The most blocks one search walks (about eight hours of blocks) before it reports the cap.
pub(in crate::eth::tron::swap) const BLOCK_WALK_CAP: u64 = 10_000;

/// The caps of one search.
#[derive(Clone, Copy, Debug)]
pub(in crate::eth::tron::swap) struct Caps {
    pub event_pages: u32,
    pub walk_blocks: u64,
}

pub(in crate::eth::tron::swap) const DEFAULT_CAPS: Caps = Caps {
    event_pages: EVENT_PAGE_CAP,
    walk_blocks: BLOCK_WALK_CAP,
};

/// Which consuming call is searched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::eth::tron::swap) enum Kind {
    Spend,
    Refund,
}

impl Kind {
    fn event_name(self) -> &'static str {
        match self {
            Kind::Spend => "ReceiverSpent",
            Kind::Refund => "SenderRefunded",
        }
    }

    fn matches_call(self, call: &TronSwapCall, id: &[u8; 32]) -> bool {
        match (self, call) {
            (Kind::Spend, TronSwapCall::ReceiverSpend { id: called, .. }) => called == id,
            (Kind::Refund, TronSwapCall::SenderRefund { id: called, .. }) => called == id,
            _ => false,
        }
    }

    fn matches_event(self, event: &TronSwapEvent, id: &[u8; 32]) -> bool {
        match (self, event) {
            (Kind::Spend, TronSwapEvent::ReceiverSpent { id: named, .. }) => named == id,
            (Kind::Refund, TronSwapEvent::SenderRefunded { id: named }) => named == id,
            _ => false,
        }
    }
}

/// The swap-contract event of a log entry, when the entry comes from `contract` and decodes as one of its events.
pub(in crate::eth::tron::swap) fn event_of_log(log: &TransactionLog, contract: &TronAddress) -> Option<TronSwapEvent> {
    let emitter = hex::decode(log.address.trim_start_matches("0x")).ok()?;
    let emitter: &[u8] = if emitter.len() == 21 && emitter[0] == 0x41 {
        &emitter[1..]
    } else {
        &emitter
    };
    if emitter != contract.to_evm_address().as_bytes() {
        return None;
    }
    let topics: Vec<H256> = log
        .topics
        .iter()
        .map(|topic| {
            hex::decode(topic.trim_start_matches("0x"))
                .ok()
                .filter(|bytes| bytes.len() == 32)
                .map(|bytes| H256::from_slice(&bytes))
        })
        .collect::<Option<Vec<_>>>()?;
    let data = hex::decode(log.data.trim_start_matches("0x")).ok()?;
    decode_event(&topics, &data).ok()
}

/// The transaction `txid` when it is a consuming call of `kind` of `contract` for `id` with a successful receipt
/// carrying the matching event; `Ok(None)` for any other transaction.
///
/// # Errors
///
/// A node failure while the transaction or its receipt is read.
pub(in crate::eth::tron::swap) async fn verify_candidate(
    api: &TronApiClient,
    contract: &TronAddress,
    id: &[u8; 32],
    kind: Kind,
    txid: &str,
) -> Result<Option<TronSwapTx>, TronSwapError> {
    let tx = match fetch_swap_tx(api, txid).await {
        Ok(tx) => tx,
        Err(e) if is_foreign(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    if format!("{:x}", tx.txid()) != txid.to_ascii_lowercase() {
        return Ok(None);
    }
    let (called, _value, call) = match call_of(&tx) {
        Ok(found) => found,
        Err(e) if is_foreign(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    if &called != contract || !kind.matches_call(&call, id) {
        return Ok(None);
    }
    let info = api
        .get_confirmed_transaction_info(txid)
        .await
        .map_err(TronSwapError::Node)?;
    if info.receipt.as_ref().and_then(|receipt| receipt.result.as_deref()) != Some("SUCCESS") {
        return Ok(None);
    }
    let carries_event = info
        .log
        .iter()
        .filter_map(|log| event_of_log(log, contract))
        .any(|event| kind.matches_event(&event, id));
    Ok(carries_event.then_some(tx))
}

enum Listing {
    Found(TronSwapTx),
    NotFound,
    Capped,
    Down,
}

/// Whether an event row of the listing may be about `id`: its decoded `id` equals it, or the row carries none
/// (the transaction is checked anyway).
fn row_may_be(row: &serde_json::Value, id: &[u8; 32]) -> bool {
    let named = row["result"]["id"].as_str().or_else(|| row["result"]["0"].as_str());
    match named {
        Some(named) => named.trim_start_matches("0x").eq_ignore_ascii_case(&hex::encode(id)),
        None => true,
    }
}

async fn from_events(
    api: &TronApiClient,
    contract: &TronAddress,
    id: &[u8; 32],
    kind: Kind,
    since_ms: i64,
    page_cap: u32,
) -> Result<Listing, TronSwapError> {
    let mut fingerprint: Option<String> = None;
    for _ in 0..page_cap {
        let page = match api
            .get_contract_events(contract, kind.event_name(), since_ms, fingerprint.as_deref())
            .await
        {
            Ok(page) => page,
            Err(_) => return Ok(Listing::Down),
        };
        for row in &page.data {
            if row["event_name"].as_str().is_some_and(|name| name != kind.event_name()) || !row_may_be(row, id) {
                continue;
            }
            let Some(txid) = row["transaction_id"].as_str() else {
                continue;
            };
            if let Some(tx) = verify_candidate(api, contract, id, kind, txid).await? {
                return Ok(Listing::Found(tx));
            }
        }
        match page.meta.and_then(|meta| meta.fingerprint) {
            Some(next) => fingerprint = Some(next),
            None => return Ok(Listing::NotFound),
        }
    }
    warn!(
        "TRON event listing for the payment {} reached its cap of {page_cap} pages",
        hex::encode(id)
    );
    Ok(Listing::Capped)
}

async fn from_walk(
    api: &TronApiClient,
    contract: &TronAddress,
    id: &[u8; 32],
    kind: Kind,
    from_block: u64,
    block_cap: u64,
) -> Result<Listing, TronSwapError> {
    let head = api
        .get_now_block()
        .await
        .map_err(TronSwapError::Node)?
        .block_header
        .raw_data
        .number;
    let mut number = from_block;
    let mut walked = 0u64;
    while number <= head {
        if walked >= block_cap {
            warn!(
                "TRON block walk for the payment {} reached its cap of {block_cap} blocks",
                hex::encode(id)
            );
            return Ok(Listing::Capped);
        }
        let infos = api
            .get_transaction_infos_by_block_num(number)
            .await
            .map_err(TronSwapError::Node)?;
        for info in &infos {
            let carries_event = info
                .log
                .iter()
                .filter_map(|log| event_of_log(log, contract))
                .any(|event| kind.matches_event(&event, id));
            let Some(txid) = info.id.as_deref().filter(|_| carries_event) else {
                continue;
            };
            if let Some(tx) = verify_candidate(api, contract, id, kind, txid).await? {
                return Ok(Listing::Found(tx));
            }
        }
        number += 1;
        walked += 1;
    }
    Ok(Listing::NotFound)
}

/// The transaction that consumed the payment `id` of `contract` (a spend or a refund, as `kind`), found from the
/// event listing and, when that is down, by a block walk from `payment_block`.
///
/// # Errors
///
/// [`TronSwapError::NotYetFinal`] when the source answered completely and the transaction is not (yet) there;
/// [`TronSwapError::SearchCapped`] when a cap was reached; the node errors of the reads.
pub(in crate::eth::tron::swap) async fn find_consumer(
    api: &TronApiClient,
    contract: &TronAddress,
    id: &[u8; 32],
    kind: Kind,
    payment_block: u64,
    payment_time_ms: Option<i64>,
    caps: Caps,
) -> Result<TronSwapTx, TronSwapError> {
    let listed = match payment_time_ms {
        Some(since_ms) => from_events(api, contract, id, kind, since_ms, caps.event_pages).await?,
        None => Listing::Down,
    };
    match listed {
        Listing::Found(tx) => Ok(tx),
        Listing::NotFound => Err(TronSwapError::NotYetFinal),
        Listing::Capped => Err(TronSwapError::SearchCapped {
            source: "contract event listing",
        }),
        Listing::Down => match from_walk(api, contract, id, kind, payment_block, caps.walk_blocks).await? {
            Listing::Found(tx) => Ok(tx),
            Listing::Capped => Err(TronSwapError::SearchCapped { source: "block walk" }),
            Listing::NotFound | Listing::Down => Err(TronSwapError::NotYetFinal),
        },
    }
}
