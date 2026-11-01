//! Per-coin gas-limit overrides of an EVM coin (CRD ch. 57 §57.5 and §57.6).
//!
//! A coin entry may carry a `gas_limit` block (the nine V1 members of R57.5.2) and a `gas_limit_v2`
//! block (the `maker`, `taker` and `nft_maker` sub-objects of R57.6). Every member is optional: an
//! absent member takes the default of the chapter, an unknown member is ignored, and a member that is
//! not a non-negative integer fails coin activation with a configuration error naming the key
//! (R57.5.1, R57.6.1). A configured value is used verbatim: nothing here clamps it, compares it against
//! a ceiling or ties one member to another (R57.5.3a).
//!
//! The same module reads the `estimate_gas_mult` member (R57.5.3b) and applies it with exact integer
//! arithmetic ([`scale_gas_estimate`]).

use super::*;
use ethereum_types::U512;
use json::Map;

/// The V1 swap gas limits of one EVM coin (R57.5.2). Gas units, used verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthGasLimit {
    /// A plain value transfer of the platform coin; also the figure deducted for a maximum-value native
    /// withdrawal and the threshold reported when a gas estimate fails for insufficient funds.
    pub eth_send_coins: u64,
    /// An ERC-20 `transfer` call.
    pub eth_send_erc20: u64,
    /// The V1 HTLC payment call of the platform coin; also its sender-side trade-preimage gas.
    pub eth_payment: u64,
    /// The V1 HTLC payment call of an ERC-20 token; also the base of its sender-side trade-preimage gas.
    pub erc20_payment: u64,
    /// The V1 HTLC receiver-spend call of the platform coin; also its receiver-side trade-preimage gas.
    pub eth_receiver_spend: u64,
    /// The V1 HTLC receiver-spend call of an ERC-20 token; also its receiver-side trade-preimage gas.
    pub erc20_receiver_spend: u64,
    /// The V1 HTLC sender-refund call of the platform coin (broadcast refund and upper-bound preimage).
    pub eth_sender_refund: u64,
    /// The V1 HTLC sender-refund call of an ERC-20 token (broadcast refund and upper-bound preimage).
    pub erc20_sender_refund: u64,
    /// The flat trade-fee figure of the non-preimage trade-fee query, for the platform coin and tokens.
    pub eth_max_trade_gas: u64,
}

impl Default for EthGasLimit {
    fn default() -> Self {
        EthGasLimit {
            eth_send_coins: 21_000,
            eth_send_erc20: 210_000,
            eth_payment: 65_000,
            erc20_payment: 150_000,
            eth_receiver_spend: 65_000,
            erc20_receiver_spend: 150_000,
            eth_sender_refund: 100_000,
            erc20_sender_refund: 150_000,
            eth_max_trade_gas: 150_000,
        }
    }
}

/// A `gas_limit` / `gas_limit_v2` member (or block) that is not a non-negative integer (or an object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GasLimitConfError {
    /// The offending key, from the block name down, e.g. `gas_limit.eth_payment` or
    /// `gas_limit_v2.maker.erc20_payment`.
    pub key: String,
    /// What the key holds, as JSON text.
    pub found: String,
}

impl fmt::Display for GasLimitConfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid coin configuration: '{}' must be a non-negative integer (or, for a block, an object), found {}",
            self.key, self.found
        )
    }
}

const GAS_LIMIT_KEY: &str = "gas_limit";
const GAS_LIMIT_V2_KEY: &str = "gas_limit_v2";

/// The object stored under `key` of a coin entry. `Ok(None)` when the key is absent (or `null`), so the
/// caller can fall back to the platform coin's entry (R57.5.3).
fn block_of<'a>(conf: &'a Json, key: &str) -> Result<Option<&'a Map<String, Json>>, GasLimitConfError> {
    match conf.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Object(map)) => Ok(Some(map)),
        Some(other) => Err(GasLimitConfError {
            key: key.to_owned(),
            found: other.to_string(),
        }),
    }
}

/// The sub-object `name` of a `gas_limit_v2` block; an absent sub-object leaves every member at its default.
fn sub_block_of<'a>(
    block: &'a Map<String, Json>,
    name: &str,
) -> Result<Option<&'a Map<String, Json>>, GasLimitConfError> {
    match block.get(name) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Object(map)) => Ok(Some(map)),
        Some(other) => Err(GasLimitConfError {
            key: format!("{GAS_LIMIT_V2_KEY}.{name}"),
            found: other.to_string(),
        }),
    }
}

/// One member: the default when absent, the value verbatim when a non-negative integer, else an error naming
/// the key. `path` is the dotted path of the block that holds `member`.
fn member(block: Option<&Map<String, Json>>, path: &str, member: &str, default: u64) -> Result<u64, GasLimitConfError> {
    match block.and_then(|block| block.get(member)) {
        None => Ok(default),
        Some(value) => value.as_u64().ok_or_else(|| GasLimitConfError {
            key: format!("{path}.{member}"),
            found: value.to_string(),
        }),
    }
}

impl EthGasLimit {
    /// Reads the `gas_limit` block of a coin entry (R57.5.1). `Ok(None)` when the entry carries no such block.
    ///
    /// # Errors
    ///
    /// A member that is not a non-negative integer, or a `gas_limit` that is not an object.
    pub fn from_conf(conf: &Json) -> Result<Option<Self>, GasLimitConfError> {
        let Some(block) = block_of(conf, GAS_LIMIT_KEY)? else {
            return Ok(None);
        };
        let block = Some(block);
        let d = Self::default();
        let read = |name: &str, default: u64| member(block, GAS_LIMIT_KEY, name, default);
        Ok(Some(EthGasLimit {
            eth_send_coins: read("eth_send_coins", d.eth_send_coins)?,
            eth_send_erc20: read("eth_send_erc20", d.eth_send_erc20)?,
            eth_payment: read("eth_payment", d.eth_payment)?,
            erc20_payment: read("erc20_payment", d.erc20_payment)?,
            eth_receiver_spend: read("eth_receiver_spend", d.eth_receiver_spend)?,
            erc20_receiver_spend: read("erc20_receiver_spend", d.erc20_receiver_spend)?,
            eth_sender_refund: read("eth_sender_refund", d.eth_sender_refund)?,
            erc20_sender_refund: read("erc20_sender_refund", d.erc20_sender_refund)?,
            eth_max_trade_gas: read("eth_max_trade_gas", d.eth_max_trade_gas)?,
        }))
    }

    /// The sender-side trade-preimage gas: the payment call plus the refund call (R57.5.2, R57.3.4).
    ///
    /// This replaces the single 300000 constant of the baseline. Reloaded has no separate non-upper-bound
    /// stage (CRD §57.9 D1), so every stage reserves the refund leg, as the old constant did.
    /// Summed in 256-bit arithmetic so that a verbatim configured value can never overflow.
    pub(crate) fn sender_preimage_gas(&self, is_token: bool) -> U256 {
        if is_token {
            U256::from(self.erc20_payment) + U256::from(self.erc20_sender_refund)
        } else {
            U256::from(self.eth_payment) + U256::from(self.eth_sender_refund)
        }
    }

    /// The receiver-side trade-preimage gas (R57.5.2).
    pub(crate) fn receiver_preimage_gas(&self, is_token: bool) -> U256 {
        U256::from(if is_token {
            self.erc20_receiver_spend
        } else {
            self.eth_receiver_spend
        })
    }
}

impl EthGasLimitV2 {
    /// Reads the `gas_limit_v2` block of a coin entry (R57.6.1). `Ok(None)` when the entry carries no such
    /// block. The configuration members of `nft_maker` are `erc721_payment`, `erc1155_payment`, ...; they
    /// are stored in the `nft_*` members of [`MakerGasLimitV2`].
    ///
    /// # Errors
    ///
    /// A member that is not a non-negative integer, or a block or sub-object that is not an object.
    pub fn from_conf(conf: &Json) -> Result<Option<Self>, GasLimitConfError> {
        let Some(block) = block_of(conf, GAS_LIMIT_V2_KEY)? else {
            return Ok(None);
        };
        let d = Self::default();

        let maker = sub_block_of(block, "maker")?;
        let path = "gas_limit_v2.maker";
        let read = |name: &str, default: u64| member(maker, path, name, default);
        let maker = MakerGasLimitV2 {
            eth_payment: read("eth_payment", d.maker.eth_payment)?,
            erc20_payment: read("erc20_payment", d.maker.erc20_payment)?,
            eth_taker_spend: read("eth_taker_spend", d.maker.eth_taker_spend)?,
            erc20_taker_spend: read("erc20_taker_spend", d.maker.erc20_taker_spend)?,
            eth_maker_refund_timelock: read("eth_maker_refund_timelock", d.maker.eth_maker_refund_timelock)?,
            erc20_maker_refund_timelock: read("erc20_maker_refund_timelock", d.maker.erc20_maker_refund_timelock)?,
            eth_maker_refund_secret: read("eth_maker_refund_secret", d.maker.eth_maker_refund_secret)?,
            erc20_maker_refund_secret: read("erc20_maker_refund_secret", d.maker.erc20_maker_refund_secret)?,
            ..d.maker.clone()
        };

        let nft = sub_block_of(block, "nft_maker")?;
        let path = "gas_limit_v2.nft_maker";
        let read = |name: &str, default: u64| member(nft, path, name, default);
        let maker = MakerGasLimitV2 {
            nft_erc721_payment: read("erc721_payment", d.maker.nft_erc721_payment)?,
            nft_erc1155_payment: read("erc1155_payment", d.maker.nft_erc1155_payment)?,
            nft_erc721_taker_spend: read("erc721_taker_spend", d.maker.nft_erc721_taker_spend)?,
            nft_erc1155_taker_spend: read("erc1155_taker_spend", d.maker.nft_erc1155_taker_spend)?,
            nft_erc721_maker_refund_timelock: read(
                "erc721_maker_refund_timelock",
                d.maker.nft_erc721_maker_refund_timelock,
            )?,
            nft_erc1155_maker_refund_timelock: read(
                "erc1155_maker_refund_timelock",
                d.maker.nft_erc1155_maker_refund_timelock,
            )?,
            nft_erc721_maker_refund_secret: read("erc721_maker_refund_secret", d.maker.nft_erc721_maker_refund_secret)?,
            nft_erc1155_maker_refund_secret: read(
                "erc1155_maker_refund_secret",
                d.maker.nft_erc1155_maker_refund_secret,
            )?,
            ..maker
        };

        let taker = sub_block_of(block, "taker")?;
        let path = "gas_limit_v2.taker";
        let read = |name: &str, default: u64| member(taker, path, name, default);
        let taker = TakerGasLimitV2 {
            eth_payment: read("eth_payment", d.taker.eth_payment)?,
            erc20_payment: read("erc20_payment", d.taker.erc20_payment)?,
            eth_maker_spend: read("eth_maker_spend", d.taker.eth_maker_spend)?,
            erc20_maker_spend: read("erc20_maker_spend", d.taker.erc20_maker_spend)?,
            eth_taker_refund_timelock: read("eth_taker_refund_timelock", d.taker.eth_taker_refund_timelock)?,
            erc20_taker_refund_timelock: read("erc20_taker_refund_timelock", d.taker.erc20_taker_refund_timelock)?,
            eth_taker_refund_secret: read("eth_taker_refund_secret", d.taker.eth_taker_refund_secret)?,
            erc20_taker_refund_secret: read("erc20_taker_refund_secret", d.taker.erc20_taker_refund_secret)?,
            approve_payment: read("approve_payment", d.taker.approve_payment)?,
        };

        Ok(Some(EthGasLimitV2 { maker, taker }))
    }
}

/// The gas-limit blocks of a coin: the first entry of `confs` that carries a block decides it, per block, as a
/// whole (R57.5.3). `confs` lists the coin's own entry first and, for a token, its platform coin's entry
/// second; for a platform coin only its own entry is consulted. A block no entry carries takes the defaults.
///
/// # Errors
///
/// [`GasLimitConfError`] naming the offending key, whichever entry holds it. The platform entry's block is
/// parsed only when the token's own entry carries none.
pub fn gas_limits_from_confs(confs: &[&Json]) -> Result<(EthGasLimit, EthGasLimitV2), GasLimitConfError> {
    let mut v1 = None;
    let mut v2 = None;
    for conf in confs {
        if v1.is_none() {
            v1 = EthGasLimit::from_conf(conf)?;
        }
        if v2.is_none() {
            v2 = EthGasLimitV2::from_conf(conf)?;
        }
    }
    Ok((v1.unwrap_or_default(), v2.unwrap_or_default()))
}

/// The key of the per-coin gas-estimate multiplier (CRD R57.5.3b).
const ESTIMATE_GAS_MULT_KEY: &str = "estimate_gas_mult";

/// An `estimate_gas_mult` member that is not a JSON number (CRD R57.5.3b item 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstimateGasMultConfError {
    /// What the member holds, as JSON text.
    pub found: String,
}

impl fmt::Display for EstimateGasMultConfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid coin configuration: '{ESTIMATE_GAS_MULT_KEY}' must be a number, found {}",
            self.found
        )
    }
}

/// The `estimate_gas_mult` of a coin: the first entry of `confs` that carries the member decides it (CRD
/// R57.5.3b item 1). `confs` lists the coin's own entry first and, for a token, its platform coin's entry
/// second; for a platform coin only its own entry is consulted. `Ok(None)` when no entry carries the member,
/// which means no scaling. The value is stored as parsed: no range is checked here or at use (item 3).
///
/// # Errors
///
/// [`EstimateGasMultConfError`] when a consulted entry carries the member with a value that is not a JSON
/// number (a string, boolean, `null`, array or object). The platform entry is consulted only when the token's
/// own entry does not carry the member.
pub fn estimate_gas_mult_from_confs(confs: &[&Json]) -> Result<Option<f64>, EstimateGasMultConfError> {
    for conf in confs {
        let Some(value) = conf.get(ESTIMATE_GAS_MULT_KEY) else {
            continue;
        };
        return match value {
            Json::Number(number) => Ok(number.as_f64()),
            other => Err(EstimateGasMultConfError {
                found: other.to_string(),
            }),
        };
    }
    Ok(None)
}

/// Why a gas estimate could not be scaled (CRD R57.5.3b item 3). Both outcomes fail the transaction at send
/// time, never at activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaleGasError {
    /// The multiplier is negative: the product cannot be represented as an unsigned gas quantity.
    NegativeMultiplier,
    /// The product does not fit a 256-bit gas quantity.
    Overflow,
}

impl fmt::Display for ScaleGasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScaleGasError::NegativeMultiplier => write!(
                f,
                "'{ESTIMATE_GAS_MULT_KEY}' is negative: the scaled gas limit cannot be represented as an unsigned quantity"
            ),
            ScaleGasError::Overflow => write!(f, "the gas limit scaled by '{ESTIMATE_GAS_MULT_KEY}' overflows"),
        }
    }
}

/// Scales a node's gas `estimate` by the coin's `estimate_gas_mult` (CRD R57.5.3b items 2-4).
///
/// `None` and a non-finite multiplier (not-a-number, positive or negative infinity) leave the estimate as
/// returned; zero yields zero; a negative multiplier is an error. Otherwise the result is the exact product of
/// the whole-number estimate and the exact binary value of the parsed double, truncated toward zero. The
/// double is split into its integer mantissa and power-of-two exponent, so the product is formed in integer
/// arithmetic and no floating-point rounding takes place: `0.7` applied to `10` gives `6`, because the
/// double nearest `0.7` lies just below it.
pub(crate) fn scale_gas_estimate(estimate: U256, mult: Option<f64>) -> Result<U256, ScaleGasError> {
    let Some(mult) = mult.filter(|m| m.is_finite()) else {
        return Ok(estimate);
    };
    if mult == 0.0 {
        return Ok(U256::zero());
    }
    if mult < 0.0 {
        return Err(ScaleGasError::NegativeMultiplier);
    }
    // mult = mantissa * 2^exponent exactly (positive, finite). A normal double has an implicit leading bit.
    let bits = mult.to_bits();
    let biased_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mantissa, exponent) = if biased_exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), biased_exponent - 1075)
    };
    // 256-bit estimate times a 53-bit mantissa fits 512 bits, so this product cannot overflow.
    let product = U512::from(estimate) * U512::from(mantissa);
    if product.is_zero() {
        return Ok(U256::zero());
    }
    let scaled = if exponent >= 0 {
        let shift = exponent as usize;
        if product.bits() + shift > 256 {
            return Err(ScaleGasError::Overflow);
        }
        product << shift
    } else {
        let shift = exponent.unsigned_abs() as usize;
        if shift >= 512 {
            U512::zero()
        } else {
            product >> shift
        }
    };
    U256::try_from(scaled).map_err(|_| ScaleGasError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn v1_defaults_are_the_chapter_table() {
        let d = EthGasLimit::default();
        assert_eq!(d.eth_send_coins, 21_000);
        assert_eq!(d.eth_send_erc20, 210_000);
        assert_eq!(d.eth_payment, 65_000);
        assert_eq!(d.erc20_payment, 150_000);
        assert_eq!(d.eth_receiver_spend, 65_000);
        assert_eq!(d.erc20_receiver_spend, 150_000);
        assert_eq!(d.eth_sender_refund, 100_000);
        assert_eq!(d.erc20_sender_refund, 150_000);
        assert_eq!(d.eth_max_trade_gas, 150_000);
    }

    #[test]
    fn v2_defaults_are_the_chapter_table() {
        let d = EthGasLimitV2::default();
        let m = &d.maker;
        assert_eq!(
            [
                m.eth_payment,
                m.erc20_payment,
                m.eth_taker_spend,
                m.erc20_taker_spend,
                m.eth_maker_refund_timelock,
                m.erc20_maker_refund_timelock,
                m.eth_maker_refund_secret,
                m.erc20_maker_refund_secret,
            ],
            [65_000, 150_000, 100_000, 150_000, 90_000, 100_000, 90_000, 100_000]
        );
        let t = &d.taker;
        assert_eq!(
            [
                t.eth_payment,
                t.erc20_payment,
                t.eth_maker_spend,
                t.erc20_maker_spend,
                t.eth_taker_refund_timelock,
                t.erc20_taker_refund_timelock,
                t.eth_taker_refund_secret,
                t.erc20_taker_refund_secret,
                t.approve_payment,
            ],
            [65_000, 150_000, 100_000, 115_000, 90_000, 100_000, 90_000, 100_000, 50_000]
        );
        assert_eq!(
            [
                m.nft_erc721_payment,
                m.nft_erc1155_payment,
                m.nft_erc721_taker_spend,
                m.nft_erc1155_taker_spend,
                m.nft_erc721_maker_refund_timelock,
                m.nft_erc1155_maker_refund_timelock,
                m.nft_erc721_maker_refund_secret,
                m.nft_erc1155_maker_refund_secret,
            ],
            [130_000, 130_000, 100_000, 100_000, 100_000, 100_000, 100_000, 100_000]
        );
    }

    #[test]
    fn an_absent_block_is_none_and_resolves_to_the_defaults() {
        let conf = json!({ "coin": "ETH" });
        assert_eq!(EthGasLimit::from_conf(&conf), Ok(None));
        assert_eq!(EthGasLimitV2::from_conf(&conf), Ok(None));
        let (v1, v2) = gas_limits_from_confs(&[&conf]).unwrap();
        assert_eq!(v1, EthGasLimit::default());
        assert_eq!(v2, EthGasLimitV2::default());
        // A null block is an absent block.
        let conf = json!({ "gas_limit": null, "gas_limit_v2": null });
        assert_eq!(EthGasLimit::from_conf(&conf), Ok(None));
        assert_eq!(EthGasLimitV2::from_conf(&conf), Ok(None));
    }

    #[test]
    fn each_v1_member_is_read_and_the_others_keep_their_default() {
        let keys = [
            "eth_send_coins",
            "eth_send_erc20",
            "eth_payment",
            "erc20_payment",
            "eth_receiver_spend",
            "erc20_receiver_spend",
            "eth_sender_refund",
            "erc20_sender_refund",
            "eth_max_trade_gas",
        ];
        let default = EthGasLimit::default();
        for key in keys {
            let conf = json!({ "gas_limit": { key: 777_777 } });
            let got = EthGasLimit::from_conf(&conf).unwrap().unwrap();
            let fields = |g: &EthGasLimit| {
                [
                    g.eth_send_coins,
                    g.eth_send_erc20,
                    g.eth_payment,
                    g.erc20_payment,
                    g.eth_receiver_spend,
                    g.erc20_receiver_spend,
                    g.eth_sender_refund,
                    g.erc20_sender_refund,
                    g.eth_max_trade_gas,
                ]
            };
            let index = keys.iter().position(|k| *k == key).unwrap();
            let mut expected = fields(&default);
            expected[index] = 777_777;
            assert_eq!(fields(&got), expected, "member {key}");
        }
    }

    #[test]
    fn unknown_members_are_ignored() {
        let conf = json!({ "gas_limit": { "eth_payment": 5, "no_such_member": "x" },
                           "gas_limit_v2": { "maker": { "bogus": -1 }, "extra": 1 } });
        assert_eq!(EthGasLimit::from_conf(&conf).unwrap().unwrap().eth_payment, 5);
        assert!(EthGasLimitV2::from_conf(&conf).unwrap().is_some());
    }

    #[test]
    fn every_v2_config_member_reaches_its_field() {
        let conf = json!({ "gas_limit_v2": {
            "maker": {
                "eth_payment": 1, "erc20_payment": 2, "eth_taker_spend": 3, "erc20_taker_spend": 4,
                "eth_maker_refund_timelock": 5, "erc20_maker_refund_timelock": 6,
                "eth_maker_refund_secret": 7, "erc20_maker_refund_secret": 8
            },
            "taker": {
                "eth_payment": 11, "erc20_payment": 12, "eth_maker_spend": 13, "erc20_maker_spend": 14,
                "eth_taker_refund_timelock": 15, "erc20_taker_refund_timelock": 16,
                "eth_taker_refund_secret": 17, "erc20_taker_refund_secret": 18, "approve_payment": 19
            },
            "nft_maker": {
                "erc721_payment": 21, "erc1155_payment": 22, "erc721_taker_spend": 23, "erc1155_taker_spend": 24,
                "erc721_maker_refund_timelock": 25, "erc1155_maker_refund_timelock": 26,
                "erc721_maker_refund_secret": 27, "erc1155_maker_refund_secret": 28
            }
        }});
        let g = EthGasLimitV2::from_conf(&conf).unwrap().unwrap();
        let m = &g.maker;
        assert_eq!(
            [
                m.eth_payment,
                m.erc20_payment,
                m.eth_taker_spend,
                m.erc20_taker_spend,
                m.eth_maker_refund_timelock,
                m.erc20_maker_refund_timelock,
                m.eth_maker_refund_secret,
                m.erc20_maker_refund_secret
            ],
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        let t = &g.taker;
        assert_eq!(
            [
                t.eth_payment,
                t.erc20_payment,
                t.eth_maker_spend,
                t.erc20_maker_spend,
                t.eth_taker_refund_timelock,
                t.erc20_taker_refund_timelock,
                t.eth_taker_refund_secret,
                t.erc20_taker_refund_secret,
                t.approve_payment
            ],
            [11, 12, 13, 14, 15, 16, 17, 18, 19]
        );
        assert_eq!(
            [
                m.nft_erc721_payment,
                m.nft_erc1155_payment,
                m.nft_erc721_taker_spend,
                m.nft_erc1155_taker_spend,
                m.nft_erc721_maker_refund_timelock,
                m.nft_erc1155_maker_refund_timelock,
                m.nft_erc721_maker_refund_secret,
                m.nft_erc1155_maker_refund_secret
            ],
            [21, 22, 23, 24, 25, 26, 27, 28]
        );
    }

    #[test]
    fn a_partial_v2_block_keeps_the_defaults_of_the_absent_members() {
        let conf = json!({ "gas_limit_v2": { "taker": { "approve_payment": 70_000 } } });
        let g = EthGasLimitV2::from_conf(&conf).unwrap().unwrap();
        let d = EthGasLimitV2::default();
        assert_eq!(g.taker.approve_payment, 70_000);
        assert_eq!(g.taker.erc20_maker_spend, d.taker.erc20_maker_spend);
        assert_eq!(g.maker.eth_payment, d.maker.eth_payment);
        assert_eq!(g.maker.nft_erc1155_payment, d.maker.nft_erc1155_payment);
    }

    #[test]
    fn a_configured_value_is_used_verbatim_with_no_clamp_and_no_cross_member_rule() {
        // Far above every default, including u64::MAX, and `eth_max_trade_gas` below every other member.
        let conf = json!({ "gas_limit": { "eth_max_trade_gas": 1, "eth_payment": 9_000_000_000u64,
                                          "erc20_sender_refund": u64::MAX },
                           "gas_limit_v2": { "maker": { "erc20_payment": 30_000_000 },
                                             "nft_maker": { "erc1155_payment": u64::MAX } } });
        let (v1, v2) = gas_limits_from_confs(&[&conf]).unwrap();
        assert_eq!(v1.eth_max_trade_gas, 1);
        assert_eq!(v1.eth_payment, 9_000_000_000);
        assert_eq!(v1.erc20_sender_refund, u64::MAX);
        assert_eq!(v1.erc20_payment, EthGasLimit::default().erc20_payment);
        assert_eq!(v2.maker.erc20_payment, 30_000_000);
        assert_eq!(v2.maker.nft_erc1155_payment, u64::MAX);
        // The trade-preimage sums cannot overflow either.
        assert_eq!(
            v1.sender_preimage_gas(true),
            U256::from(150_000u64) + U256::from(u64::MAX)
        );
    }

    #[test]
    fn a_member_that_is_not_a_non_negative_integer_names_its_key() {
        let cases = [
            (json!({ "gas_limit": { "eth_payment": -1 } }), "gas_limit.eth_payment"),
            (
                json!({ "gas_limit": { "erc20_payment": 1.5 } }),
                "gas_limit.erc20_payment",
            ),
            (
                json!({ "gas_limit": { "eth_max_trade_gas": "150000" } }),
                "gas_limit.eth_max_trade_gas",
            ),
            (
                json!({ "gas_limit": { "eth_send_coins": null } }),
                "gas_limit.eth_send_coins",
            ),
            (
                json!({ "gas_limit": { "eth_send_erc20": 210000.0 } }),
                "gas_limit.eth_send_erc20",
            ),
            (json!({ "gas_limit": 5 }), "gas_limit"),
        ];
        for (conf, key) in cases {
            let err = EthGasLimit::from_conf(&conf).unwrap_err();
            assert_eq!(err.key, key);
            assert!(err.to_string().contains(key), "{err}");
        }
        let cases = [
            (
                json!({ "gas_limit_v2": { "maker": { "eth_taker_spend": -5 } } }),
                "gas_limit_v2.maker.eth_taker_spend",
            ),
            (
                json!({ "gas_limit_v2": { "taker": { "approve_payment": "50000" } } }),
                "gas_limit_v2.taker.approve_payment",
            ),
            (
                json!({ "gas_limit_v2": { "nft_maker": { "erc721_payment": 1.25 } } }),
                "gas_limit_v2.nft_maker.erc721_payment",
            ),
            (json!({ "gas_limit_v2": { "maker": 7 } }), "gas_limit_v2.maker"),
            (json!({ "gas_limit_v2": [] }), "gas_limit_v2"),
        ];
        for (conf, key) in cases {
            let err = EthGasLimitV2::from_conf(&conf).unwrap_err();
            assert_eq!(err.key, key);
            assert!(err.to_string().contains(key), "{err}");
        }
    }

    #[test]
    fn a_token_block_wins_whole_and_a_silent_token_inherits_the_platform_block() {
        let platform = json!({ "gas_limit": { "eth_payment": 111, "erc20_payment": 222 },
                               "gas_limit_v2": { "taker": { "approve_payment": 333 } } });
        let own = json!({ "gas_limit": { "erc20_payment": 444 } });
        let silent = json!({ "coin": "JST" });

        // The token's own `gas_limit` replaces the platform's block as a whole: the member the token omits
        // is the default, not the platform's value. `gas_limit_v2` is inherited independently.
        let (v1, v2) = gas_limits_from_confs(&[&own, &platform]).unwrap();
        assert_eq!(v1.erc20_payment, 444);
        assert_eq!(v1.eth_payment, EthGasLimit::default().eth_payment);
        assert_eq!(v2.taker.approve_payment, 333);

        let (v1, v2) = gas_limits_from_confs(&[&silent, &platform]).unwrap();
        assert_eq!(v1.eth_payment, 111);
        assert_eq!(v1.erc20_payment, 222);
        assert_eq!(v2.taker.approve_payment, 333);

        // A platform coin consults only its own entry.
        let (v1, _) = gas_limits_from_confs(&[&silent]).unwrap();
        assert_eq!(v1, EthGasLimit::default());
    }

    #[test]
    fn the_platform_block_is_not_parsed_when_the_token_carries_its_own() {
        let platform = json!({ "gas_limit": { "eth_payment": -1 } });
        let own = json!({ "gas_limit": { "eth_payment": 9 } });
        let (v1, _) = gas_limits_from_confs(&[&own, &platform]).unwrap();
        assert_eq!(v1.eth_payment, 9);
        let silent = json!({});
        assert_eq!(
            gas_limits_from_confs(&[&silent, &platform]).unwrap_err().key,
            "gas_limit.eth_payment"
        );
    }

    #[test]
    fn scale_follows_r5753b_item_4_with_exact_truncation() {
        let scale = |e: u64, m: f64| scale_gas_estimate(U256::from(e), Some(m));
        // The double nearest 1.3 is slightly above it: the exact product is just above 65000.
        assert_eq!(scale(50_000, 1.3), Ok(U256::from(65_000u64)));
        // The double nearest 0.7 is slightly below it: the exact product is just below 7.
        assert_eq!(scale(10, 0.7), Ok(U256::from(6u64)));
        assert_eq!(scale(12_345, 2.0), Ok(U256::from(24_690u64)));
        assert_eq!(scale(12_345, 1.0), Ok(U256::from(12_345u64)));
        assert_eq!(scale(12_345, 0.0), Ok(U256::zero()));
        assert_eq!(scale(12_345, -0.0), Ok(U256::zero()));
        assert_eq!(scale(3, 1.5), Ok(U256::from(4u64)));
        assert_eq!(scale(0, 1.3), Ok(U256::zero()));
        // Below one: truncation to zero, and a subnormal multiplier.
        assert_eq!(scale(5, 0.1), Ok(U256::zero()));
        assert_eq!(scale(u64::MAX, f64::MIN_POSITIVE / 2.0), Ok(U256::zero()));
        assert_eq!(scale(u64::MAX, 5e-324), Ok(U256::zero()));
    }

    #[test]
    fn scale_absent_or_non_finite_multiplier_is_identity() {
        let e = U256::from(54_321u64);
        assert_eq!(scale_gas_estimate(e, None), Ok(e));
        assert_eq!(scale_gas_estimate(e, Some(f64::NAN)), Ok(e));
        assert_eq!(scale_gas_estimate(e, Some(f64::INFINITY)), Ok(e));
        assert_eq!(scale_gas_estimate(e, Some(f64::NEG_INFINITY)), Ok(e));
    }

    #[test]
    fn scale_negative_multiplier_is_a_typed_error() {
        let e = U256::from(54_321u64);
        assert_eq!(
            scale_gas_estimate(e, Some(-1.0)),
            Err(ScaleGasError::NegativeMultiplier)
        );
        assert_eq!(
            scale_gas_estimate(e, Some(-1e-300)),
            Err(ScaleGasError::NegativeMultiplier)
        );
        assert_eq!(
            scale_gas_estimate(e, Some(f64::MIN)),
            Err(ScaleGasError::NegativeMultiplier)
        );
    }

    #[test]
    fn scale_of_a_very_large_estimate_does_not_overflow_or_panic() {
        // u64::MAX x 1.3: the exact product of 18446744073709551615 and the double 1.3 truncated.
        let exact = U256::from(u64::MAX) * U256::from(5_854_679_515_581_645u64) / U256::from(1u64 << 52);
        assert_eq!(scale_gas_estimate(U256::from(u64::MAX), Some(1.3)), Ok(exact));
        // A 256-bit estimate times 1.0 and 2.0 (the latter overflows by one bit at the top).
        let big = U256::MAX >> 1;
        assert_eq!(scale_gas_estimate(big, Some(1.0)), Ok(big));
        assert_eq!(scale_gas_estimate(big, Some(2.0)), Ok(big << 1));
        assert_eq!(scale_gas_estimate(U256::MAX, Some(2.0)), Err(ScaleGasError::Overflow));
        assert_eq!(scale_gas_estimate(U256::MAX, Some(1.3)), Err(ScaleGasError::Overflow));
        assert_eq!(
            scale_gas_estimate(U256::from(1u64), Some(f64::MAX)),
            Err(ScaleGasError::Overflow)
        );
        // 2^255 x 0.5 is exact.
        assert_eq!(
            scale_gas_estimate(U256::one() << 255, Some(0.5)),
            Ok(U256::one() << 254)
        );
        assert_eq!(scale_gas_estimate(U256::MAX, Some(0.5)), Ok(U256::MAX >> 1));
    }

    #[test]
    fn estimate_gas_mult_is_read_from_the_first_entry_that_carries_it() {
        let own = json!({"estimate_gas_mult": 2.0});
        let platform = json!({"estimate_gas_mult": 1.3});
        let silent = json!({"coin": "JST"});
        assert_eq!(estimate_gas_mult_from_confs(&[&own, &platform]), Ok(Some(2.0)));
        assert_eq!(estimate_gas_mult_from_confs(&[&silent, &platform]), Ok(Some(1.3)));
        assert_eq!(estimate_gas_mult_from_confs(&[&silent, &silent]), Ok(None));
        assert_eq!(estimate_gas_mult_from_confs(&[&platform]), Ok(Some(1.3)));
        // An integer is a JSON number too, and no range is checked.
        assert_eq!(
            estimate_gas_mult_from_confs(&[&json!({"estimate_gas_mult": 3})]),
            Ok(Some(3.0))
        );
        assert_eq!(
            estimate_gas_mult_from_confs(&[&json!({"estimate_gas_mult": -7.5})]),
            Ok(Some(-7.5))
        );
        assert_eq!(
            estimate_gas_mult_from_confs(&[&json!({"estimate_gas_mult": 0})]),
            Ok(Some(0.0))
        );
    }

    #[test]
    fn estimate_gas_mult_that_is_not_a_number_is_an_error_naming_the_key() {
        for bad in [json!("1.3"), json!(true), json!(null), json!([1.3]), json!({"a": 1})] {
            let conf = json!({ "estimate_gas_mult": bad });
            let err = estimate_gas_mult_from_confs(&[&conf]).unwrap_err();
            assert!(err.to_string().contains("estimate_gas_mult"), "{err}");
            // A malformed token member is not rescued by a valid platform member.
            assert!(estimate_gas_mult_from_confs(&[&conf, &json!({"estimate_gas_mult": 1.3})]).is_err());
        }
    }
}
