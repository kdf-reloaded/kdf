///*
/// Request: Ask device for public key corresponding to address_n path
/// @start
/// @next EthereumPublicKey
/// @next Failure
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct EthereumGetPublicKey {
    /// BIP-32 path to derive the key from master node
    #[prost(uint32, repeated, packed = "false", tag = "1")]
    pub address_n: ::prost::alloc::vec::Vec<u32>,
    /// optionally show on display before sending the result
    #[prost(bool, optional, tag = "2")]
    pub show_display: ::core::option::Option<bool>,
}
///*
/// Response: Contains public key derived from device private seed
/// @end
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct EthereumPublicKey {
    /// BIP32 public node
    #[prost(message, required, tag = "1")]
    pub node: super::common::HdNodeType,
    /// serialized form of public node
    #[prost(string, required, tag = "2")]
    pub xpub: ::prost::alloc::string::String,
}
///*
/// Request: Ask device for Ethereum address corresponding to address_n path
/// @start
/// @next EthereumAddress
/// @next Failure
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct EthereumGetAddress {
    /// BIP-32 path to derive the key from master node
    #[prost(uint32, repeated, packed = "false", tag = "1")]
    pub address_n: ::prost::alloc::vec::Vec<u32>,
    /// optionally show on display before sending the result
    #[prost(bool, optional, tag = "2")]
    pub show_display: ::core::option::Option<bool>,
}
///*
/// Response: Contains an Ethereum address derived from device private seed
/// @end
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct EthereumAddress {
    /// Ethereum address as hex-encoded string
    #[prost(string, optional, tag = "2")]
    pub address: ::core::option::Option<::prost::alloc::string::String>,
}
///*
/// Request: Ask device to sign transaction
/// All fields are optional from the protocol's point of view. Each field defaults to value `0` if missing.
/// Note: the first at most 1024 bytes of data MUST be transmitted as part of this message.
/// @start
/// @next EthereumTxRequest
/// @next Failure
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct EthereumSignTx {
    /// BIP-32 path to derive the key from master node
    #[prost(uint32, repeated, packed = "false", tag = "1")]
    pub address_n: ::prost::alloc::vec::Vec<u32>,
    /// <=256 bit unsigned big endian
    #[prost(bytes = "vec", optional, tag = "2")]
    pub nonce: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
    /// <=256 bit unsigned big endian (in wei)
    #[prost(bytes = "vec", required, tag = "3")]
    pub gas_price: ::prost::alloc::vec::Vec<u8>,
    /// <=256 bit unsigned big endian
    #[prost(bytes = "vec", required, tag = "4")]
    pub gas_limit: ::prost::alloc::vec::Vec<u8>,
    /// recipient address
    #[prost(string, optional, tag = "11")]
    pub to: ::core::option::Option<::prost::alloc::string::String>,
    /// <=256 bit unsigned big endian (in wei)
    #[prost(bytes = "vec", optional, tag = "6")]
    pub value: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
    /// The initial data chunk (<= 1024 bytes)
    #[prost(bytes = "vec", optional, tag = "7")]
    pub data_initial_chunk: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
    /// Length of transaction payload
    #[prost(uint32, optional, tag = "8")]
    pub data_length: ::core::option::Option<u32>,
    /// Chain Id for EIP 155
    #[prost(uint64, required, tag = "9")]
    pub chain_id: u64,
}
///*
/// Response: Device asks for more data from transaction payload, or returns the signature.
/// If data_length is set, device awaits that many more bytes of payload.
/// Otherwise, the signature_* fields contain the computed transaction signature.
/// @next EthereumTxAck
/// @end
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct EthereumTxRequest {
    /// Number of bytes being requested (<= 1024)
    #[prost(uint32, optional, tag = "1")]
    pub data_length: ::core::option::Option<u32>,
    /// Computed signature (recovery parameter, limited to 27 or 28)
    #[prost(uint32, optional, tag = "2")]
    pub signature_v: ::core::option::Option<u32>,
    /// Computed signature R component (256 bit)
    #[prost(bytes = "vec", optional, tag = "3")]
    pub signature_r: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
    /// Computed signature S component (256 bit)
    #[prost(bytes = "vec", optional, tag = "4")]
    pub signature_s: ::core::option::Option<::prost::alloc::vec::Vec<u8>>,
}
///*
/// Request: Transaction payload data.
/// @next EthereumTxRequest
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct EthereumTxAck {
    /// Bytes from transaction payload (<= 1024 bytes)
    #[prost(bytes = "vec", required, tag = "1")]
    pub data_chunk: ::prost::alloc::vec::Vec<u8>,
}
