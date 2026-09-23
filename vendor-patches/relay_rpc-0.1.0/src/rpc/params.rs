use {
    super::{ErrorData, Params},
    pairing_delete::PairingDeleteRequest,
    pairing_extend::PairingExtendRequest,
    pairing_ping::PairingPingRequest,
    paste::paste,
    serde::{Deserialize, Serialize},
    serde_json::Value,
    session_delete::SessionDeleteRequest,
    session_event::SessionEventRequest,
    session_extend::SessionExtendRequest,
    session_propose::{SessionProposeRequest, SessionProposeResponse},
    session_request::SessionRequestRequest,
    session_settle::SessionSettleRequest,
    session_update::SessionUpdateRequest,
};

pub mod arbitrary;
pub mod pairing_delete;
pub mod pairing_extend;
pub mod pairing_ping;
pub mod session;
pub mod session_delete;
pub mod session_event;
pub mod session_extend;
pub mod session_ping;
pub mod session_propose;
pub mod session_request;
pub mod session_settle;
pub mod session_update;

/// Metadata associated with a pairing.
#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub description: String,
    pub url: String,
    pub icons: Vec<String>,
    pub name: String,
}

/// Information about the relay used for communication.
#[derive(Debug, Serialize, PartialEq, Eq, Hash, Deserialize, Clone, Default)]
pub struct Relay {
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub data: Option<String>,
}

/// Relay IRN protocol metadata.
///
/// https://specs.walletconnect.com/2.0/specs/servers/relay/relay-server-rpc
/// #definitions
#[derive(Debug, Clone, Copy)]
pub struct IrnMetadata {
    pub tag: u32,
    pub ttl: u64,
    pub prompt: bool,
}

/// Relay protocol metadata.
///
///  https://specs.walletconnect.com/2.0/specs/clients/sign/rpc-methods
pub trait RelayProtocolMetadata {
    /// Retrieves IRN relay protocol metadata.
    ///
    /// Every method must return corresponding IRN metadata.
    fn irn_metadata(&self) -> IrnMetadata;
}

pub trait RelayProtocolHelpers {
    type Params;

    /// Converts "unnamed" payload parameters into typed.
    ///
    /// Example: success and error response payload does not specify the
    /// method. Thus the only way to deserialize the data into typed
    /// parameters, is to use the tag to determine the response method.
    ///
    /// This is a convenience method, so that users don't have to deal
    /// with the tags directly.
    fn irn_try_from_tag(value: Value, tag: u32) -> Result<Self::Params, ParamsError>;
}

/// Errors covering API payload parameter conversion issues.
#[derive(Debug, thiserror::Error)]
pub enum ParamsError {
    /// Pairing API serialization/deserialization issues.
    #[error("Failure serializing/deserializing Sign API parameters: {0}")]
    Serde(#[from] serde_json::Error),
    /// Pairing API invalid response tag.
    #[error("Response tag={0} does not match any of the Sign API methods")]
    ResponseTag(u32),
}

/// https://www.jsonrpc.org/specification#response_object
///
/// JSON RPC 2.0 response object can either carry success or error data.
/// Please note, that relay protocol metadata is used to disambiguate the
/// response data.
///
/// For example:
/// `RelayProtocolHelpers::irn_try_from_tag` is used to deserialize an opaque
/// response data into the typed parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseParams {
    /// A response with a result.
    #[serde(rename = "result")]
    Success(Value),

    /// A response for a failed request.
    #[serde(rename = "error")]
    Err(Value),
}

// Convenience macro to de-duplicate implementation for different parameter
// sets.
macro_rules! impl_relay_protocol_metadata {
    ($param_type:ty,$meta:ident) => {
        paste! {
            impl RelayProtocolMetadata for $param_type {
                fn irn_metadata(&self) -> IrnMetadata {
                    match self {
                        [<$param_type>]::SessionPropose(_) => session_propose::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::SessionSettle(_) => session_settle::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::SessionRequest(_) => session_request::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::SessionUpdate(_) => session_update::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::SessionDelete(_) => session_delete::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::SessionEvent(_) => session_event::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::SessionExtend(_) => session_extend::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::SessionPing(_) => session_ping::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::PairingDelete(_) => pairing_delete::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::PairingExtend(_) => pairing_extend::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::PairingPing(_) => pairing_ping::[<IRN_ $meta:upper _METADATA>],
                        [<$param_type>]::Arbitrary(_) => arbitrary::[<IRN_ $meta:upper _METADATA>],
                    }
                }
            }
        }
    }
}

// Convenience macro to de-duplicate implementation for different parameter
// sets.
macro_rules! impl_relay_protocol_helpers {
    ($param_type:ty) => {
        paste! {
            impl RelayProtocolHelpers for $param_type {
                type Params = Self;

                fn irn_try_from_tag(value: Value, tag: u32) -> Result<Self::Params, ParamsError> {
                    match tag {
                        tag if tag == session_propose::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::SessionPropose(serde_json::from_value(value)?))
                        }
                        tag if tag == session_settle::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::SessionSettle(serde_json::from_value(value)?))
                        }
                        tag if tag == session_request::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::SessionRequest(serde_json::from_value(value)?))
                        }
                        tag if tag == session_delete::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::SessionDelete(serde_json::from_value(value)?))
                        }
                        tag if tag == session_extend::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::SessionExtend(serde_json::from_value(value)?))
                        }
                        tag if tag == session_update::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::SessionUpdate(serde_json::from_value(value)?))
                        }
                        tag if tag == session_event::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::SessionEvent(serde_json::from_value(value)?))
                        }
                        tag if tag == session_event::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::SessionPing(serde_json::from_value(value)?))
                        }
                        tag if tag == pairing_delete::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::PairingDelete(serde_json::from_value(value)?))
                        }
                        tag if tag == pairing_extend::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::PairingExtend(serde_json::from_value(value)?))
                        }
                        tag if tag == pairing_ping::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::PairingPing(serde_json::from_value(value)?))
                        }
                        tag if tag == arbitrary::IRN_RESPONSE_METADATA.tag => {
                            Ok(Self::Arbitrary(serde_json::from_value(value)?))
                        }
                        _ => Err(ParamsError::ResponseTag(tag)),
                    }
                }
            }
        }
    };
}

/// Sign API request parameters.
///
/// https://specs.walletconnect.com/2.0/specs/clients/sign/rpc-methods
/// https://specs.walletconnect.com/2.0/specs/clients/sign/data-structures
#[derive(Debug, Serialize, Eq, Deserialize, Clone, PartialEq)]
#[serde(tag = "method", content = "params")]
pub enum RequestParams {
    SessionPropose(SessionProposeRequest),
    SessionSettle(SessionSettleRequest),
    SessionUpdate(SessionUpdateRequest),
    SessionExtend(SessionExtendRequest),
    SessionRequest(SessionRequestRequest),
    SessionEvent(SessionEventRequest),
    SessionDelete(SessionDeleteRequest),
    SessionPing(()),
    PairingExtend(PairingExtendRequest),
    PairingDelete(PairingDeleteRequest),
    PairingPing(PairingPingRequest),
    Arbitrary(Value),
}

impl_relay_protocol_metadata!(RequestParams, request);

impl From<RequestParams> for Params {
    fn from(value: RequestParams) -> Self {
        match value {
            RequestParams::PairingPing(param) => Params::PairingPing(param),
            RequestParams::PairingDelete(param) => Params::PairingDelete(param),
            RequestParams::SessionPing(()) => Params::SessionPing(()),
            RequestParams::SessionPropose(param) => Params::SessionPropose(param),
            RequestParams::SessionSettle(param) => Params::SessionSettle(param),
            RequestParams::SessionUpdate(param) => Params::SessionUpdate(param),
            RequestParams::SessionExtend(param) => Params::SessionExtend(param),
            RequestParams::SessionRequest(param) => Params::SessionRequest(param),
            RequestParams::SessionEvent(param) => Params::SessionEvent(param),
            RequestParams::SessionDelete(param) => Params::SessionDelete(param),
            RequestParams::PairingExtend(param) => Params::PairingExtend(param),
            RequestParams::Arbitrary(_param) => unreachable!(),
        }
    }
}

/// Typed success response parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseParamsSuccess {
    SessionPropose(SessionProposeResponse),
    SessionSettle(bool),
    SessionUpdate(bool),
    SessionExtend(bool),
    SessionRequest(bool),
    SessionEvent(bool),
    SessionDelete(bool),
    SessionPing(bool),

    PairingExtend(bool),
    PairingDelete(bool),
    PairingPing(bool),
    Arbitrary(Value),
}

impl_relay_protocol_metadata!(ResponseParamsSuccess, response);
impl_relay_protocol_helpers!(ResponseParamsSuccess);

impl TryFrom<ResponseParamsSuccess> for ResponseParams {
    type Error = ParamsError;

    fn try_from(value: ResponseParamsSuccess) -> Result<Self, Self::Error> {
        Ok(Self::Success(serde_json::to_value(value)?))
    }
}

/// Typed error response parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseParamsError {
    SessionPropose(ErrorData),
    SessionSettle(ErrorData),
    SessionUpdate(ErrorData),
    SessionExtend(ErrorData),
    SessionRequest(ErrorData),
    SessionEvent(ErrorData),
    SessionDelete(ErrorData),
    SessionPing(ErrorData),
    PairingDelete(ErrorData),
    PairingExtend(ErrorData),
    PairingPing(ErrorData),
    Arbitrary(ErrorData),
}
impl_relay_protocol_metadata!(ResponseParamsError, response);
impl_relay_protocol_helpers!(ResponseParamsError);

impl TryFrom<ResponseParamsError> for ResponseParams {
    type Error = ParamsError;

    fn try_from(value: ResponseParamsError) -> Result<Self, Self::Error> {
        Ok(Self::Err(serde_json::to_value(value)?))
    }
}

impl ResponseParamsError {
    pub fn error(&self) -> ErrorData {
        match self {
            Self::SessionPing(data)
            | Self::PairingPing(data)
            | Self::SessionEvent(data)
            | Self::SessionExtend(data)
            | Self::SessionSettle(data)
            | Self::SessionUpdate(data)
            | Self::SessionDelete(data)
            | Self::SessionPropose(data)
            | Self::SessionRequest(data)
            | Self::PairingDelete(data)
            | Self::PairingExtend(data)
            | Self::Arbitrary(data) => data.clone(),
        }
    }

    pub fn irn_metadata(&self) -> IrnMetadata {
        match self {
            Self::SessionPing(_data)
            | Self::PairingPing(_data)
            | Self::SessionEvent(_data)
            | Self::SessionExtend(_data)
            | Self::SessionSettle(_data)
            | Self::SessionUpdate(_data)
            | Self::SessionDelete(_data)
            | Self::SessionPropose(_data)
            | Self::SessionRequest(_data)
            | Self::PairingDelete(_data)
            | Self::PairingExtend(_data)
            | Self::Arbitrary(_data) => self.irn_metadata(),
        }
    }
}
