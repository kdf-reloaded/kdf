//! # Purpose
//! The pure part of the Solana transaction history (CRD ch.40 §40.6C): a finalized transaction as a node answers
//! `getTransaction` in `base64` encoding becomes the history rows of the platform coin and of the served tokens.
//!
//! # Rules implemented
//! - R40.6C.6: the account list is the static account keys, then the loaded addresses (writable, then read-only); the
//!   balances and the token-balance indices count over it. A transfer is a System `Transfer` or `TransferWithSeed`, a
//!   creation a System `CreateAccount` or `CreateAccountWithSeed`, a token transfer an SPL Token `Transfer` or
//!   `TransferChecked`, each top-level or inner.
//! - R40.6C.7: the SOL row. R40.6C.8: the SPL row. R40.6C.10: a failed transaction is a fee-only SOL row of the fee
//!   payer and no token row; its instructions had no effect, so none is read.
//! - R40.6C.11: a transaction that cannot be decoded is an error; no member is filled with a stand-in value.
//!
//! # Invariants
//! - All arithmetic on lamports and token amounts is checked; the signed change is computed in `i128`.
//! - A row satisfies `my_balance_change == received_by_me - spent_by_me`.
//! - An instruction of a program this module does not read, or one of a read program that is not one of the six
//!   instruction kinds above, has no effect here: the balances are the ground truth for the amounts.

use super::rpc_client::{ConfirmedTransaction, TokenBalanceEntry, TransactionMeta};
use super::solana_common::{units_to_amount, with_at_least_decimals};
use super::SolanaFeeDetails;
use crate::{TransactionDetails, TransactionType};
use base58::FromBase58;
use bigdecimal::BigDecimal;
use kdf_crypto::sha256;
use solana_pubkey::Pubkey;
use solana_system_interface::instruction::SystemInstruction;
use solana_transaction::versioned::VersionedTransaction;
use spl_token::instruction::TokenInstruction;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Why a transaction cannot be turned into rows. The text is fixed apart from counts and positions.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DecodeError(String);

impl DecodeError {
    fn new(text: impl Into<String>) -> Self { DecodeError(text.into()) }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// A token the history serves: the ticker of its coin, its mint and its decimals.
#[derive(Clone, Debug)]
pub(crate) struct TokenSpec {
    pub ticker: String,
    pub mint: Pubkey,
    pub decimals: u8,
}

/// The lamports of the platform coin are SOL with these decimals.
const SOL_DECIMALS: u8 = 9;

/// What one instruction of the transaction does, as far as the history reads it. Indices count over the account list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Transfer {
        from: usize,
        to: usize,
        lamports: u64,
    },
    Create {
        funder: usize,
        lamports: u64,
    },
    TokenTransfer {
        source: usize,
        destination: usize,
        /// The mint account of a `TransferChecked`; `None` for `Transfer`, whose mint the token balances tell.
        mint: Option<usize>,
        amount: u64,
    },
}

#[derive(Clone, Debug)]
struct TokenBalance {
    account_index: usize,
    mint: Pubkey,
    owner: Option<Pubkey>,
    amount: u64,
}

/// A decoded finalized transaction.
#[derive(Debug)]
pub(crate) struct ParsedTx {
    /// The signature as base58.
    pub signature: String,
    pub slot: u64,
    signature_bytes: Vec<u8>,
    wire: Vec<u8>,
    block_time: u64,
    accounts: Vec<Pubkey>,
    fee: u64,
    pre_balances: Vec<u64>,
    post_balances: Vec<u64>,
    pre_tokens: Vec<TokenBalance>,
    post_tokens: Vec<TokenBalance>,
    failed: bool,
    effects: Vec<Effect>,
}

fn parse_pubkey(text: &str) -> Result<Pubkey, DecodeError> {
    Pubkey::from_str(text).map_err(|_| DecodeError::new("an address of the transaction is not a valid key"))
}

fn parse_tokens(entries: &Option<Vec<TokenBalanceEntry>>, accounts: usize) -> Result<Vec<TokenBalance>, DecodeError> {
    entries
        .iter()
        .flatten()
        .map(|entry| {
            if entry.account_index >= accounts {
                return Err(DecodeError::new(
                    "a token balance names an account outside the account list",
                ));
            }
            Ok(TokenBalance {
                account_index: entry.account_index,
                mint: parse_pubkey(&entry.mint)?,
                owner: entry.owner.as_deref().map(parse_pubkey).transpose()?,
                amount: entry
                    .ui_token_amount
                    .amount
                    .parse::<u64>()
                    .map_err(|_| DecodeError::new("a token balance amount is not a number of units"))?,
            })
        })
        .collect()
}

/// One instruction in the form the effect decoder reads.
struct RawInstruction {
    program: usize,
    accounts: Vec<usize>,
    data: Vec<u8>,
}

/// The effect of one instruction, `None` for an instruction the history does not read.
fn decode_effect(accounts: &[Pubkey], instruction: &RawInstruction) -> Result<Option<Effect>, DecodeError> {
    let program = accounts
        .get(instruction.program)
        .ok_or_else(|| DecodeError::new("an instruction names a program outside the account list"))?;
    if instruction.accounts.iter().any(|index| *index >= accounts.len()) {
        return Err(DecodeError::new(
            "an instruction names an account outside the account list",
        ));
    }
    let at = |position: usize| instruction.accounts.get(position).copied();
    if *program == solana_system_interface::program::ID {
        let Ok(system) = bincode::deserialize::<SystemInstruction>(&instruction.data) else {
            return Ok(None);
        };
        return Ok(match system {
            SystemInstruction::Transfer { lamports } => match (at(0), at(1)) {
                (Some(from), Some(to)) => Some(Effect::Transfer { from, to, lamports }),
                _ => None,
            },
            SystemInstruction::TransferWithSeed { lamports, .. } => match (at(0), at(2)) {
                (Some(from), Some(to)) => Some(Effect::Transfer { from, to, lamports }),
                _ => None,
            },
            SystemInstruction::CreateAccount { lamports, .. }
            | SystemInstruction::CreateAccountWithSeed { lamports, .. } => {
                at(0).map(|funder| Effect::Create { funder, lamports })
            },
            _ => None,
        });
    }
    if *program == spl_token::ID {
        let Ok(token) = TokenInstruction::unpack(&instruction.data) else {
            return Ok(None);
        };
        return Ok(match token {
            TokenInstruction::Transfer { amount } => match (at(0), at(1)) {
                (Some(source), Some(destination)) => Some(Effect::TokenTransfer {
                    source,
                    destination,
                    mint: None,
                    amount,
                }),
                _ => None,
            },
            TokenInstruction::TransferChecked { amount, .. } => match (at(0), at(1), at(2)) {
                (Some(source), Some(mint), Some(destination)) => Some(Effect::TokenTransfer {
                    source,
                    destination,
                    mint: Some(mint),
                    amount,
                }),
                _ => None,
            },
            _ => None,
        });
    }
    Ok(None)
}

/// Decodes the answer of `getTransaction` for `expected_signature`.
///
/// # Errors
///
/// The transaction cannot be read (R40.6C.11): it has no meta or no block time, its wire bytes do not decode, its
/// first signature is not the one asked for, or its lists do not agree with each other.
pub(crate) fn parse_transaction(
    answer: &ConfirmedTransaction,
    expected_signature: &str,
) -> Result<ParsedTx, DecodeError> {
    let meta: &TransactionMeta = answer
        .meta
        .as_ref()
        .ok_or_else(|| DecodeError::new("the transaction has no meta"))?;
    let block_time = answer
        .block_time
        .and_then(|time| u64::try_from(time).ok())
        .ok_or_else(|| DecodeError::new("the transaction has no block time"))?;
    if answer.transaction.1 != "base64" {
        return Err(DecodeError::new("the transaction is not in base64 encoding"));
    }
    let wire = base64::decode(&answer.transaction.0).map_err(|_| DecodeError::new("the wire bytes are not base64"))?;
    let transaction: VersionedTransaction =
        bincode::deserialize(&wire).map_err(|_| DecodeError::new("the wire bytes are not a transaction"))?;
    let signature = transaction
        .signatures
        .first()
        .ok_or_else(|| DecodeError::new("the transaction has no signature"))?;
    if signature.to_string() != expected_signature {
        return Err(DecodeError::new("the transaction is not the one asked for"));
    }

    let mut accounts: Vec<Pubkey> = transaction.message.static_account_keys().to_vec();
    if let Some(loaded) = &meta.loaded_addresses {
        for text in loaded.writable.iter().chain(&loaded.readonly) {
            accounts.push(parse_pubkey(text)?);
        }
    }
    if meta.pre_balances.len() != accounts.len() || meta.post_balances.len() != accounts.len() {
        return Err(DecodeError::new("the balances do not match the account list"));
    }
    let failed = meta.err.is_some();

    let mut effects = Vec::new();
    if !failed {
        let mut inner: HashMap<usize, Vec<RawInstruction>> = HashMap::new();
        for group in meta.inner_instructions.iter().flatten() {
            let list = inner.entry(group.index).or_default();
            for instruction in &group.instructions {
                list.push(RawInstruction {
                    program: instruction.program_id_index,
                    accounts: instruction.accounts.clone(),
                    data: instruction
                        .data
                        .from_base58()
                        .map_err(|_| DecodeError::new("an inner instruction's data is not base58"))?,
                });
            }
        }
        for (position, instruction) in transaction.message.instructions().iter().enumerate() {
            let top = RawInstruction {
                program: usize::from(instruction.program_id_index),
                accounts: instruction.accounts.iter().map(|index| usize::from(*index)).collect(),
                data: instruction.data.clone(),
            };
            effects.extend(decode_effect(&accounts, &top)?);
            for instruction in inner.get(&position).into_iter().flatten() {
                effects.extend(decode_effect(&accounts, instruction)?);
            }
        }
    }

    Ok(ParsedTx {
        signature: expected_signature.to_owned(),
        slot: answer.slot,
        signature_bytes: signature.as_ref().to_vec(),
        wire,
        block_time,
        fee: meta.fee,
        pre_balances: meta.pre_balances.clone(),
        post_balances: meta.post_balances.clone(),
        pre_tokens: parse_tokens(&meta.pre_token_balances, accounts.len())?,
        post_tokens: parse_tokens(&meta.post_token_balances, accounts.len())?,
        accounts,
        failed,
        effects,
    })
}

/// `units` (signed) as an amount of `decimals` decimals.
fn signed_amount(units: i128, decimals: u8) -> Result<BigDecimal, DecodeError> {
    let magnitude =
        u64::try_from(units.unsigned_abs()).map_err(|_| DecodeError::new("an amount does not fit 64 bits"))?;
    let amount = units_to_amount(magnitude, decimals);
    Ok(if units < 0 { -amount } else { amount })
}

fn unsigned_amount(units: u128, decimals: u8) -> Result<BigDecimal, DecodeError> {
    signed_amount(
        i128::try_from(units).map_err(|_| DecodeError::new("an amount does not fit 64 bits"))?,
        decimals,
    )
}

fn push_once(list: &mut Vec<String>, address: String) {
    if !list.contains(&address) {
        list.push(address);
    }
}

/// What a row is made of before it becomes a [`TransactionDetails`]: the member values of R40.6C.7 / R40.6C.8 in
/// units of the row's own coin.
struct Figures {
    my_balance_change: i128,
    received: u128,
    spent: u128,
    total: u128,
    from: Vec<String>,
    to: Vec<String>,
}

/// `received` and `spent` from the transfers' receipt and the balance change: `change == received - spent`, with
/// `spent` never negative (R40.6C.7).
fn settle(received: u128, change: i128) -> Result<(u128, u128), DecodeError> {
    let received_signed = i128::try_from(received).map_err(|_| DecodeError::new("an amount does not fit 64 bits"))?;
    let spent = received_signed
        .checked_sub(change)
        .ok_or_else(|| DecodeError::new("an amount does not fit 64 bits"))?;
    if spent >= 0 {
        Ok((received, spent.unsigned_abs()))
    } else {
        // More arrived than the transfers explain: what is not spent is received (R40.6C.7).
        Ok((change.unsigned_abs(), 0))
    }
}

impl ParsedTx {
    fn index_of(&self, key: &Pubkey) -> Option<usize> { self.accounts.iter().position(|account| account == key) }

    fn fee_payer(&self) -> Result<&Pubkey, DecodeError> {
        self.accounts
            .first()
            .ok_or_else(|| DecodeError::new("the transaction has no account"))
    }

    fn sol_change(&self, index: usize) -> Result<i128, DecodeError> {
        let (pre, post) = (self.pre_balances.get(index), self.post_balances.get(index));
        match (pre, post) {
            (Some(pre), Some(post)) => Ok(i128::from(*post) - i128::from(*pre)),
            _ => Err(DecodeError::new("an account has no balance")),
        }
    }

    /// The token balances of `me` for `mint`: the change of their sum, post minus pre. An account absent on one side
    /// counts 0 there; an entry that names no owner is not attributed to anyone.
    fn token_change(&self, me: &Pubkey, mint: &Pubkey) -> Result<i128, DecodeError> {
        let sum = |entries: &[TokenBalance]| -> Result<u128, DecodeError> {
            entries
                .iter()
                .filter(|entry| entry.mint == *mint && entry.owner.as_ref() == Some(me))
                .try_fold(0u128, |sum, entry| sum.checked_add(u128::from(entry.amount)))
                .ok_or_else(|| DecodeError::new("an amount does not fit 64 bits"))
        };
        let (pre, post) = (sum(&self.pre_tokens)?, sum(&self.post_tokens)?);
        let (pre, post) = (
            i128::try_from(pre).map_err(|_| DecodeError::new("an amount does not fit 64 bits"))?,
            i128::try_from(post).map_err(|_| DecodeError::new("an amount does not fit 64 bits"))?,
        );
        Ok(post - pre)
    }

    /// The mint a token account holds, from the token balances (either side).
    fn mint_of_account(&self, index: usize) -> Option<Pubkey> {
        self.post_tokens
            .iter()
            .chain(&self.pre_tokens)
            .find(|entry| entry.account_index == index)
            .map(|entry| entry.mint)
    }

    /// The wallet address of a token account: its owner when a token balance names one, else the account itself
    /// (R40.6C.8).
    fn holder_of(&self, index: usize) -> Option<Pubkey> {
        let owner = self
            .post_tokens
            .iter()
            .chain(&self.pre_tokens)
            .find(|entry| entry.account_index == index)
            .and_then(|entry| entry.owner);
        owner.or_else(|| self.accounts.get(index).copied())
    }

    /// Whether the transaction is a row of the platform coin (R40.6C.7).
    fn is_sol_row(&self, me: &Pubkey) -> Result<bool, DecodeError> {
        let Some(index) = self.index_of(me) else {
            return Ok(false);
        };
        Ok(self.sol_change(index)? != 0 || self.fee_payer()? == me)
    }

    /// Whether a token transfer of the token's mint has a source or a destination held by `me` (a send to self
    /// included, whose balance change is 0).
    fn moves_token_of(&self, me: &Pubkey, token: &TokenSpec) -> Result<bool, DecodeError> {
        for effect in &self.effects {
            let Effect::TokenTransfer {
                source,
                destination,
                mint,
                ..
            } = effect
            else {
                continue;
            };
            let moved_mint = match mint {
                Some(index) => self.accounts.get(*index).copied(),
                None => self
                    .mint_of_account(*source)
                    .or_else(|| self.mint_of_account(*destination)),
            };
            if moved_mint != Some(token.mint) {
                continue;
            }
            let (Some(source_holder), Some(destination_holder)) =
                (self.holder_of(*source), self.holder_of(*destination))
            else {
                return Err(DecodeError::new("a token transfer names an account outside the list"));
            };
            if source_holder == *me || destination_holder == *me {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the transaction is a row of the token (R40.6C.8): the wallet's amount of the mint changes, or a token
    /// transfer of the mint has the wallet as source or destination holder. A failed transaction has no effects read
    /// and changes no token balance, so it is no row (R40.6C.10).
    fn is_token_row(&self, me: &Pubkey, token: &TokenSpec) -> Result<bool, DecodeError> {
        if self.failed {
            return Ok(false);
        }
        Ok(self.token_change(me, &token.mint)? != 0 || self.moves_token_of(me, token)?)
    }

    /// Whether any list gets a row from this transaction: the caller reads the block height of the slot only then.
    pub fn affects(&self, me: &Pubkey, tokens: &[TokenSpec]) -> Result<bool, DecodeError> {
        if self.is_sol_row(me)? {
            return Ok(true);
        }
        for token in tokens {
            if self.is_token_row(me, token)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The SOL the fee payer paid (`meta.fee`) when it is `me`, else 0, and the rent `me` funds with creations.
    fn fee_and_rent(&self, me: &Pubkey) -> Result<(u64, u128), DecodeError> {
        let fee = if self.fee_payer()? == me { self.fee } else { 0 };
        let mut rent: u128 = 0;
        for effect in &self.effects {
            if let Effect::Create { funder, lamports } = effect {
                if self.accounts.get(*funder) == Some(me) {
                    rent = rent
                        .checked_add(u128::from(*lamports))
                        .ok_or_else(|| DecodeError::new("an amount does not fit 64 bits"))?;
                }
            }
        }
        Ok((fee, rent))
    }

    fn fee_details(&self, me: &Pubkey) -> Result<SolanaFeeDetails, DecodeError> {
        let (fee, rent) = self.fee_and_rent(me)?;
        let rent = u64::try_from(rent).map_err(|_| DecodeError::new("an amount does not fit 64 bits"))?;
        let mut details =
            SolanaFeeDetails::new(units_to_amount(fee, SOL_DECIMALS), units_to_amount(rent, SOL_DECIMALS));
        details.total_amount = with_at_least_decimals(details.total_amount, SOL_DECIMALS);
        Ok(details)
    }

    fn sol_figures(&self, me: &Pubkey, index: usize) -> Result<Figures, DecodeError> {
        let change = self.sol_change(index)?;
        let mut received: u128 = 0;
        let mut total: u128 = 0;
        let (mut from, mut to) = (Vec::new(), Vec::new());
        for effect in &self.effects {
            if let Effect::Transfer {
                from: source,
                to: destination,
                lamports,
            } = effect
            {
                let (source, destination) = (*source, *destination);
                if source != index && destination != index {
                    continue;
                }
                total = total
                    .checked_add(u128::from(*lamports))
                    .ok_or_else(|| DecodeError::new("an amount does not fit 64 bits"))?;
                if destination == index {
                    received = received
                        .checked_add(u128::from(*lamports))
                        .ok_or_else(|| DecodeError::new("an amount does not fit 64 bits"))?;
                }
                for (list, position) in [(&mut from, source), (&mut to, destination)] {
                    let account = self
                        .accounts
                        .get(position)
                        .ok_or_else(|| DecodeError::new("a transfer names an account outside the list"))?;
                    push_once(list, account.to_string());
                }
            }
        }
        let (fee_paid, _) = self.fee_and_rent(me)?;
        let (received, spent) = settle(received, change)?;
        if from.is_empty() {
            // No transfer names the platform address (R40.6C.7).
            from.push(self.fee_payer()?.to_string());
            to.push(me.to_string());
            total = change.unsigned_abs().saturating_sub(u128::from(fee_paid));
        }
        Ok(Figures {
            my_balance_change: change,
            received,
            spent,
            total,
            from,
            to,
        })
    }

    /// The row of the platform coin (R40.6C.7). `None` when the transaction is no row of it.
    fn sol_row(&self, me: &Pubkey, ticker: &str, block_height: u64) -> Result<Option<TransactionDetails>, DecodeError> {
        let Some(index) = self.index_of(me) else {
            return Ok(None);
        };
        if !self.is_sol_row(me)? {
            return Ok(None);
        }
        let figures = self.sol_figures(me, index)?;
        let has_token_transfer = self
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::TokenTransfer { .. }));
        let names_me = self
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Transfer { from, to, .. } if *from == index || *to == index));
        let transaction_type = if self.fee_payer()? == me && has_token_transfer && !names_me {
            TransactionType::FeeForTokenTx
        } else {
            TransactionType::StandardTransfer
        };
        Ok(Some(self.details(
            ticker,
            block_height,
            transaction_type,
            &figures,
            self.fee_details(me)?,
            SOL_DECIMALS,
        )?))
    }

    /// The row of `token` (R40.6C.8). `None` when the transaction is no row of it.
    fn token_row(
        &self,
        me: &Pubkey,
        token: &TokenSpec,
        block_height: u64,
    ) -> Result<Option<TransactionDetails>, DecodeError> {
        if !self.is_token_row(me, token)? {
            return Ok(None);
        }
        let change = self.token_change(me, &token.mint)?;
        let mut received: u128 = 0;
        let mut total: u128 = 0;
        let (mut from, mut to) = (Vec::new(), Vec::new());
        for effect in &self.effects {
            let Effect::TokenTransfer {
                source,
                destination,
                mint,
                amount,
            } = effect
            else {
                continue;
            };
            let moved_mint = match mint {
                Some(index) => self.accounts.get(*index).copied(),
                None => self
                    .mint_of_account(*source)
                    .or_else(|| self.mint_of_account(*destination)),
            };
            if moved_mint != Some(token.mint) {
                continue;
            }
            let (source_holder, destination_holder) = (self.holder_of(*source), self.holder_of(*destination));
            let (Some(source_holder), Some(destination_holder)) = (source_holder, destination_holder) else {
                return Err(DecodeError::new("a token transfer names an account outside the list"));
            };
            if source_holder != *me && destination_holder != *me {
                continue;
            }
            total = total
                .checked_add(u128::from(*amount))
                .ok_or_else(|| DecodeError::new("an amount does not fit 64 bits"))?;
            if destination_holder == *me {
                received = received
                    .checked_add(u128::from(*amount))
                    .ok_or_else(|| DecodeError::new("an amount does not fit 64 bits"))?;
            }
            push_once(&mut from, source_holder.to_string());
            push_once(&mut to, destination_holder.to_string());
        }
        let (received, spent) = settle(received, change)?;
        if from.is_empty() {
            // No transfer of the mint names the platform address: R40.6C.7's rule in the token's units. The SOL fee
            // is not part of a token amount, so none is taken off.
            from.push(self.fee_payer()?.to_string());
            to.push(me.to_string());
            total = change.unsigned_abs();
        }
        let figures = Figures {
            my_balance_change: change,
            received,
            spent,
            total,
            from,
            to,
        };
        let transaction_type = TransactionType::TokenTransfer(sha256(&token.mint.to_bytes()).to_vec().into());
        Ok(Some(self.details(
            &token.ticker,
            block_height,
            transaction_type,
            &figures,
            self.fee_details(me)?,
            token.decimals,
        )?))
    }

    fn details(
        &self,
        ticker: &str,
        block_height: u64,
        transaction_type: TransactionType,
        figures: &Figures,
        fee_details: SolanaFeeDetails,
        decimals: u8,
    ) -> Result<TransactionDetails, DecodeError> {
        Ok(TransactionDetails {
            unsigned: None,
            tx_json: None,
            tx_hex: self.wire.clone().into(),
            tx_hash: self.signature.clone(),
            internal_id: self.signature_bytes.clone().into(),
            from: figures.from.clone(),
            to: figures.to.clone(),
            total_amount: unsigned_amount(figures.total, decimals)?,
            spent_by_me: with_at_least_decimals(unsigned_amount(figures.spent, decimals)?, decimals),
            received_by_me: unsigned_amount(figures.received, decimals)?,
            my_balance_change: with_at_least_decimals(signed_amount(figures.my_balance_change, decimals)?, decimals),
            block_height,
            timestamp: self.block_time,
            fee_details: Some(fee_details.into()),
            coin: ticker.to_owned(),
            kmd_rewards: None,
            transaction_type,
            memo: None,
        })
    }

    /// Every row the transaction makes, as `(ticker, row)`: the platform coin's first, then the tokens' in the order
    /// given. One transaction is at most one row in each list (R40.6C.8).
    pub fn rows(
        &self,
        me: &Pubkey,
        platform_ticker: &str,
        tokens: &[TokenSpec],
        block_height: u64,
    ) -> Result<Vec<(String, TransactionDetails)>, DecodeError> {
        let mut rows = Vec::new();
        if let Some(row) = self.sol_row(me, platform_ticker, block_height)? {
            rows.push((platform_ticker.to_owned(), row));
        }
        for token in tokens {
            if let Some(row) = self.token_row(me, token, block_height)? {
                rows.push((token.ticker.clone(), row));
            }
        }
        Ok(rows)
    }
}
