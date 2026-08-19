use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoteOut {
    pub id: i64,
    pub chain_id: i64,
    pub block_number: i64,
    pub leaf_index: i64,
    pub commitment_hex: String,
    pub ciphertext_hex: String,
    /// Sender's ECDH ephemeral public point, packed the way circomlibjs
    /// `babyJub.packPoint` does: 32 bytes of `y` **little-endian**, high bit
    /// of the last byte set when `x > (q-1)/2`. Bare hex, like
    /// `ciphertext_hex`, because it is a byte string and not a number.
    ///
    /// The receiver feeds these bytes to `decryptNote` as `epk` unchanged.
    pub eph_pub_packed_hex: String,
}
