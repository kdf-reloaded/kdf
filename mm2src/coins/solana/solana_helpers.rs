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
}

/// The balance of a token account in both forms: the raw amount in smallest units (what a withdraw with `max`
/// moves) and the amount in token units.
#[derive(Clone, Debug)]
pub(crate) struct SplTokenBalance {
    pub(crate) raw: u64,
    pub(crate) amount: BigDecimal,
}

impl SolanaCoin {
    pub async fn estimate_withdraw_fees(&self) -> Result<(solana_hash::Hash, u64), MmError<RpcError>> {
        let hash = self.rpc().get_latest_blockhash().await?;
        let to = self.key_pair.pubkey();
        let tx = solana_system_transaction::transfer(&self.key_pair, &to, LAMPORTS_DUMMY_AMOUNT, hash);
        let fees = self.rpc().get_fee_for_message(tx.message()).await?;
        Ok((hash, fees))
    }

    /// The wallet's associated token account for the token `infos`, derived with the token's `program_id` (CRD ch.40
    /// D40.6B.2). The account a balance is read from and a withdraw spends from.
    pub fn token_account_address(&self, infos: &SplTokenInfo) -> Pubkey {
        crate::solana::spl::associated_token_account(
            &self.key_pair.pubkey(),
            &infos.token_contract_address,
            &infos.program_id,
        )
    }

    /// The wallet's balance of a token, from the associated token account derived with the token's `program_id`. A
    /// wallet without such an account has balance zero, which is not an error (CRD ch.40 D40.6B.2); a node failure
    /// is.
    pub(crate) async fn spl_token_balance(
        &self,
        infos: &SplTokenInfo,
    ) -> Result<SplTokenBalance, MmError<BalanceError>> {
        let account = self.token_account_address(infos);
        if !self.rpc().get_account_exists(&account).await? {
            return Ok(SplTokenBalance {
                raw: 0,
                amount: 0.into(),
            });
        }
        let balance = self.rpc().get_token_account_balance(&account).await?;
        let raw = balance
            .amount
            .parse::<u64>()
            .map_to_mm(|_| BalanceError::InvalidResponse("The token account amount is not a number".to_string()))?;
        let amount =
            BigDecimal::from_str(&balance.ui_amount_string).map_to_mm(|e| BalanceError::Internal(e.to_string()))?;
        Ok(SplTokenBalance { raw, amount })
    }

    pub async fn my_balance_spl(&self, infos: &SplTokenInfo) -> Result<CoinBalance, MmError<BalanceError>> {
        let balance = self.spl_token_balance(infos).await?;
        Ok(CoinBalance {
            spendable: balance.amount,
            unspendable: Default::default(),
        })
    }

    /// The mint-owner check of activation (CRD ch.40 R40.2.2): when the mint account exists, the token's
    /// `program_id` must own it. A mint account that does not exist is accepted.
    pub async fn check_mint_owner(&self, mint: &Pubkey, program_id: &Pubkey) -> Result<(), MmError<MintOwnerError>> {
        let owner = self
            .rpc()
            .get_account_owner(mint)
            .await
            .map_to_mm(|e| MintOwnerError::Node(crate::node_url::scrub_urls_in_text(&e.to_string())))?;
        match owner {
            Some(owner) if owner != *program_id => MmError::err(MintOwnerError::WrongOwner),
            _ => Ok(()),
        }
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

    /// Forgets the token recorded by [`Self::add_spl_token_info`]; a ticker that is not recorded is ignored.
    pub fn remove_spl_token_info(&self, ticker: &str) { self.spl_tokens_infos.lock().unwrap().remove(ticker); }

    pub fn get_spl_tokens_infos(&self) -> HashMap<String, SplTokenInfo> {
        let guard = self.spl_tokens_infos.lock().unwrap();
        (*guard).clone()
    }
}
