use crate::domain::error::{FmdIndexerError, Result};
use async_trait::async_trait;
use database::DbPool;
use database::schema::subscriptions;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = subscriptions)]
pub struct SubscriptionRow {
    pub id: i64,
    pub detection_key: Vec<u8>,
    pub gamma: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub active: bool,
}

#[async_trait]
pub trait SubscriptionsRepo: Send + Sync {
    async fn list_active(&self) -> Result<Vec<SubscriptionRow>>;
}

pub struct PostgresSubscriptionsRepo {
    pool: DbPool,
}

impl PostgresSubscriptionsRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SubscriptionsRepo for PostgresSubscriptionsRepo {
    async fn list_active(&self) -> Result<Vec<SubscriptionRow>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let rows = subscriptions::table
            .filter(subscriptions::active.eq(true))
            .select(SubscriptionRow::as_select())
            .load(&mut conn)
            .await?;
        Ok(rows)
    }
}
