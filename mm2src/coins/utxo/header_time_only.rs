//! Time-only read of the median-time-past window (CRD ch.37 R37.5.10 to R37.5.10g).
//!
//! The list read of the median-time-past query is a cascade: step 1 is the layout-aware read of the
//! coin's [`HeaderReadPolicy`] (unchanged); when it fails, the times of the headers are taken at byte 68
//! of each header's own bytes, either from the list split at an even stride (step 2) or from each
//! header fetched on its own (step 3), and accepted only if they pass the range check (step 4). The
//! result is a list of times; nothing here is stored, hashed or linked.

use super::header_layout_fallback::{FallbackReadError, HeaderReadPolicy};
use super::rpc_clients::UtxoRpcError;
use futures::future::join_all;
use mm2_err_handle::prelude::*;
use std::future::Future;

/// 2009-01-01 00:00:00 UTC: before any chain derived from Bitcoin's code existed (R37.5.10d).
pub(crate) const EARLIEST_PLAUSIBLE_TIME: u64 = 1_230_768_000;
/// How far past the node's clock a header time may lie (R37.5.10d).
pub(crate) const CLOCK_SLACK_SECS: u64 = 86_400;

/// The shortest stride of the even split: the standard header.
const MIN_STRIDE: usize = 80;
/// The sanity ceiling of the stride (R37.5.10a step 2).
const MAX_STRIDE: usize = 4096;
/// Where the time field sits in the standard layout and in every layout this rule reads.
const TIME_OFFSET: usize = 68;

/// Which step produced the times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimeOnlyStep {
    /// Step 2, with the stride that split the list.
    EvenSplit { stride: usize },
    /// Step 3.
    Individual,
}

/// The little-endian unsigned 32-bit integer at byte 68 of the header that starts at `start`.
/// The caller guarantees that `bytes` holds at least 80 bytes from `start`.
fn time_at(bytes: &[u8], start: usize) -> u32 {
    let at = start + TIME_OFFSET;
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Step 2: the stride and the times of `count` headers served as `bytes`, or `None` when the split
/// does not apply (an even or zero `count`, a byte count not divisible by `count`, a stride outside
/// 80 to 4096).
pub(crate) fn even_split(bytes: &[u8], count: usize) -> Option<(usize, Vec<u32>)> {
    if count % 2 == 0 || bytes.len() % count != 0 {
        return None;
    }
    let stride = bytes.len() / count;
    if !(MIN_STRIDE..=MAX_STRIDE).contains(&stride) {
        return None;
    }
    let times = (0..count).map(|i| time_at(bytes, i * stride)).collect();
    Some((stride, times))
}

/// Step 4: every time must be at least 1230768000 and at most `now + 86400`. The error names the coin
/// label, the served byte count and count, and which check failed, with the offending time and the
/// clock for the two range failures.
pub(crate) fn check_times(label: &str, served_len: usize, count: usize, times: &[u32], now: u64) -> Result<(), String> {
    for &time in times {
        if (time as u64) < EARLIEST_PLAUSIBLE_TIME {
            return Err(format!(
                "{label}: the {count} headers served as {served_len} bytes were not accepted: \
                 a header time {time} is below {EARLIEST_PLAUSIBLE_TIME} (node clock {now})"
            ));
        }
        if (time as u64) > now.saturating_add(CLOCK_SLACK_SECS) {
            return Err(format!(
                "{label}: the {count} headers served as {served_len} bytes were not accepted: \
                 a header time {time} is above the node clock {now} plus {CLOCK_SLACK_SECS}"
            ));
        }
    }
    Ok(())
}

/// Step 3: fetches the headers at heights `from .. from + count` with `fetch` (all requests issued
/// together) and takes each header's time: the lone-header read of `policy` if it parses, otherwise
/// the time at byte 68 of the served bytes. A failed request or a header shorter than 80 bytes fails
/// the step; there is no partial result.
async fn individual_times<F, Fut>(
    policy: &HeaderReadPolicy,
    label: &str,
    served_len: usize,
    count: usize,
    from: u64,
    fetch: &F,
) -> Result<Vec<u32>, String>
where
    F: Fn(u64) -> Fut,
    Fut: Future<Output = Result<Vec<u8>, MmError<UtxoRpcError>>>,
{
    let fetched = join_all((0..count as u64).map(|i| fetch(from + i))).await;
    let mut times = Vec::with_capacity(count);
    for (height, served) in (from..).zip(fetched) {
        let Ok(bytes) = served else {
            return Err(format!(
                "{label}: the {count} headers served as {served_len} bytes were read one by one and the request \
                 for the header at height {height} failed"
            ));
        };
        if bytes.len() < MIN_STRIDE {
            return Err(format!(
                "{label}: the {count} headers served as {served_len} bytes were read one by one and the header \
                 at height {height} is {} bytes, shorter than {MIN_STRIDE}",
                bytes.len()
            ));
        }
        let time = match policy.read_header(&bytes) {
            Ok(header) => header.time,
            Err(_) => time_at(&bytes, 0),
        };
        times.push(time);
    }
    Ok(times)
}

/// Reads the times of the `count` headers that a `blockchain.block.headers` request from height `from`
/// served as `bytes` (R37.5.10a). `fetch(height)` returns the bytes of one header
/// (`blockchain.block.header`); `now` is the node's wall clock in seconds; `label` is the coin ticker.
///
/// Step 1 is `policy.read_list`; if it succeeds its times are returned as before. The steps 2 to 4 run
/// only when it fails, and the failure is a typed transient error of the query.
pub(crate) async fn read_window_times<F, Fut>(
    policy: &HeaderReadPolicy,
    label: &str,
    bytes: &[u8],
    count: usize,
    from: u64,
    now: u64,
    fetch: F,
) -> Result<Vec<u32>, MmError<UtxoRpcError>>
where
    F: Fn(u64) -> Fut,
    Fut: Future<Output = Result<Vec<u8>, MmError<UtxoRpcError>>>,
{
    let invalid = |reason: String| MmError::new(UtxoRpcError::InvalidResponse(reason));
    let split = match policy.read_list(bytes, count) {
        Ok(headers) => return Ok(headers.into_iter().map(|header| header.time).collect()),
        // The ambiguity of R37.5.9e is not split: the individual read resolves it.
        Err(FallbackReadError::Ambiguous(_)) => None,
        Err(FallbackReadError::Rejected(_)) => even_split(bytes, count),
    };
    let (step, times) = match split {
        Some((stride, times)) => (TimeOnlyStep::EvenSplit { stride }, times),
        None => {
            let times = individual_times(policy, label, bytes.len(), count, from, &fetch)
                .await
                .map_err(invalid)?;
            (TimeOnlyStep::Individual, times)
        },
    };
    check_times(label, bytes.len(), count, &times, now).map_err(invalid)?;
    policy.time_only_chosen(label, step);
    Ok(times)
}
