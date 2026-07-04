//! # Purpose
//! Implements the interactive data-asker facility (R19): an always-present
//! registry on [`crate::mm_ctx::MmCtx`] by which an internal daemon flow ASKS
//! an external client (e.g. a GUI) for a piece of data over the event-stream
//! substrate and asynchronously awaits the client's answer.
//!
//! # Public exports
//! - [`DataAsker`] — the registry: a process-unique data-id counter plus a
//!   map from data id to a single-use one-shot answer sender.
//! - [`AskForDataError`] — failure modes of the async ask operation.
//! - [`WaiterDropped`] — internal answer-delivery failure (the awaiting caller
//!   has already departed); the RPC layer maps it to `500`.
//!
//! # Invariants
//! - The data-needed wire event is dictated interop: its category renders as
//!   `DATA_NEEDED:<data_type>` and its message carries exactly `data_id`,
//!   `timeout_secs` (whole seconds), and `data`.
//! - Unanswered entries are reclaimed by the ask side on timeout (self-cleanup);
//!   no background reaper is required.

use common::executor::Timer;
use futures::channel::oneshot;
use futures::FutureExt;
use mm2_event_stream::{Event, StreamerId, StreamingManager};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// Failure modes of [`DataAsker::ask_for_data`].
#[derive(Debug)]
pub enum AskForDataError {
    /// The data-type discriminator contained whitespace (rejected before any
    /// data id is allocated).
    InvalidDataType(String),
    /// The ask timed out before an answer arrived; the pending entry has been
    /// removed from the registry.
    Timeout { data_type: String, timeout_secs: u64 },
    /// The delivered answer could not be deserialised into the expected type.
    ResponseDeserialization(String),
    /// An internal error occurred while preparing or registering the ask.
    Internal(String),
}

impl fmt::Display for AskForDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AskForDataError::InvalidDataType(data_type) => {
                write!(f, "Data type '{}' must not contain whitespace", data_type)
            },
            AskForDataError::Timeout {
                data_type,
                timeout_secs,
            } => write!(f, "Ask for '{}' timed out after {}s", data_type, timeout_secs),
            AskForDataError::ResponseDeserialization(err) => {
                write!(f, "Failed to deserialize answer payload: {}", err)
            },
            AskForDataError::Internal(err) => write!(f, "Internal data-asker error: {}", err),
        }
    }
}

/// The awaiting caller of an ask has already departed, so the answer could not
/// be delivered. The RPC layer maps this to `500 Internal Server Error`.
#[derive(Debug)]
pub struct WaiterDropped;

/// Interactive data-asker registry (R19).
///
/// Owns a process-unique monotonically-increasing data-id counter and a
/// mutex-guarded map from data id to a single-use one-shot answer sender.
#[derive(Default)]
pub struct DataAsker {
    next_data_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Json>>>,
}

impl DataAsker {
    /// Submit an ask: register a one-shot waiter, emit the data-needed event
    /// over `streaming_manager`, then await the answer until it arrives or the
    /// timeout elapses.
    ///
    /// The data-type discriminator must not contain whitespace. The emitted
    /// `timeout_secs` and the awaited timeout are both the whole-seconds
    /// truncation of `timeout_secs`.
    pub async fn ask_for_data<Input, Output>(
        &self,
        streaming_manager: &StreamingManager,
        data_type: &str,
        data: Input,
        timeout_secs: f64,
    ) -> Result<Output, AskForDataError>
    where
        Input: Serialize,
        Output: DeserializeOwned,
    {
        if data_type.chars().any(char::is_whitespace) {
            return Err(AskForDataError::InvalidDataType(data_type.to_owned()));
        }

        let payload = serde_json::to_value(data).map_err(|e| AskForDataError::Internal(e.to_string()))?;
        let timeout_secs = timeout_secs.max(0.0).trunc() as u64;

        let data_id = self.next_data_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(data_id, tx);

        let message = json!({
            "data_id": data_id,
            "timeout_secs": timeout_secs,
            "data": payload,
        });
        streaming_manager.broadcast(Event::new(StreamerId::DataNeeded(data_type.to_owned()), message));

        let mut rx = rx.fuse();
        let mut timeout = Timer::sleep(timeout_secs as f64).fuse();
        futures::select! {
            answer = rx => match answer {
                Ok(value) => serde_json::from_value(value)
                    .map_err(|e| AskForDataError::ResponseDeserialization(e.to_string())),
                // The sender was dropped without delivering an answer.
                Err(_canceled) => Err(AskForDataError::Internal("Answer channel closed".to_owned())),
            },
            _ = timeout => {
                self.pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&data_id);
                Err(AskForDataError::Timeout {
                    data_type: data_type.to_owned(),
                    timeout_secs,
                })
            },
        }
    }

    /// Deliver an answer to a pending ask.
    ///
    /// Returns `Ok(true)` if a matching pending ask existed and was resolved,
    /// `Ok(false)` if no pending ask exists for `data_id` (the RPC layer maps
    /// this to `404`), and `Err(WaiterDropped)` if the awaiting caller has
    /// already departed (mapped to `500`).
    pub fn send_asked_data(&self, data_id: u64, data: Json) -> Result<bool, WaiterDropped> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        match pending.remove(&data_id) {
            None => Ok(false),
            Some(sender) => sender.send(data).map(|()| true).map_err(|_| WaiterDropped),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use mm2_event_stream::StreamingManager;

    #[test]
    fn ask_and_answer_round_trip() {
        let asker = DataAsker::default();
        let manager = StreamingManager::default();

        let (answer, deliver_result) = block_on(async {
            let ask = asker.ask_for_data::<_, String>(&manager, "needed_data", "question", 30.0);
            let deliver = async {
                // `join` polls `ask` first, so it registers data id 0 before this
                // future runs and answers a genuinely-pending ask.
                asker.send_asked_data(0, serde_json::json!("answer"))
            };
            futures::future::join(ask, deliver).await
        });

        assert_eq!(answer.expect("ask should resolve with the answer"), "answer");
        assert!(deliver_result.expect("answer delivery should succeed"));
    }

    #[test]
    fn answer_unknown_data_id_reports_not_found() {
        let asker = DataAsker::default();
        // No ask was ever registered for id 42.
        let resolved = asker
            .send_asked_data(42, serde_json::json!("answer"))
            .expect("delivery to a missing entry is not an internal error");
        assert!(!resolved);
    }
}
