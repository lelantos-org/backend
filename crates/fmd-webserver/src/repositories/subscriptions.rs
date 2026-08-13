use crate::domain::error::{AppError, AppResult};
use crate::domain::token::TokenHash;
use database::DbPool;
use database::schema::subscriptions;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use std::fmt;

#[derive(Clone, Queryable, Selectable)]
#[diesel(table_name = subscriptions)]
/// `token_hash` is deliberately absent: every lookup supplies the hash it
/// searches for, so no caller needs to read one back.
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

#[derive(Clone, Insertable)]
#[diesel(table_name = subscriptions)]
struct NewSubscription {
    detection_key: Vec<u8>,
    gamma: i32,
    active: bool,
    token_hash: Vec<u8>,
}

/// Resolve a capability token to the internal subscription id. `None` when it
/// matches nothing — callers must not distinguish that from an empty result
/// set.
pub async fn id_by_token(pool: &DbPool, token: &TokenHash) -> AppResult<Option<i64>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    subscriptions::table
        .filter(subscriptions::token_hash.eq(token.as_bytes()))
        .select(subscriptions::id)
        .first(&mut conn)
        .await
        .optional()
        .map_err(|e| AppError::Db(e.to_string()))
}

/// Full row behind a token. The create path uses it to distinguish a client
/// re-registering from one claiming a token that is already taken.
pub async fn find_by_token(pool: &DbPool, token: &TokenHash) -> AppResult<Option<SubscriptionRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    subscriptions::table
        .filter(subscriptions::token_hash.eq(token.as_bytes()))
        .select(SubscriptionRow::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(|e| AppError::Db(e.to_string()))
}

/// `Ok(None)` when the token is already taken. The unique index on
/// `token_hash` is the authority on token ownership, so its violation is an
/// expected outcome rather than an error: it detects a re-registration
/// without a lookup on the path that creates.
pub async fn create(
    pool: &DbPool,
    dk: Vec<u8>,
    gamma: i32,
    token: &TokenHash,
) -> AppResult<Option<SubscriptionRow>> {
    use diesel::result::{DatabaseErrorKind, Error as DieselError};

    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    match diesel::insert_into(subscriptions::table)
        .values(NewSubscription {
            detection_key: dk,
            gamma,
            active: true,
            token_hash: token.as_bytes().to_vec(),
        })
        .returning(SubscriptionRow::as_returning())
        .get_result(&mut conn)
        .await
    {
        Ok(row) => Ok(Some(row)),
        Err(DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _)) => Ok(None),
        Err(e) => Err(AppError::Db(e.to_string())),
    }
}

pub async fn delete_by_token(pool: &DbPool, token: &TokenHash) -> AppResult<usize> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    diesel::delete(subscriptions::table.filter(subscriptions::token_hash.eq(token.as_bytes())))
        .execute(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
