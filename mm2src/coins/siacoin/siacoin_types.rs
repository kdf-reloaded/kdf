// siacoin_types — Structs, enums, constants, and conversion utilities for Siacoin.

use super::*;

lazy_static! {
    pub static ref SINGLE_ADDRESS_MODE_PATH: DalekDerivationPath =
        DalekDerivationPath::from_str("m/44'/1991'/0'/0'/0'").expect("Valid single address mode path");
}

/// Default HD gap limit for a `SiaHDWallet` built at activation (CRD ch.20 D1) when
/// the activation request's `gap_limit` field is absent. Matches UTXO's own
/// `DEFAULT_GAP_LIMIT` (`utxo.rs`) and `sia_hd_wallet.rs`'s own test fixtures.
pub(crate) const DEFAULT_HD_GAP_LIMIT: u32 = 20;

/// The index of the HTLC output in the transaction that locks the funds
pub(crate) const HTLC_VOUT_INDEX: u32 = 0;

// ── Config types ─────────────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize)]
pub struct SiaCoinConf {
    #[serde(rename = "coin")]
    pub ticker: String,
    pub required_confirmations: u64,
}

#[derive(Clone, Deserialize)]
pub struct SiaCoinActivationRequest {
    #[serde(default)]
    pub tx_history: bool,
    pub required_confirmations: Option<u64>,
    pub gap_limit: Option<u32>,
    pub client_conf: SiaClientConf,
}

/// A debug view of the walletd client configuration that shows the server in host-only form and no secret:
/// the external library's own debug form prints the password and the browser build's header values (CRD ch.20
/// R-N2, ch.35 R35.1.17). The configuration type is defined by the external Sia library, so its own `Debug`
/// is never used for it.
pub struct RedactedClientConf<'a>(pub &'a SiaClientConf);

impl fmt::Debug for RedactedClientConf<'_> {
    #[cfg(not(target_arch = "wasm32"))]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conf")
            .field("server_url", &crate::node_url::host_only(self.0.server_url.as_str()))
            .field(
                "password",
                &self.0.password.as_ref().map(|_| crate::node_url::SECRET_SET),
            )
            .field("timeout", &self.0.timeout)
            .finish()
    }

    #[cfg(target_arch = "wasm32")]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut header_names: Vec<&String> = self.0.headers.keys().collect();
        header_names.sort();
        f.debug_struct("Conf")
            .field("server_url", &crate::node_url::host_only(self.0.server_url.as_str()))
            .field("headers", &header_names)
            .finish()
    }
}

/// The debug form redacts the client configuration (CRD ch.20 R-N2).
impl fmt::Debug for SiaCoinActivationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SiaCoinActivationRequest")
            .field("tx_history", &self.tx_history)
            .field("required_confirmations", &self.required_confirmations)
            .field("gap_limit", &self.gap_limit)
            .field("client_conf", &RedactedClientConf(&self.client_conf))
            .finish()
    }
}

#[derive(Debug, Display)]
pub enum SiaCoinFromLegacyReqErr {
    InvalidRequiredConfs(serde_json::Error),
    InvalidGapLimit(serde_json::Error),
    InvalidClientConf(serde_json::Error),
}

impl SiaCoinActivationRequest {
    pub fn from_legacy_req(req: &Json) -> Result<Self, MmError<SiaCoinFromLegacyReqErr>> {
        let tx_history = req["tx_history"].as_bool().unwrap_or_default();
        let required_confirmations = serde_json::from_value(req["required_confirmations"].clone())
            .map_to_mm(SiaCoinFromLegacyReqErr::InvalidRequiredConfs)?;
        let gap_limit =
            serde_json::from_value(req["gap_limit"].clone()).map_to_mm(SiaCoinFromLegacyReqErr::InvalidGapLimit)?;
        let client_conf =
            serde_json::from_value(req["client_conf"].clone()).map_to_mm(SiaCoinFromLegacyReqErr::InvalidClientConf)?;

        Ok(SiaCoinActivationRequest {
            tx_history,
            required_confirmations,
            gap_limit,
            client_conf,
        })
    }
}

// ── Protocol / fee types ─────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SiaCoinProtocolInfo;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum SiaFeePolicy {
    Fixed,
    HastingsPerByte(Currency),
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SiaFeeDetails {
    pub coin: String,
    pub policy: SiaFeePolicy,
    pub total_amount: BigDecimal,
}

// From impl for SiaFeeDetails -> TxFeeDetails is in lp_coins.rs

// ── Conversion utilities ─────────────────────────────────────────────

/// Convert hastings representation to "coin" amount
pub(crate) fn hastings_to_siacoin(hastings: Currency) -> BigDecimal {
    let hastings: u128 = hastings.into();
    let divisor: BigDecimal = "1000000000000000000000000".parse().expect("valid decimal");
    let hastings_bd: BigDecimal = hastings.to_string().parse().expect("u128 is valid BigDecimal");
    hastings_bd / divisor
}

/// CRD ch.20 R-S11, ch.60 R60.6.4: the value of the single output of an HTLC spend or refund, that is the HTLC
/// value minus the fixed miner fee. `None` when the fee is not smaller than the HTLC value; such a spend is
/// refused before anything is built or broadcast.
pub(crate) fn htlc_value_after_miner_fee(htlc_value: Currency, miner_fee: Currency) -> Option<Currency> {
    if miner_fee >= htlc_value {
        return None;
    }
    Some(htlc_value - miner_fee)
}

/// CRD ch.60 R60.6.7 item 2: the Sia safe amount, the fixed HTLC miner fee plus one hasting.
pub(crate) fn htlc_safe_amount() -> BigDecimal { hastings_to_siacoin(Currency::DEFAULT_FEE + Currency::from(1u64)) }

/// Convert "coin" representation to hastings amount
pub(crate) fn siacoin_to_hastings(siacoin: BigDecimal) -> Result<Currency, SiacoinToHastingsError> {
    let multiplier: BigDecimal = "1000000000000000000000000".parse().expect("valid decimal");
    let hastings = siacoin.clone() * multiplier;
    // Parse as string to get u128
    let hastings_str = hastings.with_scale(0).to_string();
    hastings_str
        .parse::<u128>()
        .map_err(|_| SiacoinToHastingsError::BigDecimalToU128(siacoin))
        .map(Currency)
}

// ── SiaTransaction ───────────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, From, Into)]
#[serde(transparent)]
pub struct SiaTransaction(pub V2Transaction);

impl fmt::Display for SiaTransaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match serde_json::to_string(self) {
            Ok(json) => write!(f, "{}", json),
            Err(err) => write!(f, "Failed to serialize SiaTransaction:{:?} to JSON: {}", self, err),
        }
    }
}

impl SiaTransaction {
    pub fn txid(&self) -> Hash256 { self.0.txid() }
}

impl TryFrom<SiaTransaction> for Vec<u8> {
    type Error = SiaTransactionError;

    fn try_from(tx: SiaTransaction) -> Result<Self, Self::Error> {
        serde_json::ser::to_vec(&tx).map_err(SiaTransactionError::ToVec)
    }
}

impl TryFrom<&[u8]> for SiaTransaction {
    type Error = SiaTransactionError;

    fn try_from(tx_slice: &[u8]) -> Result<Self, Self::Error> {
        serde_json::de::from_slice(tx_slice).map_err(SiaTransactionError::FromVec)
    }
}

impl TryFrom<Vec<u8>> for SiaTransaction {
    type Error = SiaTransactionError;

    fn try_from(tx: Vec<u8>) -> Result<Self, Self::Error> {
        serde_json::de::from_slice(&tx).map_err(SiaTransactionError::FromVec)
    }
}

impl Transaction for SiaTransaction {
    fn tx_hex(&self) -> Vec<u8> { serde_json::ser::to_vec(self).unwrap_or_default() }

    fn tx_hash(&self) -> BytesJson { BytesJson(self.txid().0.to_vec()) }
}

// ── SiaTransactionTypes ──────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SiaTransactionTypes {
    V1Transaction(V1Transaction),
    V2Transaction(V2Transaction),
    EventPayout(EventPayout),
}

// ── Internal arg conversion types ────────────────────────────────────

/// Sia typed ValidateFeeArgs
#[derive(Clone, Debug)]
pub(crate) struct SiaValidateFeeArgs {
    pub fee_tx: SiaTransaction,
    pub taker_public_key: PublicKey,
    pub dex_fee_amount: Currency,
    pub min_block_number: u64,
    pub uuid: Uuid,
}

impl<'a> TryFrom<ValidateFeeArgs<'a>> for SiaValidateFeeArgs {
    type Error = SiaValidateFeeArgsError;

    fn try_from(args: ValidateFeeArgs<'a>) -> Result<Self, Self::Error> {
        let fee_tx = match args.fee_tx {
            TransactionEnum::SiaTransaction(tx) => tx.clone(),
            _ => return Err(SiaValidateFeeArgsError::TxEnumVariant),
        };

        if args.expected_sender.len() != 33 {
            return Err(SiaValidateFeeArgsError::InvalidTakerPublicKeyLength(
                args.expected_sender.to_vec(),
            ));
        }

        let expected_sender_public_key = PublicKey::from_bytes(&args.expected_sender[..32])?;

        let dex_fee_amount = match args.dex_fee {
            DexFee::Standard(mm_num) => siacoin_to_hastings(BigDecimal::from(mm_num.clone()))?,
            other => return Err(SiaValidateFeeArgsError::DexFeeVariant(other.to_string())),
        };

        let uuid = Uuid::from_slice(args.uuid)?;

        match uuid.get_version_num() {
            4 => (),
            version => return Err(SiaValidateFeeArgsError::UuidVersion(version)),
        }

        Ok(SiaValidateFeeArgs {
            fee_tx,
            taker_public_key: expected_sender_public_key,
            dex_fee_amount,
            min_block_number: args.min_block_number,
            uuid,
        })
    }
}

/// Sia typed RefundPaymentArgs (adapted for fork's positional params)
pub(crate) struct SiaRefundPaymentArgs {
    pub payment_tx: SiaTransaction,
    pub time_lock: u64,
    pub success_public_key: PublicKey,
    pub secret_hash: Hash256,
}

impl SiaRefundPaymentArgs {
    pub fn try_from_positional(
        payment_tx_bytes: &[u8],
        time_lock: u32,
        other_pubkey: &[u8],
        secret_hash: &[u8],
    ) -> Result<Self, SiaRefundPaymentArgsError> {
        let payment_tx = SiaTransaction::try_from(payment_tx_bytes.to_vec())?;

        if other_pubkey.len() != 33 {
            return Err(SiaRefundPaymentArgsError::InvalidOtherPublicKeyLength(
                other_pubkey.to_vec(),
            ));
        }
        let success_public_key = PublicKey::from_bytes(&other_pubkey[..32])?;

        let secret_hash = Hash256::try_from(secret_hash)?;

        Ok(SiaRefundPaymentArgs {
            payment_tx,
            time_lock: time_lock as u64,
            success_public_key,
            secret_hash,
        })
    }
}

/// Sia typed CheckIfMyPaymentSentArgs (adapted for positional params)
pub(crate) struct SiaCheckIfMyPaymentSentArgs {
    pub time_lock: u64,
    pub success_public_key: PublicKey,
    pub secret_hash: Hash256,
    #[allow(dead_code)]
    pub amount: Currency,
}

impl SiaCheckIfMyPaymentSentArgs {
    pub fn try_from_positional(
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
    ) -> Result<Self, SiaCheckIfMyPaymentSentArgsError> {
        if other_pub.len() != 33 {
            return Err(SiaCheckIfMyPaymentSentArgsError::InvalidOtherPublicKeyLength(
                other_pub.to_vec(),
            ));
        }
        let success_public_key = PublicKey::from_bytes(&other_pub[..32])?;
        if secret_hash.len() != 32 {
            return Err(SiaCheckIfMyPaymentSentArgsError::WrongSecretHashLength {
                actual: secret_hash.len(),
            });
        }
        let secret_hash = Hash256::try_from(secret_hash)?;
        let amount = siacoin_to_hastings(amount)?;

        Ok(SiaCheckIfMyPaymentSentArgs {
            time_lock: time_lock as u64,
            success_public_key,
            secret_hash,
            amount,
        })
    }
}

/// `ValidatePaymentInput.taker_pub`/`.maker_pub` are always the real taker's
/// and real maker's keys -- never "mine" vs "other" (both `validate_maker_payment`
/// and `validate_taker_payment` share the same struct and populate it that way;
/// see maker_swap.rs's and taker_swap.rs's construction of it). So the pubkey
/// a Sia HTLC validation needs as its refund/counterparty key depends on which
/// side's payment is being validated, not on a fixed field -- the caller must
/// say which, since `SiaValidatePaymentInputArgs` alone can't tell.
pub(crate) enum ValidatingPaymentOwner {
    /// We are the taker, validating the maker's payment (SwapOps::validate_maker_payment).
    Maker,
    /// We are the maker, validating the taker's payment (SwapOps::validate_taker_payment).
    Taker,
}

/// Sia typed ValidatePaymentInput
#[derive(Clone, Debug)]
pub(crate) struct SiaValidatePaymentInputArgs {
    pub payment_tx: SiaTransaction,
    pub time_lock: u64,
    pub other_pub: PublicKey,
    pub secret_hash: Hash256,
    pub amount: Currency,
}

impl SiaValidatePaymentInputArgs {
    pub fn try_from_validate_payment_input(
        args: ValidatePaymentInput,
        payment_owner: ValidatingPaymentOwner,
    ) -> Result<Self, SiaValidatePaymentInputError> {
        let payment_tx = SiaTransaction::try_from(args.payment_tx.to_vec())?;

        // Whichever side's payment we were asked to validate names the
        // counterparty pubkey directly: validating the maker's payment means
        // *we* are the taker and the maker is the counterparty (maker_pub),
        // and vice versa. Using the wrong field here silently validates a
        // payment against our own key instead of the counterparty's,
        // producing a spend-policy address that can never match what the
        // real counterparty actually paid to (ch.51 R63).
        let other_pub_bytes = match payment_owner {
            ValidatingPaymentOwner::Maker => &args.maker_pub,
            ValidatingPaymentOwner::Taker => &args.taker_pub,
        };
        if other_pub_bytes.len() != 33 {
            return Err(SiaValidatePaymentInputError::InvalidOtherPublicKeyLength(
                other_pub_bytes.clone(),
            ));
        }
        let other_pub = PublicKey::from_bytes(&other_pub_bytes[..32])?;

        if args.secret_hash.len() != 32 {
            return Err(SiaValidatePaymentInputError::WrongSecretHashLength {
                actual: args.secret_hash.len(),
            });
        }
        let secret_hash = Hash256::try_from(args.secret_hash.as_slice())?;
        let amount = siacoin_to_hastings(args.amount)?;

        Ok(SiaValidatePaymentInputArgs {
            payment_tx,
            time_lock: args.time_lock as u64,
            other_pub,
            secret_hash,
            amount,
        })
    }
}

// ── Debug impl ───────────────────────────────────────────────────────

impl fmt::Debug for SiaCoinGeneric<SiaClient> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "SiaCoin({})", self.conf.ticker) }
}
