//! LP-17 Phase 4c: Ethereum JSON-RPC wire types vendored from the
//! upstream `web3 = artemii235/rust-web3` git crate.
//!
//! These structs replace the `web3::types::*` re-exports while
//! preserving byte-for-byte wire compatibility (serde rename
//! attributes, enum variants, hex encoding, etc.) so that:
//!
//! * SQLite-persisted `TransactionDetails::tx_hex` (RLP of
//!   `SignedEthTx`) and `tx_hash` outputs remain identical pre/post
//!   migration,
//! * trace JSON serialised with `json::to_vec(&trace)` for history
//!   `internal_id` hashing keeps the same hash,
//! * Filter / TraceFilter builders serialise to the same
//!   `eth_getLogs` / `trace_filter` request bodies.
//!
//! Primitive types (`Address`, `H160`, `H256`, `U256`) come from
//! `ethereum_types` 0.4 (the same version the dropped `web3` 0.6.0
//! crate consumed), so the Serde representations are identical.
//!
//! Sourced from rust-web3 commit `9357249` files
//! `src/types/{block.rs, bytes.rs, log.rs, trace_filtering.rs,
//! transaction_request.rs}` under the upstream MIT license.

use ethereum_types::{H160, H256, U256};
use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

// ---------------------------------------------------------------- Bytes

/// Raw bytes wrapper with `0x`-prefixed hex serde.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Bytes(pub Vec<u8>);

impl<T: Into<Vec<u8>>> From<T> for Bytes {
    fn from(data: T) -> Self { Bytes(data.into()) }
}

impl Serialize for Bytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = String::with_capacity(2 + self.0.len() * 2);
        s.push_str("0x");
        s.push_str(&hex::encode(&self.0));
        serializer.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BytesVisitor;
        impl<'a> Visitor<'a> for BytesVisitor {
            type Value = Bytes;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a 0x-prefixed hex-encoded vector of bytes")
            }
            fn visit_str<E: DeError>(self, value: &str) -> Result<Bytes, E> {
                if value.len() >= 2 && &value[0..2] == "0x" && value.len() & 1 == 0 {
                    Ok(Bytes(hex::decode(&value[2..]).map_err(|_| E::custom("invalid hex"))?))
                } else {
                    Err(E::custom("invalid format"))
                }
            }
            fn visit_string<E: DeError>(self, value: String) -> Result<Bytes, E> { self.visit_str(&value) }
        }
        deserializer.deserialize_identifier(BytesVisitor)
    }
}

// ---------------------------------------------------------------- BlockNumber

/// Block selector for JSON-RPC calls.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BlockNumber {
    Latest,
    Earliest,
    Pending,
    Number(u64),
}

impl From<u64> for BlockNumber {
    fn from(num: u64) -> Self { BlockNumber::Number(num) }
}

impl Serialize for BlockNumber {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match *self {
            BlockNumber::Number(ref x) => serializer.serialize_str(&format!("0x{:x}", x)),
            BlockNumber::Latest => serializer.serialize_str("latest"),
            BlockNumber::Earliest => serializer.serialize_str("earliest"),
            BlockNumber::Pending => serializer.serialize_str("pending"),
        }
    }
}

// ---------------------------------------------------------------- CallRequest

/// `eth_call` / `eth_estimateGas` request payload.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CallRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<H160>,
    pub to: H160,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas: Option<U256>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "gasPrice")]
    pub gas_price: Option<U256>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<U256>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Bytes>,
}

// ---------------------------------------------------------------- Log + Filter

/// Event log returned by `eth_getLogs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Log {
    pub address: H160,
    pub topics: Vec<H256>,
    pub data: Bytes,
    #[serde(rename = "blockHash")]
    pub block_hash: Option<H256>,
    #[serde(rename = "blockNumber")]
    pub block_number: Option<U256>,
    #[serde(rename = "transactionHash")]
    pub transaction_hash: Option<H256>,
    #[serde(rename = "transactionIndex")]
    pub transaction_index: Option<U256>,
    #[serde(rename = "logIndex")]
    pub log_index: Option<U256>,
    #[serde(rename = "transactionLogIndex")]
    pub transaction_log_index: Option<U256>,
    #[serde(rename = "logType")]
    pub log_type: Option<String>,
    pub removed: Option<bool>,
}

impl Log {
    pub fn is_removed(&self) -> bool {
        if let Some(r) = self.removed {
            return r;
        }
        matches!(self.log_type.as_deref(), Some("removed"))
    }
}

#[derive(Default, Debug, PartialEq, Eq, Clone)]
struct ValueOrArray<T>(Vec<T>);

impl<T: Serialize> Serialize for ValueOrArray<T> {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0.len() {
            0 => s.serialize_none(),
            1 => Serialize::serialize(&self.0[0], s),
            _ => Serialize::serialize(&self.0, s),
        }
    }
}

#[derive(Default, Debug, PartialEq, Eq, Clone, Serialize)]
pub struct Filter {
    #[serde(rename = "fromBlock", skip_serializing_if = "Option::is_none")]
    from_block: Option<BlockNumber>,
    #[serde(rename = "toBlock", skip_serializing_if = "Option::is_none")]
    to_block: Option<BlockNumber>,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<ValueOrArray<H160>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    topics: Option<Vec<Option<ValueOrArray<H256>>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

#[derive(Default, Clone, Debug)]
pub struct FilterBuilder {
    filter: Filter,
}

impl FilterBuilder {
    pub fn from_block(mut self, block: BlockNumber) -> Self {
        self.filter.from_block = Some(block);
        self
    }
    pub fn to_block(mut self, block: BlockNumber) -> Self {
        self.filter.to_block = Some(block);
        self
    }
    pub fn address(mut self, address: Vec<H160>) -> Self {
        self.filter.address = Some(ValueOrArray(address));
        self
    }
    pub fn topics(
        mut self,
        topic1: Option<Vec<H256>>,
        topic2: Option<Vec<H256>>,
        topic3: Option<Vec<H256>>,
        topic4: Option<Vec<H256>>,
    ) -> Self {
        let mut topics = vec![topic1, topic2, topic3, topic4]
            .into_iter()
            .rev()
            .skip_while(Option::is_none)
            .map(|o| o.map(ValueOrArray))
            .collect::<Vec<_>>();
        topics.reverse();
        self.filter.topics = Some(topics);
        self
    }
    pub fn limit(mut self, limit: usize) -> Self {
        self.filter.limit = Some(limit);
        self
    }
    pub fn build(&self) -> Filter { self.filter.clone() }
}

// ---------------------------------------------------------------- TraceFilter + Trace

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceFilter {
    #[serde(rename = "fromBlock", skip_serializing_if = "Option::is_none")]
    from_block: Option<BlockNumber>,
    #[serde(rename = "toBlock", skip_serializing_if = "Option::is_none")]
    to_block: Option<BlockNumber>,
    #[serde(rename = "fromAddress", skip_serializing_if = "Option::is_none")]
    from_address: Option<Vec<H160>>,
    #[serde(rename = "toAddress", skip_serializing_if = "Option::is_none")]
    to_address: Option<Vec<H160>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<usize>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct TraceFilterBuilder {
    filter: TraceFilter,
}

impl TraceFilterBuilder {
    pub fn from_block(mut self, block: BlockNumber) -> Self {
        self.filter.from_block = Some(block);
        self
    }
    pub fn to_block(mut self, block: BlockNumber) -> Self {
        self.filter.to_block = Some(block);
        self
    }
    pub fn to_address(mut self, address: Vec<H160>) -> Self {
        self.filter.to_address = Some(address);
        self
    }
    pub fn from_address(mut self, address: Vec<H160>) -> Self {
        self.filter.from_address = Some(address);
        self
    }
    pub fn after(mut self, after: usize) -> Self {
        self.filter.after = Some(after);
        self
    }
    pub fn count(mut self, count: usize) -> Self {
        self.filter.count = Some(count);
        self
    }
    pub fn build(&self) -> TraceFilter { self.filter.clone() }
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
pub struct Trace {
    pub action: Action,
    pub result: Option<Res>,
    #[serde(rename = "traceAddress")]
    pub trace_address: Vec<usize>,
    pub subtraces: usize,
    #[serde(rename = "transactionPosition")]
    pub transaction_position: Option<usize>,
    #[serde(rename = "transactionHash")]
    pub transaction_hash: Option<H256>,
    #[serde(rename = "blockNumber")]
    pub block_number: u64,
    #[serde(rename = "blockHash")]
    pub block_hash: H256,
    #[serde(rename = "type")]
    action_type: ActionType,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Res {
    Call(CallResult),
    Create(CreateResult),
    FailedCallOrCreate(String),
    None,
}

impl Default for Res {
    fn default() -> Res { Res::None }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(untagged, rename_all = "lowercase")]
pub enum Action {
    Call(Call),
    Create(Create),
    Suicide(Suicide),
    Reward(Reward),
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionType {
    Call,
    Create,
    Suicide,
    Reward,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct CallResult {
    #[serde(rename = "gasUsed")]
    pub gas_used: U256,
    pub output: Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct CreateResult {
    #[serde(rename = "gasUsed")]
    pub gas_used: U256,
    pub code: Bytes,
    pub address: H160,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct Call {
    pub from: H160,
    pub to: H160,
    pub value: U256,
    pub gas: U256,
    pub input: Bytes,
    #[serde(rename = "callType")]
    pub call_type: CallType,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum CallType {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "call")]
    Call,
    #[serde(rename = "callcode")]
    CallCode,
    #[serde(rename = "delegatecall")]
    DelegateCall,
    #[serde(rename = "staticcall")]
    StaticCall,
}

impl Default for CallType {
    fn default() -> CallType { CallType::None }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct Create {
    pub from: H160,
    pub value: U256,
    pub gas: U256,
    pub init: Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct Suicide {
    pub address: H160,
    #[serde(rename = "refundAddress")]
    pub refund_address: H160,
    pub balance: U256,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Reward {
    pub author: H160,
    pub value: U256,
    #[serde(rename = "rewardType")]
    pub reward_type: RewardType,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum RewardType {
    #[serde(rename = "block")]
    Block,
    #[serde(rename = "uncle")]
    Uncle,
    #[serde(rename = "emptyStep")]
    EmptyStep,
    #[serde(rename = "external")]
    External,
}

// ---------------------------------------------------------------- Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_request_serializes_like_web3() {
        let req = CallRequest {
            from: None,
            to: H160::from_low_u64_be(5),
            gas: Some(21_000.into()),
            gas_price: None,
            value: Some(5_000_000.into()),
            data: Some(vec![1, 2, 3].into()),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"to":"0x0000000000000000000000000000000000000005","gas":"0x5208","value":"0x4c4b40","data":"0x010203"}"#
        );
    }

    #[test]
    fn block_number_serializes_like_web3() {
        assert_eq!(serde_json::to_string(&BlockNumber::Latest).unwrap(), "\"latest\"");
        assert_eq!(serde_json::to_string(&BlockNumber::Earliest).unwrap(), "\"earliest\"");
        assert_eq!(serde_json::to_string(&BlockNumber::Pending).unwrap(), "\"pending\"");
        assert_eq!(
            serde_json::to_string(&BlockNumber::Number(0x1234)).unwrap(),
            "\"0x1234\""
        );
    }

    #[test]
    fn bytes_round_trip() {
        let b = Bytes(vec![0xde, 0xad, 0xbe, 0xef]);
        let s = serde_json::to_string(&b).unwrap();
        assert_eq!(s, "\"0xdeadbeef\"");
        let back: Bytes = serde_json::from_str(&s).unwrap();
        assert_eq!(back, b);
    }
}
