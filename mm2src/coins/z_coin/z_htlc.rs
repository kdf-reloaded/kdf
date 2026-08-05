// historical milestone, first performed RICK/ZOMBIE swap
// dex fee - https://zombie.explorer.lordofthechains.com/tx/40bec29f268c349722a3228743e5c5b461cf16d124cddcfd2fc624fe895a0bdd
// maker payment - https://rick.explorer.dexstats.info/tx/9d36e95e5147450399895f0f248ac2e2de13382401c2986e134cc3d62bda738e
// taker payment - https://zombie.explorer.lordofthechains.com/tx/b248992e064fab579774c0479b04043091cf62f3975cb1664ea7d4f857ebe6f8
// taker payment spend - https://zombie.explorer.lordofthechains.com/tx/af6bb0f99f9a5a070a0c1f53d69e4189b0e9b68f9d66e69f201a6b6d9f93897e
// maker payment spend - https://rick.explorer.dexstats.info/tx/6a2dcc866ad75cebecb780a02320073a88bcf5e57ddccbe2657494e7747d591e

use super::ZCoin;
use crate::utxo::rpc_clients::{UtxoRpcClientEnum, UtxoRpcError};
use crate::utxo::utxo_common::payment_script;
use crate::utxo::{sat_from_big_decimal, UtxoAddressFormat};
use crate::z_coin::{SendOutputsErr, ZOutput, DEX_FEE_OVK};
use crate::{NumConversError, PrivKeyNotAllowed, TransactionEnum};
use bigdecimal::BigDecimal;
use chain::Transaction as UtxoTx;
use derive_more::Display;
use futures::compat::Future01CompatExt;
use kdf_crypto::dhash160;
use keys::{Address, Public};
use mm2_err_handle::prelude::*;
use script::{Builder as ScriptBuilder, Opcode, Script};
use serialization::deserialize;
use std::convert::Infallible;
use zcash_primitives::transaction::builder::{BuildConfig, Builder as ZTxBuilder};
use zcash_primitives::transaction::fees::fixed::FeeRule as FixedFeeRule;
use zcash_primitives::transaction::Transaction as ZTransaction;
use zcash_protocol::consensus;
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis as Amount;
use zcash_script::script::Code as ZCashScriptCode;
use zcash_secp256k1::SecretKey;
use zcash_transparent::address::Script as ZCashScript;
use zcash_transparent::builder::TransparentSigningSet;
use zcash_transparent::bundle::{OutPoint as ZCashOutpoint, TxOut};

type ZTxBuilderError = zcash_primitives::transaction::builder::Error<Infallible>;

/// Sends HTLC output from the coin's my_z_addr
pub async fn z_send_htlc(
    coin: &ZCoin,
    time_lock: u32,
    my_pub: &Public,
    other_pub: &Public,
    secret_hash: &[u8],
    amount: BigDecimal,
) -> Result<ZTransaction, MmError<SendOutputsErr>> {
    let payment_script = payment_script(time_lock, secret_hash, my_pub, other_pub);
    let script_hash = dhash160(&payment_script);
    let htlc_address = Address {
        prefix: coin.utxo_arc.conf.p2sh_addr_prefix,
        t_addr_prefix: coin.utxo_arc.conf.p2sh_t_addr_prefix,
        hash: script_hash.into(),
        checksum_type: coin.utxo_arc.conf.checksum_type,
        addr_format: UtxoAddressFormat::Standard,
        hrp: None,
    };

    let amount_sat = sat_from_big_decimal(&amount, coin.utxo_arc.decimals).mm_err(Into::into)?;
    let address = htlc_address.to_string();
    if let UtxoRpcClientEnum::Native(native) = coin.rpc_client() {
        native.import_address(&address, &address, false).compat().await.unwrap();
    }

    let htlc_script = ScriptBuilder::build_p2sh(&script_hash.into()).to_bytes().take();
    let htlc_output = TxOut::new(
        Amount::from_u64(amount_sat).map_err(|_| NumConversError::new("Invalid ZCash amount".into()))?,
        ZCashScript(ZCashScriptCode(htlc_script)),
    );

    let opret_script = ScriptBuilder::default()
        .push_opcode(Opcode::OP_RETURN)
        .push_data(&payment_script)
        .into_bytes()
        .take();
    let op_return_out = TxOut::new(Amount::ZERO, ZCashScript(ZCashScriptCode(opret_script)));
    let mm_tx = coin.send_outputs(vec![htlc_output, op_return_out], vec![]).await?;

    Ok(mm_tx)
}

/// Sends HTLC output from the coin's my_z_addr
pub async fn z_send_dex_fee(
    coin: &ZCoin,
    amount: BigDecimal,
    uuid: &[u8],
) -> Result<ZTransaction, MmError<SendOutputsErr>> {
    let dex_fee_amount = sat_from_big_decimal(&amount, coin.utxo_arc.decimals).mm_err(Into::into)?;
    let dex_fee_out = ZOutput {
        to_addr: coin.z_fields.dex_fee_addr.clone(),
        amount: Amount::from_u64(dex_fee_amount).map_err(|_| NumConversError::new("Invalid ZCash amount".into()))?,
        viewing_key: Some(DEX_FEE_OVK),
        memo: Some(MemoBytes::from_bytes(uuid).expect("uuid length < 512")),
    };

    let tx = coin.send_outputs(vec![], vec![dex_fee_out]).await?;

    Ok(tx)
}

#[derive(Debug, Display)]
#[allow(clippy::large_enum_variant, clippy::upper_case_acronyms)]
pub enum ZP2SHSpendError {
    ZTxBuilderError(ZTxBuilderError),
    PrivKeyNotAllowed(PrivKeyNotAllowed),
    Rpc(UtxoRpcError),
    #[display(fmt = "Invalid ZCash HTLC payment transaction: {}", _0)]
    InvalidPaymentTx(String),
    #[display(fmt = "{:?} {}", _0, _1)]
    TxRecoverable(TransactionEnum, String),
}

impl From<ZTxBuilderError> for ZP2SHSpendError {
    fn from(tx_builder: ZTxBuilderError) -> ZP2SHSpendError { ZP2SHSpendError::ZTxBuilderError(tx_builder) }
}

impl From<PrivKeyNotAllowed> for ZP2SHSpendError {
    fn from(err: PrivKeyNotAllowed) -> Self { ZP2SHSpendError::PrivKeyNotAllowed(err) }
}

impl From<UtxoRpcError> for ZP2SHSpendError {
    fn from(rpc: UtxoRpcError) -> ZP2SHSpendError { ZP2SHSpendError::Rpc(rpc) }
}

impl ZP2SHSpendError {
    #[inline]
    pub fn get_tx(&self) -> Option<TransactionEnum> {
        match self {
            ZP2SHSpendError::TxRecoverable(ref tx, _) => Some(tx.clone()),
            _ => None,
        }
    }
}

/// Spends P2SH output 0 to the coin's my_z_addr
pub async fn z_p2sh_spend(
    coin: &ZCoin,
    p2sh_tx: ZTransaction,
    tx_locktime: u32,
    input_sequence: u32,
    redeem_script: Script,
    script_data: Script,
    htlc_privkey: &[u8],
) -> Result<UtxoTx, MmError<ZP2SHSpendError>> {
    let current_block = coin
        .utxo_arc
        .rpc_client
        .get_block_count()
        .compat()
        .await
        .mm_err(Into::into)? as u32;
    let mut tx_builder = ZTxBuilder::new(
        coin.z_fields.consensus_params.clone(),
        current_block.into(),
        BuildConfig::Standard {
            sapling_anchor: Some(sapling::Anchor::empty_tree()),
            orchard_anchor: None,
        },
    );
    tx_builder.set_lock_time(tx_locktime);

    let secp_secret = SecretKey::from_slice(htlc_privkey)
        .map_err(|e| MmError::new(ZP2SHSpendError::InvalidPaymentTx(e.to_string())))?;
    let mut signing_set = TransparentSigningSet::new();
    let pubkey = signing_set.add_key(secp_secret);

    let outpoint = ZCashOutpoint::new(*p2sh_tx.txid().as_ref(), 0);
    let tx_out = p2sh_tx
        .transparent_bundle()
        .and_then(|bundle| bundle.vout.first())
        .cloned()
        .or_mm_err(|| ZP2SHSpendError::InvalidPaymentTx("transparent output 0 is missing".to_owned()))?;
    let redeem_script = zcash_script::script::FromChain::parse(&ZCashScriptCode(redeem_script.to_vec()))
        .map_err(|e| MmError::new(ZP2SHSpendError::InvalidPaymentTx(e.to_string())))?;
    let script_data = zcash_script::script::Sig::parse(&ZCashScriptCode(script_data.to_vec()))
        .map_err(|e| MmError::new(ZP2SHSpendError::InvalidPaymentTx(e.to_string())))?;
    tx_builder
        .add_kdf_p2sh_input(pubkey, outpoint, tx_out, redeem_script, script_data, input_sequence)
        .map_err(|e| MmError::new(ZP2SHSpendError::InvalidPaymentTx(e.to_string())))?;
    let fee = Amount::const_from_u64(1_000);
    let payment_value = p2sh_tx
        .transparent_bundle()
        .and_then(|bundle| bundle.vout.first())
        .map(TxOut::value)
        .and_then(|value| value - fee)
        .or_mm_err(|| ZP2SHSpendError::InvalidPaymentTx("output 0 cannot cover the swap spend fee".to_owned()))?;
    tx_builder
        .add_sapling_output(None, coin.z_fields.my_z_addr.clone(), payment_value, MemoBytes::empty())
        .map_to_mm(ZP2SHSpendError::from)?;

    let zcash_tx = tx_builder
        .build(
            &signing_set,
            &[],
            &[],
            rand::rngs::OsRng,
            &coin.z_fields.z_tx_prover,
            &coin.z_fields.z_tx_prover,
            &FixedFeeRule::non_standard(fee),
        )
        .map_to_mm(ZP2SHSpendError::from)?
        .into_transaction();

    let mut tx_buffer = Vec::with_capacity(1024);
    zcash_tx
        .write(&mut tx_buffer)
        .map_err(|e| MmError::new(ZP2SHSpendError::InvalidPaymentTx(e.to_string())))?;
    let refund_tx: UtxoTx = deserialize(tx_buffer.as_slice())
        .map_err(|e| MmError::new(ZP2SHSpendError::InvalidPaymentTx(e.to_string())))?;

    match coin.rpc_client().send_raw_transaction(tx_buffer.into()).compat().await {
        Ok(_) => (),
        Err(e) => return Err(ZP2SHSpendError::TxRecoverable(refund_tx.into(), e.to_string()).into()),
    };

    Ok(refund_tx)
}
