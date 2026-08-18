use crate::domain::error::Result;
use async_trait::async_trait;
use database::DbPool;
use database::schema::matches;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = matches)]
pub struct NewMatch {
    pub subscription_id: i64,
    pub note_id: i64,
    pub chain_id: i64,
}

#[async_trait]
pub trait MatchesRepo: Send + Sync {
    async fn insert_batch(&self, rows: &[NewMatch]) -> Result<usize>;
}

/// Rows per INSERT, against Postgres' 65535 bind-parameter cap at 3 columns
/// each. Unlike the other repos this one is not bounded by `filter_batch`:
/// hits are a note × subscription cartesian product, so a busy batch with a
/// large subscriber set overshoots easily.
const INSERT_CHUNK: usize = 5000;

pub struct PostgresMatchesRepo {
    pool: DbPool,
}

impl PostgresMatchesRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MatchesRepo for PostgresMatchesRepo {
    async fn insert_batch(&self, rows: &[NewMatch]) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut conn = super::conn(&self.pool).await?;
        let mut n = 0;
        for chunk in rows.chunks(INSERT_CHUNK) {
            n += diesel::insert_into(matches::table)
                .values(chunk)
                .on_conflict((matches::subscription_id, matches::note_id))
                .do_nothing()
                .execute(&mut conn)
                .await?;
        }
        Ok(n)
    }
}
