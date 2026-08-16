use serde::Serialize;
use utoipa::ToSchema;

/// What a transaction did. Mutually exclusive and exact — see
/// `repositories::transactions` for the contract-level derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TxKind {
    /// Escrowed deposit whose note has landed in the tree, counted at flush
    /// time. `DepositFlushed` is per deposit, so a batch of eight is eight.
    Deposit,
    /// Escrowed deposit still waiting for a flush. Becomes `deposit` once the
    /// relayer batches it, so a bucket's composition can change until then.
    Pending,
    /// Internal transfer between shielded notes. Moves no public value.
    Transfer,
    /// Unshield to a public recipient.
    Withdraw,
}

impl TxKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "deposit" => Self::Deposit,
            "pending" => Self::Pending,
            "transfer" => Self::Transfer,
            "withdraw" => Self::Withdraw,
            _ => return None,
        })
    }
}

/// One classified transaction.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TxOut {
    pub chain_id: i64,
    pub tx_hash_hex: String,
    pub block_number: i64,
    pub block_ts: i64,
    pub kind: TxKind,
    /// `null` for transfers, which move no public value.
    pub asset_id_u64: Option<i64>,
    /// Whole tokens as a decimal string. `null` for transfers, and for any
    /// asset whose decimals the indexer has not resolved yet.
    pub amount: Option<String>,
}

/// Transaction counts for one time bucket, split by kind.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KindCounts {
    pub ts: i64,
    pub deposit: i64,
    pub pending: i64,
    pub transfer: i64,
    pub withdraw: i64,
}
