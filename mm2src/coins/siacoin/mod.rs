// Siacoin module — adapted from upstream KDF for the reloaded-gplv2-base fork.
//
// Key adaptations from upstream:
// - Uses fork's older positional-parameter SwapOps trait (not struct-based args)
// - Uses `PrivKeyPolicy::KeyPair` (not `Iguana`)
// - No AbortableQueue/AbortableSystem (fork lacks this infrastructure)
// - TransactionDetails uses tx_hex/tx_hash (no TransactionData enum)
// - Fork's `extract_secret` returns `Vec<u8>` not `[u8; 32]`
// - Fork's `can_refund_htlc` returns `Box<dyn Future<...>>` not async
//
// Split into sub-modules for maintainability:
//   siacoin_types      – constants, config types, SiaTransaction, conversion utilities
//   siacoin_helpers    – constructor, builder, and internal utility methods
//   siacoin_swap_ops   – SwapOps trait impl + typed swap helper methods
//   siacoin_market_ops – MarketCoinOps trait impl
//   siacoin_mm_coin    – MmCoin + WatcherOps trait impls
//

// ─── Imports (pub(crate) so child modules inherit via `use super::*`) ───────

pub(crate) use super::{BalanceError, CoinBalance, CoinsContext, HistorySyncState, MarketCoinOps, MmCoin,
                       RawTransactionError, RawTransactionFut, RawTransactionRequest, RawTransactionResult,
                       SignRawTransactionRequest, SignatureError, SwapOps, TradeFee, TransactionDetails,
                       TransactionEnum, TransactionErr, TransactionFut, TransactionType, UnexpectedDerivationMethod,
                       VerificationError};
pub(crate) use crate::{BalanceFut, CanRefundHtlc, CoinWithDerivationMethod, DerivationMethod, DexFee, FeeApproxStage,
                       FoundSwapTxSpend, NegotiateSwapContractAddrErr, PrivKeyBuildPolicy, PrivKeyPolicy,
                       RawTransactionRes, SignatureResult, TradePreimageFut, TradePreimageResult, TradePreimageValue,
                       Transaction, TxFeeDetails, ValidateAddressResult, ValidateFeeArgs, ValidatePaymentInput,
                       VerificationResult, WatcherOps, WithdrawFut, WithdrawRequest};

pub(crate) use async_trait::async_trait;
pub(crate) use bigdecimal::BigDecimal;
pub(crate) use common::executor::Timer;
pub(crate) use common::log::{debug, info};
pub(crate) use common::mm_number::MmNumber;
pub(crate) use common::now_ms;
pub(crate) use derive_more::{Display, From, Into};
pub(crate) use ed25519_dalek_bip32::DerivationPath as DalekDerivationPath;
pub(crate) use futures::compat::Future01CompatExt;
pub(crate) use futures::{FutureExt, TryFutureExt};
pub(crate) use futures01::Future;
pub(crate) use hex;
pub(crate) use kdf_crypto::sha256;
pub(crate) use keys::KeyPair;
pub(crate) use mm2_core::mm_ctx::MmArc;
pub(crate) use mm2_err_handle::prelude::*;
pub(crate) use num_traits::ToPrimitive;
pub(crate) use rpc::v1::types::Bytes as BytesJson;
pub(crate) use serde_json::Value as Json;
pub(crate) use std::collections::hash_map::Entry;
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::convert::TryFrom;
pub(crate) use std::fmt;
pub(crate) use std::path::PathBuf;
pub(crate) use std::str::FromStr;
pub(crate) use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use uuid::Uuid;

// ─── Public re-exports from sia-rust ────────────────────────────────────────

// expose all of sia-rust so mm2_main can use it via coins::siacoin::sia_rust
pub use sia_rust;
pub use sia_rust::transport::client::{error as client_error, ApiClient as SiaApiClient, ApiClientHelpers,
                                      Client as SiaClient};
pub use sia_rust::transport::endpoints::{AddressesEventsRequest, ConsensusTipRequest, GetAddressUtxosRequest,
                                         GetEventRequest, TxpoolBroadcastRequest, TxpoolFeeRequest,
                                         TxpoolTransactionsRequest, TxpoolTransactionsResponse};
pub use sia_rust::types::{Address, Currency, Event, EventDataWrapper, EventPayout, EventType, Hash256, Hash256Error,
                          Keypair as SiaKeypair, KeypairError, Preimage, PreimageError, PublicKey, PublicKeyError,
                          SiacoinElement, SiacoinOutput, SiacoinOutputId, SpendPolicy, TransactionId, V1Transaction,
                          V2Transaction};
pub use sia_rust::utils::{V2TransactionBuilder, V2TransactionBuilderError};

// ─── Existing sub-modules ───────────────────────────────────────────────────

pub mod error;
pub use error::SiaCoinNewError;
pub(crate) use error::*;

pub mod sia_hd_wallet;
pub(crate) use sia_hd_wallet::SiaHDWallet;
mod sia_withdraw;
pub(crate) use sia_withdraw::SiaWithdrawBuilder;

// ─── Split sub-modules ─────────────────────────────────────────────────────

mod siacoin_helpers;
mod siacoin_history;
mod siacoin_market_ops;
mod siacoin_mm_coin;
mod siacoin_swap_ops;
mod siacoin_types;

pub use siacoin_history::process_history_loop;

// Re-export split module contents for backward-compatible access paths
pub use siacoin_types::*;

// ─── Core struct + type aliases ─────────────────────────────────────────────

pub type SiaCoin = SiaCoinGeneric<SiaClient>;
pub type SiaClientConf = <SiaClient as SiaApiClient>::Conf;

#[derive(Clone)]
pub struct SiaCoinGeneric<T: SiaApiClient + ApiClientHelpers> {
    pub conf: SiaCoinConf,
    pub priv_key_policy: Arc<PrivKeyPolicy<SiaKeypair>>,
    pub client: Arc<T>,
    pub history_sync_state: Arc<Mutex<HistorySyncState>>,
    required_confirmations: Arc<AtomicU64>,
    /// DEX-fee destination address resolved from `mm2_net_config` at activation time.
    /// Keyed on the active netid, so trades on netid 6133 use 6133's fee address
    /// rather than the netid 8762 default.
    pub(crate) fee_address: Address,
    /// Iguana (single-key) vs. HD-wallet address-derivation mode (CRD ch.20 D1).
    /// Arc-wrapped like the coin's other shared state (`client`, `priv_key_policy`,
    /// `history_sync_state`) so `SiaCoinGeneric` can stay cheaply `Clone`: `SiaHDWallet`
    /// holds an internal `HDAccountsMutex` and is not itself `Clone`.
    pub derivation_method: Arc<DerivationMethod<Address, SiaHDWallet>>,
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;
    use std::str::FromStr;

    #[test]
    fn test_siacoin_from_hastings_u128_max() {
        let hastings = u128::MAX;
        let siacoin = hastings_to_siacoin(hastings.into());
        assert_eq!(
            siacoin,
            BigDecimal::from_str("340282366920938.463463374607431768211455").unwrap()
        );
    }

    #[test]
    fn test_siacoin_from_hastings_total_supply() {
        let hastings = 57769875000000000000000000000000000u128;
        let siacoin = hastings_to_siacoin(hastings.into());
        assert_eq!(siacoin, BigDecimal::from_str("57769875000").unwrap());
    }

    #[test]
    fn test_siacoin_to_hastings_supply() {
        let siacoin = BigDecimal::from_str("57769875000").unwrap();
        let hastings = siacoin_to_hastings(siacoin).unwrap();
        assert_eq!(hastings, Currency(57769875000000000000000000000000000));
    }

    #[test]
    fn test_sia_transaction_serde_roundtrip() {
        let j = json!(
            {"siacoinInputs":[{"parent":{"id":"0f088eddda5320f8453a55349063abe43ba5b282631d5d2b9e684548f083055a","stateElement":{"leafIndex":3,"merkleProof":["ff9ce7f558df52b35d40fda59a8ab6d5ffb3dfab029992d02d1e929e0a36b6eb","b37a5387883748f73c1475ca85c8f3200eef09126c44824d0f44574109dabedc","0f1d4ef5b1bf0e6eb45240e717a1d548326f8379878944c6536fc73989cf2e7a","f85d8f6578bc2db41e8a206a060a10386c18505b533f64a5ceff574d602b57bd","c21c0b980cd4184996558b49e8b901c70cadb17fd27c62f472a2707f0eb6b092","482e9402c0c5e43d599b9683ba25224f74499f89e9bc33249794ff5e9f55e337","a29aabab81d0cf20e1bd33bb3e1138f1628a1337eb0430e4de47cf695eb25897","aeb60668a7e0ee0232f81642626104a1d4eb9ce3d0d9cf81196de48112e3ea41"]},"siacoinOutput":{"value":"299999000000000000000000000000","address":"c34caa97740668de2bbdb7174572ed64c861342bf27e80313cbfa02e9251f52e30aad3892533"},"maturityHeight":11},"satisfiedPolicy":{"policy":{"type":"pk","policy":"ed25519:a729be53dae7b0ed812f2a123ce93556014bbad8516ba6b1b496a112b46bbd97"},"signatures":["160e79ac52e0eaab5e92bd1675604a94b56ec58fdd0be3f3a842a4ece07d794f7ee1e8cc8f29b596bf71b2dc594df53347b9a4bcbec46fe09244ce6d3f6a6708"]}}],"siacoinOutputs":[{"value":"50000000000000000000000","address":"71731d7efe821794742c72a8376f56355b3c8a1984b861ccd42eed77a779a26626ea26ced3e2"},{"value":"299998949990000000000000000000","address":"c34caa97740668de2bbdb7174572ed64c861342bf27e80313cbfa02e9251f52e30aad3892533"}],"minerFee":"10000000000000000000"}
        );
        let tx = serde_json::from_value::<V2Transaction>(j).unwrap();
        let sia_tx = SiaTransaction(tx);

        let vec = serde_json::ser::to_vec(&sia_tx).unwrap();
        let tx2: SiaTransaction = serde_json::from_slice(&vec).unwrap();

        assert_eq!(sia_tx, tx2);
    }

    #[test]
    fn test_sia_fee_pubkey_init() {
        // Sanity-check that every supported netid has a valid hex-encoded ed25519
        // pubkey decodable into a Sia `PublicKey` and a derivable fee `Address`.
        for &netid in mm2_net_config::SUPPORTED_NETIDS {
            let cfg = mm2_net_config::net_config_or_panic(netid);
            let bytes = hex::decode(cfg.dex_fee_pubkey_ed25519())
                .unwrap_or_else(|_| panic!("netid {netid}: dex_fee_pubkey_ed25519 must be valid hex"));
            let pk = PublicKey::from_bytes(&bytes)
                .unwrap_or_else(|_| panic!("netid {netid}: dex_fee_pubkey_ed25519 must decode into PublicKey"));
            let _addr = Address::from_public_key(&pk);
        }
    }

    #[test]
    fn test_siacoin_from_hastings_coin() {
        let coin = hastings_to_siacoin(Currency::COIN);
        assert_eq!(coin, BigDecimal::from(1));
    }

    #[test]
    fn test_siacoin_from_hastings_zero() {
        let coin = hastings_to_siacoin(Currency::ZERO);
        assert_eq!(coin, BigDecimal::from(0));
    }

    #[test]
    fn test_siacoin_to_hastings_coin() {
        let coin = BigDecimal::from(1);
        let hastings = siacoin_to_hastings(coin).unwrap();
        assert_eq!(hastings, Currency::COIN);
    }

    #[test]
    fn test_siacoin_to_hastings_zero() {
        let coin = BigDecimal::from(0);
        let hastings = siacoin_to_hastings(coin).unwrap();
        assert_eq!(hastings, Currency::ZERO);
    }

    #[test]
    fn test_siacoin_to_hastings_one() {
        let coin = serde_json::from_str::<BigDecimal>("0.000000000000000000000001").unwrap();
        let hastings = siacoin_to_hastings(coin).unwrap();
        assert_eq!(hastings, Currency(1));
    }
}
