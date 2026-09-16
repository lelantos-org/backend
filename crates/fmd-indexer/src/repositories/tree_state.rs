use crate::domain::error::Result;
use async_trait::async_trait;
use database::DbPool;
pub use database::models::TreeStateRow;
use database::schema::tree_state;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[async_trait]
pub trait TreeStateRepo: Send + Sync {
    async fn load(&self, chain_id: i64) -> Result<Option<TreeStateRow>>;

    /// Advance the chain's stored tree, but only if it still stands at
    /// `expected_from` leaves. Returns whether it applied.
    ///
    /// The guard is what makes a replay safe. A consume tick is a sequence of
    /// separate statements rather than one transaction, and the cursor rewinds to
    /// 0 after a reorg, so the same leaves can be presented twice. `notes` absorbs
    /// that through `ON CONFLICT DO NOTHING`; a frontier cannot, because applying
    /// the same leaf twice moves the root somewhere no chain ever was. Refusing
    /// the write when the base has moved leaves the caller to re-read and skip
    /// what is already in.
    async fn advance(&self, expected_from: i64, next: &TreeStateRow) -> Result<bool>;

    /// The root the chain published once it held exactly `leaf_count` leaves, from
    /// `tree_advances`, or `None` if no advance ends there.
    ///
    /// On this repository because `tree_advances` is the chain's own record of the
    /// same tree this table mirrors. It is the only external check available: a
    /// frontier folded from `notes` is self-consistent whether or not it is right,
    /// and comparing against the published root is what tells the two apart.
    async fn published_root(&self, chain_id: i64, leaf_count: i64) -> Result<Option<Vec<u8>>>;
}

#[derive(QueryableByName)]
struct PublishedRoot {
    #[diesel(sql_type = diesel::sql_types::Bytea)]
    new_root: Vec<u8>,
}

pub struct PostgresTreeStateRepo {
    pool: DbPool,
}

impl PostgresTreeStateRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TreeStateRepo for PostgresTreeStateRepo {
    async fn load(&self, chain_id: i64) -> Result<Option<TreeStateRow>> {
        let mut conn = super::conn(&self.pool).await?;
        tree_state::table
            .find(chain_id)
            .select(TreeStateRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(Into::into)
    }

    async fn published_root(&self, chain_id: i64, leaf_count: i64) -> Result<Option<Vec<u8>>> {
        let mut conn = super::conn(&self.pool).await?;
        // Raw SQL: `inserted` is an INTEGER and `start_index` a BIGINT, and the
        // sum of the two is not an expression diesel's DSL will build. Runs once
        // per backfill, so the scan this costs over
        // `tree_advances_chain_start_idx` is not worth reshaping the predicate
        // for.
        let found = diesel::sql_query(
            "SELECT new_root FROM tree_advances \
              WHERE chain_id = $1 AND start_index + inserted = $2 \
              LIMIT 1",
        )
        .bind::<diesel::sql_types::BigInt, _>(chain_id)
        .bind::<diesel::sql_types::BigInt, _>(leaf_count)
        .load::<PublishedRoot>(&mut conn)
        .await?;
        Ok(found.into_iter().next().map(|r| r.new_root))
    }

    async fn advance(&self, expected_from: i64, next: &TreeStateRow) -> Result<bool> {
        let mut conn = super::conn(&self.pool).await?;
        // Raw SQL because diesel cannot attach a `WHERE` to `DO UPDATE`, and the
        // guard has to ride on the same statement as the write: checking the leaf
        // count in a prior query would leave a window for a second writer between
        // the two. `DO NOTHING` on a base that has moved is what the `bool` says.
        let n = diesel::sql_query(
            "INSERT INTO tree_state (chain_id, leaf_count, root, frontier) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (chain_id) DO UPDATE \
                SET leaf_count = EXCLUDED.leaf_count, \
                    root       = EXCLUDED.root, \
                    frontier   = EXCLUDED.frontier, \
                    updated_at = now() \
              WHERE tree_state.leaf_count = $5",
        )
        .bind::<diesel::sql_types::BigInt, _>(next.chain_id)
        .bind::<diesel::sql_types::BigInt, _>(next.leaf_count)
        .bind::<diesel::sql_types::Bytea, _>(&next.root)
        .bind::<diesel::sql_types::Bytea, _>(&next.frontier)
        .bind::<diesel::sql_types::BigInt, _>(expected_from)
        .execute(&mut conn)
        .await?;
        Ok(n > 0)
    }
}
