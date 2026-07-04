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

// ————————————————————————————————————————————————————————————————
// Denom-parameterised helpers (pub(super) for token reuse)
// ————————————————————————————————————————————————————————————————

impl TendermintCoin {
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
                let burn_pubkey = match burn_destination {
                    DexFeeBurnDestination::PreBurnAccount { burn_pubkey } => burn_pubkey.clone(),
                    _ => try_tx_fus!(Err::<Vec<u8>, String>("KmdOpReturn not supported on Tendermint".into())),
                };
                let burn_pubkey_hash = dhash160(&burn_pubkey);
                let burn_address = try_tx_fus!(AccountId::new(
                    &self.protocol_info.account_prefix,
                    burn_pubkey_hash.as_slice()
                ));
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

    /// Validate a taker DEX-fee transaction for a given denom.
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

        let dex_fee = dex_fee.clone();
        let fut = async move {
            let tx_body = try_s!(TxBody::decode(tx.data.body_bytes.as_slice()));

            if tx_body.memo != uuid_str {
                return Err(format!("Invalid memo: expected {}, found {}", uuid_str, tx_body.memo));
            }

            let first_msg = try_s!(tx_body.messages.first().ok_or("No messages in tx body"));

            match &dex_fee {
                DexFee::Standard(_) => {
                    let msg = try_s!(MsgSendProto::decode(first_msg.value.as_slice()));
                    if msg.from_address != expected_sender_addr.to_string() {
                        return Err(format!(
                            "Invalid sender: {}, expected {}",
                            msg.from_address, expected_sender_addr
                        ));
                    }
                    if msg.to_address != expected_fee_addr.to_string() {
                        return Err(format!(
                            "Invalid receiver: {}, expected {}",
                            msg.to_address, expected_fee_addr
                        ));
                    }
                    let expected = CoinProto {
                        denom,
                        amount: fee_amount_sat.to_string(),
                    };
                    if msg.amount.first() != Some(&expected) {
                        return Err(format!(
                            "Invalid fee amount: {:?}, expected {:?}",
                            msg.amount.first(),
                            expected
                        ));
                    }
                },
                DexFee::WithBurn { .. } => {
                    let msg = try_s!(MsgMultiSendProto::decode(first_msg.value.as_slice()));
                    if let Some(input) = msg.inputs.first() {
                        if input.address != expected_sender_addr.to_string() {
                            return Err(format!(
                                "Invalid sender: {}, expected {}",
                                input.address, expected_sender_addr
                            ));
                        }
                    } else {
                        return Err("MsgMultiSend has no inputs".to_string());
                    }
                    if let Some(output) = msg.outputs.first() {
                        if output.address != expected_fee_addr.to_string() {
                            return Err(format!(
                                "Invalid fee output address: {}, expected {}",
                                output.address, expected_fee_addr
                            ));
                        }
                    } else {
                        return Err("MsgMultiSend has no outputs".to_string());
                    }
                },
                DexFee::NoFee => return Err("Unexpected DexFee::NoFee".to_string()),
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

    /// Search for an HTLC we sent, matching by secret hash.
    ///
    /// The fork's trait does not pass `amount`, so we search create_htlc
    /// events from our address and match the hash_lock field.
    pub(super) fn check_if_my_payment_sent_for_denom(
        &self,
        other_pub: &[u8],
        secret_hash: &[u8],
    ) -> Box<dyn futures01::Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        let secret_hash_hex = hex::encode(secret_hash).to_uppercase();
        let coin = self.clone();
        let _other_pub = other_pub.to_vec();
        let fut = async move {
            let rpc_client = try_s!(coin.rpc_client().await);
            let query = format!("create_htlc.sender='{}'", coin.account_id);

            let response = try_s!(
                rpc_client
                    .perform(TxSearchRequest {
                        query,
                        order_by: TendermintResultOrder::Descending.into(),
                        page: 1,
                        per_page: 50,
                        prove: false,
                    })
                    .await
            );

            let htlc_type = match HtlcType::from_str(&coin.protocol_info.account_prefix) {
                Ok(t) => t,
                Err(e) => return Err(format!("{}", e)),
            };

            for raw_tx in &response.txs {
                if let cosmrs::tendermint::abci::Code::Err(_) = raw_tx.tx_result.code {
                    continue; // skip failed txs
                }

                let tx = match cosmrs::Tx::from_bytes(&raw_tx.tx) {
                    Ok(tx) => tx,
                    Err(_) => continue,
                };
                let msg = match tx.body.messages.first() {
                    Some(m) => m,
                    None => continue,
                };
                let htlc_proto = match CreateHtlcProto::decode(htlc_type, msg.value.as_slice()) {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                if htlc_proto.hash_lock().to_uppercase() == secret_hash_hex {
                    let htlc = TransactionEnum::CosmosTransaction(CosmosTransaction {
                        data: try_s!(TxRaw::decode(raw_tx.tx.as_slice())),
                    });
                    return Ok(Some(htlc));
                }
            }

            Ok(None)
        };

        Box::new(fut.boxed().compat())
    }
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

    fn check_if_my_payment_sent(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn futures01::Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        self.check_if_my_payment_sent_for_denom(other_pub, secret_hash)
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        _time_lock: u32,
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

        hex::decode(htlc.secret()).map_err(|e| format!("{}", e))
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
