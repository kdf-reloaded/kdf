// utxo_common_helpers — address utilities, balance queries, config, withdraw

use super::*;

const MIN_BTC_TRADING_VOL: &str = "0.00777";
const FIXED_FEE_MIN_VOL_TX_SIZE_BYTES: u64 = 496;

/// Requests balance of the given `address`.
pub async fn address_balance<T>(coin: &T, address: &Address) -> BalanceResult<CoinBalance>
where
    T: UtxoCommonOps + GetUtxoListOps + MarketCoinOps,
{
    if coin.as_ref().check_utxo_maturity {
        let (unspents, _) = coin.get_mature_unspent_ordered_list(address).await.mm_err(Into::into)?;
        return Ok(unspents.to_coin_balance(coin.as_ref().decimals));
    }

    let balance = coin
        .as_ref()
        .rpc_client
        .display_balance(address.clone(), coin.as_ref().decimals)
        .compat()
        .await?;

    // Electrum `display_balance` tracks the address script-hash (P2PKH/P2SH)
    // and doesn't include legacy pay-to-pubkey outputs. Add P2PK script-level
    // unspents for this address when applicable.
    let p2pk_extra = crate::utxo::electrum_p2pk_unspents_for_address(coin.as_ref(), address)
        .await
        .mm_err(Into::into)?
        .into_iter()
        .fold(BigDecimal::default(), |acc, unspent| {
            acc + big_decimal_from_sat_unsigned(unspent.value, coin.as_ref().decimals)
        });

    Ok(CoinBalance {
        spendable: balance + p2pk_extra,
        unspendable: BigDecimal::from(0),
        ..Default::default()
    })
}

/// Requests balances of the given `addresses`.
/// The pairs `(Address, CoinBalance)` are guaranteed to be in the same order in which they were requested.
pub async fn addresses_balances<T>(coin: &T, addresses: Vec<Address>) -> BalanceResult<Vec<(Address, CoinBalance)>>
where
    T: UtxoCommonOps + GetUtxoMapOps + MarketCoinOps,
{
    if coin.as_ref().check_utxo_maturity {
        let (unspents_map, _) = coin
            .get_mature_unspent_ordered_map(addresses.clone())
            .await
            .mm_err(Into::into)?;
        addresses
            .into_iter()
            .map(|address| {
                let unspents = unspents_map.get(&address).or_mm_err(|| {
                    let error = format!("'get_mature_unspent_ordered_map' should have returned '{}'", address);
                    BalanceError::Internal(error)
                })?;
                let balance = unspents.to_coin_balance(coin.as_ref().decimals);
                Ok((address, balance))
            })
            .collect()
    } else {
        Ok(coin
            .as_ref()
            .rpc_client
            .display_balances(addresses.clone(), coin.as_ref().decimals)
            .compat()
            .await
            .mm_err(Into::into)?
            .into_iter()
            .map(|(address, spendable)| {
                let unspendable = BigDecimal::from(0);
                let balance = CoinBalance {
                    spendable,
                    unspendable,
                    ..Default::default()
                };
                (address, balance)
            })
            .collect())
    }
}

pub fn derivation_method(coin: &UtxoCoinFields) -> &DerivationMethod<Address, UtxoHDWallet> { &coin.derivation_method }

pub fn addresses_from_script<T: UtxoCommonOps>(coin: &T, script: &Script) -> Result<Vec<Address>, String> {
    let destinations: Vec<ScriptAddress> = try_s!(script.extract_destinations());

    let conf = &coin.as_ref().conf;

    let addresses = destinations
        .into_iter()
        .map(|dst| {
            let (prefix, t_addr_prefix, addr_format) = match dst.kind {
                ScriptType::P2PKH => (
                    conf.pub_addr_prefix,
                    conf.pub_t_addr_prefix,
                    coin.addr_format_for_standard_scripts(),
                ),
                ScriptType::P2SH => (
                    conf.p2sh_addr_prefix,
                    conf.p2sh_t_addr_prefix,
                    coin.addr_format_for_standard_scripts(),
                ),
                ScriptType::P2WPKH => (conf.pub_addr_prefix, conf.pub_t_addr_prefix, UtxoAddressFormat::Segwit),
                ScriptType::P2WSH => (conf.pub_addr_prefix, conf.pub_t_addr_prefix, UtxoAddressFormat::Segwit),
            };

            Address {
                hash: dst.hash,
                checksum_type: conf.checksum_type,
                prefix,
                t_addr_prefix,
                hrp: conf.bech32_hrp.clone(),
                addr_format,
            }
        })
        .collect();

    Ok(addresses)
}

pub fn denominate_satoshis(coin: &UtxoCoinFields, satoshi: i64) -> f64 {
    satoshi as f64 / 10f64.powf(coin.decimals as f64)
}

pub fn base_coin_balance<T>(coin: &T) -> BalanceFut<BigDecimal>
where
    T: MarketCoinOps,
{
    coin.my_spendable_balance()
}

pub fn address_from_str_unchecked(coin: &UtxoCoinFields, address: &str) -> Result<Address, String> {
    if let Ok(legacy) = Address::from_str(address) {
        return Ok(legacy);
    }

    if let Ok(segwit) = Address::from_segwitaddress(
        address,
        coin.conf.checksum_type,
        coin.conf.pub_addr_prefix,
        coin.conf.pub_t_addr_prefix,
    ) {
        return Ok(segwit);
    }

    if let Ok(cashaddress) = Address::from_cashaddress(
        address,
        coin.conf.checksum_type,
        coin.conf.pub_addr_prefix,
        coin.conf.p2sh_addr_prefix,
        coin.conf.pub_t_addr_prefix,
    ) {
        return Ok(cashaddress);
    }

    return ERR!("Invalid address: {}", address);
}

pub fn my_public_key(coin: &UtxoCoinFields) -> Result<&Public, MmError<UnexpectedDerivationMethod>> {
    match coin.priv_key_policy {
        PrivKeyPolicy::KeyPair(ref key_pair) => Ok(key_pair.public()),
        PrivKeyPolicy::HDWallet { ref activated_key, .. } => Ok(activated_key.public()),
        // Hardware Wallets requires BIP39/BIP44 derivation path to extract a public key.
        PrivKeyPolicy::Trezor => MmError::err(UnexpectedDerivationMethod::IguanaPrivKeyUnavailable),
    }
}

pub fn checked_address_from_str<T: UtxoCommonOps>(coin: &T, address: &str) -> Result<Address, String> {
    let addr = try_s!(address_from_str_unchecked(coin.as_ref(), address));
    try_s!(check_withdraw_address_supported(coin, &addr));
    Ok(addr)
}

pub async fn get_current_mtp(coin: &UtxoCoinFields, coin_variant: CoinVariant) -> UtxoRpcResult<u32> {
    let current_block = coin.rpc_client.get_block_count().compat().await?;
    coin.rpc_client
        .get_median_time_past(current_block, coin.conf.mtp_block_count, coin_variant)
        .compat()
        .await
}

pub fn my_address<T: UtxoCommonOps>(coin: &T) -> Result<String, String> {
    match coin.as_ref().derivation_method {
        DerivationMethod::Iguana(ref my_address) => my_address.display_address(),
        DerivationMethod::HDWallet(_) => ERR!("'my_address' is deprecated for HD wallets"),
    }
}

/// Hash message for signature using Bitcoin's message signing format.
/// sha256(sha256(PREFIX_LENGTH + PREFIX + MESSAGE_LENGTH + MESSAGE))
pub fn sign_message_hash(coin: &UtxoCoinFields, message: &str) -> Option<[u8; 32]> {
    let message_prefix = coin.conf.sign_message_prefix.clone()?;
    let mut stream = Stream::new();
    let prefix_len = CompactInteger::from(message_prefix.len());
    prefix_len.serialize(&mut stream);
    stream.append_slice(message_prefix.as_bytes());
    let msg_len = CompactInteger::from(message.len());
    msg_len.serialize(&mut stream);
    stream.append_slice(message.as_bytes());
    Some(dhash256(&stream.out()).take())
}

pub fn sign_message(coin: &UtxoCoinFields, message: &str) -> SignatureResult<String> {
    let message_hash = sign_message_hash(coin, message).ok_or(SignatureError::PrefixNotFound)?;
    let private_key = coin.priv_key_policy.key_pair_or_err().mm_err(Into::into)?.private();
    let signature = private_key.sign_compact(&H256::from(message_hash))?;
    Ok(base64::encode(&*signature))
}

pub fn verify_message<T: UtxoCommonOps>(
    coin: &T,
    signature_base64: &str,
    message: &str,
    address: &str,
) -> VerificationResult<bool> {
    let message_hash = sign_message_hash(coin.as_ref(), message).ok_or(VerificationError::PrefixNotFound)?;
    let signature = CompactSignature::from(base64::decode(signature_base64)?);
    let recovered_pubkey = Public::recover_compact(&H256::from(message_hash), &signature)?;
    let received_address = checked_address_from_str(coin, address).map_err(VerificationError::AddressDecodingError)?;
    Ok(AddressHashEnum::from(recovered_pubkey.address_hash()) == received_address.hash)
}

pub fn my_balance<T>(coin: T) -> BalanceFut<CoinBalance>
where
    T: UtxoCommonOps + GetUtxoListOps + MarketCoinOps,
{
    let my_address = try_f!(coin
        .as_ref()
        .derivation_method
        .iguana_or_err()
        .mm_err(BalanceError::from))
    .clone();
    let fut = async move { address_balance(&coin, &my_address).await };
    Box::new(fut.boxed().compat())
}

pub fn wait_for_confirmations(
    coin: &UtxoCoinFields,
    tx: &[u8],
    confirmations: u64,
    requires_nota: bool,
    wait_until: u64,
    check_every: u64,
) -> Box<dyn Future<Item = (), Error = String> + Send> {
    let mut tx: UtxoTx = try_fus!(deserialize(tx).map_err(|e| ERRL!("{:?}", e)));
    tx.tx_hash_algo = coin.tx_hash_algo;
    coin.rpc_client.wait_for_confirmations(
        tx.hash().reversed().into(),
        tx.expiry_height,
        confirmations as u32,
        requires_nota,
        wait_until,
        check_every,
    )
}

pub fn wait_for_output_spend(
    coin: &UtxoCoinFields,
    tx_bytes: &[u8],
    output_index: usize,
    from_block: u64,
    wait_until: u64,
) -> TransactionFut {
    let mut tx: UtxoTx = try_tx_fus!(deserialize(tx_bytes).map_err(|e| ERRL!("{:?}", e)));
    tx.tx_hash_algo = coin.tx_hash_algo;
    let client = coin.rpc_client.clone();
    let tx_hash_algo = coin.tx_hash_algo;
    let fut = async move {
        loop {
            match client
                .find_output_spend(
                    tx.hash(),
                    &tx.outputs[output_index].script_pubkey,
                    output_index,
                    BlockHashOrHeight::Height(from_block as i64),
                )
                .compat()
                .await
            {
                Ok(Some(spent_output_info)) => {
                    let mut tx = spent_output_info.spending_tx;
                    tx.tx_hash_algo = tx_hash_algo;
                    return Ok(tx.into());
                },
                Ok(None) => (),
                Err(e) => {
                    log!("Error " (e) " on find_output_spend of tx " [e]);
                },
            };

            if now_ms() / 1000 > wait_until {
                return TX_PLAIN_ERR!(
                    "Waited too long until {} for transaction {:?} {} to be spent ",
                    wait_until,
                    tx,
                    output_index,
                );
            }
            Timer::sleep(10.).await;
        }
    };
    Box::new(fut.boxed().compat())
}

pub fn current_block(coin: &UtxoCoinFields) -> Box<dyn Future<Item = u64, Error = String> + Send> {
    Box::new(coin.rpc_client.get_block_count().map_err(|e| ERRL!("{}", e)))
}

pub fn display_priv_key(coin: &UtxoCoinFields) -> Result<String, String> {
    match coin.priv_key_policy {
        PrivKeyPolicy::KeyPair(ref key_pair) => Ok(key_pair.private().to_string()),
        PrivKeyPolicy::HDWallet { ref activated_key, .. } => Ok(activated_key.private().to_string()),
        PrivKeyPolicy::Trezor => ERR!("'display_priv_key' doesn't support Hardware Wallets"),
    }
}

pub fn min_tx_amount(coin: &UtxoCoinFields) -> BigDecimal {
    big_decimal_from_sat(coin.dust_amount as i64, coin.decimals)
}

pub fn min_trading_vol(coin: &UtxoCoinFields) -> MmNumber {
    if coin.conf.ticker == "BTC" {
        return MmNumber::from(MIN_BTC_TRADING_VOL);
    }

    let min_vol_sat = non_btc_min_trading_vol_sat(coin.dust_amount, &coin.tx_fee);
    big_decimal_from_sat_unsigned(min_vol_sat, coin.decimals).into()
}

fn non_btc_min_trading_vol_sat(dust_amount: u64, tx_fee: &TxFee) -> u64 {
    let dust_based = dust_amount.saturating_mul(10);

    let fee_based = match tx_fee {
        TxFee::Dynamic(_) => 0,
        // Fixed-fee policy uses a representative tx size (~496 bytes) and
        // rounds up to avoid underestimating required trading volume.
        TxFee::FixedPerKb(fee_per_kb) => {
            let fee_for_repr_tx = fee_per_kb
                .saturating_mul(FIXED_FEE_MIN_VOL_TX_SIZE_BYTES)
                .saturating_add(KILO_BYTE - 1)
                / KILO_BYTE;
            fee_for_repr_tx.saturating_mul(10)
        },
    };

    std::cmp::max(dust_based, fee_based)
}

pub fn is_asset_chain(coin: &UtxoCoinFields) -> bool { coin.conf.asset_chain }

pub async fn get_raw_transaction(coin: &UtxoCoinFields, req: RawTransactionRequest) -> RawTransactionResult {
    let hash = H256Json::from_str(&req.tx_hash).map_to_mm(|e| RawTransactionError::InvalidHashError(e.to_string()))?;
    let hex = coin
        .rpc_client
        .get_transaction_bytes(&hash)
        .compat()
        .await
        .map_err(|e| RawTransactionError::Transport(e.to_string()))?;
    Ok(RawTransactionRes { tx_hex: hex })
}

pub async fn withdraw<T>(coin: T, req: WithdrawRequest) -> WithdrawResult
where
    T: UtxoCommonOps + GetUtxoListOps + MarketCoinOps,
{
    StandardUtxoWithdraw::new(coin, req)?.build().await
}

pub async fn init_withdraw<T>(
    ctx: MmArc,
    coin: T,
    req: WithdrawRequest,
    task_handle: &WithdrawTaskHandle,
) -> WithdrawResult
where
    T: UtxoCommonOps
        + GetUtxoListOps
        + UtxoSignerOps
        + CoinWithDerivationMethod
        + GetWithdrawSenderAddress<Address = Address, Pubkey = Public>,
{
    InitUtxoWithdraw::new(ctx, coin, req, task_handle).await?.build().await
}

pub async fn get_withdraw_from_address<T>(
    coin: &T,
    req: &WithdrawRequest,
) -> MmResult<WithdrawSenderAddress<Address, Public>, WithdrawError>
where
    T: CoinWithDerivationMethod<Address = Address, HDWallet = <T as HDWalletCoinOps>::HDWallet>
        + HDWalletCoinOps<Address = Address, Pubkey = Public>
        + UtxoCommonOps,
{
    match coin.derivation_method() {
        DerivationMethod::Iguana(my_address) => get_withdraw_iguana_sender(coin, req, my_address),
        DerivationMethod::HDWallet(hd_wallet) => get_withdraw_hd_sender(coin, req, hd_wallet).await,
    }
}

pub fn get_withdraw_iguana_sender<T: UtxoCommonOps>(
    coin: &T,
    req: &WithdrawRequest,
    my_address: &Address,
) -> MmResult<WithdrawSenderAddress<Address, Public>, WithdrawError> {
    if req.from.is_some() {
        let error = "'from' is not supported if the coin is initialized with an Iguana private key";
        return MmError::err(WithdrawError::UnexpectedFromAddress(error.to_owned()));
    }
    let pubkey = coin
        .my_public_key()
        .mm_err(|e| WithdrawError::InternalError(e.to_string()))?;
    Ok(WithdrawSenderAddress {
        address: my_address.clone(),
        pubkey: *pubkey,
        derivation_path: None,
    })
}

pub fn decimals(coin: &UtxoCoinFields) -> u8 { coin.decimals }

pub fn convert_to_address<T: UtxoCommonOps>(coin: &T, from: &str, to_address_format: Json) -> Result<String, String> {
    let to_address_format: UtxoAddressFormat =
        json::from_value(to_address_format).map_err(|e| ERRL!("Error on parse UTXO address format {:?}", e))?;
    let mut from_address = try_s!(coin.address_from_str(from));
    match to_address_format {
        UtxoAddressFormat::Standard => {
            from_address.addr_format = UtxoAddressFormat::Standard;
            Ok(from_address.to_string())
        },
        UtxoAddressFormat::Segwit => {
            let bech32_hrp = &coin.as_ref().conf.bech32_hrp;
            match bech32_hrp {
                Some(hrp) => Ok(SegwitAddress::new(&from_address.hash, hrp.clone()).to_string()),
                None => ERR!("Cannot convert to a segwit address for a coin with no bech32_hrp in config"),
            }
        },
        UtxoAddressFormat::CashAddress { network, .. } => Ok(try_s!(from_address
            .to_cashaddress(
                &network,
                coin.as_ref().conf.pub_addr_prefix,
                coin.as_ref().conf.p2sh_addr_prefix
            )
            .and_then(|cashaddress| cashaddress.encode()))),
    }
}

pub fn validate_address<T: UtxoCommonOps>(coin: &T, address: &str) -> ValidateAddressResult {
    let result = coin.address_from_str(address);
    let address = match result {
        Ok(addr) => addr,
        Err(e) => {
            return ValidateAddressResult {
                is_valid: false,
                reason: Some(e),
            }
        },
    };

    let is_p2pkh = address.prefix == coin.as_ref().conf.pub_addr_prefix
        && address.t_addr_prefix == coin.as_ref().conf.pub_t_addr_prefix;
    let is_p2sh = address.prefix == coin.as_ref().conf.p2sh_addr_prefix
        && address.t_addr_prefix == coin.as_ref().conf.p2sh_t_addr_prefix
        && coin.as_ref().conf.segwit;
    let is_segwit = address.hrp.is_some() && address.hrp == coin.as_ref().conf.bech32_hrp && coin.as_ref().conf.segwit;

    if is_p2pkh || is_p2sh || is_segwit {
        ValidateAddressResult {
            is_valid: true,
            reason: None,
        }
    } else {
        ValidateAddressResult {
            is_valid: false,
            reason: Some(ERRL!("Address {} has invalid prefixes", address)),
        }
    }
}

pub fn required_confirmations(coin: &UtxoCoinFields) -> u64 {
    coin.conf.required_confirmations.load(AtomicOrdering::Relaxed)
}

pub fn requires_notarization(coin: &UtxoCoinFields) -> bool {
    coin.conf.requires_notarization.load(AtomicOrdering::Relaxed)
}

pub fn set_required_confirmations(coin: &UtxoCoinFields, confirmations: u64) {
    coin.conf
        .required_confirmations
        .store(confirmations, AtomicOrdering::Relaxed);
}

pub fn set_requires_notarization(coin: &UtxoCoinFields, requires_nota: bool) {
    coin.conf
        .requires_notarization
        .store(requires_nota, AtomicOrdering::Relaxed);
}

pub fn coin_protocol_info<T: UtxoCommonOps>(coin: &T) -> Vec<u8> {
    rmp_serde::to_vec(coin.addr_format()).expect("Serialization should not fail")
}

pub fn is_coin_protocol_supported<T: UtxoCommonOps>(coin: &T, info: &Option<Vec<u8>>) -> bool {
    match info {
        Some(format) => rmp_serde::from_read_ref::<_, UtxoAddressFormat>(format).is_ok(),
        None => !coin.addr_format().is_segwit(),
    }
}

/// Swap contract address is not used by standard UTXO coins.
pub fn swap_contract_address() -> Option<BytesJson> { None }

/// Convert satoshis to BigDecimal amount of coin units
pub fn big_decimal_from_sat(satoshis: i64, decimals: u8) -> BigDecimal {
    BigDecimal::from(satoshis) / BigDecimal::from(10u64.pow(decimals as u32))
}

pub fn big_decimal_from_sat_unsigned(satoshis: u64, decimals: u8) -> BigDecimal {
    BigDecimal::from(satoshis) / BigDecimal::from(10u64.pow(decimals as u32))
}

pub fn address_from_raw_pubkey(
    pub_key: &[u8],
    prefix: u8,
    t_addr_prefix: u8,
    checksum_type: ChecksumType,
    hrp: Option<String>,
    addr_format: UtxoAddressFormat,
) -> Result<Address, String> {
    Ok(Address {
        t_addr_prefix,
        prefix,
        hash: try_s!(Public::from_slice(pub_key)).address_hash().into(),
        checksum_type,
        hrp,
        addr_format,
    })
}

pub fn address_from_pubkey(
    pub_key: &Public,
    prefix: u8,
    t_addr_prefix: u8,
    checksum_type: ChecksumType,
    hrp: Option<String>,
    addr_format: UtxoAddressFormat,
) -> Address {
    Address {
        t_addr_prefix,
        prefix,
        hash: pub_key.address_hash().into(),
        checksum_type,
        hrp,
        addr_format,
    }
}

pub fn addr_format(coin: &dyn AsRef<UtxoCoinFields>) -> &UtxoAddressFormat {
    match coin.as_ref().derivation_method {
        DerivationMethod::Iguana(ref my_address) => &my_address.addr_format,
        DerivationMethod::HDWallet(UtxoHDWallet { ref address_format, .. }) => address_format,
    }
}

pub fn addr_format_for_standard_scripts(coin: &dyn AsRef<UtxoCoinFields>) -> UtxoAddressFormat {
    match &coin.as_ref().conf.default_address_format {
        UtxoAddressFormat::Segwit => UtxoAddressFormat::Standard,
        format @ (UtxoAddressFormat::Standard | UtxoAddressFormat::CashAddress { .. }) => format.clone(),
    }
}

fn check_withdraw_address_supported<T>(coin: &T, addr: &Address) -> Result<(), MmError<UnsupportedAddr>>
where
    T: UtxoCommonOps,
{
    let conf = &coin.as_ref().conf;

    match addr.addr_format {
        // Considering that legacy is supported with any configured formats
        // This can be changed depending on the coins implementation
        UtxoAddressFormat::Standard => {
            let is_p2pkh = addr.prefix == conf.pub_addr_prefix && addr.t_addr_prefix == conf.pub_t_addr_prefix;
            let is_p2sh =
                addr.prefix == conf.p2sh_addr_prefix && addr.t_addr_prefix == conf.p2sh_t_addr_prefix && conf.segwit;
            if !is_p2pkh && !is_p2sh {
                MmError::err(UnsupportedAddr::PrefixError(conf.ticker.clone()))
            } else {
                Ok(())
            }
        },
        UtxoAddressFormat::Segwit => {
            if !conf.segwit {
                return MmError::err(UnsupportedAddr::SegwitNotActivated(conf.ticker.clone()));
            }

            if addr.hrp != conf.bech32_hrp {
                MmError::err(UnsupportedAddr::HrpError {
                    ticker: conf.ticker.clone(),
                    hrp: addr.hrp.clone().unwrap_or_default(),
                })
            } else {
                Ok(())
            }
        },
        UtxoAddressFormat::CashAddress { .. } => {
            if addr.addr_format == conf.default_address_format || addr.addr_format == *coin.addr_format() {
                Ok(())
            } else {
                MmError::err(UnsupportedAddr::FormatMismatch {
                    ticker: conf.ticker.clone(),
                    activated_format: coin.addr_format().to_string(),
                    used_format: addr.addr_format.to_string(),
                })
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_btc_min_trading_vol_dynamic_is_dust_based() {
        let min_sat = non_btc_min_trading_vol_sat(1000, &TxFee::Dynamic(EstimateFeeMethod::Standard));
        assert_eq!(min_sat, 10_000);
    }

    #[test]
    fn non_btc_min_trading_vol_fixed_uses_max_of_dust_and_fee_based() {
        // fee_per_kb=1000 -> ceil(1000*496/1000)=496, then *10=4960 < dust-based 10000
        let min_sat = non_btc_min_trading_vol_sat(1000, &TxFee::FixedPerKb(1000));
        assert_eq!(min_sat, 10_000);

        // fee_per_kb=5000 -> ceil(5000*496/1000)=2480, then *10=24800 > dust-based 10000
        let min_sat = non_btc_min_trading_vol_sat(1000, &TxFee::FixedPerKb(5000));
        assert_eq!(min_sat, 24_800);
    }
}
