use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayerSubmitResponse {
    /// Tx hash returned once the on-chain `transact()` call confirms.
    pub tx_hash: String,
}
