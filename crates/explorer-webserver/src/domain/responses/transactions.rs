use serde::Serialize;
use utoipa::ToSchema;

/// What a transaction did. Mutually exclusive and exact — see
/// `repositories::transactions` for the contract-level derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
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
    /// Every kind, in wire order. The exhaustive `match` in `as_str` is what
    /// makes a new variant a compile error rather than a silent gap; this array
    /// is what makes it show up in the parser and the error message too.
    pub const ALL: [Self; 4] = [Self::Deposit, Self::Pending, Self::Transfer, Self::Withdraw];

    /// The wire spelling, and the literal the classification SQL emits — the
    /// two are the same string, which is what lets a kind be matched in SQL.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Pending => "pending",
            Self::Transfer => "transfer",
            Self::Withdraw => "withdraw",
        }
    }

    /// Inverse of `as_str`, so the two cannot drift apart.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == s)
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
