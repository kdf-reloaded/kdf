//! The history row of a withdrawal names its recipient (CRD ch.39 R39.8.9d1, R39.8.9d2, T39.8.9d1,
//! T39.8.9d2).
//!
//! The harness is the one of the parent module: a real `ZCoin` over a real `zcash_client_sqlite` wallet
//! database and the scripted lightwalletd transport. A withdrawal is built by `withdraw`, submitted
//! through `send_raw_tx` and mined by feeding the scanner a compact block made of the transaction; a
//! "restart" is a new coin over the same database directory. The tests make real Sapling spend proofs
//! (the Pirate1 one also a real Halo2 proof), so they need the Sapling parameter files and say so where
//! those are absent.

use super::*;
use crate::z_coin::z_coin_wallet_db::ZCoinTxHistoryDetails;
use crate::z_coin::z_pirate1_tests::MAIN_ADDRESS;
use crate::z_coin::z_tx_read::read_tx;
use crate::{TransactionDetails, WithdrawError, WithdrawRequest};

const IRONWOOD_HEIGHT: u32 = 5;

fn request(to: &str) -> WithdrawRequest {
    serde_json::from_value(json!({ "coin": "ARRR", "to": to, "amount": "0.01", "max": false })).unwrap()
}

async fn withdraw(fixture: &LightCoinFixture, to: &str) -> Result<TransactionDetails, MmError<WithdrawError>> {
    fixture.coin.withdraw(request(to)).compat().await
}

async fn submit(fixture: &LightCoinFixture, details: &TransactionDetails) {
    fixture
        .coin
        .send_raw_tx(&hex::encode(&details.tx_hex.0))
        .compat()
        .await
        .unwrap();
}

/// A Sapling address of the coin that is not the fixture's own and not its default recipient.
fn other_sapling_address(fixture: &LightCoinFixture, seed: u8) -> String {
    encode_payment_address(
        fixture.coin.z_fields.consensus_params.hrp_sapling_payment_address(),
        &ExtendedSpendingKey::master(&[seed; 32]).default_address().1,
    )
}

fn own_address(fixture: &LightCoinFixture) -> String { fixture.coin.z_fields.my_z_addr_encoded.clone() }

/// The first history page as `z_coin_tx_history` serves it, with the wallet's address in the form
/// `my_balance` reports.
fn page(fixture: &LightCoinFixture) -> ZCoinTxHistoryPage {
    fixture
        .history()
        .load_page(
            "ARRR",
            &own_address(fixture),
            8,
            fixture.tip,
            &common::PagingOptionsEnum::PageNumber(std::num::NonZeroUsize::new(1).unwrap()),
            10,
            true,
        )
        .unwrap()
}

fn row_of<'a>(page: &'a ZCoinTxHistoryPage, tx: &ZTransaction) -> &'a ZCoinTxHistoryDetails {
    let hash = tx.txid().to_string();
    page.transactions
        .iter()
        .find(|row| row.tx_hash == hash)
        .expect("the row of the transaction is served")
}

fn sorted(mut entries: Vec<String>) -> Vec<String> {
    entries.sort();
    entries
}

/// The strings the wallet-database crate keeps as the addresses of its account.
fn crate_addresses(fixture: &LightCoinFixture) -> Vec<String> {
    let conn = db_common::sqlite::rusqlite::Connection::open(fixture.history().wallet_db_path()).unwrap();
    let mut stmt = conn.prepare("SELECT address FROM addresses").unwrap();
    let addresses = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    addresses
}

/// Every entry of `from` and `to` of every row is the wallet's own address or `recipient`; none is a
/// form the wallet-database crate keeps (R39.8.9d1).
fn assert_only_known_addresses(fixture: &LightCoinFixture, page: &ZCoinTxHistoryPage, recipient: &str) {
    let crate_forms = crate_addresses(fixture);
    assert!(!crate_forms.is_empty(), "the account has its unified address");
    let own = own_address(fixture);
    for row in &page.transactions {
        for entry in row.from.iter().chain(row.to.iter()) {
            assert!(*entry == own || entry == recipient, "unexpected {entry}");
            assert!(!crate_forms.contains(entry), "the crate's form {entry}");
        }
    }
}

/// The recipient of a withdrawal in the unmined row, in the mined row and after a restart of each
/// (R39.8.9d2), the change under the wallet's own address, and the deposit row with the own address in
/// one form (R39.8.9d1). `destination` is what the request carries, byte for byte.
async fn recipient_lifecycle(fixture: LightCoinFixture, destination: &str) {
    let own = own_address(&fixture);
    let expected_to = sorted(vec![destination.to_owned(), own.clone()]);

    let details = withdraw(&fixture, destination).await.unwrap();
    assert_eq!(details.to, vec![destination.to_owned()]);
    let tx = read_tx(&details.tx_hex.0).unwrap();

    // Built and not yet submitted: no row, no recipient anywhere.
    let before = page(&fixture);
    assert_eq!(before.total, 1, "the receipt of the funding note only");
    assert!(before
        .transactions
        .iter()
        .all(|row| !row.to.iter().any(|a| a == destination)));

    submit(&fixture, &details).await;

    let check = |fixture: &LightCoinFixture, mined: bool| {
        let page = page(fixture);
        assert_eq!(page.total, 2, "the funding note and the withdrawal");
        let withdrawal = row_of(&page, &tx);
        assert_eq!(withdrawal.to, expected_to, "mined: {mined}");
        assert_eq!(withdrawal.from, vec![own.clone()], "mined: {mined}");
        assert_eq!(withdrawal.block_height != 0, mined);
        // The deposit row names no recipient and lists the own address in one form.
        let deposit = page
            .transactions
            .iter()
            .find(|row| row.tx_hash != withdrawal.tx_hash)
            .unwrap();
        assert!(deposit.from.is_empty());
        assert_eq!(deposit.to, vec![own.clone()]);
        assert_only_known_addresses(fixture, &page, destination);
    };

    // The unmined row, and after a restart.
    check(&fixture, false);
    let mut fixture = fixture.reopen();
    check(&fixture, false);

    // The mined row, and after a restart.
    fixture.scan_block_containing(&tx, 12);
    check(&fixture, true);
    let fixture = fixture.reopen();
    check(&fixture, true);

    // The recipient adds nothing to the amounts of the row.
    let page = page(&fixture);
    let withdrawal = row_of(&page, &tx);
    assert_eq!(withdrawal.spent_by_me, details.spent_by_me);
    assert_eq!(withdrawal.received_by_me, details.received_by_me);
}

/// T39.8.9d2 and T39.8.9d1 for a Sapling recipient.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9d2_a_sapling_withdrawal_names_its_recipient_in_every_row() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let destination = other_sapling_address(&fixture, 97);
    recipient_lifecycle(fixture, &destination).await;
}

/// T39.8.9d2 for a Pirate1 recipient: the wallet's Sapling keys cannot open the output, so the recipient
/// can only have come from the request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9d2_a_pirate1_withdrawal_names_its_recipient_in_every_row() {
    let mut params = test_params_with_zip212();
    params.ironwood_activation_height = Some(IRONWOOD_HEIGHT);
    let Some(fixture) = LightCoinFixture::with_notes_under(1, 2, params, 1_000) else {
        return;
    };
    recipient_lifecycle(fixture, MAIN_ADDRESS).await;
}

/// T39.8.9d2: a withdrawal that is built and never submitted adds no row, and leaves no recipient on the
/// row of a withdrawal that is submitted afterwards. Its entry stays until its transaction expires, then
/// the scan pass removes it; the entry of the mined withdrawal is kept for good.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t39_8_9d2_an_unsubmitted_withdrawal_adds_no_row_and_its_entry_goes_after_expiry() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let abandoned_to = other_sapling_address(&fixture, 96);
    let kept_to = other_sapling_address(&fixture, 95);

    // Built, never submitted.
    let abandoned = withdraw(&fixture, &abandoned_to).await.unwrap();
    let abandoned_tx = read_tx(&abandoned.tx_hex.0).unwrap();
    let abandoned_txid: [u8; 32] = *abandoned_tx.txid().as_ref();
    assert_eq!(
        page(&fixture).total,
        1,
        "no row for a withdrawal that was not submitted"
    );
    let record = fixture.history().tx_addresses_path().to_path_buf();
    assert!(record.exists());

    // Built and submitted afterwards: its row lists its own recipient only.
    let kept = withdraw(&fixture, &kept_to).await.unwrap();
    let kept_tx = read_tx(&kept.tx_hex.0).unwrap();
    submit(&fixture, &kept).await;
    let served = page(&fixture);
    assert_eq!(served.total, 2);
    assert_eq!(
        row_of(&served, &kept_tx).to,
        sorted(vec![kept_to.clone(), own_address(&fixture)])
    );
    assert!(served.transactions.iter().all(|row| !row.to.contains(&abandoned_to)));

    // Mined, then the chain passes the expiry height of both transactions.
    fixture.scan_block_containing(&kept_tx, 12);
    let expiry = u32::from(abandoned_tx.expiry_height()).max(u32::from(kept_tx.expiry_height()));
    let entries = |fixture: &LightCoinFixture| {
        fixture
            .history()
            .recorded_addresses_for_test()
            .into_keys()
            .collect::<BTreeSet<_>>()
    };
    let kept_txid: [u8; 32] = *kept_tx.txid().as_ref();
    assert_eq!(
        entries(&fixture),
        BTreeSet::from([abandoned_txid.to_vec(), kept_txid.to_vec()]),
        "before the expiry height both entries are held"
    );

    fixture.advance_empty_blocks_to(expiry + 1);
    assert_eq!(
        entries(&fixture),
        BTreeSet::from([kept_txid.to_vec()]),
        "the entry of the transaction that expired unmined is gone; the mined one stays"
    );
    let served = page(&fixture);
    assert_eq!(
        row_of(&served, &kept_tx).to,
        sorted(vec![kept_to, own_address(&fixture)])
    );
}
