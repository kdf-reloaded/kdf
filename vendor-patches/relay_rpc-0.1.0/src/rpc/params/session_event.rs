//! https://specs.walletconnect.com/2.0/specs/clients/sign/rpc-methods
//! #wc_sessionevent

use {
    super::IrnMetadata,
    serde::{Deserialize, Serialize},
};

pub(super) const IRN_REQUEST_METADATA: IrnMetadata = IrnMetadata {
    tag: 1110,
    ttl: 300,
    prompt: true,
};

pub(super) const IRN_RESPONSE_METADATA: IrnMetadata = IrnMetadata {
    tag: 1111,
    ttl: 300,
    prompt: false,
};

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub name: String,
    /// Opaque blockchain RPC data.
    ///
    /// Parsing is deferred to a higher level, blockchain RPC aware code.
    pub data: serde_json::Value,
}

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventRequest {
    pub event: Event,
    pub chain_id: String,
}

#[cfg(test)]
mod tests {
    use {super::*, crate::rpc::params::session::param_serde_test, anyhow::Result};

    #[test]
    fn test_serde_accounts_changed_event() -> Result<()> {
        // https://specs.walletconnect.com/2.0/specs/clients/sign/
        // session-events#session_event
        let json = r#"
        {
            "event": {
                "name": "accountsChanged",
                "data": ["0xab16a96D359eC26a11e2C2b3d8f8B8942d5Bfcdb"]
            },
            "chainId": "eip155:5"
        }
        "#;

        param_serde_test::<SessionEventRequest>(json)
    }
}
