//! SwapOps implementation for TendermintCoin.
//!
//! Provides atomic swap operations (HTLC create, claim, validate) for
//! Cosmos/Tendermint chains.  The `_for_denom` helpers are `pub(super)` so
//! `TendermintToken` can reuse them with a different denom.

use super::htlc::{ClaimHtlcMsg, ClaimHtlcProto, CreateHtlcMsg, CreateHtlcProto, HtlcType, HTLC_STATE_COMPLETED,
                  HTLC_STATE_OPEN, HTLC_STATE_REFUNDED};
use super::rpc::*;
use super::tendermint_helpers::TendermintCommons;
use super::tendermint_types::*;
use crate::utxo::sat_from_big_decimal;
use crate::{DexFee, DexFeeBurnDestination, FoundSwapTxSpend, MarketCoinOps, NegotiateSwapContractAddrErr, SwapOps,
            TransactionEnum, TransactionErr, TransactionFut, ValidateFeeArgs, ValidatePaymentInput};
use bigdecimal::BigDecimal;
use common::{drop_mutability, now_ms};
use cosmrs::proto::cosmos::bank::v1beta1::{Input as InputProto, MsgMultiSend as MsgMultiSendProto,
                                           MsgSend as MsgSendProto, Output as OutputProto};
use cosmrs::proto::cosmos::base::v1beta1::Coin as CoinProto;
use cosmrs::proto::cosmos::tx::v1beta1::{TxBody, TxRaw};
use cosmrs::proto::prost::Message;
use cosmrs::{AccountId, Any, Coin, Denom};
use crypto::privkey::key_pair_from_secret;
use futures::compat::Future01CompatExt;
use futures::{FutureExt, TryFutureExt};
use kdf_crypto::dhash160;
use keys::KeyPair;
use mm2_err_handle::prelude::*;
use rpc::v1::types::Bytes as BytesJson;
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;

const MSG_SEND_TYPE_URL: &str = "/cosmos.bank.v1beta1.MsgSend";
const MSG_MULTI_SEND_TYPE_URL: &str = "/cosmos.bank.v1beta1.MsgMultiSend";

/// The length of a swap secret in bytes (CRD ch.18 R18.11.14).
const SECRET_LEN: usize = 32;

/// The answer of `check_if_my_payment_sent` of the Tendermint coin and token (CRD ch.18 R18.11.22).
pub(super) const PAYMENT_SENT_NEEDS_AMOUNT: &str =
    "The Tendermint payment lookup needs the payment amount: use check_if_my_payment_sent_for_amount";

// ————————————————————————————————————————————————————————————————
// Denom-parameterised helpers (pub(super) for token reuse)
// ————————————————————————————————————————————————————————————————

impl TendermintCoin {
    /// The account that receives the burn part of a burn-form DEX fee: the bech32 of `dhash160` of the burn
    /// pubkey, with the coin's prefix. The fee sender and the fee validator derive it the same way.
    fn burn_account_id(&self, burn_destination: &DexFeeBurnDestination) -> Result<AccountId, String> {
        let burn_pubkey = match burn_destination {
            DexFeeBurnDestination::PreBurnAccount { burn_pubkey } => burn_pubkey,
            _ => return Err("KmdOpReturn not supported on Tendermint".to_string()),
        };
        let burn_pubkey_hash = dhash160(burn_pubkey);
        AccountId::new(&self.protocol_info.account_prefix, burn_pubkey_hash.as_slice()).map_err(|e| e.to_string())
    }

    /// Broadcast a taker DEX-fee transaction (MsgSend or MsgMultiSend).
    pub(super) fn send_taker_fee_for_denom(
        &self,
        dex_fee: &DexFee,
        denom: Denom,
        decimals: u8,
        fee_addr: &[u8],
        uuid: &[u8],
    ) -> TransactionFut {
        let memo = try_tx_fus!(Uuid::from_slice(uuid)).to_string();
        let from_address = self.account_id.clone();

        let fee_pubkey_hash = dhash160(fee_addr);
        let fee_address = try_tx_fus!(AccountId::new(
            &self.protocol_info.account_prefix,
            fee_pubkey_hash.as_slice()
        ));

        let fee_amount_dec: BigDecimal = dex_fee.fee_amount().into();
        let fee_amount_sat = try_tx_fus!(sat_from_big_decimal(&fee_amount_dec, decimals));

        let tx_payload = match dex_fee {
            DexFee::NoFee => try_tx_fus!(Err::<Any, String>("Unexpected DexFee::NoFee".into())),
            DexFee::Standard(_) => {
                let msg = MsgSendProto {
                    from_address: from_address.to_string(),
                    to_address: fee_address.to_string(),
                    amount: vec![CoinProto {
                        denom: denom.to_string(),
                        amount: fee_amount_sat.to_string(),
                    }],
                };
                Any {
                    type_url: MSG_SEND_TYPE_URL.to_string(),
                    value: msg.encode_to_vec(),
                }
            },
            DexFee::WithBurn {
                burn_amount,
                burn_destination,
                ..
            } => {
                let burn_address = try_tx_fus!(self.burn_account_id(burn_destination));
                let burn_dec: BigDecimal = burn_amount.clone().into();
                let burn_sat = try_tx_fus!(sat_from_big_decimal(&burn_dec, decimals));
                let total_sat = fee_amount_sat + burn_sat;

                let msg = MsgMultiSendProto {
                    inputs: vec![InputProto {
                        address: from_address.to_string(),
                        coins: vec![CoinProto {
                            denom: denom.to_string(),
                            amount: total_sat.to_string(),
                        }],
                    }],
                    outputs: vec![
                        OutputProto {
                            address: fee_address.to_string(),
                            coins: vec![CoinProto {
                                denom: denom.to_string(),
                                amount: fee_amount_sat.to_string(),
                            }],
                        },
                        OutputProto {
                            address: burn_address.to_string(),
                            coins: vec![CoinProto {
                                denom: denom.to_string(),
                                amount: burn_sat.to_string(),
                            }],
                        },
                    ],
                };
                Any {
                    type_url: MSG_MULTI_SEND_TYPE_URL.to_string(),
                    value: msg.encode_to_vec(),
                }
            },
        };

        let coin = self.clone();
        let fut = async move {
            let current_block = try_tx_s!(coin.current_block().compat().await);
            let timeout_height = current_block + TIMEOUT_HEIGHT_DELTA;

            let fee = try_tx_s!(
                coin.calculate_fee(tx_payload.clone(), timeout_height, TX_DEFAULT_MEMO, None)
                    .await
            );

            let (_tx_id, tx_raw) = try_tx_s!(
                coin.common_send_raw_tx_bytes(tx_payload, fee, timeout_height, &memo, Duration::from_secs(30),)
                    .await
            );

            Ok(TransactionEnum::CosmosTransaction(CosmosTransaction {
                data: tx_raw.into(),
            }))
        };

        Box::new(fut.boxed().compat())
    }

    /// Broadcast an HTLC-create transaction for a swap payment.
    pub(super) fn send_htlc_for_denom(
        &self,
        time_lock_duration: u64,
        other_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        denom: Denom,
        decimals: u8,
    ) -> TransactionFut {
        let pubkey_hash = dhash160(other_pub);
        let to = try_tx_fus!(AccountId::new(
            &self.protocol_info.account_prefix,
            pubkey_hash.as_slice()
        ));

        let amount_sat = try_tx_fus!(sat_from_big_decimal(&amount, decimals));
        let amount_u = cosmrs::Amount::from(amount_sat as u64);

        let secret_hash = secret_hash.to_vec();
        let coin = self.clone();
        let fut = async move {
            let time_lock = coin.estimate_blocks_from_duration(time_lock_duration);

            let create_htlc_tx =
                try_tx_s!(coin.gen_create_htlc_tx(denom, &to, amount_u, &secret_hash, time_lock as u64));

            let current_block = try_tx_s!(coin.current_block().compat().await);
            let timeout_height = current_block + TIMEOUT_HEIGHT_DELTA;

            let fee = try_tx_s!(
                coin.calculate_fee(
                    create_htlc_tx.msg_payload.clone(),
                    timeout_height,
                    TX_DEFAULT_MEMO,
                    None,
                )
                .await
            );

            let (_tx_id, tx_raw) = try_tx_s!(
                coin.common_send_raw_tx_bytes(
                    create_htlc_tx.msg_payload,
                    fee,
                    timeout_height,
                    TX_DEFAULT_MEMO,
                    Duration::from_secs(time_lock_duration),
                )
                .await
            );

            Ok(TransactionEnum::CosmosTransaction(CosmosTransaction {
                data: tx_raw.into(),
            }))
        };

        Box::new(fut.boxed().compat())
    }

    /// Claim an HTLC (shared helper for maker-spends-taker and taker-spends-maker).
    pub(super) fn spend_htlc(&self, payment_tx: &[u8], secret: &[u8], secret_hash: &[u8]) -> TransactionFut {
        let tx = try_tx_fus!(cosmrs::Tx::from_bytes(payment_tx));
        let msg = try_tx_fus!(tx.body.messages.first().ok_or("Tx body couldn't be read."));

        let htlc_type = try_tx_fus!(HtlcType::from_str(&self.protocol_info.account_prefix));
        let htlc_proto = try_tx_fus!(CreateHtlcProto::decode(htlc_type, msg.value.as_slice()));
        let htlc = try_tx_fus!(CreateHtlcMsg::try_from(htlc_proto));

        let mut amount = htlc.amount().to_vec();
        amount.sort();
        drop_mutability!(amount);

        let htlc_id = self.calculate_htlc_id(htlc.sender(), htlc.to(), &amount, secret_hash);
        let claim_htlc_tx = try_tx_fus!(self.gen_claim_htlc_tx(htlc_id, secret));

        let coin = self.clone();
        let fut = async move {
            let current_block = try_tx_s!(coin.current_block().compat().await);
            let timeout_height = current_block + TIMEOUT_HEIGHT_DELTA;

            let fee = try_tx_s!(
                coin.calculate_fee(claim_htlc_tx.msg_payload.clone(), timeout_height, TX_DEFAULT_MEMO, None,)
                    .await
            );

            let (_tx_id, tx_raw) = try_tx_s!(
                coin.common_send_raw_tx_bytes(
                    claim_htlc_tx.msg_payload,
                    fee,
                    timeout_height,
                    TX_DEFAULT_MEMO,
                    Duration::from_secs(120),
                )
                .await
            );

            Ok(TransactionEnum::CosmosTransaction(CosmosTransaction {
                data: tx_raw.into(),
            }))
        };

        Box::new(fut.boxed().compat())
    }

    /// Validate a taker DEX-fee transaction for a given denom (CRD ch.18 R18.11.21).
    ///
    /// The memo is the swap uuid; the body has exactly one message; the message is the standard `MsgSend` or the
    /// burn-form `MsgMultiSend` with the exact recipients, sender and single-coin amounts; and the node holds a
    /// copy of the transaction that is byte-equal to the supplied one.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn validate_fee_for_denom(
        &self,
        fee_tx: &TransactionEnum,
        expected_sender: &[u8],
        fee_addr: &[u8],
        dex_fee: &DexFee,
        decimals: u8,
        uuid: &[u8],
        denom: String,
    ) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        let tx = match fee_tx {
            TransactionEnum::CosmosTransaction(tx) => tx.clone(),
            other => return Box::new(futures01::future::err(format!("Unexpected tx variant: {:?}", other))),
        };

        let uuid_str = match Uuid::from_slice(uuid) {
            Ok(u) => u.to_string(),
            Err(e) => return Box::new(futures01::future::err(format!("{}", e))),
        };

        let sender_hash = dhash160(expected_sender);
        let expected_sender_addr = try_fus!(AccountId::new(
            &self.protocol_info.account_prefix,
            sender_hash.as_slice()
        ));
        let fee_hash = dhash160(fee_addr);
        let expected_fee_addr = try_fus!(AccountId::new(&self.protocol_info.account_prefix, fee_hash.as_slice()));

        let fee_amount_dec: BigDecimal = dex_fee.fee_amount().into();
        let fee_amount_sat = try_fus!(sat_from_big_decimal(&fee_amount_dec, decimals));
        let fee_coin = CoinProto {
            denom: denom.clone(),
            amount: fee_amount_sat.to_string(),
        };

        // The burn-form descriptor also names the burn account and the burn coin; the standard one has neither.
        let burn = match dex_fee {
            DexFee::NoFee => return Box::new(futures01::future::err("Unexpected DexFee::NoFee".to_string())),
            DexFee::Standard(_) => None,
            DexFee::WithBurn {
                burn_amount,
                burn_destination,
                ..
            } => {
                let burn_address = try_fus!(self.burn_account_id(burn_destination));
                let burn_dec: BigDecimal = burn_amount.clone().into();
                let burn_sat = try_fus!(sat_from_big_decimal(&burn_dec, decimals));
                let burn_coin = CoinProto {
                    denom,
                    amount: burn_sat.to_string(),
                };
                Some((burn_address, burn_coin))
            },
        };

        let coin = self.clone();
        let fut = async move {
            let tx_body = try_s!(TxBody::decode(tx.data.body_bytes.as_slice()));

            if tx_body.memo != uuid_str {
                return Err(format!("Invalid memo: expected {}, found {}", uuid_str, tx_body.memo));
            }

            let msg = match tx_body.messages.as_slice() {
                [msg] => msg,
                messages => {
                    return Err(format!(
                        "Invalid number of messages in the fee tx: {}, expected exactly 1",
                        messages.len()
                    ))
                },
            };

            match &burn {
                None => check_standard_fee_msg(
                    &msg.type_url,
                    msg.value.as_slice(),
                    &expected_sender_addr,
                    &expected_fee_addr,
                    &fee_coin,
                )?,
                Some((burn_address, burn_coin)) => check_burn_fee_msg(
                    &msg.type_url,
                    msg.value.as_slice(),
                    &expected_sender_addr,
                    &expected_fee_addr,
                    &fee_coin,
                    burn_address,
                    burn_coin,
                )?,
            }

            // The node must hold the very transaction that was supplied (its hash is the SHA-256 of the raw
            // bytes, upper-case hex). The node's copy is a decoded transaction; its encoding is the raw form.
            let tx_bytes = tx.data.encode_to_vec();
            let tx_hash = hex::encode_upper(kdf_crypto::sha256(&tx_bytes).as_slice());
            let node_tx = coin.request_tx(tx_hash.clone()).await.map_err(|e| {
                format!(
                    "The fee tx {} cannot be fetched from the node: {}",
                    tx_hash,
                    e.into_inner()
                )
            })?;
            if node_tx.encode_to_vec() != tx_bytes {
                return Err(format!("The fee tx {} differs from the copy held by the node", tx_hash));
            }

            Ok(())
        };

        Box::new(fut.boxed().compat())
    }

    /// Validate an HTLC payment (maker or taker) for a given denom.
    ///
    /// `sender_pubkey` is the raw compressed pubkey of the payment creator.
    pub(super) fn validate_payment_for_denom(
        &self,
        input: ValidatePaymentInput,
        sender_pubkey: &[u8],
        denom: Denom,
        decimals: u8,
    ) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        let sender_hash = dhash160(sender_pubkey);
        let expected_sender = try_fus!(AccountId::new(
            &self.protocol_info.account_prefix,
            sender_hash.as_slice()
        ));
        let my_account_id = self.account_id.clone();
        let account_prefix = self.protocol_info.account_prefix.clone();
        let fut = async move {
            let tx = try_s!(cosmrs::Tx::from_bytes(&input.payment_tx));
            let msg = try_s!(tx.body.messages.first().ok_or("Tx body couldn't be read."));

            let htlc_type = try_s!(HtlcType::from_str(&account_prefix));
            let htlc_proto = try_s!(CreateHtlcProto::decode(htlc_type, msg.value.as_slice()));

            // Check secret hash before consuming the proto.
            let expected_hash = hex::encode(&input.secret_hash).to_uppercase();
            if htlc_proto.hash_lock().to_uppercase() != expected_hash {
                return Err(format!(
                    "Invalid secret hash: {}, expected {}",
                    htlc_proto.hash_lock(),
                    expected_hash
                ));
            }

            let htlc = try_s!(CreateHtlcMsg::try_from(htlc_proto));

            // Verify sender.
            if htlc.sender() != &expected_sender {
                return Err(format!(
                    "HTLC sender {} does not match expected {}",
                    htlc.sender(),
                    expected_sender
                ));
            }

            // Verify receiver is our address.
            if htlc.to() != &my_account_id {
                return Err(format!(
                    "HTLC receiver {} does not match our address {}",
                    htlc.to(),
                    my_account_id
                ));
            }

            // Verify amount.
            let expected_sat = try_s!(sat_from_big_decimal(&input.amount, decimals));
            let expected_coin = Coin {
                denom,
                amount: (expected_sat as u64).into(),
            };
            if !htlc
                .amount()
                .iter()
                .any(|c| c.denom == expected_coin.denom && c.amount == expected_coin.amount)
            {
                return Err(format!(
                    "Invalid HTLC amount: {:?}, expected {:?}",
                    htlc.amount(),
                    expected_coin
                ));
            }

            Ok(())
        };

        Box::new(fut.boxed().compat())
    }

    /// Look up the HTLC we sent for a swap (CRD ch.18 R18.11.22).
    ///
    /// The HTLC id is computed from our own account, the counterparty account, the single expected coin and the
    /// secret hash, so the amount and the counterparty take part in it. The module is asked for that id: no HTLC
    /// means the payment was not sent. Otherwise the create transaction is searched by the id, and accepted when
    /// its hash lock equals the one the module recorded (without regard to the letter case).
    pub(super) fn check_if_my_payment_sent_for_denom(
        &self,
        other_pub: &[u8],
        secret_hash: &[u8],
        amount: &BigDecimal,
        denom: Denom,
        decimals: u8,
    ) -> Box<dyn futures01::Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        let account_prefix = self.protocol_info.account_prefix.clone();
        let other_hash = dhash160(other_pub);
        let to = try_fus!(AccountId::new(&account_prefix, other_hash.as_slice()));
        let expected_coin = Coin {
            denom,
            amount: try_fus!(sat_from_big_decimal(amount, decimals)).into(),
        };
        let htlc_id = self.calculate_htlc_id(&self.account_id, &to, &[expected_coin], secret_hash);

        let coin = self.clone();
        let fut = async move {
            let htlc_type = try_s!(HtlcType::from_str(&account_prefix));

            let response = try_s!(coin.query_htlc(htlc_id.clone()).await);
            let (state, recorded_hash_lock) = match (response.htlc_state(), response.hash_lock()) {
                (Some(state), Some(hash_lock)) => (state, hash_lock.to_owned()),
                // The module has no HTLC under this id: the payment was not sent.
                _ => return Ok(None),
            };
            match state {
                HTLC_STATE_OPEN | HTLC_STATE_COMPLETED | HTLC_STATE_REFUNDED => {},
                unexpected => return Err(format!("Unexpected state {} of the HTLC {}", unexpected, htlc_id)),
            }

            let rpc_client = try_s!(coin.rpc_client().await);
            let response = try_s!(
                rpc_client
                    .perform(TxSearchRequest {
                        query: format!("create_htlc.id='{htlc_id}'"),
                        order_by: TendermintResultOrder::Descending.into(),
                        page: 1,
                        per_page: 1,
                        prove: false,
                    })
                    .await
            );
            let raw_tx = match response.txs.first() {
                Some(raw_tx) => raw_tx,
                None => return Err(format!("The create tx of the HTLC {} is not found", htlc_id)),
            };
            if let cosmrs::tendermint::abci::Code::Err(code) = raw_tx.tx_result.code {
                return Err(format!(
                    "The create tx of the HTLC {} failed with the result code {}",
                    htlc_id, code
                ));
            }

            let tx = try_s!(cosmrs::Tx::from_bytes(&raw_tx.tx));
            let msg = try_s!(tx.body.messages.first().ok_or("Tx body couldn't be read."));
            let htlc_proto = try_s!(CreateHtlcProto::decode(htlc_type, msg.value.as_slice()));
            if !htlc_proto.hash_lock().eq_ignore_ascii_case(&recorded_hash_lock) {
                return Err(format!(
                    "The hash lock {} of the create tx of the HTLC {} differs from the recorded {}",
                    htlc_proto.hash_lock(),
                    htlc_id,
                    recorded_hash_lock
                ));
            }

            Ok(Some(TransactionEnum::CosmosTransaction(CosmosTransaction {
                data: try_s!(TxRaw::decode(raw_tx.tx.as_slice())),
            })))
        };

        Box::new(fut.boxed().compat())
    }
}

/// The standard descriptor of a DEX fee: a `MsgSend` of exactly one coin, from the taker to the fee address
/// (CRD ch.18 R18.11.21).
fn check_standard_fee_msg(
    type_url: &str,
    value: &[u8],
    from: &AccountId,
    fee_address: &AccountId,
    fee_coin: &CoinProto,
) -> Result<(), String> {
    if type_url != MSG_SEND_TYPE_URL {
        return Err(format!(
            "Invalid fee message type: {}, expected {}",
            type_url, MSG_SEND_TYPE_URL
        ));
    }
    let msg = try_s!(MsgSendProto::decode(value));
    if msg.from_address != from.to_string() {
        return Err(format!("Invalid sender: {}, expected {}", msg.from_address, from));
    }
    if msg.to_address != fee_address.to_string() {
        return Err(format!(
            "Invalid receiver: {}, expected {}",
            msg.to_address, fee_address
        ));
    }
    if msg.amount.as_slice() != std::slice::from_ref(fee_coin) {
        return Err(format!(
            "Invalid fee amount: {:?}, expected exactly one coin {:?}",
            msg.amount, fee_coin
        ));
    }
    Ok(())
}

/// The burn descriptor of a DEX fee: a `MsgMultiSend` with one input (the taker) and two outputs, the fee address
/// with exactly the fee coin, then the burn address with exactly the burn coin (CRD ch.18 R18.11.21).
fn check_burn_fee_msg(
    type_url: &str,
    value: &[u8],
    from: &AccountId,
    fee_address: &AccountId,
    fee_coin: &CoinProto,
    burn_address: &AccountId,
    burn_coin: &CoinProto,
) -> Result<(), String> {
    if type_url != MSG_MULTI_SEND_TYPE_URL {
        return Err(format!(
            "Invalid fee message type: {}, expected {}",
            type_url, MSG_MULTI_SEND_TYPE_URL
        ));
    }
    let msg = try_s!(MsgMultiSendProto::decode(value));
    let input = match msg.inputs.as_slice() {
        [input] => input,
        inputs => {
            return Err(format!(
                "Invalid number of inputs: {}, expected exactly 1",
                inputs.len()
            ))
        },
    };
    let (fee_output, burn_output) = match msg.outputs.as_slice() {
        [fee_output, burn_output] => (fee_output, burn_output),
        outputs => {
            return Err(format!(
                "Invalid number of outputs: {}, expected exactly 2",
                outputs.len()
            ))
        },
    };
    if input.address != from.to_string() {
        return Err(format!("Invalid sender: {}, expected {}", input.address, from));
    }
    if fee_output.address != fee_address.to_string() {
        return Err(format!(
            "Invalid fee output address: {}, expected {}",
            fee_output.address, fee_address
        ));
    }
    if fee_output.coins.as_slice() != std::slice::from_ref(fee_coin) {
        return Err(format!(
            "Invalid fee output amount: {:?}, expected exactly one coin {:?}",
            fee_output.coins, fee_coin
        ));
    }
    if burn_output.address != burn_address.to_string() {
        return Err(format!(
            "Invalid burn output address: {}, expected {}",
            burn_output.address, burn_address
        ));
    }
    if burn_output.coins.as_slice() != std::slice::from_ref(burn_coin) {
        return Err(format!(
            "Invalid burn output amount: {:?}, expected exactly one coin {:?}",
            burn_output.coins, burn_coin
        ));
    }
    Ok(())
}

// ————————————————————————————————————————————————————————————————
// SwapOps trait implementation
// ————————————————————————————————————————————————————————————————

#[async_trait::async_trait]
impl SwapOps for TendermintCoin {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], uuid: &[u8]) -> TransactionFut {
        self.send_taker_fee_for_denom(
            dex_fee,
            self.protocol_info.denom.clone(),
            self.protocol_info.decimals,
            fee_addr,
            uuid,
        )
    }

    fn send_maker_payment(
        &self,
        time_lock: u32,
        _maker_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let now_sec = (now_ms() / 1000) as u32;
        let duration = time_lock.saturating_sub(now_sec) as u64;
        self.send_htlc_for_denom(
            duration,
            taker_pub,
            secret_hash,
            amount,
            self.protocol_info.denom.clone(),
            self.protocol_info.decimals,
        )
    }

    fn send_taker_payment(
        &self,
        time_lock: u32,
        _taker_pub: &[u8],
        maker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let now_sec = (now_ms() / 1000) as u32;
        let duration = time_lock.saturating_sub(now_sec) as u64;
        self.send_htlc_for_denom(
            duration,
            maker_pub,
            secret_hash,
            amount,
            self.protocol_info.denom.clone(),
            self.protocol_info.decimals,
        )
    }

    fn send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        _time_lock: u32,
        _taker_pub: &[u8],
        secret: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let secret_hash = kdf_crypto::sha256(secret);
        self.spend_htlc(taker_payment_tx, secret, secret_hash.as_slice())
    }

    fn send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        _time_lock: u32,
        _maker_pub: &[u8],
        secret: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let secret_hash = kdf_crypto::sha256(secret);
        self.spend_htlc(maker_payment_tx, secret, secret_hash.as_slice())
    }

    fn send_taker_refunds_payment(
        &self,
        _taker_payment_tx: &[u8],
        _time_lock: u32,
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        // IRIS/Nucleus HTLCs auto-refund on chain expiry; no broadcast needed.
        Box::new(futures01::future::err(TransactionErr::Plain(
            "Tendermint HTLCs auto-refund on chain; no broadcast required.".into(),
        )))
    }

    fn send_maker_refunds_payment(
        &self,
        _maker_payment_tx: &[u8],
        _time_lock: u32,
        _taker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(
            "Tendermint HTLCs auto-refund on chain; no broadcast required.".into(),
        )))
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        self.validate_fee_for_denom(
            args.fee_tx,
            args.expected_sender,
            args.fee_addr,
            args.dex_fee,
            self.protocol_info.decimals,
            args.uuid,
            self.protocol_info.denom.to_string(),
        )
    }

    fn validate_maker_payment(
        &self,
        input: ValidatePaymentInput,
    ) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        let sender_pub = input.maker_pub.clone();
        self.validate_payment_for_denom(
            input,
            &sender_pub,
            self.protocol_info.denom.clone(),
            self.protocol_info.decimals,
        )
    }

    fn validate_taker_payment(
        &self,
        input: ValidatePaymentInput,
    ) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        let sender_pub = input.taker_pub.clone();
        self.validate_payment_for_denom(
            input,
            &sender_pub,
            self.protocol_info.denom.clone(),
            self.protocol_info.decimals,
        )
    }

    /// Answers an error: the payment lookup needs the amount (CRD ch.18 R18.11.22), which this method does not
    /// carry. A hash-lock-only fallback would find a payment of another amount or receiver, so none is offered;
    /// the swap machine calls [`SwapOps::check_if_my_payment_sent_for_amount`].
    fn check_if_my_payment_sent(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        _secret_hash: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn futures01::Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        Box::new(futures01::future::err(PAYMENT_SENT_NEEDS_AMOUNT.to_string()))
    }

    fn check_if_my_payment_sent_for_amount(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
        amount: &BigDecimal,
    ) -> Box<dyn futures01::Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        self.check_if_my_payment_sent_for_denom(
            other_pub,
            secret_hash,
            amount,
            self.protocol_info.denom.clone(),
            self.protocol_info.decimals,
        )
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        self.search_for_swap_tx_spend(tx, secret_hash)
            .await
            .map_err(|e| e.to_string())
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        self.search_for_swap_tx_spend(tx, secret_hash)
            .await
            .map_err(|e| e.to_string())
    }

    fn extract_secret(&self, _secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        let tx = try_s!(cosmrs::Tx::from_bytes(spend_tx));
        let msg = try_s!(tx.body.messages.first().ok_or("Tx body couldn't be read."));

        let htlc_type = try_s!(HtlcType::from_str(&self.protocol_info.account_prefix));
        let htlc_proto = try_s!(ClaimHtlcProto::decode(htlc_type, msg.value.as_slice()));
        let htlc = try_s!(ClaimHtlcMsg::try_from(htlc_proto));

        // CRD ch.18 R18.11.14: the secret of a claim is exactly 32 bytes.
        let secret = hex::decode(htlc.secret()).map_err(|e| format!("{}", e))?;
        if secret.len() != SECRET_LEN {
            return Err(format!(
                "Invalid secret length: {} bytes, expected {}",
                secret.len(),
                SECRET_LEN
            ));
        }
        Ok(secret)
    }

    fn negotiate_swap_contract_addr(
        &self,
        _other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        Ok(None)
    }

    fn get_htlc_key_pair(&self) -> Option<KeyPair> {
        let priv_key = self.activation_policy.activated_key_or_err().ok()?;
        key_pair_from_secret(priv_key.as_slice()).ok()
    }
}

// ————————————————————————————————————————————————————————————————
// WatcherOps (empty default impls)
// ————————————————————————————————————————————————————————————————

impl crate::WatcherOps for TendermintCoin {}
