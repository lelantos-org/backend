use crate::domain::error::Result;
use async_trait::async_trait;
use database::DbPool;
pub use database::models::SubscriptionRow;
use database::schema::subscriptions;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// `(active row count, highest active id)`.
///
/// Cheap stand-in for the active subscription set. Registration inserts and
/// deregistration deletes, and nothing rewrites a detection key in place, so this
/// pair changes exactly when the set does: a new row raises the id, a removed one
/// lowers the count.
pub type ActiveFingerprint = (i64, i64);

#[async_trait]
pub trait SubscriptionsRepo: Send + Sync {
    async fn list_active(&self) -> Result<Vec<SubscriptionRow>>;

    /// Fingerprint of the active set, without reading any detection key.
    async fn active_fingerprint(&self) -> Result<ActiveFingerprint>;

    /// The active subscription furthest behind on history, if any is still short
    /// of `through_note_id`. One per call, so a burst of registrations costs
    /// bounded work per tick rather than a fleet-wide rescan.
    async fn next_backfilling(&self, through_note_id: i64) -> Result<Option<SubscriptionRow>>;

    /// Advance the backfill pointer; never rewinds it.
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
        let mut conn = super::conn(&self.pool).await?;
        let rows = subscriptions::table
            .filter(subscriptions::active.eq(true))
            .select(SubscriptionRow::as_select())
            .load(&mut conn)
            .await?;
        Ok(rows)
    }

    async fn active_fingerprint(&self) -> Result<ActiveFingerprint> {
        let mut conn = super::conn(&self.pool).await?;
        let (count, max_id): (i64, Option<i64>) = subscriptions::table
            .filter(subscriptions::active.eq(true))
            .select((
                diesel::dsl::count_star(),
                diesel::dsl::max(subscriptions::id),
            ))
            .first(&mut conn)
            .await?;
        Ok((count, max_id.unwrap_or(0)))
    }

    async fn next_backfilling(&self, through_note_id: i64) -> Result<Option<SubscriptionRow>> {
        let mut conn = super::conn(&self.pool).await?;
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
        let mut conn = super::conn(&self.pool).await?;
        // Advance only. The filter loop is unlocked, so two replicas can be
        // mid-backfill on the same subscription; without this guard the slower
        // one's older pointer lands last and rewinds the faster one, re-scanning
        // an unbounded range every round.
        diesel::update(
            subscriptions::table
                .filter(subscriptions::id.eq(id))
                .filter(subscriptions::backfilled_through_note_id.lt(through_note_id)),
        )
        .set(subscriptions::backfilled_through_note_id.eq(through_note_id))
        .execute(&mut conn)
        .await?;
        Ok(())
    }
}
