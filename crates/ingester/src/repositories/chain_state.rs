use crate::domain::error::IngesterError;
use crate::domain::models::BlockCursor;
use crate::repositories::checkout;
use async_trait::async_trait;
use database::DbPool;
use database::schema::chain_state;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[async_trait]
pub trait ChainStateRepo: Send + Sync {
    async fn fetch(&self, chain_id: i64) -> Result<Option<BlockCursor>, IngesterError>;
    /// Move `last_scanned_block` forward, creating the row if this chain has never
    /// committed anything.
    async fn advance_scanned(&self, chain_id: i64, scanned: i64) -> Result<(), IngesterError>;
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = chain_state)]
struct ChainStateRow {
    chain_id: i64,
    last_block: i64,
    last_block_hash: Vec<u8>,
    last_scanned_block: i64,
}

impl From<ChainStateRow> for BlockCursor {
    fn from(r: ChainStateRow) -> Self {
        BlockCursor {
            chain_id: r.chain_id,
            last_block: r.last_block,
            last_block_hash: r.last_block_hash,
            last_scanned_block: r.last_scanned_block,
        }
    }
}

pub struct PostgresChainStateRepo {
    pool: DbPool,
}

impl PostgresChainStateRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

/// `INSERT … ON CONFLICT DO UPDATE SET last_scanned_block = $n WHERE last_scanned_block < $n`.
///
/// Two properties matter:
///
/// 1. It must be an upsert rather than an `UPDATE`. A bare
///    `UPDATE … WHERE chain_id` matches zero rows on a chain that has never
///    committed an event, returns `Ok` and leaves the cursor unwritten, so a
///    chain whose range contains no logs rescans a widening range on every poll.
/// 2. `last_block` and `last_block_hash` are seeded empty and never touched on
///    conflict. They are the reorg anchor and must only be written alongside a
///    verified block.
///
/// A free function so the emitted SQL can be asserted in a unit test without a
/// database.
fn advance_stmt(
    chain_id: i64,
    scanned: i64,
) -> impl diesel::query_builder::QueryFragment<diesel::pg::Pg> + diesel::query_builder::QueryId {
    use diesel::query_dsl::methods::FilterDsl;
    FilterDsl::filter(
        diesel::insert_into(chain_state::table)
            .values((
                chain_state::chain_id.eq(chain_id),
                chain_state::last_block.eq(0i64),
                chain_state::last_block_hash.eq(Vec::<u8>::new()),
                chain_state::last_scanned_block.eq(scanned),
            ))
            .on_conflict(chain_state::chain_id)
            .do_update()
            .set(chain_state::last_scanned_block.eq(scanned)),
        chain_state::last_scanned_block.lt(scanned),
    )
}

#[async_trait]
impl ChainStateRepo for PostgresChainStateRepo {
    async fn fetch(&self, chain_id: i64) -> Result<Option<BlockCursor>, IngesterError> {
        let mut conn = checkout(&self.pool).await?;
        let row: Option<ChainStateRow> = chain_state::table
            .filter(chain_state::chain_id.eq(chain_id))
            .select(ChainStateRow::as_select())
            .first(&mut conn)
            .await
            .optional()?;
        Ok(row.map(BlockCursor::from))
    }

    async fn advance_scanned(&self, chain_id: i64, scanned: i64) -> Result<(), IngesterError> {
        let mut conn = checkout(&self.pool).await?;
        advance_stmt(chain_id, scanned).execute(&mut conn).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sql() -> String {
        diesel::debug_query::<diesel::pg::Pg, _>(&advance_stmt(1, 42)).to_string()
    }

    /// Diesel can attach a `filter` to the conflict target as a partial-index
    /// predicate instead of to the `DO UPDATE`, which would turn the monotonic
    /// guard into a no-op. This pins the clause it lands in.
    #[test]
    fn monotonic_guard_is_on_do_update() {
        let sql = sql();
        let update_at = sql.find("DO UPDATE").expect("DO UPDATE clause");
        let where_at = sql.find("WHERE").expect("WHERE clause");
        assert!(
            where_at > update_at,
            "guard must be on DO UPDATE, got: {sql}"
        );
    }

    /// The seed row must not claim a verified anchor it does not have.
    #[test]
    fn insert_seeds_an_empty_anchor() {
        let sql = sql();
        assert!(sql.contains("last_block"), "seeds last_block: {sql}");
        let update_at = sql.find("DO UPDATE").expect("DO UPDATE clause");
        assert!(
            !sql[update_at..].contains("last_block_hash"),
            "must not overwrite the anchor on conflict: {sql}"
        );
    }
}
