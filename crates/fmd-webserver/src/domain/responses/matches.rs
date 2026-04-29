use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatchOut {
    pub note_id: i64,
    pub chain_id: i64,
    pub block_number: i64,
    pub leaf_index: i64,
    pub commitment_hex: String,
    pub clue_bits_hex: String,
    pub ciphertext_hex: String,
    pub eph_pub_x: String,
    pub eph_pub_y: String,
}
