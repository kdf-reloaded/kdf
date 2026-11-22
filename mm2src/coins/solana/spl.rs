use super::{CoinBalance, HistorySyncState, MarketCoinOps, MmCoin, SwapOps, TradeFee, TransactionEnum, WatcherOps};
use crate::solana::rpc_pool::SolanaRpcPool;
use crate::solana::solana_common::{amount_to_units, parse_recipient, truncate_amount, units_to_amount,
                                   with_at_least_decimals};
use crate::solana::{solana_common, swaps_unsupported, SolanaCoinSettings, SolanaCommonOps, SolanaFeeDetails};
use crate::{BalanceFut, DexFee, FeeApproxStage, FoundSwapTxSpend, NegotiateSwapContractAddrErr, RawTransactionError,
            RawTransactionFut, RawTransactionRequest, SignatureResult, SolanaCoin, TradePreimageError,
            TradePreimageFut, TradePreimageResult, TradePreimageValue, TransactionDetails, TransactionErr,
            TransactionFut, TransactionType, UnexpectedDerivationMethod, ValidateAddressResult, ValidateFeeArgs,
            ValidatePaymentInput, VerificationResult, WithdrawError, WithdrawFut, WithdrawRequest, WithdrawResult};
use async_trait::async_trait;
use bigdecimal::BigDecimal;
use bincode::serialize;
use common::{mm_number::MmNumber, now_ms};
use futures::{FutureExt, TryFutureExt};
use futures01::Future;
use kdf_crypto::sha256;
use keys::KeyPair;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc::v1::types::Bytes as BytesJson;
use serde_json::Value as Json;
use solana_hash::Hash;
use solana_message::Message;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use std::{fmt::{Debug, Formatter, Result as FmtResult},
          sync::Arc};

/// The SPL Token program id, the token program of a `SPLTOKEN` configuration.
pub const SPL_TOKEN_PROGRAM_ID: Pubkey = spl_token::ID;

/// The Token-2022 program id (`TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`). Token-2022 tokens are not supported
/// yet (CRD ch.40 D40.6B.4); the id is needed to tell a token account of either program from a wallet.
pub const TOKEN_2022_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    6, 221, 246, 225, 238, 117, 143, 222, 24, 66, 93, 188, 228, 108, 205, 218, 182, 26, 252, 77, 131, 185, 13, 39, 254,
    189, 249, 40, 216, 161, 139, 252,
]);

/// The text of the activation refusal of a Token-2022 token, after the token ticker and a colon (CRD ch.40 D40.6B.4,
/// the maintainer's decision of 2026-11-18). Both activation surfaces use it.
pub const TOKEN_2022_NOT_SUPPORTED: &str = "Token-2022 tokens are not supported yet";

/// The size in bytes of a classic token account: what the rent of a created recipient account is quoted for (CRD
/// ch.40 R40.6B.10).
pub const SPL_TOKEN_ACCOUNT_LEN: usize = 165;

/// The associated token account of `wallet` for `mint` under the token program `program_id`: the account a balance
/// is read from, a withdraw spends from and a withdraw pays into (CRD ch.40 D40.6B.2). The one place the address is
/// derived.
pub fn associated_token_account(wallet: &Pubkey, mint: &Pubkey, program_id: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(wallet, mint, program_id)
}

/// What the message of a token withdraw is built from.
pub struct TokenTransferParams<'a> {
    /// The wallet: authority of the source account and payer of the fee and of the created account.
    pub wallet: &'a Pubkey,
    /// The recipient's wallet address; the destination is its associated token account.
    pub recipient: &'a Pubkey,
    pub mint: &'a Pubkey,
    pub program_id: &'a Pubkey,
    pub decimals: u8,
    /// The amount in the token's smallest units.
    pub amount: u64,
    /// Whether the destination account does not exist and is created first.
    pub create_destination: bool,
}

/// The legacy message of a token withdraw: the idempotent creation of the destination associated token account
/// when it is to be created (paid by the wallet), then the checked transfer from the wallet's own associated token
/// account (CRD ch.40 R40.6B.10). The one place the token instructions are built.
pub fn token_transfer_message(params: &TokenTransferParams<'_>, blockhash: &Hash) -> Result<Message, ProgramError> {
    let source = associated_token_account(params.wallet, params.mint, params.program_id);
    let destination = associated_token_account(params.recipient, params.mint, params.program_id);
    let mut instructions = Vec::with_capacity(2);
    if params.create_destination {
        instructions.push(create_associated_token_account_idempotent(
            params.wallet,
            params.recipient,
            params.mint,
            params.program_id,
        ));
    }
    instructions.push(spl_token::instruction::transfer_checked(
        params.program_id,
        &source,
        params.mint,
        &destination,
        params.wallet,
        &[],
        params.amount,
        params.decimals,
    )?);
    Ok(Message::new_with_blockhash(
        &instructions,
        Some(params.wallet),
        blockhash,
    ))
}

#[derive(Debug)]
pub struct SplTokenConf {
    pub decimals: u8,
    pub ticker: String,
    pub token_contract_address: Pubkey,
    /// The token program that owns the mint (SPL Token or Token-2022). The token's account, balance and transfer are
    /// derived with it; only the classic program can be withdrawn so far (CRD ch.40 D40.6B.4).
    pub program_id: Pubkey,
    /// The confirmation settings of this token (`set_required_confirmations`, `set_requires_notarization`).
    pub settings: SolanaCoinSettings,
    /// The token was activated with a request-supplied `protocol` and has no coins-configuration entry (a custom
    /// token). It is treated as wallet-only wherever the configuration would be consulted (CRD ch.40 R40.6B.6).
    pub custom: bool,
}

#[derive(Clone, Debug)]
pub struct SplTokenInfo {
    pub token_contract_address: Pubkey,
    pub program_id: Pubkey,
    pub decimals: u8,
}

#[derive(Debug)]
pub struct SplProtocolConf {
    pub platform_coin_ticker: String,
    pub decimals: u8,
    pub token_contract_address: Pubkey,
    /// The token program id: the one the configuration names for `SOLANATOKEN`, the SPL Token program for `SPLTOKEN`.
    pub program_id: Pubkey,
}

#[derive(Clone)]
pub struct SplToken {
    pub conf: Arc<SplTokenConf>,
    pub platform_coin: SolanaCoin,
}

impl Debug for SplToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult { f.write_str(&*self.conf.ticker) }
}

impl SplToken {
    pub fn new(
        decimals: u8,
        ticker: String,
        token_contract_address: Pubkey,
        program_id: Pubkey,
        platform_coin: SolanaCoin,
    ) -> SplToken {
        Self::with_custom(
            decimals,
            ticker,
            token_contract_address,
            program_id,
            platform_coin,
            false,
        )
    }

    /// A token from a request-supplied `protocol`: it has no coins-configuration entry and is wallet-only (CRD ch.40
    /// R40.6B.6).
    pub fn new_custom(
        decimals: u8,
        ticker: String,
        token_contract_address: Pubkey,
        program_id: Pubkey,
        platform_coin: SolanaCoin,
    ) -> SplToken {
        Self::with_custom(
            decimals,
            ticker,
            token_contract_address,
            program_id,
            platform_coin,
            true,
        )
    }

    /// A token that is custom (see [`Self::new_custom`]) or not.
    pub fn with_custom(
        decimals: u8,
        ticker: String,
        token_contract_address: Pubkey,
        program_id: Pubkey,
        platform_coin: SolanaCoin,
        custom: bool,
    ) -> SplToken {
        let conf = Arc::new(SplTokenConf {
            decimals,
            ticker,
            token_contract_address,
            program_id,
            settings: SolanaCoinSettings::default(),
            custom,
        });
        SplToken { conf, platform_coin }
    }

    pub fn get_info(&self) -> SplTokenInfo {
        SplTokenInfo {
            token_contract_address: self.conf.token_contract_address,
            program_id: self.conf.program_id,
            decimals: self.decimals(),
        }
    }
}

/// What the reads of a token withdraw decided: the amount, the quoted fee and rent, and the unsigned message.
struct TokenWithdrawPlan {
    units: u64,
    fee: u64,
    rent: u64,
    /// The decimals of the mint, from the token-account read of the same node.
    mint_decimals: u8,
    blockhash: Hash,
    message: Message,
}

/// The reads and checks of a token withdraw on one node (CRD ch.40 R40.6B.10, D40.6B.3, D40.6B.6). The outer error is
/// the node's (a transport failure reruns this on the next node); the inner one is the withdraw's own refusal.
async fn plan_withdraw_on_node(
    coin: &SplToken,
    node: &crate::solana::rpc_client::SolanaRpcClient,
    recipient: &Pubkey,
    max: bool,
    request_amount: &BigDecimal,
    info: &SplTokenInfo,
) -> Result<Result<TokenWithdrawPlan, MmError<WithdrawError>>, crate::solana::rpc_client::RpcError> {
    let platform = &coin.platform_coin;
    let decimals = coin.conf.decimals;
    let wallet = platform.key_pair.pubkey();
    // D40.6B.6 (a): `to` is an owner (wallet) address; an existing account owned by a token program is refused.
    if let Some(owner) = node.get_account_owner(recipient).await? {
        if owner == SPL_TOKEN_PROGRAM_ID || owner == TOKEN_2022_PROGRAM_ID {
            return Ok(Err(MmError::new(WithdrawError::InvalidAddress(
                "The recipient is a token account; give the address of its owner (a wallet)".to_string(),
            ))));
        }
    }

    let account = platform.token_account_address(info);
    let answer = crate::solana::solana_helpers::read_token_account(node, &account).await?;
    let balance = match crate::solana::solana_helpers::token_balance_from_answer(answer) {
        Ok(balance) => balance,
        Err(e) => return Ok(Err(e.map(WithdrawError::from))),
    };
    let threshold = coin.min_tx_amount();
    // D40.6B.3 (a): with `max` the transfer carries the account's whole raw amount.
    let units = if max {
        if balance.raw == 0 {
            return Ok(Err(MmError::new(WithdrawError::AmountTooLow {
                amount: 0.into(),
                threshold,
            })));
        }
        balance.raw
    } else {
        let amount = truncate_amount(request_amount, decimals);
        if amount <= BigDecimal::from(0) {
            return Ok(Err(MmError::new(WithdrawError::AmountTooLow { amount, threshold })));
        }
        match amount_to_units(&amount, decimals) {
            Ok(units) if units <= balance.raw => units,
            _ => {
                return Ok(Err(MmError::new(WithdrawError::NotSufficientBalance {
                    coin: coin.conf.ticker.clone(),
                    available: balance.amount,
                    required: amount,
                })))
            },
        }
    };

    let destination = associated_token_account(recipient, &info.token_contract_address, &info.program_id);
    let create_destination = !node.get_account_exists(&destination).await?;
    let rent = if create_destination {
        node.get_minimum_balance_for_rent_exemption(SPL_TOKEN_ACCOUNT_LEN)
            .await?
    } else {
        0
    };
    let blockhash = node.get_latest_blockhash().await?;
    let message = match token_transfer_message(
        &TokenTransferParams {
            wallet: &wallet,
            recipient,
            mint: &info.token_contract_address,
            program_id: &info.program_id,
            decimals,
            amount: units,
            create_destination,
        },
        &blockhash,
    ) {
        Ok(message) => message,
        Err(e) => return Ok(Err(MmError::new(WithdrawError::from(e)))),
    };
    let fee = node.get_fee_for_message(&message).await?;

    // The fee and the rent are paid in SOL by the wallet (R40.6B.10).
    let sol_balance = node.get_balance(&wallet).await?;
    let sol_required = match fee.checked_add(rent) {
        Some(required) => required,
        None => {
            return Ok(Err(MmError::new(WithdrawError::InternalError(
                "The fee and the rent overflow".to_string(),
            ))))
        },
    };
    if sol_required > sol_balance {
        return Ok(Err(MmError::new(WithdrawError::NotSufficientPlatformBalanceForFee {
            coin: platform.ticker.clone(),
            available: units_to_amount(sol_balance, platform.decimals),
            // R40.2A.4, error members: `required` is the exact sum of the fee and the rent, in the full decimals of
            // SOL.
            required: with_at_least_decimals(
                units_to_amount(fee, platform.decimals) + units_to_amount(rent, platform.decimals),
                platform.decimals,
            ),
        })));
    }
    // A withdraw that reaches here moves a positive amount the account holds, so the account exists and the node
    // stated the mint's decimals.
    let Some(mint_decimals) = balance.mint_decimals else {
        return Ok(Err(MmError::new(WithdrawError::InternalError(
            "The token account read carries no decimals".to_string(),
        ))));
    };
    Ok(Ok(TokenWithdrawPlan {
        units,
        fee,
        rent,
        mint_decimals,
        blockhash,
        message,
    }))
}

/// `withdraw` of a classic SPL token (CRD ch.40 R40.6B.10, D40.6B.3, D40.6B.6).
async fn withdraw_impl(coin: SplToken, req: WithdrawRequest) -> WithdrawResult {
    let recipient = parse_recipient(&req.to)?;
    // Token-2022 is not supported yet (D40.6B.4): its transfer, account size and rent differ, so no transaction is
    // built for a token of any other program than the classic one.
    if coin.conf.program_id != SPL_TOKEN_PROGRAM_ID {
        return MmError::err(WithdrawError::UnsupportedError(format!(
            "Withdrawing {} is not supported: only tokens of the SPL Token program can be withdrawn, Token-2022 \
             tokens are not supported yet",
            coin.conf.ticker
        )));
    }
    let platform = &coin.platform_coin;
    let info = coin.get_info();
    let decimals = coin.conf.decimals;
    let wallet = platform.key_pair.pubkey();

    // Every read of the withdraw goes to one node; a transport failure reruns them on the next one. Nothing is sent
    // and nothing is signed inside, so a rerun only repeats reads (CRD ch.40 R40.1.5).
    let planned = {
        let (coin, max, amount) = (coin.clone(), req.max, req.amount.clone());
        let info = info.clone();
        platform
            .rpc()
            .run(move |node| {
                let (coin, amount, info) = (coin.clone(), amount.clone(), info.clone());
                async move { plan_withdraw_on_node(&coin, &node, &recipient, max, &amount, &info).await }
            })
            .await?
    }?;
    let TokenWithdrawPlan {
        units,
        fee,
        rent,
        mint_decimals,
        blockhash,
        message,
    } = planned;

    let signers = vec![&platform.key_pair];
    let mut tx = Transaction::new_unsigned(message);
    tx.try_sign(&signers, blockhash)
        .map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
    let serialized_tx = serialize(&tx).map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
    let tx_hash = tx
        .signatures
        .first()
        .map(ToString::to_string)
        .or_mm_err(|| WithdrawError::InternalError("The transaction carries no signature".to_string()))?;

    let amount = units_to_amount(units, decimals);
    let received_by_me = if recipient == wallet { amount.clone() } else { 0.into() };
    // R40.2A.4 and D40.2A.1: the total is the sum of the fee and the rent, in the full decimals of SOL; the fee and
    // the rent stay single figures.
    let mut fee_details = SolanaFeeDetails::new(
        units_to_amount(fee, platform.decimals),
        units_to_amount(rent, platform.decimals),
    );
    fee_details.total_amount = with_at_least_decimals(fee_details.total_amount, platform.decimals);
    Ok(TransactionDetails {
        unsigned: None,
        tx_json: None,
        tx_hex: serialized_tx.into(),
        internal_id: tx_hash.as_bytes().to_vec().into(),
        tx_hash,
        from: vec![platform.my_address.clone()],
        to: vec![req.to],
        total_amount: amount.clone(),
        spent_by_me: amount.clone(),
        // R40.2A.4 and D40.2A.1: a sum member carries the mint's full decimals.
        my_balance_change: with_at_least_decimals(&received_by_me - &amount, mint_decimals),
        received_by_me,
        block_height: 0,
        timestamp: now_ms() / 1000,
        fee_details: Some(fee_details.into()),
        coin: coin.conf.ticker.clone(),
        kmd_rewards: None,
        transaction_type: TransactionType::TokenTransfer(
            sha256(&info.token_contract_address.to_bytes()).to_vec().into(),
        ),
        memo: None,
    })
}

#[async_trait]
impl SolanaCommonOps for SplToken {
    fn rpc(&self) -> &SolanaRpcPool { &self.platform_coin.client }

    fn is_token(&self) -> bool { true }
}

impl SplToken {
    fn my_balance_impl(&self) -> BalanceFut<CoinBalance> {
        let coin = self.clone();
        let fut = async move { coin.platform_coin.my_balance_spl(&coin.get_info()).await };
        Box::new(fut.boxed().compat())
    }
}

impl MarketCoinOps for SplToken {
    fn ticker(&self) -> &str { &self.conf.ticker }

    fn my_address(&self) -> Result<String, String> { Ok(self.platform_coin.my_address.clone()) }

    /// The platform's public key (the address); a token has no key of its own.
    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> {
        self.platform_coin.get_public_key()
    }

    /// No prefix and no pre-hash exist (see the platform coin).
    fn sign_message_hash(&self, _message: &str) -> Option<[u8; 32]> { None }

    fn sign_message(&self, message: &str) -> SignatureResult<String> {
        solana_common::sign_message(&self.platform_coin, message)
    }

    fn verify_message(&self, signature: &str, message: &str, pubkey_bs58: &str) -> VerificationResult<bool> {
        solana_common::verify_message(&self.platform_coin, signature, message, pubkey_bs58)
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let fut = self.my_balance_impl().and_then(Ok);
        Box::new(fut)
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> { self.platform_coin.base_coin_balance() }

    fn platform_ticker(&self) -> &str { self.platform_coin.ticker() }

    #[inline(always)]
    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        self.platform_coin.send_raw_tx(tx)
    }

    #[inline(always)]
    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        self.platform_coin.send_raw_tx_bytes(tx)
    }

    fn wait_for_confirmations(
        &self,
        _tx: &[u8],
        _confirmations: u64,
        _requires_nota: bool,
        _wait_until: u64,
        _check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.conf.ticker)))
    }

    fn wait_for_tx_spend(
        &self,
        _transaction: &[u8],
        _wait_until: u64,
        _from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn tx_enum_from_bytes(&self, _bytes: &[u8]) -> Result<TransactionEnum, String> {
        Err(format!(
            "Transaction decoding is not supported for {}",
            self.conf.ticker
        ))
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> { self.platform_coin.current_block() }

    fn display_priv_key(&self) -> Result<String, String> { self.platform_coin.display_priv_key() }

    /// One smallest unit of the token, 10^-decimals (CRD ch.40 D40.6B.1, maintainer decision).
    fn min_tx_amount(&self) -> BigDecimal { solana_common::smallest_unit(self.conf.decimals) }

    fn min_trading_vol(&self) -> MmNumber { MmNumber::from("0.00777") }
}

// Swaps are not supported for Solana coins; every operation answers an error. Orders and trade previews are also
// refused earlier (`untradable_coin_ticker`, CRD ch.60 R60.6.5). A swap that was imported from outside reaches these
// methods, so none of them may abort (CRD ch.40 R40.6B.14).
#[allow(forgetting_references, forgetting_copy_types, invalid_reference_casting)]
#[async_trait]
impl SwapOps for SplToken {
    fn send_taker_fee(&self, _dex_fee: &DexFee, _fee_addr: &[u8], _uuid: &[u8]) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_maker_payment(
        &self,
        time_lock: u32,
        maker_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_taker_payment(
        &self,
        time_lock: u32,
        taker_pub: &[u8],
        maker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_taker_refunds_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_maker_refunds_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn validate_fee(&self, _args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.conf.ticker)))
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.conf.ticker)))
    }

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.conf.ticker)))
    }

    fn check_if_my_payment_sent(
        &self,
        time_lock: u32,
        my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.conf.ticker)))
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        _secret_hash: &[u8],
        _tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        Err(swaps_unsupported(&self.conf.ticker))
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        _secret_hash: &[u8],
        _tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        Err(swaps_unsupported(&self.conf.ticker))
    }

    fn extract_secret(&self, _secret_hash: &[u8], _spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        Err(format!("Secret extraction is not supported for {}", self.conf.ticker))
    }

    fn negotiate_swap_contract_addr(
        &self,
        _other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        MmError::err(NegotiateSwapContractAddrErr::NoSwapContract)
    }

    /// `None` is safe only because every send, spend, refund and validate method of this impl answers an error, so no
    /// funds can move with a fallback key.
    fn get_htlc_key_pair(&self) -> Option<KeyPair> { None }
}

#[async_trait]
impl WatcherOps for SplToken {}

#[allow(forgetting_references, forgetting_copy_types, invalid_reference_casting)]
#[async_trait]
impl MmCoin for SplToken {
    fn is_asset_chain(&self) -> bool { false }

    /// A custom token (a request-supplied `protocol`, no configuration entry) is always wallet-only; a configured
    /// token follows its entry (CRD ch.40 R40.6B.6).
    fn wallet_only(&self, ctx: &MmArc) -> bool {
        if self.conf.custom {
            return true;
        }
        crate::coin_conf(ctx, self.ticker())["wallet_only"]
            .as_bool()
            .unwrap_or(false)
    }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        Box::new(Box::pin(withdraw_impl(self.clone(), req)).compat())
    }

    fn get_raw_transaction(&self, _req: RawTransactionRequest) -> RawTransactionFut {
        let coin = self.conf.ticker.to_string();
        Box::new(futures01::future::err(MmError::new(
            RawTransactionError::NotImplemented { coin },
        )))
    }

    fn decimals(&self) -> u8 { self.conf.decimals }

    fn convert_to_address(&self, _from: &str, _to_address_format: Json) -> Result<String, String> {
        Err(format!("convertaddress is not supported for {}", self.conf.ticker))
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult { self.platform_coin.validate_address(address) }

    /// Solana has no transaction history yet (CRD ch.40 R40.6B.15): the loop has nothing to do and ends at once, so
    /// a background start can never abort the process.
    fn process_history_loop(&self, _ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        Box::new(futures01::future::ok(()))
    }

    /// There is no history to sync, so the state is the neutral `NotEnabled`.
    fn history_sync_status(&self) -> HistorySyncState { HistorySyncState::NotEnabled }

    /// Get fee to be paid per 1 swap transaction
    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        let error = format!("get_trade_fee is not supported for {}", self.conf.ticker);
        Box::new(futures01::future::err(error))
    }

    async fn get_sender_trade_fee(
        &self,
        _value: TradePreimageValue,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Err(MmError::new(TradePreimageError::InternalError(format!(
            "Trade fees are not supported for {}",
            self.conf.ticker
        ))))
    }

    fn get_receiver_trade_fee(&self, _stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        let error = TradePreimageError::InternalError(format!("Trade fees are not supported for {}", self.conf.ticker));
        Box::new(futures01::future::err(MmError::new(error)))
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        _dex_fee_amount: BigDecimal,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Err(MmError::new(TradePreimageError::InternalError(format!(
            "Trade fees are not supported for {}",
            self.conf.ticker
        ))))
    }

    fn required_confirmations(&self) -> u64 { self.conf.settings.required_confirmations() }

    fn requires_notarization(&self) -> bool { self.conf.settings.requires_notarization() }

    fn set_required_confirmations(&self, confirmations: u64) {
        self.conf.settings.set_required_confirmations(confirmations)
    }

    fn set_requires_notarization(&self, requires_nota: bool) {
        self.conf.settings.set_requires_notarization(requires_nota)
    }

    /// No swap contract exists: Solana swaps are not supported.
    fn swap_contract_address(&self) -> Option<BytesJson> { None }

    fn mature_confirmations(&self) -> Option<u32> { Some(1) }

    fn coin_protocol_info(&self) -> Vec<u8> { Vec::new() }

    fn is_coin_protocol_supported(&self, _info: &Option<Vec<u8>>) -> bool { true }
}
