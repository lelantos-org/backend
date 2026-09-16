//! Path 1: the rate from this deployment's own record of the index.

use crate::domain::apy::annualize_bps;
use crate::repositories::yield_samples;
use asset_registry::{ApyEstimate, AssetRow, bigdecimal_to_u256};

/// The rate from this deployment's own record of the index.
///
/// The preferred path, and the only one that keeps working on a node without
/// archive state. Nothing is corrected for the pool's cut here: the index is
/// already what a note is worth, so two of them difference to what a holder
/// earned rather than to what the venue paid.
///
/// Free-standing and given its sample rather than fetching one: the whole chain's
/// history arrives in a single query, so this is arithmetic with no I/O in it.
pub(super) fn rate(a: &AssetRow, sample: Option<&yield_samples::Sample>) -> Option<ApyEstimate> {
    let sample = sample?;
    let now = bigdecimal_to_u256(a.index_ray.as_ref()?).ok()?;
    let then = bigdecimal_to_u256(&sample.index_ray).ok()?;
    Some(ApyEstimate {
        bps: annualize_bps(now, then, sample.elapsed_s)?,
        window_s: sample.elapsed_s,
    })
}
