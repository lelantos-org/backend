//! Per-chain batcher: lands several tree-advancing operations in one
//! transaction through this relayer's `Bundler`.
//!
//! Every tree-advancing call must extend the live root, so operations on one
//! chain can only land in order. Rather than sending one transaction per
//! operation and waiting for each receipt, the batcher reserves the next `K`
//! queued operations in a row — each building on the tree the one before it
//! leaves — proves the `K` chained tree updates, and sends them as one
//! `Bundler.execute`. Each call sees the state the previous one left, so every
//! position check passes on chain.
//!
//! One task per chain owns the mirror from reserve through receipt, which is
//! what keeps bundles in order and nonces sequential. Batching is natural: an
//! idle batcher sends a lone operation straight away, and whatever queues while a
//! bundle is in flight becomes the next one, up to `bundle_max_items`.
//!
//! A bundle is checked twice before it is sent. A dry run replaces the two
//! verifiers' code with an always-accepting stub, so every other on-chain check
//! runs against dummy proofs while the real proofs are being made; a failing
//! operation is dropped before its successors are proved. Once the proofs exist,
//! the real bundle is simulated once more, catching state that moved meanwhile.
//! `execute` stops at the first failing call rather than reverting, so both
//! checks read its return value.
//!
//! What the chain did not keep is unwound. A bundle that stopped at operation
//! `j` keeps the first `j`; operation `j` is failed back to its caller, and the
//! rest go back to the head of the queue. A stale root on the first operation
//! means the chain moved without this relayer — another relayer's bundle, or a
//! third party's `flushBatch` — so the mirror resyncs and everything is retried.
//! The exception is leading flushes the chain already holds, landed by someone
//! else first: their leaves are kept and the rest retried without a resync.

//!
//! - `worker`: the per-chain task that runs a bundle through its phases.
//! - `bundle`: assembling a bundle and answering its jobs.
//! - `outcome`: reading what `execute` returned, logged or reverted with.
//! - `dry_run`: the always-accepting verifier stub a dry run installs.

mod bundle;
mod dry_run;
mod outcome;
mod worker;

pub use dry_run::supports_code_overrides;
pub(crate) use outcome::zero_proof;

use crate::adapters::abi::{IBundler, IMasp};
use crate::adapters::masp::MaspReader;
use crate::domain::error::{AppError, AppResult};
use crate::services::admission::nullifier_guard::PendingGuard;
use crate::services::fees::gas_witness::{EntryPoint, GasWitness};
use crate::services::submitter::{SubmissionReceipt, Submitter};
use crate::services::tree::{AdvancedState, ReservedSlot, TreeMirror};
use alloy::primitives::{Address, U256};
use crypto::tree::Field;
use database::DbPool;
use groth16::{TreeUpdateBatchProver, TreeUpdateBatchWitness};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{Mutex, Notify, mpsc, oneshot};
use worker::Worker;

/// What one operation contributes to a bundle.
///
/// Implemented once per pipeline — spend, swap, flush — so the batcher treats
/// every operation the same way and holds no knowledge of calldata shapes.
pub trait BundleItem: Send + Sync {
    fn entry(&self) -> EntryPoint;

    /// `(cm, cv_dep)` leaves in insertion order, as `TreeMirror` wants them.
    fn leaves(&self) -> Vec<(Field, [U256; 2])>;

    /// The root the wallet proved membership against; `None` for a flush, which
    /// proves none.
    fn merkle_root(&self) -> Option<Field>;

    /// The tree-update witness for this item at its reserved position.
    fn witness(&self, slot: &ReservedSlot, advanced: &AdvancedState) -> TreeUpdateBatchWitness;

    /// This item's `Bundler.Call` against tree-update proof `tp`, which is the
    /// real proof or a placeholder for the dry run.
    ///
    /// Everything it parses must already have been parsed by the pipeline, before
    /// the item was queued: a failure here would cost the bundle's reservations.
    fn encode(
        &self,
        slot: &ReservedSlot,
        advanced: &AdvancedState,
        tp: IMasp::Proof,
    ) -> AppResult<IBundler::Call>;

    /// Gas this item is expected to use, for splitting a receipt among items.
    fn gas_weight(&self, witness: &GasWitness) -> u64 {
        witness.gas_for(self.entry())
    }

    /// What `GET /test/bundler/{chain_id}/queue` shows for this item.
    fn view(&self) -> QueuedItem;
}

/// A queued item as the test hooks report it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedItem {
    pub kind: &'static str,
    /// 0x-hex; empty for a flush.
    pub nullifiers: Vec<String>,
    /// Empty for anything but a flush.
    pub deposit_ids: Vec<u64>,
}

/// What an operation's caller gets back once its bundle lands.
#[derive(Debug)]
pub struct BundledReceipt {
    /// The bundle's receipt, with `gas_used` narrowed to this item's share.
    pub receipt: SubmissionReceipt,
    /// Position within the bundle.
    pub index: usize,
    pub bundle_size: usize,
}

struct Job {
    item: Box<dyn BundleItem>,
    reply: oneshot::Sender<AppResult<BundledReceipt>>,
    /// Held until the bundle lands, so a caller that disconnects cannot free its
    /// nullifiers while the operation is still in flight.
    guard: Option<PendingGuard>,
    /// The proof made at this item's last reservation. A retried item that lands
    /// on the same slot again reuses it instead of proving twice.
    proof: Option<CachedProof>,
}

struct CachedProof {
    start_index: u64,
    old_root: Field,
    new_root: Field,
    proof: IMasp::Proof,
}

impl Job {
    fn fail(self, e: AppError) {
        let _ = self.reply.send(Err(e));
    }
}

/// Test-hook state shared between a chain's handle and its worker.
#[derive(Default)]
struct HoldGate {
    held: AtomicBool,
    released: Notify,
    queue: StdMutex<Vec<QueuedItem>>,
}

/// Cloneable handle to one chain's batcher.
#[derive(Clone)]
pub struct Batcher {
    chain_id: i64,
    tx: mpsc::UnboundedSender<Job>,
    gate: Arc<HoldGate>,
}

impl Batcher {
    /// Queue `item` and wait for its bundle to land.
    ///
    /// `guard` is released when the batcher is done with the item and marked spent
    /// if it landed. A dropped caller does not cancel the item.
    pub async fn submit(
        &self,
        item: Box<dyn BundleItem>,
        guard: Option<PendingGuard>,
    ) -> AppResult<BundledReceipt> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Job {
                item,
                reply,
                guard,
                proof: None,
            })
            .map_err(|_| AppError::Internal(format!("chain {}: batcher stopped", self.chain_id)))?;
        rx.await.map_err(|_| {
            AppError::Internal(format!(
                "chain {}: batcher dropped the operation",
                self.chain_id
            ))
        })?
    }

    /// Stop dispatching; queued items accumulate until [`Self::release`].
    pub fn hold(&self) {
        self.gate.held.store(true, Ordering::SeqCst);
    }

    pub fn release(&self) {
        self.gate.held.store(false, Ordering::SeqCst);
        self.gate.released.notify_waiters();
    }

    /// Items queued behind a hold, oldest first.
    pub fn queued(&self) -> Vec<QueuedItem> {
        self.gate
            .queue
            .lock()
            .map(|q| q.clone())
            .unwrap_or_default()
    }
}

/// What the batcher needs to resync the mirror after the chain moved.
pub struct ResyncCtx {
    pub pool: DbPool,
    pub masp: MaspReader,
}

/// Everything one chain's batcher is built from.
pub struct BatcherCfg {
    pub chain_id: i64,
    pub mirror: Arc<Mutex<TreeMirror>>,
    pub prover: Arc<dyn TreeUpdateBatchProver>,
    /// Targets the chain's `Bundler`.
    pub submitter: Arc<Submitter>,
    pub max_items: usize,
    pub linger: Duration,
    pub max_tx_bytes: usize,
    /// Splits a bundle's receipt among its items.
    pub gas_witness: Arc<GasWitness>,
    /// Verifier addresses to stub out in a dry run; `None` skips dry runs, as on a
    /// node that ignores state overrides.
    pub dry_run_verifiers: Option<[Address; 2]>,
    pub resync: ResyncCtx,
}

/// Build a chain's batcher and spawn its worker.
pub fn spawn(cfg: BatcherCfg) -> Batcher {
    let (tx, rx) = mpsc::unbounded_channel();
    let gate = Arc::new(HoldGate::default());
    let chain_id = cfg.chain_id;
    tokio::spawn(Worker::new(cfg, rx, gate.clone()).run());
    Batcher { chain_id, tx, gate }
}

#[cfg(test)]
mod tests;
