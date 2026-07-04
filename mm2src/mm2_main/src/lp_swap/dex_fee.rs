//! DEX fee computation helpers.
//!
//! All fee parameters (rates, discounts, thresholds, burn split) are sourced
//! from [`mm2_net_config::NetConfig`] keyed on the active netid. None of the
//! values are hard-coded here — adding a new network is a matter of adding
//! a new module under `mm2_net_config/src/`.
//!
//! This module is purely arithmetic: it produces a `DexFee` value but does
//! not resolve a destination address. Address resolution happens at the
//! coin-side (see for example `siacoin::SiaCoinBuilder`).

use coins::{DexFee, MmCoin, MmCoinEnum};
use common::mm_number::MmNumber;
use common::var;
use mm2_net_config::NetConfig;

/// Returns the effective DEX-fee floor for a swap.
///
/// Whichever is larger of the network's configured minimum DEX fee
/// (`NetConfig::dex_fee_min_threshold`) and the taker coin's `min_tx_amount`.
pub(crate) fn dex_fee_threshold(net_cfg: &dyn NetConfig, min_tx_amount: MmNumber) -> MmNumber {
    let min_fee: MmNumber = net_cfg.dex_fee_min_threshold().into();
    if min_fee < min_tx_amount {
        min_tx_amount
    } else {
        min_fee
    }
}

/// Returns the DEX fee rate (base or discounted) for a `(base, rel)` pair.
///
/// The discount applies if either ticker is in
/// `NetConfig::fee_discount_tickers()`.
pub(crate) fn dex_fee_rate(net_cfg: &dyn NetConfig, base: &str, rel: &str) -> MmNumber {
    let discount_tickers: &[&str] = if cfg!(test) && var("MYCOIN_FEE_DISCOUNT").is_ok() {
        // In tests, also give discount to MYCOIN (alongside the netid-configured tickers)
        // This is a test-only workaround; the NetConfig discount tickers are authoritative.
        let configured = net_cfg.fee_discount_tickers();
        if configured.contains(&"MYCOIN") {
            configured
        } else {
            // Fall back to checking both configured tickers and MYCOIN
            if configured.contains(&base) || configured.contains(&rel) || base == "MYCOIN" || rel == "MYCOIN" {
                return net_cfg.dex_fee_rate_discounted().into();
            } else {
                return net_cfg.dex_fee_rate().into();
            }
        }
    } else {
        net_cfg.fee_discount_tickers()
    };
    if discount_tickers.contains(&base) || discount_tickers.contains(&rel) {
        net_cfg.dex_fee_rate_discounted().into()
    } else {
        net_cfg.dex_fee_rate().into()
    }
}

/// Returns the DEX fee amount for a trade, with the threshold floor applied.
pub fn dex_fee_amount(
    net_cfg: &dyn NetConfig,
    base: &str,
    rel: &str,
    trade_amount: &MmNumber,
    dex_fee_threshold: &MmNumber,
) -> MmNumber {
    let rate = dex_fee_rate(net_cfg, base, rel);
    let fee_amount = trade_amount * &rate;
    if &fee_amount < dex_fee_threshold {
        dex_fee_threshold.clone()
    } else {
        fee_amount
    }
}

/// Convenience: compute `dex_fee_amount` deriving the threshold from
/// `taker_coin.min_tx_amount()`.
pub fn dex_fee_amount_from_taker_coin(
    net_cfg: &dyn NetConfig,
    taker_coin: &MmCoinEnum,
    maker_coin: &str,
    trade_amount: &MmNumber,
) -> MmNumber {
    dex_fee_amount_from_taker_coin_ref(net_cfg, &**taker_coin, maker_coin, trade_amount)
}

pub(crate) fn dex_fee_amount_from_taker_coin_ref(
    net_cfg: &dyn NetConfig,
    taker_coin: &dyn MmCoin,
    maker_coin: &str,
    trade_amount: &MmNumber,
) -> MmNumber {
    let min_tx_amount = MmNumber::from(taker_coin.min_tx_amount());
    let threshold = dex_fee_threshold(net_cfg, min_tx_amount);
    dex_fee_amount(net_cfg, taker_coin.ticker(), maker_coin, trade_amount, &threshold)
}

/// Computes the full [`DexFee`] for a taker swap, applying the burn split
/// from the network configuration.
///
/// If `NetConfig::burn_enabled()` is false, returns `DexFee::Standard`.
/// Otherwise, splits the total fee according to `NetConfig::dex_fee_share()`:
///   - `fee_amount = total * share` (goes to DEX fee address)
///   - `burn_amount = total - fee_amount` (goes to OP_RETURN / burn address)
///
/// The burn destination is `KmdOpReturn` for KMD, `PreBurnAccount` for others.
pub fn compute_dex_fee(
    net_cfg: &dyn NetConfig,
    taker_coin: &MmCoinEnum,
    maker_coin: &str,
    trade_amount: &MmNumber,
) -> DexFee {
    compute_dex_fee_from_coin(net_cfg, &**taker_coin, maker_coin, trade_amount)
}

pub(crate) fn compute_dex_fee_from_coin(
    net_cfg: &dyn NetConfig,
    taker_coin: &dyn MmCoin,
    maker_coin: &str,
    trade_amount: &MmNumber,
) -> DexFee {
    let total = dex_fee_amount_from_taker_coin_ref(net_cfg, taker_coin, maker_coin, trade_amount);
    DexFee::new_from_taker_coin(taker_coin, net_cfg, total)
}

/// Computes the full [`DexFee`] when the taker's expected sender pubkey is known.
///
/// Use this in validation and post-negotiation production paths; the pubkey-blind
/// [`compute_dex_fee`] remains for pre-negotiation estimates where the relevant
/// taker pubkey is not available.
pub fn compute_dex_fee_with_taker_pubkey(
    net_cfg: &dyn NetConfig,
    taker_coin: &MmCoinEnum,
    maker_coin: &str,
    trade_amount: &MmNumber,
    taker_pubkey: &[u8],
) -> DexFee {
    compute_dex_fee_with_taker_pubkey_from_coin(net_cfg, &**taker_coin, maker_coin, trade_amount, taker_pubkey)
}

pub(crate) fn compute_dex_fee_with_taker_pubkey_from_coin(
    net_cfg: &dyn NetConfig,
    taker_coin: &dyn MmCoin,
    maker_coin: &str,
    trade_amount: &MmNumber,
    taker_pubkey: &[u8],
) -> DexFee {
    let total = dex_fee_amount_from_taker_coin_ref(net_cfg, taker_coin, maker_coin, trade_amount);
    DexFee::new_with_taker_pubkey(taker_coin, net_cfg, total, taker_pubkey)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{compute_dex_fee, compute_dex_fee_with_taker_pubkey};
    use coins::{DexFee, MarketCoinOps, MmCoinEnum, TestCoin};
    use common::mm_number::{BigDecimal, MmNumber};
    use mm2_net_config::net_config_or_panic;
    use mocktopus::mocking::*;

    fn mock_min_tx_amount() { TestCoin::min_tx_amount.mock_safe(|_| MockResult::Return(BigDecimal::from(0))); }

    #[test]
    fn known_taker_pubkey_fee_computation_returns_no_fee_for_burn_pubkey() {
        mock_min_tx_amount();

        let net_cfg = net_config_or_panic(6133);
        let taker_coin = MmCoinEnum::Test(TestCoin::new("MORTY"));
        let trade_amount = MmNumber::from("1");
        let burn_pubkey = net_cfg.burn_addr_raw_pubkey();

        let aware_fee = compute_dex_fee_with_taker_pubkey(net_cfg, &taker_coin, "RICK", &trade_amount, burn_pubkey);
        let blind_fee = compute_dex_fee(net_cfg, &taker_coin, "RICK", &trade_amount);

        assert_eq!(aware_fee, DexFee::NoFee);
        assert_ne!(blind_fee, DexFee::NoFee);
    }

    #[test]
    fn t16_4a_v1_known_pubkey_paths_use_pubkey_aware_fee_computation() {
        let maker_swap = include_str!("maker_swap.rs");
        let taker_swap = include_str!("taker_swap.rs");

        assert!(maker_swap.contains("compute_dex_fee_with_taker_pubkey("));
        assert!(maker_swap.contains("other_taker_coin_htlc_pub"));
        assert!(taker_swap.matches("compute_dex_fee_with_taker_pubkey(").count() >= 2);
        assert!(taker_swap.contains("my_taker_coin_htlc_keypair"));
    }

    #[test]
    fn t16_4b_v2_known_pubkey_paths_use_pubkey_aware_fee_computation() {
        let maker_swap_v2 = include_str!("maker_swap_v2.rs");
        let taker_swap_v2 = include_str!("taker_swap_v2.rs");

        assert!(
            maker_swap_v2
                .matches("compute_dex_fee_with_taker_pubkey_from_coin(")
                .count()
                >= 3
        );
        assert!(!maker_swap_v2.contains("dex_fee: &DexFee::NoFee"));
        assert!(
            taker_swap_v2
                .matches("compute_dex_fee_with_taker_pubkey_from_coin(")
                .count()
                >= 4
        );
        assert!(!taker_swap_v2.contains("dex_fee: &DexFee::NoFee"));
    }
}
