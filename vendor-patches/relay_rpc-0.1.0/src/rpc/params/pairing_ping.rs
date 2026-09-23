//! https://specs.walletconnect.com/2.0/specs/clients/core/pairing/rpc-methods
//! #wc_pairingping

use {
    super::IrnMetadata,
    serde::{Deserialize, Serialize},
};

pub(crate) const IRN_REQUEST_METADATA: IrnMetadata = IrnMetadata {
    tag: 1002,
    ttl: 30,
    prompt: false,
};

pub(crate) const IRN_RESPONSE_METADATA: IrnMetadata = IrnMetadata {
    tag: 1003,
    ttl: 30,
    prompt: false,
};

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PairingPingRequest {}
