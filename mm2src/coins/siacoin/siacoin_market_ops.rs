// siacoin_market_ops — MarketCoinOps trait implementation.

use super::*;

impl MarketCoinOps for SiaCoin {
    fn ticker(&self) -> &str { &self.conf.ticker }

    fn my_address(&self) -> Result<String, String> {
        let key_pair = match &*self.priv_key_policy {
            PrivKeyPolicy::KeyPair(key_pair) => key_pair,
            _ => return Err("SiaCoin::my_address: Unexpected Key Derivation Method.".to_string()),
        };
        let address = key_pair.public().address();
        Ok(address.to_string())
    }

    fn get_public_key(&self) -> Result<String, MmError<super::UnexpectedDerivationMethod>> {
        let public_key = match &*self.priv_key_policy {
            PrivKeyPolicy::KeyPair(key_pair) => key_pair.public(),
            _ => return MmError::err(super::UnexpectedDerivationMethod::IguanaPrivKeyUnavailable),
        };
        Ok(public_key.to_string())
    }

    fn sign_message_hash(&self, _message: &str) -> Option<[u8; 32]> { None }

    fn sign_message(&self, _message: &str) -> SignatureResult<String> {
        MmError::err(SignatureError::InternalError(
            "SiaCoin::sign_message: Unsupported".to_string(),
        ))
    }

    fn verify_message(&self, _signature: &str, _message: &str, _address: &str) -> VerificationResult<bool> {
        MmError::err(VerificationError::InternalError(
            "SiaCoin::verify_message: Unsupported".to_string(),
        ))
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let coin = self.clone();
        let fut = async move {
            let my_address = match &*coin.priv_key_policy {
                PrivKeyPolicy::KeyPair(key_pair) => key_pair.public().address(),
                _ => {
                    return MmError::err(BalanceError::UnexpectedDerivationMethod(
                        super::UnexpectedDerivationMethod::IguanaPrivKeyUnavailable,
                    ))
                },
            };
            let balance = coin
                .client
                .address_balance(my_address)
                .await
                .map_to_mm(|e| BalanceError::Transport(e.to_string()))?;
            Ok(CoinBalance {
                spendable: hastings_to_siacoin(balance.siacoins),
                unspendable: hastings_to_siacoin(balance.immature_siacoins),
                ..Default::default()
            })
        };
        Box::new(fut.boxed().compat())
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> { Box::new(self.my_balance().map(|res| res.spendable)) }

    fn platform_ticker(&self) -> &str { self.ticker() }

    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let client = self.client.clone();
        let tx = tx.to_owned();

        let fut = async move {
            let tx: Json = serde_json::from_str(&tx).map_err(|e| e.to_string())?;
            let transaction = serde_json::from_str::<V2Transaction>(&tx.to_string()).map_err(|e| e.to_string())?;
            let txid = transaction.txid().to_string();

            client
                .broadcast_transaction(&transaction)
                .await
                .map_err(|e| e.to_string())?;
            Ok(txid)
        };
        Box::new(fut.boxed().compat())
    }

    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let tx: V2Transaction = try_fus!(serde_json::from_slice(tx).map_err(|e| e.to_string()));
        let str_tx = try_fus!(serde_json::to_string(&tx).map_err(|e| e.to_string()));
        self.send_raw_tx(&str_tx)
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        _requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let tx: SiaTransaction = try_fus!(serde_json::from_slice(tx)
            .map_err(|e| format!("siacoin wait_for_confirmations payment_tx deser failed: {}", e)));
        let txid = tx.txid();
        let client = self.client.clone();
        let tx_request = GetEventRequest { txid: txid.clone() };

        let fut = async move {
            loop {
                if now_ms() / 1000 > wait_until {
                    return ERR!(
                        "Waited too long until {} for payment {} to be received",
                        wait_until,
                        tx.txid()
                    );
                }

                match client.dispatcher(tx_request.clone()).await {
                    Ok(event) => {
                        if event.confirmations >= confirmations {
                            return Ok(());
                        }
                    },
                    Err(e) => info!("Waiting for confirmation of Sia txid {}: {}", txid, e),
                }

                Timer::sleep(check_every as f64).await;
            }
        };

        Box::new(fut.boxed().compat())
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        _from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> super::TransactionFut {
        let tx_bytes = transaction.to_vec();
        let client = self.client.clone();

        let fut = async move {
            let tx = SiaTransaction::try_from(tx_bytes).map_err(|e| TransactionErr::Plain(e.to_string()))?;
            let htlc_lock_txid = tx.txid();
            let output_id = SiacoinOutputId::new(htlc_lock_txid.clone(), HTLC_VOUT_INDEX);
            let check_every = 10f64;

            loop {
                let found_in_mempool = client
                    .dispatcher(TxpoolTransactionsRequest)
                    .await
                    .unwrap_or_default()
                    .v2transactions
                    .into_iter()
                    .find(|tx| tx.siacoin_inputs.iter().any(|input| input.parent.id == output_id));

                if let Some(tx) = found_in_mempool {
                    return Ok(TransactionEnum::SiaTransaction(SiaTransaction(tx)));
                }

                let found_in_block = client.find_where_utxo_spent(&output_id).await;

                match found_in_block {
                    Ok(Some(tx)) => return Ok(TransactionEnum::SiaTransaction(SiaTransaction(tx))),
                    Err(e) => debug!("SiaCoin::wait_for_tx_spend: find_where_utxo_spent failed: {}", e),
                    _ => (),
                }

                if now_ms() / 1000 >= wait_until {
                    return Err(TransactionErr::Plain(format!(
                        "Timed out waiting for spend of txid:{} vout 0",
                        htlc_lock_txid
                    )));
                }

                Timer::sleep(check_every).await;
            }
        };

        Box::new(fut.boxed().compat())
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        let tx: V2Transaction = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        Ok(TransactionEnum::SiaTransaction(SiaTransaction(tx)))
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> {
        let client = self.client.clone();
        let height_fut = async move { client.current_height().await.map_err(|e| e.to_string()) }
            .boxed()
            .compat();
        Box::new(height_fut)
    }

    fn display_priv_key(&self) -> Result<String, String> { Err("SiaCoin::display_priv_key: Unsupported".to_string()) }

    fn min_tx_amount(&self) -> BigDecimal { hastings_to_siacoin(1u64.into()) }

    fn min_trading_vol(&self) -> MmNumber { hastings_to_siacoin(1u64.into()).into() }
}
