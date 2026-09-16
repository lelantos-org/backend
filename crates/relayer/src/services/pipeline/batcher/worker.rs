//! The per-chain task: take a bundle off the queue, reserve, check, prove and
//! send it, and act on what the chain kept.

use super::bundle::{
    abandon_all, drop_stale_roots, encode_calls, fail_all, fail_one, fits, gas_shares,
    reserve_item, take_bundle,
};
use super::dry_run::accept_all_at;
use super::outcome::{
    ItemFailure, classify, decode_execute, decode_logs, execute_calldata, zero_proof,
};
use super::{BatcherCfg, BundledReceipt, CachedProof, HoldGate, Job, ResyncCtx};
use crate::adapters::abi::IBundler;
use crate::adapters::calldata::build_tu_proof;
use crate::domain::error::{AppError, AppResult, revert_reason};
use crate::services::fees::gas_witness::{EntryPoint, GasWitness};
use crate::services::submitter::{SubmissionReceipt, Submitter};
use crate::services::tree::{AdvancedState, ReservedSlot, TreeMirror};
use alloy::primitives::Bytes;
use alloy::rpc::types::state::StateOverride;
use crypto::tree::Field;
use groth16::{Priority, TreeUpdateBatchProver};
use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Resync attempts after the chain moved without this relayer, spaced by
/// [`RESYNC_BACKOFF`]. The indexer is usually a few blocks behind the chain, and a
/// resync can only succeed once it has caught up.
const RESYNC_ATTEMPTS: u32 = 30;
const RESYNC_BACKOFF: Duration = Duration::from_secs(2);

/// Upper bound on the random delay before retrying after a lost race, so two
/// relayers that collided do not collide again in lockstep.
const RETRY_JITTER_MS: u64 = 1_000;

/// Doublings tried on a bundle's gas limit before giving up; see
/// [`Worker::gas_limit`]. Starting from the items' expected gas, three reach eight
/// times it.
const GAS_SEARCH_ROUNDS: u32 = 4;

pub(super) struct Worker {
    chain_id: i64,
    mirror: Arc<Mutex<TreeMirror>>,
    prover: Arc<dyn TreeUpdateBatchProver>,
    submitter: Arc<Submitter>,
    max_items: usize,
    linger: Duration,
    max_tx_bytes: usize,
    gas_witness: Arc<GasWitness>,
    overrides: Option<StateOverride>,
    resync: ResyncCtx,
    rx: mpsc::UnboundedReceiver<Job>,
    gate: Arc<HoldGate>,
    /// Items waiting for a bundle, oldest first. Retried items are pushed back to
    /// the front.
    pending: VecDeque<Job>,
    /// Consecutive stale-root reports from a node behind the mirror; see
    /// [`Worker::resync`].
    lagging: u32,
}

/// How one pass over a bundle ended.
pub(super) enum Pass {
    /// Every job has been answered or sent back to the queue.
    Done,
    /// Jobs remain for another pass: one was answered, or those that did not fit
    /// were sent back to the queue.
    Retry,
}

/// One phase of [`Worker::pass`]. `Continue` hands its result to the next phase;
/// `Break` ends the pass, after the phase has answered or requeued what it must.
pub(super) type Phase<T = ()> = ControlFlow<Pass, T>;

impl Worker {
    pub(super) fn new(
        cfg: BatcherCfg,
        rx: mpsc::UnboundedReceiver<Job>,
        gate: Arc<HoldGate>,
    ) -> Self {
        Self {
            chain_id: cfg.chain_id,
            mirror: cfg.mirror,
            prover: cfg.prover,
            submitter: cfg.submitter,
            max_items: cfg.max_items,
            linger: cfg.linger,
            max_tx_bytes: cfg.max_tx_bytes,
            gas_witness: cfg.gas_witness,
            overrides: cfg.dry_run_verifiers.map(accept_all_at),
            resync: cfg.resync,
            rx,
            gate,
            pending: VecDeque::new(),
            lagging: 0,
        }
    }

    pub(super) async fn run(mut self) {
        loop {
            if self.pending.is_empty() {
                let Some(job) = self.rx.recv().await else {
                    return;
                };
                self.pending.push_back(job);
            }
            if !self.linger.is_zero() {
                tokio::time::sleep(self.linger).await;
            }
            self.drain();
            if !self.wait_while_held().await {
                return;
            }
            let jobs = take_bundle(&mut self.pending, self.max_items);
            self.publish_queue();
            self.run_bundle(jobs).await;
        }
    }

    /// Keep queueing while a test hook holds dispatch. `false` once every handle
    /// is gone.
    async fn wait_while_held(&mut self) -> bool {
        let gate = self.gate.clone();
        loop {
            // Registered before the flag is read, so a release landing in
            // between still wakes this wait.
            let released = gate.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if !gate.held.load(Ordering::SeqCst) {
                return true;
            }
            self.publish_queue();
            tokio::select! {
                _ = &mut released => {}
                job = self.rx.recv() => match job {
                    Some(job) => self.pending.push_back(job),
                    None => return false,
                },
            }
            self.drain();
        }
    }

    fn drain(&mut self) {
        while let Ok(job) = self.rx.try_recv() {
            self.pending.push_back(job);
        }
    }

    fn publish_queue(&self) {
        if let Ok(mut q) = self.gate.queue.lock() {
            *q = self.pending.iter().map(|j| j.item.view()).collect();
        }
    }

    fn requeue_front(&mut self, jobs: Vec<Job>) {
        for job in jobs.into_iter().rev() {
            self.pending.push_front(job);
        }
    }

    async fn run_bundle(&mut self, mut jobs: Vec<Job>) {
        let mirror_arc = self.mirror.clone();
        let mut mirror = mirror_arc.lock().await;
        // Every pass answers a job, shrinks the bundle or finishes it, so this
        // bounds the loop at one pass per job plus the last.
        for _ in 0..=jobs.len() {
            if jobs.is_empty() {
                return;
            }
            match self.pass(&mut mirror, &mut jobs).await {
                Pass::Done => return,
                Pass::Retry => continue,
            }
        }
        for job in jobs {
            job.fail(AppError::Internal(format!(
                "chain {}: bundle did not converge",
                self.chain_id
            )));
        }
    }

    /// Reserve, check, prove and send `jobs` once.
    async fn pass(&mut self, mirror: &mut TreeMirror, jobs: &mut Vec<Job>) -> Pass {
        match self.send_bundle(mirror, jobs).await {
            ControlFlow::Continue(receipt) => {
                self.landed(mirror, std::mem::take(jobs), receipt).await;
                Pass::Done
            }
            ControlFlow::Break(outcome) => outcome,
        }
    }

    /// Every phase up to a mined bundle, in order.
    async fn send_bundle(
        &mut self,
        mirror: &mut TreeMirror,
        jobs: &mut Vec<Job>,
    ) -> Phase<SubmissionReceipt> {
        let slots = self.reserve(mirror, jobs).await?;

        let dummy = encode_calls(mirror, jobs, &slots, |_| zero_proof())?;
        self.check_size(mirror, jobs, &dummy)?;
        self.prove_all(mirror, jobs, &slots, dummy).await?;

        // Every job holds a proof for its slot once proving went through.
        let calls = encode_calls(mirror, jobs, &slots, |job| {
            job.proof
                .as_ref()
                .map_or_else(zero_proof, |p| p.proof.clone())
        })?;
        let data = execute_calldata(calls);
        self.simulate(mirror, jobs, &data).await?;
        let gas_limit = self.gas_limit(mirror, jobs, &data).await?;

        match self.submitter.submit(data, gas_limit).await {
            Ok(receipt) => ControlFlow::Continue(receipt),
            Err(e) => ControlFlow::Break(abandon_all(mirror, jobs, e)),
        }
    }

    /// Open a bundle and reserve every job in it, in order. A reservation that
    /// fails changes nothing, so its job is answered and the rest carry on.
    async fn reserve(
        &mut self,
        mirror: &mut TreeMirror,
        jobs: &mut Vec<Job>,
    ) -> Phase<Vec<(ReservedSlot, AdvancedState)>> {
        drop_stale_roots(mirror, jobs);
        if jobs.is_empty() {
            return ControlFlow::Break(Pass::Done);
        }
        if mirror.is_desynced() {
            // Parked by an unknown outcome or a failed resync. The chain is the
            // arbiter, so try to re-adopt its state before refusing work.
            self.resync(mirror).await;
        }
        if let Err(e) = mirror.begin_bundle() {
            fail_all(std::mem::take(jobs), &e);
            return ControlFlow::Break(Pass::Done);
        }

        let mut slots = Vec::with_capacity(jobs.len());
        let mut reserved = Vec::with_capacity(jobs.len());
        for job in std::mem::take(jobs) {
            match reserve_item(mirror, job.item.as_ref()) {
                Ok(slot) => {
                    slots.push(slot);
                    reserved.push(job);
                }
                Err(e) => job.fail(e),
            }
        }
        *jobs = reserved;
        if jobs.is_empty() {
            let _ = mirror.rollback_bundle();
            return ControlFlow::Break(Pass::Done);
        }
        ControlFlow::Continue(slots)
    }

    /// Send back the jobs that take the bundle past `max_tx_bytes`, and answer a
    /// job too large to travel even alone.
    fn check_size(
        &mut self,
        mirror: &mut TreeMirror,
        jobs: &mut Vec<Job>,
        calls: &[IBundler::Call],
    ) -> Phase {
        let Some(fit) = fits(calls, self.max_tx_bytes) else {
            return ControlFlow::Continue(());
        };
        let _ = mirror.rollback_bundle();
        if fit == 0 {
            jobs.remove(0).fail(AppError::BadRequest(format!(
                "operation exceeds this chain's max_tx_bytes ({})",
                self.max_tx_bytes
            )));
        } else {
            let rest = jobs.split_off(fit);
            self.requeue_front(rest);
        }
        ControlFlow::Break(Pass::Retry)
    }

    /// Prove every job while the dry run checks the bundle with `dummy` proofs.
    ///
    /// The dry run is read between proofs, so a failing job stops the proving
    /// before its successors are proved. The jobs ahead of it keep their slots on
    /// the next pass, and with them their proofs.
    async fn prove_all(
        &mut self,
        mirror: &mut TreeMirror,
        jobs: &mut Vec<Job>,
        slots: &[(ReservedSlot, AdvancedState)],
        dummy: Vec<IBundler::Call>,
    ) -> Phase {
        let mut dry_run = self.start_dry_run(dummy);
        let mut failure: Option<ItemFailure> = None;
        for index in 0..jobs.len() {
            if dry_run.as_ref().is_some_and(|h| h.is_finished()) {
                failure = self.read_dry_run(dry_run.take(), jobs.len()).await;
            }
            if failure.as_ref().is_some_and(|f| f.index <= index) {
                break;
            }
            if let Err(e) = self.prove(&mut jobs[index], &slots[index]).await {
                return ControlFlow::Break(fail_one(mirror, jobs, index, e));
            }
        }
        if failure.is_none() {
            failure = self.read_dry_run(dry_run.take(), jobs.len()).await;
        }
        match failure {
            Some(f) => ControlFlow::Break(self.item_failed(mirror, jobs, f).await),
            None => ControlFlow::Continue(()),
        }
    }

    async fn prove(&self, job: &mut Job, slot: &(ReservedSlot, AdvancedState)) -> AppResult<()> {
        let (slot, advanced) = slot;
        if let Some(p) = job.proof.as_ref()
            && p.start_index == slot.start_index
            && p.old_root == slot.old_root
            && p.new_root == advanced.new_root
        {
            return Ok(());
        }
        let witness = job.item.witness(slot, advanced);
        let proof = self.prover.prove(witness, Priority::Spend).await?;
        job.proof = Some(CachedProof {
            start_index: slot.start_index,
            old_root: slot.old_root,
            new_root: advanced.new_root,
            proof: build_tu_proof(&proof)?,
        });
        Ok(())
    }

    /// `eth_call` the bundle with the verifiers stubbed, in the background; `None`
    /// when this chain does not dry-run.
    fn start_dry_run(
        &self,
        calls: Vec<IBundler::Call>,
    ) -> Option<JoinHandle<Result<Bytes, String>>> {
        let overrides = self.overrides.clone()?;
        let submitter = self.submitter.clone();
        let data = execute_calldata(calls);
        Some(tokio::spawn(async move {
            submitter.simulate(data, Some(&overrides), None).await
        }))
    }

    /// The dry run's verdict: the first failing item, or `None` if every item
    /// passed or the run could not say.
    async fn read_dry_run(
        &self,
        handle: Option<JoinHandle<Result<Bytes, String>>>,
        total: usize,
    ) -> Option<ItemFailure> {
        let out = match handle?.await {
            Ok(Ok(out)) => out,
            Ok(Err(text)) => {
                warn!(chain_id = self.chain_id, error = %text, "dry run unavailable");
                return None;
            }
            Err(e) => {
                warn!(chain_id = self.chain_id, error = %e, "dry run task failed");
                return None;
            }
        };
        decode_execute(&out, total)
    }

    /// Simulate the proved bundle once more against the latest state.
    async fn simulate(
        &mut self,
        mirror: &mut TreeMirror,
        jobs: &mut Vec<Job>,
        data: &[u8],
    ) -> Phase {
        match self.submitter.simulate(data.to_vec(), None, None).await {
            Ok(out) => match decode_execute(&out, jobs.len()) {
                Some(f) => ControlFlow::Break(self.item_failed(mirror, jobs, f).await),
                None => ControlFlow::Continue(()),
            },
            Err(text) => match revert_reason(&text) {
                // `execute` itself reverted: an operator or target misconfiguration,
                // which no retry fixes.
                Some(reason) => {
                    let e = AppError::Internal(format!(
                        "chain {}: Bundler.execute reverted: {reason}",
                        self.chain_id
                    ));
                    ControlFlow::Break(abandon_all(mirror, jobs, e))
                }
                // A transport failure says nothing about the bundle; the submission's
                // own gas estimate is the next check.
                None => {
                    warn!(chain_id = self.chain_id, error = %text, "bundle simulation unavailable");
                    ControlFlow::Continue(())
                }
            },
        }
    }

    /// A gas limit at which every item of the bundle executes.
    ///
    /// `execute` absorbs a failing item, out-of-gas included, so `eth_estimateGas`
    /// finds the least gas at which the outer call succeeds, typically one where
    /// the first item's verifier runs dry. The limit is searched on `execute`'s own
    /// return instead: start from the items' expected gas, which the gas witness
    /// keeps at its high-water mark, pad it, and double until a simulation capped at
    /// that limit runs every item. The uncapped simulation just passed, so running
    /// short here is a question of gas.
    async fn gas_limit(
        &mut self,
        mirror: &mut TreeMirror,
        jobs: &mut Vec<Job>,
        data: &[u8],
    ) -> Phase<u64> {
        let total = jobs.len();
        let expected: u64 = jobs
            .iter()
            .map(|j| j.item.gas_weight(&self.gas_witness))
            .sum();
        let mut limit = expected.saturating_add(expected / 10);
        let mut last = None;
        for _ in 0..GAS_SEARCH_ROUNDS {
            match self
                .submitter
                .simulate(data.to_vec(), None, Some(limit))
                .await
            {
                Ok(out) => match decode_execute(&out, total) {
                    None => return ControlFlow::Continue(limit),
                    Some(f) => last = Some(f),
                },
                Err(text) => {
                    warn!(chain_id = self.chain_id, limit, error = %text, "bundle simulation at a gas limit failed")
                }
            }
            limit = limit.saturating_mul(2);
        }
        match last {
            Some(f) => ControlFlow::Break(self.item_failed(mirror, jobs, f).await),
            None => ControlFlow::Break(abandon_all(
                mirror,
                jobs,
                AppError::Rpc(format!(
                    "chain {}: no gas limit up to {limit} could be checked for the bundle",
                    self.chain_id
                )),
            )),
        }
    }

    /// Item `f.index` would fail or did fail on chain.
    async fn item_failed(
        &mut self,
        mirror: &mut TreeMirror,
        jobs: &mut Vec<Job>,
        f: ItemFailure,
    ) -> Pass {
        let failure = classify(&f.reason);
        let stale_head = f.index == 0 && failure.stale_root;
        if stale_head && self.keep_external_flushes(mirror, jobs, None).await {
            return if jobs.is_empty() {
                Pass::Done
            } else {
                Pass::Retry
            };
        }
        let _ = mirror.rollback_bundle();
        if stale_head {
            self.resync_and_requeue(mirror, std::mem::take(jobs)).await;
            return Pass::Done;
        }
        let total = jobs.len();
        if f.index >= total {
            fail_all(
                std::mem::take(jobs),
                &AppError::Internal(format!(
                    "chain {}: bundle reported failure at item {} of {total}",
                    self.chain_id, f.index
                )),
            );
            return Pass::Done;
        }
        jobs.remove(f.index).fail(failure.error);
        Pass::Retry
    }

    /// A bundle mined. Keep what executed, answer those callers, fail the item
    /// the chain stopped at and requeue the rest.
    ///
    /// A receipt without the Bundler's `BundleExecuted` says nothing about how
    /// far the bundle got, since `execute` always emits it when it returns, so
    /// the outcome is treated as unknown: the mirror parks and resyncs.
    async fn landed(
        &mut self,
        mirror: &mut TreeMirror,
        mut jobs: Vec<Job>,
        receipt: SubmissionReceipt,
    ) {
        let total = jobs.len();
        let (executed, reason) = decode_logs(&receipt, self.submitter.target, total);
        let Some(executed) = executed else {
            let e = AppError::SubmitUnknown(format!(
                "chain {}: tx {} mined without the Bundler's BundleExecuted event",
                self.chain_id, receipt.tx_hash
            ));
            abandon_all(mirror, &mut jobs, e);
            return;
        };
        let failure = (executed < total).then(|| classify(&reason.unwrap_or_default()));
        let stale_head = executed == 0 && failure.as_ref().is_some_and(|f| f.stale_root);
        if stale_head
            && self
                .keep_external_flushes(mirror, &mut jobs, u64::try_from(receipt.block_number).ok())
                .await
        {
            self.requeue_front(jobs);
            return;
        }
        if let Err(e) = mirror.commit_prefix(executed) {
            error!(chain_id = self.chain_id, error = %e, "could not keep the landed prefix");
        }
        if executed > 0 {
            self.lagging = 0;
        }

        let mut rest = jobs.split_off(executed);
        let weights: Vec<u64> = jobs
            .iter()
            .map(|j| j.item.gas_weight(&self.gas_witness))
            .collect();
        let shares = gas_shares(receipt.gas_used, &weights);
        for (index, (job, share)) in jobs.into_iter().zip(shares).enumerate() {
            if let Some(guard) = job.guard.as_ref() {
                guard.spent().await;
            }
            let _ = job.reply.send(Ok(BundledReceipt {
                receipt: SubmissionReceipt {
                    gas_used: share,
                    ..receipt.clone()
                },
                index,
                bundle_size: total,
            }));
        }
        info!(
            chain_id = self.chain_id,
            tx_hash = %receipt.tx_hash,
            executed,
            total,
            gas_used = receipt.gas_used,
            "bundle landed"
        );

        let Some(failure) = failure else {
            return;
        };
        if stale_head {
            self.resync_and_requeue(mirror, rest).await;
        } else {
            rest.remove(0).fail(failure.error);
            self.requeue_front(rest);
        }
    }

    /// Keep the open bundle's leading flushes that already landed without this
    /// relayer, when its first item failed on a stale root. `flushBatch` is
    /// permissionless, and the same deposits flushed in the same order reach the
    /// same root, so the chain may hold this bundle's own flushes.
    ///
    /// Reads the chain's root, at `block` when given so a later transaction
    /// cannot move it, and looks for the bundle prefix that reaches it. A prefix
    /// of flushes only is committed and its jobs answered `LandedExternally`;
    /// anything else leaves the bundle open. Returns whether a prefix was kept.
    async fn keep_external_flushes(
        &self,
        mirror: &mut TreeMirror,
        jobs: &mut Vec<Job>,
        block: Option<u64>,
    ) -> bool {
        let flushes = jobs
            .iter()
            .take_while(|j| j.item.entry() == EntryPoint::Flush)
            .count();
        if flushes == 0 {
            return false;
        }
        let root = match self.chain_root(block).await {
            Ok(root) => root,
            Err(e) => {
                warn!(chain_id = self.chain_id, error = %e, "currentRoot unavailable");
                return false;
            }
        };
        let Some(kept) = mirror
            .bundle_prefix_reaching(&root)
            .filter(|&k| k <= flushes)
        else {
            return false;
        };
        if let Err(e) = mirror.commit_prefix(kept) {
            error!(chain_id = self.chain_id, error = %e, "could not keep the external flushes");
        }
        info!(
            chain_id = self.chain_id,
            kept, "flushes already landed without this relayer"
        );
        for job in jobs.drain(..kept) {
            job.fail(AppError::LandedExternally(format!(
                "chain {}: the chain already holds this flush's root",
                self.chain_id
            )));
        }
        true
    }

    /// The chain moved without this relayer: resync, then send `jobs` round
    /// again, or fail them if the mirror could not follow.
    async fn resync_and_requeue(&mut self, mirror: &mut TreeMirror, jobs: Vec<Job>) {
        self.resync(mirror).await;
        if mirror.is_desynced() {
            fail_all(
                jobs,
                &AppError::MirrorDesynced(format!(
                    "chain {}: could not resync after the chain root moved",
                    self.chain_id
                )),
            );
        } else {
            self.requeue_front(jobs);
        }
    }

    /// Re-adopt the chain's state, retrying while the indexer catches up, then
    /// wait a random moment so a relayer that lost a race does not lose the next
    /// one the same way.
    ///
    /// A node that has not yet seen this relayer's last bundle reports a stale root
    /// too, and the indexer trails further still, so rebuilding from it would
    /// throw away a correct mirror. While the chain's root is one the mirror has
    /// held, the report is taken as lag and only retried; a reorg that dropped a
    /// bundle looks the same, so a lag that persists resyncs after all.
    async fn resync(&mut self, mirror: &mut TreeMirror) {
        if !mirror.is_desynced() && self.lagging < RESYNC_ATTEMPTS {
            match self.chain_root(None).await {
                Ok(root) if mirror.root_age(&root).is_some() => {
                    self.lagging += 1;
                    warn!(
                        chain_id = self.chain_id,
                        lagging = self.lagging,
                        "node is behind this relayer's own bundles; retrying"
                    );
                    tokio::time::sleep(jitter()).await;
                    return;
                }
                Ok(_) => {}
                Err(e) => warn!(chain_id = self.chain_id, error = %e, "currentRoot unavailable"),
            }
        }
        self.lagging = 0;
        warn!(
            chain_id = self.chain_id,
            "chain root moved under the mirror; resyncing"
        );
        for attempt in 1..=RESYNC_ATTEMPTS {
            match mirror
                .resync(
                    &self.resync.pool,
                    &self.resync.masp,
                    "chain root moved without this relayer",
                )
                .await
            {
                Ok(()) => break,
                Err(e) => {
                    warn!(chain_id = self.chain_id, attempt, error = %e, "resync not yet possible");
                    tokio::time::sleep(RESYNC_BACKOFF).await;
                }
            }
        }
        tokio::time::sleep(jitter()).await;
    }

    /// The pool's `currentRoot`, at `block` or the latest.
    async fn chain_root(&self, block: Option<u64>) -> AppResult<Field> {
        self.resync.masp.current_root(block).await
    }
}

/// A random delay below [`RETRY_JITTER_MS`].
fn jitter() -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    Duration::from_millis(u64::from(nanos) % RETRY_JITTER_MS)
}
