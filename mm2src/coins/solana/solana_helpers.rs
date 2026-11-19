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

/// The block height and the balances an activation reports.
pub struct ActivationBalances {
    pub current_block: u64,
    pub balance: CoinBalance,
    pub token_balances: HashMap<String, CoinBalance>,
}

/// What the reads of one activation found, all from one node (CRD ch.40 R40.1.5).
pub(crate) struct ActivationReads {
    pub(crate) current_block: u64,
    pub(crate) lamports: u64,
    /// The token account of each token as the node answered, `None` when the wallet has no such account.
    pub(crate) token_accounts: Vec<(String, Option<crate::solana::rpc_client::TokenAmount>)>,
}

/// The balance of a token from the node's answer for its account; no account is balance zero (CRD ch.40 D40.6B.2).
pub(crate) fn token_balance_from_answer(
    answer: Option<crate::solana::rpc_client::TokenAmount>,
) -> Result<SplTokenBalance, MmError<BalanceError>> {
    let Some(balance) = answer else {
        return Ok(SplTokenBalance {
            raw: 0,
            amount: 0.into(),
        });
    };
    let raw = balance
        .amount
        .parse::<u64>()
        .map_to_mm(|_| BalanceError::InvalidResponse("The token account amount is not a number".to_string()))?;
    let amount =
        BigDecimal::from_str(&balance.ui_amount_string).map_to_mm(|e| BalanceError::Internal(e.to_string()))?;
    Ok(SplTokenBalance { raw, amount })
}

/// The reads of a token account on the node `node`: `None` when the account does not exist.
pub(crate) async fn read_token_account(
    node: &SolanaRpcClient,
    account: &Pubkey,
) -> Result<Option<crate::solana::rpc_client::TokenAmount>, RpcError> {
    if !node.get_account_exists(account).await? {
        return Ok(None);
    }
    node.get_token_account_balance(account).await.map(Some)
}

impl SolanaCoin {
    pub async fn estimate_withdraw_fees(&self) -> Result<(solana_hash::Hash, u64), MmError<RpcError>> {
        let coin = self.clone();
        let result = self
            .rpc()
            .run(move |node| {
                let coin = coin.clone();
                async move {
                    let hash = node.get_latest_blockhash().await?;
                    let to = coin.key_pair.pubkey();
                    let tx = solana_system_transaction::transfer(&coin.key_pair, &to, LAMPORTS_DUMMY_AMOUNT, hash);
                    let fees = node.get_fee_for_message(tx.message()).await?;
                    Ok((hash, fees))
                }
            })
            .await?;
        Ok(result)
    }

    /// The reads of a SOL withdraw, all from one node: the latest blockhash, the fee quote for a transfer and the
    /// balance of the wallet.
    pub(crate) async fn withdraw_reads(&self) -> Result<(solana_hash::Hash, u64, u64), RpcError> {
        let coin = self.clone();
        self.rpc()
            .run(move |node| {
                let coin = coin.clone();
                async move {
                    let hash = node.get_latest_blockhash().await?;
                    let to = coin.key_pair.pubkey();
                    let tx = solana_system_transaction::transfer(&coin.key_pair, &to, LAMPORTS_DUMMY_AMOUNT, hash);
                    let fee = node.get_fee_for_message(tx.message()).await?;
                    let balance = node.get_balance(&coin.key_pair.pubkey()).await?;
                    Ok((hash, fee, balance))
                }
            })
            .await
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
    /// is. Both reads go to one node.
    pub(crate) async fn spl_token_balance(
        &self,
        infos: &SplTokenInfo,
    ) -> Result<SplTokenBalance, MmError<BalanceError>> {
        let account = self.token_account_address(infos);
        let answer = self
            .rpc()
            .run(move |node| async move { read_token_account(&node, &account).await })
            .await?;
        token_balance_from_answer(answer)
    }

    pub async fn my_balance_spl(&self, infos: &SplTokenInfo) -> Result<CoinBalance, MmError<BalanceError>> {
        let balance = self.spl_token_balance(infos).await?;
        Ok(CoinBalance {
            spendable: balance.amount,
            unspendable: Default::default(),
        })
    }

    /// The reads of an activation, all from one node: the block height, the SOL balance and the token account of each
    /// of `tokens` (CRD ch.40 R40.1.5).
    async fn activation_reads(&self, tokens: &[(String, SplTokenInfo)]) -> Result<ActivationReads, RpcError> {
        let owner = self.key_pair.pubkey();
        let accounts: Vec<(String, Pubkey)> = tokens
            .iter()
            .map(|(ticker, infos)| (ticker.clone(), self.token_account_address(infos)))
            .collect();
        self.rpc()
            .run(move |node| {
                let accounts = accounts.clone();
                async move {
                    let current_block = node.get_block_height().await?;
                    let lamports = node.get_balance(&owner).await?;
                    let mut token_accounts = Vec::with_capacity(accounts.len());
                    for (ticker, account) in accounts {
                        token_accounts.push((ticker, read_token_account(&node, &account).await?));
                    }
                    Ok(ActivationReads {
                        current_block,
                        lamports,
                        token_accounts,
                    })
                }
            })
            .await
    }

    /// What an activation reports: the block height, the SOL balance and the balance of every token recorded on the
    /// platform, all read from one node (CRD ch.40 R40.1.5). A failure is the text of the first failed read.
    pub async fn activation_balances(&self) -> Result<ActivationBalances, String> {
        let tokens: Vec<(String, SplTokenInfo)> = self.get_spl_tokens_infos().into_iter().collect();
        let reads = self.activation_reads(&tokens).await.map_err(|e| e.to_string())?;
        let balance = CoinBalance {
            spendable: lamports_to_sol(reads.lamports).with_prec(u64::from(self.decimals)),
            unspendable: 0.into(),
        };
        let mut token_balances = HashMap::new();
        for (ticker, answer) in reads.token_accounts {
            let amount = token_balance_from_answer(answer)
                .map_err(|e| e.into_inner().to_string())?
                .amount;
            token_balances.insert(ticker, CoinBalance {
                spendable: amount,
                unspendable: Default::default(),
            });
        }
        Ok(ActivationBalances {
            current_block: reads.current_block,
            balance,
            token_balances,
        })
    }

    /// The mint-owner check of activation (CRD ch.40 R40.2.2): for each `(mint, program_id)` of `tokens`, when the
    /// mint account exists, the token's `program_id` must own it. A mint account that does not exist is accepted. The
    /// reads go to one node.
    pub async fn check_mint_owners(&self, tokens: &[(Pubkey, Pubkey)]) -> Result<(), MmError<MintOwnerError>> {
        if tokens.is_empty() {
            // No token, no node contact: an empty list of tokens is not a read.
            return Ok(());
        }
        let mints: Vec<Pubkey> = tokens.iter().map(|(mint, _)| *mint).collect();
        let owners = self
            .rpc()
            .run(move |node| {
                let mints = mints.clone();
                async move {
                    let mut owners = Vec::with_capacity(mints.len());
                    for mint in mints {
                        owners.push(node.get_account_owner(&mint).await?);
                    }
                    Ok(owners)
                }
            })
            .await
            .map_to_mm(|e| MintOwnerError::Node(crate::node_url::scrub_urls_in_text(&e.to_string())))?;
        for ((_, program_id), owner) in tokens.iter().zip(owners) {
            if matches!(owner, Some(owner) if owner != *program_id) {
                return MmError::err(MintOwnerError::WrongOwner);
            }
        }
        Ok(())
    }

    /// The mint-owner check of one token; see [`Self::check_mint_owners`].
    pub async fn check_mint_owner(&self, mint: &Pubkey, program_id: &Pubkey) -> Result<(), MmError<MintOwnerError>> {
        self.check_mint_owners(&[(*mint, *program_id)]).await
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
