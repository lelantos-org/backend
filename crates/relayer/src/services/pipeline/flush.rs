use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{MAX_N_BATCH, build_tu_batch_pub_inputs, build_tu_proof};
use crate::domain::error::AppResult;
use crate::domain::fiat_shamir;
use crate::services::events::{EventBroadcaster, IntentEvent};
use crate::services::intent_mempool::IntentMempool;
use crate::services::prover::TreeUpdateBatchProver;
use crate::services::submitter::Submitter;
use crate::services::tree::TreeMirror;
use crate::services::witness::{self, PairDeposit};
use alloy::primitives::{B256, FixedBytes, U256};
use alloy::sol_types::SolCall;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, instrument, warn};

pub struct FlushPipeline {
    pub chain_id: i64,
    pub mirror: Arc<Mutex<TreeMirror>>,
    pub submitter: Arc<Submitter>,
    pub prover: Arc<dyn TreeUpdateBatchProver>,
    pub mempool: Arc<IntentMempool>,
    pub max_n: usize,
    pub events: Arc<EventBroadcaster>,
}

impl FlushPipeline {
    /// One flush attempt. Returns `Ok(None)` if no pending intents.
    #[instrument(skip_all, fields(chain_id = self.chain_id, n, start_index))]
    pub async fn tick(&self) -> AppResult<Option<B256>> {
        let pending = self.mempool.pop_pending(self.max_n).await?;
        if pending.is_empty() {
            return Ok(None);
        }
        let n = pending.len();
        tracing::Span::current().record("n", n);
        info!("flush batch starting");

        let mut cms_real: Vec<FixedBytes<32>> = Vec::with_capacity(2 * n);
        let mut leaf_pairs: Vec<(fmd_crypto::tree::Field, [U256; 2])> = Vec::with_capacity(2 * n);
        let mut ids_u64: Vec<u64> = Vec::with_capacity(n);
        let mut ids_u256: Vec<alloy::primitives::U256> = Vec::with_capacity(n);
        let mut pairs: Vec<PairDeposit> = Vec::with_capacity(n);
        for p in &pending {
            cms_real.push(FixedBytes::<32>::from(p.cm0));
            cms_real.push(FixedBytes::<32>::from(p.cm1));
            leaf_pairs.push((p.cm0, p.cv_dep0));
            leaf_pairs.push((p.cm1, p.cv_dep1));
            ids_u64.push(p.id);
            ids_u256.push(alloy::primitives::U256::from(p.id));
            pairs.push(PairDeposit {
                cv_dep0: p.cv_dep0,
                cv_dep1: p.cv_dep1,
                pair_asset: p.public_asset_id,
                pair_public_in: p.public_in,
                rcv_total: p.rcv_total,
            });
        }

        // Hold the mirror lock through prove+submit. SpendPipeline shares
        // this mutex; concurrent ops on the same chain serialize.
        let mut mirror = self.mirror.lock().await;
        let (slot, advanced) = mirror.reserve_and_advance_batch(&leaf_pairs)?;
        let start_index = slot.start_index;
        tracing::Span::current().record("start_index", start_index);

        let mut cms_padded: [FixedBytes<32>; 2 * MAX_N_BATCH] =
            [FixedBytes::<32>::ZERO; 2 * MAX_N_BATCH];
        for (i, c) in cms_real.iter().enumerate() {
            cms_padded[i] = *c;
        }
        let mut cv_deps_padded: [[U256; 2]; 2 * MAX_N_BATCH] =
            [[U256::ZERO, U256::ZERO]; 2 * MAX_N_BATCH];
        let mut pair_asset_padded: [u64; MAX_N_BATCH] = [0u64; MAX_N_BATCH];
        let mut pair_public_in_padded: [u64; MAX_N_BATCH] = [0u64; MAX_N_BATCH];
        let mut is_deposit_padded: [u8; MAX_N_BATCH] = [0u8; MAX_N_BATCH];
        for (i, p) in pairs.iter().enumerate() {
            cv_deps_padded[2 * i] = p.cv_dep0;
            cv_deps_padded[2 * i + 1] = p.cv_dep1;
            pair_asset_padded[i] = p.pair_asset;
            pair_public_in_padded[i] = p.pair_public_in;
            is_deposit_padded[i] = 1;
        }

        let z = fiat_shamir::compute_z(
            &slot.old_root,
            &advanced.new_root,
            start_index,
            n as u64,
            &cms_padded,
            &cv_deps_padded,
            &pair_asset_padded,
            &pair_public_in_padded,
            &is_deposit_padded,
        );
        let tu_witness = witness::build_batch(&slot, &advanced, &cms_real, &pairs, n as u64, z);

        let prove_started = std::time::Instant::now();
        let tu_proof = match self.prover.prove(tu_witness).await {
            Ok(p) => p,
            Err(e) => return Err(mirror.unwind(2 * n, e)),
        };
        info!(
            elapsed_ms = prove_started.elapsed().as_millis() as u64,
            "flush prove ok"
        );

        let tp = build_tu_proof(&tu_proof)?;
        let tpi = build_tu_batch_pub_inputs(
            start_index,
            &slot.old_root,
            &advanced.new_root,
            cms_padded,
            cv_deps_padded,
            pair_asset_padded,
            pair_public_in_padded,
            is_deposit_padded,
            n as u64,
        );

        let calldata = IMasp::flushBatchCall {
            ids: ids_u256,
            tp,
            tpi,
        }
        .abi_encode();
        let receipt = match self.submitter.submit(calldata).await {
            Ok(r) => r,
            Err(e) => return Err(mirror.unwind(2 * n, e)),
        };
        // Drop the mirror lock before the DB write so other pipelines can proceed.
        drop(mirror);

        // Optimistic mark — keeps these IDs out of subsequent `pop_pending`
        // until the ingester observes the on-chain `IntentFlushed` event
        // and overwrites with the canonical block number.
        match self
            .mempool
            .mark_submitted(&ids_u64, receipt.block_number)
            .await
        {
            Ok(claimed) if claimed != n => warn!(
                claimed,
                batched = n,
                "flush claimed fewer intents than it submitted; another relayer may share this chain"
            ),
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
            self.events.publish(IntentEvent::Flushed {
                intent_id: *id,
                chain_id: self.chain_id,
                tx_hash: tx_hash_hex.clone(),
                block_number: receipt.block_number,
            });
        }
        Ok(Some(receipt.tx_hash))
    }
}
