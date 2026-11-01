//! # Siacoin withdraw flow
//!
//! Builds a signed Sia v2 transaction from an Iguana-keypair wallet in
//! response to a `withdraw` RPC. The flow is:
//!
//! 1. Pull all unspent outputs for the from-address.
//! 2. Pick a coin-selection plan (either `max` — spend everything minus
//!    fee — or `specific amount` — largest-first selection).
//! 3. Assemble the transaction (recipient output, miner fee, optional
//!    change output), sign with the keypair, and return a
//!    [`TransactionDetails`] envelope for the RPC layer.
//!
//! Coin selection is deliberately simple (largest-first, no UTXO
//! consolidation heuristics) because Sia's per-input cost model is
//! flat.
//!
//! # Invariants
//! - Only `PrivKeyPolicy::KeyPair` (Iguana) is supported. HD-wallet
//!   support is gated on upstream `sia-rust` work and is intentionally
//!   not implemented here.
//! - The miner fee is size-aware: it is derived from walletd's current
//!   hastings-per-byte rate (`GET /api/txpool/fee`) multiplied by the
//!   built transaction's serialized `V2TransactionBuilder::weight()`,
//!   rather than a flat constant.

use std::str::FromStr;

use bigdecimal::BigDecimal;
use mm2_err_handle::mm_error::MmError;
use rpc::v1::types::Bytes as BytesJson;

use common::now_ms;

use crate::siacoin::sia_text;
use crate::siacoin::{hastings_to_siacoin, siacoin_to_hastings, Address, ApiClientHelpers, Currency, SiaApiClient,
                     SiaCoin, SiaFeeDetails, SiaFeePolicy, SiaKeypair as Keypair, SiacoinElement, SiacoinOutput,
                     SpendPolicy, TxpoolFeeRequest, V2Transaction, V2TransactionBuilder};
use crate::{Json, MarketCoinOps, PrivKeyPolicy, TransactionDetails, TransactionType, WithdrawError, WithdrawFee,
            WithdrawRequest, WithdrawResult};

/// Serialized size, in bytes, of a transaction shaped like the one
/// `inputs`/`with_change` describe: a single output to `to`, the given
/// inputs, and (optionally) a change output back to `from_address`.
///
/// Only the *shape* (input/output counts) matters for the byte count:
/// Sia's V2 wire encoding writes every `Currency` as a fixed-width `u128`,
/// so the placeholder `Currency::ZERO` values used here produce the exact
/// same size as the real amounts would. This reuses
/// `V2TransactionBuilder::weight()` (the v2 builder's own serialized-size
/// helper) directly rather than re-deriving a byte count independently.
fn probe_weight(
    inputs: &[SiacoinElement],
    to: &Address,
    from_address: &Address,
    key_pair: &Keypair,
    with_change: bool,
) -> u64 {
    let mut builder = V2TransactionBuilder::new().add_siacoin_output(SiacoinOutput {
        value: Currency::ZERO,
        address: to.clone(),
    });
    for input in inputs {
        builder = builder.add_siacoin_input(input.clone(), SpendPolicy::PublicKey(key_pair.public()));
    }
    if with_change {
        builder = builder.add_siacoin_output(SiacoinOutput {
            value: Currency::ZERO,
            address: from_address.clone(),
        });
    }
    builder.weight()
}

/// The miner fee for a transaction of the given serialized `weight` (bytes)
/// at walletd's current `fee_per_byte` rate.
fn fee_for_weight(fee_per_byte: Currency, weight: u64) -> Currency {
    Currency(fee_per_byte.0.saturating_mul(weight as u128))
}

/// A miner-fee rate, either walletd's per-byte estimate or the operator's
/// per-kilobyte override from the request's `fee` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeeRate {
    /// walletd's current hastings-per-byte rate (`GET /api/txpool/fee`).
    PerByte(Currency),
    /// Operator override, in hastings per kilobyte (1000 bytes).
    PerKbyte(u128),
}

impl FeeRate {
    /// The miner fee for a transaction of the given serialized `weight`
    /// (bytes). The walletd path keeps [`fee_for_weight`]'s semantics
    /// unchanged; the override computes `ceil(hastings_per_kb * weight / 1000)`
    /// and reports an overflow as a typed error instead of wrapping.
    #[allow(clippy::result_large_err)]
    fn fee_for_weight(self, ticker: &str, weight: u64) -> Result<Currency, MmError<WithdrawError>> {
        match self {
            FeeRate::PerByte(rate) => Ok(fee_for_weight(rate, weight)),
            FeeRate::PerKbyte(per_kb) => {
                let product = per_kb.checked_mul(weight as u128).ok_or_else(|| {
                    MmError::new(WithdrawError::InvalidFeePolicy(format!(
                        "The {ticker} fee of {per_kb} hastings per kilobyte overflows for a transaction of {weight} bytes"
                    )))
                })?;
                Ok(Currency(product / 1000 + u128::from(product % 1000 != 0)))
            },
        }
    }

    /// The per-byte rate reported in `fee_details.policy`. For an override this
    /// is the exact ceiling of `hastings_per_kb / 1000`; it can exceed the rate
    /// actually applied, so `fee_details.total_amount` is the authoritative fee.
    fn reported_per_byte(self) -> Currency {
        match self {
            FeeRate::PerByte(rate) => rate,
            FeeRate::PerKbyte(per_kb) => Currency(per_kb / 1000 + u128::from(per_kb % 1000 != 0)),
        }
    }
}

/// Where the fee rate comes from, decided from the request alone (no network).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeeSource {
    /// No `fee` in the request: use walletd's estimate.
    WalletdEstimate,
    /// A validated operator override; walletd's rate is not needed.
    Custom(FeeRate),
}

impl FeeSource {
    /// The rate when it is known without asking walletd, `None` when walletd's
    /// estimate must be fetched first.
    fn rate_without_network(self) -> Option<FeeRate> {
        match self {
            FeeSource::WalletdEstimate => None,
            FeeSource::Custom(rate) => Some(rate),
        }
    }
}

/// Validate the request's `fee` for Siacoin (ch.20 R-W3).
///
/// Only `UtxoPerKbyte { amount }` is accepted, with `amount` in SC per kilobyte;
/// it replaces walletd's `/api/txpool/fee` rate. There is deliberately NO floor:
/// any positive rate is accepted, including one below walletd's estimate. Whether
/// walletd or the Sia network accepts such a transaction is untested and will be
/// checked in a live run. Every other variant is rejected rather than ignored.
#[allow(clippy::result_large_err)]
fn resolve_fee_source(ticker: &str, fee: Option<&WithdrawFee>) -> Result<FeeSource, MmError<WithdrawError>> {
    let invalid = |msg: String| MmError::new(WithdrawError::InvalidFeePolicy(msg));
    let amount = match fee {
        None => return Ok(FeeSource::WalletdEstimate),
        Some(WithdrawFee::UtxoPerKbyte { amount }) => amount,
        Some(other) => {
            return Err(invalid(format!(
                "Expected 'UtxoPerKbyte' fee type for {ticker}, found {other:?}"
            )))
        },
    };
    if amount <= &BigDecimal::from(0) {
        return Err(invalid(format!(
            "The {ticker} fee must be a positive amount of SC per kilobyte, found {amount}"
        )));
    }
    let hastings = siacoin_to_hastings(amount.clone()).map_err(|e| {
        invalid(format!(
            "The {ticker} fee of {amount} SC per kilobyte is not representable: {e}"
        ))
    })?;
    // `siacoin_to_hastings` truncates; require an exact conversion. This also
    // rejects a rate that would truncate to 0 hastings per kilobyte.
    if hastings_to_siacoin(hastings) != *amount {
        return Err(invalid(format!(
            "The {ticker} fee of {amount} SC per kilobyte is not a whole number of hastings"
        )));
    }
    Ok(FeeSource::Custom(FeeRate::PerKbyte(hastings.0)))
}

/// Pure fee decision: request `fee` + walletd's rate (if fetched) + serialized
/// `weight` -> miner fee in hastings. Needs no network.
#[allow(clippy::result_large_err)]
#[cfg_attr(not(test), allow(dead_code))]
fn compute_fee(
    ticker: &str,
    fee: Option<&WithdrawFee>,
    walletd_rate: Option<Currency>,
    weight: u64,
) -> Result<Currency, MmError<WithdrawError>> {
    let source = resolve_fee_source(ticker, fee)?;
    let rate = match (source.rate_without_network(), walletd_rate) {
        (Some(rate), _) => rate,
        (None, Some(rate)) => FeeRate::PerByte(rate),
        (None, None) => {
            return Err(MmError::new(WithdrawError::InternalError(
                "walletd's fee rate is required when the request carries no fee".to_string(),
            )))
        },
    };
    rate.fee_for_weight(ticker, weight)
}

/// `a + b` that saturates instead of overflowing.
fn saturating_add(a: Currency, b: Currency) -> Currency { Currency(a.0.saturating_add(b.0)) }

/// Result of [`plan_inputs`]: the inputs we will
/// spend plus the derived split into recipient amount, change, total
/// input value, and the size-aware fee that was used to plan them (all
/// in hastings).
#[derive(Debug)]
struct InputPlan {
    /// UTXOs that will be consumed as inputs.
    inputs: Vec<SiacoinElement>,
    /// Amount to send to the recipient.
    recipient_amount: Currency,
    /// Change returned to the from-address (`ZERO` when `max == true`).
    change_amount: Currency,
    /// Sum of `inputs.value` — equals `recipient_amount + change + fee`.
    input_sum: Currency,
    /// Miner fee, computed from the planned transaction's serialized size.
    fee: Currency,
}

/// What the pure planners need from the builder.
struct PlanEnv<'a> {
    ticker: &'a str,
    from_address: &'a Address,
    key_pair: &'a Keypair,
    to: &'a Address,
}

/// Largest-first coin-selection: keep adding sorted UTXOs until their
/// sum covers `recipient_amount` plus the size-aware fee of the
/// transaction they would produce (a change output back to
/// `env.from_address` is assumed present for sizing purposes; if the
/// final leftover turns out to be exactly zero, `plan_inputs` simply
/// omits the change output and the fee is paid as computed here).
///
/// Returns the chosen inputs together with the fee that was used to
/// select them.
///
/// # Errors
/// - [`WithdrawError::NotSufficientBalance`] when the *total* of the
///   provided outputs is below what's needed (caller has already
///   converted `recipient_amount` to hastings).
#[allow(clippy::result_large_err)]
fn pick_inputs_largest_first(
    env: &PlanEnv<'_>,
    mut candidates: Vec<SiacoinElement>,
    recipient_amount: Currency,
    fee_rate: FeeRate,
) -> Result<(Vec<SiacoinElement>, Currency), MmError<WithdrawError>> {
    candidates.sort_by(|a, b| b.siacoin_output.value.0.cmp(&a.siacoin_output.value.0));

    let mut chosen = Vec::new();
    let mut running_sum: u128 = 0;
    for output in candidates {
        running_sum = running_sum.saturating_add(*output.siacoin_output.value);
        chosen.push(output);

        let weight = probe_weight(&chosen, env.to, env.from_address, env.key_pair, true);
        let fee = fee_rate.fee_for_weight(env.ticker, weight)?;
        let target = saturating_add(recipient_amount, fee).0;
        if running_sum >= target {
            return Ok((chosen, fee));
        }
    }

    let weight = probe_weight(&chosen, env.to, env.from_address, env.key_pair, true);
    let fee = fee_rate.fee_for_weight(env.ticker, weight)?;
    let target = saturating_add(recipient_amount, fee);
    Err(MmError::new(WithdrawError::NotSufficientBalance {
        coin: env.ticker.to_string(),
        available: hastings_to_siacoin(running_sum.into()),
        required: hastings_to_siacoin(target),
    }))
}

/// Decide which UTXOs to spend and how to split the proceeds
/// between the recipient, the miner fee, and change. `fee_rate` is either
/// walletd's hastings-per-byte rate or the operator's per-kilobyte override;
/// the actual fee is derived from it and the planned transaction's
/// serialized size.
#[allow(clippy::result_large_err)]
fn plan_inputs(
    env: &PlanEnv<'_>,
    max: bool,
    amount: &BigDecimal,
    available: Vec<SiacoinElement>,
    fee_rate: FeeRate,
) -> Result<InputPlan, MmError<WithdrawError>> {
    if max {
        // `max` mode: drain every available UTXO, leaving only the fee behind.
        let input_sum: Currency = available.iter().map(|o| o.siacoin_output.value).sum();
        let weight = probe_weight(&available, env.to, env.from_address, env.key_pair, false);
        let fee = fee_rate.fee_for_weight(env.ticker, weight)?;
        if input_sum <= fee {
            return Err(MmError::new(WithdrawError::NotSufficientBalance {
                coin: env.ticker.to_string(),
                available: hastings_to_siacoin(input_sum),
                required: hastings_to_siacoin(fee),
            }));
        }
        return Ok(InputPlan {
            recipient_amount: input_sum - fee,
            inputs: available,
            change_amount: Currency::ZERO,
            input_sum,
            fee,
        });
    }

    // Specific-amount mode: take only what's needed plus fee, return change.
    let recipient_amount =
        siacoin_to_hastings(amount.clone()).map_err(|e| WithdrawError::InternalError(e.to_string()))?;
    let (inputs, fee) = pick_inputs_largest_first(env, available, recipient_amount, fee_rate)?;
    let input_sum: Currency = inputs.iter().map(|o| o.siacoin_output.value).sum();
    let target = saturating_add(recipient_amount, fee);
    Ok(InputPlan {
        inputs,
        recipient_amount,
        change_amount: input_sum - target,
        input_sum,
        fee,
    })
}

/// Assembles, signs, and returns a Sia withdraw transaction.
pub struct SiaWithdrawBuilder<'a> {
    coin: &'a SiaCoin,
    req: WithdrawRequest,
    from_address: Address,
    key_pair: &'a Keypair,
}

impl<'a> SiaWithdrawBuilder<'a> {
    /// Construct a builder by extracting the Iguana keypair and its
    /// derived address from `coin`.
    ///
    /// # Errors
    /// - [`WithdrawError::InternalError`] when the wallet is not in
    ///   `PrivKeyPolicy::KeyPair` mode (HD/Trezor are not supported).
    #[allow(clippy::result_large_err)]
    pub fn new(coin: &'a SiaCoin, req: WithdrawRequest) -> Result<Self, MmError<WithdrawError>> {
        let (key_pair, from_address) = match &*coin.priv_key_policy {
            PrivKeyPolicy::KeyPair(kp) => (kp, kp.public().address()),
            _ => {
                return Err(WithdrawError::InternalError(
                    "Only Iguana keypair is supported for Sia coin for now!".to_string(),
                )
                .into())
            },
        };

        Ok(SiaWithdrawBuilder {
            coin,
            req,
            from_address,
            key_pair,
        })
    }

    /// Fetch UTXOs, plan the spend, build, sign, and return the transaction.
    pub async fn build(self) -> WithdrawResult {
        let to = Address::from_str(&self.req.to).map_err(|e| WithdrawError::InvalidAddress(e.to_string()))?;

        // Validate the request's fee before any network call; a custom fee
        // does not need walletd's rate.
        let fee_source = resolve_fee_source(self.coin.ticker(), self.req.fee.as_ref())?;
        let fee_rate = match fee_source.rate_without_network() {
            Some(rate) => rate,
            // walletd's current hastings-per-byte rate; see `GET /api/txpool/fee`.
            None => FeeRate::PerByte(
                self.coin
                    .client
                    .dispatcher(TxpoolFeeRequest)
                    .await
                    .map_err(|e| WithdrawError::Transport(sia_text(&e)))?
                    .0,
            ),
        };

        let unspent = self
            .coin
            .client
            .get_unspent_outputs(&self.from_address, None, None, true)
            .await
            .map_err(|e| WithdrawError::Transport(sia_text(&e)))?;
        let basis = unspent.basis;

        let ticker = self.coin.ticker().to_string();
        let env = PlanEnv {
            ticker: &ticker,
            from_address: &self.from_address,
            key_pair: self.key_pair,
            to: &to,
        };
        let plan = plan_inputs(&env, self.req.max, &self.req.amount, unspent.outputs, fee_rate)?;

        let mut tx = V2TransactionBuilder::new()
            .update_basis(basis)
            .add_siacoin_output(SiacoinOutput {
                value: plan.recipient_amount,
                address: to.clone(),
            })
            .miner_fee(plan.fee);
        for input in plan.inputs {
            tx = tx.add_siacoin_input(input, SpendPolicy::PublicKey(self.key_pair.public()));
        }
        if plan.change_amount > Currency::ZERO {
            tx = tx.add_siacoin_output(SiacoinOutput {
                value: plan.change_amount,
                address: self.from_address.clone(),
            });
        }
        let signed = tx.sign_simple(vec![self.key_pair]).build();

        // SC-denominated views, matching the RPC TransactionDetails contract.
        let spent = hastings_to_siacoin(plan.input_sum);
        let fee_sc = hastings_to_siacoin(plan.fee);
        let received_back = hastings_to_siacoin(plan.change_amount);

        let (tx_bytes, tx_json) = transaction_carriers(&signed)?;
        let txid = signed.txid();
        let tx_hash = txid.to_string();

        Ok(TransactionDetails {
            tx_hex: BytesJson(tx_bytes),
            tx_json: Some(tx_json),
            tx_hash,
            from: vec![self.from_address.to_string()],
            to: vec![self.req.to.clone()],
            total_amount: spent.clone() - fee_sc.clone(),
            spent_by_me: spent.clone(),
            received_by_me: received_back.clone(),
            my_balance_change: received_back - spent,
            fee_details: Some(
                SiaFeeDetails {
                    coin: self.coin.ticker().to_string(),
                    policy: SiaFeePolicy::HastingsPerByte(fee_rate.reported_per_byte()),
                    total_amount: fee_sc,
                }
                .into(),
            ),
            block_height: 0,
            coin: self.coin.ticker().to_string(),
            // The raw id bytes whose hex form is this record's own `tx_hash`
            // (ch.20 R-W10). The history path keys its records the same way
            // (ch.53 R53.5.2), so a withdrawal and the history record that
            // later appears for the same transaction share one primary key.
            internal_id: BytesJson(txid.0.to_vec()),
            timestamp: now_ms() / 1000,
            kmd_rewards: None,
            // What the record *is* -- a Sia v2 transaction -- not which
            // subsystem produced it (ch.20 R-W9, ch.53 R53.5.10). The history
            // path reports the same value for the same transaction.
            transaction_type: TransactionType::SiaV2Transaction,
        })
    }
}

/// Derive the two transaction carriers ch.20 R-W6/R-W7 bind, from one signed
/// transaction.
///
/// Sia is the coin family whose native serialisation of a signed transaction is
/// JSON text rather than a binary encoding, so the carriers are two encodings of
/// one byte sequence: `tx_hex` is its lowercase hex (R-W6) and `tx_json` the same
/// JSON emitted unencoded (R-W7). Returning `tx_json` by parsing the very bytes
/// `tx_hex` will encode -- rather than serialising `signed` a second time -- is
/// what makes R-W7's "never two different transactions" hold by construction.
///
/// # Errors
///
/// A serialisation failure aborts the withdrawal, per R-W6: a completed
/// withdrawal's `tx_hex` is mandatory and every broadcast path reads it, so
/// reporting success with an empty or placeholder carrier would hand the caller
/// a withdrawal it cannot broadcast.
fn transaction_carriers(signed: &V2Transaction) -> Result<(Vec<u8>, Json), MmError<WithdrawError>> {
    let tx_bytes = serde_json::ser::to_vec(signed)
        .map_err(|e| WithdrawError::InternalError(format!("Failed to serialize the signed transaction: {e}")))?;
    let tx_json = serde_json::from_slice(&tx_bytes)
        .map_err(|e| WithdrawError::InternalError(format!("Failed to reparse the serialized transaction: {e}")))?;
    Ok((tx_bytes, tx_json))
}

#[cfg(test)]
mod tests {
    //! Regression surface for `probe_weight`/`fee_for_weight` (CRD ch.20
    //! §20.10 D6). These are the pure, size-aware fee-estimation primitives
    //! `pick_inputs_largest_first`/`plan_inputs` build on; the coin-selection
    //! methods themselves need a live `SiaCoin` (its production `SiaClient`
    //! pings walletd on construction) and so aren't exercised directly here.

    use super::*;

    const ADDR: &str = "c34caa97740668de2bbdb7174572ed64c861342bf27e80313cbfa02e9251f52e30aad3892533";

    fn test_keypair() -> Keypair {
        Keypair::from_private_bytes(&[7u8; 32]).expect("32 bytes is a valid ed25519 secret key")
    }

    fn test_address() -> Address { Address::from_str(ADDR).expect("valid address") }

    /// A well-formed `SiacoinElement` with the given output id and value;
    /// the merkle proof/leaf index are never inspected by `probe_weight`.
    fn siacoin_element(output_id: &str, value: u128) -> SiacoinElement {
        let json = json!({
            "id": output_id,
            "stateElement": { "leafIndex": 3, "merkleProof": [] },
            "siacoinOutput": { "value": value.to_string(), "address": ADDR },
            "maturityHeight": 0,
        });
        serde_json::from_value(json).expect("valid siacoin element")
    }

    /// Sia's V2 wire encoding writes every `Currency` as a fixed-width
    /// `u128` (never a variable-length integer), so `probe_weight`'s
    /// placeholder-valued outputs must report the exact same size
    /// regardless of the real amounts involved.
    #[test]
    fn probe_weight_is_independent_of_output_values() {
        let key_pair = test_keypair();
        let to = test_address();
        let input = siacoin_element("11".repeat(32).as_str(), 1);

        let small = probe_weight(&[input.clone()], &to, &to, &key_pair, false);
        let big_input = siacoin_element("11".repeat(32).as_str(), u128::MAX);
        let big = probe_weight(&[big_input], &to, &to, &key_pair, false);

        assert_eq!(small, big);
    }

    /// Every additional input contributes the same number of bytes (just
    /// the parent output id -- see `V2TransactionBuilder`'s `Encodable`
    /// impl, which encodes only `parent.id` per input, not the satisfied
    /// policy). This is the property `pick_inputs_largest_first` relies on
    /// to keep its target re-computation exact rather than approximate.
    #[test]
    fn probe_weight_grows_by_a_fixed_amount_per_input() {
        let key_pair = test_keypair();
        let to = test_address();
        let one = vec![siacoin_element("11".repeat(32).as_str(), 1)];
        let two = vec![
            siacoin_element("11".repeat(32).as_str(), 1),
            siacoin_element("22".repeat(32).as_str(), 2),
        ];
        let three = vec![
            siacoin_element("11".repeat(32).as_str(), 1),
            siacoin_element("22".repeat(32).as_str(), 2),
            siacoin_element("33".repeat(32).as_str(), 3),
        ];

        let w0 = probe_weight(&[], &to, &to, &key_pair, false);
        let w1 = probe_weight(&one, &to, &to, &key_pair, false);
        let w2 = probe_weight(&two, &to, &to, &key_pair, false);
        let w3 = probe_weight(&three, &to, &to, &key_pair, false);

        let delta = w1 - w0;
        assert!(delta > 0);
        assert_eq!(w2 - w1, delta);
        assert_eq!(w3 - w2, delta);
    }

    /// Including the change output adds a fixed number of bytes on top,
    /// independent of the input count.
    #[test]
    fn probe_weight_change_output_adds_a_fixed_amount() {
        let key_pair = test_keypair();
        let to = test_address();
        let one = vec![siacoin_element("11".repeat(32).as_str(), 1)];
        let two = vec![
            siacoin_element("11".repeat(32).as_str(), 1),
            siacoin_element("22".repeat(32).as_str(), 2),
        ];

        let with_change_delta =
            probe_weight(&one, &to, &to, &key_pair, true) - probe_weight(&one, &to, &to, &key_pair, false);
        let with_change_delta_2 =
            probe_weight(&two, &to, &to, &key_pair, true) - probe_weight(&two, &to, &to, &key_pair, false);

        assert!(with_change_delta > 0);
        assert_eq!(with_change_delta, with_change_delta_2);
    }

    #[test]
    fn fee_for_weight_multiplies_rate_by_size() {
        assert_eq!(fee_for_weight(Currency(3), 1000), Currency(3_000));
        assert_eq!(fee_for_weight(Currency::ZERO, 1000), Currency::ZERO);
    }

    /// Saturates instead of overflow-panicking on pathological inputs.
    #[test]
    fn fee_for_weight_saturates_on_overflow() {
        assert_eq!(fee_for_weight(Currency(u128::MAX), u64::MAX), Currency(u128::MAX));
    }

    /// T-W1: the two carriers a withdrawal returns are two encodings of one
    /// transaction. Hex-decoding `tx_hex` and parsing the result as JSON yields
    /// the same JSON value as `tx_json` -- never two different transactions.
    #[test]
    fn both_carriers_encode_the_same_transaction() {
        let signed = V2TransactionBuilder::new().build();
        let (tx_bytes, tx_json) = transaction_carriers(&signed).expect("a built transaction serializes");

        let from_hex: Json = serde_json::from_slice(&hex::decode(hex::encode(&tx_bytes)).expect("valid hex"))
            .expect("the decoded bytes are the transaction's own JSON");
        assert_eq!(from_hex, tx_json);
    }

    /// R-W6: the bytes `tx_hex` encodes are the bound library's own
    /// serialisation, so they parse straight back into an equivalent
    /// transaction -- which is exactly what the broadcast path does with them.
    #[test]
    fn the_hex_carrier_decodes_back_into_the_same_transaction() {
        let signed = V2TransactionBuilder::new().build();
        let (tx_bytes, _) = transaction_carriers(&signed).expect("a built transaction serializes");

        let recovered: V2Transaction = serde_json::from_slice(&tx_bytes).expect("round-trips through its own form");
        assert_eq!(recovered.txid().to_string(), signed.txid().to_string());
    }

    /// R-W10: `internal_id` carries the raw id bytes whose lowercase hex form
    /// the same record reports as `tx_hash`, so a withdrawal record and the
    /// history record for the same transaction share one primary key
    /// (ch.53 R53.5.2). Guards the relationship between the two fields, which
    /// is the part a caller joins on.
    #[test]
    fn the_record_id_is_the_raw_form_of_the_reported_hash() {
        let txid = V2TransactionBuilder::new().build().txid();

        assert_eq!(hex::encode(txid.0), txid.to_string());
    }

    // ---- operator fee override (ch.20 R-W3) ----

    use std::str::FromStr as _;

    const TICKER: &str = "SC";

    fn per_kb(amount: &str) -> WithdrawFee {
        WithdrawFee::UtxoPerKbyte {
            amount: BigDecimal::from_str(amount).expect("decimal"),
        }
    }

    fn env<'a>(to: &'a Address, kp: &'a Keypair) -> PlanEnv<'a> {
        PlanEnv {
            ticker: TICKER,
            from_address: to,
            key_pair: kp,
            to,
        }
    }

    fn id(n: u8) -> String { format!("{n:02x}").repeat(32) }

    fn bd(s: &str) -> BigDecimal { BigDecimal::from_str(s).expect("decimal") }

    fn expect_invalid_policy(err: MmError<WithdrawError>) -> String {
        match err.into_inner() {
            WithdrawError::InvalidFeePolicy(msg) => msg,
            other => panic!("expected InvalidFeePolicy, got {other:?}"),
        }
    }

    #[test]
    fn wallet_fee_json_deserializes() {
        let fee: WithdrawFee =
            serde_json::from_str(r#"{"type":"UtxoPerKbyte","amount":"0.001"}"#).expect("wallet fee object");
        assert_eq!(fee, per_kb("0.001"));
    }

    #[test]
    fn withdraw_request_json_with_and_without_fee() {
        let with: WithdrawRequest = serde_json::from_value(json!({
            "coin": "SC", "to": ADDR, "amount": "1.5",
            "fee": { "type": "UtxoPerKbyte", "amount": "0.001" }
        }))
        .expect("request with fee");
        assert_eq!(with.fee, Some(per_kb("0.001")));
        assert!(!with.max);

        let without: WithdrawRequest =
            serde_json::from_value(json!({ "coin": "SC", "to": ADDR, "amount": "1.5" })).expect("request without fee");
        assert_eq!(without.fee, None);
    }

    #[test]
    fn per_kbyte_fee_rounds_up() {
        // 1 SC/kB = 1e24 hastings/kB; 1000 bytes divides evenly.
        let fee = per_kb("1");
        assert_eq!(
            compute_fee(TICKER, Some(&fee), None, 1000).unwrap(),
            Currency(1_000_000_000_000_000_000_000_000)
        );
        // 1 hasting/kB (1e-24 SC) at 1500 bytes: 1500/1000 = 1.5 -> 2.
        let tiny = per_kb("0.000000000000000000000001");
        assert_eq!(compute_fee(TICKER, Some(&tiny), None, 1500).unwrap(), Currency(2));
        assert_eq!(compute_fee(TICKER, Some(&tiny), None, 1000).unwrap(), Currency(1));
        assert_eq!(compute_fee(TICKER, Some(&tiny), None, 1001).unwrap(), Currency(2));
    }

    #[test]
    fn absent_fee_uses_walletd_rate_exactly_as_before() {
        for weight in [0u64, 1, 999, 1234] {
            assert_eq!(
                compute_fee(TICKER, None, Some(Currency(7)), weight).unwrap(),
                fee_for_weight(Currency(7), weight)
            );
        }
        // No walletd rate and no custom fee is an error, not a zero fee.
        assert!(compute_fee(TICKER, None, None, 100).is_err());
    }

    #[test]
    fn custom_fee_ignores_walletd_rate_and_has_no_floor() {
        // A custom rate far below walletd's is accepted and applied.
        let fee = per_kb("0.000000000000000000000001");
        let with_high = compute_fee(TICKER, Some(&fee), Some(Currency(1_000_000)), 2000).unwrap();
        let without = compute_fee(TICKER, Some(&fee), None, 2000).unwrap();
        assert_eq!(with_high, Currency(2));
        assert_eq!(without, Currency(2));
    }

    #[test]
    fn custom_fee_needs_no_network_but_absent_fee_does() {
        let src = resolve_fee_source(TICKER, Some(&per_kb("0.001"))).unwrap();
        assert!(src.rate_without_network().is_some());
        let src = resolve_fee_source(TICKER, None).unwrap();
        assert_eq!(src, FeeSource::WalletdEstimate);
        assert!(src.rate_without_network().is_none());
    }

    #[test]
    fn invalid_amounts_are_rejected() {
        for bad in ["0", "-0.001", "-1"] {
            let msg = expect_invalid_policy(resolve_fee_source(TICKER, Some(&per_kb(bad))).unwrap_err());
            assert!(msg.contains("SC") && msg.contains("positive"), "{msg}");
        }
        // More than 24 decimals cannot be an exact number of hastings.
        let msg = expect_invalid_policy(
            resolve_fee_source(TICKER, Some(&per_kb("0.0000000000000000000000015"))).unwrap_err(),
        );
        assert!(msg.contains("whole number of hastings"), "{msg}");
        // Below one hasting per kB.
        expect_invalid_policy(resolve_fee_source(TICKER, Some(&per_kb("0.000000000000000000000000001"))).unwrap_err());
        // Does not fit in u128 hastings.
        expect_invalid_policy(
            resolve_fee_source(TICKER, Some(&per_kb("1000000000000000000000000000000000000000"))).unwrap_err(),
        );
    }

    #[test]
    fn other_fee_variants_are_rejected_naming_the_accepted_type() {
        let others = [
            WithdrawFee::UtxoFixed { amount: bd("0.1") },
            WithdrawFee::EthGas {
                gas_price: bd("1"),
                gas: 21000,
            },
            WithdrawFee::Qrc20Gas {
                gas_limit: 1,
                gas_price: 1,
            },
        ];
        for fee in &others {
            let msg = expect_invalid_policy(resolve_fee_source(TICKER, Some(fee)).unwrap_err());
            assert!(
                msg.starts_with("Expected 'UtxoPerKbyte' fee type for SC, found "),
                "{msg}"
            );
        }
    }

    #[test]
    fn rate_times_weight_overflow_is_a_typed_error() {
        let err = FeeRate::PerKbyte(u128::MAX).fee_for_weight(TICKER, 2).unwrap_err();
        assert!(expect_invalid_policy(err).contains("overflows"));
    }

    #[test]
    fn reported_rate_is_ceiling_of_per_kb_over_1000() {
        assert_eq!(FeeRate::PerKbyte(1000).reported_per_byte(), Currency(1));
        assert_eq!(FeeRate::PerKbyte(1001).reported_per_byte(), Currency(2));
        assert_eq!(FeeRate::PerKbyte(1).reported_per_byte(), Currency(1));
        assert_eq!(FeeRate::PerByte(Currency(5)).reported_per_byte(), Currency(5));
    }

    /// Four equal UTXOs; a rate that makes the fee exceed one UTXO forces more
    /// inputs, and the fee is recomputed from the larger probe weight.
    #[test]
    fn custom_rate_changes_selection_and_fee_like_the_estimate_path() {
        let kp = test_keypair();
        let to = test_address();
        let e = env(&to, &kp);
        let utxos = |v: u128| (1..=4u8).map(|n| siacoin_element(&id(n), v)).collect::<Vec<_>>();

        let w1 = probe_weight(&utxos(1)[..1], &to, &to, &kp, true);
        let w2 = probe_weight(&utxos(1)[..2], &to, &to, &kp, true);
        assert!(w2 > w1);

        // 1 hasting/byte equivalent: 1000 hastings/kB.
        let rate = FeeRate::PerKbyte(1000);
        let value = 1_000_000u128;
        let amount_h = value - w1 as u128 + 1; // one UTXO is short by 1 hasting
        let amount = hastings_to_siacoin(Currency(amount_h));
        let plan = plan_inputs(&e, false, &amount, utxos(value), rate).unwrap();
        assert_eq!(plan.inputs.len(), 2);
        assert_eq!(plan.fee, Currency(w2 as u128));
        assert_eq!(plan.input_sum, Currency(2 * value));
        assert_eq!(plan.change_amount, Currency(2 * value - amount_h - w2 as u128));

        // Same inputs with the equivalent walletd per-byte rate: identical plan.
        let est = plan_inputs(&e, false, &amount, utxos(value), FeeRate::PerByte(Currency(1))).unwrap();
        assert_eq!(est.inputs.len(), plan.inputs.len());
        assert_eq!(est.fee, plan.fee);
        assert_eq!(est.change_amount, plan.change_amount);

        // A lower custom rate keeps the single input.
        let low = plan_inputs(&e, false, &amount, utxos(value), FeeRate::PerKbyte(1)).unwrap();
        assert_eq!(low.inputs.len(), 1);
    }

    #[test]
    fn max_mode_uses_no_change_weight_and_checks_value() {
        let kp = test_keypair();
        let to = test_address();
        let e = env(&to, &kp);
        let utxos = vec![siacoin_element(&id(1), 10_000), siacoin_element(&id(2), 10_000)];
        let w = probe_weight(&utxos, &to, &to, &kp, false);
        let rate = FeeRate::PerKbyte(2000); // 2 per byte
        let plan = plan_inputs(&e, true, &BigDecimal::from(0), utxos.clone(), rate).unwrap();
        assert_eq!(plan.fee, Currency(2 * w as u128));
        assert_eq!(plan.recipient_amount, Currency(20_000 - 2 * w as u128));
        assert_eq!(plan.change_amount, Currency::ZERO);

        // Fee >= input sum: not enough.
        let err = plan_inputs(
            &e,
            true,
            &BigDecimal::from(0),
            utxos,
            FeeRate::PerKbyte(u128::from(1_000_000u32)),
        )
        .unwrap_err();
        assert!(matches!(err.into_inner(), WithdrawError::NotSufficientBalance { .. }));
    }

    #[test]
    fn insufficient_balance_requires_amount_plus_custom_fee() {
        let kp = test_keypair();
        let to = test_address();
        let e = env(&to, &kp);
        let utxos = vec![siacoin_element(&id(1), 1_000)];
        let w = probe_weight(&utxos, &to, &to, &kp, true);
        let rate = FeeRate::PerKbyte(1000);
        let amount = hastings_to_siacoin(Currency(900));
        match plan_inputs(&e, false, &amount, utxos, rate).unwrap_err().into_inner() {
            WithdrawError::NotSufficientBalance {
                coin,
                available,
                required,
            } => {
                assert_eq!(coin, "SC");
                assert_eq!(available, hastings_to_siacoin(Currency(1_000)));
                assert_eq!(required, hastings_to_siacoin(Currency(900 + w as u128)));
            },
            other => panic!("expected NotSufficientBalance, got {other:?}"),
        }
    }

    #[test]
    fn absent_fee_plan_matches_walletd_rate_plan() {
        let kp = test_keypair();
        let to = test_address();
        let e = env(&to, &kp);
        let utxos = vec![siacoin_element(&id(1), 5_000_000), siacoin_element(&id(2), 7_000_000)];
        let source = resolve_fee_source(TICKER, None).unwrap();
        assert_eq!(source.rate_without_network(), None);
        let amount = hastings_to_siacoin(Currency(6_000_000));
        let plan = plan_inputs(&e, false, &amount, utxos.clone(), FeeRate::PerByte(Currency(3))).unwrap();
        // Hand-computed with the pre-existing primitives.
        let chosen_weight = probe_weight(&[utxos[1].clone()], &to, &to, &kp, true);
        assert_eq!(plan.inputs.len(), 1);
        assert_eq!(plan.fee, fee_for_weight(Currency(3), chosen_weight));
    }
}
