use ark_ed_on_bn254::Fq;
use ark_ff::PrimeField;

use super::{Field, TreeError};
use crate::poseidon;

pub(super) const ARITY: usize = 4;
pub const TAG_MERKLE: u64 = 5;

/// Domain-separation tag for Merkle leaf hashing, mirroring `TAG_LEAF` in
/// `circuits/src/lib/tags.circom`.
pub const TAG_LEAF: u64 = 10;

/// Serialise straight out of the `BigInt` limbs rather than through
/// `to_bytes_be`, which heap-allocates a `Vec` per call. The tree calls this
/// once per node, so a bootstrap replay of the full tree would otherwise make
/// millions of one-shot allocations for a value whose width is known.
pub(crate) fn fq_to_be(x: Fq) -> Field {
    let mut out = [0u8; 32];
    // `BigInt` limbs are little-endian; big-endian bytes want them reversed.
    for (chunk, limb) in out.chunks_exact_mut(8).zip(x.into_bigint().0.iter().rev()) {
        chunk.copy_from_slice(&limb.to_be_bytes());
    }
    out
}

pub(crate) fn be_to_fq(x: &Field) -> Fq {
    Fq::from_be_bytes_mod_order(x)
}

/// `Poseidon(TAG_MERKLE, c0, c1, c2, c3)` over a whole sibling group.
///
/// Takes the group as one array rather than four references: every caller
/// already builds the four children as a run -- a `frontier` fold, a level of
/// `rebuild_from` -- and spelling them out separately only gave each call site a
/// chance to transpose two of them.
pub(super) fn hash_node(children: &[Field; ARITY]) -> Result<Field, TreeError> {
    let inputs = [
        Fq::from(TAG_MERKLE),
        be_to_fq(&children[0]),
        be_to_fq(&children[1]),
        be_to_fq(&children[2]),
        be_to_fq(&children[3]),
    ];
    let out = poseidon::hash(&inputs).map_err(|e| TreeError::Poseidon(e.to_string()))?;
    Ok(fq_to_be(out))
}

/// The in-circuit Merkle leaf: `Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`.
///
/// One definition rather than one per service: fmd-indexer needs it to advance
/// the stored frontier and fmd-webserver to serve the commitment chunk feed, and
/// a copy that drifted would put a root on the wire that the circuit never
/// accepts, with nothing short of a failed proof to reveal it.
///
/// The relayer keeps its own (`relayer::services::tree::leaf_hash`) rather than
/// calling this. It feeds the same inputs through `poseidon::hash_bytes_be`,
/// which *rejects* a value at or above the modulus where `be_to_fq` reduces it.
/// The two agree on every canonical field element and so on every real leaf;
/// they differ only in which one refuses a malformed one, and the relayer wants
/// the refusal.
pub fn leaf_hash(cm: &Field, cv_dep_x: &Field, cv_dep_y: &Field) -> Result<Field, TreeError> {
    let inputs = [
        Fq::from(TAG_LEAF),
        be_to_fq(cm),
        be_to_fq(cv_dep_x),
        be_to_fq(cv_dep_y),
    ];
    let out = poseidon::hash(&inputs).map_err(|e| TreeError::Poseidon(e.to_string()))?;
    Ok(fq_to_be(out))
}
