//! Test-only network configurations.
//!
//! Compiled in only when the `regtest-netid` feature is enabled. The
//! production `mm2` binary never enables this feature, so these netids
//! are unreachable in shipped builds.
//!
//! Each test netid shares the same fee-rate and recipient parameters as netid
//! 8762 (AtomicDEX), while leaving burn disabled so existing regtest fixtures
//! retain their single-output fee transactions. The DEX fee receivers are
//! intentionally the netid-8762 ones — regtest coins have no real value, so
//! the addresses are inert.
//!
//! Why so many: the upstream test fixtures use four distinct numeric
//! netids depending on the test suite (`docker_tests` use 9000;
//! `mm2_tests` use 8100, 8999, 9998 across various subsuites).
//! Migrating every fixture to a single netid would be a large,
//! mechanical change with no behavioural value, so we register all
//! four under one feature flag.

use lazy_static::lazy_static;
use num_rational::BigRational;

use crate::NetConfig;

const DEX_FEE_ADDR_PUBKEY: &str = "03bc2c7ba671bae4a6fc835244c9762b41647b9827d4780a89a949b984a8ddcc06";
const DEX_FEE_Z_ADDR: &str = "zs1rp6426e9r6jkq2nsanl66tkd34enewrmr0uvj0zelhkcwmsy0uvxz2fhm9eu9rl3ukxvgzy2v9f";
const DEX_FEE_PUBKEY_ED25519: &str = "77b0936728f63257b074c7b3fb2c4fad98df345f57de1ec418fc42619e4e29f8";
const SEED_NODES: &[&str] = &[];

lazy_static! {
    static ref DEX_FEE_ADDR_RAW: Vec<u8> =
        hex::decode(DEX_FEE_ADDR_PUBKEY).expect("test_netids: invalid DEX_FEE_ADDR_PUBKEY hex");
}

macro_rules! define_test_netid {
    ($struct_name:ident, $id:expr) => {
        pub struct $struct_name;

        impl NetConfig for $struct_name {
            fn netid(&self) -> u16 { $id }

            fn network_name(&self) -> &'static str { "Regtest" }

            fn dex_fee_addr_pubkey(&self) -> &'static str { DEX_FEE_ADDR_PUBKEY }

            fn dex_fee_addr_raw_pubkey(&self) -> &'static [u8] { &DEX_FEE_ADDR_RAW }

            fn dex_fee_z_addr(&self) -> &'static str { DEX_FEE_Z_ADDR }

            fn dex_fee_pubkey_ed25519(&self) -> &'static str { DEX_FEE_PUBKEY_ED25519 }

            fn dex_fee_rate(&self) -> BigRational { BigRational::new(1.into(), 777.into()) }

            fn fee_discount_tickers(&self) -> &'static [&'static str] { &["KMD"] }

            fn dex_fee_rate_discounted(&self) -> BigRational { BigRational::new(9.into(), 7770.into()) }

            fn seed_nodes(&self) -> &'static [&'static str] { SEED_NODES }
        }
    };
}

define_test_netid!(Netid8100, 8100);
define_test_netid!(Netid8999, 8999);
define_test_netid!(Netid9000, 9000);
define_test_netid!(Netid9998, 9998);
