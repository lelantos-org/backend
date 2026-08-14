// Per-chain in-memory tree mirror. Bootstraps from the existing `notes` +
// `tree_advances` rows on startup. The relayer is otherwise stateless across
// restarts — no DB tables of its own.
//
// Concurrency: each chain owns one `Arc<Mutex<TreeMirror>>`. Pipeline holds
// the mutex through {reserve(start_index, frontier) → prove → submit →
// receipt}, so the next bundle pipelines optimistically against the post-
// state. On revert, `rollback(2)` undoes the speculative inserts.

use crate::adapters::abi::IMasp;
use crate::adapters::numeric::bigdecimal_to_u256;
use crate::adapters::rpc::RpcEndpoint;
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use database::DbPool;
use database::schema::{notes, tree_advances};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use fmd_crypto::poseidon as fmd_poseidon;
use fmd_crypto::tree::{Field, MerkleTree};
use rayon::prelude::*;
use std::str::FromStr;
use tracing::{error, info};

const DEPTH: usize = 10;
/// Domain-separation tag for Merkle leaf hashing. Mirrors `TAG_LEAF` in
/// `circuits/src/lib/tags.circom` — `leaf = Poseidon(TAG_LEAF, cm,
/// cv_dep_x, cv_dep_y)` so the spender can rebuild the same leaf hash from
/// (cm, cv_dep) without learning anything else about the deposit.
const TAG_LEAF: u64 = 10;

/// Compute the in-circuit Merkle leaf:
///   leaf = Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)
/// Must match `tree_update_batch.circom` section 5 byte-for-byte; any drift
/// here desyncs the relayer's mirror from the on-chain tree.
fn leaf_hash(cm: &Field, cv_dep: &[U256; 2]) -> AppResult<Field> {
    let mut tag = [0u8; 32];
    tag[31] = TAG_LEAF as u8;
    let cv_x = cv_dep[0].to_be_bytes::<32>();
    let cv_y = cv_dep[1].to_be_bytes::<32>();
    fmd_poseidon::hash_bytes_be(&[&tag, cm, &cv_x, &cv_y])
        .map_err(|e| AppError::Internal(format!("leaf_hash: {}", e)))
}

pub struct TreeMirror {
    pub chain_id: i64,
    tree: MerkleTree,
    /// Why this mirror was parked, if it was (see [`TreeMirror::unwind`]).
    /// Every reserve then fails fast rather than building on state that may
    /// no longer match the chain.
    desynced: Option<String>,
}

pub struct ReservedSlot {
    pub start_index: u64,
    pub old_root: Field,
    pub old_frontier: Vec<[Field; 3]>,
}

pub struct AdvancedState {
    pub new_root: Field,
}

impl TreeMirror {
    pub fn new(chain_id: i64) -> AppResult<Self> {
        let tree = MerkleTree::new(DEPTH).map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(Self {
            chain_id,
            tree,
            desynced: None,
        })
    }

    /// Undo `leaves` speculative inserts after a failed pipeline stage, and
    /// return the error to propagate.
    ///
    /// Rollback is only sound when the transaction provably did not land. On
    /// [`AppError::SubmitUnknown`] the mirror is parked instead: truncating
    /// would permanently diverge it if the tx mines later, and keeping the
    /// leaves would diverge it if it never does.
    ///
    /// A rollback that itself fails is also a desync. Either way the original
    /// error wins — it is what actually explains the failure.
    #[must_use = "the returned error must be propagated"]
    pub fn unwind(&mut self, leaves: usize, cause: AppError) -> AppError {
        if let AppError::SubmitUnknown(reason) = &cause {
            error!(chain_id = self.chain_id, error = %cause, "submit outcome unknown; parking mirror");
            self.park(reason.clone());
            return cause;
        }
        error!(chain_id = self.chain_id, error = %cause, "stage failed; rolling back mirror");
        if let Err(e) = self.rollback(leaves) {
            error!(chain_id = self.chain_id, error = %e, "rollback failed; parking mirror");
            self.park(e.to_string());
        }
        cause
    }

    /// Refuse further work on this chain until a restart re-bootstraps from
    /// the indexer. Only [`Self::unwind`] should need this.
    fn park(&mut self, reason: String) {
        self.desynced.get_or_insert(reason);
    }

    pub fn is_desynced(&self) -> bool {
        self.desynced.is_some()
    }

    fn check_usable(&self) -> AppResult<()> {
        match &self.desynced {
            None => Ok(()),
            Some(reason) => Err(AppError::MirrorDesynced(format!(
                "chain {}: {} (restart to re-bootstrap once the indexer has caught up)",
                self.chain_id, reason
            ))),
        }
    }

    /// Replay all confirmed cms from `notes` ordered by leaf_index. Validate
    /// that the post-replay root matches the latest `tree_advances.new_root`.
    pub async fn bootstrap(&mut self, pool: &DbPool) -> AppResult<()> {
        info!(chain_id = self.chain_id, "tree mirror bootstrap start");
        let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
        let cms: Vec<(i64, Vec<u8>, bigdecimal::BigDecimal, bigdecimal::BigDecimal)> = notes::table
            .filter(notes::chain_id.eq(self.chain_id))
            .order(notes::leaf_index.asc())
            .select((
                notes::leaf_index,
                notes::cm,
                notes::cv_dep_x,
                notes::cv_dep_y,
            ))
            .load(&mut conn)
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;

        // Validate row contiguity sequentially (cheap), then hash leaves in
        // parallel — leaf_hash is a pure Poseidon call, independent per row.
        for (i, (leaf_index, _, _, _)) in cms.iter().enumerate() {
            if *leaf_index != i as i64 {
                return Err(AppError::Internal(format!(
                    "tree desync chain {}: notes row {} has leaf_index {}",
                    self.chain_id, i, leaf_index
                )));
            }
        }
        let leaves: Vec<Field> = cms
            .par_iter()
            .map(|(_, cm, cv_dep_x, cv_dep_y)| {
                let cm_f = vec_to_field(cm)?;
                let cv_x = bigdecimal_to_u256(cv_dep_x)?;
                let cv_y = bigdecimal_to_u256(cv_dep_y)?;
                leaf_hash(&cm_f, &[cv_x, cv_y])
            })
            .collect::<AppResult<Vec<Field>>>()?;
        self.tree
            .extend(leaves)
            .map_err(|e| AppError::Internal(e.to_string()))?;

        // Sanity: latest tree_advances.new_root must match local root.
        let latest: Option<(Vec<u8>, i64)> = tree_advances::table
            .filter(tree_advances::chain_id.eq(self.chain_id))
            .order(tree_advances::start_index.desc())
            .select((tree_advances::new_root, tree_advances::start_index))
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| AppError::Db(e.to_string()))?;
        info!(
            chain_id = self.chain_id,
            leaves = cms.len(),
            "tree mirror replayed notes"
        );
        if let Some((expected_root, _)) = latest {
            let local = self
                .tree
                .root()
                .map_err(|e| AppError::Internal(e.to_string()))?;
            if local.to_vec() != expected_root {
                return Err(AppError::Internal(format!(
                    "tree mirror diverges from chain on chain_id {}",
                    self.chain_id
                )));
            }
        }
        Ok(())
    }

    pub fn committed_count(&self) -> u64 {
        self.tree.leaf_count() as u64
    }

    pub fn current_root(&self) -> AppResult<Field> {
        self.tree
            .root()
            .map_err(|e| AppError::Internal(e.to_string()))
    }

    /// Insert N `(cm, cv_dep)` pairs. The mirror hashes each pair into a
    /// leaf before insertion to stay in sync with the on-chain tree, which
    /// advances via SNARK-verified leaf roots.
    pub fn reserve_and_advance_batch(
        &mut self,
        cms: &[(Field, [U256; 2])],
    ) -> AppResult<(ReservedSlot, AdvancedState)> {
        self.check_usable()?;
        let start_index = self.tree.leaf_count() as u64;
        let old_root = self
            .tree
            .root()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let old_frontier = self
            .tree
            .frontier()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        for (cm, cv_dep) in cms {
            let leaf = leaf_hash(cm, cv_dep)?;
            self.tree
                .insert(leaf)
                .map_err(|e| AppError::Internal(e.to_string()))?;
        }
        let new_root = self
            .tree
            .root()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        Ok((
            ReservedSlot {
                start_index,
                old_root,
                old_frontier,
            },
            AdvancedState { new_root },
        ))
    }

    /// Cross-check the in-memory mirror against the on-chain `currentRoot()`.
    /// Catches DB / chain divergence (e.g. anvil redeploy without DB reset)
    /// before the first `transact` reverts with `StaleOldRoot()`.
    pub async fn verify_chain_root(
        &self,
        rpc: &RpcEndpoint,
        pool_address_hex: &str,
    ) -> AppResult<()> {
        let pool_address = Address::from_str(pool_address_hex)
            .map_err(|e| AppError::Internal(format!("pool addr: {}", e)))?;
        let provider = ProviderBuilder::new().on_client(rpc.client());
        let masp = IMasp::new(pool_address, provider);
        let chain_root = masp
            .currentRoot()
            .call()
            .await
            .map_err(|e| AppError::Rpc(format!("currentRoot: {}", e)))?
            ._0;
        let local_root = self.current_root()?;
        if chain_root.0 != local_root {
            return Err(AppError::Internal(format!(
                "tree mirror diverges from chain {}: local={} chain={} (DB likely stale; reset notes/tree_advances for this chain)",
                self.chain_id,
                field_to_hex(&local_root),
                hex::encode(chain_root.0),
            )));
        }
        info!(
            chain_id = self.chain_id,
            root = field_to_hex(&local_root),
            "tree mirror matches chain root"
        );
        Ok(())
    }

    /// Undo `n` speculative leaves after a submission that provably never
    /// landed (RPC rejected the broadcast, or the tx reverted on-chain). An
    /// *ambiguous* failure must go to `mark_desynced` instead — see there.
    pub fn rollback(&mut self, n: usize) -> AppResult<()> {
        let before = self.tree.leaf_count();
        if n > before {
            return Err(AppError::Internal(format!(
                "rollback {} > leaf_count {} on chain {}",
                n, before, self.chain_id
            )));
        }
        self.tree
            .truncate_leaves(n)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        info!(
            chain_id = self.chain_id,
            n,
            before,
            after = self.tree.leaf_count(),
            "tree mirror rollback"
        );
        Ok(())
    }
}

pub fn vec_to_field(v: &[u8]) -> AppResult<Field> {
    if v.len() != 32 {
        return Err(AppError::Internal(format!(
            "expected 32-byte field, got {}",
            v.len()
        )));
    }
    let mut f = [0u8; 32];
    f.copy_from_slice(v);
    Ok(f)
}

pub fn field_to_hex(f: &Field) -> String {
    format!("0x{}", hex::encode(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAIN_ID: i64 = 31337;

    fn cm(n: u8) -> Field {
        let mut f = [0u8; 32];
        f[31] = n;
        f
    }

    fn cv(n: u8) -> [U256; 2] {
        [U256::from(n), U256::from(n) + U256::from(1u8)]
    }

    /// Two-leaf advance. Real spends insert `TRANSACT_OUT` leaves and a
    /// flush one per deposit; two keeps the arithmetic in these tests easy
    /// to read without changing what is under test.
    fn advance2(
        m: &mut TreeMirror,
        cm0: Field,
        cm1: Field,
        cv0: [U256; 2],
        cv1: [U256; 2],
    ) -> AppResult<(ReservedSlot, AdvancedState)> {
        m.reserve_and_advance_batch(&[(cm0, cv0), (cm1, cv1)])
    }

    /// A mirror with `pairs` pairs already committed.
    fn mirror(pairs: u8) -> TreeMirror {
        let mut m = TreeMirror::new(CHAIN_ID).unwrap();
        for i in 0..pairs {
            advance2(&mut m, cm(2 * i), cm(2 * i + 1), cv(i), cv(i + 1)).unwrap();
        }
        m
    }

    fn reserve_one(m: &mut TreeMirror) -> AppResult<()> {
        advance2(m, cm(200), cm(201), cv(9), cv(10)).map(|_| ())
    }

    #[test]
    fn reserve_advances_by_two_leaves() {
        let mut m = mirror(1);
        assert_eq!(m.committed_count(), 2);
        let (slot, advanced) = advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();
        assert_eq!(slot.start_index, 2);
        assert_eq!(m.committed_count(), 4);
        assert_ne!(slot.old_root, advanced.new_root);
    }

    /// A revert or a refused broadcast provably left no leaves on-chain, so
    /// the speculative pair comes back off and the mirror stays usable.
    #[test]
    fn unwind_rolls_back_a_clean_failure() {
        let mut m = mirror(1);
        let before = m.current_root().unwrap();
        advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();

        let err = m.unwind(2, AppError::Reverted("tx reverted".into()));

        assert!(matches!(err, AppError::Reverted(_)));
        assert!(!m.is_desynced());
        assert_eq!(m.committed_count(), 2);
        assert_eq!(m.current_root().unwrap(), before, "root must be restored");
        reserve_one(&mut m).expect("mirror should still accept work");
    }

    /// The tx may still mine, so the leaves must stay — and because the mirror
    /// can no longer be trusted either way, it stops accepting work.
    #[test]
    fn unwind_parks_on_an_unknown_outcome() {
        let mut m = mirror(1);
        advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();

        let err = m.unwind(2, AppError::SubmitUnknown("no receipt".into()));

        assert!(matches!(err, AppError::SubmitUnknown(_)));
        assert!(m.is_desynced());
        assert_eq!(m.committed_count(), 4, "speculative leaves must be kept");
        assert!(matches!(
            reserve_one(&mut m),
            Err(AppError::MirrorDesynced(_))
        ));
    }

    /// Rolling back more leaves than exist cannot be honoured, so the mirror
    /// parks rather than silently carrying on — but the caller still sees the
    /// error that actually caused the unwind.
    #[test]
    fn unwind_parks_when_the_rollback_itself_fails() {
        let mut m = mirror(1);

        let err = m.unwind(99, AppError::Reverted("tx reverted".into()));

        assert!(matches!(err, AppError::Reverted(_)));
        assert!(m.is_desynced());
        assert!(matches!(
            reserve_one(&mut m),
            Err(AppError::MirrorDesynced(_))
        ));
    }

    #[test]
    fn parking_keeps_the_first_reason() {
        let mut m = mirror(1);
        let _ = m.unwind(2, AppError::SubmitUnknown("first".into()));
        let _ = m.unwind(2, AppError::SubmitUnknown("second".into()));

        let Err(AppError::MirrorDesynced(reason)) = reserve_one(&mut m) else {
            panic!("expected a desynced mirror");
        };
        assert!(reason.contains("first"), "got {reason}");
    }

    #[test]
    fn rollback_past_the_start_is_rejected() {
        let mut m = mirror(1);
        assert!(m.rollback(3).is_err());
        assert_eq!(
            m.committed_count(),
            2,
            "a rejected rollback changes nothing"
        );
    }
}
