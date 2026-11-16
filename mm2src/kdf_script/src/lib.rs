// kdf_script — Bitcoin script types and signer for KDF.
//
// Clean-room implementation, GPL-2.0-only. Written against:
//   * Bitcoin Core `src/script/script.h` (opcode table — protocol facts)
//   * BIP-143 (segwit v0 sighash)
//   * ZIP-243 (Zcash sapling sighash)
//   * Bitcoin Core `src/script/standard.cpp` (script type classification)
//
// Only the subset actually consumed by the KDF stack is implemented.
// The interpreter (script execution / signature verification stack
// machine) is intentionally absent — KDF builds and signs scripts but
// never executes them; chain nodes do that.

pub use primitives::{bytes, hash};

mod builder;
mod error;
mod num;
mod opcode;
mod script;
mod sign;

pub use crate::builder::Builder;
pub use crate::error::Error;
pub use crate::num::Num;
pub use crate::opcode::Opcode;
pub use crate::script::{is_witness_commitment_script, Instruction, Script, ScriptAddress, ScriptType, ScriptWitness};
pub use crate::sign::{SignatureHashError, SignatureVersion, SignerHashAlgo, TransactionInputSigner,
                      UnsignedTransactionInput};
