// lightning_market_ops — MarketCoinOps trait impl for LightningCoin
use super::*;

impl MarketCoinOps for LightningCoin {
    fn ticker(&self) -> &str { &self.conf.ticker }

    fn my_address(&self) -> Result<String, String> { Ok(self.my_node_id()) }

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> { unimplemented!() }

    fn sign_message_hash(&self, message: &str) -> Option<[u8; 32]> {
        let mut _message_prefix = self.conf.sign_message_prefix.clone()?;
        let prefixed_message = format!("{}{}", _message_prefix, message);
        Some(dhash256(prefixed_message.as_bytes()).take())
    }

    fn sign_message(&self, message: &str) -> SignatureResult<String> {
        let message_hash = self.sign_message_hash(message).ok_or(SignatureError::PrefixNotFound)?;
        let secret_key = self
            .keys_manager
            .get_node_secret(Recipient::Node)
            .map_err(|_| SignatureError::InternalError("Error accessing node keys".to_string()))?;
        let private = Private {
            prefix: 239,
            secret: H256::from(*secret_key.as_ref()),
            compressed: true,
            checksum_type: ChecksumType::DSHA256,
        };
        let signature = private.sign_compact(&H256::from(message_hash))?;
        Ok(zbase32::encode_full_bytes(&*signature))
    }

    fn verify_message(&self, signature: &str, message: &str, pubkey: &str) -> VerificationResult<bool> {
        let message_hash = self
            .sign_message_hash(message)
            .ok_or(VerificationError::PrefixNotFound)?;
        let signature = CompactSignature::from(
            zbase32::decode_full_bytes_str(signature)
                .map_err(|e| VerificationError::SignatureDecodingError(e.to_string()))?,
        );
        let recovered_pubkey = Public::recover_compact(&H256::from(message_hash), &signature)?;
        Ok(recovered_pubkey.to_string() == pubkey)
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let decimals = self.decimals();
        let (spendable_msat, unspendable_msat) = self.get_balance_msat();
        let my_balance = CoinBalance {
            spendable: big_decimal_from_sat_unsigned(spendable_msat, decimals),
            unspendable: big_decimal_from_sat_unsigned(unspendable_msat, decimals),
        };
        Box::new(futures01::future::ok(my_balance))
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> {
        Box::new(self.platform_coin().my_balance().map(|res| res.spendable))
    }

    fn platform_ticker(&self) -> &str { self.platform_coin().ticker() }

    fn send_raw_tx(&self, _tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        Box::new(futures01::future::err(
            MmError::new(
                "send_raw_tx is not supported for lightning, please use send_payment method instead.".to_string(),
            )
            .to_string(),
        ))
    }

    fn send_raw_tx_bytes(&self, _tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        Box::new(futures01::future::err(
            MmError::new(
                "send_raw_tx is not supported for lightning, please use send_payment method instead.".to_string(),
            )
            .to_string(),
        ))
    }

    // Todo: Implement this when implementing swaps for lightning as it's is used mainly for swaps
    fn wait_for_confirmations(
        &self,
        _tx: &[u8],
        _confirmations: u64,
        _requires_nota: bool,
        _wait_until: u64,
        _check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        unimplemented!()
    }

    // Todo: Implement this when implementing swaps for lightning as it's is used mainly for swaps
    fn wait_for_tx_spend(
        &self,
        _transaction: &[u8],
        _wait_until: u64,
        _from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        unimplemented!()
    }

    // Todo: Implement this when implementing swaps for lightning as it's is used mainly for swaps
    fn tx_enum_from_bytes(&self, _bytes: &[u8]) -> Result<TransactionEnum, String> { unimplemented!() }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> { Box::new(futures01::future::ok(0)) }

    fn display_priv_key(&self) -> Result<String, String> {
        Ok(self
            .keys_manager
            .get_node_secret(Recipient::Node)
            .map_err(|_| "Unsupported recipient".to_string())?
            .to_string())
    }

    fn min_tx_amount(&self) -> BigDecimal {
        let htlc_minimums = self
            .channel_manager
            .list_channels()
            .into_iter()
            .map(|chan| chan.outbound_htlc_minimum_msat);
        big_decimal_from_sat_unsigned(min_outbound_htlc_msat(htlc_minimums), self.decimals())
    }

    fn min_trading_vol(&self) -> MmNumber { self.min_tx_amount().into() }
}

/// The largest counterparty minimum outbound HTLC amount over the node's channels in millisatoshi.
/// A channel that states none counts as one millisatoshi, and so does a node with no channels
/// (CRD ch.41 R41.11A.1).
fn min_outbound_htlc_msat<I: IntoIterator<Item = Option<u64>>>(channel_minimums: I) -> u64 {
    channel_minimums
        .into_iter()
        .map(|min| min.unwrap_or(1))
        .max()
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_outbound_htlc_no_channels_is_one_msat() {
        assert_eq!(min_outbound_htlc_msat(Vec::new()), 1);
        // One millisatoshi in the coin's own decimals (11 for a millisatoshi-denominated coin).
        assert_eq!(
            big_decimal_from_sat_unsigned(min_outbound_htlc_msat(Vec::new()), 11),
            BigDecimal::from_str("0.00000000001").unwrap()
        );
    }

    #[test]
    fn min_outbound_htlc_is_maximum_over_channels() {
        assert_eq!(
            min_outbound_htlc_msat(vec![Some(1000), Some(250_000), None, Some(5)]),
            250_000
        );
        // A channel that states none counts as one millisatoshi.
        assert_eq!(min_outbound_htlc_msat(vec![None, None]), 1);
    }
}
