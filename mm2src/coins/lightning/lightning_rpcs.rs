// lightning_rpcs — RPC handler functions and their request/response types
use super::*;

#[derive(Deserialize)]
pub struct ConnectToNodeRequest {
    pub coin: String,
    pub node_address: NodeAddress,
}

/// Connect to a certain node on the lightning network.
pub async fn connect_to_lightning_node(ctx: MmArc, req: ConnectToNodeRequest) -> ConnectToNodeResult<String> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(ConnectToNodeError::UnsupportedCoin(coin.ticker().to_string())),
    };

    let node_pubkey = req.node_address.pubkey;
    let node_addr = req.node_address.addr;
    let res = connect_to_node(node_pubkey, node_addr, ln_coin.peer_manager.clone()).await?;

    // If a node that we have an open channel with changed it's address, "connect_to_lightning_node"
    // can be used to reconnect to the new address while saving this new address for reconnections.
    if let ConnectToNodeRes::ConnectedSuccessfully { .. } = res {
        if let Entry::Occupied(mut entry) = ln_coin.open_channels_nodes.lock().entry(node_pubkey) {
            entry.insert(node_addr);
        }
        ln_coin
            .persister
            .save_nodes_addresses(ln_coin.open_channels_nodes)
            .await?;
    }

    Ok(res.to_string())
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value")]
pub enum ChannelOpenAmount {
    Exact(BigDecimal),
    Max,
}

#[derive(Deserialize)]
pub struct OpenChannelRequest {
    pub coin: String,
    pub node_address: NodeAddress,
    pub amount: ChannelOpenAmount,
    /// The amount to push to the counterparty as part of the open, in milli-satoshi. Creates inbound liquidity for the channel.
    /// By setting push_msat to a value, opening channel request will be equivalent to opening a channel then sending a payment with
    /// the push_msat amount.
    #[serde(default)]
    pub push_msat: u64,
    pub channel_options: Option<ChannelOptions>,
    pub counterparty_locktime: Option<u16>,
    pub our_htlc_minimum_msat: Option<u64>,
}

#[derive(Serialize)]
pub struct OpenChannelResponse {
    rpc_channel_id: u64,
    node_address: NodeAddress,
}

/// Opens a channel on the lightning network.
pub async fn open_channel(ctx: MmArc, req: OpenChannelRequest) -> OpenChannelResult<OpenChannelResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(OpenChannelError::UnsupportedCoin(coin.ticker().to_string())),
    };

    // Making sure that the node data is correct and that we can connect to it before doing more operations
    let node_pubkey = req.node_address.pubkey;
    let node_addr = req.node_address.addr;
    connect_to_node(node_pubkey, node_addr, ln_coin.peer_manager.clone())
        .await
        .mm_err(Into::into)?;

    let platform_coin = ln_coin.platform_coin().clone();
    let decimals = platform_coin.as_ref().decimals;
    let my_address = platform_coin
        .as_ref()
        .derivation_method
        .iguana_or_err()
        .mm_err(Into::into)?;
    let (unspents, _) = platform_coin
        .get_unspent_ordered_list(my_address)
        .await
        .mm_err(Into::into)?;
    let (value, fee_policy) = match req.amount.clone() {
        ChannelOpenAmount::Max => (
            unspents.iter().fold(0, |sum, unspent| sum + unspent.value),
            FeePolicy::DeductFromOutput(0),
        ),
        ChannelOpenAmount::Exact(v) => {
            let value = sat_from_big_decimal(&v, decimals).mm_err(Into::into)?;
            (value, FeePolicy::SendExact)
        },
    };

    // The actual script_pubkey will replace this before signing the transaction after receiving the required
    // output script from the other node when the channel is accepted
    let script_pubkey =
        Builder::build_witness_script(&AddressHashEnum::WitnessScriptHash(Default::default())).to_bytes();
    let outputs = vec![TransactionOutput { value, script_pubkey }];

    let mut tx_builder = UtxoTxBuilder::new(&platform_coin)
        .add_available_inputs(unspents)
        .add_outputs(outputs)
        .with_fee_policy(fee_policy);

    let fee = platform_coin
        .get_tx_fee()
        .await
        .map_err(|e| OpenChannelError::RpcError(e.to_string()))?;
    tx_builder = tx_builder.with_fee(fee);

    let (unsigned, _) = tx_builder.build().await.mm_err(Into::into)?;

    let amount_in_sat = unsigned.outputs[0].value;
    let push_msat = req.push_msat;
    let channel_manager = ln_coin.channel_manager.clone();

    let mut conf = ln_coin.conf.clone();
    if let Some(options) = req.channel_options {
        match conf.channel_options.as_mut() {
            Some(o) => o.update(options),
            None => conf.channel_options = Some(options),
        }
    }

    let mut user_config: UserConfig = conf.into();
    if let Some(locktime) = req.counterparty_locktime {
        user_config.own_channel_config.our_to_self_delay = locktime;
    }
    if let Some(min) = req.our_htlc_minimum_msat {
        user_config.own_channel_config.our_htlc_minimum_msat = min;
    }

    let rpc_channel_id = ln_coin.persister.get_last_channel_rpc_id().await? as u64 + 1;

    let temp_channel_id = async_blocking(move || {
        channel_manager
            .create_channel(node_pubkey, amount_in_sat, push_msat, rpc_channel_id, Some(user_config))
            .map_to_mm(|e| OpenChannelError::FailureToOpenChannel(node_pubkey.to_string(), format!("{:?}", e)))
    })
    .await?;

    {
        let mut unsigned_funding_txs = ln_coin.platform.unsigned_funding_txs.lock();
        unsigned_funding_txs.insert(rpc_channel_id, unsigned);
    }

    let pending_channel_details = SqlChannelDetails::new(
        rpc_channel_id,
        temp_channel_id,
        node_pubkey,
        true,
        user_config.channel_options.announced_channel,
    );

    // Saving node data to reconnect to it on restart
    ln_coin.open_channels_nodes.lock().insert(node_pubkey, node_addr);
    ln_coin
        .persister
        .save_nodes_addresses(ln_coin.open_channels_nodes)
        .await?;

    ln_coin.persister.add_channel_to_db(pending_channel_details).await?;

    Ok(OpenChannelResponse {
        rpc_channel_id,
        node_address: req.node_address,
    })
}

#[derive(Deserialize)]
pub struct OpenChannelsFilter {
    pub channel_id: Option<H256Json>,
    pub counterparty_node_id: Option<PublicKeyForRPC>,
    pub funding_tx: Option<H256Json>,
    pub from_funding_value_sats: Option<u64>,
    pub to_funding_value_sats: Option<u64>,
    pub is_outbound: Option<bool>,
    pub from_balance_msat: Option<u64>,
    pub to_balance_msat: Option<u64>,
    pub from_outbound_capacity_msat: Option<u64>,
    pub to_outbound_capacity_msat: Option<u64>,
    pub from_inbound_capacity_msat: Option<u64>,
    pub to_inbound_capacity_msat: Option<u64>,
    pub confirmed: Option<bool>,
    pub is_usable: Option<bool>,
    pub is_public: Option<bool>,
}

pub(crate) fn apply_open_channel_filter(channel_details: &ChannelDetailsForRPC, filter: &OpenChannelsFilter) -> bool {
    let is_channel_id = filter.channel_id.is_none() || Some(&channel_details.channel_id) == filter.channel_id.as_ref();

    let is_counterparty_node_id = filter.counterparty_node_id.is_none()
        || Some(&channel_details.counterparty_node_id) == filter.counterparty_node_id.as_ref();

    let is_funding_tx = filter.funding_tx.is_none() || channel_details.funding_tx == filter.funding_tx;

    let is_from_funding_value_sats =
        Some(&channel_details.funding_tx_value_sats) >= filter.from_funding_value_sats.as_ref();

    let is_to_funding_value_sats = filter.to_funding_value_sats.is_none()
        || Some(&channel_details.funding_tx_value_sats) <= filter.to_funding_value_sats.as_ref();

    let is_outbound = filter.is_outbound.is_none() || Some(&channel_details.is_outbound) == filter.is_outbound.as_ref();

    let is_from_balance_msat = Some(&channel_details.balance_msat) >= filter.from_balance_msat.as_ref();

    let is_to_balance_msat =
        filter.to_balance_msat.is_none() || Some(&channel_details.balance_msat) <= filter.to_balance_msat.as_ref();

    let is_from_outbound_capacity_msat =
        Some(&channel_details.outbound_capacity_msat) >= filter.from_outbound_capacity_msat.as_ref();

    let is_to_outbound_capacity_msat = filter.to_outbound_capacity_msat.is_none()
        || Some(&channel_details.outbound_capacity_msat) <= filter.to_outbound_capacity_msat.as_ref();

    let is_from_inbound_capacity_msat =
        Some(&channel_details.inbound_capacity_msat) >= filter.from_inbound_capacity_msat.as_ref();

    let is_to_inbound_capacity_msat = filter.to_inbound_capacity_msat.is_none()
        || Some(&channel_details.inbound_capacity_msat) <= filter.to_inbound_capacity_msat.as_ref();

    let is_confirmed = filter.confirmed.is_none() || Some(&channel_details.confirmed) == filter.confirmed.as_ref();

    let is_usable = filter.is_usable.is_none() || Some(&channel_details.is_usable) == filter.is_usable.as_ref();

    let is_public = filter.is_public.is_none() || Some(&channel_details.is_public) == filter.is_public.as_ref();

    is_channel_id
        && is_counterparty_node_id
        && is_funding_tx
        && is_from_funding_value_sats
        && is_to_funding_value_sats
        && is_outbound
        && is_from_balance_msat
        && is_to_balance_msat
        && is_from_outbound_capacity_msat
        && is_to_outbound_capacity_msat
        && is_from_inbound_capacity_msat
        && is_to_inbound_capacity_msat
        && is_confirmed
        && is_usable
        && is_public
}

#[derive(Deserialize)]
pub struct ListOpenChannelsRequest {
    pub coin: String,
    pub filter: Option<OpenChannelsFilter>,
    #[serde(default = "ten")]
    limit: usize,
    #[serde(default)]
    paging_options: PagingOptionsEnum<u64>,
}

#[derive(Clone, Serialize)]
pub struct ChannelDetailsForRPC {
    pub rpc_channel_id: u64,
    pub channel_id: H256Json,
    pub counterparty_node_id: PublicKeyForRPC,
    pub funding_tx: Option<H256Json>,
    pub funding_tx_output_index: Option<u16>,
    pub funding_tx_value_sats: u64,
    /// True if the channel was initiated (and thus funded) by us.
    pub is_outbound: bool,
    pub balance_msat: u64,
    pub outbound_capacity_msat: u64,
    pub inbound_capacity_msat: u64,
    // Channel is confirmed onchain, this means that funding_locked messages have been exchanged,
    // the channel is not currently being shut down, and the required confirmation count has been reached.
    pub confirmed: bool,
    // Channel is confirmed and funding_locked messages have been exchanged, the peer is connected,
    // and the channel is not currently negotiating a shutdown.
    pub is_usable: bool,
    // A publicly-announced channel.
    pub is_public: bool,
}

impl From<ChannelDetails> for ChannelDetailsForRPC {
    fn from(details: ChannelDetails) -> ChannelDetailsForRPC {
        ChannelDetailsForRPC {
            rpc_channel_id: details.user_channel_id,
            channel_id: details.channel_id.into(),
            counterparty_node_id: PublicKeyForRPC(details.counterparty.node_id),
            funding_tx: details.funding_txo.map(|tx| h256_json_from_txid(tx.txid)),
            funding_tx_output_index: details.funding_txo.map(|tx| tx.index),
            funding_tx_value_sats: details.channel_value_satoshis,
            is_outbound: details.is_outbound,
            balance_msat: details.balance_msat,
            outbound_capacity_msat: details.outbound_capacity_msat,
            inbound_capacity_msat: details.inbound_capacity_msat,
            confirmed: details.is_funding_locked,
            is_usable: details.is_usable,
            is_public: details.is_public,
        }
    }
}

pub(crate) struct GetOpenChannelsResult {
    pub channels: Vec<ChannelDetailsForRPC>,
    pub skipped: usize,
    pub total: usize,
}

#[derive(Serialize)]
pub struct ListOpenChannelsResponse {
    open_channels: Vec<ChannelDetailsForRPC>,
    limit: usize,
    skipped: usize,
    total: usize,
    total_pages: usize,
    paging_options: PagingOptionsEnum<u64>,
}

pub async fn list_open_channels_by_filter(
    ctx: MmArc,
    req: ListOpenChannelsRequest,
) -> ListChannelsResult<ListOpenChannelsResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(ListChannelsError::UnsupportedCoin(coin.ticker().to_string())),
    };

    let result = ln_coin
        .get_open_channels_by_filter(req.filter, req.paging_options.clone(), req.limit)
        .await?;

    Ok(ListOpenChannelsResponse {
        open_channels: result.channels,
        limit: req.limit,
        skipped: result.skipped,
        total: result.total,
        total_pages: calc_total_pages(result.total, req.limit),
        paging_options: req.paging_options,
    })
}

#[derive(Deserialize)]
pub struct ListClosedChannelsRequest {
    pub coin: String,
    pub filter: Option<ClosedChannelsFilter>,
    #[serde(default = "ten")]
    limit: usize,
    #[serde(default)]
    paging_options: PagingOptionsEnum<u64>,
}

#[derive(Serialize)]
pub struct ListClosedChannelsResponse {
    closed_channels: Vec<SqlChannelDetails>,
    limit: usize,
    skipped: usize,
    total: usize,
    total_pages: usize,
    paging_options: PagingOptionsEnum<u64>,
}

pub async fn list_closed_channels_by_filter(
    ctx: MmArc,
    req: ListClosedChannelsRequest,
) -> ListChannelsResult<ListClosedChannelsResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(ListChannelsError::UnsupportedCoin(coin.ticker().to_string())),
    };
    let closed_channels_res = ln_coin
        .persister
        .get_closed_channels_by_filter(req.filter, req.paging_options.clone(), req.limit)
        .await?;

    Ok(ListClosedChannelsResponse {
        closed_channels: closed_channels_res.channels,
        limit: req.limit,
        skipped: closed_channels_res.skipped,
        total: closed_channels_res.total,
        total_pages: calc_total_pages(closed_channels_res.total, req.limit),
        paging_options: req.paging_options,
    })
}

#[derive(Deserialize)]
pub struct GetChannelDetailsRequest {
    pub coin: String,
    pub rpc_channel_id: u64,
}

#[derive(Serialize)]
#[serde(tag = "status", content = "details")]
pub enum GetChannelDetailsResponse {
    Open(ChannelDetailsForRPC),
    Closed(SqlChannelDetails),
}

pub async fn get_channel_details(
    ctx: MmArc,
    req: GetChannelDetailsRequest,
) -> GetChannelDetailsResult<GetChannelDetailsResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(GetChannelDetailsError::UnsupportedCoin(coin.ticker().to_string())),
    };
    let channel_details = match ln_coin
        .channel_manager
        .list_channels()
        .into_iter()
        .find(|chan| chan.user_channel_id == req.rpc_channel_id)
    {
        Some(details) => GetChannelDetailsResponse::Open(details.into()),
        None => GetChannelDetailsResponse::Closed(
            ln_coin
                .persister
                .get_channel_from_db(req.rpc_channel_id)
                .await?
                .ok_or(GetChannelDetailsError::NoSuchChannel(req.rpc_channel_id))?,
        ),
    };

    Ok(channel_details)
}

#[derive(Deserialize)]
pub struct GenerateInvoiceRequest {
    pub coin: String,
    pub amount_in_msat: Option<u64>,
    pub description: String,
}

#[derive(Serialize)]
pub struct GenerateInvoiceResponse {
    payment_hash: H256Json,
    invoice: InvoiceForRPC,
}

/// Generates an invoice (request for payment) that can be paid on the lightning network by another node using send_payment.
pub async fn generate_invoice(
    ctx: MmArc,
    req: GenerateInvoiceRequest,
) -> GenerateInvoiceResult<GenerateInvoiceResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(GenerateInvoiceError::UnsupportedCoin(coin.ticker().to_string())),
    };
    let open_channels_nodes = ln_coin.open_channels_nodes.lock().clone();
    for (node_pubkey, node_addr) in open_channels_nodes {
        connect_to_node(node_pubkey, node_addr, ln_coin.peer_manager.clone())
            .await
            .error_log_with_msg(&format!(
                "Channel with node: {} can't be used for invoice routing hints due to connection error.",
                node_pubkey
            ));
    }
    let network = ln_coin.platform.network.clone().into();
    let invoice = create_invoice_from_channelmanager(
        &ln_coin.channel_manager,
        ln_coin.keys_manager,
        network,
        req.amount_in_msat,
        req.description.clone(),
    )?;
    let payment_hash = invoice.payment_hash().into_inner();
    let payment_info = PaymentInfo {
        payment_hash: PaymentHash(payment_hash),
        payment_type: PaymentType::InboundPayment,
        description: req.description,
        preimage: None,
        secret: Some(*invoice.payment_secret()),
        amt_msat: req.amount_in_msat,
        fee_paid_msat: None,
        status: HTLCStatus::Pending,
        created_at: now_ms() / 1000,
        last_updated: now_ms() / 1000,
    };
    ln_coin.persister.add_or_update_payment_in_db(payment_info).await?;
    Ok(GenerateInvoiceResponse {
        payment_hash: payment_hash.into(),
        invoice: invoice.into(),
    })
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Payment {
    #[serde(rename = "invoice")]
    Invoice { invoice: InvoiceForRPC },
    #[serde(rename = "keysend")]
    Keysend {
        // The recieving node pubkey (node ID)
        destination: PublicKeyForRPC,
        // Amount to send in millisatoshis
        amount_in_msat: u64,
        // The number of blocks the payment will be locked for if not claimed by the destination,
        // It's can be assumed that 6 blocks = 1 hour. We can claim the payment amount back after this cltv expires.
        // Minmum value allowed is MIN_FINAL_CLTV_EXPIRY which is currently 24 for rust-lightning.
        expiry: u32,
    },
}

#[derive(Deserialize)]
pub struct SendPaymentReq {
    pub coin: String,
    pub payment: Payment,
}

#[derive(Serialize)]
pub struct SendPaymentResponse {
    payment_hash: H256Json,
}

pub async fn send_payment(ctx: MmArc, req: SendPaymentReq) -> SendPaymentResult<SendPaymentResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(SendPaymentError::UnsupportedCoin(coin.ticker().to_string())),
    };
    let open_channels_nodes = ln_coin.open_channels_nodes.lock().clone();
    for (node_pubkey, node_addr) in open_channels_nodes {
        connect_to_node(node_pubkey, node_addr, ln_coin.peer_manager.clone())
            .await
            .error_log_with_msg(&format!(
                "Channel with node: {} can't be used to route this payment due to connection error.",
                node_pubkey
            ));
    }
    let payment_info = match req.payment {
        Payment::Invoice { invoice } => ln_coin.pay_invoice(invoice.into())?,
        Payment::Keysend {
            destination,
            amount_in_msat,
            expiry,
        } => ln_coin.keysend(destination.into(), amount_in_msat, expiry)?,
    };
    ln_coin
        .persister
        .add_or_update_payment_in_db(payment_info.clone())
        .await?;
    Ok(SendPaymentResponse {
        payment_hash: payment_info.payment_hash.0.into(),
    })
}

#[derive(Deserialize)]
pub struct PaymentsFilterForRPC {
    pub payment_type: Option<PaymentTypeForRPC>,
    pub description: Option<String>,
    pub status: Option<HTLCStatus>,
    pub from_amount_msat: Option<u64>,
    pub to_amount_msat: Option<u64>,
    pub from_fee_paid_msat: Option<u64>,
    pub to_fee_paid_msat: Option<u64>,
    pub from_timestamp: Option<u64>,
    pub to_timestamp: Option<u64>,
}

impl From<PaymentsFilterForRPC> for PaymentsFilter {
    fn from(filter: PaymentsFilterForRPC) -> Self {
        PaymentsFilter {
            payment_type: filter.payment_type.map(From::from),
            description: filter.description,
            status: filter.status,
            from_amount_msat: filter.from_amount_msat,
            to_amount_msat: filter.to_amount_msat,
            from_fee_paid_msat: filter.from_fee_paid_msat,
            to_fee_paid_msat: filter.to_fee_paid_msat,
            from_timestamp: filter.from_timestamp,
            to_timestamp: filter.to_timestamp,
        }
    }
}

#[derive(Deserialize)]
pub struct ListPaymentsReq {
    pub coin: String,
    pub filter: Option<PaymentsFilterForRPC>,
    #[serde(default = "ten")]
    limit: usize,
    #[serde(default)]
    paging_options: PagingOptionsEnum<H256Json>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum PaymentTypeForRPC {
    #[serde(rename = "Outbound Payment")]
    OutboundPayment { destination: PublicKeyForRPC },
    #[serde(rename = "Inbound Payment")]
    InboundPayment,
}

impl From<PaymentType> for PaymentTypeForRPC {
    fn from(payment_type: PaymentType) -> Self {
        match payment_type {
            PaymentType::OutboundPayment { destination } => PaymentTypeForRPC::OutboundPayment {
                destination: PublicKeyForRPC(destination),
            },
            PaymentType::InboundPayment => PaymentTypeForRPC::InboundPayment,
        }
    }
}

impl From<PaymentTypeForRPC> for PaymentType {
    fn from(payment_type: PaymentTypeForRPC) -> Self {
        match payment_type {
            PaymentTypeForRPC::OutboundPayment { destination } => PaymentType::OutboundPayment {
                destination: destination.into(),
            },
            PaymentTypeForRPC::InboundPayment => PaymentType::InboundPayment,
        }
    }
}

#[derive(Serialize)]
pub struct PaymentInfoForRPC {
    payment_hash: H256Json,
    payment_type: PaymentTypeForRPC,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    amount_in_msat: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fee_paid_msat: Option<u64>,
    status: HTLCStatus,
    created_at: u64,
    last_updated: u64,
}

impl From<PaymentInfo> for PaymentInfoForRPC {
    fn from(info: PaymentInfo) -> Self {
        PaymentInfoForRPC {
            payment_hash: info.payment_hash.0.into(),
            payment_type: info.payment_type.into(),
            description: info.description,
            amount_in_msat: info.amt_msat,
            fee_paid_msat: info.fee_paid_msat,
            status: info.status,
            created_at: info.created_at,
            last_updated: info.last_updated,
        }
    }
}

#[derive(Serialize)]
pub struct ListPaymentsResponse {
    payments: Vec<PaymentInfoForRPC>,
    limit: usize,
    skipped: usize,
    total: usize,
    total_pages: usize,
    paging_options: PagingOptionsEnum<H256Json>,
}

pub async fn list_payments_by_filter(ctx: MmArc, req: ListPaymentsReq) -> ListPaymentsResult<ListPaymentsResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(ListPaymentsError::UnsupportedCoin(coin.ticker().to_string())),
    };
    let get_payments_res = ln_coin
        .persister
        .get_payments_by_filter(
            req.filter.map(From::from),
            req.paging_options.clone().map(|h| PaymentHash(h.0)),
            req.limit,
        )
        .await?;

    Ok(ListPaymentsResponse {
        payments: get_payments_res.payments.into_iter().map(From::from).collect(),
        limit: req.limit,
        skipped: get_payments_res.skipped,
        total: get_payments_res.total,
        total_pages: calc_total_pages(get_payments_res.total, req.limit),
        paging_options: req.paging_options,
    })
}

#[derive(Deserialize)]
pub struct GetPaymentDetailsRequest {
    pub coin: String,
    pub payment_hash: H256Json,
}

#[derive(Serialize)]
pub struct GetPaymentDetailsResponse {
    payment_details: PaymentInfoForRPC,
}

pub async fn get_payment_details(
    ctx: MmArc,
    req: GetPaymentDetailsRequest,
) -> GetPaymentDetailsResult<GetPaymentDetailsResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(GetPaymentDetailsError::UnsupportedCoin(coin.ticker().to_string())),
    };

    if let Some(payment_info) = ln_coin
        .persister
        .get_payment_from_db(PaymentHash(req.payment_hash.0))
        .await?
    {
        return Ok(GetPaymentDetailsResponse {
            payment_details: payment_info.into(),
        });
    }

    MmError::err(GetPaymentDetailsError::NoSuchPayment(req.payment_hash))
}

#[derive(Deserialize)]
pub struct CloseChannelReq {
    pub coin: String,
    pub channel_id: H256Json,
    #[serde(default)]
    pub force_close: bool,
}

pub async fn close_channel(ctx: MmArc, req: CloseChannelReq) -> CloseChannelResult<String> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(CloseChannelError::UnsupportedCoin(coin.ticker().to_string())),
    };
    if req.force_close {
        ln_coin
            .channel_manager
            .force_close_channel(&req.channel_id.0)
            .map_to_mm(|e| CloseChannelError::CloseChannelError(format!("{:?}", e)))?;
    } else {
        ln_coin
            .channel_manager
            .close_channel(&req.channel_id.0)
            .map_to_mm(|e| CloseChannelError::CloseChannelError(format!("{:?}", e)))?;
    }

    Ok(format!("Initiated closing of channel: {:?}", req.channel_id))
}

#[derive(Deserialize)]
pub struct UpdateChannelReq {
    pub coin: String,
    pub rpc_channel_id: u64,
    pub channel_options: ChannelOptions,
}

/// The publicly exposed subset of per-channel options echoed back by `update_channel`.
/// Only the five forwarding-policy / fee parameters are part of the public contract; the
/// internal `force_close_avoidance_max_fee_satoshis` field is exposed on the wire under the
/// shorter `force_close_avoidance_max_fee_sats` name.
#[derive(Serialize)]
pub struct ChannelOptionsForRPC {
    pub proportional_fee_in_millionths_sats: Option<u32>,
    pub base_fee_msat: Option<u32>,
    pub cltv_expiry_delta: Option<u16>,
    pub max_dust_htlc_exposure_msat: Option<u64>,
    pub force_close_avoidance_max_fee_sats: Option<u64>,
}

impl From<ChannelOptions> for ChannelOptionsForRPC {
    fn from(options: ChannelOptions) -> Self {
        ChannelOptionsForRPC {
            proportional_fee_in_millionths_sats: options.proportional_fee_in_millionths_sats,
            base_fee_msat: options.base_fee_msat,
            cltv_expiry_delta: options.cltv_expiry_delta,
            max_dust_htlc_exposure_msat: options.max_dust_htlc_exposure_msat,
            force_close_avoidance_max_fee_sats: options.force_close_avoidance_max_fee_sats,
        }
    }
}

#[derive(Serialize)]
pub struct UpdateChannelResponse {
    channel_options: ChannelOptionsForRPC,
}

/// Mutates the configurable parameters (fees, CLTV delta, dust cap, force-close fee ceiling) of a
/// single live channel on the running channel manager and re-advertises the channel's forwarding
/// policy. The effective option set (coin defaults overlaid with the request) is applied,
/// persisted, and echoed back.
pub async fn update_channel(ctx: MmArc, req: UpdateChannelReq) -> UpdateChannelResult<UpdateChannelResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(UpdateChannelError::UnsupportedCoin(coin.ticker().to_string())),
    };

    // Resolve the channel to its live counterparty node id and channel id.
    let channel_details = ln_coin
        .channel_manager
        .list_channels()
        .into_iter()
        .find(|chan| chan.user_channel_id == req.rpc_channel_id)
        .ok_or(UpdateChannelError::NoSuchChannel(req.rpc_channel_id))?;
    let counterparty_node_id = channel_details.counterparty.node_id;
    let channel_id = channel_details.channel_id;

    // Merge base: the coin's configured channel-option defaults, overlaid with the request
    // options. Where the coin carries no defaults, the request itself is the base.
    let mut channel_options = ln_coin
        .conf
        .channel_options
        .clone()
        .unwrap_or_else(|| req.channel_options.clone());
    channel_options.update(req.channel_options);

    let channel_config: ChannelConfig = channel_options.clone().into();
    let channel_manager = ln_coin.channel_manager.clone();
    async_blocking(move || {
        channel_manager
            .update_channel_config(&counterparty_node_id, &[channel_id], &channel_config)
            .map_to_mm(|e| UpdateChannelError::FailureToUpdateChannel(format!("{:?}", e)))
    })
    .await?;

    Ok(UpdateChannelResponse {
        channel_options: channel_options.into(),
    })
}

/// Details about the balance(s) available for spending once the channel appears on chain.
#[derive(Serialize)]
pub enum ClaimableBalance {
    /// The channel is not yet closed (or the commitment or closing transaction has not yet
    /// appeared in a block). The given balance is claimable (less on-chain fees) if the channel is
    /// force-closed now.
    ClaimableOnChannelClose {
        /// The amount available to claim, in satoshis, excluding the on-chain fees which will be
        /// required to do so.
        claimable_amount_satoshis: u64,
    },
    /// The channel has been closed, and the given balance is ours but awaiting confirmations until
    /// we consider it spendable.
    ClaimableAwaitingConfirmations {
        /// The amount available to claim, in satoshis, possibly excluding the on-chain fees which
        /// were spent in broadcasting the transaction.
        claimable_amount_satoshis: u64,
        /// The height at which an [`Event::SpendableOutputs`] event will be generated for this
        /// amount.
        confirmation_height: u32,
    },
    /// The channel has been closed, and the given balance should be ours but awaiting spending
    /// transaction confirmation. If the spending transaction does not confirm in time, it is
    /// possible our counterparty can take the funds by broadcasting an HTLC timeout on-chain.
    ///
    /// Once the spending transaction confirms, before it has reached enough confirmations to be
    /// considered safe from chain reorganizations, the balance will instead be provided via
    /// [`Balance::ClaimableAwaitingConfirmations`].
    ContentiousClaimable {
        /// The amount available to claim, in satoshis, excluding the on-chain fees which will be
        /// required to do so.
        claimable_amount_satoshis: u64,
        /// The height at which the counterparty may be able to claim the balance if we have not
        /// done so.
        timeout_height: u32,
    },
    /// HTLCs which we sent to our counterparty which are claimable after a timeout (less on-chain
    /// fees) if the counterparty does not know the preimage for the HTLCs. These are somewhat
    /// likely to be claimed by our counterparty before we do.
    MaybeClaimableHTLCAwaitingTimeout {
        /// The amount available to claim, in satoshis, excluding the on-chain fees which will be
        /// required to do so.
        claimable_amount_satoshis: u64,
        /// The height at which we will be able to claim the balance if our counterparty has not
        /// done so.
        claimable_height: u32,
    },
}

impl From<Balance> for ClaimableBalance {
    fn from(balance: Balance) -> Self {
        match balance {
            Balance::ClaimableOnChannelClose {
                claimable_amount_satoshis,
            } => ClaimableBalance::ClaimableOnChannelClose {
                claimable_amount_satoshis,
            },
            Balance::ClaimableAwaitingConfirmations {
                claimable_amount_satoshis,
                confirmation_height,
            } => ClaimableBalance::ClaimableAwaitingConfirmations {
                claimable_amount_satoshis,
                confirmation_height,
            },
            Balance::ContentiousClaimable {
                claimable_amount_satoshis,
                timeout_height,
            } => ClaimableBalance::ContentiousClaimable {
                claimable_amount_satoshis,
                timeout_height,
            },
            Balance::MaybeClaimableHTLCAwaitingTimeout {
                claimable_amount_satoshis,
                claimable_height,
            } => ClaimableBalance::MaybeClaimableHTLCAwaitingTimeout {
                claimable_amount_satoshis,
                claimable_height,
            },
        }
    }
}

#[derive(Deserialize)]
pub struct ClaimableBalancesReq {
    pub coin: String,
    #[serde(default)]
    pub include_open_channels_balances: bool,
}

pub async fn get_claimable_balances(
    ctx: MmArc,
    req: ClaimableBalancesReq,
) -> ClaimableBalancesResult<Vec<ClaimableBalance>> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(ClaimableBalancesError::UnsupportedCoin(coin.ticker().to_string())),
    };
    let ignored_channels = if req.include_open_channels_balances {
        Vec::new()
    } else {
        ln_coin.channel_manager.list_channels()
    };
    let claimable_balances = ln_coin
        .chain_monitor
        .get_claimable_balances(&ignored_channels.iter().collect::<Vec<_>>()[..])
        .into_iter()
        .map(From::from)
        .collect();

    Ok(claimable_balances)
}

#[derive(Deserialize)]
pub struct AddTrustedNodeReq {
    pub coin: String,
    pub node_id: PublicKeyForRPC,
}

#[derive(Serialize)]
pub struct AddTrustedNodeResponse {
    pub added_node: PublicKeyForRPC,
}

/// Adds a node to the set of trusted nodes from which zero-confirmation inbound channel funding is accepted.
pub async fn add_trusted_node(ctx: MmArc, req: AddTrustedNodeReq) -> TrustedNodeResult<AddTrustedNodeResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(TrustedNodeError::UnsupportedCoin(coin.ticker().to_string())),
    };

    ln_coin.trusted_nodes.lock().insert(req.node_id.clone().into());

    ln_coin
        .persister
        .save_trusted_nodes(ln_coin.trusted_nodes.clone())
        .await?;

    Ok(AddTrustedNodeResponse {
        added_node: req.node_id,
    })
}

#[derive(Deserialize)]
pub struct RemoveTrustedNodeReq {
    pub coin: String,
    pub node_id: PublicKeyForRPC,
}

#[derive(Serialize)]
pub struct RemoveTrustedNodeResponse {
    pub removed_node: PublicKeyForRPC,
}

/// Removes a node from the set of trusted nodes from which zero-confirmation inbound channel funding is accepted.
pub async fn remove_trusted_node(
    ctx: MmArc,
    req: RemoveTrustedNodeReq,
) -> TrustedNodeResult<RemoveTrustedNodeResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(TrustedNodeError::UnsupportedCoin(coin.ticker().to_string())),
    };

    ln_coin.trusted_nodes.lock().remove(&req.node_id.clone().into());

    ln_coin
        .persister
        .save_trusted_nodes(ln_coin.trusted_nodes.clone())
        .await?;

    Ok(RemoveTrustedNodeResponse {
        removed_node: req.node_id,
    })
}

#[derive(Deserialize)]
pub struct ListTrustedNodesReq {
    pub coin: String,
}

#[derive(Serialize)]
pub struct ListTrustedNodesResponse {
    pub trusted_nodes: Vec<String>,
}

/// Lists the node public keys currently in the coin's trusted-node set.
pub async fn list_trusted_nodes(ctx: MmArc, req: ListTrustedNodesReq) -> TrustedNodeResult<ListTrustedNodesResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let ln_coin = match coin {
        MmCoinEnum::LightningCoin(c) => c,
        _ => return MmError::err(TrustedNodeError::UnsupportedCoin(coin.ticker().to_string())),
    };

    let trusted_nodes = ln_coin
        .trusted_nodes
        .lock()
        .iter()
        .map(|pubkey| pubkey.to_string())
        .collect();

    Ok(ListTrustedNodesResponse { trusted_nodes })
}

#[cfg(test)]
mod update_channel_tests {
    use super::*;

    #[test]
    fn update_channel_response_serializes_force_close_avoidance_with_sats_suffix() {
        let resp = UpdateChannelResponse {
            channel_options: ChannelOptionsForRPC {
                proportional_fee_in_millionths_sats: Some(7),
                base_fee_msat: Some(11),
                cltv_expiry_delta: Some(72),
                max_dust_htlc_exposure_msat: Some(5_000_000),
                force_close_avoidance_max_fee_sats: Some(1000),
            },
        };
        let json = serde_json::to_value(&resp).unwrap();
        let opts = &json["channel_options"];
        // The wire contract exposes the shorter `_sats` spelling, not the internal `_satoshis`.
        assert!(opts.get("force_close_avoidance_max_fee_sats").is_some());
        assert!(opts.get("force_close_avoidance_max_fee_satoshis").is_none());
        assert_eq!(opts["proportional_fee_in_millionths_sats"], 7);
        assert_eq!(opts["base_fee_msat"], 11);
        assert_eq!(opts["cltv_expiry_delta"], 72);
        assert_eq!(opts["max_dust_htlc_exposure_msat"], 5_000_000);
        assert_eq!(opts["force_close_avoidance_max_fee_sats"], 1000);
    }

    #[test]
    fn update_channel_request_accepts_partial_channel_options() {
        let raw = serde_json::json!({
            "coin": "tBTC-lightning",
            "rpc_channel_id": 3,
            "channel_options": { "base_fee_msat": 1 }
        });
        let req: UpdateChannelReq = serde_json::from_value(raw).unwrap();
        assert_eq!(req.rpc_channel_id, 3);
        assert_eq!(req.channel_options.base_fee_msat, Some(1));
        assert_eq!(req.channel_options.proportional_fee_in_millionths_sats, None);
        assert_eq!(req.channel_options.force_close_avoidance_max_fee_sats, None);
    }
}
