//! `wc_sessionPing` payloads.

use super::IrnTag;

/// IRN relay tags for `wc_sessionPing`.
pub const TAG: IrnTag = IrnTag {
    request: 1114,
    response: 1115,
};

/// JSON-RPC `method` name for a session-ping request.
pub const METHOD: &str = "wc_sessionPing";

/// `wc_sessionPing` carries no parameters; the unit type stands in for it.
pub type PingRequest = ();
