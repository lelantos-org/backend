use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct PoolNotesQuery {
    /// Restrict to one chain. Absent returns one row per chain, which is the
    /// only correct way to read these counts: trees are per chain and their
    /// occupancies do not add.
    pub chain_id: Option<i64>,
}
