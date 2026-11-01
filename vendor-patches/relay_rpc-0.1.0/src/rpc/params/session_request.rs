//! https://specs.walletconnect.com/2.0/specs/clients/sign/rpc-methods
//! #wc_sessionrequest

use {
    super::IrnMetadata,
    serde::{Deserialize, Serialize},
};

pub(super) const IRN_REQUEST_METADATA: IrnMetadata = IrnMetadata {
    tag: 1108,
    ttl: 300,
    prompt: true,
};

pub(super) const IRN_RESPONSE_METADATA: IrnMetadata = IrnMetadata {
    tag: 1109,
    ttl: 300,
    prompt: false,
};

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub method: String,
    /// Opaque blockchain RPC parameters.
    ///
    /// Parsing is deferred to a higher level, blockchain RPC aware code.
    pub params: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<u64>,
}

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionRequestRequest {
    pub request: Request,
    pub chain_id: String,
}

#[cfg(test)]
mod tests {
    use {super::*, crate::rpc::params::session::param_serde_test, anyhow::Result};

    #[test]
    fn test_serde_eth_sign_transaction() -> Result<()> {
        // https://specs.walletconnect.com/2.0/specs/clients/sign/
        // session-events#session_request
        let json = r#"
        {
            "request": {
                "method": "eth_signTransaction",
                "params": [
                    {
                        "data": "0x",
                        "from": "0x1456225dE90927193F7A171E64a600416f96f2C8",
                        "gasLimit": "0x5208",
                        "gasPrice": "0xa72c",
                        "nonce": "0x00",
                        "to": "0x1456225dE90927193F7A171E64a600416f96f2C8",
                        "value": "0x00"
                    }
                ]
            },
            "chainId": "eip155:5"
        }
        "#;

        param_serde_test::<SessionRequestRequest>(json)
    }
}
