//! The logging of the order-book synchronization (logging is left to the implementation, CRD ch.32 R36).
//!
//! Two sites feed it:
//!
//! * the responder, where the answer to a `SyncPubkeyOrderbookState` request is decided
//!   (`sync_answer_response`): one [`AnswerClass`] per answer, with the [`AnswerPath`] that led to it;
//! * the requester, where this node's own keep-alive-driven request ends (`ask_for_sync_state`): one
//!   [`RequestClass`] per request, with its [`Attempt`] (1 is the request, 2 the re-ask of R-Q5c, `alt` the
//!   request to one other relay of R-Q5d, with the reason that led to it). The steps of R-Q5d around that
//!   request are counted as [`AltEvent`]s.
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
use std::fmt;

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
    /// Parked before the placeholder existed (CRD ch.32 R-Q5b item 6), and answered before or at the end of
    /// the wait.
    PreParkedWoken,
    /// Parked before the placeholder existed (R-Q5b item 6), and answered at the bound.
    PreParkedBound,
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
            AnswerPath::PreParkedWoken => "parked_pre:woken",
            AnswerPath::PreParkedBound => "parked_pre:bound",
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

/// Why the request to one other relay (CRD ch.32 R-Q5d) was made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AltReason {
    /// Trigger (a): the request to the forwarding peer ended without an answer.
    Failure,
    /// Trigger (b): the answer of the forwarding peer left a pair at a root other than the advertised one.
    Mismatch,
}

impl fmt::Display for AltReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AltReason::Failure => "failure",
            AltReason::Mismatch => "mismatch",
        })
    }
}

/// Which request of one keep-alive a [`RequestEvent`] is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Attempt {
    /// The request to the forwarding peer.
    First,
    /// The re-ask of R-Q5c.
    Second,
    /// The request to one other relay (R-Q5d).
    Alternate(AltReason),
}

impl fmt::Display for Attempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Attempt::First => "1",
            Attempt::Second => "2",
            Attempt::Alternate(_) => "alt",
        })
    }
}

/// A step of the request to one other relay (CRD ch.32 R-Q5d) that is counted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AltEvent {
    /// Trigger (a) fired.
    Failure,
    /// Trigger (b) fired.
    Mismatch,
    /// No other relay was connected.
    NoRelay,
    /// The trie worker had not applied the answer within the wait, so no request followed.
    WorkerLate,
    /// The request was sent.
    Sent,
    /// The request was answered and the answer applied.
    Ok,
}

impl AltEvent {
    fn as_str(self) -> &'static str {
        match self {
            AltEvent::Failure => "failure",
            AltEvent::Mismatch => "mismatch",
            AltEvent::NoRelay => "no_relay",
            AltEvent::WorkerLate => "worker_late",
            AltEvent::Sent => "sent",
            AltEvent::Ok => "ok",
        }
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
    /// Placeholder answers by the parked paths, of R-Q5b item 1 and of item 6 together.
    pub(crate) placeholder_parked_woken: u32,
    pub(crate) placeholder_parked_bound: u32,
    pub(crate) answers_trie_error: u32,
    /// Answers of every class that went through a parked path, of item 1 and of item 6.
    pub(crate) answers_parked: u32,
    /// Answers of every class to requests parked before the placeholder existed (item 6 (f)), by how they ended.
    pub(crate) pre_parked_woken: u32,
    pub(crate) pre_parked_bound: u32,
    /// Keep-alives handled without a registration because a limit was full (item 6 (c), (f)).
    pub(crate) registrations_capped: u32,
    pub(crate) requests_ok: u32,
    pub(crate) requests_no_data: u32,
    pub(crate) requests_peer_error: u32,
    pub(crate) requests_timeout: u32,
    pub(crate) requests_transport: u32,
    pub(crate) requests_decode: u32,
    pub(crate) alt_failure: u32,
    pub(crate) alt_mismatch: u32,
    pub(crate) alt_no_relay: u32,
    pub(crate) alt_worker_late: u32,
    pub(crate) alt_sent: u32,
    pub(crate) alt_ok: u32,
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
                    AnswerPath::ParkedWoken | AnswerPath::PreParkedWoken => self.placeholder_parked_woken += 1,
                    AnswerPath::ParkedBound | AnswerPath::PreParkedBound => self.placeholder_parked_bound += 1,
                }
            },
            AnswerClass::TrieError => self.answers_trie_error += 1,
        }
        match path {
            AnswerPath::AtOnce(_) => (),
            AnswerPath::ParkedWoken | AnswerPath::ParkedBound => self.answers_parked += 1,
            AnswerPath::PreParkedWoken => {
                self.answers_parked += 1;
                self.pre_parked_woken += 1;
            },
            AnswerPath::PreParkedBound => {
                self.answers_parked += 1;
                self.pre_parked_bound += 1;
            },
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

    fn count_alt(&mut self, event: AltEvent) {
        match event {
            AltEvent::Failure => self.alt_failure += 1,
            AltEvent::Mismatch => self.alt_mismatch += 1,
            AltEvent::NoRelay => self.alt_no_relay += 1,
            AltEvent::WorkerLate => self.alt_worker_late += 1,
            AltEvent::Sent => self.alt_sent += 1,
            AltEvent::Ok => self.alt_ok += 1,
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
            + self.alt_failure
            + self.alt_mismatch
            + self.registrations_capped
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
             no_data={} peer_error={} timeout={} transport={} decode={}; alt failure={} mismatch={} no_relay={} \
             worker_late={} sent={} ok={}; pre_placeholder parked_woken={} parked_bound={} registrations_capped={}",
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
            self.alt_failure,
            self.alt_mismatch,
            self.alt_no_relay,
            self.alt_worker_late,
            self.alt_sent,
            self.alt_ok,
            self.pre_parked_woken,
            self.pre_parked_bound,
            self.registrations_capped,
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
    requests: Vec<(Attempt, RequestClass)>,
    /// The answers recorded so far (tests only).
    #[cfg(test)]
    answers: Vec<(AnswerClass, AnswerPath)>,
    /// The skip counts of the mismatch lines recorded so far (tests only).
    #[cfg(test)]
    skips: Vec<SkipCounts>,
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
    pub(crate) attempt: Attempt,
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

/// The DEBUG line of one step of the request to one other relay: `sync alternate relay: pubkey=<P> event=<event>`
/// and, for `event=mismatch` with `skips`, the tail of the candidate rules that would have skipped the diverged
/// pairs (`pairs=<n> skip_a=<k> skip_b=<k> skip_d=<k> skip_ad=<k> skip_ab=<k> since_keep_alive_ms=<ms>`).
struct AltLine<'a> {
    event: AltEvent,
    pubkey: &'a str,
    skips: Option<&'a SkipCounts>,
}

impl fmt::Display for AltLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sync alternate relay: pubkey={} event={}",
            self.pubkey,
            self.event.as_str()
        )?;
        match (self.event, self.skips) {
            (AltEvent::Mismatch, Some(skips)) => write!(
                f,
                " pairs={} skip_a={} skip_b={} skip_d={} skip_ad={} skip_ab={} since_keep_alive_ms={}",
                skips.pairs,
                skips.skip_a,
                skips.skip_b,
                skips.skip_d,
                skips.skip_ad,
                skips.skip_ab,
                skips.since_keep_alive_ms
            ),
            _ => Ok(()),
        }
    }
}

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
        let reason = match event.attempt {
            Attempt::Alternate(reason) => format!(" reason={}", reason),
            Attempt::First | Attempt::Second => String::new(),
        };
        log::debug!(
            "sync request: peer={} pubkey={} pairs={} attempt={}{} class={} elapsed_ms={}{}",
            event.peer,
            event.pubkey,
            event.pairs,
            event.attempt,
            reason,
            event.class.as_str(),
            event.elapsed_ms,
            error
        );
        if let Some(summary) = self.record_request(event.attempt, event.class, now_ms) {
            log::info!("{}", summary);
        }
    }

    /// Records one step of the request to one other relay: its DEBUG line, its count and, when the window has
    /// ended, the summary. `pubkey` is the keep-alive's.
    pub(crate) fn alt(&self, event: AltEvent, pubkey: &str, now_ms: u64) {
        self.alt_with_skips(event, pubkey, None, now_ms)
    }

    /// Like [`SyncLog::alt`]; for [`AltEvent::Mismatch`] the DEBUG line carries the `skips` of the diverged pairs
    /// when there are any. The counters and the summary are those of `alt`.
    pub(crate) fn alt_with_skips(&self, event: AltEvent, pubkey: &str, skips: Option<&SkipCounts>, now_ms: u64) {
        log::debug!("{}", AltLine { event, pubkey, skips });
        #[cfg(test)]
        {
            if let (AltEvent::Mismatch, Some(skips)) = (event, skips) {
                self.0.lock().skips.push(skips.clone());
            }
        }
        if let Some(summary) = self.record_alt(event, now_ms) {
            log::info!("{}", summary);
        }
    }

    /// Records a keep-alive handled without a registration because a limit of CRD ch.32 R-Q5b item 6 (c) was
    /// full: its DEBUG line, its count and, when the window has ended, the summary.
    pub(crate) fn registration_capped(&self, forwarding_peer: &str, pubkey: &str, now_ms: u64) {
        log::debug!("sync registration capped: peer={} pubkey={}", forwarding_peer, pubkey);
        if let Some(summary) = self.count(now_ms, |counters| counters.registrations_capped += 1) {
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

    fn record_alt(&self, event: AltEvent, now_ms: u64) -> Option<String> {
        self.count(now_ms, |counters| counters.count_alt(event))
    }

    #[cfg_attr(not(test), allow(unused_variables))]
    fn record_request(&self, attempt: Attempt, class: RequestClass, now_ms: u64) -> Option<String> {
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
    pub(crate) fn requests(&self) -> Vec<(Attempt, RequestClass)> { self.0.lock().requests.clone() }

    #[cfg(test)]
    pub(crate) fn skips(&self) -> Vec<SkipCounts> { self.0.lock().skips.clone() }
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
        assert_eq!(
            log.record_request(Attempt::First, RequestClass::Timeout, t0 + 30_000),
            None
        );
        assert_eq!(log.snapshot().placeholder_caps, 1);
        assert_eq!(log.snapshot().requests_timeout, 1);

        // The event that finds the window ended is counted into it, and then the counters are reset.
        let summary = log
            .record_request(Attempt::Second, RequestClass::Transport, t0 + SUMMARY_WINDOW_MS)
            .expect("a failure was counted");
        assert!(summary.starts_with("sync summary 60s: answers ok=0 no_data=0 missing_pair=0 placeholder=1("));
        assert!(summary.contains("caps=1"), "{}", summary);
        assert!(summary.contains("timeout=1 transport=1 decode=0"), "{}", summary);
        assert_eq!(log.snapshot(), SyncCounters::default());

        // The next window starts from zero: its own failure is the only one reported.
        assert_eq!(
            log.record_request(Attempt::First, RequestClass::NoData, t0 + 70_000),
            None
        );
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
        assert_eq!(log.record_request(Attempt::First, RequestClass::Ok, t0 + 10), None);
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
            alt_failure: 18,
            alt_mismatch: 19,
            alt_no_relay: 20,
            alt_worker_late: 21,
            alt_sent: 22,
            alt_ok: 23,
            pre_parked_woken: 24,
            pre_parked_bound: 25,
            registrations_capped: 26,
        };
        assert_eq!(
            counters.summary_line().unwrap(),
            "sync summary 60s: answers ok=3 no_data=3 missing_pair=4 placeholder=15(at_once=5 own_not_in_flight=6 \
             caps=7 parked_woken=8 parked_bound=9) trie_error=10 parked=11; requests ok=12 no_data=13 peer_error=14 \
             timeout=15 transport=16 decode=17; alt failure=18 mismatch=19 no_relay=20 worker_late=21 sent=22 ok=23; \
             pre_placeholder parked_woken=24 parked_bound=25 registrations_capped=26"
        );
    }

    /// A window with a trigger of R-Q5d is printed; the steps after a trigger alone are not a failure.
    #[test]
    fn a_window_with_an_alternate_relay_trigger_prints_its_line() {
        for trigger in [AltEvent::Failure, AltEvent::Mismatch] {
            let log = SyncLog::default();
            let t0 = 3_000;
            assert_eq!(log.record_alt(trigger, t0), None);
            assert_eq!(log.record_alt(AltEvent::Sent, t0 + 1), None);
            assert_eq!(log.record_alt(AltEvent::Ok, t0 + 2), None);
            let summary = log
                .count(t0 + SUMMARY_WINDOW_MS, |_| ())
                .expect("a trigger was counted");
            assert!(
                summary.contains("; alt failure=1 mismatch=0 no_relay=0 worker_late=0 sent=1 ok=1;")
                    || summary.contains("; alt failure=0 mismatch=1 no_relay=0 worker_late=0 sent=1 ok=1;"),
                "{}",
                summary
            );
        }
    }

    #[test]
    fn the_steps_after_a_trigger_are_counted_one_by_one() {
        let log = SyncLog::default();
        for event in [
            AltEvent::Failure,
            AltEvent::Mismatch,
            AltEvent::Mismatch,
            AltEvent::NoRelay,
            AltEvent::WorkerLate,
            AltEvent::Sent,
            AltEvent::Sent,
            AltEvent::Sent,
            AltEvent::Ok,
        ] {
            assert_eq!(log.record_alt(event, 1_000), None);
        }
        let counters = log.snapshot();
        assert_eq!(
            (
                counters.alt_failure,
                counters.alt_mismatch,
                counters.alt_no_relay,
                counters.alt_worker_late,
                counters.alt_sent,
                counters.alt_ok
            ),
            (1, 2, 1, 1, 3, 1)
        );
        // No trigger, no line: the later steps alone are not a failure.
        let log = SyncLog::default();
        assert_eq!(log.record_alt(AltEvent::NoRelay, 1_000), None);
        assert_eq!(log.record_alt(AltEvent::WorkerLate, 1_001), None);
        assert_eq!(log.count(1_000 + SUMMARY_WINDOW_MS, |_| ()), None);
    }

    /// Hex constants only: no value derived from a key pair reaches a logging call.
    const PUBKEY: &str = "02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn the_mismatch_line_carries_the_skip_counts_in_the_documented_order() {
        let skips = SkipCounts {
            pairs: 5,
            skip_a: 1,
            skip_b: 2,
            skip_d: 3,
            skip_ad: 4,
            skip_ab: 3,
            since_keep_alive_ms: 1234,
        };
        assert_eq!(
            AltLine {
                event: AltEvent::Mismatch,
                pubkey: PUBKEY,
                skips: Some(&skips)
            }
            .to_string(),
            format!(
                "sync alternate relay: pubkey={} event=mismatch pairs=5 skip_a=1 skip_b=2 skip_d=3 skip_ad=4 \
                 skip_ab=3 since_keep_alive_ms=1234",
                PUBKEY
            )
        );
    }

    #[test]
    fn the_alternate_relay_line_without_counts_is_unchanged() {
        for event in [
            AltEvent::Failure,
            AltEvent::Mismatch,
            AltEvent::NoRelay,
            AltEvent::WorkerLate,
            AltEvent::Sent,
            AltEvent::Ok,
        ] {
            assert_eq!(
                AltLine {
                    event,
                    pubkey: PUBKEY,
                    skips: None
                }
                .to_string(),
                format!("sync alternate relay: pubkey={} event={}", PUBKEY, event.as_str())
            );
        }
        // The tail belongs to the mismatch line alone.
        let skips = SkipCounts::default();
        assert_eq!(
            AltLine {
                event: AltEvent::Sent,
                pubkey: PUBKEY,
                skips: Some(&skips)
            }
            .to_string(),
            format!("sync alternate relay: pubkey={} event=sent", PUBKEY)
        );
        assert_eq!(
            AltLine {
                event: AltEvent::Mismatch,
                pubkey: PUBKEY,
                skips: None
            }
            .to_string(),
            format!("sync alternate relay: pubkey={} event=mismatch", PUBKEY)
        );
    }

    #[test]
    fn the_skip_counts_do_not_change_the_counters() {
        let with = SyncLog::default();
        let without = SyncLog::default();
        with.alt_with_skips(AltEvent::Mismatch, PUBKEY, Some(&SkipCounts::default()), 1_000);
        without.alt(AltEvent::Mismatch, PUBKEY, 1_000);
        assert_eq!(with.snapshot(), without.snapshot());
        assert_eq!(with.skips(), vec![SkipCounts::default()]);
        assert!(without.skips().is_empty());
    }

    #[test]
    fn the_attempt_is_a_label_and_the_reason_names_the_alternate_request() {
        assert_eq!(Attempt::First.to_string(), "1");
        assert_eq!(Attempt::Second.to_string(), "2");
        assert_eq!(Attempt::Alternate(AltReason::Failure).to_string(), "alt");
        assert_eq!(Attempt::Alternate(AltReason::Mismatch).to_string(), "alt");
        assert_eq!(AltReason::Failure.to_string(), "failure");
        assert_eq!(AltReason::Mismatch.to_string(), "mismatch");
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
