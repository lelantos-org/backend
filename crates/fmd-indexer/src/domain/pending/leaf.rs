//! One leaf's payload, and the two things built from it: its tree hash and its
//! `notes` row.

use crate::domain::error::FmdIndexerError;
use alloy::primitives::U256;
use chain_types::numeric::u256_to_bigdecimal;
use crypto::tree::{Field, leaf_hash};
use database::models::{NewNote, RawEventRow};

/// Bytes of clueBits the FMD filter reads off the front of every ciphertext.
/// A leaf whose ciphertext is shorter cannot be scanned, so it is not stored.
const CLUE_BITS_PREFIX: usize = 2;

/// The FMD payload one tree leaf carries.
///
/// `NoteCreated` supplies it inline; a `DepositFlushed` sources it from the
/// `DepositEscrowed` event that opened the deposit. The columns are the same
/// either way, so both paths build the note through `LeafPayload::into_note`.
#[derive(Clone)]
pub struct LeafPayload {
    pub cm: Vec<u8>,
    pub clue_rx: U256,
    pub clue_ry: U256,
    pub eph_pub_x: U256,
    pub eph_pub_y: U256,
    pub ciphertext: Vec<u8>,
    pub cv_dep_x: U256,
    pub cv_dep_y: U256,
}

/// One leaf as the commitment tree sees it, whether or not it became a note.
///
/// This is the difference between the tree state the indexer can produce and the
/// one a reader of `notes` can. A leaf whose ciphertext is too short to scan is
/// still a leaf the contract inserted, and it still moves the root; dropping it
/// from the tree would put every later leaf one position out.
#[derive(Debug)]
pub struct TreeLeaf {
    /// Absolute once the transaction's first root is known, and the bare ordinal
    /// until then, exactly like [`NewNote::leaf_index`]. `PendingTx::set_root`
    /// rebases both together.
    pub leaf_index: i64,
    pub hash: Field,
}

impl LeafPayload {
    pub(super) fn has_clue_bits(&self) -> bool {
        self.ciphertext.len() >= CLUE_BITS_PREFIX
    }

    /// `Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`, the value the contract
    /// inserted. Depends on none of the fields that can make a leaf unusable.
    pub(super) fn tree_hash(&self) -> Result<Field, FmdIndexerError> {
        let cm = crypto::tree::field_from_bytes(&self.cm)
            .map_err(|e| FmdIndexerError::Decode(format!("leaf cm: {e}")))?;
        leaf_hash(
            &cm,
            &self.cv_dep_x.to_be_bytes::<32>(),
            &self.cv_dep_y.to_be_bytes::<32>(),
        )
        .map_err(|e| FmdIndexerError::Decode(format!("leaf hash: {e}")))
    }

    /// Carries block coordinates only. A tx hash in the log would point from an
    /// operator's log stream straight into the note-to-deposit-to-payer join.
    pub(super) fn into_note(self, chain_id: i64, row: &RawEventRow, leaf_index: i64) -> NewNote {
        NewNote {
            chain_id,
            block_number: row.block_number,
            tx_hash: row.tx_hash.clone(),
            log_index: row.log_index,
            cm: self.cm,
            clue_rx: u256_to_bigdecimal(self.clue_rx),
            clue_ry: u256_to_bigdecimal(self.clue_ry),
            eph_pub_x: u256_to_bigdecimal(self.eph_pub_x),
            eph_pub_y: u256_to_bigdecimal(self.eph_pub_y),
            ciphertext: self.ciphertext,
            leaf_index,
            cv_dep_x: u256_to_bigdecimal(self.cv_dep_x),
            cv_dep_y: u256_to_bigdecimal(self.cv_dep_y),
        }
    }
}
