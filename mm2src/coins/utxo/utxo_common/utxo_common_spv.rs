// utxo_common_spv — SPV proof validation, block header management

use super::*;

/// Cadence, in seconds, between header-sync ticks of `block_header_utxo_loop`.
const BLOCK_HEADER_SYNC_CHECK_EVERY: f64 = 60.;

pub async fn validate_spv_proof<T: UtxoCommonOps>(
    coin: T,
    tx: UtxoTx,
    try_spv_proof_until: u64,
) -> Result<(), MmError<SPVError>> {
    let client = match &coin.as_ref().rpc_client {
        UtxoRpcClientEnum::Native(_) => return Ok(()),
        UtxoRpcClientEnum::Electrum(electrum_client) => electrum_client,
    };
    if tx.outputs.is_empty() {
        return MmError::err(SPVError::InvalidVout);
    }

    let (merkle_branch, block_header) = spv_proof_retry_pool(&coin, client, &tx, try_spv_proof_until).await?;
    let raw_header = RawBlockHeader::new(block_header.raw().take())?;
    let intermediate_nodes: Vec<H256> = merkle_branch
        .merkle
        .into_iter()
        .map(|hash| hash.reversed().into())
        .collect();

    let proof = SPVProof {
        tx_id: tx.hash(),
        vin: serialize_list(&tx.inputs).take(),
        vout: serialize_list(&tx.outputs).take(),
        index: merkle_branch.pos as u64,
        confirming_header: block_header,
        raw_header,
        intermediate_nodes,
    };

    proof.validate().map_err(MmError::new)
}

async fn spv_proof_retry_pool<T: UtxoCommonOps>(
    coin: &T,
    client: &ElectrumClient,
    tx: &UtxoTx,
    try_spv_proof_until: u64,
) -> Result<(TxMerkleBranch, BlockHeader), MmError<SPVError>> {
    let mut height: Option<u64> = None;
    let mut merkle_branch: Option<TxMerkleBranch> = None;

    loop {
        if now_ms() / 1000 > try_spv_proof_until {
            error!(
                "Waited too long until {} for transaction {:?} to validate spv proof",
                try_spv_proof_until,
                tx.hash(),
            );
            return Err(SPVError::Timeout.into());
        }

        if height.is_none() {
            match get_tx_height(tx, client).await {
                Ok(h) => height = Some(h),
                Err(e) => {
                    debug!("`get_tx_height` returned an error {:?}", e);
                    error!("{:?} for tx {:?}", SPVError::InvalidHeight, tx);
                },
            }
        }

        if height.is_some() && merkle_branch.is_none() {
            match client
                .blockchain_transaction_get_merkle(tx.hash().reversed().into(), height.unwrap())
                .compat()
                .await
            {
                Ok(m) => merkle_branch = Some(m),
                Err(e) => {
                    debug!("`blockchain_transaction_get_merkle` returned an error {:?}", e);
                    error!(
                        "{:?} by tx: {:?}, height: {}",
                        SPVError::UnableToGetMerkle,
                        H256Json::from(tx.hash().reversed()),
                        height.unwrap()
                    );
                },
            }
        }

        if height.is_some() && merkle_branch.is_some() {
            match block_header_from_storage_or_rpc(&coin, height.unwrap(), &coin.as_ref().block_headers_storage, client)
                .await
            {
                Ok(block_header) => {
                    return Ok((merkle_branch.unwrap(), block_header));
                },
                Err(e) => {
                    debug!("`block_header_from_storage_or_rpc` returned an error {:?}", e);
                    error!(
                        "{:?}, Received header likely not compatible with header format in mm2",
                        SPVError::UnableToGetHeader
                    );
                },
            }
        }

        error!(
            "Failed spv proof validation for transaction {:?}, retrying in {} seconds.",
            tx.hash(),
            TRY_SPV_PROOF_INTERVAL,
        );

        Timer::sleep(TRY_SPV_PROOF_INTERVAL as f64).await;
    }
}

pub async fn get_tx_height(tx: &UtxoTx, client: &ElectrumClient) -> Result<u64, MmError<GetTxHeightError>> {
    for output in tx.outputs.clone() {
        let script_pubkey_str = hex::encode(electrum_script_hash(&output.script_pubkey));
        if let Ok(history) = client.scripthash_get_history(script_pubkey_str.as_str()).compat().await {
            if let Some(item) = history
                .into_iter()
                .find(|item| item.tx_hash.reversed() == H256Json(*tx.hash()) && item.height > 0)
            {
                return Ok(item.height as u64);
            }
        }
    }
    MmError::err(GetTxHeightError::HeightNotFound)
}

pub async fn valid_block_header_from_storage<T>(
    coin: &T,
    height: u64,
    storage: &BlockHeaderStorage,
    client: &ElectrumClient,
) -> Result<BlockHeader, MmError<GetBlockHeaderError>>
where
    T: AsRef<UtxoCoinFields>,
{
    match storage
        .get_block_header(coin.as_ref().conf.ticker.as_str(), height)
        .await
        .mm_err(Into::into)?
    {
        None => {
            let bytes = client.blockchain_block_header(height).compat().await?;
            let header: BlockHeader = deserialize(bytes.0.as_slice())?;
            let conf = &storage.conf;
            let blocks_limit = NonZeroU64::new(crate::utxo::DIFFICULTY_RETARGET_INTERVAL)
                .expect("difficulty-retarget interval is non-zero");
            let (headers_registry, headers) = client
                .retrieve_last_headers(blocks_limit, height)
                .compat()
                .await
                .mm_err(Into::into)?;
            // In trusted-RPC mode (no `validation_params`) headers are stored without validation.
            if conf.validation_params.is_some() {
                if let Err(err) = spv_validation::helpers_validation::validate_headers(
                    headers,
                    conf.difficulty_check(),
                    conf.constant_difficulty(),
                ) {
                    return MmError::err(GetBlockHeaderError::SPVError(err));
                }
            }
            storage
                .add_block_headers_to_storage(coin.as_ref().conf.ticker.as_str(), headers_registry)
                .await
                .mm_err(Into::into)?;
            Ok(header)
        },
        Some(header) => Ok(header),
    }
}

#[inline]
pub async fn block_header_from_storage_or_rpc<T>(
    coin: &T,
    height: u64,
    storage: &Option<BlockHeaderStorage>,
    client: &ElectrumClient,
) -> Result<BlockHeader, MmError<GetBlockHeaderError>>
where
    T: AsRef<UtxoCoinFields>,
{
    match storage {
        Some(ref storage) => valid_block_header_from_storage(&coin, height, storage, client).await,
        None => Ok(deserialize(
            client.blockchain_block_header(height).compat().await?.as_slice(),
        )?),
    }
}

pub async fn block_header_utxo_loop<T: UtxoCommonOps>(weak: UtxoWeak, constructor: impl Fn(UtxoArc) -> T) {
    {
        let coin = match weak.upgrade() {
            Some(arc) => constructor(arc),
            None => return,
        };
        let ticker = coin.as_ref().conf.ticker.as_str();
        let storage = match &coin.as_ref().block_headers_storage {
            None => return,
            Some(storage) => storage,
        };
        match storage.is_initialized_for(ticker).await {
            Ok(true) => info!("Block Header Storage already initialized for {}", ticker),
            Ok(false) => {
                if let Err(e) = storage.init(ticker).await {
                    error!(
                        "Couldn't initiate storage - aborting the block_header_utxo_loop: {:?}",
                        e
                    );
                    return;
                }
                info!("Block Header Storage successfully initialized for {}", ticker);
            },
            Err(_e) => return,
        };
        // Verify the configured trusted anchor against the coin's RPC at sync start (R37.1.3).
        if let UtxoRpcClientEnum::Electrum(client) = &coin.as_ref().rpc_client {
            let anchor = &storage.conf.starting_block_header;
            match verify_anchor_header(client, anchor).await {
                Ok(true) => {},
                Ok(false) => {
                    error!(
                        "Configured SPV starting header for {} does not match the chain - aborting the \
                         block_header_utxo_loop",
                        ticker
                    );
                    return;
                },
                Err(e) => {
                    error!(
                        "Couldn't verify the SPV starting header for {} - aborting the block_header_utxo_loop: {}",
                        ticker, e
                    );
                    return;
                },
            }
        }
    }
    while let Some(arc) = weak.upgrade() {
        let coin = constructor(arc);
        let storage = match &coin.as_ref().block_headers_storage {
            None => break,
            Some(storage) => storage,
        };
        let conf = storage.conf.clone();
        let check_every = BLOCK_HEADER_SYNC_CHECK_EVERY;
        let blocks_limit = NonZeroU64::new(crate::utxo::DIFFICULTY_RETARGET_INTERVAL)
            .expect("difficulty-retarget interval is non-zero");
        let height =
            ok_or_continue_after_sleep!(coin.as_ref().rpc_client.get_block_count().compat().await, check_every);
        let client = match &coin.as_ref().rpc_client {
            UtxoRpcClientEnum::Native(_) => break,
            UtxoRpcClientEnum::Electrum(client) => client,
        };
        let (block_registry, block_headers) = ok_or_continue_after_sleep!(
            client.retrieve_last_headers(blocks_limit, height).compat().await,
            check_every
        );
        // In trusted-RPC mode (no `validation_params`) headers are stored without validation.
        if conf.validation_params.is_some() {
            ok_or_continue_after_sleep!(
                validate_headers(block_headers, conf.difficulty_check(), conf.constant_difficulty()),
                check_every
            );
        }

        let ticker = coin.as_ref().conf.ticker.as_str();
        let anchor_height = conf.starting_block_header.height;
        // Active reorg detect-and-resolve (R37.7.2): compare the fetched batch against the stored
        // chain before the passive forward overwrite.
        let outcome = ok_or_continue_after_sleep!(
            reconcile_reorg(storage, ticker, &block_registry, anchor_height).await,
            check_every
        );
        match outcome {
            ReorgOutcome::NoReorg => {
                ok_or_continue_after_sleep!(
                    storage.add_block_headers_to_storage(ticker, block_registry).await,
                    check_every
                );
            },
            // The divergent suffix has already been removed and the candidate chain stored.
            ReorgOutcome::ForkAt(_) => {},
            ReorgOutcome::NeedMoreHistory => {
                ok_or_continue_after_sleep!(
                    walk_back_reorg(storage, client, &conf, ticker, height).await,
                    check_every
                );
            },
            ReorgOutcome::BadStartingHeaderChain => {
                error!(
                    "Bad starting-header chain for {}: the configured trusted anchor must be reconfigured \
                     - aborting the block_header_utxo_loop",
                    ticker
                );
                return;
            },
        }
        // Oldest-pruning: keep at most `max_stored_block_headers` of the most recent headers.
        if let Some(max_stored) = conf.max_stored_block_headers {
            ok_or_continue_after_sleep!(prune_old_headers(storage, ticker, max_stored.get()).await, check_every);
        }
        debug!("tick block_header_utxo_loop for {}", coin.as_ref().conf.ticker);
        Timer::sleep(check_every).await;
    }
}

/// Removes the oldest stored headers so that at most `max_stored` of the most recent headers remain
/// (R37.3.1 oldest-pruning). Pruning runs only when the stored tip exceeds the configured limit.
async fn prune_old_headers(
    storage: &BlockHeaderStorage,
    ticker: &str,
    max_stored: u64,
) -> Result<(), MmError<BlockHeaderStorageError>> {
    if let Some(tip) = storage.get_last_block_height(ticker).await? {
        if tip > max_stored {
            let bound = tip - max_stored;
            storage.remove_block_headers_from_to_height(ticker, 0, bound).await?;
        }
    }
    Ok(())
}

/// Compact difficulty bits of a header as a `u32`.
fn block_header_bits(header: &BlockHeader) -> u32 {
    match header.bits.clone() {
        chain::BlockHeaderBits::Compact(compact) => u32::from(compact),
        chain::BlockHeaderBits::U32(bits) => bits,
    }
}

/// Fetches the header at the configured anchor height from the coin's RPC and checks that its
/// hash, compact bits, and timestamp match the configured `starting_block_header` (R37.1.3).
async fn verify_anchor_header(client: &ElectrumClient, anchor: &crate::utxo::SPVBlockHeader) -> Result<bool, String> {
    let bytes = client
        .blockchain_block_header(anchor.height)
        .compat()
        .await
        .map_err(|e| e.to_string())?;
    let header: BlockHeader = deserialize(bytes.0.as_slice()).map_err(|e| format!("{:?}", e))?;
    // The configured hash is in the displayed (big-endian) hex form; reverse it for comparison.
    let configured_hash = anchor
        .hash
        .parse::<H256>()
        .map_err(|e| format!("invalid anchor hash '{}': {:?}", anchor.hash, e))?
        .reversed();
    Ok(header.hash() == configured_hash && block_header_bits(&header) == anchor.bits && header.time == anchor.time)
}

/// Outcome of comparing a freshly-fetched candidate batch against the stored header chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReorgOutcome {
    /// Stored and candidate headers agree over their overlap; normal forward sync proceeds.
    NoReorg,
    /// A fork was located: the inclusive suffix `[fork_height, tip]` is stale and is replaced by the
    /// candidate chain.
    ForkAt(u64),
    /// The divergence extends below the candidate window; a lower chunk must be re-fetched, bounded
    /// below by the configured anchor.
    NeedMoreHistory,
    /// The divergence reaches the configured trusted anchor; the starting header itself is bad.
    BadStartingHeaderChain,
}

/// Pure reorg detector (R37.7.2a). Locates the lowest height at which `stored` and `candidate`
/// disagree and classifies the divergence relative to the trusted `anchor_height`. `stored` is
/// expected to carry context down to one height below the lowest candidate height.
pub(crate) fn detect_reorg(
    stored: &HashMap<u64, BlockHeader>,
    candidate: &HashMap<u64, BlockHeader>,
    anchor_height: u64,
) -> ReorgOutcome {
    let low = match candidate.keys().min() {
        Some(low) => *low,
        None => return ReorgOutcome::NoReorg,
    };
    let high = candidate.keys().max().copied().unwrap_or(low);

    let mut fork = None;
    for height in low..=high {
        if let (Some(stored_header), Some(candidate_header)) = (stored.get(&height), candidate.get(&height)) {
            if stored_header.hash() != candidate_header.hash() {
                fork = Some(height);
                break;
            }
        }
    }
    let divergent = match fork {
        Some(height) => height,
        None => return ReorgOutcome::NoReorg,
    };
    // The divergence cannot be resolved below the trusted anchor.
    if divergent <= anchor_height {
        return ReorgOutcome::BadStartingHeaderChain;
    }
    // The header one below the divergence must be the agreed common ancestor that the candidate
    // chain builds upon. If it is not present or does not line up, the real fork is lower still.
    match (stored.get(&(divergent - 1)), candidate.get(&divergent)) {
        (Some(predecessor), Some(divergent_header)) if divergent_header.previous_header_hash == predecessor.hash() => {
            ReorgOutcome::ForkAt(divergent)
        },
        _ => ReorgOutcome::NeedMoreHistory,
    }
}

/// Detects a reorg against persisted storage and, on a resolvable fork, removes the divergent
/// suffix `[fork, tip]` and stores the candidate chain (R37.7.2b). Returns the classification.
async fn reconcile_reorg(
    storage: &BlockHeaderStorage,
    ticker: &str,
    candidate: &HashMap<u64, BlockHeader>,
    anchor_height: u64,
) -> Result<ReorgOutcome, MmError<BlockHeaderStorageError>> {
    let low = match candidate.keys().min() {
        Some(low) => *low,
        None => return Ok(ReorgOutcome::NoReorg),
    };
    let high = candidate.keys().max().copied().unwrap_or(low);
    let from = low.saturating_sub(1);

    let mut stored = HashMap::new();
    for height in from..=high {
        if let Some(header) = storage.get_block_header(ticker, height).await? {
            stored.insert(height, header);
        }
    }

    let outcome = detect_reorg(&stored, candidate, anchor_height);
    if let ReorgOutcome::ForkAt(fork) = outcome {
        if let Some(tip) = storage.get_last_block_height(ticker).await? {
            if fork <= tip {
                storage.remove_block_headers_from_to_height(ticker, fork, tip).await?;
            }
        }
        storage.add_block_headers_to_storage(ticker, candidate.clone()).await?;
    }
    Ok(outcome)
}

/// Walks the reorg search window strictly backward in bounded chunks toward the configured anchor
/// (R37.7.2b/c), re-fetching and re-validating each lower chunk until the fork is resolved, the
/// chain converges, or the anchor is reached (a bad starting-header chain).
async fn walk_back_reorg(
    storage: &BlockHeaderStorage,
    client: &ElectrumClient,
    conf: &crate::utxo::SPVConf,
    ticker: &str,
    from_height: u64,
) -> Result<(), String> {
    let anchor_height = conf.starting_block_header.height;
    let limit =
        NonZeroU64::new(crate::utxo::DIFFICULTY_RETARGET_INTERVAL).expect("difficulty-retarget interval is non-zero");
    let mut top = from_height;
    while top > anchor_height {
        let (registry, headers) = client
            .retrieve_last_headers(limit, top)
            .compat()
            .await
            .map_err(|e| e.to_string())?;
        // A fork that fails proof-of-work / difficulty is rejected; abandon this walk-back.
        if conf.validation_params.is_some()
            && validate_headers(headers, conf.difficulty_check(), conf.constant_difficulty()).is_err()
        {
            return Ok(());
        }
        match reconcile_reorg(storage, ticker, &registry, anchor_height)
            .await
            .map_err(|e| e.to_string())?
        {
            ReorgOutcome::ForkAt(_) | ReorgOutcome::NoReorg => return Ok(()),
            ReorgOutcome::BadStartingHeaderChain => {
                error!(
                    "Bad starting-header chain for {}: the configured trusted anchor must be reconfigured",
                    ticker
                );
                return Ok(());
            },
            ReorgOutcome::NeedMoreHistory => {
                let low = registry.keys().min().copied().unwrap_or(anchor_height);
                if low <= anchor_height {
                    error!(
                        "Bad starting-header chain for {}: the configured trusted anchor must be reconfigured",
                        ticker
                    );
                    return Ok(());
                }
                top = low - 1;
            },
        }
    }
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod reorg_tests {
    use super::*;
    use crate::utxo::utxo_sql_block_header_storage::SqliteBlockHeadersStorage;
    use crate::utxo::{SPVBlockHeader, SPVConf};
    use chain::BlockHeaderNonce;
    use common::block_on;

    // A real standard Bitcoin header used as the byte template for synthetic chains; only the
    // parent hash and nonce are mutated to chain and diverge headers.
    const TEMPLATE_HEADER_HEX: &str = "0000002076d41d3e4b0bfd4c0d3b30aa69fdff3ed35d85829efd04000000000000000000b386498b583390959d9bac72346986e3015e83ac0b54bc7747a11a494ac35c94bb3ce65a53fb45177f7e311c";

    fn template_header() -> BlockHeader {
        let bytes = hex::decode(TEMPLATE_HEADER_HEX).unwrap();
        deserialize(bytes.as_slice()).unwrap()
    }

    fn header_with(prev: H256, nonce: u32) -> BlockHeader {
        let mut header = template_header();
        header.previous_header_hash = prev;
        header.nonce = BlockHeaderNonce::U32(nonce);
        header
    }

    /// Builds a linked chain `[from_height..=to_height]` where each header's nonce is `nonce_base +
    /// height`, starting from the given parent hash, and returns it as a height-keyed map.
    fn build_chain(from_height: u64, to_height: u64, parent: H256, nonce_base: u32) -> HashMap<u64, BlockHeader> {
        let mut chain = HashMap::new();
        let mut prev = parent;
        for height in from_height..=to_height {
            let header = header_with(prev, nonce_base + height as u32);
            prev = header.hash();
            chain.insert(height, header);
        }
        chain
    }

    fn spv_conf(anchor_height: u64) -> SPVConf {
        SPVConf {
            starting_block_header: SPVBlockHeader {
                height: anchor_height,
                hash: "00".repeat(32),
                time: 0,
                bits: 0,
            },
            max_stored_block_headers: None,
            validation_params: None,
        }
    }

    fn in_memory_storage(anchor_height: u64) -> BlockHeaderStorage {
        BlockHeaderStorage {
            inner: Box::new(SqliteBlockHeadersStorage::in_memory()),
            conf: spv_conf(anchor_height),
        }
    }

    #[test]
    fn reorg_converges_on_heavier_valid_suffix() {
        let ticker = "REORG";
        let anchor = 2016u64;
        let tip = 2030u64;
        let fork = 2025u64;
        let new_tip = 2035u64;

        let storage = in_memory_storage(anchor);
        block_on(storage.init(ticker)).unwrap();

        // Seed the stored chain [anchor..=tip].
        let stored = build_chain(anchor, tip, H256::default(), 0);
        block_on(storage.add_block_headers_to_storage(ticker, stored.clone())).unwrap();

        // A divergent, heavier valid suffix sharing the common ancestor at `fork - 1`.
        let ancestor_hash = stored[&(fork - 1)].hash();
        let candidate = build_chain(fork, new_tip, ancestor_hash, 1000);

        let outcome = block_on(reconcile_reorg(&storage, ticker, &candidate, anchor)).unwrap();

        // (i) the fork height is identified.
        assert_eq!(outcome, ReorgOutcome::ForkAt(fork));
        // (ii) the stale suffix [fork, old tip] is removed and (iii) the store converges on the
        // heavier valid chain.
        assert_eq!(block_on(storage.get_last_block_height(ticker)).unwrap(), Some(new_tip));
        let converged_fork = block_on(storage.get_block_header(ticker, fork)).unwrap().unwrap();
        assert_eq!(converged_fork.hash(), candidate[&fork].hash());
        // The common prefix below the fork is untouched.
        let prefix = block_on(storage.get_block_header(ticker, fork - 1)).unwrap().unwrap();
        assert_eq!(prefix.hash(), stored[&(fork - 1)].hash());
        // Retained count: prefix [anchor..fork-1] + candidate [fork..new_tip].
        let expected_count = (fork - anchor) + (new_tip - fork + 1);
        assert_eq!(
            block_on(storage.get_block_headers_count(ticker)).unwrap(),
            expected_count
        );
    }

    #[test]
    fn reorg_reaching_anchor_reports_bad_starting_header_chain() {
        let ticker = "BADCHAIN";
        let anchor = 2016u64;
        let tip = 2030u64;

        let storage = in_memory_storage(anchor);
        block_on(storage.init(ticker)).unwrap();

        let stored = build_chain(anchor, tip, H256::default(), 0);
        block_on(storage.add_block_headers_to_storage(ticker, stored.clone())).unwrap();

        // A divergence that extends down to the trusted anchor itself.
        let candidate = build_chain(anchor, anchor + 5, H256::default(), 1000);

        let outcome = block_on(reconcile_reorg(&storage, ticker, &candidate, anchor)).unwrap();
        assert_eq!(outcome, ReorgOutcome::BadStartingHeaderChain);
        // The store is left unchanged rather than walking back unbounded.
        assert_eq!(block_on(storage.get_last_block_height(ticker)).unwrap(), Some(tip));
        let anchor_header = block_on(storage.get_block_header(ticker, anchor)).unwrap().unwrap();
        assert_eq!(anchor_header.hash(), stored[&anchor].hash());
    }
}
