use serde::Serialize;
use utoipa::ToSchema;

/// What a transaction did. Mutually exclusive; see `repositories::transactions`
/// for the contract-level derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TxKind {
    /// Escrowed deposit whose note has landed in the tree, counted at flush
    /// time. `DepositFlushed` is emitted per deposit, so a batch of eight counts
    /// as eight.
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
    /// Every kind, in wire order. The exhaustive `match` in `as_str` turns a new
    /// variant into a compile error, and this array carries it into the parser
    /// and the error message.
    pub const ALL: [Self; 4] = [Self::Deposit, Self::Pending, Self::Transfer, Self::Withdraw];

    /// The wire spelling, identical to the literal the classification SQL emits,
    /// so a kind can be matched in SQL.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Pending => "pending",
            Self::Transfer => "transfer",
            Self::Withdraw => "withdraw",
        }
    }

    /// Inverse of `as_str`, derived from it so the two cannot drift.
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
    /// Whole tokens as a decimal string. `null` for transfers and for any asset
    /// whose decimals the indexer has not resolved.
    pub amount: Option<String>,
    /// The circuit value this withdrawal published, as a decimal string — the
    /// key its anonymity set is grouped by. Join it against `/v1/anonymity-set`
    /// on `(chainId, assetIdU64, publicOut)` to get the cohort size.
    ///
    /// `null` for every non-withdrawal kind, and for a withdrawal indexed before
    /// the contract emitted the field. Both mean the denomination is unknown, so
    /// a consumer must render that as unknown rather than as a cohort of zero.
    ///
    /// A string for the same reason as `AnonymitySetOut::public_out`: the value
    /// is a `uint64` and would not survive a JSON number intact.
    pub public_out: Option<String>,
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
