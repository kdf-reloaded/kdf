// siacoin_history — Sia transaction history (CRD ch.53).
//
// Two independent pieces live here:
//
//   * `tx_details_from_event` — the *pure* walletd-event to
//     transaction-details projection of §53.5. It reads nothing but the event
//     and the wallet address (R53.5.12), so it is unit-testable from
//     constructed events without a walletd instance.
//   * `process_history_loop` — the background tracking pass of §53.6, which
//     populates the coin-generic runtime history store.
//
// Sia is deliberately *not* classified through `HistoryCoinType` and creates
// no per-coin SQL history tables (R53.3.1/R53.3.3); the runtime store keyed by
// (ticker, wallet address) is the whole persistence story.

use super::*;

/// Number of events requested per walletd address-events call.
///
/// Paging is only a transport concern: a pass keeps requesting until walletd
/// returns a short page, so the *complete* event set is always retrieved
/// before the pass completes (R53.4.3).
const EVENTS_PAGE_LEN: i64 = 100;

/// Steady-state interval between tracking passes, in seconds (R53.6.6).
const HISTORY_POLL_INTERVAL: f64 = 30.;

/// Interval between retries after a walletd failure, in seconds (R53.6.6).
const HISTORY_RETRY_INTERVAL: f64 = 10.;

/// How many consecutive times the loop tolerates the coin being absent from
/// the registry before giving up (R53.6.7).
const MISSING_COIN_ATTEMPTS: u32 = 5;

/// Sum `u128` hastings, refusing to wrap.
///
/// # Errors
///
/// Fails when the running total would exceed `u128::MAX`, which no honest
/// event set can reach but which must not wrap silently.
fn sum_hastings<'a>(
    values: impl IntoIterator<Item = &'a Currency>,
    event_id: &Hash256,
    field: &'static str,
) -> Result<u128, SiaHistoryMapError> {
    values
        .into_iter()
        .try_fold(0u128, |acc, value| acc.checked_add(value.0))
        .ok_or_else(|| SiaHistoryMapError::AmountOverflow {
            event_id: event_id.to_string(),
            field,
        })
}

/// Deduplicate and sort address strings so re-mapping the same event yields a
/// byte-identical record (R53.5.12).
fn address_list(addresses: impl IntoIterator<Item = Address>) -> Vec<String> {
    let mut list: Vec<String> = addresses
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|address| address.to_string())
        .collect();
    list.sort();
    list
}

/// Project a walletd event onto the shared transaction-details record (§53.5).
///
/// Returns `Ok(None)` for the three event kinds history does not represent —
/// siafund-claim payouts and v1/v2 contract resolutions — which are skipped
/// silently and are not an error (R53.5.1, D53.3).
///
/// # Errors
///
/// Fails only when summing an event's output values would overflow `u128`
/// (R53.5.13); such an event is not stored as a partial record.
pub(crate) fn tx_details_from_event(
    ticker: &str,
    my_address: &Address,
    event: &Event,
) -> Result<Option<TransactionDetails>, SiaHistoryMapError> {
    // `consumed`/`created` are (address, value) pairs of the outputs the event
    // spends and creates. A payout consumes nothing (R53.5.7).
    let (transaction_type, consumed, created): (TransactionType, Vec<(Address, Currency)>, Vec<(Address, Currency)>) =
        match &event.data {
            EventDataWrapper::V1Transaction(v1) => (
                TransactionType::SiaV1Transaction,
                v1.spent_siacoin_elements
                    .iter()
                    .map(|element| (element.siacoin_output.address.clone(), element.siacoin_output.value))
                    .collect(),
                v1.transaction
                    .siacoin_outputs
                    .iter()
                    .map(|output| (output.address.clone(), output.value))
                    .collect(),
            ),
            EventDataWrapper::V2Transaction(v2) => (
                TransactionType::SiaV2Transaction,
                v2.siacoin_inputs
                    .iter()
                    .map(|input| {
                        (
                            input.parent.siacoin_output.address.clone(),
                            input.parent.siacoin_output.value,
                        )
                    })
                    .collect(),
                v2.siacoin_outputs
                    .iter()
                    .map(|output| (output.address.clone(), output.value))
                    .collect(),
            ),
            // Miner and foundation payouts share one wire value (R53.5.10).
            EventDataWrapper::MinerPayout(payout) | EventDataWrapper::FoundationPayout(payout) => {
                (TransactionType::SiaMinerPayout, Vec::new(), vec![(
                    payout.siacoin_element.siacoin_output.address.clone(),
                    payout.siacoin_element.siacoin_output.value,
                )])
            },
            // Not represented in history (R53.5.1, D53.3).
            EventDataWrapper::ClaimPayout(_)
            | EventDataWrapper::V2FileContractResolution(_)
            | EventDataWrapper::EventV1ContractResolution(_) => return Ok(None),
        };

    let is_payout = matches!(transaction_type, TransactionType::SiaMinerPayout);

    let consumed_total = sum_hastings(consumed.iter().map(|(_, value)| value), &event.id, "consumed")?;
    let created_total = sum_hastings(created.iter().map(|(_, value)| value), &event.id, "created")?;
    let spent_by_me = sum_hastings(
        consumed
            .iter()
            .filter(|(address, _)| address == my_address)
            .map(|(_, value)| value),
        &event.id,
        "spent_by_me",
    )?;
    let received_by_me = sum_hastings(
        created
            .iter()
            .filter(|(address, _)| address == my_address)
            .map(|(_, value)| value),
        &event.id,
        "received_by_me",
    )?;

    // A payout reports the value it creates; a transaction reports the value it
    // consumes (R53.5.4 vs R53.5.7).
    let total_amount = if is_payout { created_total } else { consumed_total };

    // A payout pays no fee. A v1 transaction may create more value than it
    // consumes (consensus-funded creation), so the subtraction saturates at
    // zero rather than underflowing (R53.5.5).
    let fee_details = if is_payout {
        None
    } else {
        Some(TxFeeDetails::Sia(SiaFeeDetails {
            coin: ticker.to_owned(),
            // walletd does not record which policy produced the fee (R53.5.6).
            policy: SiaFeePolicy::Unknown,
            total_amount: hastings_to_siacoin(Currency(consumed_total.saturating_sub(created_total))),
        }))
    };

    let received_by_me = hastings_to_siacoin(Currency(received_by_me));
    let spent_by_me = hastings_to_siacoin(Currency(spent_by_me));

    Ok(Some(TransactionDetails {
        // A walletd event is not a rebroadcastable serialised transaction
        // (R53.5.11, D53.4), so neither carrier is populated here: the JSON
        // carrier ch.20 R-W7 binds is the withdraw path's, not the history
        // path's.
        tx_hex: BytesJson(Vec::new()),
        tx_json: None,
        tx_hash: event.id.to_string(),
        from: address_list(consumed.into_iter().map(|(address, _)| address)),
        to: address_list(created.into_iter().map(|(address, _)| address)),
        total_amount: hastings_to_siacoin(Currency(total_amount)),
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        block_height: event.index.height,
        // Whole Unix seconds; a pre-epoch timestamp cannot be represented and
        // clamps to zero rather than wrapping.
        timestamp: event.timestamp.timestamp().max(0) as u64,
        fee_details,
        coin: ticker.to_owned(),
        internal_id: BytesJson(event.id.0.to_vec()),
        kmd_rewards: None,
        transaction_type,
    }))
}

impl SiaCoin {
    /// Retrieve an address's **complete** event set from walletd (R53.4.3),
    /// paging until a short page arrives.
    ///
    /// `pub(super)`: the swap-spend event-walk (D3, `siacoin_swap_ops.rs`)
    /// reuses this same paging rather than adding a second implementation.
    pub(super) async fn fetch_all_events(&self, address: &Address) -> Result<Vec<Event>, SiaHistoryFetchError> {
        let mut all = Vec::new();
        let mut offset = 0i64;
        loop {
            let request = AddressesEventsRequest {
                address: address.clone(),
                limit: Some(EVENTS_PAGE_LEN),
                offset: Some(offset),
            };
            let page = self
                .client
                .dispatcher(request)
                .await
                .map_err(|e| SiaHistoryFetchError::Transport(e.to_string()))?;
            let page_len = page.len() as i64;
            all.extend(page);
            if page_len < EVENTS_PAGE_LEN {
                return Ok(all);
            }
            offset += page_len;
        }
    }

    fn set_history_sync_state(&self, state: HistorySyncState) {
        match self.history_sync_state.lock() {
            Ok(mut guard) => *guard = state,
            // A poisoned lock means another thread panicked while holding it;
            // the status cell is diagnostic only, so recover rather than
            // propagating the panic into the history loop.
            Err(poisoned) => *poisoned.into_inner() = state,
        }
    }
}

/// The background tracking pass of §53.6.
///
/// Populates the coin-generic runtime history store for an activated Sia coin.
/// Never blocks activation or a history request (R53.6.2), and never returns an
/// error to a caller: failures surface through the sync status (R53.6.3).
pub async fn process_history_loop(coin: SiaCoin, ctx: MmArc) {
    let ticker = coin.ticker().to_owned();
    // Cheap, permanent confirmation that this task actually got scheduled
    // and its first HTTP call has an execution context to run in --
    // sync_status alone can't distinguish "loop never started" from "loop
    // started and is legitimately waiting" (see lp_spawn_tx_history's own
    // comment for why the former was possible here until now).
    info!("Sia history loop starting for {}", ticker);

    // Defensive: tracking is only spawned when `tx_history` is set, but a coin
    // built without it must never touch the store (R53.6.1).
    if matches!(coin.history_sync_status(), HistorySyncState::NotEnabled) {
        return;
    }

    let my_address = match coin.my_keypair() {
        Ok(keypair) => keypair.public().address(),
        Err(e) => {
            // A policy that yields no single address cannot be tracked
            // (R53.4.2); report the error state rather than panicking.
            coin.set_history_sync_state(HistorySyncState::Error(
                json!({ "code": 0, "message": format!("Sia history needs a single address: {}", e) }),
            ));
            return;
        },
    };

    let mut history_map: HashMap<BytesJson, TransactionDetails> = match coin.load_history_from_file(&ctx).compat().await
    {
        Ok(history) => history.into_iter().map(|tx| (tx.internal_id.clone(), tx)).collect(),
        Err(e) => {
            // Nothing has been written yet, so the stored set is still whatever
            // was last persisted (R53.3.5).
            coin.set_history_sync_state(HistorySyncState::Error(
                json!({ "code": 0, "message": format!("Error loading Sia history: {}", e) }),
            ));
            return;
        },
    };

    let mut last_balance: Option<CoinBalance> = None;
    let mut missing_coin_attempts = 0u32;

    loop {
        if ctx.is_stopping() {
            break;
        }

        // Stop cleanly once the coin is no longer registered, tolerating a
        // brief absence (R53.6.7).
        {
            let coins_ctx = match CoinsContext::from_ctx(&ctx) {
                Ok(coins_ctx) => coins_ctx,
                Err(e) => {
                    debug!("Sia history loop for {}: no coins context ({}), stopping", ticker, e);
                    break;
                },
            };
            let registered = coins_ctx.coins.lock().await.contains_key(&ticker);
            if registered {
                missing_coin_attempts = 0;
            } else {
                missing_coin_attempts += 1;
                if missing_coin_attempts >= MISSING_COIN_ATTEMPTS {
                    info!("Sia history loop for {} stopped: coin is no longer active", ticker);
                    break;
                }
                Timer::sleep(HISTORY_RETRY_INTERVAL).await;
                continue;
            }
        }

        // Skip a pass while nothing can have changed (R53.6.6).
        let balance = coin.my_balance().compat().await.ok();
        if let (Some(previous), Some(current)) = (&last_balance, &balance) {
            if previous == current {
                Timer::sleep(HISTORY_POLL_INTERVAL).await;
                continue;
            }
        }

        let events = match coin.fetch_all_events(&my_address).await {
            Ok(events) => events,
            Err(e) => {
                // Leave the stored set intact and retry (R53.4.7).
                debug!("Sia history loop for {}: {}, retrying", ticker, e);
                coin.set_history_sync_state(HistorySyncState::Error(json!({ "code": 0, "message": e.to_string() })));
                Timer::sleep(HISTORY_RETRY_INTERVAL).await;
                continue;
            },
        };

        let mut fetched_ids = HashSet::with_capacity(events.len());
        let mut new_records = Vec::new();
        let mut mapping_error = None;
        for event in &events {
            match tx_details_from_event(&ticker, &my_address, event) {
                // Skipped kinds are neither stored nor counted (R53.5.1).
                Ok(None) => continue,
                Ok(Some(details)) => {
                    fetched_ids.insert(details.internal_id.clone());
                    if !history_map.contains_key(&details.internal_id) {
                        new_records.push(details);
                    }
                },
                Err(e) => {
                    mapping_error = Some(e.to_string());
                    break;
                },
            }
        }

        if let Some(message) = mapping_error {
            // A represented event that cannot be mapped is not stored as a
            // partial record (R53.5.13).
            coin.set_history_sync_state(HistorySyncState::Error(json!({ "code": 0, "message": message })));
            Timer::sleep(HISTORY_RETRY_INTERVAL).await;
            continue;
        }

        let mut transactions_left = new_records.len();
        coin.set_history_sync_state(HistorySyncState::InProgress(
            json!({ "transactions_left": transactions_left }),
        ));

        // Insert-only: an identifier already present is left untouched
        // (R53.6.4).
        for details in new_records {
            history_map.insert(details.internal_id.clone(), details);
            transactions_left = transactions_left.saturating_sub(1);
            coin.set_history_sync_state(HistorySyncState::InProgress(
                json!({ "transactions_left": transactions_left }),
            ));
        }

        // Prune records the fetched set no longer carries (R53.6.5).
        history_map.retain(|internal_id, _| fetched_ids.contains(internal_id));

        // The persisted set is replaced wholesale so it never lags the
        // in-memory set (R53.3.4).
        let mut to_save: Vec<TransactionDetails> = history_map.values().cloned().collect();
        to_save.sort_by(|a, b| {
            b.block_height
                .cmp(&a.block_height)
                .then_with(|| a.internal_id.0.cmp(&b.internal_id.0))
        });

        if let Err(e) = coin.save_history_to_file(&ctx, to_save).compat().await {
            // Persistence failure stops tracking and leaves the last
            // successfully persisted set readable (R53.3.5).
            coin.set_history_sync_state(HistorySyncState::Error(
                json!({ "code": 0, "message": format!("Error saving Sia history: {}", e) }),
            ));
            return;
        }

        coin.set_history_sync_state(HistorySyncState::Finished);
        last_balance = balance;
        Timer::sleep(HISTORY_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    //! Regression surface for the pure event mapping of CRD ch.53 §53.5.
    //!
    //! Every case constructs a walletd event from the JSON shape walletd
    //! actually returns, so the tests cover the wire shape as well as the
    //! projection, and none of them needs a walletd instance (R53.5.12).

    use super::*;
    use std::str::FromStr;

    const TICKER: &str = "SC";
    /// Address funding the v2 fixture below, and the change recipient.
    const ADDR_A: &str = "c34caa97740668de2bbdb7174572ed64c861342bf27e80313cbfa02e9251f52e30aad3892533";
    /// Payment recipient in the v2 fixture below.
    const ADDR_B: &str = "71731d7efe821794742c72a8376f56355b3c8a1984b861ccd42eed77a779a26626ea26ced3e2";
    const EVENT_ID: &str = "0f088eddda5320f8453a55349063abe43ba5b282631d5d2b9e684548f083055a";
    const BLOCK_ID: &str = "b37a5387883748f73c1475ca85c8f3200eef09126c44824d0f44574109dabedc";

    fn address(s: &str) -> Address { Address::from_str(s).expect("valid Sia address") }

    fn sc(s: &str) -> BigDecimal { BigDecimal::from_str(s).expect("valid decimal") }

    /// Wrap an event payload in the walletd event envelope.
    fn event(event_type: &str, data: Json) -> Event {
        let json = json!({
            "id": EVENT_ID,
            "index": { "height": 42, "id": BLOCK_ID },
            "confirmations": 7,
            "timestamp": "2024-05-01T12:00:00Z",
            "maturityHeight": 0,
            "type": event_type,
            "data": data,
        });
        serde_json::from_value(json).expect("valid walletd event")
    }

    /// A siacoin element (UTXO) as walletd serialises it.
    fn element(value: &str, addr: &str) -> Json {
        json!({
            "id": EVENT_ID,
            "stateElement": { "leafIndex": 3, "merkleProof": [] },
            "siacoinOutput": { "value": value, "address": addr },
            "maturityHeight": 0,
        })
    }

    /// Spends 299999 SC from A, pays 0.05 SC to B, returns 299998.94999 SC to
    /// A, leaving a 0.00001 SC miner fee.
    fn v2_transaction_event() -> Event {
        event(
            "v2Transaction",
            json!({
                "siacoinInputs": [{
                    "parent": element("299999000000000000000000000000", ADDR_A),
                    "satisfiedPolicy": {
                        "policy": { "type": "pk", "policy": "ed25519:a729be53dae7b0ed812f2a123ce93556014bbad8516ba6b1b496a112b46bbd97" },
                        "signatures": ["160e79ac52e0eaab5e92bd1675604a94b56ec58fdd0be3f3a842a4ece07d794f7ee1e8cc8f29b596bf71b2dc594df53347b9a4bcbec46fe09244ce6d3f6a6708"]
                    }
                }],
                "siacoinOutputs": [
                    { "value": "50000000000000000000000", "address": ADDR_B },
                    { "value": "299998949990000000000000000000", "address": ADDR_A }
                ],
                "minerFee": "10000000000000000000"
            }),
        )
    }

    #[test]
    fn v2_transaction_maps_amounts_addresses_and_fee() {
        let details = tx_details_from_event(TICKER, &address(ADDR_A), &v2_transaction_event())
            .expect("mapping succeeds")
            .expect("v2 transactions are represented");

        assert_eq!(details.transaction_type, TransactionType::SiaV2Transaction);
        assert_eq!(details.coin, TICKER);
        // total_amount is the value consumed (R53.5.4).
        assert_eq!(details.total_amount, sc("299999"));
        assert_eq!(details.spent_by_me, sc("299999"));
        assert_eq!(details.received_by_me, sc("299998.94999"));
        assert_eq!(details.my_balance_change, sc("-0.05001"));
        assert_eq!(details.from, vec![ADDR_A.to_owned()]);
        // Deduplicated and sorted; A appears twice among the outputs.
        let mut expected_to = vec![ADDR_A.to_owned(), ADDR_B.to_owned()];
        expected_to.sort();
        assert_eq!(details.to, expected_to);

        match details.fee_details {
            Some(TxFeeDetails::Sia(fee)) => {
                assert_eq!(fee.coin, TICKER);
                assert_eq!(fee.policy, SiaFeePolicy::Unknown);
                assert_eq!(fee.total_amount, sc("0.00001"));
            },
            other => panic!("expected Sia fee details, got {:?}", other),
        }
    }

    #[test]
    fn record_identity_and_block_fields_come_from_the_event() {
        let details = tx_details_from_event(TICKER, &address(ADDR_A), &v2_transaction_event())
            .expect("mapping succeeds")
            .expect("v2 transactions are represented");

        // internal_id is the raw identifier bytes, tx_hash its hex form
        // (R53.5.2).
        assert_eq!(details.tx_hash, EVENT_ID);
        assert_eq!(details.internal_id, BytesJson(hex::decode(EVENT_ID).unwrap()));
        assert_eq!(details.block_height, 42);
        // 2024-05-01T12:00:00Z
        assert_eq!(details.timestamp, 1_714_564_800);
        // The event is not a rebroadcastable transaction (R53.5.11).
        assert_eq!(details.tx_hex, BytesJson(Vec::new()));
        assert!(details.kmd_rewards.is_none());
        // The walletd-reported confirmation count is not stored (R53.5.8).
        assert_eq!(details.block_height, 42);
    }

    #[test]
    fn v1_transaction_creating_more_than_it_consumes_reports_zero_fee() {
        // Consensus-funded creation: 1 SC in, 2 SC out. The fee saturates at
        // zero instead of underflowing (R53.5.5).
        let event = event(
            "v1Transaction",
            json!({
                "transaction": {
                    "siacoinOutputs": [{ "value": "2000000000000000000000000", "address": ADDR_A }]
                },
                "spentSiacoinElements": [element("1000000000000000000000000", ADDR_B)],
            }),
        );

        let details = tx_details_from_event(TICKER, &address(ADDR_A), &event)
            .expect("mapping succeeds")
            .expect("v1 transactions are represented");

        assert_eq!(details.transaction_type, TransactionType::SiaV1Transaction);
        assert_eq!(details.total_amount, sc("1"));
        assert_eq!(details.received_by_me, sc("2"));
        assert_eq!(details.spent_by_me, sc("0"));
        match details.fee_details {
            Some(TxFeeDetails::Sia(fee)) => assert_eq!(fee.total_amount, sc("0")),
            other => panic!("expected Sia fee details, got {:?}", other),
        }
    }

    #[test]
    fn miner_payout_to_my_address_is_credited_and_pays_no_fee() {
        let event = event(
            "miner",
            json!({ "siacoinElement": element("3000000000000000000000000", ADDR_A) }),
        );

        let details = tx_details_from_event(TICKER, &address(ADDR_A), &event)
            .expect("mapping succeeds")
            .expect("miner payouts are represented");

        assert_eq!(details.transaction_type, TransactionType::SiaMinerPayout);
        // Payouts are created by consensus, not spent from an address
        // (R53.5.7).
        assert!(details.from.is_empty());
        assert_eq!(details.to, vec![ADDR_A.to_owned()]);
        assert_eq!(details.total_amount, sc("3"));
        assert_eq!(details.spent_by_me, sc("0"));
        assert_eq!(details.received_by_me, sc("3"));
        assert_eq!(details.my_balance_change, sc("3"));
        assert!(details.fee_details.is_none());
    }

    #[test]
    fn foundation_payout_shares_the_miner_payout_wire_value() {
        let event = event(
            "foundation",
            json!({ "siacoinElement": element("1000000000000000000000000", ADDR_A) }),
        );

        let details = tx_details_from_event(TICKER, &address(ADDR_A), &event)
            .expect("mapping succeeds")
            .expect("foundation payouts are represented");

        // Both consensus payout kinds carry one wire value (R53.5.10).
        assert_eq!(details.transaction_type, TransactionType::SiaMinerPayout);
        assert!(details.fee_details.is_none());
    }

    #[test]
    fn payout_to_another_address_credits_nothing() {
        let event = event(
            "miner",
            json!({ "siacoinElement": element("3000000000000000000000000", ADDR_B) }),
        );

        let details = tx_details_from_event(TICKER, &address(ADDR_A), &event)
            .expect("mapping succeeds")
            .expect("miner payouts are represented");

        assert_eq!(details.total_amount, sc("3"));
        assert_eq!(details.received_by_me, sc("0"));
        assert_eq!(details.my_balance_change, sc("0"));
    }

    #[test]
    fn siafund_claim_is_skipped_without_error() {
        // Siafund claims are not represented in history: skipping is not an
        // error and the event is not counted (R53.5.1, D53.3). The two
        // contract-resolution kinds are unrepresented for the same reason and
        // share this match arm; they are not fixtured here because a valid
        // resolution payload requires a whole file-contract tree, which would
        // test serde rather than the projection.
        let claim = event(
            "siafundClaim",
            json!({ "siacoinElement": element("1000000000000000000000000", ADDR_A) }),
        );
        assert!(tx_details_from_event(TICKER, &address(ADDR_A), &claim)
            .expect("skipping is not an error")
            .is_none());
    }

    #[test]
    fn mapping_the_same_event_twice_yields_identical_records() {
        // Mapping is pure, so a re-run cannot perturb a stored record
        // (R53.5.12) — which is what makes the insert-only pass of R53.6.4
        // safe.
        let event = v2_transaction_event();
        let my_address = address(ADDR_A);
        let first = tx_details_from_event(TICKER, &my_address, &event).unwrap();
        let second = tx_details_from_event(TICKER, &my_address, &event).unwrap();
        assert_eq!(first, second);
    }
}
