//! # Purpose
//! `MarketCoinOps` impl for `SolanaCoin`. All RPC calls are async.

use super::*;

impl MarketCoinOps for SolanaCoin {
    fn ticker(&self) -> &str { &self.ticker }

    fn my_address(&self) -> Result<String, String> { Ok(self.my_address.clone()) }

    /// The base58 ed25519 public key of the platform key, which is the address (`get_private_keys` reads it).
    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> { Ok(self.my_address.clone()) }

    /// Solana signs the message bytes as they are, with no prefix and no pre-hash (`sign_message`): there is no hash.
    fn sign_message_hash(&self, _message: &str) -> Option<[u8; 32]> { None }

    fn sign_message(&self, message: &str) -> SignatureResult<String> { solana_common::sign_message(self, message) }

    fn verify_message(&self, signature: &str, message: &str, pubkey_bs58: &str) -> VerificationResult<bool> {
        solana_common::verify_message(self, signature, message, pubkey_bs58)
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let decimals = self.decimals as u64;
        let fut = self.my_balance_impl().and_then(move |result| {
            Ok(CoinBalance {
                spendable: result.with_prec(decimals),
                unspendable: 0.into(),
            })
        });
        Box::new(fut)
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> {
        let decimals = self.decimals as u64;
        let fut = self
            .my_balance_impl()
            .and_then(move |result| Ok(result.with_prec(decimals)));
        Box::new(fut)
    }

    fn platform_ticker(&self) -> &str { self.ticker() }

    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let coin = self.clone();
        let tx = tx.to_owned();
        let fut = async move {
            let bytes = hex::decode(tx).map_err(|e| format!("{:?}", e))?;
            let tx: Transaction = deserialize(bytes.as_slice()).map_err(|e| format!("{:?}", e))?;
            let signature = coin.rpc().send_transaction(&tx).await.map_err(|e| format!("{:?}", e))?;
            Ok(signature.to_string())
        };
        Box::new(fut.boxed().compat())
    }

    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let coin = self.clone();
        let tx = tx.to_owned();
        let fut = async move {
            let tx: Transaction = try_s!(deserialize(tx.as_slice()));
            let signature = coin.rpc().send_transaction(&tx).await.map_err(|e| format!("{:?}", e))?;
            Ok(signature.to_string())
        };
        Box::new(fut.boxed().compat())
    }

    fn wait_for_confirmations(
        &self,
        _tx: &[u8],
        _confirmations: u64,
        _requires_nota: bool,
        _wait_until: u64,
        _check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.ticker)))
    }

    fn wait_for_tx_spend(
        &self,
        _transaction: &[u8],
        _wait_until: u64,
        _from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.ticker,
        ))))
    }

    fn tx_enum_from_bytes(&self, _bytes: &[u8]) -> Result<TransactionEnum, String> {
        Err(format!("Transaction decoding is not supported for {}", self.ticker))
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> {
        let coin = self.clone();
        let fut = async move { coin.rpc().get_block_height().await.map_err(|e| format!("{:?}", e)) };
        Box::new(fut.boxed().compat())
    }

    /// The 64-byte keypair (32-byte secret, then the 32-byte public key) in base58 (D40.6B.1 option (b)).
    fn display_priv_key(&self) -> Result<String, String> { Ok(solana_common::keypair_base58(self)) }

    /// One lamport (CRD ch.40 D40.6B.1, maintainer decision).
    fn min_tx_amount(&self) -> BigDecimal { solana_common::smallest_unit(SOLANA_DEFAULT_DECIMALS as u8) }

    fn min_trading_vol(&self) -> MmNumber { MmNumber::from("0.00777") }
}
