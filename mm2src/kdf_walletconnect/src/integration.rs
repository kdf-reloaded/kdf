//! The §22.3 integration trait.
//!
//! Coin support modules implement [`WcCoinSigner`] to surface
//! WalletConnect-backed signing for the chain family they own. The trait is
//! deliberately narrow — a CAIP-2 chain-id resolver, a sign flow, a sign-and-send
//! flow, and a session pointer — and **chain-family agnostic**: it carries no
//! EVM / Cosmos / UTXO transaction knowledge. Each implementor picks its own
//! associated parameter and return types so the WalletConnect subsystem never
//! needs to understand the concrete transaction encodings involved (chapter 22
//! §22.3, binding R1/R2).
//!
//! The trait methods take the [`WalletConnectCtx`] handle as a parameter, so an
//! implementing coin need not store the handle itself.

use crate::error::WalletConnectError;
use crate::{Topic, WalletConnectCtx};
use async_trait::async_trait;

/// WalletConnect-backed signing for one chain family (chapter 22 §22.3).
///
/// The associated types let each chain family pick its own shapes:
/// - [`UnsignedTx`](Self::UnsignedTx): the chain-specific unsigned-transaction
///   description the sign / send flows consume (an EVM transaction object, a
///   Cosmos sign-direct payload, a UTXO PSBT, ...).
/// - [`SignedTx`](Self::SignedTx): what a sign-only flow returns (e.g. the
///   signed raw transaction bytes).
/// - [`TxHash`](Self::TxHash): what a sign-and-broadcast flow returns (e.g. the
///   broadcast transaction hash / id).
///
/// All flows report failures through [`WalletConnectError`]; implementors must
/// not panic.
#[async_trait]
pub trait WcCoinSigner {
    /// The chain-specific unsigned-transaction description the sign flows take.
    type UnsignedTx: Send;
    /// What a sign-only flow returns (e.g. the signed raw transaction).
    type SignedTx;
    /// What a sign-and-broadcast flow returns (e.g. the transaction hash).
    type TxHash;

    /// The CAIP-2 chain id this coin is bound to (e.g. `eip155:1`), resolved
    /// against the WalletConnect handle.
    async fn wc_chain_id(&self, wc: &WalletConnectCtx) -> Result<String, WalletConnectError>;

    /// Signs `tx` over WalletConnect **without** broadcasting it.
    async fn wc_sign_transaction(
        &self,
        wc: &WalletConnectCtx,
        tx: Self::UnsignedTx,
    ) -> Result<Self::SignedTx, WalletConnectError>;

    /// Signs `tx` over WalletConnect **and** broadcasts it.
    async fn wc_sign_and_send_transaction(
        &self,
        wc: &WalletConnectCtx,
        tx: Self::UnsignedTx,
    ) -> Result<Self::TxHash, WalletConnectError>;

    /// The topic of the settled session this coin should route signing requests
    /// through, resolved from the coin's CAIP-2 chain id (chapter 22 §22.3
    /// session pointer; see [`SessionManager::session_topic_for_chain`]).
    ///
    /// [`SessionManager::session_topic_for_chain`]: crate::session::SessionManager::session_topic_for_chain
    async fn wc_session_topic(&self, wc: &WalletConnectCtx) -> Result<Topic, WalletConnectError>;
}
