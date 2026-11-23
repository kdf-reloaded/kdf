//! # Purpose
//! The pure part of the SOL legs of the legacy swap (CRD ch.40 §40.6D): the instructions of the public swap program,
//! its payment address, the Memo instruction of the dex fee, the reading of an instruction's data, and the program
//! check of the activation. Nothing here reads a node or a key.
//!
//! # Rules implemented
//! - R40.6D.6: the payment is the program's `lamports_payment` with the secret hash, the lock time (a `u64`) and the
//!   amount in lamports; the payment account is the program-derived address of §40.6B.7.
//! - R40.6D.10: the claim is `receiver_spend_lamports`, the refund `sender_refund_lamports`.
//! - R40.6D.12: the secret is the 32 bytes after the discriminator of a claim instruction.
//! - R40.6D.13: the memo instruction of the public Memo program, no accounts, data = the text of the swap's uuid.
//! - R40.6D.2: which loader and which authority let a program pass.
//!
//! # Invariants
//! - Instruction data is the eight-byte discriminator, then the arguments in order at fixed width, integers
//!   little-endian (§40.6B.7). The data of an instruction is read only when its length is exactly that.
//! - Account metas follow the order of §40.6B.7.

use solana_message::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::fmt;

/// `lamports_payment` (§40.6B.7).
pub(crate) const DISC_LAMPORTS_PAYMENT: [u8; 8] = [0x9f, 0x43, 0x53, 0x6c, 0x62, 0x9d, 0xb0, 0x85];
/// `receiver_spend_lamports` (§40.6B.7).
pub(crate) const DISC_RECEIVER_SPEND_LAMPORTS: [u8; 8] = [0x3d, 0x53, 0xe0, 0x89, 0xb4, 0xbe, 0x6c, 0xf9];
/// `sender_refund_lamports` (§40.6B.7).
pub(crate) const DISC_SENDER_REFUND_LAMPORTS: [u8; 8] = [0x0f, 0x41, 0x43, 0xdd, 0x6c, 0x33, 0x04, 0xd4];

/// The seed text of a SOL payment address (§40.6B.7).
const PAYMENT_SEED: &[u8] = b"payment";

/// The public Memo program (version 2), R40.6D.13.
pub(crate) const MEMO_PROGRAM_ID: Pubkey = Pubkey::from_str_const("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

const HASH_LEN: usize = 32;
const U64_LEN: usize = 8;
/// Arguments of a payment or a refund: hash, lock time, amount.
const TERMS_DATA_LEN: usize = HASH_LEN + 2 * U64_LEN;

/// The terms of one SOL payment: everything the payment address is derived from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SolTerms {
    pub sender: Pubkey,
    pub receiver: Pubkey,
    pub secret_hash: [u8; HASH_LEN],
    /// The lock time in seconds since the epoch (the protocol's 32-bit value, widened).
    pub lock_time: u64,
    /// The amount in lamports.
    pub amount: u64,
}

impl SolTerms {
    /// The payment address of these terms under `program` (§40.6B.7) and its bump.
    pub(crate) fn payment_address(&self, program: &Pubkey) -> (Pubkey, u8) {
        let lock = self.lock_time.to_le_bytes();
        let amount = self.amount.to_le_bytes();
        Pubkey::find_program_address(
            &[
                PAYMENT_SEED,
                self.sender.as_ref(),
                self.receiver.as_ref(),
                &self.secret_hash,
                &lock,
                &amount,
            ],
            program,
        )
    }

    fn terms_data(&self) -> [u8; TERMS_DATA_LEN] {
        let mut data = [0u8; TERMS_DATA_LEN];
        data[..HASH_LEN].copy_from_slice(&self.secret_hash);
        data[HASH_LEN..HASH_LEN + U64_LEN].copy_from_slice(&self.lock_time.to_le_bytes());
        data[HASH_LEN + U64_LEN..].copy_from_slice(&self.amount.to_le_bytes());
        data
    }

    /// The `lamports_payment` instruction; the sender signs and pays.
    pub(crate) fn payment_instruction(&self, program: &Pubkey) -> Instruction {
        let (payment, _) = self.payment_address(program);
        let mut data = DISC_LAMPORTS_PAYMENT.to_vec();
        data.extend_from_slice(&self.terms_data());
        Instruction {
            program_id: *program,
            accounts: vec![
                AccountMeta::new(self.sender, true),
                AccountMeta::new_readonly(self.receiver, false),
                AccountMeta::new(payment, false),
                AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
            ],
            data,
        }
    }

    /// The `receiver_spend_lamports` instruction; the receiver signs.
    pub(crate) fn spend_instruction(&self, program: &Pubkey, secret: &[u8; HASH_LEN]) -> Instruction {
        let (payment, _) = self.payment_address(program);
        let mut data = DISC_RECEIVER_SPEND_LAMPORTS.to_vec();
        data.extend_from_slice(secret);
        data.extend_from_slice(&self.terms_data());
        Instruction {
            program_id: *program,
            accounts: vec![
                AccountMeta::new(self.receiver, true),
                AccountMeta::new(self.sender, false),
                AccountMeta::new(payment, false),
            ],
            data,
        }
    }

    /// The `sender_refund_lamports` instruction; the sender signs.
    pub(crate) fn refund_instruction(&self, program: &Pubkey) -> Instruction {
        let (payment, _) = self.payment_address(program);
        let mut data = DISC_SENDER_REFUND_LAMPORTS.to_vec();
        data.extend_from_slice(&self.terms_data());
        Instruction {
            program_id: *program,
            accounts: vec![
                AccountMeta::new(self.sender, true),
                AccountMeta::new_readonly(self.receiver, false),
                AccountMeta::new(payment, false),
            ],
            data,
        }
    }
}

/// The Memo instruction of the dex fee (R40.6D.13): no accounts, the data is the text.
pub(crate) fn memo_instruction(text: &str) -> Instruction {
    Instruction {
        program_id: MEMO_PROGRAM_ID,
        accounts: vec![],
        data: text.as_bytes().to_vec(),
    }
}

/// What an instruction of the swap program's data says, for the three SOL instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SolCall {
    Payment {
        secret_hash: [u8; HASH_LEN],
        lock_time: u64,
        amount: u64,
    },
    Spend {
        secret: [u8; HASH_LEN],
        secret_hash: [u8; HASH_LEN],
        lock_time: u64,
        amount: u64,
    },
    Refund {
        secret_hash: [u8; HASH_LEN],
        lock_time: u64,
        amount: u64,
    },
}

fn read_terms(data: &[u8]) -> Option<([u8; HASH_LEN], u64, u64)> {
    if data.len() != TERMS_DATA_LEN {
        return None;
    }
    let hash: [u8; HASH_LEN] = data[..HASH_LEN].try_into().ok()?;
    let lock = u64::from_le_bytes(data[HASH_LEN..HASH_LEN + U64_LEN].try_into().ok()?);
    let amount = u64::from_le_bytes(data[HASH_LEN + U64_LEN..].try_into().ok()?);
    Some((hash, lock, amount))
}

impl SolCall {
    /// Reads the data of an instruction of the swap program; `None` for any other instruction or any other length.
    pub(crate) fn parse(data: &[u8]) -> Option<SolCall> {
        let (disc, rest) = data.split_at_checked(8)?;
        if disc == DISC_LAMPORTS_PAYMENT {
            let (secret_hash, lock_time, amount) = read_terms(rest)?;
            Some(SolCall::Payment {
                secret_hash,
                lock_time,
                amount,
            })
        } else if disc == DISC_SENDER_REFUND_LAMPORTS {
            let (secret_hash, lock_time, amount) = read_terms(rest)?;
            Some(SolCall::Refund {
                secret_hash,
                lock_time,
                amount,
            })
        } else if disc == DISC_RECEIVER_SPEND_LAMPORTS {
            let (secret, rest) = rest.split_at_checked(HASH_LEN)?;
            let (secret_hash, lock_time, amount) = read_terms(rest)?;
            Some(SolCall::Spend {
                secret: secret.try_into().ok()?,
                secret_hash,
                lock_time,
                amount,
            })
        } else {
            None
        }
    }
}

/// Why a platform stays wallet-only (R40.6D.2). The text names which case applies and carries no node address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalletOnlyReason {
    /// The entry has no `swap_program_id`.
    NoProgramConfigured,
    /// The program account does not exist at `finalized`.
    ProgramMissing,
    /// The program account is not executable.
    ProgramNotExecutable,
    /// The program is upgradeable: its program-data account names an upgrade authority.
    ProgramUpgradeable,
    /// The program's loader is none of the loaders without upgrades the check accepts, or its account is malformed.
    ProgramLoaderUnsupported,
    /// The program account could not be read.
    ProgramUnreadable,
}

impl fmt::Display for WalletOnlyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            WalletOnlyReason::NoProgramConfigured => "the coins entry names no swap program (`swap_program_id`)",
            WalletOnlyReason::ProgramMissing => "the swap program of the coins entry does not exist on the cluster",
            WalletOnlyReason::ProgramNotExecutable => "the swap program of the coins entry is not executable",
            WalletOnlyReason::ProgramUpgradeable => "the swap program of the coins entry can be upgraded",
            WalletOnlyReason::ProgramLoaderUnsupported => {
                "the swap program of the coins entry has an unsupported loader"
            },
            WalletOnlyReason::ProgramUnreadable => "the swap program of the coins entry could not be read",
        })
    }
}

/// The tag of a program account of the upgradeable loader (public layout: `u32` LE tag 2, then the program-data
/// address).
const LOADER_STATE_PROGRAM: u32 = 2;
/// The tag of a program-data account of the upgradeable loader (public layout: `u32` LE tag 3, `u64` slot, then an
/// optional key: one tag byte and 32 bytes).
const LOADER_STATE_PROGRAM_DATA: u32 = 3;

/// The first step of the program check, from the program account (R40.6D.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProgramCheck {
    /// The program passes: a loader without upgrades.
    Passes,
    /// The program is of the upgradeable loader: its program-data account at this address decides.
    ReadProgramData(Pubkey),
    /// The program does not pass.
    Refused(WalletOnlyReason),
}

/// Judges a program account at `finalized`: its executable flag, its owner (the loader) and its data.
pub(crate) fn check_program_account(owner: &Pubkey, executable: bool, data: &[u8]) -> ProgramCheck {
    if !executable {
        return ProgramCheck::Refused(WalletOnlyReason::ProgramNotExecutable);
    }
    if *owner == solana_sdk_ids::bpf_loader::ID || *owner == solana_sdk_ids::bpf_loader_deprecated::ID {
        return ProgramCheck::Passes;
    }
    if *owner == solana_sdk_ids::bpf_loader_upgradeable::ID {
        let state = data
            .get(..4)
            .and_then(|tag| <[u8; 4]>::try_from(tag).ok())
            .map(u32::from_le_bytes);
        let address = data.get(4..36).and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
        return match (state, address) {
            (Some(LOADER_STATE_PROGRAM), Some(address)) if data.len() == 36 => {
                ProgramCheck::ReadProgramData(Pubkey::new_from_array(address))
            },
            _ => ProgramCheck::Refused(WalletOnlyReason::ProgramLoaderUnsupported),
        };
    }
    // The loader v4 and every other owner.
    ProgramCheck::Refused(WalletOnlyReason::ProgramLoaderUnsupported)
}

/// The program-data account of an upgradeable program judged (R40.6D.2): it passes only when it names no upgrade
/// authority. `owner` must be the upgradeable loader.
pub(crate) fn check_program_data(owner: &Pubkey, data: &[u8]) -> Result<(), WalletOnlyReason> {
    if *owner != solana_sdk_ids::bpf_loader_upgradeable::ID {
        return Err(WalletOnlyReason::ProgramLoaderUnsupported);
    }
    let tag = data
        .get(..4)
        .and_then(|tag| <[u8; 4]>::try_from(tag).ok())
        .map(u32::from_le_bytes);
    if tag != Some(LOADER_STATE_PROGRAM_DATA) {
        return Err(WalletOnlyReason::ProgramLoaderUnsupported);
    }
    // 4 (tag) + 8 (slot) = 12; then the option's tag byte.
    match data.get(12) {
        Some(0) => Ok(()),
        Some(1) if data.len() >= 12 + 1 + 32 => Err(WalletOnlyReason::ProgramUpgradeable),
        _ => Err(WalletOnlyReason::ProgramLoaderUnsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_discriminators_are_the_public_convention() {
        // SHA-256("global:<name>") first eight bytes (§40.6B.7, recomputed by the chapter).
        use kdf_crypto::sha256;
        for (name, disc) in [
            ("lamports_payment", DISC_LAMPORTS_PAYMENT),
            ("receiver_spend_lamports", DISC_RECEIVER_SPEND_LAMPORTS),
            ("sender_refund_lamports", DISC_SENDER_REFUND_LAMPORTS),
        ] {
            let hash = sha256(format!("global:{name}").as_bytes());
            assert!(hash.as_slice()[..8] == disc, "a discriminator is the public convention");
        }
    }
}
