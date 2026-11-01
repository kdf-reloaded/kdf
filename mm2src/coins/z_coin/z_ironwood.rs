//! Runtime discovery of a shielded coin's Ironwood activation height (CRD ch.39 R39.6.4e).
//!
//! The dictated chain publishes no activation height ahead of the upgrade: each node derives it from
//! the first block whose time exceeds a fixed timestamp, plus a settling margin, once that block is
//! buried deep enough. A light client therefore has two independent sources, and this module keeps
//! them apart so that each can check the other:
//!
//! * the **server's** consensus branch at its tip (`GetLightdInfo`), which says whether Ironwood is
//!   already active there, and
//! * the **timestamp rule**, evaluated over the compact blocks' times, which gives the height.
//!
//! [`decide`] is the pure decision over those two facts; [`first_block_after`] is the search that
//! feeds it. Nothing here touches the network on its own.

use super::z_lightwalletd::{parse_branch_id, LightwalletdBackend};
use derive_more::Display;
use std::future::Future;
use zcash_protocol::consensus::BranchId;

/// Blocks added to the transition block's height to give the activation height (the dictated
/// chain's own settling margin).
pub(crate) const IRONWOOD_SETTLING_BLOCKS: u64 = 60;

/// How deep the transition block must be before the chain itself treats the derived height as
/// final. Below this the height is not trusted, exactly as the nodes do not trust it.
pub(crate) const IRONWOOD_TRUST_DEPTH: u64 = 30;

/// Block times are not strictly monotonic, so the first block over the threshold is looked for in a
/// small window below the one a bisection lands on.
const TRANSITION_JITTER_WINDOW: u64 = 16;

/// The consensus branch identifier the server reports once Ironwood is active.
pub(crate) fn ironwood_branch_id() -> u32 { u32::from(BranchId::Nu6_3) }

/// What discovery concluded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum IronwoodStatus {
    /// Ironwood is not active at the server's tip and no activation height is known to lie ahead.
    /// The builder stays on the pre-Ironwood transaction version.
    NotActivated,
    /// Ironwood is active from this height, which is exact when derived from the timestamp rule and
    /// an upper bound (the server's tip) when only the server's branch established it. A build at
    /// any height above the tip is the same either way.
    ActiveFrom(u32),
}

/// Discovery cannot give an answer the builder may act on.
#[derive(Clone, Debug, Display, PartialEq)]
pub(crate) enum IronwoodDiscoveryError {
    /// The two sources disagree in a way that cannot be resolved by trusting either: building on
    /// the wrong side would produce a transaction the network rejects.
    #[display(fmt = "{}", _0)]
    Inconsistent(String),
    /// A read needed to decide failed.
    #[display(fmt = "the Ironwood activation could not be determined: {}", _0)]
    Unavailable(String),
}

/// Decides the status from the two facts.
///
/// `tip` and `server_branch` come from one `GetLightdInfo` read. `transition` is the height of the
/// first block whose time exceeds the activation time, when there is such a block.
///
/// | server at tip | derived height | result |
/// |---|---|---|
/// | Ironwood | at or below tip | that height |
/// | Ironwood | above tip or none | the tip: the server says it is active, so it was at the tip |
/// | not Ironwood | above tip | that height (upcoming) |
/// | not Ironwood | at or below tip | refused: the server and the timestamp rule disagree |
/// | not Ironwood | none | not activated |
pub(crate) fn decide(
    tip: u64,
    server_branch: u32,
    transition: Option<u64>,
) -> Result<IronwoodStatus, IronwoodDiscoveryError> {
    let derived = transition
        .filter(|transition| tip >= transition.saturating_add(IRONWOOD_TRUST_DEPTH))
        .map(|transition| transition.saturating_add(IRONWOOD_SETTLING_BLOCKS));
    let to_height = |height: u64| {
        u32::try_from(height)
            .map_err(|_| IronwoodDiscoveryError::Inconsistent(format!("height {} does not fit a block height", height)))
    };
    let server_active = server_branch == ironwood_branch_id();

    match (server_active, derived) {
        (true, Some(derived)) if derived <= tip => to_height(derived).map(IronwoodStatus::ActiveFrom),
        (true, _) => to_height(tip).map(IronwoodStatus::ActiveFrom),
        (false, Some(derived)) if derived > tip => to_height(derived).map(IronwoodStatus::ActiveFrom),
        (false, Some(derived)) => Err(IronwoodDiscoveryError::Inconsistent(format!(
            "the activation time puts Ironwood at height {}, at or below the server's tip {}, yet the server \
             reports consensus branch {:08x} rather than Ironwood's {:08x}; refusing to guess a transaction version",
            derived,
            tip,
            server_branch,
            ironwood_branch_id()
        ))),
        (false, None) => Ok(IronwoodStatus::NotActivated),
    }
}

/// The height of the first block in `floor..=tip` whose time exceeds `threshold`, or `None` when the
/// tip itself does not.
///
/// Block times are read through `time_at`. The search is a bisection on the assumption that times
/// rise with height, followed by a look at the [`TRANSITION_JITTER_WINDOW`] blocks below the landing
/// point, because the assumption fails by a few blocks around any given instant.
pub(crate) async fn first_block_after<F, Fut, E>(
    time_at: F,
    floor: u64,
    tip: u64,
    threshold: u32,
) -> Result<Option<u64>, E>
where
    F: Fn(u64) -> Fut,
    Fut: Future<Output = Result<u32, E>>,
{
    if tip < floor || time_at(tip).await? <= threshold {
        return Ok(None);
    }
    let (mut low, mut high) = (floor, tip);
    while low < high {
        let mid = low + (high - low) / 2;
        if time_at(mid).await? > threshold {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    let window_start = low.saturating_sub(TRANSITION_JITTER_WINDOW).max(floor);
    for height in window_start..low {
        if time_at(height).await? > threshold {
            return Ok(Some(height));
        }
    }
    Ok(Some(low))
}

/// Reads the two facts from the admitted endpoints and decides.
///
/// `activation_time` is the timestamp the chain's rule is anchored on, when the coin has one. Without
/// it only the server's branch can establish that Ironwood is active, which is the whole answer for
/// a chain that is already past the upgrade and no answer at all for one that is not.
///
/// A failed read is an error, never a default, except one: once the server has said Ironwood is
/// active the exact height only refines the answer, so a failure of the timestamp search then costs
/// precision about *past* heights and nothing about the version of a new transaction.
pub(crate) async fn discover(
    backend: &LightwalletdBackend,
    activation_time: Option<u32>,
) -> Result<IronwoodStatus, IronwoodDiscoveryError> {
    let info = backend
        .server_info()
        .await
        .map_err(|e| IronwoodDiscoveryError::Unavailable(e.to_string()))?;
    let server_branch = parse_branch_id(&info.consensus_branch_id).ok_or_else(|| {
        IronwoodDiscoveryError::Unavailable(format!(
            "the server reported an unparseable consensus branch {:?}",
            info.consensus_branch_id
        ))
    })?;
    let tip = info.block_height;

    let transition = match activation_time {
        None => None,
        Some(threshold) => {
            let floor = backend.sapling_activation_height();
            match first_block_after(|h| backend.block_time(h), floor, tip, threshold).await {
                Ok(transition) => transition,
                Err(e) if server_branch == ironwood_branch_id() => {
                    common::log::warn!(
                        "Ironwood activation time could not be searched ({}); the server reports Ironwood active at \
                         its tip {}, which is all a new transaction needs",
                        e,
                        tip
                    );
                    None
                },
                Err(e) => return Err(IronwoodDiscoveryError::Unavailable(e.to_string())),
            }
        },
    };
    decide(tip, server_branch, transition)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::convert::Infallible;

    const T: u32 = 1_000_000;
    const IRONWOOD: u32 = 0x37a5_165b;
    const SAPLING: u32 = 0x76b8_09bb;

    /// A chain with one block every 60 s whose block 1000 is the first over `T`.
    fn steady(height: u64) -> Result<u32, Infallible> { Ok(((T as i64) + (height as i64 - 1000) * 60 + 1) as u32) }

    #[test]
    fn the_branch_constant_is_the_one_the_chain_reports() {
        assert_eq!(ironwood_branch_id(), IRONWOOD);
    }

    #[test]
    fn the_search_finds_the_first_block_over_the_threshold() {
        let found = block_on(first_block_after(|h| async move { steady(h) }, 61, 5_000, T)).unwrap();
        assert_eq!(found, Some(1000));
    }

    #[test]
    fn a_chain_that_has_not_reached_the_threshold_has_no_transition() {
        assert_eq!(
            block_on(first_block_after(|h| async move { steady(h) }, 61, 999, T)).unwrap(),
            None
        );
    }

    #[test]
    fn a_jitter_below_the_landing_point_is_still_found() {
        // Block 995 is already over the threshold although 996..=999 are not.
        let time = |h: u64| async move {
            if h == 995 {
                Ok::<u32, Infallible>(T + 5)
            } else {
                steady(h)
            }
        };
        assert_eq!(block_on(first_block_after(time, 61, 5_000, T)).unwrap(), Some(995));
    }

    #[test]
    fn a_failing_read_fails_the_search_instead_of_guessing() {
        let time = |_h: u64| async { Err::<u32, &str>("down") };
        assert_eq!(block_on(first_block_after(time, 61, 5_000, T)), Err("down"));
    }

    #[test]
    fn the_height_is_the_transition_plus_the_settling_margin_once_trusted() {
        // Not active at the tip, transition 1000, tip 5000: derived 1060 is long past ...
        assert!(decide(5_000, SAPLING, Some(1000)).is_err());
        // ... and the same facts with the server on Ironwood give exactly that height.
        assert_eq!(
            decide(5_000, IRONWOOD, Some(1000)),
            Ok(IronwoodStatus::ActiveFrom(1060))
        );
    }

    #[test]
    fn an_upcoming_height_is_reported_only_once_the_transition_is_buried() {
        // Transition 1000 buried 30 deep at tip 1030: derived 1060 lies ahead of the tip.
        assert_eq!(decide(1_030, SAPLING, Some(1000)), Ok(IronwoodStatus::ActiveFrom(1060)));
        // One block short of that depth the chain itself does not know the height yet.
        assert_eq!(decide(1_029, SAPLING, Some(1000)), Ok(IronwoodStatus::NotActivated));
    }

    #[test]
    fn a_server_on_ironwood_with_no_derivable_height_is_taken_at_its_word() {
        assert_eq!(decide(71_454, IRONWOOD, None), Ok(IronwoodStatus::ActiveFrom(71_454)));
        // A derived height above the tip contradicts a server that is already active; the server
        // is what the network will judge the transaction by.
        assert_eq!(
            decide(71_454, IRONWOOD, Some(80_000)),
            Ok(IronwoodStatus::ActiveFrom(71_454))
        );
    }

    #[test]
    fn a_server_before_ironwood_with_nothing_ahead_is_not_activated() {
        assert_eq!(decide(4_000_000, SAPLING, None), Ok(IronwoodStatus::NotActivated));
    }

    #[test]
    fn disagreement_between_the_server_and_the_timestamp_rule_is_refused() {
        let err = decide(5_000, SAPLING, Some(1000)).unwrap_err();
        assert!(matches!(err, IronwoodDiscoveryError::Inconsistent(_)), "{err}");
    }
}
