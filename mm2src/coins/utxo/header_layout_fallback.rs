//! Structural fallback for header reads of a coin that has no `protocol.header_layout` key
//! (CRD ch.37 R37.5.9e). The cascade lives here, in the coins layer; `kdf_chain` only offers the
//! per-layout readers.

use super::UtxoCoinConf;
use chain::BlockHeader;
use serialization::CoinVariant;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The layouts tried, in order, after the standard read of a list or lone header fails exact
/// consumption. The first one that parses with exact consumption wins.
pub(crate) const FALLBACK_CASCADE: [CoinVariant; 2] = [CoinVariant::Riecoin, CoinVariant::KawpowAuxpowMixed];

/// The value of a coin's `protocol.chain_variant` key (CRD ch.37 R37.5.1): the closed set of eight
/// header families. The eight spellings live in [`ChainVariant::name`] and nowhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainVariant {
    Btc,
    Qtum,
    Lbc,
    Ppc,
    Rick,
    Morty,
    Rvn,
    Pivx,
}

impl ChainVariant {
    pub const ALL: [ChainVariant; 8] = [
        ChainVariant::Btc,
        ChainVariant::Qtum,
        ChainVariant::Lbc,
        ChainVariant::Ppc,
        ChainVariant::Rick,
        ChainVariant::Morty,
        ChainVariant::Rvn,
        ChainVariant::Pivx,
    ];

    /// The `chain_variant` spelling of this family, as coin files carry it.
    pub const fn name(self) -> &'static str {
        match self {
            ChainVariant::Btc => "BTC",
            ChainVariant::Qtum => "QTUM",
            ChainVariant::Lbc => "LBC",
            ChainVariant::Ppc => "PPC",
            ChainVariant::Rick => "RICK",
            ChainVariant::Morty => "MORTY",
            ChainVariant::Rvn => "RVN",
            ChainVariant::Pivx => "PIVX",
        }
    }

    /// The family a spelling names; exact and case-sensitive.
    pub fn from_name(value: &str) -> Option<Self> { Self::ALL.into_iter().find(|v| v.name() == value) }

    /// The spellings, comma-separated, for error texts.
    pub fn names() -> String { Self::ALL.map(ChainVariant::name).join(", ") }

    /// The reader this family selects, or `None` for a family whose header layout is not implemented.
    ///
    /// The exclusivity and thresholded rules of R37.5.3 and R37.5.5 (`"BTC"` reading standard-80 only,
    /// `"PPC"` suppressing the Equihash reading of version-4 headers, `"RICK"`/`"MORTY"` reading
    /// Equihash for every version) are plan step H4 and are not implemented: those families read the
    /// standard reader, which keeps the version-signalled recognition of R37.5.4 (for `"RVN"` that
    /// includes the KawPoW trailer at version 0x30000000).
    fn reader(self) -> Option<CoinVariant> {
        match self {
            ChainVariant::Qtum => Some(CoinVariant::Qtum),
            ChainVariant::Btc | ChainVariant::Ppc | ChainVariant::Rick | ChainVariant::Morty | ChainVariant::Rvn => {
                Some(CoinVariant::Standard)
            },
            ChainVariant::Lbc | ChainVariant::Pivx => None,
        }
    }
}

/// The family a coin belongs to for the purposes of the `header_layout_fallback_families` switch:
/// its `chain_variant` value, or `"BTC"` when the key is absent (CRD ch.37 R37.5.9e, "Family before
/// and after D1"). The protocol-type Qtum reader of a keyless QTUM/QRC20 coin is a reader choice, not
/// a family name.
pub(crate) fn coin_family(conf: &UtxoCoinConf) -> &'static str {
    conf.chain_variant.unwrap_or(ChainVariant::Btc).name()
}

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

/// Why a fallback-aware read failed.
#[derive(Debug)]
pub enum FallbackReadError<E> {
    /// The standard read failed and no alternative consumed the bytes exactly (or the fallback was
    /// off): the error of the standard read.
    Rejected(E),
    /// More than one alternative layout consumed the bytes exactly; the operator resolves it by
    /// pinning the coin with `header_layout` (CRD ch.37 R37.5.9e).
    Ambiguous(String),
    /// The coin's `chain_variant` family has no header reader in this build: every header read of
    /// the coin is refused, never read as a standard header.
    NotImplemented(ChainVariant),
}

/// The refusal text of a family without a header reader.
pub(crate) fn not_implemented_reason(family: ChainVariant) -> String {
    format!("header layout not implemented for chain_variant {}", family.name())
}

fn layout_name(layout: CoinVariant) -> &'static str {
    match layout {
        CoinVariant::Riecoin => "riecoin",
        CoinVariant::KawpowAuxpowMixed => "kawpow_auxpow_mixed",
        CoinVariant::Standard => "standard",
        CoinVariant::Qtum => "qtum",
    }
}

/// Runs `read` with `standard` and, only when that fails, with every layout of [`FALLBACK_CASCADE`],
/// accepting the result only if exactly one alternative consumed the bytes exactly. With `fallback`
/// `None`, or when no alternative fits, the error of the standard read is returned; when two fit the
/// read is [`FallbackReadError::Ambiguous`]. The fallback never runs when the standard read succeeded.
pub(crate) fn read_with_fallback<T, E>(
    standard: CoinVariant,
    fallback: Option<&HeaderLayoutFallback>,
    read: impl Fn(CoinVariant) -> Result<T, E>,
) -> Result<T, FallbackReadError<E>> {
    let original = match read(standard) {
        Ok(parsed) => return Ok(parsed),
        Err(e) => e,
    };
    let Some(fallback) = fallback else {
        return Err(FallbackReadError::Rejected(original));
    };
    let mut fits: Vec<(CoinVariant, T)> = FALLBACK_CASCADE
        .into_iter()
        .filter_map(|layout| read(layout).ok().map(|parsed| (layout, parsed)))
        .collect();
    match fits.len() {
        0 => Err(FallbackReadError::Rejected(original)),
        1 => {
            let (layout, parsed) = fits.remove(0);
            fallback.chosen(layout);
            Ok(parsed)
        },
        _ => Err(FallbackReadError::Ambiguous(format!(
            "{}: the served headers consume exactly under both the '{}' and '{}' layouts; \
             pin the coin with \"header_layout\"",
            fallback.ticker,
            layout_name(fits[0].0),
            layout_name(fits[1].0)
        ))),
    }
}

impl<E> FallbackReadError<E> {
    /// The text of the failure: `describe` renders the error of the standard read, the ambiguity
    /// already carries its own reason.
    pub(crate) fn reason(self, describe: impl FnOnce(E) -> String) -> String {
        match self {
            FallbackReadError::Rejected(e) => describe(e),
            FallbackReadError::Ambiguous(reason) => reason,
            FallbackReadError::NotImplemented(family) => not_implemented_reason(family),
        }
    }
}

/// How one coin reads served header bytes on every path (CRD ch.37 R37.5.1a, R37.5.9a, R37.5.9e):
/// one layout per coin, plus the structural fallback when the coin has no `header_layout` key and
/// its family is in the node switch. Cheap to clone; the clones of one coin share its
/// once-per-activation log flag.
#[derive(Clone, Debug)]
pub struct HeaderReadPolicy {
    layout: CoinVariant,
    /// Set for a coin whose `chain_variant` family has no reader: every read is refused.
    refusal: Option<ChainVariant>,
    fallback: Option<HeaderLayoutFallback>,
}

impl HeaderReadPolicy {
    /// The policy of `conf`'s coin, resolving its one layout in this order: the `header_layout` key
    /// (never the fallback); else the family reader of `chain_variant`; else the protocol-type
    /// reader (Qtum for QTUM and QRC20 coins, a documented divergence from R37.5.1 that the
    /// maintainer keeps); else the standard reader.
    pub(crate) fn for_conf(conf: &UtxoCoinConf) -> Self {
        let (layout, refusal) = match (conf.header_layout, conf.chain_variant) {
            (Some(layout), _) => (layout, None),
            (None, Some(family)) => match family.reader() {
                Some(layout) => (layout, None),
                None => (CoinVariant::Standard, Some(family)),
            },
            (None, None) => (conf.protocol_header_reader, None),
        };
        HeaderReadPolicy {
            layout,
            refusal,
            fallback: HeaderLayoutFallback::for_conf(conf),
        }
    }

    /// Reads with `layout` only: no refusal, no fallback.
    pub(crate) fn with_layout(layout: CoinVariant) -> Self {
        HeaderReadPolicy {
            layout,
            refusal: None,
            fallback: None,
        }
    }

    /// Reads with the standard layout only: no pinned layout, no fallback.
    pub fn standard() -> Self { Self::with_layout(CoinVariant::Standard) }

    /// The layout this policy reads first.
    pub(crate) fn layout(&self) -> CoinVariant { self.layout }

    /// Reads one header from exactly the bytes served for it.
    pub fn read_header(&self, bytes: &[u8]) -> Result<BlockHeader, FallbackReadError<serialization::Error>> {
        self.check_implemented()?;
        read_with_fallback(self.layout, self.fallback.as_ref(), |layout| {
            BlockHeader::from_served_bytes(bytes, layout)
        })
    }

    /// Reads `count` consecutive headers from exactly the bytes served for them.
    pub fn read_list(
        &self,
        bytes: &[u8],
        count: usize,
    ) -> Result<Vec<BlockHeader>, FallbackReadError<serialization::Error>> {
        self.check_implemented()?;
        read_with_fallback(self.layout, self.fallback.as_ref(), |layout| {
            BlockHeader::list_from_served_bytes(bytes, count, layout)
        })
    }

    /// Fails for a coin whose family has no header reader. Every read of the policy goes through it.
    pub(crate) fn check_implemented<E>(&self) -> Result<(), FallbackReadError<E>> {
        match self.refusal {
            Some(family) => Err(FallbackReadError::NotImplemented(family)),
            None => Ok(()),
        }
    }
}
