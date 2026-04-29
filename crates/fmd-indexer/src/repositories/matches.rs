use crate::domain::error::{FmdIndexerError, Result};
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
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let n = diesel::insert_into(matches::table)
            .values(rows)
            .on_conflict((matches::subscription_id, matches::note_id))
            .do_nothing()
            .execute(&mut conn)
            .await?;
        Ok(n)
    }
}
