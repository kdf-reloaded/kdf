// qrc20_market_ops — MarketCoinOps trait implementation.

use super::*;

impl MarketCoinOps for Qrc20Coin {
    fn ticker(&self) -> &str { &self.utxo.conf.ticker }

    fn my_address(&self) -> Result<String, String> { utxo_common::my_address(self) }

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> { unimplemented!() }

    fn sign_message_hash(&self, message: &str) -> Option<[u8; 32]> {
        utxo_common::sign_message_hash(self.as_ref(), message)
    }

    fn sign_message(&self, message: &str) -> SignatureResult<String> {
        utxo_common::sign_message(self.as_ref(), message)
    }

    fn verify_message(&self, signature_base64: &str, message: &str, address: &str) -> VerificationResult<bool> {
        utxo_common::verify_message(self, signature_base64, message, address)
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let decimals = self.utxo.decimals;

        let coin = self.clone();
        let fut = async move {
            let my_address = coin
                .my_addr_as_contract_addr()
                .mm_err(|e| BalanceError::Internal(e.to_string()))?;
            let params = [Token::Address(my_address)];
            let contract_address = coin.contract_address;
            let tokens = coin
                .utxo
                .rpc_client
                .rpc_contract_call(ViewContractCallType::BalanceOf, &contract_address, &params)
                .compat()
                .await
                .mm_err(Into::into)?;
            let spendable = match tokens.first() {
                Some(Token::Uint(bal)) => u256_to_big_decimal(*bal, decimals).mm_err(Into::into)?,
                _ => {
                    let error = format!("Expected U256 as balanceOf result but got {:?}", tokens);
                    return MmError::err(BalanceError::InvalidResponse(error));
                },
            };
            Ok(CoinBalance {
                spendable,
                unspendable: BigDecimal::from(0),
                ..Default::default()
            })
        };
        Box::new(fut.boxed().compat())
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> {
        // use standard UTXO my_balance implementation that returns Qtum balance instead of QRC20
        Box::new(utxo_common::my_balance(self.clone()).map(|CoinBalance { spendable, .. }| spendable))
    }

    fn platform_ticker(&self) -> &str { &self.0.platform }

    #[inline(always)]
    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        utxo_common::send_raw_tx(&self.utxo, tx)
    }

    #[inline(always)]
    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        utxo_common::send_raw_tx_bytes(&self.utxo, tx)
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let tx: UtxoTx = try_fus!(deserialize(tx).map_err(|e| ERRL!("{:?}", e)));
        let selfi = self.clone();
        let fut = async move {
            selfi
                .wait_for_confirmations_and_check_result(tx, confirmations, requires_nota, wait_until, check_every)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let tx: UtxoTx = try_tx_fus!(deserialize(transaction).map_err(|e| ERRL!("{:?}", e)));

        let selfi = self.clone();
        let fut = async move {
            selfi
                .wait_for_tx_spend_impl(tx, wait_until, from_block)
                .map_err(TransactionErr::Plain)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        utxo_common::tx_enum_from_bytes(self.as_ref(), bytes)
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> {
        utxo_common::current_block(&self.utxo)
    }

    fn display_priv_key(&self) -> Result<String, String> { utxo_common::display_priv_key(&self.utxo) }

    fn min_tx_amount(&self) -> BigDecimal { BigDecimal::from(0) }

    fn min_trading_vol(&self) -> MmNumber {
        let pow = self.utxo.decimals / 3;
        MmNumber::from(1) / MmNumber::from(10u64.pow(pow as u32))
    }

    fn sign_raw_tx(&self, args: &SignRawTransactionRequest) -> RawTransactionFut {
        Box::new(utxo_common::sign_raw_tx(self.clone(), args.clone()).boxed().compat())
    }
}
