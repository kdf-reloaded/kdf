use crate::utxo::rpc_clients::{UnspentInfo, UtxoRpcClientEnum, UtxoRpcClientOps, UtxoRpcError, UtxoRpcFut,
                               UtxoRpcResult};
use crate::utxo::utxo_builder::{UtxoCoinBuilderCommonOps, UtxoCoinWithIguanaPrivKeyBuilder,
                                UtxoFieldsWithIguanaPrivKeyBuilder};
use crate::utxo::utxo_common::{big_decimal_from_sat_unsigned, payment_script};
use crate::utxo::{sat_from_big_decimal, utxo_common, zcash_params_path, ActualTxFee, AdditionalTxData, Address,
                  BroadcastTxErr, FeePolicy, GetUtxoListOps, HistoryUtxoTx, HistoryUtxoTxMap, MatureUnspentList,
                  RecentlySpentOutPointsGuard, UtxoActivationParams, UtxoAddressFormat, UtxoArc, UtxoCoinFields,
                  UtxoCommonOps, UtxoFeeDetails, UtxoTxBroadcastOps, UtxoTxGenerationOps, UtxoWeak,
                  VerboseTransactionFrom};
use crate::{BalanceFut, CoinBalance, DexFee, FeeApproxStage, FoundSwapTxSpend, HistorySyncState, MarketCoinOps,
            MmCoin, NegotiateSwapContractAddrErr, NumConversError, RawTransactionFut, RawTransactionRequest,
            SignRawTransactionRequest, SignatureError, SignatureResult, SwapOps, TradeFee, TradePreimageFut,
            TradePreimageResult, TradePreimageValue, TransactionDetails, TransactionEnum, TransactionFut,
            TxFeeDetails, UnexpectedDerivationMethod, ValidateAddressResult, ValidateFeeArgs, ValidatePaymentInput,
            VerificationError, VerificationResult, WatcherOps, WithdrawFut, WithdrawRequest};
use crate::{Transaction, WithdrawError};
use async_trait::async_trait;
use chain::constants::SEQUENCE_FINAL;
use chain::{Transaction as UtxoTx, TransactionOutput};
use common::executor::{spawn, Timer};
use common::mm_number::{BigDecimal, MmNumber};
use common::{log, now_ms};
use crypto::privkey::key_pair_from_secret;
use db_common::sqlite::rusqlite::types::Type;
use db_common::sqlite::rusqlite::{Connection, Error as SqliteError, Row, ToSql, NO_PARAMS};
use futures::compat::Future01CompatExt;
use futures::lock::Mutex as AsyncMutex;
use futures::{FutureExt, TryFutureExt};
use futures01::Future;
use kdf_crypto::dhash160;
use keys::hash::H256;
use keys::{KeyPair, Public};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
#[cfg(test)] use mocktopus::macros::*;
use primitives::bytes::Bytes;
use rpc::v1::types::{Bytes as BytesJson, ToTxHash, Transaction as RpcTransaction, H256 as H256Json};
use script::{Builder as ScriptBuilder, Opcode, Script, TransactionInputSigner};
use serde_json::Value as Json;
use serialization::{deserialize, serialize_list, CoinVariant, Reader};
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use zcash_client_backend::decrypt_transaction;
use zcash_client_backend::encoding::{decode_payment_address, encode_extended_spending_key, encode_payment_address};
use zcash_client_backend::wallet::AccountId;
use zcash_primitives::consensus::{BlockHeight, NetworkUpgrade, H0};
use zcash_primitives::memo::MemoBytes;
use zcash_primitives::merkle_tree::{CommitmentTree, Hashable, IncrementalWitness};
use zcash_primitives::sapling::keys::OutgoingViewingKey;
use zcash_primitives::sapling::note_encryption::try_sapling_output_recovery;
use zcash_primitives::sapling::{Node, Note};
use zcash_primitives::transaction::builder::Builder as ZTxBuilder;
use zcash_primitives::transaction::components::{Amount, TxOut};
use zcash_primitives::transaction::Transaction as ZTransaction;
use zcash_primitives::{consensus, constants::mainnet as z_mainnet_constants, sapling::PaymentAddress,
                       zip32::ExtendedFullViewingKey, zip32::ExtendedSpendingKey};
use zcash_proofs::prover::LocalTxProver;

mod z_htlc;
use z_htlc::{z_p2sh_spend, z_send_dex_fee, z_send_htlc};

mod z_rpc;
use z_rpc::{ZRpcOps, ZUnspent};

mod z_coin_errors;
pub use z_coin_errors::*;

/// `ZP2SHSpendError` compatible `TransactionErr` handling macro.
macro_rules! try_ztx_s {
    ($e: expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => {
                if let Some(tx) = err.get_inner().get_tx() {
                    return Err(crate::TransactionErr::TxRecoverable(
                        tx,
                        format!("{}:{}] {:?}", file!(), line!(), err),
                    ));
                }

                return Err(crate::TransactionErr::Plain(ERRL!("{:?}", err)));
            },
        }
    };
}

mod z_coin_ops;
mod z_swap_ops;

#[cfg(all(test, feature = "zhtlc-native-tests"))]
mod z_coin_tests;

#[derive(Debug, Clone)]
pub struct ARRRConsensusParams {}

impl consensus::Parameters for ARRRConsensusParams {
    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match nu {
            NetworkUpgrade::Sapling => Some(BlockHeight::from_u32(1)),
            _ => None,
        }
    }

    fn coin_type(&self) -> u32 { z_mainnet_constants::COIN_TYPE }

    fn hrp_sapling_extended_spending_key(&self) -> &str { z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY }

    fn hrp_sapling_extended_full_viewing_key(&self) -> &str {
        z_mainnet_constants::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY
    }

    fn hrp_sapling_payment_address(&self) -> &str { z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS }

    fn b58_pubkey_address_prefix(&self) -> [u8; 2] { z_mainnet_constants::B58_PUBKEY_ADDRESS_PREFIX }

    fn b58_script_address_prefix(&self) -> [u8; 2] { z_mainnet_constants::B58_SCRIPT_ADDRESS_PREFIX }
}

/// Outgoing Viewing Key (OVK) used to encrypt the outgoing-cipher portion of
/// every Sapling output that pays a swap dex-fee on Pirate Chain (ARRR).
///
/// The byte value `[7; 32]` is **shared protocol convention**, not an
/// authorial choice:
///
/// 1. **Audit / interoperability convention.** Every atomic-swap participant
///    on the ARRR shielded fee path must use the same OVK so that anyone can
///    decrypt the outgoing memos of fee outputs and verify that fees were
///    actually paid to the expected address. Changing the value would
///    silently break the audit convention shared with all other
///    implementations of the same swap protocol (AtomicDEX, GLEEC, KDF-
///    Reloaded, and any third-party participant).
/// 2. **Baseline continuity.** The value, type, and source location are
///    preserved from the shared GPLv2 baseline `c1d46c0c1` to maintain
///    protocol-level continuity across legacy and current participants.
///
/// Do not refactor away or replace this constant without a protocol-level
/// compatibility analysis of the ARRR shielded-fee path.
const DEX_FEE_OVK: OutgoingViewingKey = OutgoingViewingKey([7; 32]);

const SAPLING_SPEND_NAME: &str = "sapling-spend.params";
const SAPLING_OUTPUT_NAME: &str = "sapling-output.params";
const SAPLING_SPEND_HASH: &str =
    "8270785a1a0d0bc77196f000ee6d221c9c9894f55307bd9357c3f0105d31ca63991ab91324160d8f53e2bbd3c2633a6eb8bdf5205d822e7f3f73edac51b2b70c";
const SAPLING_OUTPUT_HASH: &str =
    "657e3d38dbb5cb5e7dd2970e8b03d69b4787dd907285b5a7f0790dcc8072f60bf593b32cc2d1c030e00ff5ae64bf84c5c3beb84ddc841d48264b4a171744d028";

fn blake2b_file_hash(path: &Path) -> Result<String, std::io::Error> {
    let mut file = File::open(path)?;
    let mut state = blake2b_simd::State::new();
    let mut buffer = [0_u8; 1024 * 1024];

    loop {
        let read_bytes = file.read(&mut buffer)?;
        if read_bytes == 0 {
            break;
        }
        state.update(&buffer[..read_bytes]);
    }

    Ok(state.finalize().to_hex().to_string())
}

fn verify_zcash_params_integrity(params_dir: &Path) -> MmResult<(), ZCoinBuildError> {
    if !params_dir.exists() {
        return MmError::err(ZCoinBuildError::ZCashParamsDirNotFound {
            path: params_dir.display().to_string(),
        });
    }

    let verify = |file_name: &str, expected_hash: &str| {
        let path = params_dir.join(file_name);
        if !path.exists() {
            return MmError::err(ZCoinBuildError::ZCashParamsNotFound);
        }

        let actual_hash = blake2b_file_hash(&path).map_to_mm(|error| ZCoinBuildError::ZCashParamsReadError {
            file: file_name.to_string(),
            path: path.display().to_string(),
            error,
        })?;

        if actual_hash != expected_hash {
            return MmError::err(ZCoinBuildError::ZCashParamsHashMismatch {
                file: file_name.to_string(),
                path: path.display().to_string(),
                expected: expected_hash.to_string(),
                actual: actual_hash,
            });
        }

        Ok(())
    };

    verify(SAPLING_SPEND_NAME, SAPLING_SPEND_HASH)?;
    verify(SAPLING_OUTPUT_NAME, SAPLING_OUTPUT_HASH)?;
    Ok(())
}

pub struct ZCoinFields {
    dex_fee_addr: PaymentAddress,
    my_z_addr: PaymentAddress,
    my_z_addr_encoded: String,
    z_spending_key: ExtendedSpendingKey,
    z_tx_prover: LocalTxProver,
    /// Mutex preventing concurrent transaction generation/same input usage
    z_unspent_mutex: AsyncMutex<()>,
    sapling_state_synced: AtomicBool,
    /// SQLite connection that is used to cache Sapling data for shielded transactions creation
    sqlite: Mutex<Connection>,
}

impl std::fmt::Debug for ZCoinFields {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "ZCoinFields {{ my_z_addr: {:?}, my_z_addr_encoded: {} }}",
            self.my_z_addr, self.my_z_addr_encoded
        )
    }
}

impl Transaction for ZTransaction {
    fn tx_hex(&self) -> Vec<u8> {
        let mut hex = Vec::with_capacity(1024);
        self.write(&mut hex).expect("Writing should not fail");
        hex
    }

    fn tx_hash(&self) -> BytesJson {
        let mut bytes = self.txid().0.to_vec();
        bytes.reverse();
        bytes.into()
    }
}

#[derive(Clone, Debug)]
pub struct ZCoin {
    utxo_arc: UtxoArc,
    z_fields: Arc<ZCoinFields>,
}

pub struct ZOutput {
    pub to_addr: PaymentAddress,
    pub amount: Amount,
    pub viewing_key: Option<OutgoingViewingKey>,
    pub memo: Option<MemoBytes>,
}

impl AsRef<UtxoCoinFields> for ZCoin {
    fn as_ref(&self) -> &UtxoCoinFields { &self.utxo_arc }
}

pub async fn z_coin_from_conf_and_params(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &UtxoActivationParams,
    secp_priv_key: &[u8],
) -> Result<ZCoin, MmError<ZCoinBuildError>> {
    let db_dir_path = ctx.dbdir();
    let z_key = ExtendedSpendingKey::master(secp_priv_key);
    z_coin_from_conf_and_params_with_z_key(ctx, ticker, conf, params, secp_priv_key, db_dir_path, z_key).await
}

fn init_db(sql: &Connection) -> Result<(), SqliteError> {
    const INIT_SAPLING_CACHE_TABLE_STMT: &str = "CREATE TABLE IF NOT EXISTS sapling_cache (
        height INTEGER NOT NULL PRIMARY KEY,
        prev_tree_state BLOB NOT NULL,
        cmus BLOB NOT NULL
    );";

    sql.execute(INIT_SAPLING_CACHE_TABLE_STMT, NO_PARAMS).map(|_| ())
}

struct SaplingBlockState {
    height: u32,
    prev_tree_state: CommitmentTree<Node>,
    cmus: Vec<H256>,
}

impl TryFrom<&Row<'_>> for SaplingBlockState {
    type Error = SqliteError;

    fn try_from(row: &Row<'_>) -> Result<SaplingBlockState, SqliteError> {
        let height = row.get(0)?;
        let prev_state_bytes: Vec<u8> = row.get(1)?;
        let cmus_bytes: Vec<u8> = row.get(2)?;

        let prev_tree_state = CommitmentTree::read(prev_state_bytes.as_slice())
            .map_err(|e| SqliteError::FromSqlConversionFailure(1, Type::Blob, Box::new(e)))?;

        let mut reader = Reader::from_read(cmus_bytes.as_slice());
        let cmus = reader
            .read_list()
            .map_err(|e| SqliteError::FromSqlConversionFailure(2, Type::Blob, Box::new(e)))?;
        Ok(SaplingBlockState {
            height,
            prev_tree_state,
            cmus,
        })
    }
}

fn query_latest_block(conn: &Connection) -> Result<SaplingBlockState, SqliteError> {
    const QUERY_LATEST_BLOCK_STMT: &str =
        "SELECT height, prev_tree_state, cmus FROM sapling_cache ORDER BY height desc LIMIT 1";

    conn.query_row(QUERY_LATEST_BLOCK_STMT, NO_PARAMS, |row: &Row<'_>| {
        SaplingBlockState::try_from(row)
    })
}

#[allow(clippy::needless_question_mark)]
fn query_states_after_height(conn: &Connection, height: u32) -> Result<Vec<SaplingBlockState>, SqliteError> {
    const GET_BLOCK_STATES_AFTER_HEIGHT: &str =
        "SELECT height, prev_tree_state, cmus from sapling_cache WHERE height >= ?1 ORDER BY height ASC;";

    let mut statement = conn.prepare(GET_BLOCK_STATES_AFTER_HEIGHT)?;

    #[allow(clippy::redundant_closure)]
    let rows: Result<Vec<_>, _> = statement
        .query_map(&[height.to_sql()?], |row: &Row<'_>| SaplingBlockState::try_from(row))?
        .collect();

    Ok(rows?)
}

fn insert_block_state(conn: &Connection, state: SaplingBlockState) -> Result<(), SqliteError> {
    const INSERT_BLOCK_STMT: &str = "INSERT INTO sapling_cache (height, prev_tree_state, cmus) VALUES (?1, ?2, ?3);";
    let block = state.height.to_sql()?;
    let mut tree_bytes = Vec::new();
    state
        .prev_tree_state
        .write(&mut tree_bytes)
        .expect("write should not fail");

    let prev_tree = tree_bytes.to_sql()?;

    let cmus = serialize_list(&state.cmus).take();
    let cmus = cmus.to_sql()?;

    conn.execute(INSERT_BLOCK_STMT, &[block, prev_tree, cmus]).map(|_| ())
}

async fn sapling_state_cache_loop(coin: ZCoin) {
    let query = tokio::task::block_in_place(|| query_latest_block(&coin.sqlite_conn()));
    let (mut processed_height, mut current_tree) = match query {
        Ok(state) => {
            let mut tree = state.prev_tree_state;
            for cmu in state.cmus {
                tree.append(Node::new(cmu.take())).expect("Commitment tree not full");
            }
            (state.height, tree)
        },
        Err(_) => (0, CommitmentTree::empty()),
    };

    let (utxo_weak, z_fields_weak) = coin.into_weak_parts();

    let zero_root = Some(H256Json::default());
    while let Some(coin) = ZCoin::from_weak_parts(&utxo_weak, &z_fields_weak) {
        coin.z_fields.sapling_state_synced.store(false, AtomicOrdering::Relaxed);
        let current_block = match coin.rpc_client().get_block_count().compat().await {
            Ok(b) => b,
            Err(e) => {
                log::error!("Error {} on getting block count", e);
                Timer::sleep(10.).await;
                continue;
            },
        };

        let native_client = match coin.rpc_client() {
            UtxoRpcClientEnum::Native(n) => n,
            _ => unimplemented!("Implemented only for native client"),
        };
        while processed_height as u64 <= current_block {
            let block = match native_client.get_block_by_height(processed_height as u64).await {
                Ok(b) => b,
                Err(e) => {
                    log::error!("Error {} on getting block", e);
                    Timer::sleep(1.).await;
                    continue;
                },
            };
            let current_sapling_root = current_tree.root();
            let mut root_bytes = [0u8; 32];
            current_sapling_root
                .write(&mut root_bytes as &mut [u8])
                .expect("Root len is 32 bytes");

            let current_sapling_root = Some(H256::from(root_bytes).reversed().into());
            if current_sapling_root != block.final_sapling_root && block.final_sapling_root != zero_root {
                let prev_tree_state = current_tree.clone();
                let mut cmus = Vec::new();
                for hash in block.tx {
                    let tx = native_client
                        .get_transaction_bytes(&hash)
                        .compat()
                        .await
                        .expect("Panic here to avoid storing invalid tree state to the DB");
                    let tx: UtxoTx = deserialize(tx.as_slice()).expect("Panic here to avoid invalid tree state");
                    for output in tx.shielded_outputs {
                        current_tree
                            .append(Node::new(output.cmu.take()))
                            .expect("Commitment tree not full");
                        cmus.push(output.cmu);
                    }
                }

                let state_to_insert = SaplingBlockState {
                    height: processed_height + 1,
                    prev_tree_state,
                    cmus,
                };
                insert_block_state(&coin.sqlite_conn(), state_to_insert).expect("Insertion should not fail");
            }
            processed_height += 1;
        }
        coin.z_fields.sapling_state_synced.store(true, AtomicOrdering::Relaxed);
        drop(coin);
        Timer::sleep(10.).await;
    }
}

pub struct ZCoinBuilder<'a> {
    ctx: &'a MmArc,
    ticker: &'a str,
    conf: &'a Json,
    params: &'a UtxoActivationParams,
    secp_priv_key: &'a [u8],
    db_dir_path: PathBuf,
    z_spending_key: ExtendedSpendingKey,
}

impl<'a> UtxoCoinBuilderCommonOps for ZCoinBuilder<'a> {
    fn ctx(&self) -> &MmArc { self.ctx }

    fn conf(&self) -> &Json { self.conf }

    fn activation_params(&self) -> &UtxoActivationParams { self.params }

    fn ticker(&self) -> &str { self.ticker }
}

#[async_trait]
impl<'a> UtxoFieldsWithIguanaPrivKeyBuilder for ZCoinBuilder<'a> {}

#[async_trait]
impl<'a> UtxoCoinWithIguanaPrivKeyBuilder for ZCoinBuilder<'a> {
    type ResultCoin = ZCoin;
    type Error = ZCoinBuildError;

    fn priv_key(&self) -> &[u8] { self.secp_priv_key }

    async fn build(self) -> MmResult<Self::ResultCoin, Self::Error> {
        let utxo = self
            .build_utxo_fields_with_iguana_priv_key(self.priv_key())
            .await
            .mm_err(Into::into)?;
        let utxo_arc = UtxoArc::new(utxo);
        let db_name = format!("{}_CACHE.db", self.ticker);
        let mut db_dir_path = self.db_dir_path;

        db_dir_path.push(&db_name);
        let sqlite = tokio::task::block_in_place(move || {
            if !db_dir_path.exists() {
                let default_cache_path = PathBuf::from(format!("./{}", db_name));
                if !default_cache_path.exists() {
                    return MmError::err(ZCoinBuildError::SaplingCacheDbDoesNotExist {
                        path: std::env::current_dir()?.join(&default_cache_path).display().to_string(),
                    });
                }
                std::fs::copy(default_cache_path, &db_dir_path)?;
            }

            let sqlite = Connection::open(db_dir_path)?;
            init_db(&sqlite)?;
            Ok(sqlite)
        })?;

        let (_, my_z_addr) = self
            .z_spending_key
            .default_address()
            .map_err(|_| MmError::new(ZCoinBuildError::GetAddressError))?;

        let dex_fee_z_addr = mm2_net_config::net_config_or_panic(self.ctx.netid()).dex_fee_z_addr();
        let dex_fee_addr = decode_payment_address(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS, dex_fee_z_addr)
            .expect("NetConfig dex_fee_z_addr must be a valid z-address")
            .expect("NetConfig dex_fee_z_addr must be a valid z-address");

        let params_dir = zcash_params_path();
        verify_zcash_params_integrity(&params_dir)?;
        let z_tx_prover = tokio::task::block_in_place(|| {
            LocalTxProver::new(
                &params_dir.join(SAPLING_SPEND_NAME),
                &params_dir.join(SAPLING_OUTPUT_NAME),
            )
        });

        let my_z_addr_encoded = encode_payment_address(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS, &my_z_addr);
        let my_z_key_encoded = encode_extended_spending_key(
            z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY,
            &self.z_spending_key,
        );

        let z_fields = ZCoinFields {
            dex_fee_addr,
            my_z_addr,
            my_z_addr_encoded,
            z_spending_key: self.z_spending_key,
            z_tx_prover,
            z_unspent_mutex: AsyncMutex::new(()),
            sapling_state_synced: AtomicBool::new(false),
            sqlite: Mutex::new(sqlite),
        };

        let z_coin = ZCoin {
            utxo_arc,
            z_fields: Arc::new(z_fields),
        };

        z_coin
            .z_rpc()
            .z_import_key(&my_z_key_encoded)
            .compat()
            .await
            .mm_err(Into::into)?;
        spawn(sapling_state_cache_loop(z_coin.clone()));
        Ok(z_coin)
    }
}

impl<'a> ZCoinBuilder<'a> {
    pub fn new(
        ctx: &'a MmArc,
        ticker: &'a str,
        conf: &'a Json,
        params: &'a UtxoActivationParams,
        secp_priv_key: &'a [u8],
        db_dir_path: PathBuf,
        z_spending_key: ExtendedSpendingKey,
    ) -> ZCoinBuilder<'a> {
        ZCoinBuilder {
            ctx,
            ticker,
            conf,
            params,
            secp_priv_key,
            db_dir_path,
            z_spending_key,
        }
    }
}

async fn z_coin_from_conf_and_params_with_z_key(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &UtxoActivationParams,
    secp_priv_key: &[u8],
    db_dir_path: PathBuf,
    z_spending_key: ExtendedSpendingKey,
) -> Result<ZCoin, MmError<ZCoinBuildError>> {
    let builder = ZCoinBuilder::new(ctx, ticker, conf, params, secp_priv_key, db_dir_path, z_spending_key);
    builder.build().await
}

impl MarketCoinOps for ZCoin {
    fn ticker(&self) -> &str { &self.utxo_arc.conf.ticker }

    fn my_address(&self) -> Result<String, String> { Ok(self.z_fields.my_z_addr_encoded.clone()) }

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> { unimplemented!() }

    fn sign_message_hash(&self, _message: &str) -> Option<[u8; 32]> { unimplemented!() }

    fn sign_message(&self, _message: &str) -> SignatureResult<String> {
        MmError::err(SignatureError::InvalidRequest(
            "Message signing is not supported by the given coin type".to_string(),
        ))
    }

    fn verify_message(&self, _signature_base64: &str, _message: &str, _address: &str) -> VerificationResult<bool> {
        MmError::err(VerificationError::InvalidRequest(
            "Message verification is not supported by the given coin type".to_string(),
        ))
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let coin = self.clone();
        let fut = async move {
            let unspents = coin.my_z_unspents_ordered().await.mm_err(Into::into)?;
            let (spendable, unspendable) = unspents.iter().fold(
                (BigDecimal::from(0), BigDecimal::from(0)),
                |(cur_spendable, cur_unspendable), unspent| {
                    if unspent.confirmations > 0 {
                        (cur_spendable + unspent.amount.to_decimal(), cur_unspendable)
                    } else {
                        (cur_spendable, cur_unspendable + unspent.amount.to_decimal())
                    }
                },
            );
            Ok(CoinBalance { spendable, unspendable })
        };
        Box::new(fut.boxed().compat())
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> { utxo_common::base_coin_balance(self) }

    fn platform_ticker(&self) -> &str { self.ticker() }

    #[inline(always)]
    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        utxo_common::send_raw_tx(self.as_ref(), tx)
    }

    #[inline(always)]
    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        utxo_common::send_raw_tx_bytes(self.as_ref(), tx)
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        utxo_common::wait_for_confirmations(self.as_ref(), tx, confirmations, requires_nota, wait_until, check_every)
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::wait_for_output_spend(
            self.as_ref(),
            transaction,
            utxo_common::DEFAULT_SWAP_VOUT,
            from_block,
            wait_until,
        )
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        ZTransaction::read(bytes).map(|tx| tx.into()).map_err(|e| e.to_string())
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> {
        utxo_common::current_block(&self.utxo_arc)
    }

    fn display_priv_key(&self) -> Result<String, String> {
        Ok(encode_extended_spending_key(
            z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY,
            &self.z_fields.z_spending_key,
        ))
    }

    fn min_tx_amount(&self) -> BigDecimal { utxo_common::min_tx_amount(self.as_ref()) }

    fn min_trading_vol(&self) -> MmNumber { utxo_common::min_trading_vol(self.as_ref()) }

    fn sign_raw_tx(&self, args: &SignRawTransactionRequest) -> RawTransactionFut {
        Box::new(utxo_common::sign_raw_tx(self.clone(), args.clone()).boxed().compat())
    }

    fn is_privacy(&self) -> bool { true }
}

#[async_trait]
impl MmCoin for ZCoin {
    fn is_asset_chain(&self) -> bool { self.utxo_arc.conf.asset_chain }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        let coin = self.clone();
        let fut = async move {
            if req.fee.is_some() {
                return MmError::err(WithdrawError::InternalError(
                    "Setting a custom withdraw fee is not supported for ZCoin yet".to_owned(),
                ));
            }

            let to_addr = decode_payment_address(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS, &req.to)
                .map_to_mm(|e| WithdrawError::InvalidAddress(format!("{}", e)))?
                .or_mm_err(|| WithdrawError::InvalidAddress(format!("Address {} decoded to None", req.to)))?;
            let amount = if req.max {
                let fee = coin.get_one_kbyte_tx_fee().await.mm_err(Into::into)?;
                let balance = coin.my_balance().compat().await.mm_err(Into::into)?;
                balance.spendable - fee
            } else {
                req.amount
            };
            let satoshi = sat_from_big_decimal(&amount, coin.decimals()).mm_err(Into::into)?;
            let z_output = ZOutput {
                to_addr,
                amount: Amount::from_u64(satoshi)
                    .map_to_mm(|_| NumConversError(format!("Failed to get ZCash amount from {}", amount)))
                    .mm_err(Into::into)?,
                // TODO add optional viewing_key and memo fields to the WithdrawRequest
                viewing_key: None,
                memo: None,
            };

            let (tx, data) = coin.gen_tx(vec![], vec![z_output]).await.mm_err(Into::into)?;
            let mut tx_bytes = Vec::with_capacity(1024);
            tx.write(&mut tx_bytes)
                .map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
            let mut tx_hash = tx.txid().0.to_vec();
            tx_hash.reverse();

            let my_balance_change = data.spent_by_me - data.received_by_me;

            Ok(TransactionDetails {
                tx_hex: tx_bytes.into(),
                tx_hash: tx_hash.to_tx_hash(),
                from: vec![coin.z_fields.my_z_addr_encoded.clone()],
                to: vec![req.to],
                total_amount: big_decimal_from_sat_unsigned(data.spent_by_me, coin.decimals()),
                spent_by_me: big_decimal_from_sat_unsigned(data.spent_by_me, coin.decimals()),
                received_by_me: big_decimal_from_sat_unsigned(data.received_by_me, coin.decimals()),
                my_balance_change: big_decimal_from_sat_unsigned(my_balance_change, coin.decimals()),
                block_height: 0,
                timestamp: 0,
                fee_details: Some(TxFeeDetails::Utxo(UtxoFeeDetails {
                    coin: Some(coin.utxo_arc.conf.ticker.clone()),
                    amount: big_decimal_from_sat_unsigned(data.fee_amount, coin.decimals()),
                })),
                coin: coin.ticker().to_owned(),
                internal_id: tx_hash.into(),
                kmd_rewards: None,
                transaction_type: Default::default(),
            })
        };
        Box::new(fut.boxed().compat())
    }

    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut {
        Box::new(utxo_common::get_raw_transaction(&self.utxo_arc, req).boxed().compat())
    }

    fn decimals(&self) -> u8 { self.utxo_arc.decimals }

    fn convert_to_address(&self, _from: &str, _to_address_format: Json) -> Result<String, String> {
        Err(MmError::new("Address conversion is not available for ZCoin".to_string()).to_string())
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult {
        match decode_payment_address(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS, address) {
            Ok(Some(_)) => ValidateAddressResult {
                is_valid: true,
                reason: None,
            },
            Ok(None) => ValidateAddressResult {
                is_valid: false,
                reason: Some("decode_payment_address returned None".to_owned()),
            },
            Err(e) => ValidateAddressResult {
                is_valid: false,
                reason: Some(format!("Error {} on decode_payment_address", e)),
            },
        }
    }

    fn process_history_loop(&self, _ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        log::warn!("process_history_loop is not implemented for ZCoin yet!");
        Box::new(futures01::future::err(()))
    }

    fn history_sync_status(&self) -> HistorySyncState { HistorySyncState::NotEnabled }

    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        utxo_common::get_trade_fee(self.clone())
    }

    async fn get_sender_trade_fee(
        &self,
        _value: TradePreimageValue,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Ok(TradeFee {
            coin: self.ticker().to_owned(),
            amount: self.get_one_kbyte_tx_fee().await.mm_err(Into::into)?.into(),
            paid_from_trading_vol: false,
        })
    }

    fn get_receiver_trade_fee(&self, _stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        utxo_common::get_receiver_trade_fee(self.clone())
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        _dex_fee_amount: BigDecimal,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Ok(TradeFee {
            coin: self.ticker().to_owned(),
            amount: self.get_one_kbyte_tx_fee().await.mm_err(Into::into)?.into(),
            paid_from_trading_vol: false,
        })
    }

    fn required_confirmations(&self) -> u64 { utxo_common::required_confirmations(&self.utxo_arc) }

    fn requires_notarization(&self) -> bool { utxo_common::requires_notarization(&self.utxo_arc) }

    fn set_required_confirmations(&self, confirmations: u64) {
        utxo_common::set_required_confirmations(&self.utxo_arc, confirmations)
    }

    fn set_requires_notarization(&self, requires_nota: bool) {
        utxo_common::set_requires_notarization(&self.utxo_arc, requires_nota)
    }

    fn swap_contract_address(&self) -> Option<BytesJson> { utxo_common::swap_contract_address() }

    fn mature_confirmations(&self) -> Option<u32> { Some(self.utxo_arc.conf.mature_confirmations) }

    fn coin_protocol_info(&self) -> Vec<u8> { utxo_common::coin_protocol_info(self) }

    fn is_coin_protocol_supported(&self, info: &Option<Vec<u8>>) -> bool {
        utxo_common::is_coin_protocol_supported(self, info)
    }
}

#[async_trait]
impl UtxoTxGenerationOps for ZCoin {
    async fn get_tx_fee(&self) -> UtxoRpcResult<ActualTxFee> { utxo_common::get_tx_fee(&self.utxo_arc).await }

    async fn calc_interest_if_required(
        &self,
        unsigned: TransactionInputSigner,
        data: AdditionalTxData,
        my_script_pub: Bytes,
    ) -> UtxoRpcResult<(TransactionInputSigner, AdditionalTxData)> {
        utxo_common::calc_interest_if_required(self, unsigned, data, my_script_pub).await
    }
}

#[async_trait]
impl UtxoTxBroadcastOps for ZCoin {
    async fn broadcast_tx(&self, tx: &UtxoTx) -> Result<H256Json, MmError<BroadcastTxErr>> {
        utxo_common::broadcast_tx(self, tx).await
    }
}

/// Please note `ZCoin` is not assumed to work with transparent UTXOs.
/// Remove implementation of the `GetUtxoListOps` trait for `ZCoin`
/// when [`ZCoin::preimage_trade_fee_required_to_send_outputs`] is refactored.
#[async_trait]
#[cfg_attr(test, mockable)]
impl GetUtxoListOps for ZCoin {
    async fn get_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_unspent_ordered_list(self, address).await
    }

    async fn get_all_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_all_unspent_ordered_list(self, address).await
    }

    async fn get_mature_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(MatureUnspentList, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_mature_unspent_ordered_list(self, address).await
    }
}

#[async_trait]
impl UtxoCommonOps for ZCoin {
    async fn get_htlc_spend_fee(&self, tx_size: u64) -> UtxoRpcResult<u64> {
        utxo_common::get_htlc_spend_fee(self, tx_size).await
    }

    fn addresses_from_script(&self, script: &Script) -> Result<Vec<Address>, String> {
        utxo_common::addresses_from_script(self, script)
    }

    fn denominate_satoshis(&self, satoshi: i64) -> f64 { utxo_common::denominate_satoshis(&self.utxo_arc, satoshi) }

    fn my_public_key(&self) -> Result<&Public, MmError<UnexpectedDerivationMethod>> {
        utxo_common::my_public_key(self.as_ref())
    }

    fn address_from_str(&self, address: &str) -> Result<Address, String> {
        utxo_common::checked_address_from_str(self, address)
    }

    async fn get_current_mtp(&self) -> UtxoRpcResult<u32> {
        utxo_common::get_current_mtp(&self.utxo_arc, CoinVariant::Standard).await
    }

    fn is_unspent_mature(&self, output: &RpcTransaction) -> bool {
        utxo_common::is_unspent_mature(self.utxo_arc.conf.mature_confirmations, output)
    }

    async fn calc_interest_of_tx(
        &self,
        _tx: &UtxoTx,
        _input_transactions: &mut HistoryUtxoTxMap,
    ) -> UtxoRpcResult<u64> {
        MmError::err(UtxoRpcError::Internal(
            "ZCoin doesn't support transaction rewards".to_owned(),
        ))
    }

    async fn get_mut_verbose_transaction_from_map_or_rpc<'a, 'b>(
        &'a self,
        tx_hash: H256Json,
        utxo_tx_map: &'b mut HistoryUtxoTxMap,
    ) -> UtxoRpcResult<&'b mut HistoryUtxoTx> {
        utxo_common::get_mut_verbose_transaction_from_map_or_rpc(self, tx_hash, utxo_tx_map).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn p2sh_spending_tx(
        &self,
        prev_transaction: UtxoTx,
        redeem_script: Bytes,
        outputs: Vec<TransactionOutput>,
        script_data: Script,
        sequence: u32,
        lock_time: u32,
        keypair: &KeyPair,
    ) -> Result<UtxoTx, String> {
        utxo_common::p2sh_spending_tx(
            self,
            prev_transaction,
            redeem_script,
            outputs,
            script_data,
            sequence,
            lock_time,
            keypair,
        )
        .await
    }

    fn get_verbose_transactions_from_cache_or_rpc(
        &self,
        tx_ids: HashSet<H256Json>,
    ) -> UtxoRpcFut<HashMap<H256Json, VerboseTransactionFrom>> {
        let selfi = self.clone();
        let fut = async move { utxo_common::get_verbose_transactions_from_cache_or_rpc(&selfi.utxo_arc, tx_ids).await };
        Box::new(fut.boxed().compat())
    }

    async fn preimage_trade_fee_required_to_send_outputs(
        &self,
        outputs: Vec<TransactionOutput>,
        fee_policy: FeePolicy,
        gas_fee: Option<u64>,
        stage: &FeeApproxStage,
    ) -> TradePreimageResult<BigDecimal> {
        utxo_common::preimage_trade_fee_required_to_send_outputs(self, outputs, fee_policy, gas_fee, stage).await
    }

    fn increase_dynamic_fee_by_stage(&self, dynamic_fee: u64, stage: &FeeApproxStage) -> u64 {
        utxo_common::increase_dynamic_fee_by_stage(self, dynamic_fee, stage)
    }

    async fn p2sh_tx_locktime(&self, htlc_locktime: u32) -> Result<u32, MmError<UtxoRpcError>> {
        utxo_common::p2sh_tx_locktime(self, self.ticker(), htlc_locktime).await
    }

    fn addr_format(&self) -> &UtxoAddressFormat { utxo_common::addr_format(self) }

    fn addr_format_for_standard_scripts(&self) -> UtxoAddressFormat {
        utxo_common::addr_format_for_standard_scripts(self)
    }

    fn address_from_pubkey(&self, pubkey: &Public) -> Address {
        let conf = &self.utxo_arc.conf;
        utxo_common::address_from_pubkey(
            pubkey,
            conf.pub_addr_prefix,
            conf.pub_t_addr_prefix,
            conf.checksum_type,
            conf.bech32_hrp.clone(),
            self.addr_format().clone(),
        )
    }
}

#[test]
fn derive_z_key_from_mm_seed() {
    use crypto::privkey::key_pair_from_seed;
    use zcash_client_backend::encoding::encode_extended_spending_key;

    let seed = "spice describe gravity federal blast come thank unfair canal monkey style afraid";
    let secp_keypair = key_pair_from_seed(seed).unwrap();
    let z_spending_key = ExtendedSpendingKey::master(&*secp_keypair.private().secret);
    let encoded = encode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, &z_spending_key);
    assert_eq!(encoded, "secret-extended-key-main1qqqqqqqqqqqqqqytwz2zjt587n63kyz6jawmflttqu5rxavvqx3lzfs0tdr0w7g5tgntxzf5erd3jtvva5s52qx0ms598r89vrmv30r69zehxy2r3vesghtqd6dfwdtnauzuj8u8eeqfx7qpglzu6z54uzque6nzzgnejkgq569ax4lmk0v95rfhxzxlq3zrrj2z2kqylx2jp8g68lqu6alczdxd59lzp4hlfuj3jp54fp06xsaaay0uyass992g507tdd7psua5w6q76dyq3");

    let (_, address) = z_spending_key.default_address().unwrap();
    let encoded_addr = encode_payment_address(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS, &address);
    assert_eq!(
        encoded_addr,
        "zs182ht30wnnnr8jjhj2j9v5dkx3qsknnr5r00jfwk2nczdtqy7w0v836kyy840kv2r8xle5gcl549"
    );

    let seed = "also shoot benefit prefer juice shell elder veteran woman mimic image kidney";
    let secp_keypair = key_pair_from_seed(seed).unwrap();
    let z_spending_key = ExtendedSpendingKey::master(&*secp_keypair.private().secret);
    let encoded = encode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, &z_spending_key);
    assert_eq!(encoded, "secret-extended-key-main1qqqqqqqqqqqqqq8jnhc9stsqwts6pu5ayzgy4szplvy03u227e50n3u8e6dwn5l0q5s3s8xfc03r5wmyh5s5dq536ufwn2k89ngdhnxy64sd989elwas6kr7ygztsdkw6k6xqyvhtu6e0dhm4mav8rus0fy8g0hgy9vt97cfjmus0m2m87p4qz5a00um7gwjwk494gul0uvt3gqyjujcclsqry72z57kr265jsajactgfn9m3vclqvx8fsdnwp4jwj57ffw560vvwks9g9hpu");

    let (_, address) = z_spending_key.default_address().unwrap();
    let encoded_addr = encode_payment_address(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS, &address);
    assert_eq!(
        encoded_addr,
        "zs1funuwrjr2stlr6fnhkdh7fyz3p7n0p8rxase9jnezdhc286v5mhs6q3myw0phzvad5mvqgfxpam"
    );
}
