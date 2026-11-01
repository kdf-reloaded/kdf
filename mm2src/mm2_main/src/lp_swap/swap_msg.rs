use super::*;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum SwapMsg {
    Negotiation(NegotiationDataMsg),
    NegotiationReply(NegotiationDataMsg),
    Negotiated(bool),
    TakerFee(Vec<u8>),
    MakerPayment(Vec<u8>),
    TakerPayment(Vec<u8>),
}

#[derive(Debug, Default)]
pub struct SwapMsgStore {
    pub(crate) negotiation: Option<NegotiationDataMsg>,
    pub(crate) negotiation_reply: Option<NegotiationDataMsg>,
    pub(crate) negotiated: Option<bool>,
    pub(crate) taker_fee: Option<Vec<u8>>,
    pub(crate) maker_payment: Option<Vec<u8>>,
    pub(crate) taker_payment: Option<Vec<u8>>,
    pub(crate) accept_only_from: bits256,
}

impl SwapMsgStore {
    pub fn new(accept_only_from: bits256) -> Self {
        SwapMsgStore {
            accept_only_from,
            ..Default::default()
        }
    }
}

/// The AbortHandle that aborts on drop
pub struct AbortOnDropHandle(AbortHandle);

impl Drop for AbortOnDropHandle {
    fn drop(&mut self) { self.0.abort(); }
}

/// Spawns the loop that broadcasts message every `interval` seconds returning the AbortOnDropHandle
/// to stop it
pub fn broadcast_swap_message_every(
    ctx: MmArc,
    topic: String,
    msg: SwapMsg,
    interval: f64,
    p2p_privkey: Option<KeyPair>,
) -> AbortOnDropHandle {
    let fut = async move {
        loop {
            broadcast_swap_message(&ctx, topic.clone(), msg.clone(), &p2p_privkey);
            Timer::sleep(interval).await;
        }
    };
    let (abortable, abort_handle) = abortable(fut);
    spawn(abortable.unwrap_or_else(|_| ()));
    AbortOnDropHandle(abort_handle)
}

/// Broadcast the swap message once
pub fn broadcast_swap_message(ctx: &MmArc, topic: String, msg: SwapMsg, p2p_privkey: &Option<KeyPair>) {
    let (p2p_private, from) = match p2p_privkey {
        Some(keypair) => (keypair.private_bytes(), Some(keypair.libp2p_peer_id())),
        None => (ctx.secp256k1_key_pair().private().secret.take(), None),
    };
    let encoded_msg = encode_and_sign(&msg, &p2p_private).unwrap();
    broadcast_p2p_msg(ctx, vec![topic], encoded_msg, from);
}

/// Broadcast the tx message once
pub fn broadcast_p2p_tx_msg(ctx: &MmArc, topic: String, msg: &TransactionEnum, p2p_privkey: &Option<KeyPair>) {
    let (p2p_private, from) = match p2p_privkey {
        Some(keypair) => (keypair.private_bytes(), Some(keypair.libp2p_peer_id())),
        None => (ctx.secp256k1_key_pair().private().secret.take(), None),
    };

    let encoded_msg = encode_and_sign(&msg.tx_hex(), &p2p_private).unwrap();
    broadcast_p2p_msg(ctx, vec![topic], encoded_msg, from);
}

/// Spawns a loop that encodes, signs, and broadcasts a serializable message on `topic`
/// every `interval` seconds. Returns an `AbortOnDropHandle` to stop it.
pub fn broadcast_signed_msg_every<T: 'static + Serialize + Clone + Send>(
    ctx: MmArc,
    topic: String,
    msg: T,
    interval: f64,
    p2p_privkey: Option<KeyPair>,
) -> AbortOnDropHandle {
    let fut = async move {
        loop {
            let (p2p_private, from) = match &p2p_privkey {
                Some(keypair) => (keypair.private_bytes(), Some(keypair.libp2p_peer_id())),
                None => (ctx.secp256k1_key_pair().private().secret.take(), None),
            };
            if let Ok(encoded) = encode_and_sign(&msg, &p2p_private) {
                broadcast_p2p_msg(&ctx, vec![topic.clone()], encoded, from);
            }
            Timer::sleep(interval).await;
        }
    };
    let (abortable, abort_handle) = abortable(fut);
    spawn(abortable.unwrap_or_else(|_| ()));
    AbortOnDropHandle(abort_handle)
}

pub async fn process_msg(ctx: MmArc, topic: &str, msg: &[u8]) {
    let uuid = match Uuid::from_str(topic) {
        Ok(u) => u,
        Err(_) => return,
    };
    let msg = match decode_signed::<SwapMsg>(msg) {
        Ok(m) => m,
        Err(swap_msg_err) => {
            #[cfg(not(target_arch = "wasm32"))]
            match json::from_slice::<SwapStatus>(msg) {
                Ok(status) => {
                    if let Err(e) = save_stats_swap(&ctx, &status.data).await {
                        error!("Error saving the swap {} status: {}", status.data.uuid(), e);
                    }
                },
                Err(swap_status_err) => {
                    error!("Couldn't deserialize 'SwapMsg': {:?}", swap_msg_err);
                    error!("Couldn't deserialize 'SwapStatus': {:?}", swap_status_err);
                },
            };
            // Drop it to avoid dead_code warning
            drop(swap_msg_err);
            return;
        },
    };

    debug!("Processing swap msg {:?} for uuid {}", msg, uuid);
    let swap_ctx = SwapsContext::from_ctx(&ctx).unwrap();
    let mut msgs = swap_ctx.swap_msgs.lock().unwrap();
    if let Some(msg_store) = msgs.get_mut(&uuid) {
        if msg_store.accept_only_from.bytes == msg.2.unprefixed() {
            match msg.0 {
                SwapMsg::Negotiation(data) => msg_store.negotiation = Some(data),
                SwapMsg::NegotiationReply(data) => msg_store.negotiation_reply = Some(data),
                SwapMsg::Negotiated(negotiated) => msg_store.negotiated = Some(negotiated),
                SwapMsg::TakerFee(taker_fee) => msg_store.taker_fee = Some(taker_fee),
                SwapMsg::MakerPayment(maker_payment) => msg_store.maker_payment = Some(maker_payment),
                SwapMsg::TakerPayment(taker_payment) => msg_store.taker_payment = Some(taker_payment),
            }
        } else {
            warn!("Received message from unexpected sender for swap {}", uuid);
        }
    }
}

pub fn swap_topic(uuid: &Uuid) -> String { pub_sub_topic(SWAP_PREFIX, &uuid.to_string()) }

/// Formats and returns a topic format for `txhlp`.
///
/// # Usage
/// ```ignore
/// let topic = tx_helper_topic("BTC");
/// // Returns topic format `txhlp/BTC` as String type.
/// ```
#[inline(always)]
pub fn tx_helper_topic(coin: &str) -> String { pub_sub_topic(TX_HELPER_PREFIX, coin) }

/// Returns the P2P topic for a V2 swap: `swapv2/<uuid>`.
pub fn swap_v2_topic(uuid: &Uuid) -> String { pub_sub_topic(SWAP_V2_PREFIX, &uuid.to_string()) }

// ────────────────────────────────────────────────────────────────────────────
// V2 swap P2P messaging (protobuf / prost)
// ────────────────────────────────────────────────────────────────────────────

/// Broadcast a V2 swap protobuf message once on the given topic.
pub fn broadcast_swap_v2_message<T: prost::Message>(
    ctx: &MmArc,
    topic: String,
    msg: &T,
    p2p_keypair: &Option<KeyPair>,
) {
    use prost::Message;

    let (p2p_private, from) = match p2p_keypair {
        Some(kp) => (kp.private_bytes(), Some(kp.libp2p_peer_id())),
        None => (ctx.secp256k1_key_pair().private().secret.take(), None),
    };
    let encoded_msg = msg.encode_to_vec();

    let secp_secret = SecretKey::from_slice(&p2p_private).expect("valid secret key");
    let secp_message =
        secp256k1::Message::from_digest_slice(sha256(&encoded_msg).as_slice()).expect("sha256 is 32 bytes hash");
    let signature = SECP_SIGN.sign_ecdsa(&secp_message, &secp_secret);

    let signed_message = SignedMessage {
        from: PublicKey::from_secret_key(&*SECP_SIGN, &secp_secret).serialize().into(),
        signature: signature.serialize_compact().into(),
        payload: encoded_msg,
    };
    broadcast_p2p_msg(ctx, vec![topic], signed_message.encode_to_vec(), from);
}

/// Spawns the loop that broadcasts a protobuf message every `interval_sec` seconds,
/// returning the AbortOnDropHandle to stop it.
pub fn broadcast_swap_v2_msg_every<T: prost::Message + 'static>(
    ctx: MmArc,
    topic: String,
    msg: T,
    interval_sec: f64,
    p2p_keypair: Option<KeyPair>,
) -> AbortOnDropHandle {
    let fut = async move {
        loop {
            broadcast_swap_v2_message(&ctx, topic.clone(), &msg, &p2p_keypair);
            Timer::sleep(interval_sec).await;
        }
    };
    let (abortable, abort_handle) = abortable(fut);
    spawn(abortable.unwrap_or_else(|_| ()));
    AbortOnDropHandle(abort_handle)
}

/// Processes messages received during execution of the upgraded swap protocol.
///
/// Decodes the protobuf `SignedMessage` envelope, verifies the secp256k1 signature
/// and the sender identity, then routes the inner `SwapMessage` variant to the
/// appropriate slot in the per-swap `SwapV2MsgStore`.
pub fn process_swap_v2_msg(ctx: MmArc, topic: &str, msg: &[u8]) -> Result<(), String> {
    use prost::Message;

    let uuid = Uuid::from_str(topic).map_err(|e| format!("Invalid UUID in V2 swap topic: {}", e))?;

    let swap_ctx = SwapsContext::from_ctx(&ctx).map_err(|e| e.to_string())?;
    let mut msgs = swap_ctx.swap_v2_msgs.lock().unwrap();
    if let Some(msg_store) = msgs.get_mut(&uuid) {
        let signed_message =
            SignedMessage::decode(msg).map_err(|e| format!("Failed to decode SignedMessage: {}", e))?;

        let pubkey =
            PublicKey::from_slice(&signed_message.from).map_err(|e| format!("Invalid sender pubkey: {}", e))?;
        if pubkey != msg_store.accept_only_from {
            return Err(format!("Unexpected sender: {}", pubkey));
        }

        let signature =
            Signature::from_compact(&signed_message.signature).map_err(|e| format!("Invalid signature: {}", e))?;
        let secp_message = secp256k1::Message::from_digest_slice(sha256(&signed_message.payload).as_slice())
            .expect("sha256 is 32 bytes hash");

        SECP_VERIFY
            .verify_ecdsa(&secp_message, &signature, &pubkey)
            .map_err(|e| format!("Signature verification failed: {}", e))?;

        let swap_message = SwapMessage::decode(signed_message.payload.as_slice())
            .map_err(|e| format!("Failed to decode SwapMessage: {}", e))?;

        let uuid_from_message =
            Uuid::from_slice(&swap_message.swap_uuid).map_err(|e| format!("Invalid swap_uuid in message: {}", e))?;

        if uuid_from_message != uuid {
            return Err(format!(
                "uuid from message {} doesn't match uuid from topic {}",
                uuid_from_message, uuid
            ));
        }

        debug!("Processing swap v2 msg {:?} for uuid {}", swap_message, uuid);
        match swap_message.inner {
            Some(swap_message::Inner::MakerNegotiation(data)) => msg_store.maker_negotiation = Some(data),
            Some(swap_message::Inner::TakerNegotiation(data)) => msg_store.taker_negotiation = Some(data),
            Some(swap_message::Inner::MakerNegotiated(data)) => msg_store.maker_negotiated = Some(data),
            Some(swap_message::Inner::TakerFundingInfo(data)) => msg_store.taker_funding = Some(data),
            Some(swap_message::Inner::MakerPaymentInfo(data)) => msg_store.maker_payment = Some(data),
            Some(swap_message::Inner::TakerPaymentInfo(data)) => msg_store.taker_payment = Some(data),
            Some(swap_message::Inner::TakerPaymentSpendPreimage(data)) => {
                msg_store.taker_payment_spend_preimage = Some(data)
            },
            None => return Err("swap_message.inner is None".into()),
        }
    }
    Ok(())
}

/// Wait for a specific V2 swap message, polling the per-swap store.
///
/// The `getter` closure extracts the desired message from the store and returns
/// `Some(T)` once it arrives. Returns `Err` on timeout.
pub async fn recv_swap_v2_msg<T>(
    ctx: MmArc,
    mut getter: impl FnMut(&mut SwapV2MsgStore) -> Option<T>,
    uuid: &Uuid,
    timeout: u64,
) -> Result<T, String> {
    let started = now_ms() / 1000;
    let timeout = BASIC_COMM_TIMEOUT + timeout;
    let wait_until = started + timeout;
    loop {
        Timer::sleep(1.).await;
        let swap_ctx = SwapsContext::from_ctx(&ctx).unwrap();
        let mut msgs = swap_ctx.swap_v2_msgs.lock().unwrap();
        if let Some(store) = msgs.get_mut(uuid) {
            if let Some(msg) = getter(store) {
                return Ok(msg);
            }
        }
        let now = now_ms() / 1000;
        if now > wait_until {
            return ERR!("V2 swap msg timeout ({} > {})", now - started, timeout);
        }
    }
}
