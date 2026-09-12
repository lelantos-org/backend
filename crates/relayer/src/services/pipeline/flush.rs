use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{
    DepositLeaf, PaddedBatch, build_tu_batch_pub_inputs, build_tu_proof,
};
use crate::domain::error::{AppError, AppResult};
use crate::domain::fiat_shamir;
use crate::domain::responses::EstimateResponse;
use crate::services::asset_registry::AssetRegistry;
use crate::services::deposit_fee::{FeeNote, assess};
use crate::services::deposit_mempool::{DepositMempool, EscrowLeaf, PendingDeposit};
use crate::services::escrow::EscrowReader;
use crate::services::events::{DepositEvent, EventBroadcaster};
use crate::services::fee_quote::FeeQuoter;
use crate::services::gas_witness::{EntryPoint, GasWitness};
use crate::services::pipeline::common::FeeContext;
use crate::services::pipeline::deposit_failures::DepositFailures;
use crate::services::pipeline::deposit_preflight::{FeeGate, Verdict, classify};
use crate::services::shielded_fee::ShieldedFeeChecker;
use crate::services::submitter::{SubmissionReceipt, Submitter};
use crate::services::tree::{AdvancedState, ReservedSlot, TreeMirror};
use crate::services::witness::{self, LeafDeposit};
use alloy::primitives::{Address, B256, FixedBytes, U256};
use alloy::sol_types::SolCall;
use groth16::{Priority, TreeUpdateBatchProof, TreeUpdateBatchProver};
use std::sync::Arc;
use tokio::sync::Mutex;
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
    pub mirror: Arc<Mutex<TreeMirror>>,
    pub submitter: Arc<Submitter>,
    pub prover: Arc<dyn TreeUpdateBatchProver>,
    pub mempool: Arc<DepositMempool>,
    pub escrow: Arc<EscrowReader>,
    pub max_n: usize,
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
    #[instrument(skip_all, fields(chain_id = self.chain_id, n, start_index))]
    pub async fn tick(&self) -> AppResult<Option<B256>> {
        // `Priority::Flush` never queues for the process-wide prover permit, so a
        // busy prover means this tick cannot finish. Bailing out here avoids the
        // database read, the escrow `eth_call`s and the mirror lock: reserving
        // leaves only to unwind them would block this chain's spends, and the tick
        // runs frequently.
        if self.prover.is_busy() {
            return Err(AppError::ProverBusy);
        }

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
        if pending.is_empty() {
            return Ok(None);
        }
        let plan = BatchPlan::new(&pending);
        let n = plan.n;
        tracing::Span::current().record("n", n);
        info!("flush batch starting");

        let receipt = self.prove_and_submit(&plan).await?;
        self.failures.note_success();
        self.record_flush(&plan, &receipt).await;
        Ok(Some(receipt.tx_hash))
    }

    /// Reserve the batch's leaves, prove `tree_update_batch` over them and submit
    /// `flushBatch`, unwinding the speculative inserts on any failure.
    ///
    /// The mirror lock is held from reserve through the receipt and released
    /// before the caller's database write. `SpendPipeline` and `SwapPipeline`
    /// share this mutex, so every operation on the chain serialises; their own
    /// equivalent is `pipeline::common::reserve_prove_submit`, which this cannot
    /// reuse because a flush proves deposit leaves rather than a transact payload.
    async fn prove_and_submit(&self, plan: &BatchPlan) -> AppResult<SubmissionReceipt> {
        let mut mirror = self.mirror.lock().await;
        let (slot, advanced) = mirror.reserve_and_advance_batch(&plan.tree_leaves)?;
        let start_index = slot.start_index;
        tracing::Span::current().record("start_index", start_index);

        let z =
            fiat_shamir::compute_z(&slot.old_root, &advanced.new_root, start_index, &plan.batch);
        let tu_witness = witness::build_batch(&slot, &advanced, &plan.cms, &plan.deposits, z);

        let prove_started = std::time::Instant::now();
        let tu_proof = match self.prover.prove(tu_witness, Priority::Flush).await {
            Ok(p) => p,
            // `ProverBusy` means another chain holds the prover, which is normal
            // under load, and `abandon` does not charge it to the batch.
            Err(e) => return Err(self.abandon(&mut mirror, plan, e.into())),
        };
        info!(
            elapsed_ms = prove_started.elapsed().as_millis() as u64,
            "flush prove ok"
        );

        let calldata = match plan.encode(&slot, &advanced, &tu_proof) {
            Ok(c) => c,
            Err(e) => return Err(self.abandon(&mut mirror, plan, e)),
        };
        match self.submitter.submit(calldata).await {
            Ok(r) => Ok(r),
            Err(e) => Err(self.abandon(&mut mirror, plan, e)),
        }
    }

    /// Everything a landed `flushBatch` leaves behind: the optimistic ledger mark,
    /// the gas observation the next quote is built on, and the SSE events.
    ///
    /// None of it can fail the flush — the transaction has already confirmed — so
    /// each step reports and moves on rather than propagating.
    async fn record_flush(&self, plan: &BatchPlan, receipt: &SubmissionReceipt) {
        self.mark_submitted(plan, receipt.block_number).await;

        // Per deposit, since that is what a deposit's fee note is quoted against
        // while the receipt covers the whole batch. Recorded only on a confirmed
        // submission, so a reverted flush cannot move the quote.
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

    /// Roll the mirror back after a failed stage and charge the batch for it.
    ///
    /// [`DepositFailures::note_failure`] decides which failures are the batch's
    /// own; infrastructure faults pass through uncounted.
    #[must_use = "the returned error must be propagated"]
    fn abandon(&self, mirror: &mut TreeMirror, plan: &BatchPlan, cause: AppError) -> AppError {
        let cause = mirror.unwind(plan.leaf_count(), cause);
        self.failures.note_failure(&plan.ids, &cause);
        cause
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
        let stored = self.escrow.digests(&ids).await?;
        // Deposits are matched to slots by position, so a short read would judge
        // deposits against the wrong slot.
        if stored.len() != pending.len() {
            return Err(AppError::Internal(format!(
                "escrow read returned {} digests for {} deposits",
                stored.len(),
                pending.len()
            )));
        }
        let masp = self.escrow.pool_address();
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

    /// What this deposit's fee leaf pays, and what it would have to pay.
    ///
    /// The quote is per deposit rather than per batch: `flush.rs` observes
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
        let required = match checker.deposit_fee_required(d.public_asset_id, gas).await {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    chain_id = self.chain_id,
                    deposit_id = d.id,
                    error = %e,
                    "cannot price this deposit's flush; leaving it for a later tick"
                );
                return Ok(FeeGate::Unpriceable);
            }
        };
        // What the deposit escrowed against what this flush needs, in circuit units
        // of the deposit's asset. `fee_in` is public on chain, so this reveals
        // nothing the escrow event did not.
        //
        // Logged whatever the verdict and before `classify` reaches one: a skip
        // reports that the fee was short, and this reports by how much, which
        // separates an under-quoting payer from a moving gas price.
        info!(
            chain_id = self.chain_id,
            deposit_id = d.id,
            asset_id = d.public_asset_id,
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

/// One tick's batch, in every shape the flush path needs it.
///
/// Built once from the pending deposits so that the leaf-indexed arrays cannot
/// disagree: they all derive from the single `leaves` vector, which is
/// [`PendingDeposit::leaves`] flattened in the order `_drainDeposit` reads the
/// pair back at `2i` and `2i + 1`.
struct BatchPlan {
    /// Deposits in the batch. `flushBatch` inserts `2n` leaves and the contract
    /// enforces the doubling, so an odd leaf count is a `BadBatchSize`.
    n: usize,
    /// Deposit ids, in batch order.
    ids: Vec<u64>,
    /// Commitments as the witness builder wants them.
    cms: Vec<FixedBytes<32>>,
    /// `(cm, cv_dep)` pairs as `TreeMirror` wants them.
    tree_leaves: Vec<(common_crypto::tree::Field, [U256; 2])>,
    /// Digest preimage the contract dropped from storage; a wrong field here
    /// reverts `DigestMismatch` for the whole batch.
    meta: Vec<IMasp::DepositMeta>,
    /// The public half of each leaf, at the circuit's full width.
    batch: PaddedBatch,
    /// The private blinders the circuit binds that public half against.
    deposits: Vec<LeafDeposit>,
    /// Escrowed fee, summed over the batch. Deposits in one batch can name
    /// different assets, so this is a batch total rather than an amount of a
    /// single token, and is meaningful only alongside the per-deposit lines
    /// `fee_gate` emits.
    fees_collected: u64,
}

impl BatchPlan {
    fn new(pending: &[PendingDeposit]) -> Self {
        let leaves: Vec<EscrowLeaf> = pending.iter().flat_map(PendingDeposit::leaves).collect();
        Self {
            n: pending.len(),
            ids: pending.iter().map(|p| p.id).collect(),
            cms: leaves.iter().map(|l| l.cm.into()).collect(),
            tree_leaves: leaves.iter().map(|l| (l.cm, l.cv_dep)).collect(),
            meta: pending
                .iter()
                .map(|p| IMasp::DepositMeta {
                    payer: Address::from(p.payer),
                    submittedAt: p.submitted_at,
                    fbps: p.fee_bps_at_submit,
                })
                .collect(),
            batch: PaddedBatch::from_deposits(
                &leaves
                    .iter()
                    .map(|l| DepositLeaf {
                        cm: l.cm.into(),
                        cv_dep: l.cv_dep,
                        leaf_asset: l.asset_id,
                        leaf_public_in: l.public_in,
                    })
                    .collect::<Vec<_>>(),
            ),
            deposits: leaves
                .iter()
                .map(|l| LeafDeposit {
                    cv_dep: l.cv_dep,
                    leaf_asset: l.asset_id,
                    leaf_public_in: l.public_in,
                    rcv: l.rcv,
                })
                .collect(),
            fees_collected: pending.iter().map(|p| p.fee_in).sum(),
        }
    }

    /// Leaves this batch reserved in the mirror, which is what an unwind rolls
    /// back.
    fn leaf_count(&self) -> usize {
        self.tree_leaves.len()
    }

    /// `flushBatch` calldata for this batch against the proof just produced.
    fn encode(
        &self,
        slot: &ReservedSlot,
        advanced: &AdvancedState,
        tu_proof: &TreeUpdateBatchProof,
    ) -> AppResult<Vec<u8>> {
        Ok(IMasp::flushBatchCall {
            ids: self.ids.iter().copied().map(U256::from).collect(),
            meta: self.meta.clone(),
            tp: build_tu_proof(tu_proof)?,
            tpi: build_tu_batch_pub_inputs(
                slot.start_index,
                &slot.old_root,
                &advanced.new_root,
                &self.batch,
            ),
        }
        .abi_encode())
    }
}
