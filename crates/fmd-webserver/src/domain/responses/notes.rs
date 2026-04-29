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
    /// First 2 bytes of `ciphertext` decoded as a big-endian u16, formatted
    /// `0xNNNN`. Convenience for FMD bucket scans.
    pub clue_bits_hex: String,
    pub ciphertext_hex: String,
    /// Sender's ECDH ephemeral public point (Baby-Jubjub coordinates as
    /// decimal field-element strings). Receiver feeds these into
    /// `decryptNote` to recover the note plaintext.
    pub eph_pub_x: String,
    pub eph_pub_y: String,
}
