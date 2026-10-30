use super::*;
use common::mm_number::DecimalMemo;
use std::sync::LazyLock;

/// Number of entries the memo of unreserved available amounts holds before it is emptied.
const UNRESERVED_AVAILABLE_MEMO_CAPACITY: usize = 32_768;

pub(crate) fn get_true() -> bool { true }

/// Result of match_order_and_request function
#[derive(Debug, PartialEq)]
pub(crate) enum OrderMatchResult {
    /// Order and request matched, contains base and rel resulting amounts
    Matched((MmNumber, MmNumber)),
    /// Orders didn't match
    NotMatched,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TakerAction {
    Buy,
    Sell,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(test, derive(Default))]
pub struct OrderConfirmationsSettings {
    pub base_confs: u64,
    pub base_nota: bool,
    pub rel_confs: u64,
    pub rel_nota: bool,
}

impl OrderConfirmationsSettings {
    pub fn reversed(&self) -> OrderConfirmationsSettings {
        OrderConfirmationsSettings {
            base_confs: self.rel_confs,
            base_nota: self.rel_nota,
            rel_confs: self.base_confs,
            rel_nota: self.base_nota,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TakerRequest {
    pub base: String,
    pub rel: String,
    pub base_amount: MmNumber,
    pub rel_amount: MmNumber,
    pub action: TakerAction,
    pub(crate) uuid: Uuid,
    pub(crate) sender_pubkey: H256Json,
    pub(crate) dest_pub_key: H256Json,
    #[serde(default)]
    pub(crate) match_by: MatchBy,
    pub(crate) conf_settings: Option<OrderConfirmationsSettings>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_protocol_info: Option<Vec<u8>>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel_protocol_info: Option<Vec<u8>>,
    /// Swap protocol version this taker request supports.
    #[serde(default, skip_serializing_if = "SwapVersion::is_legacy")]
    pub swap_version: SwapVersion,
}

impl TakerRequest {
    pub(crate) fn from_new_proto_and_pubkey(message: new_protocol::TakerRequest, sender_pubkey: H256Json) -> Self {
        let base_amount = MmNumber::from(message.base_amount);
        let rel_amount = MmNumber::from(message.rel_amount);

        TakerRequest {
            base: message.base,
            rel: message.rel,
            base_amount,
            rel_amount,
            action: message.action,
            uuid: message.uuid.into(),
            sender_pubkey,
            dest_pub_key: Default::default(),
            match_by: message.match_by.into(),
            conf_settings: Some(message.conf_settings),
            base_protocol_info: message.base_protocol_info,
            rel_protocol_info: message.rel_protocol_info,
            swap_version: message.swap_version,
        }
    }

    pub(crate) fn can_match_with_maker_pubkey(&self, maker_pubkey: &H256Json) -> bool {
        match &self.match_by {
            MatchBy::Pubkeys(pubkeys) => pubkeys.contains(maker_pubkey),
            _ => true,
        }
    }

    pub(crate) fn can_match_with_uuid(&self, uuid: &Uuid) -> bool {
        match &self.match_by {
            MatchBy::Orders(uuids) => uuids.contains(uuid),
            _ => true,
        }
    }

    pub(crate) fn base_protocol_info_for_maker(&self) -> &Option<Vec<u8>> {
        match &self.action {
            TakerAction::Buy => &self.base_protocol_info,
            TakerAction::Sell => &self.rel_protocol_info,
        }
    }

    pub(crate) fn rel_protocol_info_for_maker(&self) -> &Option<Vec<u8>> {
        match &self.action {
            TakerAction::Buy => &self.rel_protocol_info,
            TakerAction::Sell => &self.base_protocol_info,
        }
    }
}

impl From<TakerOrder> for new_protocol::OrdermatchMessage {
    fn from(taker_order: TakerOrder) -> Self {
        new_protocol::OrdermatchMessage::TakerRequest(new_protocol::TakerRequest {
            base_amount: taker_order.request.get_base_amount().to_ratio(),
            rel_amount: taker_order.request.get_rel_amount().to_ratio(),
            base: taker_order.base_orderbook_ticker().to_owned(),
            rel: taker_order.rel_orderbook_ticker().to_owned(),
            action: taker_order.request.action,
            uuid: taker_order.request.uuid.into(),
            match_by: taker_order.request.match_by.into(),
            conf_settings: taker_order.request.conf_settings.unwrap(),
            base_protocol_info: taker_order.request.base_protocol_info,
            rel_protocol_info: taker_order.request.rel_protocol_info,
            swap_version: taker_order.request.swap_version,
        })
    }
}

impl TakerRequest {
    pub(crate) fn get_base_amount(&self) -> &MmNumber { &self.base_amount }

    pub(crate) fn get_rel_amount(&self) -> &MmNumber { &self.rel_amount }
}

pub struct TakerOrderBuilder<'a> {
    pub(crate) base_coin: &'a MmCoinEnum,
    pub(crate) rel_coin: &'a MmCoinEnum,
    pub(crate) base_orderbook_ticker: Option<String>,
    pub(crate) rel_orderbook_ticker: Option<String>,
    pub(crate) base_amount: MmNumber,
    pub(crate) rel_amount: MmNumber,
    pub(crate) sender_pubkey: H256Json,
    pub(crate) action: TakerAction,
    pub(crate) match_by: MatchBy,
    pub(crate) order_type: OrderType,
    pub(crate) conf_settings: Option<OrderConfirmationsSettings>,
    pub(crate) min_volume: Option<MmNumber>,
    pub(crate) timeout: u64,
    pub(crate) save_in_history: bool,
    pub(crate) swap_version: SwapVersion,
}

/// Returns the ticker of the first coin of the pair that cannot trade: a Lightning coin, until
/// Lightning trading is supported, or a Solana coin or SPL token, whose swap, fee and trade-preimage
/// operations are unimplemented (CRD ch.60 R60.2.3, R60.6.5). Order creation and the trade preimage
/// refuse such a pair with an error before any of those operations is reached.
pub(crate) fn untradable_coin_ticker(base: &MmCoinEnum, rel: &MmCoinEnum) -> Option<String> {
    [base, rel]
        .into_iter()
        .find(|coin| is_untradable_coin(coin))
        .map(|coin| coin.ticker().to_owned())
}

#[cfg(not(target_arch = "wasm32"))]
fn is_untradable_coin(coin: &MmCoinEnum) -> bool {
    matches!(
        coin,
        MmCoinEnum::LightningCoin(_) | MmCoinEnum::SolanaCoin(_) | MmCoinEnum::SplToken(_)
    )
}

#[cfg(target_arch = "wasm32")]
fn is_untradable_coin(_coin: &MmCoinEnum) -> bool { false }

/// CRD ch.60 R60.6.7 item 7: the MM2.json boolean `pre_trade_spend_check`, default `true`. Only an explicit
/// JSON `false` turns the pre-trade spend-safety check off, which is the reference behaviour.
pub(crate) fn pre_trade_spend_check_enabled(ctx: &MmArc) -> bool {
    ctx.conf["pre_trade_spend_check"].as_bool() != Some(false)
}

/// CRD ch.59 R59.4.6: the MM2.json boolean `suppress_unchanged_order_updates`, default `true`. Only an
/// explicit JSON `false` makes `update_maker_order` broadcast an update that changes nothing.
pub(crate) fn suppress_unchanged_order_updates(ctx: &MmArc) -> bool {
    ctx.conf["suppress_unchanged_order_updates"].as_bool() != Some(false)
}

/// CRD ch.32 §32.9B R-MP1 item 5 (D9): the MM2.json boolean `maker_update_coalescing`, default `false`. Only an
/// explicit JSON `true` turns the maker outbox on. Otherwise (missing, `false`, any other value) every maker-order
/// message leaves at once, as without the outbox.
pub(crate) fn maker_update_coalescing(ctx: &MmArc) -> bool {
    ctx.conf["maker_update_coalescing"].as_bool() == Some(true)
}

/// CRD ch.32 §32.9B R-MP2 item 1: the MM2.json number `maker_update_rate_max`, the ceiling of the rate at
/// which the outbox releases created and updated messages, per second per maker pubkey. The default is 20;
/// a value that is not a positive finite number is ignored.
pub(crate) fn maker_update_rate_max(ctx: &MmArc) -> f64 {
    ctx.conf["maker_update_rate_max"]
        .as_f64()
        .filter(|rate| rate.is_finite() && *rate > 0.0)
        .unwrap_or(DEFAULT_MAKER_UPDATE_RATE_MAX)
}

/// CRD ch.59 R59.6.1a: the MM2.json boolean `order_messages_refresh_liveness`, default `true`. Only an
/// explicit JSON `false` makes a maker's liveness instant depend on its keep-alives alone, which is the
/// reference behaviour.
pub(crate) fn order_messages_refresh_liveness(ctx: &MmArc) -> bool {
    ctx.conf["order_messages_refresh_liveness"].as_bool() != Some(false)
}

/// Refusal of the pre-trade spend-safety check (CRD ch.60 R60.6.7 items 4 and 6).
#[derive(Debug, Display)]
pub(crate) enum SpendSafetyError {
    /// A covered leg of the smallest trade the order can produce is below the coin's safe amount.
    #[display(
        fmt = "The volume {} of the {} coin is too low, required at least {}: a payment of that size could not be spent or refunded",
        volume,
        coin,
        threshold
    )]
    VolumeTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The dynamic-fee estimate of a covered coin could not be obtained.
    #[display(fmt = "Cannot check the {} coin spend safety: {}", coin, error)]
    EstimateUnavailable { coin: String, error: String },
}

/// CRD ch.60 R60.6.7 items 2 to 6: the pre-trade spend-safety check of an order of this node's own.
///
/// The smallest trade the order can produce is its minimum volume of the base coin and that volume times
/// `price` of the rel coin; each covered leg shall be at least that coin's safe amount. `min_base_vol` is the
/// operator-set minimum volume, `None` for the default of R60.4.2 step 3, which is never raised by this check.
/// An order the order builder refuses anyway (price below the minimum, operator-set minimum below the default)
/// is left to the builder, whose refusals precede this one.
pub(crate) async fn check_pre_trade_spend_safety(
    ctx: &MmArc,
    base_coin: &MmCoinEnum,
    rel_coin: &MmCoinEnum,
    min_base_vol: Option<MmNumber>,
    price: &MmNumber,
) -> Result<(), SpendSafetyError> {
    if !pre_trade_spend_check_enabled(ctx) || validate_price(price.clone()).is_err() {
        return Ok(());
    }
    let min_base_vol = match validate_and_get_min_vol(
        base_coin.min_trading_vol(),
        rel_coin.min_trading_vol(),
        min_base_vol,
        price.clone(),
    ) {
        Ok(min_base_vol) => min_base_vol,
        Err(_) => return Ok(()),
    };
    let rel_vol = &min_base_vol * price;
    for (coin, volume) in [(base_coin, min_base_vol), (rel_coin, rel_vol)] {
        let safe_amount = coin
            .spend_safe_amount()
            .await
            .map_err(|e| SpendSafetyError::EstimateUnavailable {
                coin: coin.ticker().to_owned(),
                error: e.to_string(),
            })?;
        if let Some(safe_amount) = safe_amount {
            if volume < safe_amount {
                return Err(SpendSafetyError::VolumeTooLow {
                    coin: coin.ticker().to_owned(),
                    volume: volume.to_decimal(),
                    threshold: safe_amount.to_decimal(),
                });
            }
        }
    }
    Ok(())
}

pub enum TakerOrderBuildError {
    BaseEqualRel,
    /// Base amount too low with threshold
    BaseAmountTooLow {
        actual: MmNumber,
        threshold: MmNumber,
    },
    /// Rel amount too low with threshold
    RelAmountTooLow {
        actual: MmNumber,
        threshold: MmNumber,
    },
    /// Min volume too low with threshold
    MinVolumeTooLow {
        actual: MmNumber,
        threshold: MmNumber,
    },
    /// Max vol below min base vol
    MaxBaseVolBelowMinBaseVol {
        max: MmNumber,
        min: MmNumber,
    },
    SenderPubkeyIsZero,
    ConfsSettingsNotSet,
    /// The pair involves a coin that cannot trade (Lightning, until Lightning trading exists).
    TradingNotSupported {
        coin: String,
    },
}

impl fmt::Display for TakerOrderBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TakerOrderBuildError::BaseEqualRel => write!(f, "Rel coin can not be same as base"),
            TakerOrderBuildError::BaseAmountTooLow { actual, threshold } => write!(
                f,
                "Base amount {} is too low, required: {}",
                actual.to_decimal(),
                threshold.to_decimal()
            ),
            TakerOrderBuildError::RelAmountTooLow { actual, threshold } => write!(
                f,
                "Rel amount {} is too low, required: {}",
                actual.to_decimal(),
                threshold.to_decimal()
            ),
            TakerOrderBuildError::MinVolumeTooLow { actual, threshold } => write!(
                f,
                "Min volume {} is too low, required: {}",
                actual.to_decimal(),
                threshold.to_decimal()
            ),
            TakerOrderBuildError::MaxBaseVolBelowMinBaseVol { min, max } => write!(
                f,
                "Max base vol {} is below min base vol: {}",
                max.to_decimal(),
                min.to_decimal()
            ),
            TakerOrderBuildError::SenderPubkeyIsZero => write!(f, "Sender pubkey can not be zero"),
            TakerOrderBuildError::ConfsSettingsNotSet => write!(f, "Confirmation settings must be set"),
            TakerOrderBuildError::TradingNotSupported { coin } => {
                write!(f, "Trading is not supported for coin {}", coin)
            },
        }
    }
}

impl<'a> TakerOrderBuilder<'a> {
    pub fn new(base_coin: &'a MmCoinEnum, rel_coin: &'a MmCoinEnum) -> TakerOrderBuilder<'a> {
        TakerOrderBuilder {
            base_coin,
            rel_coin,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            base_amount: MmNumber::from(0),
            rel_amount: MmNumber::from(0),
            sender_pubkey: H256Json::default(),
            action: TakerAction::Buy,
            match_by: MatchBy::Any,
            conf_settings: None,
            min_volume: None,
            order_type: OrderType::GoodTillCancelled,
            timeout: TAKER_ORDER_TIMEOUT,
            save_in_history: true,
            swap_version: SwapVersion::default(),
        }
    }

    pub fn with_base_amount(mut self, vol: MmNumber) -> Self {
        self.base_amount = vol;
        self
    }

    pub fn with_rel_amount(mut self, vol: MmNumber) -> Self {
        self.rel_amount = vol;
        self
    }

    pub fn with_min_volume(mut self, vol: Option<MmNumber>) -> Self {
        self.min_volume = vol;
        self
    }

    pub fn with_action(mut self, action: TakerAction) -> Self {
        self.action = action;
        self
    }

    pub fn with_match_by(mut self, match_by: MatchBy) -> Self {
        self.match_by = match_by;
        self
    }

    pub(crate) fn with_order_type(mut self, order_type: OrderType) -> Self {
        self.order_type = order_type;
        self
    }

    pub fn with_conf_settings(mut self, settings: OrderConfirmationsSettings) -> Self {
        self.conf_settings = Some(settings);
        self
    }

    pub fn with_sender_pubkey(mut self, sender_pubkey: H256Json) -> Self {
        self.sender_pubkey = sender_pubkey;
        self
    }

    pub fn with_timeout(mut self, timeout: u64) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_save_in_history(mut self, save_in_history: bool) -> Self {
        self.save_in_history = save_in_history;
        self
    }

    pub fn with_base_orderbook_ticker(mut self, ticker: Option<String>) -> Self {
        self.base_orderbook_ticker = ticker;
        self
    }

    pub fn with_rel_orderbook_ticker(mut self, ticker: Option<String>) -> Self {
        self.rel_orderbook_ticker = ticker;
        self
    }

    /// Validate fields and build
    pub fn build(self) -> Result<TakerOrder, TakerOrderBuildError> {
        if let Some(coin) = untradable_coin_ticker(self.base_coin, self.rel_coin) {
            return Err(TakerOrderBuildError::TradingNotSupported { coin });
        }

        let min_base_amount = self.base_coin.min_trading_vol();
        let min_rel_amount = self.rel_coin.min_trading_vol();

        if self.base_coin.ticker() == self.rel_coin.ticker() {
            return Err(TakerOrderBuildError::BaseEqualRel);
        }

        if self.base_amount < min_base_amount {
            return Err(TakerOrderBuildError::BaseAmountTooLow {
                actual: self.base_amount,
                threshold: min_base_amount,
            });
        }

        if self.rel_amount < min_rel_amount {
            return Err(TakerOrderBuildError::RelAmountTooLow {
                actual: self.rel_amount,
                threshold: min_rel_amount,
            });
        }

        if self.sender_pubkey == H256Json::default() {
            return Err(TakerOrderBuildError::SenderPubkeyIsZero);
        }

        if self.conf_settings.is_none() {
            return Err(TakerOrderBuildError::ConfsSettingsNotSet);
        }

        let price = &self.rel_amount / &self.base_amount;
        let base_min_by_rel = &min_rel_amount / &price;
        let base_min_vol_threshold = min_base_amount.max(base_min_by_rel);

        let min_volume = self.min_volume.unwrap_or_else(|| base_min_vol_threshold.clone());

        if min_volume < base_min_vol_threshold {
            return Err(TakerOrderBuildError::MinVolumeTooLow {
                actual: min_volume,
                threshold: base_min_vol_threshold,
            });
        }

        if self.base_amount < min_volume {
            return Err(TakerOrderBuildError::MaxBaseVolBelowMinBaseVol {
                max: self.base_amount,
                min: min_volume,
            });
        }

        let my_coin = match &self.action {
            TakerAction::Buy => &self.rel_coin,
            TakerAction::Sell => &self.base_coin,
        };

        let p2p_privkey = if my_coin.is_privacy() {
            Some(SerializableSecp256k1Keypair::random())
        } else {
            None
        };

        Ok(TakerOrder {
            created_at: now_ms(),
            request: TakerRequest {
                base: self.base_coin.ticker().into(),
                rel: self.rel_coin.ticker().into(),
                base_amount: self.base_amount,
                rel_amount: self.rel_amount,
                action: self.action,
                uuid: new_uuid(),
                sender_pubkey: self.sender_pubkey,
                dest_pub_key: Default::default(),
                match_by: self.match_by,
                conf_settings: self.conf_settings,
                base_protocol_info: Some(self.base_coin.coin_protocol_info()),
                rel_protocol_info: Some(self.rel_coin.coin_protocol_info()),
                swap_version: self.swap_version,
            },
            matches: Default::default(),
            min_volume,
            order_type: self.order_type,
            timeout: self.timeout,
            save_in_history: self.save_in_history,
            base_orderbook_ticker: self.base_orderbook_ticker,
            rel_orderbook_ticker: self.rel_orderbook_ticker,
            p2p_privkey,
        })
    }

    #[cfg(test)]
    /// skip validation for tests
    pub(crate) fn build_unchecked(self) -> TakerOrder {
        TakerOrder {
            created_at: now_ms(),
            request: TakerRequest {
                base: self.base_coin.ticker().to_owned(),
                rel: self.rel_coin.ticker().to_owned(),
                base_amount: self.base_amount,
                rel_amount: self.rel_amount,
                action: self.action,
                uuid: new_uuid(),
                sender_pubkey: self.sender_pubkey,
                dest_pub_key: Default::default(),
                match_by: self.match_by,
                conf_settings: self.conf_settings,
                base_protocol_info: Some(self.base_coin.coin_protocol_info()),
                rel_protocol_info: Some(self.rel_coin.coin_protocol_info()),
                swap_version: self.swap_version,
            },
            matches: HashMap::new(),
            min_volume: Default::default(),
            order_type: Default::default(),
            timeout: self.timeout,
            save_in_history: false,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data")]
#[derive(Default)]
pub enum MatchBy {
    #[default]
    Any,
    Orders(HashSet<Uuid>),
    Pubkeys(HashSet<H256Json>),
}

#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data")]
#[derive(Default)]
pub(crate) enum OrderType {
    FillOrKill,
    #[default]
    GoodTillCancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TakerOrder {
    pub created_at: u64,
    pub request: TakerRequest,
    pub(crate) matches: HashMap<Uuid, TakerMatch>,
    pub(crate) min_volume: MmNumber,
    pub(crate) order_type: OrderType,
    pub(crate) timeout: u64,
    #[serde(default = "get_true")]
    pub(crate) save_in_history: bool,
    #[serde(default)]
    pub(crate) base_orderbook_ticker: Option<String>,
    #[serde(default)]
    pub(crate) rel_orderbook_ticker: Option<String>,
    /// A custom priv key for more privacy to prevent linking orders of the same node between each other
    /// Commonly used with privacy coins (ARRR, ZCash, etc.)
    pub(crate) p2p_privkey: Option<SerializableSecp256k1Keypair>,
}

/// Result of match_reserved function
#[derive(Debug, PartialEq)]
pub(crate) enum MatchReservedResult {
    /// Order and reserved message matched,
    Matched,
    /// Order and reserved didn't match
    NotMatched,
}

impl TakerOrder {
    pub(crate) fn is_cancellable(&self) -> bool { self.matches.is_empty() }

    pub(crate) fn match_reserved(&self, reserved: &MakerReserved) -> MatchReservedResult {
        match &self.request.match_by {
            MatchBy::Any => (),
            MatchBy::Orders(uuids) => {
                if !uuids.contains(&reserved.maker_order_uuid) {
                    return MatchReservedResult::NotMatched;
                }
            },
            MatchBy::Pubkeys(pubkeys) => {
                if !pubkeys.contains(&reserved.sender_pubkey) {
                    return MatchReservedResult::NotMatched;
                }
            },
        }

        let my_base_amount = self.request.get_base_amount();
        let my_rel_amount = self.request.get_rel_amount();
        let other_base_amount = reserved.get_base_amount();
        let other_rel_amount = reserved.get_rel_amount();

        match self.request.action {
            TakerAction::Buy => {
                let match_ticker = (self.request.base == reserved.base
                    || self.base_orderbook_ticker.as_ref() == Some(&reserved.base))
                    && (self.request.rel == reserved.rel || self.rel_orderbook_ticker.as_ref() == Some(&reserved.rel));
                if match_ticker && my_base_amount == other_base_amount && other_rel_amount <= my_rel_amount {
                    MatchReservedResult::Matched
                } else {
                    MatchReservedResult::NotMatched
                }
            },
            TakerAction::Sell => {
                let match_ticker = (self.request.base == reserved.rel
                    || self.base_orderbook_ticker.as_ref() == Some(&reserved.rel))
                    && (self.request.rel == reserved.base
                        || self.rel_orderbook_ticker.as_ref() == Some(&reserved.base));
                if match_ticker && my_base_amount == other_rel_amount && my_rel_amount <= other_base_amount {
                    MatchReservedResult::Matched
                } else {
                    MatchReservedResult::NotMatched
                }
            },
        }
    }

    /// Returns the ticker of the taker coin
    pub(crate) fn taker_coin_ticker(&self) -> &str {
        match &self.request.action {
            TakerAction::Buy => &self.request.rel,
            TakerAction::Sell => &self.request.base,
        }
    }

    /// Returns the ticker of the maker coin
    pub(crate) fn maker_coin_ticker(&self) -> &str {
        match &self.request.action {
            TakerAction::Buy => &self.request.base,
            TakerAction::Sell => &self.request.rel,
        }
    }

    pub(crate) fn base_orderbook_ticker(&self) -> &str {
        self.base_orderbook_ticker.as_deref().unwrap_or(&self.request.base)
    }

    pub(crate) fn rel_orderbook_ticker(&self) -> &str {
        self.rel_orderbook_ticker.as_deref().unwrap_or(&self.request.rel)
    }

    pub(crate) fn orderbook_topic(&self) -> String {
        orderbook_topic_from_base_rel(self.base_orderbook_ticker(), self.rel_orderbook_ticker())
    }

    pub(crate) fn p2p_keypair(&self) -> Option<&KeyPair> { self.p2p_privkey.as_ref().map(|key| key.key_pair()) }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// Market maker order
/// The "action" is missing here because it's easier to always consider maker order as "sell"
/// So upon ordermatch with request we have only 2 combinations "sell":"sell" and "sell":"buy"
/// Adding "action" to maker order will just double possible combinations making order match more complex.
pub struct MakerOrder {
    pub max_base_vol: MmNumber,
    pub min_base_vol: MmNumber,
    pub price: MmNumber,
    pub created_at: u64,
    pub updated_at: Option<u64>,
    pub base: String,
    pub rel: String,
    pub(crate) matches: HashMap<Uuid, MakerMatch>,
    pub(crate) started_swaps: Vec<Uuid>,
    pub(crate) uuid: Uuid,
    pub(crate) conf_settings: Option<OrderConfirmationsSettings>,
    // Keeping this for now for backward compatibility when kickstarting maker orders
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) changes_history: Option<Vec<HistoricalOrder>>,
    #[serde(default = "get_true")]
    pub(crate) save_in_history: bool,
    #[serde(default)]
    pub(crate) base_orderbook_ticker: Option<String>,
    #[serde(default)]
    pub(crate) rel_orderbook_ticker: Option<String>,
    /// A custom priv key for more privacy to prevent linking orders of the same node between each other
    /// Commonly used with privacy coins (ARRR, ZCash, etc.)
    pub(crate) p2p_privkey: Option<SerializableSecp256k1Keypair>,
    /// Optional per-order timeout in minutes.  When set the order will be
    /// removed automatically once the TTL elapses.  `None` means the order
    /// lives until explicitly cancelled or until balance is insufficient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_in_minutes: Option<u16>,
    /// Swap protocol version this order supports.  Legacy (V1) is the default.
    /// When both maker and taker advertise V2, the V2 state-machine swap is used.
    #[serde(default, skip_serializing_if = "SwapVersion::is_legacy")]
    pub swap_version: SwapVersion,
}

pub struct MakerOrderBuilder<'a> {
    pub(crate) max_base_vol: MmNumber,
    pub(crate) min_base_vol: Option<MmNumber>,
    pub(crate) price: MmNumber,
    pub(crate) base_coin: &'a MmCoinEnum,
    pub(crate) rel_coin: &'a MmCoinEnum,
    pub(crate) base_orderbook_ticker: Option<String>,
    pub(crate) rel_orderbook_ticker: Option<String>,
    pub(crate) conf_settings: Option<OrderConfirmationsSettings>,
    pub(crate) save_in_history: bool,
    pub(crate) timeout_in_minutes: Option<u16>,
    pub(crate) swap_version: SwapVersion,
}

pub enum MakerOrderBuildError {
    BaseEqualRel,
    /// Max base vol too low with threshold
    MaxBaseVolTooLow {
        actual: MmNumber,
        threshold: MmNumber,
    },
    /// Min base vol too low with threshold
    MinBaseVolTooLow {
        actual: MmNumber,
        threshold: MmNumber,
    },
    /// Price too low with threshold
    PriceTooLow {
        actual: MmNumber,
        threshold: MmNumber,
    },
    /// Rel vol too low with threshold
    RelVolTooLow {
        actual: MmNumber,
        threshold: MmNumber,
    },
    ConfSettingsNotSet,
    MaxBaseVolBelowMinBaseVol {
        min: MmNumber,
        max: MmNumber,
    },
    /// The pair involves a coin that cannot trade (Lightning, until Lightning trading exists).
    TradingNotSupported {
        coin: String,
    },
}

impl fmt::Display for MakerOrderBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MakerOrderBuildError::BaseEqualRel => write!(f, "Rel coin can not be same as base"),
            MakerOrderBuildError::MaxBaseVolTooLow { actual, threshold } => write!(
                f,
                "Max base vol {} is too low, required: {}",
                actual.to_decimal(),
                threshold.to_decimal()
            ),
            MakerOrderBuildError::MinBaseVolTooLow { actual, threshold } => write!(
                f,
                "Min base vol {} is too low, required: {}",
                actual.to_decimal(),
                threshold.to_decimal()
            ),
            MakerOrderBuildError::PriceTooLow { actual, threshold } => write!(
                f,
                "Price {} is too low, required: {}",
                actual.to_decimal(),
                threshold.to_decimal()
            ),
            MakerOrderBuildError::RelVolTooLow { actual, threshold } => write!(
                f,
                "Max rel vol {} is too low, required: {}",
                actual.to_decimal(),
                threshold.to_decimal()
            ),
            MakerOrderBuildError::ConfSettingsNotSet => write!(f, "Confirmation settings must be set"),
            MakerOrderBuildError::MaxBaseVolBelowMinBaseVol { min, max } => write!(
                f,
                "Max base vol {} is below min base vol: {}",
                max.to_decimal(),
                min.to_decimal()
            ),
            MakerOrderBuildError::TradingNotSupported { coin } => {
                write!(f, "Trading is not supported for coin {}", coin)
            },
        }
    }
}

pub(crate) fn validate_price(price: MmNumber) -> Result<(), MakerOrderBuildError> {
    let min_price = MmNumber::from(BigRational::new(1.into(), 100_000_000.into()));

    if price < min_price {
        return Err(MakerOrderBuildError::PriceTooLow {
            actual: price,
            threshold: min_price,
        });
    }

    Ok(())
}

pub(crate) fn validate_and_get_min_vol(
    min_base_amount: MmNumber,
    min_rel_amount: MmNumber,
    min_base_vol: Option<MmNumber>,
    price: MmNumber,
) -> Result<MmNumber, MakerOrderBuildError> {
    let base_min_by_rel = min_rel_amount / price;
    let base_min_vol_threshold = min_base_amount.max(base_min_by_rel);
    let actual_min_base_vol = min_base_vol.unwrap_or_else(|| base_min_vol_threshold.clone());

    if actual_min_base_vol < base_min_vol_threshold {
        return Err(MakerOrderBuildError::MinBaseVolTooLow {
            actual: actual_min_base_vol,
            threshold: base_min_vol_threshold,
        });
    }

    Ok(actual_min_base_vol)
}

pub(crate) fn validate_max_vol(
    min_base_amount: MmNumber,
    min_rel_amount: MmNumber,
    max_base_vol: MmNumber,
    min_base_vol: Option<MmNumber>,
    price: MmNumber,
) -> Result<(), MakerOrderBuildError> {
    if let Some(min) = min_base_vol {
        if max_base_vol < min {
            return Err(MakerOrderBuildError::MaxBaseVolBelowMinBaseVol { min, max: max_base_vol });
        }
    }

    if max_base_vol < min_base_amount {
        return Err(MakerOrderBuildError::MaxBaseVolTooLow {
            actual: max_base_vol,
            threshold: min_base_amount,
        });
    }

    let rel_vol = max_base_vol * price;
    if rel_vol < min_rel_amount {
        return Err(MakerOrderBuildError::RelVolTooLow {
            actual: rel_vol,
            threshold: min_rel_amount,
        });
    }

    Ok(())
}

impl<'a> MakerOrderBuilder<'a> {
    pub fn new(base_coin: &'a MmCoinEnum, rel_coin: &'a MmCoinEnum) -> MakerOrderBuilder<'a> {
        MakerOrderBuilder {
            base_coin,
            rel_coin,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            max_base_vol: 0.into(),
            min_base_vol: None,
            price: 0.into(),
            conf_settings: None,
            save_in_history: true,
            timeout_in_minutes: None,
            swap_version: SwapVersion::default(),
        }
    }

    pub fn with_max_base_vol(mut self, vol: MmNumber) -> Self {
        self.max_base_vol = vol;
        self
    }

    pub fn with_min_base_vol(mut self, vol: Option<MmNumber>) -> Self {
        self.min_base_vol = vol;
        self
    }

    pub fn with_price(mut self, price: MmNumber) -> Self {
        self.price = price;
        self
    }

    pub fn with_conf_settings(mut self, conf_settings: OrderConfirmationsSettings) -> Self {
        self.conf_settings = Some(conf_settings);
        self
    }

    pub fn with_save_in_history(mut self, save_in_history: bool) -> Self {
        self.save_in_history = save_in_history;
        self
    }

    pub fn with_base_orderbook_ticker(mut self, base_orderbook_ticker: Option<String>) -> Self {
        self.base_orderbook_ticker = base_orderbook_ticker;
        self
    }

    pub fn with_rel_orderbook_ticker(mut self, rel_orderbook_ticker: Option<String>) -> Self {
        self.rel_orderbook_ticker = rel_orderbook_ticker;
        self
    }

    pub fn with_timeout(mut self, timeout_in_minutes: Option<u16>) -> Self {
        self.timeout_in_minutes = timeout_in_minutes;
        self
    }

    pub fn with_swap_version(mut self, swap_version: SwapVersion) -> Self {
        self.swap_version = swap_version;
        self
    }

    /// Build MakerOrder
    pub fn build(self) -> Result<MakerOrder, MakerOrderBuildError> {
        if let Some(coin) = untradable_coin_ticker(self.base_coin, self.rel_coin) {
            return Err(MakerOrderBuildError::TradingNotSupported { coin });
        }

        if self.base_coin.ticker() == self.rel_coin.ticker() {
            return Err(MakerOrderBuildError::BaseEqualRel);
        }

        if self.conf_settings.is_none() {
            return Err(MakerOrderBuildError::ConfSettingsNotSet);
        }

        let min_base_amount = self.base_coin.min_trading_vol();
        let min_rel_amount = self.rel_coin.min_trading_vol();

        validate_price(self.price.clone())?;

        let actual_min_base_vol = validate_and_get_min_vol(
            min_base_amount.clone(),
            min_rel_amount.clone(),
            self.min_base_vol.clone(),
            self.price.clone(),
        )?;

        validate_max_vol(
            min_base_amount,
            min_rel_amount,
            self.max_base_vol.clone(),
            self.min_base_vol.clone(),
            self.price.clone(),
        )?;

        let created_at = now_ms();

        let p2p_privkey = if self.base_coin.is_privacy() {
            Some(SerializableSecp256k1Keypair::random())
        } else {
            None
        };

        Ok(MakerOrder {
            base: self.base_coin.ticker().to_owned(),
            rel: self.rel_coin.ticker().to_owned(),
            created_at,
            updated_at: Some(created_at),
            max_base_vol: self.max_base_vol,
            min_base_vol: actual_min_base_vol,
            price: self.price,
            matches: HashMap::new(),
            started_swaps: Vec::new(),
            uuid: new_uuid(),
            conf_settings: self.conf_settings,
            changes_history: None,
            save_in_history: self.save_in_history,
            base_orderbook_ticker: self.base_orderbook_ticker,
            rel_orderbook_ticker: self.rel_orderbook_ticker,
            p2p_privkey,
            timeout_in_minutes: self.timeout_in_minutes,
            swap_version: self.swap_version,
        })
    }

    #[cfg(test)]
    pub(crate) fn build_unchecked(self) -> MakerOrder {
        let created_at = now_ms();
        MakerOrder {
            base: self.base_coin.ticker().to_owned(),
            rel: self.rel_coin.ticker().to_owned(),
            created_at,
            updated_at: Some(created_at),
            max_base_vol: self.max_base_vol,
            min_base_vol: self.min_base_vol.unwrap_or(self.base_coin.min_trading_vol()),
            price: self.price,
            matches: HashMap::new(),
            started_swaps: Vec::new(),
            uuid: new_uuid(),
            conf_settings: self.conf_settings,
            changes_history: None,
            save_in_history: false,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
            timeout_in_minutes: None,
            swap_version: SwapVersion::default(),
        }
    }
}

#[allow(dead_code)]
pub(crate) fn zero_rat() -> BigRational { BigRational::zero() }

impl MakerOrder {
    pub(crate) fn available_amount(&self) -> MmNumber { &self.max_base_vol - &self.reserved_amount() }

    /// The decimal rendering of [`MakerOrder::available_amount`] for RPC answers.
    ///
    /// An order without matches reserves nothing, so its available amount is a function of `max_base_vol`
    /// alone, and the rendering is memoized under the exact `max_base_vol` (a separate memo from the one for
    /// plain decimals, because the subtraction can reduce a fraction that was stored unreduced). An order
    /// with matches is computed in full.
    pub(crate) fn available_amount_rendered(&self) -> RenderedDecimal {
        if self.matches.is_empty() {
            static UNRESERVED_AVAILABLE: LazyLock<DecimalMemo> =
                LazyLock::new(|| DecimalMemo::with_capacity(UNRESERVED_AVAILABLE_MEMO_CAPACITY));
            UNRESERVED_AVAILABLE
                .get_or_compute_with(self.max_base_vol.as_ratio(), || self.available_amount().to_decimal())
        } else {
            self.available_amount().to_rendered_decimal()
        }
    }

    pub(crate) fn reserved_amount(&self) -> MmNumber {
        self.matches.iter().fold(
            MmNumber::from(BigRational::from_integer(0.into())),
            |reserved, (_, order_match)| &reserved + order_match.reserved.get_base_amount(),
        )
    }

    pub(crate) fn is_cancellable(&self) -> bool { !self.has_ongoing_matches() }

    pub(crate) fn has_ongoing_matches(&self) -> bool {
        for order_match in self.matches.values() {
            // if there's at least 1 ongoing match the order is not cancellable
            if order_match.connected.is_none() && order_match.connect.is_none() {
                return true;
            }
        }
        false
    }

    pub(crate) fn match_with_request(&self, taker: &TakerRequest) -> OrderMatchResult {
        let taker_base_amount = taker.get_base_amount();
        let taker_rel_amount = taker.get_rel_amount();

        let zero = MmNumber::from(0);
        if taker_base_amount <= &zero || taker_rel_amount <= &zero {
            return OrderMatchResult::NotMatched;
        }

        match taker.action {
            TakerAction::Buy => {
                let ticker_match = (self.base == taker.base
                    || self.base_orderbook_ticker.as_ref() == Some(&taker.base))
                    && (self.rel == taker.rel || self.rel_orderbook_ticker.as_ref() == Some(&taker.rel));
                let taker_price = taker_rel_amount / taker_base_amount;
                if ticker_match
                    && taker_base_amount <= &self.available_amount()
                    && taker_base_amount >= &self.min_base_vol
                    && taker_price >= self.price
                {
                    OrderMatchResult::Matched((taker_base_amount.clone(), taker_base_amount * &self.price))
                } else {
                    OrderMatchResult::NotMatched
                }
            },
            TakerAction::Sell => {
                let ticker_match = (self.base == taker.rel || self.base_orderbook_ticker.as_ref() == Some(&taker.rel))
                    && (self.rel == taker.base || self.rel_orderbook_ticker.as_ref() == Some(&taker.base));
                let taker_price = taker_base_amount / taker_rel_amount;

                // Calculate the resulting base amount using the Maker's price instead of the Taker's.
                let matched_base_amount = taker_base_amount / &self.price;
                let matched_rel_amount = taker_base_amount.clone();

                if ticker_match
                    && matched_base_amount <= self.available_amount()
                    && matched_base_amount >= self.min_base_vol
                    && taker_price >= self.price
                {
                    OrderMatchResult::Matched((matched_base_amount, matched_rel_amount))
                } else {
                    OrderMatchResult::NotMatched
                }
            },
        }
    }

    pub(crate) fn apply_updated(&mut self, msg: &new_protocol::MakerOrderUpdated) {
        if let Some(new_price) = msg.new_price() {
            self.price = new_price;
        }

        if let Some(new_max_volume) = msg.new_max_volume() {
            self.max_base_vol = new_max_volume;
        }

        if let Some(new_min_volume) = msg.new_min_volume() {
            self.min_base_vol = new_min_volume;
        }

        if let Some(conf_settings) = msg.new_conf_settings() {
            self.conf_settings = conf_settings.into();
        }

        self.updated_at = Some(now_ms());
    }

    pub(crate) fn base_orderbook_ticker(&self) -> &str { self.base_orderbook_ticker.as_deref().unwrap_or(&self.base) }

    pub(crate) fn rel_orderbook_ticker(&self) -> &str { self.rel_orderbook_ticker.as_deref().unwrap_or(&self.rel) }

    pub(crate) fn orderbook_topic(&self) -> String {
        orderbook_topic_from_base_rel(self.base_orderbook_ticker(), self.rel_orderbook_ticker())
    }

    pub(crate) fn was_updated(&self) -> bool { self.updated_at != Some(self.created_at) }

    pub(crate) fn p2p_keypair(&self) -> Option<&KeyPair> { self.p2p_privkey.as_ref().map(|key| key.key_pair()) }
}

impl From<TakerOrder> for MakerOrder {
    fn from(taker_order: TakerOrder) -> Self {
        let created_at = now_ms();
        match taker_order.request.action {
            TakerAction::Sell => MakerOrder {
                price: (taker_order.request.get_rel_amount() / taker_order.request.get_base_amount()),
                max_base_vol: taker_order.request.get_base_amount().clone(),
                min_base_vol: taker_order.min_volume,
                created_at,
                updated_at: Some(created_at),
                base: taker_order.request.base,
                rel: taker_order.request.rel,
                matches: HashMap::new(),
                started_swaps: Vec::new(),
                uuid: taker_order.request.uuid,
                conf_settings: taker_order.request.conf_settings,
                changes_history: None,
                save_in_history: taker_order.save_in_history,
                base_orderbook_ticker: taker_order.base_orderbook_ticker,
                rel_orderbook_ticker: taker_order.rel_orderbook_ticker,
                p2p_privkey: taker_order.p2p_privkey,
                timeout_in_minutes: None,
                swap_version: taker_order.request.swap_version,
            },
            // The "buy" taker order is recreated with reversed pair as Maker order is always considered as "sell"
            TakerAction::Buy => {
                let price = taker_order.request.get_base_amount() / taker_order.request.get_rel_amount();
                let min_base_vol = &taker_order.min_volume / &price;
                MakerOrder {
                    price,
                    max_base_vol: taker_order.request.get_rel_amount().clone(),
                    min_base_vol,
                    created_at,
                    updated_at: Some(created_at),
                    base: taker_order.request.rel,
                    rel: taker_order.request.base,
                    matches: HashMap::new(),
                    started_swaps: Vec::new(),
                    uuid: taker_order.request.uuid,
                    conf_settings: taker_order.request.conf_settings.map(|s| s.reversed()),
                    changes_history: None,
                    save_in_history: taker_order.save_in_history,
                    base_orderbook_ticker: taker_order.rel_orderbook_ticker,
                    rel_orderbook_ticker: taker_order.base_orderbook_ticker,
                    p2p_privkey: taker_order.p2p_privkey,
                    timeout_in_minutes: None,
                    swap_version: taker_order.request.swap_version,
                }
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TakerConnect {
    pub(crate) taker_order_uuid: Uuid,
    pub(crate) maker_order_uuid: Uuid,
    pub(crate) sender_pubkey: H256Json,
    pub(crate) dest_pub_key: H256Json,
}

impl From<new_protocol::TakerConnect> for TakerConnect {
    fn from(message: new_protocol::TakerConnect) -> TakerConnect {
        TakerConnect {
            taker_order_uuid: message.taker_order_uuid.into(),
            maker_order_uuid: message.maker_order_uuid.into(),
            sender_pubkey: Default::default(),
            dest_pub_key: Default::default(),
        }
    }
}

impl From<TakerConnect> for new_protocol::OrdermatchMessage {
    fn from(taker_connect: TakerConnect) -> Self {
        new_protocol::OrdermatchMessage::TakerConnect(new_protocol::TakerConnect {
            taker_order_uuid: taker_connect.taker_order_uuid.into(),
            maker_order_uuid: taker_connect.maker_order_uuid.into(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(test, derive(Default))]
pub struct MakerReserved {
    pub(crate) base: String,
    pub(crate) rel: String,
    pub(crate) base_amount: MmNumber,
    pub(crate) rel_amount: MmNumber,
    pub(crate) taker_order_uuid: Uuid,
    pub(crate) maker_order_uuid: Uuid,
    pub(crate) sender_pubkey: H256Json,
    pub(crate) dest_pub_key: H256Json,
    pub(crate) conf_settings: Option<OrderConfirmationsSettings>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_protocol_info: Option<Vec<u8>>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel_protocol_info: Option<Vec<u8>>,
    /// Swap protocol version the maker advertises with this reservation.
    #[serde(default, skip_serializing_if = "SwapVersion::is_legacy")]
    pub swap_version: SwapVersion,
}

impl MakerReserved {
    pub(crate) fn get_base_amount(&self) -> &MmNumber { &self.base_amount }

    pub(crate) fn get_rel_amount(&self) -> &MmNumber { &self.rel_amount }

    pub(crate) fn price(&self) -> MmNumber { &self.rel_amount / &self.base_amount }
}

impl MakerReserved {
    pub(crate) fn from_new_proto_and_pubkey(message: new_protocol::MakerReserved, sender_pubkey: H256Json) -> Self {
        let base_amount = MmNumber::from(message.base_amount);
        let rel_amount = MmNumber::from(message.rel_amount);

        MakerReserved {
            base: message.base,
            rel: message.rel,
            base_amount,
            rel_amount,
            taker_order_uuid: message.taker_order_uuid.into(),
            maker_order_uuid: message.maker_order_uuid.into(),
            sender_pubkey,
            dest_pub_key: Default::default(),
            conf_settings: Some(message.conf_settings),
            base_protocol_info: message.base_protocol_info,
            rel_protocol_info: message.rel_protocol_info,
            swap_version: message.swap_version,
        }
    }
}

impl From<MakerReserved> for new_protocol::OrdermatchMessage {
    fn from(maker_reserved: MakerReserved) -> Self {
        new_protocol::OrdermatchMessage::MakerReserved(new_protocol::MakerReserved {
            base_amount: maker_reserved.get_base_amount().to_ratio(),
            rel_amount: maker_reserved.get_rel_amount().to_ratio(),
            base: maker_reserved.base,
            rel: maker_reserved.rel,
            taker_order_uuid: maker_reserved.taker_order_uuid.into(),
            maker_order_uuid: maker_reserved.maker_order_uuid.into(),
            conf_settings: maker_reserved.conf_settings.unwrap(),
            base_protocol_info: maker_reserved.base_protocol_info,
            rel_protocol_info: maker_reserved.rel_protocol_info,
            swap_version: maker_reserved.swap_version,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MakerConnected {
    pub(crate) taker_order_uuid: Uuid,
    pub(crate) maker_order_uuid: Uuid,
    pub(crate) method: String,
    pub(crate) sender_pubkey: H256Json,
    pub(crate) dest_pub_key: H256Json,
}

impl From<new_protocol::MakerConnected> for MakerConnected {
    fn from(message: new_protocol::MakerConnected) -> MakerConnected {
        MakerConnected {
            taker_order_uuid: message.taker_order_uuid.into(),
            maker_order_uuid: message.maker_order_uuid.into(),
            method: "".to_string(),
            sender_pubkey: Default::default(),
            dest_pub_key: Default::default(),
        }
    }
}

impl From<MakerConnected> for new_protocol::OrdermatchMessage {
    fn from(maker_connected: MakerConnected) -> Self {
        new_protocol::OrdermatchMessage::MakerConnected(new_protocol::MakerConnected {
            taker_order_uuid: maker_connected.taker_order_uuid.into(),
            maker_order_uuid: maker_connected.maker_order_uuid.into(),
        })
    }
}
