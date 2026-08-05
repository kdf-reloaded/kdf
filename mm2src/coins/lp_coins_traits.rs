use super::*;

/// Swap operations (mostly based on the Hash/Time locked transactions implemented by coin wallets).
#[async_trait]
pub trait SwapOps {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], uuid: &[u8]) -> TransactionFut;

    fn send_maker_payment(
        &self,
        time_lock: u32,
        maker_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut;

    fn send_taker_payment(
        &self,
        time_lock: u32,
        taker_pub: &[u8],
        maker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut;

    fn send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut;

    fn send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut;

    fn send_taker_refunds_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut;

    fn send_maker_refunds_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut;

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send>;

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send>;

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send>;

    fn check_if_my_payment_sent(
        &self,
        time_lock: u32,
        my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send>;

    async fn search_for_swap_tx_spend_my(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String>;

    async fn search_for_swap_tx_spend_other(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String>;

    fn extract_secret(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String>;

    /// Whether the refund transaction can be sent now
    /// For example: there are no additional conditions for ETH, but for some UTXO coins we should wait for
    /// locktime < MTP
    fn can_refund_htlc(&self, locktime: u64) -> Box<dyn Future<Item = CanRefundHtlc, Error = String> + Send + '_> {
        let now = now_ms() / 1000;
        let result = if now > locktime {
            CanRefundHtlc::CanRefundNow
        } else {
            CanRefundHtlc::HaveToWait(locktime - now + 1)
        };
        Box::new(futures01::future::ok(result))
    }

    fn negotiate_swap_contract_addr(
        &self,
        other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>>;

    fn get_htlc_key_pair(&self) -> Option<KeyPair>;
}
/// Operations required for watcher node functionality.
///
/// Coins that support watcher nodes implement these methods to allow third-party
/// watchers to validate, spend, or refund swap payments when one party disappears.
#[async_trait]
pub trait WatcherOps {
    /// Whether this coin supports being monitored by watcher nodes.
    fn is_supported_by_watchers(&self) -> bool { false }

    /// Watcher-specific taker fee validation (retrieves tx from chain by hash).
    fn watcher_validate_taker_fee(
        &self,
        _input: WatcherValidateTakerFeeInput,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(
            "watcher_validate_taker_fee not supported".into(),
        ))
    }

    /// Watcher-specific taker payment validation (checks script, amounts, confirmations).
    fn watcher_validate_taker_payment(
        &self,
        _input: WatcherValidatePaymentInput,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(
            "watcher_validate_taker_payment not supported".into(),
        ))
    }

    /// Create a preimage transaction that spends the maker payment (watcher executes on success).
    fn create_maker_payment_spend_preimage(
        &self,
        _maker_payment_tx: &[u8],
        _time_lock: u32,
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _swap_unique_data: &[u8],
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(
            "create_maker_payment_spend_preimage not supported".into(),
        )))
    }

    /// Create a preimage transaction that refunds the taker payment (watcher executes on timeout).
    fn create_taker_payment_refund_preimage(
        &self,
        _taker_payment_tx: &[u8],
        _time_lock: u32,
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _swap_unique_data: &[u8],
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(
            "create_taker_payment_refund_preimage not supported".into(),
        )))
    }

    /// Watcher search for how a swap tx was spent (by secret reveal or by refund).
    async fn watcher_search_for_swap_tx_spend(
        &self,
        _time_lock: u32,
        _other_pub: &[u8],
        _secret_hash: &[u8],
        _tx: &[u8],
        _search_from_block: u64,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        Err("watcher_search_for_swap_tx_spend not supported".into())
    }
}
/// Operations that coins have independently from the MarketMaker.
/// That is, things implemented by the coin wallets or public coin services.
pub trait MarketCoinOps {
    fn ticker(&self) -> &str;

    fn my_address(&self) -> Result<String, String>;

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>>;

    fn sign_message_hash(&self, _message: &str) -> Option<[u8; 32]>;

    fn sign_message(&self, _message: &str) -> SignatureResult<String>;

    fn verify_message(&self, _signature: &str, _message: &str, _address: &str) -> VerificationResult<bool>;

    fn get_non_zero_balance(&self) -> NonZeroBalanceFut<MmNumber> {
        let closure = |spendable: BigDecimal| {
            if spendable.is_zero() {
                return MmError::err(GetNonZeroBalance::BalanceIsZero);
            }
            Ok(MmNumber::from(spendable))
        };
        Box::new(
            self.my_spendable_balance()
                .map_err(|e| e.map(GetNonZeroBalance::from))
                .and_then(closure),
        )
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance>;

    fn my_spendable_balance(&self) -> BalanceFut<BigDecimal> {
        Box::new(self.my_balance().map(|CoinBalance { spendable, .. }| spendable))
    }

    /// Base coin balance for tokens, e.g. ETH balance in ERC20 case
    fn base_coin_balance(&self) -> BalanceFut<BigDecimal>;
    fn platform_ticker(&self) -> &str;

    /// Receives raw transaction bytes in hexadecimal format as input and returns tx hash in hexadecimal format
    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send>;

    /// Receives raw transaction bytes as input and returns tx hash in hexadecimal format
    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send>;

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send>;

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut;

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String>;

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send>;

    fn display_priv_key(&self) -> Result<String, String>;

    /// Get the minimum amount to send.
    fn min_tx_amount(&self) -> BigDecimal;

    /// Get the minimum amount to trade.
    fn min_trading_vol(&self) -> MmNumber;

    /// Signs a raw transaction without broadcasting.
    /// Default implementation returns NotImplemented for coins that don't support it.
    fn sign_raw_tx(&self, _args: &SignRawTransactionRequest) -> RawTransactionFut {
        let coin = self.ticker().to_string();
        Box::new(futures01::future::err(MmError::new(
            RawTransactionError::NotImplemented { coin },
        )))
    }

    fn is_privacy(&self) -> bool { false }
}
/// Rename to `GetWithdrawSenderAddresses` when withdraw supports multiple `from` addresses.
#[async_trait]
pub trait GetWithdrawSenderAddress {
    type Address;
    type Pubkey;

    async fn get_withdraw_sender_address(
        &self,
        req: &WithdrawRequest,
    ) -> MmResult<WithdrawSenderAddress<Self::Address, Self::Pubkey>, WithdrawError>;
}
/// Conversion to raw bytes, used by V2 swap associated types (pubkeys, preimages, signatures).
pub trait ToBytes {
    fn to_bytes(&self) -> Vec<u8>;
}
/// Blanket impl: anything that can deref to `[u8]` can produce bytes.
impl<T: AsRef<[u8]>> ToBytes for T {
    fn to_bytes(&self) -> Vec<u8> { self.as_ref().to_vec() }
}
/// Converts an address to its canonical string representation.
/// Separated from `fmt::Display` so impls can have both a debug repr and a wire repr.
pub trait AddrToString {
    fn addr_to_string(&self) -> String;
}
/// Blanket: any `Display` type can produce an address string.
impl<T: fmt::Display> AddrToString for T {
    fn addr_to_string(&self) -> String { self.to_string() }
}
/// Allows a coin to parse its strongly-typed associated types from raw bytes.
///
/// Every V2 coin must define concrete types for addresses, pubkeys, transactions,
/// preimages (unsigned-tx fragments), and signatures.  The `parse_*` methods
/// reconstruct those types from the byte representations exchanged over P2P.
#[async_trait]
pub trait ParseCoinAssocTypes {
    type Address: Send + Sync + fmt::Display + AddrToString;
    type AddressParseError: fmt::Debug + Send + fmt::Display;
    type Pubkey: ToBytes + Send + Sync;
    type PubkeyParseError: fmt::Debug + Send + fmt::Display;
    type Tx: Transaction + Send + Sync;
    type TxParseError: fmt::Debug + Send + fmt::Display;
    type Preimage: ToBytes + Send + Sync;
    type PreimageParseError: fmt::Debug + Send + fmt::Display;
    type Sig: ToBytes + Send + Sync;
    type SigParseError: fmt::Debug + Send + fmt::Display;

    /// Returns this coin's current HTLC address.
    async fn my_addr(&self) -> Self::Address;

    /// Fallible form of [`ParseCoinAssocTypes::my_addr`] for V2 paths that can
    /// report address-selection errors before constructing P2P messages.
    async fn try_my_addr(&self) -> Result<Self::Address, String> { Ok(self.my_addr().await) }

    fn parse_address(&self, address: &str) -> Result<Self::Address, Self::AddressParseError>;
    fn parse_pubkey(&self, pubkey: &[u8]) -> Result<Self::Pubkey, Self::PubkeyParseError>;
    fn parse_tx(&self, tx: &[u8]) -> Result<Self::Tx, Self::TxParseError>;
    fn parse_preimage(&self, preimage: &[u8]) -> Result<Self::Preimage, Self::PreimageParseError>;
    fn parse_signature(&self, sig: &[u8]) -> Result<Self::Sig, Self::SigParseError>;
}
/// Operations shared between maker-coin and taker-coin V2 roles.
#[async_trait]
pub trait CommonSwapOpsV2: ParseCoinAssocTypes + Send + Sync + 'static {
    /// Derive the HTLC pubkey for this swap (may differ from the main wallet pubkey).
    fn derive_htlc_pubkey_v2(&self, swap_unique_data: &[u8]) -> Self::Pubkey;

    /// Fallible form of [`CommonSwapOpsV2::derive_htlc_pubkey_v2`] used by
    /// state-machine call sites that must reject unsupported key policies
    /// before P2P negotiation or transaction construction.
    fn try_derive_htlc_pubkey_v2(&self, swap_unique_data: &[u8]) -> Result<Self::Pubkey, String> {
        Ok(self.derive_htlc_pubkey_v2(swap_unique_data))
    }

    /// Same as [`derive_htlc_pubkey_v2`] but returns raw bytes for P2P transmission.
    fn derive_htlc_pubkey_v2_bytes(&self, swap_unique_data: &[u8]) -> Vec<u8>;

    /// Fallible form of [`CommonSwapOpsV2::derive_htlc_pubkey_v2_bytes`].
    fn try_derive_htlc_pubkey_v2_bytes(&self, swap_unique_data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.derive_htlc_pubkey_v2_bytes(swap_unique_data))
    }
}
/// V2 swap operations for the **maker coin** (the coin the maker locks first).
#[async_trait]
pub trait MakerCoinSwapOpsV2: ParseCoinAssocTypes + CommonSwapOpsV2 + Send + Sync + 'static {
    async fn send_maker_payment_v2(&self, args: SendMakerPaymentArgs<'_, Self>) -> Result<Self::Tx, TransactionErr>;

    async fn validate_maker_payment_v2(&self, args: ValidateMakerPaymentArgs<'_, Self>) -> ValidateSwapV2TxResult;

    async fn refund_maker_payment_v2_timelock(
        &self,
        args: RefundMakerPaymentTimelockArgs<'_>,
    ) -> Result<Self::Tx, TransactionErr>;

    async fn refund_maker_payment_v2_secret(
        &self,
        args: RefundMakerPaymentSecretArgs<'_, Self>,
    ) -> Result<Self::Tx, TransactionErr>;

    async fn spend_maker_payment_v2(&self, args: SpendMakerPaymentArgs<'_, Self>) -> Result<Self::Tx, TransactionErr>;
}
/// V2 swap operations for the **taker coin** (funding + taker payment).
#[async_trait]
pub trait TakerCoinSwapOpsV2: ParseCoinAssocTypes + CommonSwapOpsV2 + Send + Sync + 'static {
    async fn send_taker_funding(&self, args: SendTakerFundingArgs<'_>) -> Result<Self::Tx, TransactionErr>;

    async fn validate_taker_funding(&self, args: ValidateTakerFundingArgs<'_, Self>) -> ValidateSwapV2TxResult;

    async fn refund_taker_funding_timelock(&self, args: RefundTakerPaymentArgs<'_>)
        -> Result<Self::Tx, TransactionErr>;

    async fn refund_taker_funding_secret(
        &self,
        args: RefundFundingSecretArgs<'_, Self>,
    ) -> Result<Self::Tx, TransactionErr>;

    /// Look up on-chain how the taker funding output was consumed.
    async fn search_for_taker_funding_spend(
        &self,
        tx: &Self::Tx,
        from_block: u64,
        secret_hash: &[u8],
    ) -> Result<Option<FundingTxSpend<Self>>, SearchForFundingSpendErr>;

    /// Maker generates a preimage that will convert funding → taker payment.
    async fn gen_taker_funding_spend_preimage(
        &self,
        args: &GenTakerFundingSpendArgs<'_, Self>,
        swap_unique_data: &[u8],
    ) -> GenPreimageResult<Self>;

    /// Taker validates the funding-spend preimage from the maker.
    async fn validate_taker_funding_spend_preimage(
        &self,
        gen_args: &GenTakerFundingSpendArgs<'_, Self>,
        preimage: &TxPreimageWithSig<Self>,
    ) -> ValidateTakerFundingSpendPreimageResult;

    /// Taker co-signs and broadcasts the funding spend → taker payment.
    async fn sign_and_send_taker_funding_spend(
        &self,
        preimage: &TxPreimageWithSig<Self>,
        args: &GenTakerFundingSpendArgs<'_, Self>,
        swap_unique_data: &[u8],
    ) -> Result<Self::Tx, TransactionErr>;

    /// Refund a combined taker payment (after it was created from funding).
    async fn refund_combined_taker_payment(&self, args: RefundTakerPaymentArgs<'_>)
        -> Result<Self::Tx, TransactionErr>;

    /// Whether this coin can skip taker payment spend preimage validation.
    /// Returns `true` for EVM coins that don't need preimage exchange.
    fn skip_taker_payment_spend_preimage(&self) -> bool { false }

    /// Taker generates the taker-payment spend preimage for the maker.
    async fn gen_taker_payment_spend_preimage(
        &self,
        args: &GenTakerPaymentSpendArgs<'_, Self>,
        swap_unique_data: &[u8],
    ) -> GenPreimageResult<Self>;

    /// Maker validates the taker-payment spend preimage.
    async fn validate_taker_payment_spend_preimage(
        &self,
        gen_args: &GenTakerPaymentSpendArgs<'_, Self>,
        preimage: &TxPreimageWithSig<Self>,
    ) -> ValidateTakerPaymentSpendPreimageResult;

    /// Maker signs and broadcasts the taker payment spend (revealing maker secret).
    async fn sign_and_broadcast_taker_payment_spend(
        &self,
        preimage: Option<&TxPreimageWithSig<Self>>,
        gen_args: &GenTakerPaymentSpendArgs<'_, Self>,
        secret: &[u8],
        swap_unique_data: &[u8],
    ) -> Result<Self::Tx, TransactionErr>;

    /// Wait for the taker payment to be spent on-chain.
    async fn find_taker_payment_spend_tx(
        &self,
        taker_payment: &Self::Tx,
        from_block: u64,
        wait_until: u64,
    ) -> MmResult<Self::Tx, FindPaymentSpendError>;

    /// Extract the maker secret from the taker payment spend transaction.
    async fn extract_secret_v2(&self, secret_hash: &[u8], spend_tx: &Self::Tx) -> Result<[u8; 32], String>;
}
/// NB: Implementations are expected to follow the pImpl idiom, providing cheap reference-counted cloning and garbage collection.
#[async_trait]
pub trait MmCoin: SwapOps + WatcherOps + MarketCoinOps + fmt::Debug + Send + Sync + 'static {
    // `MmCoin` is an extension fulcrum for something that doesn't fit the `MarketCoinOps`. Practical examples:
    // name (might be required for some APIs, CoinMarketCap for instance);
    // coin statistics that we might want to share with UI;
    // state serialization, to get full rewind and debugging information about the coins participating in a SWAP operation.
    // status/availability check: https://github.com/artemii235/SuperNET/issues/156#issuecomment-446501816

    fn is_asset_chain(&self) -> bool;

    /// The coin can be initialized, but it cannot participate in the swaps.
    fn wallet_only(&self, ctx: &MmArc) -> bool {
        let coin_conf = coin_conf(ctx, self.ticker());
        coin_conf["wallet_only"].as_bool().unwrap_or(false)
    }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut;

    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut;

    /// Maximum number of digits after decimal point used to denominate integer coin units (satoshis, wei, etc.)
    fn decimals(&self) -> u8;

    /// Convert input address to the specified address format.
    fn convert_to_address(&self, from: &str, to_address_format: Json) -> Result<String, String>;

    fn validate_address(&self, address: &str) -> ValidateAddressResult;

    /// Loop collecting coin transaction history and saving it to local DB
    fn process_history_loop(&self, ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send>;

    /// Path to tx history file
    fn tx_history_path(&self, ctx: &MmArc) -> PathBuf {
        let my_address = self.my_address().unwrap_or_default();
        // BCH cash address format has colon after prefix, e.g. bitcoincash:
        // Colon can't be used in file names on Windows so it should be escaped
        let my_address = my_address.replace(':', "_");
        ctx.dbdir()
            .join("TRANSACTIONS")
            .join(format!("{}_{}.json", self.ticker(), my_address))
    }

    /// Loads existing tx history from file, returns empty vector if file is not found
    /// Cleans the existing file if deserialization fails
    fn load_history_from_file(&self, ctx: &MmArc) -> TxHistoryFut<Vec<TransactionDetails>> {
        load_history_from_file_impl(self, ctx)
    }

    fn save_history_to_file(&self, ctx: &MmArc, history: Vec<TransactionDetails>) -> TxHistoryFut<()> {
        save_history_to_file_impl(self, ctx, history)
    }

    /// Transaction history background sync status
    fn history_sync_status(&self) -> HistorySyncState;

    /// Get fee to be paid per 1 swap transaction
    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send>;

    /// Get fee to be paid by sender per whole swap using the sending value and check if the wallet has sufficient balance to pay the fee.
    async fn get_sender_trade_fee(
        &self,
        value: TradePreimageValue,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee>;

    /// Get fee to be paid by receiver per whole swap and check if the wallet has sufficient balance to pay the fee.
    fn get_receiver_trade_fee(&self, stage: FeeApproxStage) -> TradePreimageFut<TradeFee>;

    /// Get transaction fee the Taker has to pay to send a `TakerFee` transaction and check if the wallet has sufficient balance to pay the fee.
    async fn get_fee_to_send_taker_fee(
        &self,
        dex_fee_amount: BigDecimal,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee>;

    /// required transaction confirmations number to ensure double-spend safety
    fn required_confirmations(&self) -> u64;

    /// whether coin requires notarization to ensure double-spend safety
    fn requires_notarization(&self) -> bool;

    /// set required transaction confirmations number
    fn set_required_confirmations(&self, confirmations: u64);

    /// set requires notarization
    fn set_requires_notarization(&self, requires_nota: bool);

    /// Get swap contract address if the coin uses it in Atomic Swaps.
    fn swap_contract_address(&self) -> Option<BytesJson>;

    /// The minimum number of confirmations at which a transaction is considered mature.
    fn mature_confirmations(&self) -> Option<u32>;

    /// Get some of the coin config info in serialized format for p2p messaging.
    fn coin_protocol_info(&self) -> Vec<u8>;

    /// Check if serialized coin protocol info is supported by current version.
    fn is_coin_protocol_supported(&self, info: &Option<Vec<u8>>) -> bool;

    /// Compressed public key of the network-designated burn address for this
    /// coin family. Empty when burn is not configured for the coin.
    fn burn_pubkey(&self) -> Vec<u8> { Vec::new() }

    /// True iff the burn portion should be attached as an `OP_RETURN` output
    /// rather than sent to a P2PKH burn address (currently KMD-only).
    fn should_burn_directly(&self) -> bool { false }

    /// True iff this coin participates in the pre-burn DEX-fee split.
    fn should_burn_dex_fee(&self) -> bool { false }
}
#[async_trait]
pub trait BalanceTradeFeeUpdatedHandler {
    async fn balance_updated(&self, coin: &MmCoinEnum, new_balance: &BigDecimal);
}
#[async_trait]
pub trait CoinWithDerivationMethod {
    type Address;
    type HDWallet;

    fn derivation_method(&self) -> &DerivationMethod<Self::Address, Self::HDWallet>;

    fn has_hd_wallet_derivation_method(&self) -> bool {
        matches!(self.derivation_method(), DerivationMethod::HDWallet(_))
    }
}
pub type RpcTransportEventHandlerShared = Arc<dyn RpcTransportEventHandler + Send + Sync + 'static>;
/// Common methods to measure the outgoing requests and incoming responses statistics.
pub trait RpcTransportEventHandler {
    fn debug_info(&self) -> String;

    fn on_outgoing_request(&self, data: &[u8]);

    fn on_incoming_response(&self, data: &[u8]);

    fn on_connected(&self, address: String) -> Result<(), String>;
}
impl fmt::Debug for dyn RpcTransportEventHandler + Send + Sync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.debug_info()) }
}
impl RpcTransportEventHandler for RpcTransportEventHandlerShared {
    fn debug_info(&self) -> String { self.deref().debug_info() }

    fn on_outgoing_request(&self, data: &[u8]) { self.as_ref().on_outgoing_request(data) }

    fn on_incoming_response(&self, data: &[u8]) { self.as_ref().on_incoming_response(data) }

    fn on_connected(&self, address: String) -> Result<(), String> { self.as_ref().on_connected(address) }
}
impl<T: RpcTransportEventHandler> RpcTransportEventHandler for Vec<T> {
    fn debug_info(&self) -> String {
        let selfi: Vec<String> = self.iter().map(|x| x.debug_info()).collect();
        format!("{:?}", selfi)
    }

    fn on_outgoing_request(&self, data: &[u8]) {
        for handler in self {
            handler.on_outgoing_request(data)
        }
    }

    fn on_incoming_response(&self, data: &[u8]) {
        for handler in self {
            handler.on_incoming_response(data)
        }
    }

    fn on_connected(&self, address: String) -> Result<(), String> {
        for handler in self {
            try_s!(handler.on_connected(address.clone()))
        }
        Ok(())
    }
}
