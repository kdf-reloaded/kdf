//! # mm2_event_stream
//!
//! Reloaded SSE fan-out core. Streamers publish JSON events; the
//! [`StreamingManager`] multiplexes them out to per-client channels.
//!
//! ## Public API
//! - [`Event`] — single broadcast payload tagged with its origin streamer.
//! - [`EventStreamer`] — async trait every streamer implements.
//! - [`Broadcaster`] — handle a streamer uses to push events to subscribers.
//! - [`StreamerId`] — wire-stable origin identifier (`HEARTBEAT`, `BALANCE:KMD`, …).
//! - [`StreamingManager`] — registry that spawns streamers and routes events.
//! - [`NoDataIn`] — uninhabited marker for self-driven streamers.
//!
//! ## Invariants
//! - A single streamer instance is shared across all subscribers; the first
//!   subscribe spawns it, the last unsubscribe shuts it down.
//! - Per-client channels are bounded; back-pressured events are dropped for
//!   the slow client only, never block the broadcaster.
//! - [`StreamerId`] string forms are part of the SSE wire contract and must
//!   not change without coordinating GUI consumers.
//!
//! ## Non-goals
//! - This crate does not implement the HTTP/SSE transport — that lives in
//!   `mm2_main::rpc`.
//! - It does not own any concrete streamers; those are defined where their
//!   data sources live.

mod event;
mod manager;
mod streamer;

pub use event::Event;
pub use manager::{StopStreamError, StreamingManager};
pub use streamer::{Broadcaster, EventStreamer, NoDataIn, StreamerId};

// Re-export the tokio channel primitives surfaced through `EventStreamer`
// so downstream crates need no direct tokio dependency (it stays optional
// on WASM targets).
pub use tokio::sync::{mpsc, oneshot};
