//! https://specs.walletconnect.com/2.0/specs/clients/sign/rpc-methods
//! #wc_sessionsettle

use {
    super::{session::SettleNamespaces, IrnMetadata},
    crate::rpc::params::{Metadata, Relay},
    serde::{Deserialize, Serialize},
    serde_json::Value,
};

pub(super) const IRN_REQUEST_METADATA: IrnMetadata = IrnMetadata {
    tag: 1102,
    ttl: 300,
    prompt: false,
};

pub(super) const IRN_RESPONSE_METADATA: IrnMetadata = IrnMetadata {
    tag: 1103,
    ttl: 300,
    prompt: false,
};

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Controller {
    pub public_key: String,
    pub metadata: Metadata,
}

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionSettleRequest {
    pub relay: Relay,
    pub controller: Controller,
    pub namespaces: SettleNamespaces,
    /// Unix timestamp.
    ///
    /// Expiry should be between .now() + TTL.
    pub expiry: u64,
    pub session_properties: Option<Value>,
}

#[cfg(test)]
mod tests {
    use {super::*, crate::rpc::params::session::param_serde_test, anyhow::Result};

    #[test]
    fn test_serde_session_settle_request() -> Result<()> {
        // Coppied from `session_propose` and adjusted slightly.
        let json = r#"
        {
            "relay": {
                "protocol": "irn"
            },
            "controller": {
                "publicKey": "a3ad5e26070ddb2809200c6f56e739333512015bceeadbb8ea1731c4c7ddb207",
                "metadata": {
                    "description": "React App for WalletConnect",
                    "url": "http://localhost:3000",
                    "icons": [
                        "https://avatars.githubusercontent.com/u/37784886"
                    ],
                    "name": "React App"
                }
            },
            "namespaces": {
                "eip155": {
                    "chains": [],
                    "accounts": [
                        "eip155:5:0xBA5BA3955463ADcc7aa3E33bbdfb8A68e0933dD8"
                    ],
                    "methods": [
                        "eth_sendTransaction",
                        "eth_sign",
                        "eth_signTransaction",
                        "eth_signTypedData",
                        "personal_sign"
                    ],
                    "events": [
                        "accountsChanged",
                        "chainChanged"
                    ]
                }
            },
            "expiry": 1675734962,
            "sessionProperties": null
        }
        "#;

        param_serde_test::<SessionSettleRequest>(json)
    }
}
