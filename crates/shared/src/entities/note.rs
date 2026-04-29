use crate::chain::ChainId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: i64,
    pub chain_id: ChainId,
    pub block_number: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub commitment: Vec<u8>,
    /// 2-byte big-endian clueBits prefix + ChaCha20-Poly1305 body. Indexers
    /// MUST split as `clue_bits = u16::from_be_bytes(ciphertext[0..2])`,
    /// `body = ciphertext[2..]`.
    pub ciphertext: Vec<u8>,
    /// Leaf position in the canonical merkle tree. Set by fmd-indexer once
    /// the matching `RootAdvanced` event is observed (cm0 = startIndex,
    /// cm1 = startIndex + 1).
    pub leaf_index: i64,
}
