//! # Purpose
//! `SolanaCommonOps` impl plus inherent helpers on `SolanaCoin`.
//!
//! All blockchain RPC calls are async — the legacy `async_blocking`
//! wrappers around the synchronous `solana-client` 1.x are gone.

use super::*;

#[async_trait]
impl SolanaCommonOps for SolanaCoin {
    fn rpc(&self) -> &SolanaRpcPool { &self.client }

    fn is_token(&self) -> bool { false }

    async fn check_balance_and_prepare_transfer(
        &self,
        max: bool,
        amount: BigDecimal,
        fees: u64,
    ) -> Result<PrepareTransferData, MmError<SufficientBalanceError>> {
        solana_common::check_balance_and_prepare_transfer(self, max, amount, fees).await
    }
}

impl SolanaCoin {
    pub async fn estimate_withdraw_fees(&self) -> Result<(solana_hash::Hash, u64), MmError<RpcError>> {
        let hash = self.rpc().get_latest_blockhash().await?;
        let to = self.key_pair.pubkey();
        let tx = solana_system_transaction::transfer(&self.key_pair, &to, LAMPORTS_DUMMY_AMOUNT, hash);
        let fees = self.rpc().get_fee_for_message(tx.message()).await?;
        Ok((hash, fees))
    }

    pub async fn my_balance_spl(&self, infos: &SplTokenInfo) -> Result<CoinBalance, MmError<BalanceError>> {
        let token_accounts = self
            .rpc()
            .get_token_accounts_by_owner(
                &self.key_pair.pubkey(),
                TokenAccountsFilter::Mint(infos.token_contract_address),
            )
            .await?;
        if token_accounts.is_empty() {
            return Ok(CoinBalance {
                spendable: Default::default(),
                unspendable: Default::default(),
                ..Default::default()
            });
        }
        let actual_token_pubkey =
            Pubkey::from_str(&token_accounts[0].pubkey).map_err(|e| BalanceError::Internal(format!("{:?}", e)))?;
        let amount = self.rpc().get_token_account_balance(&actual_token_pubkey).await?;
        let balance =
            BigDecimal::from_str(&amount.ui_amount_string).map_to_mm(|e| BalanceError::Internal(e.to_string()))?;
        Ok(CoinBalance {
            spendable: balance,
            unspendable: Default::default(),
            ..Default::default()
        })
    }

    pub(crate) fn my_balance_impl(&self) -> BalanceFut<BigDecimal> {
        let coin = self.clone();
        let fut = async move {
            let res = coin.rpc().get_balance(&coin.key_pair.pubkey()).await?;
            Ok(lamports_to_sol(res))
        };
        Box::new(fut.boxed().compat())
    }

    pub fn add_spl_token_info(&self, ticker: String, info: SplTokenInfo) {
        self.spl_tokens_infos.lock().unwrap().insert(ticker, info);
    }

    pub fn get_spl_tokens_infos(&self) -> HashMap<String, SplTokenInfo> {
        let guard = self.spl_tokens_infos.lock().unwrap();
        (*guard).clone()
    }
}
