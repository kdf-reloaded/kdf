//! The logging of the order-book synchronization (logging is left to the implementation, CRD ch.32 R36).
//!
//! Two sites feed it:
//!
//! * the responder, where the answer to a `SyncPubkeyOrderbookState` request is decided
//!   (`sync_answer_response`): one [`AnswerClass`] per answer, with the [`AnswerPath`] that led to it;
//! * the requester, where this node's own keep-alive-driven request ends (`ask_for_sync_state`): one
//!   [`RequestClass`] per request, with its attempt (1 is the request, 2 the re-ask of R-Q5c).
//!
//! Every event is one DEBUG line. The events are also counted per node, and once a window of
//! [`SUMMARY_WINDOW_MS`] has passed the counters are reported in one INFO line and reset, but only when a
//! failure was counted. Nothing is logged above INFO. No line carries message contents; peer IDs, pubkeys
//! and pairs are public network data.
//!
//! The time is an argument of every recording function, so the window can be tested without waiting.

use super::*;
use crate::mm2::lp_network::{P2PRequestError, P2PRequestResult};
use common::log::log_crate::{log_enabled, Level};
use mm2_p2p::request_response::LocalFailure;
use mm2_p2p::PeerId;

/// The length of the window of one summary line.
const SUMMARY_WINDOW_MS: u64 = 60_000;
/// The pairs a DEBUG line lists before it turns to `+<k>`.
const MAX_LISTED_PAIRS: usize = 4;
/// The characters of a peer's error text a DEBUG line carries.
const MAX_ERROR_CHARS: usize = 200;

/// What a `SyncPubkeyOrderbookState` request was answered with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnswerClass {
    /// Only deltas.
    OkDelta,
    /// At least one pair as a full set.
    OkFull,
    /// The pubkey is unknown.
    NoData,
    /// A requested pair has no root.
    MissingPair,
    /// A requested pair is held at the all-zero placeholder root.
    Placeholder,
    /// Any other trie or getter failure, or an answer that could not be encoded.
    TrieError,
}

impl AnswerClass {
    fn as_str(self) -> &'static str {
        match self {
            AnswerClass::OkDelta => "ok_delta",
            AnswerClass::OkFull => "ok_full",
            AnswerClass::NoData => "no_data",
            AnswerClass::MissingPair => "missing_pair",
            AnswerClass::Placeholder => "placeholder",
            AnswerClass::TrieError => "trie_error",
        }
    }
}

/// Why a request was answered at once and not parked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtOnce {
    /// No requested pair is a placeholder an own request is in flight for (including an unknown pubkey, a
    /// missing pair and a request nothing waits for).
    NotDeferrable,
    /// A requested pair is a placeholder, but no own request for it is in flight.
    OwnRequestNotInFlight,
    /// The parked-request limits are reached.
    CapsFull,
    /// The bound `sync_answer_deferral_ms` is 0.
    DeferralOff,
}

/// How the answer was reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnswerPath {
    AtOnce(AtOnce),
    /// Parked, and woken by the end of the own requests it waited for.
    ParkedWoken,
    /// Parked, and answered at the bound.
    ParkedBound,
}

impl AnswerPath {
    fn as_str(self) -> &'static str {
        match self {
            AnswerPath::AtOnce(AtOnce::NotDeferrable) => "at_once:not_deferrable",
            AnswerPath::AtOnce(AtOnce::OwnRequestNotInFlight) => "at_once:own_request_not_in_flight",
            AnswerPath::AtOnce(AtOnce::CapsFull) => "at_once:caps_full",
            AnswerPath::AtOnce(AtOnce::DeferralOff) => "at_once:deferral_off",
            AnswerPath::ParkedWoken => "parked:woken",
            AnswerPath::ParkedBound => "parked:bound",
        }
    }
}

/// How this node's own `SyncPubkeyOrderbookState` request ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestClass {
    Ok,
    /// The peer answered "no data".
    NoData,
    /// The peer answered with an error.
    PeerError,
    /// The request timed out, in libp2p or in the local pending-request table.
    Timeout,
    /// Any other failure to send the request or to receive the answer.
    Transport,
    /// The answer arrived but could not be decoded.
    Decode,
}

impl RequestClass {
    fn as_str(self) -> &'static str {
        match self {
            RequestClass::Ok => "ok",
            RequestClass::NoData => "no_data",
            RequestClass::PeerError => "peer_error",
            RequestClass::Timeout => "timeout",
            RequestClass::Transport => "transport",
            RequestClass::Decode => "decode_error",
        }
    }
}

impl From<LocalFailure> for RequestClass {
    fn from(kind: LocalFailure) -> Self {
        match kind {
            LocalFailure::Timeout => RequestClass::Timeout,
            LocalFailure::Transport => RequestClass::Transport,
            LocalFailure::Decode => RequestClass::Decode,
        }
    }
}

/// The class of the outcome of one own request, with the peer's error text when the peer sent one. The
/// class comes from the type of the error, never from its text.
pub(crate) fn classify_sync_request<T>(result: &P2PRequestResult<Option<T>>) -> (RequestClass, Option<&str>) {
    match result {
        Ok(Some(_)) => (RequestClass::Ok, None),
        Ok(None) => (RequestClass::NoData, None),
        Err(e) => match e.get_inner() {
            P2PRequestError::ResponseError(error) => (RequestClass::PeerError, Some(error.as_str())),
            P2PRequestError::LocalResponseError(kind, _) => ((*kind).into(), None),
            P2PRequestError::DecodeError(_) => (RequestClass::Decode, None),
            // Failures to build or to send the request, and a response set of an unexpected size, are local
            // failures of the transport.
            P2PRequestError::EncodeError(_)
            | P2PRequestError::SendError(_)
            | P2PRequestError::ExpectedSingleResponseError(_) => (RequestClass::Transport, None),
        },
    }
}

/// The counters of one window.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SyncCounters {
    pub(crate) answers_ok_delta: u32,
    pub(crate) answers_ok_full: u32,
    pub(crate) answers_no_data: u32,
    pub(crate) answers_missing_pair: u32,
    pub(crate) answers_placeholder: u32,
    /// Placeholder answers by path: `at_once:not_deferrable` and `at_once:deferral_off`.
    pub(crate) placeholder_at_once: u32,
    pub(crate) placeholder_own_not_in_flight: u32,
    pub(crate) placeholder_caps: u32,
    pub(crate) placeholder_parked_woken: u32,
    pub(crate) placeholder_parked_bound: u32,
    pub(crate) answers_trie_error: u32,
    /// Answers of every class that went through the parked path.
    pub(crate) answers_parked: u32,
    pub(crate) requests_ok: u32,
    pub(crate) requests_no_data: u32,
    pub(crate) requests_peer_error: u32,
    pub(crate) requests_timeout: u32,
    pub(crate) requests_transport: u32,
    pub(crate) requests_decode: u32,
}

impl SyncCounters {
    fn count_answer(&mut self, class: AnswerClass, path: AnswerPath) {
        match class {
            AnswerClass::OkDelta => self.answers_ok_delta += 1,
            AnswerClass::OkFull => self.answers_ok_full += 1,
            AnswerClass::NoData => self.answers_no_data += 1,
            AnswerClass::MissingPair => self.answers_missing_pair += 1,
            AnswerClass::Placeholder => {
                self.answers_placeholder += 1;
                match path {
                    AnswerPath::AtOnce(AtOnce::NotDeferrable) | AnswerPath::AtOnce(AtOnce::DeferralOff) => {
                        self.placeholder_at_once += 1
                    },
                    AnswerPath::AtOnce(AtOnce::OwnRequestNotInFlight) => self.placeholder_own_not_in_flight += 1,
                    AnswerPath::AtOnce(AtOnce::CapsFull) => self.placeholder_caps += 1,
                    AnswerPath::ParkedWoken => self.placeholder_parked_woken += 1,
                    AnswerPath::ParkedBound => self.placeholder_parked_bound += 1,
                }
            },
            AnswerClass::TrieError => self.answers_trie_error += 1,
        }
        if matches!(path, AnswerPath::ParkedWoken | AnswerPath::ParkedBound) {
            self.answers_parked += 1;
        }
    }

    fn count_request(&mut self, class: RequestClass) {
        match class {
            RequestClass::Ok => self.requests_ok += 1,
            RequestClass::NoData => self.requests_no_data += 1,
            RequestClass::PeerError => self.requests_peer_error += 1,
            RequestClass::Timeout => self.requests_timeout += 1,
            RequestClass::Transport => self.requests_transport += 1,
            RequestClass::Decode => self.requests_decode += 1,
        }
    }

    fn has_failure(&self) -> bool {
        self.answers_no_data
            + self.answers_missing_pair
            + self.answers_placeholder
            + self.answers_trie_error
            + self.requests_no_data
            + self.requests_peer_error
            + self.requests_timeout
            + self.requests_transport
            + self.requests_decode
            > 0
    }

    /// The INFO line of the window, or `None` when nothing failed in it.
    pub(crate) fn summary_line(&self) -> Option<String> {
        if !self.has_failure() {
            return None;
        }
        Some(format!(
            "sync summary 60s: answers ok={} no_data={} missing_pair={} placeholder={}(at_once={} \
             own_not_in_flight={} caps={} parked_woken={} parked_bound={}) trie_error={} parked={}; requests ok={} \
             no_data={} peer_error={} timeout={} transport={} decode={}",
            self.answers_ok_delta + self.answers_ok_full,
            self.answers_no_data,
            self.answers_missing_pair,
            self.answers_placeholder,
            self.placeholder_at_once,
            self.placeholder_own_not_in_flight,
            self.placeholder_caps,
            self.placeholder_parked_woken,
            self.placeholder_parked_bound,
            self.answers_trie_error,
            self.answers_parked,
            self.requests_ok,
            self.requests_no_data,
            self.requests_peer_error,
            self.requests_timeout,
            self.requests_transport,
            self.requests_decode,
        ))
    }
}

#[derive(Default)]
struct Window {
    /// When the current window opened: at the first recording or flush.
    started_ms: Option<u64>,
    counters: SyncCounters,
    /// The requests recorded so far, with their attempts (tests only).
    #[cfg(test)]
    requests: Vec<(u8, RequestClass)>,
    /// The answers recorded so far (tests only).
    #[cfg(test)]
    answers: Vec<(AnswerClass, AnswerPath)>,
}

/// The counters of one node (a field of [`OrdermatchContext`]).
#[derive(Default)]
pub(crate) struct SyncLog(PaMutex<Window>);

/// One answer to a synchronization request.
pub(crate) struct AnswerEvent<'a> {
    pub(crate) peer: &'a PeerId,
    pub(crate) pubkey: &'a str,
    /// The label of the requested pairs ([`pairs_label`]); empty when DEBUG is off.
    pub(crate) pairs: &'a str,
    pub(crate) class: AnswerClass,
    pub(crate) path: AnswerPath,
    /// How long a parked request waited.
    pub(crate) waited_ms: Option<u64>,
    /// The orders (or order changes) of an `ok` answer, over all its pairs.
    pub(crate) entries: Option<usize>,
}

/// One own synchronization request.
pub(crate) struct RequestEvent<'a> {
    pub(crate) peer: &'a str,
    pub(crate) pubkey: &'a str,
    /// The label of the requested pairs ([`pairs_label`]); empty when DEBUG is off.
    pub(crate) pairs: &'a str,
    /// 1 for the request, 2 for the re-ask.
    pub(crate) attempt: u8,
    pub(crate) class: RequestClass,
    pub(crate) elapsed_ms: u64,
    /// The peer's error text, for [`RequestClass::PeerError`].
    pub(crate) error: Option<&'a str>,
}

/// Whether the per-event lines are written, so that a caller can skip building their labels.
pub(crate) fn debug_enabled() -> bool { log_enabled!(Level::Debug) }

/// `A:B,C:D,...` for at most [`MAX_LISTED_PAIRS`] pairs in order, then `+<k>` for the rest.
pub(crate) fn pairs_label<'a>(pairs: impl Iterator<Item = &'a String>) -> String {
    let mut pairs: Vec<&String> = pairs.collect();
    pairs.sort();
    let mut label = pairs
        .iter()
        .take(MAX_LISTED_PAIRS)
        .map(|pair| pair.as_str())
        .collect::<Vec<_>>()
        .join(",");
    if pairs.len() > MAX_LISTED_PAIRS {
        label.push_str(&format!(",+{}", pairs.len() - MAX_LISTED_PAIRS));
    }
    label
}

fn cut_error(error: &str) -> String { error.chars().take(MAX_ERROR_CHARS).collect() }

impl SyncLog {
    /// Records one answer: its DEBUG line, its count and, when the window has ended, the summary.
    pub(crate) fn answer(&self, event: &AnswerEvent<'_>, now_ms: u64) {
        let waited = event
            .waited_ms
            .map(|ms| format!(" waited_ms={}", ms))
            .unwrap_or_default();
        let entries = event.entries.map(|n| format!(" entries={}", n)).unwrap_or_default();
        log::debug!(
            "sync answer: peer={} pubkey={} pairs={} class={} path={}{}{}",
            event.peer,
            event.pubkey,
            event.pairs,
            event.class.as_str(),
            event.path.as_str(),
            waited,
            entries
        );
        if let Some(summary) = self.record_answer(event.class, event.path, now_ms) {
            log::info!("{}", summary);
        }
    }

    /// Records one own request: its DEBUG line, its count and, when the window has ended, the summary.
    pub(crate) fn request(&self, event: &RequestEvent<'_>, now_ms: u64) {
        let error = event
            .error
            .map(|error| format!(" error={}", cut_error(error)))
            .unwrap_or_default();
        log::debug!(
            "sync request: peer={} pubkey={} pairs={} attempt={} class={} elapsed_ms={}{}",
            event.peer,
            event.pubkey,
            event.pairs,
            event.attempt,
            event.class.as_str(),
            event.elapsed_ms,
            error
        );
        if let Some(summary) = self.record_request(event.attempt, event.class, now_ms) {
            log::info!("{}", summary);
        }
    }

    /// Reports and resets the counters if the window has ended, for a node that sees no event for a while.
    pub(crate) fn flush(&self, now_ms: u64) {
        if let Some(summary) = self.count(now_ms, |_| ()) {
            log::info!("{}", summary);
        }
    }

    fn record_answer(&self, class: AnswerClass, path: AnswerPath, now_ms: u64) -> Option<String> {
        #[cfg(test)]
        self.0.lock().answers.push((class, path));
        self.count(now_ms, |counters| counters.count_answer(class, path))
    }

    #[cfg_attr(not(test), allow(unused_variables))]
    fn record_request(&self, attempt: u8, class: RequestClass, now_ms: u64) -> Option<String> {
        #[cfg(test)]
        self.0.lock().requests.push((attempt, class));
        self.count(now_ms, |counters| counters.count_request(class))
    }

    /// Applies `apply` to the counters. When the window has ended, the counters are reset and the summary of
    /// the window, if anything failed in it, is returned.
    fn count(&self, now_ms: u64, apply: impl FnOnce(&mut SyncCounters)) -> Option<String> {
        let mut window = self.0.lock();
        let started_ms = *window.started_ms.get_or_insert(now_ms);
        apply(&mut window.counters);
        if now_ms.saturating_sub(started_ms) < SUMMARY_WINDOW_MS {
            return None;
        }
        window.started_ms = Some(now_ms);
        std::mem::take(&mut window.counters).summary_line()
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> SyncCounters { self.0.lock().counters.clone() }

    #[cfg(test)]
    pub(crate) fn answers(&self) -> Vec<(AnswerClass, AnswerPath)> { self.0.lock().answers.clone() }

    #[cfg(test)]
    pub(crate) fn requests(&self) -> Vec<(u8, RequestClass)> { self.0.lock().requests.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_are_listed_up_to_four_then_counted() {
        let pairs: Vec<String> = ["E:F", "A:B", "C:D"].iter().map(|p| p.to_string()).collect();
        assert_eq!(pairs_label(pairs.iter()), "A:B,C:D,E:F");

        let pairs: Vec<String> = (0..7).map(|i| format!("A:{}", i)).collect();
        assert_eq!(pairs_label(pairs.iter()), "A:0,A:1,A:2,A:3,+3");

        let four: Vec<String> = (0..4).map(|i| format!("A:{}", i)).collect();
        assert_eq!(pairs_label(four.iter()), "A:0,A:1,A:2,A:3");
        assert_eq!(pairs_label(std::iter::empty()), "");
    }

    #[test]
    fn the_peer_error_is_cut_to_200_characters() {
        let error = "é".repeat(300);
        let cut = cut_error(&error);
        assert_eq!(cut.chars().count(), MAX_ERROR_CHARS);
        assert_eq!(cut_error("short"), "short");
    }

    #[test]
    fn the_counters_are_reset_after_a_window_and_the_next_one_starts_from_zero() {
        let log = SyncLog::default();
        let t0 = 1_000_000;
        // Failures inside a window are only counted.
        assert_eq!(
            log.record_answer(AnswerClass::Placeholder, AnswerPath::AtOnce(AtOnce::CapsFull), t0),
            None
        );
        assert_eq!(log.record_request(1, RequestClass::Timeout, t0 + 30_000), None);
        assert_eq!(log.snapshot().placeholder_caps, 1);
        assert_eq!(log.snapshot().requests_timeout, 1);

        // The event that finds the window ended is counted into it, and then the counters are reset.
        let summary = log
            .record_request(2, RequestClass::Transport, t0 + SUMMARY_WINDOW_MS)
            .expect("a failure was counted");
        assert!(summary.starts_with("sync summary 60s: answers ok=0 no_data=0 missing_pair=0 placeholder=1("));
        assert!(summary.contains("caps=1"), "{}", summary);
        assert!(summary.contains("timeout=1 transport=1 decode=0"), "{}", summary);
        assert_eq!(log.snapshot(), SyncCounters::default());

        // The next window starts from zero: its own failure is the only one reported.
        assert_eq!(log.record_request(1, RequestClass::NoData, t0 + 70_000), None);
        let summary = log
            .record_answer(
                AnswerClass::OkDelta,
                AnswerPath::AtOnce(AtOnce::NotDeferrable),
                t0 + 2 * SUMMARY_WINDOW_MS,
            )
            .expect("a failure was counted");
        assert!(summary.contains("placeholder=0("), "{}", summary);
        assert!(
            summary.contains("requests ok=0 no_data=1 peer_error=0 timeout=0"),
            "{}",
            summary
        );
    }

    #[test]
    fn a_window_with_only_ok_events_emits_no_line() {
        let log = SyncLog::default();
        let t0 = 5_000;
        assert_eq!(
            log.record_answer(AnswerClass::OkFull, AnswerPath::ParkedWoken, t0),
            None
        );
        assert_eq!(log.record_request(1, RequestClass::Ok, t0 + 10), None);
        assert_eq!(
            log.record_answer(
                AnswerClass::OkDelta,
                AnswerPath::AtOnce(AtOnce::NotDeferrable),
                t0 + SUMMARY_WINDOW_MS
            ),
            None
        );
        assert_eq!(log.count(t0 + 3 * SUMMARY_WINDOW_MS, |_| ()), None);
        // The counters were reset all the same.
        assert_eq!(log.snapshot(), SyncCounters::default());
    }

    #[test]
    fn a_flush_reports_a_window_that_got_no_later_event() {
        let log = SyncLog::default();
        let t0 = 9_000;
        assert_eq!(
            log.record_answer(AnswerClass::NoData, AnswerPath::AtOnce(AtOnce::NotDeferrable), t0),
            None
        );
        assert_eq!(log.count(t0 + SUMMARY_WINDOW_MS - 1, |_| ()), None);
        let summary = log.count(t0 + SUMMARY_WINDOW_MS, |_| ()).expect("the window ended");
        assert!(summary.contains("answers ok=0 no_data=1"), "{}", summary);
        assert_eq!(log.count(t0 + 2 * SUMMARY_WINDOW_MS, |_| ()), None);
    }

    #[test]
    fn the_summary_lists_the_counters_in_the_documented_order() {
        let counters = SyncCounters {
            answers_ok_delta: 1,
            answers_ok_full: 2,
            answers_no_data: 3,
            answers_missing_pair: 4,
            answers_placeholder: 15,
            placeholder_at_once: 5,
            placeholder_own_not_in_flight: 6,
            placeholder_caps: 7,
            placeholder_parked_woken: 8,
            placeholder_parked_bound: 9,
            answers_trie_error: 10,
            answers_parked: 11,
            requests_ok: 12,
            requests_no_data: 13,
            requests_peer_error: 14,
            requests_timeout: 15,
            requests_transport: 16,
            requests_decode: 17,
        };
        assert_eq!(
            counters.summary_line().unwrap(),
            "sync summary 60s: answers ok=3 no_data=3 missing_pair=4 placeholder=15(at_once=5 own_not_in_flight=6 \
             caps=7 parked_woken=8 parked_bound=9) trie_error=10 parked=11; requests ok=12 no_data=13 peer_error=14 \
             timeout=15 transport=16 decode=17"
        );
    }

    fn classify(result: P2PRequestResult<Option<u8>>) -> (RequestClass, Option<String>) {
        let (class, error) = classify_sync_request(&result);
        (class, error.map(str::to_owned))
    }

    /// Each class comes from the type of the outcome, never from the text: a peer's error that reads like
    /// a local failure is still the peer's.
    #[test]
    fn requests_are_classified_by_type() {
        assert_eq!(classify(Ok(Some(1))), (RequestClass::Ok, None));
        assert_eq!(classify(Ok(None)), (RequestClass::NoData, None));
        assert_eq!(
            classify(MmError::err(P2PRequestError::ResponseError("Timeout".to_owned()))),
            (RequestClass::PeerError, Some("Timeout".to_owned()))
        );
        for (kind, class) in [
            (LocalFailure::Timeout, RequestClass::Timeout),
            (LocalFailure::Transport, RequestClass::Transport),
            (LocalFailure::Decode, RequestClass::Decode),
        ] {
            assert_eq!(
                classify(MmError::err(P2PRequestError::LocalResponseError(
                    kind,
                    "ResponseError".to_owned()
                ))),
                (class, None)
            );
        }
        assert_eq!(
            classify(MmError::err(P2PRequestError::SendError("full".to_owned()))),
            (RequestClass::Transport, None)
        );
        assert_eq!(
            classify(MmError::err(P2PRequestError::DecodeError("bad".to_owned()))),
            (RequestClass::Decode, None)
        );
    }
}
