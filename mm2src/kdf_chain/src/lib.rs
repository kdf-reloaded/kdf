//! KDF chain primitives — Block, BlockHeader, Transaction with multi-coin extensions.
//!
//! Wire format mirrors the previous parity-derived implementation byte-for-byte;
//! the implementation is original, GPL-2.0-only.

#[macro_use] extern crate serialization_derive;

mod block;
mod constants_;
mod header;
mod merkle;
mod raw_header;
mod read_hash;
mod repr;
mod transaction;
pub mod zip244;

pub mod constants {
    pub use crate::constants_::*;
}

pub use block::Block;
pub use header::{BlockHeader, BlockHeaderBits, BlockHeaderNonce};
pub use merkle::{merkle_node_hash, merkle_root};
pub use primitives::{bytes, compact, hash, U256};
pub use raw_header::{RawBlockHeader, RawHeaderError};
pub use read_hash::{HashedData, ReadAndHash};
pub use repr::RepresentH256;
pub use transaction::{JoinSplit, OpaqueTx, OrchardAction, OrchardBundle, OutPoint, SaplingBundleV5, SaplingSpendV5,
                      ShieldedOutput, ShieldedSpend, Transaction, TransactionInput, TransactionOutput, TxHashAlgo};

pub type ShortTransactionId = hash::H48;
