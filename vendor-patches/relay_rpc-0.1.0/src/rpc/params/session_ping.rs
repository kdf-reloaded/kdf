//! https://specs.walletconnect.com/2.0/specs/clients/sign/rpc-methods
//! #wc_sessionping
use {
    super::IrnMetadata,
    serde::{Deserialize, Serialize},
};

pub(super) const IRN_REQUEST_METADATA: IrnMetadata = IrnMetadata {
    tag: 1114,
    ttl: 30,
    prompt: false,
};

pub(super) const IRN_RESPONSE_METADATA: IrnMetadata = IrnMetadata {
    tag: 1115,
    ttl: 30,
    prompt: false,
};

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionPingRequest {}

#[cfg(test)]
mod tests {
    use {super::*, crate::rpc::params::session::param_serde_test, anyhow::Result};

    #[test]
    fn test_serde_session_ping_request() -> Result<()> {
        let json = r#"{}"#;

        param_serde_test::<SessionPingRequest>(json)
    }
}
