//! In-memory SQLite tests for [`SqliteNftStore`].

use crate::eth::EthTxFeeDetails;
use crate::nft::model::{Chain, ContractType, Nft, NftCommon, NftListFilters, NftTransfer, NftTransferCommon,
                        NftTransfersFilters, TransferStatus, UriMeta};
use crate::nft::store::history::NftHistoryStore;
use crate::nft::store::list::NftListStore;
use crate::nft::store::sqlite::SqliteNftStore;
use db_common::async_sql_conn::AsyncConnection;
use ethereum_types::Address;
use mm2_number::{BigDecimal, BigUint};
use std::sync::Arc;

const ADDR_A: &str = "0x00000000000000000000000000000000000000a1";
const ADDR_B: &str = "0x00000000000000000000000000000000000000a2";

async fn fresh_store() -> SqliteNftStore {
    let conn = AsyncConnection::open_in_memory().await.expect("open in-memory db");
    SqliteNftStore::new(Arc::new(conn))
}

fn sample_nft(token_id: u64, block: u64, possible_spam: bool) -> Nft {
    Nft {
        common: NftCommon {
            token_address: Address::from_slice(&hex::decode(&ADDR_A[2..]).unwrap()),
            amount: BigDecimal::from(1u32),
            owner_of: Address::from_slice(&hex::decode(&ADDR_B[2..]).unwrap()),
            token_hash: None,
            collection_name: Some("Cats".into()),
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
        block_number_minted: None,
        block_number: block,
        contract_type: ContractType::Erc721,
        possible_phishing: false,
        uri_meta: UriMeta {
            external_domain: Some("example.com".into()),
            ..UriMeta::default()
        },
    }
}

fn sample_transfer(token_id: u64, log_index: u32, block: u64) -> NftTransfer {
    NftTransfer {
        common: NftTransferCommon {
            block_hash: None,
            transaction_hash: format!("0xhash{}", token_id),
            transaction_index: Some(0),
            log_index,
            value: None,
            transaction_type: None,
            token_address: Address::from_slice(&hex::decode(&ADDR_A[2..]).unwrap()),
            from_address: Address::from_slice(&hex::decode(&ADDR_B[2..]).unwrap()),
            to_address: Address::from_slice(&hex::decode(&ADDR_A[2..]).unwrap()),
            amount: BigDecimal::from(1u32),
            verified: None,
            operator: None,
            possible_spam: false,
        },
        chain: Chain::Eth,
        token_id: BigUint::from(token_id),
        block_number: block,
        block_timestamp: 1_700_000_000 + block,
        contract_type: ContractType::Erc721,
        token_uri: None,
        token_domain: Some("example.com".into()),
        collection_name: None,
        image_url: None,
        image_domain: None,
        token_name: None,
        status: TransferStatus::Receive,
        possible_phishing: false,
        fee_details: None::<EthTxFeeDetails>,
        confirmations: 1,
    }
}

#[tokio::test]
async fn ensure_chain_creates_inventory_table() {
    let store = fresh_store().await;
    assert!(!NftListStore::chain_ready(&store, &Chain::Eth).await.unwrap());
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    assert!(NftListStore::chain_ready(&store, &Chain::Eth).await.unwrap());
}

#[tokio::test]
async fn register_owned_round_trips_through_fetch() {
    let store = fresh_store().await;
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let nft = sample_nft(1, 100, false);
    store.register_owned(Chain::Eth, vec![nft.clone()], 100).await.unwrap();
    let back = store
        .fetch_token(&Chain::Eth, ADDR_A.to_string(), BigUint::from(1u32))
        .await
        .unwrap()
        .expect("token should be present");
    assert_eq!(back, nft);
}

#[tokio::test]
async fn list_owned_paginates_and_skips_spam() {
    let store = fresh_store().await;
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let mut items = Vec::new();
    for i in 0..5u64 {
        items.push(sample_nft(i + 1, 100 + i, i == 4));
    }
    store.register_owned(Chain::Eth, items, 104).await.unwrap();
    let filtered = store
        .list_owned(
            vec![Chain::Eth],
            true,
            10,
            None,
            Some(NftListFilters {
                exclude_spam: true,
                exclude_phishing: false,
            }),
        )
        .await
        .unwrap();
    assert_eq!(filtered.total, 4);
    assert_eq!(filtered.skipped, 1);
    assert_eq!(filtered.nfts.len(), 4);
}

#[tokio::test]
async fn drop_token_reports_outcome_and_advances_bookmark() {
    let store = fresh_store().await;
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    store
        .register_owned(Chain::Eth, vec![sample_nft(1, 100, false)], 100)
        .await
        .unwrap();
    let removed = store
        .drop_token(&Chain::Eth, ADDR_A.to_string(), BigUint::from(1u32), 200)
        .await
        .unwrap();
    assert!(matches!(removed, crate::nft::store::errors::RemoveOutcome::Removed));
    let absent = store
        .drop_token(&Chain::Eth, ADDR_A.to_string(), BigUint::from(1u32), 201)
        .await
        .unwrap();
    assert!(matches!(absent, crate::nft::store::errors::RemoveOutcome::Absent));
    let bookmark = NftListStore::latest_scanned_block(&store, &Chain::Eth).await.unwrap();
    assert_eq!(bookmark, Some(201));
}

#[tokio::test]
async fn mark_contract_spam_updates_column_and_payload() {
    let store = fresh_store().await;
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    store
        .register_owned(
            Chain::Eth,
            vec![sample_nft(1, 100, false), sample_nft(2, 101, false)],
            101,
        )
        .await
        .unwrap();
    NftListStore::mark_contract_spam(&store, &Chain::Eth, ADDR_A.to_string(), true)
        .await
        .unwrap();
    let updated = store
        .fetch_token(&Chain::Eth, ADDR_A.to_string(), BigUint::from(1u32))
        .await
        .unwrap()
        .unwrap();
    assert!(updated.common.possible_spam);
}

#[tokio::test]
async fn list_external_domains_aggregates_unique_values() {
    let store = fresh_store().await;
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    store
        .register_owned(Chain::Eth, vec![sample_nft(1, 100, false)], 100)
        .await
        .unwrap();
    let domains = store.list_external_domains(&Chain::Eth).await.unwrap();
    assert!(domains.contains("example.com"));
}

#[tokio::test]
async fn append_transfers_round_trips_and_paginates() {
    let store = fresh_store().await;
    NftHistoryStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let mut items = Vec::new();
    for i in 0..3u64 {
        items.push(sample_transfer(i + 1, i as u32, 100 + i));
    }
    store.append_transfers(Chain::Eth, items).await.unwrap();
    let list = store
        .list_transfers(vec![Chain::Eth], true, 10, None, None)
        .await
        .unwrap();
    assert_eq!(list.total, 3);
    assert_eq!(list.transfer_history.len(), 3);
    assert_eq!(list.transfer_history[0].block_number, 102);
}

#[tokio::test]
async fn transfer_filters_by_status_and_date() {
    let store = fresh_store().await;
    NftHistoryStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    let mut a = sample_transfer(1, 0, 100);
    a.status = TransferStatus::Receive;
    let mut b = sample_transfer(2, 1, 200);
    b.status = TransferStatus::Send;
    store.append_transfers(Chain::Eth, vec![a, b]).await.unwrap();
    let list = store
        .list_transfers(
            vec![Chain::Eth],
            true,
            10,
            None,
            Some(NftTransfersFilters {
                receive: true,
                send: false,
                from_date: None,
                to_date: None,
                exclude_spam: false,
                exclude_phishing: false,
            }),
        )
        .await
        .unwrap();
    assert_eq!(list.transfer_history.len(), 1);
    assert!(matches!(list.transfer_history[0].status, TransferStatus::Receive));
}

#[tokio::test]
async fn purge_chain_removes_inventory_and_bookmark() {
    let store = fresh_store().await;
    NftListStore::ensure_chain(&store, &Chain::Eth).await.unwrap();
    store
        .register_owned(Chain::Eth, vec![sample_nft(1, 100, false)], 100)
        .await
        .unwrap();
    NftListStore::purge_chain(&store, &Chain::Eth).await.unwrap();
    assert!(!NftListStore::chain_ready(&store, &Chain::Eth).await.unwrap());
    let bookmark = NftListStore::latest_scanned_block(&store, &Chain::Eth).await.unwrap();
    assert_eq!(bookmark, None);
}
