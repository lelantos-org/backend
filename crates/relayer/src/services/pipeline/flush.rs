use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{
    DepositLeaf, PaddedBatch, build_tu_batch_pub_inputs, build_tu_proof,
};
use crate::domain::error::{AppError, AppResult};
use crate::domain::fiat_shamir;
use crate::services::deposit_mempool::{DepositMempool, PendingDeposit};
use crate::services::escrow::EscrowReader;
use crate::services::events::{DepositEvent, EventBroadcaster};
use crate::services::pipeline::deposit_failures::DepositFailures;
use crate::services::pipeline::deposit_preflight::{Verdict, classify};
use crate::services::prover::{Priority, TreeUpdateBatchProver};
use crate::services::submitter::Submitter;
use crate::services::tree::TreeMirror;
use crate::services::witness::{self, LeafDeposit};
use alloy::primitives::{Address, B256, FixedBytes, U256};
use alloy::sol_types::SolCall;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, instrument, warn};

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
}

impl FlushPipeline {
    /// One flush attempt. Returns `Ok(None)` if no pending deposits.
    ///
    /// A deposit is one leaf, so `n` deposits advance the tree by `n` leaves
    /// and `actualCount = n` — an odd count included.
    #[instrument(skip_all, fields(chain_id = self.chain_id, n, start_index))]
    pub async fn tick(&self) -> AppResult<Option<B256>> {
        // `Priority::Flush` never queues for the process-wide prover permit, so
        // a busy prover means this tick cannot finish. Bail out before the DB
        // read, the escrow eth_calls and — the point — the mirror lock:
        // reserving leaves only to unwind them blocks this chain's spends for
        // nothing, and at `flush_interval_s = 3` that is a frequent tick.
        if self.prover.is_busy() {
            return Err(AppError::ProverBusy);
        }

        let limit = self.failures.batch_limit(self.max_n);
        let pending = self
            .mempool
            .pop_pending(limit, &self.failures.quarantined_ids())
            .await?;
        if pending.is_empty() {
            return Ok(None);
        }
        // Everything below this line is paid for — a `tree_update_batch`
        // Groth16 and a transaction — and `flushBatch` is all-or-nothing, so
        // deposits that cannot land are dropped first.
        let pending = self.preflight(pending).await?;
        if pending.is_empty() {
            return Ok(None);
        }
        let n = pending.len();
        tracing::Span::current().record("n", n);
        info!("flush batch starting");

        let cms_real: Vec<FixedBytes<32>> = pending.iter().map(|p| p.cm.into()).collect();
        let leaves: Vec<(fmd_crypto::tree::Field, [U256; 2])> =
            pending.iter().map(|p| (p.cm, p.cv_dep)).collect();
        let ids_u64: Vec<u64> = pending.iter().map(|p| p.id).collect();
        // Digest preimage the contract dropped from storage; a wrong field
        // here reverts `DigestMismatch` for the whole batch.
        let meta: Vec<IMasp::DepositMeta> = pending
            .iter()
            .map(|p| IMasp::DepositMeta {
                payer: Address::from(p.payer),
                submittedAt: p.submitted_at,
                fbps: p.fee_bps_at_submit,
            })
            .collect();
        // The public half of each leaf, at the circuit's full width...
        let batch = PaddedBatch::from_deposits(
            &pending
                .iter()
                .map(|p| DepositLeaf {
                    cm: p.cm.into(),
                    cv_dep: p.cv_dep,
                    leaf_asset: p.public_asset_id,
                    leaf_public_in: p.public_in,
                })
                .collect::<Vec<_>>(),
        );
        // ...and the private blinder the circuit binds it against.
        let deposits: Vec<LeafDeposit> = pending
            .iter()
            .map(|p| LeafDeposit {
                cv_dep: p.cv_dep,
                leaf_asset: p.public_asset_id,
                leaf_public_in: p.public_in,
                rcv: p.rcv,
            })
            .collect();

        // Hold the mirror lock through prove+submit. SpendPipeline shares
        // this mutex; concurrent ops on the same chain serialize.
        let mut mirror = self.mirror.lock().await;
        let (slot, advanced) = mirror.reserve_and_advance_batch(&leaves)?;
        let start_index = slot.start_index;
        tracing::Span::current().record("start_index", start_index);

        let z = fiat_shamir::compute_z(&slot.old_root, &advanced.new_root, start_index, &batch);
        let tu_witness = witness::build_batch(&slot, &advanced, &cms_real, &deposits, z);

        let prove_started = std::time::Instant::now();
        let tu_proof = match self.prover.prove(tu_witness, Priority::Flush).await {
            Ok(p) => p,
            // `ProverBusy` means another chain holds the prover — expected
            // under load, and `abandon` does not charge it to the batch.
            Err(e) => return Err(self.abandon(&mut mirror, n, &ids_u64, e)),
        };
        info!(
            elapsed_ms = prove_started.elapsed().as_millis() as u64,
            "flush prove ok"
        );

        let tp = build_tu_proof(&tu_proof)?;
        let tpi =
            build_tu_batch_pub_inputs(start_index, &slot.old_root, &advanced.new_root, &batch);

        let calldata = IMasp::flushBatchCall {
            ids: ids_u64.iter().copied().map(U256::from).collect(),
            meta,
            tp,
            tpi,
        }
        .abi_encode();
        let receipt = match self.submitter.submit(calldata).await {
            Ok(r) => r,
            Err(e) => return Err(self.abandon(&mut mirror, n, &ids_u64, e)),
        };
        // Drop the mirror lock before the DB write so other pipelines can proceed.
        drop(mirror);
        self.failures.note_success();

        // Optimistic mark — keeps these IDs out of subsequent `pop_pending`
        // until the ingester observes the on-chain `DepositFlushed` event
        // and overwrites with the canonical block number.
        match self
            .mempool
            .mark_submitted(&ids_u64, receipt.block_number)
            .await
        {
            Ok(claimed) if claimed != n => {
                // The indexer writes the canonical flush unconditionally, and
                // `submit` waits for a confirmation — so it often wins this
                // race. Nothing is lost then; its row is the better one.
                match self.mempool.count_unflushed(&ids_u64).await {
                    Ok(0) => info!(
                        claimed,
                        batched = n,
                        "flush rows already marked flushed by the indexer"
                    ),
                    Ok(unflushed) => warn!(
                        claimed,
                        batched = n,
                        unflushed,
                        "flush claimed fewer deposits than it submitted; another relayer may share this chain"
                    ),
                    Err(e) => warn!(
                        claimed,
                        batched = n,
                        error = %e,
                        "flush claimed fewer deposits than it submitted; could not tell indexer race from a second relayer"
                    ),
                }
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, "flush mark_submitted failed (ingester will catch up)"),
        }

        let tx_hash_hex = format!("0x{}", hex::encode(receipt.tx_hash));
        info!(
            tx_hash = %tx_hash_hex,
            gas_used = receipt.gas_used,
            block = receipt.block_number,
            "flushBatch submitted"
        );
        for id in &ids_u64 {
            self.events.publish(DepositEvent::Flushed {
                deposit_id: *id,
                chain_id: self.chain_id,
                tx_hash: tx_hash_hex.clone(),
                block_number: receipt.block_number,
            });
        }
        Ok(Some(receipt.tx_hash))
    }

    /// Roll the mirror back after a failed stage and charge the batch for it.
    ///
    /// Which failures are the batch's own is [`DepositFailures::note_failure`]'s
    /// call; infrastructure faults pass through uncounted.
    #[must_use = "the returned error must be propagated"]
    fn abandon(
        &self,
        mirror: &mut TreeMirror,
        leaves: usize,
        ids: &[u64],
        cause: AppError,
    ) -> AppError {
        let cause = mirror.unwind(leaves, cause);
        self.failures.note_failure(ids, &cause);
        cause
    }

    /// Drop deposits `flushBatch` would refuse, before the prover runs.
    ///
    /// The contract keeps only `escrowed[id]`, a digest over every field the
    /// relayer replays. Reading it back and re-deriving the digest locally
    /// reproduces the per-deposit guards in `_drainDeposit` for one `eth_call`
    /// each, instead of one wasted Groth16 per tick. [`classify`] holds the
    /// decision table; this applies it.
    ///
    /// An RPC failure aborts the tick rather than rejecting anything: a
    /// deposit must never be judged unflushable because the node was down.
    async fn preflight(&self, pending: Vec<PendingDeposit>) -> AppResult<Vec<PendingDeposit>> {
        let ids: Vec<u64> = pending.iter().map(|d| d.id).collect();
        let stored = self.escrow.digests(&ids).await?;
        // Deposits are matched to slots by position, so a short read would
        // silently judge deposits against the wrong slot.
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
            match classify(&deposit, stored, masp, chain_id) {
                Verdict::Flushable => flushable.push(deposit),
                Verdict::Skip(why) => warn!(
                    chain_id = self.chain_id,
                    deposit_id = deposit.id,
                    why,
                    "deposit left for a later tick"
                ),
                Verdict::Reject(why) => self.failures.quarantine(deposit.id, why),
                Verdict::DigestMismatch => mismatched.push(deposit.id),
            }
        }
        // One deposit that hashed correctly proves the derivation agrees with
        // this pool, which is what makes the mismatches below trustworthy.
        if !flushable.is_empty() {
            self.failures.note_digest_verified();
        }
        self.judge_mismatches(&mismatched);
        Ok(flushable)
    }

    /// Act on deposits whose replayed fields did not hash to their escrow slot.
    ///
    /// A wrong derivation in `deposit_digest` — or a misconfigured
    /// `pool_address` — looks exactly like every deposit being corrupt, and
    /// acting on that would quarantine the whole mempool on a single bug. So a
    /// mismatch is only believed once some deposit on this pool has matched.
    /// Until then the deposits are still dropped from the batch, which costs
    /// an `eth_call` per tick and no proof.
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
