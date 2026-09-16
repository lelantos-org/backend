//! The flush worker's pipeline: pending escrowed deposits into `flushBatch`.
//!
//! - `preflight`: the pure decision table for one deposit.
//! - `failures`: what the worker refuses to batch, and for how long.
//! - `plan`: one tick's batch, as the batcher bundles it.

pub mod failures;
mod plan;
pub mod preflight;

use crate::adapters::masp::MaspReader;
use crate::domain::deposit::PendingDeposit;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::EstimateResponse;
use crate::repositories::deposit_escrowed_events::DepositMempool;
use crate::services::events::{DepositEvent, EventBroadcaster};
use crate::services::fees::gas_witness::{EntryPoint, GasWitness};
use crate::services::fees::quote::FeeQuoter;
use crate::services::fees::shielded::ShieldedFeeChecker;
use crate::services::fees::shielded::deposit_note::{FeeNote, assess};
use crate::services::pipeline::batcher::Batcher;
use crate::services::pipeline::transact::FeeContext;
use crate::services::submitter::SubmissionReceipt;
use ::asset_registry::AssetRegistry;
use alloy::primitives::B256;
use failures::DepositFailures;
use plan::{BatchPlan, FlushItem};
use preflight::{FeeGate, Verdict, classify};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tracing::{error, info, instrument, warn};

/// Pending rows one tick reads while looking for deposits it can batch.
///
/// Only a chain whose oldest deposits are quarantined or deferred scans past the
/// first page, and this bounds what that costs. A backlog deeper than this is
/// walked over successive ticks, oldest first, since the deposits ahead of it
/// stay excluded.
const MAX_SCAN_PER_TICK: usize = 512;

pub struct FlushPipeline {
    pub chain_id: i64,
    /// The chain's batcher, shared with spends and swaps, so a flush can land in
    /// the same transaction as their operations.
    pub batcher: Batcher,
    pub mempool: Arc<DepositMempool>,
    /// The pool: `flushBatch`'s target, and where pre-flight reads escrow slots.
    pub masp: MaspReader,
    pub max_n: usize,
    /// How long a batch smaller than `max_n` waits for more deposits; zero flushes
    /// it on the tick that finds it.
    pub partial_after: Duration,
    /// When the current partial batch was first seen, keyed by its oldest deposit.
    pub partial_since: StdMutex<Option<(u64, Instant)>>,
    pub events: Arc<EventBroadcaster>,
    pub failures: DepositFailures,
    /// Absent means this chain subsidises flushes: the fee leaf is still minted,
    /// but nothing here inspects it.
    pub shielded_fee: Option<Arc<ShieldedFeeChecker>>,
    pub gas_witness: Arc<GasWitness>,
    /// Held for `/v1/deposit/estimate`, which prices the same `EntryPoint::Flush`
    /// the fee gate does, so a wallet is quoted what `preflight` later requires.
    pub fee_quoter: Arc<FeeQuoter>,
    pub assets: Arc<AssetRegistry>,
}

impl FlushPipeline {
    /// What a deposit must pay this relayer to be flushed.
    ///
    /// Priced against `EntryPoint::Flush`, this deposit's share of a future
    /// `flushBatch`, rather than against a transaction the caller is about to send.
    /// It is the number `fee_gate` holds the deposit to, so a wallet can build a
    /// note that will be accepted.
    pub async fn estimate(&self) -> AppResult<EstimateResponse> {
        self.fees()
            .quote(self.gas_witness.gas_for(EntryPoint::Flush))
            .await
    }

    /// This chain's quoting and charging context, as `SpendPipeline` and
    /// `SwapPipeline` build it.
    fn fees(&self) -> FeeContext<'_> {
        FeeContext {
            chain_id: self.chain_id,
            fee_quoter: &self.fee_quoter,
            assets: &self.assets,
            shielded_fee: self.shielded_fee.as_deref(),
        }
    }

    /// One flush attempt. Returns `Ok(None)` if no pending deposits.
    ///
    /// A deposit is two leaves, the depositor's note and the note paying whoever
    /// flushes it, so `n` deposits advance the tree by `2n` leaves and
    /// `actualCount = 2n`. The contract enforces the doubling, so an odd leaf count
    /// is a `BadBatchSize` rather than a valid batch.
    #[instrument(skip_all, fields(chain_id = self.chain_id, n))]
    pub async fn tick(&self) -> AppResult<Option<B256>> {
        let limit = self.failures.batch_limit(self.max_n);
        self.failures.begin_tick();
        // Quarantined and deferred deposits are the oldest pending rows, so the
        // query looks past them rather than letting them fill the window; see
        // `DepositMempool::pop_pending`.
        let pending = self
            .mempool
            .pop_pending(limit, &self.failures.excluded_ids(), MAX_SCAN_PER_TICK)
            .await?;
        if pending.is_empty() {
            return Ok(None);
        }
        // Everything below costs a `tree_update_batch` Groth16 and a transaction,
        // and `flushBatch` is all-or-nothing, so deposits that cannot land are
        // dropped first.
        let pending = self.preflight(pending).await?;
        if pending.is_empty() || !self.ready_to_flush(&pending, limit) {
            return Ok(None);
        }
        let plan = Arc::new(BatchPlan::new(&pending));
        let n = plan.n;
        tracing::Span::current().record("n", n);
        info!("flush batch starting");

        // The batcher reserves, proves and submits, possibly alongside spends and
        // swaps. A failure it reports goes to `DepositFailures`, which decides
        // whether the batch is to blame.
        let item = FlushItem {
            plan: plan.clone(),
            pool: self.masp.address(),
        };
        let receipt = match self.batcher.submit(Box::new(item), None).await {
            Ok(bundled) => bundled.receipt,
            // Someone else flushed exactly this batch first. The deposits landed,
            // so nothing counts against them; there is no receipt of this
            // relayer's to record, and the next tick's preflight finds their
            // escrow slots empty while the indexer catches up.
            Err(AppError::LandedExternally(why)) => {
                info!(why = %why, "flush batch already landed without this relayer");
                return Ok(None);
            }
            Err(e) => {
                self.failures.note_failure(&plan.ids, &e);
                return Err(e);
            }
        };
        self.failures.note_success();
        self.record_flush(&plan, &receipt).await;
        Ok(Some(receipt.tx_hash))
    }

    /// Whether `pending` should be flushed now rather than waiting for more
    /// deposits to fill the batch.
    ///
    /// A full batch always goes. A partial one waits `partial_after` from when this
    /// worker first saw its oldest deposit, so a lone deposit is still flushed
    /// while a burst shares one proof and one call.
    fn ready_to_flush(&self, pending: &[PendingDeposit], limit: usize) -> bool {
        if pending.len() >= limit || self.partial_after.is_zero() {
            return true;
        }
        let oldest = pending[0].id;
        let Ok(mut since) = self.partial_since.lock() else {
            return true;
        };
        match *since {
            Some((id, at)) if id == oldest => at.elapsed() >= self.partial_after,
            _ => {
                *since = Some((oldest, Instant::now()));
                false
            }
        }
    }

    /// Everything a landed `flushBatch` leaves behind: the optimistic ledger mark,
    /// the gas observation the next quote is built on, and the SSE events.
    ///
    /// None of it can fail the flush — the transaction has already confirmed — so
    /// each step reports and moves on rather than propagating.
    async fn record_flush(&self, plan: &BatchPlan, receipt: &SubmissionReceipt) {
        self.mark_submitted(plan, receipt.block_number).await;

        // Per deposit, since that is what a deposit's fee note is quoted against,
        // while `gas_used` is this flush's share of the bundle it landed in.
        // Recorded only on a confirmed submission, so a reverted flush cannot move
        // the quote.
        let gas_per_deposit = receipt.gas_used / plan.n as u64;
        self.gas_witness.observe(EntryPoint::Flush, gas_per_deposit);

        let tx_hash_hex = format!("0x{}", hex::encode(receipt.tx_hash));
        info!(
            tx_hash = %tx_hash_hex,
            gas_used = receipt.gas_used,
            gas_per_deposit = gas_per_deposit,
            // Circuit units, summed over the batch. The gas above came out of the
            // relayer's own account and this is what came back. The two are in
            // different units, and converting between them needs the asset's scale
            // and a price, so this is a raw pair to reconcile from rather than a
            // margin.
            fees_collected = plan.fees_collected,
            block = receipt.block_number,
            "flushBatch submitted"
        );
        for id in &plan.ids {
            self.events.publish(DepositEvent::Flushed {
                deposit_id: *id,
                chain_id: self.chain_id,
                tx_hash: tx_hash_hex.clone(),
                block_number: receipt.block_number,
            });
        }
    }

    /// Claim the flushed rows optimistically, keeping these ids out of later
    /// `pop_pending` calls until the indexer observes the on-chain
    /// `DepositFlushed` event and overwrites with the canonical block number.
    ///
    /// Claiming fewer rows than were batched has two very different causes, so the
    /// ledger is re-read to tell them apart rather than warning about both.
    async fn mark_submitted(&self, plan: &BatchPlan, block_number: i64) {
        let claimed = match self.mempool.mark_submitted(&plan.ids, block_number).await {
            Ok(claimed) => claimed,
            Err(e) => {
                warn!(error = %e, "flush mark_submitted failed (ingester will catch up)");
                return;
            }
        };
        if claimed == plan.n {
            return;
        }
        // The indexer writes the canonical flush unconditionally and `submit`
        // waits for a confirmation, so the indexer often wins this race. Nothing
        // is lost: its row is the authoritative one.
        match self.mempool.count_unflushed(&plan.ids).await {
            Ok(0) => info!(
                claimed,
                batched = plan.n,
                "flush rows already marked flushed by the indexer"
            ),
            Ok(unflushed) => warn!(
                claimed,
                batched = plan.n,
                unflushed,
                "flush claimed fewer deposits than it submitted; another relayer may share this chain"
            ),
            Err(e) => warn!(
                claimed,
                batched = plan.n,
                error = %e,
                "flush claimed fewer deposits than it submitted; could not tell indexer race from a second relayer"
            ),
        }
    }

    /// Drop deposits `flushBatch` would refuse, before the prover runs.
    ///
    /// The contract keeps only `escrowed[id]`, a digest over every field the
    /// relayer replays. Reading it back and re-deriving the digest locally
    /// reproduces the per-deposit guards in `_drainDeposit` at one `eth_call` each
    /// rather than one wasted Groth16 per tick. [`classify`] holds the decision
    /// table and this applies it.
    ///
    /// An RPC failure aborts the tick rather than rejecting anything: a deposit
    /// must never be judged unflushable because the node was down.
    async fn preflight(&self, pending: Vec<PendingDeposit>) -> AppResult<Vec<PendingDeposit>> {
        let ids: Vec<u64> = pending.iter().map(|d| d.id).collect();
        let stored = self.masp.escrowed(&ids).await?;
        // Deposits are matched to slots by position, so a short read would judge
        // deposits against the wrong slot.
        if stored.len() != pending.len() {
            return Err(AppError::Internal(format!(
                "escrow read returned {} digests for {} deposits",
                stored.len(),
                pending.len()
            )));
        }
        let masp = self.masp.address();
        let chain_id = self.chain_id as u64;

        let mut flushable = Vec::with_capacity(pending.len());
        let mut mismatched = Vec::new();
        for (deposit, stored) in std::iter::zip(pending, stored) {
            // Priced before `classify` decides whether the fee matters, so this
            // loop does not need to know which verdicts outrank a fee; that is the
            // decision table `classify` owns. A batch is at most
            // `MAX_DEPOSITS_PER_BATCH` deposits and the quoter caches, so the
            // redundant work is bounded.
            let gate = self.fee_gate(&deposit).await?;
            match classify(&deposit, stored, masp, chain_id, &gate) {
                Verdict::Flushable => {
                    // Clears any backoff it accumulated while it was underpaid.
                    self.failures.note_flushable(deposit.id);
                    flushable.push(deposit);
                }
                Verdict::Skip(why) => warn!(
                    chain_id = self.chain_id,
                    deposit_id = deposit.id,
                    why,
                    "deposit left for a later tick"
                ),
                // Its own fee leaf is the reason, so re-judging it every tick would
                // reach the same verdict while keeping payable deposits out of the
                // window behind it.
                Verdict::Defer(why) => self.failures.defer(deposit.id, why),
                Verdict::Reject(why) => self.failures.quarantine(deposit.id, why),
                Verdict::DigestMismatch => mismatched.push(deposit.id),
            }
        }
        // One deposit that hashed correctly shows the derivation agrees with this
        // pool, which is what makes the mismatches below trustworthy.
        if !flushable.is_empty() {
            self.failures.note_digest_verified();
        }
        self.judge_mismatches(&mismatched);
        Ok(flushable)
    }

    /// What this deposit's fee leaf pays, and what it would have to pay in
    /// [`PendingDeposit::fee_pricing_asset`].
    ///
    /// The quote is per deposit rather than per batch: `record_flush` observes
    /// `gas_used / deposits` after each submission, so `EntryPoint::Flush` already
    /// holds a per-deposit figure.
    ///
    /// A pricing failure, such as an asset this relayer will not take or an oracle
    /// that is down, is not the deposit's fault, so it reads as an unpaid fee and
    /// the deposit waits rather than being quarantined.
    async fn fee_gate(&self, d: &PendingDeposit) -> AppResult<FeeGate> {
        let Some(checker) = self.shielded_fee.as_ref() else {
            return Ok(FeeGate::Subsidised);
        };
        let note = assess(checker.recipient(), d)?;
        let gas = self.gas_witness.gas_for(EntryPoint::Flush);
        let asset_id = d.fee_pricing_asset();
        let required = match checker.deposit_fee_required(asset_id, gas).await {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    chain_id = self.chain_id,
                    deposit_id = d.id,
                    asset_id,
                    error = %e,
                    "cannot price this deposit's flush; leaving it for a later tick"
                );
                return Ok(FeeGate::Unpriceable);
            }
        };
        // What the deposit escrowed against what this flush needs, in circuit units
        // of `asset_id`. `fee_in` is public on chain, so this reveals nothing the
        // escrow event did not.
        //
        // Logged whatever the verdict and before `classify` reaches one: a skip
        // reports that the fee was short, and this reports by how much, which
        // separates an under-quoting payer from a moving gas price.
        info!(
            chain_id = self.chain_id,
            deposit_id = d.id,
            asset_id,
            escrowed = d.fee_in,
            required,
            gas,
            ours = matches!(note, FeeNote::Paid { .. }),
            "deposit flush fee priced"
        );
        Ok(FeeGate::Charged { note, required })
    }

    /// Act on deposits whose replayed fields did not hash to their escrow slot.
    ///
    /// A wrong derivation in `deposit_digest`, or a misconfigured `pool_address`,
    /// looks identical to every deposit being corrupt, and acting on that would
    /// quarantine the whole mempool over one bug. A mismatch is therefore believed
    /// only once some deposit on this pool has matched. Until then the deposits are
    /// still dropped from the batch, costing one `eth_call` per tick and no proof.
    fn judge_mismatches(&self, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        if !self.failures.digest_verified() {
            error!(
                chain_id = self.chain_id,
                n = ids.len(),
                "every escrowed digest mismatched and none has ever matched; suspect the local \
                 derivation or pool_address, not the deposits"
            );
            return;
        }
        for id in ids {
            self.failures.quarantine(*id, "escrow digest mismatch");
        }
    }
}
