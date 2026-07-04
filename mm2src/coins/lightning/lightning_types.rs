// lightning_types — LightningCoin struct, LightningParams, start_lightning activation
use super::*;

type Router = DefaultRouter<Arc<NetworkGraph>, Arc<LogState>>;
pub(crate) type InvoicePayer<E> =
    payment::InvoicePayer<Arc<ChannelManager>, Router, Arc<Mutex<Scorer>>, Arc<LogState>, E>;

#[derive(Clone)]
pub struct LightningCoin {
    pub platform: Arc<Platform>,
    pub conf: LightningCoinConf,
    /// The lightning node peer manager that takes care of connecting to peers, etc..
    pub peer_manager: Arc<PeerManager>,
    /// The lightning node background processor that takes care of tasks that need to happen periodically
    pub background_processor: Arc<BackgroundProcessor>,
    /// The lightning node channel manager which keeps track of the number of open channels and sends messages to the appropriate
    /// channel, also tracks HTLC preimages and forwards onion packets appropriately.
    pub channel_manager: Arc<ChannelManager>,
    /// The lightning node chain monitor that takes care of monitoring the chain for transactions of interest.
    pub chain_monitor: Arc<ChainMonitor>,
    /// The lightning node keys manager that takes care of signing invoices.
    pub keys_manager: Arc<KeysManager>,
    /// The lightning node invoice payer.
    pub invoice_payer: Arc<InvoicePayer<Arc<LightningEventHandler>>>,
    /// The lightning node persister that takes care of writing/reading data from storage.
    pub persister: Arc<LightningPersister>,
    /// The mutex storing the addresses of the nodes that the lightning node has open channels with,
    /// these addresses are used for reconnecting.
    pub open_channels_nodes: NodesAddressesMapShared,
    /// The set of nodes (by public key) from which the lightning node accepts zero-confirmation
    /// inbound channel funding. Persisted through the coin's persister and reloaded at activation.
    pub trusted_nodes: TrustedNodesShared,
}

impl fmt::Debug for LightningCoin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "LightningCoin {{ conf: {:?} }}", self.conf) }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LightningParams {
    // The listening port for the p2p LN node
    pub listening_port: u16,
    // Printable human-readable string to describe this node to other users.
    pub node_name: [u8; 32],
    // Node's RGB color. This is used for showing the node in a network graph with the desired color.
    pub node_color: [u8; 3],
    // Invoice Payer is initialized while starting the lightning node, and it requires the number of payment retries that
    // it should do before considering a payment failed or partially failed. If not provided the number of retries will be 5
    // as this is a good default value.
    pub payment_retries: Option<usize>,
    // Node's backup path for channels and other data that requires backup.
    pub backup_path: Option<String>,
}

pub async fn start_lightning(
    ctx: &MmArc,
    platform_coin: UtxoStandardCoin,
    protocol_conf: LightningProtocolConf,
    conf: LightningCoinConf,
    params: LightningParams,
) -> EnableLightningResult<LightningCoin> {
    // Todo: add support for Hardware wallets for funding transactions and spending spendable outputs (channel closing transactions)
    if let DerivationMethod::HDWallet(_) = platform_coin.as_ref().derivation_method {
        return MmError::err(EnableLightningError::UnsupportedMode(
            "'start_lightning'".into(),
            "iguana".into(),
        ));
    }

    let platform = Arc::new(Platform::new(
        platform_coin.clone(),
        protocol_conf.network.clone(),
        protocol_conf.confirmations,
    ));

    // Initialize the Logger
    let logger = ctx.log.0.clone();

    // Initialize Persister
    let persister = ln_utils::init_persister(ctx, platform.clone(), conf.ticker.clone(), params.backup_path).await?;

    // Initialize the KeysManager
    let keys_manager = ln_utils::init_keys_manager(ctx)?;

    // Initialize the NetGraphMsgHandler. This is used for providing routes to send payments over
    let network_graph = Arc::new(persister.get_network_graph(protocol_conf.network.into()).await?);
    spawn(ln_utils::persist_network_graph_loop(
        persister.clone(),
        network_graph.clone(),
    ));
    let network_gossip = Arc::new(NetGraphMsgHandler::new(
        network_graph.clone(),
        None::<Arc<dyn Access + Send + Sync>>,
        logger.clone(),
    ));

    // Initialize the ChannelManager
    let (chain_monitor, channel_manager) = ln_utils::init_channel_manager(
        platform.clone(),
        logger.clone(),
        persister.clone(),
        keys_manager.clone(),
        conf.clone().into(),
    )
    .await?;

    // Initialize the PeerManager
    let peer_manager = ln_p2p::init_peer_manager(
        ctx.clone(),
        params.listening_port,
        channel_manager.clone(),
        network_gossip.clone(),
        keys_manager
            .get_node_secret(Recipient::Node)
            .map_to_mm(|_| EnableLightningError::UnsupportedMode("'start_lightning'".into(), "local node".into()))?,
        logger.clone(),
    )
    .await?;

    // Initialize the event handler
    let event_handler = Arc::new(ln_events::LightningEventHandler::new(
        // It's safe to use unwrap here for now until implementing Native Client for Lightning
        platform.clone(),
        channel_manager.clone(),
        keys_manager.clone(),
        persister.clone(),
    ));

    // Initialize routing Scorer
    let scorer = Arc::new(Mutex::new(persister.get_scorer(network_graph.clone()).await?));
    spawn(ln_utils::persist_scorer_loop(persister.clone(), scorer.clone()));

    // Create InvoicePayer
    let router = DefaultRouter::new(network_graph, logger.clone(), keys_manager.get_secure_random_bytes());
    let invoice_payer = Arc::new(InvoicePayer::new(
        channel_manager.clone(),
        router,
        scorer,
        logger.clone(),
        event_handler,
        payment::RetryAttempts(params.payment_retries.unwrap_or(5)),
    ));

    // Persist ChannelManager
    // Note: if the ChannelManager is not persisted properly to disk, there is risk of channels force closing the next time LN starts up
    let channel_manager_persister = persister.clone();
    let persist_channel_manager_callback =
        move |node: &ChannelManager| channel_manager_persister.persist_manager(&*node);

    // Start Background Processing. Runs tasks periodically in the background to keep LN node operational.
    // InvoicePayer will act as our event handler as it handles some of the payments related events before
    // delegating it to LightningEventHandler.
    let background_processor = Arc::new(BackgroundProcessor::start(
        persist_channel_manager_callback,
        invoice_payer.clone(),
        chain_monitor.clone(),
        channel_manager.clone(),
        Some(network_gossip),
        peer_manager.clone(),
        logger,
    ));

    // If channel_nodes_data file exists, read channels nodes data from disk and reconnect to channel nodes/peers if possible.
    let open_channels_nodes = Arc::new(PaMutex::new(
        ln_utils::get_open_channels_nodes_addresses(persister.clone(), channel_manager.clone()).await?,
    ));
    spawn(ln_p2p::connect_to_nodes_loop(
        open_channels_nodes.clone(),
        peer_manager.clone(),
    ));

    // Load the persisted set of trusted nodes (zero-conf inbound channel funding is accepted from these).
    let trusted_nodes = Arc::new(PaMutex::new(persister.get_trusted_nodes().await?));

    // Broadcast Node Announcement
    spawn(ln_p2p::ln_node_announcement_loop(
        channel_manager.clone(),
        params.node_name,
        params.node_color,
        params.listening_port,
    ));

    Ok(LightningCoin {
        platform,
        conf,
        peer_manager,
        background_processor,
        channel_manager,
        chain_monitor,
        keys_manager,
        invoice_payer,
        persister,
        open_channels_nodes,
        trusted_nodes,
    })
}
