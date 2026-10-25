//! Structural fallback for header reads of a coin that has no `protocol.header_layout` key
//! (CRD ch.37 R37.5.9e). The cascade lives here, in the coins layer; `kdf_chain` only offers the
//! per-layout readers.

use super::UtxoCoinConf;
use serialization::CoinVariant;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The layouts tried, in order, after the standard read of a list or lone header fails exact
/// consumption. The first one that parses with exact consumption wins.
pub(crate) const FALLBACK_CASCADE: [CoinVariant; 2] = [CoinVariant::Riecoin, CoinVariant::KawpowAuxpowMixed];

/// The family a coin belongs to for the purposes of the `header_layout_fallback_families` switch.
///
/// Every coin without the `header_layout` key is `"BTC"` today: this build does not read
/// `chain_variant` (CRD ch.37 D1 is open), so there is no other family to report. When
/// `chain_variant` is read, this is the one place that starts returning its value.
pub(crate) fn coin_family(_conf: &UtxoCoinConf) -> &'static str { "BTC" }

/// The right to fall back for one coin, plus the once-per-activation log flag. Exists only for a coin
/// that has no `header_layout` key and whose family is in the node switch's set.
#[derive(Clone, Debug)]
pub struct HeaderLayoutFallback {
    ticker: String,
    logged: Arc<AtomicBool>,
}

impl HeaderLayoutFallback {
    /// `None` when the coin pins its layout with `header_layout` (the fallback is then never
    /// consulted) or when its family is not in `header_layout_fallback_families`.
    pub(crate) fn for_conf(conf: &UtxoCoinConf) -> Option<Self> {
        if conf.header_layout.is_some() {
            return None;
        }
        let family = coin_family(conf);
        if !conf.header_layout_fallback_families.iter().any(|f| f == family) {
            return None;
        }
        Some(HeaderLayoutFallback {
            ticker: conf.ticker.clone(),
            logged: conf.header_layout_fallback_logged.clone(),
        })
    }

    /// Records that the fallback chose `layout`; logs once per coin per activation.
    fn chosen(&self, layout: CoinVariant) {
        if self.logged.swap(true, Ordering::Relaxed) {
            return;
        }
        let name = match layout {
            CoinVariant::Riecoin => "riecoin",
            CoinVariant::KawpowAuxpowMixed => "kawpow_auxpow_mixed",
            CoinVariant::Standard | CoinVariant::Qtum => return,
        };
        common::log::info!(
            "{}: the standard header layout did not fit the served headers; read them with the '{}' layout \
             by structural fallback (header_layout_fallback_families). Pin it with \"header_layout\": \"{}\" \
             in the coin's protocol object.",
            self.ticker,
            name,
            name
        );
    }
}

/// Runs `read` with `standard` and, only when that fails, with each layout of [`FALLBACK_CASCADE`]
/// in order, returning the first success. With `fallback` `None`, or when every layout fails, the
/// error of the standard read is returned. The fallback never runs when the standard read succeeded.
pub(crate) fn read_with_fallback<T, E>(
    standard: CoinVariant,
    fallback: Option<&HeaderLayoutFallback>,
    read: impl Fn(CoinVariant) -> Result<T, E>,
) -> Result<T, E> {
    let original = match read(standard) {
        Ok(parsed) => return Ok(parsed),
        Err(e) => e,
    };
    let Some(fallback) = fallback else { return Err(original) };
    for layout in FALLBACK_CASCADE {
        if let Ok(parsed) = read(layout) {
            fallback.chosen(layout);
            return Ok(parsed);
        }
    }
    Err(original)
}
