//! High-precision numeric types for the Komodo DeFi Framework.
//!
//! Provides [`MmNumber`], a rational-number wrapper suitable for financial
//! calculations where floating-point rounding is unacceptable. Numbers are
//! stored internally as `BigRational` (arbitrary-precision numerator and
//! denominator) and can be serialized to/from decimal strings, rational
//! pairs, or [`Fraction`] objects.

mod decimal_memo;

pub use decimal_memo::{DecimalMemo, RenderedDecimal};

use core::ops::{Add, AddAssign, Div, Mul, Sub};
use num_traits::{Pow, Zero};
use serde::de;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

pub use bigdecimal::BigDecimal;
pub use num_bigint::{self, BigInt, BigUint, ParseBigIntError, Sign};
pub use num_rational::BigRational;
pub use paste::paste;

// ---------------------------------------------------------------------------
// BigIntStr — serializable BigInt wrapper (string representation)
// ---------------------------------------------------------------------------

/// Wrapper around [`BigInt`] that serializes to and from a string.
///
/// Used internally by [`Fraction`] to transport arbitrary-precision integers
/// across JSON boundaries without precision loss.
#[derive(Clone, Eq, PartialEq)]
pub struct BigIntStr(BigInt);

impl fmt::Debug for BigIntStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0.to_string()) }
}

impl BigIntStr {
    pub fn inner(&self) -> &BigInt { &self.0 }
}

impl From<BigInt> for BigIntStr {
    fn from(n: BigInt) -> Self { Self(n) }
}

impl From<BigIntStr> for BigInt {
    fn from(s: BigIntStr) -> Self { s.0 }
}

impl Serialize for BigIntStr {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> { ser.serialize_str(&self.0.to_string()) }
}

impl<'de> Deserialize<'de> for BigIntStr {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Vis;
        impl<'de> serde::de::Visitor<'de> for Vis {
            type Value = BigIntStr;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "a string representing an integer") }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                let n: BigInt = v
                    .parse()
                    .map_err(|e| serde::de::Error::custom(format!("invalid BigInt string '{}': {}", v, e)))?;
                Ok(BigIntStr(n))
            }
        }
        de.deserialize_str(Vis)
    }
}

// ---------------------------------------------------------------------------
// Fraction — human-readable rational representation
// ---------------------------------------------------------------------------

/// A rational number expressed as a numerator/denominator pair.
///
/// Suitable for JSON transport where decimal precision is ambiguous.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Fraction {
    numer: BigIntStr,
    denom: BigIntStr,
}

impl Fraction {
    pub fn numer(&self) -> &BigInt { self.numer.inner() }
    pub fn denom(&self) -> &BigInt { self.denom.inner() }
}

impl From<BigRational> for Fraction {
    fn from(r: BigRational) -> Self {
        let (n, d) = r.into();
        Self {
            numer: n.into(),
            denom: d.into(),
        }
    }
}

impl From<Fraction> for BigRational {
    fn from(f: Fraction) -> Self { BigRational::new(f.numer.into(), f.denom.into()) }
}

impl From<BigDecimal> for Fraction {
    fn from(d: BigDecimal) -> Self { dec_to_rational(&d).into() }
}

impl<'de> Deserialize<'de> for Fraction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            numer: BigIntStr,
            denom: BigIntStr,
        }
        let raw: Raw = Deserialize::deserialize(deserializer)?;
        if raw.denom.inner() == &BigInt::from(0) {
            return Err(de::Error::custom("denom can not be 0"));
        }
        Ok(Self {
            numer: raw.numer,
            denom: raw.denom,
        })
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

/// Convert a [`BigDecimal`] to an equivalent [`BigRational`].
pub fn dec_to_rational(d: &BigDecimal) -> BigRational {
    let (mantissa, scale) = d.as_bigint_and_exponent();
    let ten = BigInt::from(10);
    if scale >= 0 {
        BigRational::new(mantissa, ten.pow(scale as u64))
    } else {
        BigRational::new(mantissa * ten.pow((-scale) as u64), 1.into())
    }
}

/// Convert a [`BigRational`] to a [`BigDecimal`] (may lose repeating-fraction precision).
pub fn rational_to_dec(r: &BigRational) -> BigDecimal {
    BigDecimal::from(r.numer().clone()) / BigDecimal::from(r.denom().clone())
}

// Keep the old names as aliases for backward compatibility with code that uses them.
pub use dec_to_rational as from_dec_to_ratio;
pub use rational_to_dec as from_ratio_to_dec;

static GLOBAL_DECIMAL_MEMO: LazyLock<DecimalMemo> =
    LazyLock::new(|| DecimalMemo::with_capacity(decimal_memo::GLOBAL_DECIMAL_MEMO_CAPACITY));

// ---------------------------------------------------------------------------
// MmNumber — the core high-precision numeric type
// ---------------------------------------------------------------------------

/// Arbitrary-precision rational number for financial arithmetic.
///
/// Wraps [`BigRational`] and provides convenient conversions from decimal
/// strings, fractions, and primitive integers. All arithmetic is exact.
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord, Serialize)]
pub struct MmNumber(pub(crate) BigRational);

/// Deserializes from a decimal string, a rational pair `[numer, denom]`, or a
/// [`Fraction`] object `{"numer":"…","denom":"…"}`.
///
/// **Note:** the deserializer relies on JSON `RawValue`; non-JSON formats
/// should use [`BigRational`] directly.
impl<'de> Deserialize<'de> for MmNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Box<RawValue> = Deserialize::deserialize(deserializer)?;

        if let Ok(d) = BigDecimal::from_str(raw.get().trim_matches('"')) {
            return Ok(Self(dec_to_rational(&d)));
        }
        if let Ok(r) = serde_json::from_str::<BigRational>(raw.get()) {
            return Ok(Self(r));
        }
        if let Ok(f) = serde_json::from_str::<Fraction>(raw.get()) {
            return Ok(Self(f.into()));
        }
        Err(de::Error::custom(format!(
            "Could not deserialize any variant of MmNumber from {}",
            raw.get()
        )))
    }
}

impl fmt::Display for MmNumber {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "{}", rational_to_dec(&self.0)) }
}

impl Default for MmNumber {
    fn default() -> Self { BigRational::from_integer(0.into()).into() }
}

// -- From conversions -------------------------------------------------------

impl From<BigDecimal> for MmNumber {
    fn from(d: BigDecimal) -> Self { dec_to_rational(&d).into() }
}

impl From<BigRational> for MmNumber {
    fn from(r: BigRational) -> Self { Self(r) }
}

impl From<Fraction> for MmNumber {
    fn from(f: Fraction) -> Self { Self(f.into()) }
}

impl From<u64> for MmNumber {
    fn from(n: u64) -> Self { BigRational::from_integer(n.into()).into() }
}

impl From<i32> for MmNumber {
    fn from(n: i32) -> Self { Self(BigRational::from_integer(n.into())) }
}

impl From<(u64, u64)> for MmNumber {
    fn from((n, d): (u64, u64)) -> Self { BigRational::new(n.into(), d.into()).into() }
}

/// Convenience for tests — panics on invalid input.
impl From<&'static str> for MmNumber {
    fn from(s: &'static str) -> Self {
        let d: BigDecimal = s.parse().expect("string should be a valid decimal");
        d.into()
    }
}

// -- Into conversions -------------------------------------------------------

impl From<MmNumber> for BigDecimal {
    fn from(n: MmNumber) -> Self { rational_to_dec(&n.0) }
}

impl From<MmNumber> for BigRational {
    fn from(n: MmNumber) -> Self { n.0 }
}

// -- Arithmetic operators ---------------------------------------------------

impl Mul for MmNumber {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self { (self.0 * rhs.0).into() }
}

impl Mul for &MmNumber {
    type Output = MmNumber;
    fn mul(self, rhs: Self) -> MmNumber { MmNumber(&self.0 * &rhs.0) }
}

impl Add for MmNumber {
    type Output = Self;
    fn add(self, rhs: Self) -> Self { (self.0 + rhs.0).into() }
}

impl Add for &MmNumber {
    type Output = MmNumber;
    fn add(self, rhs: Self) -> MmNumber { MmNumber(&self.0 + &rhs.0) }
}

impl AddAssign for MmNumber {
    fn add_assign(&mut self, rhs: Self) { self.0 += rhs.0; }
}

impl AddAssign<&MmNumber> for MmNumber {
    fn add_assign(&mut self, rhs: &Self) { self.0 += &rhs.0; }
}

impl Sub for MmNumber {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self { (self.0 - rhs.0).into() }
}

impl Sub for &MmNumber {
    type Output = MmNumber;
    fn sub(self, rhs: Self) -> MmNumber { (&self.0 - &rhs.0).into() }
}

impl Div for MmNumber {
    type Output = Self;
    fn div(self, rhs: Self) -> Self { (self.0 / rhs.0).into() }
}

impl Div for &MmNumber {
    type Output = MmNumber;
    fn div(self, rhs: &MmNumber) -> MmNumber { MmNumber(&self.0 / &rhs.0) }
}

// -- Comparison with BigDecimal ---------------------------------------------

impl PartialOrd<BigDecimal> for MmNumber {
    fn partial_cmp(&self, other: &BigDecimal) -> Option<std::cmp::Ordering> {
        Some(self.0.cmp(&dec_to_rational(other)))
    }
}

impl PartialEq<BigDecimal> for MmNumber {
    fn eq(&self, rhs: &BigDecimal) -> bool { self.0 == dec_to_rational(rhs) }
}

// -- Methods ----------------------------------------------------------------

impl MmNumber {
    /// Fractional representation (numerator + denominator as strings).
    pub fn to_fraction(&self) -> Fraction {
        Fraction {
            numer: self.0.numer().clone().into(),
            denom: self.0.denom().clone().into(),
        }
    }

    /// Clone the underlying rational.
    pub fn to_ratio(&self) -> BigRational { self.0.clone() }

    /// Decimal approximation.
    pub fn to_decimal(&self) -> BigDecimal { rational_to_dec(&self.0) }

    /// The same decimal as [`MmNumber::to_decimal`] with its printed form, taken from a process-wide bounded
    /// memo keyed by the exact numerator and denominator when this rational was converted before.
    ///
    /// Meant for RPC rendering of long-lived values (own orders) that are converted again on every call.
    /// Arithmetic paths keep using [`MmNumber::to_decimal`], which touches no shared state.
    pub fn to_rendered_decimal(&self) -> RenderedDecimal { GLOBAL_DECIMAL_MEMO.get_or_compute(&self.0) }

    /// The underlying rational, borrowed.
    pub fn as_ratio(&self) -> &BigRational { &self.0 }

    pub fn numer(&self) -> &BigInt { self.0.numer() }
    pub fn denom(&self) -> &BigInt { self.0.denom() }
    pub fn is_zero(&self) -> bool { self.0.is_zero() }
}

// ---------------------------------------------------------------------------
// construct_detailed! — generates triple-representation structs
// ---------------------------------------------------------------------------

/// Generates a struct with decimal, fraction and rational fields for a number.
///
/// ```ignore
/// mm2_number::construct_detailed!(MyVolume, volume);
/// // expands to:
/// // struct MyVolume { volume: BigDecimal, volume_fraction: Fraction, volume_rat: BigRational }
/// // impl From<MmNumber> for MyVolume { … }
/// ```
///
/// Note: crates that import `common` via `#[macro_use]` use its own
/// forwarding version of this macro.  Direct consumers of `mm2_number`
/// can use this macro directly.
#[macro_export]
macro_rules! construct_detailed {
    ($name:ident, $field:ident) => {
        $crate::paste! {
            #[derive(Clone, Debug, Serialize)]
            pub struct $name {
                $field: $crate::BigDecimal,
                [<$field _fraction>]: $crate::Fraction,
                [<$field _rat>]: $crate::BigRational,
            }

            impl From<$crate::MmNumber> for $name {
                fn from(num: $crate::MmNumber) -> Self {
                    Self {
                        $field: num.to_decimal(),
                        [<$field _fraction>]: num.to_fraction(),
                        [<$field _rat>]: num.to_ratio(),
                    }
                }
            }

            #[allow(dead_code)]
            impl $name {
                pub fn as_ratio(&self) -> &$crate::BigRational {
                    &self.[<$field _rat>]
                }
            }
        }
    };
}

// ---------------------------------------------------------------------------
// MmNumberMultiRepr — all three representations at once
// ---------------------------------------------------------------------------

/// All available representations of an [`MmNumber`] in one struct.
#[derive(Debug, Serialize)]
pub struct MmNumberMultiRepr {
    pub decimal: BigDecimal,
    pub rational: BigRational,
    pub fraction: Fraction,
}

impl From<MmNumber> for MmNumberMultiRepr {
    fn from(n: MmNumber) -> Self {
        Self {
            decimal: n.to_decimal(),
            fraction: n.to_fraction(),
            rational: n.0,
        }
    }
}

impl From<BigRational> for MmNumberMultiRepr {
    fn from(r: BigRational) -> Self {
        Self {
            decimal: rational_to_dec(&r),
            fraction: r.clone().into(),
            rational: r,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json as json;

    #[test]
    fn decimal_to_rational_roundtrip() {
        let d: BigDecimal = "11.00000000000000000000000000000000000000".parse().unwrap();
        let r = dec_to_rational(&d);
        assert_eq!(*r.numer(), 11.into());
        assert_eq!(*r.denom(), 1.into());

        let d: BigDecimal = "0.00000001".parse().unwrap();
        let r = dec_to_rational(&d);
        assert_eq!(*r.numer(), 1.into());
        assert_eq!(*r.denom(), BigInt::from(100_000_000));

        let d: BigDecimal = 1.into();
        let r = dec_to_rational(&d);
        assert_eq!(*r.numer(), 1.into());
        assert_eq!(*r.denom(), 1.into());
    }

    #[test]
    fn deserialize_from_decimal_string() {
        let cases = vec![
            "1.0",
            "0.5",
            "50",
            "1e-3",
            "1e12",
            "0.3333333333333333",
            "3.141592653589793",
            "12.0010",
        ];
        for s in cases {
            let d: BigDecimal = BigDecimal::from_str(s).unwrap();
            let expected: MmNumber = dec_to_rational(&d).into();
            let actual: MmNumber = json::from_str(s).unwrap();
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn deserialize_from_rational() {
        let cases: Vec<BigRational> = vec![
            BigRational::from_integer(0.into()),
            BigRational::from_integer(81516161.into()),
            BigRational::new(370.into(), 5123.into()),
            BigRational::new(1742152.into(), 848841.into()),
        ];
        for r in cases {
            let json_str = json::to_string(&r).unwrap();
            let expected: MmNumber = r.into();
            let actual: MmNumber = json::from_str(&json_str).unwrap();
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn deserialize_mixed_formats() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Bag {
            num: MmNumber,
            nums: Vec<MmNumber>,
        }

        let expected = Bag {
            num: BigRational::new(1.into(), 10.into()).into(),
            nums: vec![
                BigRational::from_integer(50.into()).into(),
                BigRational::new(1.into(), 1000.into()).into(),
                BigRational::from_integer(1_000_000_000_000i64.into()).into(),
                BigRational::new(33.into(), 100.into()).into(),
                BigRational::new(5.into(), 2.into()).into(),
            ],
        };

        let input = json::json!({
            "num": "0.1",
            "nums": ["50", "1e-3", "1e12", "0.33", "2.5"]
        });

        assert_eq!(expected, json::from_value(input).unwrap());
    }

    #[test]
    fn deserialize_fraction_object() {
        let s = r#"{"numer":"2000","denom":"3"}"#;
        let f: Fraction = json::from_str(s).unwrap();
        assert_eq!(&BigInt::from(2000), f.numer());
        assert_eq!(&BigInt::from(3), f.denom());

        let bad = r#"{"numer":"2000","denom":"0"}"#;
        let err = json::from_str::<Fraction>(bad).unwrap_err();
        assert_eq!("denom can not be 0", err.to_string());
    }

    #[test]
    fn deserialize_from_fraction_object() {
        let input = r#"{"numer":"2000","denom":"3"}"#;
        let n: MmNumber = json::from_str(input).unwrap();
        let expected = MmNumber(BigRational::new(2000.into(), 3.into()));
        assert_eq!(expected, n);
    }

    #[test]
    fn to_fraction_round_trip() {
        let n = MmNumber::from(BigRational::new(4.into(), 9.into()));
        let f = n.to_fraction();
        let back: MmNumber = f.into();
        assert_eq!(n, back);
    }

    #[test]
    fn construct_detailed_macro() {
        construct_detailed!(TestNum, value);
        let n = MmNumber::from((7u64, 3u64));
        let detail: TestNum = n.into();
        assert_eq!(*detail.as_ratio(), BigRational::new(7.into(), 3.into()));
    }

    #[test]
    fn big_int_str_serde() {
        let s = BigIntStr(1023.into());
        assert_eq!(r#""1023""#, json::to_string(&s).unwrap());

        let back: BigIntStr = json::from_str(r#""1023""#).unwrap();
        assert_eq!(BigInt::from(1023), back.0);

        assert!(json::from_str::<BigIntStr>("abc").is_err());
    }

    #[test]
    fn big_int_str_debug() {
        let s = BigIntStr(1023.into());
        assert_eq!("1023", format!("{:?}", s));
    }
}
