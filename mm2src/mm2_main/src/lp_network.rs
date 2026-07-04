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
use mm2_p2p::{decode_message, decode_signed, encode_and_sign, encode_message, pub_sub_topic, DecodingError,
              GossipsubMessage, Libp2pPublic, Libp2pSecpPublic, MessageId, NetworkPorts, PeerId, TOPIC_SEPARATOR};
#[cfg(test)] use mocktopus::macros::*;
use parking_lot::Mutex as PaMutex;
use rand::random;
use serde::de;
use std::collections::HashMap;
use std::net::ToSocketAddrs;
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
    ResponseError(String),
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

pub struct P2PContext {
    /// Using Mutex helps to prevent cloning which can actually result to channel being unbounded in case of using 1 tx clone per 1 message.
    pub cmd_tx: PaMutex<AdexCmdTx>,
    pub(crate) pending_healthchecks: PaMutex<HashMap<String, PendingHealthcheckWaiter>>,
}

#[cfg_attr(test, mockable)]
impl P2PContext {
    pub fn new(cmd_tx: AdexCmdTx) -> Self {
        P2PContext {
            cmd_tx: PaMutex::new(cmd_tx),
            pending_healthchecks: PaMutex::new(HashMap::new()),
        }
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
            None => break,
            _ => (),
        }
    }
}

async fn process_p2p_message(
    ctx: MmArc,
    peer_id: PeerId,
    message_id: MessageId,
    message: GossipsubMessage,
    i_am_relay: bool,
) {
    let mut to_propagate = false;
    let mut orderbook_pairs = vec![];

    for topic in message.topics {
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
                        match coin.send_raw_tx_bytes(&message.data).compat().await {
                            Ok(id) => log::debug!("Transaction broadcasted successfully: {:?} ", id),
                            Err(e) => log::error!("Broadcast transaction failed. {}", e),
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

    if !orderbook_pairs.is_empty() {
        let process_fut = lp_ordermatch::process_msg(
            ctx.clone(),
            orderbook_pairs,
            peer_id.to_string(),
            &message.data,
            i_am_relay,
        );

        if process_fut.await {
            to_propagate = true;
        }
    }

    if to_propagate && i_am_relay {
        propagate_message(&ctx, message_id, peer_id);
    }
}

fn process_p2p_request(
    ctx: MmArc,
    _peer_id: PeerId,
    request: Vec<u8>,
    response_channel: AdexResponseChannel,
) -> P2PRequestResult<()> {
    let mut decode_error = None;
    let res = match decode_message::<P2PRequest>(&request) {
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

    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd = AdexBehaviourCmd::SendResponse { res, response_channel };
    p2p_ctx
        .cmd_tx
        .lock()
        .try_send(cmd)
        .map_to_mm(|e| P2PRequestError::SendError(e.to_string()))?;

    if let Some(error) = decode_error {
        return MmError::err(P2PRequestError::DecodeError(error));
    }

    Ok(())
}

pub fn broadcast_p2p_msg(ctx: &MmArc, topics: Vec<String>, msg: Vec<u8>, from: Option<PeerId>) {
    let ctx = ctx.clone();
    let cmd = match from {
        Some(from) => AdexBehaviourCmd::PublishMsgFrom { topics, msg, from },
        None => AdexBehaviourCmd::PublishMsg { topics, msg },
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
        .map_to_mm(|e| P2PRequestError::ResponseError(e.to_string()))?
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
    Err(String),
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
        .map_to_mm(|e| P2PRequestError::ResponseError(e.to_string()))?;
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
        .map_to_mm(|e| P2PRequestError::ResponseError(e.to_string()))?;
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
                    Err(e) => PeerDecodedResponse::Err(ERRL!("{}", e)),
                },
                AdexResponse::None => PeerDecodedResponse::None,
                AdexResponse::Err { error } => PeerDecodedResponse::Err(error),
            };
            (peer_id, res)
        })
        .collect()
}

pub fn propagate_message(ctx: &MmArc, message_id: MessageId, propagation_source: PeerId) {
    let ctx = ctx.clone();
    let p2p_ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd = AdexBehaviourCmd::PropagateMessage {
        message_id,
        propagation_source,
    };
    if let Err(e) = p2p_ctx.cmd_tx.lock().try_send(cmd) {
        log::error!("propagate_message cmd_tx.send error {:?}", e);
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
    let public_key = Libp2pSecpPublic::decode(secp_public)?;
    Ok(PeerId::from_public_key(&Libp2pPublic::Secp256k1(public_key)))
}

#[cfg(test)]
mod tests {
    use super::{peer_connection_healthcheck, peer_healthcheck_topic};
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;

    #[test]
    fn peer_healthcheck_topic_is_prefixed_by_peer() {
        assert_eq!(peer_healthcheck_topic("12D3KooWtest"), "peer_healthcheck/12D3KooWtest");
    }

    #[test]
    fn peer_connection_healthcheck_is_true_for_self_peer() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        ctx.peer_id.pin("12D3KooWself".to_string()).unwrap();

        let result = block_on(peer_connection_healthcheck(ctx, "12D3KooWself".to_string())).unwrap();
        assert!(result);
    }
}
