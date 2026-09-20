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

use mm2_err_handle::mm_error::MmError;
use rpc::v1::types::Bytes as BytesJson;

use common::now_ms;

use crate::siacoin::{hastings_to_siacoin, siacoin_to_hastings, Address, ApiClientHelpers, Currency, SiaApiClient,
                     SiaCoin, SiaFeeDetails, SiaFeePolicy, SiaKeypair as Keypair, SiacoinElement, SiacoinOutput,
                     SpendPolicy, TxpoolFeeRequest, V2Transaction, V2TransactionBuilder};
use crate::{Json, MarketCoinOps, PrivKeyPolicy, TransactionDetails, TransactionType, WithdrawError, WithdrawRequest,
            WithdrawResult};

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

/// Result of [`SiaWithdrawBuilder::plan_inputs`]: the inputs we will
/// spend plus the derived split into recipient amount, change, total
/// input value, and the size-aware fee that was used to plan them (all
/// in hastings).
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

    /// Largest-first coin-selection: keep adding sorted UTXOs until their
    /// sum covers `recipient_amount` plus the size-aware fee of the
    /// transaction they would produce (a change output back to
    /// `self.from_address` is assumed present for sizing purposes; if the
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
        &self,
        mut candidates: Vec<SiacoinElement>,
        recipient_amount: Currency,
        fee_per_byte: Currency,
        to: &Address,
    ) -> Result<(Vec<SiacoinElement>, Currency), MmError<WithdrawError>> {
        candidates.sort_by(|a, b| b.siacoin_output.value.0.cmp(&a.siacoin_output.value.0));

        let mut chosen = Vec::new();
        let mut running_sum: u128 = 0;
        for output in candidates {
            running_sum = running_sum.saturating_add(*output.siacoin_output.value);
            chosen.push(output);

            let weight = probe_weight(&chosen, to, &self.from_address, self.key_pair, true);
            let fee = fee_for_weight(fee_per_byte, weight);
            let target: u128 = (recipient_amount + fee).into();
            if running_sum >= target {
                return Ok((chosen, fee));
            }
        }

        let weight = probe_weight(&chosen, to, &self.from_address, self.key_pair, true);
        let fee = fee_for_weight(fee_per_byte, weight);
        let target = recipient_amount + fee;
        Err(MmError::new(WithdrawError::NotSufficientBalance {
            coin: self.coin.ticker().to_string(),
            available: hastings_to_siacoin(running_sum.into()),
            required: hastings_to_siacoin(target),
        }))
    }

    /// Decide which UTXOs to spend and how to split the proceeds
    /// between the recipient, the miner fee, and change. `fee_per_byte`
    /// is walletd's current hastings-per-byte rate; the actual fee is
    /// derived from it and the planned transaction's serialized size.
    #[allow(clippy::result_large_err)]
    fn plan_inputs(
        &self,
        available: Vec<SiacoinElement>,
        fee_per_byte: Currency,
        to: &Address,
    ) -> Result<InputPlan, MmError<WithdrawError>> {
        if self.req.max {
            // `max` mode: drain every available UTXO, leaving only the fee behind.
            let input_sum: Currency = available.iter().map(|o| o.siacoin_output.value).sum();
            let weight = probe_weight(&available, to, &self.from_address, self.key_pair, false);
            let fee = fee_for_weight(fee_per_byte, weight);
            if input_sum <= fee {
                return Err(MmError::new(WithdrawError::NotSufficientBalance {
                    coin: self.coin.ticker().to_string(),
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
            siacoin_to_hastings(self.req.amount.clone()).map_err(|e| WithdrawError::InternalError(e.to_string()))?;
        let (inputs, fee) = self.pick_inputs_largest_first(available, recipient_amount, fee_per_byte, to)?;
        let input_sum: Currency = inputs.iter().map(|o| o.siacoin_output.value).sum();
        let target = recipient_amount + fee;
        Ok(InputPlan {
            inputs,
            recipient_amount,
            change_amount: input_sum - target,
            input_sum,
            fee,
        })
    }

    /// Fetch UTXOs, plan the spend, build, sign, and return the transaction.
    pub async fn build(self) -> WithdrawResult {
        let to = Address::from_str(&self.req.to).map_err(|e| WithdrawError::InvalidAddress(e.to_string()))?;

        // walletd's current hastings-per-byte rate; see `GET /api/txpool/fee`.
        let fee_per_byte = self
            .coin
            .client
            .dispatcher(TxpoolFeeRequest)
            .await
            .map_err(|e| WithdrawError::Transport(e.to_string()))?
            .0;

        let unspent = self
            .coin
            .client
            .get_unspent_outputs(&self.from_address, None, None, true)
            .await
            .map_err(|e| WithdrawError::Transport(e.to_string()))?;
        let basis = unspent.basis;

        let plan = self.plan_inputs(unspent.outputs, fee_per_byte, &to)?;

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
                    policy: SiaFeePolicy::HastingsPerByte(fee_per_byte),
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
}
