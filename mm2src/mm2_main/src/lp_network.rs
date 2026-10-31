/******************************************************************************
 * Copyright © 2014-2019 The SuperNET Developers.                             *
 *                                                                            *
 * See the AUTHORS, DEVELOPER-AGREEMENT and LICENSE files at                  *
 * the top-level directory of this distribution for the individual copyright  *
 * holder information and the developer policies on copyright and licensing.  *
 *                                                                            *
 * Unless otherwise agreed in a custom licensing agreement, no part of the    *
 * SuperNET software, including this file may be copied, modified, propagated *
 * or distributed except according to the terms contained in the LICENSE file *
 *                                                                            *
 * Removal or modification of this copyright notice is prohibited.            *
 *                                                                            *
 ******************************************************************************/
//
//  lp_network.rs
//  marketmaker
//
use coins::lp_coinfind;
use common::executor::{spawn, Timer};
use common::{log, now_ms, Future01CompatExt, HttpStatusCode};
use derive_more::Display;
use futures::{channel::oneshot,
              future::{select, Either},
              FutureExt, StreamExt};
use http::StatusCode;
use keys::KeyPair;
use mm2_core::mm_ctx::{MmArc, MmWeak};
use mm2_err_handle::prelude::*;
use mm2_metrics::{ClockOps, MetricsOps};
use mm2_p2p::atomicdex_behaviour::{AdexBehaviourCmd, AdexBehaviourEvent, AdexCmdTx, AdexEventRx, AdexResponse,
                                   AdexResponseChannel};
use mm2_p2p::peers_exchange::PeerAddresses;
use mm2_p2p::request_response::LocalFailure;
use mm2_p2p::{decode_message, decode_signed, encode_and_sign, encode_message, pub_sub_topic, DecodingError,
              GossipsubMessage, Libp2pPublic, Libp2pSecpPublic, MessageId, NetworkPorts, PeerId, TOPIC_SEPARATOR};
#[cfg(test)] use mocktopus::macros::*;
use parking_lot::Mutex as PaMutex;
use rand::random;
use serde::de;
use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::mm2::{lp_ordermatch, lp_stats, lp_swap};

pub type P2PRequestResult<T> = Result<T, MmError<P2PRequestError>>;

const PEER_HEALTHCHECK_PREFIX: &str = "peer_healthcheck";
const PEER_HEALTHCHECK_TIMEOUT_SEC: f64 = 10.;

pub trait Libp2pPeerId {
    fn libp2p_peer_id(&self) -> PeerId;
}

impl Libp2pPeerId for KeyPair {
    #[inline(always)]
    fn libp2p_peer_id(&self) -> PeerId { peer_id_from_secp_public(self.public_slice()).expect("valid public") }
}

#[derive(Debug, Display)]
#[allow(clippy::enum_variant_names)]
pub enum P2PRequestError {
    EncodeError(String),
    DecodeError(String),
    SendError(String),
    /// The peer answered with an error.
    ResponseError(String),
    /// This node's own request failed without an answer from the peer (the order-book synchronization logging
    /// classifies by it); the text is the same as it was when this was a [`P2PRequestError::ResponseError`].
    #[display(fmt = "{}", _1)]
    LocalResponseError(LocalFailure, String),
    #[display(fmt = "Expected 1 response, found {}", _0)]
    ExpectedSingleResponseError(usize),
}

pub type PeerHealthcheckRpcResult<T> = Result<T, MmError<PeerHealthcheckError>>;

#[derive(Debug, Serialize, Display, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
// Variants intentionally share the idiomatic `Error` suffix; renaming them is
// churny and hurts readability.
#[allow(clippy::enum_variant_names)]
pub enum PeerHealthcheckError {
    ProbeGenerationError(String),
    ProbeEncodingError(String),
    InternalError(String),
}

impl HttpStatusCode for PeerHealthcheckError {
    fn status_code(&self) -> StatusCode {
        match self {
            PeerHealthcheckError::ProbeGenerationError(_)
            | PeerHealthcheckError::ProbeEncodingError(_)
            | PeerHealthcheckError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<rmp_serde::encode::Error> for P2PRequestError {
    fn from(e: rmp_serde::encode::Error) -> Self { P2PRequestError::EncodeError(e.to_string()) }
}

impl From<rmp_serde::decode::Error> for P2PRequestError {
    fn from(e: rmp_serde::decode::Error) -> Self { P2PRequestError::DecodeError(e.to_string()) }
}

#[derive(Eq, Debug, Deserialize, PartialEq, Serialize)]
pub enum P2PRequest {
    Ordermatch(lp_ordermatch::OrdermatchRequest),
    NetworkInfo(lp_stats::NetworkInfoRequest),
}

#[derive(Deserialize, Serialize)]
enum PeerHealthcheckMsg {
    Probe {
        source_peer: String,
        target_peer: String,
        nonce: u64,
        expires_at: u64,
    },
    Ack {
        source_peer: String,
        target_peer: String,
        nonce: u64,
        expires_at: u64,
    },
}

pub(crate) struct PendingHealthcheckWaiter {
    nonce: u64,
    ack_tx: oneshot::Sender<()>,
}

/// The queue signal toward the node's relays (CRD ch.32 §32.9B R-MP2 item 2), as the swarm side last saw it. The
/// swarm side writes it on every poll; the maker outbox reads it.
#[derive(Debug, Default)]
pub struct RelayQueueSignal {
    /// Whether the signal is raised toward any connection to a relay.
    pub raised: AtomicBool,
    /// How many times it was raised toward a relay connection since start.
    pub raised_count: AtomicU64,
}

pub struct P2PContext {
    /// Using Mutex helps to prevent cloning which can actually result to channel being unbounded in case of using 1 tx clone per 1 message.
    pub cmd_tx: PaMutex<AdexCmdTx>,
    pub(crate) pending_healthchecks: PaMutex<HashMap<String, PendingHealthcheckWaiter>>,
    /// The queue signal toward the relays (CRD ch.32 §32.9B R-MP2 item 2).
    pub relay_queue_signal: Arc<RelayQueueSignal>,
}

#[cfg_attr(test, mockable)]
impl P2PContext {
    pub fn new(cmd_tx: AdexCmdTx) -> Self {
        P2PContext {
            cmd_tx: PaMutex::new(cmd_tx),
            pending_healthchecks: PaMutex::new(HashMap::new()),
            relay_queue_signal: Arc::default(),
        }
    }

    /// Uses `relay_queue_signal`, which the swarm side already updates, as the queue signal of the context.
    pub fn with_relay_queue_signal(mut self, relay_queue_signal: Arc<RelayQueueSignal>) -> Self {
        self.relay_queue_signal = relay_queue_signal;
        self
    }

    pub fn store_to_mm_arc(self, ctx: &MmArc) { *ctx.p2p_ctx.lock().unwrap() = Some(Arc::new(self)) }

    pub fn fetch_from_mm_arc(ctx: &MmArc) -> Arc<Self> {
        ctx.p2p_ctx
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone()
            .downcast()
            .unwrap()
    }
}

fn relay_queue_signal_of(ctx: &MmArc) -> Option<Arc<RelayQueueSignal>> {
    match ctx.p2p_ctx.lock() {
        Ok(guard) => guard
            .as_ref()
            .and_then(|p2p_ctx| p2p_ctx.clone().downcast::<P2PContext>().ok())
            .map(|p2p_ctx| p2p_ctx.relay_queue_signal.clone()),
        Err(_) => None,
    }
}

/// Whether the queue signal is raised toward any connection to a relay (CRD ch.32 §32.9B R-MP2 item 2). `false`
/// while there is no P2P context.
pub fn relay_queue_signal(ctx: &MmArc) -> bool {
    relay_queue_signal_of(ctx).is_some_and(|signal| signal.raised.load(Ordering::Relaxed))
}

/// How many times the queue signal was raised toward a relay connection since start (CRD ch.32 §32.9B R-MP5).
pub fn relay_queue_signal_count(ctx: &MmArc) -> u64 {
    relay_queue_signal_of(ctx).map_or(0, |signal| signal.raised_count.load(Ordering::Relaxed))
}

fn peer_healthcheck_topic(peer_id: &str) -> String { pub_sub_topic(PEER_HEALTHCHECK_PREFIX, peer_id) }

pub fn subscribe_to_own_peer_healthcheck_topic(ctx: &MmArc, peer_id: &str) {
    subscribe_to_topic(ctx, peer_healthcheck_topic(peer_id));
}

pub async fn peer_connection_healthcheck(ctx: MmArc, peer_address: String) -> PeerHealthcheckRpcResult<bool> {
    let my_peer_id = ctx
        .peer_id
        .ok_or("Peer ID is not initialized")
        .map_to_mm(|e| PeerHealthcheckError::InternalError(e.to_string()))?
        .to_string();

    if peer_address == my_peer_id {
        return Ok(true);
    }

    let nonce = random::<u64>();
    let expires_at = now_ms() / 1000 + PEER_HEALTHCHECK_TIMEOUT_SEC as u64;

    let probe = PeerHealthcheckMsg::Probe {
        source_peer: my_peer_id.clone(),
        target_peer: peer_address.clone(),
        nonce,
        expires_at,
    };
    let signed_probe = encode_and_sign(&probe, &ctx.secp256k1_key_pair().private().secret.take())
        .map_to_mm(|e| PeerHealthcheckError::ProbeGenerationError(e.to_string()))?;

    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);

    subscribe_to_topic(&ctx, peer_healthcheck_topic(&peer_address));

    let (ack_tx, ack_rx) = oneshot::channel();
    p2p_ctx
        .pending_healthchecks
        .lock()
        .insert(peer_address.clone(), PendingHealthcheckWaiter { nonce, ack_tx });

    let publish_cmd = AdexBehaviourCmd::PublishMsg {
        topics: vec![peer_healthcheck_topic(&peer_address)],
        msg: signed_probe,
        priority: false,
    };
    p2p_ctx
        .cmd_tx
        .lock()
        .try_send(publish_cmd)
        .map_to_mm(|e| PeerHealthcheckError::ProbeEncodingError(e.to_string()))?;

    let timeout = Timer::sleep(PEER_HEALTHCHECK_TIMEOUT_SEC).fuse();
    futures::pin_mut!(timeout);
    let ack_rx = ack_rx.fuse();
    futures::pin_mut!(ack_rx);

    let result = match select(ack_rx, timeout).await {
        Either::Left((Ok(()), _)) => true,
        Either::Left((Err(_), _)) => false,
        Either::Right(_) => false,
    };

    p2p_ctx.pending_healthchecks.lock().remove(&peer_address);

    Ok(result)
}

async fn process_peer_healthcheck_message(ctx: MmArc, sender_peer_id: PeerId, topic_peer: &str, message: &[u8]) {
    let my_peer_id = match ctx.peer_id.ok_or("Peer ID is not initialized") {
        Ok(peer_id) => peer_id.to_string(),
        Err(_) => return,
    };

    let (healthcheck_msg, _, _) = match decode_signed::<PeerHealthcheckMsg>(message) {
        Ok(decoded) => decoded,
        Err(e) => {
            log::error!("Error decoding peer healthcheck message: {}", e);
            return;
        },
    };

    match healthcheck_msg {
        PeerHealthcheckMsg::Probe {
            source_peer,
            target_peer,
            nonce,
            expires_at,
        } => {
            if target_peer != my_peer_id || topic_peer != my_peer_id || expires_at <= now_ms() / 1000 {
                return;
            }

            let ack = PeerHealthcheckMsg::Ack {
                source_peer: my_peer_id,
                target_peer: source_peer,
                nonce,
                expires_at,
            };

            let signed_ack = match encode_and_sign(&ack, &ctx.secp256k1_key_pair().private().secret.take()) {
                Ok(msg) => msg,
                Err(e) => {
                    log::error!("Error signing peer healthcheck ack: {}", e);
                    return;
                },
            };

            broadcast_p2p_msg(&ctx, vec![peer_healthcheck_topic(topic_peer)], signed_ack, None);
        },
        PeerHealthcheckMsg::Ack {
            target_peer,
            nonce,
            expires_at,
            ..
        } => {
            if target_peer != my_peer_id || expires_at <= now_ms() / 1000 {
                return;
            }

            let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
            let sender_peer_id = sender_peer_id.to_string();
            let waiter = { p2p_ctx.pending_healthchecks.lock().remove(&sender_peer_id) };
            if let Some(waiter) = waiter {
                if waiter.nonce == nonce {
                    let _ = waiter.ack_tx.send(());
                }
            }
        },
    }
}

pub async fn p2p_event_process_loop(ctx: MmWeak, mut rx: AdexEventRx, i_am_relay: bool) {
    loop {
        let adex_event = rx.next().await;
        let ctx = match MmArc::from_weak(&ctx) {
            Some(ctx) => ctx,
            None => return,
        };
        match adex_event {
            Some(AdexBehaviourEvent::Message(peer_id, message_id, message)) => {
                spawn(process_p2p_message(ctx, peer_id, message_id, message, i_am_relay));
            },
            Some(AdexBehaviourEvent::PeerRequest {
                peer_id,
                request,
                response_channel,
            }) => {
                if let Err(e) = process_p2p_request(ctx, peer_id, request, response_channel) {
                    log::error!("Error on process P2P request: {:?}", e);
                }
            },
            Some(AdexBehaviourEvent::RelayConnectionClosed { peer_id }) => {
                log::debug!("The last connection to relay {} closed", peer_id);
                // CRD ch.32 §32.9B R-MP2 item 2: the close signal.
                lp_ordermatch::relay_connection_closed(&ctx);
            },
            Some(AdexBehaviourEvent::RelayMeshJoined { peer_id }) => {
                log::debug!("Relay {} joined the relay mesh", peer_id);
                // CRD ch.32 §32.9B R-MP4: the keep-alive round that replaces what a closed connection lost.
                lp_ordermatch::schedule_reconnect_keep_alive_round(ctx);
            },
            None => break,
            _ => (),
        }
    }
}

/// The transaction a transaction-helper message carries (CRD ch.51 R50B). The sender signs the transaction's bytes
/// (`lp_swap::broadcast_p2p_tx_msg`), so the message is a signed envelope; one that does not decode or verify is an
/// error.
fn tx_helper_transaction(data: &[u8]) -> Result<Vec<u8>, String> {
    mm2_p2p::decode_signed::<Vec<u8>>(data)
        .map(|(tx, _signature, _pubkey)| tx)
        .map_err(|e| e.to_string())
}

pub(crate) async fn process_p2p_message(
    ctx: MmArc,
    peer_id: PeerId,
    message_id: MessageId,
    message: GossipsubMessage,
    i_am_relay: bool,
) {
    let mut to_propagate = false;
    let mut priority = false;
    let mut orderbook_pairs = vec![];
    let priority_only = relay_second_copy_priority_only(&ctx);

    for topic in std::iter::once(&message.topic) {
        let mut split = topic.as_str().split(TOPIC_SEPARATOR);
        match split.next() {
            Some(lp_ordermatch::ORDERBOOK_PREFIX) => {
                if let Some(pair) = split.next() {
                    orderbook_pairs.push(pair.to_string());
                }
            },
            Some(PEER_HEALTHCHECK_PREFIX) => {
                if let Some(peer) = split.next() {
                    process_peer_healthcheck_message(ctx.clone(), peer_id, peer, &message.data).await;
                }
            },
            Some(lp_swap::SWAP_PREFIX) => {
                lp_swap::process_msg(ctx.clone(), split.next().unwrap_or_default(), &message.data).await;
                to_propagate = true;
            },
            Some(lp_swap::SWAP_V2_PREFIX) => {
                if let Err(e) =
                    lp_swap::process_swap_v2_msg(ctx.clone(), split.next().unwrap_or_default(), &message.data)
                {
                    log::error!("{}", e);
                    return;
                }
                to_propagate = true;
            },
            Some(lp_swap::TX_HELPER_PREFIX) => {
                if let Some(pair) = split.next() {
                    if let Ok(Some(coin)) = lp_coinfind(&ctx, pair).await {
                        // CRD ch.51 R50B: the message is the signed envelope of the transaction, not the transaction.
                        match tx_helper_transaction(&message.data) {
                            Ok(tx) => match coin.send_raw_tx_bytes(&tx).compat().await {
                                Ok(id) => log::debug!("Transaction broadcasted successfully: {:?} ", id),
                                // The usual outcome: the parties have already broadcast it themselves.
                                Err(e) => log::debug!("Broadcast transaction failed. {}", e),
                            },
                            Err(e) => log::warn!("Transaction-helper message for {} does not decode: {}", pair, e),
                        }
                    }
                }
            },
            Some(lp_swap::WATCHER_PREFIX) => {
                lp_swap::process_watcher_msg(ctx.clone(), &message.data).await;
                to_propagate = true;
            },
            None | Some(_) => (),
        }
    }

    let orderbook_message = !orderbook_pairs.is_empty();
    if orderbook_message {
        let process_fut = lp_ordermatch::process_msg_with_priority(
            ctx.clone(),
            orderbook_pairs,
            peer_id.to_string(),
            &message.data,
            i_am_relay,
        );

        let (propagate, is_priority) = process_fut.await;
        if propagate {
            to_propagate = true;
            // CRD ch.32 §32.9B R-MP1 item 2: a cancellation or a swap-negotiation message is propagated
            // with priority.
            priority = is_priority;
        }
    }

    if to_propagate && i_am_relay {
        if relay_sends_second_copy(priority, orderbook_message, priority_only) {
            propagate_message_with_priority(&ctx, message_id, peer_id, priority);
        } else {
            validate_message(&ctx, message_id);
        }
    }
}

/// CRD ch.32 R-F3b (D11): the MM2.json boolean `relay_second_copy_priority_only`, default `true`. Only an explicit
/// JSON `false` restores the reference relay's application-level second copy of every message.
fn relay_second_copy_priority_only(ctx: &MmArc) -> bool {
    ctx.conf["relay_second_copy_priority_only"].as_bool() != Some(false)
}

/// Whether a relay sends the application-level second copy of a message (CRD ch.32 R-F3b, amended, D11).
/// With `priority_only` (the default) a priority message keeps it, and so does a message outside the orderbook
/// topics; for any other orderbook message the gossip-layer forward is the only copy. Without `priority_only` every
/// message gets it, as on a reference relay. The relay validates the message in its message cache either way.
fn relay_sends_second_copy(priority: bool, orderbook_message: bool, priority_only: bool) -> bool {
    !priority_only || priority || !orderbook_message
}

fn process_p2p_request(
    ctx: MmArc,
    peer_id: PeerId,
    request: Vec<u8>,
    response_channel: AdexResponseChannel,
) -> P2PRequestResult<()> {
    // Sends the answer through the response channel. A synchronization request may be parked, and then
    // the answer is sent later from another task (CRD ch.32 R-Q5b), so the channel travels in a closure.
    let reply_ctx = ctx.clone();
    let reply: lp_ordermatch::SyncAnswerReply = Box::new(move |res| {
        let cmd = AdexBehaviourCmd::SendResponse { res, response_channel };
        let p2p_ctx = P2PContext::fetch_from_mm_arc(&reply_ctx);
        let sent = p2p_ctx.cmd_tx.lock().try_send(cmd).map_err(|e| e.to_string());
        sent
    });

    let mut decode_error = None;
    let res = match decode_message::<P2PRequest>(&request) {
        Ok(P2PRequest::Ordermatch(lp_ordermatch::OrdermatchRequest::SyncPubkeyOrderbookState {
            pubkey,
            trie_roots,
        })) => {
            return lp_ordermatch::answer_sync_request(&ctx, peer_id, pubkey, trie_roots, reply)
                .map_to_mm(P2PRequestError::SendError);
        },
        Ok(request) => {
            let result = match request {
                P2PRequest::Ordermatch(req) => lp_ordermatch::process_peer_request(ctx.clone(), req),
                P2PRequest::NetworkInfo(req) => lp_stats::process_info_request(ctx.clone(), req),
            };

            match result {
                Ok(Some(response)) => AdexResponse::Ok { response },
                Ok(None) => AdexResponse::None,
                Err(e) => AdexResponse::Err { error: e },
            }
        },
        Err(e) => {
            let error = e.to_string();
            decode_error = Some(error.clone());
            AdexResponse::Err { error }
        },
    };

    reply(res).map_to_mm(P2PRequestError::SendError)?;

    if let Some(error) = decode_error {
        return MmError::err(P2PRequestError::DecodeError(error));
    }

    Ok(())
}

pub fn broadcast_p2p_msg(ctx: &MmArc, topics: Vec<String>, msg: Vec<u8>, from: Option<PeerId>) {
    broadcast_p2p_msg_with_priority(ctx, topics, msg, from, false)
}

/// Like [`broadcast_p2p_msg`]; a `priority` message goes through the priority lane of the send queue of every
/// connection it is written to (CRD ch.28 §28.9B RQ1 item 9).
pub fn broadcast_p2p_msg_with_priority(
    ctx: &MmArc,
    topics: Vec<String>,
    msg: Vec<u8>,
    from: Option<PeerId>,
    priority: bool,
) {
    let ctx = ctx.clone();
    let cmd = match from {
        Some(from) => AdexBehaviourCmd::PublishMsgFrom {
            topics,
            msg,
            from,
            priority,
        },
        None => AdexBehaviourCmd::PublishMsg { topics, msg, priority },
    };
    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
    if let Err(e) = p2p_ctx.cmd_tx.lock().try_send(cmd) {
        log::error!("broadcast_p2p_msg cmd_tx.send error {:?}", e);
    };
}

/// Subscribe to the given `topic`.
///
/// # Safety
///
/// The function locks the [`MmCtx::p2p_ctx`] mutex.
pub fn subscribe_to_topic(ctx: &MmArc, topic: String) {
    let p2p_ctx = P2PContext::fetch_from_mm_arc(ctx);
    let cmd = AdexBehaviourCmd::Subscribe { topic };
    if let Err(e) = p2p_ctx.cmd_tx.lock().try_send(cmd) {
        log::error!("subscribe_to_topic cmd_tx.send error {:?}", e);
    };
}

pub async fn request_any_relay<T: de::DeserializeOwned>(
    ctx: MmArc,
    req: P2PRequest,
) -> P2PRequestResult<Option<(T, PeerId)>> {
    let encoded = encode_message(&req)?;

    let (response_tx, response_rx) = oneshot::channel();
    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd = AdexBehaviourCmd::RequestAnyRelay {
        req: encoded,
        response_tx,
    };
    p2p_ctx
        .cmd_tx
        .lock()
        .try_send(cmd)
        .map_to_mm(|e| P2PRequestError::SendError(e.to_string()))?;
    match response_rx
        .await
        .map_to_mm(|e| P2PRequestError::LocalResponseError(LocalFailure::Transport, e.to_string()))?
    {
        Some((from_peer, response)) => {
            let response = decode_message::<T>(&response)?;
            Ok(Some((response, from_peer)))
        },
        None => Ok(None),
    }
}

pub enum PeerDecodedResponse<T> {
    Ok(T),
    None,
    /// The peer answered with an error.
    Err(String),
    /// This node's own request failed without an answer, or its answer could not be decoded.
    LocalErr(LocalFailure, String),
}

#[allow(dead_code)]
pub async fn request_relays<T: de::DeserializeOwned>(
    ctx: MmArc,
    req: P2PRequest,
) -> P2PRequestResult<Vec<(PeerId, PeerDecodedResponse<T>)>> {
    let encoded = encode_message(&req)?;

    let (response_tx, response_rx) = oneshot::channel();
    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd = AdexBehaviourCmd::RequestRelays {
        req: encoded,
        response_tx,
    };
    p2p_ctx
        .cmd_tx
        .lock()
        .try_send(cmd)
        .map_to_mm(|e| P2PRequestError::SendError(e.to_string()))?;
    let responses = response_rx
        .await
        .map_to_mm(|e| P2PRequestError::LocalResponseError(LocalFailure::Transport, e.to_string()))?;
    Ok(parse_peers_responses(responses))
}

pub async fn request_peers<T: de::DeserializeOwned>(
    ctx: MmArc,
    req: P2PRequest,
    peers: Vec<String>,
) -> P2PRequestResult<Vec<(PeerId, PeerDecodedResponse<T>)>> {
    let encoded = encode_message(&req)?;

    let (response_tx, response_rx) = oneshot::channel();
    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd = AdexBehaviourCmd::RequestPeers {
        req: encoded,
        peers,
        response_tx,
    };
    p2p_ctx
        .cmd_tx
        .lock()
        .try_send(cmd)
        .map_to_mm(|e| P2PRequestError::SendError(e.to_string()))?;
    let responses = response_rx
        .await
        .map_to_mm(|e| P2PRequestError::LocalResponseError(LocalFailure::Transport, e.to_string()))?;
    Ok(parse_peers_responses(responses))
}

pub async fn request_one_peer<T: de::DeserializeOwned>(
    ctx: MmArc,
    req: P2PRequest,
    peer: String,
) -> P2PRequestResult<Option<T>> {
    let clock = ctx.metrics.clock().expect("Metrics clock is not available");
    let start = clock.now();
    let mut responses = request_peers::<T>(ctx.clone(), req, vec![peer.clone()]).await?;
    let end = clock.now();
    mm_timing!(ctx.metrics, "peer.outgoing_request.timing", start, end, "peer" => peer);
    if responses.len() != 1 {
        return MmError::err(P2PRequestError::ExpectedSingleResponseError(responses.len()));
    }
    let (_, response) = responses.remove(0);
    match response {
        PeerDecodedResponse::Ok(response) => Ok(Some(response)),
        PeerDecodedResponse::None => Ok(None),
        PeerDecodedResponse::Err(e) => MmError::err(P2PRequestError::ResponseError(e)),
        PeerDecodedResponse::LocalErr(kind, e) => MmError::err(P2PRequestError::LocalResponseError(kind, e)),
    }
}

fn parse_peers_responses<T: de::DeserializeOwned>(
    responses: Vec<(PeerId, AdexResponse)>,
) -> Vec<(PeerId, PeerDecodedResponse<T>)> {
    responses
        .into_iter()
        .map(|(peer_id, res)| {
            let res = match res {
                AdexResponse::Ok { response } => match decode_message::<T>(&response) {
                    Ok(res) => PeerDecodedResponse::Ok(res),
                    Err(e) => PeerDecodedResponse::LocalErr(LocalFailure::Decode, ERRL!("{}", e)),
                },
                AdexResponse::None => PeerDecodedResponse::None,
                AdexResponse::Err { error } => PeerDecodedResponse::Err(error),
                AdexResponse::LocalErr { kind, error } => PeerDecodedResponse::LocalErr(kind, error),
            };
            (peer_id, res)
        })
        .collect()
}

pub fn propagate_message(ctx: &MmArc, message_id: MessageId, propagation_source: PeerId) {
    propagate_message_with_priority(ctx, message_id, propagation_source, false)
}

/// Like [`propagate_message`]; a `priority` message goes through the priority lane of the send queue of every
/// connection it is written to (CRD ch.28 §28.9B RQ1 item 9).
pub fn propagate_message_with_priority(ctx: &MmArc, message_id: MessageId, propagation_source: PeerId, priority: bool) {
    let ctx = ctx.clone();
    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd = AdexBehaviourCmd::PropagateMessage {
        message_id,
        propagation_source,
        priority,
    };
    if let Err(e) = p2p_ctx.cmd_tx.lock().try_send(cmd) {
        log::error!("propagate_message cmd_tx.send error {:?}", e);
    };
}

/// Marks a cached message as validated without forwarding it (CRD ch.32 R-F3b, D11).
pub fn validate_message(ctx: &MmArc, message_id: MessageId) {
    let ctx = ctx.clone();
    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd = AdexBehaviourCmd::ValidateMessage { message_id };
    if let Err(e) = p2p_ctx.cmd_tx.lock().try_send(cmd) {
        log::error!("validate_message cmd_tx.send error {:?}", e);
    };
}

pub fn add_reserved_peer_addresses(ctx: &MmArc, peer: PeerId, addresses: PeerAddresses) {
    let ctx = ctx.clone();
    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd = AdexBehaviourCmd::AddReservedPeer { peer, addresses };
    if let Err(e) = p2p_ctx.cmd_tx.lock().try_send(cmd) {
        log::error!("add_reserved_peer_addresses cmd_tx.send error {:?}", e);
    };
}

#[derive(Debug, Display)]
pub enum ParseAddressError {
    #[display(fmt = "Address/Seed {} resolved to IPv6 which is not supported", _0)]
    UnsupportedIPv6Address(String),
    #[display(fmt = "Address/Seed {} to_socket_addrs empty iter", _0)]
    EmptyIterator(String),
    #[display(fmt = "Couldn't resolve '{}' Address/Seed: {}", _0, _1)]
    UnresolvedAddress(String, String),
}

#[cfg(not(target_arch = "wasm32"))]
pub fn addr_to_ipv4_string(address: &str) -> Result<String, MmError<ParseAddressError>> {
    // Remove "https:// or http://" etc.. from address str
    let formated_address = address.split("://").last().unwrap_or(address);
    let address_with_port = if formated_address.contains(':') {
        formated_address.to_string()
    } else {
        format!("{}:0", formated_address)
    };
    match address_with_port.as_str().to_socket_addrs() {
        Ok(mut iter) => match iter.next() {
            Some(addr) => {
                if addr.is_ipv4() {
                    Ok(addr.ip().to_string())
                } else {
                    log::warn!(
                        "Address/Seed {} resolved to IPv6 {} which is not supported",
                        address,
                        addr
                    );
                    MmError::err(ParseAddressError::UnsupportedIPv6Address(address.into()))
                }
            },
            None => {
                log::warn!("Address/Seed {} to_socket_addrs empty iter", address);
                MmError::err(ParseAddressError::EmptyIterator(address.into()))
            },
        },
        Err(e) => {
            log::error!("Couldn't resolve '{}' seed: {}", address, e);
            MmError::err(ParseAddressError::UnresolvedAddress(address.into(), e.to_string()))
        },
    }
}

#[derive(Clone, Debug, Display, Serialize)]
pub enum NetIdError {
    #[display(fmt = "Netid {} is larger than max {}", netid, max_netid)]
    LargerThanMax { netid: u16, max_netid: u16 },
}

pub fn lp_ports(netid: u16) -> Result<(u16, u16, u16), MmError<NetIdError>> {
    const LP_RPCPORT: u16 = 7783;
    let max_netid = (65535 - 40 - LP_RPCPORT) / 4;
    if netid > max_netid {
        return MmError::err(NetIdError::LargerThanMax { netid, max_netid });
    }

    let other_ports = if netid != 0 {
        let net_mod = netid % 10;
        let net_div = netid / 10;
        (net_div * 40) + LP_RPCPORT + net_mod
    } else {
        LP_RPCPORT
    };
    Ok((other_ports + 10, other_ports + 20, other_ports + 30))
}

pub fn lp_network_ports(netid: u16) -> Result<NetworkPorts, MmError<NetIdError>> {
    let (_, network_port, network_wss_port) = lp_ports(netid)?;
    Ok(NetworkPorts {
        tcp: network_port,
        wss: network_wss_port,
    })
}

pub fn peer_id_from_secp_public(secp_public: &[u8]) -> Result<PeerId, MmError<DecodingError>> {
    let public_key = Libp2pSecpPublic::try_from_bytes(secp_public)?;
    Ok(PeerId::from_public_key(&Libp2pPublic::from(public_key)))
}

#[cfg(test)]
mod tests {
    use super::{peer_connection_healthcheck, peer_healthcheck_topic, relay_second_copy_priority_only,
                relay_sends_second_copy, tx_helper_transaction};
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::json;

    /// CRD ch.51 R50B: the receiver broadcasts the transaction inside the signed envelope, never the envelope.
    #[test]
    fn tx_helper_message_yields_the_signed_transaction() {
        // The first bytes of a transaction, with values above 0x7f as in a real one.
        let tx: Vec<u8> = vec![1, 0, 0, 0, 1, 0xa6, 0x7a, 0x32, 0x94, 0xd4, 0x80, 0xfb, 0, 0, 0, 0];
        // What `lp_swap::broadcast_p2p_tx_msg` publishes.
        let envelope = mm2_p2p::encode_and_sign(&tx, &[1; 32]).unwrap();
        assert_ne!(envelope, tx);
        assert_eq!(tx_helper_transaction(&envelope).unwrap(), tx);

        // A bare transaction is not an envelope.
        assert!(tx_helper_transaction(&tx).is_err());
        // A changed byte of the signed payload fails the signature.
        let mut tampered = envelope.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(tx_helper_transaction(&tampered).is_err());
    }

    #[test]
    fn peer_healthcheck_topic_is_prefixed_by_peer() {
        assert_eq!(peer_healthcheck_topic("12D3KooWtest"), "peer_healthcheck/12D3KooWtest");
    }

    /// T-F3d: a relay sends the second copy of a priority message and of a message outside the orderbook topics, and
    /// not of any other orderbook message (CRD ch.32 R-F3b, D11).
    #[test]
    fn relay_sends_second_copy_for_priority_and_non_orderbook_messages() {
        // (priority, orderbook message, priority only)
        assert!(
            !relay_sends_second_copy(false, true, true),
            "ordinary orderbook message"
        );
        assert!(
            relay_sends_second_copy(true, true, true),
            "cancellation or swap negotiation"
        );
        assert!(
            relay_sends_second_copy(false, false, true),
            "swap, swap v2 or watcher message"
        );
        assert!(
            relay_sends_second_copy(true, false, true),
            "priority outside the orderbook topics"
        );
    }

    /// With `relay_second_copy_priority_only` off, a relay sends the second copy of every message, as a reference
    /// relay does (CRD ch.32 R-F3b, Compatibility note).
    #[test]
    fn relay_sends_second_copy_of_every_message_when_priority_only_is_off() {
        for priority in [false, true] {
            for orderbook_message in [false, true] {
                assert!(
                    relay_sends_second_copy(priority, orderbook_message, false),
                    "priority {priority}, orderbook message {orderbook_message}"
                );
            }
        }
    }

    /// The switch defaults to on; only a JSON `false` turns it off (CRD ch.32 R-F3b).
    #[test]
    fn relay_second_copy_priority_only_defaults_to_true() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        assert!(relay_second_copy_priority_only(&ctx), "absent");
        for (conf, expected) in [
            (json!({ "relay_second_copy_priority_only": true }), true),
            (json!({ "relay_second_copy_priority_only": false }), false),
            (json!({ "relay_second_copy_priority_only": "false" }), true),
            (json!({ "relay_second_copy_priority_only": null }), true),
        ] {
            let ctx = MmCtxBuilder::default().with_conf(conf.clone()).into_mm_arc();
            assert_eq!(relay_second_copy_priority_only(&ctx), expected, "{conf}");
        }
    }

    #[test]
    fn peer_connection_healthcheck_is_true_for_self_peer() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        ctx.peer_id.pin("12D3KooWself".to_string()).unwrap();

        let result = block_on(peer_connection_healthcheck(ctx, "12D3KooWself".to_string())).unwrap();
        assert!(result);
    }
}
