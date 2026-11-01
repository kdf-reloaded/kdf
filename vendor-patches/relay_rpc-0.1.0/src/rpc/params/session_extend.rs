//! https://specs.walletconnect.com/2.0/specs/clients/sign/rpc-methods
//! #wc_sessionextend

use {
    super::IrnMetadata,
    serde::{Deserialize, Serialize},
};

pub(super) const IRN_REQUEST_METADATA: IrnMetadata = IrnMetadata {
    tag: 1106,
    ttl: 86400,
    prompt: false,
};

pub(super) const IRN_RESPONSE_METADATA: IrnMetadata = IrnMetadata {
    tag: 1107,
    ttl: 86400,
    prompt: false,
};

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionExtendRequest {
    pub expiry: u64,
}

#[cfg(test)]
mod tests {
    use {super::*, crate::rpc::params::session::param_serde_test, anyhow::Result};

    #[test]
    fn test_serde_session_extend_request() -> Result<()> {
        let json = r#"{"expiry": 86400}"#;

        param_serde_test::<SessionExtendRequest>(json)
    }
}
