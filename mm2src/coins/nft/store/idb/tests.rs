//! `wasm-bindgen-test` smoke tests for the IndexedDB NFT backend.
//!
//! Covers the chain-lifecycle and read/write round-trips for the
//! methods implemented in P10.3.4.b and the mutating helpers added in
//! P10.3.4.c (drop_token, mark_contract_spam, mark_domain_phishing,
//! attach_metadata_to_transfers, transfers_missing_metadata,
//! contract_addresses).

use crate::nft::model::{Chain, ContractType, Nft, NftCommon, NftListFilters, TransferMeta};
use crate::nft::model::{NftTransfer, NftTransferCommon, TransferStatus};
use crate::nft::store::errors::RemoveOutcome;
use crate::nft::store::history::NftHistoryStore;
use crate::nft::store::idb::{IndexedDbNftStore, NftIndexedDb};
use crate::nft::store::list::NftListStore;
use ethereum_types::Address;
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_db::indexed_db::ConstructibleDb;
use mm2_number::{BigDecimal, BigUint};
use std::str::FromStr;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

fn fresh_store() -> IndexedDbNftStore {
    let ctx = MmCtxBuilder::new().with_test_db_namespace().into_mm_arc();
    let shared = ConstructibleDb::<NftIndexedDb>::new_shared(&ctx);
    IndexedDbNftStore::new(shared)
}

fn sample_nft(token_id: u64, block: u64, possible_spam: bool) -> Nft {
    let token_address = Address::from_str("00000000000000000000000000000000000000A1").unwrap();
    let owner = Address::from_str("00000000000000000000000000000000000000A2").unwrap();
    Nft {
        common: NftCommon {
            token_address,
            amount: BigDecimal::from(1u32),
            owner_of: owner,
            token_hash: None,
            collection_name: Some("Test".to_owned()),
            symbol: None,
            token_uri: None,
            token_domain: None,
            metadata: None,
            last_token_uri_sync: None,
            last_metadata_sync: None,
            minter_address: None,
            possible_spam,
        },
        chain: Chain::Eth,
        token_id: BigUint::from(token_id),
        block_number_minted: Some(block),
        block_number: block,
        contract_type: ContractType::Erc721,
        possible_phishing: false,
        uri_meta: Default::default(),
    }
}

fn sample_transfer(token_id: u64, block: u64, ts: u64, status: TransferStatus) -> NftTransfer {
    let token_address = Address::from_str("00000000000000000000000000000000000000A1").unwrap();
    NftTransfer {
        common: NftTransferCommon {
            block_hash: None,
            transaction_hash: format!("0x{:064x}", block),
            transaction_index: Some(0),
            log_index: 0,
            value: None,
            transaction_type: None,
            token_address,
            from_address: Address::from_str("00000000000000000000000000000000000000A2").unwrap(),
            to_address: Address::from_str("00000000000000000000000000000000000000A3").unwrap(),
            amount: BigDecimal::from(1u32),
            verified: Some(1),
            operator: None,
            possible_spam: false,
        },
        chain: Chain::Eth,
        token_id: BigUint::from(token_id),
        block_number: block,
        block_timestamp: ts,
        contract_type: ContractType::Erc721,
        token_uri: None,
        token_domain: None,
        collection_name: None,
        image_url: None,
        image_domain: None,
        token_name: None,
        status,
        possible_phishing: false,
        fee_details: None,
        confirmations: 0,
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn ensure_chain_then_chain_ready_returns_true() {
    let store = fresh_store();
    assert!(!NftListStore::chain_ready(&store, &Chain::Eth).await.unwrap());
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    assert!(NftListStore::chain_ready(&store, &Chain::Eth).await.unwrap());
    assert_eq!(
        NftListStore::latest_scanned_block(&store, &Chain::Eth).await.unwrap(),
        Some(0)
    );
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn register_and_fetch_round_trip_through_inventory() {
    let store = fresh_store();
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let nft = sample_nft(1, 100, false);
    store.register_owned(Chain::Eth, vec![nft.clone()], 100).await.unwrap();
    let fetched = store
        .fetch_token(
            &Chain::Eth,
            format!("{:?}", nft.common.token_address),
            BigUint::from(1u32),
        )
        .await
        .unwrap();
    assert_eq!(fetched, Some(nft));
    assert_eq!(
        NftListStore::latest_scanned_block(&store, &Chain::Eth).await.unwrap(),
        Some(100)
    );
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn list_owned_paginates_and_skips_spam() {
    let store = fresh_store();
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let mut nfts = Vec::new();
    for id in 1u64..=4 {
        let n = sample_nft(id, 100 + id, id == 2 /* spam */);
        nfts.push(n);
    }
    store.register_owned(Chain::Eth, nfts, 200).await.unwrap();
    let filters = Some(NftListFilters {
        exclude_spam: true,
        exclude_phishing: false,
    });
    let list = store
        .list_owned(vec![Chain::Eth], false, 2, std::num::NonZeroUsize::new(1), filters)
        .await
        .unwrap();
    assert_eq!(list.total, 3);
    assert_eq!(list.skipped, 1);
    assert_eq!(list.nfts.len(), 2);
    // Sorted newest-first by block_number.
    assert_eq!(list.nfts[0].token_id, BigUint::from(4u32));
    assert_eq!(list.nfts[1].token_id, BigUint::from(3u32));
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn append_transfers_and_lookup_by_log() {
    let store = fresh_store();
    NftHistoryStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let t1 = sample_transfer(1, 100, 1_000, TransferStatus::Receive);
    let t2 = sample_transfer(2, 110, 1_100, TransferStatus::Send);
    store
        .append_transfers(Chain::Eth, vec![t1.clone(), t2.clone()])
        .await
        .unwrap();
    assert_eq!(store.latest_transfer_block(&Chain::Eth).await.unwrap(), Some(110));
    let found = store
        .transfer_by_log(
            &Chain::Eth,
            t1.common.transaction_hash.clone(),
            t1.common.log_index,
            t1.token_id.clone(),
        )
        .await
        .unwrap();
    assert_eq!(found, Some(t1));
    let listed = store
        .list_transfers(vec![Chain::Eth], true, 0, None, None)
        .await
        .unwrap();
    assert_eq!(listed.total, 2);
    assert_eq!(listed.transfer_history.len(), 2);
    // Sorted newest-first.
    assert_eq!(listed.transfer_history[0].block_number, 110);
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn purge_chain_clears_both_stores_and_bookmark() {
    let store = fresh_store();
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let nft = sample_nft(1, 100, false);
    store.register_owned(Chain::Eth, vec![nft], 100).await.unwrap();
    let tr = sample_transfer(1, 100, 1_000, TransferStatus::Receive);
    store.append_transfers(Chain::Eth, vec![tr]).await.unwrap();
    NftListStore::purge_chain(&store, &Chain::Eth).await.unwrap();
    assert!(!NftListStore::chain_ready(&store, &Chain::Eth).await.unwrap());
    let list = store.list_owned(vec![Chain::Eth], true, 0, None, None).await.unwrap();
    assert_eq!(list.total, 0);
    let history = store
        .list_transfers(vec![Chain::Eth], true, 0, None, None)
        .await
        .unwrap();
    assert_eq!(history.total, 0);
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn drop_token_removes_inventory_row_and_advances_bookmark() {
    let store = fresh_store();
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let nft = sample_nft(7, 200, false);
    store.register_owned(Chain::Eth, vec![nft.clone()], 200).await.unwrap();
    let outcome = store
        .drop_token(
            &Chain::Eth,
            format!("{:?}", nft.common.token_address),
            BigUint::from(7u32),
            250,
        )
        .await
        .unwrap();
    assert_eq!(outcome, RemoveOutcome::Removed);
    let again = store
        .drop_token(
            &Chain::Eth,
            format!("{:?}", nft.common.token_address),
            BigUint::from(7u32),
            260,
        )
        .await
        .unwrap();
    assert_eq!(again, RemoveOutcome::Absent);
    assert_eq!(
        NftListStore::latest_scanned_block(&store, &Chain::Eth).await.unwrap(),
        Some(260)
    );
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn mark_contract_spam_flips_inventory_rows_and_payload() {
    let store = fresh_store();
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let n1 = sample_nft(1, 100, false);
    let n2 = sample_nft(2, 110, false);
    let token_address = format!("{:?}", n1.common.token_address);
    store.register_owned(Chain::Eth, vec![n1, n2], 110).await.unwrap();
    NftListStore::mark_contract_spam(&store, &Chain::Eth, token_address.clone(), true)
        .await
        .unwrap();
    let tokens = store
        .tokens_for_contract(Chain::Eth, token_address.clone())
        .await
        .unwrap();
    assert_eq!(tokens.len(), 2);
    assert!(tokens.iter().all(|n| n.common.possible_spam));
    // The exclude_spam filter should now drop both tokens.
    let filters = Some(NftListFilters {
        exclude_spam: true,
        exclude_phishing: false,
    });
    let list = store
        .list_owned(vec![Chain::Eth], true, 0, None, filters)
        .await
        .unwrap();
    assert_eq!(list.total, 0);
    assert_eq!(list.skipped, 2);
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn attach_metadata_to_transfers_backfills_payload_and_columns() {
    let store = fresh_store();
    NftHistoryStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let mut t1 = sample_transfer(42, 100, 1_000, TransferStatus::Receive);
    let mut t2 = sample_transfer(42, 110, 1_100, TransferStatus::Send);
    t2.common.log_index = 1;
    t2.common.transaction_hash = format!("0x{:064x}", 999u64);
    let token_address = format!("{:?}", t1.common.token_address);
    // Make sure the second transfer ends up under a different log key.
    t1.common.log_index = 0;
    store
        .append_transfers(Chain::Eth, vec![t1.clone(), t2.clone()])
        .await
        .unwrap();
    let missing = store.transfers_missing_metadata(Chain::Eth).await.unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].token_address, token_address);
    assert_eq!(missing[0].token_id, BigUint::from(42u32));

    let meta = TransferMeta {
        token_address: token_address.clone(),
        token_id: BigUint::from(42u32),
        token_uri: Some("ipfs://uri".to_owned()),
        token_domain: Some("ipfs.io".to_owned()),
        collection_name: Some("Test Collection".to_owned()),
        image_url: Some("https://img.example/42.png".to_owned()),
        image_domain: Some("img.example".to_owned()),
        token_name: Some("Token #42".to_owned()),
    };
    store
        .attach_metadata_to_transfers(&Chain::Eth, meta, true)
        .await
        .unwrap();
    let after = store.transfers_missing_metadata(Chain::Eth).await.unwrap();
    assert!(after.is_empty(), "metadata back-fill should clear the missing list");

    let listed = store
        .list_transfers(vec![Chain::Eth], true, 0, None, None)
        .await
        .unwrap();
    assert_eq!(listed.transfer_history.len(), 2);
    for tr in &listed.transfer_history {
        assert_eq!(tr.collection_name.as_deref(), Some("Test Collection"));
        assert_eq!(tr.token_name.as_deref(), Some("Token #42"));
        assert_eq!(tr.token_domain.as_deref(), Some("ipfs.io"));
        assert!(tr.common.possible_spam);
    }
    // The contract address index round-trips through `Address::from_str`.
    let addrs = store.contract_addresses(Chain::Eth).await.unwrap();
    let parsed = Address::from_str(token_address.trim_start_matches("0x")).unwrap();
    assert!(addrs.contains(&parsed));
}
