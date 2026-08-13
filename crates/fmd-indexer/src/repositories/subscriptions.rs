use crate::domain::error::{FmdIndexerError, Result};
use async_trait::async_trait;
use database::DbPool;
use database::schema::subscriptions;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use std::fmt;

#[derive(Clone, Queryable, Selectable)]
#[diesel(table_name = subscriptions)]
/// `token` is deliberately absent: the indexer only ever needs the detection
/// key and the backfill pointer, so the client capability never enters this
/// process at all.
pub struct SubscriptionRow {
    pub id: i64,
    pub detection_key: Vec<u8>,
    pub gamma: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub active: bool,
    pub backfilled_through_note_id: i64,
}

/// Hand-written so a stray `{:?}` can never print `detection_key`. It is
/// omitted rather than masked; `finish_non_exhaustive` renders the trailing
/// `..` that says so.
impl fmt::Debug for SubscriptionRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubscriptionRow")
            .field("id", &self.id)
            .field("gamma", &self.gamma)
            .field("created_at", &self.created_at)
            .field("active", &self.active)
            .field(
                "backfilled_through_note_id",
                &self.backfilled_through_note_id,
            )
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait SubscriptionsRepo: Send + Sync {
    async fn list_active(&self) -> Result<Vec<SubscriptionRow>>;

    /// The active subscription furthest behind on history, if any is still
    /// short of `through_note_id`. One per call so a burst of registrations
    /// costs bounded work per tick instead of a fleet-wide rescan.
    async fn next_backfilling(&self, through_note_id: i64) -> Result<Option<SubscriptionRow>>;

    async fn advance_backfill(&self, id: i64, through_note_id: i64) -> Result<()>;
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

    async fn next_backfilling(&self, through_note_id: i64) -> Result<Option<SubscriptionRow>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        let row = subscriptions::table
            .filter(subscriptions::active.eq(true))
            .filter(subscriptions::backfilled_through_note_id.lt(through_note_id))
            .order(subscriptions::backfilled_through_note_id.asc())
            .select(SubscriptionRow::as_select())
            .first(&mut conn)
            .await
            .optional()?;
        Ok(row)
    }

    async fn advance_backfill(&self, id: i64, through_note_id: i64) -> Result<()> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| FmdIndexerError::Db(e.to_string()))?;
        diesel::update(subscriptions::table.filter(subscriptions::id.eq(id)))
            .set(subscriptions::backfilled_through_note_id.eq(through_note_id))
            .execute(&mut conn)
            .await?;
        Ok(())
    }
}
