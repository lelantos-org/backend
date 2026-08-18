use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{
    DepositLeaf, PaddedBatch, build_tu_batch_pub_inputs, build_tu_proof,
};
use crate::domain::error::AppResult;
use crate::domain::fiat_shamir;
use crate::services::deposit_mempool::DepositMempool;
use crate::services::events::{DepositEvent, EventBroadcaster};
use crate::services::prover::TreeUpdateBatchProver;
use crate::services::submitter::Submitter;
use crate::services::tree::TreeMirror;
use crate::services::witness::{self, LeafDeposit};
use alloy::primitives::{Address, B256, FixedBytes, U256};
use alloy::sol_types::SolCall;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, instrument, warn};

pub struct FlushPipeline {
    pub chain_id: i64,
    pub mirror: Arc<Mutex<TreeMirror>>,
    pub submitter: Arc<Submitter>,
    pub prover: Arc<dyn TreeUpdateBatchProver>,
    pub mempool: Arc<DepositMempool>,
    pub max_n: usize,
    pub events: Arc<EventBroadcaster>,
}

impl FlushPipeline {
    /// One flush attempt. Returns `Ok(None)` if no pending deposits.
    ///
    /// A deposit is one leaf, so `n` deposits advance the tree by `n` leaves
    /// and `actualCount = n` — an odd count included.
    #[instrument(skip_all, fields(chain_id = self.chain_id, n, start_index))]
    pub async fn tick(&self) -> AppResult<Option<B256>> {
        let pending = self.mempool.pop_pending(self.max_n).await?;
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
        let tu_proof = match self.prover.prove(tu_witness).await {
            Ok(p) => p,
            Err(e) => return Err(mirror.unwind(n, e)),
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
            Err(e) => return Err(mirror.unwind(n, e)),
        };
        // Drop the mirror lock before the DB write so other pipelines can proceed.
        drop(mirror);

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
}
