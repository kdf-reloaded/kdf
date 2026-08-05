// utxo_common_swap — HTLC/swap operations, payment scripts, validation

use super::*;

pub const DEFAULT_SWAP_TX_SPEND_SIZE: u64 = 305;

pub const DEFAULT_SWAP_VOUT: usize = 0;

/// returns the fee required to be paid for HTLC spend transaction
pub async fn get_htlc_spend_fee<T: UtxoCommonOps>(coin: &T, tx_size: u64) -> UtxoRpcResult<u64> {
    let coin_fee = coin.get_tx_fee().await?;
    let mut fee = match coin_fee {
        // atomic swap payment spend transaction is slightly more than 300 bytes in average as of now
        ActualTxFee::Dynamic(fee_per_kb) => (fee_per_kb * tx_size) / KILO_BYTE,
        // return satoshis here as swap spend transaction size is always less than 1 kb
        ActualTxFee::FixedPerKb(satoshis) => {
            let tx_size_kb = if tx_size % KILO_BYTE == 0 {
                tx_size / KILO_BYTE
            } else {
                tx_size / KILO_BYTE + 1
            };
            satoshis * tx_size_kb
        },
    };
    if coin.as_ref().conf.force_min_relay_fee {
        let relay_fee = coin.as_ref().rpc_client.get_relay_fee().compat().await?;
        let relay_fee_sat = sat_from_big_decimal(&relay_fee, coin.as_ref().decimals).mm_err(Into::into)?;
        if fee < relay_fee_sat {
            fee = relay_fee_sat;
        }
    }
    Ok(fee)
}

/// Constructs and broadcasts the taker DEX fee transaction.
///
/// For `DexFee::Standard`: a single P2PKH output to the fee-collection address.
/// For `DexFee::WithBurn`: two outputs — fee to the DEX address, burn via
/// OP_RETURN (KMD) or P2PKH to a burn address (other coins).
pub fn send_taker_fee<T>(coin: T, dex_fee: &DexFee, fee_pub_key: &[u8]) -> TransactionFut
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    let fee_address = try_tx_fus!(address_from_raw_pubkey(
        fee_pub_key,
        coin.as_ref().conf.pub_addr_prefix,
        coin.as_ref().conf.pub_t_addr_prefix,
        coin.as_ref().conf.checksum_type,
        coin.as_ref().conf.bech32_hrp.clone(),
        coin.addr_format().clone(),
    ));

    let outputs = try_tx_fus!(generate_taker_fee_tx_outputs(&coin, dex_fee, &fee_address));
    match taker_fee_allowed_underdust_output(dex_fee) {
        Some(output_index) => send_outputs_from_my_address_with_underdust_output(coin, outputs, output_index),
        None => send_outputs_from_my_address(coin, outputs),
    }
}

/// The stable netid-8762 KMD implementation applies dust to the unsplit DEX
/// fee, then preserves both positive 75/25 split legs on the wire. Therefore
/// only the fee-collection output of that exact descriptor may bypass the
/// generic per-output dust check.
pub(crate) fn taker_fee_allowed_underdust_output(dex_fee: &DexFee) -> Option<usize> {
    match dex_fee {
        DexFee::WithBurn {
            burn_destination: DexFeeBurnDestination::KmdOpReturn,
            ..
        } => Some(DEFAULT_FEE_VOUT),
        DexFee::NoFee | DexFee::Standard(_) | DexFee::WithBurn { .. } => None,
    }
}

/// Builds the transaction outputs for a taker fee payment.
///
/// Returns 0 outputs for `NoFee`, 1 for `Standard`, or 2 for `WithBurn`.
pub(crate) fn generate_taker_fee_tx_outputs(
    coin: &impl UtxoCommonOps,
    dex_fee: &DexFee,
    fee_address: &Address,
) -> Result<Vec<TransactionOutput>, String> {
    match dex_fee {
        DexFee::NoFee => Ok(vec![]),
        DexFee::Standard(amount) => {
            let sat = sat_from_big_decimal(&amount.to_decimal(), coin.as_ref().decimals).map_err(|e| e.to_string())?;
            Ok(vec![TransactionOutput {
                value: sat,
                script_pubkey: Builder::build_p2pkh(&fee_address.hash).to_bytes(),
            }])
        },
        DexFee::WithBurn {
            fee_amount,
            burn_amount,
            burn_destination,
        } => {
            let fee_sat =
                sat_from_big_decimal(&fee_amount.to_decimal(), coin.as_ref().decimals).map_err(|e| e.to_string())?;
            let burn_sat =
                sat_from_big_decimal(&burn_amount.to_decimal(), coin.as_ref().decimals).map_err(|e| e.to_string())?;

            // Output 0: fee portion → DEX address (P2PKH)
            let fee_output = TransactionOutput {
                value: fee_sat,
                script_pubkey: Builder::build_p2pkh(&fee_address.hash).to_bytes(),
            };

            // Output 1: burn portion → OP_RETURN (KMD) or burn address (others)
            let burn_output = match burn_destination {
                DexFeeBurnDestination::KmdOpReturn => TransactionOutput {
                    value: burn_sat,
                    script_pubkey: Builder::default().push_opcode(Opcode::OP_RETURN).into_bytes(),
                },
                DexFeeBurnDestination::PreBurnAccount { burn_pubkey } => {
                    let burn_address = address_from_raw_pubkey(
                        burn_pubkey,
                        coin.as_ref().conf.pub_addr_prefix,
                        coin.as_ref().conf.pub_t_addr_prefix,
                        coin.as_ref().conf.checksum_type,
                        coin.as_ref().conf.bech32_hrp.clone(),
                        coin.addr_format().clone(),
                    )
                    .map_err(|e| format!("Failed to derive burn address: {}", e))?;
                    TransactionOutput {
                        value: burn_sat,
                        script_pubkey: Builder::build_p2pkh(&burn_address.hash).to_bytes(),
                    }
                },
            };

            Ok(vec![fee_output, burn_output])
        },
    }
}

pub fn send_maker_payment<T>(
    coin: T,
    time_lock: u32,
    maker_pub: &[u8],
    taker_pub: &[u8],
    secret_hash: &[u8],
    amount: BigDecimal,
) -> TransactionFut
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    let SwapPaymentOutputsResult {
        payment_address,
        outputs,
    } = try_tx_fus!(generate_swap_payment_outputs(
        &coin,
        time_lock,
        maker_pub,
        taker_pub,
        secret_hash,
        amount
    ));
    let send_fut = match &coin.as_ref().rpc_client {
        UtxoRpcClientEnum::Electrum(_) => Either::A(send_outputs_from_my_address(coin, outputs)),
        UtxoRpcClientEnum::Native(client) => {
            let addr_string = try_tx_fus!(payment_address.display_address());
            Either::B(
                client
                    .import_address(&addr_string, &addr_string, false)
                    .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
                    .and_then(move |_| send_outputs_from_my_address(coin, outputs)),
            )
        },
    };
    Box::new(send_fut)
}

pub fn send_taker_payment<T>(
    coin: T,
    time_lock: u32,
    taker_pub: &[u8],
    maker_pub: &[u8],
    secret_hash: &[u8],
    amount: BigDecimal,
) -> TransactionFut
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    let SwapPaymentOutputsResult {
        payment_address,
        outputs,
    } = try_tx_fus!(generate_swap_payment_outputs(
        &coin,
        time_lock,
        taker_pub,
        maker_pub,
        secret_hash,
        amount
    ));

    let send_fut = match &coin.as_ref().rpc_client {
        UtxoRpcClientEnum::Electrum(_) => Either::A(send_outputs_from_my_address(coin, outputs)),
        UtxoRpcClientEnum::Native(client) => {
            let addr_string = try_tx_fus!(payment_address.display_address());
            Either::B(
                client
                    .import_address(&addr_string, &addr_string, false)
                    .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
                    .and_then(move |_| send_outputs_from_my_address(coin, outputs)),
            )
        },
    };
    Box::new(send_fut)
}

pub fn send_maker_spends_taker_payment<T: UtxoCommonOps>(
    coin: T,
    taker_payment_tx: &[u8],
    time_lock: u32,
    taker_pub: &[u8],
    secret: &[u8],
    htlc_privkey: &[u8],
) -> TransactionFut {
    let key_pair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
    let my_address = try_tx_fus!(coin.as_ref().derivation_method.iguana_or_err()).clone();

    let mut prev_tx: UtxoTx = try_tx_fus!(deserialize(taker_payment_tx).map_err(|e| ERRL!("{:?}", e)));
    prev_tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
    let script_data = Builder::default()
        .push_data(secret)
        .push_opcode(Opcode::OP_0)
        .into_script();
    let redeem_script = payment_script(
        time_lock,
        &*dhash160(secret),
        &try_tx_fus!(Public::from_slice(taker_pub)),
        key_pair.public(),
    );
    let fut = async move {
        let fee = try_tx_s!(coin.get_htlc_spend_fee(DEFAULT_SWAP_TX_SPEND_SIZE).await);
        let script_pubkey = output_script(&my_address, ScriptType::P2PKH).to_bytes();
        let output = TransactionOutput {
            value: prev_tx.outputs[0].value - fee,
            script_pubkey,
        };

        let transaction = try_tx_s!(
            coin.p2sh_spending_tx(
                prev_tx,
                redeem_script.into(),
                vec![output],
                script_data,
                SEQUENCE_FINAL,
                time_lock,
                &key_pair,
            )
            .await
        );

        let tx_fut = coin.as_ref().rpc_client.send_transaction(&transaction).compat();
        try_tx_s!(tx_fut.await, transaction);

        Ok(transaction.into())
    };
    Box::new(fut.boxed().compat())
}

pub fn send_taker_spends_maker_payment<T: UtxoCommonOps>(
    coin: T,
    maker_payment_tx: &[u8],
    time_lock: u32,
    maker_pub: &[u8],
    secret: &[u8],
    htlc_privkey: &[u8],
) -> TransactionFut {
    let key_pair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
    let my_address = try_tx_fus!(coin.as_ref().derivation_method.iguana_or_err()).clone();

    let mut prev_tx: UtxoTx = try_tx_fus!(deserialize(maker_payment_tx).map_err(|e| ERRL!("{:?}", e)));
    prev_tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
    let script_data = Builder::default()
        .push_data(secret)
        .push_opcode(Opcode::OP_0)
        .into_script();
    let redeem_script = payment_script(
        time_lock,
        &*dhash160(secret),
        &try_tx_fus!(Public::from_slice(maker_pub)),
        key_pair.public(),
    );
    let fut = async move {
        let fee = try_tx_s!(coin.get_htlc_spend_fee(DEFAULT_SWAP_TX_SPEND_SIZE).await);
        let script_pubkey = output_script(&my_address, ScriptType::P2PKH).to_bytes();
        let output = TransactionOutput {
            value: prev_tx.outputs[0].value - fee,
            script_pubkey,
        };

        let transaction = try_tx_s!(
            coin.p2sh_spending_tx(
                prev_tx,
                redeem_script.into(),
                vec![output],
                script_data,
                SEQUENCE_FINAL,
                time_lock,
                &key_pair,
            )
            .await
        );

        let tx_fut = coin.as_ref().rpc_client.send_transaction(&transaction).compat();
        try_tx_s!(tx_fut.await, transaction);

        Ok(transaction.into())
    };
    Box::new(fut.boxed().compat())
}

pub fn send_taker_refunds_payment<T: UtxoCommonOps>(
    coin: T,
    taker_payment_tx: &[u8],
    time_lock: u32,
    maker_pub: &[u8],
    secret_hash: &[u8],
    htlc_privkey: &[u8],
) -> TransactionFut {
    let key_pair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
    let my_address = try_tx_fus!(coin.as_ref().derivation_method.iguana_or_err()).clone();

    let mut prev_tx: UtxoTx =
        try_tx_fus!(deserialize(taker_payment_tx).map_err(|e| TransactionErr::Plain(format!("{:?}", e))));
    prev_tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
    let script_data = Builder::default().push_opcode(Opcode::OP_1).into_script();
    let redeem_script = payment_script(
        time_lock,
        secret_hash,
        key_pair.public(),
        &try_tx_fus!(Public::from_slice(maker_pub)),
    );
    let fut = async move {
        let fee = try_tx_s!(coin.get_htlc_spend_fee(DEFAULT_SWAP_TX_SPEND_SIZE).await);
        let script_pubkey = output_script(&my_address, ScriptType::P2PKH).to_bytes();
        let output = TransactionOutput {
            value: prev_tx.outputs[0].value - fee,
            script_pubkey,
        };

        let transaction = try_tx_s!(
            coin.p2sh_spending_tx(
                prev_tx,
                redeem_script.into(),
                vec![output],
                script_data,
                SEQUENCE_FINAL - 1,
                time_lock,
                &key_pair,
            )
            .await
        );

        let tx_fut = coin.as_ref().rpc_client.send_transaction(&transaction).compat();
        try_tx_s!(tx_fut.await, transaction);

        Ok(transaction.into())
    };
    Box::new(fut.boxed().compat())
}

pub fn send_maker_refunds_payment<T: UtxoCommonOps>(
    coin: T,
    maker_payment_tx: &[u8],
    time_lock: u32,
    taker_pub: &[u8],
    secret_hash: &[u8],
    htlc_privkey: &[u8],
) -> TransactionFut {
    let key_pair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
    let my_address = try_tx_fus!(coin.as_ref().derivation_method.iguana_or_err()).clone();

    let mut prev_tx: UtxoTx = try_tx_fus!(deserialize(maker_payment_tx).map_err(|e| ERRL!("{:?}", e)));
    prev_tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
    let script_data = Builder::default().push_opcode(Opcode::OP_1).into_script();
    let redeem_script = payment_script(
        time_lock,
        secret_hash,
        key_pair.public(),
        &try_tx_fus!(Public::from_slice(taker_pub)),
    );
    let fut = async move {
        let fee = try_tx_s!(coin.get_htlc_spend_fee(DEFAULT_SWAP_TX_SPEND_SIZE).await);
        let script_pubkey = output_script(&my_address, ScriptType::P2PKH).to_bytes();
        let output = TransactionOutput {
            value: prev_tx.outputs[0].value - fee,
            script_pubkey,
        };

        let transaction = try_tx_s!(
            coin.p2sh_spending_tx(
                prev_tx,
                redeem_script.into(),
                vec![output],
                script_data,
                SEQUENCE_FINAL - 1,
                time_lock,
                &key_pair,
            )
            .await
        );

        let tx_fut = coin.as_ref().rpc_client.send_transaction(&transaction).compat();
        try_tx_s!(tx_fut.await, transaction);

        Ok(transaction.into())
    };
    Box::new(fut.boxed().compat())
}

/// Extracts pubkey from script sig
fn pubkey_from_script_sig(script: &Script) -> Result<H264, String> {
    match script.get_instruction(0) {
        Some(Ok(instruction)) => match instruction.opcode {
            Opcode::OP_PUSHBYTES_70 | Opcode::OP_PUSHBYTES_71 | Opcode::OP_PUSHBYTES_72 => match instruction.data {
                Some(bytes) => try_s!(Signature::from_der(&bytes[..bytes.len() - 1])),
                None => return ERR!("No data at instruction 0 of script {:?}", script),
            },
            _ => return ERR!("Unexpected opcode {:?}", instruction.opcode),
        },
        Some(Err(e)) => return ERR!("Error {} on getting instruction 0 of script {:?}", e, script),
        None => return ERR!("None instruction 0 of script {:?}", script),
    };

    let pubkey = match script.get_instruction(1) {
        Some(Ok(instruction)) => match instruction.opcode {
            Opcode::OP_PUSHBYTES_33 => match instruction.data {
                Some(bytes) => try_s!(PublicKey::from_slice(bytes)),
                None => return ERR!("No data at instruction 1 of script {:?}", script),
            },
            _ => return ERR!("Unexpected opcode {:?}", instruction.opcode),
        },
        Some(Err(e)) => return ERR!("Error {} on getting instruction 1 of script {:?}", e, script),
        None => return ERR!("None instruction 1 of script {:?}", script),
    };

    if script.get_instruction(2).is_some() {
        return ERR!("Unexpected instruction at position 2 of script {:?}", script);
    }
    Ok(pubkey.serialize().into())
}

/// Extracts pubkey from witness script
fn pubkey_from_witness_script(witness_script: &[Bytes]) -> Result<H264, String> {
    if witness_script.len() != 2 {
        return ERR!("Invalid witness length {}", witness_script.len());
    }

    let signature = witness_script[0].clone().take();
    if signature.is_empty() {
        return ERR!("Empty signature data in witness script");
    }
    try_s!(Signature::from_der(&signature[..signature.len() - 1]));

    let pubkey = try_s!(PublicKey::from_slice(&witness_script[1]));

    Ok(pubkey.serialize().into())
}

pub async fn is_tx_confirmed_before_block<T>(coin: &T, tx: &RpcTransaction, block_number: u64) -> Result<bool, String>
where
    T: UtxoCommonOps,
{
    match tx.height {
        Some(confirmed_at) => Ok(confirmed_at <= block_number),
        // fallback to a number of confirmations
        None => {
            if tx.confirmations > 0 {
                let current_block = try_s!(coin.as_ref().rpc_client.get_block_count().compat().await);
                let confirmed_at = current_block + 1 - tx.confirmations as u64;
                Ok(confirmed_at <= block_number)
            } else {
                Ok(false)
            }
        },
    }
}

pub fn check_all_inputs_signed_by_pub(tx: &UtxoTx, expected_pub: &[u8]) -> Result<bool, String> {
    for input in &tx.inputs {
        let pubkey = if input.has_witness() {
            try_s!(pubkey_from_witness_script(&input.script_witness))
        } else {
            let script: Script = input.script_sig.clone().into();
            try_s!(pubkey_from_script_sig(&script))
        };
        if *pubkey != expected_pub {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Validates a taker DEX fee transaction against expected parameters.
///
/// For `DexFee::Standard`: checks output 0 pays the fee address the expected amount.
/// For `DexFee::WithBurn`: additionally validates the burn output (OP_RETURN or P2PKH).
pub fn validate_fee<T: UtxoCommonOps>(
    coin: T,
    tx: UtxoTx,
    output_index: usize,
    sender_pubkey: &[u8],
    dex_fee: &DexFee,
    min_block_number: u64,
    fee_addr: &[u8],
) -> Box<dyn Future<Item = (), Error = String> + Send> {
    let dex_fee = dex_fee.clone();
    let address = try_fus!(address_from_raw_pubkey(
        fee_addr,
        coin.as_ref().conf.pub_addr_prefix,
        coin.as_ref().conf.pub_t_addr_prefix,
        coin.as_ref().conf.checksum_type,
        coin.as_ref().conf.bech32_hrp.clone(),
        coin.addr_format().clone(),
    ));

    if !try_fus!(check_all_inputs_signed_by_pub(&tx, sender_pubkey)) {
        return Box::new(futures01::future::err(ERRL!("The dex fee was sent from wrong address")));
    }
    let fut = async move {
        let tx_from_rpc = try_s!(
            coin.as_ref()
                .rpc_client
                .get_verbose_transaction(&tx.hash().reversed().into())
                .compat()
                .await
        );

        if try_s!(is_tx_confirmed_before_block(&coin, &tx_from_rpc, min_block_number).await) {
            return ERR!(
                "Fee tx {:?} confirmed before min_block {}",
                tx_from_rpc,
                min_block_number,
            );
        }
        if tx_from_rpc.hex.0 != serialize(&tx).take()
            && tx_from_rpc.hex.0 != serialize_with_flags(&tx, SERIALIZE_TRANSACTION_WITNESS).take()
        {
            return ERR!(
                "Provided dex fee tx {:?} doesn't match tx data from rpc {:?}",
                tx,
                tx_from_rpc
            );
        }

        // Validate fee output(s) based on DexFee variant
        match &dex_fee {
            DexFee::NoFee => {},
            DexFee::Standard(amount) => {
                let expected_sat = try_s!(sat_from_big_decimal(&amount.to_decimal(), coin.as_ref().decimals));
                try_s!(validate_dex_output(&tx, output_index, &address, expected_sat));
            },
            DexFee::WithBurn {
                fee_amount,
                burn_amount,
                burn_destination,
            } => {
                // Validate fee output (output_index)
                let fee_sat = try_s!(sat_from_big_decimal(&fee_amount.to_decimal(), coin.as_ref().decimals));
                try_s!(validate_dex_output(&tx, output_index, &address, fee_sat));

                // Validate burn output (output_index + 1)
                let burn_sat = try_s!(sat_from_big_decimal(&burn_amount.to_decimal(), coin.as_ref().decimals));
                let expected_burn_script = match burn_destination {
                    DexFeeBurnDestination::KmdOpReturn => {
                        Builder::default().push_opcode(Opcode::OP_RETURN).into_bytes()
                    },
                    DexFeeBurnDestination::PreBurnAccount { burn_pubkey } => {
                        let burn_address = try_s!(address_from_raw_pubkey(
                            burn_pubkey,
                            coin.as_ref().conf.pub_addr_prefix,
                            coin.as_ref().conf.pub_t_addr_prefix,
                            coin.as_ref().conf.checksum_type,
                            coin.as_ref().conf.bech32_hrp.clone(),
                            coin.addr_format().clone(),
                        ));
                        Builder::build_p2pkh(&burn_address.hash).to_bytes()
                    },
                };
                try_s!(validate_burn_output(
                    &tx,
                    output_index + 1,
                    &expected_burn_script,
                    burn_sat
                ));
            },
        }
        Ok(())
    };
    Box::new(fut.boxed().compat())
}

/// Validates that a specific output pays the expected address the expected amount.
fn validate_dex_output(tx: &UtxoTx, index: usize, expected_addr: &Address, expected_sat: u64) -> Result<(), String> {
    match tx.outputs.get(index) {
        Some(out) => {
            let expected_script = Builder::build_p2pkh(&expected_addr.hash).to_bytes();
            if out.script_pubkey != expected_script {
                return ERR!(
                    "Dex fee tx output {} script_pubkey mismatch: got {:?}, expected {:?}",
                    index,
                    out.script_pubkey,
                    expected_script
                );
            }
            if out.value < expected_sat {
                return ERR!(
                    "Dex fee tx output {} value {} is less than expected {}",
                    index,
                    out.value,
                    expected_sat
                );
            }
            Ok(())
        },
        None => ERR!("Dex fee tx does not have output index {}", index),
    }
}

/// Validates that a specific output matches the expected burn script and amount.
fn validate_burn_output(tx: &UtxoTx, index: usize, expected_script: &[u8], expected_sat: u64) -> Result<(), String> {
    match tx.outputs.get(index) {
        Some(out) => {
            if out.script_pubkey.as_ref() != expected_script {
                return ERR!(
                    "Burn output {} script mismatch: got {:?}, expected {:?}",
                    index,
                    out.script_pubkey,
                    expected_script
                );
            }
            if out.value < expected_sat {
                return ERR!(
                    "Burn output {} value {} is less than expected {}",
                    index,
                    out.value,
                    expected_sat
                );
            }
            Ok(())
        },
        None => ERR!("Dex fee tx does not have burn output at index {}", index),
    }
}

pub fn validate_maker_payment<T: UtxoCommonOps>(
    coin: &T,
    input: ValidatePaymentInput,
) -> Box<dyn Future<Item = (), Error = String> + Send> {
    let my_public = try_fus!(Public::from_slice(&input.taker_pub));
    let mut tx: UtxoTx = try_fus!(deserialize(input.payment_tx.as_slice()).map_err(|e| ERRL!("{:?}", e)));
    tx.tx_hash_algo = coin.as_ref().tx_hash_algo;

    validate_payment(
        coin.clone(),
        tx,
        DEFAULT_SWAP_VOUT,
        &try_fus!(Public::from_slice(&input.maker_pub)),
        &my_public,
        &input.secret_hash,
        input.amount,
        input.time_lock,
        input.try_spv_proof_until,
        input.confirmations,
    )
}

pub fn validate_taker_payment<T: UtxoCommonOps>(
    coin: &T,
    input: ValidatePaymentInput,
) -> Box<dyn Future<Item = (), Error = String> + Send> {
    let my_public = try_fus!(Public::from_slice(&input.maker_pub));
    let mut tx: UtxoTx = try_fus!(deserialize(input.payment_tx.as_slice()).map_err(|e| ERRL!("{:?}", e)));
    tx.tx_hash_algo = coin.as_ref().tx_hash_algo;

    validate_payment(
        coin.clone(),
        tx,
        DEFAULT_SWAP_VOUT,
        &try_fus!(Public::from_slice(&input.taker_pub)),
        &my_public,
        &input.secret_hash,
        input.amount,
        input.time_lock,
        input.try_spv_proof_until,
        input.confirmations,
    )
}

pub fn check_if_my_payment_sent<T: UtxoCommonOps>(
    coin: T,
    time_lock: u32,
    my_pub: &[u8],
    other_pub: &[u8],
    secret_hash: &[u8],
) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
    let my_public = try_fus!(Public::from_slice(my_pub));
    let script = payment_script(
        time_lock,
        secret_hash,
        &my_public,
        &try_fus!(Public::from_slice(other_pub)),
    );
    let hash = dhash160(&script);
    let p2sh = Builder::build_p2sh(&hash.into());
    let script_hash = electrum_script_hash(&p2sh);
    let fut = async move {
        match &coin.as_ref().rpc_client {
            UtxoRpcClientEnum::Electrum(client) => {
                let history = try_s!(client.scripthash_get_history(&hex::encode(script_hash)).compat().await);
                match history.first() {
                    Some(item) => {
                        let tx_bytes = try_s!(client.get_transaction_bytes(&item.tx_hash).compat().await);
                        let mut tx: UtxoTx = try_s!(deserialize(tx_bytes.0.as_slice()).map_err(|e| ERRL!("{:?}", e)));
                        tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
                        Ok(Some(tx.into()))
                    },
                    None => Ok(None),
                }
            },
            UtxoRpcClientEnum::Native(client) => {
                let target_addr = Address {
                    t_addr_prefix: coin.as_ref().conf.p2sh_t_addr_prefix,
                    prefix: coin.as_ref().conf.p2sh_addr_prefix,
                    hash: hash.into(),
                    checksum_type: coin.as_ref().conf.checksum_type,
                    hrp: coin.as_ref().conf.bech32_hrp.clone(),
                    addr_format: coin.addr_format().clone(),
                };
                let target_addr = target_addr.to_string();
                let is_imported = try_s!(client.is_address_imported(&target_addr).await);
                if !is_imported {
                    return Ok(None);
                }
                let received_by_addr = try_s!(client.list_received_by_address(0, true, true).compat().await);
                for item in received_by_addr {
                    if item.address == target_addr && !item.txids.is_empty() {
                        let tx_bytes = try_s!(client.get_transaction_bytes(&item.txids[0]).compat().await);
                        let mut tx: UtxoTx = try_s!(deserialize(tx_bytes.0.as_slice()).map_err(|e| ERRL!("{:?}", e)));
                        tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
                        return Ok(Some(tx.into()));
                    }
                }
                Ok(None)
            },
        }
    };
    Box::new(fut.boxed().compat())
}

pub async fn search_for_swap_tx_spend_my(
    coin: &UtxoCoinFields,
    time_lock: u32,
    other_pub: &[u8],
    secret_hash: &[u8],
    tx: &[u8],
    output_index: usize,
    search_from_block: u64,
) -> Result<Option<FoundSwapTxSpend>, String> {
    let my_public = try_s!(coin.priv_key_policy.key_pair_or_err()).public();
    search_for_swap_output_spend(
        coin,
        time_lock,
        my_public,
        &try_s!(Public::from_slice(other_pub)),
        secret_hash,
        tx,
        output_index,
        search_from_block,
    )
    .await
}

pub async fn search_for_swap_tx_spend_other(
    coin: &UtxoCoinFields,
    time_lock: u32,
    other_pub: &[u8],
    secret_hash: &[u8],
    tx: &[u8],
    output_index: usize,
    search_from_block: u64,
) -> Result<Option<FoundSwapTxSpend>, String> {
    let my_public = try_s!(coin.priv_key_policy.key_pair_or_err()).public();
    search_for_swap_output_spend(
        coin,
        time_lock,
        &try_s!(Public::from_slice(other_pub)),
        my_public,
        secret_hash,
        tx,
        output_index,
        search_from_block,
    )
    .await
}

/// Extract a secret from the `spend_tx`.
/// Note spender could generate the spend with several inputs where the only one input is the p2sh script.
pub fn extract_secret(secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
    let spend_tx: UtxoTx = try_s!(deserialize(spend_tx).map_err(|e| ERRL!("{:?}", e)));
    for (input_idx, input) in spend_tx.inputs.into_iter().enumerate() {
        let script: Script = input.script_sig.clone().into();
        let instruction = match script.get_instruction(1) {
            Some(Ok(instr)) => instr,
            Some(Err(e)) => {
                log!("Warning: "[e]);
                continue;
            },
            None => {
                log!("Warning: couldn't find secret in "[input_idx]" input");
                continue;
            },
        };

        if instruction.opcode != Opcode::OP_PUSHBYTES_32 {
            log!("Warning: expected "[Opcode::OP_PUSHBYTES_32]" opcode, found "[instruction.opcode] " in "[input_idx]" input");
            continue;
        }

        let secret = match instruction.data {
            Some(data) => data.to_vec(),
            None => {
                log!("Warning: secret is empty in "[input_idx] " input");
                continue;
            },
        };

        let actual_secret_hash = &*dhash160(&secret);
        if actual_secret_hash != secret_hash {
            log!("Warning: invalid 'dhash160(secret)' "[actual_secret_hash]", expected "[secret_hash]);
            continue;
        }
        return Ok(secret);
    }
    ERR!("Couldn't extract secret")
}

#[allow(clippy::too_many_arguments)]
pub fn validate_payment<T: UtxoCommonOps>(
    coin: T,
    tx: UtxoTx,
    output_index: usize,
    first_pub0: &Public,
    second_pub0: &Public,
    priv_bn_hash: &[u8],
    amount: BigDecimal,
    time_lock: u32,
    try_spv_proof_until: u64,
    confirmations: u64,
) -> Box<dyn Future<Item = (), Error = String> + Send> {
    let amount = try_fus!(sat_from_big_decimal(&amount, coin.as_ref().decimals));

    let expected_redeem = payment_script(time_lock, priv_bn_hash, first_pub0, second_pub0);
    let fut = async move {
        let mut attempts = 0;
        loop {
            let tx_from_rpc = match coin
                .as_ref()
                .rpc_client
                .get_transaction_bytes(&tx.hash().reversed().into())
                .compat()
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    if attempts > 2 {
                        return ERR!(
                            "Got error {:?} after 3 attempts of getting tx {:?} from RPC",
                            e,
                            tx.tx_hash()
                        );
                    };
                    attempts += 1;
                    log!("Error " [e] " getting the tx " [tx.tx_hash()] " from rpc");
                    Timer::sleep(10.).await;
                    continue;
                },
            };
            if serialize(&tx).take() != tx_from_rpc.0
                && serialize_with_flags(&tx, SERIALIZE_TRANSACTION_WITNESS).take() != tx_from_rpc.0
            {
                return ERR!(
                    "Provided payment tx {:?} doesn't match tx data from rpc {:?}",
                    tx,
                    tx_from_rpc
                );
            }

            let expected_output = TransactionOutput {
                value: amount,
                script_pubkey: Builder::build_p2sh(&dhash160(&expected_redeem).into()).into(),
            };

            let actual_output = tx.outputs.get(output_index);
            if actual_output != Some(&expected_output) {
                // Distinguish amount mismatch from script mismatch so failed swaps
                // and `validate_*_payment` callers get an actionable diagnostic
                // instead of a raw struct dump.
                let kind = match actual_output {
                    Some(actual)
                        if actual.value != expected_output.value
                            && actual.script_pubkey == expected_output.script_pubkey =>
                    {
                        "amount mismatch"
                    },
                    Some(actual) if actual.script_pubkey != expected_output.script_pubkey => "script mismatch",
                    Some(_) => "output mismatch",
                    None => "missing output",
                };
                return ERR!(
                    "Provided payment tx output {}: actual {:?}, expected {:?}",
                    kind,
                    actual_output,
                    expected_output
                );
            }

            if !coin.as_ref().conf.enable_spv_proof {
                return Ok(());
            }

            return match confirmations {
                0 => Ok(()),
                _ => validate_spv_proof(coin, tx, try_spv_proof_until)
                    .await
                    .map_err(|e| format!("{:?}", e)),
            };
        }
    };
    Box::new(fut.boxed().compat())
}

#[allow(clippy::too_many_arguments)]
async fn search_for_swap_output_spend(
    coin: &UtxoCoinFields,
    time_lock: u32,
    first_pub: &Public,
    second_pub: &Public,
    secret_hash: &[u8],
    tx: &[u8],
    output_index: usize,
    search_from_block: u64,
) -> Result<Option<FoundSwapTxSpend>, String> {
    let mut tx: UtxoTx = try_s!(deserialize(tx).map_err(|e| ERRL!("{:?}", e)));
    tx.tx_hash_algo = coin.tx_hash_algo;
    let script = payment_script(time_lock, secret_hash, first_pub, second_pub);
    let expected_script_pubkey = Builder::build_p2sh(&dhash160(&script).into()).to_bytes();
    if tx.outputs[0].script_pubkey != expected_script_pubkey {
        return ERR!(
            "Transaction {:?} output 0 script_pubkey doesn't match expected {:?}",
            tx,
            expected_script_pubkey
        );
    }

    let spend = try_s!(
        coin.rpc_client
            .find_output_spend(
                tx.hash(),
                &tx.outputs[output_index].script_pubkey,
                output_index,
                BlockHashOrHeight::Height(search_from_block as i64)
            )
            .compat()
            .await
    );
    match spend {
        Some(spent_output_info) => {
            let mut tx = spent_output_info.spending_tx;
            tx.tx_hash_algo = coin.tx_hash_algo;
            let script: Script = tx.inputs[0].script_sig.clone().into();
            if let Some(Ok(ref i)) = script.iter().nth(2) {
                if i.opcode == Opcode::OP_0 {
                    return Ok(Some(FoundSwapTxSpend::Spent(tx.into())));
                }
            }

            if let Some(Ok(ref i)) = script.iter().nth(1) {
                if i.opcode == Opcode::OP_1 {
                    return Ok(Some(FoundSwapTxSpend::Refunded(tx.into())));
                }
            }

            ERR!(
                "Couldn't find required instruction in script_sig of input 0 of tx {:?}",
                tx
            )
        },
        None => Ok(None),
    }
}

pub(crate) struct SwapPaymentOutputsResult {
    pub(crate) payment_address: Address,
    pub(crate) outputs: Vec<TransactionOutput>,
}

pub(crate) fn generate_swap_payment_outputs<T>(
    coin: T,
    time_lock: u32,
    my_pub: &[u8],
    other_pub: &[u8],
    secret_hash: &[u8],
    amount: BigDecimal,
) -> Result<SwapPaymentOutputsResult, String>
where
    T: AsRef<UtxoCoinFields>,
{
    let my_public = try_s!(Public::from_slice(my_pub));
    let redeem_script = payment_script(
        time_lock,
        secret_hash,
        &my_public,
        &try_s!(Public::from_slice(other_pub)),
    );
    let redeem_script_hash = dhash160(&redeem_script);
    let amount = try_s!(sat_from_big_decimal(&amount, coin.as_ref().decimals));
    let htlc_out = TransactionOutput {
        value: amount,
        script_pubkey: Builder::build_p2sh(&redeem_script_hash.into()).into(),
    };
    // record secret hash to blockchain too making it impossible to lose
    // lock time may be easily brute forced so it is not mandatory to record it
    let mut op_return_builder = Builder::default().push_opcode(Opcode::OP_RETURN);

    // add the full redeem script to the OP_RETURN for ARRR to simplify the validation for the daemon
    op_return_builder = if coin.as_ref().conf.ticker == "ARRR" {
        op_return_builder.push_data(&redeem_script)
    } else {
        op_return_builder.push_bytes(secret_hash)
    };

    let op_return_script = op_return_builder.into_bytes();

    let op_return_out = TransactionOutput {
        value: 0,
        script_pubkey: op_return_script,
    };

    let payment_address = Address {
        checksum_type: coin.as_ref().conf.checksum_type,
        hash: redeem_script_hash.into(),
        prefix: coin.as_ref().conf.p2sh_addr_prefix,
        t_addr_prefix: coin.as_ref().conf.p2sh_t_addr_prefix,
        hrp: coin.as_ref().conf.bech32_hrp.clone(),
        addr_format: UtxoAddressFormat::Standard,
    };
    let result = SwapPaymentOutputsResult {
        payment_address,
        outputs: vec![htlc_out, op_return_out],
    };
    Ok(result)
}

pub fn payment_script(time_lock: u32, secret_hash: &[u8], pub_0: &Public, pub_1: &Public) -> Script {
    let builder = Builder::default();
    builder
        .push_opcode(Opcode::OP_IF)
        .push_bytes(&time_lock.to_le_bytes())
        .push_opcode(Opcode::OP_CHECKLOCKTIMEVERIFY)
        .push_opcode(Opcode::OP_DROP)
        .push_bytes(pub_0)
        .push_opcode(Opcode::OP_CHECKSIG)
        .push_opcode(Opcode::OP_ELSE)
        .push_opcode(Opcode::OP_SIZE)
        .push_bytes(&[32])
        .push_opcode(Opcode::OP_EQUALVERIFY)
        .push_opcode(Opcode::OP_HASH160)
        .push_bytes(secret_hash)
        .push_opcode(Opcode::OP_EQUALVERIFY)
        .push_bytes(pub_1)
        .push_opcode(Opcode::OP_CHECKSIG)
        .push_opcode(Opcode::OP_ENDIF)
        .into_script()
}

pub fn dex_fee_script(uuid: [u8; 16], time_lock: u32, watcher_pub: &Public, sender_pub: &Public) -> Script {
    let builder = Builder::default();
    builder
        .push_bytes(&uuid)
        .push_opcode(Opcode::OP_DROP)
        .push_opcode(Opcode::OP_IF)
        .push_bytes(&time_lock.to_le_bytes())
        .push_opcode(Opcode::OP_CHECKLOCKTIMEVERIFY)
        .push_opcode(Opcode::OP_DROP)
        .push_bytes(sender_pub)
        .push_opcode(Opcode::OP_CHECKSIG)
        .push_opcode(Opcode::OP_ELSE)
        .push_bytes(watcher_pub)
        .push_opcode(Opcode::OP_CHECKSIG)
        .push_opcode(Opcode::OP_ENDIF)
        .into_script()
}

pub async fn can_refund_htlc<T>(coin: &T, locktime: u64) -> Result<CanRefundHtlc, MmError<UtxoRpcError>>
where
    T: UtxoCommonOps,
{
    let now = now_ms() / 1000;
    if now < locktime {
        let to_wait = locktime - now + 1;
        return Ok(CanRefundHtlc::HaveToWait(to_wait.max(3600)));
    }

    let mtp = coin.get_current_mtp().await?;
    let locktime = coin.p2sh_tx_locktime(locktime as u32).await?;

    if locktime < mtp {
        Ok(CanRefundHtlc::CanRefundNow)
    } else {
        let to_wait = (locktime - mtp + 1) as u64;
        Ok(CanRefundHtlc::HaveToWait(to_wait.max(3600)))
    }
}

pub async fn p2sh_tx_locktime<T>(coin: &T, ticker: &str, htlc_locktime: u32) -> Result<u32, MmError<UtxoRpcError>>
where
    T: UtxoCommonOps,
{
    let lock_time = if ticker == "KMD" {
        (now_ms() / 1000) as u32 - 3600 + 2 * 777
    } else {
        coin.get_current_mtp().await? - 1
    };
    Ok(lock_time.max(htlc_locktime))
}

pub fn get_htlc_key_pair<T>(coin: &T) -> Option<KeyPair>
where
    T: AsRef<UtxoCoinFields>,
{
    match &coin.as_ref().priv_key_policy {
        PrivKeyPolicy::KeyPair(_) | PrivKeyPolicy::HDWallet { .. } => None,
        PrivKeyPolicy::Trezor => Some(KeyPair::random_compressed()),
    }
}

// ─── V2 swap helpers (chapter 15 §15.4) ─────────────────────────────────────
//
// These mirror the V1 `send_*`/`refund_*`/`validate_*`/`spend_*` helpers
// above but use the V2 maker-payment script (`swap_proto_v2_scripts`) and the
// V2 trait argument structs from `lp_coins_types`.

use crate::utxo::swap_proto_v2_scripts::{maker_payment_script, taker_funding_script, taker_payment_script};
use crate::utxo::utxo_standard_swap_v2::UtxoTxPreimage;
use crate::{FindPaymentSpendError, FundingTxSpend, GenPreimageResult, GenTakerFundingSpendArgs,
            GenTakerPaymentSpendArgs, RefundFundingSecretArgs, RefundMakerPaymentSecretArgs,
            RefundMakerPaymentTimelockArgs, RefundTakerPaymentArgs, SearchForFundingSpendErr, SendMakerPaymentArgs,
            SendTakerFundingArgs, SpendMakerPaymentArgs, SwapTxTypeWithSecretHash, TxGenError, TxPreimageWithSig,
            ValidateMakerPaymentArgs, ValidateSwapV2TxError, ValidateSwapV2TxResult, ValidateTakerFundingArgs,
            ValidateTakerFundingSpendPreimageError, ValidateTakerFundingSpendPreimageResult,
            ValidateTakerPaymentSpendPreimageError, ValidateTakerPaymentSpendPreimageResult};
use crypto::derive_secp256k1_secret;

/// Derives the maker/taker per-swap HTLC public key for V2 swaps.
///
/// For single-key activation this is the activated public key. For HD and
/// hardware activation this is the enabled external address public key cached
/// in the HD account metadata. The `_swap_unique_data` argument is accepted to
/// match the future per-swap derivation surface, but is not consulted yet.
pub fn get_htlc_pubkey_v2<T>(coin: &T, _swap_unique_data: &[u8]) -> Result<Public, String>
where
    T: AsRef<UtxoCoinFields>,
{
    let fields = coin.as_ref();
    match (&fields.derivation_method, &fields.priv_key_policy) {
        (_, PrivKeyPolicy::KeyPair(kp)) => Ok(*kp.public()),
        (DerivationMethod::HDWallet(hd_wallet), PrivKeyPolicy::HDWallet { .. })
        | (DerivationMethod::HDWallet(hd_wallet), PrivKeyPolicy::Trezor) => {
            crate::utxo::utxo_standard_swap_v2::try_enabled_hd_address_info(
                fields,
                hd_wallet,
                "UTXO Standard Swap V2 HTLC public-key derivation",
            )
            .map(|info| info.pubkey)
        },
        (DerivationMethod::Iguana(_), PrivKeyPolicy::HDWallet { .. }) => {
            Err("UTXO Standard Swap V2 HD HTLC public-key derivation requires an HD derivation method".to_owned())
        },
        (DerivationMethod::Iguana(_), PrivKeyPolicy::Trezor) => Err(
            crate::utxo::utxo_standard_swap_v2::trezor_v2_missing_derivation_metadata_error(
                "UTXO Standard Swap V2 HTLC public-key derivation",
            ),
        ),
    }
}

/// Derives the maker/taker per-swap HTLC software keypair for V2 local spends.
///
/// Hardware-backed swaps intentionally do not expose or synthesize host private
/// keys. Until the Trezor signer supports arbitrary P2SH HTLC script inputs,
/// the first local HTLC signing step fails with a structured unsupported mode.
pub fn get_htlc_key_pair_v2<T>(coin: &T, _swap_unique_data: &[u8]) -> Result<KeyPair, String>
where
    T: AsRef<UtxoCoinFields>,
{
    let fields = coin.as_ref();
    match (&fields.derivation_method, &fields.priv_key_policy) {
        (_, PrivKeyPolicy::KeyPair(kp)) => Ok(*kp),
        (
            DerivationMethod::HDWallet(hd_wallet),
            PrivKeyPolicy::HDWallet {
                bip39_secp_priv_key, ..
            },
        ) => {
            let active = crate::utxo::utxo_standard_swap_v2::try_enabled_hd_address_info(
                fields,
                hd_wallet,
                "UTXO Standard Swap V2 HTLC software key derivation",
            )?;
            let secret = derive_secp256k1_secret(bip39_secp_priv_key.clone(), &active.derivation_path)
                .map_err(|e| e.to_string())?;
            key_pair_from_secret(secret.as_slice()).map_err(|e| e.to_string())
        },
        (DerivationMethod::Iguana(_), PrivKeyPolicy::HDWallet { .. }) => {
            Err("UTXO Standard Swap V2 HD HTLC key derivation requires an HD derivation method".to_owned())
        },
        (_, PrivKeyPolicy::Trezor) => {
            Err(crate::utxo::utxo_standard_swap_v2::trezor_v2_unsupported_script_signing_error())
        },
    }
}

/// §15.4.1 — Build and broadcast the maker-payment V2 transaction.
pub async fn send_maker_payment_v2<T>(
    coin: T,
    args: SendMakerPaymentArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    let htlc_pub = try_tx_s!(get_htlc_pubkey_v2(&coin, args.swap_unique_data));
    let redeem = maker_payment_script(
        args.time_lock as u32,
        args.maker_secret_hash,
        args.taker_secret_hash,
        &htlc_pub,
        args.taker_pub,
    );
    let amount_sat = try_tx_s!(sat_from_big_decimal(&args.amount, coin.as_ref().decimals));
    let htlc_out = TransactionOutput {
        value: amount_sat,
        script_pubkey: Builder::build_p2sh(&dhash160(&redeem).into()).into(),
    };
    // Mirror the V1 OP_RETURN convention: record the maker_secret_hash on-chain
    // so wallet recovery flows can locate swap outputs without out-of-band data.
    let op_return = TransactionOutput {
        value: 0,
        script_pubkey: Builder::default()
            .push_opcode(Opcode::OP_RETURN)
            .push_bytes(args.maker_secret_hash)
            .into_bytes(),
    };
    send_outputs_from_my_address_impl(coin, vec![htlc_out, op_return]).await
}

/// §15.4.2 — Validate a received maker-payment V2 transaction.
pub async fn validate_maker_payment_v2<T>(
    coin: &T,
    args: ValidateMakerPaymentArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
) -> ValidateSwapV2TxResult
where
    T: UtxoCommonOps,
{
    let expected_amount_sat = sat_from_big_decimal(&args.amount, coin.as_ref().decimals)
        .mm_err(|e| ValidateSwapV2TxError::InternalError(e.to_string()))?;
    // Spec layout: first_pub = maker_pub, second_pub = taker_pub (per
    // `SwapTxTypeWithSecretHash::redeem_script` dispatch table).
    // The taker pub is recovered from args.maker_pub on the validator side?
    // No — validators know both pubs from the swap context. Per §15.4.2 the
    // verifier passes maker_pub and reconstructs the taker pub from
    // its own HTLC keypair (the validator is the taker).
    let taker_htlc_pub =
        get_htlc_pubkey_v2(coin, args.swap_unique_data).map_to_mm(ValidateSwapV2TxError::InternalError)?;
    let tx_type = SwapTxTypeWithSecretHash::MakerPaymentV2 {
        maker_secret_hash: args.maker_secret_hash,
        taker_secret_hash: args.taker_secret_hash,
    };
    let expected_redeem = tx_type.redeem_script(args.time_lock as u32, args.maker_pub, &taker_htlc_pub);
    let expected_script_pubkey: Bytes = Builder::build_p2sh(&dhash160(&expected_redeem).into()).into();

    let actual = args
        .maker_payment_tx
        .outputs
        .get(DEFAULT_SWAP_VOUT)
        .ok_or_else(|| MmError::new(ValidateSwapV2TxError::WrongPaymentTx("missing output 0".to_string())))?;
    if actual.script_pubkey != expected_script_pubkey {
        return MmError::err(ValidateSwapV2TxError::WrongPaymentTx(format!(
            "script mismatch: expected {:?}, got {:?}",
            expected_script_pubkey, actual.script_pubkey
        )));
    }
    if actual.value != expected_amount_sat {
        return MmError::err(ValidateSwapV2TxError::WrongPaymentTx(format!(
            "amount mismatch: expected {}, got {}",
            expected_amount_sat, actual.value
        )));
    }
    Ok(())
}

/// Common spend-path machinery shared by §15.4.3 / §15.4.4 / §15.4.5: build a
/// P2SH spending tx with the supplied `script_data` selector and broadcast it.
async fn build_and_broadcast_maker_v2_spend<T>(
    coin: &T,
    prev_tx_bytes: &[u8],
    redeem: Script,
    script_data: Script,
    sequence: u32,
    lock_time: u32,
    keypair: &KeyPair,
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps,
{
    let mut prev_tx: UtxoTx = try_tx_s!(deserialize(prev_tx_bytes).map_err(|e| ERRL!("{:?}", e)));
    prev_tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
    let my_address = match &coin.as_ref().derivation_method {
        DerivationMethod::Iguana(address) => address.clone(),
        DerivationMethod::HDWallet(hd_wallet) => {
            try_tx_s!(
                crate::utxo::utxo_standard_swap_v2::enabled_hd_address_info(
                    coin.as_ref(),
                    hd_wallet,
                    "UTXO Standard Swap V2 HTLC spend output address selection",
                )
                .await
            )
            .address
        },
    };

    let fee = try_tx_s!(coin.get_htlc_spend_fee(DEFAULT_SWAP_TX_SPEND_SIZE).await);
    let script_pubkey = output_script(&my_address, ScriptType::P2PKH).to_bytes();
    let output = TransactionOutput {
        value: prev_tx.outputs[DEFAULT_SWAP_VOUT].value - fee,
        script_pubkey,
    };

    let transaction = try_tx_s!(
        coin.p2sh_spending_tx(
            prev_tx,
            redeem.into(),
            vec![output],
            script_data,
            sequence,
            lock_time,
            keypair,
        )
        .await
    );

    let tx_fut = coin.as_ref().rpc_client.send_transaction(&transaction).compat();
    try_tx_s!(tx_fut.await, transaction);
    Ok(transaction)
}

/// §15.4.3 — Maker timelock-refund of the maker payment.
pub async fn refund_maker_payment_v2_timelock<T>(
    coin: &T,
    args: RefundMakerPaymentTimelockArgs<'_>,
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps,
{
    let htlc_kp = try_tx_s!(get_htlc_key_pair_v2(coin, args.swap_unique_data));
    let taker_pub = try_tx_s!(Public::from_slice(args.taker_pub));
    // Use the dispatch table — both branches of the V2 redeem script live in
    // `SwapTxTypeWithSecretHash::redeem_script` so refund paths share the
    // script construction with `validate_*`.
    let redeem = args
        .tx_type_with_secret_hash
        .redeem_script(args.time_lock as u32, htlc_kp.public(), &taker_pub);
    let script_data = Builder::default().push_opcode(Opcode::OP_1).into_script();
    build_and_broadcast_maker_v2_spend(
        coin,
        args.payment_tx,
        redeem,
        script_data,
        SEQUENCE_FINAL - 1,
        args.time_lock as u32,
        &htlc_kp,
    )
    .await
}

/// §15.4.4 — Maker immediate refund by revealing the taker's secret.
pub async fn refund_maker_payment_v2_secret<T>(
    coin: &T,
    args: RefundMakerPaymentSecretArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps,
{
    let htlc_kp = try_tx_s!(get_htlc_key_pair_v2(coin, args.swap_unique_data));
    let tx_type = SwapTxTypeWithSecretHash::MakerPaymentV2 {
        maker_secret_hash: args.maker_secret_hash,
        taker_secret_hash: args.taker_secret_hash,
    };
    let redeem = tx_type.redeem_script(args.time_lock as u32, htlc_kp.public(), args.taker_pub);
    // Selects the maker-refund-with-taker-secret branch (outer ELSE → inner ELSE).
    let script_data = Builder::default()
        .push_data(args.taker_secret.as_slice())
        .push_opcode(Opcode::OP_0)
        .push_opcode(Opcode::OP_0)
        .into_script();
    let prev_bytes = serialize(args.maker_payment_tx).take();
    build_and_broadcast_maker_v2_spend(coin, &prev_bytes, redeem, script_data, SEQUENCE_FINAL, 0, &htlc_kp).await
}

/// §15.4.5 — Taker spends the maker payment by revealing the maker secret.
pub async fn spend_maker_payment_v2<T>(
    coin: &T,
    args: SpendMakerPaymentArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps,
{
    let htlc_kp = try_tx_s!(get_htlc_key_pair_v2(coin, args.swap_unique_data));
    // Note: in this branch the taker is the spender, the maker provided the
    // payment. The script's `(first_pub, second_pub)` for `MakerPaymentV2` is
    // `(maker_pub, taker_pub)`, so:
    let redeem = maker_payment_script(
        args.time_lock as u32,
        args.maker_secret_hash,
        args.taker_secret_hash,
        args.maker_pub,
        htlc_kp.public(),
    );
    // Selects the taker-spends-with-maker-secret branch (outer ELSE → inner IF).
    let script_data = Builder::default()
        .push_data(&args.maker_secret)
        .push_opcode(Opcode::OP_1)
        .push_opcode(Opcode::OP_0)
        .into_script();
    let prev_bytes = serialize(args.maker_payment_tx).take();
    build_and_broadcast_maker_v2_spend(coin, &prev_bytes, redeem, script_data, SEQUENCE_FINAL, 0, &htlc_kp).await
}

// ─── V2 taker-funding helpers (chapter 15 §15.5.1 — §15.5.5, §15.5.15) ─────

/// §15.5.5 classification tags. Public(crate) to enable unit tests that
/// fabricate a script_sig without going through RPC.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FundingSpendBranchTag {
    Timelock,
    Secret([u8; 32]),
    Cooperative,
}

/// Inspect a funding-spend tx's input[0] script_sig at instruction index 1
/// (immediately after the spender's signature push) and tell which branch
/// of the V2 funding redeem script was taken.
pub(crate) fn classify_funding_spend_script_sig(script_sig: &Bytes) -> FundingSpendBranchTag {
    let script: Script = script_sig.clone().into();
    match script.get_instruction(1) {
        Some(Ok(instr)) => match instr.opcode {
            Opcode::OP_1 => FundingSpendBranchTag::Timelock,
            Opcode::OP_PUSHBYTES_32 => match instr.data {
                Some(d) if d.len() == 32 => {
                    let mut secret = [0u8; 32];
                    secret.copy_from_slice(d);
                    FundingSpendBranchTag::Secret(secret)
                },
                _ => FundingSpendBranchTag::Cooperative,
            },
            _ => FundingSpendBranchTag::Cooperative,
        },
        _ => FundingSpendBranchTag::Cooperative,
    }
}

/// §15.5.1 — Build and broadcast the taker funding V2 transaction.
pub async fn send_taker_funding<T>(coin: T, args: SendTakerFundingArgs<'_>) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    let htlc_pub = try_tx_s!(get_htlc_pubkey_v2(&coin, args.swap_unique_data));
    let maker_pub = try_tx_s!(Public::from_slice(args.maker_pub));
    let redeem = taker_funding_script(
        args.funding_time_lock as u32,
        args.taker_secret_hash,
        &htlc_pub,
        &maker_pub,
    );
    let total = &args.trading_amount + &args.premium_amount + &args.dex_fee.fee_amount().to_decimal();
    let amount_sat = try_tx_s!(sat_from_big_decimal(&total, coin.as_ref().decimals));
    let htlc_out = TransactionOutput {
        value: amount_sat,
        script_pubkey: Builder::build_p2sh(&dhash160(&redeem).into()).into(),
    };
    // Mirror V1/V2 OP_RETURN convention: record taker_secret_hash on-chain for
    // wallet-recovery discoverability.
    let op_return = TransactionOutput {
        value: 0,
        script_pubkey: Builder::default()
            .push_opcode(Opcode::OP_RETURN)
            .push_bytes(args.taker_secret_hash)
            .into_bytes(),
    };
    send_outputs_from_my_address_impl(coin, vec![htlc_out, op_return]).await
}

/// §15.5.2 — Validate a received taker funding V2 transaction.
pub async fn validate_taker_funding<T>(
    coin: &T,
    args: ValidateTakerFundingArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
) -> ValidateSwapV2TxResult
where
    T: UtxoCommonOps,
{
    let maker_htlc_pub =
        get_htlc_pubkey_v2(coin, args.swap_unique_data).map_to_mm(ValidateSwapV2TxError::InternalError)?;
    let expected_redeem = taker_funding_script(
        args.funding_time_lock as u32,
        args.taker_secret_hash,
        args.taker_pub,
        &maker_htlc_pub,
    );
    let expected_script_pubkey: Bytes = Builder::build_p2sh(&dhash160(&expected_redeem).into()).into();

    let total = &args.trading_amount + &args.premium_amount + &args.dex_fee.fee_amount().to_decimal();
    let expected_amount_sat = sat_from_big_decimal(&total, coin.as_ref().decimals)
        .mm_err(|e| ValidateSwapV2TxError::InternalError(e.to_string()))?;

    let actual = args
        .funding_tx
        .outputs
        .get(DEFAULT_SWAP_VOUT)
        .ok_or_else(|| MmError::new(ValidateSwapV2TxError::WrongPaymentTx("missing output 0".to_string())))?;
    if actual.script_pubkey != expected_script_pubkey {
        return MmError::err(ValidateSwapV2TxError::WrongPaymentTx(format!(
            "script mismatch: expected {:?}, got {:?}",
            expected_script_pubkey, actual.script_pubkey
        )));
    }
    if actual.value != expected_amount_sat {
        return MmError::err(ValidateSwapV2TxError::WrongPaymentTx(format!(
            "amount mismatch: expected {}, got {}",
            expected_amount_sat, actual.value
        )));
    }

    // §15.5.2 step 5: on native mode, import the P2SH address so the node
    // tracks spends. Best-effort — log and continue on failure.
    if let UtxoRpcClientEnum::Native(client) = &coin.as_ref().rpc_client {
        let p2sh_addr = Address {
            checksum_type: coin.as_ref().conf.checksum_type,
            hash: dhash160(&expected_redeem).into(),
            prefix: coin.as_ref().conf.p2sh_addr_prefix,
            t_addr_prefix: coin.as_ref().conf.p2sh_t_addr_prefix,
            hrp: coin.as_ref().conf.bech32_hrp.clone(),
            addr_format: UtxoAddressFormat::Standard,
        };
        if let Ok(addr_string) = p2sh_addr.display_address() {
            if let Err(e) = client.import_address(&addr_string, &addr_string, false).compat().await {
                log!("validate_taker_funding: import_address failed: "[e]);
            }
        }
    }

    Ok(())
}

/// §15.5.3 — Taker timelock-refund of the funding payment.
pub async fn refund_taker_funding_timelock<T>(
    coin: &T,
    args: RefundTakerPaymentArgs<'_>,
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps,
{
    let htlc_kp = try_tx_s!(get_htlc_key_pair_v2(coin, args.swap_unique_data));
    let maker_pub = try_tx_s!(Public::from_slice(args.maker_pub));
    // `TakerFunding` redeem-script dispatch uses (taker_pub, maker_pub).
    let redeem = args
        .tx_type_with_secret_hash
        .redeem_script(args.time_lock as u32, htlc_kp.public(), &maker_pub);
    // Outer-IF true arm selects the timelock branch.
    let script_data = Builder::default()
        .push_opcode(Opcode::OP_1)
        .push_opcode(Opcode::OP_0)
        .into_script();
    build_and_broadcast_maker_v2_spend(
        coin,
        args.payment_tx,
        redeem,
        script_data,
        SEQUENCE_FINAL - 1,
        args.time_lock as u32,
        &htlc_kp,
    )
    .await
}

/// §15.5.4 — Taker immediate refund of the funding payment by revealing the taker's own secret.
pub async fn refund_taker_funding_secret<T>(
    coin: &T,
    args: RefundFundingSecretArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps,
{
    let htlc_kp = try_tx_s!(get_htlc_key_pair_v2(coin, args.swap_unique_data));
    let redeem = taker_funding_script(
        args.funding_time_lock as u32,
        args.taker_secret_hash,
        htlc_kp.public(),
        args.maker_pubkey,
    );
    // Outer-ELSE / inner-ELSE branch: reveal taker secret then prove ownership.
    let script_data = Builder::default()
        .push_data(args.taker_secret.as_slice())
        .push_opcode(Opcode::OP_0)
        .push_opcode(Opcode::OP_0)
        .into_script();
    let prev_bytes = serialize(args.funding_tx).take();
    build_and_broadcast_maker_v2_spend(coin, &prev_bytes, redeem, script_data, SEQUENCE_FINAL, 0, &htlc_kp).await
}

/// §15.5.5 — Locate and classify the spend of a taker funding output.
pub async fn search_for_taker_funding_spend<T>(
    coin: &T,
    tx: &UtxoTx,
    from_block: u64,
    _secret_hash: &[u8],
) -> Result<Option<FundingTxSpend<crate::utxo::utxo_standard::UtxoStandardCoin>>, SearchForFundingSpendErr>
where
    T: UtxoCommonOps,
{
    let output = tx
        .outputs
        .get(DEFAULT_SWAP_VOUT)
        .ok_or_else(|| SearchForFundingSpendErr::InvalidInputTx("missing funding output 0".to_string()))?;
    let spent = coin
        .as_ref()
        .rpc_client
        .find_output_spend(
            tx.hash(),
            &output.script_pubkey,
            DEFAULT_SWAP_VOUT,
            BlockHashOrHeight::Height(from_block as i64),
        )
        .compat()
        .await
        .map_err(SearchForFundingSpendErr::Rpc)?;
    let spent = match spent {
        Some(s) => s,
        None => return Ok(None),
    };
    let mut spend_tx = spent.spending_tx;
    spend_tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
    let input = spend_tx
        .inputs
        .get(DEFAULT_SWAP_VOUT)
        .ok_or_else(|| SearchForFundingSpendErr::FailedToProcessSpendTx("spend tx has no inputs".to_string()))?;
    let branch = classify_funding_spend_script_sig(&input.script_sig);
    let res = match branch {
        FundingSpendBranchTag::Timelock => FundingTxSpend::RefundedTimelock(spend_tx),
        FundingSpendBranchTag::Secret(secret) => FundingTxSpend::RefundedSecret { tx: spend_tx, secret },
        FundingSpendBranchTag::Cooperative => FundingTxSpend::TransferredToTakerPayment(spend_tx),
    };
    Ok(Some(res))
}

/// §15.5.15 — Recover the 32-byte protocol secret from a spend transaction
/// by walking its input[0] script_sig instructions and matching the push
/// whose `dhash160` equals `secret_hash`.
pub fn extract_secret_v2(secret_hash: &[u8], spend_tx: &UtxoTx) -> Result<[u8; 32], String> {
    let input = spend_tx
        .inputs
        .first()
        .ok_or_else(|| "Spend tx has no inputs".to_string())?;
    let script: Script = input.script_sig.clone().into();
    for instr in script.iter().flatten() {
        if instr.opcode != Opcode::OP_PUSHBYTES_32 {
            continue;
        }
        let push = match instr.data {
            Some(d) if d.len() == 32 => d,
            _ => continue,
        };
        if dhash160(push).as_slice() == secret_hash {
            let mut out = [0u8; 32];
            out.copy_from_slice(push);
            return Ok(out);
        }
    }
    Err("Secret not found in spend transaction".to_string())
}

// ─── V2 cooperative funding-spend helpers (chapter 15 §15.5.6 — §15.5.9) ───
//
// The funding-spend converts the taker-funding UTXO into a taker-payment
// P2SH output. The funding script's cooperative branch requires both
// maker_sig and taker_sig (no OP_CHECKMULTISIG — sequential
// CHECKSIGVERIFY/CHECKSIG), so this is a two-party exchange:
//
//   1. Party A (`gen_taker_funding_spend_preimage`) builds the unsigned
//      spend tx and signs the input with its own HTLC keypair, returning
//      `TxPreimageWithSig { preimage, signature: a_sig }`.
//   2. Party B (`validate_taker_funding_spend_preimage`) re-derives the
//      same skeleton (allowing ±10% on the fee output) and verifies
//      `a_sig` against the cooperative-branch sighash for the funding
//      redeem script.
//   3. Party B (`sign_and_send_taker_funding_spend`) signs with its own
//      HTLC keypair, splices both sigs + branch-flags + redeem into a
//      script_sig (maker_sig at the bottom of the stack), and broadcasts.
//
// "A" / "B" labels: the `MakerCoinSwapOpsV2` / `TakerCoinSwapOpsV2` trait
// doc-comments specify A = maker, B = taker, but the underlying mechanics
// are symmetric — `get_htlc_key_pair_v2` always returns the calling
// instance's keypair, and the counterpart pub is read from `args`.

/// Build the unsigned funding-spend preimage with a single P2SH output bound
/// to the taker-payment redeem script (§15.5.6 step 3). Pure / sync so that
/// unit tests can drive it without RPC; the async wrapper computes `fee`
/// via `get_htlc_spend_fee` first.
pub(crate) fn build_funding_spend_preimage_tx(
    fields: &UtxoCoinFields,
    funding_tx: &UtxoTx,
    taker_payment_redeem: &Script,
    fee: u64,
) -> Result<TransactionInputSigner, TxGenError> {
    let funding_output = funding_tx
        .outputs
        .get(DEFAULT_SWAP_VOUT)
        .ok_or_else(|| TxGenError::PrevTxIsNotValid("funding tx has no output 0".to_string()))?;
    if funding_output.value <= fee {
        return Err(TxGenError::PrevOutputTooLow);
    }
    let output = TransactionOutput {
        value: funding_output.value - fee,
        script_pubkey: Builder::build_p2sh(&dhash160(taker_payment_redeem).into()).into(),
    };
    let n_time = if fields.conf.is_pos {
        Some((now_ms() / 1000) as u32)
    } else {
        None
    };
    Ok(TransactionInputSigner {
        version: fields.conf.tx_version,
        n_time,
        overwintered: fields.conf.overwintered,
        inputs: vec![UnsignedTransactionInput {
            sequence: SEQUENCE_FINAL,
            previous_output: OutPoint {
                hash: funding_tx.hash(),
                index: DEFAULT_SWAP_VOUT as u32,
            },
            amount: funding_output.value,
            witness: Vec::new(),
        }],
        outputs: vec![output],
        lock_time: 0,
        expiry_height: 0,
        join_splits: vec![],
        shielded_spends: vec![],
        shielded_outputs: vec![],
        value_balance: 0,
        version_group_id: fields.conf.version_group_id,
        consensus_branch_id: fields.conf.consensus_branch_id,
        zcash: fields.conf.zcash,
        str_d_zeel: None,
        hash_algo: fields.tx_hash_algo.into(),
    })
}

/// Sign input 0 of `signer` over the cooperative branch of the funding redeem
/// script with SIGHASH_ALL (§15.5.6 step 5). Returns the raw DER signature
/// — the trailing sighash byte is added at script_sig assembly time.
pub(crate) fn sign_funding_spend_input(
    signer: &TransactionInputSigner,
    funding_redeem: &Script,
    keypair: &KeyPair,
    fields: &UtxoCoinFields,
) -> Result<keys::Signature, String> {
    let sighash_type = 1u32 | fields.conf.fork_id;
    let digest = signer.signature_hash(
        DEFAULT_SWAP_VOUT,
        signer.inputs[DEFAULT_SWAP_VOUT].amount,
        funding_redeem,
        fields.conf.signature_version,
        sighash_type,
    );
    keypair.private().sign(&digest).map_err(|e| e.to_string())
}

/// Build the cooperative-branch script_sig of a funding-spend. The script's
/// inner-IF body runs `<taker_pub> CHECKSIGVERIFY <maker_pub> CHECKSIG`,
/// which consumes the stack `[..., maker_sig, taker_sig]`. With branch flags
/// `OP_1`/`OP_0` on top, the spender's stack contributions become (bottom→top):
/// `maker_sig, taker_sig, OP_1 (inner-IF), OP_0 (outer-ELSE)`. The script is
/// not OP_CHECKMULTISIG, so the leading OP_0 stuffer is omitted.
pub(crate) fn build_funding_spend_cooperative_script_sig(
    maker_sig_der: &[u8],
    taker_sig_der: &[u8],
    fork_id: u32,
    funding_redeem: &Script,
) -> Bytes {
    let sighash_byte = (1u32 | fork_id) as u8;
    let mut maker_sig = maker_sig_der.to_vec();
    maker_sig.push(sighash_byte);
    let mut taker_sig = taker_sig_der.to_vec();
    taker_sig.push(sighash_byte);
    Builder::default()
        .push_data(&maker_sig)
        .push_data(&taker_sig)
        .push_opcode(Opcode::OP_1)
        .push_opcode(Opcode::OP_0)
        .push_data(funding_redeem)
        .into_script()
        .to_bytes()
}

/// §15.5.6 — Generate the funding-spend preimage and the caller's
/// partial signature over the cooperative branch.
pub async fn gen_taker_funding_spend_preimage<T>(
    coin: &T,
    args: &GenTakerFundingSpendArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
    swap_unique_data: &[u8],
) -> GenPreimageResult<crate::utxo::utxo_standard::UtxoStandardCoin>
where
    T: UtxoCommonOps,
{
    let htlc_kp = get_htlc_key_pair_v2(coin, swap_unique_data).map_to_mm(TxGenError::Signing)?;
    let taker_payment_redeem = taker_payment_script(
        args.taker_payment_time_lock as u32,
        args.maker_secret_hash,
        args.taker_pub,
        args.maker_pub,
    );
    let funding_redeem = taker_funding_script(
        args.funding_time_lock as u32,
        args.taker_secret_hash,
        args.taker_pub,
        args.maker_pub,
    );
    let fee = coin
        .get_htlc_spend_fee(DEFAULT_SWAP_TX_SPEND_SIZE)
        .await
        .mm_err(|e| TxGenError::Rpc(e.to_string()))?;
    let signer =
        build_funding_spend_preimage_tx(coin.as_ref(), args.funding_tx, &taker_payment_redeem, fee).map_to_mm(|e| e)?;
    let sig =
        sign_funding_spend_input(&signer, &funding_redeem, &htlc_kp, coin.as_ref()).map_to_mm(TxGenError::Signing)?;
    Ok(TxPreimageWithSig {
        preimage: UtxoTxPreimage(signer),
        signature: sig,
    })
}

/// §15.5.7 — Validate a received funding-spend preimage and the
/// counterparty's partial signature.
pub async fn validate_taker_funding_spend_preimage<T>(
    coin: &T,
    gen_args: &GenTakerFundingSpendArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
    preimage: &TxPreimageWithSig<crate::utxo::utxo_standard::UtxoStandardCoin>,
) -> ValidateTakerFundingSpendPreimageResult
where
    T: UtxoCommonOps,
{
    let funding_output = gen_args.funding_tx.outputs.get(DEFAULT_SWAP_VOUT).ok_or_else(|| {
        MmError::new(ValidateTakerFundingSpendPreimageError::InvalidPreimage(
            "funding tx has no output 0".to_string(),
        ))
    })?;
    let expected_fee = coin
        .get_htlc_spend_fee(DEFAULT_SWAP_TX_SPEND_SIZE)
        .await
        .mm_err(|e| ValidateTakerFundingSpendPreimageError::InternalError(e.to_string()))?;
    let expected_value = funding_output.value.checked_sub(expected_fee).ok_or_else(|| {
        MmError::new(ValidateTakerFundingSpendPreimageError::InvalidPreimage(
            "funding value below expected fee".to_string(),
        ))
    })?;

    let signer = &preimage.preimage.0;
    if signer.inputs.len() != 1
        || signer.inputs[0].previous_output.hash != gen_args.funding_tx.hash()
        || signer.inputs[0].previous_output.index != DEFAULT_SWAP_VOUT as u32
    {
        return MmError::err(ValidateTakerFundingSpendPreimageError::InvalidPreimage(
            "preimage input does not spend the funding outpoint".to_string(),
        ));
    }
    if signer.outputs.len() != 1 {
        return MmError::err(ValidateTakerFundingSpendPreimageError::InvalidPreimage(format!(
            "expected 1 output in preimage, got {}",
            signer.outputs.len()
        )));
    }
    let taker_payment_redeem = taker_payment_script(
        gen_args.taker_payment_time_lock as u32,
        gen_args.maker_secret_hash,
        gen_args.taker_pub,
        gen_args.maker_pub,
    );
    let expected_script_pubkey: Bytes = Builder::build_p2sh(&dhash160(&taker_payment_redeem).into()).into();
    if signer.outputs[0].script_pubkey != expected_script_pubkey {
        return MmError::err(ValidateTakerFundingSpendPreimageError::InvalidPreimage(
            "preimage output script does not match taker-payment P2SH".to_string(),
        ));
    }
    // ±10% fee tolerance on the output value.
    let actual_value = signer.outputs[0].value;
    let diff = actual_value.abs_diff(expected_value);
    let tolerance = expected_value / 10;
    if diff > tolerance {
        return MmError::err(ValidateTakerFundingSpendPreimageError::InvalidPreimage(format!(
            "preimage output value {} differs from expected {} by more than 10% (fee tolerance)",
            actual_value, expected_value
        )));
    }

    // Verify the supplied signature against the counterparty's pub. The
    // caller's own keypair tells us which side they are.
    let htlc_pub = get_htlc_pubkey_v2(coin, b"").map_to_mm(ValidateTakerFundingSpendPreimageError::InternalError)?;
    let counterparty_pub = if &htlc_pub == gen_args.taker_pub {
        gen_args.maker_pub
    } else {
        gen_args.taker_pub
    };
    let funding_redeem = taker_funding_script(
        gen_args.funding_time_lock as u32,
        gen_args.taker_secret_hash,
        gen_args.taker_pub,
        gen_args.maker_pub,
    );
    let sighash_type = 1u32 | coin.as_ref().conf.fork_id;
    let digest = signer.signature_hash(
        DEFAULT_SWAP_VOUT,
        signer.inputs[DEFAULT_SWAP_VOUT].amount,
        &funding_redeem,
        coin.as_ref().conf.signature_version,
        sighash_type,
    );
    let ok = counterparty_pub
        .verify(&digest, &preimage.signature)
        .map_to_mm(|e| ValidateTakerFundingSpendPreimageError::InternalError(e.to_string()))?;
    if !ok {
        return MmError::err(ValidateTakerFundingSpendPreimageError::InvalidPreimage(
            "counterparty signature does not verify against cooperative branch".to_string(),
        ));
    }
    Ok(())
}

/// §15.5.8 — Add the caller's signature on top of the validated preimage and
/// broadcast the finalised funding-spend transaction.
pub async fn sign_and_send_taker_funding_spend<T>(
    coin: &T,
    preimage: &TxPreimageWithSig<crate::utxo::utxo_standard::UtxoStandardCoin>,
    args: &GenTakerFundingSpendArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
    swap_unique_data: &[u8],
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps,
{
    let htlc_kp = try_tx_s!(get_htlc_key_pair_v2(coin, swap_unique_data));
    let funding_redeem = taker_funding_script(
        args.funding_time_lock as u32,
        args.taker_secret_hash,
        args.taker_pub,
        args.maker_pub,
    );
    let signer = &preimage.preimage.0;
    let my_sig = try_tx_s!(sign_funding_spend_input(
        signer,
        &funding_redeem,
        &htlc_kp,
        coin.as_ref()
    ));

    // Determine which side `my_sig` belongs to so the cooperative-branch
    // script_sig keeps `maker_sig` at the bottom of the stack.
    let (maker_sig_der, taker_sig_der): (&[u8], &[u8]) = if htlc_kp.public() == args.maker_pub {
        (my_sig.as_ref(), preimage.signature.as_ref())
    } else {
        (preimage.signature.as_ref(), my_sig.as_ref())
    };
    let script_sig = build_funding_spend_cooperative_script_sig(
        maker_sig_der,
        taker_sig_der,
        coin.as_ref().conf.fork_id,
        &funding_redeem,
    );

    let signed_input = chain::TransactionInput {
        previous_output: signer.inputs[DEFAULT_SWAP_VOUT].previous_output,
        sequence: signer.inputs[DEFAULT_SWAP_VOUT].sequence,
        script_sig,
        script_witness: vec![],
    };
    let mut tx: UtxoTx = signer.clone().into();
    tx.inputs = vec![signed_input];
    tx.tx_hash_algo = coin.as_ref().tx_hash_algo;

    let tx_fut = coin.as_ref().rpc_client.send_transaction(&tx).compat();
    try_tx_s!(tx_fut.await, tx);
    Ok(tx)
}

/// §15.5.9 — Timelock-refund of a taker-payment (post cooperative funding spend).
pub async fn refund_combined_taker_payment<T>(
    coin: &T,
    args: RefundTakerPaymentArgs<'_>,
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps,
{
    let htlc_kp = try_tx_s!(get_htlc_key_pair_v2(coin, args.swap_unique_data));
    let maker_pub = try_tx_s!(Public::from_slice(args.maker_pub));
    // Dispatch table for `TakerPaymentV2` produces the same script as
    // `taker_payment_script(time_lock, maker_secret_hash, first_pub=taker, second_pub=maker)`.
    let redeem = args
        .tx_type_with_secret_hash
        .redeem_script(args.time_lock as u32, htlc_kp.public(), &maker_pub);
    // The taker-payment script has only one OP_IF — `OP_1` selects the
    // timelock-refund branch.
    let script_data = Builder::default().push_opcode(Opcode::OP_1).into_script();
    build_and_broadcast_maker_v2_spend(
        coin,
        args.payment_tx,
        redeem,
        script_data,
        SEQUENCE_FINAL - 1,
        args.time_lock as u32,
        &htlc_kp,
    )
    .await
}

// ─── V2 taker-payment-spend helpers (chapter 15 §15.5.10 — §15.5.14) ───
//
// The taker-payment output (a P2SH-wrapped `taker_payment_script`) is the
// post-cooperative-funding-spend UTXO held by the taker. Its cooperative
// branch (`OP_0`) requires both sigs plus the maker's secret. The
// preimage-exchange flow mirrors the funding-spend coop flow (§15.5.6 —
// §15.5.8) but operates on this taker-payment output:
//
//   1. Taker (`gen_taker_payment_spend_preimage`) builds the unsigned spend
//      tx paying the maker, partial-signs and returns the bundle.
//   2. Maker (`validate_taker_payment_spend_preimage`) re-derives the
//      skeleton and verifies the taker sig.
//   3. Maker (`sign_and_broadcast_taker_payment_spend`) appends the dex-fee
//      output (Standard only), signs, assembles a cooperative-branch
//      script_sig that also pushes the maker secret, and broadcasts.
//
// For `DexFee::Standard` the taker signs with `SIGHASH_SINGLE` so the maker
// can append outputs without invalidating the taker sig. `DexFee::WithBurn`
// fixes every output up-front and uses `SIGHASH_ALL` — that variant is
// deferred to the pre-burn chapter (ch16) and currently returns a
// `TxGenError::Other` describing the deferral.

const SIGHASH_ALL_BASE: u32 = 1;
const SIGHASH_SINGLE_BASE: u32 = 3;

/// Build the unsigned taker-payment-spend preimage with the supplied output
/// list (single maker-bound output for `DexFee::Standard`, or all fixed
/// outputs for fully-locked variants).
pub(crate) fn build_taker_payment_spend_preimage_tx(
    fields: &UtxoCoinFields,
    taker_payment_tx: &UtxoTx,
    outputs: Vec<TransactionOutput>,
) -> Result<TransactionInputSigner, TxGenError> {
    let taker_output = taker_payment_tx
        .outputs
        .get(DEFAULT_SWAP_VOUT)
        .ok_or_else(|| TxGenError::PrevTxIsNotValid("taker-payment tx has no output 0".to_string()))?;
    let n_time = if fields.conf.is_pos {
        Some((now_ms() / 1000) as u32)
    } else {
        None
    };
    Ok(TransactionInputSigner {
        version: fields.conf.tx_version,
        n_time,
        overwintered: fields.conf.overwintered,
        inputs: vec![UnsignedTransactionInput {
            sequence: SEQUENCE_FINAL,
            previous_output: OutPoint {
                hash: taker_payment_tx.hash(),
                index: DEFAULT_SWAP_VOUT as u32,
            },
            amount: taker_output.value,
            witness: Vec::new(),
        }],
        outputs,
        lock_time: 0,
        expiry_height: 0,
        join_splits: vec![],
        shielded_spends: vec![],
        shielded_outputs: vec![],
        value_balance: 0,
        version_group_id: fields.conf.version_group_id,
        consensus_branch_id: fields.conf.consensus_branch_id,
        zcash: fields.conf.zcash,
        str_d_zeel: None,
        hash_algo: fields.tx_hash_algo.into(),
    })
}

/// Sign input 0 of `signer` over the cooperative branch of the taker-payment
/// redeem script with the supplied `sighash_type` (raw 32-bit value already
/// or'd with the fork id). Returns the raw DER signature; the trailing
/// sighash byte is added at script_sig assembly time.
pub(crate) fn sign_taker_payment_spend_input(
    signer: &TransactionInputSigner,
    taker_payment_redeem: &Script,
    keypair: &KeyPair,
    fields: &UtxoCoinFields,
    sighash_type: u32,
) -> Result<keys::Signature, String> {
    let digest = signer.signature_hash(
        DEFAULT_SWAP_VOUT,
        signer.inputs[DEFAULT_SWAP_VOUT].amount,
        taker_payment_redeem,
        fields.conf.signature_version,
        sighash_type,
    );
    keypair.private().sign(&digest).map_err(|e| e.to_string())
}

/// Build the cooperative-branch script_sig of a taker-payment spend. The
/// taker-payment script's inner cooperative body is
/// `OP_SIZE <32> OP_EQUALVERIFY OP_HASH160 <h(secret_hash)> OP_EQUALVERIFY
///  <taker_pub> OP_CHECKSIGVERIFY <maker_pub> OP_CHECKSIG`, fed by an outer
/// OP_IF whose ELSE arm runs cooperative spending. Stack contributions
/// (bottom→top): `maker_sig, taker_sig, maker_secret, OP_0 (selects ELSE)`.
/// The script is not OP_CHECKMULTISIG so no leading OP_0 stuffer.
pub(crate) fn build_taker_payment_spend_cooperative_script_sig(
    maker_sig_der: &[u8],
    taker_sig_der: &[u8],
    maker_secret: &[u8],
    sighash_byte: u8,
    taker_payment_redeem: &Script,
) -> Bytes {
    let mut maker_sig = maker_sig_der.to_vec();
    maker_sig.push(sighash_byte);
    let mut taker_sig = taker_sig_der.to_vec();
    taker_sig.push(sighash_byte);
    Builder::default()
        .push_data(&maker_sig)
        .push_data(&taker_sig)
        .push_data(maker_secret)
        .push_opcode(Opcode::OP_0)
        .push_data(taker_payment_redeem)
        .into_script()
        .to_bytes()
}

/// Resolve the dex-fee P2PKH output for a given `DexFee::Standard` amount.
fn dex_fee_standard_output<T: UtxoCommonOps>(coin: &T, fee_sat: u64) -> Result<TransactionOutput, TxGenError> {
    let fee_address = address_from_raw_pubkey(
        &common::DEX_FEE_ADDR_RAW_PUBKEY,
        coin.as_ref().conf.pub_addr_prefix,
        coin.as_ref().conf.pub_t_addr_prefix,
        coin.as_ref().conf.checksum_type,
        coin.as_ref().conf.bech32_hrp.clone(),
        coin.addr_format().clone(),
    )
    .map_err(TxGenError::Other)?;
    Ok(TransactionOutput {
        value: fee_sat,
        script_pubkey: Builder::build_p2pkh(&fee_address.hash).to_bytes(),
    })
}

/// §16.5.4 — Build the burn-leg output of a `DexFee::WithBurn`
/// taker-payment-spend. `KmdOpReturn` encodes the burned amount as an
/// 8-byte little-endian payload of an `OP_RETURN` (output value is zero).
/// `PreBurnAccount` produces a P2PKH paying the burn address.
fn build_burn_output<T: UtxoCommonOps>(
    coin: &T,
    burn_amount_sat: u64,
    destination: &DexFeeBurnDestination,
) -> Result<TransactionOutput, TxGenError> {
    match destination {
        DexFeeBurnDestination::KmdOpReturn => Ok(TransactionOutput {
            value: 0,
            script_pubkey: Builder::default()
                .push_opcode(Opcode::OP_RETURN)
                .push_bytes(&burn_amount_sat.to_le_bytes())
                .into_bytes(),
        }),
        DexFeeBurnDestination::PreBurnAccount { burn_pubkey } => {
            let burn_address = address_from_raw_pubkey(
                burn_pubkey,
                coin.as_ref().conf.pub_addr_prefix,
                coin.as_ref().conf.pub_t_addr_prefix,
                coin.as_ref().conf.checksum_type,
                coin.as_ref().conf.bech32_hrp.clone(),
                coin.addr_format().clone(),
            )
            .map_err(TxGenError::Other)?;
            Ok(TransactionOutput {
                value: burn_amount_sat,
                script_pubkey: Builder::build_p2pkh(&burn_address.hash).to_bytes(),
            })
        },
    }
}

/// §15.5.11 — Taker builds the unsigned taker-payment-spend preimage and
/// partial-signs it. `DexFee::Standard` uses SIGHASH_SINGLE so the maker can
/// later append the dex-fee output; other variants are deferred to ch16.
pub async fn gen_taker_payment_spend_preimage<T>(
    coin: &T,
    args: &GenTakerPaymentSpendArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
    swap_unique_data: &[u8],
) -> GenPreimageResult<crate::utxo::utxo_standard::UtxoStandardCoin>
where
    T: UtxoCommonOps,
{
    let htlc_kp = get_htlc_key_pair_v2(coin, swap_unique_data).map_to_mm(TxGenError::Signing)?;
    let taker_payment_redeem = taker_payment_script(
        args.time_lock as u32,
        args.maker_secret_hash,
        args.taker_pub,
        args.maker_pub,
    );
    let taker_output = args.taker_tx.outputs.get(DEFAULT_SWAP_VOUT).ok_or_else(|| {
        MmError::new(TxGenError::PrevTxIsNotValid(
            "taker-payment tx has no output 0".to_string(),
        ))
    })?;
    let fee = coin
        .get_htlc_spend_fee(DEFAULT_SWAP_TX_SPEND_SIZE)
        .await
        .mm_err(|e| TxGenError::Rpc(e.to_string()))?;
    let maker_script_pubkey = output_script(args.maker_address, ScriptType::P2PKH).to_bytes();

    let (outputs, sighash_type) = match args.dex_fee {
        DexFee::Standard(amount) => {
            let dex_fee_sat = sat_from_big_decimal(&amount.to_decimal(), coin.as_ref().decimals)
                .map_err(|e| MmError::new(TxGenError::NumConversion(e.to_string())))?;
            let maker_value = taker_output
                .value
                .checked_sub(dex_fee_sat)
                .and_then(|v| v.checked_sub(fee))
                .ok_or_else(|| MmError::new(TxGenError::PrevOutputTooLow))?;
            let outs = vec![TransactionOutput {
                value: maker_value,
                script_pubkey: maker_script_pubkey,
            }];
            (outs, SIGHASH_SINGLE_BASE | coin.as_ref().conf.fork_id)
        },
        DexFee::WithBurn {
            fee_amount,
            burn_amount,
            burn_destination,
        } => {
            let dex_fee_sat = sat_from_big_decimal(&fee_amount.to_decimal(), coin.as_ref().decimals)
                .map_err(|e| MmError::new(TxGenError::NumConversion(e.to_string())))?;
            let burn_sat = sat_from_big_decimal(&burn_amount.to_decimal(), coin.as_ref().decimals)
                .map_err(|e| MmError::new(TxGenError::NumConversion(e.to_string())))?;
            let maker_value = taker_output
                .value
                .checked_sub(dex_fee_sat)
                .and_then(|v| v.checked_sub(burn_sat))
                .and_then(|v| v.checked_sub(fee))
                .ok_or_else(|| MmError::new(TxGenError::PrevOutputTooLow))?;
            let fee_out = dex_fee_standard_output(coin, dex_fee_sat).map_to_mm(|e| e)?;
            let burn_out = build_burn_output(coin, burn_sat, burn_destination).map_to_mm(|e| e)?;
            let outs = vec![
                TransactionOutput {
                    value: maker_value,
                    script_pubkey: maker_script_pubkey,
                },
                fee_out,
                burn_out,
            ];
            (outs, SIGHASH_ALL_BASE | coin.as_ref().conf.fork_id)
        },
        DexFee::NoFee => {
            let maker_value = taker_output
                .value
                .checked_sub(fee)
                .ok_or_else(|| MmError::new(TxGenError::PrevOutputTooLow))?;
            let outs = vec![TransactionOutput {
                value: maker_value,
                script_pubkey: maker_script_pubkey,
            }];
            (outs, SIGHASH_ALL_BASE | coin.as_ref().conf.fork_id)
        },
    };

    let signer = build_taker_payment_spend_preimage_tx(coin.as_ref(), args.taker_tx, outputs).map_to_mm(|e| e)?;
    let sig = sign_taker_payment_spend_input(&signer, &taker_payment_redeem, &htlc_kp, coin.as_ref(), sighash_type)
        .map_to_mm(TxGenError::Signing)?;
    Ok(TxPreimageWithSig {
        preimage: UtxoTxPreimage(signer),
        signature: sig,
    })
}

/// §15.5.12 — Maker validates the taker-payment-spend preimage's shape and
/// the taker's partial signature.
pub async fn validate_taker_payment_spend_preimage<T>(
    coin: &T,
    gen_args: &GenTakerPaymentSpendArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
    preimage: &TxPreimageWithSig<crate::utxo::utxo_standard::UtxoStandardCoin>,
) -> ValidateTakerPaymentSpendPreimageResult
where
    T: UtxoCommonOps,
{
    let taker_output = gen_args.taker_tx.outputs.get(DEFAULT_SWAP_VOUT).ok_or_else(|| {
        MmError::new(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
            "taker-payment tx has no output 0".to_string(),
        ))
    })?;
    let expected_fee = coin
        .get_htlc_spend_fee(DEFAULT_SWAP_TX_SPEND_SIZE)
        .await
        .mm_err(|e| ValidateTakerPaymentSpendPreimageError::InternalError(e.to_string()))?;
    let maker_script_pubkey = output_script(gen_args.maker_address, ScriptType::P2PKH).to_bytes();

    let (expected_outputs_len, expected_maker_value, sighash_type, with_burn_extras) = match gen_args.dex_fee {
        DexFee::Standard(ref amount) => {
            let dex_fee_sat = sat_from_big_decimal(&amount.to_decimal(), coin.as_ref().decimals)
                .map_err(|e| ValidateTakerPaymentSpendPreimageError::InternalError(e.to_string()))?;
            let expected = taker_output
                .value
                .checked_sub(dex_fee_sat)
                .and_then(|v| v.checked_sub(expected_fee))
                .ok_or_else(|| {
                    MmError::new(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
                        "taker-payment value below dex_fee + spend_fee".to_string(),
                    ))
                })?;
            (1usize, expected, SIGHASH_SINGLE_BASE | coin.as_ref().conf.fork_id, None)
        },
        DexFee::WithBurn {
            fee_amount,
            burn_amount,
            burn_destination,
        } => {
            let dex_fee_sat = sat_from_big_decimal(&fee_amount.to_decimal(), coin.as_ref().decimals)
                .map_err(|e| ValidateTakerPaymentSpendPreimageError::InternalError(e.to_string()))?;
            let burn_sat = sat_from_big_decimal(&burn_amount.to_decimal(), coin.as_ref().decimals)
                .map_err(|e| ValidateTakerPaymentSpendPreimageError::InternalError(e.to_string()))?;
            let expected = taker_output
                .value
                .checked_sub(dex_fee_sat)
                .and_then(|v| v.checked_sub(burn_sat))
                .and_then(|v| v.checked_sub(expected_fee))
                .ok_or_else(|| {
                    MmError::new(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
                        "taker-payment value below fee + burn + spend_fee".to_string(),
                    ))
                })?;
            (
                3usize,
                expected,
                SIGHASH_ALL_BASE | coin.as_ref().conf.fork_id,
                Some((dex_fee_sat, burn_sat, burn_destination.clone())),
            )
        },
        DexFee::NoFee => {
            let expected = taker_output.value.checked_sub(expected_fee).ok_or_else(|| {
                MmError::new(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
                    "taker-payment value below spend_fee".to_string(),
                ))
            })?;
            (1usize, expected, SIGHASH_ALL_BASE | coin.as_ref().conf.fork_id, None)
        },
    };

    let signer = &preimage.preimage.0;
    if signer.inputs.len() != 1
        || signer.inputs[0].previous_output.hash != gen_args.taker_tx.hash()
        || signer.inputs[0].previous_output.index != DEFAULT_SWAP_VOUT as u32
    {
        return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
            "preimage input does not spend the taker-payment outpoint".to_string(),
        ));
    }
    if signer.outputs.len() != expected_outputs_len {
        return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(format!(
            "expected {} output(s) in preimage, got {}",
            expected_outputs_len,
            signer.outputs.len()
        )));
    }
    if signer.outputs[0].script_pubkey != maker_script_pubkey {
        return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
            "preimage output 0 script does not pay the maker address".to_string(),
        ));
    }
    let actual_value = signer.outputs[0].value;
    let diff = actual_value.abs_diff(expected_maker_value);
    let tolerance = expected_maker_value / 10;
    if diff > tolerance {
        return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(format!(
            "preimage maker output value {} differs from expected {} by more than 10% (fee tolerance)",
            actual_value, expected_maker_value
        )));
    }

    if let Some((expected_dex_fee_sat, expected_burn_sat, burn_destination)) = with_burn_extras {
        let expected_fee_out = dex_fee_standard_output(coin, expected_dex_fee_sat)
            .map_to_mm(|e| ValidateTakerPaymentSpendPreimageError::InternalError(format!("{:?}", e)))?;
        if signer.outputs[1].script_pubkey != expected_fee_out.script_pubkey {
            return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
                "preimage output 1 script does not pay the dex-fee address".to_string(),
            ));
        }
        let actual_fee = signer.outputs[1].value;
        let fee_diff = actual_fee.abs_diff(expected_dex_fee_sat);
        let fee_tol = (expected_dex_fee_sat / 10).max(1);
        if fee_diff > fee_tol {
            return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(format!(
                "preimage dex-fee output value {} differs from expected {} by more than 10%",
                actual_fee, expected_dex_fee_sat
            )));
        }

        let expected_burn_out = build_burn_output(coin, expected_burn_sat, &burn_destination)
            .map_to_mm(|e| ValidateTakerPaymentSpendPreimageError::InternalError(format!("{:?}", e)))?;
        if signer.outputs[2].script_pubkey != expected_burn_out.script_pubkey {
            return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
                "preimage burn output script does not match expected destination".to_string(),
            ));
        }
        if signer.outputs[2].value != expected_burn_out.value {
            return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(format!(
                "preimage burn output value {} does not match expected {}",
                signer.outputs[2].value, expected_burn_out.value
            )));
        }
    }

    let taker_payment_redeem = taker_payment_script(
        gen_args.time_lock as u32,
        gen_args.maker_secret_hash,
        gen_args.taker_pub,
        gen_args.maker_pub,
    );
    let digest = signer.signature_hash(
        DEFAULT_SWAP_VOUT,
        signer.inputs[DEFAULT_SWAP_VOUT].amount,
        &taker_payment_redeem,
        coin.as_ref().conf.signature_version,
        sighash_type,
    );
    let ok = gen_args
        .taker_pub
        .verify(&digest, &preimage.signature)
        .map_to_mm(|e| ValidateTakerPaymentSpendPreimageError::InternalError(e.to_string()))?;
    if !ok {
        return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
            "taker signature does not verify against cooperative branch".to_string(),
        ));
    }
    Ok(())
}

/// §15.5.13 — Maker appends the dex-fee output (Standard only), signs with
/// her HTLC key under the same sighash scheme the taker used, assembles the
/// cooperative-branch script_sig (revealing the maker secret) and
/// broadcasts.
pub async fn sign_and_broadcast_taker_payment_spend<T>(
    coin: &T,
    preimage: Option<&TxPreimageWithSig<crate::utxo::utxo_standard::UtxoStandardCoin>>,
    gen_args: &GenTakerPaymentSpendArgs<'_, crate::utxo::utxo_standard::UtxoStandardCoin>,
    secret: &[u8],
    swap_unique_data: &[u8],
) -> Result<UtxoTx, TransactionErr>
where
    T: UtxoCommonOps,
{
    let preimage = match preimage {
        Some(p) => p,
        None => return TX_PLAIN_ERR!("UTXO sign_and_broadcast_taker_payment_spend requires Some(preimage)"),
    };
    let htlc_kp = try_tx_s!(get_htlc_key_pair_v2(coin, swap_unique_data));
    let taker_payment_redeem = taker_payment_script(
        gen_args.time_lock as u32,
        gen_args.maker_secret_hash,
        gen_args.taker_pub,
        gen_args.maker_pub,
    );

    let (mut outputs, sighash_type) = match gen_args.dex_fee {
        DexFee::Standard(ref amount) => {
            let dex_fee_sat = try_tx_s!(sat_from_big_decimal(&amount.to_decimal(), coin.as_ref().decimals));
            let fee_out = try_tx_s!(dex_fee_standard_output(coin, dex_fee_sat));
            let mut outs = preimage.preimage.0.outputs.clone();
            outs.push(fee_out);
            (outs, SIGHASH_SINGLE_BASE | coin.as_ref().conf.fork_id)
        },
        DexFee::WithBurn { .. } | DexFee::NoFee => {
            // For WithBurn / NoFee the preimage already contains the final output set;
            // the maker does not append anything, only (re-)signs under SIGHASH_ALL.
            let outs = preimage.preimage.0.outputs.clone();
            (outs, SIGHASH_ALL_BASE | coin.as_ref().conf.fork_id)
        },
    };
    // Re-attach outputs onto a fresh signer so the maker's signature commits
    // to the final output set under SIGHASH_SINGLE (which only covers
    // output[input_index] anyway, but keep the signer self-consistent so the
    // serialised tx matches what is signed).
    let signer = TransactionInputSigner {
        outputs: std::mem::take(&mut outputs),
        ..preimage.preimage.0.clone()
    };

    let maker_sig = try_tx_s!(sign_taker_payment_spend_input(
        &signer,
        &taker_payment_redeem,
        &htlc_kp,
        coin.as_ref(),
        sighash_type,
    ));

    let sighash_byte = sighash_type as u8;
    let script_sig = build_taker_payment_spend_cooperative_script_sig(
        maker_sig.as_ref(),
        preimage.signature.as_ref(),
        secret,
        sighash_byte,
        &taker_payment_redeem,
    );

    let signed_input = chain::TransactionInput {
        previous_output: signer.inputs[DEFAULT_SWAP_VOUT].previous_output,
        sequence: signer.inputs[DEFAULT_SWAP_VOUT].sequence,
        script_sig,
        script_witness: vec![],
    };
    let mut tx: UtxoTx = signer.clone().into();
    tx.inputs = vec![signed_input];
    tx.tx_hash_algo = coin.as_ref().tx_hash_algo;

    let tx_fut = coin.as_ref().rpc_client.send_transaction(&tx).compat();
    try_tx_s!(tx_fut.await, tx);
    Ok(tx)
}

/// §15.5.14 — Poll the chain from `from_block` for any transaction spending
/// the taker-payment output. Polls every 10 seconds until `wait_until`
/// (unix seconds); returns the spending tx or `FindPaymentSpendError::Timeout`.
pub async fn find_taker_payment_spend_tx<T>(
    coin: &T,
    taker_payment: &UtxoTx,
    from_block: u64,
    wait_until: u64,
) -> MmResult<UtxoTx, FindPaymentSpendError>
where
    T: UtxoCommonOps,
{
    let output = taker_payment.outputs.get(DEFAULT_SWAP_VOUT).ok_or_else(|| {
        MmError::new(FindPaymentSpendError::InvalidInputTx(
            "missing taker-payment output 0".to_string(),
        ))
    })?;
    let tx_hash = taker_payment.hash();
    let script_pubkey = output.script_pubkey.clone();
    let from_block_i64 = from_block as i64;

    loop {
        let now = now_ms() / 1000;
        if now > wait_until {
            return MmError::err(FindPaymentSpendError::Timeout { wait_until, now });
        }
        match coin
            .as_ref()
            .rpc_client
            .find_output_spend(
                tx_hash,
                &script_pubkey,
                DEFAULT_SWAP_VOUT,
                BlockHashOrHeight::Height(from_block_i64),
            )
            .compat()
            .await
        {
            Ok(Some(spent)) => {
                let mut spend_tx = spent.spending_tx;
                spend_tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
                return Ok(spend_tx);
            },
            Ok(None) => {},
            Err(e) => warn!("find_output_spend error: {}; retrying", e),
        }
        Timer::sleep(10.).await;
    }
}

#[test]
fn test_pubkey_from_script_sig() {
    let script_sig = Script::from("473044022071edae37cf518e98db3f7637b9073a7a980b957b0c7b871415dbb4898ec3ebdc022031b402a6b98e64ffdf752266449ca979a9f70144dba77ed7a6a25bfab11648f6012103ad6f89abc2e5beaa8a3ac28e22170659b3209fe2ddf439681b4b8f31508c36fa");
    let expected_pub = H264::from("03ad6f89abc2e5beaa8a3ac28e22170659b3209fe2ddf439681b4b8f31508c36fa");
    let actual_pub = pubkey_from_script_sig(&script_sig).unwrap();
    assert_eq!(expected_pub, actual_pub);

    let script_sig_err = Script::from("473044022071edae37cf518e98db3f7637b9073a7a980b957b0c7b871415dbb4898ec3ebdc022031b402a6b98e64ffdf752266449ca979a9f70144dba77ed7a6a25bfab11648f6012103ad6f89abc2e5beaa8a3ac28e22170659b3209fe2ddf439681b4b8f31508c36fa21");
    pubkey_from_script_sig(&script_sig_err).unwrap_err();

    let script_sig_err = Script::from("493044022071edae37cf518e98db3f7637b9073a7a980b957b0c7b871415dbb4898ec3ebdc022031b402a6b98e64ffdf752266449ca979a9f70144dba77ed7a6a25bfab11648f6012103ad6f89abc2e5beaa8a3ac28e22170659b3209fe2ddf439681b4b8f31508c36fa");
    pubkey_from_script_sig(&script_sig_err).unwrap_err();
}
