use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::schema::{consumer_cursors, subscriptions};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Must match `fmd_indexer::services::filter::NAME`. Kept as a literal
/// here so the webserver doesn't pull in the indexer crate.
const FMD_FILTER_CURSOR: &str = "fmd-filter";

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = subscriptions)]
pub struct SubscriptionRow {
    pub id: i64,
    pub detection_key: Vec<u8>,
    pub gamma: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub active: bool,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = subscriptions)]
pub struct NewSubscription {
    pub detection_key: Vec<u8>,
    pub gamma: i32,
    pub active: bool,
}

pub async fn list(pool: &DbPool) -> AppResult<Vec<SubscriptionRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    subscriptions::table
        .order(subscriptions::id.asc())
        .select(SubscriptionRow::as_select())
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

pub async fn create(pool: &DbPool, dk: Vec<u8>, gamma: i32) -> AppResult<SubscriptionRow> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    diesel::insert_into(subscriptions::table)
        .values(NewSubscription {
            detection_key: dk,
            gamma,
            active: true,
        })
        .returning(SubscriptionRow::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

/// Reset the fmd-filter cursor to 0 across all chains so the next
/// indexer tick re-scans every existing note against the new subscription.
/// No-op if no cursor row exists yet (first-run case).
pub async fn reset_filter_cursor(pool: &DbPool) -> AppResult<usize> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    diesel::update(consumer_cursors::table.filter(consumer_cursors::name.eq(FMD_FILTER_CURSOR)))
        .set((
            consumer_cursors::last_event_id.eq(0_i64),
            consumer_cursors::last_block_number.eq(0_i64),
        ))
        .execute(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

pub async fn delete(pool: &DbPool, id: i64) -> AppResult<usize> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    diesel::delete(subscriptions::table.filter(subscriptions::id.eq(id)))
        .execute(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
