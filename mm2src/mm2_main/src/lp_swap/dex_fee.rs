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
/// Whichever is larger of the optional network override and the taker coin's
/// `min_tx_amount`. Both production reference networks leave the override at
/// zero, making the coin minimum the effective floor.
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
/// Otherwise, the coin policy selects a direct OP_RETURN burn, an account
/// burn, or the standard single-output form.
///
/// Active production policy burns only KMD on netid 8762, splitting 75% to the
/// fee address and 25% to OP_RETURN. Netid 6133 and non-KMD netid-8762 takers
/// use the standard form.
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

/// CRD ch.16 R12B: on the version-two protocol only, if either coin's ticker
/// is in the network's version-two no-fee ticker set, every version-two
/// dex-fee computation MUST yield `NoFee`, before any step of R7. Empty on
/// netid 6133; `["KMD"]` on netid 8762.
fn v2_ticker_exemption_applies(net_cfg: &dyn NetConfig, maker_coin: &str, taker_coin: &str) -> bool {
    let no_fee_tickers = net_cfg.no_fee_tickers_v2();
    no_fee_tickers.contains(&maker_coin) || no_fee_tickers.contains(&taker_coin)
}

/// Version-two, taker-pubkey-aware dex-fee computation (CRD ch.16 R12A/R12B).
///
/// Every version-two maker/taker call site that knows the taker's pubkey MUST
/// route through this (not the plain [`compute_dex_fee_with_taker_pubkey`]),
/// so the netid-selected version-two ticker exemption of R12B is applied
/// before the R7 no-fee waiver and the ordinary fee computation.
pub fn compute_dex_fee_v2_with_taker_pubkey(
    net_cfg: &dyn NetConfig,
    taker_coin: &MmCoinEnum,
    maker_coin: &str,
    trade_amount: &MmNumber,
    taker_pubkey: &[u8],
) -> DexFee {
    compute_dex_fee_v2_with_taker_pubkey_from_coin(net_cfg, &**taker_coin, maker_coin, trade_amount, taker_pubkey)
}

pub(crate) fn compute_dex_fee_v2_with_taker_pubkey_from_coin(
    net_cfg: &dyn NetConfig,
    taker_coin: &dyn MmCoin,
    maker_coin: &str,
    trade_amount: &MmNumber,
    taker_pubkey: &[u8],
) -> DexFee {
    if v2_ticker_exemption_applies(net_cfg, maker_coin, taker_coin.ticker()) {
        return DexFee::NoFee;
    }
    compute_dex_fee_with_taker_pubkey_from_coin(net_cfg, taker_coin, maker_coin, trade_amount, taker_pubkey)
}

/// Version-two, pubkey-blind dex-fee estimate (CRD ch.16 R12A/R12B).
///
/// The pre-negotiation counterpart of [`compute_dex_fee_v2_with_taker_pubkey`]
/// for a future version-two call site that needs an estimate before the
/// taker's pubkey is known (none currently exists: every V2 maker/taker
/// dex-fee call site in `maker_swap_v2.rs`/`taker_swap_v2.rs` already knows
/// the relevant pubkey). Provided for R12B completeness and symmetry with
/// the V1 pubkey-blind/pubkey-aware pair.
pub fn compute_dex_fee_v2(
    net_cfg: &dyn NetConfig,
    taker_coin: &MmCoinEnum,
    maker_coin: &str,
    trade_amount: &MmNumber,
) -> DexFee {
    compute_dex_fee_v2_from_coin(net_cfg, &**taker_coin, maker_coin, trade_amount)
}

pub(crate) fn compute_dex_fee_v2_from_coin(
    net_cfg: &dyn NetConfig,
    taker_coin: &dyn MmCoin,
    maker_coin: &str,
    trade_amount: &MmNumber,
) -> DexFee {
    if v2_ticker_exemption_applies(net_cfg, maker_coin, taker_coin.ticker()) {
        return DexFee::NoFee;
    }
    compute_dex_fee_from_coin(net_cfg, taker_coin, maker_coin, trade_amount)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{compute_dex_fee, compute_dex_fee_v2, compute_dex_fee_v2_with_taker_pubkey,
                compute_dex_fee_with_taker_pubkey};
    use coins::{DexFee, MarketCoinOps, MmCoinEnum, TestCoin};
    use common::mm_number::{BigDecimal, MmNumber};
    use mm2_net_config::net_config_or_panic;
    use mocktopus::mocking::*;

    fn mock_min_tx_amount() { TestCoin::min_tx_amount.mock_safe(|_| MockResult::Return(BigDecimal::from(0))); }

    /// CRD ch.08 R15C / ch.16 R7 (issue #11): the no-fee waiver does not
    /// depend on the network burn gate. Netid 6133 has `burn_enabled() ==
    /// false`, but a taker holding the network's burn/waiver key still pays
    /// no fee via the pubkey-aware path. This inverts the withdrawn test of
    /// the same name that asserted the opposite before the R7 correction.
    #[test]
    fn burn_disabled_network_still_waives_fee_for_burn_pubkey() {
        mock_min_tx_amount();

        let net_cfg = net_config_or_panic(6133);
        let taker_coin = MmCoinEnum::Test(TestCoin::new("MARTY"));
        let trade_amount = MmNumber::from("1");
        let burn_pubkey = net_cfg.burn_addr_raw_pubkey();

        let aware_fee = compute_dex_fee_with_taker_pubkey(net_cfg, &taker_coin, "DOC", &trade_amount, burn_pubkey);
        let blind_fee = compute_dex_fee(net_cfg, &taker_coin, "DOC", &trade_amount);

        // The pubkey-blind path cannot apply the waiver (it doesn't know the
        // taker's pubkey), so it still returns the standard fee.
        assert_eq!(blind_fee, DexFee::Standard(MmNumber::from((2, 100))));
        // The pubkey-aware path applies the R7 waiver regardless of the
        // (false) burn gate.
        assert_eq!(aware_fee, DexFee::NoFee);
    }

    /// CRD ch.51 R14 ordering regression (issue #11 follow-up). The taker's
    /// `negotiate` step blocks on receiving the maker's `SwapMsg::Negotiated(true)`
    /// before it will do anything else, including deciding it owes no fee; that
    /// broadcast is sent only from inside `MakerSwap::wait_taker_fee`. R14's "MUST
    /// NOT require or decode a fee transaction" governs the fee transaction only.
    /// If the `DexFee::NoFee` short-circuit ran before the `Negotiated` broadcast,
    /// a burn-key taker's negotiation would time out because the maker never
    /// acknowledges it. There is no harness to drive `MakerSwap::wait_taker_fee`
    /// end-to-end (same limitation `t18` below documents for the V2 machines), so
    /// this pins the ordering at the source level instead: within
    /// `wait_taker_fee`, the `Negotiated(true)` broadcast and the taker-fee
    /// message receipt MUST both textually precede the `DexFee::NoFee` match.
    #[test]
    fn ch51_r14_negotiated_broadcast_precedes_nofee_short_circuit_in_wait_taker_fee() {
        let maker_swap = include_str!("maker_swap.rs");
        let fn_start = maker_swap
            .find("async fn wait_taker_fee(")
            .expect("wait_taker_fee must exist in maker_swap.rs");
        let fn_body = &maker_swap[fn_start..];
        let fn_end = fn_body
            .find("\n    async fn maker_payment(")
            .expect("wait_taker_fee must be followed by maker_payment in maker_swap.rs");
        let fn_body = &fn_body[..fn_end];

        let negotiated_pos = fn_body
            .find("SwapMsg::Negotiated(true)")
            .expect("wait_taker_fee must broadcast SwapMsg::Negotiated(true)");
        let recv_pos = fn_body
            .find("store.taker_fee.take()")
            .expect("wait_taker_fee must still receive the taker's SwapMsg::TakerFee");
        let nofee_pos = fn_body
            .find("DexFee::NoFee")
            .expect("wait_taker_fee must short-circuit on DexFee::NoFee");

        assert!(
            negotiated_pos < nofee_pos,
            "the Negotiated(true) broadcast must precede the NoFee short-circuit, \
             otherwise a burn-key taker's negotiation times out"
        );
        assert!(
            recv_pos < nofee_pos,
            "the taker-fee message must still be received (unparsed) before the \
             NoFee short-circuit, since the taker's SwapMsg::TakerFee carries no \
             other progress signal for this combined broadcast+receive step"
        );
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

    /// CRD ch.16 R12A/R12B (updated for issue #11 batch 2): the V2 machines
    /// now MUST route every dex-fee computation through the `_v2_` wrapper so
    /// the netid-selected ticker exemption of R12B applies before R7.
    #[test]
    fn t16_4b_v2_known_pubkey_paths_use_pubkey_aware_fee_computation() {
        let maker_swap_v2 = include_str!("maker_swap_v2.rs");
        let taker_swap_v2 = include_str!("taker_swap_v2.rs");

        assert!(
            maker_swap_v2
                .matches("compute_dex_fee_v2_with_taker_pubkey_from_coin(")
                .count()
                >= 3
        );
        assert!(!maker_swap_v2.contains("dex_fee: &DexFee::NoFee"));
        assert!(
            taker_swap_v2
                .matches("compute_dex_fee_v2_with_taker_pubkey_from_coin(")
                .count()
                >= 4
        );
        assert!(!taker_swap_v2.contains("dex_fee: &DexFee::NoFee"));
    }

    /// CRD ch.16 T4B. The V2 maker- and taker-side dex-fee decision point is
    /// `compute_dex_fee_v2_with_taker_pubkey`, now wired into every V2 call
    /// site (see `t16_4b` below); for a non-KMD pair (so ch.16 R12B does not
    /// apply) and a taker key equal to the network waiver key, it MUST yield
    /// `NoFee` on both production netids (ch.16 R7, R12A).
    #[test]
    fn t4b_v2_dex_fee_decision_point_waives_fee_for_burn_pubkey_both_netids() {
        mock_min_tx_amount();

        for netid in [8762u16, 6133u16] {
            let net_cfg = net_config_or_panic(netid);
            let taker_coin = MmCoinEnum::Test(TestCoin::new("MARTY"));
            let trade_amount = MmNumber::from("1");
            let burn_pubkey = net_cfg.burn_addr_raw_pubkey().to_vec();
            assert!(!burn_pubkey.is_empty(), "netid {} burn key must be non-empty", netid);

            let fee = compute_dex_fee_v2_with_taker_pubkey(net_cfg, &taker_coin, "DOC", &trade_amount, &burn_pubkey);
            assert_eq!(fee, DexFee::NoFee, "netid {}", netid);
        }
    }

    /// CRD ch.16 T5 -- version-two ticker exemption (R12B). On netid 8762, a
    /// version-two swap with KMD as the taker coin, and separately as the
    /// maker coin, MUST yield `NoFee` in both the pubkey-blind estimate and
    /// the pubkey-aware value (both roles, both before and after
    /// negotiation). On netid 6133 the same pairs MUST yield `Standard`. On
    /// netid 8762 the *legacy* (non-V2) computation for the same pair MUST
    /// still yield the chapter-08 R8 descriptor -- R12B never applies to the
    /// legacy protocol.
    #[test]
    fn t5_v2_ticker_exemption_applies_to_both_roles_both_forms() {
        mock_min_tx_amount();
        let trade_amount = MmNumber::from("1");
        let some_pubkey = vec![0x02; 33];

        // KMD as taker coin.
        let kmd_taker = MmCoinEnum::Test(TestCoin::new("KMD"));
        // KMD as maker coin (taker coin is an arbitrary non-KMD ticker).
        let non_kmd_taker = MmCoinEnum::Test(TestCoin::new("MARTY"));

        let net_cfg_8762 = net_config_or_panic(8762);
        assert_eq!(
            compute_dex_fee_v2(net_cfg_8762, &kmd_taker, "DOC", &trade_amount),
            DexFee::NoFee,
            "8762 blind, KMD taker"
        );
        assert_eq!(
            compute_dex_fee_v2_with_taker_pubkey(net_cfg_8762, &kmd_taker, "DOC", &trade_amount, &some_pubkey),
            DexFee::NoFee,
            "8762 aware, KMD taker"
        );
        assert_eq!(
            compute_dex_fee_v2(net_cfg_8762, &non_kmd_taker, "KMD", &trade_amount),
            DexFee::NoFee,
            "8762 blind, KMD maker"
        );
        assert_eq!(
            compute_dex_fee_v2_with_taker_pubkey(net_cfg_8762, &non_kmd_taker, "KMD", &trade_amount, &some_pubkey),
            DexFee::NoFee,
            "8762 aware, KMD maker"
        );

        let net_cfg_6133 = net_config_or_panic(6133);
        assert!(!matches!(
            compute_dex_fee_v2(net_cfg_6133, &kmd_taker, "DOC", &trade_amount),
            DexFee::NoFee
        ));
        assert!(!matches!(
            compute_dex_fee_v2_with_taker_pubkey(net_cfg_6133, &kmd_taker, "DOC", &trade_amount, &some_pubkey),
            DexFee::NoFee
        ));

        // Legacy (non-V2) protocol on 8762 with the same taker coin MUST NOT
        // apply the V2-only exemption -- `compute_dex_fee` (not the `_v2_`
        // wrapper) never yields `NoFee` from a ticker match alone.
        let legacy_fee = compute_dex_fee(net_cfg_8762, &kmd_taker, "DOC", &trade_amount);
        assert_ne!(
            legacy_fee,
            DexFee::NoFee,
            "legacy protocol must not apply the V2 ticker exemption"
        );
    }

    /// T9/P2.1: the 7 `net_config_or_panic` call sites in the V2 swap `on_changed`
    /// handlers (4 in `taker_swap_v2.rs`, 3 in `maker_swap_v2.rs`) must all have been
    /// replaced with the fallible `net_config_for`, which each site matches on and
    /// aborts the state machine cleanly (`AbortReason::InternalError`) rather than
    /// panicking, on `None`. `on_changed` itself needs a live coin implementing
    /// `MakerCoinSwapOpsV2`/`TakerCoinSwapOpsV2` to actually drive execution down to
    /// this call, which this codebase has no mocking infrastructure for (same
    /// limitation `taker_swap_v2.rs`'s own
    /// `maker_payment_spent_confirmation_timeout_no_longer_falls_back_to_empty_bytes`
    /// documents for T1/D8); this checks the fixed call sites' source shape directly
    /// instead, the same way `t16_4a`/`t16_4b` above do.
    #[test]
    fn t18_net_config_for_replaces_net_config_or_panic_in_v2_swap_files() {
        let maker_swap_v2 = include_str!("maker_swap_v2.rs");
        let taker_swap_v2 = include_str!("taker_swap_v2.rs");

        assert!(
            !maker_swap_v2.contains("net_config_or_panic"),
            "maker_swap_v2.rs must not call net_config_or_panic from a fallible on_changed context"
        );
        assert!(
            !taker_swap_v2.contains("net_config_or_panic"),
            "taker_swap_v2.rs must not call net_config_or_panic from a fallible on_changed context"
        );
        assert_eq!(
            maker_swap_v2
                .matches("mm2_net_config::net_config_for(sm.ctx.netid())")
                .count(),
            3,
            "maker_swap_v2.rs should have exactly 3 net_config_for call sites (P2.1)"
        );
        assert_eq!(
            taker_swap_v2
                .matches("mm2_net_config::net_config_for(sm.ctx.netid())")
                .count(),
            4,
            "taker_swap_v2.rs should have exactly 4 net_config_for call sites (P2.1)"
        );
    }
}
