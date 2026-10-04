//! Assembling a bundle from the queue, and answering the jobs in it.

use super::worker::{Pass, Phase};
use super::{BundleItem, Job};
use crate::adapters::abi::{IBundler, IMasp};
use crate::domain::error::{AppError, AppResult};
use crate::domain::fiat_shamir::BatchChallenge;
use crate::services::fees::gas_witness::EntryPoint;
use crate::services::tree::{AdvancedState, ROOT_HISTORY, ReservedSlot, TreeMirror};
use crate::services::witness;
use std::collections::VecDeque;
use std::ops::ControlFlow;

/// The next bundle: up to `max_items` from the front of `pending`, in queue order
/// except that swaps go last. A swap's success depends on the market between
/// simulation and inclusion, and a failure invalidates every later item's tree
/// proof, so the fewer items behind a swap the better.
pub(super) fn take_bundle(pending: &mut VecDeque<Job>, max_items: usize) -> Vec<Job> {
    let n = pending.len().min(max_items);
    let mut jobs: Vec<Job> = pending.drain(..n).collect();
    jobs.sort_by_key(|j| j.item.entry() == EntryPoint::Swap);
    jobs
}

/// Answer every job whose root would leave the pool's window before its item
/// lands. A bundle of `k` evicts `k` roots, so a root of age `a` must satisfy
/// `a + k < ROOT_HISTORY`.
pub(super) fn drop_stale_roots(mirror: &TreeMirror, jobs: &mut Vec<Job>) {
    let k = jobs.len();
    let (fresh, stale): (Vec<Job>, Vec<Job>) =
        std::mem::take(jobs)
            .into_iter()
            .partition(|job| match job.item.merkle_root() {
                None => true,
                Some(root) => mirror
                    .root_age(&root)
                    .is_some_and(|age| age + k < ROOT_HISTORY),
            });
    *jobs = fresh;
    for job in stale {
        job.fail(AppError::BadRequest(
            "pubInputs.merkleRoot is not a root this relayer has held recently; \
             refresh the tree state and re-prove"
                .into(),
        ));
    }
}

/// Reserve `item`'s leaves on top of the bundle so far, with the ring slot of the
/// root it proved against.
///
/// The slot is read before the reservation, while the root is certainly still in
/// the window. Earlier items of the bundle do not move it: a written ring slot
/// only changes when `ROOT_HISTORY` advances overwrite it, which
/// [`drop_stale_roots`] has already ruled out for the whole bundle.
pub(super) fn reserve_item(
    mirror: &mut TreeMirror,
    item: &dyn BundleItem,
) -> AppResult<(ReservedSlot, AdvancedState)> {
    let anchor_index = item
        .merkle_root()
        .map(|root| mirror.anchor_index(&root))
        .transpose()?;
    let (mut slot, advanced) = mirror.reserve_and_advance_batch(&item.batch().leaves()?)?;
    slot.anchor_index = anchor_index;
    Ok((slot, advanced))
}

/// A job's reservation: where its leaves land, the root they advance the tree
/// to, and the digest and challenge of that advance.
pub(super) struct Reserved {
    pub(super) slot: ReservedSlot,
    pub(super) advanced: AdvancedState,
    pub(super) challenge: BatchChallenge,
}

/// Derive every reserved job's challenge, once per reservation. A job that
/// kept the slot its proof was made at reuses that proof's. A batch with no
/// digest fails its job.
pub(super) fn with_challenges(
    mirror: &mut TreeMirror,
    jobs: &mut Vec<Job>,
    slots: Vec<(ReservedSlot, AdvancedState)>,
) -> Phase<Vec<Reserved>> {
    let mut reserved = Vec::with_capacity(slots.len());
    for (index, (slot, advanced)) in slots.into_iter().enumerate() {
        let job = &jobs[index];
        let challenge = match job.proof_at(&slot, &advanced) {
            Some(cached) => Ok(cached.challenge),
            None => witness::challenge(&slot, &advanced, job.item.batch()),
        };
        match challenge {
            Ok(challenge) => reserved.push(Reserved {
                slot,
                advanced,
                challenge,
            }),
            Err(e) => return ControlFlow::Break(fail_one(mirror, jobs, index, e)),
        }
    }
    ControlFlow::Continue(reserved)
}

/// How many of `calls` fit in `max_tx_bytes` of `execute` calldata, or `None` if
/// all do.
pub(super) fn fits(calls: &[IBundler::Call], max_tx_bytes: usize) -> Option<usize> {
    // `execute`'s selector, the array offset and length, and per call an offset,
    // a target, a data offset and a length, plus the data itself padded to a word.
    let mut size = 4 + 32 + 32;
    for (i, c) in calls.iter().enumerate() {
        size += 32 * 4 + c.data.len().div_ceil(32) * 32;
        if size > max_tx_bytes {
            return Some(i);
        }
    }
    None
}

/// Every job's call, with `proof` choosing its tree-update proof. A call that
/// cannot be encoded fails its job.
pub(super) fn encode_calls(
    mirror: &mut TreeMirror,
    jobs: &mut Vec<Job>,
    slots: &[Reserved],
    proof: impl Fn(&Job) -> IMasp::Proof,
) -> Phase<Vec<IBundler::Call>> {
    let encoded: Result<Vec<IBundler::Call>, (usize, AppError)> = jobs
        .iter()
        .zip(slots)
        .enumerate()
        .map(|(i, (job, at))| {
            job.item
                .encode(&at.slot, &at.advanced, at.challenge.digest, proof(job))
                .map_err(|e| (i, e))
        })
        .collect();
    match encoded {
        Ok(calls) => ControlFlow::Continue(calls),
        Err((index, e)) => ControlFlow::Break(fail_one(mirror, jobs, index, e)),
    }
}

/// Answer job `index` with `e` and unwind the bundle, so the rest go round again.
/// An error that leaves the mirror unusable fails every job instead.
pub(super) fn fail_one(
    mirror: &mut TreeMirror,
    jobs: &mut Vec<Job>,
    index: usize,
    e: AppError,
) -> Pass {
    let e = mirror.abandon_bundle(e);
    if matches!(e, AppError::MirrorDesynced(_) | AppError::SubmitUnknown(_)) {
        fail_all(std::mem::take(jobs), &e);
        return Pass::Done;
    }
    jobs.remove(index).fail(e);
    Pass::Retry
}

/// Unwind the bundle after a failure that is no one job's, and answer every job
/// with it.
pub(super) fn abandon_all(mirror: &mut TreeMirror, jobs: &mut Vec<Job>, e: AppError) -> Pass {
    let e = mirror.abandon_bundle(e);
    fail_all(std::mem::take(jobs), &e);
    Pass::Done
}

pub(super) fn fail_all(jobs: Vec<Job>, e: &AppError) {
    for job in jobs {
        job.fail(AppError::mirrored(e));
    }
}

/// Split `gas_used` across items in proportion to `weights`, rounding so the
/// shares sum to exactly `gas_used`.
pub(super) fn gas_shares(gas_used: u64, weights: &[u64]) -> Vec<u64> {
    if weights.is_empty() {
        return Vec::new();
    }
    let total: u128 = weights.iter().map(|w| u128::from((*w).max(1))).sum();
    let mut shares: Vec<u64> = weights
        .iter()
        .map(|w| (u128::from(gas_used) * u128::from((*w).max(1)) / total) as u64)
        .collect();
    let assigned: u64 = shares.iter().sum();
    if let Some(last) = shares.last_mut() {
        *last += gas_used - assigned;
    }
    shares
}
