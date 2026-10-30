//! CRD ch.32 §32.7.3 R-Q5b: a pair still being synchronized is answered when its synchronization
//! lands.
//!
//! A relay forwards a keep-alive before it handles it, so its subscribers ask it for a pair while its
//! own keep-alive-driven request for that pair is still in flight, and the answer it can give at that
//! moment is the placeholder-root failure of R-Q5a. This module parks such a request instead of
//! answering it at once:
//!
//! * [`SyncAnswerDeferral`] holds two tables behind one lock: the own requests in flight (registered by
//!   the keep-alive path, [`SyncAnswerDeferral::track_own_request`]) and the parked requests;
//! * [`answer_sync_request`] is the entry point of the inbound dispatcher: it parks the request when it
//!   is deferrable and otherwise answers at once, as R-Q5 binds. Parking hands the response channel (as
//!   a [`SyncAnswerReply`]) to a task of its own and returns, so nothing waits on the inbound path
//!   (R-T3b item 3);
//! * [`SyncInFlight`] is the record of one own request. Whatever ends the keep-alive task (the answer was
//!   applied, the request failed, the future was dropped) drops it, and dropping it wakes the parked
//!   requests that waited for it.
//!
//! Lock order (R-T3b): orderbook, then trie store, then the tables of this module, which are the
//! innermost lock and are never held across an await.

use super::*;
use futures::channel::oneshot;
use futures::future::{select, Either};
use mm2_p2p::atomicdex_behaviour::AdexResponse;
use mm2_p2p::PeerId;

/// Default of the MM2.json integer `sync_answer_deferral_ms`.
pub(crate) const DEFAULT_SYNC_ANSWER_DEFERRAL_MS: u64 = 5000;
/// R-Q5b item 4: parked requests per requesting peer.
pub(crate) const MAX_PARKED_PER_PEER: usize = 8;
/// R-Q5b item 4: parked requests in total.
pub(crate) const MAX_PARKED_TOTAL: usize = 256;

/// Number of inbound streams a connection negotiates concurrently. A parked request holds its inbound
/// stream in negotiation until the answer is written, so the per-peer cap must stay below it.
///
/// This is the default of `libp2p-swarm` (`connection/pool.rs`, `max_negotiating_inbound_streams: 128`)
/// in the pinned `rust-libp2p` tag `k-0.52.12`; the swarm of `mm2_p2p` is built with
/// `SwarmBuilder::with_executor(..).build()` and does not override it. The limit is per connection, so
/// it bounds [`MAX_PARKED_PER_PEER`]; [`MAX_PARKED_TOTAL`] is a sum over connections and is not
/// comparable with it.
const SWARM_MAX_NEGOTIATING_INBOUND_STREAMS: usize = 128;
/// Time after which the request-response layer abandons an inbound substream that has not been answered:
/// `RequestResponseConfig::default()` of `libp2p-request-response` 0.25.0 (`request_timeout`, 10 s),
/// which `build_request_response_behaviour` in `mm2_p2p` uses unchanged. It is also the requester's
/// timeout (R-Q6). The default bound must stay well below it.
const REQUEST_RESPONSE_TIMEOUT_MS: u64 = 10_000;

const _: () = assert!(MAX_PARKED_PER_PEER < SWARM_MAX_NEGOTIATING_INBOUND_STREAMS);
const _: () = assert!(DEFAULT_SYNC_ANSWER_DEFERRAL_MS * 2 <= REQUEST_RESPONSE_TIMEOUT_MS);

/// Pause between two checks of the trie worker's progress.
const APPLIED_CHECK_STEP_MS: u64 = 2;

/// The bound of R-Q5b item 3: the top-level MM2.json integer `sync_answer_deferral_ms`, default `5000`;
/// an absent or non-integer value means the default, `0` turns deferral off. Operators who raise it must
/// keep it below the request-response timeout ([`REQUEST_RESPONSE_TIMEOUT_MS`]), or the transport
/// abandons the stream before the answer is written.
pub(crate) fn sync_answer_deferral_ms(ctx: &MmArc) -> u64 {
    ctx.conf["sync_answer_deferral_ms"]
        .as_u64()
        .unwrap_or(DEFAULT_SYNC_ANSWER_DEFERRAL_MS)
}

/// Default of the MM2.json integer `sync_request_retry_ms`.
pub(crate) const DEFAULT_SYNC_REQUEST_RETRY_MS: u64 = 1000;

// R-Q5c item 3: the default pause leaves room within the default bound for the second answer.
const _: () = assert!(DEFAULT_SYNC_REQUEST_RETRY_MS < DEFAULT_SYNC_ANSWER_DEFERRAL_MS);

/// The pause of CRD ch.32 R-Q5c before a failed keep-alive-driven request is asked once more: the top-level
/// MM2.json integer `sync_request_retry_ms`, default `1000`; an absent or non-integer value means the default,
/// `0` turns the second request off. The second request is sent only while it can still be answered within the
/// bound of [`sync_answer_deferral_ms`], so a pause at or above that bound turns it off as well.
pub(crate) fn sync_request_retry_ms(ctx: &MmArc) -> u64 {
    ctx.conf["sync_request_retry_ms"]
        .as_u64()
        .unwrap_or(DEFAULT_SYNC_REQUEST_RETRY_MS)
}

/// Sends the answer of a synchronization request to the peer that asked. The inbound dispatcher builds
/// it around the request's response channel; a parked request keeps it until it is answered.
pub(crate) type SyncAnswerReply = Box<dyn FnOnce(AdexResponse) -> Result<(), String> + Send>;

/// An own request is identified by a token, a parked request by an id; both come from one counter.
type Id = u64;
/// `(pubkey, ordered pair)`.
type PairKey = (String, String);

struct Parked {
    peer: PeerId,
    /// The own requests (tokens) this request waits for.
    waits: HashSet<Id>,
    wake: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
struct Tables {
    next_id: Id,
    /// Own requests in flight, with the pairs each covers.
    in_flight: HashMap<Id, Vec<PairKey>>,
    /// The same, indexed by pair. An entry is never empty.
    in_flight_by_pair: HashMap<PairKey, Vec<Id>>,
    parked: HashMap<Id, Parked>,
}

impl Tables {
    fn next_id(&mut self) -> Id {
        self.next_id += 1;
        self.next_id
    }
}

/// The in-flight and parked tables of one node (a field of [`OrdermatchContext`]). Cloning shares them.
#[derive(Clone, Default)]
pub(crate) struct SyncAnswerDeferral(Arc<PaMutex<Tables>>);

impl SyncAnswerDeferral {
    /// Record that this node's own keep-alive-driven request for `pairs` of `pubkey` is about to be
    /// sent. The caller holds the trie store lock that guards the placeholder entries of `pairs`, so a
    /// request arriving afterwards sees the placeholder and the record together (lock order: trie
    /// store, then these tables). The record lasts until the returned guard is dropped.
    pub(crate) fn track_own_request<'a>(&self, pubkey: &str, pairs: impl Iterator<Item = &'a String>) -> SyncInFlight {
        let keys: Vec<PairKey> = pairs.map(|pair| (pubkey.to_owned(), pair.clone())).collect();
        let mut tables = self.0.lock();
        let token = tables.next_id();
        for key in &keys {
            tables.in_flight_by_pair.entry(key.clone()).or_default().push(token);
        }
        tables.in_flight.insert(token, keys);
        SyncInFlight {
            table: self.clone(),
            token,
        }
    }

    /// The own request `token` has completed: forget it and wake the parked requests that no longer
    /// wait for anything.
    fn complete(&self, token: Id) {
        let mut tables = self.0.lock();
        if let Some(keys) = tables.in_flight.remove(&token) {
            for key in keys {
                if let Some(tokens) = tables.in_flight_by_pair.get_mut(&key) {
                    tokens.retain(|t| *t != token);
                    if tokens.is_empty() {
                        tables.in_flight_by_pair.remove(&key);
                    }
                }
            }
        }
        for parked in tables.parked.values_mut() {
            if parked.waits.remove(&token) && parked.waits.is_empty() {
                if let Some(wake) = parked.wake.take() {
                    // The receiver is gone only when the parked task is; nothing to wake then.
                    let _ = wake.send(());
                }
            }
        }
    }

    /// R-Q5b: park the request when it is deferrable and within the limits. `trie_store` is the store
    /// the caller holds locked while asking. The error says why the request is to be answered at once.
    fn try_park(
        &self,
        peer: PeerId,
        pubkey: &str,
        trie_roots: &HashMap<AlbOrderedOrderbookPair, H64>,
        trie_store: &TrieStore,
    ) -> Result<(Id, oneshot::Receiver<()>), AtOnce> {
        // An unknown pubkey is the "no data" outcome, answered at once.
        let pubkey_state = trie_store.pubkeys_state.get(pubkey).ok_or(AtOnce::NotDeferrable)?;
        let mut tables = self.0.lock();
        let mut waits = HashSet::new();
        let mut without_own_request = false;
        for pair in trie_roots.keys() {
            // A pair without an entry is the missing-pair failure, answered at once.
            let held = pubkey_state.trie_roots.get(pair).ok_or(AtOnce::NotDeferrable)?;
            if *held != H64::default() {
                continue;
            }
            // A placeholder without an own request in flight is answered at once.
            match tables.in_flight_by_pair.get(&(pubkey.to_owned(), pair.clone())) {
                Some(tokens) => waits.extend(tokens.iter().copied()),
                None => without_own_request = true,
            }
        }
        if without_own_request {
            return Err(AtOnce::OwnRequestNotInFlight);
        }
        // Nothing to wait for: the answer is the same now as it would be later.
        if waits.is_empty() {
            return Err(AtOnce::NotDeferrable);
        }
        if tables.parked.len() >= MAX_PARKED_TOTAL
            || tables.parked.values().filter(|p| p.peer == peer).count() >= MAX_PARKED_PER_PEER
        {
            return Err(AtOnce::CapsFull);
        }

        let id = tables.next_id();
        let (wake_tx, wake_rx) = oneshot::channel();
        tables.parked.insert(id, Parked {
            peer,
            waits,
            wake: Some(wake_tx),
        });
        Ok((id, wake_rx))
    }

    #[cfg(test)]
    pub(crate) fn parked_len(&self) -> usize { self.0.lock().parked.len() }

    #[cfg(test)]
    pub(crate) fn in_flight_len(&self) -> usize { self.0.lock().in_flight.len() }
}

/// The record of one own keep-alive-driven request, kept from just before it is sent until its answer
/// has been applied by the trie worker or the request has failed. Dropping it ends the record, so a
/// task that fails, returns early or is cancelled can never leave it (or a request parked behind it)
/// stuck.
pub(crate) struct SyncInFlight {
    table: SyncAnswerDeferral,
    token: Id,
}

impl Drop for SyncInFlight {
    fn drop(&mut self) { self.table.complete(self.token); }
}

impl SyncInFlight {
    /// The answer's trie events have been queued; `target_batches` is [`Orderbook::trie_ops_enqueued`]
    /// right after queueing them. The record ends once the worker has applied that many batches, or
    /// after `max_wait_ms` if it has not. Returns at once: the wait happens on a task of its own, so
    /// the forward of the keep-alive (R-F3b) does not wait for the worker.
    pub(crate) fn complete_after_apply(
        self,
        trie_store: Arc<PaMutex<TrieStore>>,
        target_batches: u64,
        max_wait_ms: u64,
    ) {
        let applied = {
            let trie_store = trie_store.clone();
            move || trie_store.lock().applied_batches >= target_batches
        };
        if applied() {
            return;
        }
        spawn(async move {
            let mut waited = 0;
            while waited < max_wait_ms && !applied() {
                Timer::sleep(APPLIED_CHECK_STEP_MS as f64 / 1000.).await;
                waited += APPLIED_CHECK_STEP_MS;
            }
            drop(self);
        });
    }
}

/// A parked request's slot in the table; dropping it frees the slot.
struct ParkedSlot {
    table: SyncAnswerDeferral,
    id: Id,
}

impl Drop for ParkedSlot {
    fn drop(&mut self) { self.table.0.lock().parked.remove(&self.id); }
}

/// The answer R-Q5 gives to the request at this moment, in the form the transport sends. It is the one place
/// where the answer is decided, so it is also where the answer is logged: `path` says how the answer
/// was reached and `waited_ms` how long a parked request waited.
fn sync_answer_response(
    ctx: &MmArc,
    peer: &PeerId,
    pubkey: String,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
    path: AnswerPath,
    waited_ms: Option<u64>,
) -> AdexResponse {
    let (pairs, log_pubkey) = if debug_enabled() {
        (pairs_label(trie_roots.keys()), pubkey.clone())
    } else {
        (String::new(), String::new())
    };
    let mut entries = None;
    let (class, response) = match process_sync_pubkey_orderbook_state_classified(ctx.clone(), pubkey, trie_roots) {
        Ok(Some(answer)) => {
            let class = answer_class(&answer);
            entries = Some(answer_entries(&answer));
            match encode_message(&answer) {
                Ok(response) => (class, AdexResponse::Ok { response }),
                Err(e) => (AnswerClass::TrieError, AdexResponse::Err { error: e.to_string() }),
            }
        },
        Ok(None) => (AnswerClass::NoData, AdexResponse::None),
        Err(SyncAnswerError { failure, text }) => (failure.into(), AdexResponse::Err { error: text }),
    };
    match OrdermatchContext::from_ctx(ctx) {
        Ok(ordermatch_ctx) => ordermatch_ctx.sync_log.answer(
            &AnswerEvent {
                peer,
                pubkey: &log_pubkey,
                pairs: &pairs,
                class,
                path,
                waited_ms,
                entries,
            },
            now_ms(),
        ),
        Err(e) => log::error!("Cannot log a synchronization answer: {}", e),
    }
    response
}

/// `ok_full` when any pair is answered with a full set, `ok_delta` when all are deltas.
fn answer_class(answer: &SyncPubkeyOrderbookStateRes) -> AnswerClass {
    if answer
        .pair_orders_diff
        .values()
        .any(|diff| matches!(diff, DeltaOrFullTrie::FullTrie(_)))
    {
        AnswerClass::OkFull
    } else {
        AnswerClass::OkDelta
    }
}

/// The orders (or order changes) the answer carries over all its pairs.
fn answer_entries(answer: &SyncPubkeyOrderbookStateRes) -> usize {
    answer
        .pair_orders_diff
        .values()
        .map(|diff| match diff {
            DeltaOrFullTrie::Delta(delta) => delta.len(),
            DeltaOrFullTrie::FullTrie(orders) => orders.len(),
        })
        .sum()
}

/// Park the request when it is deferrable (R-Q5b); give `reply` back, with the reason, when it is to be
/// answered at once.
fn park_sync_request(
    ctx: &MmArc,
    peer: PeerId,
    pubkey: &str,
    trie_roots: &HashMap<AlbOrderedOrderbookPair, H64>,
    reply: SyncAnswerReply,
) -> Result<(), (SyncAnswerReply, AtOnce)> {
    let bound_ms = sync_answer_deferral_ms(ctx);
    if bound_ms == 0 {
        return Err((reply, AtOnce::DeferralOff));
    }
    let ordermatch_ctx = match OrdermatchContext::from_ctx(ctx) {
        Ok(ordermatch_ctx) => ordermatch_ctx,
        Err(e) => {
            log::error!("Cannot consider deferring a synchronization request: {}", e);
            // Logged above; the request is simply not deferred.
            return Err((reply, AtOnce::NotDeferrable));
        },
    };

    let table = ordermatch_ctx.sync_answer_deferral.clone();
    let parked = {
        let trie_store = ordermatch_ctx.trie_store.lock();
        table.try_park(peer, pubkey, trie_roots, &trie_store)
    };
    let (id, wake_rx) = match parked {
        Ok(parked) => parked,
        Err(reason) => return Err((reply, reason)),
    };

    let slot = ParkedSlot { table, id };
    let ctx = ctx.clone();
    let pubkey = pubkey.to_owned();
    let trie_roots = trie_roots.clone();
    let parked_at_ms = now_ms();
    spawn(async move {
        // Woken when every own request it waits for has completed, or at the bound, whichever is first.
        let timeout = Box::pin(Timer::sleep(bound_ms as f64 / 1000.));
        let path = match select(wake_rx, timeout).await {
            Either::Left(_) => AnswerPath::ParkedWoken,
            Either::Right(_) => AnswerPath::ParkedBound,
        };
        // The slot is freed before the answer is sent, so the table is empty by the time it arrives.
        drop(slot);
        let waited_ms = now_ms().saturating_sub(parked_at_ms);
        let res = sync_answer_response(&ctx, &peer, pubkey, trie_roots, path, Some(waited_ms));
        if let Err(e) = reply(res) {
            log::error!("Error on sending the answer of a parked synchronization request: {}", e);
        }
    });
    Ok(())
}

/// The answer to a `SyncPubkeyOrderbookState` request (R-Q5, R-Q5b): the request is parked when it is
/// deferrable and answered at once otherwise. Either way the call returns without waiting (R-T3b item
/// 3); a parked request is answered later from a task of its own through `reply`.
///
/// # Errors
///
/// The error of `reply` when the answer was sent at once and sending failed.
pub(crate) fn answer_sync_request(
    ctx: &MmArc,
    peer: PeerId,
    pubkey: String,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
    reply: SyncAnswerReply,
) -> Result<(), String> {
    match park_sync_request(ctx, peer, &pubkey, &trie_roots, reply) {
        Ok(()) => Ok(()),
        Err((reply, reason)) => reply(sync_answer_response(
            ctx,
            &peer,
            pubkey,
            trie_roots,
            AnswerPath::AtOnce(reason),
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;

    const PUBKEY: &str = "pubkey";
    const PAIR: &str = "C1:C2";

    fn store_with_placeholder() -> TrieStore {
        let mut store = TrieStore::default();
        let state = pubkey_state_mut(&mut store.pubkeys_state, PUBKEY);
        order_pair_root_mut(&mut state.trie_roots, PAIR);
        store
    }

    fn requested() -> HashMap<AlbOrderedOrderbookPair, H64> { HashMap::from([(PAIR.to_owned(), H64::default())]) }

    /// The own request counts as in flight until the worker has applied its batch, not until the answer
    /// has merely arrived and been queued (R-Q5b: "its answer's mutation events have been applied").
    #[test]
    fn the_record_ends_when_the_worker_has_applied_the_batch() {
        let deferral = SyncAnswerDeferral::default();
        let store = Arc::new(PaMutex::new(store_with_placeholder()));
        let in_flight = deferral.track_own_request(PUBKEY, [PAIR.to_owned()].iter());
        let (_, mut wake) = deferral
            .try_park(PeerId::random(), PUBKEY, &requested(), &store.lock())
            .expect("deferrable");

        // The answer is queued as batch 1; the worker has applied none yet.
        in_flight.complete_after_apply(store.clone(), 1, 5000);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(wake.try_recv(), Ok(None), "still waiting for the worker");
        assert_eq!(deferral.in_flight_len(), 1);

        store.lock().applied_batches = 1;
        block_on(wake).expect("woken");
        assert_eq!(deferral.in_flight_len(), 0);
    }

    /// A worker that never catches up does not keep the record forever: it ends at the bound.
    #[test]
    fn the_record_ends_at_the_bound_if_the_worker_does_not_catch_up() {
        let deferral = SyncAnswerDeferral::default();
        let store = Arc::new(PaMutex::new(store_with_placeholder()));
        let in_flight = deferral.track_own_request(PUBKEY, [PAIR.to_owned()].iter());
        let (_, wake) = deferral
            .try_park(PeerId::random(), PUBKEY, &requested(), &store.lock())
            .expect("deferrable");

        in_flight.complete_after_apply(store, 1, 50);
        block_on(wake).expect("woken at the bound");
        assert_eq!(deferral.in_flight_len(), 0);
    }

    /// A request waits for the own requests in flight when it arrives, not for ones started later.
    #[test]
    fn a_request_waits_only_for_the_own_requests_in_flight_when_it_arrives() {
        let deferral = SyncAnswerDeferral::default();
        let store = store_with_placeholder();
        let first = deferral.track_own_request(PUBKEY, [PAIR.to_owned()].iter());
        let (_, mut wake) = deferral
            .try_park(PeerId::random(), PUBKEY, &requested(), &store)
            .expect("deferrable");
        let second = deferral.track_own_request(PUBKEY, [PAIR.to_owned()].iter());

        drop(first);
        assert_eq!(wake.try_recv(), Ok(Some(())));
        assert_eq!(deferral.in_flight_len(), 1);
        drop(second);
        assert_eq!(deferral.in_flight_len(), 0);
    }
}
