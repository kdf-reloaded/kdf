// utxo_common_swap — HTLC/swap operations, payment scripts, validation

use super::*;
use crate::SpendSafeAmountError;
use kdf_crypto::ripemd160;
use primitives::hash::H160;

/// CRD ch.38 R38.6.6c / ch.15 R40: the reference size of an HTLC spend or refund transaction, in bytes.
pub const DEFAULT_SWAP_TX_SPEND_SIZE: u64 = 496;

pub const DEFAULT_SWAP_VOUT: usize = 0;

/// CRD ch.15 R40 / ch.16 R14A: the reference spend size for the version-two funding-spend and
/// taker-payment-spend fee estimate. It is the same figure as the legacy spend size.
pub const V2_SWAP_SPEND_TX_SIZE: u64 = DEFAULT_SWAP_TX_SPEND_SIZE;

/// CRD ch.38 R38.6.6b-c: the fee required to be paid for an HTLC spend transaction of `tx_size` bytes.
/// Plain fixed and dynamic rates are proportional and truncated, the whole-kilobyte fixed policy is its
/// rate per started kilobyte. A dynamic rate carries no volatility increase on a spend the node builds
/// itself. If the coin forces the node's minimum relay fee, the estimate is at least
/// `floor(relay_rate * tx_size / 1000)`.
pub async fn get_htlc_spend_fee<T: UtxoCommonOps>(coin: &T, tx_size: u64) -> UtxoRpcResult<u64> {
    let coin_fee = coin.get_tx_fee().await?;
    let mut fee = coin_fee.fee_for_size(tx_size);
    if coin.as_ref().conf.force_min_relay_fee {
        let relay_fee = get_min_relay_fee(coin.as_ref()).await?;
        let relay_rate = sat_from_big_decimal(&relay_fee, coin.as_ref().decimals).mm_err(Into::into)?;
        let relay_component = ActualTxFee::Dynamic(relay_rate).fee_for_size(tx_size);
        if fee < relay_component {
            fee = relay_component;
        }
    }
    Ok(fee)
}

/// CRD ch.60 R60.6.4, ch.38 R38.6.6c: the value of the single output of an HTLC spend or refund, that is
/// the HTLC value minus the spend `fee`. A spend whose fee is not smaller than the HTLC value is refused
/// with an orderly error, before anything is built or broadcast.
fn htlc_spend_output_value(htlc_value: u64, fee: u64) -> Result<u64, TransactionErr> {
    if fee >= htlc_value {
        return TX_PLAIN_ERR!(
            "HTLC spend fee {} is not smaller than the HTLC value {}, refusing to spend",
            fee,
            htlc_value
        );
    }
    Ok(htlc_value - fee)
}

/// CRD ch.60 R60.6.7 item 2: the safe amount of a UTXO-family coin, that is the R38.6.6c HTLC spend-fee estimate
/// under the coin's fee policy at the moment of the call plus the coin's dust (R38.6.6d).
pub async fn spend_safe_amount<T: UtxoCommonOps>(coin: &T) -> MmResult<Option<MmNumber>, SpendSafeAmountError> {
    let fee = get_htlc_spend_fee(coin, DEFAULT_SWAP_TX_SPEND_SIZE)
        .await
        .mm_err(|e| SpendSafeAmountError::EstimateUnavailable(e.to_string()))?;
    let safe_amount = fee
        .checked_add(coin.as_ref().dust_amount)
        .or_mm_err(|| SpendSafeAmountError::EstimateUnavailable(format!("spend fee {fee} plus dust overflows")))?;
    Ok(Some(
        big_decimal_from_sat_unsigned(safe_amount, coin.as_ref().decimals).into(),
    ))
}

/// CRD ch.16 R14A: the version-two funding-spend / taker-payment-spend spend-fee estimate `S`.
/// It is the R38.6.6c estimate over [`V2_SWAP_SPEND_TX_SIZE`] bytes, exactly as the legacy spends use it.
pub async fn get_v2_swap_spend_fee<T: UtxoCommonOps>(coin: &T, tx_size: u64) -> UtxoRpcResult<u64> {
    get_htlc_spend_fee(coin, tx_size).await
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
    send_outputs_from_my_address(coin, outputs)
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
        #[cfg(not(target_arch = "wasm32"))]
        UtxoRpcClientEnum::LightWalletD(_) => Either::A(send_outputs_from_my_address(coin, outputs)),
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
        #[cfg(not(target_arch = "wasm32"))]
        UtxoRpcClientEnum::LightWalletD(_) => Either::A(send_outputs_from_my_address(coin, outputs)),
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
    // `derivation_method.iguana_or_err()` only ever succeeds for a
    // single-address (Iguana) account -- it fails outright with
    // IguanaPrivKeyUnavailable for an HD-wallet-activated one, since there
    // is no single "the" address to hand back for those (ch.51 R63: ask
    // for what's actually needed instead of assuming one shared shape).
    // What this spend/refund actually needs is the output address for
    // *this* key -- and `key_pair` above, from `htlc_privkey`, already is
    // the correct per-swap signing key for either derivation mode (R63's
    // selection already resolved that at negotiation time, in
    // get_htlc_key_pair). Deriving the address from it directly is both
    // more correct (guaranteed to match what's actually signing) and
    // works for both derivation modes, so there is no need to separately
    // consult `derivation_method` at all.
    let my_address = address_from_pubkey(
        key_pair.public(),
        coin.as_ref().conf.pub_addr_prefix,
        coin.as_ref().conf.pub_t_addr_prefix,
        coin.as_ref().conf.checksum_type,
        coin.as_ref().conf.bech32_hrp.clone(),
        coin.addr_format().clone(),
    );

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
            value: htlc_spend_output_value(prev_tx.outputs[0].value, fee)?,
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
    // `derivation_method.iguana_or_err()` only ever succeeds for a
    // single-address (Iguana) account -- it fails outright with
    // IguanaPrivKeyUnavailable for an HD-wallet-activated one, since there
    // is no single "the" address to hand back for those (ch.51 R63: ask
    // for what's actually needed instead of assuming one shared shape).
    // What this spend/refund actually needs is the output address for
    // *this* key -- and `key_pair` above, from `htlc_privkey`, already is
    // the correct per-swap signing key for either derivation mode (R63's
    // selection already resolved that at negotiation time, in
    // get_htlc_key_pair). Deriving the address from it directly is both
    // more correct (guaranteed to match what's actually signing) and
    // works for both derivation modes, so there is no need to separately
    // consult `derivation_method` at all.
    let my_address = address_from_pubkey(
        key_pair.public(),
        coin.as_ref().conf.pub_addr_prefix,
        coin.as_ref().conf.pub_t_addr_prefix,
        coin.as_ref().conf.checksum_type,
        coin.as_ref().conf.bech32_hrp.clone(),
        coin.addr_format().clone(),
    );

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
            value: htlc_spend_output_value(prev_tx.outputs[0].value, fee)?,
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
    // `derivation_method.iguana_or_err()` only ever succeeds for a
    // single-address (Iguana) account -- it fails outright with
    // IguanaPrivKeyUnavailable for an HD-wallet-activated one, since there
    // is no single "the" address to hand back for those (ch.51 R63: ask
    // for what's actually needed instead of assuming one shared shape).
    // What this spend/refund actually needs is the output address for
    // *this* key -- and `key_pair` above, from `htlc_privkey`, already is
    // the correct per-swap signing key for either derivation mode (R63's
    // selection already resolved that at negotiation time, in
    // get_htlc_key_pair). Deriving the address from it directly is both
    // more correct (guaranteed to match what's actually signing) and
    // works for both derivation modes, so there is no need to separately
    // consult `derivation_method` at all.
    let my_address = address_from_pubkey(
        key_pair.public(),
        coin.as_ref().conf.pub_addr_prefix,
        coin.as_ref().conf.pub_t_addr_prefix,
        coin.as_ref().conf.checksum_type,
        coin.as_ref().conf.bech32_hrp.clone(),
        coin.addr_format().clone(),
    );

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
            value: htlc_spend_output_value(prev_tx.outputs[0].value, fee)?,
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
    // `derivation_method.iguana_or_err()` only ever succeeds for a
    // single-address (Iguana) account -- it fails outright with
    // IguanaPrivKeyUnavailable for an HD-wallet-activated one, since there
    // is no single "the" address to hand back for those (ch.51 R63: ask
    // for what's actually needed instead of assuming one shared shape).
    // What this spend/refund actually needs is the output address for
    // *this* key -- and `key_pair` above, from `htlc_privkey`, already is
    // the correct per-swap signing key for either derivation mode (R63's
    // selection already resolved that at negotiation time, in
    // get_htlc_key_pair). Deriving the address from it directly is both
    // more correct (guaranteed to match what's actually signing) and
    // works for both derivation modes, so there is no need to separately
    // consult `derivation_method` at all.
    let my_address = address_from_pubkey(
        key_pair.public(),
        coin.as_ref().conf.pub_addr_prefix,
        coin.as_ref().conf.pub_t_addr_prefix,
        coin.as_ref().conf.checksum_type,
        coin.as_ref().conf.bech32_hrp.clone(),
        coin.addr_format().clone(),
    );

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
            value: htlc_spend_output_value(prev_tx.outputs[0].value, fee)?,
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
            // Already-sent detection over lightwalletd rests on the address-index workaround that is
            // not yet proved on a funded HTLC (CRD ch.39 R39.11.4d): fail loudly, because an empty
            // answer here would read as "not sent" and double-fund the payment.
            #[cfg(not(target_arch = "wasm32"))]
            UtxoRpcClientEnum::LightWalletD(_) => ERR!(
                "check_if_my_payment_sent is not implemented over lightwalletd: the address-index workaround it \
                 needs (R39.11.4d) has not been proved on a funded HTLC"
            ),
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
    my_pub: &[u8],
    other_pub: &[u8],
    secret_hash: &[u8],
    tx: &[u8],
    output_index: usize,
    search_from_block: u64,
) -> Result<Option<FoundSwapTxSpend>, String> {
    search_for_swap_output_spend(
        coin,
        time_lock,
        &try_s!(Public::from_slice(my_pub)),
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
    my_pub: &[u8],
    other_pub: &[u8],
    secret_hash: &[u8],
    tx: &[u8],
    output_index: usize,
    search_from_block: u64,
) -> Result<Option<FoundSwapTxSpend>, String> {
    search_for_swap_output_spend(
        coin,
        time_lock,
        &try_s!(Public::from_slice(other_pub)),
        &try_s!(Public::from_slice(my_pub)),
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
    extract_secret_from_script_sigs(
        secret_hash,
        spend_tx.inputs.into_iter().map(|input| input.script_sig.into()),
    )
}

/// The secret-extraction rule of [`extract_secret`], over the input scripts alone, so a coin that reads
/// its transactions with a different parser (ZHTLC, CRD ch.39 R39.11.16a) applies the identical rule.
pub(crate) fn extract_secret_from_script_sigs(
    secret_hash: &[u8],
    script_sigs: impl IntoIterator<Item = Script>,
) -> Result<Vec<u8>, String> {
    for (input_idx, script) in script_sigs.into_iter().enumerate() {
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

        // ch.51 R72A: `secret_hash` may be R71's 20-byte default or R72's
        // 32-byte alternate; either way, the revealed value on-chain is
        // always checked against `OP_HASH160(secret)`, so that's what this
        // extraction must reproduce and compare against too -- see
        // htlc_hash160_comparator's doc comment for why that isn't simply
        // `dhash160(secret_hash)` in the 32-byte case.
        let expected_secret_hash = match htlc_hash160_comparator(secret_hash) {
            Ok(h) => h,
            Err(e) => {
                error!("extract_secret: {}", e);
                continue;
            },
        };
        let actual_secret_hash = dhash160(&secret);
        if actual_secret_hash != expected_secret_hash {
            log!("Warning: invalid 'dhash160(secret)' "[actual_secret_hash]", expected "[expected_secret_hash]);
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
    let tx_hash = tx.hash();
    let output0_script_pubkey = tx.outputs[0].script_pubkey.clone();
    let spent_script_pubkey = tx.outputs[output_index].script_pubkey.clone();
    search_for_swap_output_spend_of(
        coin,
        time_lock,
        first_pub,
        second_pub,
        secret_hash,
        PaymentOutputs {
            tx_hash,
            output0_script_pubkey,
            spent_script_pubkey,
        },
        output_index,
        search_from_block,
    )
    .await
}

/// What the spend search needs to know about the payment transaction, independent of which parser
/// read it.
pub(crate) struct PaymentOutputs {
    /// Internal-order transaction hash, as `find_output_spend` takes it.
    pub(crate) tx_hash: H256,
    pub(crate) output0_script_pubkey: Bytes,
    pub(crate) spent_script_pubkey: Bytes,
}

/// How an HTLC output was spent, read from the spend's input script.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SwapSpendKind {
    Spent,
    Refunded,
}

/// Classifies a spend of the swap output by its input script: `OP_0` third means the secret path
/// (spent), `OP_1` second means the timelock path (refunded). Shared so that a coin which reads its
/// transactions differently makes the same decision.
pub(crate) fn classify_swap_spend(script: &Script) -> Option<SwapSpendKind> {
    if let Some(Ok(ref i)) = script.iter().nth(2) {
        if i.opcode == Opcode::OP_0 {
            return Some(SwapSpendKind::Spent);
        }
    }
    if let Some(Ok(ref i)) = script.iter().nth(1) {
        if i.opcode == Opcode::OP_1 {
            return Some(SwapSpendKind::Refunded);
        }
    }
    None
}

/// The check that a payment transaction's output 0 pays the HTLC script this swap negotiated. A
/// mismatch is a hard stop, not something to retry: see the message.
pub(crate) fn ensure_payment_pays_htlc(
    time_lock: u32,
    first_pub: &Public,
    second_pub: &Public,
    secret_hash: &[u8],
    payment: &PaymentOutputs,
) -> Result<(), String> {
    let script = payment_script(time_lock, secret_hash, first_pub, second_pub);
    let expected_script_pubkey = Builder::build_p2sh(&dhash160(&script).into()).to_bytes();
    if payment.output0_script_pubkey != expected_script_pubkey {
        // Reached from `recover_funds`: the pubkeys/secret_hash/locktime this
        // recovery attempt reconstructed from the swap's own saved record
        // don't reproduce the HTLC script that's actually on chain in this
        // payment, so it's not safe to search for or act on its spend --
        // could mean the on-chain tx isn't really this swap's payment, or
        // the saved record's negotiated data is itself inconsistent with it
        // (seen in practice on swaps predating later fixes to how that data
        // gets derived/persisted). Either way this is a hard stop, not
        // something to retry -- report the two script hashes and this
        // transaction's own hash rather than a full Debug-formatted dump of
        // the deserialized transaction, which said nothing a caller could
        // act on.
        return ERR!(
            "Payment tx {:?} output 0 does not pay the HTLC script this swap negotiated \
             (found script_pubkey {:?}, expected {:?} from the swap's saved pubkeys/secret_hash/locktime) \
             -- this payment cannot be recovered automatically",
            // `payment.tx_hash` is in internal byte order; the transaction id is shown reversed.
            payment.tx_hash.reversed(),
            payment.output0_script_pubkey,
            expected_script_pubkey
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn search_for_swap_output_spend_of(
    coin: &UtxoCoinFields,
    time_lock: u32,
    first_pub: &Public,
    second_pub: &Public,
    secret_hash: &[u8],
    payment: PaymentOutputs,
    output_index: usize,
    search_from_block: u64,
) -> Result<Option<FoundSwapTxSpend>, String> {
    try_s!(ensure_payment_pays_htlc(
        time_lock,
        first_pub,
        second_pub,
        secret_hash,
        &payment
    ));

    let spend = try_s!(
        coin.rpc_client
            .find_output_spend(
                payment.tx_hash,
                &payment.spent_script_pubkey,
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
            match classify_swap_spend(&script) {
                Some(SwapSpendKind::Spent) => Ok(Some(FoundSwapTxSpend::Spent(tx.into()))),
                Some(SwapSpendKind::Refunded) => Ok(Some(FoundSwapTxSpend::Refunded(tx.into()))),
                None => ERR!(
                    "Couldn't find required instruction in script_sig of input 0 of tx {:?}",
                    tx
                ),
            }
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

/// Derives the 20-byte value UTXO's `OP_HASH160`-based HTLC satisfaction
/// opcode must be compared against, from the swap's single shared
/// `secret_hash` (ch.51 R72A): UTXO's native hash-lock can only ever verify
/// a fixed-width `RIPEMD160(SHA-256(secret))` digest -- that is what
/// `OP_HASH160 <secret>` computes on-chain when the real secret is pushed,
/// unconditionally, regardless of which algorithm ch.51 R71/R72 selected at
/// the swap level.
///
/// - 20 bytes (R71's default): already that exact digest -- used unchanged.
/// - 32 bytes (R72's alternate, forced whenever a native-32-byte-hash-lock
///   coin -- Sia, Tendermint/IBC-HTLC, Lightning -- is on either side): this
///   value *is* `SHA-256(secret)`, the inner half of the same
///   `RIPEMD160(SHA-256(secret))` composite (R72A), so applying `RIPEMD160`
///   alone -- the remaining, outer half -- reproduces exactly what
///   `OP_HASH160(secret)` computes on-chain. Applying the *whole* `dhash160`
///   composite here instead (i.e. re-hashing with SHA-256 first) would
///   derive `RIPEMD160(SHA-256(SHA-256(secret)))`, which nothing on-chain
///   ever produces -- a payment built or checked against a wrong-by-that-
///   difference constant is unspendable by anyone, including the legitimate
///   secret holder, invisible until an actual spend is attempted (this is
///   the third gap ch.51 §51.9.1's Code-quality finding describes, and what
///   a live testnet swap pairing SC against a UTXO coin was observed to
///   fail on).
///
/// Errors on any other width: R65's wire contract admits only these two.
fn htlc_hash160_comparator(secret_hash: &[u8]) -> Result<H160, String> {
    match secret_hash.len() {
        20 => Ok(H160::from(secret_hash)),
        32 => Ok(ripemd160(secret_hash)),
        other => ERR!("secret_hash must be 20 or 32 bytes (ch.51 R65/R71/R72), got {}", other),
    }
}

pub fn payment_script(time_lock: u32, secret_hash: &[u8], pub_0: &Public, pub_1: &Public) -> Script {
    // A malformed width here would otherwise silently build a script nobody
    // can ever satisfy (see htlc_hash160_comparator's doc comment) -- fail
    // loudly and immediately instead. Every caller already validates
    // secret_hash's width against R65 before reaching this point in the
    // ordinary case, so this is a last-resort guard, not the primary check.
    let secret_hash_hash160 = match htlc_hash160_comparator(secret_hash) {
        Ok(h) => h,
        Err(e) => {
            error!(
                "payment_script: {} -- building an unsatisfiable placeholder script rather than panicking",
                e
            );
            H160::default()
        },
    };
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
        .push_bytes(&*secret_hash_hash160)
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

/// Upper bound, in seconds, on the wait returned by [`can_refund_htlc`]: a caller sleeps at most this
/// long before it re-checks, never longer than the real remaining wait.
pub(crate) const MAX_REFUND_WAIT_SECS: u64 = 3600;

/// Caps the remaining wait at [`MAX_REFUND_WAIT_SECS`].
pub(crate) fn capped_refund_wait(remaining_secs: u64) -> CanRefundHtlc {
    CanRefundHtlc::HaveToWait(remaining_secs.min(MAX_REFUND_WAIT_SECS))
}

/// Decides refund eligibility from the wall clock `now` (seconds), the HTLC `locktime`, and a lazily
/// obtained `(p2sh_locktime, mtp)` pair consulted only once the wall-clock lock-time has passed.
fn refund_eligibility(now: u64, locktime: u64, p2sh_locktime_and_mtp: Option<(u32, u32)>) -> CanRefundHtlc {
    if now < locktime {
        return capped_refund_wait(locktime - now + 1);
    }
    match p2sh_locktime_and_mtp {
        Some((p2sh_locktime, mtp)) if p2sh_locktime >= mtp => capped_refund_wait((p2sh_locktime - mtp + 1) as u64),
        _ => CanRefundHtlc::CanRefundNow,
    }
}

pub async fn can_refund_htlc<T>(coin: &T, locktime: u64) -> Result<CanRefundHtlc, MmError<UtxoRpcError>>
where
    T: UtxoCommonOps,
{
    let now = now_ms() / 1000;
    if now < locktime {
        return Ok(refund_eligibility(now, locktime, None));
    }

    let mtp = coin.get_current_mtp().await?;
    let p2sh_locktime = coin.p2sh_tx_locktime(locktime as u32).await?;
    Ok(refund_eligibility(now, locktime, Some((p2sh_locktime, mtp))))
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

/// Returns `Some(keypair)` when this coin's negotiated V1 HTLC key must be
/// something other than the node's persistent secp256k1 identity key, `None`
/// when the persistent key is itself the address that will sign (ch.51 R63).
///
/// `PrivKeyPolicy::KeyPair` (Iguana/legacy single-address activation) derives
/// its one and only address straight from the node's persistent key, so
/// `None` (fall back to it) is correct. `PrivKeyPolicy::HDWallet` is not: its
/// `activated_key` is a distinct, path-derived key that both the wallet's
/// address display and its normal UTXO-selection/signing path already use
/// for this coin -- returning `None` here used to negotiate the *node's* key
/// while `send_taker_fee`/`send_maker_payment` went on signing with
/// `activated_key`, so the counterparty's `check_all_inputs_signed_by_pub`
/// validation (comparing against the negotiated key) failed every time an
/// HD-activated coin was used, with "The dex fee was sent from wrong
/// address" surfacing on the maker side. Mirrors `get_htlc_pubkey_v2` below,
/// which already reads the enabled HD address for the V2 path.
pub fn get_htlc_key_pair<T>(coin: &T) -> Option<KeyPair>
where
    T: AsRef<UtxoCoinFields>,
{
    match &coin.as_ref().priv_key_policy {
        PrivKeyPolicy::KeyPair(_) => None,
        PrivKeyPolicy::HDWallet { activated_key, .. } => Some(*activated_key),
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
pub(crate) async fn build_and_broadcast_maker_v2_spend<T>(
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
        value: htlc_spend_output_value(prev_tx.outputs[DEFAULT_SWAP_VOUT].value, fee)?,
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
    // CRD ch.15 R16: funding value is trading amount + premium + the dex-fee
    // *total spend amount* (fee plus burn for `WithBurn`, zero for `NoFee`) --
    // not `fee_amount()` alone, which would silently omit the burn leg.
    let total = &args.trading_amount + &args.premium_amount + &args.dex_fee.total_spend_amount().to_decimal();
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

    // CRD ch.15 R17: same total-spend-amount formula as the builder (R16).
    let total = &args.trading_amount + &args.premium_amount + &args.dex_fee.total_spend_amount().to_decimal();
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
    // CRD ch.15 R21 step 2, ch.60 R60.6.4: the funding value must be at least the spend fee plus the coin's dust.
    let min_funding_value = fee
        .checked_add(fields.dust_amount)
        .ok_or(TxGenError::PrevOutputTooLow)?;
    if funding_output.value < min_funding_value {
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
    // CRD ch.15 R21 / ch.16 R14A: the version-two funding-spend fee estimate
    // (496 bytes, proportional, not rounded up to a whole kilobyte).
    let fee = get_v2_swap_spend_fee(coin, V2_SWAP_SPEND_TX_SIZE)
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
    // CRD ch.15 R22 / ch.16 R14A: same 496-byte proportional fee estimate as
    // the builder (R21); the ±10% tolerance below is chapter 15 R22's own
    // and is unchanged by this batch (chapter 16 D5).
    let expected_fee = get_v2_swap_spend_fee(coin, V2_SWAP_SPEND_TX_SIZE)
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
/// CRD ch.16 R15/R18: each signature in the script-sig carries its own flag
/// byte. The maker always signs `ALL`; the taker's byte is `SINGLE` for
/// `Standard` and `ALL` for `WithBurn`/`NoFee` -- the two bytes differ for
/// `Standard`, so they MUST be supplied separately (a single shared byte,
/// the withdrawn pre-correction contract, cannot represent that case).
pub(crate) fn build_taker_payment_spend_cooperative_script_sig(
    maker_sig_der: &[u8],
    taker_sig_der: &[u8],
    maker_secret: &[u8],
    maker_sighash_byte: u8,
    taker_sighash_byte: u8,
    taker_payment_redeem: &Script,
) -> Bytes {
    let mut maker_sig = maker_sig_der.to_vec();
    maker_sig.push(maker_sighash_byte);
    let mut taker_sig = taker_sig_der.to_vec();
    taker_sig.push(taker_sighash_byte);
    Builder::default()
        .push_data(&maker_sig)
        .push_data(&taker_sig)
        .push_data(maker_secret)
        .push_opcode(Opcode::OP_0)
        .push_data(taker_payment_redeem)
        .into_script()
        .to_bytes()
}

/// Resolve the dex-fee P2PKH output for a given `DexFee::Standard`/`WithBurn`
/// fee-leg amount. CRD ch.16 R14 / chapter 08 R16 finding: the
/// fee-collection address MUST be derived from the active network's
/// `NetConfig::dex_fee_addr_raw_pubkey()`, passed in by the caller -- not
/// from the deprecated, netid-8762-fixed `common::DEX_FEE_ADDR_RAW_PUBKEY`
/// (chapter 29 §29.1.4's code-quality finding).
fn dex_fee_standard_output<T: UtxoCommonOps>(
    coin: &T,
    fee_sat: u64,
    dex_fee_addr_raw_pubkey: &[u8],
) -> Result<TransactionOutput, TxGenError> {
    let fee_address = address_from_raw_pubkey(
        dex_fee_addr_raw_pubkey,
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

/// CRD ch.16 R20 — Build the burn-leg output of a `DexFee::WithBurn`
/// taker-payment-spend. `KmdOpReturn` carries the burned value in the
/// output's own value with a bare `OP_RETURN` script (no data push) -- the
/// same form as the legacy taker-fee burn output of chapter 08 R15B/R20.
/// `PreBurnAccount` produces a P2PKH paying the burn address.
fn build_burn_output<T: UtxoCommonOps>(
    coin: &T,
    burn_amount_sat: u64,
    destination: &DexFeeBurnDestination,
) -> Result<TransactionOutput, TxGenError> {
    match destination {
        DexFeeBurnDestination::KmdOpReturn => Ok(TransactionOutput {
            value: burn_amount_sat,
            script_pubkey: Builder::default().push_opcode(Opcode::OP_RETURN).into_bytes(),
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

/// CRD ch.16 R14/R20 — the taker-payment-spend preimage's expected output
/// list and signature-hash type (R15) for a given `DexFee` variant. Shared
/// by the builder and the validator so the two sides can never independently
/// drift: R17 requires the maker to "re-derive the expected preimage
/// itself," which this makes literal.
fn taker_payment_spend_outputs<T: UtxoCommonOps>(
    coin: &T,
    dex_fee: &DexFee,
    taker_output_value: u64,
    spend_fee: u64,
    maker_script_pubkey: &Bytes,
    dex_fee_addr_raw_pubkey: &[u8],
) -> Result<(Vec<TransactionOutput>, u32), TxGenError> {
    match dex_fee {
        DexFee::Standard(amount) => {
            let dex_fee_sat = sat_from_big_decimal(&amount.to_decimal(), coin.as_ref().decimals)
                .map_err(|e| TxGenError::NumConversion(e.to_string()))?;
            let fee_out = dex_fee_standard_output(coin, dex_fee_sat, dex_fee_addr_raw_pubkey)?;
            Ok((vec![fee_out], SIGHASH_SINGLE_BASE | coin.as_ref().conf.fork_id))
        },
        DexFee::WithBurn {
            fee_amount,
            burn_amount,
            burn_destination,
        } => {
            let dex_fee_sat = sat_from_big_decimal(&fee_amount.to_decimal(), coin.as_ref().decimals)
                .map_err(|e| TxGenError::NumConversion(e.to_string()))?;
            let burn_sat = sat_from_big_decimal(&burn_amount.to_decimal(), coin.as_ref().decimals)
                .map_err(|e| TxGenError::NumConversion(e.to_string()))?;
            let maker_value = taker_output_value
                .checked_sub(dex_fee_sat)
                .and_then(|v| v.checked_sub(burn_sat))
                .and_then(|v| v.checked_sub(spend_fee))
                .ok_or(TxGenError::PrevOutputTooLow)?;
            let fee_out = dex_fee_standard_output(coin, dex_fee_sat, dex_fee_addr_raw_pubkey)?;
            let burn_out = build_burn_output(coin, burn_sat, burn_destination)?;
            let outs = vec![fee_out, burn_out, TransactionOutput {
                value: maker_value,
                script_pubkey: maker_script_pubkey.clone(),
            }];
            Ok((outs, SIGHASH_ALL_BASE | coin.as_ref().conf.fork_id))
        },
        DexFee::NoFee => {
            let maker_value = taker_output_value
                .checked_sub(spend_fee)
                .ok_or(TxGenError::PrevOutputTooLow)?;
            let outs = vec![TransactionOutput {
                value: maker_value,
                script_pubkey: maker_script_pubkey.clone(),
            }];
            Ok((outs, SIGHASH_ALL_BASE | coin.as_ref().conf.fork_id))
        },
    }
}

/// CRD ch.16 R14/R14A/R15/R20 — Taker builds the unsigned taker-payment-spend
/// preimage and partial-signs it. Output layout (fee-first, per variant):
///
/// - `Standard`: one output, the fee-collection P2PKH (`fee_sat`). Signed
///   `SINGLE` so the maker can append its own payout without invalidating
///   the signature (R18).
/// - `WithBurn`: three outputs, in order: fee-collection P2PKH, the R20 burn
///   output, then the maker payout. Signed `ALL`.
/// - `NoFee`: one output, the maker payout. Signed `ALL`.
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
    let spend_fee = get_v2_swap_spend_fee(coin, V2_SWAP_SPEND_TX_SIZE)
        .await
        .mm_err(|e| TxGenError::Rpc(e.to_string()))?;
    let maker_script_pubkey = output_script(args.maker_address, ScriptType::P2PKH).to_bytes();

    let (outputs, sighash_type) = taker_payment_spend_outputs(
        coin,
        args.dex_fee,
        taker_output.value,
        spend_fee,
        &maker_script_pubkey,
        args.dex_fee_addr_raw_pubkey,
    )
    .map_to_mm(|e| e)?;

    let signer = build_taker_payment_spend_preimage_tx(coin.as_ref(), args.taker_tx, outputs).map_to_mm(|e| e)?;
    let sig = sign_taker_payment_spend_input(&signer, &taker_payment_redeem, &htlc_kp, coin.as_ref(), sighash_type)
        .map_to_mm(TxGenError::Signing)?;
    Ok(TxPreimageWithSig {
        preimage: UtxoTxPreimage(signer),
        signature: sig,
    })
}

/// CRD ch.16 R17 — Maker validates the taker-payment-spend preimage's shape
/// (rebuilt exactly, no value tolerance) and the taker's partial signature.
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
    let spend_fee = get_v2_swap_spend_fee(coin, V2_SWAP_SPEND_TX_SIZE)
        .await
        .mm_err(|e| ValidateTakerPaymentSpendPreimageError::InternalError(e.to_string()))?;
    let maker_script_pubkey = output_script(gen_args.maker_address, ScriptType::P2PKH).to_bytes();

    let (expected_outputs, sighash_type) = taker_payment_spend_outputs(
        coin,
        gen_args.dex_fee,
        taker_output.value,
        spend_fee,
        &maker_script_pubkey,
        gen_args.dex_fee_addr_raw_pubkey,
    )
    .map_to_mm(|e| ValidateTakerPaymentSpendPreimageError::InternalError(format!("{:?}", e)))?;

    let signer = &preimage.preimage.0;

    if signer.inputs.len() != 1
        || signer.inputs[0].previous_output.hash != gen_args.taker_tx.hash()
        || signer.inputs[0].previous_output.index != DEFAULT_SWAP_VOUT as u32
        || signer.inputs[0].sequence != SEQUENCE_FINAL
    {
        return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
            "preimage input does not spend the taker-payment outpoint".to_string(),
        ));
    }
    if signer.version != coin.as_ref().conf.tx_version
        || signer.overwintered != coin.as_ref().conf.overwintered
        || signer.lock_time != 0
    {
        return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
            "preimage version/overwintered/lock_time does not match the expected transaction shape".to_string(),
        ));
    }
    // Exact equality on output count, order, every value and every script --
    // no value tolerance (R17). `TransactionOutput` derives `PartialEq`.
    if signer.outputs != expected_outputs {
        return MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(format!(
            "preimage outputs {:?} do not exactly match the expected {:?}",
            signer.outputs, expected_outputs
        )));
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

/// CRD ch.16 R18 — For `Standard`, the maker first requires
/// `S + dust + fee_sat <= P`, then appends its own payout as output 1 (the
/// preimage carries the fee output alone, at output 0). For `WithBurn` and
/// `NoFee` the preimage already carries every output; the maker MUST NOT
/// add, remove, or change any of them. In every case the maker signs with
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

    // CRD ch.16 R15: the taker's flag is SINGLE for `Standard`, ALL for
    // `WithBurn`/`NoFee`. R18: the maker's OWN signature is always ALL,
    // regardless of the taker's flag -- the two bytes differ for `Standard`.
    let (mut outputs, taker_sighash_type) = match gen_args.dex_fee {
        DexFee::Standard(ref amount) => {
            let taker_output = match gen_args.taker_tx.outputs.get(DEFAULT_SWAP_VOUT) {
                Some(o) => o,
                None => return TX_PLAIN_ERR!("taker-payment tx has no output 0"),
            };
            let dex_fee_sat = try_tx_s!(sat_from_big_decimal(&amount.to_decimal(), coin.as_ref().decimals));
            let spend_fee = try_tx_s!(get_v2_swap_spend_fee(coin, V2_SWAP_SPEND_TX_SIZE).await);
            let dust = coin.as_ref().dust_amount;
            let min_required = spend_fee.checked_add(dust).and_then(|v| v.checked_add(dex_fee_sat));
            let maker_value = match min_required {
                Some(min_required) if taker_output.value >= min_required => {
                    taker_output.value - spend_fee - dex_fee_sat
                },
                _ => return TX_PLAIN_ERR!("taker-payment value below spend fee + dust + dex fee"),
            };
            let maker_script_pubkey = output_script(gen_args.maker_address, ScriptType::P2PKH).to_bytes();
            let mut outs = preimage.preimage.0.outputs.clone();
            outs.push(TransactionOutput {
                value: maker_value,
                script_pubkey: maker_script_pubkey,
            });
            (outs, SIGHASH_SINGLE_BASE | coin.as_ref().conf.fork_id)
        },
        DexFee::WithBurn { .. } | DexFee::NoFee => {
            // For WithBurn / NoFee the preimage already contains the final output set;
            // the maker does not append anything.
            let outs = preimage.preimage.0.outputs.clone();
            (outs, SIGHASH_ALL_BASE | coin.as_ref().conf.fork_id)
        },
    };
    // Re-attach outputs onto a fresh signer so the maker's signature commits
    // to the final output set.
    let signer = TransactionInputSigner {
        outputs: std::mem::take(&mut outputs),
        ..preimage.preimage.0.clone()
    };

    let maker_sighash_type = SIGHASH_ALL_BASE | coin.as_ref().conf.fork_id;
    let maker_sig = try_tx_s!(sign_taker_payment_spend_input(
        &signer,
        &taker_payment_redeem,
        &htlc_kp,
        coin.as_ref(),
        maker_sighash_type,
    ));

    let maker_sighash_byte = maker_sighash_type as u8;
    let taker_sighash_byte = taker_sighash_type as u8;
    let script_sig = build_taker_payment_spend_cooperative_script_sig(
        maker_sig.as_ref(),
        preimage.signature.as_ref(),
        secret,
        maker_sighash_byte,
        taker_sighash_byte,
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

// ch.51 R72A: htlc_hash160_comparator's whole job is to reproduce, from the
// swap's shared secret_hash alone, exactly what OP_HASH160(secret) computes
// on-chain when the real secret is later pushed. The tests below aren't
// exercisable through an integration test in this environment (every
// existing send/spend/extract test needs a live electrum connection), so
// they check the derivation directly -- most importantly, that it agrees
// with dhash160(secret) for a real secret, which is the property the whole
// fix depends on.
#[test]
fn test_htlc_hash160_comparator_20_byte_input_is_unchanged() {
    // dhash160 of the real secret used by test_extract_secret_finds_the_secret_against_a_32_byte_secret_hash below.
    let narrow = H160::from("2fb610d856c19fd57f2d0cffe8dff689074b3d8a");
    assert_eq!(htlc_hash160_comparator(&*narrow).unwrap(), narrow);
}

#[test]
fn test_htlc_hash160_comparator_32_byte_input_is_ripemd160_of_it() {
    let wide = sha256(&[7u8; 32]);
    let expected = ripemd160(wide.as_slice());
    assert_eq!(htlc_hash160_comparator(wide.as_slice()).unwrap(), expected);
}

#[test]
fn test_htlc_hash160_comparator_rejects_other_widths() {
    assert!(htlc_hash160_comparator(&[0u8; 19]).is_err());
    assert!(htlc_hash160_comparator(&[0u8; 21]).is_err());
    assert!(htlc_hash160_comparator(&[0u8; 33]).is_err());
    assert!(htlc_hash160_comparator(&[]).is_err());
}

/// The property the whole fix depends on: for a real secret, deriving the
/// comparator from R72's 32-byte alternate (`SHA-256(secret)`) must equal
/// `dhash160(secret)` -- exactly what `OP_HASH160(secret)` computes
/// on-chain when the real secret is pushed at spend time, regardless of
/// which algorithm ch.51 R71/R72 selected. If this property doesn't hold,
/// nothing built or checked with the 32-byte alternate is spendable by
/// anyone, which is the bug this fix closes.
#[test]
fn test_htlc_hash160_comparator_agrees_with_dhash160_of_the_real_secret() {
    let secret = [7u8; 32];
    let secret_hash_32 = sha256(&secret); // R72's alternate: SHA-256(secret)
    let derived = htlc_hash160_comparator(secret_hash_32.as_slice()).unwrap();
    assert_eq!(derived, dhash160(&secret));
}

#[test]
fn test_extract_secret_finds_the_secret_against_a_32_byte_secret_hash() {
    // Same real spend tx and secret as utxo_tests::test_extract_secret,
    // which confirms this raw tx's script_sig pushes this exact secret at
    // instruction index 1 and that dhash160(secret) is the 20-byte hash
    // embedded at that spend's origin (R71's default). This test checks the
    // other half of the same property (R72's 32-byte alternate): the same
    // observed spend must also extract successfully when the swap had
    // instead negotiated secret_hash = SHA-256(secret).
    let tx_hex = hex::decode("0100000001de7aa8d29524906b2b54ee2e0281f3607f75662cbc9080df81d1047b78e21dbc00000000d7473044022079b6c50820040b1fbbe9251ced32ab334d33830f6f8d0bf0a40c7f1336b67d5b0220142ccf723ddabb34e542ed65c395abc1fbf5b6c3e730396f15d25c49b668a1a401209da937e5609680cb30bff4a7661364ca1d1851c2506fa80c443f00a3d3bf7365004c6b6304f62b0e5cb175210270e75970bb20029b3879ec76c4acd320a8d0589e003636264d01a7d566504bfbac6782012088a9142fb610d856c19fd57f2d0cffe8dff689074b3d8a882103f368228456c940ac113e53dad5c104cf209f2f102a409207269383b6ab9b03deac68ffffffff01d0dc9800000000001976a9146d9d2b554d768232320587df75c4338ecc8bf37d88ac40280e5c").unwrap();
    let expected_secret = hex::decode("9da937e5609680cb30bff4a7661364ca1d1851c2506fa80c443f00a3d3bf7365").unwrap();
    let secret_hash_32 = sha256(&expected_secret);

    let extracted = extract_secret(secret_hash_32.as_slice(), &tx_hex).unwrap();
    assert_eq!(extracted, expected_secret);
}

#[cfg(test)]
mod refund_wait_tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;

    #[test]
    fn wall_clock_wait_below_cap_is_not_floored() {
        // 5 minutes remaining (+1 s margin) must be returned as is, not raised to an hour.
        let r = refund_eligibility(NOW, NOW + 300, None);
        assert!(matches!(r, CanRefundHtlc::HaveToWait(301)));
    }

    #[test]
    fn wall_clock_wait_above_cap_is_capped_at_one_hour() {
        let r = refund_eligibility(NOW, NOW + 3 * 3600, None);
        assert!(matches!(r, CanRefundHtlc::HaveToWait(3600)));
    }

    #[test]
    fn mtp_wait_below_cap_is_not_floored() {
        // Wall clock is past the lock-time but the median is 5 minutes short of the P2SH lock-time.
        let r = refund_eligibility(NOW, NOW - 10, Some((1_000_300, 1_000_000)));
        assert!(matches!(r, CanRefundHtlc::HaveToWait(301)));
    }

    #[test]
    fn mtp_wait_above_cap_is_capped_at_one_hour() {
        let r = refund_eligibility(NOW, NOW - 10, Some((1_000_000 + 3 * 3600, 1_000_000)));
        assert!(matches!(r, CanRefundHtlc::HaveToWait(3600)));
    }

    #[test]
    fn past_lock_time_is_refundable() {
        let r = refund_eligibility(NOW, NOW - 10, Some((999_999, 1_000_000)));
        assert!(matches!(r, CanRefundHtlc::CanRefundNow));
    }
}
