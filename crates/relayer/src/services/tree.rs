// Per-chain in-memory tree mirror. Bootstraps from the existing `notes` +
// `tree_advances` rows on startup. The relayer is otherwise stateless across
// restarts — no DB tables of its own.
//
// Concurrency: each chain owns one `Arc<Mutex<TreeMirror>>`. Pipeline holds
// the mutex through {reserve(start_index, frontier) → prove → submit →
// receipt}, so the next bundle pipelines optimistically against the post-
// state. On revert, `rollback(2)` undoes the speculative inserts.

use crate::adapters::abi::IMasp;
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
use tracing::info;

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
        Ok(Self { chain_id, tree })
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
        for leaf in leaves {
            self.tree.insert(leaf);
        }

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
                .root_par()
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

    /// Snapshot the pre-insert state, then advance the tree by inserting the
    /// pair of leaves `Poseidon(TAG_LEAF, cm_j, cv_dep_j_x, cv_dep_j_y)`.
    /// Caller rolls back via `rollback(2)` on chain revert.
    pub fn reserve_and_advance(
        &mut self,
        cm0: Field,
        cm1: Field,
        cv_dep0: &[U256; 2],
        cv_dep1: &[U256; 2],
    ) -> AppResult<(ReservedSlot, AdvancedState)> {
        self.reserve_and_advance_batch(&[(cm0, *cv_dep0), (cm1, *cv_dep1)])
    }

    /// Insert N `(cm, cv_dep)` pairs. The mirror hashes each pair into a
    /// leaf before insertion to stay in sync with the on-chain tree, which
    /// advances via SNARK-verified leaf roots.
    pub fn reserve_and_advance_batch(
        &mut self,
        cms: &[(Field, [U256; 2])],
    ) -> AppResult<(ReservedSlot, AdvancedState)> {
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
            self.tree.insert(leaf);
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
    pub async fn verify_chain_root(&self, rpc_url: &str, pool_address_hex: &str) -> AppResult<()> {
        let url: alloy::transports::http::reqwest::Url = rpc_url
            .parse()
            .map_err(|e: url::ParseError| AppError::Internal(format!("rpc url: {}", e)))?;
        let pool_address = Address::from_str(pool_address_hex)
            .map_err(|e| AppError::Internal(format!("pool addr: {}", e)))?;
        let provider = ProviderBuilder::new().on_http(url);
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

    /// Hypothetical advance: insert `(cm0, cm1)` to compute `start_index`,
    /// `old_root`, `new_root`, and `old_frontier`, then immediately roll
    /// back so external state is unchanged. Used by estimate endpoints to
    /// build calldata for `eth_estimateGas` without committing leaves.
    pub fn preview_advance(
        &mut self,
        cm0: Field,
        cm1: Field,
        cv_dep0: &[U256; 2],
        cv_dep1: &[U256; 2],
    ) -> AppResult<(ReservedSlot, AdvancedState)> {
        let r = self.reserve_and_advance(cm0, cm1, cv_dep0, cv_dep1)?;
        self.rollback(2)?;
        Ok(r)
    }

    pub fn rollback(&mut self, n: usize) -> AppResult<()> {
        let before = self.tree.leaf_count();
        if n > before {
            return Err(AppError::Internal(format!(
                "rollback {} > leaf_count {} on chain {}",
                n, before, self.chain_id
            )));
        }
        self.tree.truncate_leaves(n);
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

fn bigdecimal_to_u256(v: &bigdecimal::BigDecimal) -> AppResult<U256> {
    use bigdecimal::num_bigint::Sign;
    let (bi, exp) = v.as_bigint_and_exponent();
    if exp != 0 {
        return Err(AppError::Internal(format!(
            "cv_dep has fractional part: {}",
            v
        )));
    }
    if bi.sign() == Sign::Minus {
        return Err(AppError::Internal("cv_dep negative".into()));
    }
    let bytes = bi.to_bytes_be().1;
    if bytes.len() > 32 {
        return Err(AppError::Internal("cv_dep > 32 bytes".into()));
    }
    let mut buf = [0u8; 32];
    buf[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(U256::from_be_bytes(buf))
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
