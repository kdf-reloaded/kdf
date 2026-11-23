//! # Purpose
//! The reading of a finalized transaction for the swap (CRD ch.40 §40.6D): the account list, the status and every
//! instruction, top-level and inner, with its program, its accounts and its data. The wire bytes the node answers are
//! the only input; nothing a peer sent is read here (R40.6D.9).
//!
//! # Invariants
//! - The account list is the static account keys, then the loaded addresses (writable, then read-only), as in
//!   `history_decode`; an index outside it is an error.
//! - A transaction whose first signature is not the one asked for is an error.
//! - Inner instructions follow their top-level instruction.

use super::rpc_client::ConfirmedTransaction;
use base58::FromBase58;
use solana_pubkey::Pubkey;
use solana_transaction::versioned::VersionedTransaction;
use std::collections::HashMap;
use std::str::FromStr;

/// One instruction with its accounts resolved to keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedInstruction {
    pub program: Pubkey,
    pub accounts: Vec<Pubkey>,
    pub data: Vec<u8>,
}

/// A transaction as the node answers it, for the swap's reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SwapTxView {
    pub slot: u64,
    /// Whether the transaction was included with an error (it then had no effect).
    pub failed: bool,
    /// The wire bytes of the transaction.
    pub wire: Vec<u8>,
    /// The account list; the first key is the fee payer.
    pub accounts: Vec<Pubkey>,
    /// Every instruction, each top-level one followed by its inner ones.
    pub instructions: Vec<ResolvedInstruction>,
}

fn pubkey(text: &str) -> Result<Pubkey, String> {
    Pubkey::from_str(text).map_err(|_| "an account key of the transaction is not base58".to_owned())
}

fn resolve(
    accounts: &[Pubkey],
    program: usize,
    indices: &[usize],
    data: Vec<u8>,
) -> Result<ResolvedInstruction, String> {
    let program = *accounts
        .get(program)
        .ok_or_else(|| "an instruction names a program outside the account list".to_owned())?;
    let accounts = indices
        .iter()
        .map(|index| {
            accounts
                .get(*index)
                .copied()
                .ok_or_else(|| "an instruction names an account outside the account list".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResolvedInstruction {
        program,
        accounts,
        data,
    })
}

/// Decodes the `getTransaction` answer for `expected_signature` (base58).
///
/// # Errors
///
/// The answer has no meta, is not in base64, does not decode, is another transaction than the one asked for, or its
/// lists do not agree with each other.
pub(crate) fn decode_swap_tx(answer: &ConfirmedTransaction, expected_signature: &str) -> Result<SwapTxView, String> {
    let meta = answer
        .meta
        .as_ref()
        .ok_or_else(|| "the transaction has no meta".to_owned())?;
    if answer.transaction.1 != "base64" {
        return Err("the transaction is not in base64 encoding".to_owned());
    }
    let wire = base64::decode(&answer.transaction.0).map_err(|_| "the wire bytes are not base64".to_owned())?;
    let transaction: VersionedTransaction =
        bincode::deserialize(&wire).map_err(|_| "the wire bytes are not a transaction".to_owned())?;
    let signature = transaction
        .signatures
        .first()
        .ok_or_else(|| "the transaction has no signature".to_owned())?;
    if signature.to_string() != expected_signature {
        return Err("the transaction is not the one asked for".to_owned());
    }
    let mut accounts: Vec<Pubkey> = transaction.message.static_account_keys().to_vec();
    if let Some(loaded) = &meta.loaded_addresses {
        for text in loaded.writable.iter().chain(&loaded.readonly) {
            accounts.push(pubkey(text)?);
        }
    }

    let mut inner: HashMap<usize, Vec<ResolvedInstruction>> = HashMap::new();
    for group in meta.inner_instructions.iter().flatten() {
        let list = inner.entry(group.index).or_default();
        for instruction in &group.instructions {
            let data = instruction
                .data
                .from_base58()
                .map_err(|_| "an inner instruction's data is not base58".to_owned())?;
            list.push(resolve(
                &accounts,
                instruction.program_id_index,
                &instruction.accounts,
                data,
            )?);
        }
    }
    let mut instructions = Vec::new();
    for (position, instruction) in transaction.message.instructions().iter().enumerate() {
        let indices: Vec<usize> = instruction.accounts.iter().map(|index| usize::from(*index)).collect();
        instructions.push(resolve(
            &accounts,
            usize::from(instruction.program_id_index),
            &indices,
            instruction.data.clone(),
        )?);
        instructions.extend(inner.remove(&position).into_iter().flatten());
    }
    Ok(SwapTxView {
        slot: answer.slot,
        failed: meta.err.is_some(),
        wire,
        accounts,
        instructions,
    })
}

/// The first signature of wire bytes, in base58: the transaction's identity (R40.6D.9).
///
/// # Errors
///
/// The bytes are not a transaction or it carries no signature.
pub(crate) fn first_signature_of(wire: &[u8]) -> Result<String, String> {
    let transaction: VersionedTransaction =
        bincode::deserialize(wire).map_err(|_| "the bytes are not a Solana transaction".to_owned())?;
    transaction
        .signatures
        .first()
        .map(ToString::to_string)
        .ok_or_else(|| "the transaction has no signature".to_owned())
}

/// The instructions of the top level of wire bytes (no node answer behind them), with their accounts resolved against
/// the static account keys; a transaction that loads addresses is refused.
///
/// # Errors
///
/// The bytes are not a transaction, or an index is outside the static account list.
pub(crate) fn top_level_instructions(wire: &[u8]) -> Result<Vec<ResolvedInstruction>, String> {
    let transaction: VersionedTransaction =
        bincode::deserialize(wire).map_err(|_| "the bytes are not a Solana transaction".to_owned())?;
    let accounts = transaction.message.static_account_keys();
    transaction
        .message
        .instructions()
        .iter()
        .map(|instruction| {
            let indices: Vec<usize> = instruction.accounts.iter().map(|index| usize::from(*index)).collect();
            resolve(
                accounts,
                usize::from(instruction.program_id_index),
                &indices,
                instruction.data.clone(),
            )
        })
        .collect()
}
