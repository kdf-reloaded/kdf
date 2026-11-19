//! The swap layer's reader of the saved swaps that are not finished, for the activation of a shielded
//! coin (CRD ch.39 R39.8.0bd items 5 and 6). It only reads the saved swap storage and hands over data; the
//! decisions are made by the coin.

use super::maker_swap::{maker_htlc_key_input, MakerSavedSwap, MakerSwapEvent};
use super::saved_swap::{SavedSwap, SavedSwapIo};
use super::taker_swap::{TakerSavedSwap, TakerSwapEvent};
use async_trait::async_trait;
use coins::register_unfinished_swaps_view;
use coins::z_coin::{UnfinishedSwapLeg, UnfinishedSwapsView};
use crypto::privkey::SerializableSecp256k1Keypair;
use mm2_core::mm_ctx::MmArc;
use primitives::hash::H264;
use std::sync::Arc;

/// Reads the saved V1 swaps of this node.
pub(crate) struct SavedUnfinishedSwapsView {
    ctx: MmArc,
}

impl SavedUnfinishedSwapsView {
    pub(crate) fn new(ctx: MmArc) -> Self { SavedUnfinishedSwapsView { ctx } }
}

#[async_trait]
impl UnfinishedSwapsView for SavedUnfinishedSwapsView {
    async fn unfinished_v1_swaps_with_coin(&self, ticker: &str) -> Result<Vec<UnfinishedSwapLeg>, String> {
        // The same read the kick-start does, so a swap that is waiting for its coin is seen as well as
        // a running one.
        let swaps = SavedSwap::load_all_my_swaps_from_db(&self.ctx)
            .await
            .map_err(|e| e.to_string())?;
        legs_of_unfinished_swaps(&swaps, ticker)
    }
}

/// The legs in `ticker` of the swaps that are not finished; a finished swap contributes none.
///
/// # Errors
///
/// Fails when an unfinished swap cannot be read.
fn legs_of_unfinished_swaps(swaps: &[SavedSwap], ticker: &str) -> Result<Vec<UnfinishedSwapLeg>, String> {
    let mut legs = Vec::new();
    for swap in swaps {
        if swap.is_finished() {
            continue;
        }
        let leg = match swap {
            SavedSwap::Maker(maker) => maker_leg(maker, ticker)?,
            SavedSwap::Taker(taker) => taker_leg(taker, ticker)?,
        };
        legs.extend(leg);
    }
    Ok(legs)
}

/// The public half of the private key a swap saved for a leg.
fn saved_pubkey(saved: Option<SerializableSecp256k1Keypair>) -> Option<H264> {
    saved.map(|pair| H264::from(pair.into_inner().public_slice()))
}

/// Registers the reader of the saved swaps in the coins context of this node (CRD ch.39 R39.8.0bd). The node's
/// start calls it before the RPC is available and before the swaps are kick-started.
///
/// # Errors
///
/// Returns the reason when the coins context cannot be created.
pub(crate) fn register_for_node(ctx: &MmArc) -> Result<(), String> {
    register_unfinished_swaps_view(ctx, Arc::new(SavedUnfinishedSwapsView::new(ctx.clone())))
}

/// The leg of an unfinished maker swap in `ticker`, if it has one.
///
/// # Errors
///
/// Fails when the swap has no `Started` event to read.
fn maker_leg(swap: &MakerSavedSwap, ticker: &str) -> Result<Option<UnfinishedSwapLeg>, String> {
    let Some(MakerSwapEvent::Started(data)) = swap.events.first().map(|saved| &saved.event) else {
        return Err(format!("swap {}: its first saved event is not Started", swap.uuid));
    };
    let (recorded, saved_privkey, pays_in_this_coin) = if data.maker_coin == ticker {
        (data.maker_coin_htlc_pubkey, data.maker_coin_htlc_privkey, true)
    } else if data.taker_coin == ticker {
        (data.taker_coin_htlc_pubkey, data.taker_coin_htlc_privkey, false)
    } else {
        return Ok(None);
    };
    // The maker pays the maker coin; in the taker coin it only spends the taker's payment.
    let own_payments_sent = !pays_in_this_coin
        || swap
            .events
            .iter()
            .any(|saved| matches!(saved.event, MakerSwapEvent::MakerPaymentSent(_)));
    Ok(Some(UnfinishedSwapLeg {
        uuid: data.uuid.to_string(),
        recorded_htlc_pubkey: recorded.map(Into::into),
        swap_unique_input: maker_htlc_key_input(data),
        saved_privkey_pubkey: saved_pubkey(saved_privkey),
        own_payments_sent,
    }))
}

/// The leg of an unfinished taker swap in `ticker`, if it has one.
///
/// # Errors
///
/// Fails when the swap has no `Started` event to read.
fn taker_leg(swap: &TakerSavedSwap, ticker: &str) -> Result<Option<UnfinishedSwapLeg>, String> {
    let data = swap.swap_data().map_err(|e| format!("swap {}: {}", swap.uuid, e))?;
    let (recorded, saved_privkey, pays_in_this_coin) = if data.taker_coin == ticker {
        (data.taker_coin_htlc_pubkey, data.taker_coin_htlc_privkey, true)
    } else if data.maker_coin == ticker {
        (data.maker_coin_htlc_pubkey, data.maker_coin_htlc_privkey, false)
    } else {
        return Ok(None);
    };
    // The taker pays the dex fee and its payment in the taker coin; in the maker coin it only spends the
    // maker's payment.
    let has_event = |wanted: fn(&TakerSwapEvent) -> bool| swap.events.iter().any(|saved| wanted(&saved.event));
    let own_payments_sent = !pays_in_this_coin
        || (has_event(|event| matches!(event, TakerSwapEvent::TakerFeeSent(_)))
            && has_event(|event| matches!(event, TakerSwapEvent::TakerPaymentSent(_))));
    Ok(Some(UnfinishedSwapLeg {
        uuid: data.uuid.to_string(),
        recorded_htlc_pubkey: recorded.map(Into::into),
        swap_unique_input: data.uuid.as_bytes().to_vec(),
        saved_privkey_pubkey: saved_pubkey(saved_privkey),
        own_payments_sent,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm2::lp_swap::maker_swap::{MakerSavedEvent, MakerSwapData};
    use crate::mm2::lp_swap::taker_swap::{TakerSavedEvent, TakerSwapData};
    use crate::mm2::lp_swap::TransactionIdentifier;
    use rpc::v1::types::H264 as H264Json;
    use uuid::Uuid;

    fn pk(byte: u8) -> H264Json { serde_json::from_str(&format!("\"{}\"", hex::encode([byte; 33]))).unwrap() }

    fn txid() -> TransactionIdentifier {
        TransactionIdentifier {
            tx_hex: Vec::new().into(),
            tx_hash: Vec::new().into(),
        }
    }

    fn maker_started(maker_coin: &str, taker_coin: &str, uuid: Uuid) -> MakerSwapData {
        MakerSwapData {
            maker_coin: maker_coin.to_owned(),
            taker_coin: taker_coin.to_owned(),
            uuid,
            secret_hash: Some(vec![7u8; 20].into()),
            maker_coin_htlc_pubkey: Some(pk(2)),
            taker_coin_htlc_pubkey: Some(pk(3)),
            ..Default::default()
        }
    }

    fn maker_swap(data: MakerSwapData, later: Vec<MakerSwapEvent>) -> MakerSavedSwap {
        let mut events = vec![MakerSavedEvent {
            timestamp: 0,
            event: MakerSwapEvent::Started(data.clone()),
        }];
        events.extend(later.into_iter().map(|event| MakerSavedEvent { timestamp: 1, event }));
        MakerSavedSwap {
            uuid: data.uuid,
            events,
            ..Default::default()
        }
    }

    /// The maker's leg in the coin it pays: key, input and the sent flag follow the saved events.
    #[test]
    fn a_maker_leg_in_the_paying_coin_reports_its_key_and_whether_the_payment_was_sent() {
        let uuid = Uuid::from_bytes([1; 16]);
        let unsent = maker_swap(maker_started("ARRR", "KMD", uuid), vec![]);
        let leg = maker_leg(&unsent, "ARRR").unwrap().unwrap();
        assert_eq!(leg.uuid, uuid.to_string());
        assert!(
            leg.recorded_htlc_pubkey.unwrap().as_slice() == &[2u8; 33][..],
            "the maker-coin key is reported"
        );
        assert!(
            leg.swap_unique_input == vec![7u8; 20],
            "the key input is the secret hash"
        );
        assert!(leg.saved_privkey_pubkey.is_none());
        assert!(!leg.own_payments_sent);

        let sent = maker_swap(maker_started("ARRR", "KMD", uuid), vec![
            MakerSwapEvent::MakerPaymentSent(txid()),
        ]);
        assert!(maker_leg(&sent, "ARRR").unwrap().unwrap().own_payments_sent);
    }

    /// In the coin the maker receives, it sends nothing, and the key is the taker-coin key.
    #[test]
    fn a_maker_leg_in_the_receiving_coin_has_nothing_to_send_and_reads_the_other_key() {
        let swap = maker_swap(maker_started("KMD", "ARRR", Uuid::from_bytes([2; 16])), vec![]);
        let leg = maker_leg(&swap, "ARRR").unwrap().unwrap();
        assert!(leg.own_payments_sent);
        assert!(
            leg.recorded_htlc_pubkey.unwrap().as_slice() == &[3u8; 33][..],
            "the taker-coin key is reported"
        );
        assert!(maker_leg(&swap, "ZEC").unwrap().is_none());
    }

    /// The taker's leg in the coin it pays needs both the fee and the payment recorded as sent; its key
    /// input is the swap's uuid.
    #[test]
    fn a_taker_leg_in_the_paying_coin_needs_the_fee_and_the_payment() {
        let uuid = Uuid::from_bytes([3; 16]);
        let data = TakerSwapData {
            maker_coin: "KMD".to_owned(),
            taker_coin: "ARRR".to_owned(),
            uuid,
            taker_coin_htlc_pubkey: Some(pk(4)),
            ..Default::default()
        };
        let with = |later: Vec<TakerSwapEvent>| {
            let mut events = vec![TakerSavedEvent {
                timestamp: 0,
                event: TakerSwapEvent::Started(data.clone()),
            }];
            events.extend(later.into_iter().map(|event| TakerSavedEvent { timestamp: 1, event }));
            TakerSavedSwap {
                uuid,
                my_order_uuid: None,
                events,
                maker_amount: None,
                maker_coin: None,
                taker_amount: None,
                taker_coin: None,
                gui: None,
                mm_version: None,
                success_events: Vec::new(),
                error_events: Vec::new(),
            }
        };

        let nothing = taker_leg(&with(vec![]), "ARRR").unwrap().unwrap();
        assert!(
            nothing.swap_unique_input == uuid.as_bytes().to_vec(),
            "the key input is the uuid"
        );
        assert!(
            nothing.recorded_htlc_pubkey.unwrap().as_slice() == &[4u8; 33][..],
            "the taker's key is reported"
        );
        assert!(!nothing.own_payments_sent);
        let fee_only = taker_leg(&with(vec![TakerSwapEvent::TakerFeeSent(txid())]), "ARRR")
            .unwrap()
            .unwrap();
        assert!(!fee_only.own_payments_sent);
        let both = taker_leg(
            &with(vec![
                TakerSwapEvent::TakerFeeSent(txid()),
                TakerSwapEvent::TakerPaymentSent(txid()),
            ]),
            "ARRR",
        )
        .unwrap()
        .unwrap();
        assert!(both.own_payments_sent);
        // In the maker coin the taker only spends.
        assert!(taker_leg(&with(vec![]), "KMD").unwrap().unwrap().own_payments_sent);
    }

    /// The node's start registers the reader: before the registration the context has none, after it the
    /// context has one. `lp_native_dex` calls exactly this function (`register_for_node`); the whole start path
    /// needs a network, so this is the test that does not.
    #[test]
    fn the_registration_the_node_start_calls_installs_the_view() {
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new().into_mm_arc();
        assert!(!coins::unfinished_swaps_view_registered(&ctx).unwrap());

        register_for_node(&ctx).unwrap();

        assert!(coins::unfinished_swaps_view_registered(&ctx).unwrap());
    }

    /// A finished swap contributes no leg, whatever it holds; an unfinished one of the same coin does.
    #[test]
    fn a_finished_swap_is_skipped_and_an_unfinished_one_is_not() {
        let finished_uuid = Uuid::from_bytes([8; 16]);
        let open_uuid = Uuid::from_bytes([9; 16]);
        let finished = maker_swap(maker_started("ARRR", "KMD", finished_uuid), vec![
            MakerSwapEvent::Finished,
        ]);
        let open = maker_swap(maker_started("ARRR", "KMD", open_uuid), vec![]);
        let swaps = vec![SavedSwap::Maker(finished), SavedSwap::Maker(open)];

        let legs = legs_of_unfinished_swaps(&swaps, "ARRR").unwrap();

        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0].uuid, open_uuid.to_string());
        assert!(legs_of_unfinished_swaps(&swaps[..1], "ARRR").unwrap().is_empty());
    }

    /// A saved private key is reported by its public half.
    #[test]
    fn a_leg_reports_the_public_half_of_its_saved_private_key() {
        let pair = crypto::privkey::key_pair_from_secret(&[0x33; 32]).unwrap();
        let mut data = maker_started("ARRR", "KMD", Uuid::from_bytes([6; 16]));
        data.maker_coin_htlc_privkey = Some(pair.into());
        let leg = maker_leg(&maker_swap(data, vec![]), "ARRR").unwrap().unwrap();
        assert!(
            leg.saved_privkey_pubkey.unwrap().as_slice() == pair.public_slice(),
            "the public half of the saved key is reported"
        );
    }

    /// A swap whose first event is not `Started` cannot be read, which refuses the activation.
    #[test]
    fn a_swap_without_a_started_event_is_an_error() {
        let swap = MakerSavedSwap {
            uuid: Uuid::from_bytes([4; 16]),
            events: vec![MakerSavedEvent {
                timestamp: 0,
                event: MakerSwapEvent::Finished,
            }],
            ..Default::default()
        };
        assert!(maker_leg(&swap, "ARRR").is_err());
        let empty = TakerSavedSwap {
            uuid: Uuid::from_bytes([5; 16]),
            my_order_uuid: None,
            events: Vec::new(),
            maker_amount: None,
            maker_coin: None,
            taker_amount: None,
            taker_coin: None,
            gui: None,
            mm_version: None,
            success_events: Vec::new(),
            error_events: Vec::new(),
        };
        assert!(taker_leg(&empty, "ARRR").is_err());
    }
}
