//! # Siacoin error taxonomy
//!
//! All errors raised by the Sia coin adapter live in this module. They are
//! grouped into three sections:
//!
//! 1. **Conversion / parse errors** — pure data-shape failures (BigDecimal
//!    overflow, malformed pubkey bytes, malformed args structs, etc.).
//! 2. **Swap-operation errors** — one enum per HTLC step (taker fee, maker
//!    payment, taker payment, refund, spend, validate, …) that mixes the
//!    operation's parse errors with its transport / build / broadcast
//!    failures.
//! 3. **Lifecycle errors** — coin builder, `SiaCoin::new`, and keypair
//!    accessor errors raised during activation.
//!
//! ## Display-string convention
//!
//! Each enum's `#[error(...)]` strings carry a short bracketed tag of the
//! form `"[op] detail"`. The bracket replaces the legacy `"Type::method:"`
//! prefix; the tag is informational only and is *not* a wire contract — it
//! is surfaced inside JSON-RPC `error` payloads which clients are expected
//! to treat as opaque text.

use bigdecimal::BigDecimal;
use thiserror::Error;
use uuid::Uuid;

use crypto::privkey::PrivKeyError;

use crate::node_url::scrub_urls_in_text;
use crate::siacoin::client_error::{BroadcastTransactionError, ClientError, CurrentHeightError,
                                   FindWhereUtxoSpentError, GetMedianTimestampError, GetUnconfirmedTransactionError,
                                   UtxoFromTxidError};
use crate::siacoin::{Address, Currency, Event, EventDataWrapper, Hash256, Hash256Error, KeypairError, PreimageError,
                     PublicKeyError, SiaTransaction, SiacoinOutput, TransactionId, V2TransactionBuilderError};
use crate::{DexFee, TransactionEnum};

/// The text of an error that comes from the external Sia library or its HTTP client, with every URL in it reduced
/// to its host-only form (CRD ch.20 R-N1): the library's request errors carry the full request URL.
pub(crate) fn sia_text(error: &dyn std::fmt::Display) -> String { scrub_urls_in_text(&error.to_string()) }

// =====================================================================
// 1. Conversion / parse errors
// =====================================================================

/// Failure converting a SC-denominated `BigDecimal` to a `u128` of hastings.
#[derive(Debug, Error)]
pub enum SiacoinToHastingsError {
    #[error("[siacoin->hastings] cannot fit BigDecimal `{0}` into u128")]
    BigDecimalToU128(BigDecimal),
}

/// Failure (de)serializing a `SiaTransaction` blob.
#[derive(Debug, Error)]
pub enum SiaTransactionError {
    #[error("[sia-tx ser] failed encoding to bytes: {0}")]
    ToVec(serde_json::Error),
    #[error("[sia-tx de] failed decoding from bytes: {0}")]
    FromVec(serde_json::Error),
}

/// Validation of `SiaRefundPaymentArgs` derived from the cross-coin
/// `RefundPaymentArgs`.
#[derive(Debug, Error)]
pub enum SiaRefundPaymentArgsError {
    #[error("[refund-args] payment_tx parse failed: {0}")]
    ParseTx(#[from] SiaTransactionError),
    #[error("[refund-args] other_pubkey wrong length, expected 33 bytes, got: {0:?}")]
    InvalidOtherPublicKeyLength(Vec<u8>),
    #[error("[refund-args] other_pubkey parse failed: {0}")]
    ParseOtherPublicKey(#[from] PublicKeyError),
    #[error("[refund-args] secret_hash parse failed: {0}")]
    ParseSecretHash(#[from] Hash256Error),
}

/// Validation of `SiaValidateFeeArgs` derived from the cross-coin
/// `ValidateFeeArgs`.
#[derive(Debug, Error)]
#[allow(clippy::large_enum_variant)]
pub enum SiaValidateFeeArgsError {
    #[error("[validate-fee-args] uuid parse failed: {0}")]
    ParseUuid(#[from] uuid::Error),
    #[error("[validate-fee-args] uuid version {0} not supported")]
    UuidVersion(usize),
    #[error("[validate-fee-args] taker pubkey wrong length, expected 33 bytes, got: {0:?}")]
    InvalidTakerPublicKeyLength(Vec<u8>),
    #[error("[validate-fee-args] taker pubkey parse failed: {0}")]
    InvalidTakerPublicKey(#[from] PublicKeyError),
    #[error("[validate-fee-args] trade fee amount conversion failed: {0}")]
    SiacoinToHastings(#[from] SiacoinToHastingsError),
    #[error("[validate-fee-args] DexFee variant `{0}` not supported")]
    DexFeeVariant(String),
    #[error("[validate-fee-args] unexpected TransactionEnum variant")]
    TxEnumVariant,
}

/// Validation of `SiaCheckIfMyPaymentSentArgs`.
#[derive(Debug, Error)]
pub enum SiaCheckIfMyPaymentSentArgsError {
    #[error("[chk-payment-args] other_pub wrong length, expected 33 bytes, got: {0:?}")]
    InvalidOtherPublicKeyLength(Vec<u8>),
    #[error("[chk-payment-args] other_pub parse failed: {0}")]
    ParseOtherPublicKey(#[from] PublicKeyError),
    // Checked explicitly ahead of `Hash256::try_from` (see try_from_positional)
    // rather than left to fall through to `ParseSecretHash` below, because the
    // 20-byte case isn't malformed input -- it's the legacy RIPEMD160(SHA256(_))
    // secret hash width every non-Sia-paired swap still negotiates
    // (select_secret_hash_algo, ch.51 R71/R72). A swap negotiated before that
    // 32-byte requirement existed for a Sia-paired swap carries a 20-byte hash
    // that can never satisfy it; that's a structural fact about the swap, not
    // a bug to retry past, so it gets a message that says so instead of
    // `Hash256Error`'s generic "invalid slice length" (observed verbatim in a
    // real recover_funds_of_swap failure with no indication of why 20 bytes
    // showed up at all).
    #[error(
        "[chk-payment-args] secret_hash is {actual} bytes, Siacoin HTLCs require exactly 32 -- \
         this swap most likely negotiated the legacy 20-byte secret hash used for non-Sia pairs, \
         so its Sia-side payment cannot be checked or recovered automatically"
    )]
    WrongSecretHashLength { actual: usize },
    #[error("[chk-payment-args] secret_hash parse failed: {0}")]
    ParseSecretHash(#[from] Hash256Error),
    #[error("[chk-payment-args] amount conversion failed: {0}")]
    SiacoinToHastings(#[from] SiacoinToHastingsError),
}

/// Validation of `SiaWaitForHTLCTxSpendArgs`.
#[derive(Debug, Error)]
pub enum SiaWaitForHTLCTxSpendArgsError {
    #[error("[wait-htlc-args] transaction parse failed: {0}")]
    ParseTx(#[from] SiaTransactionError),
    #[error("[wait-htlc-args] secret_hash parse failed: {0}")]
    ParseSecretHash(#[from] Hash256Error),
}

/// Validation of `SiaValidatePaymentInput`.
#[derive(Debug, Error)]
pub enum SiaValidatePaymentInputError {
    #[error("[validate-payment-in] payment_tx parse failed: {0}")]
    ParseTx(#[from] SiaTransactionError),
    #[error("[validate-payment-in] other_pub wrong length, expected 33 bytes, got: {0:?}")]
    InvalidOtherPublicKeyLength(Vec<u8>),
    #[error("[validate-payment-in] other_pub parse failed: {0}")]
    ParseOtherPublicKey(#[from] PublicKeyError),
    // See SiaCheckIfMyPaymentSentArgsError::WrongSecretHashLength -- same
    // reasoning, same fix, for the live validate_maker_payment/
    // validate_taker_payment path rather than recover_funds's.
    #[error(
        "[validate-payment-in] secret_hash is {actual} bytes, Siacoin HTLCs require exactly 32 -- \
         this swap most likely negotiated the legacy 20-byte secret hash used for non-Sia pairs, \
         so its Sia-side payment cannot be validated"
    )]
    WrongSecretHashLength { actual: usize },
    #[error("[validate-payment-in] secret_hash parse failed: {0}")]
    ParseSecretHash(#[from] Hash256Error),
    #[error("[validate-payment-in] amount conversion failed: {0}")]
    SiacoinToHastings(#[from] SiacoinToHastingsError),
}

// =====================================================================
// 2. Swap-operation errors
// =====================================================================

/// Errors raised while sending the taker's `dex_fee` transaction.
#[derive(Debug, Error)]
pub enum SendTakerFeeError {
    #[error("[taker-fee] uuid parse failed: {0}")]
    ParseUuid(#[from] uuid::Error),
    #[error("[taker-fee] uuid version {0} not supported")]
    UuidVersion(usize),
    #[error("[taker-fee] trade fee amount conversion failed: {0}")]
    SiacoinToHastings(#[from] SiacoinToHastingsError),
    #[error("[taker-fee] DexFee variant `{0}` not supported")]
    DexFeeVariant(String),
    #[error("[taker-fee] keypair fetch failed: {0}")]
    MyKeypair(#[from] SiaCoinMyKeypairError),
    #[error("[taker-fee] funding failed: {0}")]
    FundTx(#[from] V2TransactionBuilderError),
    #[error("[taker-fee] broadcast failed: {}", sia_text(.0))]
    BroadcastTx(#[from] BroadcastTransactionError),
}

/// Errors raised while sending the maker's payment.
#[derive(Debug, Error)]
pub enum SendMakerPaymentError {
    #[error("[maker-payment] keypair fetch failed: {0}")]
    MyKeypair(#[from] SiaCoinMyKeypairError),
    #[error("[maker-payment] taker pubkey wrong length, expected 33 bytes, got: {0:?}")]
    InvalidTakerPublicKeyLength(Vec<u8>),
    #[error("[maker-payment] taker pubkey parse failed: {0}")]
    InvalidTakerPublicKey(#[from] PublicKeyError),
    #[error("[maker-payment] trade amount conversion failed: {0}")]
    SiacoinToHastings(#[from] SiacoinToHastingsError),
    #[error("[maker-payment] secret_hash parse failed: {0}")]
    ParseSecretHash(#[from] Hash256Error),
    #[error("[maker-payment] funding failed: {0}")]
    FundTx(#[from] V2TransactionBuilderError),
    #[error("[maker-payment] broadcast failed: {}", sia_text(.0))]
    BroadcastTx(#[from] BroadcastTransactionError),
}

/// Errors raised while sending the taker's payment.
#[derive(Debug, Error)]
pub enum SendTakerPaymentError {
    #[error("[taker-payment] keypair fetch failed: {0}")]
    MyKeypair(#[from] SiaCoinMyKeypairError),
    #[error("[taker-payment] maker pubkey wrong length, expected 33 bytes, got: {0:?}")]
    InvalidMakerPublicKeyLength(Vec<u8>),
    #[error("[taker-payment] maker pubkey parse failed: {0}")]
    InvalidMakerPublicKey(#[from] PublicKeyError),
    #[error("[taker-payment] trade amount conversion failed: {0}")]
    SiacoinToHastings(#[from] SiacoinToHastingsError),
    #[error("[taker-payment] secret_hash wrong length: {0}")]
    SecretHashLength(#[from] Hash256Error),
    #[error("[taker-payment] funding failed: {0}")]
    FundTx(#[from] V2TransactionBuilderError),
    #[error("[taker-payment] broadcast failed: {}", sia_text(.0))]
    BroadcastTx(#[from] BroadcastTransactionError),
}

/// Disambiguates whether an HLTC refund failure happened on the maker
/// or the taker side; the inner [`SendRefundHltcError`] carries the
/// concrete cause.
#[derive(Debug, Error)]
pub enum SendRefundHltcMakerOrTakerError {
    #[error("[refund-hltc maker] {0}")]
    Maker(SendRefundHltcError),
    #[error("[refund-hltc taker] {0}")]
    Taker(SendRefundHltcError),
}

/// Errors raised while broadcasting an HLTC refund.
#[derive(Debug, Error)]
pub enum SendRefundHltcError {
    #[error("[refund-hltc] keypair fetch failed: {0}")]
    MyKeypair(#[from] SiaCoinMyKeypairError),
    #[error("[refund-hltc] arg parse failed: {0}")]
    ParseArgs(#[from] SiaRefundPaymentArgsError),
    #[error("[refund-hltc] utxo lookup failed: {}", sia_text(.0))]
    UtxoFromTxid(#[from] Box<UtxoFromTxidError>),
    #[error("[refund-hltc] HTLC SpendPolicy unsatisfied: {0}")]
    SatisfyHtlc(#[from] V2TransactionBuilderError),
    #[error("[refund-hltc] broadcast failed: {}", sia_text(.0))]
    BroadcastTx(#[from] BroadcastTransactionError),
    #[error("[refund-hltc] HTLC value {htlc_value} is not larger than the miner fee {miner_fee}, refusing to spend")]
    HtlcBelowMinerFee { htlc_value: Currency, miner_fee: Currency },
}

/// Errors raised while the taker spends the maker's payment.
#[derive(Debug, Error)]
pub enum TakerSpendsMakerPaymentError {
    #[error("[taker-spends-maker] keypair fetch failed: {0}")]
    MyKeypair(#[from] SiaCoinMyKeypairError),
    #[error("[taker-spends-maker] maker pubkey wrong length, expected 33 bytes, got: {0:?}")]
    InvalidMakerPublicKeyLength(Vec<u8>),
    #[error("[taker-spends-maker] maker pubkey parse failed: {0}")]
    InvalidMakerPublicKey(#[from] PublicKeyError),
    #[error("[taker-spends-maker] taker_payment_tx parse failed: {0}")]
    ParseTx(#[from] SiaTransactionError),
    #[error("[taker-spends-maker] secret parse failed: {0}")]
    ParseSecret(#[from] PreimageError),
    #[error("[taker-spends-maker] secret_hash parse failed: {0}")]
    ParseSecretHash(#[from] Hash256Error),
    #[error("[taker-spends-maker] utxo lookup failed: {}", sia_text(.0))]
    UtxoFromTxid(#[from] Box<UtxoFromTxidError>),
    #[error("[taker-spends-maker] HTLC SpendPolicy unsatisfied: {0}")]
    SatisfyHtlc(#[from] V2TransactionBuilderError),
    #[error("[taker-spends-maker] broadcast failed: {}", sia_text(.0))]
    BroadcastTx(#[from] BroadcastTransactionError),
    #[error(
        "[taker-spends-maker] HTLC value {htlc_value} is not larger than the miner fee {miner_fee}, refusing to spend"
    )]
    HtlcBelowMinerFee { htlc_value: Currency, miner_fee: Currency },
}

/// Errors raised while the maker spends the taker's payment.
#[derive(Debug, Error)]
pub enum MakerSpendsTakerPaymentError {
    #[error("[maker-spends-taker] keypair fetch failed: {0}")]
    MyKeypair(#[from] SiaCoinMyKeypairError),
    #[error("[maker-spends-taker] taker pubkey wrong length, expected 33 bytes, got: {0:?}")]
    InvalidTakerPublicKeyLength(Vec<u8>),
    #[error("[maker-spends-taker] taker pubkey parse failed: {0}")]
    InvalidTakerPublicKey(#[from] PublicKeyError),
    #[error("[maker-spends-taker] taker_payment_tx parse failed: {0}")]
    ParseTx(#[from] SiaTransactionError),
    #[error("[maker-spends-taker] secret parse failed: {0}")]
    ParseSecret(#[from] PreimageError),
    #[error("[maker-spends-taker] secret_hash parse failed: {0}")]
    ParseSecretHash(#[from] Hash256Error),
    #[error("[maker-spends-taker] utxo lookup failed: {}", sia_text(.0))]
    UtxoFromTxid(#[from] Box<UtxoFromTxidError>),
    #[error("[maker-spends-taker] HTLC SpendPolicy unsatisfied: {0}")]
    SatisfyHtlc(#[from] V2TransactionBuilderError),
    #[error("[maker-spends-taker] broadcast failed: {}", sia_text(.0))]
    BroadcastTx(#[from] BroadcastTransactionError),
    #[error(
        "[maker-spends-taker] HTLC value {htlc_value} is not larger than the miner fee {miner_fee}, refusing to spend"
    )]
    HtlcBelowMinerFee { htlc_value: Currency, miner_fee: Currency },
}

// ---------- Validation ----------

/// Errors raised while validating the taker's `dex_fee` transaction.
#[derive(Debug, Error)]
pub enum ValidateFeeError {
    #[error("[validate-fee] arg parse failed: {0}")]
    ParseArgs(#[from] SiaValidateFeeArgsError),
    #[error("[validate-fee] mempool fetch failed: {}", sia_text(.0))]
    FetchMempool(#[from] GetUnconfirmedTransactionError),
    #[error("[validate-fee] tx {0} not found on chain or in mempool")]
    TxNotFound(TransactionId),
    #[error("[validate-fee] unexpected event variant: {0:?}")]
    EventVariant(Event),
    #[error("[validate-fee] tx {txid} confirmed before min_block_number {min_block_number}")]
    MininumConfirmedHeight { txid: TransactionId, min_block_number: u64 },
    #[error("[validate-fee] current_height fetch failed: {}", sia_text(.0))]
    FetchHeight(#[from] CurrentHeightError),
    #[error("[validate-fee] tx {txid} in mempool before height {min_block_number}")]
    MininumMempoolHeight { txid: TransactionId, min_block_number: u64 },
    #[error("[validate-fee] tx {0}: not all inputs originate from taker address")]
    InputsOrigin(TransactionId),
    #[error("[validate-fee] tx {txid}: {outputs_length} outputs, expected 1 or 2")]
    VoutLength { txid: TransactionId, outputs_length: usize },
    #[error("[validate-fee] tx {txid}: pays wrong address {address}")]
    InvalidFeeAddress { txid: TransactionId, address: Address },
    #[error("[validate-fee] tx {txid}: wrong amount, expected {expected}, got {actual}")]
    InvalidFeeAmount {
        txid: TransactionId,
        expected: Currency,
        actual: Currency,
    },
    #[error("[validate-fee] arbitrary_bytes uuid parse failed: {0}")]
    ParseUuid(#[from] uuid::Error),
    #[error("[validate-fee] tx {txid}: wrong uuid, expected {expected}, got {actual}")]
    InvalidUuid {
        txid: TransactionId,
        expected: Uuid,
        actual: Uuid,
    },
}

/// Generic HLTC-payment validation errors used by both maker and taker.
#[derive(Debug, Error)]
#[allow(clippy::large_enum_variant)]
pub enum SiaValidateHtlcPaymentError {
    #[error("[validate-htlc] arg parse failed: {0}")]
    ParseArgs(#[from] SiaValidatePaymentInputError),
    #[error("[validate-htlc] keypair fetch failed: {0}")]
    MyKeypair(#[from] SiaCoinMyKeypairError),
    #[error("[validate-htlc] unexpected event variant (want V2Transaction): {0:?}")]
    EventVariant(Event),
    #[error("[validate-htlc] tx {txid}: {actual} inputs, expected at least {expected}")]
    InvalidOutputLength {
        expected: u32,
        actual: u32,
        txid: TransactionId,
    },
    #[error("[validate-htlc] tx {txid}: unexpected output {actual:?}, expected {expected:?}")]
    InvalidOutput {
        expected: SiacoinOutput,
        actual: SiacoinOutput,
        txid: TransactionId,
    },
}

/// Maker-side wrapper for the generic HLTC validation error.
#[derive(Debug, Error)]
pub enum SiaValidateMakerPaymentError {
    #[error("[validate-maker-payment] {0}")]
    ValidatePayment(#[from] SiaValidateHtlcPaymentError),
}

/// Taker-side wrapper for the generic HLTC validation error.
#[derive(Debug, Error)]
pub enum SiaValidateTakerPaymentError {
    #[error("[validate-taker-payment] {0}")]
    ValidatePayment(#[from] SiaValidateHtlcPaymentError),
}

// ---------- Misc swap-side queries ----------

/// Errors raised when checking whether our payment was already sent.
#[derive(Debug, Error)]
#[allow(clippy::large_enum_variant)]
pub enum SiaCheckIfMyPaymentSentError {
    #[error("[chk-payment] arg parse failed: {0}")]
    ParseArgs(#[from] SiaCheckIfMyPaymentSentArgsError),
    #[error("[chk-payment] keypair unavailable (Iguana required): {0}")]
    MyKeypair(#[from] SiaCoinMyKeypairError),
    #[error("[chk-payment] unexpected event variant: {0:?}")]
    EventVariant(EventDataWrapper),
}

/// Errors raised while searching for the spend of an HTLC payment (CRD
/// ch.20 §20.10 D3).
#[derive(Debug, Error)]
pub enum SiaCoinSearchSwapTxSpendError {
    #[error("[search-swap-spend] keypair fetch failed: {0}")]
    MyKeypair(#[from] SiaCoinMyKeypairError),
    #[error("[search-swap-spend] other_pub wrong length, expected 33 bytes, got: {0:?}")]
    InvalidOtherPublicKeyLength(Vec<u8>),
    #[error("[search-swap-spend] other_pub parse failed: {0}")]
    ParseOtherPublicKey(#[from] PublicKeyError),
    #[error("[search-swap-spend] payment tx parse failed: {0}")]
    ParseTx(#[from] SiaTransactionError),
    #[error("[search-swap-spend] secret_hash parse failed: {0}")]
    ParseSecretHash(#[from] Hash256Error),
    #[error("[search-swap-spend] walletd event fetch failed: {0}")]
    FetchEvents(#[from] SiaHistoryFetchError),
}

/// Errors raised while extracting the HTLC preimage from a spend tx.
#[derive(Debug, Error)]
#[allow(clippy::large_enum_variant)]
pub enum SiaCoinSiaExtractSecretError {
    #[error("[extract-secret] spend_tx parse failed: {0}")]
    ParseTx(#[from] SiaTransactionError),
    #[error("[extract-secret] secret_hash parse failed: {0}")]
    ParseSecretHash(#[from] Hash256Error),
    #[error("[extract-secret] no preimage of {expected_hash} in tx: {tx}")]
    FailedToExtract { expected_hash: Hash256, tx: SiaTransaction },
}

/// Errors raised while testing whether an HTLC is refundable yet.
#[derive(Debug, Error)]
pub enum SiaCoinSiaCanRefundHtlcError {
    #[error("[can-refund] median_timestamp fetch failed: {}", sia_text(.0))]
    FetchTimestamp(#[from] GetMedianTimestampError),
}

/// Errors raised while waiting for an HTLC to be spent.
#[derive(Debug, Error)]
pub enum SiaWaitForHTLCTxSpendError {
    #[error("[wait-htlc] arg parse failed: {0}")]
    ParseArgs(#[from] SiaWaitForHTLCTxSpendArgsError),
    #[error("[wait-htlc] timed out waiting for spend of tx {txid} vout 0")]
    Timeout { txid: TransactionId },
    #[error("[wait-htlc] find_where_utxo_spent failed: {}", sia_text(.0))]
    FindWhereUtxoSpent(#[from] Box<FindWhereUtxoSpentError>),
}

// =====================================================================
// 3. Lifecycle errors
// =====================================================================

/// Errors raised while building a `SiaCoin` from configuration.
#[derive(Debug, Error)]
pub enum SiaCoinBuilderError {
    #[error("[builder] client init failed: {}", sia_text(.0))]
    Client(#[from] ClientError),
    #[error("[builder] DEX fee pubkey hex invalid: {0}")]
    FeePubkeyHex(String),
    #[error("[builder] DEX fee pubkey decode failed: {0}")]
    FeePubkey(String),
    #[error("[builder] no compiled network configuration for netid {0}")]
    UnsupportedNetId(u16),
}

/// Errors raised by `SiaCoin::new` during coin activation.
#[derive(Debug, Error)]
pub enum SiaCoinNewError {
    #[error("[new] SiaCoinConf JSON parse failed: {0}")]
    InvalidConf(#[from] serde_json::Error),
    #[error("[new] private key invalid: {0}")]
    InvalidPrivateKey(#[from] KeypairError),
    #[error("[new] PrivKeyPolicy unsupported (Iguana seed required)")]
    UnsupportedPrivKeyPolicy,
    #[error("[new] SiaCoin build failed: {0}")]
    Builder(#[from] SiaCoinBuilderError),
    #[error("[new] address derivation from master xkey failed: {0}")]
    DeriveExtendedKey(#[from] PrivKeyError),
    #[error("[new] HD wallet root derivation failed: {0}")]
    HDWalletCreation(#[from] SiaHDWalletCreationError),
}

/// Errors raised by the `my_keypair` accessor when the wallet is not
/// in Iguana keypair mode.
#[derive(Debug, Error)]
pub enum SiaCoinMyKeypairError {
    #[error("[my_keypair] PrivKeyPolicy unsupported (Iguana seed required)")]
    PrivKeyPolicy,
}

// =====================================================================
// 4. Transaction-history errors (CRD ch.53)
// =====================================================================

/// Failure projecting a walletd event onto a transaction-history record
/// (CRD ch.53 §53.5).
///
/// Mapping is otherwise total: every represented event kind yields a record,
/// and the three unrepresented kinds are skipped without error (R53.5.1).
#[derive(Debug, Error)]
pub enum SiaHistoryMapError {
    #[error("[sia-history] event {event_id}: summing {field} values overflowed u128")]
    AmountOverflow { event_id: String, field: &'static str },
}

/// Failure retrieving the wallet address's event set from walletd
/// (CRD ch.53 §53.4).
#[derive(Debug, Error)]
pub enum SiaHistoryFetchError {
    #[error("[sia-history] walletd address-events request failed: {0}")]
    Transport(String),
}

// =====================================================================
// 5. Multi-account HD wallet errors (CRD ch.20 D1)
// =====================================================================

/// Failure deriving a [`crate::siacoin::sia_hd_wallet::SiaHDWallet`]'s SLIP-10
/// ed25519 root key (`m/44'/1991'`, the node every HD account is derived from).
#[derive(Debug, Error)]
pub enum SiaHDWalletCreationError {
    #[error("[hd-wallet] failed to derive the Sia ed25519 HD root key: {0}")]
    DeriveRoot(String),
}
